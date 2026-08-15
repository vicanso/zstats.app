//! One-shot directory analyser — docs/disk-analysis.md, P2a.
//!
//! A parallel walk (jwalk, dedicated pool of ≤3 threads capped at half
//! the cores; both the readdir batches and the per-file stat run on the
//! workers) over a root the
//! Spotlight index cannot see into, aggregated into three rankings:
//! regenerable caches (signature-checked `CACHEDIR.TAG`), big directories
//! (dominance-filtered so the table is not one nested chain), and files
//! the walk alone can see. No tree is retained — the working set is one
//! counter per directory, which is why a million-inode `~/Library` fits
//! a menu-bar panel.
//!
//! The whole run lives on its own thread; progress and the final
//! `Done`/`Failed` arrive over a channel — completion is never signalled
//! through shared state. Cancellation is a flag the walk polls; a
//! cancelled run says nothing at all (the UI has already moved on).

use jwalk::{Parallelism, WalkDirGeneric};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Files worth a row of their own; everything smaller folds into its
/// directory's total.
pub const BIG_FILE_IN_TREE: u64 = 50 * 1024 * 1024;
/// A directory whose single child holds at least this share is
/// represented by that child — chased to the first real fork.
const DOMINANCE_PERCENT: u64 = 90;
/// Rows kept per table. Three tables share a 320px card; the tail is
/// noise the skip counters still account for.
pub const TABLE_CAP: usize = 8;
/// The big-directory and big-file tables may grow past `TABLE_CAP` up to
/// this, but only while every extra row still exceeds `TABLE_EXTEND_MIN`
/// — a tail that heavy is exactly what the reader came for; a lighter
/// one is the noise the cap exists to cut. The regenerable table stays
/// at `TABLE_CAP`.
const TABLE_CAP_EXTENDED: usize = 10;
/// Binary 500 MB, the same convention as `bigfiles`' thresholds.
const TABLE_EXTEND_MIN: u64 = 500 * 1024 * 1024;
/// Directories below this never reach any table, so the retained index
/// drops them: ~10 MiB keeps a home tree's index at a few thousand
/// entries (low MBs resident) instead of the walk's tens of MB.
const INDEX_FLOOR: u64 = 10 * 1024 * 1024;
/// The spec's fixed signature line — content-checked so a stray file
/// merely *named* CACHEDIR.TAG cannot mark a tree regenerable.
const CACHE_TAG_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";
/// Inode-pathological suffixes (hundreds of thousands of small files).
/// Deliberately NOT interesting cache roots — `Library/Caches` needs
/// per-child breakdown, not folding. Matched as trailing components.
const PATHOLOGICAL: &[&str] = &["node_modules", ".git", ".pnpm", "DerivedData", "Pods"];
/// Bundle extensions folded into one file-like leaf, so a photo library
/// reads as "one 80 GB item" instead of vanishing into per-file noise.
const BUNDLES: &[&str] = &[
    "app",
    "photoslibrary",
    "musiclibrary",
    "tvlibrary",
    "sparsebundle",
];
/// TCC-protected subtrees under ~/Library, pruned WITHOUT touching them:
/// a read attempt makes macOS pop "zstats wants to access data from
/// Mail" — the dialog would not be ours, but the user sees a dialog.
const TCC_DENY: &[&str] = &[
    "Mail",
    "Messages",
    "Safari",
    "HomeKit",
    "Cookies",
    "Suggestions",
];
/// `chflags` dataless bit (sys/stat.h) — an iCloud / file-provider
/// placeholder whose content is not local. Reading one triggers a
/// download, which is worse than a slow scan. libc does not export it.
#[cfg(target_os = "macos")]
const UF_DATALESS: u32 = 0x4000_0000;
/// How often progress lands on the channel.
const PROGRESS_EVERY: Duration = Duration::from_millis(500);
/// How often a partial table snapshot lands. Costlier than the counter —
/// each one clones the aggregates and runs the full ranking — so it gets
/// its own, slower throttle; still cheap in absolute terms (a rollup over
/// ~100k dirs is milliseconds).
const PARTIAL_EVERY: Duration = Duration::from_secs(2);
/// Dedicated rayon pool size: 3 threads, but never more than half the
/// machine's cores (floor 1). The cap exists because the resident
/// collector samples disk IO on its 2s tick, and a default-parallelism
/// walk reads as "machine is copying a disk"; the half-the-cores bound
/// keeps small machines breathable. Note the walk is syscall-bound, not
/// compute-bound: these threads spend most of their time blocked in the
/// kernel, so the process's CPU% stays low by nature.
fn walk_threads() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    (cores / 2).clamp(1, 3)
}

