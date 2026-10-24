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
use crate::alertbars::{self, BarDisplay, Resolved};
use crate::applist;
use crate::confirm;
use crate::font;
use crate::format;
use crate::fullscan;
use crate::gpui_ui;
use crate::hot;
use crate::i18n;
use crate::rows::{self, RowSlot};
use crate::state::{AppSort, FullAppScan, FullAppScanData, ZStatsAppState, ZStatsGlobalStore};
use crate::terminate;
use crate::theme;
use crate::trend;
use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, list, px,
};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::process;
use zstats::snapshot::{ProcessGroupSnapshot, ProcessSnapshot};

/// Header + caveat line. Same budget as the process full-scan card so
/// the list fills the panel instead of leaving a strip of empty card.
const FULL_CHROME_HEIGHT: f32 = processes::LIST_HEADER + 34.;
// Same value as processes::FILTER_ROW_HEIGHT — the row itself is shared
// (`processes::filter_row`), so a smaller budget here made the full list
// overflow by the difference and the header slid before pinning.
const FILTER_ROW_HEIGHT: f32 = 34.;

/// Collapsed app row, hairline included. A process row with two
/// differences: the figure is 13px, which at 2× snaps to a 21px line
/// and is the taller of that and the 12px name, and the gap under the
/// name is 3. Same reason as [`processes::COLLAPSED_ROW`].
const COLLAPSED_ROW: f32 = processes::ROW_PAD + processes::ROW_PAD + 21. + 3. + 16. + 4. + 3. + 1.;
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

    let rows = applist::top_rows(state, groups);
    let sort = state.app_sort();
    let filter = state.proc_filter_text();
    let shown = rows.len();
    let no_match = !filter.is_empty() && shown == 0;
    let bar_full = applist::bar_full_for(sort, rows.iter().copied());

    // A jump from Overview: put the selected row in view on this one
    // paint. The index is into `rows` as sorted and filtered — exactly
    // the children the scroll container is about to get. A target the
    // filter hides scrolls nowhere, which is honest: there is nothing
    // to show.
    if state.take_app_reveal()
        && let Some(ix) = state
            .selected_app()
            .and_then(|sel| rows.iter().position(|g| g.root_pid == sel))
    {
        gpui_ui::app_rows_scroll().scroll_to_item(ix);
    }

    let fit = rows::fit_rows(
        processes::rows_height(state),
        COLLAPSED_ROW,
        processes::ROW_PAD,
    );
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
                // One ⓘ for both notes: what the CPU figure is, and what
                // a tree is (why `login` can head the list, where the cap
                // comes from). The second used to be its own ⓘ at the far
                // end of the controls, and two identical icons in one
                // header left the reader to guess which said what.
                .child(widgets::info_icon_lines(
                    "apps-cpu-basis",
                    vec![
                        i18n::tr("apps.cpu_basis_tip").into(),
                        i18n::tr("apps.cap_note").into(),
                    ],
                )),
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
        .children(processes::filter_row(state))
        .child(
            // Same pinned-header model as the Processes top-N card; the
            // height budget is shared because the chrome is identical.
            v_flex()
                .id("app-rows")
                .track_scroll(&gpui_ui::app_rows_scroll())
                .overflow_y_scroll()
                .max_h(px(fit.viewport))
                .children({
                    // Ambiguity is about what is on screen: two
                    // stock-Electron trees showing distinct bundle
                    // names are not ambiguous, two identical shown
                    // names are — including two login compiles both
                    // wearing `cargo`. The tail counts: `Zed · cargo`
                    // and `Zed` are two different things on screen.
                    let shown_names: Vec<String> = rows
                        .iter()
                        .map(|g| applist::face_of(g, state).text())
                        .collect();
                    let repeated: HashSet<String> =
                        applist::repeated_names(shown_names.iter().map(String::as_str))
                            .into_iter()
                            .map(str::to_string)
                            .collect();
                    rows.into_iter().enumerate().map(move |(i, g)| {
                        let ambiguous = repeated.contains(&shown_names[i]);
                        let slot = RowSlot {
                            index: i,
                            fit: Some(fit),
                        };
                        app_row(g, slot, state, ambiguous, sort, bar_full)
                    })
                })
                .when(no_match, |d| {
                    d.child(
                        div()
                            .px(px(widgets::CARD_PAD))
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
    let faces: Vec<String> = groups
        .iter()
        .map(|g| applist::face_of(g, state).text())
        .collect();
    let repeated: HashSet<String> = applist::repeated_names(faces.iter().map(String::as_str))
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
    applist::sort_indices(&mut visible, &groups, sort);
    // Whole listing, not the filtered cut — meters must not rescale on
    // every keystroke, same as the process full-scan card.
    let bar_full = applist::bar_full_for(sort, groups.iter());

    if state.take_app_reveal()
        && let Some(sel) = state.selected_app()
        && let Some(ix) = visible.iter().position(|&i| groups[i].root_pid == sel)
    {
        gpui_ui::app_list(data).scroll_to_reveal_item(ix);
    }

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
                .px(px(widgets::CARD_PAD))
                .pb(px(8.))
                .child(widgets::note(full_scan_note(data))),
        )
        .children(processes::filter_row(state))
        .when(count == 0, |d| {
            d.child(
                div()
                    .px(px(widgets::CARD_PAD))
                    .py(px(10.))
                    .child(widgets::note(i18n::tr("apps.filter_no_match"))),
            )
        })
        .child(
            list(gpui_ui::app_list(data), move |i, _window, cx| {
                let state = cx.global::<ZStatsGlobalStore>().read(cx);
                let g = &groups[visible[i]];
                let ambiguous = repeated.contains(applist::face_of(g, state).text().as_str());
                let slot = RowSlot {
                    index: i,
                    fit: None,
                };
                app_row(g, slot, state, ambiguous, sort, bar_full)
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
    slot: RowSlot,
    state: &ZStatsAppState,
    ambiguous: bool,
    sort: AppSort,
    bar_full: f32,
) -> AnyElement {
    // The burst half of `hot::is_hot`, as Overview's trees: a tree has no
    // sustained flag of its own.
    let hot = hot::is_hot(f64::from(g.cpu_usage_percent), false)
        && !hot::own_check_cpu(g.root_pid, state);
    let expanded = state.selected_app() == Some(g.root_pid);
    let root_pid = g.root_pid;
    let face = applist::face_of(g, state);
    let mark = widgets::app_mark(root_pid, &g.name, &face.title);

    let mut row = v_flex()
        .id(("app", root_pid as usize))
        .w_full()
        .px(px(widgets::CARD_PAD))
        // Same rhythm as a process row, meter included — the two lists
        // used to disagree about whether a ranking has a trough. The
        // share, the fixed height and the hairline over the row are
        // that row's too (`processes::process_row`).
        .py(px(slot.pad(processes::ROW_PAD)))
        .flex_none()
        .when_some(slot.height().filter(|_| !expanded), |d, h| d.h(px(h)))
        .when(slot.index != 0, |d| {
            d.border_t(px(1.)).border_color(theme::border_subtle())
        })
        .when(expanded, |d| d.bg(theme::surface_raised()))
        .hover(|d| d.bg(theme::surface_raised()))
        // The app's own icon, or its letter, beside the two lines it
        // names; the meter under all three keeps the row's full width.
        .child(
            h_flex().items_start().gap(px(10.)).child(mark).child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .items_baseline()
                            .justify_between()
                            .gap(px(8.))
                            // The name takes the slack, so the figure
                            // keeps the right edge on every row. This
                            // app's own tree wears the disk check's
                            // badge beside it while the check is its
                            // work — on this line, because the one
                            // under it is full of figures and cut
                            // "1 process" to "1 p…".
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .items_baseline()
                                    .gap(px(6.))
                                    .child(div().min_w_0().child(widgets::truncating_name_tailed(
                                        ("app-name", root_pid as usize),
                                        // Bundle name, or a bare tree's job
                                        // (`cargo` instead of `login`); an app
                                        // whose tree a bare job burns keeps
                                        // its name and gains the job as a
                                        // muted tail (`Zed · cargo`). The
                                        // matchable name stays in the
                                        // expansion, beside the bars that
                                        // key on it.
                                        face.title,
                                        face.job.map(gpui::SharedString::from),
                                        12.,
                                        gpui::FontWeight::MEDIUM,
                                        Hsla::from(theme::text()),
                                    )))
                                    .when(root_pid == process::id(), |d| {
                                        d.children(processes::disk_check_badge(state))
                                    }),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .font_features(font::tnum())
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
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
                            // What opens, and the mark that says it
                            // does (`widgets::expand_mark`).
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .items_center()
                                    .gap(px(3.))
                                    .child(widgets::expand_mark(expanded))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(theme::text_dim())
                                            .child(applist::tree_meta(g, ambiguous)),
                                    ),
                            )
                            // Memory last and in the text ink: it is the
                            // figure the row is read for, and the
                            // rightmost spot is what lets the eye run
                            // down it as a column (`io_mem_line` says
                            // why). IO stays muted beside it — it comes
                            // and goes.
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap(px(4.))
                                    .font_features(font::tnum())
                                    .text_size(px(9.5))
                                    .text_color(theme::text_muted())
                                    .children(
                                        applist::io_line(
                                            g.read_bytes_per_sec,
                                            g.write_bytes_per_sec,
                                        )
                                        .map(|io| format!("{io} ·")),
                                    )
                                    .child(
                                        div()
                                            .text_color(theme::text())
                                            .child(format::memory(applist::shown_memory(g))),
                                    ),
                            ),
                    ),
            ),
        )
        .child(div().mt(px(4.)).child(widgets::meter(
            applist::bar_fraction(sort, g, bar_full),
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
/// Both whole-app alert bars for this tree's name — zstats' `app_cpu` /
/// `app_mem` rules resolved through the live template table, in the
/// same words as the process expansion (`processes::pct_bar_text`).
fn app_bars(g: &ProcessGroupSnapshot, state: &ZStatsAppState) -> (BarDisplay, BarDisplay) {
    let Some(file) = state.settings() else {
        return (
            BarDisplay::plain(format::PLACEHOLDER),
            BarDisplay::plain(format::PLACEHOLDER),
        );
    };
    let bars = Resolved::of(file);
    let eff = bars.eff();
    let total = state.latest().map(|t| t.snapshot.memory.total_bytes);
    (
        alertbars::pct_bar_text(eff.app_cpu.override_for(&g.name), eff.app_cpu.base())
            .decided_by(bars.in_force("alert-app-cpu", &g.name)),
        alertbars::mem_bar_text(
            eff.app_memory.override_for(&g.name),
            total.and_then(|t| eff.app_memory_bar_bytes(&g.name, t)),
            eff.app_memory.base(),
        )
        .decided_by(bars.in_force("alert-app-mem", &g.name)),
    )
}

fn expand_block(g: &ProcessGroupSnapshot, state: &ZStatsAppState) -> AnyElement {
    let (cpu_bar, mem_bar) = app_bars(g, state);
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
        // Full width: in the two-up grid "CPU alert" truncated the
        // label to "CPU…". These are the lines the chips below edit,
        // so both words have to stay readable.
        .child(processes::bar_row(
            "app-bar-cpu",
            g.root_pid as usize,
            i18n::tr("processes.bar_cpu"),
            cpu_bar,
            false,
        ))
        .child(processes::bar_row(
            "app-bar-mem",
            g.root_pid as usize,
            i18n::tr("processes.bar_mem"),
            mem_bar,
            true,
        ))
        // Title is not the matchable name: bundle vs Electron, or
        // cargo vs the login session that spawned it. The bars above
        // key on `g.name`; this line is that name, the sentence in ⓘ.
        .when(applist::face_of(g, state).text() != g.name, |d| {
            d.child(
                h_flex()
                    .items_center()
                    .gap(px(4.))
                    .mt(px(6.))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(font::MONO)
                            .text_size(px(10.))
                            .text_color(theme::text())
                            .child(g.name.clone()),
                    )
                    .child(widgets::info_icon(
                        ("apps-exec-name", g.root_pid as usize),
                        i18n::tr("apps.exec_name"),
                    )),
            )
        })
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
        // The app-level rules, editable where they are shown. Keyed on
        // `g.name` like the bars above — the title can be a bundle or a
        // job tail, and a threshold matches the executable name.
        .children(super::processes::threshold_editor(
            "app-th",
            g.root_pid as usize,
            &g.name,
            "alert-app-cpu",
            "alert-app-mem",
            state,
        ))
        .when(terminate::can_quit_app(g.root_pid), |d| {
            d.child(h_flex().justify_end().child(quit_button(
                g.root_pid,
                trend::tree_key(g).to_string(),
                g.name.clone(),
            )))
        })
        .into_any_element()
}

fn quit_button(pid: u32, display: String, identity: String) -> AnyElement {
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
            let display = display.clone();
            let identity = identity.clone();
            confirm::ask(
                window,
                cx,
                t!("apps.quit_title", name = display.clone()).to_string(),
                t!("apps.quit_body", name = display).to_string(),
                i18n::tr("apps.quit_ok"),
                move |_| {
                    if !terminate::request_quit(pid, &identity) {
                        tracing::warn!("quit request for pid {pid} was not delivered");
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
        // Label yields: a long label used to be flex_none and the
        // number truncated. The figure is the fact.
        .child(div().flex_1().min_w_0().truncate().child(label))
        .child(value)
        .into_any_element()
}

fn expand_value(text: impl Into<gpui::SharedString>) -> AnyElement {
    div()
        .flex_none()
        .font_features(font::tnum())
        .text_color(theme::text())
        .child(text.into())
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
            p.display_name.clone().unwrap_or_else(|| p.name.clone()),
            11.,
            gpui::FontWeight::MEDIUM,
            Hsla::from(theme::text()),
        ))
        .child(
            h_flex()
                .flex_none()
                .gap(px(8.))
                .font_features(font::tnum())
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

/// CPU or memory, as a menu like the Processes page's chip beside
/// which it would otherwise be the one that behaves differently. Name
/// order is omitted on purpose: fifty trees are still findable with the
/// filter.
fn sort_control(state: &ZStatsAppState) -> AnyElement {
    let sort = state.app_sort();
    let full = matches!(state.full_app_scan(), FullAppScan::Ready(_));
    let tip = i18n::tr(if full {
        sort.full_tip_key()
    } else {
        sort.tip_key()
    });
    super::menu_trigger(
        ("sort-apps", state.panel_visit()),
        tip,
        format!("↓ {}", i18n::tr(sort.label_key())),
    )
    .chip()
    .flex_none()
    .px(px(7.))
    .py(px(1.))
    .text_size(px(9.))
    .font_weight(gpui::FontWeight::MEDIUM)
    .text_color(theme::text_muted())
    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
        AppSort::ALL.iter().fold(menu, |menu, &order| {
            menu.item(
                super::menu_text(i18n::tr(order.label_key()))
                    .checked(order == sort)
                    .on_click(move |_, _, cx| {
                        cx.global::<ZStatsGlobalStore>()
                            .clone()
                            .update(cx, |state, cx| state.set_app_sort(order, cx));
                    }),
            )
        })
    })
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same regions as the process list's test. An app row is 62 tall:
    /// six on the short panel, seven with its notice row and eleven on
    /// the long one — the last two by a squeeze of a point at most,
    /// where natural rows left most of a row empty under the card.
    #[test]
    fn a_list_fills_its_region_with_whole_rows() {
        for (budget, rows) in [(385.5, 6), (431.5, 7), (674.5, 11)] {
            let fit = rows::fit_rows(budget, COLLAPSED_ROW, processes::ROW_PAD);
            assert_eq!(fit.viewport, budget);
            let reach: f32 = (0..rows).map(|i| fit.height(i)).sum();
            assert_eq!(reach, budget + 0.5, "at {budget}");
            for i in 0..rows {
                let over = fit.height(i) - COLLAPSED_ROW;
                assert!((-1.0..=3.0).contains(&over), "{over} at {budget}");
            }
        }
    }
}
