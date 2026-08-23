//! Three things this app watches that `zstats` alerting cannot see.
//!
//! Each is a small state machine with its own clock and its own bar, and each
//! exists because the library's own rules answer a different question:
//!
//! - **Sustained load** never crosses a threshold — that is the definition of
//!   it — so no alert rule can fire on it.
//! - **Abnormal processes** are ranked out of the process table before the UI
//!   ever sees it: selection is top-N by CPU then memory, and a zombie scores
//!   near zero on both.
//! - **Interface activity** is not in the snapshot at all: the kernel's
//!   counters are cumulative since boot and cannot say *when* bytes moved.
//!
//! They live here rather than in [`crate::state`] because none of them needs
//! gpui — no `Context`, no `notify`, nothing to render. That is what lets them
//! be tested against a hand-built sequence of samples instead of a live app.

use crate::procscan::AbnormalProcess;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::mem;
use std::time::{Duration, Instant};
use zstats::rolling::ProcessStats;
use zstats::snapshot::{NetworkSnapshot, ProcessSnapshot};

// ---- sustained low-grade CPU ------------------------------------------

/// How long a process must stay above the low-grade bar before it is
/// worth pointing at, unless `app.toml` says otherwise
/// (`prefs::sustained_after`).
///
/// Long enough to rule out ordinary work — a build, a backup, an import all
/// finish well inside it — and short enough that the finding still lands in
/// the session that caused it, rather than the next morning. A default,
/// not a law: a machine that compiles all day and one that writes prose
/// do not agree on what "sustained" means, and this watcher is the
/// panel's own, so the knob can live in the panel's own file.
pub const DEFAULT_SUSTAINED_AFTER: Duration = Duration::from_secs(2 * 60 * 60);

/// How long a process may drop below the bar without resetting its clock.
///
/// CPU wobbles: something averaging 11% dips under 10% constantly, and a
/// strict "consecutive" rule would restart the count every few seconds and
/// never reach [`DEFAULT_SUSTAINED_AFTER`]. Only a real stop counts as a stop.
const SUSTAINED_GRACE: Duration = Duration::from_secs(5 * 60);

/// A process that has just crossed into sustained-load territory.
pub struct SustainedNotice {
    pub pid: u32,
    pub name: String,
    pub cpu_avg: f64,
    pub duration: Duration,
}

/// What counts as sustained: the bar (percent of one core the integral
/// must clear) and how long it must be held. Both come from outside —
/// the bar from `alert-cpu` divided down, the duration from `app.toml`
/// — so every question this watcher answers is asked with the same
/// pair, and the badge, the banner and the card can never disagree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SustainedRule {
    pub bar: f64,
    pub after: Duration,
}

/// A process sitting on a low-but-real share of CPU for a long time.
///
/// This never trips zstats' alerting, which asks "is it over the line right
/// now" — the whole point is that it never is. What makes it worth surfacing
/// is the integral: 10% for twelve hours is more CPU burnt than a minute at
/// 100%, and it is the kind of thing that keeps a fan running with nothing
/// obvious to blame.
///
/// Measured by differencing [`ProcessSnapshot::cpu_time_ms`], the kernel's
/// lifetime counter, rather than by averaging the percentages we happen to
/// sample. The counter is the honest answer to "what did this cost": it is
/// immune to our own adaptive cadence (2s open, 5s idle), to system sleep,
/// and to the jitter that makes any single percentage unreliable.
///
/// It also closes a hole the percentage version could not. A process that
/// bursts to 40% for thirty seconds every four minutes is over the bar every
/// time we look at it, so a "was it over recently" rule keeps its clock
/// running forever and eventually reports a steady 10% that never happened.
/// Its integral is 5%, and the integral is what this tests.
#[derive(Clone)]
struct Stretch {
    /// For the Alerts card — notices carry it, and the live table may no
    /// longer hold this pid by the time someone looks.
    name: String,
    /// Start of the current stretch.
    since: Instant,
    /// The process's lifetime CPU counter at [`Self::since`].
    cpu_time_start_ms: u64,
    /// Most recent sample, and the counter then — the far end of the
    /// integral, and how we notice a process that has left the table.
    last_seen: Instant,
    cpu_time_last_ms: u64,
    /// When the *recent* rate first fell below the bar, or `None` while it
    /// is above. Drives the grace window; see [`SUSTAINED_GRACE`].
    quiet_since: Option<Instant>,
}

