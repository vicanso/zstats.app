//! Overview's charts on an NSH canvas: a port of the gpui front end's
//! `sparkline`, `paint_guides`, `paint_gaps`, `paint_curve` and
//! `paint_bars` (`views/overview.rs`), over the shared arithmetic
//! (`chart.rs`) — whose comments say why each mark is drawn as it is.
//!
//! A canvas paints in plain colours, so the palette is read in the half
//! NSH resolved (`palette::in_mode` over NSH's `dark()`), and the canvas
//! draws again when that or the store changes. Its layer is drawn in place
//! rather than asynchronously (`host::draw_canvases_in_place`, the
//! prototype's finding 1).

use crate::chart::{
    self, AREA_ALPHA, CHART_BUCKETS, CHART_DIM, CHART_PAD, CHART_WELL_RADIUS, CURVE_STROKE,
    CurveStroke, GAP_DOT, GAP_DOT_STEP,
};
use crate::color::Rgba;
use crate::nsh::theme;
use crate::palette;
use crate::series::{self, Point};
use nsh_core::canvas::{CanvasGradient, LineJoin};
use nsh_core::ui::prelude::*;
use std::time::{Duration, Instant};

/// What one chart draws: a bucketed series per line, the lead first, how
/// far back the window reaches, the axis top and how the lines are drawn.
pub struct Chart {
    pub marks: Vec<Vec<Point>>,
    pub span: Option<Duration>,
    pub now: Instant,
    pub scale: f64,
    pub stroke: CurveStroke,
}

/// A chart in its well, `height` tall, with what `corner` says written in
/// its top-left corner (the axis top, once there is a line to read it
/// against) — `overview::cpu_pulse` and the long form's charts. `chart` is
/// read again whenever what it reads changes.
pub fn well(
    chart: impl Fn() -> Chart + 'static,
    height: f32,
    corner: impl Fn() -> Option<String> + 'static,
) -> View {
    let size = signal((0.0_f32, 0.0_f32));
    view()
        .relative()
        .w_full()
        .h(px(height))
        .rounded(px(CHART_WELL_RADIUS))
        .bg(theme::color(palette::inset))
        .overflow_hidden()
        .child(
            canvas(move || {
                let (w, h) = size.get();
                let dark = dark();
                let chart = chart();
                palette::in_mode(dark, || paint(&chart, w, h))
            })
            .absolute()
            .size_full()
            .on_layout(move |b: &Bounds<Points>| {
                // A layout event already queued for the box a region's
                // previous run made is still delivered after the re-run
                // dropped that run's signals (longbridge/nsh#448).
                let _ = size.try_set((b.size.width.0, b.size.height.0));
            }),
        )
        .child(
            text(move || corner().unwrap_or_default())
                .absolute()
                .top(px(2.))
                .left(px(CHART_PAD))
                .tabular_nums()
                .text_size(px(9.))
                .line_height(px(11.))
                .text_color(theme::color(palette::text_dim)),
        )
}

/// A palette colour as the canvas takes it.
pub fn ink(c: Rgba) -> Hsla {
    let c: nsh_core::Rgba = c.into();
    c.into()
}

/// `palette::gauge` at [`CHART_DIM`]: the history behind the lead, and
/// the memory meter's compressed slice.
pub fn dim_ink() -> Rgba {
    let gauge = palette::gauge();
    Rgba {
        a: gauge.a * CHART_DIM,
        ..gauge
    }
}

fn paint(chart: &Chart, w: f32, h: f32) -> Painter {
    let mut p = Painter::new();
    let (left, top) = (CHART_PAD, CHART_PAD);
    let (width, bottom) = (w - CHART_PAD * 2., h - CHART_PAD);
    if width <= 0. || bottom <= top {
        return p;
    }
    p = guides(p, chart.span, left, width, h);
    if chart.span.is_none() {
        return p;
    }
    // Behind first, so the lead line is painted on top; only the lead has
    // a wash.
    for (index, points) in chart.marks.iter().enumerate().rev() {
        let lead = index == 0;
        let look = if lead { palette::gauge() } else { dim_ink() };
        p = match chart.stroke {
            CurveStroke::Smooth => curve(p, points, chart, left, width, top, bottom, look, lead),
            CurveStroke::Bars => bars(p, points, chart, left, width, top, bottom),
        };
    }
    if let Some(lead) = chart.marks.first() {
        p = gaps(p, lead, chart, left, width, bottom - 1.);
    }
    p
}

/// Hairlines at every `GUIDE_EVERY` the span has reached, whole pixels,
/// full well height, under the stroke.
fn guides(mut p: Painter, span: Option<Duration>, left: f32, width: f32, h: f32) -> Painter {
    let mut age = chart::GUIDE_EVERY;
    while age < series::WINDOW {
        if chart::guide_due(age, span) {
            let x = (left + width * chart::axis_t(age) as f32).round();
            p = p
                .fill_color(ink(palette::border_subtle()))
                .fill_rect(x, 0., 1., h);
        }
        age += chart::GUIDE_EVERY;
    }
    p
}

