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
//!
//! Off macOS there is no application tier at all: nothing answers "is this
//! pid an application" the way LaunchServices does, so [`method_for`] is
//! always `Term`, [`can_quit_app`] is always false and the Apps expansion
//! grows no Quit button. SIGTERM, the half that is POSIX, is unchanged.

#[cfg(target_os = "macos")]
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
#[cfg(target_os = "macos")]
pub fn method_for(pid: u32) -> QuitMethod {
    if running_application(pid).is_some() {
        QuitMethod::App
    } else {
        QuitMethod::Term
    }
}

/// Always the signal tier: see the module doc.
#[cfg(not(target_os = "macos"))]
pub fn method_for(_pid: u32) -> QuitMethod {
    QuitMethod::Term
}

/// Deliver the quit request. `false` means nothing was delivered (the
/// process is already gone, permissions changed, this is us / init, or
/// `pid` no longer belongs to `expected_name`) — *not* that the target
/// refused, which both tiers are free to do.
///
/// `expected_name` is the matching identity ([`zstats`] `name`, not
/// `display_name`). Re-checked here because a live card's pid can be
/// recycled in-session; [`can_quit`] only answers "may I signal this
/// pid". [`can_term`] is the same policy [`request_term`] re-checks.
pub fn request_quit(pid: u32, expected_name: &str) -> bool {
    if !can_term(pid) {
        tracing::warn!(pid, "refusing to quit");
        return false;
    }
    match crate::procscan::comm(pid) {
        Some(live) if names_match(expected_name, &live) => {}
        Some(live) => {
            tracing::warn!(
                pid,
                expected = expected_name,
                live,
                "pid is no longer that process"
            );
            return false;
        }
        None => {
            tracing::warn!(pid, expected = expected_name, "could not read process name");
            return false;
        }
    }
    // The audit line for the app's rarest act: asking something to die.
    // Logged at the delivery point so every caller (alert card, Apps
    // expansion) is covered once.
    tracing::info!(
        pid,
        name = expected_name,
        "quit requested (app-level, SIGTERM fallback)"
    );
    #[cfg(target_os = "macos")]
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
    // would signal a whole group. `can_term` already refused those.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) == 0 }
}

/// Kernel `p_comm` is 16 bytes. zstats' `name` can be longer
/// (`Google Chrome Helper (Renderer)`), so a live comm that is exactly
/// that width is treated as a prefix of the expected name — and the
/// reverse, if a truncated expected ever arrives.
/// The width the kernel truncates a process name to: 16 bytes of `p_comm`
/// on macOS, 15 characters in `/proc/<pid>/comm` on Linux.
#[cfg(target_os = "macos")]
const COMM_MAX: usize = 16;
#[cfg(not(target_os = "macos"))]
const COMM_MAX: usize = 15;

fn names_match(expected: &str, live: &str) -> bool {
    expected == live
        || (live.len() == COMM_MAX && expected.starts_with(live))
        || (expected.len() == COMM_MAX && live.starts_with(expected))
}

#[cfg(target_os = "macos")]
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
        tracing::warn!(pid, "refusing to signal");
        return false;
    }
    tracing::info!(pid, "SIGTERM requested");
    // SAFETY: SIGTERM to a pid this user may signal; the kernel does the
    // permission check and reports failure through the return value.
    let sent = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0;
    if !sent {
        tracing::warn!(pid, "SIGTERM was not delivered");
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
        assert!(!request_quit(1, "launchd"));
        assert!(!request_quit(std::process::id(), "zstats"));
    }
    use std::process;

    #[test]
    fn own_process_is_signalable_and_pid_zero_is_refused() {
        assert!(can_quit(process::id()));
        // pid 0 addresses the whole process group; request_quit must refuse
        // it outright rather than pass it to kill().
        assert!(!request_quit(0, "anything"));
    }

    #[test]
    fn names_match_accepts_a_truncated_kernel_comm() {
        let full = "Google Chrome Helper (Renderer)";
        // Truncated the way this kernel truncates: 16 bytes on macOS, 15
        // on Linux. Hard-coding either width made the test a claim about
        // the other platform's kernel.
        let capped = &full[..COMM_MAX];

        assert!(names_match("helper", "helper"));
        assert!(names_match(full, capped));
        assert!(names_match(capped, full));
        assert!(!names_match("helper", "bash"));
        assert!(
            !names_match(full, "Google Chrome"),
            "a shorter live name that is not the cap is a different process"
        );
    }
}
