//! In-memory desktop lock state machine, display-awake guard, and macOS
//! Accessibility permission checks.
//!
//! Nothing in this module is ever written to disk (`prefs` or files): if the
//! process is killed or the machine is force-restarted, the lock does not
//! persist across relaunch.

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::ptr;
use std::sync::Mutex;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};

const HASH_SCHEME: &str = "pbkdf2-sha256";
const HASH_ITERS: u32 = 100_000;
const MIN_HASH_ITERS: u32 = 10_000;
const MAX_HASH_ITERS: u32 = 2_000_000;
const SALT_LEN: usize = 16;

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
    RequestTouchId,
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
    pub touch_id_enabled: bool,
    pub touch_id_busy: bool,
    pub dimmed: bool,
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

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut key_block = [0_u8; 64];
    if key.len() > 64 {
        let digest = Sha256::digest(key);
        key_block[..32].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36_u8; 64];
    let mut opad = [0x5c_u8; 64];
    for i in 0..64 {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    let out: [u8; 32] = outer.finalize().into();

    for b in key_block
        .iter_mut()
        .chain(ipad.iter_mut())
        .chain(opad.iter_mut())
    {
        // SAFETY: `b` is a valid, uniquely borrowed stack byte.
        unsafe { ptr::write_volatile(b, 0) };
    }
    out
}

fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut block = Vec::with_capacity(salt.len() + 4);
    block.extend_from_slice(salt);
    block.extend_from_slice(&1_u32.to_be_bytes());
    let mut u = hmac_sha256(password, &block);
    let mut dk = u;
    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        for (dst, src) in dk.iter_mut().zip(u.iter()) {
            *dst ^= *src;
        }
    }
    zero_bytes(&mut block);
    for b in u.iter_mut() {
        // SAFETY: `b` is a valid, uniquely borrowed stack byte.
        unsafe { ptr::write_volatile(b, 0) };
    }
    dk
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    let (pairs, rem) = hex.as_bytes().as_chunks::<2>();
    if !rem.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let s = std::str::from_utf8(pair).ok()?;
        let byte = u8::from_str_radix(s, 16).ok()?;
        out.push(byte);
    }
    Some(out)
}

fn parse_password_hash(encoded: &str) -> Option<(u32, Vec<u8>, Vec<u8>)> {
    let mut parts = encoded.split('$');
    let scheme = parts.next()?;
    let iters_str = parts.next()?;
    let salt_hex = parts.next()?;
    let dk_hex = parts.next()?;
    if parts.next().is_some() || scheme != HASH_SCHEME {
        return None;
    }
    let iters: u32 = iters_str.parse().ok()?;
    if !(MIN_HASH_ITERS..=MAX_HASH_ITERS).contains(&iters) {
        return None;
    }
    if salt_hex.len() != SALT_LEN * 2 || dk_hex.len() != 64 {
        return None;
    }
    let salt = decode_hex(salt_hex)?;
    let dk = decode_hex(dk_hex)?;
    Some((iters, salt, dk))
}

/// Hashes `password` with a fresh 16-byte random salt using PBKDF2-HMAC-SHA256
/// (`100_000` rounds). Never writes or logs the plaintext password.
pub fn hash_password(password: &str) -> Result<String, &'static str> {
    if password.is_empty() {
        return Err("empty password");
    }
    let mut salt = [0_u8; SALT_LEN];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut salt))
        .map_err(|_| "random salt unavailable")?;
    let mut dk = pbkdf2_hmac_sha256(password.as_bytes(), &salt, HASH_ITERS);
    let encoded = format!(
        "{HASH_SCHEME}${HASH_ITERS}${}${}",
        encode_hex(&salt),
        encode_hex(&dk)
    );
    for b in dk.iter_mut() {
        // SAFETY: `b` is a valid, uniquely borrowed stack byte.
        unsafe { ptr::write_volatile(b, 0) };
    }
    Ok(encoded)
}

/// Verifies `password` against an encoded `pbkdf2-sha256$...` hash in constant time.
pub fn verify_password_hash(password: &str, encoded: &str) -> bool {
    let Some((iters, salt, expected)) = parse_password_hash(encoded) else {
        return false;
    };
    let mut actual = pbkdf2_hmac_sha256(password.as_bytes(), &salt, iters);
    let matched = constant_time_eq(&actual, &expected);
    for b in actual.iter_mut() {
        // SAFETY: `b` is a valid, uniquely borrowed stack byte.
        unsafe { ptr::write_volatile(b, 0) };
    }
    matched
}

