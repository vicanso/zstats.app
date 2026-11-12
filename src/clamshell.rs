//! Keep a MacBook running with the lid closed (`pmset -a disablesleep`),
//! including on battery.
//!
//! [`crate::awake`] holds `PreventUserIdleDisplaySleep`, which stops idle
//! dimming and idle sleep while the lid is open — and intentionally yields
//! the moment the lid closes. Closing the lid on battery without an external
//! display needs `pmset -a disablesleep 1`, which sets `SleepDisabled` on
//! `IOPMrootDomain` in `powerd` and requires root.
//!
//! **Read without root, write without a daemon.** Reading `SleepDisabled`
//! needs neither `sudo` nor a child process: `IOPMrootDomain` publishes the
//! boolean in the IOKit registry, where `pmset -g` reads it too. Writing it
//! needs root, and asking for an administrator password on every toggle — or
//! worse, when a countdown expires — would make the footer control unusable.
//! Instead of shipping a `SMJobBless` root helper daemon, the first toggle
//! asks macOS once (`osascript … with administrator privileges`), validates a
//! narrow rule with `/usr/sbin/visudo -cf`, and moves it atomically into
//! `/private/etc/sudoers.d/zstats-pmset` (`0440 root:wheel`). The rule grants
//! the current user `NOPASSWD` for `/usr/bin/pmset -a disablesleep 1` and
//! `/usr/bin/pmset -a disablesleep 0` — nothing else. Every later toggle,
//! countdown expiry, low-battery restore and quit cleanup runs `sudo -n`
//! silently. Settings → Permissions shows whether the rule is in place and
//! can install or remove it on its own.
//!
//! **Kernel persistence and our three safeguards.** Unlike an IOKit assertion,
//! `disablesleep 1` lives in `powerd` past the process that set it, and stops
//! *every* sleep — low-battery hibernation included. So whenever zstats itself
//! turned `disablesleep` on in this session (`Timed` or `Indefinite`):
//! 1. Timed holds end on the wall clock in [`on_tick`].
//! 2. [`on_tick`] restores `disablesleep 0` if the battery is discharging at
//!    or below [`LOW_BATTERY_FLOOR_PERCENT`] (15%), before a closed laptop in
//!    a bag can drain to a hard power-off.
//! 3. [`restore_on_exit`] restores `disablesleep 0` on normal quit (`⌘Q`,
//!    footer/tray Quit, update restart) and from a `SIGTERM`/`SIGINT` watcher.
//!
//! If `SleepDisabled` was already `1` outside zstats (for example from a
//! terminal), the footer still lights the laptop icon and says so, and zstats
//! leaves that external setting alone on exit unless the user takes it over
//! from the menu.

use crate::state::Effects;
use crate::state::effects;
use std::ffi::CStr;
use std::fs;
use std::mem::MaybeUninit;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, Once};
use std::thread;
use std::time::{Duration, SystemTime};
use zstats::snapshot::BatterySnapshot;

/// Offered timed lengths, in minutes — the same set as [`crate::awake`].
pub const HOLD_MINUTES: [u32; 5] = crate::awake::HOLD_MINUTES;

/// Discharging at or below this charge restores normal sleep while zstats
/// owns `disablesleep`, so a closed MacBook on battery keeps a reserve to
/// sleep instead of running flat.
pub const LOW_BATTERY_FLOOR_PERCENT: f32 = 15.0;

/// Drop-in `sudoers` rule managed by zstats.
pub const SUDOERS_PATH: &str = "/private/etc/sudoers.d/zstats-pmset";

/// How system-wide sleep suppression stands right now, as the footer reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Normal sleep (`SleepDisabled = 0`).
    Off,
    /// Enabled by zstats for `minutes`, ending at `until` on the wall clock.
    Timed { minutes: u32, until: SystemTime },
    /// Enabled by zstats until turned off or zstats exits.
    Indefinite,
    /// `SleepDisabled = 1` was set outside zstats.
    External,
}

impl Mode {
    pub fn is_on(&self) -> bool {
        !matches!(self, Mode::Off)
    }

    /// Whole minutes left at `now`, rounded up, when running on a timer —
    /// same rounding as [`crate::awake::Hold::minutes_left`].
    pub fn minutes_left(&self, now: SystemTime) -> Option<u32> {
        match self {
            Mode::Timed { until, .. } => {
                let left = until.duration_since(now).unwrap_or_default();
                Some(u32::try_from(left.as_secs().div_ceil(60)).unwrap_or(u32::MAX))
            }
            _ => None,
        }
    }
}

