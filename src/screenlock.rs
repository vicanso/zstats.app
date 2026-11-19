//! In-memory desktop lock state machine, display-awake guard, and macOS
//! Accessibility permission checks.
//!
//! Nothing in this module is ever written to disk (`prefs` or files): if the
//! process is killed or the machine is force-restarted, the lock does not
//! persist across relaunch.

use std::ptr;
use std::sync::Mutex;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};

/// Maximum characters accepted in one unlock attempt buffer, bounding memory
/// growth when a key is held down or pasted.
const MAX_ATTEMPT_LEN: usize = 256;

/// Validation failure while setting the temporary unlock password.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupError {
    Empty,
    Mismatch,
}

/// Validates a temporary password and its confirmation before arming the lock.
/// Passwords are never trimmed so intentional leading/trailing spaces work.
pub fn validate_setup(password: &str, confirm: &str) -> Result<(), SetupError> {
    if password.is_empty() {
        return Err(SetupError::Empty);
    }
    if password != confirm {
        return Err(SetupError::Mismatch);
    }
    Ok(())
}

/// Keyboard input routed from the active platform lock engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyInput {
    Char(char),
    Backspace,
    Clear,
    Submit,
}

/// Outcome of processing one keyboard event while locked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    Updated,
    WrongPassword,
    Unlocked,
    Ignored,
}

/// Read-only view rendered by the overlay card (`entered_len` only; never the
/// characters themselves).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub active: bool,
    pub entered_len: usize,
    pub error: bool,
    pub wrong_attempts: u32,
}

/// Overwrites every byte in `buf` with volatile writes before clearing so
/// password material is scrubbed from heap memory immediately.
fn zero_bytes(buf: &mut Vec<u8>) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a valid, aligned, uniquely borrowed `u8`.
        unsafe { ptr::write_volatile(b, 0) };
    }
    buf.clear();
}

fn zero_string(s: &mut String) {
    // SAFETY: zeroing all bytes leaves an empty UTF-8 buffer once cleared.
    let bytes = unsafe { s.as_mut_vec() };
    zero_bytes(bytes);
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= std::hint::black_box(x ^ y);
    }
    std::hint::black_box(diff) == 0
}

#[derive(Default)]
struct LockState {
    active: bool,
    password: Vec<u8>,
    attempt: String,
    error: bool,
    wrong_attempts: u32,
    last_start_error: bool,
    display_guard: Option<DisplayAwakeGuard>,
}

impl LockState {
    fn arm(
        &mut self,
        password: &str,
        acquire_guard: impl FnOnce() -> Option<DisplayAwakeGuard>,
    ) -> bool {
        if password.is_empty() || self.active {
            return false;
        }
        zero_bytes(&mut self.password);
        zero_string(&mut self.attempt);
        self.password.extend_from_slice(password.as_bytes());
        self.error = false;
        self.wrong_attempts = 0;
        self.last_start_error = false;
        self.display_guard = acquire_guard();
        self.active = true;
        true
    }

    fn abort_arm(&mut self) {
        zero_bytes(&mut self.password);
        zero_string(&mut self.attempt);
        self.active = false;
        self.error = false;
        self.wrong_attempts = 0;
        self.last_start_error = true;
        self.display_guard = None;
    }

    fn force_disarm(&mut self) {
        zero_bytes(&mut self.password);
        zero_string(&mut self.attempt);
        self.active = false;
        self.error = false;
        self.wrong_attempts = 0;
        self.display_guard = None;
    }

    fn handle_key(&mut self, input: KeyInput) -> KeyOutcome {
        if !self.active {
            return KeyOutcome::Ignored;
        }
        match input {
            KeyInput::Char(c) => {
                if c.is_control() {
                    return KeyOutcome::Ignored;
                }
                self.error = false;
                if self.attempt.chars().count() < MAX_ATTEMPT_LEN {
                    self.attempt.push(c);
                    KeyOutcome::Updated
                } else {
                    KeyOutcome::Ignored
                }
            }
            KeyInput::Backspace => {
                self.error = false;
                if self.attempt.pop().is_some() {
                    KeyOutcome::Updated
                } else {
                    KeyOutcome::Ignored
                }
            }
            KeyInput::Clear => {
                let had = !self.attempt.is_empty() || self.error;
                zero_string(&mut self.attempt);
                self.error = false;
                if had {
                    KeyOutcome::Updated
                } else {
                    KeyOutcome::Ignored
                }
            }
            KeyInput::Submit => {
                if self.attempt.is_empty() {
                    return KeyOutcome::Ignored;
                }
                if constant_time_eq(self.attempt.as_bytes(), &self.password) {
                    self.force_disarm();
                    KeyOutcome::Unlocked
                } else {
                    zero_string(&mut self.attempt);
                    self.error = true;
                    self.wrong_attempts = self.wrong_attempts.saturating_add(1);
                    KeyOutcome::WrongPassword
                }
            }
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            active: self.active,
            entered_len: self.attempt.chars().count(),
            error: self.error,
            wrong_attempts: self.wrong_attempts,
        }
    }
}

