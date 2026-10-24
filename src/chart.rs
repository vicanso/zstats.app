//! The geometry of Overview's three recent-readings charts: how the
//! half-hour rings (`series.rs`) are averaged into the points a chart
//! draws, where those points sit on the axis, how tall the axis is, and
//! the monotone cubic the processor and network lines are stroked with.
//!
//! None of it ever needed gpui; it lived in `views/overview.rs` because
//! that is where it was drawn. It moved here so both front ends draw the
//! same chart from the same arithmetic (docs/nsh-migration.md, phase 1a):
//! the painting itself — paths, washes, guides — stays with each front
//! end, along with the inks it is painted in.

use crate::format;
use crate::series::{self, Point};
use std::time::{Duration, Instant};

/// Height of an Overview chart. CPU and network are curves; memory is
/// bars. Three of them, each with [`CURVE_TOP`] above and a
/// [`CHART_CAPTION`] under it, are what `placement::DEFAULT_WINDOW_SIZE`
/// grew by so this tab still ends a few pixels above the footer. The
/// well stays this tall in both themes: a ceiling change moves the
/// mark inside the slot and does not resize the card.
pub const CURVE_H: f32 = 40.;

/// Air above a curve. Part of the window-height budget, with [`CURVE_H`].
pub const CURVE_TOP: f32 = 4.;

/// Air between a chart and the caption under it. Same budget.
pub const CHART_CAPTION_GAP: f32 = 4.;

/// The caption itself: peak, latest sample, and collected span, in one
/// line. 10px type in 14px, so the line does not clip. Three charts
/// add this plus [`CHART_CAPTION_GAP`] to the panel height.
pub const CHART_CAPTION: f32 = 14.;

/// Stroke. [`CHART_PAD`] is wider than half of this, so a peak on the
/// top of the drawing is not cut by the well.
pub const CURVE_STROKE: f32 = 1.5;

/// Share of the lead line's alpha the wash under it starts with, at the
/// line's highest point; it fades to nothing at the axis. A line alone
/// in a 40-tall well read thin beside the memory card's bars, and the
/// wash gives a busy stretch the weight of an area. Faint on purpose:
/// sixty full-ink bars were once the brightest block in the panel for
/// the least news, and the line's shape is still the news here.
pub const AREA_ALPHA: f32 = 0.24;

/// Radius of the chart well. Nested inside the card's 12px corner, and
/// the same in both themes: only the fill changes with the theme.
pub const CHART_WELL_RADIUS: f32 = 8.;

/// How far the marks sit inside the well. The row stays [`CURVE_H`];
/// this only keeps a bar or a peak off the rounded edge.
pub const CHART_PAD: f32 = 4.;

/// How one run of samples is drawn.
#[derive(Clone, Copy)]
pub enum CurveStroke {
    /// Monotone cubic through the samples ([`smooth_cubics`]). CPU and
    /// the network rates are a new point every tick, and a polyline of
    /// those is a run of corners. The cubic bends between readings that
    /// differ and does not rise above a sample that was the peak. A
    /// network total that holds still has a flat tangent, so the hold
    /// stays a hold.
    Smooth,
    /// One bar per reading, from the axis up to the sample. Memory's
    /// percent moves slowly; a line through it reads as flat, and the
    /// bars keep each reading's share of the 40px slot.
    Bars,
}

/// CPU axis while every reading in the window stays at or under this.
/// A machine at 8% drawn on 0–100 is a stripe on the floor; 0–30 lets
/// that range use the slot. 12% then 14% stays on this top, so the
/// line does not rescale while the machine is in the quiet band.
pub const CPU_AXIS_LOW: f64 = 30.0;

/// Past [`CPU_AXIS_LOW`] the top is the next multiple of this above the
/// highest point: 33% draws on 0–40. It used to be the highest point
/// itself, which the corner label would have printed as "33.2%" and
/// moved on every decimal of a new high; a round step reads as an axis
/// and holds still until the peak crosses it.
pub const CPU_AXIS_STEP: f64 = 10.0;

/// Memory axis. The series is `used_percent`, so the top is the whole
/// machine. It does not zoom to the recent band — a line that grew
/// because the percent wobbled would read as the machine filling up.
pub const MEM_AXIS_TOP: f64 = 100.0;

