//! History: what actually burned the CPU today.
//!
//! The only view here ranked by an *amount* rather than a rate. Everything
//! else answers "how busy is it right now", which structurally cannot see the
//! process that never looks busy and yet costs the most over a day — see
//! [`crate::history`].

use super::widgets;
use crate::assets::CustomIconName;
use crate::font;
use crate::format;
use crate::history::{self, HistoryShape};
use crate::i18n;
use crate::state::{HistoryRange, HistorySort, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, div, px,
};
use gpui_component::{Icon, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::time::Duration;

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
        ordered.sort_by_key(|s| Reverse(s.peak_memory_bytes));
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
    let has_note = rows.len() > TOP_N;

    // Two `yes` rows with different pids read as a duplicate at a squint;
    // a repeated name gets its pid inline so identity is visible without
    // reading the caption line.
    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for s in &shown {
        *name_counts.entry(s.name.as_str()).or_default() += 1;
    }

    let list = widgets::list_shell()
        .child(widgets::list_header(
            // The note this carries used to be a card of its own under
            // the list. It is a read-once fact about how the ranking
            // works, which is what an ⓘ beside the title is for (the
            // Processes header does the same with its CPU basis) — and
            // moving it out is what lets the list card be exactly one
            // screen tall, so the header can stay put for real.
            h_flex()
                .items_center()
                .gap(px(4.))
                .min_w_0()
                .child(div().min_w_0().truncate().child(title))
                .child(widgets::info_icon(
                    "history-basis",
                    i18n::tr("history.about_body"),
                )),
            Some(header_controls(state, range)),
        ))
        .child(
            // The rows scroll under the header rather than taking the
            // whole card past the top of the panel — the same pinned
            // model as the Processes and Apps lists, and the reason is
            // the same: the range and sort controls live in that header,
            // and a control you have to scroll back up to reach is a
            // control you stop using.
            v_flex()
                .id("history-rows")
                .track_scroll(state.history_rows_scroll())
                .overflow_y_scroll()
                .max_h(px(rows_height(state, has_note)))
                .children(shown.into_iter().enumerate().map(|(i, s)| {
                    let repeated = name_counts[s.name.as_str()] > 1;
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
                                        .child(widgets::truncating_name(
                                            ("hist-name", s.pid as usize),
                                            s.name.clone(),
                                            12.,
                                            gpui::FontWeight::MEDIUM,
                                            Hsla::from(theme::text()),
                                        ))
                                        .when(repeated, |d| {
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
                                                .text_color(theme::tiny_label(theme::text_dim()))
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
                                    h_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .items_center()
                                        .gap(px(6.))
                                        .children(s.shape.map(|shape| {
                                            shape_pill(
                                                ("hist-shape", s.pid as usize),
                                                shape,
                                                s.peak_cpu_percent,
                                                s.span,
                                                s.minutes,
                                            )
                                        }))
                                        .child(
                                            // The pid is said once per row. A repeated
                                            // name already carries it inline above,
                                            // where the eye needs it to tell two rows
                                            // apart; repeating it here was the same
                                            // fact twice on one row.
                                            div().min_w_0().truncate().child(
                                                match (sort, repeated) {
                                                    (HistorySort::CpuTime, false) => t!(
                                                        "history.pid_mem",
                                                        pid = s.pid,
                                                        mem = format::memory(s.peak_memory_bytes)
                                                    )
                                                    .to_string(),
                                                    (HistorySort::CpuTime, true) => t!(
                                                        "history.mem_only",
                                                        mem = format::memory(s.peak_memory_bytes)
                                                    )
                                                    .to_string(),
                                                    (HistorySort::PeakMemory, false) => t!(
                                                        "history.pid_cpu",
                                                        pid = s.pid,
                                                        cpu = format::core_time(s.cpu_time_ms)
                                                    )
                                                    .to_string(),
                                                    (HistorySort::PeakMemory, true) => t!(
                                                        "history.cpu_only",
                                                        cpu = format::core_time(s.cpu_time_ms)
                                                    )
                                                    .to_string(),
                                                },
                                            ),
                                        ),
                                )
                                // Peak beside total on purpose: a small peak next to a
                                // large total is exactly the process this view exists
                                // to find, and the pairing is what makes that legible.
                                .child(
                                    // Same as the volume card's R/W
                                    // line: zh spells this "峰值 … ·
                                    // … 分钟", Han characters the
                                    // bundled mono face has no glyph
                                    // for.
                                    font::mono_unless_cjk(div())
                                        .flex_none()
                                        .min_w_0()
                                        .truncate()
                                        .child(
                                            t!(
                                                "history.peak",
                                                cpu = format::pct(s.peak_cpu_percent),
                                                minutes = s.minutes
                                            )
                                            .to_string(),
                                        ),
                                ),
                        )
                        .child(div().mt(px(6.)).child(match &s.band {
                            // Today: WHEN it burned, on a shared
                            // 00:00 → now axis — the share meter it
                            // replaces repeated what the rank order
                            // and the headline number already said.
                            // Deliberately under both sorts: the axis
                            // is time, not a magnitude comparison, so
                            // reordering the rows does not change what
                            // a row's band means.
                            Some(cells) => band(cells),
                            // Wider windows: no single day to draw, the
                            // share meter stays.
                            None => widgets::meter(
                                match sort {
                                    HistorySort::CpuTime => s.cpu_time_ms as f32,
                                    HistorySort::PeakMemory => s.peak_memory_bytes as f32,
                                } / top as f32,
                                Hsla::from(theme::ink()),
                                4.,
                            ),
                        }))
                        .into_any_element()
                })),
        );

    let mut out = vec![list.into_any_element()];
    if has_note {
        out.push(widgets::note(
            t!("history.more", count = rows.len() - TOP_N).to_string(),
        ));
    }
    out
}

