//! Applications: whole process trees, summed.
//!
//! A browser's 37 helpers never individually cross a threshold that the
//! app as a whole blows past. zstats sums each tree over the FULL process
//! table, then keeps only `max-processes` groups (default 50) ranked by
//! CPU — the same cap as the Processes tab. Idle apps are missing on
//! purpose, not because their members were dropped. The All chip is the
//! escape hatch: a one-shot scan with the cap off, same reasons as
//! [`crate::fullscan`].

use super::processes;
use super::widgets;
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{FullAppScan, FullAppScanData, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, list, px,
};
use gpui_component::{h_flex, v_flex};
use rust_i18n::t;
use std::collections::HashSet;
use zstats::snapshot::ProcessGroupSnapshot;

const HOT_PERCENT: f32 = 200.0;

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

    let filter = state.proc_filter_text();
    if !filter.is_empty() {
        rows.retain(|g| g.name.to_lowercase().contains(filter));
    }

    let shown = rows.len();
    let no_match = !filter.is_empty() && shown == 0;

    let list_el = widgets::list_shell()
        .child(widgets::list_header(
            h_flex()
                .items_center()
                .gap(px(4.))
                .child(t!("apps.count_of", shown = shown).to_string())
                .child(widgets::info_icon(
                    "apps-cpu-basis",
                    i18n::tr("apps.cpu_basis_tip"),
                )),
            Some(
                h_flex()
                    .items_center()
                    .gap(px(5.))
                    .child(processes::filter_chip(state))
                    .child(full_scan_chip(state))
                    // The whole explanation — what a tree is, why `login`
                    // can legitimately head the list, where the cap comes
                    // from — lives on this label's tooltip instead of a
                    // permanent note block: it is worth reading once, not
                    // worth a paragraph of chrome on every open.
                    .child(
                        div()
                            .id("apps-tree-totals")
                            .text_size(px(10.))
                            .text_color(theme::text_dim())
                            .tooltip(widgets::wrap_tooltip(i18n::tr("apps.cap_note")))
                            .child(i18n::tr("apps.tree_totals")),
                    )
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
                        app_row(g, i + 1 == shown, state, ambiguous)
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
    let visible = data.visible.clone();
    let count = visible.len();
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
            t!("apps.full_count", shown = count, total = data.total).to_string(),
            Some(
                h_flex()
                    .items_center()
                    .gap(px(5.))
                    .child(processes::filter_chip(state))
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
                app_row(g, i + 1 == count, state, ambiguous)
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
) -> AnyElement {
    let hot = g.cpu_usage_percent > HOT_PERCENT;
    let expanded = state.selected_app() == Some(g.root_pid);
    let root_pid = g.root_pid;

    let mut row = v_flex()
        .id(("app", root_pid as usize))
        .w_full()
        .px(px(13.))
        // Same rhythm as a process row: this list carries less per row
        // (no meter), so it has no business being the looser of the two.
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
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::text())
                        .truncate()
                        .child(g.name.clone()),
                )
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
        );

    if expanded {
        let mut detail = vec![
            (i18n::tr("apps.root_pid"), g.root_pid.to_string()),
            (i18n::tr("apps.processes"), g.process_count.to_string()),
            // Both figures, like the process rows: the row above shows
            // the footprint, and this is where "80 MB resident, 300 MB
            // actually held" stops being a contradiction. A group with
            // no footprint at all reads `—` here rather than repeating
            // its RSS under the other name.
            (
                i18n::tr("processes.mem_footprint"),
                g.phys_footprint_bytes
                    .map_or(format::PLACEHOLDER.into(), format::memory),
            ),
            (
                i18n::tr("processes.mem_rss"),
                format::memory(g.memory_bytes),
            ),
            (i18n::tr("apps.cpu"), format::pct(g.cpu_usage_percent)),
        ];
        // Real zeros still get rows here — the expansion is where "measured
        // nothing" and "not collected" (both None) must stay distinguishable.
        if g.read_bytes_per_sec.is_some() || g.write_bytes_per_sec.is_some() {
            detail.push((i18n::tr("apps.read"), format::rate(g.read_bytes_per_sec)));
            detail.push((i18n::tr("apps.write"), format::rate(g.write_bytes_per_sec)));
        }
        row = row.child(
            v_flex()
                .mt(px(8.))
                .p(px(10.))
                .rounded(px(8.))
                .bg(theme::inset())
                .child(widgets::kv_columns(detail)),
        );
    }

    row.on_click(move |_, _window, cx| {
        cx.global::<ZStatsGlobalStore>()
            .clone()
            .update(cx, |state, cx| state.toggle_app(root_pid, cx));
    })
    .into_any_element()
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
}
