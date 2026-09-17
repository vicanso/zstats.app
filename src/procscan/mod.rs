//! Scanning for processes in a state that should not persist.
//!
//! These never show up in the metrics panel's process table, and cannot: zstats
//! keeps the top N by CPU then memory (`select_top_pids`), while a zombie
//! consumes neither. Measured on a live machine, the abnormal processes ranked
//! 435th, 589th and 591st by CPU — nowhere near the top 50.
//!
//! So this walks the whole table separately, and it is far cheaper than a full
//! metrics sample: no CPU/memory accounting and nothing to diff against a
//! previous sample. How that walk happens is the platform's business —
//! `sysctl(KERN_PROC_ALL)` on macOS, `/proc` on Linux — and the two backends
//! below are the only places that differ. Everything above this module sees
//! one contract: [`scan`], [`comm`], [`process_groups`].
//!
//! A zombie is a child that exited and whose parent never called `wait()`. It
//! is NOT guaranteed to be reaped — if the parent simply never reaps, the entry
//! survives until the parent itself exits. Two on the development machine had
//! been sitting there for 6 and 15 days under a live parent, which is exactly
//! the kind of leak worth surfacing.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
use linux as sys;
#[cfg(target_os = "macos")]
use macos as sys;

use std::collections::HashMap;
use std::time::Duration;

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
    /// The parent's name, resolved from the same table snapshot the
    /// abnormal entry came out of — no second syscall. `None` when the
    /// parent is no longer in the table (exited between fork and scan)
    /// or the pid row could not be read.
    pub parent_name: Option<String>,
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
///
/// `None` is a failed read, not an empty machine: the caller must not
/// treat it as "no zombies" or the observation clocks reset. `Some(vec![])`
/// is the real empty.
pub fn scan() -> Option<Vec<AbnormalProcess>> {
    sys::scan()
}

/// The kernel's own short name for `pid`, or `None` if the process is
/// gone or the table cannot be read. Truncated by the kernel — 16 bytes
/// of `p_comm` on macOS, 15 characters of `/proc/<pid>/comm` on Linux —
/// and `terminate::names_match` is where that width is accounted for.
///
/// Used at quit delivery to confirm the pid is still the program the
/// card named. Not a metrics source: `ProcessSnapshot::name` stays
/// zstats'. `KERN_PROC_PID` rather than the full table, so a click
/// does not walk every process.
pub fn comm(pid: u32) -> Option<String> {
    sys::comm(pid)
}

/// pid → process group id for every process in the table: the kernel's
/// own job boundaries, read for `trend::tree_face`.
///
/// A job-control shell puts each command it launches in a fresh process
/// group, and everything that command forks inherits it — `cargo` and
/// its ten `rustc`s share one pgid, the idle `zsh` above them has its
/// own. That is the fact the face needs ("which job is this tree's
/// CPU"), and it is process *state*, which is what this module reads;
/// zstats' snapshots do not carry it. Same single syscall as [`scan`],
/// no accounting. Empty when the table cannot be read.
pub fn process_groups() -> HashMap<u32, u32> {
    sys::process_groups()
}
