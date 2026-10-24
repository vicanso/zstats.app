//! Apps on NSH: the gpui front end's `views/apps.rs`, whose comments say
//! why every row, figure and control is the way it is — this file carries
//! the same choices and does not repeat their reasons. The page's rules
//! are shared (`applist.rs`), the alert bars' words too (`alertbars.rs`),
//! and the region's division among whole rows (`rows.rs`).
//!
//! The top list: a pinned header (the count, the filter, the sort and the
//! All chip), the name filter while it is open, and the rows in a region
//! that ends on a whole row at the gap above the footer. A row is kept by
//! its tree's root pid (`each`), so it keeps its icon while its figures
//! move; the open row carries its expansion — the tree's figures, its two
//! alert bars with the chips that edit them, its members, and Quit. The
//! All chip swaps the list for a one-off scan of every tree, a virtual
//! list whose rows are the same rows at their own height.

use crate::alertbars::{self, BarDisplay, Resolved};
use crate::applist;
use crate::assets::CustomIconName;
use crate::diskwatch::Hold;
use crate::nsh::root::{CARD_GAP, CARD_PAD, Live};
use crate::nsh::widgets::{self, CLEAR};
use crate::nsh::{store, theme};
use crate::rows::{self, RowFit};
use crate::state::{AppSort, FullAppScan, ZStatsAppState};
use crate::{format, fullscan, hot, i18n, palette, terminate, trend};
use nsh_core::ui::prelude::*;
use rust_i18n::t;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::process;
use zstats::snapshot::{ProcessGroupSnapshot, ProcessSnapshot};

/// What stands above and below a collapsed row's lines, before the fit
/// gives the row its share of the leftover (`processes::ROW_PAD`).
const ROW_PAD: f32 = 7.;

/// A collapsed row, hairline included: the padding twice, the first line
/// (the 13px figure's 21, over the 12px name's 19), the 3 under it, the
/// 10px second line's 16, the meter's 4 and 3, the hairline. The gpui
/// row's figure, line for line (`apps::COLLAPSED_ROW`) — NSH's lines are
/// whole points (`widgets::line`).
const COLLAPSED_ROW: f32 = ROW_PAD + ROW_PAD + 21. + 3. + 16. + 4. + 3. + 1.;

/// The pinned header: 11 above the 12px title's 19, 9 under it
/// (`processes::LIST_HEADER`).
const LIST_HEADER: f32 = 11. + 19. + 9.;

/// The open filter row: the field's 28 and the 6 under it
/// (`processes::FILTER_ROW_HEIGHT`).
const FILTER_ROW: f32 = 34.;
const FIELD_H: f32 = 28.;

/// Never shorter than this, however small the window gets
/// (`processes::FULL_LIST_MIN`).
const LIST_MIN: f32 = 120.;

/// The page: waiting, collection off, the top list, or the full scan.
#[derive(Clone, Copy, PartialEq)]
enum Shown {
    Waiting,
    Off,
    Top,
    Full,
}

fn shown(state: &ZStatsAppState) -> Shown {
    if matches!(state.full_app_scan(), FullAppScan::Ready(_)) {
        return Shown::Full;
    }
    match state.latest() {
        None => Shown::Waiting,
        Some(tick) if tick.snapshot.process_groups.is_none() => Shown::Off,
        Some(_) => Shown::Top,
    }
}

