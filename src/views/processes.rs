//! Processes: the collector's top-N list, ranked by the 60s rolling average.
//!
//! The list is not the whole process table. zstats materialises
//! `max-processes` (default 50) by CPU then memory; everything else is a
//! count in the header. Sorting only talks about this already-truncated
//! set. The row shape is the original one: name + CPU, pid + memory,
//! then a meter — the trough is what keeps each row from floating.

use super::widgets;
use crate::alerttpl;
use crate::confirm;
use crate::font;
use crate::format;
use crate::i18n;
use crate::procscan;
use crate::procscan::ProcState;
use crate::state::{FullScan, FullScanData, ProcSort, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, div, list, px,
};
use gpui_component::input::Input;
use gpui_component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::process;
use zstats::Tick;
use zstats::alerts::ActiveThresholds;
use zstats::snapshot::{Capabilities, ProcessSnapshot};

/// Floor for the per-row CPU bar. The original scale was 800% (eight
/// cores), which left every normal row looking empty. One core is the
/// unit the number is already in; the page max wins if something is
/// hotter, so the trough still fills on a busy machine.
pub(super) const BAR_FLOOR_PERCENT: f32 = 100.0;

/// What a row's meter is drawn from, and what a full track means.
///
/// The measure follows the list's ranking key on purpose. With rows
/// ordered by memory and meters drawn from CPU — which is what this used
/// to do — one list carries two different rankings, and the sort reads as
/// broken. Name order ranks by no magnitude at all, so there the page's
/// own subject keeps the track.
#[derive(Clone, Copy)]
struct BarScale {
    by_memory: bool,
    full: f32,
}

impl BarScale {
    /// Which page the maxima come from is the caller's call: the top
    /// list measures the rows it shows, the full listing measures the
    /// whole table rather than the filtered cut, so its meters do not
    /// rescale on every keystroke.
    fn new(sort: ProcSort, cpu_max: f32, mem_max: u64) -> Self {
        let by_memory = matches!(sort, ProcSort::Memory);
        Self {
            by_memory,
            // Memory gets no floor-at-one-core equivalent: bytes have no
            // natural unit to hold the track against, so the largest row
            // on the page is the only honest full. A `max(1)` only keeps
            // the division defined.
            full: if by_memory {
                (mem_max as f32).max(1.0)
            } else {
                cpu_max.max(BAR_FLOOR_PERCENT)
            },
        }
    }

    fn fraction(&self, p: &ProcessSnapshot, cpu: f64) -> f32 {
        if self.by_memory {
            shown_memory(p) as f32 / self.full
        } else {
            cpu as f32 / self.full
        }
    }
}

/// A burst this big is worth colour — one whole core.
///
/// Used to be two cores (200%). On a 12-core machine that is a sixth of
/// the box, so the list stayed ink all day and the bar lost its only
/// severity cue. One core is still a fixed percentage, not scaled by
/// hardware: the same 110% is red here and there.
const HOT_PERCENT: f64 = 100.0;

/// Whether a row gets the page's only colour. Overview's top trees use
/// the burst half of this (`sustained = false`) so a 110% tree is red
/// here and there.
///
/// Two ways in, because "worth a look" has two shapes. A burst over
/// [`HOT_PERCENT`] is the obvious one. The other is a process holding a
/// low-but-real share for a long time (`watch.rs`), which **by
/// definition never crosses a threshold** — it was the whole reason that
/// watcher exists, and the colour used to ignore it: a compile pegged
/// at 110% lit up, while a daemon sitting on 60% for four hours looked
/// exactly like one at 3%.
///
/// Display only, like every threshold in `views/` — the alert engine is
/// untouched, and the sustained watcher still notifies on its own terms.
pub(super) fn is_hot(cpu: f64, sustained: bool) -> bool {
    cpu > HOT_PERCENT || sustained
}

/// Header plus caveat above the list: 36 for the header (11 + 16 + 9), and
/// 34 for the note, which wraps to two lines at this width in every locale.
/// Subtracted from the body height to leave the list its bound.
///
/// Overshooting is the safe direction — the list then ends a few pixels
/// short of the footer, where undershooting makes the body itself scroll
/// and the panel grow a second scrollbar.
const FULL_CHROME_HEIGHT: f32 = 70.;

/// What the open filter row adds to the chrome above the full-scan list.
const FILTER_ROW_HEIGHT: f32 = 34.;

/// Used for the frame or two before a window size has been reported. Roughly
/// one panel's worth; the next frame corrects it.
const FULL_LIST_FALLBACK: f32 = 480.;

/// Never shorter than this, however small the window gets — below it the
/// list shows one row and reads as broken rather than as short.
const FULL_LIST_MIN: f32 = 120.;

/// The memory figure a row shows and sorts by: the physical footprint when
/// zstats reports one, resident memory otherwise.
///
/// Footprint is what macOS bills the process for — private dirty pages,
/// compressed pages, GPU/IOKit allocations — i.e. Activity Monitor's Memory
/// column. RSS misses the compressed and GPU parts entirely, which is how a
/// Metal-heavy app can read 80 MB while actually costing 300 MB. The
/// fallback exists because `proc_pid_rusage` fails on other users'
/// processes, so an unprivileged collector reports `None` for root-owned
/// daemons; RSS is then the best figure zstats has for them. Both numbers
/// come off the snapshot — nothing is derived here — and the expanded row
/// spells out which is which.
fn shown_memory(p: &ProcessSnapshot) -> u64 {
    p.phys_footprint_bytes.unwrap_or(p.memory_bytes)
}