/// Per-file metadata is fetched on the worker pool (`process_read_dir`
/// runs there), not in the consumer loop — the per-file stat is the
/// walk's most numerous syscall, and doing it serially on the consumer
/// thread would idle the pool.
type Walk = WalkDirGeneric<((), Option<std::fs::Metadata>)>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HitKind {
    /// Signature-checked CACHEDIR.TAG — the owner declared it regenerable.
    Tag,
    /// Suffix heuristic (node_modules and friends) — a guess, labelled so.
    Heuristic,
    Plain,
}

#[derive(Clone)]
pub struct DirHit {
    pub path: PathBuf,
    pub bytes: u64,
    /// Read by nothing yet: the Tag/Heuristic row labels are P2c scope
    /// (docs/disk-analysis.md), but the classification happens during the
    /// walk, so the field is carried from day one.
    #[allow(dead_code)]
    pub kind: HitKind,
}

#[derive(Clone)]
pub struct FileHit {
    pub path: PathBuf,
    pub bytes: u64,
}

pub struct ScanResult {
    pub root: PathBuf,
    pub scanned_at: Instant,
    /// Wall clock from walk start to this snapshot — on `Done`, the
    /// whole run's cost, shown so the user can decide whether a re-scan
    /// is a "wait for it" or a "come back later".
    pub took: Duration,
    pub dirs_seen: usize,
    pub skipped_denied: usize,
    pub skipped_protected: usize,
    pub skipped_dataless: usize,
    /// CACHEDIR.TAG trees, largest first.
    pub regenerable: Vec<DirHit>,
    /// The root's level-1 breakdown, each entry chased through dominant
    /// chains to its first fork.
    pub dirs: Vec<DirHit>,
    /// Walk-only big files (Spotlight blind spots), largest first.
    pub files: Vec<FileHit>,
    /// The one-click cleanup set: every signature-checked CACHEDIR.TAG
    /// tree plus every hint-trashable cache directory, nested entries
    /// folded into their listed ancestor, largest first. The FULL set —
    /// the bulk clear acts on all of it, so it is deliberately uncapped
    /// (the view shows the head and states the total). Empty on partial
    /// snapshots.
    pub suggestions: Vec<DirHit>,
    /// The pruned totals the drill-downs are served from. `Some` only on
    /// a finished scan (partials skip the cost); drill-derived results
    /// share their parent's index, so deeper levels stay instant.
    pub index: Option<Arc<DirIndex>>,
}

/// What survives a finished scan for instant drill-downs: subtree totals
/// for every directory that cleared `INDEX_FLOOR` (plus all fold roots),
/// the fold classifications, and the full blind-spot file list. A few
/// thousand entries — low MBs — deliberately not the tree; folded
/// interiors were never recorded and still take a live walk.
pub struct DirIndex {
    totals: HashMap<PathBuf, u64>,
    fold: HashMap<PathBuf, HitKind>,
    files: Vec<FileHit>,
}

pub enum ScanEvent {
    Progress {
        dirs_done: usize,
    },
    /// A snapshot of the tables as aggregated SO FAR — every figure is a
    /// lower bound that only grows, and the ranking reshuffles as data
    /// lands. The UI shows these under the running banner so a
    /// minutes-long walk pays out from its first seconds.
    Partial(Box<ScanResult>),
    Done(Box<ScanResult>),
    Failed(String),
}

/// Launch the analysis on its own thread. Everything the caller learns —
/// progress, completion, failure — arrives over `tx`; a cancelled run
/// simply goes quiet.
pub fn spawn(root: PathBuf, cancel: Arc<AtomicBool>, tx: smol::channel::Sender<ScanEvent>) {
    std::thread::spawn(move || match run(&root, &cancel, &tx) {
        Ok(Some(result)) => {
            let _ = tx.send_blocking(ScanEvent::Done(Box::new(result)));
        }
        Ok(None) => {} // cancelled
        Err(e) => {
            let _ = tx.send_blocking(ScanEvent::Failed(e));
        }
    });
}

/// The default root: the whole home directory. With the leaf rules in
/// place (node_modules / .git / bundle folding, TCC deep data pruned)
/// this stays a minutes-scale walk, and it answers "where did MY disk
/// go" in one picture — visible directories included, which the
/// Spotlight big-file query alone cannot rank. First run may surface
/// macOS's one-time Desktop/Documents/Downloads consent prompts: those
/// folders ARE the point of a home-wide analysis, so they are walked,
/// not pruned — unlike Mail and friends, whose prompts would read as
/// snooping.
pub fn default_root() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