/// What the footer menu asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Off,
    Indefinite,
    Timed(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owned {
    Timed { minutes: u32, until: SystemTime },
    Indefinite,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct State {
    system_on: bool,
    owned: Option<Owned>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum AutoRestore {
    Expired { minutes: u32 },
    LowBattery { charge_percent: f32 },
}

impl State {
    fn mode(&self) -> Mode {
        if !self.system_on {
            return Mode::Off;
        }
        match self.owned {
            Some(Owned::Timed { minutes, until }) => Mode::Timed { minutes, until },
            Some(Owned::Indefinite) => Mode::Indefinite,
            None => Mode::External,
        }
    }

    /// Fold a fresh `SleepDisabled` reading into our state. An external
    /// `pmset -a disablesleep 0` clears any ownership we thought we had.
    fn reconcile(&mut self, system_on: bool) -> (Mode, Mode) {
        let before = self.mode();
        self.system_on = system_on;
        if !system_on {
            self.owned = None;
        }
        (before, self.mode())
    }

    fn apply_request(&mut self, req: Request, now: SystemTime) {
        match req {
            Request::Off => {
                self.system_on = false;
                self.owned = None;
            }
            Request::Indefinite => {
                self.system_on = true;
                self.owned = Some(Owned::Indefinite);
            }
            Request::Timed(minutes) => {
                let until = now + Duration::from_secs(u64::from(minutes) * 60);
                self.system_on = true;
                self.owned = Some(Owned::Timed { minutes, until });
            }
        }
    }

    fn due_auto_restore(
        &self,
        now: SystemTime,
        battery: Option<&BatterySnapshot>,
    ) -> Option<AutoRestore> {
        if !self.system_on {
            return None;
        }
        let owned = self.owned?;
        if let Owned::Timed { minutes, until } = owned
            && now >= until
        {
            return Some(AutoRestore::Expired { minutes });
        }
        if let Some(charge_percent) = low_battery_charge(battery) {
            return Some(AutoRestore::LowBattery { charge_percent });
        }
        None
    }

    /// Claims ownership once for exit cleanup. Returns `true` only when
    /// zstats itself enabled `disablesleep` and it is still on — never for
    /// [`Mode::External`].
    fn take_for_exit(&mut self) -> bool {
        let restore = self.system_on && self.owned.take().is_some();
        if restore {
            self.system_on = false;
        } else {
            self.owned = None;
        }
        restore
    }
}

static STATE: Mutex<State> = Mutex::new(State {
    system_on: false,
    owned: None,
});
static BUSY: AtomicBool = AtomicBool::new(false);
static AUTHORIZED: AtomicBool = AtomicBool::new(false);
static AUTH_LOGGED: AtomicBool = AtomicBool::new(false);
static SIG_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// Seed the live state and install exit / signal hooks once at startup.
pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = refresh();
        let _ = refresh_authorized();
        // SAFETY: `atexit_restore` is an `extern "C"` function with no
        // preconditions; registered once for `⌘Q` (`NSApp terminate:`).
        unsafe {
            libc::atexit(atexit_restore);
        }
        install_signal_watcher();
    });
}

/// Current mode snapshot.
pub fn mode() -> Mode {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .mode()
}

/// Whether an admin prompt or `pmset` call is currently running.
pub fn busy() -> bool {
    BUSY.load(Ordering::Relaxed)
}

/// Whether `/private/etc/sudoers.d/zstats-pmset` is installed (`0440`,
/// owned by `root`).
pub fn is_authorized() -> bool {
    AUTHORIZED.load(Ordering::Relaxed)
}

/// Re-check the `sudoers.d` rule on disk, logging only when it changes.
pub fn refresh_authorized() -> bool {
    let now = sudoers_file_valid(Path::new(SUDOERS_PATH));
    let prev = AUTHORIZED.swap(now, Ordering::Relaxed);
    let first = !AUTH_LOGGED.swap(true, Ordering::Relaxed);
    if (first && now) || (!first && prev != now) {
        tracing::info!(
            authorized = now,
            "clamshell sudoers authorization refreshed"
        );
    }
    prev != now
}