/// Height budget for the scrolling rows region of the top-N card: the
/// body, minus the pinned header (36 — same figure FULL_CHROME_HEIGHT
/// breaks down) and the filter row when open. Shared with the Apps card,
/// whose chrome is identical.
pub(super) fn rows_height(state: &ZStatsAppState) -> f32 {
    let chrome = 36.
        + if state.proc_filter_open() {
            FILTER_ROW_HEIGHT
        } else {
            0.
        };
    (super::body_height(state).unwrap_or(FULL_LIST_FALLBACK) - chrome).max(FULL_LIST_MIN)
}

/// Ranked by the 60s rolling average so the order is stable enough to read.
pub(super) fn ranked(tick: &Tick) -> Option<Vec<(&ProcessSnapshot, f64)>> {
    let processes = tick.snapshot.processes.as_deref()?;
    let mut rows: Vec<_> = processes
        .iter()
        .map(|p| {
            let cpu = tick
                .process_stats
                .get(&p.pid)
                .map(|s| s.cpu_avg)
                .unwrap_or(p.cpu_usage_percent as f64);
            (p, cpu)
        })
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    Some(rows)
}

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    // A landed scan replaces the tab rather than sitting under it: it is a
    // different measurement (see `full_scan_note`), and stacking the two
    // lists would invite reading a row from one against a row from the
    // other.
    if let FullScan::Ready(data) = state.full_scan() {
        return vec![full_scan_card(state, data)];
    }

    let Some(tick) = state.latest() else {
        return vec![widgets::empty_card(
            i18n::tr("processes.title"),
            i18n::tr("common.waiting_sample"),
        )];
    };
    let Some(mut rows) = ranked(tick) else {
        return vec![widgets::empty_card(
            i18n::tr("processes.off"),
            i18n::tr("processes.off_body"),
        )];
    };
    // `ranked` hands back CPU order; the user's choice is applied here
    // so it only affects this tab.
    match state.proc_sort() {
        ProcSort::Cpu => {}
        ProcSort::Memory => rows.sort_by_key(|(p, _)| Reverse(shown_memory(p))),
        // Case-insensitive, or every capitalised app name would sort ahead of
        // every lowercase daemon — not what "by name" means to someone
        // scanning for one. `_cached_` because the key allocates: plain
        // `sort_by_key` would rebuild every lowercased name on each of the
        // O(n log n) comparisons, every frame.
        ProcSort::Name => rows.sort_by_cached_key(|(p, _)| p.name.to_lowercase()),
    }

    // The name filter, shared with the full-scan view. The query is stored
    // lowercased; names are lowercased here to match.
    let filter = state.proc_filter_text();
    if !filter.is_empty() {
        rows.retain(|(p, _)| p.name.to_lowercase().contains(filter));
    }

    let total = tick
        .snapshot
        .total_processes
        .map(|t| t.to_string())
        .unwrap_or_else(|| format::PLACEHOLDER.to_string());
    let only_abnormal = state.only_abnormal();
    // Gathered once: `abnormal()` filters and collects, and the header alone
    // used to ask for it three times per frame.
    let abnormal = state.abnormal();
    let name_by_pid: HashMap<u32, &str> = tick
        .snapshot
        .processes
        .as_deref()
        .map(|v| v.iter().map(|p| (p.pid, p.name.as_str())).collect())
        .unwrap_or_default();
    // Floor at one core so a quiet page does not stretch its hottest
    // 8% process across the whole track.
    let bar = BarScale::new(
        state.proc_sort(),
        rows.iter().map(|(_, cpu)| *cpu as f32).fold(0.0, f32::max),
        rows.iter().map(|(p, _)| shown_memory(p)).max().unwrap_or(0),
    );

    let no_match = !filter.is_empty() && rows.is_empty() && !only_abnormal;

    let title = if only_abnormal {
        t!("processes.abnormal_only", count = abnormal.len()).to_string()
    } else {
        t!("processes.count_of", shown = rows.len(), total = total).to_string()
    };

    let list_el = widgets::list_shell()
        .child(widgets::list_header(
            h_flex()
                .items_center()
                .gap(px(4.))
                .min_w_0()
                .child(div().min_w_0().truncate().child(title))
                .child(widgets::info_icon(
                    "proc-cpu-basis",
                    i18n::tr("processes.cpu_basis_tip"),
                )),
            Some(
                h_flex()
                    .items_center()
                    .gap(px(5.))
                    .when(!only_abnormal, |d| d.child(filter_chip(state)))
                    .when(!only_abnormal, |d| d.child(avg_chip()))
                    .when(!abnormal.is_empty(), |d| {
                        d.child(abnormal_filter(state, abnormal.len()))
                    })
                    .when(!only_abnormal, |d| d.child(sort_control(state)))
                    .when(!only_abnormal, |d| d.child(full_scan_chip(state)))
                    .into_any_element(),
            ),
        ))
        .children(filter_row(state))
        .child(
            // Rows scroll inside the card while the header, chips and
            // filter row stay pinned — the full listing's model, minus the
            // virtualisation (at most max-processes rows, all of which the
            // old whole-panel scroll built per frame anyway). `max_h`, not
            // `h`, so a short list keeps a short card.
            v_flex()
                .id("proc-rows")
                .track_scroll(state.proc_rows_scroll())
                .overflow_y_scroll()
                .max_h(px(rows_height(state)))
                .children(abnormal_rows(
                    state,
                    &abnormal,
                    !only_abnormal && !rows.is_empty(),
                ))
                .children({
                    let shown = if only_abnormal { 0 } else { rows.len() };
                    rows.into_iter()
                        .take(shown)
                        .enumerate()
                        .map(move |(i, (p, avg))| {
                            let parent_name =
                                p.parent_pid.and_then(|pp| name_by_pid.get(&pp).copied());
                            process_row(p, avg, bar, i + 1 == shown, state, parent_name)
                        })
                })
                .when(no_match, |d| {
                    d.child(
                        div()
                            .px(px(13.))
                            .py(px(10.))
                            .child(widgets::note(i18n::tr("processes.filter_no_match"))),
                    )
                }),
        );

    let mut cards = Vec::with_capacity(2);
    // A scan that failed says so here rather than through the chip alone: a
    // control that quietly goes back to its old label reads as a click that
    // never registered.
    if matches!(state.full_scan(), FullScan::Failed) {
        cards.push(widgets::empty_card(
            i18n::tr("processes.full_failed"),
            i18n::tr("processes.full_failed_body"),
        ));
    }
    cards.push(list_el.into_any_element());
    cards
}