fn run(
    root: &Path,
    cancel: &Arc<AtomicBool>,
    tx: &smol::channel::Sender<ScanEvent>,
) -> Result<Option<ScanResult>, String> {
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }
    let protected = Arc::new(AtomicUsize::new(0));

    let walk = {
        let deny: Vec<PathBuf> = std::env::var("HOME")
            .ok()
            .map(|h| {
                TCC_DENY
                    .iter()
                    .map(|s| Path::new(&h).join("Library").join(s))
                    .collect()
            })
            .unwrap_or_default();
        let protected = protected.clone();
        let cancelled = cancel.clone();
        Walk::new(root)
            .follow_links(false)
            .skip_hidden(false)
            .parallelism(Parallelism::RayonNewPool(walk_threads()))
            .process_read_dir(move |_depth, _path, _state, children| {
                if cancelled.load(Ordering::Relaxed) {
                    // Stop expanding: without this the workers keep
                    // stat'ing ahead while the consumer is bailing out.
                    children.clear();
                    return;
                }
                for child in children.iter_mut().flatten() {
                    if child.file_type.is_dir() {
                        if deny.iter().any(|d| child.path() == *d) {
                            // Pruned, not visited: the whole point is that
                            // no syscall ever lands inside.
                            child.read_children_path = None;
                            protected.fetch_add(1, Ordering::Relaxed);
                        }
                    } else if child.file_type.is_file() {
                        // On the worker pool by design — see `Walk`.
                        child.client_state = std::fs::symlink_metadata(child.path()).ok();
                    }
                }
            })
    };

    // Raw collection: one bytes counter per owning directory, the fold
    // map, and the candidates for the file table. No tree.
    let mut own_bytes: HashMap<PathBuf, u64> = HashMap::new();
    let mut plain_dirs: Vec<PathBuf> = Vec::new();
    let mut fold: HashMap<PathBuf, HitKind> = HashMap::new();
    let mut files: Vec<FileHit> = Vec::new();
    let mut denied = 0usize;
    let mut dataless = 0usize;
    let mut dirs_done = 0usize;
    let started = Instant::now();
    let mut last_progress = started;
    let mut last_partial = started;

    for entry in walk {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let mut entry = match entry {
            Ok(e) => e,
            Err(_) => {
                denied += 1;
                continue;
            }
        };
        let path = entry.path();
        if entry.file_type.is_dir() {
            dirs_done += 1;
            if last_progress.elapsed() >= PROGRESS_EVERY {
                last_progress = Instant::now();
                let _ = tx.try_send(ScanEvent::Progress { dirs_done });
            }
            if last_partial.elapsed() >= PARTIAL_EVERY {
                last_partial = Instant::now();
                let partial = snapshot(
                    root,
                    started.elapsed(),
                    own_bytes.clone(),
                    &plain_dirs,
                    &fold,
                    files.clone(),
                    dirs_done,
                    denied,
                    protected.load(Ordering::Relaxed),
                    dataless,
                    false,
                );
                let _ = tx.try_send(ScanEvent::Partial(Box::new(partial)));
            }
            if path == root || fold_owner(&fold, &path, root).is_some() {
                continue; // no structure recorded beneath a folded tree
            }
            // depth 1 = the root's direct children; the heuristic fold is
            // suspended there so the level-1 breakdown cannot swallow
            // itself (Tag keeps its meaning at any depth).
            match classify(&path, entry.depth <= 1) {
                Some(kind) => {
                    fold.insert(path, kind);
                }
                None => plain_dirs.push(path),
            }
            continue;
        }
        if !entry.file_type.is_file() {
            continue; // symlinks are never descended or counted
        }
        let meta = match entry.client_state.take() {
            Some(m) => m,
            None => {
                denied += 1;
                continue;
            }
        };
        if is_dataless(&meta) {
            dataless += 1;
            continue;
        }
        let bytes = physical_size(&meta);
        let folded = fold_owner(&fold, &path, root);
        let owner = folded
            .clone()
            .or_else(|| path.parent().map(Path::to_path_buf));
        if let Some(owner) = owner {
            *own_bytes.entry(owner).or_default() += bytes;
        }
        if folded.is_none() && bytes >= BIG_FILE_IN_TREE && in_blind_spot(&path) {
            files.push(FileHit { path, bytes });
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }

    Ok(Some(snapshot(
        root,
        started.elapsed(),
        own_bytes,
        &plain_dirs,
        &fold,
        files,
        dirs_done,
        denied,
        protected.load(Ordering::Relaxed),
        dataless,
        true,
    )))
}

