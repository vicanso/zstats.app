//! Applications: whole process trees, summed.
//!
//! A browser's 37 helpers never individually cross a threshold that the
//! app as a whole blows past. zstats sums each tree over the FULL process
//! table, then keeps only `max-processes` groups (default 50) ranked by
//! CPU — the same cap as the Processes tab. Idle apps are missing on
//! purpose, not because their members were dropped. Sorting by memory
//! reorders that already-truncated set; it does not fetch the
//! memory-hottest trees the collector dropped. The All chip is the
//! escape hatch: a one-shot scan with the cap off, same reasons as
//! [`crate::fullscan`].

use super::processes;
use super::widgets;
use crate::confirm;
use crate::font;
use crate::format;
use crate::fullscan;
use crate::i18n;
use crate::state::{AppSort, FullAppScan, FullAppScanData, ZStatsAppState, ZStatsGlobalStore};
use crate::terminate;
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, div, list, px,
};
use gpui_component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use zstats::snapshot::{ProcessGroupSnapshot, ProcessSnapshot};

/// Same one-core colour line as [`super::processes`] — a tree and a
/// process that both read 110% must agree about "hot". Display only.
const HOT_PERCENT: f32 = 100.0;

/// Header + caveat line. Same budget as the process full-scan card so
/// the list fills the panel instead of leaving a strip of empty card.
const FULL_CHROME_HEIGHT: f32 = 70.;
// Same value as processes::FILTER_ROW_HEIGHT — the row itself is shared
// (`processes::filter_row`), so a smaller budget here made the full list
// overflow by the difference and the header slid before pinning.
const FILTER_ROW_HEIGHT: f32 = 34.;
const FULL_LIST_FALLBACK: f32 = 480.;
const FULL_LIST_MIN: f32 = 120.;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    if let FullAppScan::Ready(data) = state.full_app_scan() {
        return vec![full_scan_card(state, data)];
    }

    let Some(tick) = state.latest() else {
        return vec![widgets::empty_card(
            i18n::tr("apps.title"),
            i18n::tr("common.waiting_sample"),
        )];
    };
    let Some(groups) = tick.snapshot.process_groups.as_deref() else {
        return vec![widgets::empty_card(
            i18n::tr("apps.off"),
            i18n::tr("apps.off_body"),
        )];
    };

    let mut rows: Vec<_> = groups.iter().collect();
    rows.sort_by(|a, b| b.cpu_usage_percent.total_cmp(&a.cpu_usage_percent));
    // Collector already ranked by CPU and capped; this only reorders that
    // set — same contract as [`ProcSort`] on the process list.
    let sort = state.app_sort();
    match sort {
        AppSort::Cpu => {}
        AppSort::Memory => rows.sort_by_key(|g| Reverse(shown_memory(g))),
    }

    let filter = state.proc_filter_text();
    if !filter.is_empty() {
        rows.retain(|g| g.name.to_lowercase().contains(filter));
    }

    let shown = rows.len();
    let no_match = !filter.is_empty() && shown == 0;
    let bar_full = bar_full_for(sort, rows.iter().copied());

    let list_el = widgets::list_shell()
        .child(widgets::list_header(
            h_flex()
                .items_center()
                .gap(px(4.))
                .min_w_0()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(t!("apps.count_of", shown = shown).to_string()),
                )
                .child(widgets::info_icon(
                    "apps-cpu-basis",
                    i18n::tr("apps.cpu_basis_tip"),
                )),
            Some(
                h_flex()
                    .items_center()
                    .gap(px(5.))
                    .child(processes::filter_chip(state))
                    .child(sort_control(state))
                    .child(full_scan_chip(state))
                    // The whole explanation — what a tree is, why `login`
                    // can legitimately head the list, where the cap comes
                    // from. Used to be a "tree totals" label that sat next
                    // to All and read as a third chip; ⓘ is the same idiom
                    // the title already uses for the CPU-basis note.
                    .child(widgets::info_icon(
                        "apps-tree-totals",
                        i18n::tr("apps.cap_note"),
                    ))
                    .into_any_element(),
            ),
        ))
        .children(processes::filter_row(state))
        .child(
            // Same pinned-header model as the Processes top-N card; the
            // height budget is shared because the chrome is identical.
            v_flex()
                .id("app-rows")
                .track_scroll(state.app_rows_scroll())
                .overflow_y_scroll()
                .max_h(px(processes::rows_height(state)))
                .children({
                    let repeated = repeated_names(rows.iter().map(|g| g.name.as_str()));
                    rows.into_iter().enumerate().map(move |(i, g)| {
                        let ambiguous = repeated.contains(g.name.as_str());
                        app_row(g, i + 1 == shown, state, ambiguous, sort, bar_full)
                    })
                })
                .when(no_match, |d| {
                    d.child(
                        div()
                            .px(px(13.))
                            .py(px(10.))
                            .child(widgets::note(i18n::tr("apps.filter_no_match"))),
                    )
                }),
        );

    let mut cards = Vec::with_capacity(2);
    if matches!(state.full_app_scan(), FullAppScan::Failed) {
        cards.push(widgets::empty_card(
            i18n::tr("apps.full_failed"),
            i18n::tr("apps.full_failed_body"),
        ));
    }
    cards.push(list_el.into_any_element());
    cards
}

