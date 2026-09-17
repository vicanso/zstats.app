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
//! every tick the settings window is open, and the OS `status()` is an
//! XPC round-trip to the background-task daemon — one every couple of
//! seconds to answer a question that changes a few times a year. So the
//! answer is cached and [`refresh`]ed exactly where it can have moved:
//! at launch, when the settings window opens, when it becomes active
//! again (coming back from Login Items), and right after this app
//! changes it. A revoke made in System Settings *while* our window
//! stays key still waits for the next activation.
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
//! macOS's four states are not a boolean. [`Status::Enabled`] and
//! [`Status::NotRegistered`] are a switch; [`Status::RequiresApproval`]
//! (revoked in Login Items — `register` will not win) and
//! [`Status::NotFound`] (no .app for launchd to relaunch, the usual
//! `cargo run` case) are sentences, because painting a toggle there is
//! a lie. Only the installed bundle can register.

use std::sync::atomic::{AtomicU8, Ordering};

/// The last status read, as the OS's own raw value. `UNREAD` until the
/// first [`refresh`], which is what makes that first read log.
static STATUS: AtomicU8 = AtomicU8::new(UNREAD);

/// Distinct from every real `SMAppServiceStatus` (0–3).
const UNREAD: u8 = u8::MAX;

/// macOS 13's four states. The log keeps the raw name; the Interface
/// row branches on [`status`] so `requiresApproval` and `notFound` are
/// not painted as an off switch.
fn status_name(raw: u8) -> &'static str {
    match raw {
        0 => "notRegistered",
        1 => "enabled",
        2 => "requiresApproval",
        3 => "notFound",
        _ => "unknown",
    }
}

/// What the Interface row can actually do. Only [`Enabled`] and
/// [`NotRegistered`] are a switch; the other two are reasons the OS
/// will not honour `register`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    NotRegistered,
    Enabled,
    RequiresApproval,
    NotFound,
}

/// Ask the OS and remember the answer. Called at launch, on settings
/// window open, when that window becomes active again, and after
/// [`set_enabled`].
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

/// The remembered status, collapsed to what the Interface row can do.
///
/// `UNREAD` and any future raw value fall through to [`Status::NotRegistered`]:
/// we have not been told that `register` would fail, so the switch is
/// still the right control. [`is_enabled`] is `Enabled` alone.
pub fn status() -> Status {
    match STATUS.load(Ordering::Relaxed) {
        1 => Status::Enabled,
        2 => Status::RequiresApproval,
        3 => Status::NotFound,
        _ => Status::NotRegistered,
    }
}

/// What a boolean switch would render: [`Status::Enabled`] alone.
pub fn is_enabled() -> bool {
    status() == Status::Enabled
}

/// Apple's own jump into System Settings → Login Items. The prompt
/// `requiresApproval` is exactly the case this API is documented for;
/// a guessed `x-apple.systempreferences:` URL would be a second map.
#[cfg(target_os = "macos")]
pub fn open_login_items() {
    use objc2_service_management::SMAppService;
    unsafe { SMAppService::openSystemSettingsLoginItems() };
}

#[cfg(not(target_os = "macos"))]
pub fn open_login_items() {}

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

    /// Only `Enabled` is on. `requiresApproval` and `notFound` are not a
    /// switch that happens to be off — they are distinct, because
    /// `register` cannot win them.
    #[test]
    fn only_the_enabled_status_reads_as_on() {
        STATUS.store(1, Ordering::Relaxed);
        assert!(is_enabled());
        assert_eq!(status(), Status::Enabled);
        for off in [0, 2, 3, UNREAD] {
            STATUS.store(off, Ordering::Relaxed);
            assert!(!is_enabled(), "raw {off} must not read as on");
        }
        STATUS.store(0, Ordering::Relaxed);
        assert_eq!(status(), Status::NotRegistered);
        STATUS.store(2, Ordering::Relaxed);
        assert_eq!(status(), Status::RequiresApproval);
        STATUS.store(3, Ordering::Relaxed);
        assert_eq!(status(), Status::NotFound);
        STATUS.store(UNREAD, Ordering::Relaxed);
        assert_eq!(status(), Status::NotRegistered);
        assert_eq!(status_name(2), "requiresApproval");
        assert_eq!(status_name(3), "notFound");
        assert_eq!(status_name(UNREAD), "unknown");
    }
}