#[allow(clippy::too_many_arguments)]
fn curve(
    mut p: Painter,
    points: &[Point],
    chart: &Chart,
    left: f32,
    width: f32,
    top: f32,
    bottom: f32,
    look: Rgba,
    lead: bool,
) -> Painter {
    if chart.scale <= 0. {
        return p;
    }
    let height = bottom - top;
    let x_of =
        |at: Instant| left + width * chart::axis_t(chart.now.saturating_duration_since(at)) as f32;
    let y_of = |value: f64| bottom - height * (value / chart.scale).clamp(0., 1.) as f32;
    for run in chart::runs(points) {
        let plotted: Vec<(f32, f32)> = run.iter().map(|(at, v)| (x_of(*at), y_of(*v))).collect();
        let Some(&(x0, y0)) = plotted.first() else {
            continue;
        };
        // One bucket has nothing to bend through (`chart::lone_reading`).
        if plotted.len() == 1 {
            let slice = width / CHART_BUCKETS as f32;
            let (from, to) = chart::lone_reading(x0, slice, left, left + width);
            p = p
                .stroke_color(ink(look))
                .line_width(CURVE_STROKE)
                .begin_path()
                .move_to(from, y0)
                .line_to(to, y0)
                .stroke();
            continue;
        }
        let cubics = chart::smooth_cubics(&plotted);
        let last = plotted[plotted.len() - 1];
        if lead {
            // Under the stroke, from the line's highest point down to
            // nothing, each run closed down to the axis.
            let peak = plotted.iter().map(|&(_, y)| y).fold(bottom, f32::min);
            let wash = CanvasGradient::linear(point(px(0.), px(peak)), point(px(0.), px(bottom)))
                .add_color_stop(
                    Percentage(0.),
                    ink(Rgba {
                        a: look.a * AREA_ALPHA,
                        ..look
                    }),
                )
                .add_color_stop(Percentage(1.), ink(Rgba { a: 0., ..look }));
            p = bezier(p.begin_path().move_to(x0, bottom).line_to(x0, y0), &cubics)
                .line_to(last.0, bottom)
                .close_path()
                .fill_style(wash)
                .fill();
        }
        p = bezier(p.begin_path().move_to(x0, y0), &cubics)
            .stroke_color(ink(look))
            .line_width(CURVE_STROKE)
            .line_join(LineJoin::Round)
            .stroke();
    }
    p
}

fn bezier(mut p: Painter, cubics: &[chart::Cubic]) -> Painter {
    for c in cubics {
        p = p.bezier_curve_to(
            point(px(c.ctrl_a.0), px(c.ctrl_a.1)),
            point(px(c.ctrl_b.0), px(c.ctrl_b.1)),
            point(px(c.to.0), px(c.to.1)),
        );
    }
    p
}

/// One bar per reading with a value, from the axis up; only the newest in
/// the full ink, the history dimmed.
#[allow(clippy::too_many_arguments)]
fn bars(
    mut p: Painter,
    points: &[Point],
    chart: &Chart,
    left: f32,
    width: f32,
    top: f32,
    bottom: f32,
) -> Painter {
    if chart.scale <= 0. {
        return p;
    }
    let height = bottom - top;
    let newest = points.iter().rposition(|sample| sample.value.is_some());
    for (position, sample) in points.iter().enumerate() {
        let Some(value) = sample.value else {
            continue;
        };
        let Some(index) = chart::bucket_index(sample.at, chart.now) else {
            continue;
        };
        let bar_top = bottom - height * (value / chart.scale).clamp(0., 1.) as f32;
        let (bar_left, bar_right) = chart::bar_edges(index, left, width);
        if bar_top >= bottom || bar_right <= bar_left {
            continue;
        }
        let look = if Some(position) == newest {
            palette::gauge()
        } else {
            dim_ink()
        };
        p = p.fill_color(ink(look)).fill_rect(
            bar_left,
            bar_top,
            bar_right - bar_left,
            bottom - bar_top,
        );
    }
    p
}

/// A dotted baseline across every hole between the lead line's readings
/// (`overview::paint_gaps`): time with nothing recorded.
fn gaps(mut p: Painter, points: &[Point], chart: &Chart, left: f32, width: f32, y: f32) -> Painter {
    let x_of =
        |at: Instant| left + width * chart::axis_t(chart.now.saturating_duration_since(at)) as f32;
    let mut holes = Vec::new();
    match chart.stroke {
        CurveStroke::Smooth => {
            let half = width / CHART_BUCKETS as f32 / 2.;
            let runs = chart::runs(points);
            for pair in runs.windows(2) {
                let (prev, next) = (&pair[0], &pair[1]);
                let (Some(last), Some(first)) = (prev.last(), next.first()) else {
                    continue;
                };
                let from = x_of(last.0) + if prev.len() == 1 { half } else { 0. };
                let to = x_of(first.0) - if next.len() == 1 { half } else { 0. };
                holes.push((from, to));
            }
        }
        CurveStroke::Bars => {
            let mut slots: Vec<usize> = points
                .iter()
                .filter(|sample| sample.value.is_some())
                .filter_map(|sample| chart::bucket_index(sample.at, chart.now))
                .collect();
            slots.sort_unstable_by(|a, b| b.cmp(a));
            slots.dedup();
            for pair in slots.windows(2) {
                if pair[0] - pair[1] < 2 {
                    continue;
                }
                let (_, from) = chart::bar_edges(pair[0], left, width);
                let (to, _) = chart::bar_edges(pair[1], left, width);
                holes.push((from, to));
            }
        }
    }
    p = p.fill_color(ink(dim_ink()));
    for (from, to) in holes {
        // A hole narrower than two dots says nothing a dot could.
        if to - from < GAP_DOT_STEP * 2. {
            continue;
        }
        let mut x = from + (GAP_DOT_STEP - GAP_DOT) / 2.;
        while x + GAP_DOT <= to {
            p = p.fill_rect(x.round(), y, GAP_DOT, 1.);
            x += GAP_DOT_STEP;
        }
    }
    p
}
