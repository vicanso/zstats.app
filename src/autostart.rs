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
//!
//! **Linux is the freedesktop autostart directory**, and the same
//! stance: the file under `~/.config/autostart` IS the state, nothing is
//! kept in `app.toml`, and the user can delete it by hand at any time.
//! The one thing that changes is *who reads that directory*, because a
//! compositor is not a session manager. systemd sessions serve it
//! through `xdg-desktop-autostart.target`, full desktops through their
//! own session manager, and a bare Hyprland/Sway session through
//! nobody — where dropping the file in would be an "on" switch that
//! starts nothing. So [`Status::NotFound`] is reused for "this session
//! has no autostart reader" (the mechanism is absent, as on a
//! bundle-less macOS run), and the row shows a sentence instead of a
//! switch. `install-linux.sh` makes the same decision the same way.

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

/// Linux: is the entry there, and does anything read the directory. A
/// subprocess (`systemctl`) at the same moments macOS pays an XPC
/// round-trip, never per frame.
#[cfg(target_os = "linux")]
pub fn refresh() {
    let raw = if !xdg::autostart_is_read() {
        3 // notFound: nothing in this session reads the directory
    } else if xdg::entry_path().is_some_and(|p| p.is_file()) {
        1
    } else {
        0
    };
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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
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

/// Linux: write or remove the desktop entry. The entry names *this*
/// executable by absolute path — the one the user is running when they
/// flip the switch, wherever it lives — and no arguments, because a
/// bare launch is the login path (`ipc::Invocation::Launch` deliberately
/// opens no panel). After the write the systemd generator is told to
/// look again; without that it serves the directory as it last saw it,
/// and the switch would read on while the next login started nothing.
#[cfg(target_os = "linux")]
pub fn set_enabled(enabled: bool) {
    use std::fs;
    use std::process::Command;

    let Some(path) = xdg::entry_path() else {
        tracing::warn!("launch-at-login: no config directory to write to");
        refresh();
        return;
    };
    let result = if enabled {
        std::env::current_exe()
            .map_err(|e| e.to_string())
            .and_then(|exe| {
                path.parent()
                    .map(fs::create_dir_all)
                    .transpose()
                    .map_err(|e| e.to_string())?;
                fs::write(&path, xdg::desktop_entry(&exe)).map_err(|e| e.to_string())
            })
    } else {
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    };
    match result {
        Ok(()) => tracing::info!(
            path = %path.display(),
            "launch-at-login {}",
            if enabled { "on" } else { "off" }
        ),
        Err(e) => tracing::warn!(
            "launch-at-login {}: {e}",
            if enabled { "on" } else { "off" }
        ),
    }
    // Best effort: a session without systemd has no generator to tell,
    // and the file is already the truth for the readers that have none.
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();
    // The directory is the record: re-read rather than assume the write took.
    refresh();
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn set_enabled(_enabled: bool) {}

/// The freedesktop half. Its two judgements — where the entry lives and
/// whether anyone reads it — are the ones `install-linux.sh` makes too,
/// and they must keep agreeing: an installer that writes the file and a
/// switch that cannot see it would be two truths.
#[cfg(target_os = "linux")]
mod xdg {
    use std::env;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// `$XDG_CONFIG_HOME/autostart/zstats.desktop`, `~/.config` failing
    /// that. `None` only with no home at all.
    pub fn entry_path() -> Option<PathBuf> {
        let config = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(config.join("autostart").join("zstats.desktop"))
    }

    /// Does anything in this session read the autostart directory.
    ///
    /// systemd's generator first — on a systemd session that target is
    /// active whether or not a desktop environment is present, which is
    /// what makes autostart work under a bare compositor on those
    /// distributions. Then the desktops that implement it themselves.
    /// Anything else is a session where the file would be inert.
    pub fn autostart_is_read() -> bool {
        let systemd = Command::new("systemctl")
            .args(["--user", "is-active", "xdg-desktop-autostart.target"])
            .output()
            .is_ok_and(|out| out.status.success());
        if systemd {
            return true;
        }
        let desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        desktop.split(':').any(|d| {
            matches!(
                d,
                "GNOME"
                    | "KDE"
                    | "XFCE"
                    | "X-Cinnamon"
                    | "MATE"
                    | "LXQt"
                    | "Budgie"
                    | "Pantheon"
                    | "Deepin"
            )
        })
    }

    /// The entry itself. Same content `install-linux.sh` writes, for the
    /// same reasons it gives: no arguments (the login launch must not
    /// open the panel), an absolute `Exec` (generated units do not
    /// inherit a shell's `PATH`), and `StartupNotify=false` (a
    /// layer-shell surface never maps the toplevel a startup
    /// notification waits for, so the cursor would spin to a timeout).
    pub fn desktop_entry(exe: &Path) -> String {
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=zstats\n\
             Comment=Menu-bar metrics panel\n\
             Exec={}\n\
             Icon=zstats\n\
             Terminal=false\n\
             StartupNotify=false\n\
             Categories=Utility;System;Monitor;\n",
            exec_arg(exe)
        )
    }

    /// One `Exec` argument per the Desktop Entry spec: quoted, with the
    /// four characters the spec reserves inside quotes escaped, whenever
    /// the path holds anything the spec reserves outside them. A plain
    /// path stays plain — `/home/x/.local/bin/zstats` should read like
    /// the installer's.
    pub fn exec_arg(path: &Path) -> String {
        let raw = path.to_string_lossy();
        let reserved = |c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | '\''
                        | '\\'
                        | '>'
                        | '<'
                        | '~'
                        | '|'
                        | '&'
                        | ';'
                        | '$'
                        | '*'
                        | '?'
                        | '#'
                        | '('
                        | ')'
                        | '`'
                )
        };
        if !raw.chars().any(reserved) {
            return raw.into_owned();
        }
        let mut quoted = String::with_capacity(raw.len() + 2);
        quoted.push('"');
        for c in raw.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                quoted.push('\\');
            }
            quoted.push(c);
        }
        quoted.push('"');
        quoted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The entry has to be the login path and nothing else: an absolute
    /// `Exec` with no arguments, and no startup notification for a
    /// surface that never maps a toplevel. `install-linux.sh` writes the
    /// same lines; a drift between the two would be two different
    /// "launch at login"s.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_desktop_entry_is_the_bare_login_launch() {
        let entry = xdg::desktop_entry(std::path::Path::new("/home/x/.local/bin/zstats"));
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(
            entry.contains("\nExec=/home/x/.local/bin/zstats\n"),
            "{entry}"
        );
        assert!(
            !entry.contains("--toggle"),
            "a login launch must not open the panel"
        );
        assert!(entry.contains("\nStartupNotify=false\n"));
        assert!(entry.contains("\nType=Application\n"));
    }

    /// A path with a space is quoted per the spec; a plain one is not
    /// touched, so the common case reads like the installer's.
    #[cfg(target_os = "linux")]
    #[test]
    fn exec_is_quoted_only_when_the_spec_demands_it() {
        use std::path::Path;
        assert_eq!(
            xdg::exec_arg(Path::new("/opt/zstats/zstats")),
            "/opt/zstats/zstats"
        );
        assert_eq!(
            xdg::exec_arg(Path::new("/home/a b/zstats")),
            "\"/home/a b/zstats\""
        );
        assert_eq!(
            xdg::exec_arg(Path::new("/x/$y\"z")),
            "\"/x/\\$y\\\"z\"",
            "the four in-quote specials are escaped"
        );
    }

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