/// Network ceiling while every point stays at or under this — the
/// [`CPU_AXIS_LOW`] of the network line. It was 64 KiB/s (the Network
/// tab's per-program floor), and everyday traffic swinging between
/// 100 and 300 kB/s in the first minutes doubled it 128 → 256 → 512 KiB
/// with each new high, so the line rescaled inside the well while the
/// window was still filling. 1 MB/s holds still across ordinary use;
/// past it the doubling resumes (2, 4, 8 MB/s), and a few kB/s of
/// background traffic stays on the floor. Not shared with the program
/// curves on purpose: those draw each program's own shape, this is the
/// whole machine.
pub const NET_SCALE_FLOOR: f64 = 1024.0 * 1024.0;

/// How many points a curve draws. The ring keeps every tick; this is
/// only the display. 30 minutes / 90 is 20s.
pub const CHART_BUCKETS: usize = 90;

/// How many memory bars the chart draws. Same 30-minute axis as the
/// curves, coarser so a bar stays a bar: 30 minutes / 60 is 30s.
pub const BAR_BUCKETS: usize = 60;

/// Share of each of the [`BAR_BUCKETS`] slots left as the gap between
/// bars. The rest is the bar. A quarter keeps a gap on the minimum
/// window and stays a hairline on the default panel.
pub const BAR_GAP_SHARE: f32 = 0.25;

// One slice is wider than the raw missed-sample gap, so an empty slice
// is a break and not a tick that merely arrived late. Both grids.
const _: () = assert!(series::WINDOW.as_secs() / CHART_BUCKETS as u64 > series::GAP.as_secs());
const _: () = assert!(series::WINDOW.as_secs() / BAR_BUCKETS as u64 > series::GAP.as_secs());

/// One display slice. [`series::WINDOW`] divides evenly by both grids.
pub fn slice(buckets: usize) -> Duration {
    series::WINDOW / buckets as u32
}

/// At most [`CHART_BUCKETS`] points, oldest first. CPU and network.
pub fn buckets(points: &[Point], now: Instant) -> Vec<Point> {
    bucketed(points, now, CHART_BUCKETS)
}

/// At most [`BAR_BUCKETS`] bars, oldest first. Memory.
pub fn bar_buckets(points: &[Point], now: Instant) -> Vec<Point> {
    bucketed(points, now, BAR_BUCKETS)
}

/// Averages `points` into `buckets` equal slices of [`series::WINDOW`].
///
/// A slice before the first sample is left out, so the left of the
/// 30-minute axis stays empty and the slice does not dilute the first
/// average. A slice with no reading between two that have one becomes
/// a `None`, so the line breaks there and a bar slot stays empty. A
/// `None` beside a reading in the same slice is skipped; it is not
/// counted as zero.
///
/// Averaged from the first sample, not only once the ring outgrows the
/// grid. At the 2s open cadence 90 raw samples cover three minutes, the
/// newest tenth of the axis — about 30px, a third of a pixel apiece, so
/// raw points there are noise rather than detail — and switching to
/// averages at the 91st would drop the peaks in one frame and rescale
/// the CPU top and the network ceiling with them.
fn bucketed(points: &[Point], now: Instant, buckets: usize) -> Vec<Point> {
    if buckets == 0 {
        return Vec::new();
    }
    let bucket = slice(buckets);
    let secs = bucket.as_secs();
    if secs == 0 {
        return Vec::new();
    }
    struct Acc {
        sum: f64,
        n: u32,
        saw_none: bool,
    }
    let mut acc = Vec::with_capacity(buckets);
    for _ in 0..buckets {
        acc.push(Acc {
            sum: 0.0,
            n: 0,
            saw_none: false,
        });
    }
    for point in points {
        let age = now.saturating_duration_since(point.at);
        if age >= series::WINDOW {
            continue;
        }
        let mut index = (age.as_secs() / secs) as usize;
        if index >= buckets {
            index = buckets - 1;
        }
        match point.value {
            Some(value) => {
                acc[index].sum += value;
                acc[index].n += 1;
            }
            None => acc[index].saw_none = true,
        }
    }
    let Some(oldest) = acc.iter().rposition(|slot| slot.n > 0 || slot.saw_none) else {
        return Vec::new();
    };
    let Some(newest) = acc.iter().position(|slot| slot.n > 0 || slot.saw_none) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for index in (newest..=oldest).rev() {
        let age = bucket * index as u32 + bucket / 2;
        let at = now.checked_sub(age).unwrap_or(now);
        let value = if acc[index].n > 0 {
            Some(acc[index].sum / f64::from(acc[index].n))
        } else {
            None
        };
        out.push(Point { at, value });
    }
    out
}

