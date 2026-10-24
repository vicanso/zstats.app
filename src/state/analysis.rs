//! Directory analyser and the Hardware-tab large-file query.
//!
//! Both one-shots used to live inline in the store. The analyser
//! survives hide (a `~/Library` walk is minutes, and hide-reset would
//! mean no scan ever finishes); the large-file listing does not.
//! Grouped here so a change to either does not mean hunting through
//! the rest of [`super::ZStatsAppState`].

use super::effects::{self, Effects};
use crate::diskscan::{self, DiffBaseline, ScanEvent, ScanResult, ScanScope};
use crate::diskwatch::{self, Growth, Report};
use crate::finder;
use crate::i18n;
use crate::prefs;
use std::collections::HashMap;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use super::ZStatsAppState;

/// One growth banner: the directory and the span it grew over.
#[derive(Clone, Debug)]
pub struct GrowthNotice {
    pub growth: Growth,
    pub over: Duration,
}

/// How long an announced growth stays on the Alerts tab's card — the
/// banner's landing spot. Until the next daily check could have
/// announced something new.
const GROWTH_CARD_FOR: Duration = diskwatch::CHECK_EVERY;

/// The card's rows: announced within [`GROWTH_CARD_FOR`] of `now`, the
/// newest batch first and a batch by how much it grew, each with how
/// long ago it was announced.
///
/// Against one `now` and one stamp per batch. Each row used to take its
/// own `elapsed()`, and the rows of one check its own `Instant::now()`
/// a few microseconds apart, so within a batch the sort key was the
/// difference of two timings and nothing else: the three directories of
/// one check changed places from one repaint to the next.
fn announced_order(
    announced: &[(GrowthNotice, Instant)],
    now: Instant,
) -> Vec<(GrowthNotice, Duration)> {
    let mut out: Vec<(GrowthNotice, Duration)> = announced
        .iter()
        .map(|(notice, at)| (notice.clone(), now.saturating_duration_since(*at)))
        .filter(|(_, ago)| *ago < GROWTH_CARD_FOR)
        .collect();
    out.sort_by(|(a, a_ago), (b, b_ago)| {
        a_ago
            .cmp(b_ago)
            .then_with(|| b.growth.grew.cmp(&a.growth.grew))
            .then_with(|| a.growth.path.cmp(&b.growth.path))
    });
    out
}

/// The disk-space window's three tabs, in the order the answers are
/// reached for: what is big by directory, the biggest single files,
/// then files stored twice. Kept in the store, not the window: it is
/// orientation, so it survives the window's close for the session like
/// the panel's tab does — a search left running on Duplicates is where
/// the window reopens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StorageTab {
    #[default]
    Analysis,
    Duplicates,
}

impl StorageTab {
    /// Two since 2026-10-16. "Large files" was a third, asking the
    /// Spotlight index for files over 500 MB, while the analysis listed
    /// the big files the index could not see — two lists of big files
    /// split by an implementation detail. The analysis lists them all
    /// now, and the tab went.
    pub const ALL: [StorageTab; 2] = [StorageTab::Analysis, StorageTab::Duplicates];

    pub fn index(self) -> usize {
        match self {
            StorageTab::Analysis => 0,
            StorageTab::Duplicates => 1,
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            StorageTab::Analysis => "disk.tab_analysis",
            StorageTab::Duplicates => "disk.tab_dupes",
        }
    }
}

/// What the daily check is told about the moment (`maybe_check_disk`).
pub struct DiskCheckMoment {
    pub on_battery: bool,
    pub cpu_percent: Option<f32>,
    pub someone_looking: bool,
    /// How much of the machine one thread is (`diskwatch::hold`).
    pub logical_cores: u32,
}

/// The directory analyser (docs/disk-analysis.md). Deliberately NOT reset
/// on hide, unlike every other one-shot: a `~/Library` walk is minutes,
/// and the panel auto-hides on any focus loss — hide-resets would mean no
/// scan ever finishes. Only the explicit cancel stops one.
#[derive(Default)]
pub enum DiskAnalysis {
    #[default]
    Off,
    Running {
        run_id: u64,
        dirs_done: usize,
        /// What this walk covers — named in the progress caption, so a
        /// user returning mid-run knows *which* scope is being walked.
        scope: ScanScope,
        /// The latest mid-walk snapshot — lower bounds that only grow,
        /// rendered under the running banner so minutes-long walks pay
        /// out from their first seconds.
        partial: Option<ScanResult>,
        /// The last finished walk of this scope's directory count — what
        /// the progress line measures against ("about 45% of last
        /// time"). `None` on a first walk of a scope, where there is
        /// nothing honest to compare with.
        expected_dirs: Option<usize>,
        /// Whether a finished result is written to the per-root cache.
        /// True for top-level analyses (the "last analysed X" a fresh
        /// launch opens with). Expansion sub-walks never come through
        /// here at all — they write into `expanded`, not the card.
        persist: bool,
        cancel: Arc<AtomicBool>,
    },
    Ready(ScanResult),
    Failed(String),
}

