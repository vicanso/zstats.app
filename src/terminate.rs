//! Asking a process to quit — the panel's one way of acting on the system.
//!
//! Runs only from the quit button on a memory alert card, behind a confirm
//! sheet, and never automatically: an unattended kill can take unsaved work
//! with it, so the app's posture stays "notify and offer", with the user's
//! click as the trigger. *When* something is over the line remains zstats'
//! call (the button consumes its `AlertEvent`s); this module only carries
//! out the eviction.
//!
//! Two tiers, both refusable by the target:
//! - pids LaunchServices knows as applications get
//!   [`NSRunningApplication::terminate`] — the same request as ⌘Q, so the
//!   app can still raise its own save dialog and survive the click;
//! - everything else gets `SIGTERM`, the signal a well-behaved daemon traps
//!   to clean up after itself.
//!
//! `SIGKILL` is deliberately absent. It cannot be refused, which makes it a
//! data-loss button; a process stuck enough to ignore SIGTERM is Activity
//! Monitor's job, not a metrics panel's.

use objc2_app_kit::NSRunningApplication;

/// How [`request_quit`] would deliver the request, so the confirm sheet can
/// say which of the two things it is about to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuitMethod {
    /// A LaunchServices application: gets the ⌘Q-equivalent request.
    App,
    /// A bare process: gets SIGTERM.
    Term,
}

/// Whether this user may signal `pid` at all. `kill(pid, 0)` delivers
/// nothing and just runs the kernel's permission check — the button is
/// only rendered when this holds, so a root-owned subject never shows a
/// control that could only fail.
pub fn can_quit(pid: u32) -> bool {
    // SAFETY: signal 0 performs validation only; no signal is sent.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Which tier `pid` falls in right now.
pub fn method_for(pid: u32) -> QuitMethod {
    if running_application(pid).is_some() {
        QuitMethod::App
    } else {
        QuitMethod::Term
    }
}

/// Deliver the quit request. `false` means nothing was delivered (the
/// process is already gone, or permissions changed since [`can_quit`]) —
/// *not* that the target refused, which both tiers are free to do.
pub fn request_quit(pid: u32) -> bool {
    if let Some(app) = running_application(pid) {
        // `terminate` returns false when the request could not even be
        // delivered; a live app that chooses to show a save dialog instead
        // of dying still counts as delivered.
        if app.terminate() {
            return true;
        }
        // Delivery failed (e.g. the app is terminating already, or is in a
        // state AppKit will not talk to) — fall through and try the signal.
    }
    // SAFETY: plain SIGTERM to a specific pid; never pid 0 / -1, which
    // would signal a whole group.
    pid != 0 && unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) == 0 }
}

fn running_application(pid: u32) -> Option<objc2::rc::Retained<NSRunningApplication>> {
    if pid == 0 {
        return None;
    }
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid as libc::pid_t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    #[test]
    fn own_process_is_signalable_and_pid_zero_is_refused() {
        assert!(can_quit(process::id()));
        // pid 0 addresses the whole process group; request_quit must refuse
        // it outright rather than pass it to kill().
        assert!(!request_quit(0));
    }
}
