//! The Processes page's own rules: the collector's top list ranked by the
//! 60s rolling average and reordered by the page's sort, what a row's meter
//! measures, and the words an expansion uses for a process's command,
//! parent, IO and user. The list is not the whole table — zstats
//! materialises `max-processes` (default 50) by CPU then memory (the gpui
//! page, `views/processes.rs`, says why). Moved out of it for the NSH
//! Processes page (phase 4e of docs/nsh-migration.md).

use crate::alertbars::{BarDisplay, Resolved, mem_bar_text, pct_bar_text};
use crate::format;
use crate::i18n;
#[cfg(not(target_os = "macos"))]
use crate::procscan;
use crate::rows::BAR_FLOOR_PERCENT;
use crate::state::{ProcSort, ZStatsAppState};
use std::cmp::Reverse;
use zstats::Tick;
use zstats::snapshot::ProcessSnapshot;

/// What a row's meter is drawn from, and what a full track means.
///
/// The measure follows the list's ranking key on purpose. With rows
/// ordered by memory and meters drawn from CPU — which is what this used
/// to do — one list carries two different rankings, and the sort reads as
/// broken. Name order ranks by no magnitude at all, so there the page's
/// own subject keeps the track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BarMeasure {
    Cpu,
    Memory,
    DiskIo,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarScale {
    pub measure: BarMeasure,
    full: f32,
}

/// Combined disk read+write below this does not get to define "full" —
/// same number as the Network tab, and the same reason: a machine doing
/// a few kB/s of housekeeping must not paint a saturated bar.
const IO_FLOOR_BYTES: f32 = 64.0 * 1024.0;

impl BarScale {
    /// Which page the maxima come from is the caller's call: the top
    /// list measures the rows it shows, the full listing measures the
    /// whole table rather than the filtered cut, so its meters do not
    /// rescale on every keystroke.
    pub fn new(sort: ProcSort, cpu_max: f32, mem_max: u64, io_max: u64) -> Self {
        let measure = match sort {
            ProcSort::Memory => BarMeasure::Memory,
            ProcSort::DiskIo => BarMeasure::DiskIo,
            ProcSort::Cpu | ProcSort::Name => BarMeasure::Cpu,
        };
        let full = match measure {
            // Memory / IO have no floor-at-one-core equivalent: bytes
            // have no natural unit to hold the track against, so the
            // largest row on the page is the only honest full. A
            // `max(1)` only keeps the division defined. IO still has
            // [`IO_FLOOR_BYTES`] so housekeeping cannot saturate it.
            BarMeasure::Memory => (mem_max as f32).max(1.0),
            BarMeasure::DiskIo => (io_max as f32).max(IO_FLOOR_BYTES),
            BarMeasure::Cpu => cpu_max.max(BAR_FLOOR_PERCENT),
        };
        Self { measure, full }
    }

    pub fn fraction(&self, p: &ProcessSnapshot, cpu: f64) -> f32 {
        match self.measure {
            BarMeasure::Memory => shown_memory(p) as f32 / self.full,
            BarMeasure::DiskIo => disk_io_bytes(p) as f32 / self.full,
            BarMeasure::Cpu => cpu as f32 / self.full,
        }
    }
}

/// Combined disk throughput, `None` read as zero so a first sample
/// (rates not yet differenced) sorts to the bottom rather than failing
/// the ranking.
pub fn disk_io_bytes(p: &ProcessSnapshot) -> u64 {
    p.read_bytes_per_sec.unwrap_or(0) + p.write_bytes_per_sec.unwrap_or(0)
}

/// The memory figure a row shows and sorts by: the physical footprint when
/// zstats reports one, resident memory otherwise.
///
/// Footprint is what macOS bills the process for — private dirty pages,
/// compressed pages, GPU/IOKit allocations — i.e. Activity Monitor's Memory
/// column. RSS misses the compressed and GPU parts entirely, which is how a
/// Metal-heavy app can read 80 MB while actually costing 300 MB. The
/// fallback exists because `proc_pid_rusage` fails on other users'
/// processes, so an unprivileged collector reports `None` for root-owned
/// daemons; RSS is then the best figure zstats has for them. Both numbers
/// come off the snapshot — nothing is derived here — and the expanded row
/// spells out which is which.
pub fn shown_memory(p: &ProcessSnapshot) -> u64 {
    p.phys_footprint_bytes.unwrap_or(p.memory_bytes)
}

