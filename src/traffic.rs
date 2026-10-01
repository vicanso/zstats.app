//! Per-process socket traffic, as a rate.
//!
//! `zstats::process_traffic()` (zstats ≥ 0.7.0) returns each process's
//! cumulative received and transmitted bytes and keeps nothing between
//! calls. A rate is the difference of two calls over the wall clock
//! between them — the crate's contract, not a figure the snapshot
//! already carries, so this diff is not a second source of a number
//! zstats computed. The store owns the baseline and a ten-minute curve
//! ([`CurveBook`]); this module only does the arithmetic, which is why
//! it touches no gpui types.
//!
//! Three rules, or the number lies (zstats `docs/listeners.md` §3.6):
//!
//! - the first time a pid appears, keep the total and show no rate —
//!   it is cumulative since the sockets were created;
//! - a pid whose age no longer matches was reused (the new process
//!   starts `run_time_secs` over). Start over. An age the OS would not
//!   give is the same refusal: without it a recycled pid cannot be
//!   told from the same one;
//! - a counter that fell lost a socket, which took its bytes with it.
//!   That direction is absent for the sample, not a negative and not
//!   zero. The other direction still counts.
//!
//! Diff per pid, *then* sum by program name. Summing the cumulatives
//! first would let one helper closing a connection cancel a sibling
//! that is still transferring — a browser does that constantly, and
//! the busy process would vanish from the ranking.
//!
//! The curve is that same diff, kept. One point per program per sample,
//! ↓ and ↑ added. A true zero is a point, so a program that goes idle
//! draws down to the axis. A counter that fell and left nothing
//! measurable is a break, and so is a gap longer than [`CURVE_GAP`]:
//! the line does not invent the rates in between. A program that has
//! only ever been quiet never starts a series — idle socket owners are
//! most of the table.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::mem;
use std::time::{Duration, Instant};

/// Shorter than this, the quotient is a scheduling spike, not a rate.
pub const MIN_WINDOW: Duration = Duration::from_millis(500);

/// How long a curve keeps a point. Ten minutes is the axis the card
/// labels once the book is full; older points fall off, and a series
/// with no positive rate left is dropped.
pub const CURVE_WINDOW: Duration = Duration::from_secs(10 * 60);

/// A gap wider than this is a missed sample, not a line between two
/// rates that were never neighbours. The background read is 10s and the
/// hidden tick is 5s, so a step that landed on the next tick is still
/// under this; one missed 10s sample is 20s and breaks. A sleep is the
/// same shape — `Instant` does not advance while the machine is asleep,
/// and the first reading after wake is this far from the last.
pub const CURVE_GAP: Duration = Duration::from_secs(15);

/// `run_time_secs` is whole seconds. The same process's age should
/// advance by about the wall clock between the two calls; each reading
/// can be a second coarse. A reused pid starts that age over and misses
/// by far more than this.
const AGE_SLACK_SECS: u64 = 2;

/// One call's counters for one process. The fields the diff needs —
/// zstats' [`zstats::ProcessTrafficSnapshot`], minus everything else.
#[derive(Clone, Debug)]
pub struct RowSample {
    pub pid: u32,
    pub name: Option<String>,
    pub run_time_secs: Option<u64>,
    pub received_bytes: u64,
    pub transmitted_bytes: u64,
}

/// Every process one `process_traffic()` call returned, stamped when
/// the call finished. The stamp is the window, not the moment the UI
/// painted.
#[derive(Clone, Debug)]
pub struct Sample {
    pub at: Instant,
    pub rows: Vec<RowSample>,
}

/// One pid's rate over the window between two samples.
///
/// A direction is `None` when that counter fell. Zero is a real zero:
/// the counter was there and did not move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rate {
    pub pid: u32,
    pub name: Option<String>,
    pub received_per_sec: Option<u64>,
    pub transmitted_per_sec: Option<u64>,
}

