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
use zstats::records::{MetricRecord, read_range};

/// One process's share of the chosen window.
pub struct Spender {
    pub pid: u32,
    pub name: String,
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
    Ok(rank(read_range(config_dir, start, end)?))
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
pub fn rank(mut records: Vec<MetricRecord>) -> Vec<Spender> {
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
            Spender {
                pid,
                name,
                cpu_time_ms,
                peak_cpu_percent: samples
                    .iter()
                    .map(|r| r.cpu_avg_percent)
                    .fold(0.0, f32::max),
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

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    fn record(pid: u32, name: &str, minute: i64, cpu_time_ms: u64, cpu: f32) -> MetricRecord {
        MetricRecord {
            timestamp: Timestamp::from_second(1_700_000_000 + minute * 60).unwrap(),
            pid,
            name: name.into(),
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
        let ranked = rank(vec![
            // 8% for an hour = 288 core-seconds, never alarming.
            record(1, "quiet", 0, 0, 8.0),
            record(1, "quiet", 60, 288_000, 8.0),
            // 100% for ten minutes = 60 core-seconds, and it looks dramatic.
            record(2, "spike", 0, 0, 100.0),
            record(2, "spike", 10, 60_000, 100.0),
        ]);
        assert_eq!(ranked[0].name, "quiet");
        assert_eq!(ranked[0].cpu_time_ms, 288_000);
        assert!((ranked[0].peak_cpu_percent - 8.0).abs() < f32::EPSILON);
        assert_eq!(ranked[1].name, "spike");
    }

    /// A process drops out of the file on minutes it does not qualify.
    /// Differencing a cumulative counter has to stay exact across the hole.
    #[test]
    fn gaps_do_not_undercount() {
        let ranked = rank(vec![
            record(1, "p", 0, 1_000, 5.0),
            record(1, "p", 1, 2_000, 5.0),
            // minutes 2..9 missing
            record(1, "p", 10, 30_000, 5.0),
        ]);
        assert_eq!(ranked[0].cpu_time_ms, 29_000, "1000→2000→30000");
        assert_eq!(ranked[0].minutes, 3);
    }

    /// A counter that goes backwards means the pid was reused. The two
    /// tenants are unrelated and must not be summed into one number.
    #[test]
    fn pid_reuse_does_not_produce_a_negative_or_a_merge() {
        let ranked = rank(vec![
            record(1, "old", 0, 500_000, 5.0),
            record(1, "old", 1, 510_000, 5.0),
            // Same pid, new process, counter restarts near zero.
            record(1, "new", 2, 100, 5.0),
            record(1, "new", 3, 5_100, 5.0),
        ]);
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
        let ranked = rank(vec![record(1, "p", 0, 9_999_999, 5.0)]);
        assert_eq!(ranked[0].cpu_time_ms, 0);
    }

    #[test]
    fn no_records_is_not_an_error() {
        assert!(rank(vec![]).is_empty());
    }
}
