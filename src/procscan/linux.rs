//! The Linux backend: one pass over `/proc`.
//!
//! `/proc/<pid>/stat` carries everything this module reports — state,
//! parent, process group and start time — as one line per process, so a
//! scan is a directory listing plus one small read each. No accounting and
//! nothing diffed against a previous sample, the same posture the macOS
//! backend takes with its single `sysctl`.

use super::{AbnormalProcess, ProcState};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs;
use std::time::Duration;

/// The fields of `/proc/<pid>/stat` this module reads.
struct Stat {
    pid: u32,
    name: String,
    state: char,
    ppid: u32,
    pgid: u32,
    /// Field 22, the process's start time in clock ticks since boot.
    start_ticks: u64,
}

/// `comm` sits between the first `(` and the **last** `)`, and the kernel
/// does not escape it: a process named `foo (bar) baz` is legal and common
/// enough (thread names, renamed workers) that splitting the line on
/// whitespace before cutting there reads the wrong field for everything
/// after it.
fn parse_stat(pid: u32, raw: &str) -> Option<Stat> {
    let open = raw.find('(')?;
    let close = raw.rfind(')')?;
    let name = raw.get(open + 1..close)?.to_string();
    // Fields after `comm` are numbered from 3 in proc(5), so field N is
    // index N - 3 here.
    let rest: Vec<&str> = raw.get(close + 1..)?.split_whitespace().collect();
    Some(Stat {
        pid,
        name,
        state: rest.first()?.chars().next()?,
        ppid: rest.get(1)?.parse().ok()?,
        pgid: rest.get(2)?.parse().ok()?,
        start_ticks: rest.get(19)?.parse().ok()?,
    })
}

/// Every numeric entry under `/proc`, i.e. every process. `None` when
/// `/proc` itself cannot be read — the caller must not read that as an
/// empty machine.
fn all_stats() -> Option<Vec<Stat>> {
    let mut out = Vec::new();
    for entry in fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        // A process that exits mid-walk is ordinary, not a failed read.
        if let Ok(raw) = fs::read_to_string(entry.path().join("stat"))
            && let Some(stat) = parse_stat(pid, &raw)
        {
            out.push(stat);
        }
    }
    Some(out)
}

/// Seconds since boot. `start_ticks` is measured from the same origin, so
/// this is what turns it into an age.
fn uptime_secs() -> Option<f64> {
    fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// `USER_HZ`, the unit `start_ticks` is in. 100 on every mainstream
/// configuration, but it is a compile-time kernel choice, so it is asked
/// for rather than assumed.
fn ticks_per_sec() -> f64 {
    // SAFETY: `sysconf` reads a static configuration value and cannot fail
    // destructively; a negative return means "unlimited / unknown" and is
    // filtered below.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz > 0 { hz as f64 } else { 100.0 }
}

pub(super) fn scan() -> Option<Vec<AbnormalProcess>> {
    let stats = all_stats()?;
    // Without it every age would read as zero, i.e. "just appeared" —
    // exactly the conclusion `None` exists to prevent.
    let uptime = uptime_secs()?;
    let hz = ticks_per_sec();
    // Resolved from the same pass, like the macOS backend's second walk
    // over the buffer it already holds: a zombie's parent is by definition
    // still alive, so this nearly always names it.
    let names: HashMap<u32, &str> = stats.iter().map(|s| (s.pid, s.name.as_str())).collect();
    let mut found: Vec<AbnormalProcess> = stats
        .iter()
        .filter_map(|stat| {
            let state = match stat.state {
                'Z' => ProcState::Zombie,
                // `T` is a signal stop, the same thing macOS reports as
                // SSTOP. `t` is deliberately left out: a process stopped by
                // a tracer has a debugger attached to it, which is an
                // explanation rather than a leak.
                'T' => ProcState::Stopped,
                _ => return None,
            };
            let started = stat.start_ticks as f64 / hz;
            Some(AbnormalProcess {
                pid: stat.pid,
                parent_pid: stat.ppid,
                parent_name: names
                    .get(&stat.ppid)
                    .filter(|name| !name.is_empty())
                    .map(|name| (*name).to_string()),
                name: stat.name.clone(),
                state,
                age: Duration::from_secs_f64((uptime - started).max(0.0)),
            })
        })
        .collect();
    // Oldest first, same reason: a zombie sitting there for days is the
    // signal; one that appeared this second is probably about to be reaped.
    found.sort_by_key(|p| Reverse(p.age));
    Some(found)
}

pub(super) fn comm(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let name = fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()?
        .trim_end()
        .to_string();
    (!name.is_empty()).then_some(name)
}

pub(super) fn process_groups() -> HashMap<u32, u32> {
    let Some(stats) = all_stats() else {
        return HashMap::new();
    };
    stats
        .iter()
        .filter(|stat| stat.pgid > 0)
        .map(|stat| (stat.pid, stat.pgid))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parenthesised name is why this is not a `split_whitespace`.
    #[test]
    fn a_name_with_spaces_and_parens_does_not_shift_the_fields() {
        let raw =
            "4242 (foo (bar) baz) Z 17 4200 0 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 987654 0 0";
        let stat = parse_stat(4242, raw).expect("parses");
        assert_eq!(stat.name, "foo (bar) baz");
        assert_eq!(stat.state, 'Z');
        assert_eq!(stat.ppid, 17);
        assert_eq!(stat.pgid, 4200);
        assert_eq!(stat.start_ticks, 987654);
    }

    #[test]
    fn a_truncated_line_is_no_process() {
        assert!(parse_stat(1, "1 (init) S").is_none());
        assert!(parse_stat(1, "garbage").is_none());
    }
}