/// What one pid did between two samples, including the cases [`diff`]
/// leaves out of the ranking.
///
/// The ranking only wants bytes that moved. The curve also wants a true
/// zero (the line goes to the axis) and a break (a closed socket, or a
/// pid that was reused), because connecting across either would draw a
/// rate that did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PidDelta {
    /// At least one direction rose. A direction that fell is `None` on
    /// the rate and is not part of the sum.
    Moved(Rate),
    /// Both counters were there and neither moved.
    Quiet { pid: u32, name: Option<String> },
    /// A counter fell and nothing measurable moved, or the pid no
    /// longer names the same process.
    Break { pid: u32, name: Option<String> },
}

/// One program's rate: every pid that moved bytes under that name,
/// summed. Unnamed pids stay separate — merging them would invent a
/// program the kernel did not name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramRate {
    pub name: Option<String>,
    pub pids: BTreeSet<u32>,
    pub received_per_sec: Option<u64>,
    pub transmitted_per_sec: Option<u64>,
}

/// Rates for pids present in both samples that still name the same
/// process and moved bytes in at least one direction.
///
/// An empty result is not "nothing is talking": it is also the first
/// sample, a reused pid, a window too short to divide, or a process
/// whose counters only fell. Those cases are [`PidDelta::Break`] or
/// [`PidDelta::Quiet`] from [`deltas`]; the ranking does not list them.
pub fn diff(prev: &Sample, next: &Sample) -> Vec<Rate> {
    deltas(prev, next)
        .into_iter()
        .filter_map(|delta| match delta {
            PidDelta::Moved(rate) => Some(rate),
            _ => None,
        })
        .collect()
}

/// Every pid [`diff`] considers, including the quiet ones and the breaks.
///
/// A pid seen for the first time is absent: its cumulative total is not
/// a rate and not a hole in a line it does not have yet. A window under
/// [`MIN_WINDOW`] is empty — the caller keeps the previous baseline and
/// records nothing, so this must not be stored as "everyone vanished".
pub fn deltas(prev: &Sample, next: &Sample) -> Vec<PidDelta> {
    let elapsed = next.at.saturating_duration_since(prev.at);
    if elapsed < MIN_WINDOW {
        return Vec::new();
    }
    let secs = elapsed.as_secs_f64();
    let prev_by_pid: HashMap<u32, &RowSample> =
        prev.rows.iter().map(|row| (row.pid, row)).collect();
    let mut out = Vec::new();
    for row in &next.rows {
        let Some(old) = prev_by_pid.get(&row.pid) else {
            continue;
        };
        if !same_process(old.run_time_secs, row.run_time_secs, elapsed) {
            out.push(PidDelta::Break {
                pid: row.pid,
                name: row.name.clone(),
            });
            continue;
        }
        let received = rising(old.received_bytes, row.received_bytes, secs);
        let transmitted = rising(old.transmitted_bytes, row.transmitted_bytes, secs);
        let sum = received
            .unwrap_or(0)
            .saturating_add(transmitted.unwrap_or(0));
        // A fall is `None`. Summed with a sibling that is still
        // transferring it contributes nothing, which is what keeps the
        // sibling's download in the ranking. Summed with nothing else
        // it is a hole: drawing it as zero would say the program went
        // idle, and the socket simply closed.
        if sum > 0 {
            out.push(PidDelta::Moved(Rate {
                pid: row.pid,
                name: row.name.clone(),
                received_per_sec: received,
                transmitted_per_sec: transmitted,
            }));
        } else if received.is_none() || transmitted.is_none() {
            out.push(PidDelta::Break {
                pid: row.pid,
                name: row.name.clone(),
            });
        } else {
            out.push(PidDelta::Quiet {
                pid: row.pid,
                name: row.name.clone(),
            });
        }
    }
    out
}

