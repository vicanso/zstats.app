//! Hour-window CPU trend per application tree — the question the top
//! list is structurally blind to.
//!
//! Same class as [`crate::watch`]: a display-layer observer answering
//! something zstats cannot. CPU% is a rate, and a rate has no memory —
//! the snapshot cannot tell "always 30%" from "was 2% ten minutes ago",
//! and zstats' rolling averages reach back only 60 seconds. Yet "who
//! *changed*" is usually why the panel got opened at all: the resident
//! that is always on top is normal, the tree that climbed out of nowhere
//! is the news. Answering that takes an hour of history nobody else
//! keeps, so it is kept here — fed from the resident collector's ticks,
//! alive while the panel is closed, and **display only**: no
//! `AlertEvent`, no notification, colour untouched (a rise is news, not
//! a threshold).
//!
//! Storage is sixty minute-slots per tree, `u16` percent-of-one-core —
//! the unit every CPU figure in the app already speaks. A minute keeps
//! the **max** of its samples: "rising" asks what a tree reached, and a
//! mean would let one idle sample inside the minute talk a real climb
//! back down. Whole percent is plenty for a trend, and the headroom
//! (65 534% ≈ 655 cores) means saturation is theoretical. ~100 trees ×
//! 60 × 2 B lands under 20 KB with the map around it.
//!
//! Two kinds of absence, kept distinct because they mean different
//! things (the same honesty rule that keeps History off line charts):
//!
//! - **The collector did not report a minute** (machine asleep, panel
//!   process just launched): unknowable — the minute is excluded from
//!   both sides of the comparison. A global ring of reported minutes is
//!   what tells this case apart.
//! - **The collector reported, but this tree was not in the groups**:
//!   the groups are the top-`max-processes` by CPU, so absence means
//!   "below that cut" — genuinely quiet, and it counts as zero. This is
//!   what lets a tree that climbs out of *nothing* register the full
//!   climb instead of having no baseline.
//!
//! Keyed by tree name, accepting that two same-named trees merge to
//! their max: a root pid key would be broken by every app restart, and
//! pid reuse would splice two different programs into one curve — the
//! same reason the alert cards gate their buttons on `SeenAlert::live`.
//!
//! No gpui types, minutes handed in by the caller — testable against
//! hand-built sequences, like `watch.rs`.

use crate::fullscan;
use std::collections::HashMap;
use zstats::snapshot::{ProcessGroupSnapshot, ProcessSnapshot};

/// Minutes of history per tree: the hour the card talks about.
const SLOTS: usize = 60;

/// Slot value for "no reading survived for this minute".
const NO_DATA: u16 = u16::MAX;

/// Highest storable reading, % of one core (the value below the
/// sentinel). 655 cores' worth — saturation is a formality.
const MAX_PCT: u16 = u16::MAX - 1;

/// The "now" side of the comparison: mean of the newest reported
/// minutes. Five, not one — a single minute is one scheduler mood, and
/// the card should say "has been climbing", not "just blinked".
const RECENT_MINUTES: u64 = 5;

/// Reported minutes the baseline needs before a rise is worth stating.
/// Below this the "earlier hour" is a handful of samples and the delta
/// is mostly noise; the card simply has no verdict yet (first minutes
/// after launch), which is honest.
const BASELINE_MIN: usize = 5;

/// One ring of sixty minute-slots addressed by absolute minute number.
/// Skipped minutes are cleared on advance, so a slot can never leak a
/// reading from an hour ago into the current window.
struct Ring {
    slots: [u16; SLOTS],
    /// Absolute minute of the newest written slot.
    head: u64,
}

impl Ring {
    fn new(minute: u64, value: u16) -> Self {
        let mut ring = Ring {
            slots: [NO_DATA; SLOTS],
            head: minute,
        };
        ring.slots[(minute % SLOTS as u64) as usize] = value;
        ring
    }