fn full_scan_card(state: &ZStatsAppState, data: &FullAppScanData) -> AnyElement {
    let groups = data.groups.clone();
    // Owned, unlike the top list's borrowed set: this closure outlives
    // the frame that builds it, so it cannot hold a name that belongs to
    // the tick. Only repeated names allocate, which is normally none.
    let repeated: HashSet<String> = repeated_names(groups.iter().map(|g| g.name.as_str()))
        .into_iter()
        .map(str::to_string)
        .collect();
    let count = data.visible.len();
    let chrome = FULL_CHROME_HEIGHT
        + if state.proc_filter_open() {
            FILTER_ROW_HEIGHT
        } else {
            0.
        };
    let height =
        (super::body_height(state).unwrap_or(FULL_LIST_FALLBACK) - chrome).max(FULL_LIST_MIN);
    let sort = state.app_sort();
    let mut visible = data.visible.clone();
    sort_indices(&mut visible, &groups, sort);
    // Whole listing, not the filtered cut — meters must not rescale on
    // every keystroke, same as the process full-scan card.
    let bar_full = bar_full_for(sort, groups.iter());

    widgets::list_shell()
        .child(widgets::list_header(
            t!("apps.full_count", shown = count, total = data.total).to_string(),
            Some(
                h_flex()
                    .items_center()
                    .gap(px(5.))
                    .child(processes::filter_chip(state))
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
        .children(processes::filter_row(state))
        .when(count == 0, |d| {
            d.child(
                div()
                    .px(px(13.))
                    .py(px(10.))
                    .child(widgets::note(i18n::tr("apps.filter_no_match"))),
            )
        })
        .child(
            list(data.list.clone(), move |i, _window, cx| {
                let state = cx.global::<ZStatsGlobalStore>().read(cx);
                let g = &groups[visible[i]];
                let ambiguous = repeated.contains(g.name.as_str());
                app_row(g, i + 1 == count, state, ambiguous, sort, bar_full)
            })
            .h(px(height)),
        )
        .into_any_element()
}

fn full_scan_note(data: &FullAppScanData) -> String {
    t!(
        "apps.full_note",
        ms = data.window.as_millis(),
        age = format::ago(data.at.elapsed())
    )
    .to_string()
}

fn full_scan_chip(state: &ZStatsAppState) -> AnyElement {
    let scan = state.full_app_scan();
    let running = matches!(scan, FullAppScan::Running);
    let showing = matches!(scan, FullAppScan::Ready(_));
    let label = if running {
        i18n::tr("apps.full_scanning")
    } else if showing {
        i18n::tr("apps.full_back")
    } else {
        i18n::tr("apps.full_chip")
    };
    let tip = i18n::tr(if showing {
        "apps.full_back_tip"
    } else {
        "apps.full_chip_tip"
    });

    div()
        .id("full-app-scan")
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(if showing {
            theme::border()
        } else {
            theme::border_subtle()
        })
        .when(showing, |d| d.bg(theme::chip()))
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
                .update(cx, |state, cx| state.toggle_full_app_scan(cx));
        })
        .into_any_element()
}

fn app_row(
    g: &ProcessGroupSnapshot,
    is_last: bool,
    state: &ZStatsAppState,
    ambiguous: bool,
    sort: AppSort,
    bar_full: f32,
) -> AnyElement {
    let hot = g.cpu_usage_percent > HOT_PERCENT;
    let expanded = state.selected_app() == Some(g.root_pid);
    let root_pid = g.root_pid;

    let mut row = v_flex()
        .id(("app", root_pid as usize))
        .w_full()
        .px(px(13.))
        // Same rhythm as a process row, meter included — the two lists
        // used to disagree about whether a ranking has a trough.
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
                    ("app-name", root_pid as usize),
                    g.name.clone(),
                    12.,
                    gpui::FontWeight::MEDIUM,
                    Hsla::from(theme::text()),
                ))
                .child(
                    div()
                        .flex_none()
                        .font_family(font::MONO)
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::text_for(hot))
                        .child(format::pct_col(g.cpu_usage_percent)),
                ),
        )
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .mt(px(3.))
                .text_size(px(10.))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme::text_dim())
                        .child(tree_meta(g, ambiguous)),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(font::MONO)
                        .text_size(px(9.5))
                        .text_color(theme::text_muted())
                        .child(io_mem_line(
                            shown_memory(g),
                            g.read_bytes_per_sec,
                            g.write_bytes_per_sec,
                        )),
                ),
        )
        .child(div().mt(px(4.)).child(widgets::meter(
            bar_fraction(sort, g, bar_full),
            Hsla::from(theme::fill_for(hot)),
            3.,
        )));

    if expanded {
        row = row.child(expand_block(g, state));
    }

    row.on_click(move |_, _window, cx| {
        cx.global::<ZStatsGlobalStore>()
            .clone()
            .update(cx, |state, cx| state.toggle_app(root_pid, cx));
    })
    .into_any_element()
}

