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
use std::cmp::Reverse;
use std::time::{Duration, Instant};
use zstats::OwnerCoverage;
use zstats::snapshot::Capabilities;

/// A program averaging more than this over [`AVERAGE_WINDOW`] gets a
/// row before "show more". 10 kB/s as the card prints it — `format::rate`
/// is binary — so a row reading "9.8 kB/s" all minute is not one. Below
/// it is housekeeping. No cap above it: every program over the floor is
/// what the card is for, and the floor is already the cut.
const SHOWN_FLOOR: u64 = 10 * 1024;

/// The last minute of each row's curve. Long enough that a row does not
/// come and go with each 2s reading, short enough that a download which
/// finished minutes ago stops holding a row its live rate no longer earns.
const AVERAGE_WINDOW: Duration = Duration::from_secs(60);

/// Rows shown even when fewer programs clear the floor, filled by the
/// highest averages. A quiet machine still says who is talking at all,
/// rather than collapsing to a chip.
const MIN_ROWS: usize = 3;

/// Floor for a row's curve. Same value as the interface card's bars:
/// below 64 KiB/s the line stays on the axis, and a few kB/s of
/// housekeeping must not get to define "full" for itself.
const SCALE_FLOOR_BYTES: f32 = 64.0 * 1024.0;

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
    // Every row moved bytes on the reading that ranked it, and that
    // reading is on its curve; the row's own rate covers a curve that
    // somehow has nothing in the window.
    let averages: Vec<u64> = rows
        .iter()
        .zip(&curves)
        .map(|(row, points)| {
            traffic::recent_average(points, now, AVERAGE_WINDOW)
                .unwrap_or_else(|| traffic::total(row))
        })
        .collect();
    let preview = preview(&averages);
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

/// Which rows show before "show more", as indices in ranking order:
/// every program over [`SHOWN_FLOOR`], then the next-highest averages
/// until there are [`MIN_ROWS`]. The minute's average picks the rows;
/// the live ranking still orders them, as it orders the expanded list.
fn preview(averages: &[u64]) -> Vec<usize> {
    let over = averages.iter().filter(|&&avg| avg > SHOWN_FLOOR).count();
    let mut by_average: Vec<usize> = (0..averages.len()).collect();
    // Stable, so a tie goes to the busier row in the live ranking.
    by_average.sort_by_key(|&index| Reverse(averages[index]));
    by_average.truncate(over.max(MIN_ROWS));
    by_average.sort_unstable();
    by_average
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
            .child(widgets::info_icon("traffic-tip", tip(coverage))),
        trailing,
    )
}

/// What the number is, and the one way this platform's counters differ.
/// The platform sentence is always in both catalogues; which one is
/// appended depends on where this binary runs.
fn tip(coverage: Option<OwnerCoverage>) -> String {
    let mut tip = i18n::tr("traffic.tip");
    let platform = if cfg!(target_os = "linux") {
        i18n::tr("traffic.tip_linux")
    } else {
        i18n::tr("traffic.tip_macos")
    };
    tip = format!("{tip} {platform}");
    if coverage == Some(OwnerCoverage::OwnProcessesOnly) {
        tip = format!("{tip} {}", i18n::tr("traffic.tip_partial"));
    }
    tip
}

fn note_line(text: String) -> AnyElement {
    div()
        .px(px(13.))
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
    // Two lines, not three: who and how fast on one, the curve under
    // it. The rates used to have a line of their own with ↑ pushed to the
    // far edge, which parted the pair a reader compares and spent ~20px a
    // row; the pid sits after the name, dim, as the listening card has it.
    v_flex()
        .px(px(13.))
        .py(px(8.))
        .when(rule, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
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
        )
        .into_any_element()
}

