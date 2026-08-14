//! Scanning for processes in a state that should not persist.
//!
//! These never show up in the metrics panel's process table, and cannot: zstats
//! keeps the top N by CPU then memory (`select_top_pids`), while a zombie
//! consumes neither. Measured on a live machine, the abnormal processes ranked
//! 435th, 589th and 591st by CPU — nowhere near the top 50.
//!
//! So this walks the whole table separately, through `sysctl(KERN_PROC_ALL)` —
//! not libproc, for the reasons on [`all_processes`]. It is far cheaper than a
//! full metrics sample: one syscall and a linear scan of the result, with no
//! CPU/memory accounting and nothing to diff against a previous sample.
//!
//! A zombie is a child that exited and whose parent never called `wait()`. It
//! is NOT guaranteed to be reaped — if the parent simply never reaps, the entry
//! survives until the parent itself exits. Two on the development machine had
//! been sitting there for 6 and 15 days under a live parent, which is exactly
//! the kind of leak worth surfacing.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// What kind of abnormal, for wording and colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProcState {
    /// Exited, but the parent has not reaped it.
    Zombie,
    /// Suspended by a signal (SIGSTOP/SIGTSTP).
    Stopped,
}

impl ProcState {
    pub fn label(self) -> &'static str {
        match self {
            ProcState::Zombie => "Zombie",
            ProcState::Stopped => "Stopped",
        }
    }
}

#[derive(Clone, Debug)]
pub struct AbnormalProcess {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
    pub state: ProcState,
    /// Time since the process was created.
    ///
    /// NOT time spent abnormal — the kernel does not record when a process
    /// became a zombie. `p_rtime`, which sounds like it would help, reads 0
    /// for every process on macOS (a BSD field left unmaintained), and
    /// `p_xstat` is the exit code, not a timestamp.
    ///
    /// For a zombie it is still the number that matters: zombies never return
    /// to a normal state, so a long-lived one means nobody has reaped it for
    /// that long — unless the process genuinely ran that whole time first.
    pub age: Duration,
}

/// Every process currently in an abnormal state, oldest first.
///
/// Reports everything it finds — deciding which ones have persisted long
/// enough to be worth showing needs history, which lives in the store.
pub fn scan() -> Vec<AbnormalProcess> {
    let Some(raw) = all_processes() else {
        return Vec::new();
    };
    let mut found: Vec<AbnormalProcess> = raw
        .chunks_exact(KINFO_PROC_SIZE)
        .filter_map(parse_entry)
        .collect();
    // Oldest first: a zombie sitting there for days is the signal; one that
    // appeared this second is probably about to be reaped.
    found.sort_by_key(|p| std::cmp::Reverse(p.age));
    found
}

/// `sysctl(KERN_PROC_ALL)` — the same source `ps` reads.
///
/// Not libproc: `proc_listallpids` only returns what the calling user may see
/// and omits zombies entirely. On the development machine it reported 169 pids
/// against sysctl's 666, and none of the three abnormal processes — they were
/// owned by root.
fn all_processes() -> Option<Vec<u8>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_ALL, 0];
    let mut len = 0usize;

    // Size first, then fetch. The table can grow in between, so the read is
    // allowed to come back shorter than the buffer.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len == 0 {
        return None;
    }

    let mut buf = vec![0u8; len];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(len);

    // A partial trailing record means the layout assumption below is wrong;
    // returning nothing beats reading fields out of misaligned bytes.
    if !buf.len().is_multiple_of(KINFO_PROC_SIZE) {
        return None;
    }
    Some(buf)
}

/// `struct kinfo_proc` is read by offset rather than mirrored as a Rust type:
/// it nests several kernel structs whose padding would be easy to get subtly
/// wrong, and only four fields are needed. Values verified against the SDK
/// headers on macOS 26 (arm64) with `offsetof`.
const KINFO_PROC_SIZE: usize = 648;
/// `kp_proc.p_un.__p_starttime.tv_sec`
const OFF_START_SEC: usize = 0;
/// `kp_proc.p_stat`
const OFF_STAT: usize = 36;
/// `kp_proc.p_pid`
const OFF_PID: usize = 40;
/// `kp_proc.p_comm`, NUL-padded, MAXCOMLEN + 1
const OFF_COMM: usize = 243;
const COMM_LEN: usize = 17;
/// `kp_eproc.e_ppid`
const OFF_PPID: usize = 560;

fn parse_entry(chunk: &[u8]) -> Option<AbnormalProcess> {
    let state = match u32::from(chunk[OFF_STAT]) {
        libc::SZOMB => ProcState::Zombie,
        libc::SSTOP => ProcState::Stopped,
        _ => return None,
    };
    let pid = read_i32(chunk, OFF_PID);
    // Sanity check on the offsets: a real entry always has a positive pid.
    if pid <= 0 {
        return None;
    }

    let started = UNIX_EPOCH + Duration::from_secs(read_i64(chunk, OFF_START_SEC).max(0) as u64);
    let age = SystemTime::now()
        .duration_since(started)
        .unwrap_or(Duration::ZERO);

    Some(AbnormalProcess {
        pid: pid as u32,
        parent_pid: read_i32(chunk, OFF_PPID).max(0) as u32,
        name: read_name(&chunk[OFF_COMM..OFF_COMM + COMM_LEN]),
        state,
        age,
    })
}

fn read_i32(chunk: &[u8], off: usize) -> i32 {
    i32::from_ne_bytes(chunk[off..off + 4].try_into().unwrap_or([0; 4]))
}

fn read_i64(chunk: &[u8], off: usize) -> i64 {
    i64::from_ne_bytes(chunk[off..off + 8].try_into().unwrap_or([0; 8]))
}

/// The kernel truncates this to 16 bytes, so long names arrive clipped.
fn read_name(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanning_the_live_system_is_consistent() {
        let found = scan();
        // Cannot assert on the count — a machine may legitimately have none.
        // What must hold is that every entry is well-formed and sorted.
        for p in &found {
            assert!(p.pid > 0, "pid should be real");
            assert!(
                matches!(p.state, ProcState::Zombie | ProcState::Stopped),
                "only abnormal states belong here"
            );
        }
        assert!(
            found.windows(2).all(|w| w[0].age >= w[1].age),
            "oldest first"
        );
    }
}
