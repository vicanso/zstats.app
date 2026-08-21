//! "What actually burned the CPU today", from the daily history files.
//!
//! zstats writes one JSON line per qualifying process per minute into
//! `<config-dir>/data/YYYY-MM-DD.jsonl`, kept 30 days. Two things put a
//! process in there: crossing the base alert thresholds, or being one of the
//! five biggest CPU-time spenders of that minute. The second criterion is the
//! only one that can see a process no threshold will ever catch, and it is
//! recording-only — deliberately, because a low-bar/long-window *alert* would
//! fire on every legitimate resident daemon.
//!
//! This ranks by [`MetricRecord::cpu_time_ms`], the kernel's lifetime counter,
//! not by any average. An average answers "how busy was it", and the process
//! that dominates a day is usually never busy — it is a steady 8% that
//! outspends the ten-minute 100% spike several times over while never once
//! looking alarming.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::time::Duration;
use zstats::records::{MetricRecord, read_range};

/// How the CPU was burned, from the shape of the already-written minutes.
/// Display only — the file is biased toward loud minutes, so this is never
/// an alert.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HistoryShape {
    /// Brief: first-to-last span shorter than [`SHORT_SPAN`].
    Spike,
    /// Spread out, and the peak is not far from the span-average.
    Sustained,
    /// Spread out, but the peak is several times the span-average —
    /// quiet gaps with bursts in between.
    Intermittent,
}

/// Below this, a run is a spike even if the peak sits on the average
/// (a ten-minute compile is not "steady"). Fifteen minutes is a quarter
/// of an hour — long enough that "it ran flat the whole time" starts to
/// mean something, short enough that a typical encode still counts as a
/// burst.
const SHORT_SPAN: Duration = Duration::from_secs(15 * 60);

/// Peak / span-average at or above this, over a long span, is bursts
/// rather than a flat load — necessary for Intermittent, no longer
/// sufficient: the ratio is dimensionless, and the time band exposed
/// what that misses (both fixes below have their own constant).
const BURST_RATIO: f32 = 3.0;

/// A burst has to BE a burst: below a quarter of a core, "3× the
/// average" is the normal wobble of a flat daemon, not intermittency.
/// The tombstone case is Activity Monitor — peak 4.0% over a ~1.3%
/// average, labelled intermittent, when nothing about it ever burst.
/// Same one-core unit every CPU figure in the app speaks.
const BURST_FLOOR_PERCENT: f32 = 25.0;

/// Recorded-minute coverage of the span at or above this reads as
/// continuously present — there are no quiet gaps for bursts to sit
/// between, which is what "intermittent" claims. The tombstone: a chat
/// app recorded 547 of ~560 minutes (a near-solid band on screen),
/// labelled intermittent purely on peak/average ratio, while Chrome —
/// 106 scattered minutes, real gaps — wore the same word. File-biased
/// like everything here: an absent minute means "did not qualify",
/// which is exactly what a gap is.
const DENSE_COVERAGE: f32 = 0.75;

/// Half-hour cells across one local day: 48 of them, [`BAND_BUCKET_MINUTES`]
/// wide. Coarse on purpose — inside a 320px panel a cell is ~6px, enough
/// to place "that stretch around three" without turning twelve rows into
/// hundreds of elements.
pub const BAND_BUCKETS: usize = 48;
/// Width of one band cell, in minutes of the local day.
pub const BAND_BUCKET_MINUTES: usize = 24 * 60 / BAND_BUCKETS;

/// One day of "when", per process: each cell holds the highest 1-minute
/// average recorded in that half hour, `None` where no minute qualified.
///
/// `None` is a statement about the *file*, not about the process — the
/// record is conditional (over a threshold, or in that minute's top
/// five), so an empty cell means "not recorded", never "idle". That is
/// exactly why this is a band of cells and not a line chart: a line
/// must invent a value for every x, and zero would be a lie.
///
/// Cells hold the **max** of their minutes, same reasoning as the trend
/// buffer: "when did it burn" asks what a stretch reached, and a mean
/// would let one quiet minute talk a real burst back down.
pub type Band = [Option<f32>; BAND_BUCKETS];

