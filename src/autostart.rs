//! Launch at login, via `SMAppService.mainApp` (macOS 13+).
//!
//! Both directions are the OS's own mechanism: registering adds a login
//! item the user can see and revoke in System Settings → General →
//! Login Items, unregistering removes it — the same refusable-and-
//! revocable stance as every other action this app takes. No launchd
//! plists of our own, nothing persisted in `app.toml`: the system's
//! record IS the state, so [`is_enabled`] asks it live and the two can
//! never disagree.
//!
//! Only meaningful for the installed bundle. A bare `cargo run` binary
//! has no .app for launchd to relaunch — registration fails, the error
//! lands in the log, and the switch simply stays off.

#[cfg(target_os = "macos")]
pub fn is_enabled() -> bool {
    use objc2_service_management::{SMAppService, SMAppServiceStatus};
    unsafe { SMAppService::mainAppService().status() == SMAppServiceStatus::Enabled }
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
        // chip re-reads status() next frame, so the UI stays truthful.
        eprintln!(
            "launch-at-login {}: {e}",
            if enabled { "on" } else { "off" }
        );
    }
}

#[cfg(not(target_os = "macos"))]
pub fn is_enabled() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn set_enabled(_enabled: bool) {}
