//! Thirty minutes of the three figures Overview draws.
//!
//! Whole-machine CPU, memory in use, and network download and upload
//! are already on every tick, including the 5s tick while the panel is hidden — the
//! collector does not turn those channels off. This ring is what makes
//! the last half hour readable when the panel opens. It lives in
//! memory: the view reads it every frame, and zstats' own minute
//! history (`~/.zstats/data/`) is the record that survives a restart.
//! A quit drops the ring. That is accepted; a file beside the history
//! would be a second source of the same numbers.
//!
//! A point is the figure the snapshot already carries. CPU is
//! `usage_percent` (0–100). Memory is `used_percent` (0–100). Network
//! is two rings, one per direction — the interface's received and
//! transmitted rate, each its own line, so a burst says which way it
//! went; an absent rate is a break in that line. A true zero is a
//! reading — an idle machine — so a series starts on zero and is never
//! dropped for being quiet.
//! That is the opposite of the per-process traffic curve, where an
//! idle socket is most of the table and must not start a line.
//!
//! A gap longer than [`GAP`] is also a break. The hidden tick is 5s, so
//! a step that landed on the next beat is still under this, and one
//! beat late still connects. The network channel itself refreshes
//! about every 15s; ticks in between repeat the last total, which is
//! what the snapshot holds, so the line holds flat and then steps. That
//! repeat is the reading, not a new sample.
//!
//! The half hour is wall-clock time. `Instant` stands still while the
//! machine sleeps (macOS `CLOCK_UPTIME_RAW`, Linux `CLOCK_MONOTONIC`),
//! and that used to fold a night's lid-close out of the ring: the
//! morning panel drew last night's last thirty awake minutes joined
//! straight onto this morning's, and its Max was last night's peak.
//! Each tick now compares how far the wall clock and `Instant` moved
//! ([`slept`]); past [`GAP`] of difference, every earlier point is
//! moved back by the sleep ([`Series::shift`]), so the sleep takes its
//! real width — a break — and anything it pushed past [`WINDOW`] falls
//! off at the next record.

#[cfg(test)]
use std::mem;
use std::time::{Duration, Instant, SystemTime};

/// How long a curve keeps a point. Half an hour is the axis Overview
/// labels once the ring is full. Older points fall off. The length does
/// not change what the collector does — a longer window is only more of
/// the points the tick was already going to take.
pub const WINDOW: Duration = Duration::from_secs(30 * 60);

/// A gap wider than this is a missed sample, not a line between two
/// readings that were never neighbours. The hidden tick is 5s and the
/// open panel's tick is 2s, so a step one beat late is still under
/// this. Also the tolerance on both clocks: a sleep shorter than this
/// is not one ([`slept`]), and a ring whose oldest point is within it
/// of [`WINDOW`] is full ([`Series::span`]).
pub const GAP: Duration = Duration::from_secs(15);

/// How long the machine slept between two ticks, each stamped with
/// `Instant` and the wall clock: the wall clock's progress less
/// `Instant`'s, which stood still. `None` under [`GAP`] — scheduling
/// jitter and NTP slews, not a sleep — and when the wall clock went
/// backwards (a clock change, not a sleep).
pub fn slept(prev: (Instant, SystemTime), now: (Instant, SystemTime)) -> Option<Duration> {
    let awake = now.0.saturating_duration_since(prev.0);
    let wall = now.1.duration_since(prev.1).ok()?;
    let asleep = wall.saturating_sub(awake);
    (asleep > GAP).then_some(asleep)
}

/// One sample. `None` is a break: the line stops and starts again
/// after it, rather than bridging a figure we do not have.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub at: Instant,
    pub value: Option<f64>,
}

/// One metric's half hour, oldest first.
#[derive(Clone, Debug, Default)]
pub struct Series {
    points: Vec<Point>,
}

impl Series {
    /// Oldest first. Empty until the first tick.
    pub fn points(&self) -> &[Point] {
        &self.points
    }

    /// How long the series actually covers, from its oldest point.
    ///
    /// `None` under a second: the label would otherwise flash `0s` on
    /// the sample that created the first point. Capped at [`WINDOW`].
    /// One point still has a span once it is a second old. The chart's
    /// axis is [`WINDOW`] either way; this span is only the caption
    /// beside it, and [`span_label`] decides when it reads as full.
    pub fn span(&self, now: Instant) -> Option<Duration> {
        let oldest = self.points.first()?.at;
        let span = now.saturating_duration_since(oldest);
        if span < Duration::from_secs(1) {
            None
        } else {
            Some(span.min(WINDOW))
        }
    }