/// Identity + the two memory figures, then the members the row total
/// is the sum of. Hovering a giant tooltip over those figures (the
/// previous layout) hid the numbers it was trying to explain.
fn expand_block(g: &ProcessGroupSnapshot, state: &ZStatsAppState) -> AnyElement {
    let tick_ps = state
        .latest()
        .and_then(|t| t.snapshot.processes.as_deref().map(Vec::as_slice))
        .unwrap_or(&[]);
    let source = state.member_processes().unwrap_or(tick_ps);
    let mut members = fullscan::tree_members(g.root_pid, source);
    members.sort_by_key(|p| Reverse(snap_memory(p)));
    let expected = g.process_count as usize;
    let fetching = state.member_table_running() && members.len() < expected;
    let missing = expected.saturating_sub(members.len());
    let live_by_pid: HashMap<u32, &ProcessSnapshot> = tick_ps.iter().map(|p| (p.pid, p)).collect();
    let io = match (g.read_bytes_per_sec, g.write_bytes_per_sec) {
        (None, None) => None,
        (r, w) => Some(format!("R {} · W {}", format::rate(r), format::rate(w))),
    };

    v_flex()
        .id(("app-expand", g.root_pid as usize))
        .mt(px(8.))
        .p(px(10.))
        .rounded(px(8.))
        .bg(theme::inset())
        // The parent row toggles expansion; a click inside the inset
        // (ⓘ, a member name) must not fold it.
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            h_flex()
                .gap(px(14.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(expand_row(
                            i18n::tr("apps.root_pid"),
                            expand_value(g.root_pid.to_string()),
                            false,
                        ))
                        .child(expand_row(
                            i18n::tr("processes.mem_footprint"),
                            expand_value(
                                g.phys_footprint_bytes
                                    .map_or(format::PLACEHOLDER.into(), format::memory),
                            ),
                            true,
                        )),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(expand_row(
                            i18n::tr("apps.cpu"),
                            expand_value(format::pct(g.cpu_usage_percent)),
                            false,
                        ))
                        .child(expand_row(
                            i18n::tr("processes.mem_rss"),
                            expand_value(format::memory(g.memory_bytes)),
                            true,
                        )),
                ),
        )
        .when_some(io, |d, text| {
            d.child(
                div()
                    .mt(px(6.))
                    .font_family(font::MONO)
                    .text_size(px(10.))
                    .text_color(theme::text_dim())
                    .child(text),
            )
        })
        .child(
            h_flex()
                .items_center()
                .gap(px(4.))
                .mt(px(8.))
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(theme::text_dim())
                        .child(i18n::tr("apps.members_section")),
                )
                .child(members_info(g.root_pid)),
        )
        .children(members.iter().enumerate().map(|(i, p)| {
            let live = live_by_pid.get(&p.pid).copied();
            let shown = live.unwrap_or(p);
            member_row(
                shown,
                live.map(|l| l.cpu_usage_percent),
                i + 1 == members.len() && missing == 0 && !fetching,
            )
        }))
        .when(members.is_empty() && !fetching, |d| {
            d.child(
                div()
                    .mt(px(4.))
                    .child(widgets::note(i18n::tr("apps.members_none"))),
            )
        })
        .when(fetching, |d| {
            d.child(
                div()
                    .mt(px(4.))
                    .child(widgets::note(i18n::tr("apps.members_loading"))),
            )
        })
        .when(missing > 0 && !members.is_empty() && !fetching, |d| {
            d.child(div().mt(px(4.)).child(widgets::note(
                t!("apps.members_missing", n = missing).to_string(),
            )))
        })
        .when(terminate::can_quit_app(g.root_pid), |d| {
            d.child(
                h_flex()
                    .justify_end()
                    .child(quit_button(g.root_pid, g.name.clone())),
            )
        })
        .into_any_element()
}

