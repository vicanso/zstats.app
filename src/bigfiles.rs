//! One-shot large-file query over the Spotlight index, plus the trash
//! operation behind the Hardware tab's delete button.
//!
//! Spotlight (`mdfind`) rather than a walk: the index already knows every
//! visible file's size, so "which big files" is a metadata query answered
//! in milliseconds, not a filesystem crawl. The blind spots are accepted
//! scope, not bugs — hidden dot-paths and most of `~/Library` never enter
//! the index, and a directory of a million small files has no single big
//! file to find. Those belong to the directory analyser (plan step two);
//! this feature owns the big obvious wins: videos, images, archives,
//! forgotten installers.
//!
//! Deleting goes through `NSFileManager.trashItemAtURL` — Finder's own
//! move-to-Trash, recoverable until the Trash is emptied — never a direct
//! unlink. Same posture as `terminate.rs`: the panel delivers refusable,
//! reversible requests; it does not destroy.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Primary query bar: what most people mean by "a big file". Compared
/// against LOGICAL size (all Spotlight has) and re-verified after stat.
/// The caption deliberately does not quote this figure — it states the
/// floored minimum of what the rows actually display (see
/// `disk::display_bar`), because sparse files enter on logical size and
/// show far smaller physical numbers.
pub const PRIMARY_THRESHOLD: u64 = 500 * 1024 * 1024;
/// When the primary query returns almost nothing the bar drops here — an
/// empty card reads as "the feature is broken", a handful of 100 MB files
/// reads as an answer.
pub const FALLBACK_THRESHOLD: u64 = 100 * 1024 * 1024;
/// Fewer primary hits than this triggers the fallback query.
pub const FALLBACK_BELOW: usize = 5;
/// Rows the card keeps; the caption still reports the full count.
pub const SHOWN: usize = 20;

pub struct BigFile {
    pub path: PathBuf,
    /// Physical bytes on disk (`st_blocks`) — the same coin the volume
    /// gauges above the card count, not the logical length.
    pub size: u64,
    /// Logical length. The query threshold compares against this (it is
    /// all Spotlight has), so a sparse or compressed file can sit in the
    /// "≥ 500 MB" list while its physical row reads far smaller — the two
    /// figures surface together whenever they disagree, or the list looks
    /// like it broke its own bar.
    pub logical: u64,
}

pub struct BigFilesScan {
    /// Largest first, capped at [`SHOWN`].
    pub files: Vec<BigFile>,
    /// The threshold that actually produced this list.
    pub threshold: u64,
    /// How many the query found before the cap.
    pub total: usize,
}

pub enum ScanError {
    /// Spotlight indexing is off — the query cannot work at all, and
    /// saying so beats showing a false "no big files".
    IndexingOff,
    Other(String),
}

/// Query `$HOME` for big files. Blocking (two subprocess round-trips plus
/// a stat per hit) — callers run it on the background executor.
pub fn scan() -> Result<BigFilesScan, ScanError> {
    let home = std::env::var("HOME").map_err(|e| ScanError::Other(e.to_string()))?;
    if !indexing_enabled() {
        return Err(ScanError::IndexingOff);
    }

    let mut threshold = PRIMARY_THRESHOLD;
    let mut files = collect(query(&home, threshold)?, threshold);
    // Decided on the SURVIVING count, not raw index hits: the index can
    // hold vanished files and sizes that shrank since indexing, and a
    // caption promising five entries over a lie is the bug this exists
    // to avoid.
    if files.len() < FALLBACK_BELOW {
        threshold = FALLBACK_THRESHOLD;
        files = collect(query(&home, threshold)?, threshold);
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.size));
    let total = files.len();
    files.truncate(SHOWN);
    Ok(BigFilesScan {
        files,
        threshold,
        total,
    })
}