/// Runs the chart stroke can draw. A `None` bucket is the only break:
/// neighbouring buckets are one slice apart (20s on a curve, 30s on
/// the bars), and that step is the grid, not a missed sample. One
/// occupied bucket is still a reading.
pub fn runs(points: &[Point]) -> Vec<Vec<(Instant, f64)>> {
    let mut out = Vec::new();
    let mut run = Vec::new();
    for point in points {
        match point.value {
            Some(value) => run.push((point.at, value)),
            None => {
                if !run.is_empty() {
                    out.push(run);
                    run = Vec::new();
                }
            }
        }
    }
    if !run.is_empty() {
        out.push(run);
    }
    out
}

/// 30 while every point stays at or under 30. Once one goes over, the
/// next [`CPU_AXIS_STEP`] above the window's highest point, at most
/// 100 — a 33% peak draws on 0–40 instead of sitting low on 0–100.
/// Callers pass [`buckets`], so the top follows the points on
/// screen; the corner label ([`scale_label`]) prints it.
pub fn cpu_axis_top(points: &[Point]) -> f64 {
    let peak = points
        .iter()
        .filter_map(|point| point.value)
        .fold(0.0, f64::max);
    if peak <= CPU_AXIS_LOW {
        return CPU_AXIS_LOW;
    }
    ((peak / CPU_AXIS_STEP).ceil() * CPU_AXIS_STEP).min(100.0)
}

/// Where `age` sits on the 30-minute axis. `1` is now, the right edge.
/// `0` is [`series::WINDOW`] ago, the left edge. A short history stays
/// on the right. The time before the first sample is an empty stretch
/// of this axis, not a zero line.
pub fn axis_t(age: Duration) -> f64 {
    let window = series::WINDOW.as_secs_f64();
    if window <= 0.0 {
        return 1.0;
    }
    (1.0 - age.as_secs_f64() / window).clamp(0.0, 1.0)
}

/// The row's own peak, but never under the floor, and only on a
/// doubling. The exact peak would move the ceiling on every new high
/// and the whole line would jump inside the slot.
pub fn net_scale(points: &[Point]) -> f64 {
    let peak = points
        .iter()
        .filter_map(|point| point.value)
        .fold(NET_SCALE_FLOOR, f64::max);
    let mut step = NET_SCALE_FLOOR;
    // 64 KiB/s doubled forty times is 64 TiB/s. Past that, keep the
    // last step rather than loop.
    for _ in 0..40 {
        if step >= peak {
            return step;
        }
        step *= 2.0;
    }
    step
}

/// What a chart's samples are, so the readout formats them the way the
/// rest of the card does.
#[derive(Clone, Copy)]
pub enum ChartUnit {
    /// CPU and memory, `0–100`.
    Percent,
    /// Network download or upload, bytes per second.
    Rate,
}

/// Highest point the chart draws — the bucketed average the line or bar
/// is made of, not the raw tick. It was the raw reading: a two-second
/// spike set "Max 33%" while the 20s average under it barely bumped the
/// line, so the caption named a height nothing on the chart reached and
/// read as some older, all-time figure. There is no "now" beside it:
/// the latest reading is the card's own headline (CPU %, memory, the
/// rates row), and repeating it under the chart was the same number
/// twice.
pub fn peak(points: &[Point]) -> Option<f64> {
    points
        .iter()
        .filter_map(|point| point.value)
        .reduce(f64::max)
}

pub fn reading(unit: ChartUnit, value: Option<f64>) -> String {
    let Some(value) = value else {
        return format::PLACEHOLDER.to_string();
    };
    match unit {
        ChartUnit::Percent => format::pct(value as f32),
        ChartUnit::Rate => format::rate(Some(value.max(0.0).round() as u64)),
    }
}

/// The top of the axis, printed as an axis: a whole percent, or whole
/// MB/s for the network's doubling ceilings.
pub fn scale_label(unit: ChartUnit, scale: f64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    match unit {
        ChartUnit::Percent => format!("{scale:.0}%"),
        ChartUnit::Rate if scale >= MIB => format!("{:.0} MB/s", scale / MIB),
        ChartUnit::Rate => format::rate(Some(scale.max(0.0).round() as u64)),
    }
}

/// How far apart the time guides are. Two of them, at 10 and 20
/// minutes ago, split the 30-minute axis into thirds.
pub const GUIDE_EVERY: Duration = Duration::from_secs(10 * 60);

/// Whether the guide `age` back from now sits on time the series has
/// actually collected.
pub fn guide_due(age: Duration, span: Option<Duration>) -> bool {
    span.is_some_and(|span| span >= age)
}

/// The corner figure names the top of the axis, and it waits until the
/// stroke is a line. One occupied bucket is the dash [`lone_reading`]
/// draws — a couple of pixels on the right of an empty well — and beside
/// it "30%" is a second percentage with nothing under it to scale. Two
/// buckets are a segment, and the top is then what that segment is drawn
/// against.
pub fn stroke_is_a_line(points: &[Point]) -> bool {
    runs(points).iter().any(|run| run.len() >= 2)
}