impl Stretch {
    fn new(now: Instant, cpu_time_ms: u64, name: &str) -> Self {
        Self {
            name: name.to_string(),
            since: now,
            cpu_time_start_ms: cpu_time_ms,
            last_seen: now,
            cpu_time_last_ms: cpu_time_ms,
            quiet_since: None,
        }
    }

    fn duration(&self) -> Duration {
        self.last_seen.duration_since(self.since)
    }

    /// Single-core percent burnt across the whole stretch: counter delta over
    /// wall clock. 0 before there are two samples to difference.
    fn average_percent(&self) -> f64 {
        rate_percent(
            self.cpu_time_last_ms.saturating_sub(self.cpu_time_start_ms),
            self.duration(),
        )
    }
}

/// `cpu_time_ms` burnt over `span`, as single-core percent. 0 for a
/// zero-length span, which is what a first sample has.
fn rate_percent(cpu_time_ms: u64, span: Duration) -> f64 {
    let span_ms = span.as_secs_f64() * 1000.0;
    if span_ms <= 0.0 {
        return 0.0;
    }
    cpu_time_ms as f64 / span_ms * 100.0
}

/// Tracks every process currently holding a low-but-real CPU share.
#[derive(Default)]
pub struct SustainedWatch {
    stretches: HashMap<u32, Stretch>,
    /// Pids already announced. Crossing the bar is the event; staying over it
    /// is not, so this stops a notification per tick for the next two hours.
    notified: HashSet<u32>,
    /// Drained by the collector after each round.
    pending: Vec<SustainedNotice>,
}

impl SustainedWatch {
    /// Fold one collection round in. `bar` is the share above which load
    /// counts as worth noting, passed in so this type owns no settings.
    pub fn record(
        &mut self,
        processes: &[ProcessSnapshot],
        stats: &HashMap<u32, ProcessStats>,
        rule: SustainedRule,
        now: Instant,
    ) {
        let SustainedRule { bar, after } = rule;
        for p in processes {
            let Some(entry) = self.stretches.get_mut(&p.pid) else {
                // Open a stretch only for something already looking busy, so
                // the map stays the size of the interesting set rather than
                // the whole table. The rolling average, not the raw sample:
                // one twitchy reading should not start a two-hour clock.
                let cpu = stats
                    .get(&p.pid)
                    .map(|s| s.cpu_avg)
                    .unwrap_or(f64::from(p.cpu_usage_percent));
                if cpu >= bar {
                    self.stretches
                        .insert(p.pid, Stretch::new(now, p.cpu_time_ms, &p.name));
                }
                continue;
            };

            // A lifetime counter only goes backwards when the pid was reused,
            // and the new tenant's history is not the old one's.
            if p.cpu_time_ms < entry.cpu_time_last_ms {
                *entry = Stretch::new(now, p.cpu_time_ms, &p.name);
                continue;
            }

            // Rate since the previous sample: what "still going" means.
            let recent = rate_percent(
                p.cpu_time_ms - entry.cpu_time_last_ms,
                now.duration_since(entry.last_seen),
            );
            entry.last_seen = now;
            entry.cpu_time_last_ms = p.cpu_time_ms;
            if recent >= bar {
                entry.quiet_since = None;
            } else if entry.quiet_since.is_none() {
                entry.quiet_since = Some(now);
            }

            // Quiet past the grace window: the load stopped, and whatever
            // comes next is a new episode rather than a continuation.
            if entry
                .quiet_since
                .is_some_and(|q| now.duration_since(q) > SUSTAINED_GRACE)
            {
                *entry = Stretch::new(now, p.cpu_time_ms, &p.name);
                continue;
            }

            // Worth saying only if it has run long enough *and* the integral
            // over that whole stretch clears the bar.
            let average = entry.average_percent();
            let duration = entry.duration();
            if duration >= after && average >= bar && self.notified.insert(p.pid) {
                self.pending.push(SustainedNotice {
                    pid: p.pid,
                    name: p.name.clone(),
                    cpu_avg: average,
                    duration,
                });
            }
        }

        self.prune(now);
    }

