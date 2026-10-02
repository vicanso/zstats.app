//! Directory analyser and the Hardware-tab large-file query.
//!
//! Both one-shots used to live inline in the store. The analyser
//! survives hide (a `~/Library` walk is minutes, and hide-reset would
//! mean no scan ever finishes); the large-file listing does not.
//! Grouped here so a change to either does not mean hunting through
//! the rest of [`super::ZStatsAppState`].

use crate::bigfiles;
use crate::bigfiles::BigFilesScan;
use crate::diskscan::{self, DiffBaseline, ScanEvent, ScanResult, ScanScope};
use crate::diskwatch::{self, Growth, Report};
use crate::i18n;
use crate::prefs;
use gpui::Context;
use std::collections::{HashMap, HashSet};
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
    LargeFiles,
    Duplicates,
}

impl StorageTab {
    pub const ALL: [StorageTab; 3] = [
        StorageTab::Analysis,
        StorageTab::LargeFiles,
        StorageTab::Duplicates,
    ];

    pub fn index(self) -> usize {
        match self {
            StorageTab::Analysis => 0,
            StorageTab::LargeFiles => 1,
            StorageTab::Duplicates => 2,
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            StorageTab::Analysis => "disk.tab_analysis",
            StorageTab::LargeFiles => "disk.tab_large",
            StorageTab::Duplicates => "disk.tab_dupes",
        }
    }
}

/// What the daily check is told about the moment (`maybe_check_disk`).
pub struct DiskCheckMoment {
    pub on_battery: bool,
    pub cpu_percent: Option<f32>,
    pub someone_looking: bool,
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

/// The Hardware tab's one-shot large-file query, same lifecycle shape as
/// the full process scans: `Off → Running → Ready/Failed`, reset on hide.
#[derive(Default)]
pub enum BigFiles {
    #[default]
    Off,
    Running,
    Ready {
        scan: BigFilesScan,
        /// Rows the previous listing would have shown and did not — see
        /// [`bigfiles::Baseline::is_new`]. Empty when there was nothing
        /// to compare against, which is not the same as "nothing is new".
        added: HashSet<PathBuf>,
        /// When that previous listing was taken. `None` on a first run,
        /// where marking everything new would say nothing at all.
        since: Option<SystemTime>,
    },
    /// `indexing_off` selects the honest message: a disabled Spotlight
    /// index would otherwise masquerade as "no big files".
    Failed {
        indexing_off: bool,
    },
}

/// The store's disk-analysis cluster: the analyser (survives hide) and
/// the large-file query (reset on hide). Fields stay on the store via
/// this struct so later analysis work has one place to land.
pub(crate) struct Analysis {
    storage_tab: StorageTab,
    big_files: BigFiles,
    disk_analysis: DiskAnalysis,
    expanded: HashMap<PathBuf, Expansion>,
    expand_runs: u64,
    expand_cancel: Option<Arc<AtomicBool>>,
    disk_analysis_root: Option<ScanScope>,
    analysis_diff: Option<DiffBaseline>,
    analysis_show_all_dirs: bool,
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
            big_files: BigFiles::default(),
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
            analysis_show_all_dirs: false,
            trashed: 0,
            watch_last,
            watch_cancel: None,
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