/// Re-read `SleepDisabled` from `IOPMrootDomain` and reconcile our state.
/// Returns `true` when the visible [`Mode`] moved.
pub fn refresh() -> bool {
    let Some(system_on) = read_sleep_disabled() else {
        return false;
    };
    let (before, after) = {
        let mut state = STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.reconcile(system_on)
    };
    if before != after {
        tracing::info!(?before, ?after, "clamshell sleep state observed");
        return true;
    }
    false
}

/// Apply a user choice from the footer menu on a background thread, then call
/// `on_done` (typically `store::changed`) so every window updates.
pub fn request(req: Request, on_done: fn()) {
    if BUSY
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    on_done();
    let spawn = thread::Builder::new()
        .name("clamshell".into())
        .spawn(move || {
            if matches!(req, Request::Off) && read_sleep_disabled() == Some(false) {
                let mut state = STATE
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.apply_request(Request::Off, SystemTime::now());
                BUSY.store(false, Ordering::Release);
                on_done();
                return;
            }
            let want_on = !matches!(req, Request::Off);
            // Even if `SleepDisabled` is already 1 externally, picking a timed or
            // session hold runs `apply_pmset(true)` so passwordless `sudo` is
            // verified now rather than missing when the timer or exit needs to
            // restore sleep.
            let outcome = apply_pmset(want_on);
            match outcome {
                Ok(()) => {
                    let now = SystemTime::now();
                    {
                        let mut state = STATE
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        state.apply_request(req, now);
                    }
                    match req {
                        Request::Off => {
                            tracing::info!("clamshell sleep restored: disablesleep=0");
                        }
                        Request::Indefinite => {
                            tracing::info!("clamshell no-sleep on: until turned off");
                        }
                        Request::Timed(minutes) => {
                            tracing::info!(minutes, "clamshell no-sleep on: timed hold");
                        }
                    }
                }
                Err(ApplyError::Cancelled) => {
                    tracing::info!("clamshell authorization cancelled by user");
                    let _ = refresh();
                }
                Err(ApplyError::Failed(err)) => {
                    tracing::warn!(error = %err, "could not change pmset disablesleep");
                    let _ = refresh();
                }
            }
            let _ = refresh_authorized();
            BUSY.store(false, Ordering::Release);
            on_done();
        });
    if let Err(e) = spawn {
        BUSY.store(false, Ordering::Release);
        tracing::error!("could not start clamshell thread: {e}");
        on_done();
    }
}

/// Install `/private/etc/sudoers.d/zstats-pmset` from Settings → Permissions
/// without changing `SleepDisabled`.
pub fn authorize(on_done: fn()) {
    if BUSY
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    on_done();
    let spawn = thread::Builder::new()
        .name("clamshell-auth".into())
        .spawn(move || {
            match install_sudoers_with_admin(None) {
                Ok(()) => tracing::info!("clamshell sudoers rule installed"),
                Err(ApplyError::Cancelled) => {
                    tracing::info!("clamshell sudoers authorization cancelled by user");
                }
                Err(ApplyError::Failed(err)) => {
                    tracing::warn!(error = %err, "could not install clamshell sudoers rule");
                }
            }
            let _ = refresh_authorized();
            BUSY.store(false, Ordering::Release);
            on_done();
        });
    if let Err(e) = spawn {
        BUSY.store(false, Ordering::Release);
        tracing::error!("could not start clamshell-auth thread: {e}");
        on_done();
    }
}

/// Remove `/private/etc/sudoers.d/zstats-pmset` from Settings → Permissions.
/// If zstats currently holds `disablesleep`, restores `disablesleep 0` in the
/// same administrator action so revoking never strands the Mac unable to sleep.
pub fn revoke(on_done: fn()) {
    if BUSY
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    on_done();
    let spawn = thread::Builder::new()
        .name("clamshell-revoke".into())
        .spawn(move || {
            let restore_too = {
                let state = STATE
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.system_on && state.owned.is_some()
            };
            let shell = if restore_too {
                format!("set -eu\n/usr/bin/pmset -a disablesleep 0\n/bin/rm -f {SUDOERS_PATH}\n")
            } else {
                format!("set -eu\n/bin/rm -f {SUDOERS_PATH}\n")
            };
            match run_admin_shell(&shell) {
                Ok(()) => {
                    if restore_too {
                        let mut state = STATE
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        state.apply_request(Request::Off, SystemTime::now());
                    }
                    tracing::info!(
                        restored_sleep = restore_too,
                        "clamshell sudoers rule revoked"
                    );
                }
                Err(ApplyError::Cancelled) => {
                    tracing::info!("clamshell sudoers revoke cancelled by user");
                }
                Err(ApplyError::Failed(err)) => {
                    tracing::warn!(error = %err, "could not revoke clamshell sudoers rule");
                }
            }
            let _ = refresh();
            let _ = refresh_authorized();
            BUSY.store(false, Ordering::Release);
            on_done();
        });
    if let Err(e) = spawn {
        BUSY.store(false, Ordering::Release);
        tracing::error!("could not start clamshell-revoke thread: {e}");
        on_done();
    }
}