/// Ranked by the 60s rolling average so the order is stable enough to read.
pub fn ranked(tick: &Tick) -> Option<Vec<(&ProcessSnapshot, f64)>> {
    let processes = tick.snapshot.processes.as_deref()?;
    let mut rows: Vec<_> = processes
        .iter()
        .map(|p| {
            let cpu = tick
                .process_stats
                .get(&p.pid)
                .map(|s| s.cpu_avg)
                .unwrap_or(p.cpu_usage_percent as f64);
            (p, cpu)
        })
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    Some(rows)
}

/// The top list's order and cut: `ranked` hands back CPU order, the
/// page's sort is applied here so it only affects this tab, then the name
/// filter, shared with the full-scan view (the query is stored
/// lowercased; names are lowercased here to match).
pub fn order(state: &ZStatsAppState, rows: &mut Vec<(&ProcessSnapshot, f64)>) {
    match state.proc_sort() {
        ProcSort::Cpu => {}
        ProcSort::Memory => rows.sort_by_key(|(p, _)| Reverse(shown_memory(p))),
        ProcSort::DiskIo => rows.sort_by_key(|(p, _)| Reverse(disk_io_bytes(p))),
        // Case-insensitive, or every capitalised app name would sort ahead of
        // every lowercase daemon — not what "by name" means to someone
        // scanning for one. `_cached_` because the key allocates: plain
        // `sort_by_key` would rebuild every lowercased name on each of the
        // O(n log n) comparisons, every frame.
        ProcSort::Name => rows.sort_by_cached_key(|(p, _)| p.name.to_lowercase()),
    }

    // The name filter, shared with the full-scan view. The query is stored
    // lowercased; names are lowercased here to match.
    let filter = state.proc_filter_text();
    if !filter.is_empty() {
        rows.retain(|(p, _)| p.name.to_lowercase().contains(filter));
    }
}

/// The two alert bars zstats holds for exactly this name — the answer
/// to "why is this row at 300% and quiet". Resolved through zstats' own
/// `ActiveThresholds` against the live template table ([`Resolved`]),
/// the same resolution the engine armed itself with; nothing here
/// evaluates a condition, it only reads the line back out.
pub fn alert_bar_rows(name: &str, state: &ZStatsAppState) -> Vec<(String, BarDisplay)> {
    let Some(file) = state.settings() else {
        return Vec::new();
    };
    let bars = Resolved::of(file);
    let eff = bars.eff();
    let total = state.latest().map(|t| t.snapshot.memory.total_bytes);
    vec![
        (
            i18n::tr("processes.bar_cpu"),
            pct_bar_text(eff.cpu.override_for(name), eff.cpu.base())
                .decided_by(bars.in_force("alert-cpu", name)),
        ),
        (
            i18n::tr("processes.bar_mem"),
            mem_bar_text(
                eff.memory.override_for(name),
                total.and_then(|t| eff.memory_bar_bytes(name, t)),
                eff.memory.base(),
            )
            .decided_by(bars.in_force("alert-mem", name)),
        ),
    ]
}

pub fn sort_indices(indices: &mut [usize], processes: &[ProcessSnapshot], sort: ProcSort) {
    match sort {
        // The collector already returns CPU descending; we still sort so
        // a filter cut (and a later cycle back to CPU) is not left in
        // whatever order the previous key left it.
        ProcSort::Cpu => indices.sort_by(|&a, &b| {
            processes[b]
                .cpu_usage_percent
                .total_cmp(&processes[a].cpu_usage_percent)
        }),
        ProcSort::Memory => {
            indices.sort_by_key(|&i| Reverse(shown_memory(&processes[i])));
        }
        ProcSort::DiskIo => {
            indices.sort_by_key(|&i| Reverse(disk_io_bytes(&processes[i])));
        }
        ProcSort::Name => indices.sort_by_cached_key(|&i| processes[i].name.to_lowercase()),
    }
}

/// If `cmd` lives inside a `.app` bundle, the bundle's own name.
///
/// `/Applications/Safari.app/Contents/MacOS/Safari --foo` → `Safari.app`.
/// A helper deep inside Xcode still reports `Xcode.app`, which is the
/// thing a person looking at the path actually wants to confirm.
pub fn bundle_name(cmd: &str) -> Option<&str> {
    let end = cmd.find(".app/").map(|i| i + 4).or_else(|| {
        cmd.find(".app ")
            .or_else(|| cmd.ends_with(".app").then_some(cmd.len() - 4))
            .map(|i| i + 4)
    })?;
    let start = cmd[..end].rfind('/')? + 1;
    (start < end).then(|| &cmd[start..end])
}

pub fn cmd_headline(cmd: &str) -> &str {
    bundle_name(cmd).unwrap_or(cmd)
}