    /// Drop anything not seen for a while. A process missing from one tick is
    /// not a reset — the table only keeps the top N, so it can fall out for a
    /// round without having gone quiet.
    fn prune(&mut self, now: Instant) {
        self.stretches
            .retain(|_, s| now.duration_since(s.last_seen) <= SUSTAINED_GRACE);
        // Re-arm: a process that stops and later starts again is a new
        // episode and should be announced again.
        self.notified.retain(|pid| self.stretches.contains_key(pid));
    }

    /// Every stretch currently qualifying — long enough *and* integral
    /// over the bar — longest first. The Alerts tab's read-only card:
    /// a view of what the watcher is holding, raising nothing and
    /// feeding nothing into the rule engine.
    pub fn active(&self, rule: SustainedRule) -> Vec<SustainedNotice> {
        let mut out: Vec<SustainedNotice> = self
            .stretches
            .iter()
            .filter_map(|(pid, s)| {
                let duration = s.duration();
                let average = s.average_percent();
                (duration >= rule.after && average >= rule.bar).then(|| SustainedNotice {
                    pid: *pid,
                    name: s.name.clone(),
                    cpu_avg: average,
                    duration,
                })
            })
            .collect();
        out.sort_by_key(|n| Reverse(n.duration));
        out
    }

    /// Notices raised since the last call, taken once.
    pub fn take_notices(&mut self) -> Vec<SustainedNotice> {
        mem::take(&mut self.pending)
    }

    /// How long this process has been holding a low-but-real CPU share, once
    /// that has gone on long enough to be worth saying.
    pub fn duration_for(&self, pid: u32, rule: SustainedRule) -> Option<Duration> {
        let stretch = self.stretches.get(&pid)?;
        let duration = stretch.duration();
        // Same two conditions the notice uses, so the badge and the banner
        // can never disagree about who qualifies.
        (duration >= rule.after && stretch.average_percent() >= rule.bar).then_some(duration)
    }
}

// ---- processes stuck in a state that should not persist ----------------

/// How long a process must stay abnormal before it is worth showing.
///
/// A healthy shutdown briefly produces a zombie between the child exiting and
/// the parent reaping it; only a persistent one means nobody is reaping.
///
/// Timed from when we first observed it, not from the transition itself — the
/// kernel does not record that (see `procscan::AbnormalProcess::age`). This
/// app runs continuously, so the observation window keeps growing; the cost is
/// that nothing shows for the first few minutes after a restart, even for a
/// zombie that has been there for a fortnight.
const MIN_ABNORMAL_DURATION: Duration = Duration::from_secs(5 * 60);

/// The abnormal-process scan result, plus how long each entry has been there.
#[derive(Default)]
pub struct AbnormalWatch {
    found: Vec<AbnormalProcess>,
    /// When each abnormal pid was first seen in that state.
    since: HashMap<u32, Instant>,
}

impl AbnormalWatch {
    /// Replace the scan result, keeping first-seen times for pids still there.
    pub fn replace(&mut self, found: Vec<AbnormalProcess>, now: Instant) {
        self.since
            .retain(|pid, _| found.iter().any(|p| p.pid == *pid));
        for p in &found {
            self.since.entry(p.pid).or_insert(now);
        }
        self.found = found;
    }

    /// Entries that have stayed abnormal long enough to matter. Every scan
    /// result is retained internally so the clocks keep running — only the
    /// reporting is gated.
    pub fn persistent(&self) -> Vec<&AbnormalProcess> {
        self.found
            .iter()
            .filter(|p| {
                self.observed(p.pid)
                    .is_some_and(|d| d >= MIN_ABNORMAL_DURATION)
            })
            .collect()
    }

    /// How long we have observed this pid as abnormal. Always a lower bound:
    /// it may well have been in that state before the app started.
    pub fn observed(&self, pid: u32) -> Option<Duration> {
        self.since.get(&pid).map(|t| t.elapsed())
    }
}

// ---- network interface activity ---------------------------------------