/// Either series earning a line is enough: CPU has one, the network
/// chart has two, and one ceiling covers both.
pub fn corner_label_earned(marks: &[Vec<Point>]) -> bool {
    marks.iter().any(|points| stroke_is_a_line(points))
}

/// One dot of a gap's baseline, and the pitch between dots.
pub const GAP_DOT: f32 = 2.;
pub const GAP_DOT_STEP: f32 = 5.;

/// Share of `theme::gauge`'s alpha kept by the marks behind the lead:
/// network upload under download, every memory bar but the newest, and
/// the compressed slice of the memory meter.
pub const CHART_DIM: f32 = 0.45;

/// Horizontal extent of a run that holds one bucket: a flat dash at the
/// reading's height, one slice wide, centred on the bucket and kept
/// inside the drawing. It used to be a vertical from the axis up to
/// the reading, which is how a bar is read — "from 0 to here", beside
/// the memory card's real bars — when it is one level, the first piece
/// of the line the next bucket extends.
pub fn lone_reading(x: f32, slice: f32, left: f32, right: f32) -> (f32, f32) {
    let half = slice / 2.0;
    ((x - half).max(left), (x + half).min(right))
}

/// Which of the [`BAR_BUCKETS`] slots `at` falls in. `0` is the newest,
/// on the right. A bar is drawn at its centre, and the centre of slot
/// `n` is `n * 30s + 15s`, which divides back to `n`.
pub fn bucket_index(at: Instant, now: Instant) -> Option<usize> {
    let age = now.saturating_duration_since(at);
    if age >= series::WINDOW {
        return None;
    }
    let secs = slice(BAR_BUCKETS).as_secs();
    if secs == 0 {
        return None;
    }
    Some(((age.as_secs() / secs) as usize).min(BAR_BUCKETS - 1))
}

/// Edges of the bar in slot `index` (`0` is the rightmost). The chart
/// is split into [`BAR_BUCKETS`] equal slots, and [`BAR_GAP_SHARE`] of
/// each slot is the gap, half on either side of the bar. Sixty bars
/// and their gaps fill `width` exactly.
pub fn bar_edges(index: usize, left: f32, width: f32) -> (f32, f32) {
    let pitch = width / BAR_BUCKETS as f32;
    let inset = pitch * BAR_GAP_SHARE / 2.0;
    let slot_right = left + width - index as f32 * pitch;
    (slot_right - pitch + inset, slot_right - inset)
}

/// One cubic of [`smooth_cubics`]. `from` is where the previous cubic
/// ended; the stroke starts there and does not move again.
pub struct Cubic {
    pub ctrl_a: (f32, f32),
    pub ctrl_b: (f32, f32),
    pub to: (f32, f32),
}

/// Monotone cubic through `points`, as Bézier controls.
///
/// Fritsch–Carlson: the tangent at a sample is zero wherever the
/// slope changes sign, and each segment's tangents are shortened
/// until the curve stays between that segment's two samples. A plain
/// spline overshoots, and on a 0–100 axis an overshoot is a reading
/// the machine did not take. Two samples come back as the straight
/// chord. X is the pixel position, so a 5s tray step and a 2s open
/// step keep their own slopes.
pub fn smooth_cubics(points: &[(f32, f32)]) -> Vec<Cubic> {
    let n = points.len();
    if n < 2 {
        return Vec::new();
    }
    let slope = monotone_slopes(points);
    let mut out = Vec::with_capacity(n - 1);
    for i in 0..n - 1 {
        let (x0, y0) = points[i];
        let (x1, y1) = points[i + 1];
        let dx = (x1 - x0) / 3.0;
        out.push(Cubic {
            ctrl_a: (x0 + dx, y0 + slope[i] * dx),
            ctrl_b: (x1 - dx, y1 - slope[i + 1] * dx),
            to: (x1, y1),
        });
    }
    out
}