/// One process's share of the chosen window.
pub struct Spender {
    pub pid: u32,
    pub name: String,
    /// The application `name` belongs to, where the executable's own
    /// name does not say it — carried on the records (zstats 0.5.3) "so
    /// a history can be read against the notifications it explains":
    /// the banner said CodeBuddy CN, the row must not say Electron.
    /// Presentation only, like everywhere else; grouping stays on
    /// `(pid, name)`.
    pub display_name: Option<String>,
    /// Single-core milliseconds burnt across the period.
    pub cpu_time_ms: u64,
    /// Highest 1-minute average seen. Context for the total, not the ranking:
    /// a low peak beside a large total is exactly the case this view exists
    /// for.
    pub peak_cpu_percent: f32,
    pub peak_memory_bytes: u64,
    /// Minutes this process was recorded. Fewer than the wall clock: a
    /// process only lands in the file on the minutes it qualifies.
    pub minutes: usize,
    /// First record to last, including unrecorded gaps. `minutes` is the
    /// count of files lines, not this.
    pub span: Duration,
    /// `None` when there are not two samples to difference.
    pub shape: Option<HistoryShape>,
    /// When today's burn happened, half-hour resolution — only for the
    /// one-day view. A multi-day range would fold every day onto the
    /// same 24-hour axis and paint a superposition nobody lived through,
    /// so wider windows carry `None` and keep the share meter instead.
    pub band: Option<Band>,
}

/// Read the last `days` days of history (today included) and rank them.
/// `1` is the classic today view; wider windows answer "who burned the
/// most this week" from the same 30-day files zstats already keeps.
/// Blocking file IO — call it off the UI thread.
pub fn spenders(config_dir: &Path, days: u16) -> io::Result<Vec<Spender>> {
    // Files are named by *local* date, which is what `Zoned::now` gives.
    let end = jiff::Zoned::now().date();
    let start = end
        .checked_sub(jiff::Span::new().days(i64::from(days.max(1)) - 1))
        .unwrap_or(end);
    // The band exists only when the window IS one local day — see
    // `Spender::band` for why a wider window must not get one.
    let band_day = (days <= 1).then_some(end);
    Ok(rank(read_range(config_dir, start, end)?, band_day))
}

/// Sum each process's consumption across its records.
///
/// Differencing consecutive samples rather than subtracting first from last,
/// and keeping only the positive steps. Both matter:
///
/// - a process is recorded only on the minutes it qualifies, so it drifts in
///   and out of the file; differencing a cumulative counter stays exact across
///   any such gap, where summing per-window deltas would undercount.
/// - the counter going *backwards* means the pid was reused by an unrelated
///   process, and that step contributes nothing rather than a wild negative.
pub fn rank(mut records: Vec<MetricRecord>, band_day: Option<jiff::civil::Date>) -> Vec<Spender> {
    // Keyed by name as well as pid: on reuse the name almost always changes,
    // and splitting there keeps two unrelated processes from sharing a row.
    let mut by_process: HashMap<(u32, String), Vec<MetricRecord>> = HashMap::new();
    records.sort_by_key(|r| r.timestamp);
    for record in records {
        by_process
            .entry((record.pid, record.name.clone()))
            .or_default()
            .push(record);
    }

    let mut spenders: Vec<Spender> = by_process
        .into_iter()
        .map(|((pid, name), samples)| {
            let cpu_time_ms = samples
                .windows(2)
                .map(|w| w[1].cpu_time_ms.saturating_sub(w[0].cpu_time_ms))
                .sum();
            let peak_cpu_percent = samples
                .iter()
                .map(|r| r.cpu_avg_percent)
                .fold(0.0, f32::max);
            let span = span_of(&samples);
            Spender {
                pid,
                // Any sample's Some will do — the bundle does not change
                // under a running pid; older lines predate the field.
                display_name: samples.iter().find_map(|r| r.display_name.clone()),
                name,
                cpu_time_ms,
                peak_cpu_percent,
                peak_memory_bytes: samples
                    .iter()
                    // Footprint when the record carries it (zstats ≥ 0.5;
                    // "RSS could not explain its own alerts"), RSS for
                    // older lines — same preference the process rows use,
                    // so the two tabs speak one dialect.
                    .map(|r| r.memory_footprint_bytes.unwrap_or(r.memory_avg_bytes))
                    .max()
                    .unwrap_or(0),
                minutes: samples.len(),
                span,
                shape: classify(cpu_time_ms, peak_cpu_percent, span, samples.len()),
                band: band_day.map(|day| {
                    assemble_band(samples.iter().filter_map(|r| {
                        let (date, minute) = local_day_minute(r.timestamp);
                        (date == day).then_some((minute, r.cpu_avg_percent))
                    }))
                }),
            }
        })
        .collect();

    spenders.sort_by(|a, b| {
        b.cpu_time_ms
            .cmp(&a.cpu_time_ms)
            .then_with(|| a.name.cmp(&b.name))
    });
    spenders
}