/// `pid N` for one process, `×N` when the row stands for several — a bare
/// number in that spot read as a count. The several case is where the
/// pids went, so the tooltip gives them back.
fn pid_label(index: usize, row: &ProgramRate) -> Option<AnyElement> {
    let label = match row.pids.len() {
        0 => return None,
        1 => t!("processes.pid_only", pid = row.pids.iter().next()?).to_string(),
        n => format!("×{n}"),
    };
    let el = div()
        .flex_none()
        .text_size(px(10.))
        .text_color(theme::text_dim())
        .child(label);
    if row.pids.len() < 2 {
        return Some(el.into_any_element());
    }
    let pids = row
        .pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Some(
        el.id(("traffic-pids", index))
            .tooltip(widgets::wrap_tooltip(
                t!("traffic.pids_tip", pids = pids).to_string(),
            ))
            .into_any_element(),
    )
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

/// The row's own peak, but never under the floor. A quiet program then
/// sits on the axis instead of turning a few hundred bytes into a wave.
fn curve_scale(points: &[CurvePoint]) -> f32 {
    points
        .iter()
        .filter_map(|point| point.bytes_per_sec)
        .max()
        .map_or(SCALE_FLOOR_BYTES, |peak| {
            (peak as f32).max(SCALE_FLOOR_BYTES)
        })
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
    let span_secs = span.as_secs_f64();
    if span_secs <= 0.0 {
        return;
    }
    let scale = curve_scale(points);
    // Half the stroke, rounded up, so a peak on the top edge is not cut.
    let pad = px(1.);
    let top = bounds.top() + pad;
    let bottom = bounds.bottom() - pad;
    if bottom <= top {
        return;
    }
    let height = bottom - top;
    let x_of = |at: Instant| {
        let age = now.saturating_duration_since(at).as_secs_f64();
        let t = (1.0 - age / span_secs).clamp(0.0, 1.0);
        bounds.left() + bounds.size.width * (t as f32)
    };
    let y_of = |rate: u64| {
        let h = (rate as f32 / scale).clamp(0.0, 1.0);
        bottom - height * h
    };
    let mut builder = PathBuilder::stroke(px(CURVE_STROKE));
    let mut drew = false;
    // Before its first moved byte a program was quiet or not in the
    // table — 0 either way, as `CurveBook::record` draws it once a line
    // exists — so the line starts at the card's left edge, flat, and
    // rises where the program did, instead of appearing mid-row. The
    // left edge is the book's oldest sample, so a line that started
    // there needs nothing.
    if let Some(first) = points.first()
        && let Some(rate) = first.bytes_per_sec
        && now.saturating_duration_since(first.at) < span
    {
        let floor = y_of(0);
        let x = x_of(first.at);
        builder.move_to(point(bounds.left(), floor));
        builder.line_to(point(x, floor));
        builder.line_to(point(x, y_of(rate)));
        drew = true;
    }
    for segment in traffic::segments(points) {
        let mut steps = segment.into_iter();
        let Some((at, rate)) = steps.next() else {
            continue;
        };
        builder.move_to(point(x_of(at), y_of(rate)));
        for (at, rate) in steps {
            builder.line_to(point(x_of(at), y_of(rate)));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn point(bytes: u64) -> CurvePoint {
        CurvePoint {
            at: Instant::now(),
            bytes_per_sec: Some(bytes),
        }
    }

    #[test]
    fn the_preview_is_every_row_over_the_floor_in_ranking_order() {
        let busy = SHOWN_FLOOR + 1;
        assert_eq!(
            preview(&[busy, 0, busy, busy, 10, busy]),
            vec![0, 2, 3, 5],
            "no cap above the floor; quiet rows wait behind the chip"
        );
        assert_eq!(
            preview(&[SHOWN_FLOOR, busy]),
            vec![0, 1],
            "exactly at the floor is not over it, but two rows are under three"
        );
    }

    #[test]
    fn fewer_than_three_over_the_floor_are_filled_by_the_highest_averages() {
        let busy = SHOWN_FLOOR * 4;
        assert_eq!(
            preview(&[200, 9_000, busy, 0, 5_000]),
            vec![1, 2, 4],
            "the minute's average picks, the live ranking orders"
        );
        assert_eq!(
            preview(&[100, 100, 100, 100]),
            vec![0, 1, 2],
            "a tie goes to the busier row right now"
        );
        assert_eq!(preview(&[0]), vec![0]);
    }

    #[test]
    fn the_line_follows_the_row_but_never_below_the_floor() {
        let scale = curve_scale(&[point(11_000)]);
        assert_eq!(scale, SCALE_FLOOR_BYTES, "11 kB/s does not get to be full");

        let scale = curve_scale(&[point(5 * 1024 * 1024)]);
        assert_eq!(scale, 5.0 * 1024.0 * 1024.0, "the row's own peak is full");

        assert_eq!(curve_scale(&[]), SCALE_FLOOR_BYTES);
        let hole = [CurvePoint {
            at: Instant::now(),
            bytes_per_sec: None,
        }];
        assert_eq!(curve_scale(&hole), SCALE_FLOOR_BYTES);
    }
}
