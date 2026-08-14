//! The whole process table, fetched once because somebody asked for it.
//!
//! The panel's normal list is what zstats materialises: `max-processes`
//! (default 50), and not even the top 50 by CPU — the budget is split with
//! the top-by-memory ranking so both orderings stay meaningful. That is the
//! right default, since the tail is idle daemons, but it cannot answer "is X
//! running at all". This is the escape hatch behind the All chip.
//!
//! A throwaway [`LocalCollector`] rather than a wider `max-processes` on the
//! resident one, for two reasons:
//!
//! - `Monitor` bakes its `CollectorConfig` in at construction and
//!   `reload_settings` only re-reads `[alerts]`, so raising the cap means
//!   rebuilding the monitor — losing every rate baseline and resetting the
//!   60-second rolling windows the main list ranks by.
//! - the cap is not only a display limit. Per-process alert rules and the
//!   daily history both work off the materialised set, so raising it would
//!   fire alerts for processes that were never considered before and grow
//!   the JSONL. Asking a UI question should not change what alerts.
//!
//! Measured on a 694-process machine, materialising all of them instead of
//! 50 costs under 1ms: the full table is walked either way to rank it, and
//! the cap only decides how many entries get their strings built. What this
//! actually pays for is one extra pair of sysinfo refreshes, off the UI
//! thread, and only when clicked.

use std::sync::Arc;
use std::time::{Duration, Instant};
use zstats::snapshot::{ProcessGroupSnapshot, ProcessSnapshot, SystemSnapshot};
use zstats::{CollectError, Collector, CollectorConfig, LocalCollector};

/// Gap between the priming collect and the one that is kept.
///
/// sysinfo derives per-process CPU% by differencing two of its own samples,
/// so a fresh collector reports 0% for everything on the first pass. Long
/// enough that the percentages mean something, short enough that the panel
/// is not visibly waiting.
pub const SETTLE: Duration = Duration::from_millis(300);

/// One full listing, plus the caveat it has to be read with.
pub struct Scan {
    /// CPU descending, the order the collector returns.
    pub processes: Arc<Vec<ProcessSnapshot>>,
    /// What the kernel had at scan time. Can exceed `processes.len()` by the
    /// handful that exited between the two passes.
    pub total: usize,
    /// The window the CPU percentages cover — **not** the 60-second rolling
    /// average the main list is ranked by. The view is required to say so;
    /// two numbers for the same process that disagree without explanation
    /// are worse than no second number at all.
    pub window: Duration,
}

/// Collect the entire process table. Blocking, and sleeps for [`SETTLE`] —
/// call it on a background thread.
pub fn scan() -> Result<Scan, CollectError> {
    let (snapshot, window) = collect_once(process_config())?;
    let processes = snapshot.processes.unwrap_or_default();
    Ok(Scan {
        total: snapshot
            .total_processes
            .map_or(processes.len(), |t| t as usize),
        processes,
        window,
    })
}

/// One full application-tree listing. Same two-pass shape as [`scan`],
/// same reason not to widen the resident collector — see the module docs.
pub struct GroupScan {
    /// CPU descending, every launchd/init child plus its descendants.
    pub groups: Arc<Vec<ProcessGroupSnapshot>>,
    /// Trees in this listing. Equals `groups.len()` because the cap is off.
    pub total: usize,
    /// Same caveat as [`Scan::window`]: the few hundred ms of this scan,
    /// not the resident tick's 60-second average.
    pub window: Duration,
}

/// Collect every process tree. Blocking; call it on a background thread.
pub fn scan_groups() -> Result<GroupScan, CollectError> {
    let (snapshot, window) = collect_once(group_config())?;
    let groups = snapshot.process_groups.unwrap_or_default();
    Ok(GroupScan {
        total: groups.len(),
        groups,
        window,
    })
}

/// Two sysinfo passes with [`SETTLE`] between them. The first primes CPU
/// (and per-process disk counters when those are on); only the second
/// sample has rates that mean anything.
fn collect_once(config: CollectorConfig) -> Result<(SystemSnapshot, Duration), CollectError> {
    let mut collector = LocalCollector::new(config);
    collector.collect()?;
    let started = Instant::now();
    std::thread::sleep(SETTLE);
    let snapshot = collector.collect()?;
    Ok((snapshot, started.elapsed()))
}

/// Processes and nothing else. Every other subsystem is already riding the
/// resident collector's tick, and disk capacity alone costs ~18ms a refresh.
fn process_config() -> CollectorConfig {
    CollectorConfig {
        collect_processes: true,
        max_processes: usize::MAX,
        collect_process_groups: false,
        collect_process_disk_io: false,
        per_core_cpu: false,
        collect_disks: false,
        collect_networks: false,
        collect_battery: false,
        collect_temperatures: false,
        ..Default::default()
    }
}

/// Trees, uncapped, with disk rates so the Apps listing is not a wall of
/// `—`. Groups are aggregated from the full table either way; the cap is
/// the only thing the resident collector was hiding.
fn group_config() -> CollectorConfig {
    CollectorConfig {
        collect_processes: true,
        max_processes: usize::MAX,
        collect_process_groups: true,
        collect_process_disk_io: true,
        per_core_cpu: false,
        collect_disks: false,
        collect_networks: false,
        collect_battery: false,
        collect_temperatures: false,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two-pass shape is the whole contract: sysinfo needs a baseline
    /// before it can report per-process CPU at all, so a one-pass scan would
    /// hand the UI a table of zeroes and look like an idle machine.
    ///
    /// Costs [`SETTLE`] plus two process refreshes — the slowest test here,
    /// and worth it: nothing else would catch a regression to one pass.
    #[test]
    fn scans_the_whole_table_with_usable_cpu() {
        let scan = scan().expect("collect");

        // Any real machine runs far more than the collector's default 50,
        // which is the entire reason this exists.
        assert!(
            scan.processes.len() > 50,
            "only {} processes — the cap is still being applied",
            scan.processes.len()
        );
        assert!(scan.total >= scan.processes.len());
        assert!(scan.window >= SETTLE);

        // Not every process is busy, but on a machine running a test suite
        // something is: an all-zero table means the baseline pass was lost.
        assert!(
            scan.processes.iter().any(|p| p.cpu_usage_percent > 0.0),
            "no process reported any CPU — the priming collect is missing"
        );
    }

    #[test]
    fn scans_every_process_tree() {
        let scan = scan_groups().expect("collect");
        assert!(
            scan.groups.len() > 50,
            "only {} trees — the cap is still being applied",
            scan.groups.len()
        );
        assert_eq!(scan.total, scan.groups.len());
        assert!(scan.window >= SETTLE);
        assert!(
            scan.groups.iter().any(|g| g.cpu_usage_percent > 0.0),
            "no tree reported any CPU — the priming collect is missing"
        );
        // Member counts come off the full table, not the truncated process
        // list, so a multi-process app still reports more than its root.
        assert!(
            scan.groups.iter().any(|g| g.process_count > 1),
            "every tree is a singleton — grouping did not walk descendants"
        );
    }
}