/// Group pid rates by program name and rank by ↓+↑. A direction no pid
/// could answer stays `None`, so a closed socket does not become 0 B/s
/// and does not drag the sibling pids' increase back down.
pub fn by_program(rates: &[Rate]) -> Vec<ProgramRate> {
    let mut grouped: HashMap<CurveKey, ProgramRate> = HashMap::new();
    for rate in rates {
        let key = CurveKey::for_name_pid(&rate.name, rate.pid);
        let row = grouped.entry(key).or_insert_with(|| ProgramRate {
            name: rate.name.clone().filter(|name| !name.is_empty()),
            pids: BTreeSet::new(),
            received_per_sec: None,
            transmitted_per_sec: None,
        });
        row.pids.insert(rate.pid);
        add_dir(&mut row.received_per_sec, rate.received_per_sec);
        add_dir(&mut row.transmitted_per_sec, rate.transmitted_per_sec);
    }
    let mut rows: Vec<ProgramRate> = grouped.into_values().collect();
    rows.sort_by(|a, b| {
        total(b)
            .cmp(&total(a))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.pids.iter().next().cmp(&b.pids.iter().next()))
    });
    rows
}

/// The age advanced by about the time between the samples.
///
/// `None` on either side cannot confirm it: macOS withholds the start
/// time of another user's process, and a pid reused in that gap would
/// otherwise inherit the previous counters.
fn same_process(old_run: Option<u64>, new_run: Option<u64>, elapsed: Duration) -> bool {
    let (Some(old_run), Some(new_run)) = (old_run, new_run) else {
        return false;
    };
    let expected = old_run.saturating_add(elapsed.as_secs());
    new_run.abs_diff(expected) <= AGE_SLACK_SECS
}

/// `None` when the counter fell. A fall is a closed socket, not traffic
/// in the other direction and not a rate of zero.
fn rising(old: u64, new: u64, secs: f64) -> Option<u64> {
    let delta = new.checked_sub(old)?;
    Some(per_sec(delta, secs))
}

fn per_sec(delta: u64, secs: f64) -> u64 {
    let rate = delta as f64 / secs;
    if rate <= 0.0 {
        0
    } else if rate >= u64::MAX as f64 {
        u64::MAX
    } else {
        rate.round() as u64
    }
}

fn add_dir(slot: &mut Option<u64>, add: Option<u64>) {
    match (*slot, add) {
        (Some(have), Some(more)) => *slot = Some(have.saturating_add(more)),
        (None, next) => *slot = next,
        (Some(_), None) => {}
    }
}

fn total(row: &ProgramRate) -> u64 {
    row.received_per_sec
        .unwrap_or(0)
        .saturating_add(row.transmitted_per_sec.unwrap_or(0))
}

/// Which curve a program's points belong to. The name when the kernel
/// gave one, otherwise the pid: unnamed processes are not one program.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum CurveKey {
    Name(String),
    Pid(u32),
}

impl CurveKey {
    fn for_name_pid(name: &Option<String>, pid: u32) -> Self {
        match name {
            Some(name) if !name.is_empty() => CurveKey::Name(name.clone()),
            _ => CurveKey::Pid(pid),
        }
    }

    fn for_row(row: &RowSample) -> Self {
        Self::for_name_pid(&row.name, row.pid)
    }

    /// The series a ranking row draws. Unnamed rows are one pid each.
    pub fn for_program(row: &ProgramRate) -> Self {
        match &row.name {
            Some(name) if !name.is_empty() => CurveKey::Name(name.clone()),
            _ => CurveKey::Pid(row.pids.iter().next().copied().unwrap_or(0)),
        }
    }
}

/// One sample on a program's curve. `None` is a break: the line stops
/// and starts again after it, rather than bridging a rate we do not have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurvePoint {
    pub at: Instant,
    pub bytes_per_sec: Option<u64>,
}

/// Ten minutes of ↓+↑ per program.
///
/// Keyed by name, not pid. A pid is reused; splicing the new process's
/// counters onto the old line is the bug [`same_process`] exists to stop,
/// and the curve has to make the same cut.
#[derive(Clone, Debug, Default)]
pub struct CurveBook {
    series: HashMap<CurveKey, Vec<CurvePoint>>,
}

