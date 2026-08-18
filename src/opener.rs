//! Hand a path or URL to the desktop's own launcher, without leaving
//! zombies behind.
//!
//! Every caller is fire-and-forget navigation — reveal in Finder, a
//! System Settings deep link, mounting a downloaded DMG. A bare
//! `Command::spawn` leaks a `<defunct>` entry per click: the launcher
//! exits within milliseconds, but its exit status sits in the process
//! table until the *parent* waits for it, and this app never exits. The
//! app's own abnormal-process watcher was the first to report the
//! result. Reaping happens on a throwaway thread — `wait` returns as
//! fast as the launcher does, so the thread lives milliseconds; waiting
//! inline would put a subprocess round-trip on the caller (usually the
//! main thread).
//!
//! The "free" alternative — `SIG_IGN` on `SIGCHLD` for kernel
//! auto-reaping — is off the table: it breaks the wait semantics of
//! every `Command::output()` in the app (mdfind, tmutil, scutil).
//!
//! **The launcher differs per platform, and so do its arguments.** This
//! function only picks the right binary; it does not translate flags.
//! Every current caller passes macOS-shaped arguments (`-R` to reveal,
//! `-a Finder`, an `x-apple.systempreferences:` URL), so a port has to
//! revisit the call sites, not just this file. Getting the binary right
//! anyway means the helper stops being a silent no-op off macOS — it
//! used to run `open` unconditionally, which on Linux is either missing
//! or an unrelated program.

use std::ffi::OsStr;
use std::io;
use std::process::Command;
use std::thread;

/// Spawn the platform's launcher with `args` and reap the child
/// off-thread. The error is the spawn's own (binary missing, fork
/// failure); the launcher's exit status is deliberately not consulted —
/// no caller could act on it.
pub fn open<I, S>(args: I) -> io::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = launcher().args(args).spawn()?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// macOS: LaunchServices' own front end.
#[cfg(target_os = "macos")]
fn launcher() -> Command {
    Command::new("open")
}

/// Linux and the other unixes: the freedesktop.org launcher, which
/// resolves the user's default handler through the desktop's MIME
/// database.
#[cfg(all(unix, not(target_os = "macos")))]
fn launcher() -> Command {
    Command::new("xdg-open")
}

/// Windows: `start` is a `cmd` builtin, not an executable, so it has to
/// be invoked through the shell. The empty string after it is the
/// window *title* argument — omitting it makes `start` swallow a quoted
/// path as the title and open nothing.
#[cfg(windows)]
fn launcher() -> Command {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", "start", ""]);
    cmd
}