/// The parent's name when the process table cannot supply it — the two
/// pids that are never *in* the table.
///
/// macOS has one init and one kernel task, so the names are fixed.
/// Linux's pid 1 is whichever init the distribution ships — `systemd`
/// on most, `init`, `runit`, `openrc-init` elsewhere — and
/// `/proc/1/comm` says which, so it is read rather than guessed (a
/// file read, once per paint of one row). pid 0 is the kernel's own
/// parent and has no comm to read.
pub fn well_known_name(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        match pid {
            0 => Some("kernel_task".into()),
            1 => Some("launchd".into()),
            _ => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        match pid {
            0 => Some("kernel".into()),
            1 => procscan::comm(1).or_else(|| Some("init".into())),
            _ => None,
        }
    }
}

pub fn parent_display(ppid: Option<u32>, name: Option<&str>) -> String {
    let Some(ppid) = ppid else {
        return format::PLACEHOLDER.to_string();
    };
    let known = if name.is_none() {
        well_known_name(ppid)
    } else {
        None
    };
    match name.or(known.as_deref()) {
        Some(name) => format!("{name} · {ppid}"),
        None => ppid.to_string(),
    }
}

pub fn disk_io_display(read: Option<u64>, write: Option<u64>) -> String {
    match (read, write) {
        (None, None) => format::PLACEHOLDER.to_string(),
        (r, w) => format!("R {} · W {}", format::rate(r), format::rate(w)),
    }
}

