//! Launch macOS's `open` without leaving zombies.
//!
//! Every caller is fire-and-forget navigation — reveal in Finder, a
//! System Settings deep link, mounting a downloaded DMG. A bare
//! `Command::spawn` leaks a `<defunct>` entry per click: `open` exits
//! within milliseconds, but its exit status sits in the process table
//! until the *parent* waits for it, and this app never exits. The
//! app's own abnormal-process watcher was the first to report the
//! result. Reaping happens on a throwaway thread — `wait` returns as
//! fast as `open` does, so the thread lives milliseconds; waiting
//! inline would put a subprocess round-trip on the caller (usually the
//! main thread).
//!
//! The "free" alternative — `SIG_IGN` on `SIGCHLD` for kernel
//! auto-reaping — is off the table: it breaks the wait semantics of
//! every `Command::output()` in the app (mdfind, tmutil, scutil,
//! shasum).

use std::ffi::OsStr;
use std::io;
use std::process::Command;
use std::thread;

/// Spawn `open` with `args` and reap the child off-thread. The error is
/// the spawn's own (binary missing, fork failure); `open`'s exit status
/// is deliberately not consulted — no caller could act on it.
pub fn open<I, S>(args: I) -> io::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = Command::new("open").args(args).spawn()?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