/// The Apps page; `body` is the height it has on screen.
pub fn page(live: Live, body: ReadSignal<f32>) -> AnyElement {
    let shown = live.memo(shown);
    let failed =
        live.memo(|state: &ZStatsAppState| matches!(state.full_app_scan(), FullAppScan::Failed));
    dynamic(move || match shown.get() {
        Shown::Waiting => {
            widgets::empty_card(i18n::tr("apps.title"), i18n::tr("common.waiting_sample"))
        }
        Shown::Off => widgets::empty_card(i18n::tr("apps.off"), i18n::tr("apps.off_body")),
        Shown::Top => view()
            .gap(px(CARD_GAP))
            .child(when(
                move || failed.get(),
                || {
                    widgets::empty_card(
                        i18n::tr("apps.full_failed"),
                        i18n::tr("apps.full_failed_body"),
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
    /// The executable's name: what thresholds match.
    name: String,
    title: String,
    job: Option<String>,
    meta: String,
    pct: String,
    hot: bool,
    io: Option<String>,
    mem: String,
    fraction: f32,
    open: bool,
    first: bool,
    /// What stands above and below its lines.
    pad: f32,
}

fn row_of(
    g: &ProcessGroupSnapshot,
    state: &ZStatsAppState,
    ambiguous: bool,
    bar_full: f32,
    index: usize,
    fit: Option<RowFit>,
) -> Row {
    let sort = state.app_sort();
    let face = applist::face_of(g, state);
    Row {
        pid: g.root_pid,
        name: g.name.clone(),
        title: face.title,
        job: face.job,
        meta: applist::tree_meta(g, ambiguous),
        pct: format::pct_col(g.cpu_usage_percent),
        hot: hot::is_hot(f64::from(g.cpu_usage_percent), false)
            && !hot::own_check_cpu(g.root_pid, state),
        io: applist::io_line(g.read_bytes_per_sec, g.write_bytes_per_sec)
            .map(|io| format!("{io} ·")),
        mem: format::memory(applist::shown_memory(g)),
        fraction: applist::bar_fraction(sort, g, bar_full),
        open: state.selected_app() == Some(g.root_pid),
        first: index == 0,
        pad: fit.map_or(ROW_PAD, |fit| fit.pad(index)),
    }
}

/// The names this listing shows more than once, which earn a pid.
fn repeated(groups: &[&ProcessGroupSnapshot], state: &ZStatsAppState) -> HashSet<String> {
    let faces: Vec<String> = groups
        .iter()
        .map(|g| applist::face_of(g, state).text())
        .collect();
    applist::repeated_names(faces.iter().map(String::as_str))
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// The top list's rows region and what it holds.
#[derive(Clone, PartialEq, Default)]
struct Top {
    rows: Vec<Row>,
    /// The region's height ([`rows::fit_rows`]).
    viewport: f32,
    /// Where the open row starts in the region, when it is in it.
    open_at: Option<f32>,
    no_match: bool,
}

fn top_of(state: &ZStatsAppState, body: f32) -> Top {
    let Some(groups) = state
        .latest()
        .and_then(|t| t.snapshot.process_groups.as_deref())
    else {
        return Top::default();
    };
    let shown = applist::top_rows(state, groups);
    let chrome = LIST_HEADER
        + if state.proc_filter_open() {
            FILTER_ROW
        } else {
            0.
        };
    let budget = (body - CARD_GAP - chrome).max(LIST_MIN);
    let fit = rows::fit_rows(budget, COLLAPSED_ROW, ROW_PAD);
    let repeated = repeated(&shown, state);
    let bar_full = applist::bar_full_for(state.app_sort(), shown.iter().copied());
    let rows: Vec<Row> = shown
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let ambiguous = repeated.contains(&applist::face_of(g, state).text());
            row_of(g, state, ambiguous, bar_full, i, Some(fit))
        })
        .collect();
    let open_at = rows
        .iter()
        .position(|r| r.open)
        .map(|ix| (0..ix).map(|i| fit.height(i)).sum());
    Top {
        no_match: !state.proc_filter_text().is_empty() && rows.is_empty(),
        rows,
        viewport: fit.viewport,
        open_at,
    }
}

/// The header's left side: how many trees, and the one ⓘ for both notes.
fn count_title(count: impl Fn() -> String + 'static) -> AnyElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .min_w(px(0.))
        .child(widgets::list_title(count).flex_shrink(1.))
        .child(widgets::info_icon(widgets::tooltip_lines(|| {
            vec![i18n::tr("apps.cpu_basis_tip"), i18n::tr("apps.cap_note")]
        })))
        .into_element()
}

/// The header's controls: the filter, the sort, the All chip.
fn controls(live: Live) -> AnyElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(5.))
        .child(filter_chip(live))
        .child(sort_control(live))
        .child(full_scan_chip(live))
        .into_element()
}

fn top_card(live: Live, body: ReadSignal<f32>) -> View {
    let top = live.watch(move || {
        let body = body.get();
        store::read(|state| top_of(state, body))
    });
    let count = live.memo(|state: &ZStatsAppState| {
        state
            .latest()
            .and_then(|t| t.snapshot.process_groups.as_deref())
            .map_or(0, |groups| applist::top_rows(state, groups).len())
    });
    // A jump from Overview puts the open row in view, once: asked for
    // when the page is made, done once the region is laid out — a scroll
    // sent before then reaches no view.
    let region = element_ref();
    let reveal = signal(None::<f32>);
    effect(move || {
        live.track();
        if store::read(ZStatsAppState::take_app_reveal) {
            reveal.set(top.with_untracked(|t| t.open_at));
        }
    });
    widgets::list_shell()
        .child(widgets::list_header(
            count_title(move || t!("apps.count_of", shown = count.get()).to_string()),
            Some(controls(live)),
        ))
        .child(filter_row(live))
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
                    move || top.with(|t| t.rows.clone()),
                    |row: &Row| row.pid,
                    move |row, _| app_row(computed(move || row.get()), live),
                ))
                .child(when(
                    move || top.with(|t| t.no_match),
                    || {
                        view()
                            .px(px(CARD_PAD))
                            .py(px(10.))
                            .child(widgets::note(i18n::tr("apps.filter_no_match")))
                    },
                )),
        )
}

