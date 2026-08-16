//! History: what actually burned the CPU today.
//!
//! The only view here ranked by an *amount* rather than a rate. Everything
//! else answers "how busy is it right now", which structurally cannot see the
//! process that never looks busy and yet costs the most over a day — see
//! [`crate::history`].

use super::widgets::{self, card};
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{HistoryRange, HistorySort, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, div, px,
};
use gpui_component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;

/// How many rows to name. Beyond this the tail is all daemons doing their job.
const TOP_N: usize = 12;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let range = state.history_range();
    let title = i18n::tr(range.title_key());
    let Some(rows) = state.history() else {
        // The read is on the background executor; this is the frame or two
        // before it lands (or a freshly switched range re-reading).
        return vec![widgets::empty_card(title, i18n::tr("history.loading"))];
    };
    if rows.is_empty() {
        // Keep the range chips reachable: an empty *day* must not strand
        // the user away from the week that does have data.
        return vec![
            widgets::list_shell()
                .child(widgets::list_header(
                    i18n::tr("history.empty_title"),
                    Some(header_controls(state, range)),
                ))
                .child(
                    div()
                        .px(px(13.))
                        .pb(px(11.))
                        .child(widgets::note(i18n::tr("history.empty_body"))),
                )
                .into_any_element(),
        ];
    }

    let sort = state.history_sort();
    // Re-ordered in the view: rank() ships CPU-time order, and the
    // memory order is a lens over the same rows, not a second dataset.
    let mut ordered: Vec<_> = rows.iter().collect();
    if sort == HistorySort::PeakMemory {
        ordered.sort_by_key(|s| std::cmp::Reverse(s.peak_memory_bytes));
    }
    // The bar is relative to the period's biggest — an absolute scale
    // would be meaningless (a day has 86 400 core-seconds per core).
    let top = ordered
        .first()
        .map_or(1, |s| match sort {
            HistorySort::CpuTime => s.cpu_time_ms,
            HistorySort::PeakMemory => s.peak_memory_bytes,
        })
        .max(1);
    let shown: Vec<_> = ordered.into_iter().take(TOP_N).collect();
    let last = shown.len().saturating_sub(1);

    // Two `yes` rows with different pids read as a duplicate at a squint;
    // a repeated name gets its pid inline so identity is visible without
    // reading the caption line.
    let mut name_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for s in &shown {
        *name_counts.entry(s.name.as_str()).or_default() += 1;
    }

    let list = widgets::list_shell()
        .child(widgets::list_header(
            title,
            Some(header_controls(state, range)),
        ))
        .children(shown.into_iter().enumerate().map(|(i, s)| {
            v_flex()
                .px(px(13.))
                .py(px(9.))
                .when(i != last, |d| {
                    d.border_b(px(1.)).border_color(theme::border_subtle())
                })
                .child(
                    h_flex()
                        .items_baseline()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .items_baseline()
                                .gap(px(5.))
                                .child(
                                    div()
                                        .min_w_0()
                                        .text_size(px(12.))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme::text())
                                        .truncate()
                                        .child(s.name.clone()),
                                )
                                .when(name_counts[s.name.as_str()] > 1, |d| {
                                    d.child(
                                        div()
                                            .flex_none()
                                            .font_family(font::MONO)
                                            .text_size(px(9.5))
                                            .text_color(theme::text_faint())
                                            .child(s.pid.to_string()),
                                    )
                                }),
                        )
                        // The headline figure is core-time, which shares a
                        // unit *shape* with the wall-clock minutes below —
                        // the CPU tag is what keeps "30m" from reading as a
                        // duration of day.
                        .child(
                            h_flex()
                                .flex_none()
                                .items_baseline()
                                .gap(px(3.))
                                .child(
                                    div()
                                        .font_family(font::MONO)
                                        .text_size(px(12.))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(theme::text())
                                        .child(match sort {
                                            HistorySort::CpuTime => {
                                                format::core_time(s.cpu_time_ms)
                                            }
                                            HistorySort::PeakMemory => {
                                                format::memory(s.peak_memory_bytes)
                                            }
                                        }),
                                )
                                .child(
                                    div()
                                        .text_size(px(8.5))
                                        .text_color(theme::text_dim())
                                        .child(i18n::tr(match sort {
                                            HistorySort::CpuTime => "alerts.kind_cpu",
                                            HistorySort::PeakMemory => "history.peak_tag",
                                        })),
                                ),
                        ),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .gap(px(8.))
                        .mt(px(2.))
                        .text_size(px(10.))
                        .text_color(theme::text_dim())
                        .child(
                            div().child(match sort {
                                HistorySort::CpuTime => t!(
                                    "history.pid_mem",
                                    pid = s.pid,
                                    mem = format::memory(s.peak_memory_bytes)
                                )
                                .to_string(),
                                HistorySort::PeakMemory => t!(
                                    "history.pid_cpu",
                                    pid = s.pid,
                                    cpu = format::core_time(s.cpu_time_ms)
                                )
                                .to_string(),
                            }),
                        )
                        // Peak beside total on purpose: a small peak next to a
                        // large total is exactly the process this view exists
                        // to find, and the pairing is what makes that legible.
                        .child(
                            div().font_family(font::MONO).child(
                                t!(
                                    "history.peak",
                                    cpu = format::pct(s.peak_cpu_percent),
                                    minutes = s.minutes
                                )
                                .to_string(),
                            ),
                        ),
                )
                .child(div().mt(px(6.)).child(widgets::meter(
                    match sort {
                        HistorySort::CpuTime => s.cpu_time_ms as f32,
                        HistorySort::PeakMemory => s.peak_memory_bytes as f32,
                    } / top as f32,
                    Hsla::from(theme::ink()),
                    4.,
                )))
                .into_any_element()
        }));

    let mut out = vec![list.into_any_element()];
    if rows.len() > TOP_N {
        out.push(widgets::note(
            t!("history.more", count = rows.len() - TOP_N).to_string(),
        ));
    }
    out.push(explainer());
    out
}

