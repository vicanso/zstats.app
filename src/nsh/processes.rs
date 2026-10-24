//! Processes on NSH: the gpui front end's `views/processes.rs`, whose
//! comments say why every row, figure and control is the way it is — this
//! file carries the same choices and does not repeat their reasons. The
//! page's rules are shared (`proclist.rs`); what it has in common with
//! Apps is `lists.rs`.
//!
//! The collector's top list ranked by the 60s rolling average, under a
//! pinned header (the count, the filter, the window, the abnormal filter
//! while something is abnormal, the sort and the All chip): processes
//! stuck in a state that should not persist above the ranking, then the
//! rows, in a region that ends on a whole row at the gap above the
//! footer. A row is kept by its pid (`each`); the open row carries its
//! expansion — the command, the figures two-up, the two alert bars and
//! the chips that edit them, and Quit (SIGTERM). The All chip swaps the
//! list for a one-off scan of the whole table, a virtual list.

use crate::alertbars::BarDisplay;
use crate::nsh::lists::{self, Editor, ROW_PAD, ScanChip, ScanWords};
use crate::nsh::root::{CARD_GAP, CARD_PAD, Live};
use crate::nsh::widgets::{self, CLEAR};
use crate::nsh::{store, theme};
use crate::proclist::{self, BarMeasure, BarScale};
use crate::procscan::ProcState;
use crate::rows::{self, RowFit};
use crate::state::{FullScan, ProcSort, ZStatsAppState};
use crate::{format, hot, i18n, palette, terminate};
use nsh_core::ui::prelude::*;
use rust_i18n::t;
use std::collections::HashMap;
use std::process;
use zstats::snapshot::ProcessSnapshot;

/// A collapsed row, hairline included: the padding twice, the 12px
/// first line's 19, the 2 under it, the 10px second line's 16, the
/// meter's 4 and 3, the hairline (`processes::COLLAPSED_ROW`, whose
/// lines are φ's fractions where NSH's are whole points).
const COLLAPSED_ROW: f32 = ROW_PAD + ROW_PAD + 19. + 2. + 16. + 4. + 3. + 1.;

/// An abnormal process's row, hairline included: 8 above and below the
/// 11px name's 18, the 2 under it and the 9px line's 15.
const ABNORMAL_ROW: f32 = 8. + 18. + 2. + 15. + 8. + 1.;

/// The All chip's words.
const SCAN_WORDS: ScanWords = ScanWords {
    chip: "processes.full_chip",
    scanning: "processes.full_scanning",
    back: "processes.full_back",
    chip_tip: "processes.full_chip_tip",
    back_tip: "processes.full_back_tip",
};

/// The two rules a process's chips edit.
const KEYS: [&str; 2] = ["alert-cpu", "alert-mem"];

/// The page: waiting, collection off, the top list, or the full scan.
#[derive(Clone, Copy, PartialEq)]
enum Shown {
    Waiting,
    Off,
    Top,
    Full,
}

fn shown(state: &ZStatsAppState) -> Shown {
    if matches!(state.full_scan(), FullScan::Ready(_)) {
        return Shown::Full;
    }
    match state.latest() {
        None => Shown::Waiting,
        Some(tick) if tick.snapshot.processes.is_none() => Shown::Off,
        Some(_) => Shown::Top,
    }
}

/// The Processes page; `body` is the height it has on screen.
pub fn page(live: Live, body: ReadSignal<f32>) -> AnyElement {
    let shown = live.memo(shown);
    let failed = live.memo(|state: &ZStatsAppState| matches!(state.full_scan(), FullScan::Failed));
    dynamic(move || match shown.get() {
        Shown::Waiting => widgets::empty_card(
            i18n::tr("processes.title"),
            i18n::tr("common.waiting_sample"),
        ),
        Shown::Off => {
            widgets::empty_card(i18n::tr("processes.off"), i18n::tr("processes.off_body"))
        }
        Shown::Top => view()
            .gap(px(CARD_GAP))
            .child(when(
                move || failed.get(),
                || {
                    widgets::empty_card(
                        i18n::tr("processes.full_failed"),
                        i18n::tr("processes.full_failed_body"),
                    )
                },
            ))
            .child(top_card(live, body))
            .into_element(),
        Shown::Full => full_card(live, body).into_element(),
    })
}