/// What sits under an opened directory row (`toggle_expansion`).
///
/// `Ready` is the ranked directory table for that path — it may be
/// empty, and empty is an answer: nothing inside cleared the bar the
/// tables rank by. There is no `Ready`-from-index vs `Ready`-from-walk
/// distinction on purpose; the rows are built by the same `tables()`
/// either way, and where they came from would only invite the reader to
/// trust one over the other.
pub enum Expansion {
    /// The index had nothing recorded here, so a walk of this subtree is
    /// running. Seconds, and only ever one at a time.
    Walking,
    Ready(Vec<diskscan::DirHit>),
    Failed,
}

/// The store's disk-analysis cluster: the analyser (survives hide), the
/// window's tab and what its rows have moved to the Trash. Fields stay
/// on the store via this struct so later analysis work has one place to
/// land.
pub(crate) struct Analysis {
    storage_tab: StorageTab,
    disk_analysis: DiskAnalysis,
    expanded: HashMap<PathBuf, Expansion>,
    expand_runs: u64,
    expand_cancel: Option<Arc<AtomicBool>>,
    disk_analysis_root: Option<ScanScope>,
    analysis_diff: Option<DiffBaseline>,
    /// Where the directory map stands: each directory opened from the
    /// result's root down, with the size its tile had. Empty is the root.
    /// The sizes ride along because a result restored from its cache has
    /// no index to look a total up in, and the map still needs one to say
    /// what part of a folder its subfolders are.
    map_trail: Vec<(PathBuf, u64)>,
    /// Every cleanup suggestion listed, not just the first few.
    analysis_show_all_sugs: bool,
    /// The excluded-folders drawer under the toolbar is open.
    analysis_exclude_open: bool,
    disk_analysis_runs: u64,
    /// Bytes this session moved to the Trash from the disk-space window.
    /// A move frees nothing until the Trash is emptied, and the window
    /// says so with this figure — otherwise "Trash all · 26.8 GB" reads
    /// as 26.8 GB back and the volume card does not move.
    trashed: u64,
    /// When the last walk of the home scope finished — the user's or the
    /// daily check's. Seeded at launch from that scope's cache file;
    /// `None` until a home walk has ever finished, which keeps the daily
    /// check off (`diskwatch::due`).
    watch_last: Option<SystemTime>,
    /// The daily check in flight.
    watch_cancel: Option<Arc<AtomicBool>>,
    /// Set while the check in flight holds still (`diskwatch::hold`);
    /// its walk waits on it. One per check.
    watch_hold: Arc<AtomicBool>,
    /// Why it holds, for the log's two lines and the badge on this
    /// app's row.
    watch_held: Option<diskwatch::Hold>,
    /// When the daily check last stopped — finished, failed or
    /// cancelled. Its CPU outlives it in zstats' averages
    /// (`diskwatch::explains_own_cpu`).
    watch_ended: Option<Instant>,
    /// What grew since about a week ago (`diskwatch::latest_report`),
    /// for the disk-space window. Re-read after every home walk.
    growth: Option<Report>,
    /// Growth banners the metrics loop has yet to post.
    growth_pending: Vec<GrowthNotice>,
    /// Growth announced by a banner, and when — the Alerts tab's card.
    growth_announced: Vec<(GrowthNotice, Instant)>,
}

impl Default for Analysis {
    fn default() -> Self {
        // The scope a fresh launch restores: the last finished top-level
        // walk's, from app.toml — or the default home walk when the key
        // is absent. Restoring the scope also restores what "re-analyze"
        // means, same as if the user had just picked it.
        let restored: Option<ScanScope> = {
            let roots = prefs::analysis_roots();
            (!roots.is_empty()).then(|| ScanScope {
                // The cache-set preset is the only multi-root producer,
                // and its base is home; a single stored root is its own
                // base — the same derivation `ScanScope`'s constructors
                // use.
                base: if roots.len() > 1 {
                    diskscan::default_root().unwrap_or_else(|| roots[0].clone())
                } else {
                    roots[0].clone()
                },
                roots,
            })
        };
        let launch_roots: Vec<PathBuf> = restored
            .as_ref()
            .map(|s| s.roots.clone())
            .or_else(|| diskscan::default_root().map(|home| vec![home]))
            .unwrap_or_default();
        // Cache pairs no launch can restore any more (scopes analysed
        // once and abandoned) age out here — a handful of stats.
        diskscan::sweep_orphans(&[
            &diskscan::default_root()
                .map(|h| vec![h])
                .unwrap_or_default(),
            &launch_roots,
        ]);
        // The daily check's clock: the home scope's last finished walk,
        // whichever scope the launch restores.
        let watch_last = diskscan::default_root()
            .and_then(|home| diskscan::load_cache(&[home]))
            .map(|result| result.scanned_at);
        Self {
            storage_tab: StorageTab::default(),
            // A fresh launch opens with the last finished analysis, if
            // one was cached — "see last time's numbers first".
            disk_analysis: (!launch_roots.is_empty())
                .then(|| diskscan::load_cache(&launch_roots))
                .flatten()
                .map(DiskAnalysis::Ready)
                .unwrap_or_default(),
            expanded: HashMap::new(),
            expand_runs: 0,
            expand_cancel: None,
            disk_analysis_root: restored,
            // The baseline outlives restarts the same way the result
            // does: through its file.
            analysis_diff: (!launch_roots.is_empty())
                .then(|| diskscan::load_prev_cache(&launch_roots))
                .flatten()
                .map(|prev| DiffBaseline::from_result(&prev)),
            map_trail: Vec::new(),
            analysis_show_all_sugs: false,
            analysis_exclude_open: false,
            trashed: 0,
            watch_last,
            watch_cancel: None,
            watch_hold: Arc::new(AtomicBool::new(false)),
            watch_held: None,
            watch_ended: None,
            growth: diskwatch::latest_report(),
            growth_pending: Vec::new(),
            growth_announced: Vec::new(),
            disk_analysis_runs: 0,
        }
    }
}