fn quit_button(pid: u32, name: String) -> AnyElement {
    let label = i18n::tr("apps.quit_ok");
    div()
        .id(("app-quit", pid as usize))
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
            let name = name.clone();
            confirm::ask(
                window,
                cx,
                t!("apps.quit_title", name = name.clone()).to_string(),
                t!("apps.quit_body", name = name).to_string(),
                i18n::tr("apps.quit_ok"),
                move |_| {
                    if !terminate::request_quit(pid) {
                        eprintln!("quit request for pid {pid} was not delivered");
                    }
                },
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

fn expand_row(label: String, value: AnyElement, last: bool) -> AnyElement {
    h_flex()
        .justify_between()
        .gap(px(8.))
        .py(px(5.))
        .when(!last, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .text_size(px(11.))
        .text_color(theme::text_muted())
        .child(div().flex_none().child(label))
        .child(value)
        .into_any_element()
}

fn expand_value(text: impl Into<gpui::SharedString>) -> AnyElement {
    let text = text.into();
    h_flex()
        .flex_1()
        .min_w_0()
        .justify_end()
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_family(font::MONO)
                .text_color(theme::text())
                .child(text),
        )
        .into_any_element()
}

fn members_info(root_pid: u32) -> AnyElement {
    div()
        .id(("app-members", root_pid as usize))
        .flex_none()
        .tooltip(widgets::wrap_tooltip(i18n::tr("apps.members_lead")))
        .child(
            Icon::new(IconName::Info)
                .with_size(Size::Size(px(11.)))
                .text_color(Hsla::from(theme::text_dim())),
        )
        .into_any_element()
}

fn member_row(p: &ProcessSnapshot, cpu: Option<f32>, last: bool) -> AnyElement {
    h_flex()
        .items_baseline()
        .justify_between()
        .gap(px(8.))
        .mt(px(4.))
        .py(px(3.))
        .when(!last, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .child(widgets::truncating_name(
            ("app-member", p.pid as usize),
            p.name.clone(),
            11.,
            gpui::FontWeight::MEDIUM,
            Hsla::from(theme::text()),
        ))
        .child(
            h_flex()
                .flex_none()
                .gap(px(8.))
                .font_family(font::MONO)
                .text_size(px(10.))
                .text_color(theme::text_muted())
                .child(
                    cpu.map(format::pct)
                        .unwrap_or_else(|| format::PLACEHOLDER.to_string()),
                )
                .child(format::memory(snap_memory(p))),
        )
        .into_any_element()
}

fn snap_memory(p: &ProcessSnapshot) -> u64 {
    p.phys_footprint_bytes.unwrap_or(p.memory_bytes)
}

/// Cycles CPU ↔ memory. Name order is omitted on purpose: fifty trees
/// are still findable with the filter, and a third stop would spend
/// width the header does not have (filter + All + ⓘ already sit there).
fn sort_control(state: &ZStatsAppState) -> AnyElement {
    let sort = state.app_sort();
    let full = matches!(state.full_app_scan(), FullAppScan::Ready(_));
    let tip = i18n::tr(if full {
        sort.full_tip_key()
    } else {
        sort.tip_key()
    });
    div()
        .id("sort-apps")
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
                .update(cx, |state, cx| state.cycle_app_sort(cx));
        })
        .into_any_element()
}

/// What a full track means. Memory has no one-core equivalent, so the
/// largest tree on this page is full; CPU keeps the process list's floor
/// so a quiet page does not stretch 8% across the trough.
fn bar_full_for<'a>(sort: AppSort, groups: impl Iterator<Item = &'a ProcessGroupSnapshot>) -> f32 {
    match sort {
        AppSort::Cpu => groups
            .map(|g| g.cpu_usage_percent)
            .fold(0.0, f32::max)
            .max(processes::BAR_FLOOR_PERCENT),
        AppSort::Memory => groups.map(shown_memory).max().unwrap_or(0) as f32,
    }
}