static STATE: Mutex<LockState> = Mutex::new(LockState {
    active: false,
    password: Vec::new(),
    attempt: String::new(),
    error: false,
    wrong_attempts: 0,
    last_start_error: false,
    display_guard: None,
});

/// Arms the in-memory lock with `password` and takes an independent
/// display-awake assertion/inhibitor. Returns `false` if `password` is empty
/// or the lock is already active.
pub fn arm(password: &str) -> bool {
    let ok = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.arm(password, DisplayAwakeGuard::acquire)
    };
    if ok {
        tracing::info!("screen lock engaged");
    }
    ok
}

/// Rolls back an armed lock when the platform input grab cannot be installed,
/// ensuring the user is never left behind an un-unlockable overlay.
pub fn abort_arm() {
    {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.abort_arm();
    }
    tracing::warn!("screen lock aborted: input grab failed");
}

/// Disarms the lock unconditionally and scrubs all in-memory password buffers.
pub fn force_disarm() {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state.force_disarm();
}

/// Clears any stale setup-start error when reopening the setup window.
pub fn clear_start_error() {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state.last_start_error = false;
}

/// Whether the last attempt to engage the lock aborted because input capture
/// failed.
pub fn start_error() -> bool {
    STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .last_start_error
}

/// Processes one keyboard input while locked.
pub fn handle_key(input: KeyInput) -> KeyOutcome {
    let outcome = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.handle_key(input)
    };
    match outcome {
        KeyOutcome::Unlocked => tracing::info!("screen lock unlocked"),
        KeyOutcome::WrongPassword => tracing::info!("screen lock: wrong password attempt"),
        KeyOutcome::Updated | KeyOutcome::Ignored => {}
    }
    outcome
}

/// Snapshot of the current lock state for UI rendering.
pub fn snapshot() -> Snapshot {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).snapshot()
}

/// Whether the desktop lock is currently engaged.
pub fn is_active() -> bool {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).active
}

// ---- independent display-awake guard ---------------------------------------

/// Holds the display awake while the desktop lock is active without touching
/// the user's timed `awake` menu state.
struct DisplayAwakeGuard {
    #[cfg(target_os = "macos")]
    assertion_id: u32,
    #[cfg(target_os = "linux")]
    _fd: zbus::zvariant::OwnedFd,
}

impl DisplayAwakeGuard {
    fn acquire() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            use objc2_core_foundation::CFString;
            use objc2_io_kit::{
                IOPMAssertionCreateWithName, kIOPMAssertionLevelOn, kIOPMNullAssertionID,
            };

            let kind = CFString::from_static_str("PreventUserIdleDisplaySleep");
            let name = CFString::from_static_str("zstats desktop lock");
            let mut id = kIOPMNullAssertionID;
            // SAFETY: static CFStrings outlive the call and `id` is a valid out-pointer.
            let result = unsafe {
                IOPMAssertionCreateWithName(
                    Some(&kind),
                    kIOPMAssertionLevelOn,
                    Some(&name),
                    &mut id,
                )
            };
            if result != 0 || id == kIOPMNullAssertionID {
                tracing::warn!(result, "could not acquire display-awake assertion for lock");
                return None;
            }
            Some(Self { assertion_id: id })
        }
        #[cfg(target_os = "linux")]
        {
            use zbus::blocking::{Connection, Proxy};

            let inhibit = || -> zbus::Result<zbus::zvariant::OwnedFd> {
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
                        "Desktop lock is active in zstats",
                        "block",
                    ),
                )
            };
            match inhibit() {
                Ok(fd) => Some(Self { _fd: fd }),
                Err(e) => {
                    tracing::warn!("could not acquire logind inhibitor for lock: {e}");
                    None
                }
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            None
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for DisplayAwakeGuard {
    fn drop(&mut self) {
        let _ = objc2_io_kit::IOPMAssertionRelease(self.assertion_id);
    }
}

// ---- Accessibility permission (macOS) --------------------------------------

#[cfg(target_os = "macos")]
static ACCESSIBILITY_GRANTED: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static ACCESSIBILITY_LOGGED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(
        options: Option<
            &objc2_core_foundation::CFDictionary<
                objc2_core_foundation::CFString,
                objc2_core_foundation::CFBoolean,
            >,
        >,
    ) -> bool;
}

