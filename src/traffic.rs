//! Per-process socket traffic, as a rate.
//!
//! `zstats::process_traffic()` (zstats ≥ 0.7.0) returns each process's
//! cumulative received and transmitted bytes and keeps nothing between
//! calls. A rate is the difference of two calls over the wall clock
//! between them — the crate's contract, not a figure the snapshot
//! already carries, so this diff is not a second source of a number
//! zstats computed. The Network tab asks while it is on screen
//! ([`crate::state`] owns the baseline); this module only does the
//! arithmetic, which is why it touches no gpui types.
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

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

/// Shorter than this, the quotient is a scheduling spike, not a rate.
pub const MIN_WINDOW: Duration = Duration::from_millis(500);

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
/// whose counters only fell.
pub fn diff(prev: &Sample, next: &Sample) -> Vec<Rate> {
    let elapsed = next.at.saturating_duration_since(prev.at);
    if elapsed < MIN_WINDOW {
        return Vec::new();
    }
    let secs = elapsed.as_secs_f64();
    let prev_by_pid: HashMap<u32, &RowSample> =
        prev.rows.iter().map(|row| (row.pid, row)).collect();
    let mut rates = Vec::new();
    for row in &next.rows {
        let Some(old) = prev_by_pid.get(&row.pid) else {
            continue;
        };
        if !same_process(old.run_time_secs, row.run_time_secs, elapsed) {
            continue;
        }
        let received = rising(old.received_bytes, row.received_bytes, secs);
        let transmitted = rising(old.transmitted_bytes, row.transmitted_bytes, secs);
        // A pid whose counters only fell, or did not move, is not a row.
        // Zero would read as "this program is idle" in a list that exists
        // to say who is moving bytes; the card's quiet line covers that.
        if received
            .unwrap_or(0)
            .saturating_add(transmitted.unwrap_or(0))
            == 0
        {
            continue;
        }
        rates.push(Rate {
            pid: row.pid,
            name: row.name.clone(),
            received_per_sec: received,
            transmitted_per_sec: transmitted,
        });
    }
    rates
}

/// Group pid rates by program name and rank by ↓+↑. A direction no pid
/// could answer stays `None`, so a closed socket does not become 0 B/s
/// and does not drag the sibling pids' increase back down.
pub fn by_program(rates: &[Rate]) -> Vec<ProgramRate> {
    let mut grouped: HashMap<GroupKey, ProgramRate> = HashMap::new();
    for rate in rates {
        let key = match &rate.name {
            Some(name) if !name.is_empty() => GroupKey::Name(name.clone()),
            _ => GroupKey::Pid(rate.pid),
        };
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

#[derive(Hash, PartialEq, Eq)]
enum GroupKey {
    Name(String),
    Pid(u32),
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
}