/// Assemble a `ScanResult` from the aggregates as they stand — used for
/// the final result and, on a clone of the aggregates, for the partial
/// snapshots streamed mid-walk. One builder, so the two can never drift.
#[allow(clippy::too_many_arguments)]
fn snapshot(
    root: &Path,
    took: Duration,
    own_bytes: HashMap<PathBuf, u64>,
    plain_dirs: &[PathBuf],
    fold: &HashMap<PathBuf, HitKind>,
    mut files: Vec<FileHit>,
    dirs_seen: usize,
    skipped_denied: usize,
    skipped_protected: usize,
    skipped_dataless: usize,
    build_index: bool,
) -> ScanResult {
    let (totals, children) = rollup(root, own_bytes, plain_dirs, fold);
    let (regenerable, dirs) = tables(root, &totals, &children, fold);
    files.sort_by_key(|f| std::cmp::Reverse(f.bytes));
    // Suggestions only on the finished result: a partial's lower-bound
    // set would invite trashing while the walker is inside the trees.
    let suggestions = if build_index {
        suggest(&totals, fold, &|p| {
            crate::cleanhints::lookup(p).is_some_and(|h| h.trashable)
        })
    } else {
        Vec::new()
    };
    // The index keeps the FULL blind-spot file list; the table cap below
    // only trims what one card can show.
    let index = build_index.then(|| {
        Arc::new(DirIndex {
            totals: totals
                .into_iter()
                .filter(|(p, b)| *b >= INDEX_FLOOR || fold.contains_key(p))
                .collect(),
            fold: fold.clone(),
            files: files.clone(),
        })
    });
    cap_table(&mut files, |f| f.bytes);
    ScanResult {
        root: root.to_path_buf(),
        scanned_at: Instant::now(),
        took,
        dirs_seen,
        skipped_denied,
        skipped_protected,
        skipped_dataless,
        regenerable,
        dirs,
        files,
        suggestions,
        index,
    }
}

/// Build the cleanup-suggestion set from the rolled-up totals: TAG folds
/// carry the owner's own declaration, `trashable` hint matches carry
/// ours. Pure — the trashable check is injected so tests need no hint
/// file. Nested picks fold into their listed ancestor: trashing the
/// ancestor takes the descendant with it, and keeping both would leave
/// a ghost row after the bulk clear.
fn suggest(
    totals: &HashMap<PathBuf, u64>,
    fold: &HashMap<PathBuf, HitKind>,
    trashable: &dyn Fn(&Path) -> bool,
) -> Vec<DirHit> {
    let mut picks: Vec<DirHit> = fold
        .iter()
        .filter(|(_, kind)| **kind == HitKind::Tag)
        .map(|(path, _)| DirHit {
            bytes: totals.get(path).copied().unwrap_or(0),
            path: path.clone(),
            kind: HitKind::Tag,
        })
        .collect();
    for (path, bytes) in totals {
        if !fold.contains_key(path) && trashable(path) {
            picks.push(DirHit {
                path: path.clone(),
                bytes: *bytes,
                kind: HitKind::Plain,
            });
        }
    }
    picks.sort_by_key(|d| std::cmp::Reverse(d.bytes));
    let mut kept: Vec<DirHit> = Vec::new();
    for hit in picks {
        if !kept.iter().any(|k| hit.path.starts_with(&k.path)) {
            kept.push(hit);
        }
    }
    kept
}

