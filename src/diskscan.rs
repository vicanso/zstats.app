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

use crate::assetinfo;
pub use crate::assetinfo::AssetNote;
use crate::cleanhints;
use crate::prefs;
use jwalk::{Parallelism, WalkDirGeneric};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
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
/// this by default, but only while every extra row still exceeds
/// `TABLE_EXTEND_MIN` — a tail that heavy is exactly what the reader
/// came for; a lighter one is the noise the cap exists to cut.
const TABLE_CAP_EXTENDED: usize = 10;
/// Rows *retained* per table — what the dirs section's "show more"
/// control can reveal. Display defaults stay at `TABLE_CAP`(+2); this
/// only bounds the data (and the P2b cache rows).
pub const TABLE_KEEP: usize = 20;
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
    let cores = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    (cores / 2).clamp(1, 3)
}

/// Per-file metadata is fetched on the worker pool (`process_read_dir`
/// runs there), not in the consumer loop — the per-file stat is the
/// walk's most numerous syscall, and doing it serially on the consumer
/// thread would idle the pool.
type Walk = WalkDirGeneric<((), Option<fs::Metadata>)>;

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
    /// Renders as the row's trust-tier pill: Tag wears "cache",
    /// Heuristic a fainter "guess", Plain nothing.
    pub kind: HitKind,
    /// What macOS declares about this row, for the MobileAsset
    /// directories the whole-disk scope exposes — `None` everywhere
    /// else. Resolved once per finished table (`assetinfo::note_for`
    /// shells out), never per frame.
    pub asset: Option<AssetNote>,
}

#[derive(Clone)]
pub struct FileHit {
    pub path: PathBuf,
    pub bytes: u64,
}

pub struct ScanResult {
    /// The label / drill base — every table path sits beneath it.
    pub root: PathBuf,
    /// The roots actually requested, verbatim — `[root]` except for the
    /// cache-set preset, where `root` is the common ancestor (home) and
    /// this lists the explicit walk roots. The *requested* set on
    /// purpose, existing or not: cache identity and the Δ baseline key
    /// on it, and a `mkdir ~/.cache` between runs must not orphan
    /// either.
    pub roots: Vec<PathBuf>,
    /// Wall clock, not monotonic: a cached result must still know its
    /// age after a restart.
    pub scanned_at: SystemTime,
    /// Wall clock from walk start to this snapshot — on `Done`, the
    /// whole run's cost, shown so the user can decide whether a re-scan
    /// is a "wait for it" or a "come back later".
    pub took: Duration,
    pub dirs_seen: usize,
    pub skipped_denied: usize,
    pub skipped_protected: usize,
    pub skipped_dataless: usize,
    /// Directories the user excluded (`analysis_exclude` in app.toml),
    /// pruned like the TCC list. Counted and shown, never silent: a walk
    /// that quietly leaves out 40 GB of somebody's code would make every
    /// total below it a lie by omission.
    pub skipped_excluded: usize,
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
/// What one analysis covers: the label/drill base plus the roots the
/// walker actually visits. Every preset and picked folder is a
/// single-root scope; the cache-set preset is the one multi-root case
/// (docs/disk-analysis.md's scope table).
#[derive(Clone)]
pub struct ScanScope {
    pub base: PathBuf,
    pub roots: Vec<PathBuf>,
}

impl ScanScope {
    pub fn single(root: PathBuf) -> Self {
        Self {
            base: root.clone(),
            roots: vec![root],
        }
    }

    /// Everything on the writable volume — see [`whole_disk_root`] for
    /// why that is not `/`. Single-root like every picked folder; the
    /// prunes it needs are derived from the root itself
    /// (`volume_prunes`) rather than carried here, so a hand-picked
    /// `/System/Volumes/Data` behaves identically to the preset.
    pub fn whole_disk() -> Self {
        Self::single(whole_disk_root())
    }

