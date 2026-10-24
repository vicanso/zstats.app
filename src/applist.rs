//! The Apps page's own rules: which trees the list shows and in what order,
//! what a row's figures and meter say, and when a row spends width on its
//! pid. Whole process trees, summed by zstats over the full table and
//! capped at `max-processes` groups ranked by CPU (the gpui page,
//! `views/apps.rs`, says why). Moved out of it for the NSH Apps page (phase
//! 4d of docs/nsh-migration.md).

use crate::alertbars::{self, BarDisplay, Resolved};
use crate::i18n;
use crate::rows::BAR_FLOOR_PERCENT;
use crate::state::{AppSort, ZStatsAppState};
use crate::{format, trend};
use rust_i18n::t;
use std::cmp::Reverse;
use std::collections::HashSet;
use zstats::snapshot::ProcessGroupSnapshot;

/// The top list's rows: the collector's set ranked by CPU, reordered by
/// the page's sort, cut by the name filter. The collector already ranked
/// by CPU and capped; this only reorders that set — the same contract as
/// the Processes list's sort. The filter matches the bundle, the
/// executable, or the job face a bare tree wears (a `login` compile
/// shows as `cargo`).
pub fn top_rows<'a>(
    state: &ZStatsAppState,
    groups: &'a [ProcessGroupSnapshot],
) -> Vec<&'a ProcessGroupSnapshot> {
    let mut rows: Vec<_> = groups.iter().collect();
    rows.sort_by(|a, b| b.cpu_usage_percent.total_cmp(&a.cpu_usage_percent));
    match state.app_sort() {
        AppSort::Cpu => {}
        AppSort::Memory => rows.sort_by_key(|g| Reverse(shown_memory(g))),
    }
    let filter = state.proc_filter_text();
    if !filter.is_empty() {
        rows.retain(|g| {
            g.name.to_lowercase().contains(filter)
                || trend::tree_key(g).to_lowercase().contains(filter)
                || face_of(g, state).text().to_lowercase().contains(filter)
        });
    }
    rows
}

/// Tick rates, plus the full table and its process groups once Apps has
/// asked for topology (a job face, or an expansion). The listing is
/// one-pass (CPU 0); the face scores the jobs from the tick, same
/// overlay the member rows already paint.
pub fn face_of(g: &ProcessGroupSnapshot, state: &ZStatsAppState) -> trend::Face {
    let live = state
        .latest()
        .and_then(|t| t.snapshot.processes.as_deref())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    trend::tree_face(
        g,
        state.member_processes().unwrap_or(live),
        live,
        state.member_pgids(),
    )
}

/// Both whole-app alert bars for this tree's name — zstats' `app_cpu` /
/// `app_mem` rules resolved through the live template table, in the same
/// words as the process expansion (`alertbars::pct_bar_text`).
pub fn alert_bars(g: &ProcessGroupSnapshot, state: &ZStatsAppState) -> (BarDisplay, BarDisplay) {
    let Some(file) = state.settings() else {
        return (
            BarDisplay::plain(format::PLACEHOLDER),
            BarDisplay::plain(format::PLACEHOLDER),
        );
    };
    let bars = Resolved::of(file);
    let eff = bars.eff();
    let total = state.latest().map(|t| t.snapshot.memory.total_bytes);
    (
        alertbars::pct_bar_text(eff.app_cpu.override_for(&g.name), eff.app_cpu.base())
            .decided_by(bars.in_force("alert-app-cpu", &g.name)),
        alertbars::mem_bar_text(
            eff.app_memory.override_for(&g.name),
            total.and_then(|t| eff.app_memory_bar_bytes(&g.name, t)),
            eff.app_memory.base(),
        )
        .decided_by(bars.in_force("alert-app-mem", &g.name)),
    )
}

/// What a full track means. Memory has no one-core equivalent, so the
/// largest tree on this page is full; CPU keeps the process list's floor
/// so a quiet page does not stretch 8% across the trough.
pub fn bar_full_for<'a>(
    sort: AppSort,
    groups: impl Iterator<Item = &'a ProcessGroupSnapshot>,
) -> f32 {
    match sort {
        AppSort::Cpu => groups
            .map(|g| g.cpu_usage_percent)
            .fold(0.0, f32::max)
            .max(BAR_FLOOR_PERCENT),
        AppSort::Memory => groups.map(shown_memory).max().unwrap_or(0) as f32,
    }
}