/// Returns `true` if `encoded` matches the expected salted PBKDF2 hash format.
pub fn is_valid_password_hash(encoded: &str) -> bool {
    parse_password_hash(encoded).is_some()
}

#[derive(Default)]
enum LockSecret {
    #[default]
    None,
    Plain(Vec<u8>),
    Hash(String),
}

impl LockSecret {
    fn scrub(&mut self) {
        match self {
            LockSecret::None => {}
            LockSecret::Plain(bytes) => zero_bytes(bytes),
            LockSecret::Hash(text) => zero_string(text),
        }
        *self = LockSecret::None;
    }

    fn verify(&self, attempt: &str) -> bool {
        match self {
            LockSecret::None => false,
            LockSecret::Plain(bytes) => constant_time_eq(attempt.as_bytes(), bytes),
            LockSecret::Hash(hash) => verify_password_hash(attempt, hash),
        }
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        matches!(self, LockSecret::None)
    }
}

#[derive(Default)]
struct LockState {
    active: bool,
    secret: LockSecret,
    attempt: String,
    error: bool,
    wrong_attempts: u32,
    touch_id_enabled: bool,
    touch_id_busy: bool,
    dimmed: bool,
    last_start_error: bool,
    display_guard: Option<DisplayAwakeGuard>,
}

impl LockState {
    fn arm(
        &mut self,
        password: &str,
        touch_id: bool,
        acquire_guard: impl FnOnce() -> Option<DisplayAwakeGuard>,
    ) -> bool {
        if password.is_empty() || self.active {
            return false;
        }
        self.secret.scrub();
        zero_string(&mut self.attempt);
        self.secret = LockSecret::Plain(password.as_bytes().to_vec());
        self.error = false;
        self.wrong_attempts = 0;
        self.touch_id_enabled = touch_id;
        self.touch_id_busy = false;
        self.dimmed = false;
        self.last_start_error = false;
        self.display_guard = acquire_guard();
        self.active = true;
        true
    }

    fn arm_hash(
        &mut self,
        hash: String,
        touch_id: bool,
        acquire_guard: impl FnOnce() -> Option<DisplayAwakeGuard>,
    ) -> bool {
        if !is_valid_password_hash(&hash) || self.active {
            return false;
        }
        self.secret.scrub();
        zero_string(&mut self.attempt);
        self.secret = LockSecret::Hash(hash);
        self.error = false;
        self.wrong_attempts = 0;
        self.touch_id_enabled = touch_id;
        self.touch_id_busy = false;
        self.dimmed = false;
        self.last_start_error = false;
        self.display_guard = acquire_guard();
        self.active = true;
        true
    }

    fn abort_arm(&mut self) {
        self.secret.scrub();
        zero_string(&mut self.attempt);
        self.active = false;
        self.error = false;
        self.wrong_attempts = 0;
        self.touch_id_enabled = false;
        self.touch_id_busy = false;
        self.dimmed = false;
        self.last_start_error = true;
        self.display_guard = None;
    }

    fn force_disarm(&mut self) {
        self.secret.scrub();
        zero_string(&mut self.attempt);
        self.active = false;
        self.error = false;
        self.wrong_attempts = 0;
        self.touch_id_enabled = false;
        self.touch_id_busy = false;
        self.dimmed = false;
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
                    if self.touch_id_enabled && !self.touch_id_busy {
                        self.touch_id_busy = true;
                        self.error = false;
                        return KeyOutcome::RequestTouchId;
                    }
                    return KeyOutcome::Ignored;
                }
                if self.secret.verify(&self.attempt) {
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
            touch_id_enabled: self.touch_id_enabled,
            touch_id_busy: self.touch_id_busy,
            dimmed: self.dimmed,
        }
    }
}

static STATE: Mutex<LockState> = Mutex::new(LockState {
    active: false,
    secret: LockSecret::None,
    attempt: String::new(),
    error: false,
    wrong_attempts: 0,
    touch_id_enabled: false,
    touch_id_busy: false,
    dimmed: false,
    last_start_error: false,
    display_guard: None,
});