impl CurveBook {
    /// The points for one program, oldest first.
    pub fn series(&self, key: &CurveKey) -> &[CurvePoint] {
        self.series.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    /// How long the book actually covers, measured from its oldest point.
    ///
    /// `None` under a second: the header would otherwise flash `0s` on
    /// the sample that created the first point. Capped at
    /// [`CURVE_WINDOW`], which is what a full book labels.
    pub fn span(&self, now: Instant) -> Option<Duration> {
        let oldest = self.series.values().flatten().map(|point| point.at).min()?;
        let span = now.saturating_duration_since(oldest);
        if span < Duration::from_secs(1) {
            None
        } else {
            Some(span.min(CURVE_WINDOW))
        }
    }

    /// Fold one sample into the book.
    ///
    /// `deltas` is [`deltas`] of the baseline and `next`. Any pid that
    /// moved bytes sets the program's point, even when a sibling broke —
    /// the sum is the ranking's total, and one helper closing a socket
    /// must not punch a hole through a program that is still
    /// transferring. A quiet program extends a series that already
    /// exists and does not start one. A program that left the table gets
    /// a single break, so a later return does not connect across the
    /// absence.
    pub fn record(&mut self, at: Instant, next: &Sample, deltas: &[PidDelta]) {
        let present: HashSet<CurveKey> = next.rows.iter().map(CurveKey::for_row).collect();
        let mut folded: HashMap<CurveKey, Fold> = HashMap::new();
        for delta in deltas {
            let key = delta.key();
            folded.entry(key).or_default().absorb(delta);
        }
        for (key, fold) in &folded {
            if let Some(sum) = fold.moved {
                self.push(key.clone(), at, Some(sum), sum > 0);
            } else if fold.quiet {
                self.push(key.clone(), at, Some(0), false);
            } else {
                self.push(key.clone(), at, None, false);
            }
        }
        let stale: Vec<CurveKey> = self
            .series
            .keys()
            .filter(|key| !folded.contains_key(*key) && !present.contains(*key))
            .cloned()
            .collect();
        for key in stale {
            self.push(key, at, None, false);
        }
        self.prune(at);
    }

    fn push(&mut self, key: CurveKey, at: Instant, bytes: Option<u64>, create: bool) {
        if !self.series.contains_key(&key) {
            // A first point of zero, or a break, would be a series that
            // has never moved bytes. Idle sockets are most of the table.
            if !create || bytes.unwrap_or(0) == 0 {
                return;
            }
        }
        self.series.entry(key).or_default().push(CurvePoint {
            at,
            bytes_per_sec: bytes,
        });
    }

    fn prune(&mut self, now: Instant) {
        self.series.retain(|_, points| {
            points.retain(|point| now.saturating_duration_since(point.at) <= CURVE_WINDOW);
            points
                .iter()
                .any(|point| point.bytes_per_sec.unwrap_or(0) > 0)
        });
    }
}

/// `10m` once the window is full, otherwise the real length: `9m`, `40s`.
/// Under a minute stays in seconds — [`crate::format::uptime_short`]
/// collapses that to `0m`, which would label a fresh curve as empty.
pub fn span_label(span: Duration) -> String {
    let secs = span.as_secs();
    if secs >= CURVE_WINDOW.as_secs() {
        format!("{}m", CURVE_WINDOW.as_secs() / 60)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// Runs the stroke can draw. A break, or a gap past [`CURVE_GAP`], ends
/// a run. A run of one point is not a curve — it would paint a line
/// across the whole slot from a single sample.
pub fn segments(points: &[CurvePoint]) -> Vec<Vec<(Instant, u64)>> {
    let mut out = Vec::new();
    let mut run = Vec::new();
    let mut prev: Option<Instant> = None;
    for point in points {
        let gapped = prev.is_some_and(|prev| point.at.saturating_duration_since(prev) > CURVE_GAP);
        if gapped {
            flush_run(&mut out, &mut run);
        }
        match point.bytes_per_sec {
            Some(rate) => run.push((point.at, rate)),
            None => flush_run(&mut out, &mut run),
        }
        prev = Some(point.at);
    }
    flush_run(&mut out, &mut run);
    out
}

fn flush_run(out: &mut Vec<Vec<(Instant, u64)>>, run: &mut Vec<(Instant, u64)>) {
    if run.len() >= 2 {
        out.push(mem::take(run));
    } else {
        run.clear();
    }
}

#[derive(Default)]
struct Fold {
    /// `Some` once any pid moved bytes. The value is the saturating sum.
    moved: Option<u64>,
    quiet: bool,
}

impl Fold {
    fn absorb(&mut self, delta: &PidDelta) {
        match delta {
            PidDelta::Moved(rate) => {
                let add = rate
                    .received_per_sec
                    .unwrap_or(0)
                    .saturating_add(rate.transmitted_per_sec.unwrap_or(0));
                self.moved = Some(self.moved.unwrap_or(0).saturating_add(add));
            }
            PidDelta::Quiet { .. } => self.quiet = true,
            PidDelta::Break { .. } => {}
        }
    }
}

impl PidDelta {
    fn key(&self) -> CurveKey {
        match self {
            PidDelta::Moved(rate) => CurveKey::for_name_pid(&rate.name, rate.pid),
            PidDelta::Quiet { pid, name } | PidDelta::Break { pid, name } => {
                CurveKey::for_name_pid(name, *pid)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, name: &str, run: u64, rx: u64, tx: u64) -> RowSample {
        RowSample {
            pid,
            name: Some(name.to_string()),
            run_time_secs: Some(run),
            received_bytes: rx,
            transmitted_bytes: tx,
        }
    }

    fn unnamed(pid: u32, run: u64, rx: u64, tx: u64) -> RowSample {
        RowSample {
            pid,
            name: None,
            run_time_secs: Some(run),
            received_bytes: rx,
            transmitted_bytes: tx,
        }
    }

    fn between(elapsed: Duration, old: Vec<RowSample>, new: Vec<RowSample>) -> Vec<Rate> {
        let t0 = Instant::now();
        diff(
            &Sample { at: t0, rows: old },
            &Sample {
                at: t0 + elapsed,
                rows: new,
            },
        )
    }

    #[test]
    fn the_first_sighting_of_a_pid_is_not_a_rate() {
        let rates = between(
            Duration::from_secs(2),
            vec![],
            vec![row(1, "curl", 2, 50_000, 1_000)],
        );
        assert!(rates.is_empty());
    }

    #[test]
    fn a_rising_counter_becomes_bytes_per_second() {
        let rates = between(
            Duration::from_secs(2),
            vec![row(1, "curl", 10, 1_000, 500)],
            vec![row(1, "curl", 12, 3_000, 1_500)],
        );
        assert_eq!(
            rates,
            vec![Rate {
                pid: 1,
                name: Some("curl".into()),
                received_per_sec: Some(1_000),
                transmitted_per_sec: Some(500),
            }]
        );
    }

    #[test]
    fn a_counter_that_fell_is_absent_and_the_other_direction_stands() {
        // 4_000 bytes left with a closed socket; 2_000 went out on one
        // that stayed open. The fall is not −2_000 B/s, and it does not
        // erase the send.
        let rates = between(
            Duration::from_secs(2),
            vec![row(1, "curl", 10, 5_000, 100)],
            vec![row(1, "curl", 12, 1_000, 2_100)],
        );
        assert_eq!(rates.len(), 1);
        assert_eq!(rates[0].received_per_sec, None);
        assert_eq!(rates[0].transmitted_per_sec, Some(1_000));
    }

    #[test]
    fn a_reused_pid_is_not_diffed_against_the_previous_process() {
        let rates = between(
            Duration::from_secs(2),
            vec![row(1, "old", 10_000, 1_000, 1_000)],
            vec![row(1, "new", 1, 50_000, 50_000)],
        );
        assert!(rates.is_empty(), "the age started over");
    }

    #[test]
    fn an_age_the_os_withheld_never_becomes_a_rate() {
        let mut old = row(1, "mDNSResponder", 10, 100, 100);
        let mut new = row(1, "mDNSResponder", 12, 5_000, 100);
        old.run_time_secs = None;
        new.run_time_secs = None;
        assert!(between(Duration::from_secs(2), vec![old], vec![new]).is_empty());
    }

    #[test]
    fn a_window_under_half_a_second_is_not_divided() {
        let rates = between(
            Duration::from_millis(100),
            vec![row(1, "curl", 10, 0, 0)],
            vec![row(1, "curl", 10, 10_000, 0)],
        );
        assert!(rates.is_empty());
    }

    #[test]
    fn age_slack_covers_a_second_of_truncation_and_no_more() {
        // elapsed floors to 2, so the expected age is 102. 100 and 104
        // are the slack; 99 has gone backwards.
        let ok = between(
            Duration::from_secs(2),
            vec![row(1, "curl", 100, 0, 0)],
            vec![row(1, "curl", 100, 2_000, 0)],
        );
        assert_eq!(ok.len(), 1);
        let reused = between(
            Duration::from_secs(2),
            vec![row(1, "curl", 100, 0, 0)],
            vec![row(1, "curl", 99, 2_000, 0)],
        );
        assert!(reused.is_empty());
    }

    #[test]
    fn a_quiet_pid_is_left_out_of_the_ranking() {
        let rates = between(
            Duration::from_secs(2),
            vec![row(1, "idle", 10, 100, 100)],
            vec![row(1, "idle", 12, 100, 100)],
        );
        assert!(rates.is_empty());
    }

    #[test]
    fn one_helper_closing_a_socket_does_not_cancel_the_sibling() {
        // pid 2's receive fell, so it contributes no receive. pid 1's
        // download must survive the sum — diffing the name's combined
        // cumulative would have gone negative and dropped Chrome.
        let rates = between(
            Duration::from_secs(2),
            vec![
                row(1, "Chrome", 100, 1_000, 0),
                row(2, "Chrome", 100, 80_000, 0),
            ],
            vec![
                row(1, "Chrome", 102, 5_000, 0),
                row(2, "Chrome", 102, 10_000, 0),
            ],
        );
        let rows = by_program(&rates);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name.as_deref(), Some("Chrome"));
        assert_eq!(rows[0].pids.iter().copied().collect::<Vec<_>>(), vec![1]);
        assert_eq!(rows[0].received_per_sec, Some(2_000));
    }

    #[test]
    fn programs_sum_their_pids_and_rank_by_both_directions() {
        let rates = between(
            Duration::from_secs(2),
            vec![
                row(1, "Chrome", 50, 0, 0),
                row(2, "Chrome", 50, 0, 0),
                row(3, "ssh", 50, 0, 0),
            ],
            vec![
                row(1, "Chrome", 52, 1_000, 0),
                row(2, "Chrome", 52, 1_000, 500),
                row(3, "ssh", 52, 0, 4_000),
            ],
        );
        let rows = by_program(&rates);
        assert_eq!(rows[0].name.as_deref(), Some("ssh"));
        assert_eq!(rows[0].transmitted_per_sec, Some(2_000));
        assert_eq!(rows[1].name.as_deref(), Some("Chrome"));
        assert_eq!(rows[1].pids.len(), 2);
        assert_eq!(rows[1].received_per_sec, Some(1_000));
        assert_eq!(rows[1].transmitted_per_sec, Some(250));
    }

    #[test]
    fn unnamed_pids_stay_separate_rows() {
        let rates = between(
            Duration::from_secs(2),
            vec![unnamed(4, 10, 0, 0), unnamed(9, 10, 0, 0)],
            vec![unnamed(4, 12, 2_000, 0), unnamed(9, 12, 4_000, 0)],
        );
        let rows = by_program(&rates);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.name.is_none()));
        assert_eq!(rows[0].pids.iter().copied().collect::<Vec<_>>(), vec![9]);
        assert_eq!(rows[1].pids.iter().copied().collect::<Vec<_>>(), vec![4]);
    }

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    fn sample(at: Instant, rows: Vec<RowSample>) -> Sample {
        Sample { at, rows }
    }

    fn key(name: &str) -> CurveKey {
        CurveKey::Name(name.into())
    }

    /// Two samples a step apart, recorded. `run` is the age at `t0`.
    fn record_step(
        book: &mut CurveBook,
        t0: Instant,
        step: u64,
        from: Vec<RowSample>,
        to: Vec<RowSample>,
    ) {
        let prev = sample(t0, from);
        let next = sample(at(t0, step), to);
        book.record(next.at, &next, &deltas(&prev, &next));
    }

    #[test]
    fn a_quiet_sample_draws_down_to_zero_and_stays_connected() {
        let t0 = Instant::now();
        let mut book = CurveBook::default();
        record_step(
            &mut book,
            t0,
            10,
            vec![row(1, "redis", 10, 0, 0)],
            vec![row(1, "redis", 20, 100_000, 0)],
        );
        record_step(
            &mut book,
            at(t0, 10),
            10,
            vec![row(1, "redis", 20, 100_000, 0)],
            vec![row(1, "redis", 30, 100_000, 0)],
        );
        let points = book.series(&key("redis"));
        assert_eq!(points[0].bytes_per_sec, Some(10_000));
        assert_eq!(points[1].bytes_per_sec, Some(0));
        assert_eq!(segments(points).len(), 1, "a true zero connects");
    }

    #[test]
    fn a_closed_socket_breaks_the_line_instead_of_reading_as_zero() {
        let t0 = Instant::now();
        let mut book = CurveBook::default();
        record_step(
            &mut book,
            t0,
            10,
            vec![row(1, "redis", 10, 0, 0)],
            vec![row(1, "redis", 20, 100_000, 0)],
        );
        record_step(
            &mut book,
            at(t0, 10),
            10,
            vec![row(1, "redis", 20, 100_000, 0)],
            vec![row(1, "redis", 30, 1_000, 0)],
        );
        let points = book.series(&key("redis"));
        assert_eq!(points.last().unwrap().bytes_per_sec, None);
        assert!(
            segments(points).is_empty(),
            "one point and a break is not a line"
        );
    }

    #[test]
    fn an_idle_socket_does_not_start_a_series() {
        let t0 = Instant::now();
        let mut book = CurveBook::default();
        record_step(
            &mut book,
            t0,
            10,
            vec![row(1, "idle", 10, 50, 50)],
            vec![row(1, "idle", 20, 50, 50)],
        );
        assert!(book.series(&key("idle")).is_empty());
    }

    #[test]
    fn a_program_that_leaves_and_returns_does_not_connect_across_the_gap() {
        let t0 = Instant::now();
        let mut book = CurveBook::default();
        record_step(
            &mut book,
            t0,
            10,
            vec![row(1, "curl", 10, 0, 0)],
            vec![row(1, "curl", 20, 20_000, 0)],
        );
        record_step(
            &mut book,
            at(t0, 10),
            10,
            vec![row(1, "curl", 20, 20_000, 0)],
            vec![row(1, "curl", 30, 40_000, 0)],
        );
        // Gone. The series takes a break so the next visit cannot bridge it.
        let gone = sample(at(t0, 30), vec![]);
        book.record(gone.at, &gone, &[]);
        assert_eq!(
            book.series(&key("curl")).last().unwrap().bytes_per_sec,
            None
        );
        // Back, under a new pid. Two samples so the new run is long
        // enough to draw, and it must not include the earlier points.
        record_step(
            &mut book,
            at(t0, 30),
            10,
            vec![row(7, "curl", 1, 0, 0)],
            vec![row(7, "curl", 11, 20_000, 0)],
        );
        record_step(
            &mut book,
            at(t0, 40),
            10,
            vec![row(7, "curl", 11, 20_000, 0)],
            vec![row(7, "curl", 21, 40_000, 0)],
        );
        let runs = segments(book.series(&key("curl")));
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|run| run.len() == 2));
    }