/// Serve a drill-down from a finished scan's retained index — instant,
/// no I/O, same `tables()` as a live run. `None` means the index cannot
/// honestly answer — the target is a folded leaf (its interior was never
/// recorded) or nothing under it cleared `INDEX_FLOOR` — and the caller
/// falls back to a live walk of that subtree.
pub fn drill(parent: &ScanResult, root: &Path) -> Option<ScanResult> {
    let index = parent.index.as_ref()?;
    if index.fold.contains_key(root) {
        return None;
    }
    let mut children: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for path in index.totals.keys() {
        if path != root
            && path.starts_with(root)
            && let Some(up) = path.parent()
        {
            children
                .entry(up.to_path_buf())
                .or_default()
                .push(path.clone());
        }
    }
    let (regenerable, dirs) = tables(root, &index.totals, &children, &index.fold);
    let mut files: Vec<FileHit> = index
        .files
        .iter()
        .filter(|f| f.path.starts_with(root))
        .cloned()
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.bytes));
    cap_table(&mut files, |f| f.bytes);
    if regenerable.is_empty() && dirs.is_empty() && files.is_empty() {
        return None;
    }
    // The derived result inherits the parent's vintage — scanned_at and
    // took describe the walk this data actually came from.
    Some(ScanResult {
        root: root.to_path_buf(),
        scanned_at: parent.scanned_at,
        took: parent.took,
        dirs_seen: parent.dirs_seen,
        skipped_denied: parent.skipped_denied,
        skipped_protected: parent.skipped_protected,
        skipped_dataless: parent.skipped_dataless,
        regenerable,
        dirs,
        files,
        suggestions: parent
            .suggestions
            .iter()
            .filter(|d| d.path.starts_with(root))
            .cloned()
            .collect(),
        index: Some(index.clone()),
    })
}

/// Roll direct bytes up into totals, then build the two directory tables
/// in one call — the tests' entry point; the walk goes through
/// `rollup` + `tables` directly so it can also feed the index.
#[cfg(test)]
fn rank(
    root: &Path,
    own_bytes: HashMap<PathBuf, u64>,
    plain_dirs: &[PathBuf],
    fold: &HashMap<PathBuf, HitKind>,
) -> (Vec<DirHit>, Vec<DirHit>) {
    let (totals, children) = rollup(root, own_bytes, plain_dirs, fold);
    tables(root, &totals, &children, fold)
}

/// Totals: every candidate dir starts from its direct bytes, then
/// deepest-first each adds itself into its parent. The child map is the
/// same keys grouped by parent.
type Rollup = (HashMap<PathBuf, u64>, HashMap<PathBuf, Vec<PathBuf>>);
fn rollup(
    root: &Path,
    own_bytes: HashMap<PathBuf, u64>,
    plain_dirs: &[PathBuf],
    fold: &HashMap<PathBuf, HitKind>,
) -> Rollup {
    let mut totals = own_bytes;
    for d in plain_dirs {
        totals.entry(d.clone()).or_default();
    }
    for f in fold.keys() {
        totals.entry(f.clone()).or_default();
    }
    let mut deepest_first: Vec<PathBuf> = totals.keys().cloned().collect();
    deepest_first.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for path in deepest_first {
        if path == root {
            continue;
        }
        let bytes = totals[&path];
        if let Some(parent) = path.parent() {
            *totals.entry(parent.to_path_buf()).or_default() += bytes;
        }
    }

    let mut children: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for path in totals.keys() {
        if path != root
            && path.starts_with(root)
            && let Some(parent) = path.parent()
        {
            children
                .entry(parent.to_path_buf())
                .or_default()
                .push(path.clone());
        }
    }
    (totals, children)
}

/// The two directory tables for `root`, from rolled-up totals — shared
/// by the walk's snapshots and by index-served drill-downs, so the two
/// can never disagree about what a breakdown looks like.
fn tables(
    root: &Path,
    totals: &HashMap<PathBuf, u64>,
    children: &HashMap<PathBuf, Vec<PathBuf>>,
    fold: &HashMap<PathBuf, HitKind>,
) -> (Vec<DirHit>, Vec<DirHit>) {
    let mut regenerable: Vec<DirHit> = fold
        .iter()
        .filter(|(path, kind)| **kind == HitKind::Tag && path.starts_with(root))
        .map(|(path, _)| DirHit {
            bytes: totals.get(path).copied().unwrap_or(0),
            path: path.clone(),
            kind: HitKind::Tag,
        })
        .collect();
    regenerable.sort_by_key(|d| std::cmp::Reverse(d.bytes));
    regenerable.truncate(TABLE_CAP);
    // Fixed cap here on purpose: this table carries the bulk-clear
    // button, and "the N listed" should stay a stable, small N.

    // The dirs table is the root's level-1 breakdown, each entry chased
    // through dominant chains: level-1 subtrees are disjoint, so the
    // ancestor-domination problem cannot arise between rows.
    let mut dirs: Vec<DirHit> = children
        .get(root)
        .map(|level1| {
            level1
                .iter()
                .map(|d| {
                    let rep = chase_dominant(d, totals, children);
                    DirHit {
                        bytes: totals.get(&rep).copied().unwrap_or(0),
                        kind: fold.get(&rep).copied().unwrap_or(HitKind::Plain),
                        path: rep,
                    }
                })
                .filter(|d| d.bytes > 0)
                .collect()
        })
        .unwrap_or_default();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.bytes));
    cap_table(&mut dirs, |d| d.bytes);

    (regenerable, dirs)
}