/// Arms the in-memory lock with `password` and takes an independent
/// display-awake assertion/inhibitor. Returns `false` if `password` is empty
/// or the lock is already active.
pub fn arm(password: &str) -> bool {
    let touch_id = crate::prefs::lock_touch_id() && touch_id_available();
    let ok = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.arm(password, touch_id, DisplayAwakeGuard::acquire)
    };
    if ok {
        tracing::info!("screen lock engaged");
    }
    ok
}

/// Arms the in-memory lock using the saved long-term salted PBKDF2 hash from
/// `prefs`. Returns `false` if no valid hash is configured or already locked.
pub fn arm_saved() -> bool {
    let Some(hash) = crate::prefs::lock_password_hash() else {
        return false;
    };
    if !is_valid_password_hash(&hash) {
        return false;
    }
    let touch_id = crate::prefs::lock_touch_id() && touch_id_available();
    let ok = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.arm_hash(hash, touch_id, DisplayAwakeGuard::acquire)
    };
    if ok {
        tracing::info!("screen lock engaged (saved password)");
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
        KeyOutcome::Updated | KeyOutcome::RequestTouchId | KeyOutcome::Ignored => {}
    }
    outcome
}

/// Unlocks the desktop lock after macOS Touch ID biometrics succeeds.
pub fn unlock_by_biometrics() -> bool {
    let ok = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if !state.active {
            false
        } else {
            state.force_disarm();
            true
        }
    };
    if ok {
        tracing::info!("screen lock unlocked via Touch ID");
    }
    ok
}

/// Marks the active Touch ID prompt as finished (cancelled or failed) without
/// disturbing any password error flag.
pub fn finish_touch_id() {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state.touch_id_busy = false;
}

/// Snapshot of the current lock state for UI rendering.
pub fn snapshot() -> Snapshot {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).snapshot()
}

/// Whether the desktop lock is currently engaged.
pub fn is_active() -> bool {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).active
}

/// Whether AI-friendly lock mode is configured on macOS (invisible to
/// screenshots, lets external synthetic events pass through).
pub fn ai_friendly() -> bool {
    cfg!(target_os = "macos") && crate::prefs::lock_mode() == crate::prefs::LockModePref::AiFriendly
}

/// Whether displays should black out & dim to 0 brightness while locked on macOS.
pub fn dim_display_enabled() -> bool {
    cfg!(target_os = "macos") && crate::prefs::lock_dim_display()
}

/// Updates the lock blackout/dimmed state while active. Returns `true` if changed.
pub fn set_dimmed(dimmed: bool) -> bool {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if !state.active || state.dimmed == dimmed {
        return false;
    }
    state.dimmed = dimmed;
    true
}

/// Whether the lock screen is currently in the black-out dimmed state.
pub fn is_dimmed() -> bool {
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state.active && state.dimmed
}

// ---- independent display-awake guard ---------------------------------------

/// Holds the display awake while the desktop lock is active without touching
/// the user's timed `awake` menu.
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

// ---- Touch ID biometrics availability (macOS) ------------------------------

#[cfg(target_os = "macos")]
#[link(name = "LocalAuthentication", kind = "framework")]
unsafe extern "C" {}

