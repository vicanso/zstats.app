//! Asking a process to quit — the panel's one way of acting on a process.
//!
//! Three callers, all behind a confirm sheet and never automatic: the quit
//! button on a memory alert card ([`request_quit`]), the Quit control on
//! a process row ([`request_term`]), and the Apps expansion's Quit
//! ([`can_quit_app`] → [`request_quit`]). An unattended kill can take
//! unsaved work with it, so the app's posture stays "notify and offer",
//! with the user's click as the trigger. *When* something is over the
//! line remains zstats' call (the alert button consumes its
//! `AlertEvent`s); this module only carries out the request.
//!
//! [`request_quit`] has two tiers, both refusable by the target:
//! - pids LaunchServices knows as applications get
//!   [`NSRunningApplication::terminate`] — the same request as ⌘Q, so the
//!   app can still raise its own save dialog and survive the click;
//! - everything else gets `SIGTERM`, the signal a well-behaved daemon traps
//!   to clean up after itself.
//!
//! [`request_term`] is the one-tier version: SIGTERM whatever the target
//! is. The process page offers it as "Quit process", the same thing
//! Activity Monitor's Quit does to a row, and deliberately *not* the
//! ⌘Q-equivalent — a row is a process, not an application, and promoting
//! a bare pid to an app-level quit would act on more than the row names.
//!
//! `SIGKILL` is deliberately absent from both. It cannot be refused, which
//! makes it a data-loss button; a process stuck enough to ignore SIGTERM is
//! Activity Monitor's job, not a metrics panel's.

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

/// Whether the Apps expansion should offer Quit for this tree root.
///
/// Only LaunchServices applications: ⌘Q is what "quit the whole app"
/// means, and a `login` tree heading the list is a session, not an
/// app — SIGTERM on that root would take every shell with it. Same
/// self/init refusal as [`can_term`].
pub fn can_quit_app(pid: u32) -> bool {
    can_term(pid) && can_quit(pid) && matches!(method_for(pid), QuitMethod::App)
}

/// Whether the process page should offer a Quit for `pid` at all.
///
/// Refuses pid 1 (launchd, which cannot usefully be signalled) and our
/// own pid — quitting the panel from its own process list is a footgun,
/// not a feature. Policy, not permission: [`can_quit`] is the one that
/// asks the kernel.
pub fn can_term(pid: u32) -> bool {
    pid > 1 && pid != std::process::id()
}

/// SIGTERM, and nothing above it — the process page's Quit.
///
/// Re-checks [`can_term`] rather than trusting the caller: this is the
/// delivering end, and a control that should never have rendered is not
/// a reason to signal something. `false` means nothing was sent.
pub fn request_term(pid: u32) -> bool {
    if !can_term(pid) {
        eprintln!("refusing to signal pid {pid}");
        return false;
    }
    // SAFETY: SIGTERM to a pid this user may signal; the kernel does the
    // permission check and reports failure through the return value.
    let sent = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0;
    if !sent {
        eprintln!("SIGTERM to pid {pid} was not delivered");
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn will_not_signal_init_or_self() {
        assert!(!can_term(0));
        assert!(!can_term(1));
        assert!(!can_term(std::process::id()));
        assert!(
            !can_quit_app(std::process::id()),
            "the panel must not offer to quit itself from Apps"
        );
        assert!(!can_quit_app(1));
        assert!(can_term(std::process::id().saturating_add(1000).max(2)));
        // The delivering end refuses the same pids, not just the button.
        assert!(!request_term(1));
        assert!(!request_term(std::process::id()));
    }
    use std::process;

    #[test]
    fn own_process_is_signalable_and_pid_zero_is_refused() {
        assert!(can_quit(process::id()));
        // pid 0 addresses the whole process group; request_quit must refuse
        // it outright rather than pass it to kill().
        assert!(!request_quit(0));
    }
}
