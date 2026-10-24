//! Who is moving bytes, on the Network tab above the listener list.
//!
//! The numbers are `zstats::process_traffic()`, diffed in [`crate::traffic`]
//! and kept for ten minutes. Nothing here asks for the read; views only
//! format. A row is a program, ranked by ↓+↑, and a program that moved
//! nothing is not a row — a machine has dozens of processes holding an
//! idle socket, and listing them is how the two that matter get buried.
//! The same reasoning sets the preview: only programs averaging over
//! 10 kB/s across the last minute, never fewer than three, the rest one
//! chip away. A few kB/s is keep-alives and telemetry, and a fixed six
//! filled up with them whenever nothing bigger was running.
//!
//! Under the numbers, one line of ↓+↑. Its height is that row's own
//! peak, but never below 64 KiB/s: a few hundred bytes a second must
//! not paint a shape, and a busy program must not be flattened by a
//! neighbour. The width is the card's real span — the same axis on
//! every row, labeled with how long it actually covers. There is no
//! threshold on this card, so the line stays ink. Accent is a crossed
//! line, and a fast download is not one.

use super::widgets;
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{TrafficReady, TrafficView, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use crate::traffic::{self, CurvePoint, ProgramRate};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Bounds, InteractiveElement, IntoElement, ParentElement, PathBuilder, Pixels,
    StatefulInteractiveElement, Styled, Window, div, point, px,
};
use gpui_kit::component::{h_flex, v_flex};
use rust_i18n::t;
use std::time::{Duration, Instant};
use zstats::OwnerCoverage;
use zstats::snapshot::Capabilities;

/// Height of the curve's well. Reserved on every row, including one that
/// does not have two points yet, so the list does not jump as a line
/// appears. It replaces the 4px bars; a third block under them would
/// have pushed the listener card off the panel. 14 was tried and
/// reverted: inside the well that left about 8px of swing, and a burst
/// flattened into the same faint ripple as noise.
const CURVE_H: f32 = 22.;

/// The well's inset from its rounded edge to the drawing — the same
/// recess as Overview's charts, scaled to this slot. Horizontal
/// keeps the line's ends off the corners; vertical keeps a flat 0 off
/// the well's floor, where it would read as the edge rather than a
/// reading.
const WELL_PAD_X: f32 = 4.;
const WELL_PAD_Y: f32 = 2.;

/// Corner of the well: Overview's 8px at 40px tall, scaled to the slot.
const WELL_RADIUS: f32 = 5.;

/// Thin enough to read as a trace, thick enough to survive the panel's
/// scale. The slot insets by a pixel so the stroke is not clipped.
const CURVE_STROKE: f32 = 1.5;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    // No such counters on this platform: no card, rather than an
    // apology on every visit. Windows is the one zstats refuses.
    if !Capabilities::current().process_traffic {
        return Vec::new();
    }
    let body = match state.traffic() {
        None | Some(TrafficView::Reading) => vec![note_line(i18n::tr("traffic.reading"))],
        Some(TrafficView::Restricted) => vec![note_line(i18n::tr("traffic.restricted"))],
        Some(TrafficView::Failed(error)) => {
            vec![note_line(
                t!("traffic.failed", error = error.clone()).to_string(),
            )]
        }
        Some(TrafficView::Ready(ready)) => return vec![ready_card(state, ready)],
    };
    vec![
        widgets::list_shell()
            .child(header(None, None))
            .children(body)
            .into_any_element(),
    ]
}

fn ready_card(state: &ZStatsAppState, ready: &TrafficReady) -> AnyElement {
    let rows = &ready.rows;
    if rows.is_empty() {
        let text = if ready.process_count == 0 {
            i18n::tr("traffic.none")
        } else {
            i18n::tr("traffic.quiet")
        };
        return widgets::list_shell()
            .child(header(None, Some(ready.coverage)))
            .child(note_line(text))
            .into_any_element();
    }
    // One `now` for every row, so a point sits on the same x in each.
    let now = Instant::now();
    let curves: Vec<&[CurvePoint]> = rows.iter().map(|row| state.traffic_curve(row)).collect();
    let preview = traffic::preview(&traffic::averages(rows, &curves, now));
    let hidden = rows.len() - preview.len();
    let show_all = state.show_all_traffic();
    let shown: Vec<usize> = if show_all {
        (0..rows.len()).collect()
    } else {
        preview
    };
    // The chip stays a toggle once expanded, including the sample where
    // the extras have gone quiet and `hidden` drops to zero.
    let chip = (hidden > 0 || show_all).then(|| more_chip(hidden, show_all));
    let span = state.traffic_span(now);
    let last = shown.len() - 1;
    widgets::list_shell()
        .child(header(trailing(span, chip), Some(ready.coverage)))
        .children(
            shown.iter().enumerate().map(|(i, &index)| {
                row_element(i, &rows[index], curves[index], span, now, i != last)
            }),
        )
        .into_any_element()
}

/// The real span, dim, beside the show-more chip. Absent until the book
/// holds a second — a fresh curve must not be labelled as if it were full.
fn trailing(span: Option<Duration>, chip: Option<AnyElement>) -> Option<AnyElement> {
    let label = span.map(traffic::span_label);
    if label.is_none() && chip.is_none() {
        return None;
    }
    Some(
        h_flex()
            .items_center()
            .gap(px(8.))
            .children(label.map(|text| {
                div()
                    .flex_none()
                    .text_size(px(10.))
                    .text_color(theme::text_dim())
                    .child(text)
            }))
            .children(chip)
            .into_any_element(),
    )
}