/// Whether Touch ID (biometrics only — never falling back to login password)
/// is available and enrolled on this machine.
pub fn touch_id_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        use objc2::msg_send;
        use objc2::rc::{Retained, autoreleasepool};
        use objc2::runtime::{AnyClass, AnyObject};
        use objc2_foundation::NSError;

        autoreleasepool(|_| {
            let Some(cls) = AnyClass::get(c"LAContext") else {
                return false;
            };
            // SAFETY: `LAContext` is a standard `LocalAuthentication` NSObject subclass.
            let ctx: Option<Retained<AnyObject>> = unsafe { msg_send![cls, new] };
            let Some(ctx) = ctx else {
                return false;
            };
            let mut err: *mut NSError = ptr::null_mut();
            // Policy 1 = `LAPolicyDeviceOwnerAuthenticationWithBiometrics`.
            // SAFETY: `ctx` is a live `LAContext`; `err` is a valid out-pointer.
            let ok: bool = unsafe { msg_send![&*ctx, canEvaluatePolicy: 1_isize, error: &mut err] };
            ok
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
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
    fn hash_and_verify_round_trip_and_uniqueness() {
        assert!(hash_password("").is_err());
        let h1 = hash_password("pass 12").unwrap();
        let h2 = hash_password("pass 12").unwrap();
        assert_ne!(h1, h2, "each hash must generate a fresh random salt");
        assert!(is_valid_password_hash(&h1));
        assert!(verify_password_hash("pass 12", &h1));
        assert!(verify_password_hash("pass 12", &h2));
        assert!(!verify_password_hash("pass 13", &h1));
        assert!(!verify_password_hash("pass 12 ", &h1));
        assert!(!verify_password_hash("pass 12", "invalid$1$aa$bb"));
        assert!(!is_valid_password_hash("pbkdf2-sha256$5$00$11"));
    }

    #[test]
    fn state_machine_full_cycle_and_memory_zeroing() {
        let mut s = LockState::default();
        assert!(!s.arm("", false, || None));
        assert!(s.arm("pass 12", false, || None));
        assert!(!s.arm("other", false, || None));
        assert_eq!(
            s.snapshot(),
            Snapshot {
                active: true,
                entered_len: 0,
                error: false,
                wrong_attempts: 0,
                touch_id_enabled: false,
                touch_id_busy: false,
                dimmed: false,
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
                touch_id_enabled: false,
                touch_id_busy: false,
                dimmed: false,
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
        assert!(s.secret.is_empty());
        assert!(s.attempt.is_empty());
        assert_eq!(s.snapshot(), Snapshot::default());
    }

    #[test]
    fn state_machine_saved_hash_and_touch_id_flow() {
        let hash = hash_password("long-secret").unwrap();
        let mut s = LockState::default();
        assert!(!s.arm_hash("bad".into(), true, || None));
        assert!(s.arm_hash(hash, true, || None));
        assert!(s.snapshot().touch_id_enabled);
        assert!(!s.snapshot().touch_id_busy);

        // Empty submit with Touch ID enabled triggers RequestTouchId once while idle.
        assert_eq!(s.handle_key(KeyInput::Submit), KeyOutcome::RequestTouchId);
        assert!(s.snapshot().touch_id_busy);
        assert_eq!(s.handle_key(KeyInput::Submit), KeyOutcome::Ignored);
        s.touch_id_busy = false;

        // Wrong password fails; correct password unlocks and scrubs hash.
        assert_eq!(s.handle_key(KeyInput::Char('w')), KeyOutcome::Updated);
        assert_eq!(s.handle_key(KeyInput::Submit), KeyOutcome::WrongPassword);
        for c in "long-secret".chars() {
            assert_eq!(s.handle_key(KeyInput::Char(c)), KeyOutcome::Updated);
        }
        assert_eq!(s.handle_key(KeyInput::Submit), KeyOutcome::Unlocked);
        assert!(!s.active);
        assert!(s.secret.is_empty());
    }

    #[test]
    fn state_machine_abort_and_max_len() {
        let mut s = LockState::default();
        assert!(s.arm("secret", false, || None));
        for _ in 0..MAX_ATTEMPT_LEN {
            assert_eq!(s.handle_key(KeyInput::Char('a')), KeyOutcome::Updated);
        }
        assert_eq!(s.handle_key(KeyInput::Char('b')), KeyOutcome::Ignored);
        assert_eq!(s.snapshot().entered_len, MAX_ATTEMPT_LEN);

        s.abort_arm();
        assert!(!s.active);
        assert!(s.last_start_error);
        assert!(!s.dimmed);
        assert!(s.secret.is_empty());
        assert!(s.attempt.is_empty());
    }

    #[test]
    fn dimmed_flag_resets_on_abort_and_disarm() {
        let mut s = LockState::default();
        assert!(s.arm("secret", false, || None));
        assert!(!s.snapshot().dimmed);
        s.dimmed = true;
        assert!(s.snapshot().dimmed);
        s.force_disarm();
        assert!(!s.snapshot().dimmed);

        assert!(s.arm("secret", false, || None));
        s.dimmed = true;
        s.abort_arm();
        assert!(!s.snapshot().dimmed);
    }
}