/// One row as it is drawn.
#[derive(Clone, PartialEq)]
struct Row {
    pid: u32,
    name: String,
    pct: String,
    hot: bool,
    /// The pid, and the status when it is one worth a word.
    meta: String,
    notable: bool,
    /// How long it has held a low steady load (`watch.rs`).
    sustained: Option<String>,
    /// What the list is sorted by, where the meter alone cannot say it:
    /// the memory, or the disk's read and write.
    figure: String,
    fraction: f32,
    open: bool,
    first: bool,
    pad: f32,
}

fn row_of(
    p: &ProcessSnapshot,
    cpu: f64,
    bar: BarScale,
    state: &ZStatsAppState,
    index: usize,
    fit: Option<RowFit>,
) -> Row {
    let sustained = state.sustained_load(p.pid);
    let notable = format::notable_status(&p.status);
    Row {
        pid: p.pid,
        name: p.name.clone(),
        pct: format::pct_col(cpu as f32),
        hot: hot::is_hot(cpu, sustained.is_some()) && !hot::own_check_cpu(p.pid, state),
        meta: if notable {
            t!(
                "processes.pid_status",
                pid = p.pid,
                status = p.status.clone()
            )
            .to_string()
        } else {
            t!("processes.pid_only", pid = p.pid).to_string()
        },
        notable,
        sustained: sustained.map(|dur| {
            t!(
                "processes.sustained",
                duration = format::uptime(dur.as_secs())
            )
            .to_string()
        }),
        figure: match bar.measure {
            BarMeasure::DiskIo => {
                proclist::disk_io_display(p.read_bytes_per_sec, p.write_bytes_per_sec)
            }
            _ => format::memory(proclist::shown_memory(p)),
        },
        fraction: bar.fraction(p, cpu),
        open: state.selected_pid() == Some(p.pid),
        first: index == 0,
        pad: fit.map_or(ROW_PAD, |fit| fit.pad(index)),
    }
}

/// A process stuck in a state that should not persist.
#[derive(Clone, PartialEq)]
struct Abnormal {
    pid: u32,
    name: String,
    state: &'static str,
    tip: String,
    meta: String,
    observed: Option<String>,
    last: bool,
}

/// The top list's region and what it holds.
#[derive(Clone, PartialEq, Default)]
struct Top {
    title: String,
    abnormal: Vec<Abnormal>,
    only_abnormal: bool,
    rows: Vec<Row>,
    viewport: f32,
    /// Where the open row starts in the region, when it is in it.
    open_at: Option<f32>,
    no_match: bool,
}

fn abnormal_of(state: &ZStatsAppState, more_follows: bool) -> Vec<Abnormal> {
    let found = state.abnormal();
    let total = found.len();
    found
        .iter()
        .enumerate()
        .map(|(i, p)| {
            // "zstats (60337)" when the scan could name the parent, the
            // bare pid when it could not — never a guess.
            let parent = match &p.parent_name {
                Some(name) => format!("{name} ({})", p.parent_pid),
                None => p.parent_pid.to_string(),
            };
            let meta = t!(
                "processes.abnormal_meta",
                ppid = parent,
                age = format::uptime(p.age.as_secs())
            )
            .to_string();
            let observed = state.abnormal_observed(p.pid).map(|d| {
                t!(
                    "processes.abnormal_observed",
                    observed = format!("≥{}", format::uptime(d.as_secs()))
                )
                .to_string()
            });
            let hint = i18n::tr(match p.state {
                ProcState::Zombie => "processes.zombie_hint",
                ProcState::Stopped => "processes.stopped_hint",
            });
            Abnormal {
                pid: p.pid,
                name: p.name.clone(),
                state: p.state.label(),
                tip: match &observed {
                    Some(seen) => format!("{hint}\n{meta} · {seen}"),
                    None => format!("{hint}\n{meta}"),
                },
                meta,
                observed,
                last: !more_follows && i + 1 == total,
            }
        })
        .collect()
}