fn header(trailing: Option<AnyElement>, coverage: Option<OwnerCoverage>) -> AnyElement {
    widgets::list_header(
        h_flex()
            .items_center()
            .gap(px(4.))
            .child(i18n::tr("traffic.title"))
            .child(widgets::info_icon("traffic-tip", traffic::tip(coverage))),
        trailing,
    )
}

fn note_line(text: String) -> AnyElement {
    div()
        .px(px(widgets::CARD_PAD))
        .pb(px(11.))
        .text_size(px(11.))
        .text_color(theme::text_dim())
        .child(text)
        .into_any_element()
}

fn row_element(
    index: usize,
    row: &ProgramRate,
    points: &[CurvePoint],
    span: Option<Duration>,
    now: Instant,
    rule: bool,
) -> AnyElement {
    let name = row
        .name
        .clone()
        .unwrap_or_else(|| i18n::tr("traffic.unknown"));
    // The program's app icon, or its letter (`widgets::app_mark`): a
    // tunnel or a helper is often named for its job, not its app —
    // "MacPacketTunnel" is Shadowrocket. Every pid on a row carries the
    // row's name, so the first answers for all of them.
    let mark = widgets::app_mark(
        row.pids.first().copied().unwrap_or(0),
        row.name.as_deref().unwrap_or_default(),
        &name,
    );
    // Two lines, not three: who and how fast on one, the curve under
    // it. The rates used to have a line of their own with ↑ pushed to the
    // far edge, which parted the pair a reader compares and spent ~20px a
    // row; the pid sits after the name, dim, as the listening card has it.
    v_flex()
        .px(px(widgets::CARD_PAD))
        .py(px(8.))
        .when(rule, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        // The mark beside both lines, so the row is no taller for it.
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
                            .child(
                                h_flex()
                                    .items_baseline()
                                    .gap(px(6.))
                                    .flex_1()
                                    .min_w_0()
                                    .child(widgets::truncating_name(
                                        ("traffic-owner", index),
                                        name,
                                        11.,
                                        gpui::FontWeight::MEDIUM,
                                        theme::text().into(),
                                    ))
                                    .children(pid_label(index, row)),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap(px(8.))
                                    .font_family(font::MONO)
                                    .text_size(px(11.))
                                    .text_color(theme::text())
                                    .child(format!("↓ {}", format::rate(row.received_per_sec)))
                                    .child(format!("↑ {}", format::rate(row.transmitted_per_sec))),
                            ),
                    )
                    // On the recessed meter-track fill, as Overview's charts are.
                    // Without it a quiet program's flat 0 read as a row divider.
                    .child(
                        div()
                            .mt(px(4.))
                            .h(px(CURVE_H))
                            .px(px(WELL_PAD_X))
                            .py(px(WELL_PAD_Y))
                            .rounded(px(WELL_RADIUS))
                            .bg(theme::inset())
                            .child(sparkline(points, span, now)),
                    ),
            ),
        )
        .into_any_element()
}

/// The pid or `×N` after the name; the several case is where the pids
/// went, so the tooltip gives them back.
fn pid_label(index: usize, row: &ProgramRate) -> Option<AnyElement> {
    let el = div()
        .flex_none()
        .text_size(px(10.))
        .text_color(theme::text_dim())
        .child(traffic::pid_label(row)?);
    Some(match traffic::pids_tip(row) {
        Some(tip) => el
            .id(("traffic-pids", index))
            .tooltip(widgets::wrap_tooltip(tip))
            .into_any_element(),
        None => el.into_any_element(),
    })
}

fn more_chip(hidden: usize, showing: bool) -> AnyElement {
    let label = if showing {
        i18n::tr("traffic.hide")
    } else {
        t!("traffic.show_more", count = hidden).to_string()
    };
    div()
        .id("traffic-more")
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(if showing {
            theme::border()
        } else {
            theme::border_subtle()
        })
        .when(showing, |d| d.bg(theme::chip()))
        // Only while off: the "on" fill is the state, and a hover that
        // repainted it would read as the toggle having flipped.
        .when(!showing, |d| {
            d.hover(|d| d.bg(theme::surface_raised()).border_color(theme::border()))
        })
        .px(px(7.))
        .py(px(1.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(if showing {
            theme::text()
        } else {
            theme::text_dim()
        })
        .child(label)
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.toggle_all_traffic(cx));
        })
        .into_any_element()
}

/// One ↓+↑ line. The slot is always the same height; a single point,
/// or a span the header is not ready to name, paints nothing inside it.
fn sparkline(points: &[CurvePoint], span: Option<Duration>, now: Instant) -> AnyElement {
    let points = points.to_vec();
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            if let Some(span) = span {
                paint_sparkline(bounds, &points, span, now, window);
            }
        },
    )
    .size_full()
    .into_any_element()
}

fn paint_sparkline(
    bounds: Bounds<Pixels>,
    points: &[CurvePoint],
    span: Duration,
    now: Instant,
    window: &mut Window,
) {
    // Half the stroke, rounded up, so a peak on the top edge is not cut.
    let pad = px(1.);
    let top = bounds.top() + pad;
    let bottom = bounds.bottom() - pad;
    if bottom <= top {
        return;
    }
    let height = bottom - top;
    let at = |(x, y): (f32, f32)| point(bounds.left() + bounds.size.width * x, bottom - height * y);
    let mut builder = PathBuilder::stroke(px(CURVE_STROKE));
    let mut drew = false;
    for run in traffic::trace(points, span, now) {
        let mut steps = run.into_iter();
        let Some(first) = steps.next() else {
            continue;
        };
        builder.move_to(at(first));
        for step in steps {
            builder.line_to(at(step));
        }
        drew = true;
    }
    if !drew {
        return;
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, theme::ink());
    }
}