    pub fn set_storage_tab(&mut self, tab: StorageTab, cx: &mut Context<Self>) {
        if self.analysis.storage_tab != tab {
            self.analysis.storage_tab = tab;
            cx.notify();
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
    pub fn set_disk_analysis_scope(&mut self, scope: ScanScope, cx: &mut Context<Self>) {
        self.analysis.disk_analysis_root = Some(scope);
        cx.notify();
    }

    /// Start (or restart) the top-level analysis — of the session's
    /// picked scope, or the home tree by default. A drill-down is left
    /// via "back", not by rescanning, so the stack is dropped here.
    pub fn start_disk_analysis(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self
            .analysis
            .disk_analysis_root
            .clone()
            .or_else(|| diskscan::default_root().map(ScanScope::single))
        else {
            self.analysis.disk_analysis = DiskAnalysis::Failed("HOME is not set".into());
            cx.notify();
            return;
        };
        self.launch_disk_analysis(scope, true, cx);
    }

    /// Point Analyze at a user-chosen root — the folder picker's
    /// entry. Does not walk: that is the chip's job. The bare root
    /// volume is refused rather than remembered: firmlinks
    /// double-count, and /System plus TCC would distort every figure
    /// (docs/disk-analysis.md's scope table) — the answer would be
    /// wrong, not merely slow.
    pub fn set_disk_analysis_at(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if root == Path::new("/") {
            self.cancel_disk_analysis_walk();
            self.drop_expansions();
            self.analysis.disk_analysis =
                DiskAnalysis::Failed(i18n::tr("disk.ana_root_unsupported"));
            cx.notify();
            return;
        }
        self.set_disk_analysis_scope(ScanScope::single(root), cx);
    }

    /// Point Analyze at the whole writable volume — the scope that can
    /// see what no home-shaped one can (`diskscan::whole_disk_root`
    /// explains why its root is not `/`).
    pub fn set_disk_analysis_whole_disk(&mut self, cx: &mut Context<Self>) {
        self.set_disk_analysis_scope(diskscan::ScanScope::whole_disk(), cx);
    }

    /// Point Analyze at the cache-set preset — the explicit cache roots
    /// merged into one ranked view (docs/disk-analysis.md's scope table).
    pub fn set_disk_analysis_caches(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = ScanScope::cache_set() else {
            self.analysis.disk_analysis = DiskAnalysis::Failed("HOME is not set".into());
            cx.notify();
            return;
        };
        self.set_disk_analysis_scope(scope, cx);
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
    pub fn toggle_expansion(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.analysis.expanded.remove(&path).is_some() {
            cx.notify();
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
                cx.notify();
            }
            None => self.walk_expansion(path, cx),
        }
    }

    /// The index had nothing under this row, so walk it. One at a time:
    /// a second open cancels the first, whose thread stops and whose
    /// events are dropped by the run-id guard either way.
    fn walk_expansion(&mut self, path: PathBuf, cx: &mut Context<Self>) {
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
        cx.notify();

        let (tx, rx) = smol::channel::unbounded::<ScanEvent>();
        diskscan::spawn(ScanScope::single(path.clone()), cancel, tx);
        cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
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
                let _ = this.update(cx, |state, cx| {
                    // Superseded by a newer open, or the row was closed
                    // while the walk ran — either way this lands nowhere.
                    if state.analysis.expand_runs != run_id
                        || !matches!(state.analysis.expanded.get(&path), Some(Expansion::Walking))
                    {
                        return;
                    }
                    state.analysis.expanded.insert(path.clone(), landed);
                    cx.notify();
                });
                break;
            }
        })
        .detach();
    }

    /// What is under an opened row, or `None` when it is closed.
    pub fn expansion(&self, path: &Path) -> Option<&Expansion> {
        self.analysis.expanded.get(path)
    }

