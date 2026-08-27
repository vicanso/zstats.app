//! Launch at login, via `SMAppService.mainApp` (macOS 13+).
//!
//! Both directions are the OS's own mechanism: registering adds a login
//! item the user can see and revoke in System Settings → General →
//! Login Items, unregistering removes it — the same refusable-and-
//! revocable stance as every other action this app takes. No launchd
//! plists of our own, nothing persisted in `app.toml`: the system's
//! record IS the state.
//!
//! **Asked at moments, not per frame.** The Interface card renders on
//! every tick the settings window is open, and `status()` is an XPC
//! round-trip to the background-task daemon — one every couple of
//! seconds to answer a question that changes a few times a year. So the
//! answer is cached and [`refresh`]ed exactly where it can have moved:
//! at launch, when the settings window opens, and right after this app
//! changes it. What that gives up is a change made in System Settings
//! *while* the window sits open; it lands on the next open.
//!
//! Every refresh logs the raw status when it differs from the last one,
//! and that is the point of caching it in a shape we control. A user
//! reported the switch reading off after a reboot that had plainly
//! launched the app — the system's own record said the login item was
//! enabled — and nothing in the log could say what `status()` had
//! actually returned at the time. The launch-time sample exists so that
//! question is answerable the next time it is asked, since the state
//! only exists on a machine that really did just boot.
//!
//! Only meaningful for the installed bundle. A bare `cargo run` binary
//! has no .app for launchd to relaunch — registration fails, the error
//! lands in the log, and the switch simply stays off.

use std::sync::atomic::{AtomicU8, Ordering};

/// The last status read, as the OS's own raw value. `UNREAD` until the
/// first [`refresh`], which is what makes that first read log.
static STATUS: AtomicU8 = AtomicU8::new(UNREAD);

/// Distinct from every real `SMAppServiceStatus` (0–3).
const UNREAD: u8 = u8::MAX;

/// macOS 13's four states. Only `Enabled` is on; the other three are
/// three different reasons for off, which is why the raw value is what
/// gets logged rather than the boolean the switch renders.
fn status_name(raw: u8) -> &'static str {
    match raw {
        0 => "notRegistered",
        1 => "enabled",
        2 => "requiresApproval",
        3 => "notFound",
        _ => "unknown",
    }
}

/// Ask the OS and remember the answer. Called at launch, on settings
/// window open, and after [`set_enabled`].
#[cfg(target_os = "macos")]
pub fn refresh() {
    use objc2_service_management::SMAppService;
    let raw = u8::try_from(unsafe { SMAppService::mainAppService().status() }.0).unwrap_or(UNREAD);
    let previous = STATUS.swap(raw, Ordering::Relaxed);
    if previous != raw {
        tracing::info!(
            status = status_name(raw),
            raw,
            was = status_name(previous),
            "launch-at-login status"
        );
    }
}

#[cfg(not(target_os = "macos"))]
pub fn refresh() {}

/// What the switch renders: the remembered status, `Enabled` alone.
///
/// The three other states all read as off, which is honest for
/// `notRegistered` and coarse for the other two — `requiresApproval`
/// means the user revoked it in System Settings (registering again
/// will not win), and `notFound` means the question could not be
/// answered at all. Telling those apart on screen is a separate change;
/// the log now carries what would be needed to write it.
pub fn is_enabled() -> bool {
    STATUS.load(Ordering::Relaxed) == 1
}

#[cfg(target_os = "macos")]
pub fn set_enabled(enabled: bool) {
    use objc2_service_management::SMAppService;
    let service = unsafe { SMAppService::mainAppService() };
    let result = if enabled {
        unsafe { service.registerAndReturnError() }
    } else {
        unsafe { service.unregisterAndReturnError() }
    };
    if let Err(e) = result {
        // Not fatal — the common cause is a bundle-less debug run. The
        // refresh below still runs, so the switch shows what the system
        // actually did rather than what was asked for.
        tracing::warn!(
            "launch-at-login {}: {e}",
            if enabled { "on" } else { "off" }
        );
    }
    // The OS is the record: re-read rather than assume the write took.
    refresh();
}

#[cfg(not(target_os = "macos"))]
pub fn set_enabled(_enabled: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only `Enabled` is on. The switch collapses the other three, but
    /// the names exist so the log can tell them apart — that is the
    /// whole reason the raw value is kept.
    #[test]
    fn only_the_enabled_status_reads_as_on() {
        STATUS.store(1, Ordering::Relaxed);
        assert!(is_enabled());
        for off in [0, 2, 3, UNREAD] {
            STATUS.store(off, Ordering::Relaxed);
            assert!(!is_enabled(), "raw {off} must not read as on");
        }
        assert_eq!(status_name(2), "requiresApproval");
        assert_eq!(status_name(3), "notFound");
        assert_eq!(status_name(UNREAD), "unknown");
    }
}
