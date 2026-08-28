//! Volume flags the snapshot does not carry.
//!
//! zstats' disk rule fires on used%. A read-only extra volume is full
//! by construction — a DMG is sized to its contents, so 99% / 0 GB
//! free is the packaging, not a disk running out, and nothing the
//! user can do will free space on a volume the kernel has marked
//! `MNT_RDONLY`. sysinfo already knows the flag; `DiskSnapshot` does
//! not forward it, so the engine cannot skip these.
//!
//! Only `/Volumes/…` is eligible. On Apple Silicon `/` itself is
//! read-only (the sealed system volume); skipping every `MNT_RDONLY`
//! mount would swallow the boot-disk alert, which is the one that
//! matters. Fail-open: a mount `statfs` cannot read still alerts.

use std::ffi::CString;
use std::mem::MaybeUninit;

/// Whether a disk alert for this mount should be dropped at ingest.
///
/// True only for a **read-only extra volume** — `/Volumes/…` with
/// `MNT_RDONLY`. Writable USB disks, and every system path (`/`,
/// `/System/Volumes/Data`), still fire.
pub fn skips_disk_alert(mount: &str) -> bool {
    extra_volume(mount).is_some_and(is_read_only)
}

/// The `/Volumes/Name` form Finder uses for anything that is not the
/// boot disk. Trailing slashes stripped so `/Volumes/Foo/` still
/// qualifies; `/Volumes` itself is a directory on the data volume,
/// not a mount of one.
fn extra_volume(mount: &str) -> Option<&str> {
    let mount = mount.trim_end_matches('/');
    mount
        .starts_with("/Volumes/")
        .then_some(mount)
        .filter(|m| m.len() > "/Volumes/".len())
}

fn is_read_only(mount: &str) -> bool {
    let Ok(c_path) = CString::new(mount) else {
        return false;
    };
    let mut buf = MaybeUninit::<libc::statfs>::uninit();
    let rc = unsafe { libc::statfs(c_path.as_ptr(), buf.as_mut_ptr()) };
    if rc != 0 {
        return false;
    }
    let buf = unsafe { buf.assume_init() };
    (buf.f_flags & libc::MNT_RDONLY as u32) != 0
}

#[cfg(test)]
mod tests {
    use super::{extra_volume, skips_disk_alert};

    #[test]
    fn only_named_volumes_entries_qualify() {
        assert_eq!(
            extra_volume("/Volumes/Recordly 1.3.3"),
            Some("/Volumes/Recordly 1.3.3")
        );
        assert_eq!(extra_volume("/Volumes/Foo/"), Some("/Volumes/Foo"));
        assert!(extra_volume("/").is_none());
        assert!(extra_volume("/System/Volumes/Data").is_none());
        assert!(extra_volume("/Volumes").is_none());
        assert!(extra_volume("/Volumes/").is_none());
    }

    #[test]
    fn the_boot_volume_is_never_exempted() {
        // `/` is MNT_RDONLY on a sealed Apple-silicon system volume.
        // The predicate must not care.
        assert!(!skips_disk_alert("/"));
        assert!(!skips_disk_alert("/System/Volumes/Data"));
    }

    #[test]
    fn a_path_statfs_cannot_read_is_not_exempted() {
        assert!(!skips_disk_alert("/Volumes/no-such-volume"));
        assert!(!skips_disk_alert("not\0a path"));
    }
}