/// One day of a process's recorded half-hours: 00:00 at the left edge,
/// now at the right. Every row shares the axis, so two rows dark in the
/// same column were burning at the same time — the cross-process
/// correlation the ranked totals could never show.
///
/// Cells are quantized ink opacity, not a red ramp: accent is reserved
/// for over-threshold, and what a cell encodes is magnitude — the same
/// job a meter's length does, done in the same ink. The track showing
/// through means "no line in the file for this half hour", which is not
/// zero (records are conditional — see `history::Band`) and therefore
/// must not be painted as a value.
fn band(cells: &history::Band) -> AnyElement {
    // Only the lived part of the day: at nine in the morning a fixed
    // 24-hour axis would read as fifteen hours of mysterious quiet, and
    // squeeze the real morning into a quarter of the width.
    let now = jiff::Zoned::now();
    let lived_minutes = now.hour().max(0) as usize * 60 + now.minute().max(0) as usize;
    let lived = (lived_minutes / history::BAND_BUCKET_MINUTES + 1).min(history::BAND_BUCKETS);
    h_flex()
        .h(px(8.))
        .rounded(px(2.))
        .overflow_hidden()
        // `trough`, not `inset`: in dark mode inset is a heavy black,
        // and black segments alternating with lit cells read as a
        // barcode — figure and ground become guessable, and "black =
        // strong" is a print instinct that guesses wrong. The trough is
        // a near-card lift, so the picture is unambiguous: lit cells
        // float on an almost-invisible track, nothing reads as a black
        // value. Light mode is untouched in practice (the two tokens
        // nearly coincide there), where the white ground was never
        // ambiguous to begin with.
        .bg(theme::trough())
        .children((0..lived).map(|i| {
            div().flex_1().h_full().when_some(cells[i], |d, peak| {
                d.bg(Hsla::from(theme::ink()).opacity(band_alpha(peak)))
            })
        }))
        .into_any_element()
}

/// Four intensity steps for a cell's loudest minute. Quantized rather
/// than continuous so adjacent cells read as "same-ish" or "different"
/// instead of a smear; the breakpoints are a quarter of a core, one
/// core, three cores — the same one-core unit every CPU figure in the
/// app speaks. Display only, like every threshold in `views/`.
fn band_alpha(peak: f32) -> f32 {
    match peak {
        p if p < 25.0 => 0.30,
        p if p < 100.0 => 0.55,
        p if p < 300.0 => 0.80,
        _ => 1.0,
    }
}

/// The "N more processes" note and the gap above it — the only thing
/// left under this list now that the explainer moved into the header's
/// ⓘ.
///
/// Reserving exactly it is what makes the pin real: the card then adds
/// up to the body's height with the note, the tab has nothing left to
/// scroll, and the header cannot be scrolled away no matter what the
/// wheel is over. Reserved only when the note exists — a list that fits
/// keeps the row.
const TRAILING_NOTE: f32 = 26.;

/// The rows region's height — the same budget the Processes list gets,
/// less the note when there is one.
fn rows_height(state: &ZStatsAppState, has_note: bool) -> f32 {
    let trailing = if has_note { TRAILING_NOTE } else { 0. };
    (super::processes::rows_height(state) - trailing).max(160.)
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

fn shape_pill(
    id: impl Into<gpui::ElementId>,
    shape: HistoryShape,
    peak: f32,
    span: Duration,
    minutes: usize,
) -> AnyElement {
    let label = i18n::tr(match shape {
        HistoryShape::Spike => "history.shape_spike",
        HistoryShape::Sustained => "history.shape_sustained",
        HistoryShape::Intermittent => "history.shape_intermittent",
    });
    let tip = t!(
        "history.shape_tip",
        peak = format::pct(peak),
        span = format::span(span),
        minutes = minutes
    )
    .to_string();
    div()
        .id(id)
        .flex_none()
        .rounded(px(3.))
        .px(px(4.))
        .bg(theme::inset())
        .text_size(px(9.))
        .text_color(theme::tiny_label(theme::text_muted()))
        .tooltip(widgets::wrap_tooltip(tip))
        .child(label)
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
    // Icon only: the labelled chip was the first thing `list_header`
    // clipped at 320px ("Reloa"). The word lives on the tooltip.
    let tip = i18n::tr("history.refresh");
    div()
        .id("history-refresh")
        .flex_none()
        .rounded(px(4.))
        .px(px(4.))
        .py(px(1.))
        .hover(|d| d.bg(theme::surface_raised()))
        .tooltip(widgets::wrap_tooltip(tip))
        .child(
            Icon::from(CustomIconName::RefreshCw)
                .with_size(Size::Size(px(10.)))
                .text_color(Hsla::from(theme::text_dim())),
        )
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.load_history(cx));
        })
        .into_any_element()
}