pub fn bar_fraction(sort: AppSort, g: &ProcessGroupSnapshot, full: f32) -> f32 {
    let full = full.max(1.0);
    match sort {
        AppSort::Cpu => g.cpu_usage_percent / full,
        AppSort::Memory => shown_memory(g) as f32 / full,
    }
}

pub fn sort_indices(indices: &mut [usize], groups: &[ProcessGroupSnapshot], sort: AppSort) {
    match sort {
        AppSort::Cpu => indices.sort_by(|&a, &b| {
            groups[b]
                .cpu_usage_percent
                .total_cmp(&groups[a].cpu_usage_percent)
        }),
        AppSort::Memory => indices.sort_by_key(|&i| Reverse(shown_memory(&groups[i]))),
    }
}

/// The memory figure a group row shows: the summed physical footprint
/// when zstats could take one, its summed RSS otherwise.
///
/// The same rule and the same fallback as `processes::shown_memory`,
/// deliberately — one program appearing in both tabs must be measured
/// the same way. RSS alone understates a GUI tree badly: compressed
/// pages and GPU/IOKit allocations are invisible to it, which is the
/// whole reason the process rows moved to footprint. zstats sums member
/// footprints over the FULL table (a member the kernel refused is
/// counted by its RSS), so the total is never partial and never worse
/// than the resident sum it replaces.
pub fn shown_memory(g: &ProcessGroupSnapshot) -> u64 {
    g.phys_footprint_bytes.unwrap_or(g.memory_bytes)
}

/// The row's left-hand facts: how many processes the tree holds, and the
/// pid at its root.
///
/// The count used to wear an outlined pill — the same chrome as the
/// header's All chip, on something that does not click, repeated down
/// every row. It is plain text now, like the pid line a process row
/// carries in the same slot.
///
/// The pid is here because names repeat: two `login` trees are two
/// terminal sessions, and with nothing to tell them apart the list reads
/// as if it rendered a row twice.
pub fn tree_meta(g: &ProcessGroupSnapshot, ambiguous: bool) -> String {
    let count = if g.process_count == 1 {
        i18n::tr("apps.one_process")
    } else {
        t!("apps.n_processes", count = g.process_count).to_string()
    };
    if !ambiguous {
        return count;
    }
    format!("{count} · {}", t!("processes.pid_only", pid = g.root_pid))
}

/// Which names this listing shows more than once. Only those rows spend
/// width on a pid: at 320px a busy row already carries
/// `R 253 kB/s · W 116 kB/s · 1.9 GB` on the right, and an unconditional
/// pid would push the left half into an ellipsis — truncating the very
/// digits it was added to disambiguate.
pub fn repeated_names<'a>(names: impl Iterator<Item = &'a str>) -> HashSet<&'a str> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut twice: HashSet<&str> = HashSet::new();
    for name in names {
        if !seen.insert(name) {
            twice.insert(name);
        }
    }
    twice
}