/// The full scan: every tree, filtered and sorted.
#[derive(Clone, PartialEq, Default)]
struct Full {
    title: String,
    note: String,
    rows: Vec<Row>,
}

fn full_of(state: &ZStatsAppState) -> Full {
    let FullAppScan::Ready(data) = state.full_app_scan() else {
        return Full::default();
    };
    let groups = &data.groups;
    let sort = state.app_sort();
    let mut visible = data.visible.clone();
    applist::sort_indices(&mut visible, groups, sort);
    // The whole listing's, not the filtered cut's: the meters must not
    // rescale on every keystroke.
    let bar_full = applist::bar_full_for(sort, groups.iter());
    let shown: Vec<&ProcessGroupSnapshot> = visible.iter().map(|&i| &groups[i]).collect();
    let repeated = repeated(&shown, state);
    Full {
        title: t!("apps.full_count", shown = visible.len(), total = data.total).to_string(),
        note: t!(
            "apps.full_note",
            ms = data.window.as_millis(),
            age = format::ago(data.at.elapsed())
        )
        .to_string(),
        rows: shown
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let ambiguous = repeated.contains(&applist::face_of(g, state).text());
                row_of(g, state, ambiguous, bar_full, i, None)
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
        if store::read(ZStatsAppState::take_app_reveal) {
            reveal.set(full.with_untracked(|f| f.rows.iter().position(|r| r.open)));
        }
    });
    widgets::list_shell()
        .h(move || px((body.get() - CARD_GAP).max(LIST_MIN)))
        .child(widgets::list_header(
            widgets::list_title(move || full.with(|f| f.title.clone())).into_element(),
            Some(controls(live)),
        ))
        .child(
            view()
                .px(px(CARD_PAD))
                .pb(px(8.))
                .child(widgets::note(move || full.with(|f| f.note.clone()))),
        )
        .child(filter_row(live))
        .child(when(
            move || full.with(|f| f.rows.is_empty()),
            || {
                view()
                    .px(px(CARD_PAD))
                    .py(px(10.))
                    .child(widgets::note(i18n::tr("apps.filter_no_match")))
            },
        ))
        .child(
            virtual_list(
                move || full.with(|f| f.rows.len()),
                move |at: Position| {
                    let row = computed(move || {
                        full.with(|f| f.rows.get(at.index()).cloned())
                            .unwrap_or_else(|| empty_row(at.index()))
                    });
                    app_row(row, live)
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
        title: String::new(),
        job: None,
        meta: String::new(),
        pct: String::new(),
        hot: false,
        io: None,
        mem: String::new(),
        fraction: 0.,
        open: false,
        first: index == 0,
        pad: ROW_PAD,
    }
}

/// The header control that opens the name filter.
fn filter_chip(live: Live) -> AnyElement {
    let on = live.memo(ZStatsAppState::proc_filter_open);
    view()
        .flex_none()
        .tooltip(widgets::wrap_tooltip(i18n::tr("processes.filter_tip")))
        .child(
            nsh_base::button()
                .rounded(px(4.))
                .p(px(2.))
                .bg(move || theme::color(move || if on.get() { palette::chip() } else { CLEAR }))
                .hover(|s| s.bg(theme::color(palette::surface_raised)))
                .on_press(move || {
                    let open = on.get_untracked();
                    store::update(|state, fx| {
                        if open {
                            state.close_proc_filter(fx);
                        } else {
                            state.open_proc_filter(fx);
                        }
                    });
                })
                .child(widgets::icon(CustomIconName::Search, 11., move || {
                    if on.get() {
                        palette::text()
                    } else {
                        palette::text_muted()
                    }
                })),
        )
        .into_element()
}

/// The name filter's field while it is open, focused as it appears; the
/// clear empties it, as Escape does on the gpui front end.
fn filter_row(live: Live) -> AnyElement {
    let on = live.memo(ZStatsAppState::proc_filter_open);
    let query = live.memo(|state: &ZStatsAppState| state.proc_filter_text().to_owned());
    let typed = signal(String::new());
    when(
        move || on.get(),
        move || {
            typed.set(String::new());
            view()
                .h(px(FILTER_ROW))
                .px(px(CARD_PAD))
                .pb(px(FILTER_ROW - FIELD_H))
                .child(
                    view()
                        .flex_row()
                        .h(px(FIELD_H))
                        .items_center()
                        .gap(px(6.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .border(px(1.))
                        .border_color(theme::color(palette::border))
                        .child(widgets::icon(
                            CustomIconName::Search,
                            12.,
                            palette::text_dim,
                        ))
                        .child(
                            input()
                                .flex_1()
                                .value(move || typed.get())
                                .placeholder(i18n::tr("processes.filter_placeholder"))
                                .auto_focus(true)
                                .auto_correct(false)
                                .text_size(px(12.))
                                .text_color(theme::color(palette::text))
                                .on_change(move |text: &str| {
                                    typed.set(text.to_owned());
                                    store::update(|state, fx| state.set_proc_filter_text(text, fx));
                                }),
                        )
                        .child(when(
                            move || !query.with(String::is_empty),
                            move || {
                                nsh_base::button()
                                    .flex_none()
                                    .rounded_full()
                                    .p(px(2.))
                                    .hover(|s| s.bg(theme::color(palette::surface_raised)))
                                    .on_press(move || {
                                        typed.set(String::new());
                                        store::update(|state, fx| {
                                            state.set_proc_filter_text("", fx)
                                        });
                                    })
                                    .child(widgets::icon(
                                        CustomIconName::Close,
                                        11.,
                                        palette::text_dim,
                                    ))
                            },
                        )),
                )
        },
    )
    .into_element()
}

/// CPU or memory, as a menu.
fn sort_control(live: Live) -> AnyElement {
    let sort = live.memo(ZStatsAppState::app_sort);
    let full =
        live.memo(|state: &ZStatsAppState| matches!(state.full_app_scan(), FullAppScan::Ready(_)));
    let choices = AppSort::ALL
        .iter()
        .map(|&order| widgets::Choice {
            label: i18n::tr(order.label_key()),
            on: Box::new(move || sort.get() == order),
            choose: Box::new(move || store::update(|state, fx| state.set_app_sort(order, fx))),
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

/// The chip that asks for every tree, and the one that gives the top
/// list back; no hover while a scan is in flight.
fn full_scan_chip(live: Live) -> AnyElement {
    #[derive(Clone, Copy, PartialEq)]
    enum Chip {
        Off,
        Running,
        Showing,
    }
    let chip = live.memo(|state: &ZStatsAppState| match state.full_app_scan() {
        FullAppScan::Running => Chip::Running,
        FullAppScan::Ready(_) => Chip::Showing,
        _ => Chip::Off,
    });
    view()
        .flex_none()
        .tooltip(widgets::live_tooltip(move || {
            i18n::tr(if chip.get() == Chip::Showing {
                "apps.full_back_tip"
            } else {
                "apps.full_chip_tip"
            })
        }))
        .child(
            nsh_base::button()
                .rounded_full()
                .border(px(1.))
                .border_color(move || {
                    theme::color(move || {
                        if chip.get() == Chip::Showing {
                            palette::border()
                        } else {
                            palette::border_subtle()
                        }
                    })
                })
                .bg(move || {
                    theme::color(move || {
                        if chip.get() == Chip::Showing {
                            palette::chip()
                        } else {
                            CLEAR
                        }
                    })
                })
                .hover(move |s| {
                    if chip.get_untracked() == Chip::Off {
                        s.bg(theme::color(palette::surface_raised))
                            .border_color(theme::color(palette::border))
                    } else {
                        s
                    }
                })
                .px(px(7.))
                .py(px(1.))
                .on_press(|| store::update(|state, fx| state.toggle_full_app_scan(fx)))
                .child(
                    text(move || {
                        i18n::tr(match chip.get() {
                            Chip::Running => "apps.full_scanning",
                            Chip::Showing => "apps.full_back",
                            Chip::Off => "apps.full_chip",
                        })
                    })
                    .text_size(px(9.))
                    .line_height(widgets::line(9.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(move || {
                        theme::color(move || match chip.get() {
                            Chip::Running => palette::text_dim(),
                            Chip::Showing => palette::text(),
                            Chip::Off => palette::text_muted(),
                        })
                    }),
                ),
        )
        .into_element()
}

/// This app's own row while the daily disk check is its work: what the
/// badge says.
fn disk_check_label(state: &ZStatsAppState) -> Option<String> {
    state.disk_check_running().then(|| {
        i18n::tr(match state.disk_check_held() {
            None => "processes.disk_check",
            Some(Hold::OnBattery) => "processes.disk_check_power",
            Some(Hold::Busy) => "processes.disk_check_quiet",
        })
    })
}

/// One tree: its icon beside its name and figure, the second line, the
/// meter; open, its expansion under them. The collapsed part toggles it.
fn app_row(row: Memo<Row>, live: Live) -> AnyElement {
    let open = computed(move || row.with(|r| r.open));
    let pid = computed(move || row.with(|r| r.pid));
    // The mark is made again only when the program on the row changes.
    let ident = computed(move || row.with(|r| (r.pid, r.name.clone(), r.title.clone())));
    let badge = live.memo(disk_check_label);
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
        // The hairline over every row but the first.
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
                    store::update(|state, fx| state.toggle_app(pid, fx));
                })
                .child(
                    view()
                        .flex_row()
                        .items_start()
                        .gap(px(10.))
                        .child(dynamic(move || {
                            let (pid, name, title) = ident.get();
                            widgets::app_mark(pid, &name, &title)
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
                                        .child(
                                            view()
                                                .flex_row()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .items_baseline()
                                                .gap(px(6.))
                                                .child(widgets::name_tailed(
                                                    move || row.with(|r| r.title.clone()),
                                                    move || row.with(|r| r.job.clone()),
                                                    12.,
                                                    FontWeight::MEDIUM,
                                                ))
                                                .child(when(
                                                    move || own() && badge.with(Option::is_some),
                                                    move || {
                                                        view()
                                                            .flex_none()
                                                            .tooltip(widgets::wrap_tooltip(
                                                                i18n::tr(
                                                                    "processes.disk_check_tip",
                                                                ),
                                                            ))
                                                            .child(widgets::pill(
                                                                move || {
                                                                    badge.with(|b| {
                                                                        b.clone()
                                                                            .unwrap_or_default()
                                                                    })
                                                                },
                                                                false,
                                                            ))
                                                    },
                                                )),
                                        )
                                        .child(
                                            text(move || row.with(|r| r.pct.clone()))
                                                .flex_none()
                                                .tabular_nums()
                                                .text_size(px(13.))
                                                .line_height(widgets::line(13.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(move || {
                                                    theme::color(move || {
                                                        palette::text_for(row.with(|r| r.hot))
                                                    })
                                                }),
                                        ),
                                )
                                .child(second_line(row, open)),
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
                    .child(expand_block(live, pid.get_untracked()))
            },
        ))
        .into_element()
}

/// What opens and the mark that says it does, then the IO and the
/// memory — memory last and in the text ink, the column the eye runs
/// down.
fn second_line(row: Memo<Row>, open: Memo<bool>) -> View {
    view()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .mt(px(3.))
        .child(
            view()
                .flex_row()
                .min_w(px(0.))
                .items_center()
                .gap(px(3.))
                .child(widgets::expand_mark(move || open.get()))
                .child(
                    text(move || row.with(|r| r.meta.clone()))
                        .flex_shrink(1.)
                        .number_of_lines(1u32)
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .text_color(theme::color(palette::text_dim)),
                ),
        )
        .child(
            view()
                .flex_row()
                .flex_none()
                .items_baseline()
                .gap(px(4.))
                .child(when(
                    move || row.with(|r| r.io.is_some()),
                    move || {
                        text(move || row.with(|r| r.io.clone().unwrap_or_default()))
                            .tabular_nums()
                            .text_size(px(9.5))
                            .line_height(widgets::line(9.5))
                            .text_color(theme::color(palette::text_muted))
                    },
                ))
                .child(
                    text(move || row.with(|r| r.mem.clone()))
                        .tabular_nums()
                        .text_size(px(9.5))
                        .line_height(widgets::line(9.5))
                        .text_color(theme::color(palette::text)),
                ),
        )
}

/// One member of an open tree.
#[derive(Clone, PartialEq)]
struct Member {
    pid: u32,
    name: String,
    cpu: String,
    mem: String,
    last: bool,
}

/// What an open tree's expansion shows.
#[derive(Clone, PartialEq, Default)]
struct Expansion {
    /// The executable's name: what the bars and the chips key on.
    name: String,
    root_pid: String,
    footprint: String,
    cpu: String,
    rss: String,
    cpu_bar: BarDisplay,
    mem_bar: BarDisplay,
    /// The matchable name, shown while the title is another.
    exec_name: Option<String>,
    io: Option<String>,
    members: Vec<Member>,
    fetching: bool,
    missing: usize,
    /// What the chips' two rows light, once the settings are read.
    lit: Option<[Option<String>; 2]>,
    /// What the chips' rows are percentages of.
    caption: Option<String>,
    /// The tree's name and identity for Quit, while it can be asked to.
    quit: Option<(String, String)>,
}

/// The process's memory as its member row shows it.
fn snap_memory(p: &ProcessSnapshot) -> u64 {
    p.phys_footprint_bytes.unwrap_or(p.memory_bytes)
}

fn expansion_of(state: &ZStatsAppState, pid: u32) -> Expansion {
    let Some(tick) = state.latest() else {
        return Expansion::default();
    };
    let tick_ps = tick
        .snapshot
        .processes
        .as_deref()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let full = match state.full_app_scan() {
        FullAppScan::Ready(data) => Some(data.groups.clone()),
        _ => None,
    };
    let g = tick
        .snapshot
        .process_groups
        .as_deref()
        .and_then(|groups| groups.iter().find(|g| g.root_pid == pid))
        .cloned()
        .or_else(|| full.and_then(|groups| groups.iter().find(|g| g.root_pid == pid).cloned()));
    let Some(g) = g else {
        return Expansion::default();
    };
    let total = tick.snapshot.memory.total_bytes;
    let (cpu_bar, mem_bar, lit) = match state.settings() {
        Some(file) => {
            let bars = Resolved::of(file);
            let eff = bars.eff();
            let cpu =
                alertbars::pct_bar_text(eff.app_cpu.override_for(&g.name), eff.app_cpu.base())
                    .decided_by(bars.in_force("alert-app-cpu", &g.name));
            let mem = alertbars::mem_bar_text(
                eff.app_memory.override_for(&g.name),
                eff.app_memory_bar_bytes(&g.name, total),
                eff.app_memory.base(),
            )
            .decided_by(bars.in_force("alert-app-mem", &g.name));
            let lit = ["alert-app-cpu", "alert-app-mem"]
                .map(|key| bars.in_force(key, &g.name).and_then(|held| held.lit));
            (cpu, mem, Some(lit))
        }
        None => (
            BarDisplay::plain(format::PLACEHOLDER),
            BarDisplay::plain(format::PLACEHOLDER),
            None,
        ),
    };
    let source = state.member_processes().unwrap_or(tick_ps);
    let mut members = fullscan::tree_members(g.root_pid, source);
    members.sort_by_key(|p| Reverse(snap_memory(p)));
    let expected = g.process_count as usize;
    let fetching = state.member_table_running() && members.len() < expected;
    let missing = expected.saturating_sub(members.len());
    let live_by_pid: HashMap<u32, &ProcessSnapshot> = tick_ps.iter().map(|p| (p.pid, p)).collect();
    let count = members.len();
    let members = members
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let live = live_by_pid.get(&p.pid).copied();
            let shown = live.unwrap_or(p);
            Member {
                pid: p.pid,
                name: shown
                    .display_name
                    .clone()
                    .unwrap_or_else(|| shown.name.clone()),
                cpu: live
                    .map(|l| format::pct(l.cpu_usage_percent))
                    .unwrap_or_else(|| format::PLACEHOLDER.to_string()),
                mem: format::memory(snap_memory(shown)),
                last: i + 1 == count && missing == 0 && !fetching,
            }
        })
        .collect();
    Expansion {
        root_pid: g.root_pid.to_string(),
        footprint: g
            .phys_footprint_bytes
            .map_or(format::PLACEHOLDER.into(), format::memory),
        cpu: format::pct(g.cpu_usage_percent),
        rss: format::memory(g.memory_bytes),
        cpu_bar,
        mem_bar,
        exec_name: (applist::face_of(&g, state).text() != g.name).then(|| g.name.clone()),
        io: match (g.read_bytes_per_sec, g.write_bytes_per_sec) {
            (None, None) => None,
            (r, w) => Some(format!("R {} · W {}", format::rate(r), format::rate(w))),
        },
        members,
        fetching,
        missing,
        lit,
        caption: state
            .settings()
            .map(|_| t!("processes.th_caption", total = format::memory(total)).to_string()),
        quit: terminate::can_quit_app(g.root_pid)
            .then(|| (trend::tree_key(&g).to_string(), g.name.clone())),
        name: g.name,
    }
}

/// An open tree: its figures, its two alert bars, the matchable name and
/// the IO, its members, the chips that edit its bars, Quit.
fn expand_block(live: Live, pid: u32) -> View {
    let x = live.watch(move || store::read(|state| expansion_of(state, pid)));
    let field = move |pick: fn(&Expansion) -> String| move || x.with(pick);
    view()
        .mt(px(8.))
        .p(px(10.))
        .rounded(px(8.))
        .bg(theme::color(palette::inset))
        .child(
            view()
                .flex_row()
                .gap(px(14.))
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.))
                        .child(expand_row(
                            i18n::tr("apps.root_pid"),
                            field(|x| x.root_pid.clone()),
                            false,
                        ))
                        .child(expand_row(
                            i18n::tr("processes.mem_footprint"),
                            field(|x| x.footprint.clone()),
                            true,
                        )),
                )
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.))
                        .child(expand_row(
                            i18n::tr("apps.cpu"),
                            field(|x| x.cpu.clone()),
                            false,
                        ))
                        .child(expand_row(
                            i18n::tr("processes.mem_rss"),
                            field(|x| x.rss.clone()),
                            true,
                        )),
                ),
        )
        .child(bar_row(
            i18n::tr("processes.bar_cpu"),
            move || x.with(|x| x.cpu_bar.clone()),
            false,
        ))
        .child(bar_row(
            i18n::tr("processes.bar_mem"),
            move || x.with(|x| x.mem_bar.clone()),
            true,
        ))
        .child(when(
            move || x.with(|x| x.exec_name.is_some()),
            move || {
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .mt(px(6.))
                    .child(
                        text(move || x.with(|x| x.exec_name.clone().unwrap_or_default()))
                            .flex_shrink(1.)
                            .number_of_lines(1u32)
                            .font_family(widgets::MONO)
                            .text_size(px(10.))
                            .line_height(widgets::line(10.))
                            .text_color(theme::color(palette::text)),
                    )
                    .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
                        "apps.exec_name",
                    ))))
            },
        ))
        .child(when(
            move || x.with(|x| x.io.is_some()),
            move || {
                text(move || x.with(|x| x.io.clone().unwrap_or_default()))
                    .mt(px(6.))
                    .font_family(widgets::MONO)
                    .text_size(px(10.))
                    .line_height(widgets::line(10.))
                    .text_color(theme::color(palette::text_dim))
            },
        ))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .mt(px(8.))
                .child(widgets::note(i18n::tr("apps.members_section")))
                .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
                    "apps.members_lead",
                )))),
        )
        .child(each(
            move || x.with(|x| x.members.clone()),
            |m: &Member| m.pid,
            |m, _| member_row(m),
        ))
        .child(when(
            move || x.with(|x| x.members.is_empty() && !x.fetching),
            || {
                view()
                    .mt(px(4.))
                    .child(widgets::note(i18n::tr("apps.members_none")))
            },
        ))
        .child(when(
            move || x.with(|x| x.fetching),
            || {
                view()
                    .mt(px(4.))
                    .child(widgets::note(i18n::tr("apps.members_loading")))
            },
        ))
        .child(when(
            move || x.with(|x| x.missing > 0 && !x.members.is_empty() && !x.fetching),
            move || {
                view().mt(px(4.)).child(widgets::note(move || {
                    t!("apps.members_missing", n = x.with(|x| x.missing)).to_string()
                }))
            },
        ))
        .child(when(
            move || x.with(|x| x.lit.is_some()),
            move || threshold_editor(x),
        ))
        .child(when(
            move || x.with(|x| x.quit.is_some()),
            move || view().flex_row().justify_end().child(quit_button(pid, x)),
        ))
}

