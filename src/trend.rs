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
use std::time::Duration;
use zstats::snapshot::{ProcessGroupSnapshot, ProcessSnapshot};

/// Minutes of history per tree: the hour the card talks about.
const SLOTS: usize = 60;

/// The memory ring's unit: a footprint is fed in MB so it fits the same
/// `u16` slot as a CPU percent (cap ~64 GB per tree — the machine).
pub const MIB: u64 = 1 << 20;

/// A tree's footprint must have climbed this much within the hour —
/// and still be there — before the silent banner goes out. Bigger than
/// the Overview strip's floor on purpose: the strip is a glance, the
/// banner is an interruption. One gigabyte in an hour is past what a
/// browser session or an indexer does by itself on an ordinary
/// morning, and a leak at that rate fills a laptop before lunch.
pub const CREEP_NOTIFY_BYTES: u64 = 1024 * MIB;

/// How long one creep announcement stands before the same tree may
/// banner again — one full ring, dips notwithstanding. "Re-arm when
/// the climb falls back under the bar" was tried first and read a GC
/// sawtooth as a stream of fresh leaks: Chrome's footprint oscillating
/// around its high walked the climb across 1 GB every few minutes —
/// measured, three banners in 29 minutes — because a single tick can
/// never distinguish "the climb ended" from "the collector caught the
/// low tooth". By the time this clock expires, the hour the banner
/// described has slid out of the ring entirely, so whatever a
/// re-announcement measures is against a baseline newer than the last
/// banner — which is what makes it news again.
pub const CREEP_REARM: Duration = Duration::from_secs(60 * 60);

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

/// What a tree row is called: the title, and — for an application whose
/// tree a bare job is burning — that job as a muted tail (`Zed · cargo`).
/// Two fields rather than one string so the views can paint the tail
/// in a second colour; [`Face::text`] is the one-string form for filters
/// and tooltips.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Face {
    pub title: String,
    pub job: Option<String>,
}

impl Face {
    fn plain(title: &str) -> Self {
        Face {
            title: title.to_string(),
            job: None,
        }
    }

    /// `Zed · cargo`, or just the title.
    pub fn text(&self) -> String {
        match &self.job {
            Some(job) => format!("{} · {job}", self.title),
            None => self.title.clone(),
        }
    }
}

/// The tests read a face as the string a person would: `"Zed · cargo"`.
#[cfg(test)]
impl PartialEq<&str> for Face {
    fn eq(&self, other: &&str) -> bool {
        self.text() == *other
    }
}

/// The outermost `.app` bundle an executable runs from, read off its
/// command line's argv[0]: `/Applications/Zed.app/` for Zed and for
/// every helper under `Zed.app/Contents/…`, `None` for a bare
/// executable. This, and not `display_name`, is the bundle test:
/// zstats leaves `display_name` `None` both for a bare executable and
/// for a bundle that merely repeats the process name — `Google Chrome`
/// and every one of its helpers — so that field cannot tell `login`
/// from Chrome.
fn bundle_of(cmd: &str) -> Option<&str> {
    if !cmd.starts_with('/') {
        return None;
    }
    let end = cmd.find(".app/")? + ".app/".len();
    Some(&cmd[..end])
}