    #[test]
    fn one_sibling_closing_a_socket_does_not_hole_the_program() {
        let t0 = Instant::now();
        let mut book = CurveBook::default();
        let prev = sample(
            t0,
            vec![
                row(1, "Chrome", 100, 0, 0),
                row(2, "Chrome", 100, 80_000, 0),
            ],
        );
        let next = sample(
            at(t0, 2),
            vec![
                row(1, "Chrome", 102, 4_000, 0),
                row(2, "Chrome", 102, 10_000, 0),
            ],
        );
        book.record(next.at, &next, &deltas(&prev, &next));
        let points = book.series(&key("Chrome"));
        assert_eq!(points.len(), 1);
        // pid 1 moved 2_000 B/s. pid 2's counter fell, so it adds nothing
        // and does not turn the point into a break.
        assert_eq!(points[0].bytes_per_sec, Some(2_000));
    }

    #[test]
    fn a_series_drops_once_the_window_holds_no_positive_point() {
        let t0 = Instant::now();
        let mut book = CurveBook::default();
        record_step(
            &mut book,
            t0,
            10,
            vec![row(1, "redis", 10, 0, 0)],
            vec![row(1, "redis", 20, 100_000, 0)],
        );
        assert_eq!(book.series(&key("redis")).len(), 1);
        let later = t0 + CURVE_WINDOW + Duration::from_secs(11);
        let prev = sample(later, vec![row(1, "redis", 10_000, 100_000, 0)]);
        let next = sample(
            later + Duration::from_secs(10),
            vec![row(1, "redis", 10_010, 100_000, 0)],
        );
        book.record(next.at, &next, &deltas(&prev, &next));
        assert!(
            book.series(&key("redis")).is_empty(),
            "the old peak aged out and the new sample is a zero"
        );
    }