/// Cap a descending-sorted table at `TABLE_CAP`, admitting rows 9–10
/// only while each still exceeds `TABLE_EXTEND_MIN` on its own.
fn cap_table<T>(items: &mut Vec<T>, bytes: impl Fn(&T) -> u64) {
    let mut keep = items.len().min(TABLE_CAP_EXTENDED);
    while keep > TABLE_CAP && bytes(&items[keep - 1]) <= TABLE_EXTEND_MIN {
        keep -= 1;
    }
    items.truncate(keep);
}

/// Follow single-child dominance down to the first real fork: a chain of
/// wrappers each ≥ `DOMINANCE_PERCENT` of its parent is represented by
/// its end, which is the directory a person would actually act on.
fn chase_dominant(
    start: &Path,
    totals: &HashMap<PathBuf, u64>,
    children: &HashMap<PathBuf, Vec<PathBuf>>,
) -> PathBuf {
    let mut current = start.to_path_buf();
    loop {
        let total = totals.get(&current).copied().unwrap_or(0);
        if total == 0 {
            return current;
        }
        let dominant = children
            .get(&current)
            .and_then(|kids| {
                kids.iter()
                    .map(|k| (k, totals.get(k).copied().unwrap_or(0)))
                    .max_by_key(|(_, b)| *b)
            })
            .filter(|(_, bytes)| bytes * 100 >= total * DOMINANCE_PERCENT);
        match dominant {
            Some((child, _)) => current = child.clone(),
            None => return current,
        }
    }
}

/// The nearest folded ancestor (or the path itself) that owns this entry.
fn fold_owner(fold: &HashMap<PathBuf, HitKind>, path: &Path, root: &Path) -> Option<PathBuf> {
    let mut cursor = Some(path);
    while let Some(p) = cursor {
        if p == root {
            return None;
        }
        if fold.contains_key(p) {
            return Some(p.to_path_buf());
        }
        cursor = p.parent();
    }
    None
}

/// Should this directory stop the walk from recording structure below it?
fn classify(path: &Path, heuristics_suspended: bool) -> Option<HitKind> {
    if has_cache_tag(path) {
        return Some(HitKind::Tag);
    }
    let name = path.file_name()?.to_str()?;
    if let Some(ext) = name.rsplit_once('.').map(|(_, e)| e)
        && BUNDLES.contains(&ext)
    {
        // A bundle is one item, not a guess — but it is not regenerable
        // either.
        return Some(HitKind::Plain);
    }
    if !heuristics_suspended && PATHOLOGICAL.contains(&name) {
        return Some(HitKind::Heuristic);
    }
    None
}

/// CACHEDIR.TAG with the spec's signature as its first bytes.
fn has_cache_tag(dir: &Path) -> bool {
    let Ok(bytes) = std::fs::read(dir.join("CACHEDIR.TAG")) else {
        return false;
    };
    bytes.starts_with(CACHE_TAG_SIGNATURE)
}

/// Spotlight cannot have reported this path: a dot-component, or under
/// ~/Library. A path approximation on purpose — asking mdls per file is a
/// subprocess per row, and the error direction here is listing more, not
/// missing.
fn in_blind_spot(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.starts_with('.') && s.len() > 1)
    }) || std::env::var("HOME").is_ok_and(|h| path.starts_with(Path::new(&h).join("Library")))
}

fn is_dataless(meta: &std::fs::Metadata) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        meta.st_flags() & UF_DATALESS != 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = meta;
        false
    }
}