fn top_of(state: &ZStatsAppState, body: f32) -> Top {
    let Some(tick) = state.latest() else {
        return Top::default();
    };
    let Some(mut ranked) = proclist::ranked(tick) else {
        return Top::default();
    };
    proclist::order(state, &mut ranked);
    let only_abnormal = state.only_abnormal();
    let abnormal = abnormal_of(state, !only_abnormal && !ranked.is_empty());
    let total = tick
        .snapshot
        .total_processes
        .map(|t| t.to_string())
        .unwrap_or_else(|| format::PLACEHOLDER.to_string());
    let bar = BarScale::new(
        state.proc_sort(),
        ranked
            .iter()
            .map(|(_, cpu)| *cpu as f32)
            .fold(0.0, f32::max),
        ranked
            .iter()
            .map(|(p, _)| proclist::shown_memory(p))
            .max()
            .unwrap_or(0),
        ranked
            .iter()
            .map(|(p, _)| proclist::disk_io_bytes(p))
            .max()
            .unwrap_or(0),
    );
    let fit = rows::fit_rows(lists::rows_budget(state, body), COLLAPSED_ROW, ROW_PAD);
    let rows: Vec<Row> = if only_abnormal {
        Vec::new()
    } else {
        ranked
            .iter()
            .enumerate()
            .map(|(i, (p, cpu))| row_of(p, *cpu, bar, state, i, Some(fit)))
            .collect()
    };
    let open_at = rows.iter().position(|r| r.open).map(|ix| {
        abnormal.len() as f32 * ABNORMAL_ROW + (0..ix).map(|i| fit.height(i)).sum::<f32>()
    });
    Top {
        title: if only_abnormal {
            t!("processes.abnormal_only", count = abnormal.len()).to_string()
        } else {
            t!("processes.count_of", shown = ranked.len(), total = total).to_string()
        },
        no_match: !state.proc_filter_text().is_empty() && ranked.is_empty() && !only_abnormal,
        abnormal,
        only_abnormal,
        rows,
        viewport: fit.viewport,
        open_at,
    }
}

/// The header's controls: the filter and the window while the ranking is
/// on screen, the abnormal filter while something is abnormal, the sort
/// and the All chip.
fn controls(live: Live, full: bool) -> AnyElement {
    let only = live.memo(ZStatsAppState::only_abnormal);
    let abnormal = live.memo(|state: &ZStatsAppState| state.abnormal().len());
    let ranking = move || full || !only.get();
    view()
        .flex_row()
        .items_center()
        .gap(px(5.))
        .child(when(ranking, move || lists::filter_chip(live)))
        .child(when(move || !full && !only.get(), avg_chip))
        .child(when(
            move || !full && abnormal.get() > 0,
            move || abnormal_filter(only, abnormal),
        ))
        .child(when(ranking, move || sort_control(live)))
        .child(when(ranking, move || full_scan_chip(live)))
        .into_element()
}

fn top_card(live: Live, body: ReadSignal<f32>) -> View {
    let top = live.watch(move || {
        let body = body.get();
        store::read(|state| top_of(state, body))
    });
    // A jump from an alert or History puts the open row in view, once, as
    // on Apps: asked for when the page is made, done once the region is
    // laid out.
    let region = element_ref();
    let reveal = signal(None::<f32>);
    effect(move || {
        live.track();
        if store::read(ZStatsAppState::take_proc_reveal) {
            reveal.set(top.with_untracked(|t| t.open_at));
        }
    });
    widgets::list_shell()
        .child(widgets::list_header(
            lists::count_title(
                move || top.with(|t| t.title.clone()),
                widgets::wrap_tooltip(i18n::tr("processes.cpu_basis_tip")),
            ),
            Some(controls(live, false)),
        ))
        .child(lists::filter_row(live))
        .child(
            scroll()
                .max_h(move || px(top.with(|t| t.viewport)))
                .element_ref(&region)
                .on_layout(move |_: &Bounds<Points>| {
                    if let Some(at) = reveal.get_untracked() {
                        reveal.set(None);
                        region.scroll_to(point(px(0.), px(at)), false);
                    }
                })
                .child(each(
                    move || top.with(|t| t.abnormal.clone()),
                    |a: &Abnormal| a.pid,
                    |a, _| abnormal_row(a),
                ))
                .child(each(
                    move || top.with(|t| t.rows.clone()),
                    |row: &Row| row.pid,
                    move |row, _| process_row(computed(move || row.get()), live, false),
                ))
                .child(when(
                    move || top.with(|t| t.no_match),
                    || lists::no_match("processes.filter_no_match"),
                )),
        )
}