    /// Every open row closes when the result they describe goes away —
    /// a new walk, a cleared card. Children of a replaced result would
    /// be figures from a scan that is no longer on screen.
    fn drop_expansions(&mut self) {
        self.analysis.expanded.clear();
        if let Some(cancel) = self.analysis.expand_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn analysis_show_all_dirs(&self) -> bool {
        self.analysis.analysis_show_all_dirs
    }

    pub fn set_analysis_show_all_dirs(&mut self, show: bool, cx: &mut Context<Self>) {
        self.analysis.analysis_show_all_dirs = show;
        cx.notify();
    }

    /// Dismiss the analysis entirely — straight to Off, opened rows and
    /// all. This is a view action, not a disk one: nothing is touched on
    /// disk, and dropping the result also releases the retained index
    /// every opened row was served from.
    pub fn clear_disk_analysis(&mut self, cx: &mut Context<Self>) {
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
        cx.notify();
    }

    /// The walk itself. Runs on its own thread; everything this state
    /// learns — progress, completion, failure — arrives over the channel
    /// drained below, guarded by `run_id` so a superseded run's late
    /// events fall on the floor.
    fn launch_disk_analysis(&mut self, scope: ScanScope, persist: bool, cx: &mut Context<Self>) {
        self.cancel_disk_analysis_walk();
        // The user's walk wins over the daily check, which can run
        // tomorrow; two walks would only compete for the disk.
        self.stop_disk_check();
        self.drop_expansions();
        self.analysis.disk_analysis_runs += 1;
        let run_id = self.analysis.disk_analysis_runs;
        let cancel = Arc::new(AtomicBool::new(false));
        self.analysis.disk_analysis = DiskAnalysis::Running {
            run_id,
            dirs_done: 0,
            scope: scope.clone(),
            partial: None,
            persist,
            cancel: cancel.clone(),
        };
        cx.notify();

        let (tx, rx) = smol::channel::unbounded::<ScanEvent>();
        diskscan::spawn(scope, cancel, tx);
        cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                let done = matches!(event, ScanEvent::Done(_) | ScanEvent::Failed(_));
                let _ = this.update(cx, |state, cx| {
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
                    cx.notify();
                });
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    /// The explicit cancel — the only way a walk stops early. Partial
    /// results are never kept, so this goes to Off rather than showing
    /// half a table.
    pub fn cancel_disk_analysis(&mut self, cx: &mut Context<Self>) {
        self.cancel_disk_analysis_walk();
        self.analysis.disk_analysis = DiskAnalysis::Off;
        cx.notify();
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
        }
    }

    /// Start the daily background check if it is due (`diskwatch::due`).
    /// Called every tick; everything before the spawn is a few compares.
    pub fn maybe_check_disk(&mut self, moment: DiskCheckMoment, cx: &mut Context<Self>) {
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
        self.analysis.watch_cancel = Some(cancel.clone());
        let started = Instant::now();
        tracing::info!("daily disk check: started");
        let (tx, rx) = smol::channel::unbounded::<ScanEvent>();
        diskscan::spawn_paced(
            ScanScope::single(home),
            diskscan::Pace::Background,
            cancel.clone(),
            tx,
        );
        cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                match event {
                    ScanEvent::Progress { .. } | ScanEvent::Partial(_) => continue,
                    ScanEvent::Done(result) => {
                        let _ = this.update(cx, |state, cx| {
                            state.finish_disk_check(*result, &cancel, started, cx)
                        });
                    }
                    ScanEvent::Failed(e) => {
                        tracing::warn!(error = %e, "daily disk check failed");
                        let _ = this.update(cx, |state, _| {
                            if state
                                .analysis
                                .watch_cancel
                                .as_ref()
                                .is_some_and(|c| Arc::ptr_eq(c, &cancel))
                            {
                                state.analysis.watch_cancel = None;
                                // Not retried every tick: a failing walk
                                // waits a day like a finished one.
                                state.analysis.watch_last = Some(SystemTime::now());
                            }
                        });
                    }
                }
                break;
            }
        })
        .detach();
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
        cx: &mut Context<Self>,
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
            for growth in diskwatch::unannounced(report, SystemTime::now()) {
                let notice = GrowthNotice {
                    growth,
                    over: report.over,
                };
                self.analysis
                    .growth_announced
                    .push((notice.clone(), Instant::now()));
                self.analysis.growth_pending.push(notice);
                announced += 1;
            }
        }
        tracing::info!(
            took_secs = started.elapsed().as_secs(),
            growing = self.analysis.growth.as_ref().map_or(0, |r| r.rows.len()),
            announced,
            "daily disk check: done"
        );
        cx.notify();
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

    /// Growth banners to post, once each.
    pub fn take_disk_growth_notices(&mut self) -> Vec<GrowthNotice> {
        mem::take(&mut self.analysis.growth_pending)
    }

    /// Growth announced within [`GROWTH_CARD_FOR`], newest first — the
    /// Alerts tab's card, where the banner's click lands.
    pub fn disk_growth_announced(&self) -> Vec<(GrowthNotice, Duration)> {
        let mut out: Vec<(GrowthNotice, Duration)> = self
            .analysis
            .growth_announced
            .iter()
            .filter(|(_, at)| at.elapsed() < GROWTH_CARD_FOR)
            .map(|(notice, at)| (notice.clone(), at.elapsed()))
            .collect();
        out.sort_by_key(|(_, ago)| *ago);
        out
    }

    // ---- large files ---------------------------------------------------

    pub fn big_files(&self) -> &BigFiles {
        &self.analysis.big_files
    }

    /// Run (or re-run) the large-file query on the background executor.
    pub fn start_big_files(&mut self, cx: &mut Context<Self>) {
        if matches!(self.analysis.big_files, BigFiles::Running) {
            return;
        }
        self.analysis.big_files = BigFiles::Running;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let scanned = cx
                .background_executor()
                .spawn(async { bigfiles::scan() })
                .await;
            let _ = this.update(cx, |state, cx| {
                // Same landing guard as the full scans: a hide mid-query
                // reset this to Off, and the result must not undo that.
                if !matches!(state.analysis.big_files, BigFiles::Running) {
                    return;
                }
                state.analysis.big_files = match scanned {
                    Ok(scan) => {
                        // Compare first, then rotate: the baseline this
                        // run is measured against is the one on disk
                        // before it, and every finished query becomes the
                        // next one's — so "new" always means "since you
                        // last looked", with the caption naming when that
                        // was.
                        let baseline = bigfiles::load_baseline();
                        let added = baseline
                            .as_ref()
                            .map(|base| {
                                scan.files
                                    .iter()
                                    .filter(|f| base.is_new(f))
                                    .map(|f| f.path.clone())
                                    .collect()
                            })
                            .unwrap_or_default();
                        let since = baseline.as_ref().map(bigfiles::Baseline::at);
                        bigfiles::save_baseline(&scan);
                        BigFiles::Ready { scan, added, since }
                    }
                    #[cfg(target_os = "macos")]
                    Err(bigfiles::ScanError::IndexingOff) => {
                        BigFiles::Failed { indexing_off: true }
                    }
                    Err(bigfiles::ScanError::Other(e)) => {
                        tracing::error!("large-file query failed: {e}");
                        BigFiles::Failed {
                            indexing_off: false,
                        }
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Put the listing away — back to "not asked yet", which is what the
    /// card shows before the first query. A view action only: the query
    /// costs seconds to repeat, and the stored baseline stays, so the
    /// next listing can still say what it added. Nothing on disk moves.
    pub fn clear_big_files(&mut self, cx: &mut Context<Self>) {
        self.analysis.big_files = BigFiles::Off;
        cx.notify();
    }

    /// The delete button's confirmed action: move to the Trash, then drop
    /// the row. A failed trash leaves the row — a file that is still there
    /// must not vanish from the list.
    pub fn trash_big_file(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Err(e) = bigfiles::trash(path) {
            tracing::warn!("trash {}: {e}", path.display());
            return;
        }
        if let BigFiles::Ready { scan, added, .. } = &mut self.analysis.big_files {
            if let Some(file) = scan.files.iter().find(|f| f.path == path) {
                self.analysis.trashed = self.analysis.trashed.saturating_add(file.size);
            }
            scan.files.retain(|f| f.path != path);
            scan.total = scan.total.saturating_sub(1);
            added.remove(path);
        }
        cx.notify();
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
    pub fn trash_regenerable(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let mut gone: Vec<&PathBuf> = Vec::new();
        for path in paths {
            match bigfiles::trash(path) {
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
            // table, and blind-spot files inside a trashed tree went with
            // it — those rows would dangle.
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
        cx.notify();
    }

    /// A freshly built disk-space window starts without yesterday's
    /// index query, and with the dirs table folded back to its default
    /// length. Only on a *new* window: raising one that is already open
    /// must not wipe what its owner is reading.
    ///
    /// The analysis result itself survives on purpose — it costs minutes
    /// to produce and is cached to disk across restarts; the caption says
    /// how old it is.
    pub fn reset_storage_views(&mut self, cx: &mut Context<Self>) {
        self.analysis.big_files = BigFiles::Off;
        self.analysis.analysis_show_all_dirs = false;
        // Opened rows are questions too — a window opened tomorrow should
        // show the result the way a finished scan leaves it, not a tree
        // somebody unfolded yesterday.
        self.drop_expansions();
        self.reset_dupe_views();
        cx.notify();
    }
}