fn physical_size(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn run_walks_a_real_tree_into_the_tables() {
        let root = std::env::temp_dir().join(format!("zstats-diskscan-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // A tagged cache tree, a dominant wrapper chain, and a plain dir.
        std::fs::create_dir_all(root.join("cache/deep")).unwrap();
        std::fs::write(
            root.join("cache/CACHEDIR.TAG"),
            b"Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        std::fs::write(root.join("cache/deep/blob"), vec![1u8; 20_000]).unwrap();
        std::fs::create_dir_all(root.join("wrapper/inner")).unwrap();
        std::fs::write(root.join("wrapper/inner/data"), vec![1u8; 40_000]).unwrap();
        std::fs::create_dir_all(root.join("plain")).unwrap();
        std::fs::write(root.join("plain/file"), vec![1u8; 8_000]).unwrap();

        let (tx, _rx) = smol::channel::unbounded();
        let result = run(&root, &Arc::new(AtomicBool::new(false)), &tx)
            .expect("scan should succeed")
            .expect("scan was not cancelled");

        // The tagged tree ranks as regenerable, bytes from inside it.
        assert_eq!(result.regenerable.len(), 1);
        assert!(result.regenerable[0].path.ends_with("cache"));
        assert!(result.regenerable[0].bytes >= 20_000);
        // Level-1 breakdown: wrapper chased to its dominant inner, cache
        // and plain as themselves; nothing under the fold recorded.
        let paths: Vec<_> = result.dirs.iter().map(|d| d.path.clone()).collect();
        assert!(
            paths.iter().any(|d| d.ends_with("wrapper/inner")),
            "{paths:?}"
        );
        assert!(paths.iter().any(|d| d.ends_with("cache")), "{paths:?}");
        assert!(paths.iter().any(|d| d.ends_with("plain")), "{paths:?}");
        assert!(!paths.iter().any(|d| d.ends_with("deep")), "{paths:?}");
        // A finished run carries the drill index (fold roots survive the
        // byte floor unconditionally), and the tagged tree lands in the
        // one-click suggestion set.
        assert!(result.index.is_some());
        assert!(result.suggestions.iter().any(|d| d.path.ends_with("cache")));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rank_breaks_down_level_one_and_chases_dominant_chains() {
        let root = p("/r");
        // /r/a is a pass-through wrapper: 100% of it is /r/a/b, which
        // forks. /r/c holds its own bytes.
        let own: HashMap<PathBuf, u64> =
            [(p("/r/a/b/x"), 90u64), (p("/r/a/b/y"), 60), (p("/r/c"), 40)].into();
        let plain = vec![
            p("/r/a"),
            p("/r/a/b"),
            p("/r/a/b/x"),
            p("/r/a/b/y"),
            p("/r/c"),
        ];
        let (_, dirs) = rank(&root, own, &plain, &HashMap::new());

        // Level-1 rows: /r/a collapsed to its fork /r/a/b (150), then /r/c.
        assert_eq!(dirs[0].path, p("/r/a/b"));
        assert_eq!(dirs[0].bytes, 150);
        assert_eq!(dirs[1].path, p("/r/c"));
        assert_eq!(dirs[1].bytes, 40);
    }

    #[test]
    fn rank_lists_tagged_trees_separately() {
        let root = p("/r");
        let own: HashMap<PathBuf, u64> = [(p("/r/target"), 500u64)].into();
        let fold: HashMap<PathBuf, HitKind> = [(p("/r/target"), HitKind::Tag)].into();
        let (regen, dirs) = rank(&root, own, &[], &fold);
        assert_eq!(regen.len(), 1);
        assert_eq!(regen[0].path, p("/r/target"));
        assert_eq!(regen[0].bytes, 500);
        // It is also the root's level-1 breakdown.
        assert_eq!(dirs[0].path, p("/r/target"));
    }

    #[test]
    fn fold_owner_finds_nearest_ancestor_only_below_root() {
        let root = p("/r");
        let fold: HashMap<PathBuf, HitKind> = [(p("/r/a"), HitKind::Heuristic)].into();
        assert_eq!(
            fold_owner(&fold, &p("/r/a/deep/file"), &root),
            Some(p("/r/a"))
        );
        assert_eq!(fold_owner(&fold, &p("/r/b/file"), &root), None);
    }

    #[test]
    fn cache_tag_requires_the_signature() {
        let dir = std::env::temp_dir().join(format!("zstats-diskscan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("CACHEDIR.TAG"), b"not the signature").unwrap();
        assert!(!has_cache_tag(&dir));
        std::fs::write(
            dir.join("CACHEDIR.TAG"),
            b"Signature: 8a477f597d28d172789f06886806bc55\n# by test",
        )
        .unwrap();
        assert!(has_cache_tag(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn classify_folds_bundles_and_suspends_heuristics_at_level_one() {
        assert_eq!(
            classify(&p("/r/x/Photos.photoslibrary"), false),
            Some(HitKind::Plain)
        );
        assert_eq!(
            classify(&p("/r/x/node_modules"), false),
            Some(HitKind::Heuristic)
        );
        // The root's direct children never fold on a guess.
        assert_eq!(classify(&p("/r/node_modules"), true), None);
        assert_eq!(classify(&p("/r/x/ordinary"), false), None);
    }

    #[test]
    fn drill_serves_level_one_from_the_index_and_declines_folds() {
        let mb = |n: u64| n * 1024 * 1024;
        let totals: HashMap<PathBuf, u64> = [
            (p("/r"), mb(300)),
            (p("/r/Library"), mb(200)),
            (p("/r/Library/Caches"), mb(150)),
            (p("/r/Library/Caches/big"), mb(100)),
            (p("/r/Library/Logs"), mb(40)),
            (p("/r/docs"), mb(90)),
        ]
        .into();
        let fold: HashMap<PathBuf, HitKind> = [(p("/r/Library/Caches/big"), HitKind::Tag)].into();
        let files = vec![FileHit {
            path: p("/r/Library/Logs/huge.log"),
            bytes: mb(60),
        }];
        let parent = ScanResult {
            root: p("/r"),
            scanned_at: Instant::now(),
            took: Duration::ZERO,
            dirs_seen: 6,
            skipped_denied: 0,
            skipped_protected: 0,
            skipped_dataless: 0,
            regenerable: Vec::new(),
            dirs: Vec::new(),
            files: Vec::new(),
            suggestions: Vec::new(),
            index: Some(Arc::new(DirIndex {
                totals,
                fold,
                files,
            })),
        };

        let lib = drill(&parent, &p("/r/Library")).expect("the index covers Library");
        let dir_paths: Vec<&Path> = lib.dirs.iter().map(|d| d.path.as_path()).collect();
        assert_eq!(
            dir_paths,
            [p("/r/Library/Caches"), p("/r/Library/Logs")]
                .iter()
                .map(PathBuf::as_path)
                .collect::<Vec<_>>()
        );
        assert_eq!(lib.regenerable.len(), 1, "the tagged tree stays listed");
        assert_eq!(lib.files.len(), 1, "blind-spot files follow the root");
        assert!(lib.index.is_some(), "deeper drills stay instant");

        // A folded leaf's interior was never recorded; a corner below the
        // index floor has nothing retained. Both decline → live walk.
        assert!(drill(&parent, &p("/r/Library/Caches/big")).is_none());
        assert!(drill(&parent, &p("/r/docs/sub")).is_none());
    }

    #[test]
    fn suggest_unions_tags_and_trashable_hints_folding_nested_picks() {
        let totals: HashMap<PathBuf, u64> = [
            (p("/r"), 400u64),
            (p("/r/proj/target"), 200),
            (p("/r/npm-cache"), 120),
            (p("/r/npm-cache/sub"), 90),
            (p("/r/data"), 80),
        ]
        .into();
        let fold: HashMap<PathBuf, HitKind> = [(p("/r/proj/target"), HitKind::Tag)].into();
        let hints = [p("/r/npm-cache"), p("/r/npm-cache/sub")];
        let got = suggest(&totals, &fold, &|path| hints.iter().any(|h| h == path));
        let paths: Vec<&Path> = got.iter().map(|d| d.path.as_path()).collect();
        // The Tag tree and the hint dir make the set; the nested hint
        // folds into its listed ancestor; plain data never qualifies.
        assert_eq!(
            paths,
            [p("/r/proj/target"), p("/r/npm-cache")]
                .iter()
                .map(PathBuf::as_path)
                .collect::<Vec<_>>()
        );
        assert_eq!(got[0].kind, HitKind::Tag);
        assert_eq!(got[1].kind, HitKind::Plain);
    }

    #[test]
    fn cap_table_extends_only_while_the_tail_stays_heavy() {
        let mb = |n: u64| n * 1024 * 1024;
        let capped = |sizes: Vec<u64>| {
            let mut sizes = sizes;
            cap_table(&mut sizes, |b| *b);
            sizes.len()
        };
        // Rows 9 and 10 both exceed 500 MB → the full extended cap.
        assert_eq!(capped((0..12).map(|i| mb(2000 - i * 100)).collect()), 10);
        // Row 9 heavy, row 10 light → nine rows, not a padded ten.
        let mut mixed: Vec<u64> = (0..8).map(|i| mb(9000 - i * 1000)).collect();
        mixed.extend([mb(600), mb(100), mb(90)]);
        assert_eq!(capped(mixed), 9);
        // A light tail keeps the default cap.
        assert_eq!(capped((0..12).map(|i| mb(400 - i * 10)).collect()), 8);
        // Fewer rows than the cap pass through untouched.
        assert_eq!(capped(vec![mb(1000), mb(20)]), 2);
    }
}