/// One process row — the same element in the collector's list and the
/// full-scan list, so the two present a process identically: name and CPU,
/// pid and memory, a meter, click-to-expand, kill.
///
/// `cpu` is whatever the caller ranks by — the 60s rolling average in the
/// top list, the scan-window sample in the full one. The row cannot tell
/// which it was given; the full-scan card's caveat line is what owns that
/// difference. `bar` decides what the meter measures, which is the list's
/// sort key rather than always CPU — see [`BarScale`].
fn process_row(
    p: &ProcessSnapshot,
    cpu: f64,
    bar: BarScale,
    is_last: bool,
    state: &ZStatsAppState,
    parent_name: Option<&str>,
) -> AnyElement {
    let sustained = state.sustained_load(p.pid);
    let hot = is_hot(cpu, sustained.is_some());
    // `Default` is `current()`, so a row painted before the first tick
    // reads this build's own capabilities — never a false "unsupported".
    let caps = state
        .latest()
        .map(|tick| tick.snapshot.capabilities)
        .unwrap_or_default();
    let expanded = state.selected_pid() == Some(p.pid);
    let pid = p.pid;
    let is_self = pid == process::id();
    let avg = cpu;

    let mut row = v_flex()
        .id(("proc", pid as usize))
        // Explicit because of where this row can land: a flex column
        // stretches its children, but `gpui::list` measures each item on
        // its own, and without a width the row shrinks to its content —
        // every line a different length.
        .w_full()
        .px(px(13.))
        .py(px(7.))
        .when(!is_last, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .when(expanded, |d| d.bg(theme::surface_raised()))
        .hover(|d| d.bg(theme::surface_raised()))
        .child(
            h_flex()
                .items_baseline()
                .justify_between()
                .gap(px(8.))
                .child(widgets::truncating_name(
                    ("proc-name", pid as usize),
                    p.name.clone(),
                    12.,
                    gpui::FontWeight::MEDIUM,
                    Hsla::from(theme::text()),
                ))
                .child(
                    div()
                        .flex_none()
                        .font_family(font::MONO)
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::text_for(hot))
                        .child(format::pct_col(avg as f32)),
                ),
        )
        .child(
            h_flex()
                .justify_between()
                .gap(px(8.))
                .mt(px(2.))
                .text_size(px(10.))
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(5.))
                        .min_w_0()
                        .child(
                            div()
                                .text_color(if format::notable_status(&p.status) {
                                    theme::accent_light()
                                } else {
                                    theme::text_dim()
                                })
                                .child(if format::notable_status(&p.status) {
                                    t!(
                                        "processes.pid_status",
                                        pid = p.pid,
                                        status = p.status.clone()
                                    )
                                    .to_string()
                                } else {
                                    t!("processes.pid_only", pid = p.pid).to_string()
                                }),
                        )
                        .when(is_self, |d| d.child(this_app_badge()))
                        .children(sustained.map(|dur| {
                            // This never trips the alert — that asks
                            // whether it is over the line right now,
                            // and it never is. So the badge is the
                            // only place it can announce itself.
                            pill(
                                t!(
                                    "processes.sustained",
                                    duration = format::uptime(dur.as_secs())
                                )
                                .to_string(),
                                true,
                            )
                        })),
                )
                .child(
                    div()
                        .font_family(font::MONO)
                        .text_color(theme::text_muted())
                        .child(format::memory(shown_memory(p))),
                ),
        )
        .child(div().mt(px(4.)).child(widgets::meter(
            bar.fraction(p, avg),
            Hsla::from(theme::fill_for(hot)),
            3.,
        )));

    if expanded {
        // Resolved only now: it needs a name lookup, and only one row per
        // frame is ever expanded.
        row = row.child(expand_block(
            p,
            parent_name,
            current_user_id().as_deref(),
            caps,
            state,
        ));
    }

    row.on_click(move |_, _window, cx| {
        cx.global::<ZStatsGlobalStore>()
            .clone()
            .update(cx, |state, cx| state.toggle_pid(pid, cx));
    })
    .into_any_element()
}

