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
//! A finished listing is kept in `~/.zstats/bigfiles.toml` so the next
//! one can mark what it added ([`Baseline`]). That file is a memory, not
//! a claim: it never changes what the query returns, and losing it costs
//! only the "new" marks.
//!
//! Deleting goes through `NSFileManager.trashItemAtURL` — Finder's own
//! move-to-Trash, recoverable until the Trash is emptied — never a direct
//! unlink. Same posture as `terminate.rs`: the panel delivers refusable,
//! reversible requests; it does not destroy.

use crate::opener;
use crate::prefs;
use std::cmp::Reverse;
use std::collections::HashSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

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

/// The previous listing, kept so a finished query can say which rows are
/// actually *new* rather than merely present.
///
/// Same shape of promise as the analyser's Δ baseline: absence from the
/// baseline is not by itself evidence of a new file. Two things can make
/// a row absent without it being new — the previous run queried a higher
/// bar (the threshold drops to [`FALLBACK_THRESHOLD`] only when the
/// primary query found almost nothing), or it listed [`SHOWN`] rows out
/// of more and cut this one off. [`Baseline::is_new`] refuses to claim
/// anything in either case.
pub struct Baseline {
    at: SystemTime,
    /// The bar that produced it — compared against LOGICAL size, which
    /// is what the index query filters on.
    threshold: u64,
    /// It had more hits than it listed, so it cannot speak for anything
    /// under [`Self::smallest`].
    truncated: bool,
    /// The smallest PHYSICAL size it listed — the truncation cut, since
    /// the list is ranked and cut by physical size.
    smallest: u64,
    paths: HashSet<PathBuf>,
}

impl Baseline {
    pub fn at(&self) -> SystemTime {
        self.at
    }

    /// Whether the previous listing would have shown this file and did
    /// not. False whenever the answer is "cannot tell".
    pub fn is_new(&self, file: &BigFile) -> bool {
        !self.paths.contains(&file.path)
            && file.logical >= self.threshold
            && (!self.truncated || file.size >= self.smallest)
    }
}

/// Bumped when the stored shape changes; a mismatch drops the file
/// rather than guessing, exactly like the analyser's cache.
const BASELINE_VERSION: i64 = 1;

fn baseline_path() -> PathBuf {
    zstats::settings::default_dir().join("bigfiles.toml")
}

/// Read the previous listing, or `None` on a first run / unreadable or
/// stale file. Never an error: comparison is a bonus on top of the
/// query, and a missing baseline only costs the "new" marks.
pub fn load_baseline() -> Option<Baseline> {
    load_baseline_at(&baseline_path())
}

fn load_baseline_at(path: &Path) -> Option<Baseline> {
    // `from_str`, not `str::parse::<Value>`: in toml 1.x the latter reads
    // one *value*, so a real document fails at its first key. Same call
    // the analyser's cache loader uses.
    let doc: toml::Value = toml::from_str(&fs::read_to_string(path).ok()?).ok()?;
    if doc.get("version").and_then(toml::Value::as_integer) != Some(BASELINE_VERSION) {
        return None;
    }
    let at = u64::try_from(doc.get("scanned_at")?.as_integer()?).ok()?;
    let threshold = u64::try_from(doc.get("threshold")?.as_integer()?).ok()?;
    let total = usize::try_from(doc.get("total")?.as_integer()?).ok()?;
    let rows = doc.get("file").and_then(toml::Value::as_array)?;
    let mut smallest = u64::MAX;
    let mut paths = HashSet::with_capacity(rows.len());
    for row in rows {
        let path = row.get("path")?.as_str()?;
        let size = u64::try_from(row.get("size")?.as_integer()?).ok()?;
        smallest = smallest.min(size);
        paths.insert(PathBuf::from(path));
    }
    Some(Baseline {
        at: SystemTime::UNIX_EPOCH + Duration::from_secs(at),
        threshold,
        truncated: total > paths.len(),
        smallest: if paths.is_empty() { 0 } else { smallest },
        paths,
    })
}

/// Replace the baseline with this listing. Called after every finished
/// query, so "new" always means "since the last time you looked" — the
/// caption carries that timestamp, because otherwise the word is a
/// claim nobody can check.
pub fn save_baseline(scan: &BigFilesScan) {
    save_baseline_at(&baseline_path(), scan);
}