impl ZStatsAppState {
    pub fn storage_tab(&self) -> StorageTab {
        self.analysis.storage_tab
    }

    pub fn set_storage_tab(&mut self, tab: StorageTab, fx: &mut dyn Effects) {
        if self.analysis.storage_tab != tab {
            self.analysis.storage_tab = tab;
            fx.changed();
        }
    }

    /// ⌘R in the disk-space window: run the selected tab again. Never a
    /// cancel — a stray ⌘R must not throw away a walk minutes in — so a
    /// tab that is already working ignores it.
    pub fn rerun_storage_tab(&mut self, fx: &mut dyn Effects) {
        match self.analysis.storage_tab {
            StorageTab::Analysis => {
                if !matches!(self.analysis.disk_analysis, DiskAnalysis::Running { .. }) {
                    self.start_disk_analysis(fx);
                }
            }
            StorageTab::Duplicates => {
                if !self.dupes_running() {
                    self.start_dupes(fx);
                }
            }
        }
    }

    // ---- directory analyser --------------------------------------------

    pub fn disk_analysis(&self) -> &DiskAnalysis {
        &self.analysis.disk_analysis
    }

    /// The Δ baseline for `result` — present only when a previous run of
    /// the *same scope* exists, so drill views and freshly-picked roots
    /// never show half-comparable deltas.
    pub fn analysis_diff_for(&self, result: &ScanResult) -> Option<&DiffBaseline> {
        self.analysis
            .analysis_diff
            .as_ref()
            .filter(|diff| diff.roots() == result.roots)
    }

    /// The scope Analyze will walk: the session pick, or home when
    /// nothing has been chosen. The chips read this so Home lights up
    /// as the default rather than looking unselected.
    pub fn disk_analysis_scope(&self) -> Option<ScanScope> {
        self.analysis
            .disk_analysis_root
            .clone()
            .or_else(|| diskscan::default_root().map(ScanScope::single))
    }

    /// Remember a scope without walking it. Analyze is what starts the
    /// walk — picking a chip used to launch immediately, which made a
    /// mis-tap cost minutes and hid the selected state.
    pub fn set_disk_analysis_scope(&mut self, scope: ScanScope, fx: &mut dyn Effects) {
        self.analysis.disk_analysis_root = Some(scope);
        fx.changed();
    }

    /// Start (or restart) the top-level analysis — of the session's
    /// picked scope, or the home tree by default. A drill-down is left
    /// via "back", not by rescanning, so the stack is dropped here.
    pub fn start_disk_analysis(&mut self, fx: &mut dyn Effects) {
        let Some(scope) = self
            .analysis
            .disk_analysis_root
            .clone()
            .or_else(|| diskscan::default_root().map(ScanScope::single))
        else {
            self.analysis.disk_analysis = DiskAnalysis::Failed("HOME is not set".into());
            fx.changed();
            return;
        };
        self.launch_disk_analysis(scope, true, fx);
    }

    /// Point Analyze at a user-chosen root — the folder picker's
    /// entry. Does not walk: that is the chip's job. The bare root
    /// volume is refused rather than remembered: firmlinks
    /// double-count, and /System plus TCC would distort every figure
    /// (docs/disk-analysis.md's scope table) — the answer would be
    /// wrong, not merely slow.
    pub fn set_disk_analysis_at(&mut self, root: PathBuf, fx: &mut dyn Effects) {
        if root == Path::new("/") {
            self.cancel_disk_analysis_walk();
            self.drop_expansions();
            self.analysis.disk_analysis =
                DiskAnalysis::Failed(i18n::tr("disk.ana_root_unsupported"));
            fx.changed();
            return;
        }
        self.set_disk_analysis_scope(ScanScope::single(root), fx);
    }

    /// Point Analyze at the whole writable volume — the scope that can
    /// see what no home-shaped one can (`diskscan::whole_disk_root`
    /// explains why its root is not `/`).
    pub fn set_disk_analysis_whole_disk(&mut self, fx: &mut dyn Effects) {
        self.set_disk_analysis_scope(diskscan::ScanScope::whole_disk(), fx);
    }

    /// Point Analyze at the cache-set preset — the explicit cache roots
    /// merged into one ranked view (docs/disk-analysis.md's scope table).
    pub fn set_disk_analysis_caches(&mut self, fx: &mut dyn Effects) {
        let Some(scope) = ScanScope::cache_set() else {
            self.analysis.disk_analysis = DiskAnalysis::Failed("HOME is not set".into());
            fx.changed();
            return;
        };
        self.set_disk_analysis_scope(scope, fx);
    }