/// The whole table, virtualised, in the same rows as the top list.
///
/// `gpui::list` builds only what is in view — the panel repaints on every
/// tick, and 694 rows of eager elements would be rebuilt every two seconds
/// for the sake of the twelve on screen. Unlike `uniform_list` it measures
/// each rendered row, which is what lets these rows be [`process_row`] —
/// expansion included — instead of a fixed-height reduction of it.
fn full_scan_card(state: &ZStatsAppState, data: &FullScanData) -> AnyElement {
    // Cloned into the list closure, which must be 'static — it cannot
    // borrow the store the rest of this file reads. State is re-read from
    // the App handed to the closure instead.
    let processes = data.processes.clone();
    // The filter's cut, precomputed in the store — the same query the top
    // list applies per frame. See `refresh_full_scan_filter`. Sort is
    // applied here rather than stored: the chip is the same control as
    // the top list, and rebuilding `ListState` on every cycle would
    // throw away the scroll position for a 600-row jump to the top.
    let mut visible = data.visible.clone();
    sort_indices(&mut visible, &processes, state.proc_sort());
    let count = visible.len();
    // Same floor-at-one-core rule as the top list, over this listing's own
    // page, so the meters in the two lists mean the same thing. Over the
    // whole page rather than the filtered cut, so the meters do not
    // rescale on every keystroke.
    let bar = BarScale::new(
        state.proc_sort(),
        processes
            .iter()
            .map(|p| p.cpu_usage_percent)
            .fold(0.0, f32::max),
        processes.iter().map(shown_memory).max().unwrap_or(0),
    );
    let chrome = FULL_CHROME_HEIGHT
        + if state.proc_filter_open() {
            FILTER_ROW_HEIGHT
        } else {
            0.
        };
    let height =
        (super::body_height(state).unwrap_or(FULL_LIST_FALLBACK) - chrome).max(FULL_LIST_MIN);

    widgets::list_shell()
        .child(widgets::list_header(
            t!("processes.full_count", shown = count, total = data.total).to_string(),
            Some(
                h_flex()
                    .items_center()
                    .gap(px(5.))
                    .child(filter_chip(state))
                    .child(sort_control(state))
                    .child(full_scan_chip(state))
                    .into_any_element(),
            ),
        ))
        .child(
            div()
                .px(px(13.))
                .pb(px(8.))
                .child(widgets::note(full_scan_note(data))),
        )
        .children(filter_row(state))
        .when(count == 0, |d| {
            d.child(
                div()
                    .px(px(13.))
                    .py(px(10.))
                    .child(widgets::note(i18n::tr("processes.filter_no_match"))),
            )
        })
        .child(
            list(data.list.clone(), move |i, _window, cx| {
                let state = cx.global::<ZStatsGlobalStore>().read(cx);
                let p = &processes[visible[i]];
                // Resolved with a linear find because the closure owns only
                // the Arc — the top list's pid→name map borrows from the
                // tick and cannot cross the 'static boundary. Only the one
                // expanded row per frame ever asks.
                let parent_name = (state.selected_pid() == Some(p.pid))
                    .then(|| {
                        p.parent_pid
                            .and_then(|pp| processes.iter().find(|q| q.pid == pp))
                            .map(|q| q.name.as_str())
                    })
                    .flatten();
                process_row(
                    p,
                    f64::from(p.cpu_usage_percent),
                    bar,
                    i + 1 == count,
                    state,
                    parent_name,
                )
            })
            .h(px(height)),
        )
        .into_any_element()
}

/// Why the numbers below disagree with the ones above.
///
/// Not optional garnish: this list's CPU% covers the few hundred
/// milliseconds of the scan, while every other view reports a 60-second
/// rolling average. The same process legitimately reads differently in the
/// two places, and an unexplained second number is worse than none.
fn full_scan_note(data: &FullScanData) -> String {
    t!(
        "processes.full_note",
        ms = data.window.as_millis(),
        age = format::ago(data.at.elapsed())
    )
    .to_string()
}

/// The header control that opens the name filter.
pub(super) fn filter_chip(state: &ZStatsAppState) -> AnyElement {
    let on = state.proc_filter_open();
    let tip = i18n::tr("processes.filter_tip");
    div()
        .id("proc-filter")
        .flex_none()
        .rounded(px(4.))
        .p(px(2.))
        .when(on, |d| d.bg(theme::chip()))
        .when(!on, |d| d.hover(|d| d.bg(theme::surface_raised())))
        .tooltip(widgets::wrap_tooltip(tip))
        .child(
            Icon::new(IconName::Search)
                .with_size(Size::Size(px(11.)))
                .text_color(Hsla::from(if on {
                    theme::text()
                } else {
                    theme::text_muted()
                })),
        )
        .on_click(|_, window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.toggle_proc_filter(window, cx));
        })
        .into_any_element()
}

/// The name-filter input, when open. One filter, both lists — it renders
/// under whichever header is showing.
pub(super) fn filter_row(state: &ZStatsAppState) -> Option<AnyElement> {
    if !state.proc_filter_open() {
        return None;
    }
    let input = state.proc_filter_input()?;
    Some(
        div()
            .px(px(13.))
            .pb(px(6.))
            .child(Input::new(input).xsmall().cleanable(true))
            .into_any_element(),
    )
}

/// The control that asks for the whole table, and the one that gives it back.
///
/// Deliberately not a config change: raising the collector's `max-processes`
/// would also change what alerts and what the history file records — see
/// [`crate::fullscan`].
fn full_scan_chip(state: &ZStatsAppState) -> AnyElement {
    let scan = state.full_scan();
    let running = matches!(scan, FullScan::Running);
    let showing = matches!(scan, FullScan::Ready(_));
    let label = if running {
        i18n::tr("processes.full_scanning")
    } else if showing {
        i18n::tr("processes.full_back")
    } else {
        i18n::tr("processes.full_chip")
    };
    let tip = i18n::tr(if showing {
        "processes.full_back_tip"
    } else {
        "processes.full_chip_tip"
    });

    div()
        .id("full-scan")
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(if showing {
            theme::border()
        } else {
            theme::border_subtle()
        })
        .when(showing, |d| d.bg(theme::chip()))
        // Only while off, and never while a scan is in flight: a hover fill
        // on a control that ignores the click is a lie about what it does.
        .when(!showing && !running, |d| {
            d.hover(|d| d.bg(theme::surface_raised()).border_color(theme::border()))
        })
        .px(px(7.))
        .py(px(1.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(if running {
            theme::text_dim()
        } else if showing {
            theme::text()
        } else {
            theme::text_muted()
        })
        .tooltip(widgets::wrap_tooltip(tip))
        .child(label)
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.toggle_full_scan(cx));
        })
        .into_any_element()
}