/// dy/dx at each sample. Interior points use the weighted harmonic
/// mean of the neighbouring secants; a sign change is a real peak
/// and gets a flat tangent. Endpoints use the three-point estimate,
/// then every segment is clamped so its Bézier cannot leave the
/// range of its two samples.
fn monotone_slopes(points: &[(f32, f32)]) -> Vec<f32> {
    let n = points.len();
    let mut h = vec![0.0; n - 1];
    let mut secant = vec![0.0; n - 1];
    for i in 0..n - 1 {
        h[i] = points[i + 1].0 - points[i].0;
        secant[i] = if h[i].abs() <= f32::EPSILON {
            0.0
        } else {
            (points[i + 1].1 - points[i].1) / h[i]
        };
    }
    let mut slope = vec![0.0; n];
    if n == 2 {
        slope[0] = secant[0];
        slope[1] = secant[0];
    } else {
        slope[0] = endpoint_slope(h[0], h[1], secant[0], secant[1]);
        let last = n - 2;
        slope[n - 1] = endpoint_slope(h[last], h[last - 1], secant[last], secant[last - 1]);
        for i in 1..n - 1 {
            if secant[i - 1] * secant[i] <= 0.0 {
                slope[i] = 0.0;
            } else {
                let w1 = 2.0 * h[i] + h[i - 1];
                let w2 = h[i] + 2.0 * h[i - 1];
                slope[i] = (w1 + w2) / (w1 / secant[i - 1] + w2 / secant[i]);
            }
        }
    }
    for i in 0..n - 1 {
        if secant[i].abs() <= f32::EPSILON {
            slope[i] = 0.0;
            slope[i + 1] = 0.0;
            continue;
        }
        let a = slope[i] / secant[i];
        let b = slope[i + 1] / secant[i];
        let sum = a * a + b * b;
        if sum > 9.0 {
            let tau = 3.0 / sum.sqrt();
            slope[i] *= tau;
            slope[i + 1] *= tau;
        }
    }
    slope
}