/// What the list should call this tree.
///
/// A tree rooted in a bare executable — not in any bundle (`bundle_of`):
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
/// app it belongs to, so the gate is structural, not a name. But a
/// bare job under an application's tree is not one of its helpers — a
/// build typed into Zed's terminal is `Zed → … → login → zsh → cargo`,
/// and "Zed 800%" reads as Zed running away. That tree keeps its title
/// and gains a muted tail, `Zed · cargo`: whose tree, and who is
/// burning. The tail needs what the face needs — a job leader from
/// outside the app's own bundle, holding a third of the tree.
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
) -> Face {
    let presented = tree_key(g);
    if pgids.is_empty() {
        return Face::plain(presented);
    }
    let tree_cpu = g.cpu_usage_percent;
    if tree_cpu <= 0.0 {
        return Face::plain(presented);
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
        return Face::plain(presented);
    };
    if cpu / tree_cpu < FACE_SHARE {
        return Face::plain(presented);
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
    let Some(leader) = members
        .iter()
        .find(|m| m.pid == job)
        .or_else(|| {
            members
                .iter()
                .filter(|m| job_of(m) == job)
                .min_by_key(|m| depth(m.pid))
        })
        .copied()
    else {
        return Face::plain(presented);
    };
    let job_name = leader.display_name.as_deref().unwrap_or(&leader.name);
    let Some(root) = members.iter().find(|m| m.pid == g.root_pid) else {
        // The photograph has the members but not the root: nothing to
        // reason about, and the tree's own name is never wrong.
        return Face::plain(presented);
    };
    let Some(bundle) = bundle_of(&root.cmd) else {
        // A bare tree is called by its job outright.
        return Face::plain(job_name);
    };
    // An application keeps its title. A leader from outside its bundle
    // — so not one of its own helpers, and not the app itself — rides
    // along as the tail. Same-bundle is the test, not "has a bundle":
    // Xcode's `make` has one, and it is still foreign to Zed's tree.
    let own = leader.pid == g.root_pid || bundle_of(&leader.cmd) == Some(bundle);
    Face {
        title: presented.to_string(),
        job: (!own).then(|| job_name.to_string()),
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

    /// The newest reported minutes against the **earliest** reported
    /// ones in the window — the full climb, where [`rise`](Self::rise)
    /// measures against the hour's average. The memory question: a
    /// footprint that went 300 MB → 1.5 GB over the hour has climbed
    /// 1.2 GB, and an hour-average baseline would report half of it.
    ///
    /// Only a climb that is *still there* counts: when the newest
    /// minutes sit more than [`CLIMB_HOLD`] below the hour's high, the
    /// tree grew and freed — a spike that is over, not a leak — and the
    /// verdict is `None`. `None` too until [`CLIMB_MIN_MINUTES`] of the
    /// hour have been reported: five minutes of history is a trend for
    /// CPU, where a rate can turn in a minute, but not for a footprint,
    /// which is supposed to move slowly.
    ///
    /// **A minute this tree is missing from is skipped, not read as
    /// zero** — the one place this ring's two questions part company,
    /// and the difference is entirely about what the collector's table
    /// is sorted by. [`rise`](Self::rise) may count an absent tree as
    /// 0% because the table *is* a CPU ranking: falling out of it is
    /// itself evidence of low CPU. Memory has no such luck — the sort
    /// key is still CPU, so absence says nothing whatever about a
    /// footprint, and zero-filling it turns "this tree left the top
    /// fifty for a while" into a fabricated climb the size of its
    /// whole footprint. Measured: a terminal holding ~1.1 GB, in and
    /// out of the table as commands ran, was announced as "up 1.1 GB
    /// this hour · holding 1.1 GB now" — the two figures identical,
    /// which is the signature of a baseline read as nothing. With 700
    /// trees on the machine and `max-processes` 50, drifting out is
    /// the norm, not an edge case.
    pub fn climb(&self, name: &str) -> Option<f32> {
        let reported = self.reported.as_ref()?;
        let ring = self.apps.get(name)?;
        let now = reported.head;
        let values: Vec<f32> = (now.saturating_sub(SLOTS as u64 - 1)..=now)
            .filter(|m| reported.at(*m).is_some())
            .filter_map(|m| ring.at(m).map(f32::from))
            .collect();
        if values.len() < CLIMB_MIN_MINUTES {
            return None;
        }
        let n = RECENT_MINUTES as usize;
        let mean = |slice: &[f32]| slice.iter().sum::<f32>() / slice.len() as f32;
        let early = mean(&values[..n]);
        let late = mean(&values[values.len() - n..]);
        let high = values.iter().copied().fold(0.0, f32::max);
        if late < high * CLIMB_HOLD {
            return None;
        }
        Some(late - early)
    }
}

/// Reported minutes a climb verdict needs — a third of the hour.
const CLIMB_MIN_MINUTES: usize = 20;

/// The newest minutes must hold at least this share of the hour's high
/// for the climb to still be a climb. Ten percent of slack absorbs
/// jitter in a footprint (page-outs, a GC pass) without letting a tree
/// that halved its memory keep reading as "climbing".
const CLIMB_HOLD: f32 = 0.9;

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

    /// [`proc`] with a command line — argv[0] is what `bundle_of` reads.
    fn proc_at(pid: u32, parent: Option<u32>, name: &str, cpu: f32, cmd: &str) -> ProcessSnapshot {
        let mut p = proc(pid, parent, name, cpu);
        p.cmd = cmd.into();
        p
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
        let mut g = group(100, "Electron", 40.0);
        g.display_name = Some("CodeBuddy CN".into());
        let table = vec![
            proc_at(
                100,
                Some(1),
                "Electron",
                0.0,
                "/Applications/CodeBuddy CN.app/Contents/MacOS/Electron",
            ),
            proc_at(
                2,
                Some(100),
                "CodeBuddy CN Helper (Renderer)",
                40.0,
                "/Applications/CodeBuddy CN.app/Contents/Frameworks/\
                 CodeBuddy CN Helper (Renderer).app/Contents/MacOS/\
                 CodeBuddy CN Helper (Renderer) --type=renderer",
            ),
        ];
        let pg = jobs(&[(100, 100), (2, 2)]);
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

    /// A build typed into an editor's terminal: the tree is Zed's, the
    /// CPU is cargo's. The title stays Zed — it is Zed's tree, and the
    /// app-level bars key on it — and cargo rides along as the tail,
    /// so "800%" is not read as Zed running away.
    #[test]
    fn an_app_whose_tree_a_bare_job_burns_wears_the_job_as_a_tail() {
        let mut g = group(100, "zed", 800.0);
        g.display_name = Some("Zed".into());
        let mut root = proc_at(100, Some(1), "zed", 20.0, ZED);
        root.display_name = Some("Zed".into());
        let table = vec![
            root,
            proc_at(2, Some(100), "login", 0.0, "/usr/bin/login -fp tree"),
            proc_at(3, Some(2), "zsh", 0.0, "-zsh"),
            proc_at(4, Some(3), "cargo", 1.0, "cargo build"),
            proc(5, Some(4), "rustc", 390.0),
            proc(6, Some(4), "rustc", 389.0),
        ];
        let pg = jobs(&[(100, 100), (2, 2), (3, 3), (4, 4), (5, 4), (6, 4)]);
        let face = tree_face(&g, &table, &[], &pg);
        assert_eq!(face, "Zed · cargo");
        assert_eq!(face.title, "Zed");
        assert_eq!(face.job.as_deref(), Some("cargo"));

        // Xcode's make has a bundle of its own — still foreign to Zed's
        // tree, still the tail (and, since zstats 0.5.4, still `make`).
        let table = vec![
            proc_at(100, Some(1), "zed", 20.0, ZED),
            proc_at(2, Some(100), "login", 0.0, "/usr/bin/login -fp tree"),
            proc_at(3, Some(2), "zsh", 0.0, "-zsh"),
            proc_at(
                4,
                Some(3),
                "make",
                1.0,
                "/Applications/Xcode.app/Contents/Developer/usr/bin/make dev",
            ),
            proc(5, Some(4), "cargo", 779.0),
        ];
        let pg = jobs(&[(100, 100), (2, 2), (3, 3), (4, 4), (5, 4)]);
        assert_eq!(tree_face(&g, &table, &[], &pg), "Zed · make");
    }

    const ZED: &str = "/Applications/Zed.app/Contents/MacOS/zed";
    const CHROME: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
    const CHROME_RENDERER: &str = "/Applications/Google Chrome.app/Contents/Frameworks/\
         Google Chrome Framework.framework/Versions/151.0.0.0/Helpers/\
         Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer) \
         --type=renderer";

    /// The app's own helpers never become the tail, even from a process
    /// group of their own: same bundle as the root is the gate. Chrome
    /// is the sharp case — its bundle repeats its name, so both the root
    /// and every helper carry `display_name: None`, exactly like a bare
    /// executable would; only argv[0] tells them apart.
    #[test]
    fn an_apps_own_helpers_are_not_a_tail() {
        let g = group(100, "Google Chrome", 400.0);
        let root = proc_at(100, Some(1), "Google Chrome", 5.0, CHROME);
        let renderer = proc_at(
            2,
            Some(100),
            "Google Chrome Helper (Renderer)",
            395.0,
            CHROME_RENDERER,
        );
        let table = vec![root.clone(), renderer];
        let pg = jobs(&[(100, 100), (2, 2)]);
        assert_eq!(tree_face(&g, &table, &[], &pg), "Google Chrome");

        // The app itself leads the hot group: title only.
        let table = vec![root, proc_at(3, Some(100), "node", 395.0, "node server.js")];
        let pg = jobs(&[(100, 100), (3, 100)]);
        assert_eq!(tree_face(&g, &table, &[], &pg), "Google Chrome");
    }

    /// The memory question, fed in MB: a steady 300 → 1500 climb reads
    /// as the whole climb, not as the distance from the hour's average.
    #[test]
    fn a_steady_climb_reads_as_the_whole_climb() {
        let mut trend = AppTrend::default();
        for m in 0..60u64 {
            trend.sample(m, [("leaky", 300.0 + m as f32 * 20.0)].into_iter());
        }
        let climb = trend.climb("leaky").unwrap();
        // early five ≈ 340, late five ≈ 1440
        assert!(
            (climb - 1100.0).abs() < 1.0,
            "full climb, not half: {climb}"
        );
        // `rise` is the same data against the hour's *average*: on a
        // linear climb the average sits mid-slope, so it understates.
        let rise = trend.rise("leaky").unwrap();
        assert!(
            rise < climb * 0.6,
            "rise measures against the average: {rise}"
        );
    }

    /// Grew and freed is a spike that is over, not a leak.
    #[test]
    fn a_climb_that_came_back_down_is_no_climb() {
        let mut trend = AppTrend::default();
        feed(&mut trend, "burst", 0..30, 300.0);
        feed(&mut trend, "burst", 30..50, 2000.0);
        feed(&mut trend, "burst", 50..60, 400.0);
        assert_eq!(trend.climb("burst"), None);
    }

    /// The bug this rule shipped with, and the reason `climb` skips
    /// where `rise` zero-fills: the collector's table is a CPU ranking
    /// capped at `max-processes`, so a tree drifting out of it says
    /// nothing about its footprint. Zero-filling those minutes made a
    /// terminal that merely went quiet look like it had grown its
    /// entire 1.1 GB inside the hour.
    #[test]
    fn minutes_the_tree_is_missing_from_do_not_read_as_an_empty_footprint() {
        let mut trend = AppTrend::default();
        // Present at ~900 MB, out of the top-50 for a stretch, back at
        // ~1000 MB: a 100 MB climb, not a 1000 MB one.
        feed(&mut trend, "ghostty", 0..15, 900.0);
        idle(&mut trend, 15..40);
        feed(&mut trend, "ghostty", 40..60, 1000.0);
        let climb = trend.climb("ghostty").expect("enough observed minutes");
        assert!(
            (climb - 100.0).abs() < 1.0,
            "the gap must not become a baseline of nothing: {climb}"
        );
        // And the CPU question keeps the opposite reading, deliberately:
        // there, falling out of a CPU-sorted table *is* evidence.
        let mut cpu = AppTrend::default();
        idle(&mut cpu, 0..55);
        feed(&mut cpu, "softwareupdated", 55..60, 180.0);
        assert_eq!(cpu.rise("softwareupdated"), Some(180.0));
    }

    /// A footprint moves slowly; a few minutes are not a verdict.
    #[test]
    fn a_short_history_has_no_climb_verdict() {
        let mut trend = AppTrend::default();
        feed(&mut trend, "new", 0..10, 900.0);
        assert_eq!(trend.climb("new"), None);
        feed(&mut trend, "new", 10..25, 900.0);
        assert!(
            (trend.climb("new").unwrap()).abs() < f32::EPSILON,
            "flat is zero"
        );
    }

    #[test]
    fn a_reading_saturates_instead_of_wrapping() {
        let mut trend = AppTrend::default();
        trend.sample(0, [("mega", 1.0e9)].into_iter());
        assert_eq!(trend.apps.get("mega").unwrap().at(0), Some(MAX_PCT));
    }
}
