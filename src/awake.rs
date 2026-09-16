//! Keep the Mac awake while the Interface page's switch is on.
//!
//! One IOKit power assertion, held for as long as the preference says
//! so and released the moment it is turned off. This is the app's third
//! and last way of touching the system, and it is deliberately the
//! weakest one: unlike the move-to-Trash and the quit request it needs
//! no confirm sheet, because it changes nothing on the machine, ends
//! with a second click, and cannot outlive the process — the kernel
//! drops an assertion when the holder exits, crash included.
//!
//! `PreventUserIdleSystemSleep`, the `caffeinate -i` assertion, not
//! `PreventSystemSleep`: the latter is only honoured on AC power, and a
//! switch that silently stops working on battery is worse than no
//! switch. Two things it deliberately does not do, both named on the
//! switch's own tooltip: the display still sleeps (that is a separate
//! assertion and a separate decision), and closing the lid still sleeps
//! the machine, because no assertion outranks the lid.
//!
//! The assertion carries the app's name, so `pmset -g assertions` names
//! us when someone asks the machine why it will not sleep — the same
//! reason both transitions are logged at INFO.

#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU32, Ordering};

/// The live assertion, or [`NONE`] while the switch is off. Only ever
/// touched from the main thread (startup and the switch), but an atomic
/// keeps that from being an assumption a future caller can break.
#[cfg(target_os = "macos")]
static HELD: AtomicU32 = AtomicU32::new(NONE);

/// `kIOPMNullAssertionID`. IOKit's own "no assertion" value, so it can
/// double as the idle state here.
#[cfg(target_os = "macos")]
const NONE: u32 = objc2_io_kit::kIOPMNullAssertionID;

/// Hold or drop the assertion. Idempotent: applying the state that is
/// already in force does nothing, so a repeated `apply(true)` cannot
/// leak a second assertion.
#[cfg(target_os = "macos")]
pub fn apply(on: bool) {
    if on {
        hold();
    } else {
        release();
    }
}

#[cfg(not(target_os = "macos"))]
pub fn apply(_on: bool) {}

#[cfg(target_os = "macos")]
fn hold() {
    use objc2_core_foundation::CFString;
    use objc2_io_kit::{IOPMAssertionCreateWithName, kIOPMAssertionLevelOn};

    if HELD.load(Ordering::Relaxed) != NONE {
        return;
    }
    // The type is a plain string in IOKit's header rather than an
    // exported symbol, so it is spelled out here.
    let kind = CFString::from_static_str("PreventUserIdleSystemSleep");
    let reason = CFString::from_static_str(crate::APP_NAME);
    let mut id = NONE;
    // SAFETY: both strings outlive the call, and `id` is a valid
    // pointer to write the new assertion's handle into.
    let result = unsafe {
        IOPMAssertionCreateWithName(Some(&kind), kIOPMAssertionLevelOn, Some(&reason), &mut id)
    };
    // `kIOReturnSuccess` is 0 and is not exported by the bindings.
    if result != 0 || id == NONE {
        tracing::warn!(result, "could not take the keep-awake assertion");
        return;
    }
    HELD.store(id, Ordering::Relaxed);
    tracing::info!(id, "keep awake on: system idle sleep held");
}

#[cfg(target_os = "macos")]
fn release() {
    use objc2_io_kit::IOPMAssertionRelease;

    let id = HELD.swap(NONE, Ordering::Relaxed);
    if id == NONE {
        return;
    }
    let result = IOPMAssertionRelease(id);
    if result != 0 {
        tracing::warn!(result, id, "could not release the keep-awake assertion");
        return;
    }
    tracing::info!(id, "keep awake off: system idle sleep released");
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// The real IOKit round trip: a held assertion has an id, a released
    /// one is back to `NONE`, and neither call is confused by a repeat.
    /// Cheap enough to run for real — an assertion is a registry entry,
    /// and the process exiting would drop it anyway.
    #[test]
    fn the_assertion_is_taken_once_and_released_once() {
        apply(false);
        assert_eq!(HELD.load(Ordering::Relaxed), NONE, "starts idle");
        apply(true);
        let first = HELD.load(Ordering::Relaxed);
        assert_ne!(first, NONE, "held after the switch goes on");
        apply(true);
        assert_eq!(
            HELD.load(Ordering::Relaxed),
            first,
            "applying the state already in force must not take a second one"
        );
        apply(false);
        assert_eq!(
            HELD.load(Ordering::Relaxed),
            NONE,
            "released after the switch goes off"
        );
        apply(false);
    }
}
