//! Keep the Mac awake — screen on, no idle sleep — for a set time.
//!
//! One IOKit power assertion, taken when a length is picked from the
//! cup in the panel's footer and gone when that time is up. This is the app's third
//! and last way of touching the system, and it is deliberately the
//! weakest one: unlike the move-to-Trash and the quit request it needs
//! no confirm sheet, because it changes nothing on the machine, ends
//! with one click or on its own clock, and cannot outlive the process —
//! the kernel drops an assertion when the holder exits, crash included.
//!
//! `PreventUserIdleDisplaySleep`: the display does not dim or sleep and
//! no screen saver starts, so nothing locks from sitting idle — and, in
//! IOKit's own words, "while the display is prevented from dimming, the
//! system cannot go into idle sleep". One assertion is both halves.
//! They used to be two controls, a switch that held
//! `PreventUserIdleSystemSleep` and left the display alone, and this
//! timed hold beside it; they were asked for as one thing, and a page
//! with both made a reader work out which one a machine that must stay
//! up actually needs. That page is also where the lengths used to be
//! picked — the footer menu replaced it, because a hold is something
//! done now, from the panel, not a setting to go and find.
//!
//! **Always timed.** There is no "until I turn it off": a hold with no
//! end and the screen on is how a laptop is found unlocked the next
//! morning, and the old switch was only safe to leave on because it let
//! the display sleep. The timeout rides on the assertion itself
//! (`IOPMAssertionCreateWithDescription`, `TimeoutActionRelease`), so
//! macOS drops it at the deadline even if this process is stuck — what
//! `caffeinate -d -t` takes. `caffeinate -u`, the user-activity
//! declaration, was the request this came from; IOKit's header says of
//! it that a caller who knows how long it wants the display held should
//! take this assertion instead — the declaration is a five-second pulse
//! meant to be repeated.
//!
//! The deadline here is wall-clock time, checked on every collector
//! tick as well ([`expire`]): a lid closed half-way through must not
//! leave the hold with time still on its clock when the Mac wakes an
//! hour past the moment the menu promised. Not persisted — a hold is an
//! act, not a preference. It cannot stop a lock or a sleep someone asks
//! for, and it does not outrank the lid; both are on the cup's tooltip,
//! because each otherwise reads as a bug.
//!
//! The assertion carries the app's name, so `pmset -g assertions` names
//! us when someone asks the machine why it will not sleep — the same
//! reason both transitions are logged at INFO, and the footer's cup is
//! lit while it runs and says until when.
//!
//! **Linux is one logind idle inhibitor** with the same clock, the one
//! `systemd-inhibit --what=idle` takes. logind hands back a file
//! descriptor and holds the inhibit exactly as long as that descriptor
//! is open, so dropping it is the release and the kernel ends it when
//! the process does. `idle` only — blocking a *requested* suspend (lid,
//! menu) would be the overreach an assertion that outranked the lid
//! would be. The idle daemon is what blanks, locks and suspends there,
//! so one inhibitor covers all three for the daemons that honour it:
//! hypridle does unless told not to (`ignore_systemd_inhibit`), swayidle
//! does, and `systemd-inhibit --list` names zstats as the holder either
//! way. `elogind` speaks the same interface where there is no systemd.

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// The lengths a hold is offered at, in minutes. Half an hour is a talk
/// or a long read with the hands off the keyboard; eight hours is the
/// overnight job the old open-ended switch was for, and the longest the
/// menu will promise without being asked again. Each is a whole number
/// of one unit, which is how the menu words them.
pub const HOLD_MINUTES: [u32; 5] = [30, 60, 120, 240, 480];

/// A running hold, as the views read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hold {
    /// The length that was picked — which menu item wears the check.
    pub minutes: u32,
    /// When it ends, on the wall clock.
    pub until: SystemTime,
}

/// What the platform handed back for a hold: an assertion id on macOS,
/// the inhibitor's descriptor on Linux (dropping it is the release).
#[cfg(target_os = "macos")]
type Token = u32;
#[cfg(target_os = "linux")]
type Token = zbus::zvariant::OwnedFd;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
type Token = ();

/// The hold in force, with what releases it. Only ever touched from the
/// main thread (the footer, its menu and the tick), but a mutex keeps
/// that from being an assumption a future caller can break.
static HELD: Mutex<Option<(Hold, Token)>> = Mutex::new(None);

/// Keep the machine awake for `minutes` from now, replacing a hold
/// already running; `0` ends it. A platform that refuses leaves no
/// hold, and the footer then shows none — a lit cup over a machine that
/// sleeps anyway would be the one outcome worse than no feature.
pub fn hold_for(minutes: u32) {
    let mut held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((hold, token)) = held.take() {
        release(token);
        tracing::info!(
            minutes = hold.minutes,
            "keep awake off: released before its end"
        );
    }
    if minutes == 0 {
        return;
    }
    let span = Duration::from_secs(u64::from(minutes) * 60);
    let Some(token) = take(span) else {
        return;
    };
    let until = SystemTime::now() + span;
    tracing::info!(minutes, "keep awake on: display and idle sleep held");
    *held = Some((Hold { minutes, until }, token));
}