fn bar_fraction(sort: AppSort, g: &ProcessGroupSnapshot, full: f32) -> f32 {
    let full = full.max(1.0);
    match sort {
        AppSort::Cpu => g.cpu_usage_percent / full,
        AppSort::Memory => shown_memory(g) as f32 / full,
    }
}

fn sort_indices(indices: &mut [usize], groups: &[ProcessGroupSnapshot], sort: AppSort) {
    match sort {
        AppSort::Cpu => indices.sort_by(|&a, &b| {
            groups[b]
                .cpu_usage_percent
                .total_cmp(&groups[a].cpu_usage_percent)
        }),
        AppSort::Memory => indices.sort_by_key(|&i| Reverse(shown_memory(&groups[i]))),
    }
}

/// The memory figure a group row shows: the summed physical footprint
/// when zstats could take one, its summed RSS otherwise.
///
/// The same rule and the same fallback as `processes::shown_memory`,
/// deliberately — one program appearing in both tabs must be measured
/// the same way. RSS alone understates a GUI tree badly: compressed
/// pages and GPU/IOKit allocations are invisible to it, which is the
/// whole reason the process rows moved to footprint. zstats sums member
/// footprints over the FULL table (a member the kernel refused is
/// counted by its RSS), so the total is never partial and never worse
/// than the resident sum it replaces.
fn shown_memory(g: &ProcessGroupSnapshot) -> u64 {
    g.phys_footprint_bytes.unwrap_or(g.memory_bytes)
}

/// The row's left-hand facts: how many processes the tree holds, and the
/// pid at its root.
///
/// The count used to wear an outlined pill — the same chrome as the
/// header's All chip, on something that does not click, repeated down
/// every row. It is plain text now, like the pid line a process row
/// carries in the same slot.
///
/// The pid is here because names repeat: two `login` trees are two
/// terminal sessions, and with nothing to tell them apart the list reads
/// as if it rendered a row twice.
fn tree_meta(g: &ProcessGroupSnapshot, ambiguous: bool) -> String {
    let count = if g.process_count == 1 {
        i18n::tr("apps.one_process")
    } else {
        t!("apps.n_processes", count = g.process_count).to_string()
    };
    if !ambiguous {
        return count;
    }
    format!("{count} · {}", t!("processes.pid_only", pid = g.root_pid))
}

/// Which names this listing shows more than once. Only those rows spend
/// width on a pid: at 320px a busy row already carries
/// `R 253 kB/s · W 116 kB/s · 1.9 GB` on the right, and an unconditional
/// pid would push the left half into an ellipsis — truncating the very
/// digits it was added to disambiguate.
fn repeated_names<'a>(names: impl Iterator<Item = &'a str>) -> HashSet<&'a str> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut twice: HashSet<&str> = HashSet::new();
    for name in names {
        if !seen.insert(name) {
            twice.insert(name);
        }
    }
    twice
}

/// Memory **last**, so it lands on the row's right edge in every row and
/// forms a column the eye can run down. With it first — as it was — the
/// IO half is what aligns, and IO is the part that comes and goes, so
/// the memory figure moved horizontally from row to row and could not be
/// compared at all.
fn io_mem_line(memory: u64, read: Option<u64>, write: Option<u64>) -> String {
    match io_line(read, write) {
        Some(io) => format!("{io} · {}", format::memory(memory)),
        None => format::memory(memory),
    }
}