/// Which local day a record fell on, and how many minutes into it.
/// System timezone on purpose: the files are named by local date and the
/// axis the band draws under is the user's wall clock.
fn local_day_minute(ts: jiff::Timestamp) -> (jiff::civil::Date, u16) {
    let zoned = ts.to_zoned(jiff::tz::TimeZone::system());
    (
        zoned.date(),
        zoned.hour() as u16 * 60 + zoned.minute() as u16,
    )
}

/// Fold (minute-of-day, 1-minute average) pairs into the half-hour
/// cells. Pure — the timezone stays in [`local_day_minute`] so this can
/// be tested against plain minute numbers.
fn assemble_band(minutes: impl Iterator<Item = (u16, f32)>) -> Band {
    let mut band: Band = [None; BAND_BUCKETS];
    for (minute, cpu) in minutes {
        let Some(cell) = band.get_mut(usize::from(minute) / BAND_BUCKET_MINUTES) else {
            // A minute past 23:59 can only be a corrupt line; it gets
            // no cell rather than a panic or a wrapped slot.
            continue;
        };
        *cell = Some(cell.map_or(cpu, |held: f32| held.max(cpu)));
    }
    band
}

fn span_of(samples: &[MetricRecord]) -> Duration {
    let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
        return Duration::ZERO;
    };
    let secs = last
        .timestamp
        .as_second()
        .saturating_sub(first.timestamp.as_second());
    Duration::from_secs(u64::try_from(secs).unwrap_or(0))
}