/// The words for a percent-style alert bar (CPU here, app CPU on the
/// Apps expansion), resolved from what zstats' own `Thresholds` said
/// about this name. Pure text so both pages and the tests share one
/// vocabulary. "override" means a per-name entry matched — the user's
/// own or the template table's; zstats does not say which layer won,
/// and re-matching here to find out would be a second implementation
/// of its precedence.
pub(super) fn pct_bar_text(matched: Option<Option<f32>>, base: Option<f32>) -> String {
    match matched {
        Some(Some(v)) => t!("processes.bar_override", value = format!("{v:.0}%")).to_string(),
        Some(None) => i18n::tr("processes.bar_off_override"),
        None => match base {
            Some(v) => format!("{v:.0}%"),
            None => i18n::tr("processes.bar_rule_off"),
        },
    }
}

/// Same vocabulary for the memory bar, whose effective figure is bytes
/// (zstats resolves share-vs-ceiling itself — `bytes` is its answer for
/// this name). `base_share` is the fallback wording for the frame or
/// two before a tick has reported a total to resolve against.
pub(super) fn mem_bar_text(
    matched: Option<Option<f64>>,
    bytes: Option<u64>,
    base_share: Option<f64>,
) -> String {
    match matched {
        Some(Some(_)) => match bytes {
            Some(b) => t!("processes.bar_override", value = format::memory(b)).to_string(),
            None => i18n::tr("processes.bar_rule_off"),
        },
        Some(None) => i18n::tr("processes.bar_off_override"),
        None => match bytes {
            Some(b) => format::memory(b),
            None => match base_share {
                Some(f) => format!("{:.0}%", f * 100.0),
                None => i18n::tr("processes.bar_rule_off"),
            },
        },
    }
}

/// The two alert bars zstats holds for exactly this name — the answer
/// to "why is this row at 300% and quiet". Resolved through zstats' own
/// `ActiveThresholds` against the live template table ([`alerttpl`]),
/// the same resolution the engine armed itself with; nothing here
/// evaluates a condition, it only reads the line back out.
fn alert_bar_rows(name: &str, state: &ZStatsAppState) -> Vec<(String, String)> {
    let Some(file) = state.settings() else {
        return Vec::new();
    };
    let loaded = alerttpl::info();
    let eff = ActiveThresholds::from_config_with_template(&file.alerts, &loaded.template);
    let total = state.latest().map(|t| t.snapshot.memory.total_bytes);
    vec![
        (
            i18n::tr("processes.bar_cpu"),
            pct_bar_text(eff.cpu.override_for(name), eff.cpu.base()),
        ),
        (
            i18n::tr("processes.bar_mem"),
            mem_bar_text(
                eff.memory.override_for(name),
                total.and_then(|t| eff.memory_bar_bytes(name, t)),
                eff.memory.base(),
            ),
        ),
    ]
}

fn expand_block(
    p: &ProcessSnapshot,
    parent_name: Option<&str>,
    current_uid: Option<&str>,
    caps: Capabilities,
    state: &ZStatsAppState,
) -> AnyElement {
    let pid = p.pid;
    let mut detail = vec![
        (i18n::tr("processes.live"), format::pct(p.cpu_usage_percent)),
        // Both memory figures, labeled: the row's single number is the
        // footprint when available, and this is where the split is spelled
        // out — a GUI process can legitimately read 80 MB in one and
        // 300 MB in the other (GPU + compressed vs shared pages).
        (
            i18n::tr("processes.mem_footprint"),
            // A missing footprint has two readings, and until zstats
            // 0.5.2 the row could not tell them apart. `capabilities`
            // answers the platform half; on macOS — where it is always
            // true — what is left is the permission half, which is the
            // honest word for a root daemon `proc_pid_rusage` refuses.
            match p.phys_footprint_bytes {
                Some(bytes) => format::memory(bytes),
                None if !caps.memory_footprint => i18n::tr("processes.mem_no_platform"),
                None => i18n::tr("processes.mem_no_permission"),
            },
        ),
        (
            i18n::tr("processes.mem_rss"),
            format::memory(p.memory_bytes),
        ),
        (i18n::tr("processes.run"), format::uptime(p.run_time_secs)),
        (
            i18n::tr("processes.parent"),
            parent_display(p.parent_pid, parent_name),
        ),
        (
            i18n::tr("processes.disk_io"),
            disk_io_display(p.read_bytes_per_sec, p.write_bytes_per_sec),
        ),
    ];
    if show_user(p.user_id.as_deref(), current_uid) {
        detail.push((
            i18n::tr("processes.user"),
            p.user_id
                .clone()
                .unwrap_or_else(|| format::PLACEHOLDER.into()),
        ));
    }
    // Last, after the measurements: these two are policy, not readings —
    // what has to happen before this process makes the Alerts tab.
    detail.extend(alert_bar_rows(&p.name, state));

    let cmd = p.cmd.clone();
    let headline = cmd_headline(&cmd).to_string();
    let name = p.name.clone();

    v_flex()
        .mt(px(8.))
        .p(px(10.))
        .rounded(px(8.))
        .bg(theme::inset())
        .child(
            div()
                .id(("cmd", pid as usize))
                .text_size(px(10.))
                .text_color(theme::text_muted())
                .truncate()
                .when(!cmd.is_empty(), |d| {
                    d.tooltip(widgets::wrap_tooltip(cmd.clone()))
                })
                .child(if headline.is_empty() {
                    format::PLACEHOLDER.to_string()
                } else {
                    headline
                }),
        )
        .child(widgets::kv_columns(detail))
        .when(crate::terminate::can_term(pid), |col| {
            col.child(h_flex().justify_end().child(kill_button(pid, name)))
        })
        .into_any_element()
}

fn pill(text: impl Into<gpui::SharedString>, accent: bool) -> AnyElement {
    div()
        .flex_none()
        .rounded(px(3.))
        .px(px(4.))
        .bg(if accent {
            theme::accent_wash(14)
        } else {
            theme::inset()
        })
        .text_size(px(9.))
        .text_color(if accent {
            theme::accent_light()
        } else {
            theme::tiny_label(theme::text_muted())
        })
        .child(text.into())
        .into_any_element()
}