    #[test]
    fn a_gap_past_fifteen_seconds_splits_and_a_ten_second_step_does_not() {
        let t0 = Instant::now();
        let steady = vec![
            CurvePoint {
                at: t0,
                bytes_per_sec: Some(100),
            },
            CurvePoint {
                at: at(t0, 10),
                bytes_per_sec: Some(80),
            },
            CurvePoint {
                at: at(t0, 20),
                bytes_per_sec: Some(40),
            },
        ];
        assert_eq!(segments(&steady).len(), 1);
        assert_eq!(segments(&steady)[0].len(), 3);

        let missed = vec![
            CurvePoint {
                at: t0,
                bytes_per_sec: Some(100),
            },
            CurvePoint {
                at: at(t0, 10),
                bytes_per_sec: Some(80),
            },
            CurvePoint {
                at: at(t0, 30),
                bytes_per_sec: Some(40),
            },
        ];
        let runs = segments(&missed);
        assert_eq!(runs.len(), 1, "the lone point after the gap is not a line");
        assert_eq!(runs[0].len(), 2);

        let exactly = vec![
            CurvePoint {
                at: t0,
                bytes_per_sec: Some(100),
            },
            CurvePoint {
                at: at(t0, 15),
                bytes_per_sec: Some(80),
            },
        ];
        assert_eq!(segments(&exactly).len(), 1, "15s is still one step");
    }

    #[test]
    fn the_span_label_is_the_real_length_until_the_window_is_full() {
        assert!(CurveBook::default().span(Instant::now()).is_none());
        assert_eq!(span_label(Duration::from_secs(40)), "40s");
        assert_eq!(span_label(Duration::from_secs(59)), "59s");
        assert_eq!(span_label(Duration::from_secs(9 * 60 + 40)), "9m");
        assert_eq!(span_label(CURVE_WINDOW), "10m");

        let t0 = Instant::now();
        let mut book = CurveBook::default();
        record_step(
            &mut book,
            t0,
            10,
            vec![row(1, "redis", 10, 0, 0)],
            vec![row(1, "redis", 20, 100_000, 0)],
        );
        assert!(
            book.span(at(t0, 10)).is_none(),
            "the first point is not a span"
        );
        assert_eq!(
            book.span(at(t0, 50)).unwrap(),
            Duration::from_secs(40),
            "the axis is how long the book has actually been filling"
        );
    }
}