/// One figure of the expansion's grid: its label, which gives way, and
/// the figure.
fn expand_row(label: String, value: impl Fn() -> String + 'static, last: bool) -> View {
    let row = view()
        .flex_row()
        .justify_between()
        .gap(px(8.))
        .py(px(5.))
        .child(
            text(label)
                .flex_1()
                .min_w(px(0.))
                .number_of_lines(1u32)
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::text_muted)),
        )
        .child(
            text(value)
                .flex_none()
                .tabular_nums()
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::text)),
        );
    if last {
        row
    } else {
        row.border_b(px(1.))
            .border_color(theme::color(palette::border_subtle))
    }
}

/// One alert bar: its label, and the layer in a word with its ⓘ while a
/// per-name entry decided it; the figure at the right
/// (`processes::bar_row`).
fn bar_row(label: String, bar: impl Fn() -> BarDisplay + Copy + 'static, last: bool) -> View {
    let row = view()
        .flex_row()
        .justify_between()
        .items_center()
        .gap(px(8.))
        .py(px(5.))
        .child(
            view()
                .flex_row()
                .flex_1()
                .min_w(px(0.))
                .items_center()
                .gap(px(4.))
                .child(
                    text(label)
                        .flex_shrink(1.)
                        .number_of_lines(1u32)
                        .text_size(px(11.))
                        .line_height(widgets::line(11.))
                        .text_color(theme::color(palette::text_muted)),
                )
                .child(when(
                    move || bar().overridden,
                    move || {
                        let words = move || {
                            if bar().yours {
                                ("processes.bar_yours", "processes.bar_yours_tip")
                            } else {
                                ("processes.bar_table", "processes.bar_table_tip")
                            }
                        };
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(4.))
                            .child(
                                text(move || i18n::tr(words().0))
                                    .flex_none()
                                    .text_size(px(9.5))
                                    .line_height(widgets::line(9.5))
                                    .text_color(theme::color(|| {
                                        palette::tiny_label(palette::text_dim())
                                    })),
                            )
                            .child(widgets::info_icon(widgets::live_tooltip(move || {
                                i18n::tr(words().1)
                            })))
                    },
                )),
        )
        .child(
            text(move || bar().text)
                .flex_none()
                .tabular_nums()
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::text)),
        );
    if last {
        row
    } else {
        row.border_b(px(1.))
            .border_color(theme::color(palette::border_subtle))
    }
}