/// Stat each hit and keep only what still honours the bar. The index is a
/// tip, not a fact: files vanish, and a file truncated since indexing can
/// sit in the result set at a fraction of the queried size — re-checking
/// `len()` against the threshold is what lets the caption say "≥ 500 MB"
/// and be telling the truth about every row under it.
fn collect(paths: Vec<PathBuf>, threshold: u64) -> Vec<BigFile> {
    paths
        .into_iter()
        .filter_map(|path| {
            let meta = std::fs::symlink_metadata(&path).ok()?;
            if !meta.is_file() || meta.len() < threshold {
                return None;
            }
            Some(BigFile {
                size: physical_size(&meta),
                logical: meta.len(),
                path,
            })
        })
        .collect()
}

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
        .map_err(|e| e.to_string())
}

/// Never-run stub — see "Platform reality" in CLAUDE.md.
#[cfg(not(target_os = "macos"))]
pub fn trash(_path: &Path) -> Result<(), String> {
    Err("trash is macOS-only".into())
}

/// Show the file in Finder, selected (`open -R`). Navigation rather than
/// an action on the system — nothing on disk changes — so no confirm gate.
#[cfg(target_os = "macos")]
pub fn reveal(path: &Path) {
    if let Err(e) = Command::new("open").arg("-R").arg(path).spawn() {
        eprintln!("reveal {}: {e}", path.display());
    }
}

/// Never-run stub — see "Platform reality" in CLAUDE.md.
#[cfg(not(target_os = "macos"))]
pub fn reveal(_path: &Path) {}

fn query(root: &str, threshold: u64) -> Result<Vec<PathBuf>, ScanError> {
    let out = Command::new("mdfind")
        .arg("-onlyin")
        .arg(root)
        .arg("-0")
        .arg(format!("kMDItemFSSize > {threshold}"))
        .output()
        .map_err(|e| ScanError::Other(format!("mdfind: {e}")))?;
    if !out.status.success() {
        return Err(ScanError::Other(format!(
            "mdfind: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(split_null(&out.stdout))
}

/// `mdfind -0` terminates every path with NUL, the last one included.
fn split_null(bytes: &[u8]) -> Vec<PathBuf> {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                PathBuf::from(std::ffi::OsStr::from_bytes(s))
            }
            #[cfg(not(unix))]
            {
                PathBuf::from(String::from_utf8_lossy(s).into_owned())
            }
        })
        .collect()
}

fn physical_size(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // st_blocks is always in 512-byte units, whatever the FS block size.
        meta.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// Whether Spotlight is indexing the boot volume. `mdfind` against an
/// unindexed volume returns an empty result set with a zero exit code —
/// indistinguishable from "no big files", which is exactly the lie this
/// check exists to prevent.
fn indexing_enabled() -> bool {
    Command::new("mdutil")
        .arg("-s")
        .arg("/")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains("enabled"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_null_handles_trailing_terminator_and_empty() {
        assert_eq!(
            split_null(b"/a/b\0/c d\0"),
            vec![PathBuf::from("/a/b"), PathBuf::from("/c d")]
        );
        assert!(split_null(b"").is_empty());
        assert!(split_null(b"\0\0").is_empty());
    }

    #[test]
    fn collect_reverifies_against_the_bar() {
        let dir = std::env::temp_dir().join(format!("zstats-bigcollect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small");
        let big = dir.join("big");
        let gone = dir.join("gone");
        std::fs::write(&small, vec![0u8; 1_000]).unwrap();
        std::fs::write(&big, vec![0u8; 64_000]).unwrap();

        // A stale index hands back all three; only the one still over the
        // bar survives — vanished and shrunk-below entries both drop.
        let kept = collect(vec![small, big.clone(), gone], 32_000);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path, big);
        assert_eq!(kept[0].logical, 64_000);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn physical_size_counts_blocks() {
        let dir = std::env::temp_dir().join(format!("zstats-bigfiles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("blob");
        std::fs::write(&file, vec![7u8; 1_000_000]).unwrap();
        let meta = std::fs::metadata(&file).unwrap();
        let physical = physical_size(&meta);
        // A written-out megabyte occupies real blocks; exact figures vary
        // by filesystem, so bracket rather than pin.
        assert!(
            physical >= 500_000,
            "physical {physical} suspiciously small"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