    /// Advance to `minute` (clearing everything skipped) and merge
    /// `value` in by max — see the module doc for why max.
    fn record(&mut self, minute: u64, value: u16) {
        if minute < self.head {
            // A clock that went backwards is not a reading.
            return;
        }
        if minute > self.head {
            let gap = (minute - self.head).min(SLOTS as u64);
            for step in 1..=gap {
                self.slots[((self.head + step) % SLOTS as u64) as usize] = NO_DATA;
            }
            self.head = minute;
        }
        let slot = &mut self.slots[(minute % SLOTS as u64) as usize];
        *slot = if *slot == NO_DATA {
            value
        } else {
            (*slot).max(value)
        };
    }

    /// The reading for an absolute minute, if it is inside the window
    /// and was actually written.
    fn at(&self, minute: u64) -> Option<u16> {
        if minute > self.head || self.head - minute >= SLOTS as u64 {
            return None;
        }
        let value = self.slots[(minute % SLOTS as u64) as usize];
        (value != NO_DATA).then_some(value)
    }
}

/// The hour of per-tree CPU history behind Overview's "climbing" rows.
#[derive(Default)]
pub struct AppTrend {
    /// Which minutes the collector reported at all — what separates
    /// "machine was asleep" from "tree was quiet".
    reported: Option<Ring>,
    apps: HashMap<String, Ring>,
}

/// Stable identity for the trend, filters that mean "this tree", and
/// the expansion's matchable name. Not the row title — see [`tree_face`].
///
/// `display_name` first (zstats 0.5.3): the bundle's name where the
/// executable's own says nothing — every stock-packaged Electron app
/// reports `Electron` to the kernel, and keying the trend on that merged
/// unrelated programs into one curve. Thresholds, templates and quit
/// still use `g.name`.
pub fn tree_key(g: &ProcessGroupSnapshot) -> &str {
    g.display_name.as_deref().unwrap_or(&g.name)
}

/// A job must own at least this share of the tree's CPU before the row
/// wears its leader's name. Below a third, the session is still the
/// story (idle `login` + a 2% compile is not "cargo").
const FACE_SHARE: f32 = 1.0 / 3.0;

/// What the list should call this tree.
///
/// A tree rooted in a bare executable — no bundle, so no `display_name`:
/// `login`, `sshd-session`, `tmux`, a daemon — is named after the **job**
/// its CPU belongs to. A job is the kernel's own unit: a job-control
/// shell gives every command it launches a fresh process group, and what
/// that command forks inherits it. So `login → zsh → cargo → rustc × 10`
/// is three groups — `{login}`, `{zsh}`, `{cargo, rustc…}` — and the
/// hot one's leader is `cargo`: what was typed, what a quit would land
/// on, and stable for the whole build whether one `rustc` is running or
/// ten, or the tail is `rustc → cc → ld`. No list of shell names: the
/// shell is just a ~0% group, whatever it is called. A wrapper that
/// leads the job (`sudo make`) is the face — honest, and stable.
///
/// A bundle root is an application, and its helpers are its own
/// business: a renderer in its own process group must not rebrand the
/// app it belongs to, so the gate is structural, not a name.
///
/// [`tree_key`] stays on the launchd child so the hour-window and the
/// alert bars do not jump mid-compile.
///
/// `topology` is who belongs to the tree (the expansion's full table
/// when we have one — ppid chains intact — otherwise the tick). `live`
/// is who has a rate this tick: overlay those percentages when scoring
/// the jobs, same as the member rows. A one-pass listing's CPU is 0 by
/// construction and must not decide the name; empty `live` falls back
/// to whatever `topology` itself carried. `pgids` is pid → process
/// group from the same photograph (`procscan::process_groups`); empty
/// before the table lands, and the tree keeps its own name until then.
pub fn tree_face(
    g: &ProcessGroupSnapshot,
    topology: &[ProcessSnapshot],
    live: &[ProcessSnapshot],
    pgids: &HashMap<u32, u32>,
) -> String {
    let presented = tree_key(g);
    if g.display_name.is_some() || pgids.is_empty() {
        return presented.to_string();
    }
    let tree_cpu = g.cpu_usage_percent;
    if tree_cpu <= 0.0 {
        return presented.to_string();
    }
    let live_cpu: HashMap<u32, f32> = live.iter().map(|p| (p.pid, p.cpu_usage_percent)).collect();
    let cpu_of = |p: &ProcessSnapshot| live_cpu.get(&p.pid).copied().unwrap_or(p.cpu_usage_percent);
    let members = fullscan::tree_members(g.root_pid, topology);
    // A member the photograph does not know is a job of its own.
    let job_of = |p: &ProcessSnapshot| pgids.get(&p.pid).copied().unwrap_or(p.pid);
    let mut by_job: HashMap<u32, f32> = HashMap::new();
    for m in &members {
        *by_job.entry(job_of(m)).or_default() += cpu_of(m);
    }
    let Some((&job, &cpu)) = by_job.iter().max_by(|a, b| a.1.total_cmp(b.1)) else {
        return presented.to_string();
    };
    if cpu / tree_cpu < FACE_SHARE {
        return presented.to_string();
    }
    // The leader (pid == pgid) while the tree still has it. A job whose
    // leader exited keeps running under its pgid — name the member
    // nearest the root, which is the one the rest descend from.
    let parent_of: HashMap<u32, u32> = members
        .iter()
        .filter_map(|p| p.parent_pid.map(|pp| (p.pid, pp)))
        .collect();
    let depth = |pid: u32| {
        let mut pid = pid;
        let mut steps = 0u32;
        while pid != g.root_pid && steps < 64 {
            match parent_of.get(&pid) {
                Some(&pp) => pid = pp,
                None => break,
            }
            steps += 1;
        }
        steps
    };
    let face = members
        .iter()
        .find(|m| m.pid == job)
        .or_else(|| {
            members
                .iter()
                .filter(|m| job_of(m) == job)
                .min_by_key(|m| depth(m.pid))
        })
        .copied();
    match face {
        Some(p) => p.display_name.clone().unwrap_or_else(|| p.name.clone()),
        None => presented.to_string(),
    }
}