/// Collector tick hook: reconciles external changes, ends expired timed holds,
/// and enforces the low-battery floor (`<= 15%` while discharging).
pub fn on_tick(battery: Option<&BatterySnapshot>, fx: &mut dyn Effects) {
    let _ = refresh();
    let due = {
        let state = STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.due_auto_restore(SystemTime::now(), battery)
    };
    let Some(reason) = due else {
        return;
    };
    if BUSY
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    effects::background(fx, move |reply| {
        match run_sudo_pmset(false) {
            Ok(()) => {
                {
                    let mut state = STATE
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.apply_request(Request::Off, SystemTime::now());
                }
                match reason {
                    AutoRestore::Expired { minutes } => {
                        tracing::info!(minutes, "clamshell no-sleep off: timed hold expired");
                    }
                    AutoRestore::LowBattery { charge_percent } => {
                        tracing::info!(
                            charge_percent,
                            floor = LOW_BATTERY_FLOOR_PERCENT,
                            "clamshell no-sleep off: low battery floor reached while discharging"
                        );
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    ?reason,
                    "could not auto-restore pmset disablesleep=0 via sudo -n"
                );
                let _ = refresh();
            }
        }
        BUSY.store(false, Ordering::Release);
        reply.apply(|_, fx| fx.changed());
    });
}

/// Synchronous exit cleanup: restores `disablesleep 0` at most once, and only
/// when zstats itself turned `disablesleep` on during this session.
pub fn restore_on_exit() {
    let should_restore = {
        let mut state = STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.take_for_exit()
    };
    if !should_restore {
        return;
    }
    match run_sudo_pmset(false) {
        Ok(()) => tracing::info!("clamshell sleep restored on exit: disablesleep=0"),
        Err(err) => {
            tracing::warn!(error = %err, "could not restore pmset disablesleep=0 on exit")
        }
    }
}

extern "C" fn atexit_restore() {
    restore_on_exit();
}

extern "C" fn handle_termination_signal(sig: libc::c_int) {
    let fd = SIG_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = sig as u8;
        // SAFETY: `write` on a non-blocking pipe fd is async-signal-safe.
        unsafe {
            let _ = libc::write(fd, (&raw const byte).cast(), 1);
        }
    }
}

