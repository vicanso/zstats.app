//! What Finder does for the disk-space window, and nothing it decides:
//! move a file or a folder to the Trash, show one selected in Finder,
//! open the Trash. Its three tabs' rows — the analyser's caches and big
//! files, the duplicate copies — all end here.
//!
//! Moving goes through `NSFileManager.trashItemAtURL` — Finder's own
//! move-to-Trash, recoverable until the Trash is emptied — never a
//! direct unlink. Same posture as `terminate.rs`: the panel delivers
//! refusable, reversible requests; it does not destroy. Emptying the
//! Trash stays the reader's own act; [`open_trash`] only opens it.
//!
//! This was `bigfiles.rs`, which also asked Spotlight for every file
//! over 500 MB in the home folder, for a tab of its own. That tab is
//! gone (2026-10-16): the analyser's walk sees every file, and its card
//! lists the big ones ([`crate::diskscan::BIG_FILE_IN_TREE`]).
//!
//! **Linux**: trashing is `gio trash`, GLib's own implementation of the
//! freedesktop Trash spec — it handles the per-volume `.Trash-$uid`
//! directories a hand-rolled rename would get wrong. Reveal is the file
//! manager's `ShowItems` over D-Bus, the one call that *selects* the
//! file the way `open -R` does, with `xdg-open` on the parent directory
//! as the fallback when no file manager answers.

use crate::opener;
#[cfg(target_os = "macos")]
use std::env;
#[cfg(target_os = "macos")]
use std::ffi::OsStr;
use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::Command;

/// Move a file to the Trash. Fast regardless of size — on the same volume
/// this is a rename into `.Trash`, not a copy.
#[cfg(target_os = "macos")]
pub fn trash(path: &Path) -> Result<(), String> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let Some(path) = path.to_str() else {
        return Err("path is not valid UTF-8".into());
    };
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map(|()| {
            // The audit line for the app's other act on the system,
            // at the one delivery point both confirm-gated callers
            // share. Recoverable (it is Finder's own Trash), but a
            // move a person may want to trace is a move worth a line.
            tracing::info!(path, "moved to Trash");
        })
        .map_err(|e| e.to_string())
}

/// Move a file to the Trash through `gio trash` — GLib's implementation
/// of the freedesktop Trash spec, and the same thing the desktop's file
/// manager does. Recoverable from the Trash until it is emptied; never a
/// direct unlink. A machine without `gio` (no GLib at all — rare on a
/// desktop) gets an error that names it, and the row stays.
#[cfg(target_os = "linux")]
pub fn trash(path: &Path) -> Result<(), String> {
    let out = Command::new("gio")
        .arg("trash")
        .arg("--")
        .arg(path.as_os_str())
        .output()
        .map_err(|e| {
            format!("gio trash: {e} — GLib's `gio` is what moves files to the Trash here")
        })?;
    if !out.status.success() {
        return Err(format!(
            "gio trash: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // Same audit line as the macOS delivery point, for the same reason.
    tracing::info!(path = %path.display(), "moved to Trash");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn trash(_path: &Path) -> Result<(), String> {
    Err("trash is not implemented on this platform".into())
}

/// Show the file in Finder, selected (`open -R`). Navigation rather than
/// an action on the system — nothing on disk changes — so no confirm gate.
#[cfg(target_os = "macos")]
pub fn reveal(path: &Path) {
    if let Err(e) = opener::open([OsStr::new("-R"), path.as_os_str()]) {
        tracing::warn!("reveal {}: {e}", path.display());
    }
}

/// Show the file in the file manager, selected — `ShowItems` on
/// `org.freedesktop.FileManager1`, which Nautilus, Dolphin, Thunar and
/// Nemo all implement; `xdg-open` on the parent directory when nothing
/// answers. Off the UI thread: the D-Bus call may have to *activate*
/// the file manager, and a second of that on the main thread is a
/// frozen panel.
#[cfg(target_os = "linux")]
pub fn reveal(path: &Path) {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        if let Err(e) = show_in_file_manager(&path) {
            tracing::debug!("FileManager1.ShowItems: {e}; opening the folder instead");
            let parent = path.parent().unwrap_or(&path);
            if let Err(e) = opener::open([parent.as_os_str()]) {
                tracing::warn!("reveal {}: {e}", path.display());
            }
        }
    });
}

#[cfg(target_os = "linux")]
fn show_in_file_manager(path: &Path) -> zbus::Result<()> {
    let connection = zbus::blocking::Connection::session()?;
    let manager = zbus::blocking::Proxy::new(
        &connection,
        "org.freedesktop.FileManager1",
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
    )?;
    manager.call("ShowItems", &(vec![file_uri(path)], ""))
}

/// `file://` plus the path, percent-encoded byte by byte — a path is
/// bytes on Linux, not UTF-8, and the spec's unreserved set is the only
/// thing that passes through bare.
#[cfg(target_os = "linux")]
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(byte as char);
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn reveal(_path: &Path) {}

/// Open the Trash in the file manager — where emptying it is the user's
/// own act. Nothing here empties it: that is the one irreversible step,
/// and this app's touches are recoverable by rule.
#[cfg(target_os = "macos")]
pub fn open_trash() {
    let Some(home) = env::var_os("HOME") else {
        return;
    };
    let trash = Path::new(&home).join(".Trash");
    if let Err(e) = opener::open([trash.as_os_str()]) {
        tracing::warn!("open Trash: {e}");
    }
}

/// The freedesktop Trash, through GIO's own scheme — what `gio trash`
/// (`trash` above) moved things into.
#[cfg(target_os = "linux")]
pub fn open_trash() {
    if let Err(e) = opener::open(["trash:///"]) {
        tracing::warn!("open Trash: {e}");
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn open_trash() {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Paths are bytes; the URI the file manager gets must survive a
    /// space, a non-ASCII name and a `%` without ambiguity.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_path_becomes_a_file_uri_byte_for_byte() {
        assert_eq!(file_uri(Path::new("/home/x/a b")), "file:///home/x/a%20b");
        assert_eq!(file_uri(Path::new("/tmp/50%")), "file:///tmp/50%25");
        assert_eq!(file_uri(Path::new("/tmp/é")), "file:///tmp/%C3%A9");
        assert_eq!(
            file_uri(Path::new("/tmp/a-b.c_d~e")),
            "file:///tmp/a-b.c_d~e"
        );
    }
}