pub fn show_user(user_id: Option<&str>, current: Option<&str>) -> bool {
    match (user_id, current) {
        (Some(u), Some(c)) => u != c,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

pub fn current_user_id() -> Option<String> {
    #[cfg(unix)]
    {
        // SAFETY: `getuid` takes nothing and cannot fail.
        Some(unsafe { libc::getuid() }.to_string())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_name_reads_the_app_wrapper() {
        assert_eq!(
            bundle_name("/Applications/企业微信.app/Contents/MacOS/企业微信"),
            Some("企业微信.app")
        );
        assert_eq!(
            bundle_name("/Applications/Safari.app/Contents/MacOS/Safari --foo"),
            Some("Safari.app")
        );
        assert_eq!(
            bundle_name("/Applications/Xcode.app/Contents/Developer/usr/bin/git"),
            Some("Xcode.app")
        );
        assert_eq!(bundle_name("/usr/sbin/cupsd"), None);
        assert_eq!(bundle_name("node"), None);
        assert_eq!(cmd_headline("/usr/sbin/cupsd"), "/usr/sbin/cupsd");
    }

    #[test]
    fn parent_display_names_launchd_and_known_pids() {
        assert_eq!(parent_display(None, Some("zed")), format::PLACEHOLDER);
        assert_eq!(parent_display(Some(42), Some("zed")), "zed · 42");
        assert_eq!(parent_display(Some(99), None), "99");
        #[cfg(target_os = "macos")]
        {
            assert_eq!(parent_display(Some(1), None), "launchd · 1");
            assert_eq!(parent_display(Some(0), None), "kernel_task · 0");
        }
        // pid 1's name is whatever this machine's init calls itself; the
        // point is that it is *named*, not left as a bare number.
        #[cfg(not(target_os = "macos"))]
        {
            let init = parent_display(Some(1), None);
            assert!(init.ends_with(" · 1") && init != "1", "{init}");
            assert_eq!(parent_display(Some(0), None), "kernel · 0");
        }
    }

    #[test]
    fn disk_io_marks_read_and_write() {
        assert_eq!(disk_io_display(None, None), format::PLACEHOLDER);
        assert_eq!(disk_io_display(Some(0), Some(0)), "R 0 B/s · W 0 B/s");
        assert_eq!(
            disk_io_display(Some((17.8 * 1024.0 * 1024.0) as u64), Some(0)),
            "R 17.8 MB/s · W 0 B/s"
        );
    }

    #[test]
    fn foreign_user_hides_the_current_uid() {
        assert!(!show_user(Some("501"), Some("501")));
        assert!(show_user(Some("0"), Some("501")));
        assert!(show_user(Some("501"), None));
        assert!(!show_user(None, Some("501")));
    }

    /// A list may only draw one ranking. Sorting by memory and filling
    /// the meters from CPU is two, and the second one reads as the sort
    /// having failed.
    #[test]
    fn the_meter_measures_whatever_the_list_is_sorted_by() {
        let heavy = snap("heavy", 20.0, 8_000_000_000);
        let hot = snap("hot", 400.0, 100_000_000);

        let by_cpu = BarScale::new(ProcSort::Cpu, 400.0, 8_000_000_000, 0);
        assert_eq!(by_cpu.fraction(&hot, 400.0), 1.0, "hottest fills the track");
        assert_eq!(by_cpu.fraction(&heavy, 20.0), 20.0 / 400.0);

        let by_mem = BarScale::new(ProcSort::Memory, 400.0, 8_000_000_000, 0);
        assert_eq!(by_mem.fraction(&heavy, 20.0), 1.0, "largest fills it");
        assert_eq!(
            by_mem.fraction(&hot, 400.0),
            100_000_000.0 / 8_000_000_000.0
        );

        // Name order ranks by no magnitude, so the page's own subject
        // keeps the track.
        let by_name = BarScale::new(ProcSort::Name, 400.0, 8_000_000_000, 0);
        assert_eq!(by_name.fraction(&hot, 400.0), 1.0);

        // A quiet page does not stretch its hottest 8% across the track.
        let quiet = BarScale::new(ProcSort::Cpu, 8.0, 1_000, 0);
        assert_eq!(quiet.fraction(&snap("idle", 8.0, 1_000), 8.0), 0.08);
        // An empty page divides by something.
        assert_eq!(
            BarScale::new(ProcSort::Memory, 0.0, 0, 0).fraction(&snap("x", 0.0, 0), 0.0),
            0.0
        );

        let mut writer = snap("writer", 1.0, 1);
        writer.read_bytes_per_sec = Some(0);
        writer.write_bytes_per_sec = Some(8 * 1024 * 1024);
        let mut quiet_io = snap("quiet", 1.0, 1);
        quiet_io.read_bytes_per_sec = Some(1024);
        quiet_io.write_bytes_per_sec = Some(0);
        let by_io = BarScale::new(ProcSort::DiskIo, 400.0, 8_000_000_000, 8 * 1024 * 1024);
        assert_eq!(by_io.fraction(&writer, 1.0), 1.0, "busiest fills the track");
        assert!(
            by_io.fraction(&quiet_io, 1.0) < 0.01,
            "housekeeping is a sliver, not a full bar"
        );
    }

    #[test]
    fn shown_memory_prefers_footprint_and_falls_back_to_rss() {
        let mut p = snap("zed", 0.0, 80);
        assert_eq!(shown_memory(&p), 80, "no footprint → RSS");
        p.phys_footprint_bytes = Some(300);
        assert_eq!(shown_memory(&p), 300, "footprint wins when reported");
    }

    fn snap(name: &str, cpu: f32, mem: u64) -> ProcessSnapshot {
        ProcessSnapshot {
            pid: 1,
            name: name.into(),
            display_name: None,
            cmd: String::new(),
            cpu_usage_percent: cpu,
            cpu_time_ms: 0,
            memory_bytes: mem,
            phys_footprint_bytes: None,
            virtual_memory_bytes: 0,
            run_time_secs: 0,
            parent_pid: None,
            user_id: None,
            status: String::new(),
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    #[test]
    fn full_scan_sorts_the_visible_cut() {
        let processes = vec![
            snap("zed", 2.0, 300),
            snap("Chrome", 40.0, 80),
            snap("ice", 1.0, 900),
        ];
        let mut idx = vec![0, 1, 2];
        sort_indices(&mut idx, &processes, ProcSort::Cpu);
        assert_eq!(idx, vec![1, 0, 2]);
        sort_indices(&mut idx, &processes, ProcSort::Memory);
        assert_eq!(idx, vec![2, 0, 1]);
        sort_indices(&mut idx, &processes, ProcSort::Name);
        assert_eq!(idx, vec![1, 2, 0]); // Chrome, ice, zed

        let mut io = processes;
        io[0].write_bytes_per_sec = Some(100);
        io[1].read_bytes_per_sec = Some(50);
        io[2].read_bytes_per_sec = None;
        io[2].write_bytes_per_sec = None;
        let mut idx = vec![0, 1, 2];
        sort_indices(&mut idx, &io, ProcSort::DiskIo);
        assert_eq!(idx, vec![0, 1, 2], "combined R+W, None sorts as zero");
    }

    #[test]
    fn missing_disk_rates_sort_as_zero() {
        let mut a = snap("a", 0.0, 0);
        a.read_bytes_per_sec = None;
        a.write_bytes_per_sec = None;
        let mut b = snap("b", 0.0, 0);
        b.read_bytes_per_sec = Some(10);
        b.write_bytes_per_sec = Some(5);
        assert_eq!(disk_io_bytes(&a), 0);
        assert_eq!(disk_io_bytes(&b), 15);
    }
}