fn install_signal_watcher() {
    let mut fds = [0_i32; 2];
    // SAFETY: `fds` points to two writable `c_int`s.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        tracing::warn!("could not open clamshell signal pipe");
        return;
    }
    let [read_fd, write_fd] = fds;
    // SAFETY: `read_fd` and `write_fd` were just created by `pipe`: mark both
    // `FD_CLOEXEC` so child processes (`pmset`, `tmutil`, `osascript`) never
    // inherit them, and make `write_fd` non-blocking for the signal handler.
    unsafe {
        let _ = libc::fcntl(read_fd, libc::F_SETFD, libc::FD_CLOEXEC);
        let _ = libc::fcntl(write_fd, libc::F_SETFD, libc::FD_CLOEXEC);
        let flags = libc::fcntl(write_fd, libc::F_GETFL);
        if flags >= 0 {
            let _ = libc::fcntl(write_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    SIG_WRITE_FD.store(write_fd, Ordering::Relaxed);

    // SAFETY: zeroed `sigaction` with an async-signal-safe handler and empty
    // mask; installed for `SIGTERM` and `SIGINT`.
    unsafe {
        let mut act: libc::sigaction = MaybeUninit::zeroed().assume_init();
        act.sa_sigaction = handle_termination_signal as *const () as usize;
        libc::sigemptyset(&mut act.sa_mask);
        act.sa_flags = libc::SA_RESTART;
        let _ = libc::sigaction(libc::SIGTERM, &act, ptr::null_mut());
        let _ = libc::sigaction(libc::SIGINT, &act, ptr::null_mut());
    }

    if let Err(e) = thread::Builder::new()
        .name("clamshell-sig".into())
        .spawn(move || {
            let mut byte = 0_u8;
            loop {
                // SAFETY: `read_fd` belongs exclusively to this thread.
                let n = unsafe { libc::read(read_fd, (&raw mut byte).cast(), 1) };
                if n == 1 {
                    let sig = i32::from(byte);
                    tracing::info!(signal = sig, "termination signal received");
                    restore_on_exit();
                    // Re-raise under default disposition so the exit status
                    // carries the signal.
                    unsafe {
                        libc::signal(sig, libc::SIG_DFL);
                        libc::raise(sig);
                        libc::_exit(128 + sig);
                    }
                }
                if n == 0 {
                    return;
                }
            }
        })
    {
        tracing::warn!("could not start clamshell-sig thread: {e}");
    }
}

/// Read `SleepDisabled` off `IOPMrootDomain` in the IOKit registry — no root
/// and no subprocess.
fn read_sleep_disabled() -> Option<bool> {
    use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString};
    use objc2_io_kit::{
        IO_OBJECT_NULL, IOObjectRelease, IORegistryEntryCreateCFProperty,
        IOServiceGetMatchingService, IOServiceMatching, kIOMainPortDefault,
    };

    // SAFETY: static NUL-terminated class name; `IOServiceGetMatchingService`
    // consumes one retain on `matching`.
    let entry = unsafe {
        let matching = IOServiceMatching(c"IOPMrootDomain".as_ptr())
            .map(|d| CFRetained::<CFDictionary>::from(&*d));
        IOServiceGetMatchingService(kIOMainPortDefault, matching)
    };
    if entry == IO_OBJECT_NULL {
        return None;
    }
    let key = CFString::from_static_str("SleepDisabled");
    // SAFETY: `entry` is a live registry entry released immediately below;
    // `None` allocator uses `kCFAllocatorDefault`.
    let prop = unsafe { IORegistryEntryCreateCFProperty(entry, Some(&key), None, 0) };
    let _ = IOObjectRelease(entry);
    Some(
        prop.and_then(|v| v.downcast::<CFBoolean>().ok())
            .is_some_and(|b| b.value()),
    )
}

fn low_battery_charge(battery: Option<&BatterySnapshot>) -> Option<f32> {
    if !crate::diskwatch::on_battery(battery) {
        return None;
    }
    let charge = battery?.charge_percent;
    (charge <= LOW_BATTERY_FLOOR_PERCENT).then_some(charge)
}

fn sudoers_file_valid(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.uid() == 0 && (m.mode() & 0o777) == 0o440)
}

#[derive(Debug)]
enum ApplyError {
    Cancelled,
    Failed(String),
}

fn apply_pmset(disable: bool) -> Result<(), ApplyError> {
    if sudoers_file_valid(Path::new(SUDOERS_PATH)) && run_sudo_pmset(disable).is_ok() {
        return Ok(());
    }
    install_sudoers_with_admin(Some(disable))
}

fn run_sudo_pmset(disable: bool) -> Result<(), String> {
    let arg = if disable { "1" } else { "0" };
    let out = Command::new("/usr/bin/sudo")
        .args(["-n", "/usr/bin/pmset", "-a", "disablesleep", arg])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        Err(if stderr.is_empty() {
            format!("sudo exited with {}", out.status)
        } else {
            stderr
        })
    }
}

fn install_sudoers_with_admin(pmset_disable: Option<bool>) -> Result<(), ApplyError> {
    let user = current_username()
        .ok_or_else(|| ApplyError::Failed("could not resolve a valid POSIX username".into()))?;
    let shell = install_sudoers_shell(&user, pmset_disable);
    run_admin_shell(&shell)
}