/// The full scan: the whole table, filtered and sorted.
#[derive(Clone, PartialEq, Default)]
struct Full {
    title: String,
    note: String,
    rows: Vec<Row>,
}

fn full_of(state: &ZStatsAppState) -> Full {
    let FullScan::Ready(data) = state.full_scan() else {
        return Full::default();
    };
    let processes = &data.processes;
    let sort = state.proc_sort();
    let mut visible = data.visible.clone();
    proclist::sort_indices(&mut visible, processes, sort);
    // Over the whole listing rather than the filtered cut, so the meters
    // do not rescale on every keystroke.
    let bar = BarScale::new(
        sort,
        processes
            .iter()
            .map(|p| p.cpu_usage_percent)
            .fold(0.0, f32::max),
        processes
            .iter()
            .map(proclist::shown_memory)
            .max()
            .unwrap_or(0),
        processes
            .iter()
            .map(proclist::disk_io_bytes)
            .max()
            .unwrap_or(0),
    );
    Full {
        title: t!(
            "processes.full_count",
            shown = visible.len(),
            total = data.total
        )
        .to_string(),
        note: t!(
            "processes.full_note",
            ms = data.window.as_millis(),
            age = format::ago(data.at.elapsed())
        )
        .to_string(),
        rows: visible
            .iter()
            .enumerate()
            .map(|(i, &ix)| {
                let p = &processes[ix];
                row_of(p, f64::from(p.cpu_usage_percent), bar, state, i, None)
            })
            .collect(),
    }
}

/// The All chip's listing: the card fills the page and the list takes
/// what the header, the note and the filter leave.
fn full_card(live: Live, body: ReadSignal<f32>) -> View {
    let full = live.memo(full_of);
    let list = element_ref();
    let reveal = signal(None::<usize>);
    effect(move || {
        live.track();
        if store::read(ZStatsAppState::take_proc_reveal) {
            reveal.set(full.with_untracked(|f| f.rows.iter().position(|r| r.open)));
        }
    });
    widgets::list_shell()
        .h(move || px((body.get() - CARD_GAP).max(lists::LIST_MIN)))
        .child(widgets::list_header(
            widgets::list_title(move || full.with(|f| f.title.clone())).into_element(),
            Some(controls(live, true)),
        ))
        .child(
            view()
                .px(px(CARD_PAD))
                .pb(px(8.))
                .child(widgets::note(move || full.with(|f| f.note.clone()))),
        )
        .child(lists::filter_row(live))
        .child(when(
            move || full.with(|f| f.rows.is_empty()),
            || lists::no_match("processes.filter_no_match"),
        ))
        .child(
            virtual_list(
                move || full.with(|f| f.rows.len()),
                move |at: Position| {
                    let row = computed(move || {
                        full.with(|f| f.rows.get(at.index()).cloned())
                            .unwrap_or_else(|| empty_row(at.index()))
                    });
                    process_row(row, live, true)
                },
            )
            .keys(move || full.with(|f| f.rows.iter().map(|r| r.pid).collect::<Vec<_>>()))
            .estimated_item_size(px(COLLAPSED_ROW))
            .element_ref(&list)
            .on_layout(move |_: &Bounds<Points>| {
                if let Some(ix) = reveal.get_untracked() {
                    reveal.set(None);
                    list.scroll_to_index(ix as u32, Percentage(0.), px(0.), false);
                }
            })
            .flex_1(),
        )
}