fn this_app_badge() -> AnyElement {
    pill(i18n::tr("processes.this_app"), false)
}

fn avg_chip() -> AnyElement {
    let tip = i18n::tr("processes.avg_tip");
    div()
        .id("proc-avg-window")
        .flex_none()
        .rounded(px(4.))
        .px(px(5.))
        .py(px(1.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::tiny_label(theme::text_muted()))
        .tooltip(widgets::wrap_tooltip(tip))
        .child(i18n::tr("processes.avg_window"))
        .into_any_element()
}

fn kill_button(pid: u32, name: String) -> AnyElement {
    let label = i18n::tr("processes.kill_ok");
    div()
        .id(("kill", pid as usize))
        .mt(px(8.))
        .h(px(20.))
        .px(px(8.))
        .rounded(px(5.))
        .border_1()
        .border_color(theme::accent_wash(45))
        .flex()
        .items_center()
        .justify_center()
        .hover(|d| d.bg(theme::accent_wash(10)))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let name = name.clone();
            confirm::ask(
                window,
                cx,
                i18n::tr("processes.kill_title"),
                t!("processes.kill_body", name = name, pid = pid).to_string(),
                i18n::tr("processes.kill_ok"),
                move |_| kill(pid),
            );
        })
        .child(
            div()
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::accent_light())
                .child(label),
        )
        .into_any_element()
}

fn abnormal_badge(pid: u32, state: ProcState, tip: String) -> AnyElement {
    h_flex()
        .items_center()
        .gap(px(4.))
        .flex_none()
        .child(
            div()
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent_light())
                .child(state.label()),
        )
        .child(
            div()
                .id(("abnormal-info", pid as usize))
                .flex_none()
                .p(px(1.))
                .tooltip(widgets::wrap_tooltip(tip))
                .child(
                    Icon::new(IconName::Info)
                        .with_size(Size::Size(px(12.)))
                        .text_color(Hsla::from(theme::text_dim())),
                ),
        )
        .into_any_element()
}

/// SIGTERM. Activity Monitor's "Quit", not Force Quit.
///
/// The delivery itself lives in `terminate.rs` with the alert card's
/// quit — the app acts on a process in exactly one module, and a
/// syscall does what spawning `/bin/kill` used to.
#[cfg(target_os = "macos")]
fn kill(pid: u32) {
    // Inline: a signal is a syscall that returns immediately. The
    // thread this used to spawn was there for the `/bin/kill`
    // subprocess, and went away with it.
    crate::terminate::request_term(pid);
}

/// Never-run stub — see "Platform reality" in CLAUDE.md.
#[cfg(not(target_os = "macos"))]
fn kill(_pid: u32) {}

/// The filter chip in the list header.
///
/// Only appears when there is something abnormal — on a healthy machine the
/// header keeps the 60s / sort chips and no control is offered for a
/// filter that would empty the list.
fn abnormal_filter(state: &ZStatsAppState, count: usize) -> AnyElement {
    let active = state.only_abnormal();
    div()
        .id("filter-abnormal")
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(theme::accent_wash(45))
        .when(active, |d| d.bg(theme::accent()))
        // Only while off — see the same pattern on the Network chip.
        .when(!active, |d| d.hover(|d| d.bg(theme::accent_wash(14))))
        .px(px(7.))
        .py(px(1.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(if active {
            gpui::white()
        } else {
            Hsla::from(theme::accent_light())
        })
        .child(t!("processes.abnormal_filter", count = count).to_string())
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.toggle_only_abnormal(cx));
        })
        .into_any_element()
}

/// Rows for processes stuck in a state that should not persist.
///
/// Collected separately from the metrics snapshot and shown at the top of the
/// same list: zstats keeps the top N by CPU then memory, and a zombie scores
/// near zero on both, so these would never surface in the table below however
/// far it is scrolled.
fn abnormal_rows(
    state: &ZStatsAppState,
    found: &[&procscan::AbnormalProcess],
    more_follows: bool,
) -> Vec<AnyElement> {
    let total = found.len();
    // "zstats (60337)" when the scan could name the parent, bare pid
    // when it could not — never a guess.
    let parent_label = |p: &procscan::AbnormalProcess| match &p.parent_name {
        Some(name) => format!("{name} ({})", p.parent_pid),
        None => p.parent_pid.to_string(),
    };
    found
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let is_last = !more_follows && i + 1 == total;
            let observed = state.abnormal_observed(p.pid);
            let tip = {
                let hint = i18n::tr(match p.state {
                    ProcState::Zombie => "processes.zombie_hint",
                    ProcState::Stopped => "processes.stopped_hint",
                });
                let meta = t!(
                    "processes.abnormal_meta",
                    ppid = parent_label(p),
                    age = format::uptime(p.age.as_secs())
                )
                .to_string();
                match observed {
                    Some(d) => format!(
                        "{hint}\n{meta} · {}",
                        t!(
                            "processes.abnormal_observed",
                            observed = format!("≥{}", format::uptime(d.as_secs()))
                        )
                    ),
                    None => format!("{hint}\n{meta}"),
                }
            };
            v_flex()
                .px(px(12.))
                .py(px(8.))
                .when(!is_last, |d| {
                    d.border_b(px(1.)).border_color(theme::border_subtle())
                })
                .bg(theme::accent_wash(7))
                .child(
                    h_flex()
                        .items_baseline()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::text())
                                .truncate()
                                .child(p.name.clone()),
                        )
                        .child(abnormal_badge(p.pid, p.state, tip)),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .gap(px(8.))
                        .mt(px(2.))
                        .text_size(px(9.))
                        .text_color(theme::tiny_label(theme::text_dim()))
                        .child(
                            div().child(
                                t!(
                                    "processes.abnormal_meta",
                                    ppid = parent_label(p),
                                    age = format::uptime(p.age.as_secs())
                                )
                                .to_string(),
                            ),
                        )
                        .children(observed.map(|d| {
                            div().child(
                                t!(
                                    "processes.abnormal_observed",
                                    observed = format!("≥{}", format::uptime(d.as_secs()))
                                )
                                .to_string(),
                            )
                        })),
                )
                .into_any_element()
        })
        .collect()
}