fn run_admin_shell(shell: &str) -> Result<(), ApplyError> {
    let expr = applescript_admin_expr(shell);
    let out = Command::new("/usr/bin/osascript")
        .args(["-e", &expr])
        .output()
        .map_err(|e| ApplyError::Failed(e.to_string()))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    if stderr.contains("(-128)") {
        return Err(ApplyError::Cancelled);
    }
    Err(ApplyError::Failed(if stderr.is_empty() {
        format!("osascript exited with {}", out.status)
    } else {
        stderr
    }))
}

fn current_username() -> Option<String> {
    // SAFETY: `geteuid` has no preconditions; `getpwuid_r` writes into our
    // stack struct and byte buffer.
    unsafe {
        let uid = libc::geteuid();
        let mut pwd = MaybeUninit::<libc::passwd>::uninit();
        let mut buf = vec![0_u8; 4096];
        let mut result: *mut libc::passwd = ptr::null_mut();
        let rc = libc::getpwuid_r(
            uid,
            pwd.as_mut_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        );
        if rc != 0 || result.is_null() {
            return None;
        }
        let pwd = pwd.assume_init();
        if pwd.pw_name.is_null() {
            return None;
        }
        let name = CStr::from_ptr(pwd.pw_name).to_str().ok()?;
        valid_sudoers_user(name).then(|| name.to_owned())
    }
}