fn member_row(m: ReadSignal<Member>) -> AnyElement {
    view()
        .flex_row()
        .items_baseline()
        .justify_between()
        .gap(px(8.))
        .mt(px(4.))
        .py(px(3.))
        .border_b(move || px(if m.with(|m| m.last) { 0. } else { 1. }))
        .border_color(theme::color(palette::border_subtle))
        .child(widgets::name_tailed(
            move || m.with(|m| m.name.clone()),
            || None,
            11.,
            FontWeight::MEDIUM,
        ))
        .child(
            view()
                .flex_row()
                .flex_none()
                .gap(px(8.))
                .child(member_figure(move || m.with(|m| m.cpu.clone())))
                .child(member_figure(move || m.with(|m| m.mem.clone()))),
        )
        .into_element()
}

fn member_figure(words: impl Fn() -> String + 'static) -> Text {
    text(words)
        .tabular_nums()
        .text_size(px(10.))
        .line_height(widgets::line(10.))
        .text_color(theme::color(palette::text_muted))
}

/// The tree's two app-level rules, editable where they are shown: one
/// chip row each, the one its bar reads as lit
/// (`processes::threshold_editor`). Keyed on the executable's name, as
/// the bars are.
fn threshold_editor(x: Memo<Expansion>) -> View {
    let mut col = view()
        .mt(px(8.))
        .pt(px(8.))
        .gap(px(4.))
        .border_t(px(1.))
        .border_color(theme::color(palette::border_subtle))
        .child(widgets::note(move || {
            x.with(|x| x.caption.clone().unwrap_or_default())
        }));
    for (i, (key, label)) in [
        ("alert-app-cpu", i18n::tr("processes.bar_cpu_short")),
        ("alert-app-mem", i18n::tr("processes.bar_mem_short")),
    ]
    .into_iter()
    .enumerate()
    {
        col = col.child(threshold_row(x, i, key, label));
    }
    col
}