/// The row's IO, left of its memory. Memory goes **last**, so it lands on
/// the row's right edge in every row and forms a column the eye can run
/// down; with it first — as it was — the IO half aligned, and IO is the
/// part that comes and goes.
///
/// Only the sides that moved: zero draws the same nothing as "not
/// collected". Most trees idle at R 0 B/s · W 0 B/s, and a lone
/// "W 0 B/s" beside a live read was the same ink without information.
/// The expanded detail keeps the explicit numbers.
pub fn io_line(read: Option<u64>, write: Option<u64>) -> Option<String> {
    let sides: Vec<String> = [("R", read), ("W", write)]
        .into_iter()
        .filter(|(_, rate)| rate.unwrap_or(0) > 0)
        .map(|(label, rate)| format!("{label} {}", format::rate(rate)))
        .collect();
    (!sides.is_empty()).then(|| sides.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One program in two tabs must be measured one way. This mirrors
    /// `processes::shown_memory_prefers_footprint_and_falls_back_to_rss`
    /// exactly — if either rule ever changes alone, a browser reads two
    /// different sizes on two pages of the same panel.
    #[test]
    fn shown_memory_prefers_footprint_and_falls_back_to_rss() {
        let mut g = ProcessGroupSnapshot {
            root_pid: 1,
            name: "Google Chrome".into(),
            display_name: None,
            process_count: 37,
            cpu_usage_percent: 0.0,
            memory_bytes: 80,
            phys_footprint_bytes: None,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        };
        assert_eq!(shown_memory(&g), 80, "no footprint → the resident sum");
        g.phys_footprint_bytes = Some(300);
        assert_eq!(shown_memory(&g), 300, "footprint wins where there is one");
    }

    /// Two `login` trees are two terminal sessions; without the pid the
    /// list reads as if it rendered a row twice. A unique name gets none
    /// — the row has no width to spare for a disambiguator that
    /// disambiguates nothing.
    #[test]
    fn only_a_repeated_name_earns_a_pid() {
        let repeated = repeated_names(["login", "Finder", "login", "WeChat"].into_iter());
        assert!(repeated.contains("login"));
        assert!(!repeated.contains("Finder"));
        assert_eq!(repeated.len(), 1, "only the one that actually repeats");

        let g = ProcessGroupSnapshot {
            root_pid: 4321,
            name: "login".into(),
            display_name: None,
            process_count: 4,
            cpu_usage_percent: 0.0,
            memory_bytes: 0,
            phys_footprint_bytes: None,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        };
        // Asserted on the separator this function owns, not on
        // translated text: the locale files are not loaded here, and a
        // test that reads one would be testing rust-i18n's fallback.
        assert!(
            !tree_meta(&g, false).contains(" · "),
            "a unique name carries the count alone"
        );
        assert!(
            tree_meta(&g, true).contains(" · "),
            "a repeated one gains a second fact"
        );
    }

    #[test]
    fn io_line_stays_off_when_uncollected_or_idle() {
        assert_eq!(io_line(None, None), None);
        // Measured zero draws nothing on the row either — most trees idle,
        // and "R 0 B/s · W 0 B/s" on each was ink without information.
        assert_eq!(io_line(Some(0), Some(0)), None);
        assert_eq!(io_line(Some(0), None), None);
        // Only the side that moved.
        assert_eq!(io_line(Some(2048), Some(0)), Some("R 2 kB/s".into()));
        assert_eq!(io_line(None, Some(2048)), Some("W 2 kB/s".into()));
        assert_eq!(
            io_line(Some(2048), Some(4096)),
            Some("R 2 kB/s · W 4 kB/s".into())
        );
    }

    fn group(name: &str, cpu: f32, mem: u64) -> ProcessGroupSnapshot {
        ProcessGroupSnapshot {
            root_pid: 1,
            name: name.into(),
            display_name: None,
            process_count: 1,
            cpu_usage_percent: cpu,
            memory_bytes: mem,
            phys_footprint_bytes: None,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    #[test]
    fn the_meter_follows_the_sort_and_the_full_list_reorders() {
        let groups = vec![
            group("zed", 2.0, 300),
            group("Chrome", 40.0, 80),
            group("ice", 1.0, 900),
        ];
        let mut idx = vec![0, 1, 2];
        sort_indices(&mut idx, &groups, AppSort::Cpu);
        assert_eq!(idx, vec![1, 0, 2]);
        sort_indices(&mut idx, &groups, AppSort::Memory);
        assert_eq!(idx, vec![2, 0, 1]);

        let cpu_full = bar_full_for(AppSort::Cpu, groups.iter());
        assert_eq!(
            cpu_full, BAR_FLOOR_PERCENT,
            "40% must not get to define full"
        );
        assert_eq!(
            bar_fraction(AppSort::Cpu, &groups[1], cpu_full),
            40.0 / BAR_FLOOR_PERCENT
        );
        let hot = group("compile", 210.0, 10);
        let busy = [hot.clone(), groups[0].clone()];
        let busy_full = bar_full_for(AppSort::Cpu, busy.iter());
        assert_eq!(
            bar_fraction(AppSort::Cpu, &hot, busy_full),
            1.0,
            "over a core, the hottest fills"
        );
        let mem_full = bar_full_for(AppSort::Memory, groups.iter());
        assert_eq!(
            bar_fraction(AppSort::Memory, &groups[2], mem_full),
            1.0,
            "largest fills it"
        );
        assert_eq!(
            bar_fraction(AppSort::Memory, &groups[0], mem_full),
            300.0 / 900.0
        );
    }
}