fn sort_indices(indices: &mut [usize], processes: &[ProcessSnapshot], sort: ProcSort) {
    match sort {
        // The collector already returns CPU descending; we still sort so
        // a filter cut (and a later cycle back to CPU) is not left in
        // whatever order the previous key left it.
        ProcSort::Cpu => indices.sort_by(|&a, &b| {
            processes[b]
                .cpu_usage_percent
                .total_cmp(&processes[a].cpu_usage_percent)
        }),
        ProcSort::Memory => {
            indices.sort_by_key(|&i| Reverse(shown_memory(&processes[i])));
        }
        ProcSort::Name => indices.sort_by_cached_key(|&i| processes[i].name.to_lowercase()),
    }
}

/// Cycles the list ordering. A single button rather than a menu — there are
/// only a handful of orderings and 320px of width to spend.
fn sort_control(state: &ZStatsAppState) -> AnyElement {
    let sort = state.proc_sort();
    let full = matches!(state.full_scan(), FullScan::Ready(_));
    let tip = i18n::tr(if full {
        sort.full_tip_key()
    } else {
        sort.tip_key()
    });
    div()
        .id("sort-processes")
        .flex_none()
        .rounded(px(4.))
        .px(px(5.))
        .py(px(1.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .hover(|d| d.bg(theme::surface_raised()))
        .tooltip(widgets::wrap_tooltip(tip))
        .child(format!("↓ {}", i18n::tr(sort.label_key())))
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.cycle_proc_sort(cx));
        })
        .into_any_element()
}

/// If `cmd` lives inside a `.app` bundle, the bundle's own name.
///
/// `/Applications/Safari.app/Contents/MacOS/Safari --foo` → `Safari.app`.
/// A helper deep inside Xcode still reports `Xcode.app`, which is the
/// thing a person looking at the path actually wants to confirm.
fn bundle_name(cmd: &str) -> Option<&str> {
    let end = cmd.find(".app/").map(|i| i + 4).or_else(|| {
        cmd.find(".app ")
            .or_else(|| cmd.ends_with(".app").then_some(cmd.len() - 4))
            .map(|i| i + 4)
    })?;
    let start = cmd[..end].rfind('/')? + 1;
    (start < end).then(|| &cmd[start..end])
}

fn cmd_headline(cmd: &str) -> &str {
    bundle_name(cmd).unwrap_or(cmd)
}

fn well_known_name(pid: u32) -> Option<&'static str> {
    match pid {
        0 => Some(if cfg!(target_os = "macos") {
            "kernel_task"
        } else {
            "kernel"
        }),
        1 => Some(if cfg!(target_os = "macos") {
            "launchd"
        } else {
            "init"
        }),
        _ => None,
    }
}

fn parent_display(ppid: Option<u32>, name: Option<&str>) -> String {
    let Some(ppid) = ppid else {
        return format::PLACEHOLDER.to_string();
    };
    match name.or_else(|| well_known_name(ppid)) {
        Some(name) => format!("{name} · {ppid}"),
        None => ppid.to_string(),
    }
}

fn disk_io_display(read: Option<u64>, write: Option<u64>) -> String {
    match (read, write) {
        (None, None) => format::PLACEHOLDER.to_string(),
        (r, w) => format!("R {} · W {}", format::rate(r), format::rate(w)),
    }
}