    /// The explicit cache roots, merged into one ranked view under the
    /// home base: the three macOS/XDG locations, then the tool caches
    /// that live directly in `~` and a full home walk would bury among
    /// project noise. Admission bar: a well-known location that is
    /// wholly cache-like — `~/.cargo/registry` rather than `~/.cargo`,
    /// whose `bin` is installed software, not cache. Missing paths
    /// (most machines have only a few of the dot ones) are skipped at
    /// walk time but stay in the list — see [`ScanResult::roots`].
    ///
    /// This list IS the preset's cache identity: every edit renames the
    /// cache file, orphaning the previous result and Δ baseline once.
    /// Grow it deliberately, not entry-by-entry.
    pub fn cache_set() -> Option<Self> {
        let home = default_root()?;
        Some(Self {
            roots: [
                "Library/Caches",
                ".cache",
                "Library/Developer",
                ".npm",
                ".cargo/registry",
                ".gradle",
                ".m2",
            ]
            .iter()
            .map(|sub| home.join(sub))
            .collect(),
            base: home,
        })
    }
}

pub fn spawn(scope: ScanScope, cancel: Arc<AtomicBool>, tx: smol::channel::Sender<ScanEvent>) {
    thread::spawn(move || match run(&scope, &cancel, &tx) {
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
    env::var("HOME").ok().map(PathBuf::from)
}

/// The writable volume, which on an Apple-silicon-era Mac is where
/// every reclaimable byte lives — the root of the whole-disk scope.
///
/// **Not `/`,** and the difference is not cosmetic. Since macOS 10.15
/// the boot disk is two volumes: a sealed, read-only system volume
/// mounted at `/`, and a data volume whose directories are stitched
/// into it by *firmlinks*. A firmlink is not a symlink — `/Users` and
/// `/System/Volumes/Data/Users` are literally the same inode on the
/// same device (measured: ino 16925 both ways), so `follow_links(false)`
/// does nothing about it and a walk from `/` counts the entire data
/// volume **twice**, once through each name. Nothing about a size
/// report survives being wrong by a factor of two.
///
/// Walking `/` and pruning `/System/Volumes/Data` fixes the double
/// count and creates a worse problem: the data volume's top level
/// holds directories that no firmlink points at — `.Spotlight-V100`,
/// `.DocumentRevisions-V100`, `MobileSoftwareUpdate`, `.fseventsd` —
/// which are unreachable from `/` and are exactly the "where did my
/// disk go" answers people come here for.
///
/// So the scope roots at the data volume: one volume, every byte once,
/// including the `System/Library/AssetsV2` downloads (asset caches
/// that reach gigabytes) that no home-shaped preset could ever see.
/// What it does not cover is the sealed volume itself — fixed size,
/// cryptographically sealed, not one byte of it reclaimable. Falls
/// back to `/` where there is no data volume (pre-Catalina, or a
/// non-APFS boot disk), where the split does not exist either.
pub fn whole_disk_root() -> PathBuf {
    let data = PathBuf::from(DATA_VOLUME);
    if data.is_dir() {
        data
    } else {
        PathBuf::from("/")
    }
}

/// Mount point of the boot disk's writable volume.
const DATA_VOLUME: &str = "/System/Volumes/Data";

/// The same prefix, for the view that strips it off a path before
/// showing it — one definition, so the walker and the label can never
/// disagree about what the whole-disk root is called.
pub const DATA_VOLUME_DISPLAY_PREFIX: &str = DATA_VOLUME;

/// Directories under a whole-disk root that must not be walked.
///
/// `Volumes` holds *other* disks — an external drive, a mounted DMG, a
/// network share — and folding a 2 TB backup drive into "this disk" is
/// the same class of lie as the double count. `home` is an autofs
/// trigger: walking it mounts things on demand. Both are pruned by
/// exact path, so a user who deliberately picks `/Volumes/Backup` as
/// their scope still gets it walked — the prune is about what a
/// *whole-disk* walk wanders into, not about those paths being off
/// limits.
fn volume_prunes(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter(|root| root.as_path() == Path::new(DATA_VOLUME) || root.as_path() == Path::new("/"))
        .flat_map(|root| [root.join("Volumes"), root.join("home")])
        .collect()
}

fn run(
    scope: &ScanScope,
    cancel: &Arc<AtomicBool>,
    tx: &smol::channel::Sender<ScanEvent>,
) -> Result<Option<ScanResult>, String> {
    // Missing roots are skipped, not fatal (`~/.cache` on many machines);
    // only an entirely absent scope is an error.
    let walked: Vec<PathBuf> = scope.roots.iter().filter(|r| r.is_dir()).cloned().collect();
    if walked.is_empty() {
        return Err(format!("{} is not a directory", scope.base.display()));
    }
    let protected = Arc::new(AtomicUsize::new(0));
    let excluded_hits = Arc::new(AtomicUsize::new(0));
    // The user's own list, pruned exactly like the TCC one — the
    // difference is only whose decision it was.
    let excluded: Vec<PathBuf> = prefs::analysis_exclude();
    let mut deny: Vec<PathBuf> = env::var("HOME")
        .ok()
        .map(|h| {
            TCC_DENY
                .iter()
                .map(|s| Path::new(&h).join("Library").join(s))
                .collect()
        })
        .unwrap_or_default();
    deny.extend(volume_prunes(&walked));

    // Raw collection: one bytes counter per owning directory, the fold
    // map, and the candidates for the file table. No tree. Shared across
    // the scope's roots — one merged result, one progress stream.
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

    for root in &walked {
        let walk = {
            let deny = deny.clone();
            let excluded = excluded.clone();
            let excluded_hits = excluded_hits.clone();
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
                            } else if excluded.iter().any(|d| child.path() == *d) {
                                child.read_children_path = None;
                                excluded_hits.fetch_add(1, Ordering::Relaxed);
                            }
                        } else if child.file_type.is_file() {
                            // On the worker pool by design — see `Walk`.
                            child.client_state = fs::symlink_metadata(child.path()).ok();
                        }
                    }
                })
        };

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
                    let partial = snapshot(Aggregates {
                        base: &scope.base,
                        roots: &scope.roots,
                        took: started.elapsed(),
                        own_bytes: own_bytes.clone(),
                        plain_dirs: &plain_dirs,
                        fold: &fold,
                        files: files.clone(),
                        dirs_seen: dirs_done,
                        skipped_denied: denied,
                        skipped_protected: protected.load(Ordering::Relaxed),
                        skipped_dataless: dataless,
                        skipped_excluded: excluded_hits.load(Ordering::Relaxed),
                        build_index: false,
                    });
                    let _ = tx.try_send(ScanEvent::Partial(Box::new(partial)));
                }
                if path == *root || fold_owner(&fold, &path, root).is_some() {
                    continue; // no structure recorded beneath a folded tree
                }
                // depth 1 = this walk root's direct children; the
                // heuristic fold is suspended there so the level-1
                // breakdown cannot swallow itself (Tag keeps its meaning
                // at any depth).
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
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }

    Ok(Some(snapshot(Aggregates {
        base: &scope.base,
        roots: &scope.roots,
        took: started.elapsed(),
        skipped_excluded: excluded_hits.load(Ordering::Relaxed),
        own_bytes,
        plain_dirs: &plain_dirs,
        fold: &fold,
        files,
        dirs_seen: dirs_done,
        skipped_denied: denied,
        skipped_protected: protected.load(Ordering::Relaxed),
        skipped_dataless: dataless,
        build_index: true,
    })))
}