/// A cell the list asked for past the rows it has, for the moment before
/// it hears the count changed.
fn empty_row(index: usize) -> Row {
    Row {
        pid: 0,
        name: String::new(),
        pct: String::new(),
        hot: false,
        meta: String::new(),
        notable: false,
        sustained: None,
        figure: String::new(),
        fraction: 0.,
        open: false,
        first: index == 0,
        pad: ROW_PAD,
    }
}

/// "60s avg": the window the figures are averaged over, in words.
fn avg_chip() -> View {
    view()
        .flex_none()
        .tooltip(widgets::wrap_tooltip(i18n::tr("processes.avg_tip")))
        .child(
            text(i18n::tr("processes.avg_window"))
                .rounded(px(4.))
                .px(px(5.))
                .py(px(1.))
                .text_size(px(9.))
                .line_height(widgets::line(9.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(|| palette::tiny_label(palette::text_muted()))),
        )
}

/// The filter to the abnormal processes alone, shown only while there
/// are any.
fn abnormal_filter(only: Memo<bool>, count: Memo<usize>) -> AnyElement {
    nsh_base::button()
        .flex_none()
        .rounded_full()
        .border(px(1.))
        .border_color(theme::color(|| palette::accent_wash(45)))
        .bg(move || theme::color(move || if only.get() { palette::accent() } else { CLEAR }))
        .hover(move |s| {
            if only.get_untracked() {
                s
            } else {
                s.bg(theme::color(|| palette::accent_wash(14)))
            }
        })
        .px(px(7.))
        .py(px(1.))
        .on_press(|| store::update(|state, fx| state.toggle_only_abnormal(fx)))
        .child(
            text(move || t!("processes.abnormal_filter", count = count.get()).to_string())
                .text_size(px(9.))
                .line_height(widgets::line(9.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || {
                    theme::color(move || {
                        if only.get() {
                            crate::color::rgba(0xffffffff)
                        } else {
                            palette::accent_light()
                        }
                    })
                }),
        )
        .into_element()
}

/// The ordering, as a menu.
fn sort_control(live: Live) -> AnyElement {
    let sort = live.memo(ZStatsAppState::proc_sort);
    let full = live.memo(|state: &ZStatsAppState| matches!(state.full_scan(), FullScan::Ready(_)));
    let choices = ProcSort::ALL
        .iter()
        .map(|&order| widgets::Choice {
            label: i18n::tr(order.label_key()),
            on: Box::new(move || sort.get() == order),
            choose: Box::new(move || store::update(|state, fx| state.set_proc_sort(order, fx))),
        })
        .collect();
    widgets::menu_trigger(
        move || format!("↓ {}", i18n::tr(sort.get().label_key())),
        move || {
            let sort = sort.get();
            i18n::tr(if full.get() {
                sort.full_tip_key()
            } else {
                sort.tip_key()
            })
        },
        choices,
    )
}

fn full_scan_chip(live: Live) -> AnyElement {
    let chip = live.memo(|state: &ZStatsAppState| match state.full_scan() {
        FullScan::Running => ScanChip::Running,
        FullScan::Ready(_) => ScanChip::Showing,
        _ => ScanChip::Off,
    });
    lists::full_scan_chip(chip, SCAN_WORDS, || {
        store::update(|state, fx| state.toggle_full_scan(fx))
    })
}

/// A stuck process, above the ranking: its name and state with the ⓘ
/// that says what it means, then its parent and age.
fn abnormal_row(a: ReadSignal<Abnormal>) -> AnyElement {
    view()
        .px(px(12.))
        .py(px(8.))
        .bg(theme::color(|| palette::accent_wash(7)))
        .border_b(move || px(if a.with(|a| a.last) { 0. } else { 1. }))
        .border_color(theme::color(palette::border_subtle))
        .child(
            view()
                .flex_row()
                .items_baseline()
                .justify_between()
                .gap(px(8.))
                .child(
                    text(move || a.with(|a| a.name.clone()))
                        .flex_1()
                        .min_w(px(0.))
                        .number_of_lines(1u32)
                        .text_size(px(11.))
                        .line_height(widgets::line(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::color(palette::text)),
                )
                .child(
                    view()
                        .flex_row()
                        .flex_none()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            text(move || a.with(|a| a.state))
                                .text_size(px(10.))
                                .line_height(widgets::line(10.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::color(palette::accent_light)),
                        )
                        .child(widgets::info_icon(widgets::live_tooltip(move || {
                            a.with(|a| a.tip.clone())
                        }))),
                ),
        )
        .child(
            view()
                .flex_row()
                .justify_between()
                .items_center()
                .gap(px(8.))
                .mt(px(2.))
                .child(abnormal_note(move || a.with(|a| a.meta.clone())))
                .child(when(
                    move || a.with(|a| a.observed.is_some()),
                    move || {
                        abnormal_note(move || a.with(|a| a.observed.clone().unwrap_or_default()))
                    },
                )),
        )
        .into_element()
}

fn abnormal_note(words: impl Fn() -> String + 'static) -> Text {
    text(words)
        .text_size(px(9.))
        .line_height(widgets::line(9.))
        .text_color(theme::color(|| palette::tiny_label(palette::text_dim())))
}

/// One process: the icon of the app it runs from beside its name and
/// figure, the second line, the meter; open, its expansion under them.
/// The collapsed part toggles it. `full` says which list it is in, for
/// the expansion's lookup.
fn process_row(row: Memo<Row>, live: Live, full: bool) -> AnyElement {
    let open = computed(move || row.with(|r| r.open));
    let pid = computed(move || row.with(|r| r.pid));
    let ident = computed(move || row.with(|r| (r.pid, r.name.clone())));
    let badge = live.memo(lists::disk_check_label);
    let own = move || pid.get() == process::id();
    let pad = move || row.with(|r| r.pad);
    view()
        .bg(move || {
            theme::color(move || {
                if open.get() {
                    palette::surface_raised()
                } else {
                    CLEAR
                }
            })
        })
        .child(view().h(px(1.)).bg(move || {
            theme::color(move || {
                if row.with(|r| r.first) {
                    CLEAR
                } else {
                    palette::border_subtle()
                }
            })
        }))
        .child(
            nsh_base::button()
                .px(px(CARD_PAD))
                .pt(move || px(pad()))
                .pb(move || px(if open.get() { 0. } else { pad() }))
                .hover(|s| s.bg(theme::color(palette::surface_raised)))
                .on_press(move || {
                    let pid = pid.get_untracked();
                    store::update(|state, fx| state.toggle_pid(pid, fx));
                })
                .child(
                    view()
                        .flex_row()
                        .items_start()
                        .gap(px(10.))
                        .child(dynamic(move || {
                            let (pid, name) = ident.get();
                            widgets::app_mark(pid, &name, &name)
                        }))
                        .child(
                            view()
                                .flex_1()
                                .min_w(px(0.))
                                .child(
                                    view()
                                        .flex_row()
                                        .items_baseline()
                                        .justify_between()
                                        .gap(px(8.))
                                        .child(widgets::name_tailed(
                                            move || row.with(|r| r.name.clone()),
                                            || None,
                                            12.,
                                            FontWeight::MEDIUM,
                                        ))
                                        .child(
                                            text(move || row.with(|r| r.pct.clone()))
                                                .flex_none()
                                                .tabular_nums()
                                                .text_size(px(12.))
                                                .line_height(widgets::line(12.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(move || {
                                                    theme::color(move || {
                                                        palette::text_for(row.with(|r| r.hot))
                                                    })
                                                }),
                                        ),
                                )
                                .child(second_line(row, open, own, badge)),
                        ),
                )
                .child(view().mt(px(4.)).child(widgets::meter(
                    move || row.with(|r| r.fraction),
                    move || palette::fill_for(row.with(|r| r.hot)),
                    3.,
                ))),
        )
        .child(when(
            move || open.get(),
            move || {
                view()
                    .px(px(CARD_PAD))
                    .pb(move || px(pad()))
                    .child(expand_block(live, pid.get_untracked(), full))
            },
        ))
        .into_element()
}

/// The mark that says the row opens, the pid (and a status worth a
/// word, in the accent), this app's pills and a steady load's; at the
/// right what the list is sorted by.
fn second_line(
    row: Memo<Row>,
    open: Memo<bool>,
    own: impl Fn() -> bool + Copy + 'static,
    badge: Memo<Option<String>>,
) -> View {
    view()
        .flex_row()
        .justify_between()
        .gap(px(8.))
        .mt(px(2.))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(5.))
                .min_w(px(0.))
                .child(widgets::expand_mark(move || open.get()))
                .child(
                    text(move || row.with(|r| r.meta.clone()))
                        .flex_shrink(1.)
                        .number_of_lines(1u32)
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .text_color(move || {
                            theme::color(move || {
                                if row.with(|r| r.notable) {
                                    palette::accent_light()
                                } else {
                                    palette::text_dim()
                                }
                            })
                        }),
                )
                .child(when(own, || {
                    widgets::pill(i18n::tr("processes.this_app"), false)
                }))
                .child(when(
                    move || own() && badge.with(Option::is_some),
                    move || lists::disk_check_badge(badge),
                ))
                .child(when(
                    move || row.with(|r| r.sustained.is_some()),
                    move || {
                        widgets::pill(
                            move || row.with(|r| r.sustained.clone().unwrap_or_default()),
                            true,
                        )
                    },
                )),
        )
        .child(
            text(move || row.with(|r| r.figure.clone()))
                .flex_none()
                .tabular_nums()
                .text_size(px(10.))
                .line_height(widgets::line(10.))
                .text_color(theme::color(palette::text_muted)),
        )
}

/// What an open process's expansion shows.
#[derive(Clone, PartialEq, Default)]
struct Expansion {
    /// The command's headline — the `.app` it runs from, or the command —
    /// and the whole command on hover.
    headline: String,
    cmd: String,
    labels: Vec<String>,
    values: Vec<String>,
    bars: Vec<(String, BarDisplay)>,
    editor: Option<Editor>,
    /// Its name, for Quit's sentence, while this user may signal it.
    quit: Option<String>,
}

fn expansion_of(state: &ZStatsAppState, pid: u32, full: bool) -> Expansion {
    let Some(tick) = state.latest() else {
        return Expansion::default();
    };
    let table: &[ProcessSnapshot] = match (full, state.full_scan()) {
        (true, FullScan::Ready(data)) => &data.processes,
        _ => tick
            .snapshot
            .processes
            .as_deref()
            .map(Vec::as_slice)
            .unwrap_or(&[]),
    };
    let Some(p) = table.iter().find(|p| p.pid == pid) else {
        return Expansion::default();
    };
    let names: HashMap<u32, &str> = table.iter().map(|q| (q.pid, q.name.as_str())).collect();
    let parent_name = p.parent_pid.and_then(|pp| names.get(&pp).copied());
    // `Default` is `current()`, so a row painted before the first tick
    // reads this build's own capabilities — never a false "unsupported".
    let caps = tick.snapshot.capabilities;
    let mut detail = vec![
        (i18n::tr("processes.live"), format::pct(p.cpu_usage_percent)),
        (
            i18n::tr("processes.mem_footprint"),
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
            proclist::parent_display(p.parent_pid, parent_name),
        ),
        (
            i18n::tr("processes.disk_io"),
            proclist::disk_io_display(p.read_bytes_per_sec, p.write_bytes_per_sec),
        ),
    ];
    let current = proclist::current_user_id();
    if proclist::show_user(p.user_id.as_deref(), current.as_deref()) {
        detail.push((
            i18n::tr("processes.user"),
            p.user_id
                .clone()
                .unwrap_or_else(|| format::PLACEHOLDER.into()),
        ));
    }
    let headline = proclist::cmd_headline(&p.cmd).to_string();
    let (labels, values) = detail.into_iter().unzip();
    Expansion {
        headline: if headline.is_empty() {
            format::PLACEHOLDER.to_string()
        } else {
            headline
        },
        cmd: p.cmd.clone(),
        labels,
        values,
        bars: proclist::alert_bar_rows(&p.name, state),
        editor: lists::editor_of(state, &p.name, KEYS),
        quit: terminate::can_term(pid).then(|| p.name.clone()),
    }
}

/// An open process: its command, its figures two-up, its two alert bars,
/// the chips that edit them, Quit.
fn expand_block(live: Live, pid: u32, full: bool) -> View {
    let x = live.watch(move || store::read(|state| expansion_of(state, pid, full)));
    let labels = x.with_untracked(|x| x.labels.clone());
    let bars = x.with_untracked(|x| x.bars.len());
    let mut block = view()
        .mt(px(8.))
        .p(px(10.))
        .rounded(px(8.))
        .bg(theme::color(palette::inset))
        .child(
            view()
                .min_w(px(0.))
                .tooltip(
                    widgets::live_tooltip(move || x.with(|x| x.cmd.clone()))
                        .disabled(move || x.with(|x| x.cmd.is_empty())),
                )
                .child(
                    text(move || x.with(|x| x.headline.clone()))
                        .number_of_lines(1u32)
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .text_color(theme::color(palette::text_muted)),
                ),
        )
        .child(lists::kv_columns(labels, move |i| {
            x.with(|x| x.values.get(i).cloned().unwrap_or_default())
        }));
    for i in 0..bars {
        let label = x.with_untracked(|x| x.bars[i].0.clone());
        block = block.child(lists::bar_row(
            label,
            move || x.with(|x| x.bars.get(i).map(|b| b.1.clone()).unwrap_or_default()),
            i + 1 == bars,
        ));
    }
    block
        .child(when(
            move || x.with(|x| x.editor.is_some()),
            move || {
                lists::threshold_editor(
                    move || x.with(|x| x.editor.clone().unwrap_or_default()),
                    KEYS,
                )
            },
        ))
        .child(when(
            move || x.with(|x| x.quit.is_some()),
            move || quit_button(pid, x),
        ))
}

/// Quit, behind a confirm: SIGTERM, Activity Monitor's Quit and not its
/// Force Quit (`processes::kill`).
fn quit_button(pid: u32, x: Memo<Expansion>) -> AnyElement {
    lists::quit_button(i18n::tr("processes.kill_ok"), move || {
        let Some(name) = x.with_untracked(|x| x.quit.clone()) else {
            return;
        };
        widgets::confirm(
            i18n::tr("processes.kill_title"),
            t!("processes.kill_body", name = name, pid = pid).to_string(),
            i18n::tr("processes.kill_ok"),
            move || {
                if !terminate::request_term(pid) {
                    tracing::warn!("quit request for pid {pid} was not delivered");
                }
            },
        );
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nsh::lists::LIST_HEADER;

    /// The region at the three standard heights (`apps::tests` says how
    /// they are measured): each ends on a whole row, as many as the gpui
    /// page's (`processes::tests`), none far from its own height.
    #[test]
    fn the_region_ends_on_a_whole_row() {
        for (panel, expected) in [(539., 6), (585., 7), (828., 11)] {
            let budget = panel - 60. - 44. - CARD_GAP - LIST_HEADER;
            let fit = rows::fit_rows(budget, COLLAPSED_ROW, ROW_PAD);
            let (count, reach) = rows::tests::whole_rows(fit);
            assert_eq!(count, expected, "at {panel}");
            assert!(reach >= fit.viewport, "the rows stop short at {panel}");
            for i in 0..count {
                let over = fit.height(i) - COLLAPSED_ROW;
                assert!(
                    (-rows::ROW_SQUEEZE..=6.).contains(&over),
                    "{over} at {panel}"
                );
            }
        }
    }
}