/// Last sampled Accessibility permission status.
pub fn accessibility_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        ACCESSIBILITY_GRANTED.load(Ordering::Relaxed)
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Samples Accessibility permission from the OS, logging only when it changes.
pub fn refresh_accessibility() -> bool {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: zero-arg ApplicationServices status check.
        let now = unsafe { AXIsProcessTrusted() };
        let prev = ACCESSIBILITY_GRANTED.swap(now, Ordering::Relaxed);
        let first = !ACCESSIBILITY_LOGGED.swap(true, Ordering::Relaxed);
        if (first && now) || (!first && prev != now) {
            tracing::info!(granted = now, "accessibility permission refreshed");
        }
        now
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Registers zstats in Accessibility settings via `AXIsProcessTrustedWithOptions`
/// and opens System Settings → Privacy & Security → Accessibility.
pub fn request_accessibility() {
    #[cfg(target_os = "macos")]
    {
        use objc2_core_foundation::{CFDictionary, CFString, kCFBooleanTrue};

        let key = CFString::from_static_str("AXTrustedCheckOptionPrompt");
        // SAFETY: `kCFBooleanTrue` is a static CoreFoundation constant.
        if let Some(val) = unsafe { kCFBooleanTrue } {
            let opts = CFDictionary::from_slices(&[&*key], &[val]);
            // SAFETY: valid CFDictionary of static key/value.
            let granted = unsafe { AXIsProcessTrustedWithOptions(Some(&opts)) };
            ACCESSIBILITY_GRANTED.store(granted, Ordering::Relaxed);
        }
        crate::opener::accessibility_settings();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_setup_rejects_empty_and_mismatch_and_preserves_spaces() {
        assert_eq!(validate_setup("", ""), Err(SetupError::Empty));
        assert_eq!(validate_setup("", "a"), Err(SetupError::Empty));
        assert_eq!(validate_setup("abc", "abd"), Err(SetupError::Mismatch));
        assert_eq!(validate_setup(" abc ", "abc"), Err(SetupError::Mismatch));
        assert_eq!(validate_setup(" abc ", " abc "), Ok(()));
        assert_eq!(validate_setup("s3cr3t", "s3cr3t"), Ok(()));
    }

    #[test]
    fn state_machine_full_cycle_and_memory_zeroing() {
        let mut s = LockState::default();
        assert!(!s.arm("", || None));
        assert!(s.arm("pass 12", || None));
        assert!(!s.arm("other", || None));
        assert_eq!(
            s.snapshot(),
            Snapshot {
                active: true,
                entered_len: 0,
                error: false,
                wrong_attempts: 0,
            }
        );

        // Control chars and empty submit are ignored.
        assert_eq!(s.handle_key(KeyInput::Char('\n')), KeyOutcome::Ignored);
        assert_eq!(s.handle_key(KeyInput::Submit), KeyOutcome::Ignored);

        // Wrong password sets error, clears attempt, increments counter.
        assert_eq!(s.handle_key(KeyInput::Char('x')), KeyOutcome::Updated);
        assert_eq!(s.snapshot().entered_len, 1);
        assert_eq!(s.handle_key(KeyInput::Submit), KeyOutcome::WrongPassword);
        assert_eq!(
            s.snapshot(),
            Snapshot {
                active: true,
                entered_len: 0,
                error: true,
                wrong_attempts: 1,
            }
        );

        // Typing clears error; backspace and clear work as expected.
        assert_eq!(s.handle_key(KeyInput::Char('a')), KeyOutcome::Updated);
        assert!(!s.snapshot().error);
        assert_eq!(s.handle_key(KeyInput::Backspace), KeyOutcome::Updated);
        assert_eq!(s.handle_key(KeyInput::Backspace), KeyOutcome::Ignored);
        assert_eq!(s.handle_key(KeyInput::Char('b')), KeyOutcome::Updated);
        assert_eq!(s.handle_key(KeyInput::Clear), KeyOutcome::Updated);
        assert_eq!(s.handle_key(KeyInput::Clear), KeyOutcome::Ignored);

        // Correct password unlocks and zeroes memory buffers.
        for c in "pass 12".chars() {
            assert_eq!(s.handle_key(KeyInput::Char(c)), KeyOutcome::Updated);
        }
        assert_eq!(s.handle_key(KeyInput::Submit), KeyOutcome::Unlocked);
        assert!(!s.active);
        assert!(s.password.is_empty());
        assert!(s.attempt.is_empty());
        assert_eq!(s.snapshot(), Snapshot::default());
    }

    #[test]
    fn state_machine_abort_and_max_len() {
        let mut s = LockState::default();
        assert!(s.arm("secret", || None));
        for _ in 0..MAX_ATTEMPT_LEN {
            assert_eq!(s.handle_key(KeyInput::Char('a')), KeyOutcome::Updated);
        }
        assert_eq!(s.handle_key(KeyInput::Char('b')), KeyOutcome::Ignored);
        assert_eq!(s.snapshot().entered_len, MAX_ATTEMPT_LEN);

        s.abort_arm();
        assert!(!s.active);
        assert!(s.last_start_error);
        assert!(s.password.is_empty());
        assert!(s.attempt.is_empty());
    }
}