/// Hide a network interface that has carried nothing for this long.
///
/// A machine lists plenty of interfaces that never move a byte — inactive
/// Ethernet, unused tunnels, bridges — and they crowd out the two or three
/// that matter. Long enough that an idle-but-real connection does not vanish
/// mid-session.
const NET_IDLE_AFTER: Duration = Duration::from_secs(30 * 60);

/// Last moment each interface carried any traffic.
///
/// Only covers this session, and cannot do better: the kernel's counters are
/// cumulative since boot and say nothing about *when* the bytes moved.
#[derive(Default)]
pub struct NetActivity {
    last_active: HashMap<String, Instant>,
}

impl NetActivity {
    pub fn record(&mut self, nets: &[NetworkSnapshot], now: Instant) {
        for n in nets {
            if n.received_bytes_per_sec + n.transmitted_bytes_per_sec > 0 {
                self.last_active.insert(n.interface.clone(), now);
            }
        }
        // Forget interfaces that disappeared entirely (VPN down, cable out),
        // or the map would grow for the life of the process.
        self.last_active
            .retain(|name, _| nets.iter().any(|n| &n.interface == name));
    }

    /// Whether an interface has carried traffic recently enough to be worth a
    /// row. Interfaces never seen active are excluded — including right after
    /// startup, when nothing has been observed yet.
    pub fn is_recent(&self, interface: &str) -> bool {
        self.last_active
            .get(interface)
            .is_some_and(|t| t.elapsed() < NET_IDLE_AFTER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procscan::ProcState;

    #[test]
    fn active_lists_only_qualifying_stretches() {
        let mut watch = SustainedWatch::default();
        let long_ago = Instant::now() - DEFAULT_SUSTAINED_AFTER - Duration::from_secs(60);
        // Long enough and ~10% integral → qualifies at a bar of 8.
        watch.stretches.insert(1, burnt(long_ago, 726_000));
        // Long enough but ~0.01% → under the bar.
        watch.stretches.insert(2, burnt(long_ago, 1_000));
        // Hot but only a minute old → not long enough.
        watch
            .stretches
            .insert(3, burnt(Instant::now() - Duration::from_secs(60), 30_000));

        let active = watch.active(rule(8.0));
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].pid, 1);
        assert!(active[0].duration >= DEFAULT_SUSTAINED_AFTER);
    }

    /// A stretch starting at `since` that has consumed `cpu_time_ms` of
    /// single-core time by now.
    fn burnt(since: Instant, cpu_time_ms: u64) -> Stretch {
        Stretch {
            name: "test".into(),
            since,
            cpu_time_start_ms: 0,
            last_seen: Instant::now(),
            cpu_time_last_ms: cpu_time_ms,
            quiet_since: None,
        }
    }

    const BAR: f64 = 10.0;

    /// A rule at `bar` with the default duration.
    fn rule(bar: f64) -> SustainedRule {
        SustainedRule {
            bar,
            after: DEFAULT_SUSTAINED_AFTER,
        }
    }

    #[test]
    fn sustained_load_survives_dips_but_not_a_real_stop() {
        let mut watch = SustainedWatch::default();
        let start = Instant::now() - DEFAULT_SUSTAINED_AFTER - Duration::from_secs(60);

        // Long enough, and the counter says it really did burn ~11% the whole
        // way: the case worth reporting — never trips the alert, grinds away
        // all day.
        watch.stretches.insert(42, burnt(start, 800_000));
        assert!(watch.duration_for(42, rule(BAR)).is_some());

        // Same stretch, but only 300 core-seconds in it — about 4%. This is
        // the intermittent burst the percentage version used to report as a
        // steady 10%: over the bar every time we looked, under it on average.
        watch.stretches.insert(45, burnt(start, 300_000));
        assert!(
            watch.duration_for(45, rule(BAR)).is_none(),
            "a 4% integral must not qualify however often it peaked"
        );

        // Long enough, but not seen for a while: it left the table and the
        // clock should not survive a prune.
        let mut gone = burnt(start, 800_000);
        gone.last_seen = Instant::now() - SUSTAINED_GRACE - Duration::from_secs(1);
        watch.stretches.insert(43, gone);
        watch.prune(Instant::now());
        assert!(!watch.stretches.contains_key(&43));

        // Busy right now, but only just started — not yet a story, and with a
        // single sample there is nothing to difference anyway.
        watch
            .stretches
            .insert(44, Stretch::new(Instant::now(), 0, "test"));
        assert!(watch.duration_for(44, rule(BAR)).is_none());
    }