/// Slope at an end sample. A sign that disagrees with the first
/// secant would leave the data immediately, so it becomes flat; a
/// slope steeper than three times that secant is the same overshoot
/// the per-segment clamp removes, caught here before it bends the end.
fn endpoint_slope(h0: f32, h1: f32, d0: f32, d1: f32) -> f32 {
    let width = h0 + h1;
    if width.abs() <= f32::EPSILON {
        return 0.0;
    }
    let slope = ((2.0 * h0 + h1) * d0 - h0 * d1) / width;
    if slope * d0 <= 0.0 {
        0.0
    } else if d0 * d1 <= 0.0 && slope.abs() > 3.0 * d0.abs() {
        3.0 * d0
    } else {
        slope
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bezier_y(from_y: f32, cubic: &Cubic, t: f32) -> f32 {
        let u = 1.0 - t;
        u * u * u * from_y
            + 3.0 * u * u * t * cubic.ctrl_a.1
            + 3.0 * u * t * t * cubic.ctrl_b.1
            + t * t * t * cubic.to.1
    }

    #[test]
    fn the_cpu_curve_passes_through_the_samples_and_not_above_a_peak() {
        let peak = [(0.0, 10.0), (10.0, 80.0), (20.0, 12.0)];
        let cubics = smooth_cubics(&peak);
        assert_eq!(cubics.len(), 2);
        assert!((cubics[0].to.1 - 80.0).abs() < 1e-4);
        assert!((cubics[1].to.1 - 12.0).abs() < 1e-4);
        for (i, cubic) in cubics.iter().enumerate() {
            let low = peak[i].1.min(peak[i + 1].1);
            let high = peak[i].1.max(peak[i + 1].1);
            for step in 0..=8 {
                let y = bezier_y(peak[i].1, cubic, step as f32 / 8.0);
                assert!(
                    (low - 1e-3..=high + 1e-3).contains(&y),
                    "a sample of {high} must stay the high point, got {y}"
                );
            }
        }

        let chord = smooth_cubics(&[(0.0, 10.0), (9.0, 40.0)]);
        assert_eq!(chord.len(), 1);
        assert!(
            (chord[0].ctrl_a.1 - 20.0).abs() < 1e-3,
            "two samples stay a line"
        );
        assert!((chord[0].ctrl_b.1 - 30.0).abs() < 1e-3);

        let rising = [(0.0, 5.0), (4.0, 20.0), (14.0, 30.0)];
        let cubics = smooth_cubics(&rising);
        for (i, cubic) in cubics.iter().enumerate() {
            for step in 0..=8 {
                let y = bezier_y(rising[i].1, cubic, step as f32 / 8.0);
                assert!(
                    y + 1e-3 >= rising[i].1.min(rising[i + 1].1),
                    "a climb does not dip, got {y}"
                );
                assert!(
                    y <= rising[i].1.max(rising[i + 1].1) + 1e-3,
                    "a climb does not jump the next sample, got {y}"
                );
            }
        }
    }

    #[test]
    fn the_cpu_axis_stays_at_30_until_a_reading_goes_over_it() {
        let now = Instant::now();
        let point = |value: Option<f64>| Point { at: now, value };
        assert_eq!(cpu_axis_top(&[]), CPU_AXIS_LOW);
        assert_eq!(cpu_axis_top(&[point(None)]), CPU_AXIS_LOW);
        assert_eq!(cpu_axis_top(&[point(Some(12.0))]), CPU_AXIS_LOW);
        assert_eq!(cpu_axis_top(&[point(Some(29.9))]), CPU_AXIS_LOW);
        // Exactly 30 has not gone over the line, so the top stays 30.
        assert_eq!(cpu_axis_top(&[point(Some(30.0))]), CPU_AXIS_LOW);
        // Over it, the next 10% step above the peak, never past 100.
        assert_eq!(cpu_axis_top(&[point(Some(8.0)), point(Some(64.0))]), 70.0);
        assert_eq!(cpu_axis_top(&[point(Some(30.1))]), 40.0);
        assert_eq!(cpu_axis_top(&[point(Some(33.2))]), 40.0);
        assert_eq!(cpu_axis_top(&[point(Some(40.0))]), 40.0);
        assert_eq!(cpu_axis_top(&[point(Some(95.5))]), 100.0);
        assert_eq!(cpu_axis_top(&[point(Some(100.0))]), 100.0);
    }

    #[test]
    fn the_axis_label_waits_until_the_stroke_is_a_line() {
        let now = Instant::now();
        let point = |value: Option<f64>| Point { at: now, value };
        assert!(!corner_label_earned(&[]));
        assert!(!corner_label_earned(&[vec![point(None)]]));
        // One bucket is the dash on the right, label or not.
        assert!(!corner_label_earned(&[vec![point(Some(7.0))]]));
        assert!(!corner_label_earned(&[vec![
            point(None),
            point(Some(7.0)),
            point(None),
        ]]));
        assert!(corner_label_earned(&[vec![
            point(Some(7.0)),
            point(Some(8.0))
        ]]));
        // Either series earning the line is enough: one ceiling for both.
        assert!(corner_label_earned(&[
            vec![point(Some(1.0))],
            vec![point(Some(1.0)), point(Some(2.0))],
        ]));
    }

    #[test]
    fn a_guide_waits_until_the_span_reaches_it() {
        assert!(!guide_due(GUIDE_EVERY, None));
        assert!(!guide_due(GUIDE_EVERY, Some(Duration::from_secs(13))));
        assert!(guide_due(GUIDE_EVERY, Some(GUIDE_EVERY)));
        assert!(!guide_due(GUIDE_EVERY * 2, Some(GUIDE_EVERY)));
        assert!(guide_due(
            GUIDE_EVERY * 2,
            Some(GUIDE_EVERY * 2 + Duration::from_secs(1))
        ));
    }

    #[test]
    fn the_corner_label_reads_as_an_axis() {
        assert_eq!(scale_label(ChartUnit::Percent, 30.0), "30%");
        assert_eq!(scale_label(ChartUnit::Percent, 100.0), "100%");
        assert_eq!(scale_label(ChartUnit::Rate, 1024.0 * 1024.0), "1 MB/s");
        assert_eq!(
            scale_label(ChartUnit::Rate, 8.0 * 1024.0 * 1024.0),
            "8 MB/s"
        );
        // Under a MiB (not a ceiling the network line uses today) it
        // falls back to the rate formatter rather than printing "0 MB/s".
        assert_eq!(
            scale_label(ChartUnit::Rate, 512.0 * 1024.0),
            format::rate(Some(512 * 1024))
        );
    }

    #[test]
    fn a_short_history_stays_on_the_right_of_the_thirty_minute_axis() {
        assert!((axis_t(Duration::ZERO) - 1.0).abs() < 1e-9);
        assert!(axis_t(series::WINDOW).abs() < 1e-9);
        assert!((axis_t(series::WINDOW / 2) - 0.5).abs() < 1e-9);
        // 40s is the right edge of a 30-minute axis. The left stays empty.
        let t = axis_t(Duration::from_secs(40));
        assert!(t > 0.95, "40s sits on the right, got {t}");
    }

    #[test]
    fn sixty_bars_split_the_chart_and_a_new_bar_sits_on_the_right() {
        let left = 10.0;
        let width = 270.0;
        let pitch = width / BAR_BUCKETS as f32;
        let gap = pitch * BAR_GAP_SHARE;
        let (newest_left, newest_right) = bar_edges(0, left, width);
        let (oldest_left, oldest_right) = bar_edges(BAR_BUCKETS - 1, left, width);
        assert!((newest_right - (left + width - gap / 2.0)).abs() < 1e-3);
        assert!((oldest_left - (left + gap / 2.0)).abs() < 1e-3);
        assert!(((newest_right - newest_left) - (pitch - gap)).abs() < 1e-3);
        // The gap between the two newest bars is the share of one slot.
        let (_, next_right) = bar_edges(1, left, width);
        assert!((newest_left - next_right - gap).abs() < 1e-3);
        // Outer edges of the first and last slots meet the chart edges.
        let outer = (newest_right + gap / 2.0) - (oldest_left - gap / 2.0);
        assert!((outer - width).abs() < 1e-2);
        assert!(oldest_right < newest_left);
        let now = Instant::now();
        let at = now.checked_sub(slice(BAR_BUCKETS) / 2).unwrap();
        assert_eq!(bucket_index(at, now), Some(0));
    }

    #[test]
    fn memory_draws_sixty_bars_of_thirty_seconds() {
        assert_eq!(slice(BAR_BUCKETS), Duration::from_secs(30));
        assert_eq!(slice(CHART_BUCKETS), Duration::from_secs(20));
        let now = Instant::now();
        // 5s and 20s share the newest 30s slice. 45s is the slice before.
        let bars = bar_buckets(
            &[
                Point {
                    at: ago(now, 45),
                    value: Some(6.0),
                },
                Point {
                    at: ago(now, 20),
                    value: Some(10.0),
                },
                Point {
                    at: ago(now, 5),
                    value: Some(30.0),
                },
            ],
            now,
        );
        assert_eq!(
            bars.iter().map(|point| point.value).collect::<Vec<_>>(),
            vec![Some(6.0), Some(20.0)]
        );
        let mut full = Vec::new();
        for step in 0..900 {
            full.push(Point {
                at: ago(now, step * 2),
                value: Some(1.0),
            });
        }
        assert_eq!(bar_buckets(&full, now).len(), BAR_BUCKETS);
        assert_eq!(buckets(&full, now).len(), CHART_BUCKETS);
    }

    #[test]
    fn a_lone_reading_is_one_slice_wide_and_stays_in_the_drawing() {
        // Mid-axis: one slice, centred.
        assert_eq!(lone_reading(100.0, 4.0, 0.0, 300.0), (98.0, 102.0));
        // The newest bucket's centre is half a slice from the right
        // edge, so the dash ends on that edge rather than past it.
        assert_eq!(lone_reading(298.0, 4.0, 0.0, 300.0), (296.0, 300.0));
        assert_eq!(lone_reading(299.0, 4.0, 0.0, 300.0), (297.0, 300.0));
        assert_eq!(lone_reading(1.0, 4.0, 0.0, 300.0), (0.0, 3.0));
    }

    #[test]
    fn the_readout_is_the_highest_point_drawn_not_the_raw_spike() {
        // A 70% tick and a 10% tick in one 20s slice draw as 40%, and
        // the caption says 40%: the height the line actually reaches.
        let now = Instant::now();
        let drawn = buckets(
            &[
                Point {
                    at: ago(now, 8),
                    value: Some(70.0),
                },
                Point {
                    at: ago(now, 2),
                    value: Some(10.0),
                },
            ],
            now,
        );
        assert_eq!(peak(&drawn), Some(40.0));
    }

    #[test]
    fn the_peak_is_the_highest_value_of_what_it_is_given() {
        let now = Instant::now();
        let highest = peak(&[
            Point {
                at: ago(now, 30),
                value: Some(70.0),
            },
            Point {
                at: ago(now, 8),
                value: Some(10.0),
            },
            Point {
                at: ago(now, 2),
                value: Some(4.0),
            },
        ]);
        assert_eq!(highest, Some(70.0));
        // A network break ends nothing: the earlier peak remains.
        let broken = peak(&[
            Point {
                at: ago(now, 8),
                value: Some(10.0),
            },
            Point {
                at: ago(now, 2),
                value: None,
            },
        ]);
        assert_eq!(broken, Some(10.0));
        assert_eq!(peak(&[]), None);
        assert_eq!(reading(ChartUnit::Percent, Some(12.0)), "12.0%");
        assert_eq!(
            reading(ChartUnit::Rate, Some(2048.0)),
            format::rate(Some(2048))
        );
        assert_eq!(reading(ChartUnit::Percent, None), format::PLACEHOLDER);
    }

    fn ago(now: Instant, secs: u64) -> Instant {
        now.checked_sub(Duration::from_secs(secs)).unwrap()
    }

    #[test]
    fn the_chart_averages_twenty_second_slices_and_stops_at_ninety() {
        let now = Instant::now();
        // 10 and 30 in the newest slice average to 20. A slice with
        // only a missing rate stays a break, and it does not become 0.
        let sparse = buckets(
            &[
                Point {
                    at: ago(now, 50),
                    value: Some(6.0),
                },
                Point {
                    at: ago(now, 30),
                    value: None,
                },
                Point {
                    at: ago(now, 8),
                    value: Some(10.0),
                },
                Point {
                    at: ago(now, 2),
                    value: Some(30.0),
                },
            ],
            now,
        );
        assert_eq!(
            sparse.iter().map(|point| point.value).collect::<Vec<_>>(),
            vec![Some(6.0), None, Some(20.0)]
        );
        let runs = runs(&sparse);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].len(), 1);
        assert_eq!(runs[1][0].1, 20.0);
        // A slice with no sample at all, between two readings, is the
        // same break. It is not filled with zero.
        let hole = buckets(
            &[
                Point {
                    at: ago(now, 50),
                    value: Some(6.0),
                },
                Point {
                    at: ago(now, 5),
                    value: Some(4.0),
                },
            ],
            now,
        );
        assert_eq!(
            hole.iter().map(|point| point.value).collect::<Vec<_>>(),
            vec![Some(6.0), None, Some(4.0)]
        );

        // A missing rate beside a real reading is skipped, not averaged in.
        let mixed = buckets(
            &[
                Point {
                    at: ago(now, 6),
                    value: None,
                },
                Point {
                    at: ago(now, 2),
                    value: Some(10.0),
                },
            ],
            now,
        );
        assert_eq!(mixed.len(), 1);
        assert_eq!(mixed[0].value, Some(10.0));

        // The empty slices before the first sample are not points.
        let young = buckets(
            &[Point {
                at: ago(now, 5),
                value: Some(4.0),
            }],
            now,
        );
        assert_eq!(young.len(), 1);

        let mut full = Vec::new();
        for step in 0..900 {
            full.push(Point {
                at: ago(now, step * 2),
                value: Some(1.0),
            });
        }
        let chart = buckets(&full, now);
        assert_eq!(chart.len(), CHART_BUCKETS);
        assert!(chart.iter().all(|point| point.value == Some(1.0)));

        // The top follows the averaged points. 10 and 70 in one slice
        // are 40, so the axis is 40; 10 and 20 stay under 30.
        assert_eq!(
            cpu_axis_top(&buckets(
                &[
                    Point {
                        at: ago(now, 2),
                        value: Some(10.0),
                    },
                    Point {
                        at: ago(now, 8),
                        value: Some(70.0),
                    },
                ],
                now,
            )),
            40.0
        );
        assert_eq!(
            cpu_axis_top(&buckets(
                &[
                    Point {
                        at: ago(now, 2),
                        value: Some(10.0),
                    },
                    Point {
                        at: ago(now, 8),
                        value: Some(20.0),
                    },
                ],
                now,
            )),
            CPU_AXIS_LOW
        );
    }

    #[test]
    fn the_network_line_steps_by_doubling_and_never_below_the_floor() {
        let now = Instant::now();
        let point = |value: Option<f64>| Point { at: now, value };
        assert_eq!(
            net_scale(&[point(Some(11_000.0))]),
            NET_SCALE_FLOOR,
            "11 kB/s does not get to be full"
        );
        // 5 MiB sits between 4 MiB and 8 MiB. The ceiling is the next
        // doubling, so a few more bytes do not move the line.
        assert_eq!(
            net_scale(&[point(Some(5.0 * 1024.0 * 1024.0))]),
            8.0 * 1024.0 * 1024.0
        );
        // Everyday traffic swinging under 1 MiB keeps one ceiling, so
        // the line does not rescale while the window fills.
        assert_eq!(net_scale(&[point(Some(70_000.0))]), NET_SCALE_FLOOR);
        assert_eq!(net_scale(&[point(Some(263_000.0))]), NET_SCALE_FLOOR);
        assert_eq!(net_scale(&[point(Some(1024.0 * 1024.0))]), NET_SCALE_FLOOR);
        // Past the floor, nearby peaks share a step: 1.2 and 1.9 MiB
        // are both under 2 MiB.
        let step = net_scale(&[point(Some(1.2 * 1024.0 * 1024.0))]);
        assert_eq!(step, 2.0 * 1024.0 * 1024.0);
        assert_eq!(net_scale(&[point(Some(1.9 * 1024.0 * 1024.0))]), step);

        assert_eq!(net_scale(&[]), NET_SCALE_FLOOR);
        assert_eq!(net_scale(&[point(None)]), NET_SCALE_FLOOR);
    }
}