/// Strict portable POSIX account name: starts with an ASCII letter or `_`,
/// followed only by ASCII letters, digits, `_`, `-`, or `.`. Rejects spaces,
/// quotes, colons, commas and leading `-` so the name is safe verbatim in both
/// `sudoers` and POSIX `sh`.
fn valid_sudoers_user(user: &str) -> bool {
    let Some(&first) = user.as_bytes().first() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    user.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn sudoers_rule(user: &str) -> String {
    debug_assert!(valid_sudoers_user(user));
    format!(
        "# Managed by zstats — passwordless lid-close sleep toggle\n\
         {user} ALL=(root) NOPASSWD: /usr/bin/pmset -a disablesleep 1, /usr/bin/pmset -a disablesleep 0\n"
    )
}

fn install_sudoers_shell(user: &str, pmset_disable: Option<bool>) -> String {
    let rule = sudoers_rule(user).replace('\'', "'\\''");
    let mut script = format!(
        "set -eu\n\
         /bin/mkdir -p /private/etc/sudoers.d\n\
         tmp=$(/usr/bin/mktemp /private/etc/sudoers.d/.zstats-pmset.XXXXXX)\n\
         trap '/bin/rm -f \"$tmp\"' EXIT\n\
         /usr/bin/printf '%s' '{rule}' > \"$tmp\"\n\
         /usr/sbin/chown root:wheel \"$tmp\"\n\
         /bin/chmod 0440 \"$tmp\"\n\
         /usr/sbin/visudo -cf \"$tmp\" >/dev/null\n\
         /bin/mv -f \"$tmp\" {SUDOERS_PATH}\n\
         trap - EXIT\n"
    );
    if let Some(disable) = pmset_disable {
        let bit = if disable { '1' } else { '0' };
        script.push_str(&format!("/usr/bin/pmset -a disablesleep {bit}\n"));
    }
    script
}

fn applescript_admin_expr(shell: &str) -> String {
    let mut expr = String::with_capacity(shell.len() + 64);
    expr.push_str("do shell script \"");
    for ch in shell.chars() {
        match ch {
            '\\' => expr.push_str("\\\\"),
            '"' => expr.push_str("\\\""),
            '\n' => expr.push_str("\\n"),
            '\r' => expr.push_str("\\r"),
            c => expr.push(c),
        }
    }
    expr.push_str("\" with administrator privileges");
    expr
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_battery(state: &str, charge_percent: f32) -> BatterySnapshot {
        BatterySnapshot {
            state: state.into(),
            charge_percent,
            health_percent: Some(95.0),
            power_watts: Some(10.0),
            temperature_celsius: None,
            cycle_count: Some(100),
            time_to_full_secs: None,
            time_to_empty_secs: None,
        }
    }

    /// `IOPMrootDomain`'s `SleepDisabled` property is the same boolean
    /// `pmset -g` prints.
    #[test]
    fn iokit_sleep_disabled_agrees_with_pmset() {
        let iokit = read_sleep_disabled().expect("IOPMrootDomain is present on macOS");
        let out = Command::new("/usr/bin/pmset")
            .arg("-g")
            .output()
            .expect("pmset -g runs without privileges");
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        let pmset = text
            .lines()
            .find_map(|line| {
                let mut parts = line.split_whitespace();
                let key = parts.next()?;
                let val = parts.next()?;
                (key == "SleepDisabled").then_some(val == "1")
            })
            .unwrap_or(false);
        assert_eq!(iokit, pmset);
    }

    #[test]
    fn current_username_and_sudoers_rule_are_well_formed() {
        let user = current_username().expect("process uid resolves to a valid username");
        assert!(valid_sudoers_user(&user));
        assert!(valid_sudoers_user("tree"));
        assert!(valid_sudoers_user("_service.user-1"));
        assert!(!valid_sudoers_user(""));
        assert!(!valid_sudoers_user("-dash"));
        assert!(!valid_sudoers_user("9digit"));
        assert!(!valid_sudoers_user("bad user"));
        assert!(!valid_sudoers_user("bad:user"));
        assert!(!valid_sudoers_user("bad\"user"));
        assert!(!valid_sudoers_user("bad\nuser"));

        let rule = sudoers_rule("tree");
        assert!(rule.ends_with('\n'), "visudo requires a trailing newline");
        assert!(rule.contains(
            "tree ALL=(root) NOPASSWD: /usr/bin/pmset -a disablesleep 1, /usr/bin/pmset -a disablesleep 0\n"
        ));

        let shell = install_sudoers_shell("tree", Some(true));
        assert!(shell.contains("/usr/sbin/visudo -cf \"$tmp\" >/dev/null"));
        assert!(shell.contains("/usr/bin/pmset -a disablesleep 1\n"));
        let expr = applescript_admin_expr(&shell);
        assert!(expr.starts_with("do shell script \""));
        assert!(expr.ends_with("\" with administrator privileges"));
        assert!(!expr.contains('\n'));
    }

    #[test]
    fn low_battery_trips_only_when_discharging_at_or_below_floor() {
        let now = SystemTime::now();
        let mut state = State::default();
        state.apply_request(Request::Indefinite, now);

        assert_eq!(
            state.due_auto_restore(now, Some(&sample_battery("discharging", 15.0))),
            Some(AutoRestore::LowBattery {
                charge_percent: 15.0
            })
        );
        assert_eq!(
            state.due_auto_restore(now, Some(&sample_battery("Discharging", 8.5))),
            Some(AutoRestore::LowBattery {
                charge_percent: 8.5
            })
        );
        assert_eq!(
            state.due_auto_restore(now, Some(&sample_battery("discharging", 15.1))),
            None
        );
        assert_eq!(
            state.due_auto_restore(now, Some(&sample_battery("charging", 5.0))),
            None
        );
        assert_eq!(
            state.due_auto_restore(now, Some(&sample_battery("full", 100.0))),
            None
        );
        assert_eq!(state.due_auto_restore(now, None), None);
    }

    #[test]
    fn external_and_owned_state_reconciliation() {
        let now = SystemTime::now();
        let mut state = State::default();
        assert_eq!(state.mode(), Mode::Off);
        assert!(!state.take_for_exit());

        // External `sudo pmset -a disablesleep 1` lights the control as
        // External and is left alone on exit and low battery.
        let (_, mode) = state.reconcile(true);
        assert_eq!(mode, Mode::External);
        assert!(mode.is_on());
        assert_eq!(
            state.due_auto_restore(now, Some(&sample_battery("discharging", 5.0))),
            None
        );
        assert!(!state.take_for_exit());

        // Taking over with a timed hold marks zstats as the owner, counts
        // down, expires at the deadline, and restores on exit if still held.
        state.apply_request(Request::Timed(30), now);
        assert_eq!(state.mode().minutes_left(now), Some(30));
        assert_eq!(
            state.due_auto_restore(now + Duration::from_secs(30 * 60), None),
            Some(AutoRestore::Expired { minutes: 30 })
        );

        // External `sudo pmset -a disablesleep 0` clears zstats' ownership.
        let (_, mode) = state.reconcile(false);
        assert_eq!(mode, Mode::Off);
        assert!(!state.take_for_exit());

        // Owned Indefinite restores once on exit, then is a no-op.
        state.apply_request(Request::Indefinite, now);
        assert_eq!(state.mode(), Mode::Indefinite);
        assert!(state.take_for_exit());
        assert!(!state.take_for_exit());
        assert_eq!(state.mode(), Mode::Off);
    }
}