/// Why this list disagrees with every other tab, said once where it is read.
fn explainer() -> AnyElement {
    card()
        .child(widgets::card_header(i18n::tr("history.about"), None))
        .child(
            div()
                .mt(px(4.))
                .text_size(px(10.5))
                .line_height(gpui::relative(1.4))
                .text_color(theme::text_muted())
                .child(i18n::tr("history.about_body")),
        )
        .into_any_element()
}

/// Sort, range and refresh, side by side in the header.
fn header_controls(state: &ZStatsAppState, current: HistoryRange) -> AnyElement {
    h_flex()
        .items_center()
        .gap(px(2.))
        .child(sort_chip(state.history_sort()))
        .children(HistoryRange::ALL.into_iter().enumerate().map(|(i, range)| {
            let on = range == current;
            div()
                .id(("history-range", i))
                .flex_none()
                .rounded(px(4.))
                .px(px(5.))
                .py(px(1.))
                .text_size(px(9.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .when(on, |d| d.bg(theme::chip()).text_color(theme::text()))
                .when(!on, |d| {
                    d.text_color(theme::text_muted())
                        .hover(|d| d.bg(theme::surface_raised()).text_color(theme::text()))
                })
                .child(i18n::tr(range.label_key()))
                .on_click(move |_, _window, cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.set_history_range(range, cx));
                })
        }))
        .child(refresh_control())
        .into_any_element()
}

/// One button cycling the two orders, the process page's idiom — the
/// label names the order in force, the tooltip carries its caveat.
fn sort_chip(sort: HistorySort) -> AnyElement {
    let tip = i18n::tr(sort.tip_key());
    h_flex()
        .id("history-sort")
        .items_center()
        .rounded(px(4.))
        .px(px(5.))
        .py(px(1.))
        .mr(px(4.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .hover(|d| d.bg(theme::surface_raised()).text_color(theme::text()))
        .tooltip(widgets::wrap_tooltip(tip))
        .child(i18n::tr(sort.label_key()))
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.cycle_history_sort(cx));
        })
        .into_any_element()
}

fn refresh_control() -> AnyElement {
    h_flex()
        .id("history-refresh")
        .items_center()
        .gap(px(3.))
        .rounded(px(4.))
        .px(px(5.))
        .py(px(1.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .hover(|d| d.bg(theme::surface_raised()).text_color(theme::text()))
        .child(
            Icon::new(IconName::Redo)
                .with_size(Size::Size(px(10.)))
                .text_color(Hsla::from(theme::text_dim())),
        )
        .child(i18n::tr("history.refresh"))
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.load_history(cx));
        })
        .into_any_element()
}