    #[test]
    fn crossing_the_line_notifies_once_then_re_arms() {
        let mut watch = SustainedWatch::default();
        watch.stretches.insert(
            7,
            burnt(
                Instant::now() - DEFAULT_SUSTAINED_AFTER - Duration::from_secs(1),
                800_000,
            ),
        );

        assert!(watch.notified.insert(7));
        watch.pending.push(SustainedNotice {
            pid: 7,
            name: "helper".into(),
            cpu_avg: 12.0,
            duration: DEFAULT_SUSTAINED_AFTER,
        });
        assert_eq!(watch.take_notices().len(), 1);
        // Draining is idempotent — staying over the line is not a new event,
        // or this would fire on every tick for the next twelve hours.
        assert!(watch.take_notices().is_empty());
        assert!(!watch.notified.insert(7));

        // Gone quiet long enough to be pruned: the next stretch is a new
        // episode and gets announced again.
        watch.stretches.clear();
        watch.prune(Instant::now());
        assert!(watch.notified.is_empty(), "must re-arm after a real stop");
    }

    fn zombie(pid: u32) -> AbnormalProcess {
        AbnormalProcess {
            parent_name: None,
            pid,
            parent_pid: 1,
            name: "login".into(),
            state: ProcState::Zombie,
            age: Duration::from_secs(60 * 60 * 24),
        }
    }

    #[test]
    fn abnormal_is_withheld_until_it_has_persisted() {
        let mut watch = AbnormalWatch::default();
        // Just seen: a zombie between exit and reap is normal, so nothing yet.
        watch.replace(vec![zombie(9)], Instant::now());
        assert!(watch.persistent().is_empty());
        assert!(watch.observed(9).is_some(), "the clock still runs");

        // Same pid, first seen long enough ago.
        watch.since.insert(
            9,
            Instant::now() - MIN_ABNORMAL_DURATION - Duration::from_secs(1),
        );
        assert_eq!(watch.persistent().len(), 1);
    }

    #[test]
    fn clocks_reset_only_when_a_pid_disappears() {
        let mut watch = AbnormalWatch::default();
        let long_ago = Instant::now() - MIN_ABNORMAL_DURATION - Duration::from_secs(1);
        watch.replace(vec![zombie(9)], Instant::now());
        watch.since.insert(9, long_ago);

        // Still there on the next scan: the clock must not restart, or nothing
        // would ever cross the threshold.
        watch.replace(vec![zombie(9)], Instant::now());
        assert_eq!(watch.persistent().len(), 1);

        // Gone, then back: a new process, a new clock.
        watch.replace(vec![], Instant::now());
        watch.replace(vec![zombie(9)], Instant::now());
        assert!(watch.persistent().is_empty());
    }

    fn iface(name: &str, rx: u64) -> NetworkSnapshot {
        NetworkSnapshot {
            interface: name.into(),
            received_bytes_per_sec: rx,
            transmitted_bytes_per_sec: 0,
            received_packets_per_sec: None,
            transmitted_packets_per_sec: None,
            received_errors_per_sec: None,
            transmitted_errors_per_sec: None,
        }
    }

    #[test]
    fn interfaces_are_kept_only_while_recently_active() {
        let mut net = NetActivity::default();
        let now = Instant::now();
        net.record(&[iface("en0", 1024), iface("utun3", 0)], now);

        assert!(net.is_recent("en0"));
        // Never seen carrying anything — including a machine that just booted.
        assert!(!net.is_recent("utun3"));
        assert!(!net.is_recent("nonexistent"));

        // Silent past the window.
        net.last_active
            .insert("en0".into(), now - NET_IDLE_AFTER - Duration::from_secs(1));
        assert!(!net.is_recent("en0"));

        // An interface that vanishes entirely is forgotten, or the map would
        // grow for the life of the process.
        net.record(&[iface("utun3", 0)], now);
        assert!(!net.last_active.contains_key("en0"));
    }
}