impl AppTrend {
    /// Feed one tick's application trees. `minute` is minutes since the
    /// Unix epoch — wall clock, not `Instant`, because the slots must
    /// line up across a sleep.
    pub fn sample<'a>(&mut self, minute: u64, trees: impl Iterator<Item = (&'a str, f32)>) {
        match &mut self.reported {
            Some(ring) => ring.record(minute, 1),
            None => self.reported = Some(Ring::new(minute, 1)),
        }
        for (name, pct) in trees {
            let value = pct.max(0.0).round().min(f32::from(MAX_PCT)) as u16;
            match self.apps.get_mut(name) {
                Some(ring) => ring.record(minute, value),
                None => {
                    self.apps.insert(name.to_string(), Ring::new(minute, value));
                }
            }
        }
        // A tree silent for the whole window has nothing left to say —
        // every slot it could contribute is already out of range.
        self.apps
            .retain(|_, ring| minute.saturating_sub(ring.head) < SLOTS as u64);
    }

    /// Percent-of-one-core points this tree's recent minutes sit above
    /// its earlier-hour average. `None` until enough of the hour has
    /// been reported for the comparison to mean anything.
    pub fn rise(&self, name: &str) -> Option<f32> {
        let reported = self.reported.as_ref()?;
        let ring = self.apps.get(name)?;
        let now = reported.head;
        let (mut recent_sum, mut recent_n) = (0f32, 0u32);
        let (mut base_sum, mut base_n) = (0f32, 0u32);
        for minute in now.saturating_sub(SLOTS as u64 - 1)..=now {
            if reported.at(minute).is_none() {
                // Asleep / before launch: unknowable, on neither side.
                continue;
            }
            // Reported but absent from the groups = below the collector's
            // cut = quiet. Zero, and that is a statement, not a gap.
            let value = f32::from(ring.at(minute).unwrap_or(0));
            if now - minute < RECENT_MINUTES {
                recent_sum += value;
                recent_n += 1;
            } else {
                base_sum += value;
                base_n += 1;
            }
        }
        if recent_n == 0 || (base_n as usize) < BASELINE_MIN {
            return None;
        }
        Some(recent_sum / recent_n as f32 - base_sum / base_n as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample one tree at `pct` for every minute in `minutes`.
    fn feed(trend: &mut AppTrend, name: &str, minutes: std::ops::Range<u64>, pct: f32) {
        for m in minutes {
            trend.sample(m, [(name, pct)].into_iter());
        }
    }

    /// Report minutes with no tree in them at all.
    fn idle(trend: &mut AppTrend, minutes: std::ops::Range<u64>) {
        for m in minutes {
            trend.sample(m, std::iter::empty());
        }
    }

    #[test]
    fn a_minute_keeps_the_max_of_its_samples() {
        let mut trend = AppTrend::default();
        trend.sample(10, [("zed", 40.0)].into_iter());
        trend.sample(10, [("zed", 250.0)].into_iter());
        trend.sample(10, [("zed", 90.0)].into_iter());
        // 55 quiet baseline minutes so rise() has a verdict.
        idle(&mut trend, 11..66);
        feed(&mut trend, "zed", 66..71, 250.0);
        assert_eq!(trend.rise("zed"), Some(250.0));
    }

    #[test]
    fn a_climber_reads_as_its_climb() {
        let mut trend = AppTrend::default();
        feed(&mut trend, "make", 0..55, 5.0);
        feed(&mut trend, "make", 55..60, 305.0);
        let rise = trend.rise("make").expect("a full hour has a verdict");
        assert!((rise - 300.0).abs() < 1.0, "5% → 305% is a 300-point rise");
    }

    #[test]
    fn a_flat_tree_reads_as_flat() {
        let mut trend = AppTrend::default();
        feed(&mut trend, "WindowServer", 0..60, 28.0);
        let rise = trend.rise("WindowServer").expect("verdict");
        assert!(rise.abs() < 0.5, "steady 28% is not a rise: {rise}");
    }

    #[test]
    fn climbing_out_of_nothing_counts_the_whole_climb() {
        // The card's most valuable catch: a tree that was not even in
        // the groups an hour ago. Reported-but-absent minutes are a
        // quiet baseline of zero, not a missing one.
        let mut trend = AppTrend::default();
        idle(&mut trend, 0..55);
        feed(&mut trend, "softwareupdated", 55..60, 180.0);
        assert_eq!(trend.rise("softwareupdated"), Some(180.0));
    }

    #[test]
    fn sleep_gaps_are_on_neither_side_of_the_comparison() {
        // Ten reported minutes, forty asleep, ten reported. The gap
        // must not read as a quiet baseline — the machine was not
        // running, so nothing about the tree is known.
        let mut trend = AppTrend::default();
        feed(&mut trend, "zed", 0..10, 100.0);
        // minutes 10..50 never sampled at all
        feed(&mut trend, "zed", 50..60, 100.0);
        let rise = trend.rise("zed").expect("ten baseline minutes reported");
        assert!(
            rise.abs() < 0.5,
            "steady across a sleep is still steady: {rise}"
        );
    }

    #[test]
    fn too_little_history_has_no_verdict() {
        // Four reported minutes: everything inside RECENT_MINUTES, so
        // the baseline is empty — and a baseline of almost nothing must
        // say nothing rather than guess.
        let mut trend = AppTrend::default();
        feed(&mut trend, "zed", 0..4, 200.0);
        assert_eq!(trend.rise("zed"), None);
    }

    #[test]
    fn an_hour_of_silence_evicts_the_tree() {
        let mut trend = AppTrend::default();
        feed(&mut trend, "gone", 0..2, 300.0);
        idle(&mut trend, 2..62);
        assert_eq!(trend.rise("gone"), None, "evicted, not remembered");
        assert!(trend.apps.is_empty(), "the map must not grow for a day");
    }

    #[test]
    fn the_ring_never_leaks_last_hours_reading_into_this_one() {
        // Minute 5 and minute 65 share a slot. Writing 65 must clear the
        // old reading rather than max-merge with it — a stale 400%
        // surviving into this hour would manufacture a fall — and minute
        // 5 itself must fall out of the window (65 − 5 = 60 ≥ 60).
        let mut trend = AppTrend::default();
        trend.sample(5, [("zed", 400.0)].into_iter());
        trend.sample(65, [("zed", 10.0)].into_iter());
        let ring = trend.apps.get("zed").expect("still live");
        assert_eq!(ring.at(5), None, "minute 5 is out of the window");
        assert_eq!(ring.at(65), Some(10), "the shared slot holds only 65");
        // One minute earlier the same reading was still in range: the
        // window is exactly the last sixty minutes, not fifty-nine.
        let mut edge = AppTrend::default();
        edge.sample(5, [("zed", 400.0)].into_iter());
        edge.sample(64, [("zed", 10.0)].into_iter());
        assert_eq!(edge.apps.get("zed").unwrap().at(5), Some(400));
    }

    fn proc(pid: u32, parent: Option<u32>, name: &str, cpu: f32) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            name: name.into(),
            display_name: None,
            cmd: String::new(),
            cpu_usage_percent: cpu,
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

    fn group(root: u32, name: &str, cpu: f32) -> ProcessGroupSnapshot {
        ProcessGroupSnapshot {
            root_pid: root,
            name: name.into(),
            display_name: None,
            process_count: 1,
            cpu_usage_percent: cpu,
            memory_bytes: 0,
            phys_footprint_bytes: None,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    /// `login`, `zsh` and the job are three process groups; the job's
    /// members share the leader's pid.
    fn jobs(pairs: &[(u32, u32)]) -> HashMap<u32, u32> {
        pairs.iter().copied().collect()
    }

    /// The classic session: login and zsh in groups of their own, the
    /// compile in cargo's. The face is the job's leader — what was
    /// typed — not the compiler doing the work. [`tree_key`] stays
    /// `login` so the trend does not jump when the build ends.
    #[test]
    fn a_login_tree_wears_the_job_not_the_compiler() {
        let g = group(10, "login", 100.0);
        let table = vec![
            proc(10, Some(1), "login", 0.0),
            proc(11, Some(10), "zsh", 0.1),
            proc(12, Some(11), "cargo", 0.2),
            proc(13, Some(12), "rustc", 99.7),
        ];
        let pg = jobs(&[(10, 10), (11, 11), (12, 12), (13, 12)]);
        assert_eq!(tree_key(&g), "login");
        assert_eq!(tree_face(&g, &table, &[], &pg), "cargo");
    }

    /// The case the old "hottest single process" rule got wrong: a
    /// parallel build where no one compiler passes the share, yet the
    /// job as a whole is the entire tree. And the tail of the same
    /// build — one rustc handing to cc handing to ld — reads the same.
    #[test]
    fn a_parallel_build_and_its_link_stage_wear_the_same_name() {
        let g = group(10, "login", 100.0);
        let parallel = vec![
            proc(10, Some(1), "login", 0.0),
            proc(11, Some(10), "zsh", 0.0),
            proc(12, Some(11), "cargo", 1.0),
            proc(13, Some(12), "rustc", 33.0),
            proc(14, Some(12), "rustc", 33.0),
            proc(15, Some(12), "rustc", 33.0),
        ];
        let pg = jobs(&[(10, 10), (11, 11), (12, 12), (13, 12), (14, 12), (15, 12)]);
        assert_eq!(tree_face(&g, &parallel, &[], &pg), "cargo");

        let linking = vec![
            proc(10, Some(1), "login", 0.0),
            proc(11, Some(10), "zsh", 0.0),
            proc(12, Some(11), "cargo", 0.0),
            proc(13, Some(12), "rustc", 0.0),
            proc(16, Some(13), "cc", 0.0),
            proc(17, Some(16), "ld", 100.0),
        ];
        let pg = jobs(&[(10, 10), (11, 11), (12, 12), (13, 12), (16, 12), (17, 12)]);
        assert_eq!(tree_face(&g, &linking, &[], &pg), "cargo");
    }

    /// No list of shell names: an SSH session and a wrapper-led job are
    /// handled by the same group arithmetic. `sudo` leads its job, so
    /// `sudo` is the face — stable, and what a quit would reach first.
    #[test]
    fn any_bare_root_and_any_wrapper_follow_the_groups() {
        let g = group(20, "sshd-session", 90.0);
        let table = vec![
            proc(20, Some(1), "sshd-session", 0.0),
            proc(21, Some(20), "nu", 0.0),
            proc(22, Some(21), "sudo", 0.0),
            proc(23, Some(22), "make", 2.0),
            proc(24, Some(23), "cc", 44.0),
            proc(25, Some(23), "cc", 44.0),
        ];
        let pg = jobs(&[(20, 20), (21, 21), (22, 22), (23, 22), (24, 22), (25, 22)]);
        assert_eq!(tree_face(&g, &table, &[], &pg), "sudo");
    }

    #[test]
    fn a_quiet_login_tree_keeps_its_own_name() {
        let g = group(10, "login", 8.0);
        let table = vec![
            proc(10, Some(1), "login", 0.0),
            proc(11, Some(10), "zsh", 0.1),
            proc(12, Some(11), "rustc", 2.0),
        ];
        let pg = jobs(&[(10, 10), (11, 11), (12, 12)]);
        assert_eq!(tree_face(&g, &table, &[], &pg), "login");
    }

    /// A bundle root is an application. Even when one renderer sits in
    /// a process group of its own and burns the whole tree, the row is
    /// still the app — the gate is the bundle, not a name.
    #[test]
    fn a_real_app_is_not_rebranded() {
        let mut g = group(1, "Electron", 40.0);
        g.display_name = Some("CodeBuddy CN".into());
        let table = vec![
            proc(1, Some(0), "Electron", 0.0),
            proc(2, Some(1), "Electron Helper (Renderer)", 40.0),
        ];
        let pg = jobs(&[(1, 1), (2, 2)]);
        assert_eq!(tree_key(&g), "CodeBuddy CN");
        assert_eq!(tree_face(&g, &table, &[], &pg), "CodeBuddy CN");
    }

    /// The expansion's full table is one-pass: every CPU is 0. The
    /// member rows already overlay the tick; the face must too, or a
    /// `login` whose cargo is 17% of 17% stays titled `login`.
    #[test]
    fn a_login_tree_scores_the_job_from_live_cpu() {
        let g = group(10, "login", 17.0);
        let topology = vec![
            proc(10, Some(1), "login", 0.0),
            proc(11, Some(10), "zsh", 0.0),
            proc(12, Some(11), "cargo", 0.0),
            proc(13, Some(12), "rustc", 0.0),
        ];
        let pg = jobs(&[(10, 10), (11, 11), (12, 12), (13, 12)]);
        let live = vec![proc(13, Some(12), "rustc", 17.0)];
        assert_eq!(tree_face(&g, &topology, &[], &pg), "login");
        assert_eq!(tree_face(&g, &topology, &live, &pg), "cargo");
    }

    /// A job outlives its leader: `cargo` exits the moment it finishes
    /// handing out work to a detached helper. The group is still the
    /// hot one; its topmost surviving member is the face.
    #[test]
    fn a_job_whose_leader_is_gone_names_its_topmost_member() {
        let g = group(10, "login", 50.0);
        let table = vec![
            proc(10, Some(1), "login", 0.0),
            proc(11, Some(10), "zsh", 0.0),
            proc(13, Some(11), "rustc", 10.0),
            proc(16, Some(13), "cc", 40.0),
        ];
        let pg = jobs(&[(10, 10), (11, 11), (13, 12), (16, 12)]);
        assert_eq!(tree_face(&g, &table, &[], &pg), "rustc");
    }

    /// The tick's top-N usually drops the idle shell, and the tick
    /// carries no process groups at all — which is why a collapsed row
    /// reads `login` until the full table lands, and not a guess.
    #[test]
    fn a_tick_without_the_photograph_cannot_name_the_job() {
        let g = group(10, "login", 17.0);
        let tick = vec![
            proc(10, Some(1), "login", 0.0),
            proc(12, Some(11), "cargo", 17.0),
        ];
        assert_eq!(tree_face(&g, &tick, &tick, &HashMap::new()), "login");
        let pg = jobs(&[(10, 10), (12, 12)]);
        assert_eq!(tree_face(&g, &tick, &tick, &pg), "login");
    }

    #[test]
    fn a_reading_saturates_instead_of_wrapping() {
        let mut trend = AppTrend::default();
        trend.sample(0, [("mega", 1.0e9)].into_iter());
        assert_eq!(trend.apps.get("mega").unwrap().at(0), Some(MAX_PCT));
    }
}