    /// Open or close one ranked directory, in place.
    ///
    /// This replaced a drill-down that made the clicked path the new root
    /// and rebuilt the whole card. The answer was the same; the cost was
    /// that everything else on screen moved, and a reader comparing two
    /// branches lost their place on every click. Children are inserted
    /// under the row instead, so nothing above it shifts.
    ///
    /// Two sources, and which one serves is invisible except in latency:
    /// the finished scan's retained index answers instantly wherever it
    /// recorded anything under this path (`diskscan::drill`), and the
    /// derived result shares the same `Arc`, so depth stays free. Folded
    /// leaves (`node_modules`, `.git`, a `CACHEDIR.TAG` tree) and
    /// interiors whose every child fell under `INDEX_FLOOR` were never
    /// recorded, and those take a real walk of that subtree — seconds,
    /// reported in the row itself.
    ///
    /// Only a finished result can be opened: mid-walk tables are lower
    /// bounds with no index behind them.
    pub fn toggle_expansion(&mut self, path: PathBuf, fx: &mut dyn Effects) {
        if self.analysis.expanded.remove(&path).is_some() {
            fx.changed();
            return;
        }
        let DiskAnalysis::Ready(current) = &self.analysis.disk_analysis else {
            return;
        };
        match diskscan::drill(current, &path) {
            Some(derived) => {
                self.analysis
                    .expanded
                    .insert(path, Expansion::Ready(derived.dirs));
                fx.changed();
            }
            None => self.walk_expansion(path, fx),
        }
    }

    /// The index had nothing under this row, so walk it. One at a time:
    /// a second open cancels the first, whose thread stops and whose
    /// events are dropped by the run-id guard either way.
    fn walk_expansion(&mut self, path: PathBuf, fx: &mut dyn Effects) {
        if let Some(cancel) = self.analysis.expand_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.analysis.expand_runs += 1;
        let run_id = self.analysis.expand_runs;
        let cancel = Arc::new(AtomicBool::new(false));
        self.analysis.expand_cancel = Some(cancel.clone());
        self.analysis
            .expanded
            .insert(path.clone(), Expansion::Walking);
        fx.changed();

        let (tx, rx) = smol::channel::unbounded::<ScanEvent>();
        diskscan::spawn(ScanScope::single(path.clone()), cancel, tx);
        effects::background(fx, move |reply| {
            while let Ok(event) = rx.recv_blocking() {
                // Progress and partials are dropped on purpose: a subtree
                // is seconds, and a row that reshuffles under the cursor
                // costs more than the wait it saves.
                let landed = match event {
                    ScanEvent::Done(result) => Expansion::Ready(result.dirs),
                    ScanEvent::Failed(e) => {
                        tracing::warn!("expand {}: {e}", path.display());
                        Expansion::Failed
                    }
                    _ => continue,
                };
                reply.apply(move |state, fx| {
                    // Superseded by a newer open, or the row was closed
                    // while the walk ran — either way this lands nowhere.
                    if state.analysis.expand_runs != run_id
                        || !matches!(state.analysis.expanded.get(&path), Some(Expansion::Walking))
                    {
                        return;
                    }
                    state.analysis.expanded.insert(path.clone(), landed);
                    fx.changed();
                });
                break;
            }
        });
    }

    /// What is under an opened row, or `None` when it is closed.
    pub fn expansion(&self, path: &Path) -> Option<&Expansion> {
        self.analysis.expanded.get(path)
    }