fn io_line(read: Option<u64>, write: Option<u64>) -> Option<String> {
    // Zero draws the same nothing as "not collected": most trees idle at
    // R 0 B/s · W 0 B/s, and repeating that on every row is ink without
    // information. The expanded detail keeps the explicit numbers.
    if read.unwrap_or(0) == 0 && write.unwrap_or(0) == 0 {
        return None;
    }
    Some(format!(
        "R {} · W {}",
        format::rate(read),
        format::rate(write)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One program in two tabs must be measured one way. This mirrors
    /// `processes::shown_memory_prefers_footprint_and_falls_back_to_rss`
    /// exactly — if either rule ever changes alone, a browser reads two
    /// different sizes on two pages of the same panel.
    #[test]
    fn shown_memory_prefers_footprint_and_falls_back_to_rss() {
        let mut g = ProcessGroupSnapshot {
            root_pid: 1,
            name: "Google Chrome".into(),
            process_count: 37,
            cpu_usage_percent: 0.0,
            memory_bytes: 80,
            phys_footprint_bytes: None,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        };
        assert_eq!(shown_memory(&g), 80, "no footprint → the resident sum");
        g.phys_footprint_bytes = Some(300);
        assert_eq!(shown_memory(&g), 300, "footprint wins where there is one");
    }

    /// Two `login` trees are two terminal sessions; without the pid the
    /// list reads as if it rendered a row twice. A unique name gets none
    /// — the row has no width to spare for a disambiguator that
    /// disambiguates nothing.
    #[test]
    fn only_a_repeated_name_earns_a_pid() {
        let repeated = repeated_names(["login", "Finder", "login", "WeChat"].into_iter());
        assert!(repeated.contains("login"));
        assert!(!repeated.contains("Finder"));
        assert_eq!(repeated.len(), 1, "only the one that actually repeats");

        let g = ProcessGroupSnapshot {
            root_pid: 4321,
            name: "login".into(),
            process_count: 4,
            cpu_usage_percent: 0.0,
            memory_bytes: 0,
            phys_footprint_bytes: None,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        };
        // Asserted on the separator this function owns, not on
        // translated text: the locale files are not loaded here, and a
        // test that reads one would be testing rust-i18n's fallback.
        assert!(
            !tree_meta(&g, false).contains(" · "),
            "a unique name carries the count alone"
        );
        assert!(
            tree_meta(&g, true).contains(" · "),
            "a repeated one gains a second fact"
        );
    }

    #[test]
    fn io_line_stays_off_when_uncollected_or_idle() {
        assert_eq!(io_line(None, None), None);
        // Measured zero draws nothing on the row either — most trees idle,
        // and "R 0 B/s · W 0 B/s" on each was ink without information.
        assert_eq!(io_line(Some(0), Some(0)), None);
        assert_eq!(io_line(Some(0), None), None);
        // One live side brings the whole line back, zeros included.
        assert_eq!(
            io_line(Some(2048), Some(0)),
            Some("R 2 kB/s · W 0 B/s".into())
        );
        assert_eq!(io_mem_line(98 * 1024 * 1024, None, None), "98 MB");
        assert_eq!(io_mem_line(98 * 1024 * 1024, Some(0), Some(0)), "98 MB");
        // Memory last, always — that is what gives the column an edge.
        assert!(
            io_mem_line(98 * 1024 * 1024, Some(2048), Some(0)).ends_with("98 MB"),
            "memory has to be the rightmost figure"
        );
    }

    fn group(name: &str, cpu: f32, mem: u64) -> ProcessGroupSnapshot {
        ProcessGroupSnapshot {
            root_pid: 1,
            name: name.into(),
            process_count: 1,
            cpu_usage_percent: cpu,
            memory_bytes: mem,
            phys_footprint_bytes: None,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    #[test]
    fn the_meter_follows_the_sort_and_the_full_list_reorders() {
        let groups = vec![
            group("zed", 2.0, 300),
            group("Chrome", 40.0, 80),
            group("ice", 1.0, 900),
        ];
        let mut idx = vec![0, 1, 2];
        sort_indices(&mut idx, &groups, AppSort::Cpu);
        assert_eq!(idx, vec![1, 0, 2]);
        sort_indices(&mut idx, &groups, AppSort::Memory);
        assert_eq!(idx, vec![2, 0, 1]);

        let cpu_full = bar_full_for(AppSort::Cpu, groups.iter());
        assert_eq!(
            cpu_full,
            processes::BAR_FLOOR_PERCENT,
            "40% must not get to define full"
        );
        assert_eq!(
            bar_fraction(AppSort::Cpu, &groups[1], cpu_full),
            40.0 / processes::BAR_FLOOR_PERCENT
        );
        let hot = group("compile", 210.0, 10);
        let busy = [hot.clone(), groups[0].clone()];
        let busy_full = bar_full_for(AppSort::Cpu, busy.iter());
        assert_eq!(
            bar_fraction(AppSort::Cpu, &hot, busy_full),
            1.0,
            "over a core, the hottest fills"
        );
        let mem_full = bar_full_for(AppSort::Memory, groups.iter());
        assert_eq!(
            bar_fraction(AppSort::Memory, &groups[2], mem_full),
            1.0,
            "largest fills it"
        );
        assert_eq!(
            bar_fraction(AppSort::Memory, &groups[0], mem_full),
            300.0 / 900.0
        );
    }
}