fn show_user(user_id: Option<&str>, current: Option<&str>) -> bool {
    match (user_id, current) {
        (Some(u), Some(c)) => u != c,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

fn current_user_id() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        Some(unsafe { libc::getuid() }.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_name_reads_the_app_wrapper() {
        assert_eq!(
            bundle_name("/Applications/企业微信.app/Contents/MacOS/企业微信"),
            Some("企业微信.app")
        );
        assert_eq!(
            bundle_name("/Applications/Safari.app/Contents/MacOS/Safari --foo"),
            Some("Safari.app")
        );
        assert_eq!(
            bundle_name("/Applications/Xcode.app/Contents/Developer/usr/bin/git"),
            Some("Xcode.app")
        );
        assert_eq!(bundle_name("/usr/sbin/cupsd"), None);
        assert_eq!(bundle_name("node"), None);
        assert_eq!(cmd_headline("/usr/sbin/cupsd"), "/usr/sbin/cupsd");
    }

    #[test]
    fn parent_display_names_launchd_and_known_pids() {
        assert_eq!(parent_display(None, Some("zed")), format::PLACEHOLDER);
        assert_eq!(parent_display(Some(42), Some("zed")), "zed · 42");
        assert_eq!(parent_display(Some(99), None), "99");
        #[cfg(target_os = "macos")]
        {
            assert_eq!(parent_display(Some(1), None), "launchd · 1");
            assert_eq!(parent_display(Some(0), None), "kernel_task · 0");
        }
    }

    #[test]
    fn disk_io_marks_read_and_write() {
        assert_eq!(disk_io_display(None, None), format::PLACEHOLDER);
        assert_eq!(disk_io_display(Some(0), Some(0)), "R 0 B/s · W 0 B/s");
        assert_eq!(
            disk_io_display(Some((17.8 * 1024.0 * 1024.0) as u64), Some(0)),
            "R 17.8 MB/s · W 0 B/s"
        );
    }

    #[test]
    fn foreign_user_hides_the_current_uid() {
        assert!(!show_user(Some("501"), Some("501")));
        assert!(show_user(Some("0"), Some("501")));
        assert!(show_user(Some("501"), None));
        assert!(!show_user(None, Some("501")));
    }

    /// The colour has two ways in, and the second one is the point: a
    /// process that never crosses the burst line still earns it by
    /// holding a share for long enough. The old rule could only see
    /// bursts, so the daemon that had been at 60% all afternoon looked
    /// exactly like one at 3%.
    #[test]
    fn a_long_hold_is_hot_even_though_it_never_crosses_the_burst_line() {
        assert!(is_hot(110.0, false), "one core and a bit is a burst");
        assert!(
            !is_hot(100.0, false),
            "exactly one core is the line, not over it"
        );
        assert!(!is_hot(60.0, false), "a moment at 60% is just work");
        assert!(is_hot(60.0, true), "the same 60% held long enough is not");
        assert!(is_hot(0.4, true), "the watcher's own bar is well under 100");
    }

    /// A list may only draw one ranking. Sorting by memory and filling
    /// the meters from CPU is two, and the second one reads as the sort
    /// having failed.
    #[test]
    fn the_meter_measures_whatever_the_list_is_sorted_by() {
        let heavy = snap("heavy", 20.0, 8_000_000_000);
        let hot = snap("hot", 400.0, 100_000_000);

        let by_cpu = BarScale::new(ProcSort::Cpu, 400.0, 8_000_000_000);
        assert_eq!(by_cpu.fraction(&hot, 400.0), 1.0, "hottest fills the track");
        assert_eq!(by_cpu.fraction(&heavy, 20.0), 20.0 / 400.0);

        let by_mem = BarScale::new(ProcSort::Memory, 400.0, 8_000_000_000);
        assert_eq!(by_mem.fraction(&heavy, 20.0), 1.0, "largest fills it");
        assert_eq!(
            by_mem.fraction(&hot, 400.0),
            100_000_000.0 / 8_000_000_000.0
        );

        // Name order ranks by no magnitude, so the page's own subject
        // keeps the track.
        let by_name = BarScale::new(ProcSort::Name, 400.0, 8_000_000_000);
        assert_eq!(by_name.fraction(&hot, 400.0), 1.0);

        // A quiet page does not stretch its hottest 8% across the track.
        let quiet = BarScale::new(ProcSort::Cpu, 8.0, 1_000);
        assert_eq!(quiet.fraction(&snap("idle", 8.0, 1_000), 8.0), 0.08);
        // An empty page divides by something.
        assert_eq!(
            BarScale::new(ProcSort::Memory, 0.0, 0).fraction(&snap("x", 0.0, 0), 0.0),
            0.0
        );
    }

    #[test]
    fn shown_memory_prefers_footprint_and_falls_back_to_rss() {
        let mut p = snap("zed", 0.0, 80);
        assert_eq!(shown_memory(&p), 80, "no footprint → RSS");
        p.phys_footprint_bytes = Some(300);
        assert_eq!(shown_memory(&p), 300, "footprint wins when reported");
    }

    fn snap(name: &str, cpu: f32, mem: u64) -> ProcessSnapshot {
        ProcessSnapshot {
            pid: 1,
            name: name.into(),
            cmd: String::new(),
            cpu_usage_percent: cpu,
            cpu_time_ms: 0,
            memory_bytes: mem,
            phys_footprint_bytes: None,
            virtual_memory_bytes: 0,
            run_time_secs: 0,
            parent_pid: None,
            user_id: None,
            status: String::new(),
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    #[test]
    fn full_scan_sorts_the_visible_cut() {
        let processes = vec![
            snap("zed", 2.0, 300),
            snap("Chrome", 40.0, 80),
            snap("ice", 1.0, 900),
        ];
        let mut idx = vec![0, 1, 2];
        sort_indices(&mut idx, &processes, ProcSort::Cpu);
        assert_eq!(idx, vec![1, 0, 2]);
        sort_indices(&mut idx, &processes, ProcSort::Memory);
        assert_eq!(idx, vec![2, 0, 1]);
        sort_indices(&mut idx, &processes, ProcSort::Name);
        assert_eq!(idx, vec![1, 2, 0]); // Chrome, ice, zed
    }

    /// The expansion's alert-bar rows answer "why is this at 300% and
    /// quiet" — so the wording must distinguish a matched per-name entry
    /// (user's or the template's) from the base rule, and a disabled
    /// rule from one that simply sits high.
    #[test]
    fn a_bar_says_whether_a_per_name_entry_decided_it() {
        // Base rule, no match: just the number.
        assert_eq!(pct_bar_text(None, Some(80.0)), "80%");
        // A matched entry carries the override marker — this is the
        // kernel_task case, the row the question is usually about.
        assert_eq!(
            pct_bar_text(Some(Some(300.0)), Some(80.0)),
            "300% · override"
        );
        // Matched and disabled: off, and it says why.
        assert_eq!(pct_bar_text(Some(None), Some(80.0)), "off · override");
        // Rule off entirely.
        assert_eq!(pct_bar_text(None, None), "off");
    }

    #[test]
    fn the_memory_bar_prefers_zstats_resolved_bytes() {
        // zstats resolves share-vs-ceiling itself; the row shows its
        // answer, never a locally derived one.
        assert_eq!(
            mem_bar_text(None, Some(4 * 1024 * 1024 * 1024), Some(0.25)),
            "4.0 GB"
        );
        assert_eq!(
            mem_bar_text(Some(Some(0.7)), Some(45 * 1024 * 1024 * 1024), Some(0.25)),
            "45.0 GB · override"
        );
        assert_eq!(mem_bar_text(Some(None), None, Some(0.25)), "off · override");
        // Before the first tick there is no total to resolve against —
        // the share is all that can be said honestly.
        assert_eq!(mem_bar_text(None, None, Some(0.25)), "25%");
        assert_eq!(mem_bar_text(None, None, None), "off");
    }
}