/// Classify from the already-written minutes. `minutes` is how many
/// lines landed, not how long the process ran — the span is first-to-last
/// wall time, and the average is total burn over that span (gaps included).
///
/// Intermittent takes three conditions together, because each catches a
/// different impostor: the peak/average ratio alone (the original rule)
/// also fires on a flat daemon's normal wobble ([`BURST_FLOOR_PERCENT`])
/// and on a continuously-present process with one loud stretch
/// ([`DENSE_COVERAGE`]) — both of which the time band now shows beside
/// the pill, so the pill has to tell the same story the band does.
fn classify(cpu_time_ms: u64, peak: f32, span: Duration, samples: usize) -> Option<HistoryShape> {
    if samples < 2 || cpu_time_ms == 0 {
        return None;
    }
    let span_ms = span.as_millis();
    if span_ms == 0 {
        return None;
    }
    if span < SHORT_SPAN {
        return Some(HistoryShape::Spike);
    }
    let avg = cpu_time_ms as f64 / span_ms as f64 * 100.0;
    // +1: a span of N minutes has N+1 recordable minute marks (both
    // endpoints carry a record by construction of `span_of`).
    let coverage = samples as f32 / (span.as_secs() / 60 + 1) as f32;
    let bursts = avg > 0.0 && f64::from(peak) >= f64::from(BURST_RATIO) * avg;
    if bursts && peak >= BURST_FLOOR_PERCENT && coverage < DENSE_COVERAGE {
        return Some(HistoryShape::Intermittent);
    }
    Some(HistoryShape::Sustained)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    fn record(pid: u32, name: &str, minute: i64, cpu_time_ms: u64, cpu: f32) -> MetricRecord {
        MetricRecord {
            timestamp: Timestamp::from_second(1_700_000_000 + minute * 60).unwrap(),
            pid,
            name: name.into(),
            display_name: None,
            cpu_avg_percent: cpu,
            memory_avg_bytes: 1 << 20,
            memory_footprint_bytes: None,
            memory_share_percent: 1.0,
            cpu_time_ms,
        }
    }

    /// The whole point of the view: a steady low percentage outspends a short
    /// spike, and only the counter shows it. Ranking by average would put
    /// these in the opposite order.
    #[test]
    fn ranks_by_total_burnt_not_by_peak() {
        let ranked = rank(
            vec![
                // 8% for an hour = 288 core-seconds, never alarming.
                record(1, "quiet", 0, 0, 8.0),
                record(1, "quiet", 60, 288_000, 8.0),
                // 100% for ten minutes = 60 core-seconds, and it looks dramatic.
                record(2, "spike", 0, 0, 100.0),
                record(2, "spike", 10, 60_000, 100.0),
            ],
            None,
        );
        assert_eq!(ranked[0].name, "quiet");
        assert_eq!(ranked[0].cpu_time_ms, 288_000);
        assert!((ranked[0].peak_cpu_percent - 8.0).abs() < f32::EPSILON);
        assert_eq!(ranked[0].shape, Some(HistoryShape::Sustained));
        assert_eq!(ranked[1].name, "spike");
        assert_eq!(ranked[1].shape, Some(HistoryShape::Spike));
    }

    #[test]
    fn a_long_span_with_a_tall_peak_is_bursts_not_steady() {
        // 100% for two minutes, eight times over seven hours.
        let mut records = Vec::new();
        for i in 0..8_i64 {
            let m = i * 60;
            let burnt = (i as u64) * 120_000;
            records.push(record(1, "bursty", m, burnt, 100.0));
            records.push(record(1, "bursty", m + 2, burnt + 120_000, 100.0));
        }
        let ranked = rank(records, None);
        assert_eq!(ranked[0].shape, Some(HistoryShape::Intermittent));
        assert!(ranked[0].span >= Duration::from_secs(6 * 3600));
    }

    #[test]
    fn two_samples_on_the_same_instant_have_no_shape() {
        let ranked = rank(
            vec![record(1, "p", 0, 0, 8.0), record(1, "p", 0, 100, 8.0)],
            None,
        );
        assert_eq!(ranked[0].shape, None);
    }

    /// A process drops out of the file on minutes it does not qualify.
    /// Differencing a cumulative counter has to stay exact across the hole.
    #[test]
    fn gaps_do_not_undercount() {
        let ranked = rank(
            vec![
                record(1, "p", 0, 1_000, 5.0),
                record(1, "p", 1, 2_000, 5.0),
                // minutes 2..9 missing
                record(1, "p", 10, 30_000, 5.0),
            ],
            None,
        );
        assert_eq!(ranked[0].cpu_time_ms, 29_000, "1000→2000→30000");
        assert_eq!(ranked[0].minutes, 3);
    }

    /// A counter that goes backwards means the pid was reused. The two
    /// tenants are unrelated and must not be summed into one number.
    #[test]
    fn pid_reuse_does_not_produce_a_negative_or_a_merge() {
        let ranked = rank(
            vec![
                record(1, "old", 0, 500_000, 5.0),
                record(1, "old", 1, 510_000, 5.0),
                // Same pid, new process, counter restarts near zero.
                record(1, "new", 2, 100, 5.0),
                record(1, "new", 3, 5_100, 5.0),
            ],
            None,
        );
        assert_eq!(ranked.len(), 2, "different tenants stay apart");
        let new = ranked.iter().find(|s| s.name == "new").unwrap();
        assert_eq!(new.cpu_time_ms, 5_000);
        let old = ranked.iter().find(|s| s.name == "old").unwrap();
        assert_eq!(old.cpu_time_ms, 10_000);
    }

    /// One sample has nothing to difference against, so it reports 0 rather
    /// than its lifetime total — which would count time burnt before today.
    #[test]
    fn a_single_sample_claims_nothing() {
        let ranked = rank(vec![record(1, "p", 0, 9_999_999, 5.0)], None);
        assert_eq!(ranked[0].cpu_time_ms, 0);
        assert_eq!(ranked[0].shape, None);
    }

    #[test]
    fn no_records_is_not_an_error() {
        assert!(rank(vec![], None).is_empty());
    }

    /// The Activity Monitor case: peak 4% over a ~1.3% average trips
    /// the ratio, but nothing about a process that never reached a
    /// twenty-fifth of a core ever "burst". The ratio is dimensionless;
    /// the floor is what gives it a unit.
    #[test]
    fn a_flat_daemons_wobble_is_not_intermittency() {
        // Sparse coverage and ratio ≥ 3 — the old rule's intermittent —
        // but the peak is 4%.
        let mut records = Vec::new();
        for i in 0..10_i64 {
            records.push(record(1, "monitor", i * 90, (i as u64) * 70_000, 4.0));
        }
        let ranked = rank(records, None);
        assert_eq!(ranked[0].shape, Some(HistoryShape::Sustained));
    }

    /// The chat-app case: recorded nearly every minute of its span (a
    /// near-solid band on screen), with one loud stretch tripping the
    /// ratio. "Intermittent" claims quiet gaps, and it has none — the
    /// pill must tell the same story the band does.
    #[test]
    fn continuously_present_with_one_loud_stretch_is_sustained() {
        let mut records = Vec::new();
        for i in 0..120_i64 {
            // Every single minute for two hours: coverage ≈ 1.0.
            let burnt = (i as u64) * 2_000; // ~3.3% baseline
            records.push(record(
                1,
                "chat",
                i,
                burnt,
                if i == 60 { 90.0 } else { 4.0 },
            ));
        }
        let ranked = rank(records, None);
        assert!(
            ranked[0].minutes >= 120,
            "the premise: it was recorded the whole time"
        );
        assert_eq!(ranked[0].shape, Some(HistoryShape::Sustained));
    }

    /// And the genuine article keeps its word: scattered minutes, real
    /// gaps, a peak that is actually a burst.
    #[test]
    fn sparse_real_bursts_stay_intermittent() {
        let mut records = Vec::new();
        for i in 0..8_i64 {
            let m = i * 60;
            let burnt = (i as u64) * 120_000;
            records.push(record(1, "chrome", m, burnt, 110.0));
            records.push(record(1, "chrome", m + 2, burnt + 120_000, 110.0));
        }
        let ranked = rank(records, None);
        assert_eq!(ranked[0].shape, Some(HistoryShape::Intermittent));
    }

    #[test]
    fn a_cell_keeps_the_loudest_minute_and_an_empty_cell_stays_a_gap() {
        // Two minutes inside 14:00–14:30 (840 and 855), one at 09:05.
        let band = assemble_band([(840, 12.0), (855, 38.0), (545, 5.0)].into_iter());
        assert_eq!(band[840 / BAND_BUCKET_MINUTES], Some(38.0), "max, not last");
        assert_eq!(band[545 / BAND_BUCKET_MINUTES], Some(5.0));
        // Everything unrecorded is a gap — "no line in the file", which
        // must never be paintable as zero.
        let filled = band.iter().flatten().count();
        assert_eq!(filled, 2);
    }

    #[test]
    fn the_day_edges_land_in_the_first_and_last_cell() {
        let band = assemble_band([(0, 1.0), (1439, 2.0), (1440, 99.0)].into_iter());
        assert_eq!(band[0], Some(1.0));
        assert_eq!(band[BAND_BUCKETS - 1], Some(2.0), "23:59 is the last cell");
        // Minute 1440 does not exist in a day; a corrupt line gets no
        // cell rather than a wrapped slot or a panic.
        assert_eq!(band.iter().flatten().count(), 2);
    }

    #[test]
    fn only_the_one_day_view_carries_a_band() {
        // The wide windows would fold several days onto one 24h axis —
        // a superposition nobody lived through — so they carry None and
        // the view keeps the share meter.
        let ranked = rank(vec![record(1, "p", 0, 0, 8.0)], None);
        assert!(ranked[0].band.is_none());
    }
}