fn save_baseline_at(path: &Path, scan: &BigFilesScan) {
    use toml::Value;
    use toml::value::Table;
    let clamp = |n: u64| Value::Integer(n.min(i64::MAX as u64) as i64);
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut doc = Table::new();
    doc.insert("version".into(), Value::Integer(BASELINE_VERSION));
    doc.insert("scanned_at".into(), clamp(now));
    doc.insert("threshold".into(), clamp(scan.threshold));
    doc.insert("total".into(), clamp(scan.total as u64));
    doc.insert(
        "file".into(),
        Value::Array(
            scan.files
                .iter()
                .map(|f| {
                    let mut row = Table::new();
                    row.insert("path".into(), Value::String(f.path.display().to_string()));
                    row.insert("size".into(), clamp(f.size));
                    Value::Table(row)
                })
                .collect(),
        ),
    );
    let Some(dir) = path.parent() else {
        return;
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = path.with_extension("toml.tmp");
    // `toml::to_string`, not `Value::to_string`: the latter emits the
    // whole document as one inline table, and this file is meant to be
    // readable when someone wonders what "new" was measured against.
    // Same call the analyser's cache uses.
    let Ok(text) = toml::to_string(&Value::Table(doc)) else {
        return;
    };
    if fs::write(&tmp, text).is_err() {
        return;
    }
    // 0600 like the analyser's cache: these paths leak project and file
    // names.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    if fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
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
    let home = env::var("HOME").map_err(|e| ScanError::Other(e.to_string()))?;
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
    files.sort_by_key(|f| Reverse(f.size));
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
    // The analyser's exclusion list applies here too: a directory the
    // reader has told the window to leave alone should not come back as
    // a row in the other half of the same window. Read once per query,
    // not per hit.
    let excluded = prefs::analysis_exclude();
    paths
        .into_iter()
        .filter(|path| !excluded.iter().any(|dir| path.starts_with(dir)))
        .filter_map(|path| {
            let meta = fs::symlink_metadata(&path).ok()?;
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
        .map(|()| {
            // The audit line for the app's other act on the system,
            // at the one delivery point both confirm-gated callers
            // share. Recoverable (it is Finder's own Trash), but a
            // move a person may want to trace is a move worth a line.
            tracing::info!(path, "moved to Trash");
        })
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
    if let Err(e) = opener::open([OsStr::new("-R"), path.as_os_str()]) {
        tracing::warn!("reveal {}: {e}", path.display());
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
                PathBuf::from(OsStr::from_bytes(s))
            }
            #[cfg(not(unix))]
            {
                PathBuf::from(String::from_utf8_lossy(s).into_owned())
            }
        })
        .collect()
}

fn physical_size(meta: &fs::Metadata) -> u64 {
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
    use std::process;

    fn file(path: &str, size: u64) -> BigFile {
        BigFile {
            path: PathBuf::from(path),
            size,
            logical: size,
        }
    }

    /// "New" is a claim about the previous run, so it may only be made
    /// where the previous run could have shown the file. Every case
    /// below is one where absence proves nothing.
    #[test]
    fn new_is_claimed_only_where_the_previous_listing_could_have_shown_it() {
        // Its own directory: cargo runs these in parallel threads of one
        // process, so a name shared with another test means one of them
        // deletes the other's files mid-run.
        let dir = env::temp_dir().join(format!("zstats-bigbaseline-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("bigfiles.toml");

        // No file at all: a first run marks nothing.
        assert!(load_baseline_at(&path).is_none());

        // A complete listing at the primary bar: three files, none cut.
        save_baseline_at(
            &path,
            &BigFilesScan {
                files: vec![
                    file("/u/a.mkv", 3_000_000_000),
                    file("/u/b.iso", 900_000_000),
                    file("/u/c.zip", 600_000_000),
                ],
                threshold: PRIMARY_THRESHOLD,
                total: 3,
            },
        );
        let base = load_baseline_at(&path).expect("round-trips");
        assert!(!base.is_new(&file("/u/a.mkv", 3_000_000_000)), "was listed");
        assert!(
            base.is_new(&file("/u/new.mkv", 800_000_000)),
            "over its bar"
        );
        // The bar dropped to the fallback since, so this one was invisible
        // to the run above — absent, but not new.
        assert!(
            !base.is_new(&file("/u/small.mp4", FALLBACK_THRESHOLD)),
            "under the previous run's threshold"
        );

        // A truncated listing cannot speak for anything under its cut.
        save_baseline_at(
            &path,
            &BigFilesScan {
                files: vec![
                    file("/u/a.mkv", 3_000_000_000),
                    file("/u/b.iso", 2_000_000_000),
                ],
                threshold: PRIMARY_THRESHOLD,
                total: 40,
            },
        );
        let cut = load_baseline_at(&path).expect("round-trips");
        assert!(
            !cut.is_new(&file("/u/mid.zip", 1_000_000_000)),
            "over the bar but under the cut of a truncated listing"
        );
        assert!(
            cut.is_new(&file("/u/huge.dmg", 4_000_000_000)),
            "above the cut, so it would have been listed"
        );

        let _ = fs::remove_dir_all(&dir);
    }

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
        let dir = env::temp_dir().join(format!("zstats-bigcollect-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small");
        let big = dir.join("big");
        let gone = dir.join("gone");
        fs::write(&small, vec![0u8; 1_000]).unwrap();
        fs::write(&big, vec![0u8; 64_000]).unwrap();

        // A stale index hands back all three; only the one still over the
        // bar survives — vanished and shrunk-below entries both drop.
        let kept = collect(vec![small, big.clone(), gone], 32_000);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path, big);
        assert_eq!(kept[0].logical, 64_000);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn physical_size_counts_blocks() {
        let dir = env::temp_dir().join(format!("zstats-bigfiles-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("blob");
        fs::write(&file, vec![7u8; 1_000_000]).unwrap();
        let meta = fs::metadata(&file).unwrap();
        let physical = physical_size(&meta);
        // A written-out megabyte occupies real blocks; exact figures vary
        // by filesystem, so bracket rather than pin.
        assert!(
            physical >= 500_000,
            "physical {physical} suspiciously small"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