    /// Every open row closes when the result they describe goes away —
    /// a new walk, a cleared card — and the map goes back to the root.
    /// Children of a replaced result would be figures from a scan that is
    /// no longer on screen.
    fn drop_expansions(&mut self) {
        self.analysis.expanded.clear();
        self.analysis.map_trail.clear();
        if let Some(cancel) = self.analysis.expand_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// The directory map's way down from the root; empty is the root.
    pub fn map_trail(&self) -> &[(PathBuf, u64)] {
        &self.analysis.map_trail
    }

    /// Open `chain` in the map — one tile, or a tile and the parent tile
    /// it sits in, outermost first. Where the index has nothing recorded
    /// under the new level, the same one-at-a-time walk a row's
    /// expansion takes measures it, and the map says so until it lands.
    pub fn open_in_map(&mut self, chain: Vec<(PathBuf, u64)>, fx: &mut dyn Effects) {
        if chain.is_empty() || !matches!(self.analysis.disk_analysis, DiskAnalysis::Ready(_)) {
            return;
        }
        self.analysis.map_trail.extend(chain);
        self.measure_map_level(fx);
        fx.changed();
    }

    /// Back up the map to `depth` levels below the root (0 is the root).
    pub fn map_back_to(&mut self, depth: usize, fx: &mut dyn Effects) {
        self.analysis.map_trail.truncate(depth);
        fx.changed();
    }

    /// Walk the map's current level when neither the index nor an earlier
    /// walk can say what is in it — also the map's retry after a failure.
    pub fn measure_map_level(&mut self, fx: &mut dyn Effects) {
        let Some((path, _)) = self.analysis.map_trail.last().cloned() else {
            return;
        };
        let DiskAnalysis::Ready(current) = &self.analysis.disk_analysis else {
            return;
        };
        let indexed = current
            .index
            .as_ref()
            .and_then(|index| index.children_of(&path))
            .is_some_and(|kids| !kids.is_empty());
        let measured = matches!(
            self.analysis.expanded.get(&path),
            Some(Expansion::Ready(_) | Expansion::Walking)
        );
        if !indexed && !measured {
            self.walk_expansion(path, fx);
        }
    }

    pub fn analysis_show_all_sugs(&self) -> bool {
        self.analysis.analysis_show_all_sugs
    }

    pub fn toggle_analysis_show_all_sugs(&mut self, fx: &mut dyn Effects) {
        self.analysis.analysis_show_all_sugs = !self.analysis.analysis_show_all_sugs;
        fx.changed();
    }

    pub fn analysis_exclude_open(&self) -> bool {
        self.analysis.analysis_exclude_open
    }

    pub fn toggle_analysis_exclude(&mut self, fx: &mut dyn Effects) {
        self.analysis.analysis_exclude_open = !self.analysis.analysis_exclude_open;
        fx.changed();
    }

    /// Dismiss the analysis entirely — straight to Off, opened rows and
    /// all. This is a view action, not a disk one: nothing is touched on
    /// disk, and dropping the result also releases the retained index
    /// every opened row was served from.
    pub fn clear_disk_analysis(&mut self, fx: &mut dyn Effects) {
        self.cancel_disk_analysis_walk();
        // Clean slate includes the saved result — otherwise the next
        // launch would resurrect what the user just dismissed.
        let top_roots = match &self.analysis.disk_analysis {
            DiskAnalysis::Ready(r) => Some(r.roots.clone()),
            _ => None,
        };
        if let Some(roots) = top_roots {
            diskscan::delete_cache(&roots);
        }
        // The baseline's file went with the cache; the flattened copy
        // must not outlive it.
        self.analysis.analysis_diff = None;
        self.drop_expansions();
        // Clean slate includes the picked scope: the next "Analyze"
        // means the default home tree again — this launch and the next.
        self.analysis.disk_analysis_root = None;
        prefs::set_analysis_roots(&[]);
        self.analysis.disk_analysis = DiskAnalysis::Off;
        fx.changed();
    }

    /// The walk itself. Runs on its own thread; everything this state
    /// learns — progress, completion, failure — arrives over the channel
    /// drained below, guarded by `run_id` so a superseded run's late
    /// events fall on the floor.
    fn launch_disk_analysis(&mut self, scope: ScanScope, persist: bool, fx: &mut dyn Effects) {
        self.cancel_disk_analysis_walk();
        // The user's walk wins over the daily check, which can run
        // tomorrow; two walks would only compete for the disk.
        self.stop_disk_check();
        self.drop_expansions();
        self.analysis.disk_analysis_runs += 1;
        let run_id = self.analysis.disk_analysis_runs;
        let cancel = Arc::new(AtomicBool::new(false));
        // The result on screen when it is this scope's, else its cache
        // file (one small read, once per walk).
        let expected_dirs = match &self.analysis.disk_analysis {
            DiskAnalysis::Ready(result) if result.roots == scope.roots => Some(result.dirs_seen),
            _ => diskscan::load_cache(&scope.roots).map(|result| result.dirs_seen),
        };
        self.analysis.disk_analysis = DiskAnalysis::Running {
            run_id,
            dirs_done: 0,
            scope: scope.clone(),
            partial: None,
            expected_dirs,
            persist,
            cancel: cancel.clone(),
        };
        fx.changed();

        let (tx, rx) = smol::channel::unbounded::<ScanEvent>();
        diskscan::spawn(scope, cancel, tx);
        effects::background(fx, move |reply| {
            while let Ok(event) = rx.recv_blocking() {
                let done = matches!(event, ScanEvent::Done(_) | ScanEvent::Failed(_));
                reply.apply(move |state, fx| {
                    // Only the run that owns the current Running state may
                    // write; a cancelled or superseded run stays silent.
                    let owns = matches!(
                        state.analysis.disk_analysis,
                        DiskAnalysis::Running { run_id: id, .. } if id == run_id
                    );
                    if !owns {
                        return;
                    }
                    match event {
                        ScanEvent::Progress { dirs_done } => {
                            if let DiskAnalysis::Running { dirs_done: d, .. } =
                                &mut state.analysis.disk_analysis
                            {
                                *d = dirs_done;
                            }
                        }
                        ScanEvent::Partial(result) => {
                            if let DiskAnalysis::Running { partial, .. } =
                                &mut state.analysis.disk_analysis
                            {
                                *partial = Some(*result);
                            }
                        }
                        ScanEvent::Done(result) => {
                            // Only finished top-level walks reach the cache;
                            // cancelled and failed runs never get here, so a
                            // half table cannot overwrite a full one.
                            if let DiskAnalysis::Running { persist: true, .. } =
                                state.analysis.disk_analysis
                            {
                                // The save rotated the displaced run into
                                // `.prev` — read it back as the Δ baseline.
                                diskscan::save_cache(&result);
                                state.analysis.analysis_diff =
                                    diskscan::load_prev_cache(&result.roots)
                                        .map(|prev| DiffBaseline::from_result(&prev));
                                // Remember the scope the next launch
                                // restores; the default home walk is
                                // expressed as the absent key.
                                let is_default = diskscan::default_root()
                                    .is_some_and(|home| result.roots == [home]);
                                prefs::set_analysis_roots(if is_default {
                                    &[]
                                } else {
                                    &result.roots
                                });
                                if is_default {
                                    state.note_home_walk(&result);
                                }
                            }
                            state.analysis.disk_analysis = DiskAnalysis::Ready(*result);
                        }
                        ScanEvent::Failed(e) => {
                            state.analysis.disk_analysis = DiskAnalysis::Failed(e);
                        }
                    }
                    fx.changed();
                });
                if done {
                    break;
                }
            }
        });
    }

    /// The explicit cancel — the only way a walk stops early. Partial
    /// results are never kept, so this goes to Off rather than showing
    /// half a table.
    pub fn cancel_disk_analysis(&mut self, fx: &mut dyn Effects) {
        self.cancel_disk_analysis_walk();
        self.analysis.disk_analysis = DiskAnalysis::Off;
        fx.changed();
    }

    fn cancel_disk_analysis_walk(&self) {
        if let DiskAnalysis::Running { cancel, .. } = &self.analysis.disk_analysis {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    // ---- daily check ---------------------------------------------------

    /// A finished walk of the home scope, from the user or the daily
    /// check: today's history snapshot, the growth it implies, and the
    /// check's clock.
    fn note_home_walk(&mut self, result: &ScanResult) {
        diskwatch::record(result);
        self.analysis.growth = diskwatch::latest_report();
        self.analysis.watch_last = Some(result.scanned_at);
    }

    /// Stop the daily check if one is in flight — a walk the user asked
    /// for (the analyser's or the duplicate search's) needs the disk more.
    pub(super) fn stop_disk_check(&mut self) {
        if let Some(cancel) = self.analysis.watch_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
            self.analysis.watch_ended = Some(Instant::now());
            self.analysis.watch_held = None;
        }
    }

    /// Start the daily background check if it is due (`diskwatch::due`).
    /// Called every tick; everything before the spawn is a few compares.
    pub fn maybe_check_disk(&mut self, moment: DiskCheckMoment, fx: &mut dyn Effects) {
        // One in flight is asked whether to hold still instead.
        if self.analysis.watch_cancel.is_some() {
            self.hold_disk_check(&moment);
            return;
        }
        let walk_running = self.analysis.watch_cancel.is_some()
            || matches!(self.analysis.disk_analysis, DiskAnalysis::Running { .. })
            || self.dupes_running();
        let gate = diskwatch::Gate {
            enabled: prefs::disk_watch(),
            last_walk: self.analysis.watch_last,
            now: SystemTime::now(),
            on_battery: moment.on_battery,
            cpu_percent: moment.cpu_percent,
            someone_looking: moment.someone_looking,
            walk_running,
        };
        if !diskwatch::due(&gate) {
            return;
        }
        let Some(home) = diskscan::default_root() else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let hold = Arc::new(AtomicBool::new(false));
        self.analysis.watch_cancel = Some(cancel.clone());
        self.analysis.watch_hold = hold.clone();
        self.analysis.watch_held = None;
        let started = Instant::now();
        tracing::info!("daily disk check: started");
        let (tx, rx) = smol::channel::unbounded::<ScanEvent>();
        diskscan::spawn_background(ScanScope::single(home), cancel.clone(), hold, tx);
        effects::background(fx, move |reply| {
            while let Ok(event) = rx.recv_blocking() {
                match event {
                    ScanEvent::Progress { .. } | ScanEvent::Partial(_) => continue,
                    ScanEvent::Done(result) => {
                        reply.apply(move |state, fx| {
                            state.finish_disk_check(*result, &cancel, started, fx)
                        });
                    }
                    ScanEvent::Failed(e) => {
                        tracing::warn!(error = %e, "daily disk check failed");
                        reply.apply(move |state, _| {
                            if state
                                .analysis
                                .watch_cancel
                                .as_ref()
                                .is_some_and(|c| Arc::ptr_eq(c, &cancel))
                            {
                                state.analysis.watch_cancel = None;
                                state.analysis.watch_held = None;
                                state.analysis.watch_ended = Some(Instant::now());
                                // Not retried every tick: a failing walk
                                // waits a day like a finished one.
                                state.analysis.watch_last = Some(SystemTime::now());
                            }
                        });
                    }
                }
                break;
            }
        });
    }

    /// Hold the check in flight still, or let it go on (`diskwatch::hold`),
    /// logging each change at INFO — "why is zstats busy" and "why did
    /// the check take an hour" are both asked of the installed build.
    fn hold_disk_check(&mut self, moment: &DiskCheckMoment) {
        let held = diskwatch::hold(
            moment.on_battery,
            moment.cpu_percent,
            self.analysis.watch_held,
            moment.logical_cores,
        );
        if held == self.analysis.watch_held {
            return;
        }
        match held {
            Some(why) => tracing::info!(
                reason = why.as_str(),
                cpu = ?moment.cpu_percent,
                "daily disk check: holding"
            ),
            None => tracing::info!(cpu = ?moment.cpu_percent, "daily disk check: going on"),
        }
        self.analysis.watch_held = held;
        self.analysis
            .watch_hold
            .store(held.is_some(), Ordering::Relaxed);
    }

    /// The daily check finished: record it as the home scope's result
    /// (cache, history, the window's table if home is what it shows),
    /// then queue a banner for any climb past `diskwatch::NOTIFY_BYTES`
    /// not announced this week.
    fn finish_disk_check(
        &mut self,
        result: ScanResult,
        cancel: &Arc<AtomicBool>,
        started: Instant,
        fx: &mut dyn Effects,
    ) {
        // A user's walk cancelled this one; its own result is the answer.
        if cancel.load(Ordering::Relaxed)
            || !self
                .analysis
                .watch_cancel
                .as_ref()
                .is_some_and(|c| Arc::ptr_eq(c, cancel))
        {
            return;
        }
        self.analysis.watch_cancel = None;
        self.analysis.watch_held = None;
        self.analysis.watch_ended = Some(Instant::now());
        let walked = result.took;
        diskscan::save_cache(&result);
        self.note_home_walk(&result);
        // The window shows the home tree: the fresh walk replaces it,
        // and the ± now reads against the run it displaced. Any other
        // scope on screen is left alone — the check only ever walks home.
        let showing_home = match &self.analysis.disk_analysis {
            DiskAnalysis::Ready(shown) => shown.roots == result.roots,
            DiskAnalysis::Off => self.analysis.disk_analysis_root.is_none(),
            _ => false,
        };
        if showing_home {
            self.drop_expansions();
            self.analysis.analysis_diff = diskscan::load_prev_cache(&result.roots)
                .map(|prev| DiffBaseline::from_result(&prev));
            self.analysis.disk_analysis = DiskAnalysis::Ready(result);
        }
        let mut announced = 0usize;
        if let Some(report) = &self.analysis.growth {
            // One stamp for the whole batch: they were announced together,
            // and the card orders a batch by size (`announced_order`).
            let at = Instant::now();
            for growth in diskwatch::unannounced(report, SystemTime::now()) {
                let notice = GrowthNotice {
                    growth,
                    over: report.over,
                };
                self.analysis.growth_announced.push((notice.clone(), at));
                self.analysis.growth_pending.push(notice);
                announced += 1;
            }
        }
        tracing::info!(
            took_secs = started.elapsed().as_secs(),
            // `took` less the stretches it held still.
            walked_secs = walked.as_secs(),
            growing = self.analysis.growth.as_ref().map_or(0, |r| r.rows.len()),
            announced,
            "daily disk check: done"
        );
        fx.changed();
    }

    /// What grew since about a week ago, for the disk-space window.
    pub fn disk_growth(&self) -> Option<&Report> {
        self.analysis.growth.as_ref()
    }

    /// When the last home walk finished, for the window's daily-check
    /// line.
    pub fn disk_check_last(&self) -> Option<SystemTime> {
        self.analysis.watch_last
    }

    /// Whether the daily check is walking right now.
    pub fn disk_check_running(&self) -> bool {
        self.analysis.watch_cancel.is_some()
    }

    /// Why the daily check in flight is holding still, if it is.
    pub fn disk_check_held(&self) -> Option<diskwatch::Hold> {
        self.analysis.watch_held
    }

    /// Whether this process's CPU is the daily check's doing right now
    /// (`diskwatch::explains_own_cpu`): the window a CPU alert about us
    /// is dropped in, and the one this app's rows keep their ink in
    /// (`processes::own_check_cpu`).
    pub fn disk_check_explains_cpu(&self, now: Instant) -> bool {
        diskwatch::explains_own_cpu(
            self.analysis.watch_cancel.is_some(),
            self.analysis.watch_ended,
            now,
        )
    }

    /// Growth banners to post, once each.
    pub fn take_disk_growth_notices(&mut self) -> Vec<GrowthNotice> {
        mem::take(&mut self.analysis.growth_pending)
    }

    /// Growth announced within [`GROWTH_CARD_FOR`], newest first — the
    /// Alerts tab's card, where the banner's click lands.
    pub fn disk_growth_announced(&self) -> Vec<(GrowthNotice, Duration)> {
        announced_order(&self.analysis.growth_announced, Instant::now())
    }

    // ---- the Trash -----------------------------------------------------

    /// A big file's confirmed move to the Trash, from the analysis card:
    /// Finder's own move, then the row goes — from the result on screen
    /// and the cache that restores it. A failed move leaves the row: a
    /// file still on disk must not vanish from the list. Only the row is
    /// touched; the folders above it keep their scanned totals, with
    /// `scanned_at` as the staleness boundary, as for the caches.
    pub fn trash_analysis_file(&mut self, path: &Path, fx: &mut dyn Effects) {
        if let Err(e) = finder::trash(path) {
            tracing::warn!("trash {}: {e}", path.display());
            return;
        }
        let analysis = &mut self.analysis;
        if let DiskAnalysis::Ready(result) = &mut analysis.disk_analysis {
            if let Some(file) = result.files.iter().find(|f| f.path == path) {
                analysis.trashed = analysis.trashed.saturating_add(file.bytes);
            }
            result.files.retain(|f| f.path != path);
            diskscan::resave_if_cached(result);
        }
        fx.changed();
    }

    /// What [`Analysis::trashed`] has counted this session.
    pub fn trashed_this_session(&self) -> u64 {
        self.analysis.trashed
    }

    /// Count bytes a move to the Trash will give back once it is emptied.
    pub(super) fn note_trashed(&mut self, bytes: u64) {
        self.analysis.trashed = self.analysis.trashed.saturating_add(bytes);
    }

    /// The analyser's confirmed clear action: move each listed
    /// CACHEDIR.TAG tree to the Trash, then drop the rows that actually
    /// went. A failed trash leaves its row — a directory still on disk
    /// must not vanish from the list. Only rows are touched; every other
    /// figure stays as scanned, with `scanned_at` as the staleness
    /// boundary.
    pub fn trash_regenerable(&mut self, paths: &[PathBuf], fx: &mut dyn Effects) {
        let mut gone: Vec<&PathBuf> = Vec::new();
        for path in paths {
            match finder::trash(path) {
                Ok(()) => gone.push(path),
                Err(e) => tracing::warn!("trash {}: {e}", path.display()),
            }
        }
        if gone.is_empty() {
            return;
        }
        // Each gone path's size, from the rows that listed it, before the
        // prune below drops them.
        if let DiskAnalysis::Ready(result) = &self.analysis.disk_analysis {
            let moved: u64 = gone
                .iter()
                .filter_map(|path| {
                    result
                        .suggestions
                        .iter()
                        .chain(&result.regenerable)
                        .chain(&result.dirs)
                        .find(|hit| &&hit.path == path)
                        .map(|hit| hit.bytes)
                })
                .sum();
            self.analysis.trashed = self.analysis.trashed.saturating_add(moved);
        }
        // Prune every level, not just the visible one — a parked outer
        // result restored via "back" must not resurrect trashed rows.
        let prune = |result: &mut diskscan::ScanResult| {
            result.regenerable.retain(|h| !gone.contains(&&h.path));
            // A dominance chase can land the same tree in the directory
            // table, and big files inside a trashed tree went with it —
            // those rows would dangle.
            result.dirs.retain(|h| !gone.contains(&&h.path));
            result
                .files
                .retain(|f| !gone.iter().any(|g| f.path.starts_with(g)));
            result
                .suggestions
                .retain(|h| !gone.iter().any(|g| h.path.starts_with(g)));
        };
        // The card always shows the session's top-level result now
        // (opening a row nests under it instead of replacing it), so the
        // one result on screen is exactly the one that owns a cache file.
        if let DiskAnalysis::Ready(result) = &mut self.analysis.disk_analysis {
            prune(result);
            diskscan::resave_if_cached(result);
        }
        // Opened rows are tables too: a trashed tree must not survive as
        // somebody's child row, and a row for the tree itself closes.
        self.analysis
            .expanded
            .retain(|path, _| !gone.iter().any(|g| path.starts_with(g)));
        for state in self.analysis.expanded.values_mut() {
            if let Expansion::Ready(rows) = state {
                rows.retain(|h| !gone.iter().any(|g| h.path.starts_with(g)));
            }
        }
        fx.changed();
    }

    /// A freshly built disk-space window starts with the suggestions
    /// and the dirs table folded back to their default length. Only on a *new* window: raising one that is already open
    /// must not wipe what its owner is reading.
    ///
    /// The analysis result itself survives on purpose — it costs minutes
    /// to produce and is cached to disk across restarts; the caption says
    /// how old it is.
    pub fn reset_storage_views(&mut self, fx: &mut dyn Effects) {
        self.analysis.analysis_show_all_sugs = false;
        self.analysis.analysis_exclude_open = false;
        // Opened rows are questions too — a window opened tomorrow should
        // show the result the way a finished scan leaves it, not a tree
        // somebody unfolded yesterday.
        self.drop_expansions();
        self.reset_dupe_views();
        fx.changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn notice(path: &str, grew: u64) -> GrowthNotice {
        GrowthNotice {
            growth: Growth {
                path: PathBuf::from(path),
                grew,
                now: grew,
            },
            over: Duration::from_secs(7 * 86_400),
        }
    }

    /// One check announces several directories at once. They keep one
    /// order — the newest batch first, a batch by how much it grew —
    /// however often the card is asked, and leave it once the card's
    /// span is over.
    #[test]
    fn the_growth_card_keeps_one_order() {
        // Counted forward from a base: an Instant cannot go back past
        // the machine's boot, which a test runner may be close to.
        let base = Instant::now();
        let now = base + Duration::from_secs(48 * 3600);
        let today = now - Duration::from_secs(60);
        let yesterday = base + Duration::from_secs(28 * 3600);
        let announced = vec![
            (notice("/old", 9), base),
            (notice("/c", 6), yesterday),
            (notice("/a", 95), today),
            (notice("/a/deps", 50), today),
            (notice("/a/inc", 40), today),
        ];
        let paths = |rows: Vec<(GrowthNotice, Duration)>| -> Vec<PathBuf> {
            rows.into_iter().map(|(n, _)| n.growth.path).collect()
        };
        let order = paths(announced_order(&announced, now));
        assert_eq!(order, ["/a", "/a/deps", "/a/inc", "/c"].map(PathBuf::from));
        assert_eq!(
            paths(announced_order(&announced, now + Duration::from_secs(30))),
            order,
            "asked again, the same order"
        );
    }
}