fn threshold_row(x: Memo<Expansion>, i: usize, key: &'static str, label: String) -> View {
    let chips: Vec<AnyElement> = alertbars::presets(key)
        .into_iter()
        .map(|(chip_label, value)| {
            let on =
                move || x.with(|x| x.lit.as_ref().and_then(|lit| lit[i].as_deref()) == Some(value));
            nsh_base::button()
                .flex_none()
                .rounded_full()
                .border(px(1.))
                .border_color(move || theme::color(move || palette::choice_line(on())))
                .bg(move || theme::color(move || palette::choice_fill(on())))
                .hover(move |s| s.bg(theme::color(move || palette::choice_hover(on()))))
                .px(px(6.))
                .on_press(move || {
                    let name = x.with_untracked(|x| x.name.clone());
                    store::update(|state, fx| {
                        if let Err(e) = state.apply_alert_override(key, &name, value, fx) {
                            tracing::error!("alert override failed: {e}");
                        }
                    });
                })
                .child(
                    text(chip_label)
                        .text_size(px(9.5))
                        .line_height(widgets::line(9.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(move || theme::color(move || palette::choice_ink(on()))),
                )
                .into_element()
        })
        .collect();
    view()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .child(
            text(label)
                .flex_none()
                .w(px(54.))
                .number_of_lines(1u32)
                .text_size(px(9.5))
                .line_height(widgets::line(9.5))
                .text_color(theme::color(|| palette::tiny_label(palette::text_dim()))),
        )
        .child(
            view()
                .flex_row()
                .flex_1()
                .min_w(px(0.))
                .gap(px(3.))
                .children(chips),
        )
}

/// Quit, in the accent's outline, behind a confirm: the same request as
/// ⌘Q, which the app may refuse.
fn quit_button(pid: u32, x: Memo<Expansion>) -> AnyElement {
    nsh_base::button()
        .mt(px(8.))
        .h(px(20.))
        .px(px(8.))
        .rounded(px(5.))
        .border(px(1.))
        .border_color(theme::color(|| palette::accent_wash(45)))
        .items_center()
        .justify_center()
        .hover(|s| s.bg(theme::color(|| palette::accent_wash(10))))
        .on_press(move || {
            let Some((display, identity)) = x.with_untracked(|x| x.quit.clone()) else {
                return;
            };
            widgets::confirm(
                t!("apps.quit_title", name = display.clone()).to_string(),
                t!("apps.quit_body", name = display).to_string(),
                i18n::tr("apps.quit_ok"),
                move || {
                    if !terminate::request_quit(pid, &identity) {
                        tracing::warn!("quit request for pid {pid} was not delivered");
                    }
                },
            );
        })
        .child(
            text(i18n::tr("apps.quit_ok"))
                .text_size(px(10.))
                .line_height(widgets::line(10.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(palette::accent_light)),
        )
        .into_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The region at the three standard heights: the page is the panel less
    /// the tab strip's 60 and the footer's 44, the region that less the
    /// gap under the card and the header — 386 on the short panel, 432
    /// with its notice row's 46, 675 on the long one. Each ends on a whole
    /// row, as many as the gpui page's (`apps::tests`), none far from its
    /// own height.
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