/// Everything the walk has aggregated so far, named — the snapshot
/// builder's input as a struct rather than eleven positional arguments
/// (clippy's lint was right about the call sites).
struct Aggregates<'a> {
    base: &'a Path,
    roots: &'a [PathBuf],
    took: Duration,
    own_bytes: HashMap<PathBuf, u64>,
    plain_dirs: &'a [PathBuf],
    fold: &'a HashMap<PathBuf, HitKind>,
    files: Vec<FileHit>,
    dirs_seen: usize,
    skipped_denied: usize,
    skipped_protected: usize,
    skipped_dataless: usize,
    skipped_excluded: usize,
    /// Final results build the drill index and the suggestion set;
    /// partial snapshots skip both.
    build_index: bool,
}

/// Assemble a `ScanResult` from the aggregates as they stand — used for
/// the final result and, on a clone of the aggregates, for the partial
/// snapshots streamed mid-walk. One builder, so the two can never drift.
fn snapshot(agg: Aggregates) -> ScanResult {
    let Aggregates {
        base,
        roots,
        took,
        own_bytes,
        plain_dirs,
        fold,
        mut files,
        dirs_seen,
        skipped_denied,
        skipped_protected,
        skipped_dataless,
        skipped_excluded,
        build_index,
    } = agg;
    let (totals, children) = rollup(base, own_bytes, plain_dirs, fold);
    let (regenerable, dirs) = tables(roots, &totals, &children, fold);
    files.sort_by_key(|f| Reverse(f.bytes));
    // Suggestions only on the finished result: a partial's lower-bound
    // set would invite trashing while the walker is inside the trees.
    let suggestions = if build_index {
        suggest(&totals, fold, &|p| {
            cleanhints::lookup(p).is_some_and(|h| h.trashable)
        })
    } else {
        Vec::new()
    };
    // The index keeps the FULL blind-spot file list; the retention cap
    // below only trims what a card (after "show more") can show.
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
    files.truncate(TABLE_KEEP);
    ScanResult {
        root: base.to_path_buf(),
        roots: roots.to_vec(),
        scanned_at: SystemTime::now(),
        took,
        dirs_seen,
        skipped_denied,
        skipped_protected,
        skipped_dataless,
        skipped_excluded,
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
            // A tagged cache is never a MobileAsset row: AssetsV2
            // carries no CACHEDIR.TAG.
            asset: None,
        })
        .collect();
    for (path, bytes) in totals {
        if !fold.contains_key(path) && trashable(path) {
            picks.push(DirHit {
                path: path.clone(),
                bytes: *bytes,
                kind: HitKind::Plain,
                asset: None,
            });
        }
    }
    picks.sort_by_key(|d| Reverse(d.bytes));
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
    let (regenerable, dirs) = tables(&[root.to_path_buf()], &index.totals, &children, &index.fold);
    let mut files: Vec<FileHit> = index
        .files
        .iter()
        .filter(|f| f.path.starts_with(root))
        .cloned()
        .collect();
    files.sort_by_key(|f| Reverse(f.bytes));
    files.truncate(TABLE_KEEP);
    if regenerable.is_empty() && dirs.is_empty() && files.is_empty() {
        return None;
    }
    // The derived result inherits the parent's vintage — scanned_at and
    // took describe the walk this data actually came from.
    Some(ScanResult {
        root: root.to_path_buf(),
        roots: vec![root.to_path_buf()],
        scanned_at: parent.scanned_at,
        took: parent.took,
        dirs_seen: parent.dirs_seen,
        skipped_denied: parent.skipped_denied,
        skipped_protected: parent.skipped_protected,
        skipped_dataless: parent.skipped_dataless,
        skipped_excluded: parent.skipped_excluded,
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
    tables(&[root.to_path_buf()], &totals, &children, fold)
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
    deepest_first.sort_by_key(|p| Reverse(p.components().count()));
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

/// The two directory tables for a set of roots, from rolled-up totals —
/// shared by the walk's snapshots and by index-served drill-downs, so
/// the two can never disagree about what a breakdown looks like. Single
/// element for every scope but the cache-set preset, whose roots'
/// level-1 rows merge into one ranking (the subtrees are disjoint by
/// construction, so rows still cannot dominate each other).
fn tables(
    roots: &[PathBuf],
    totals: &HashMap<PathBuf, u64>,
    children: &HashMap<PathBuf, Vec<PathBuf>>,
    fold: &HashMap<PathBuf, HitKind>,
) -> (Vec<DirHit>, Vec<DirHit>) {
    let mut regenerable: Vec<DirHit> = fold
        .iter()
        .filter(|(path, kind)| {
            **kind == HitKind::Tag && roots.iter().any(|root| path.starts_with(root))
        })
        .map(|(path, _)| DirHit {
            bytes: totals.get(path).copied().unwrap_or(0),
            path: path.clone(),
            kind: HitKind::Tag,
            // A tagged cache is never a MobileAsset row: AssetsV2
            // carries no CACHEDIR.TAG.
            asset: None,
        })
        .collect();
    regenerable.sort_by_key(|d| Reverse(d.bytes));
    regenerable.truncate(TABLE_CAP);
    // Fixed cap here on purpose: this table carries the bulk-clear
    // button, and "the N listed" should stay a stable, small N.

    // The dirs table is each root's level-1 breakdown, every entry
    // chased through dominant chains: level-1 subtrees are disjoint, so
    // the ancestor-domination problem cannot arise between rows.
    let mut dirs: Vec<DirHit> = roots
        .iter()
        .filter_map(|root| children.get(root))
        .flat_map(|level1| level1.iter())
        .map(|d| {
            let rep = chase_dominant(d, totals, children);
            DirHit {
                bytes: totals.get(&rep).copied().unwrap_or(0),
                kind: fold.get(&rep).copied().unwrap_or(HitKind::Plain),
                path: rep,
                // Filled in below, once the table is truncated.
                asset: None,
            }
        })
        .filter(|d| d.bytes > 0)
        .collect();
    dirs.sort_by_key(|d| Reverse(d.bytes));
    dirs.truncate(TABLE_KEEP);
    // After the truncate, so the plist reads are bounded by what the
    // table will actually show (TABLE_KEEP rows) rather than by how
    // many directories the walk crossed.
    for dir in &mut dirs {
        dir.asset = assetinfo::note_for(&dir.path);
    }

    (regenerable, dirs)
}

// ---- result cache (docs/disk-analysis.md, P2b) -------------------------
//
// One file per root under ~/.zstats/diskscan/: write = whole file, read =
// wrong root or version → discarded. Only finished top-level walks are
// written (a cancelled or failed run's half-table overwriting a full one
// would be a regression), and only the tables — the drill index is not
// persisted, so a drill on a loaded result re-walks that subtree. The
// honesty boundary is the timestamp: a cache serves "see last time's
// numbers first" and claims nothing about now.

/// Suggestion rows kept in the file. Bounds the file size, and a
/// reloaded "Trash all N" honestly matches what it lists.
const PERSIST_SUGGESTIONS: usize = 500;
const CACHE_VERSION: i64 = 1;

fn cache_dir() -> PathBuf {
    zstats::settings::default_dir().join("diskscan")
}

/// Readable slug plus a short FNV hash, so `/a/b` and `/a-b` cannot
/// Readable slug plus a short FNV hash, so `/a/b` and `/a-b` cannot
/// collide on the same filename. Identity is the *roots set*: a
/// multi-root scope hashes every root (newline-joined — impossible in a
/// path), so the cache-set preset can never collide with a plain walk
/// of its base. Single-root names come out byte-identical to the
/// pre-`roots` scheme, keeping existing files valid.
fn cache_path_in(dir: &Path, roots: &[PathBuf]) -> PathBuf {
    let display = roots
        .iter()
        .map(|r| r.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let mut slug: String = display
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    slug.truncate(60);
    let mut hash: u32 = 0x811c_9dc5;
    for byte in display.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    dir.join(format!("{slug}-{hash:08x}.toml"))
}

/// The previous run's file, next to the current one — what row deltas
/// are measured against. Rotated into place by [`save_cache`] just
/// before the new result lands, so it always holds the last *finished*
/// walk of the same scope, surviving restarts like the cache itself.
fn prev_cache_path_in(dir: &Path, roots: &[PathBuf]) -> PathBuf {
    cache_path_in(dir, roots).with_extension("prev.toml")
}

pub fn save_cache(result: &ScanResult) {
    save_cache_in(&cache_dir(), result, true);
}

pub fn load_cache(roots: &[PathBuf]) -> Option<ScanResult> {
    load_cache_in(&cache_dir(), roots)
}

/// The run before the cached one, if two have finished — the Δ baseline.
pub fn load_prev_cache(roots: &[PathBuf]) -> Option<ScanResult> {
    load_cache_file(&prev_cache_path_in(&cache_dir(), roots), roots)
}

pub fn delete_cache(roots: &[PathBuf]) {
    let _ = fs::remove_file(cache_path_in(&cache_dir(), roots));
    let _ = fs::remove_file(prev_cache_path_in(&cache_dir(), roots));
}

/// Files an abandoned scope left behind age out after this long. The
/// guard exists so a scope the user only stepped away from — and may
/// re-select next week — keeps its "last time" and Δ; a month of
/// silence says the scope is done with, and a few KB per pair is not
/// worth keeping forever.
const ORPHAN_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Remove cache files belonging to none of the scopes a launch can
/// restore (`keep`: the default home walk plus the last active scope),
/// once untouched for [`ORPHAN_AGE`]. Called at startup; the directory
/// holds a handful of small files, so this is a few stats.
pub fn sweep_orphans(keep: &[&Vec<PathBuf>]) {
    sweep_orphans_in(&cache_dir(), keep, ORPHAN_AGE);
}

fn sweep_orphans_in(dir: &Path, keep: &[&Vec<PathBuf>], max_age: Duration) {
    let mut keep_files: Vec<PathBuf> = Vec::new();
    for roots in keep {
        if !roots.is_empty() {
            keep_files.push(cache_path_in(dir, roots));
            keep_files.push(prev_cache_path_in(dir, roots));
        }
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if keep_files.contains(&path) {
            continue;
        }
        let expired = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= max_age);
        if expired {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Rewrite the cache after rows were pruned — but only where this exact
/// scope already has one, so drill-derived subroots never gain a file.
/// No rotation: pruning is an edit to the *current* result, and letting
/// it displace the `.prev` file would erase the baseline mid-cycle.
pub fn resave_if_cached(result: &ScanResult) {
    if cache_path_in(&cache_dir(), &result.roots).exists() {
        save_cache_in(&cache_dir(), result, false);
    }
}

fn save_cache_in(dir: &Path, result: &ScanResult, rotate: bool) {
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = cache_path_in(dir, &result.roots);
    let tmp = path.with_extension("toml.tmp");
    if fs::write(&tmp, serialise(result)).is_err() {
        return;
    }
    // 0600: the paths in here leak project names.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    if rotate {
        // Copied, not renamed: a rename would unlink the current cache
        // first, and dying between the two renames would leave the
        // scope with a baseline but no result. Copying costs a few KB
        // and the worst interruption leaves `.prev` equal to the
        // current file — a Δ of all zeros, which is merely useless.
        let _ = fs::copy(&path, prev_cache_path_in(dir, &result.roots));
    }
    let _ = fs::rename(&tmp, &path);
}

fn serialise(result: &ScanResult) -> String {
    use toml::Value;
    use toml::value::Table;
    let clamp = |n: u64| Value::Integer(n.min(i64::MAX as u64) as i64);
    let dir_row = |h: &DirHit| -> Value {
        let mut t = Table::new();
        t.insert("path".into(), Value::String(h.path.display().to_string()));
        t.insert("bytes".into(), clamp(h.bytes));
        t.insert(
            "kind".into(),
            Value::String(
                match h.kind {
                    HitKind::Tag => "tag",
                    HitKind::Heuristic => "heuristic",
                    HitKind::Plain => "plain",
                }
                .into(),
            ),
        );
        // Carried through the cache rather than re-resolved on load: a
        // restored table is shown immediately, and re-reading up to
        // eight plists per row through `plutil` is not something to do
        // while a window is opening.
        if let Some(a) = &h.asset {
            t.insert("asset_kind".into(), Value::String(a.kind.clone()));
            if let Some(v) = a.required_by_os {
                t.insert("asset_required".into(), Value::Boolean(v));
            }
            if let Some(v) = a.never_collected {
                t.insert("asset_never_collected".into(), Value::Boolean(v));
            }
            if let Some(v) = &a.locale {
                t.insert("asset_locale".into(), Value::String(v.clone()));
            }
        }
        Value::Table(t)
    };
    let mut doc = Table::new();
    doc.insert("version".into(), Value::Integer(CACHE_VERSION));
    doc.insert(
        "root".into(),
        Value::String(result.root.display().to_string()),
    );
    doc.insert(
        "roots".into(),
        Value::Array(
            result
                .roots
                .iter()
                .map(|r| Value::String(r.display().to_string()))
                .collect(),
        ),
    );
    let unix_secs = result
        .scanned_at
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    doc.insert("scanned_at_unix".into(), clamp(unix_secs));
    doc.insert("took_secs".into(), clamp(result.took.as_secs()));
    doc.insert("dirs_seen".into(), clamp(result.dirs_seen as u64));
    doc.insert("skipped_denied".into(), clamp(result.skipped_denied as u64));
    doc.insert(
        "skipped_protected".into(),
        clamp(result.skipped_protected as u64),
    );
    doc.insert(
        "skipped_dataless".into(),
        clamp(result.skipped_dataless as u64),
    );
    doc.insert(
        "excluded".into(),
        Value::Array(
            prefs::analysis_exclude()
                .iter()
                .map(|p| Value::String(p.display().to_string()))
                .collect(),
        ),
    );
    doc.insert(
        "skipped_excluded".into(),
        clamp(result.skipped_excluded as u64),
    );
    doc.insert(
        "regenerable".into(),
        Value::Array(result.regenerable.iter().map(dir_row).collect()),
    );
    doc.insert(
        "dir".into(),
        Value::Array(result.dirs.iter().map(dir_row).collect()),
    );
    doc.insert(
        "file".into(),
        Value::Array(
            result
                .files
                .iter()
                .map(|f| {
                    let mut t = Table::new();
                    t.insert("path".into(), Value::String(f.path.display().to_string()));
                    t.insert("bytes".into(), clamp(f.bytes));
                    Value::Table(t)
                })
                .collect(),
        ),
    );
    doc.insert(
        "suggestion".into(),
        Value::Array(
            result
                .suggestions
                .iter()
                .take(PERSIST_SUGGESTIONS)
                .map(dir_row)
                .collect(),
        ),
    );
    toml::to_string(&Value::Table(doc)).unwrap_or_default()
}

fn load_cache_in(dir: &Path, roots: &[PathBuf]) -> Option<ScanResult> {
    load_cache_file(&cache_path_in(dir, roots), roots)
}

fn load_cache_file(path: &Path, roots: &[PathBuf]) -> Option<ScanResult> {
    let text = fs::read_to_string(path).ok()?;
    let doc: toml::Value = toml::from_str(&text).ok()?;
    if doc.get("version")?.as_integer()? != CACHE_VERSION {
        return None;
    }
    let root = PathBuf::from(doc.get("root")?.as_str()?);
    // Files predating the `roots` key are all single-root — default to
    // `[root]` so they stay valid without a version bump.
    let stored_roots: Vec<PathBuf> = doc
        .get("roots")
        .and_then(toml::Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|v| v.as_str().map(PathBuf::from))
                .collect()
        })
        .unwrap_or_else(|| vec![root.clone()]);
    if stored_roots != roots {
        return None;
    }
    // A result produced under a different exclusion list answers a
    // different question. Showing it would look like the list did
    // nothing — so the cache misses and the card asks to be re-run,
    // which is what the roots check does for a changed scope.
    let stored_excluded: Vec<PathBuf> = doc
        .get("excluded")
        .and_then(toml::Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|v| v.as_str().map(PathBuf::from))
                .collect()
        })
        .unwrap_or_default();
    if stored_excluded != prefs::analysis_exclude() {
        return None;
    }
    let int = |key: &str| {
        doc.get(key)
            .and_then(toml::Value::as_integer)
            .unwrap_or(0)
            .max(0) as u64
    };
    let dirs_of = |key: &str| -> Vec<DirHit> {
        doc.get(key)
            .and_then(toml::Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        Some(DirHit {
                            path: PathBuf::from(row.get("path")?.as_str()?),
                            bytes: row.get("bytes")?.as_integer()?.max(0) as u64,
                            kind: match row.get("kind").and_then(toml::Value::as_str) {
                                Some("tag") => HitKind::Tag,
                                Some("heuristic") => HitKind::Heuristic,
                                _ => HitKind::Plain,
                            },
                            asset: row.get("asset_kind").and_then(toml::Value::as_str).map(
                                |kind| AssetNote {
                                    kind: kind.to_string(),
                                    required_by_os: row
                                        .get("asset_required")
                                        .and_then(toml::Value::as_bool),
                                    never_collected: row
                                        .get("asset_never_collected")
                                        .and_then(toml::Value::as_bool),
                                    locale: row
                                        .get("asset_locale")
                                        .and_then(toml::Value::as_str)
                                        .map(str::to_string),
                                },
                            ),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let files = doc
        .get("file")
        .and_then(toml::Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(FileHit {
                        path: PathBuf::from(row.get("path")?.as_str()?),
                        bytes: row.get("bytes")?.as_integer()?.max(0) as u64,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(ScanResult {
        root,
        roots: stored_roots,
        scanned_at: UNIX_EPOCH + Duration::from_secs(int("scanned_at_unix")),
        took: Duration::from_secs(int("took_secs")),
        dirs_seen: int("dirs_seen") as usize,
        skipped_denied: int("skipped_denied") as usize,
        skipped_protected: int("skipped_protected") as usize,
        skipped_dataless: int("skipped_dataless") as usize,
        skipped_excluded: int("skipped_excluded") as usize,
        regenerable: dirs_of("regenerable"),
        dirs: dirs_of("dir"),
        files,
        suggestions: dirs_of("suggestion"),
        index: None,
    })
}

/// The previous run flattened to path → bytes, for per-row deltas.
/// Built from every retained table at once (a path can sit in two —
/// a tagged cache is both a suggestion and a big directory), so a row
/// finds its old figure wherever it ranked last time. Absence proves
/// nothing: the path may simply have fallen below last run's retention
/// cut — which is why rows without a match show no delta rather than
/// claiming "new".
pub struct DiffBaseline {
    roots: Vec<PathBuf>,
    scanned_at: SystemTime,
    bytes: HashMap<PathBuf, u64>,
}

impl DiffBaseline {
    pub fn from_result(result: &ScanResult) -> Self {
        let mut bytes = HashMap::new();
        for hit in result
            .regenerable
            .iter()
            .chain(&result.dirs)
            .chain(&result.suggestions)
        {
            bytes.insert(hit.path.clone(), hit.bytes);
        }
        for hit in &result.files {
            bytes.insert(hit.path.clone(), hit.bytes);
        }
        Self {
            roots: result.roots.clone(),
            scanned_at: result.scanned_at,
            bytes,
        }
    }

    /// The scope this baseline compares against — deltas only make
    /// sense on a result of the same roots (drill tables sit mostly
    /// below last run's retention cut, and a half-comparable table
    /// reads as "nothing else changed").
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn scanned_at(&self) -> SystemTime {
        self.scanned_at
    }

    pub fn bytes_for(&self, path: &Path) -> Option<u64> {
        self.bytes.get(path).copied()
    }
}

/// Default rows shown for a descending-sorted table: `TABLE_CAP`, rows
/// 9–10 admitted only while each still exceeds `TABLE_EXTEND_MIN` on
/// its own. The rest (up to `TABLE_KEEP`) hides behind "show more".
pub fn default_rows<T>(items: &[T], bytes: impl Fn(&T) -> u64) -> usize {
    let mut keep = items.len().min(TABLE_CAP_EXTENDED);
    while keep > TABLE_CAP && bytes(&items[keep - 1]) <= TABLE_EXTEND_MIN {
        keep -= 1;
    }
    keep
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
    let Ok(bytes) = fs::read(dir.join("CACHEDIR.TAG")) else {
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
    }) || env::var("HOME").is_ok_and(|h| path.starts_with(Path::new(&h).join("Library")))
}

fn is_dataless(meta: &fs::Metadata) -> bool {
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

fn physical_size(meta: &fs::Metadata) -> u64 {
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
    use std::process;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// A whole-disk walk must not wander onto other disks, and must not
    /// trip the autofs mount at `home`. Pruned by exact path, so a
    /// deliberately picked `/Volumes/Backup` is still walked — the rule
    /// is about what a whole-disk root wanders into.
    #[test]
    fn a_whole_disk_root_prunes_other_mounts_and_the_autofs_trigger() {
        let prunes = volume_prunes(&[p(DATA_VOLUME)]);
        assert!(prunes.contains(&p("/System/Volumes/Data/Volumes")));
        assert!(prunes.contains(&p("/System/Volumes/Data/home")));
        // The pre-Catalina fallback root gets the same treatment.
        assert_eq!(volume_prunes(&[p("/")]).len(), 2);
        // Every other scope is untouched: a picked folder named
        // Volumes is the user's business, and a home walk has no
        // mount points to cross.
        assert!(volume_prunes(&[p("/Users/x")]).is_empty());
        assert!(volume_prunes(&[p("/Volumes/Backup")]).is_empty());
    }

    /// The root is the data volume where there is one — walking `/`
    /// would double-count every firmlinked directory (`/Users` and
    /// `/System/Volumes/Data/Users` are one inode) and would still miss
    /// the data volume's own top level.
    #[test]
    fn the_whole_disk_root_is_the_data_volume_where_it_exists() {
        let root = whole_disk_root();
        if Path::new(DATA_VOLUME).is_dir() {
            assert_eq!(root, p(DATA_VOLUME));
        } else {
            assert_eq!(root, p("/"));
        }
        assert_eq!(ScanScope::whole_disk().roots, vec![root]);
    }

    #[test]
    fn run_walks_a_real_tree_into_the_tables() {
        let root = env::temp_dir().join(format!("zstats-diskscan-run-{}", process::id()));
        let _ = fs::remove_dir_all(&root);
        // A tagged cache tree, a dominant wrapper chain, and a plain dir.
        fs::create_dir_all(root.join("cache/deep")).unwrap();
        fs::write(
            root.join("cache/CACHEDIR.TAG"),
            b"Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        fs::write(root.join("cache/deep/blob"), vec![1u8; 20_000]).unwrap();
        fs::create_dir_all(root.join("wrapper/inner")).unwrap();
        fs::write(root.join("wrapper/inner/data"), vec![1u8; 40_000]).unwrap();
        fs::create_dir_all(root.join("plain")).unwrap();
        fs::write(root.join("plain/file"), vec![1u8; 8_000]).unwrap();

        let (tx, _rx) = smol::channel::unbounded();
        let result = run(
            &ScanScope::single(root.clone()),
            &Arc::new(AtomicBool::new(false)),
            &tx,
        )
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

        let _ = fs::remove_dir_all(&root);
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
        let dir = env::temp_dir().join(format!("zstats-diskscan-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("CACHEDIR.TAG"), b"not the signature").unwrap();
        assert!(!has_cache_tag(&dir));
        fs::write(
            dir.join("CACHEDIR.TAG"),
            b"Signature: 8a477f597d28d172789f06886806bc55\n# by test",
        )
        .unwrap();
        assert!(has_cache_tag(&dir));
        let _ = fs::remove_dir_all(&dir);
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
            roots: vec![p("/r")],
            scanned_at: SystemTime::now(),
            took: Duration::ZERO,
            dirs_seen: 6,
            skipped_denied: 0,
            skipped_protected: 0,
            skipped_dataless: 0,
            skipped_excluded: 0,
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
    fn merged_scope_ranks_each_roots_level_one_and_nothing_else() {
        let mb = |n: u64| n * 1024 * 1024;
        let own: HashMap<PathBuf, u64> = [
            (p("/r/a/x"), mb(500)),
            (p("/r/a/x/cache"), mb(100)),
            (p("/r/b/y"), mb(300)),
            (p("/r/other/z"), mb(900)),
            (p("/r/other/cache"), mb(50)),
        ]
        .into();
        let plain = [
            p("/r/a"),
            p("/r/a/x"),
            p("/r/b"),
            p("/r/b/y"),
            p("/r/other"),
            p("/r/other/z"),
        ];
        let fold: HashMap<PathBuf, HitKind> = [
            (p("/r/a/x/cache"), HitKind::Tag),
            (p("/r/other/cache"), HitKind::Tag),
        ]
        .into();
        let (totals, children) = rollup(&p("/r"), own, &plain, &fold);
        let (regen, dirs) = tables(&[p("/r/a"), p("/r/b")], &totals, &children, &fold);
        // Both roots' level-1 rows merge into one ranking; the sibling
        // outside the scope stays out even though it is the biggest.
        let dir_paths: Vec<&Path> = dirs.iter().map(|d| d.path.as_path()).collect();
        assert_eq!(
            dir_paths,
            [p("/r/a/x"), p("/r/b/y")]
                .iter()
                .map(PathBuf::as_path)
                .collect::<Vec<_>>()
        );
        // The Tag filter honours the scope the same way.
        assert_eq!(regen.len(), 1);
        assert_eq!(regen[0].path, p("/r/a/x/cache"));
    }

    #[test]
    fn cache_round_trips_and_rejects_mismatches() {
        let dir = env::temp_dir().join(format!("zstats-diskcache-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        let result = ScanResult {
            root: p("/r"),
            roots: vec![p("/r")],
            scanned_at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            took: Duration::from_secs(31),
            dirs_seen: 42,
            skipped_denied: 1,
            skipped_protected: 2,
            skipped_dataless: 3,
            skipped_excluded: 0,
            regenerable: vec![DirHit {
                path: p("/r/cache"),
                bytes: 10,
                kind: HitKind::Tag,
                asset: None,
            }],
            dirs: vec![DirHit {
                path: p("/r/big"),
                bytes: 20,
                kind: HitKind::Plain,
                // A restored table has to keep what the system said, or
                // reopening the window would silently drop the one
                // thing that makes a MobileAsset row readable.
                asset: Some(AssetNote {
                    kind: "Font8".into(),
                    required_by_os: Some(false),
                    never_collected: Some(true),
                    locale: Some("zh_Hans".into()),
                }),
            }],
            files: vec![FileHit {
                path: p("/r/.blob"),
                bytes: 30,
            }],
            suggestions: vec![DirHit {
                path: p("/r/cache"),
                bytes: 10,
                kind: HitKind::Tag,
                asset: None,
            }],
            index: None,
        };
        save_cache_in(&dir, &result, true);

        let loaded = load_cache_in(&dir, &[p("/r")]).expect("round trip");
        assert_eq!(loaded.scanned_at, result.scanned_at);
        assert_eq!(loaded.took, result.took);
        assert_eq!(loaded.dirs_seen, 42);
        assert_eq!(loaded.skipped_protected, 2);
        assert_eq!(loaded.regenerable.len(), 1);
        assert_eq!(loaded.regenerable[0].kind, HitKind::Tag);
        assert_eq!(loaded.dirs[0].path, p("/r/big"));
        assert_eq!(loaded.dirs[0].asset, result.dirs[0].asset);
        assert!(loaded.regenerable[0].asset.is_none());
        assert_eq!(loaded.files[0].bytes, 30);
        assert_eq!(loaded.suggestions.len(), 1);
        assert!(loaded.index.is_none(), "the drill index is never persisted");

        // A different root must never read someone else's file.
        assert!(load_cache_in(&dir, &[p("/other")]).is_none());

        // 0600 on purpose: the paths in here leak project names.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(cache_path_in(&dir, &[p("/r")]))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    fn run_with(bytes: u64, secs: u64) -> ScanResult {
        ScanResult {
            root: p("/r"),
            roots: vec![p("/r")],
            scanned_at: UNIX_EPOCH + Duration::from_secs(secs),
            took: Duration::from_secs(1),
            dirs_seen: 1,
            skipped_denied: 0,
            skipped_protected: 0,
            skipped_dataless: 0,
            skipped_excluded: 0,
            regenerable: vec![DirHit {
                path: p("/r/cache"),
                bytes: 10,
                kind: HitKind::Tag,
                asset: None,
            }],
            dirs: vec![DirHit {
                path: p("/r/big"),
                bytes,
                kind: HitKind::Plain,
                asset: None,
            }],
            files: vec![FileHit {
                path: p("/r/.blob"),
                bytes: 30,
            }],
            suggestions: vec![],
            index: None,
        }
    }

    #[test]
    fn merged_run_walks_existing_roots_and_keeps_the_requested_set() {
        let base = env::temp_dir().join(format!("zstats-multiroot-{}", process::id()));
        let _ = fs::remove_dir_all(&base);
        let a = base.join("a");
        let b = base.join("b");
        fs::create_dir_all(a.join("x")).unwrap();
        fs::write(a.join("x/data"), vec![1u8; 30_000]).unwrap();
        fs::create_dir_all(b.join("y")).unwrap();
        fs::write(b.join("y/data"), vec![1u8; 20_000]).unwrap();
        let scope = ScanScope {
            base: base.clone(),
            // A missing root mid-list (— `~/.cache` on most machines)
            // must be skipped without failing the walk.
            roots: vec![a.clone(), base.join("missing"), b.clone()],
        };
        let (tx, _rx) = smol::channel::unbounded();
        let result = run(&scope, &Arc::new(AtomicBool::new(false)), &tx)
            .expect("merged scan should succeed")
            .expect("scan was not cancelled");
        assert_eq!(result.root, base);
        // Identity records the REQUESTED set verbatim, absentee included.
        assert_eq!(result.roots, scope.roots);
        let dir_paths: Vec<&Path> = result.dirs.iter().map(|d| d.path.as_path()).collect();
        assert!(dir_paths.contains(&a.join("x").as_path()));
        assert!(dir_paths.contains(&b.join("y").as_path()));

        // An entirely absent scope is the error it always was.
        let gone = ScanScope {
            base: base.clone(),
            roots: vec![base.join("nope")],
        };
        assert!(run(&gone, &Arc::new(AtomicBool::new(false)), &tx).is_err());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn cache_identity_tells_a_scope_from_its_base() {
        let dir = p("/tmp");
        // The cache-set preset and a plain walk of the base must never
        // share a file.
        assert_ne!(
            cache_path_in(&dir, &[p("/r")]),
            cache_path_in(&dir, &[p("/r/a"), p("/r/b")])
        );
    }

    #[test]
    fn legacy_cache_without_roots_still_loads() {
        let dir = env::temp_dir().join(format!("zstats-disklegacy-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // A file from before the `roots` key: same document minus that
        // line — must load as the single-root walk it always was.
        let stripped: String = serialise(&run_with(100, 1))
            .lines()
            .filter(|l| !l.starts_with("roots"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(cache_path_in(&dir, &[p("/r")]), stripped).unwrap();
        let loaded = load_cache_in(&dir, &[p("/r")]).expect("legacy file loads");
        assert_eq!(loaded.roots, vec![p("/r")]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_rotates_the_displaced_run_into_the_delta_baseline() {
        let dir = env::temp_dir().join(format!("zstats-diskprev-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        let prev_path = prev_cache_path_in(&dir, &[p("/r")]);
        // First-ever save: nothing to rotate, no baseline yet.
        save_cache_in(&dir, &run_with(100, 1), true);
        assert!(load_cache_file(&prev_path, &[p("/r")]).is_none());
        // Second save: the displaced first run becomes the baseline.
        save_cache_in(&dir, &run_with(250, 2), true);
        let prev = load_cache_file(&prev_path, &[p("/r")]).expect("baseline");
        assert_eq!(prev.dirs[0].bytes, 100);
        // A pruning resave edits the current file only — displacing the
        // baseline mid-cycle would erase what deltas compare against.
        save_cache_in(&dir, &run_with(240, 2), false);
        let kept = load_cache_file(&prev_path, &[p("/r")]).expect("kept");
        assert_eq!(kept.dirs[0].bytes, 100);
        assert_eq!(load_cache_in(&dir, &[p("/r")]).unwrap().dirs[0].bytes, 240);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_spares_kept_scopes_and_young_files() {
        let dir = env::temp_dir().join(format!("zstats-disksweep-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let kept = vec![p("/r")];
        let orphan = vec![p("/old")];
        fs::write(cache_path_in(&dir, &kept), "x").unwrap();
        fs::write(prev_cache_path_in(&dir, &kept), "x").unwrap();
        fs::write(cache_path_in(&dir, &orphan), "x").unwrap();
        // Everything is younger than a month; nothing may go yet.
        sweep_orphans_in(&dir, &[&kept], ORPHAN_AGE);
        assert!(cache_path_in(&dir, &orphan).exists());
        // Age floor zero: the orphan expires, the kept pair survives on
        // identity, not on youth.
        sweep_orphans_in(&dir, &[&kept], Duration::ZERO);
        assert!(!cache_path_in(&dir, &orphan).exists());
        assert!(cache_path_in(&dir, &kept).exists());
        assert!(prev_cache_path_in(&dir, &kept).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_baseline_flattens_every_table_and_claims_nothing_more() {
        let base = DiffBaseline::from_result(&run_with(20, 1));
        assert_eq!(base.bytes_for(&p("/r/cache")), Some(10));
        assert_eq!(base.bytes_for(&p("/r/big")), Some(20));
        assert_eq!(base.bytes_for(&p("/r/.blob")), Some(30));
        // Absent ≠ new: the path may have fallen below retention.
        assert_eq!(base.bytes_for(&p("/r/unseen")), None);
        assert_eq!(base.roots(), [p("/r")]);
    }

    #[test]
    fn cap_table_extends_only_while_the_tail_stays_heavy() {
        let mb = |n: u64| n * 1024 * 1024;
        let capped = |sizes: Vec<u64>| default_rows(&sizes, |b| *b);
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
