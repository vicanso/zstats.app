//! What the volume figures cannot say: APFS purgeable space and local
//! Time Machine snapshots — the usual answers to "the sums don't add
//! up" and to a disk that is full while `~` is not.
//!
//! Apple's own figures only. Purgeable is the gap between the two
//! availability capacities NSURL reports — the important-usage one is
//! what Finder quotes — and snapshots come from
//! `tmutil listlocalsnapshots` (listing needs no privileges). A
//! one-shot, panel-owned OS query in procscan's class: display and
//! honest pointing, never action — the system thins snapshots on
//! demand, and forcing it is `tmutil`'s job behind admin rights, not
//! ours.

pub struct SpaceInfo {
    /// Bytes the system could free on demand (snapshots, regenerable
    /// caches…). `None` when the query failed.
    pub purgeable_bytes: Option<u64>,
    /// Local Time Machine snapshots currently on the boot volume.
    pub snapshots: usize,
}

/// Blocking (spawns `tmutil`) — call on the background executor.
pub fn probe() -> SpaceInfo {
    SpaceInfo {
        purgeable_bytes: purgeable(),
        snapshots: snapshot_count(),
    }
}

#[cfg(target_os = "macos")]
fn purgeable() -> Option<u64> {
    use objc2_foundation::{
        NSArray, NSNumber, NSString, NSURL, NSURLVolumeAvailableCapacityForImportantUsageKey,
        NSURLVolumeAvailableCapacityKey,
    };
    unsafe {
        let url = NSURL::fileURLWithPath(&NSString::from_str("/"));
        let keys = NSArray::from_slice(&[
            NSURLVolumeAvailableCapacityForImportantUsageKey,
            NSURLVolumeAvailableCapacityKey,
        ]);
        let values = url.resourceValuesForKeys_error(&keys).ok()?;
        let read = |key| {
            values
                .objectForKey(key)
                .and_then(|obj| obj.downcast::<NSNumber>().ok())
                .map(|n| n.longLongValue().max(0) as u64)
        };
        let important = read(NSURLVolumeAvailableCapacityForImportantUsageKey)?;
        let plain = read(NSURLVolumeAvailableCapacityKey)?;
        Some(important.saturating_sub(plain))
    }
}

#[cfg(not(target_os = "macos"))]
fn purgeable() -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
fn snapshot_count() -> usize {
    let Ok(out) = std::process::Command::new("tmutil")
        .args(["listlocalsnapshots", "/"])
        .output()
    else {
        return 0;
    };
    if !out.status.success() {
        return 0;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.starts_with("com.apple.TimeMachine"))
        .count()
}

#[cfg(not(target_os = "macos"))]
fn snapshot_count() -> usize {
    0
}
