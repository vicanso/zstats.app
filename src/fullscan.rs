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

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
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

/// Identity, parent pointers, memory — the full table, one pass.
///
/// CPU% is unusable here (sysinfo has no baseline yet) and the caller
/// must not paint it. What this is for: naming every member of an Apps
/// tree. The resident tick only materialises `max-processes`, so a
/// 37-helper browser is a count with a handful of names; walking this
/// table is how the expansion lists the rest. No [`SETTLE`]: a sleep
/// would be paying for a rate this listing then throws away.
pub fn list_processes() -> Result<Arc<Vec<ProcessSnapshot>>, CollectError> {
    let mut collector = LocalCollector::new(process_config());
    let snapshot = collector.collect()?;
    Ok(snapshot.processes.unwrap_or_default())
}

/// Every process in `processes` whose parent chain reaches `root`.
///
/// The chain has to be intact in *this* table: a missing intermediate
/// is not guessed, because that would pin idle helpers on the wrong
/// tree. Same walk zstats uses to build `ProcessGroupSnapshot`.
pub fn tree_members(root: u32, processes: &[ProcessSnapshot]) -> Vec<&ProcessSnapshot> {
    let by_pid: HashMap<u32, u32> = processes
        .iter()
        .filter_map(|p| p.parent_pid.map(|pp| (p.pid, pp)))
        .collect();
    processes
        .iter()
        .filter(|p| belongs_to(p.pid, root, &by_pid))
        .collect()
}

fn belongs_to(mut pid: u32, root: u32, parent_of: &HashMap<u32, u32>) -> bool {
    for _ in 0..64 {
        if pid == root {
            return true;
        }
        match parent_of.get(&pid) {
            Some(&pp) if pp != 0 && pp != 1 && pp != pid => pid = pp,
            _ => return false,
        }
    }
    false
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
    thread::sleep(SETTLE);
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

    /// One pass, no settle: names and parent pointers for every process,
    /// which is what an Apps expansion needs. CPU is not part of the
    /// contract — that is why [`scan`] exists.
    #[test]
    fn list_processes_returns_the_uncapped_table() {
        let processes = list_processes().expect("collect");
        assert!(
            processes.len() > 50,
            "only {} processes — the cap is still being applied",
            processes.len()
        );
        assert!(
            processes.iter().any(|p| p.parent_pid.is_some()),
            "no parent pointers — the expansion cannot walk a tree"
        );
    }

    fn proc(pid: u32, parent: Option<u32>, name: &str) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            name: name.into(),
            cmd: String::new(),
            cpu_usage_percent: 0.0,
            cpu_time_ms: 0,
            memory_bytes: 0,
            phys_footprint_bytes: None,
            virtual_memory_bytes: 0,
            run_time_secs: 0,
            parent_pid: parent,
            user_id: None,
            status: String::new(),
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    #[test]
    fn tree_members_follow_parent_pointers_through_the_table() {
        let table = vec![
            proc(19477, Some(1), "zed"),
            proc(22573, Some(19477), "rust-analyzer"),
            proc(22659, Some(22573), "proc-macro"),
            proc(99, Some(1), "Finder"),
            proc(50, Some(19477), "login"),
            // Intermediate parent not in the materialised table.
            proc(51, Some(20463), "zsh"),
        ];
        let names: Vec<&str> = tree_members(19477, &table)
            .into_iter()
            .map(|p| p.name.as_str())
            .collect();
        assert!(names.contains(&"zed"));
        assert!(names.contains(&"rust-analyzer"));
        assert!(names.contains(&"proc-macro"));
        assert!(names.contains(&"login"), "direct child of the root");
        assert!(!names.contains(&"zsh"), "broken chain is not guessed");
        assert!(!names.contains(&"Finder"));
        assert_eq!(tree_members(99, &table).len(), 1);
    }
}