    /// Move every point recorded at or before `before` back by `by` —
    /// the sleep [`slept`] measured, which `Instant` did not count. Only
    /// those points: one recorded after the wake is already on the real
    /// clock. A point that cannot move that far (before `Instant`'s own
    /// origin) is long out of the window and is dropped.
    pub fn shift(&mut self, before: Instant, by: Duration) {
        self.points.retain_mut(|point| {
            if point.at > before {
                return true;
            }
            match point.at.checked_sub(by) {
                Some(at) => {
                    point.at = at;
                    true
                }
                None => false,
            }
        });
    }

    /// Append one reading and drop whatever has aged out of [`WINDOW`].
    ///
    /// A zero is kept. A `None` is kept — it is the break. Nothing here
    /// refuses to start a series, because these three figures exist on
    /// every tick and a quiet machine is a reading.
    pub fn record(&mut self, at: Instant, value: Option<f64>) {
        self.points.push(Point { at, value });
        self.prune(at);
    }

    fn prune(&mut self, now: Instant) {
        let stale = self
            .points
            .iter()
            .position(|point| now.saturating_duration_since(point.at) <= WINDOW)
            .unwrap_or(self.points.len());
        if stale > 0 {
            self.points.drain(..stale);
        }
    }
}

/// `30m` once the window is full, otherwise the real length: `18m`,
/// `40s`. Under a minute stays in seconds — [`crate::format::uptime_short`]
/// collapses that to `0m`, which would label a fresh curve as empty.
/// The text is the same in every locale: a unit, not a sentence.
///
/// Full means within [`GAP`] of [`WINDOW`]: pruning keeps only points
/// at most [`WINDOW`] old, so a full ring's oldest point is always a
/// tick younger than that — 29:58 — and an exact comparison labelled a
/// full ring "29m" for good.
pub fn span_label(span: Duration) -> String {
    let secs = span.as_secs();
    if span + GAP >= WINDOW {
        format!("{}m", WINDOW.as_secs() / 60)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// Raw-sample runs. The chart does not draw these; it averages into
/// 20s buckets first, and an empty bucket is the break at that grain.
/// A `None`, or a gap past [`GAP`], ends a run. A run of one point is
/// not a line.
#[cfg(test)]
fn segments(points: &[Point]) -> Vec<Vec<(Instant, f64)>> {
    let mut out = Vec::new();
    let mut run = Vec::new();
    let mut prev: Option<Instant> = None;
    for point in points {
        let gapped = prev.is_some_and(|prev| point.at.saturating_duration_since(prev) > GAP);
        if gapped {
            flush_run(&mut out, &mut run);
        }
        match point.value {
            Some(value) => run.push((point.at, value)),
            None => flush_run(&mut out, &mut run),
        }
        prev = Some(point.at);
    }
    flush_run(&mut out, &mut run);
    out
}

#[cfg(test)]
fn flush_run(out: &mut Vec<Vec<(Instant, f64)>>, run: &mut Vec<(Instant, f64)>) {
    if run.len() >= 2 {
        out.push(mem::take(run));
    } else {
        run.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    fn point(at: Instant, value: Option<f64>) -> Point {
        Point { at, value }
    }

    #[test]
    fn a_zero_is_kept_and_connects() {
        let t0 = Instant::now();
        let mut series = Series::default();
        series.record(t0, Some(0.0));
        series.record(at(t0, 5), Some(0.0));
        assert_eq!(series.points().len(), 2, "an idle machine is a reading");
        let runs = segments(series.points());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len(), 2);
        assert_eq!(runs[0][0].1, 0.0);
    }

    #[test]
    fn a_missing_rate_breaks_and_a_reported_zero_does_not() {
        let t0 = Instant::now();
        let points = [
            point(t0, Some(1.0)),
            point(at(t0, 5), Some(1.0)),
            point(at(t0, 10), None),
            point(at(t0, 15), Some(2.0)),
            point(at(t0, 20), Some(2.0)),
        ];
        let runs = segments(&points);
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|run| run.len() == 2));

        let quiet = [
            point(t0, Some(5.0)),
            point(at(t0, 5), Some(0.0)),
            point(at(t0, 10), Some(5.0)),
        ];
        let runs = segments(&quiet);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len(), 3);
    }

    #[test]
    fn a_step_the_hidden_tick_would_take_stays_one_line() {
        let t0 = Instant::now();
        let steady = [
            point(t0, Some(10.0)),
            point(at(t0, 5), Some(12.0)),
            point(at(t0, 10), Some(11.0)),
        ];
        assert_eq!(segments(&steady).len(), 1);
        assert_eq!(segments(&steady)[0].len(), 3);

        let exactly = [point(t0, Some(10.0)), point(at(t0, 15), Some(12.0))];
        assert_eq!(segments(&exactly).len(), 1, "15s is still one step");

        let missed = [
            point(t0, Some(10.0)),
            point(at(t0, 5), Some(12.0)),
            point(at(t0, 5) + GAP + Duration::from_millis(1), Some(11.0)),
            point(at(t0, 26), Some(9.0)),
        ];
        let runs = segments(&missed);
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|run| run.len() == 2));
    }

    #[test]
    fn points_older_than_the_window_fall_off() {
        let t0 = Instant::now();
        let mut series = Series::default();
        series.record(t0, Some(40.0));
        series.record(at(t0, 5), Some(41.0));
        let later = t0 + WINDOW + Duration::from_secs(6);
        series.record(later, Some(3.0));
        assert_eq!(series.points().len(), 1);
        assert_eq!(series.points()[0].value, Some(3.0));

        let mut held = Series::default();
        held.record(t0, Some(1.0));
        held.record(t0 + WINDOW, Some(2.0));
        assert_eq!(held.points().len(), 2, "exactly the window is still in");
    }

    #[test]
    fn the_span_label_is_the_real_length_until_the_window_is_full() {
        assert!(Series::default().span(Instant::now()).is_none());
        assert_eq!(span_label(Duration::from_secs(40)), "40s");
        assert_eq!(span_label(Duration::from_secs(59)), "59s");
        assert_eq!(span_label(Duration::from_secs(18 * 60)), "18m");
        assert_eq!(span_label(WINDOW), "30m");

        let t0 = Instant::now();
        let mut series = Series::default();
        series.record(t0, Some(1.0));
        assert!(series.span(t0).is_none(), "the first point is not a span");
        assert_eq!(series.span(at(t0, 40)).unwrap(), Duration::from_secs(40));
        series.record(t0 + WINDOW, Some(1.0));
        assert_eq!(series.span(t0 + WINDOW).unwrap(), WINDOW);
        // Asked past the last point, still the window: the label does
        // not grow after the ring is full.
        assert_eq!(
            series.span(t0 + WINDOW + Duration::from_secs(10)).unwrap(),
            WINDOW
        );
    }

    #[test]
    fn a_full_ring_reads_as_the_window_even_a_tick_short_of_it() {
        // Ticks that do not land on the window's edge (7s apart here):
        // the oldest point a full ring keeps is 29:59 old, and that is
        // full.
        let t0 = Instant::now();
        let mut series = Series::default();
        for tick in 0..=258 {
            series.record(at(t0, tick * 7), Some(1.0));
        }
        let now = at(t0, 258 * 7);
        let oldest = now.saturating_duration_since(series.points()[0].at);
        assert!(oldest < WINDOW, "pruning keeps the edge out: {oldest:?}");
        // The span stays the real one; the label calls it full.
        assert_eq!(series.span(now), Some(oldest));
        assert_eq!(span_label(series.span(now).unwrap()), "30m");
        // Short of the window by more than a missed sample is not full.
        assert_eq!(span_label(WINDOW - GAP - Duration::from_secs(1)), "29m");
    }

    #[test]
    fn a_sleep_is_the_wall_clock_less_the_instant() {
        let i0 = Instant::now();
        let w0 = SystemTime::now();
        // Instant moved 5s, the wall clock an hour: an hour less 5s asleep.
        let woke = (at(i0, 5), w0 + Duration::from_secs(3600));
        assert_eq!(slept((i0, w0), woke), Some(Duration::from_secs(3595)));
        // Both clocks together, or under the gap of skew: no sleep.
        assert_eq!(
            slept((i0, w0), (at(i0, 5), w0 + Duration::from_secs(5))),
            None
        );
        assert_eq!(
            slept((i0, w0), (at(i0, 5), w0 + Duration::from_secs(19))),
            None
        );
        // A wall clock that went back is a clock change, not a sleep.
        assert_eq!(
            slept((i0, w0), (at(i0, 5), w0 - Duration::from_secs(60))),
            None
        );
    }

    #[test]
    fn a_sleep_takes_its_real_width_and_ages_the_ring_out() {
        let t0 = Instant::now() + Duration::from_secs(4 * 3600);
        let mut series = Series::default();
        series.record(t0, Some(10.0));
        series.record(at(t0, 600), Some(20.0));
        // Ten minutes asleep after the second point: both earlier points
        // move back by it, so the gap to the next reading is real.
        series.shift(at(t0, 600), Duration::from_secs(600));
        series.record(at(t0, 605), Some(30.0));
        let ages: Vec<u64> = series
            .points()
            .iter()
            .map(|p| at(t0, 605).saturating_duration_since(p.at).as_secs())
            .collect();
        assert_eq!(ages, [1205, 605, 0]);
        // A night asleep pushes the whole ring past the window: the next
        // record starts the chart over instead of joining last night on.
        series.shift(at(t0, 605), Duration::from_secs(8 * 3600));
        series.record(at(t0, 610), Some(40.0));
        assert_eq!(series.points().len(), 1);
        assert_eq!(series.points()[0].value, Some(40.0));
    }
}