/// The hold in force. One past its deadline reads as none even before
/// [`expire`] has released it.
pub fn held() -> Option<Hold> {
    let held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    held.as_ref()
        .map(|(hold, _)| *hold)
        .filter(|hold| !ended(hold.until, SystemTime::now()))
}

/// Release a hold whose time is up; `true` when one just ended. Called
/// on every collector tick — the platform's own timeout is the backstop,
/// this is what keeps the promise on the wall clock across a sleep.
pub fn expire() -> bool {
    let mut held = HELD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let due = held
        .as_ref()
        .is_some_and(|(hold, _)| ended(hold.until, SystemTime::now()));
    if !due {
        return false;
    }
    if let Some((hold, token)) = held.take() {
        release(token);
        tracing::info!(minutes = hold.minutes, "keep awake off: its time is up");
    }
    true
}

fn ended(until: SystemTime, now: SystemTime) -> bool {
    now >= until
}

#[cfg(target_os = "macos")]
fn take(span: Duration) -> Option<Token> {
    use objc2_core_foundation::CFString;
    use objc2_io_kit::{IOPMAssertionCreateWithDescription, kIOPMNullAssertionID};

    // Plain strings in IOKit's header rather than exported symbols.
    let kind = CFString::from_static_str("PreventUserIdleDisplaySleep");
    let name = CFString::from_static_str(crate::APP_NAME);
    let on_timeout = CFString::from_static_str("TimeoutActionRelease");
    let mut id = kIOPMNullAssertionID;
    // SAFETY: the strings outlive the call, the three absent ones are
    // optional in the API, and `id` is a valid pointer for the handle.
    let result = unsafe {
        IOPMAssertionCreateWithDescription(
            Some(&kind),
            Some(&name),
            None,
            None,
            None,
            span.as_secs_f64(),
            Some(&on_timeout),
            &mut id,
        )
    };
    // `kIOReturnSuccess` is 0 and is not exported by the bindings.
    if result != 0 || id == kIOPMNullAssertionID {
        tracing::warn!(result, "could not take the keep-awake assertion");
        return None;
    }
    Some(id)
}

/// Not checked: at the deadline macOS has usually released the
/// assertion already (`TimeoutActionRelease`), and then this reports an
/// id it no longer knows — the outcome that was wanted.
#[cfg(target_os = "macos")]
fn release(id: Token) {
    let _ = objc2_io_kit::IOPMAssertionRelease(id);
}

/// `Inhibit(what, who, why, mode)` on the system bus. `block` rather
/// than `delay`: a delay inhibitor only postpones sleep by a bounded
/// few seconds, which is not what the menu promises. The connection
/// itself is dropped on return — the inhibit lives in the descriptor,
/// not the connection.
#[cfg(target_os = "linux")]
fn take(_span: Duration) -> Option<Token> {
    use zbus::blocking::{Connection, Proxy};

    let inhibit = || -> zbus::Result<Token> {
        let connection = Connection::system()?;
        let manager = Proxy::new(
            &connection,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )?;
        manager.call(
            "Inhibit",
            &(
                "idle",
                crate::APP_NAME,
                "Keep awake is on in zstats",
                "block",
            ),
        )
    };
    match inhibit() {
        Ok(fd) => Some(fd),
        Err(e) => {
            tracing::warn!("could not take the logind idle inhibitor: {e}");
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn release(fd: Token) {
    drop(fd);
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn take(_span: Duration) -> Option<Token> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn release(_token: Token) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The menu words a length as "30 minutes" or "2 hours", never
    /// both: a length that is neither under an hour nor whole hours
    /// would lose its minutes there.
    #[test]
    fn every_offered_length_is_one_unit() {
        for minutes in HOLD_MINUTES {
            assert!(minutes > 0 && (minutes < 60 || minutes % 60 == 0));
        }
    }

    /// The real IOKit round trip: taken with its length, replaced rather
    /// than doubled, gone on request — and a deadline that has passed
    /// reads as no hold. Cheap enough to run for real — an assertion is
    /// a registry entry, and the process exiting would drop it anyway.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_hold_is_taken_replaced_and_released() {
        hold_for(0);
        assert_eq!(held(), None, "starts with none");

        let before = SystemTime::now();
        hold_for(30);
        let hold = held().expect("held after a length is picked");
        assert_eq!(hold.minutes, 30);
        let left = hold.until.duration_since(before).unwrap();
        assert!(
            left >= Duration::from_secs(30 * 60 - 1) && left <= Duration::from_secs(30 * 60 + 5)
        );
        assert!(!expire(), "nothing to expire half an hour early");

        hold_for(60);
        assert_eq!(held().map(|h| h.minutes), Some(60), "replaced");

        hold_for(0);
        assert_eq!(held(), None, "released on request");
        assert!(!expire());

        let now = SystemTime::now();
        assert!(ended(now, now), "the deadline itself is the end");
        assert!(!ended(now + Duration::from_secs(1), now));
    }
}
