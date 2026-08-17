//! Application-level state.
//!
//! The window is only a rendering layer, and it spends most of its life off
//! screen: the panel is ordered out rather than destroyed (see `window_ext`),
//! and gpui discards element state for anything it did not paint this frame.
//! So everything that has to survive a hide → reveal round trip belongs here
//! rather than in the root view — window geometry, the selected tab, per-tab
//! scroll offsets. Collected metrics are the main tenant, and sampling runs
//! whether or not a window exists at all.

use crate::bigfiles;
use crate::bigfiles::BigFilesScan;
use crate::cleanhints;
use crate::diskscan::{self, DiffBaseline, ScanEvent, ScanResult, ScanScope};
use crate::fullscan::{self, GroupScan, Scan};
use crate::history;
use crate::history::Spender;
use crate::i18n;
use crate::metrics;
use crate::prefs;
use crate::procscan;
use crate::spaceinfo::{self, SpaceInfo};
use crate::updater;
pub use crate::watch::SustainedNotice;
use crate::watch::{AbnormalWatch, NetActivity, SustainedWatch};
use gpui::{
    AppContext, Bounds, Context, Entity, Focusable, Global, ListAlignment, ListState, Pixels,
    ScrollHandle, Window, px,
};
use gpui_component::input::{InputEvent, InputState};
use std::array;
use std::collections::{HashMap, VecDeque};
use std::mem;
use std::ops::Deref;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use zstats::settings::FileConfig;
use zstats::snapshot::{ProcessGroupSnapshot, ProcessSnapshot};
use zstats::{AlertEvent, AlertKind, AlertSubject, Tick};

/// Fraction of the CPU alert threshold at which sustained load starts to
/// matter. Derived rather than fixed so tightening `alert-cpu` tightens this
/// too.
const SUSTAINED_FRACTION: f64 = 1.0 / 3.0;

/// Used when config.toml sets no `alert-cpu` — zstats' own default is 30%.
const SUSTAINED_FALLBACK_ALERT: f64 = 30.0;

/// How many past alerts the Alerts tab can show.
const MAX_ALERTS: usize = 20;

/// How to order the process list. A view preference, deliberately not
/// persisted — it is for looking at something right now, not a setting.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ProcSort {
    /// By the 60s rolling CPU average. Uses the smoothed value rather than
    /// the instantaneous sample, which jitters enough to make rows swap
    /// places between frames.
    #[default]
    Cpu,
    Memory,
    Name,
}

impl ProcSort {
    /// Cycles through the orderings; the control is one button, not a menu.
    pub fn next(self) -> Self {
        match self {
            ProcSort::Cpu => ProcSort::Memory,
            ProcSort::Memory => ProcSort::Name,
            ProcSort::Name => ProcSort::Cpu,
        }
    }

    /// i18n key for the short label on the control.
    pub fn label_key(self) -> &'static str {
        match self {
            ProcSort::Cpu => "processes.sort_cpu",
            ProcSort::Memory => "processes.sort_memory",
            ProcSort::Name => "processes.sort_name",
        }
    }

    /// i18n key for the tooltip. Memory / name only reorder the already
    /// truncated list — that caveat has to live somewhere, and the chip
    /// itself is too short to carry it.
    pub fn tip_key(self) -> &'static str {
        match self {
            ProcSort::Cpu => "processes.sort_cpu_tip",
            ProcSort::Memory => "processes.sort_memory_tip",
            ProcSort::Name => "processes.sort_name_tip",
        }
    }

    /// Tooltip on the whole-table listing, where the same chip really
    /// does rank the machine — the truncated-list caveat would be a lie.
    pub fn full_tip_key(self) -> &'static str {
        match self {
            ProcSort::Cpu => "processes.sort_cpu_tip_full",
            ProcSort::Memory => "processes.sort_memory_tip_full",
            ProcSort::Name => "processes.sort_name_tip_full",
        }
    }
}

/// The panel's views, in tab-strip order. Config is not here: it lives
/// in its own window (the footer's gear), where a settings session is
/// not cut short by the popover auto-hiding on focus loss.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tab {
    #[default]
    Overview,
    Processes,
    Apps,
    /// Disks, temperature sensors and the battery in one place — the
    /// machine's physical substrate, as opposed to the workload tabs.
    Hardware,
    Net,
    Alerts,
    History,
}

impl Tab {
    pub const ALL: [Tab; 7] = [
        Tab::Overview,
        Tab::Processes,
        Tab::Apps,
        Tab::Hardware,
        Tab::Net,
        Tab::Alerts,
        Tab::History,
    ];

    /// Stable index, used to key per-tab UI state such as scroll position.
    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or_default()
    }

    /// Stable element id — English, not translated.
    pub fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Processes => "Processes",
            Tab::Apps => "Apps",
            Tab::Hardware => "Hardware",
            Tab::Net => "Network",
            Tab::Alerts => "Alerts",
            Tab::History => "History",
        }
    }

    /// Tooltip / spoken name in the active locale.
    pub fn title(self) -> String {
        i18n::tr(match self {
            Tab::Overview => "tabs.overview",
            Tab::Processes => "tabs.processes",
            Tab::Apps => "tabs.apps",
            Tab::Hardware => "tabs.hardware",
            Tab::Net => "tabs.network",
            Tab::Alerts => "tabs.alerts",
            Tab::History => "tabs.history",
        })
    }
}

/// Identity of an alerting episode: who, plus what about them.
///
/// Both halves are needed — one process can be over on CPU and on memory at
/// the same time, and those are two separate stories.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Episode {
    /// By pid, not name: two processes can share a name.
    Process(u32, AlertKind),
    App(u32, AlertKind),
    Volume(String, AlertKind),
    System(AlertKind),
}

/// How far back the History tab reads. A view preference like
/// [`ProcSort`] — session-only, not persisted; the daily files zstats
/// keeps go back 30 days, which bounds the widest option.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HistoryRange {
    #[default]
    Today,
    Week,
    Month,
}

impl HistoryRange {
    pub const ALL: [HistoryRange; 3] =
        [HistoryRange::Today, HistoryRange::Week, HistoryRange::Month];

    pub fn days(self) -> u16 {
        match self {
            HistoryRange::Today => 1,
            HistoryRange::Week => 7,
            HistoryRange::Month => 30,
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            HistoryRange::Today => "history.range_today",
            HistoryRange::Week => "history.range_week",
            HistoryRange::Month => "history.range_month",
        }
    }

    pub fn title_key(self) -> &'static str {
        match self {
            HistoryRange::Today => "history.title_today",
            HistoryRange::Week => "history.title_week",
            HistoryRange::Month => "history.title_month",
        }
    }
}

/// What the History list ranks by. A view preference like [`ProcSort`]
/// — session-only. Both orders read fields the daily files already
/// carry; nothing is derived.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HistorySort {
    /// Accumulated single-core time — the tab's founding question.
    #[default]
    CpuTime,
    /// Highest recorded one-minute footprint. Honesty caveat carried by
    /// the chip tooltip: only minutes that qualified for the file count.
    PeakMemory,
}

impl HistorySort {
    pub fn next(self) -> Self {
        match self {
            HistorySort::CpuTime => HistorySort::PeakMemory,
            HistorySort::PeakMemory => HistorySort::CpuTime,
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            HistorySort::CpuTime => "history.sort_cpu",
            HistorySort::PeakMemory => "history.sort_mem",
        }
    }

    pub fn tip_key(self) -> &'static str {
        match self {
            HistorySort::CpuTime => "history.sort_cpu_tip",
            HistorySort::PeakMemory => "history.sort_mem_tip",
        }
    }
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
        /// launch opens with); false for drill-fallback subwalks, which
        /// the design keeps out of the cache.
        persist: bool,
        cancel: Arc<AtomicBool>,
    },
    Ready(ScanResult),
    Failed(String),
}

/// The Hardware tab's one-shot large-file query, same lifecycle shape as
/// the full process scans: `Off → Running → Ready/Failed`, reset on hide.
#[derive(Default)]
pub enum BigFiles {
    #[default]
    Off,
    Running,
    Ready(BigFilesScan),
    /// `indexing_off` selects the honest message: a disabled Spotlight
    /// index would otherwise masquerade as "no big files".
    Failed {
        indexing_off: bool,
    },
}

/// The version check / assisted download, for the About page.
pub enum UpdateStatus {
    Checking,
    Done(updater::UpdateCheck),
    Downloading {
        received: u64,
        /// 0 while the server has not said.
        total: u64,
        url: String,
        notes: String,
    },
    /// Downloaded, verified, and the DMG handed to the OS — the drag
    /// window is (or was) on screen; installing stays the user's act.
    Installed,
    DownloadFailed {
        version: String,
        error: String,
        url: String,
        notes: String,
    },
}

/// The clean-hints update fetch, for the Config page's status line.
pub enum HintsSync {
    Running,
    Done(cleanhints::RemoteUpdate),
}

/// One episode's quiet hours: banners are skipped until the deadline.
struct Snooze {
    until: Instant,
    /// Wall-clock form of `until` ("14:32"), fixed at snooze time — the
    /// deadline does not move, so neither should its label.
    until_label: String,
}

impl Episode {
    fn of(event: &AlertEvent) -> Self {
        let kind = event.kind();
        match &event.subject {
            AlertSubject::Process { pid, .. } => Episode::Process(*pid, kind),
            AlertSubject::App { root_pid, .. } => Episode::App(*root_pid, kind),
            AlertSubject::Volume { mount_point } => Episode::Volume(mount_point.clone(), kind),
            AlertSubject::System => Episode::System(kind),
        }
    }
}

/// One alerting episode, with the freshest numbers it has reported.
///
/// [`AlertEvent`] carries no timestamp of its own, and `Tick::alerts` reports
/// the *moment* a threshold is crossed rather than a standing list — so the
/// "currently interesting" list the design shows has to be accumulated here.
///
/// Accumulated by episode, not by event. zstats alerts once on the crossing,
/// once more after 30 minutes if it still holds, then stays quiet until the
/// value falls back and re-arms. Appending a card per event would give the
/// same condition two entries, and a value hovering at the threshold could
/// push everything else out of a 20-slot list on its own.
pub struct SeenAlert {
    /// Stable id for the UI. The deque reorders as episodes resurface, so an
    /// index would silently reassign element state — hover, tooltips, the
    /// expanded editor — to a different card.
    pub seq: u64,
    /// When this episode first crossed.
    pub first_at: Instant,
    /// Most recent report within the episode.
    pub at: Instant,
    /// How many times zstats has reported it — 1 on the crossing, 2 once the
    /// 30-minute follow-up lands.
    pub reports: u32,
    pub event: AlertEvent,
}

impl SeenAlert {
    /// Time since the most recent report.
    pub fn age(&self) -> Duration {
        self.at.elapsed()
    }

    /// How long the episode has been going, once that differs from [`age`]
    /// by enough to be worth a second timestamp on the card.
    pub fn span(&self) -> Option<Duration> {
        let span = self.at.duration_since(self.first_at);
        (span >= Duration::from_secs(60)).then_some(span)
    }
}

/// The one-shot listing of every process, behind the All chip.
///
/// Separate from the collected [`Tick`] on purpose: the panel's list is the
/// collector's top-N, and this is a different measurement with a different
/// CPU window — see [`crate::fullscan`]. Keeping them in separate fields is
/// what stops one from being rendered as if it were the other.
#[derive(Default)]
pub enum FullScan {
    /// Nobody has asked. The default, and the reason the feature costs
    /// nothing at all until it is used.
    #[default]
    Off,
    Running,
    Ready(FullScanData),
    /// The collect failed. Held rather than reset so the view can say so;
    /// clicking again retries.
    Failed,
}

/// A landed [`FullScan`], with what the view needs to caveat it.
pub struct FullScanData {
    /// Shared rather than cloned into the list element: `uniform_list` takes
    /// a `'static` closure, so the rows cannot borrow the store.
    pub processes: Arc<Vec<ProcessSnapshot>>,
    pub total: usize,
    /// Window the CPU percentages were measured over.
    pub window: Duration,
    /// When it landed. A listing is a photograph, not a feed — the view
    /// prints the age so a five-minute-old answer cannot pass for live.
    pub at: Instant,
    /// Indices into `processes` that the name filter keeps — the rows the
    /// list actually shows. The whole range while no filter is active.
    pub visible: Vec<usize>,
    /// Drives the virtualised list: measured row heights plus the scroll
    /// offset. Lives here because gpui drops element state it did not
    /// paint and the panel repaints per tick — built per frame, the list
    /// would snap to the top every couple of seconds. Rebuilt with each
    /// scan, so a new photograph starts at the top with an empty cache.
    pub list: ListState,
}

/// The one-shot listing of every process tree, behind the Apps All chip.
///
/// Same shape as [`FullScan`], kept in its own field so opening All on
/// Processes does not throw away (or get confused with) an Apps listing.
#[derive(Default)]
pub enum FullAppScan {
    #[default]
    Off,
    Running,
    Ready(FullAppScanData),
    Failed,
}

/// A landed [`FullAppScan`].
pub struct FullAppScanData {
    pub groups: Arc<Vec<ProcessGroupSnapshot>>,
    pub total: usize,
    pub window: Duration,
    pub at: Instant,
    pub visible: Vec<usize>,
    pub list: ListState,
}

pub struct ZStatsAppState {
    window_bounds: Option<Bounds<Pixels>>,
    scale_factor: f32,
    last_auto_hide: Option<Instant>,
    latest: Option<Tick>,
    alerts: VecDeque<SeenAlert>,
    tab: Tab,
    selected_pid: Option<u32>,
    selected_app: Option<u32>,
    /// Which alert card is expanded for threshold editing. Keyed by the
    /// settings key + override name (process / app / mount), not the
    /// deque index — new events push to the front.
    selected_alert: Option<(String, String)>,
    settings: Option<FileConfig>,
    /// UI filter: show only the abnormal entries, not the whole table.
    only_abnormal: bool,
    /// UI filter: reveal the interfaces the recency filter would hide.
    show_unused_nets: bool,
    /// UI filter: reveal every temperature sensor, not just the preview.
    show_all_sensors: bool,
    /// The Hardware tab's large-file query. Query-like state: reset on hide.
    big_files: BigFiles,
    /// The directory analyser. Survives hide (see [`DiskAnalysis`]).
    disk_analysis: DiskAnalysis,
    /// Outer results parked while drilled into a subtree — each level is
    /// a finished `ScanResult` (a few KB), so "back" restores instantly
    /// instead of re-walking the parent for half a minute.
    disk_analysis_stack: Vec<diskscan::ScanResult>,
    /// The user-picked analysis scope for this session — a chosen folder
    /// or the cache-set preset; `None` means the default (~). The
    /// re-analyze chip re-walks whatever this says, so picking a scope
    /// once makes the chip mean that scope until the results are
    /// cleared.
    disk_analysis_root: Option<ScanScope>,
    /// The run before the current result, flattened for per-row Δs —
    /// rebuilt from the rotated `.prev` cache file whenever a top-level
    /// walk finishes (and once at launch), never during drills.
    analysis_diff: Option<DiffBaseline>,
    /// The boot volume's purgeable-space / snapshot readout, refreshed
    /// lazily while Hardware is the visible tab (throttled below) — a
    /// panel-owned query, deliberately not a Monitor metric.
    space: Option<SpaceInfo>,
    space_at: Option<Instant>,
    space_inflight: bool,
    /// Whether the Hardware tab shows the full analysis card or just its
    /// one-line summary. UI-session state, deliberately NOT persisted and
    /// reset to collapsed on every entry to the tab: the tables are long
    /// enough to bury the volumes and sensors beneath them, so unfolding
    /// is a per-visit choice. Starting or drilling an analysis unfolds —
    /// the user just asked to watch it.
    disk_analysis_expanded: bool,
    /// Whether the dirs table shows every retained row (up to
    /// `TABLE_KEEP`) or the display default. Reset together with the
    /// card fold: per-visit, per-scan choice.
    analysis_show_all_dirs: bool,
    /// The settings window, if one was ever opened. Kept so a second
    /// click focuses the existing window; a handle whose window the user
    /// closed fails its update and a fresh window is built instead.
    settings_window: Option<gpui::AnyWindowHandle>,
    /// Monotonic id for analyser runs, so a stale run's channel events
    /// can never land into a newer run's state.
    disk_analysis_runs: u64,
    /// Banner snoozes by episode: the user asked for quiet on this subject
    /// until a deadline. Delivery-layer only — events still land in the
    /// alerts list and the engine's rules are untouched. Deliberately not
    /// persisted: a snooze means "not now", and a restart is a new now.
    snoozed: HashMap<Episode, Snooze>,
    proc_sort: ProcSort,
    /// The three observers that answer questions zstats' own rules cannot —
    /// see [`crate::watch`]. They own their clocks and thresholds; this type
    /// only feeds them samples and reads the verdicts back out.
    sustained: SustainedWatch,
    abnormal: AbnormalWatch,
    net: NetActivity,
    /// Today's history, ranked. `None` until the tab is first opened — the
    /// read walks a day of JSONL and there is no reason to pay for it before
    /// somebody asks.
    history: Option<Vec<Spender>>,
    /// The window `history` was (or is being) read for.
    history_range: HistoryRange,
    /// The order the History list shows.
    history_sort: HistorySort,
    /// The last (or in-flight) clean-hints update fetch.
    hints_sync: Option<HintsSync>,
    /// The last (or in-flight) version check.
    update_status: Option<UpdateStatus>,
    /// The whole-table listing, only ever populated on request.
    full_scan: FullScan,
    /// The whole-tree listing for the Apps tab, only ever populated on request.
    full_app_scan: FullAppScan,
    /// The name-filter input, created on first open — [`InputState`] needs
    /// a `Window`, which only the toggle click has. Kept once created, so
    /// reopening the filter does not rebuild cursor/undo state.
    proc_filter: Option<Entity<InputState>>,
    /// Whether the filter row is on screen. Closing clears the text — a
    /// hidden filter that kept filtering would read as processes vanishing.
    proc_filter_open: bool,
    /// The query, lowercased, mirrored out of the entity on every change.
    /// Views read the store without an `App` in hand, and the full-scan
    /// list must be rebuilt when the row set changes — both want the text
    /// as plain state, not behind an entity read.
    proc_filter_text: String,
    /// Monotonic id source for [`SeenAlert::seq`].
    next_seq: u64,
    /// One scroll offset per tab, indexed by [`Tab::index`].
    ///
    /// Has to live here rather than on the element: gpui keys element state by
    /// id and drops whatever it did not paint, and only one tab's body is ever
    /// painted. A per-tab id alone therefore resets every tab to the top on
    /// each switch — holding the handles across frames is what actually
    /// remembers the position.
    scroll: [ScrollHandle; Tab::ALL.len()],
    /// Scroll for the rows region *inside* the Processes top-N card — the
    /// rows scroll under a pinned header, so they need a handle of their
    /// own, held here for the same reason as the per-tab ones above.
    proc_rows_scroll: ScrollHandle,
    /// Same, for the Applications card.
    app_rows_scroll: ScrollHandle,
}

impl Default for ZStatsAppState {
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
        Self {
            window_bounds: None,
            scale_factor: 1.0,
            last_auto_hide: None,
            latest: None,
            alerts: VecDeque::new(),
            tab: Tab::default(),
            selected_pid: None,
            selected_app: None,
            selected_alert: None,
            settings: None,
            only_abnormal: false,
            show_unused_nets: false,
            show_all_sensors: false,
            big_files: BigFiles::default(),
            // A fresh launch opens with the last finished analysis, if
            // one was cached — "see last time's numbers first".
            disk_analysis: (!launch_roots.is_empty())
                .then(|| diskscan::load_cache(&launch_roots))
                .flatten()
                .map(DiskAnalysis::Ready)
                .unwrap_or_default(),
            disk_analysis_stack: Vec::new(),
            disk_analysis_root: restored,
            // The baseline outlives restarts the same way the result
            // does: through its file.
            analysis_diff: (!launch_roots.is_empty())
                .then(|| diskscan::load_prev_cache(&launch_roots))
                .flatten()
                .map(|prev| DiffBaseline::from_result(&prev)),
            disk_analysis_expanded: false,
            analysis_show_all_dirs: false,
            space: None,
            space_at: None,
            space_inflight: false,
            settings_window: None,
            disk_analysis_runs: 0,
            snoozed: HashMap::new(),
            proc_sort: ProcSort::default(),
            sustained: SustainedWatch::default(),
            abnormal: AbnormalWatch::default(),
            net: NetActivity::default(),
            history: None,
            history_range: HistoryRange::default(),
            history_sort: HistorySort::default(),
            hints_sync: None,
            update_status: None,
            full_scan: FullScan::default(),
            full_app_scan: FullAppScan::default(),
            proc_filter: None,
            proc_filter_open: false,
            proc_filter_text: String::new(),
            next_seq: 0,
            scroll: array::from_fn(|_| ScrollHandle::new()),
            proc_rows_scroll: ScrollHandle::new(),
            app_rows_scroll: ScrollHandle::new(),
        }
    }
}

impl ZStatsAppState {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- metrics -------------------------------------------------------

    /// Fold one collection round into the state. Returns the events that
    /// arrived this tick so the caller can deliver desktop notifications
    /// without walking the accumulated list.
    pub fn ingest(&mut self, tick: Tick, cx: &mut Context<Self>) -> Vec<AlertEvent> {
        let now = Instant::now();
        let fresh = tick.alerts.clone();
        for event in &fresh {
            self.record_alert(event.clone(), now);
        }

        if let Some(processes) = tick.snapshot.processes.as_deref() {
            self.sustained
                .record(processes, &tick.process_stats, self.sustained_bar(), now);
        }
        if let Some(nets) = tick.snapshot.networks.as_deref() {
            self.net.record(nets, now);
        }

        self.latest = Some(tick);
        // Piggyback on the tick rather than the render: views are pure
        // functions and cannot start work, and a fresh probe is only
        // interesting while someone is looking at the Hardware tab.
        if self.tab == Tab::Hardware {
            self.ensure_space_info(cx);
        }
        cx.notify();
        fresh
    }

    /// Refresh the purgeable/snapshot readout when it has gone stale.
    /// Single-flight; the probe spawns `tmutil`, so it stays off the
    /// main thread and well below the collection cadence.
    fn ensure_space_info(&mut self, cx: &mut Context<Self>) {
        /// Purgeable space moves slowly and the probe costs a process
        /// spawn — one refresh a minute is plenty.
        const SPACE_REFRESH: Duration = Duration::from_secs(60);
        if self.space_inflight || self.space_at.is_some_and(|at| at.elapsed() < SPACE_REFRESH) {
            return;
        }
        self.space_inflight = true;
        cx.spawn(async move |this, cx| {
            let info = cx
                .background_executor()
                .spawn(async { spaceinfo::probe() })
                .await;
            let _ = this.update(cx, |state, cx| {
                state.space = Some(info);
                state.space_at = Some(Instant::now());
                state.space_inflight = false;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn space_info(&self) -> Option<&SpaceInfo> {
        self.space.as_ref()
    }

    /// Fold one alert into the list, merging into its episode if that episode
    /// is already there and moving it back to the front.
    fn record_alert(&mut self, event: AlertEvent, now: Instant) {
        let episode = Episode::of(&event);
        if let Some(i) = self
            .alerts
            .iter()
            .position(|seen| Episode::of(&seen.event) == episode)
            && let Some(mut seen) = self.alerts.remove(i)
        {
            seen.at = now;
            seen.reports += 1;
            // Keep the newest reading: the follow-up carries current numbers,
            // and a card showing the crossing value 30 minutes on is stale.
            seen.event = event;
            self.alerts.push_front(seen);
            return;
        }

        self.next_seq += 1;
        self.alerts.push_front(SeenAlert {
            seq: self.next_seq,
            first_at: now,
            at: now,
            reports: 1,
            event,
        });
        while self.alerts.len() > MAX_ALERTS {
            self.alerts.pop_back();
        }
    }

    /// The most recent collection, or `None` before the first one lands.
    pub fn latest(&self) -> Option<&Tick> {
        self.latest.as_ref()
    }

    pub fn alerts(&self) -> &VecDeque<SeenAlert> {
        &self.alerts
    }

    /// What the collector is running with. Seeded at startup, then replaced
    /// whenever the Config tab or an Alerts chip writes through
    /// [`Self::apply_setting`].
    pub fn settings(&self) -> Option<&FileConfig> {
        self.settings.as_ref()
    }

    /// Replace the abnormal-process list from a fresh scan.
    pub fn set_abnormal(&mut self, found: Vec<procscan::AbnormalProcess>, cx: &mut Context<Self>) {
        self.abnormal.replace(found, Instant::now());
        cx.notify();
    }

    /// Abnormal processes that have stayed that way long enough to matter.
    pub fn abnormal(&self) -> Vec<&procscan::AbnormalProcess> {
        self.abnormal.persistent()
    }

    /// Whether the process list is filtered down to abnormal entries only.
    pub fn only_abnormal(&self) -> bool {
        self.only_abnormal
    }

    /// The CPU share above which load counts as sustained-and-worth-noting.
    /// A third of the alert threshold, so it scales with the user's setting.
    /// The active sustained stretches, for the Alerts tab's read-only
    /// card. Judgment stays out of the rule engine: this reads the
    /// watcher's state and nothing more.
    pub fn sustained_active(&self) -> Vec<SustainedNotice> {
        self.sustained.active(self.sustained_bar())
    }

    /// The sustained bar, exposed for the Alerts empty state's
    /// watching line.
    pub fn sustained_bar_percent(&self) -> f64 {
        self.sustained_bar()
    }

    fn sustained_bar(&self) -> f64 {
        self.settings
            .as_ref()
            .and_then(|f| f.alerts.cpu)
            .map_or(SUSTAINED_FALLBACK_ALERT, f64::from)
            * SUSTAINED_FRACTION
    }

    /// Sustained-load notices raised by the last round, taken once.
    pub fn take_sustained_notices(&mut self) -> Vec<SustainedNotice> {
        self.sustained.take_notices()
    }

    /// How long this process has been holding a low-but-real CPU share, once
    /// that has gone on long enough to be worth saying.
    pub fn sustained_load(&self, pid: u32) -> Option<Duration> {
        self.sustained.duration_for(pid, self.sustained_bar())
    }

    /// Whether an interface has carried traffic recently enough for a row.
    pub fn net_is_recent(&self, interface: &str) -> bool {
        self.net.is_recent(interface)
    }

    pub fn proc_sort(&self) -> ProcSort {
        self.proc_sort
    }

    pub fn cycle_proc_sort(&mut self, cx: &mut Context<Self>) {
        self.proc_sort = self.proc_sort.next();
        cx.notify();
    }

    pub fn toggle_only_abnormal(&mut self, cx: &mut Context<Self>) {
        self.only_abnormal = !self.only_abnormal;
        cx.notify();
    }

    pub fn show_unused_nets(&self) -> bool {
        self.show_unused_nets
    }

    pub fn toggle_unused_nets(&mut self, cx: &mut Context<Self>) {
        self.show_unused_nets = !self.show_unused_nets;
        cx.notify();
    }

    // ---- directory analyser --------------------------------------------

    pub fn disk_analysis(&self) -> &DiskAnalysis {
        &self.disk_analysis
    }

    /// The Δ baseline for `result` — present only when a previous run of
    /// the *same scope* exists, so drill views and freshly-picked roots
    /// never show half-comparable deltas.
    pub fn analysis_diff_for(&self, result: &ScanResult) -> Option<&DiffBaseline> {
        self.analysis_diff
            .as_ref()
            .filter(|diff| diff.roots() == result.roots)
    }

    /// Start (or restart) the top-level analysis — of the session's
    /// picked scope, or the home tree by default. A drill-down is left
    /// via "back", not by rescanning, so the stack is dropped here.
    pub fn start_disk_analysis(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self
            .disk_analysis_root
            .clone()
            .or_else(|| diskscan::default_root().map(ScanScope::single))
        else {
            self.disk_analysis = DiskAnalysis::Failed("HOME is not set".into());
            cx.notify();
            return;
        };
        self.disk_analysis_stack.clear();
        self.disk_analysis_expanded = true;
        self.launch_disk_analysis(scope, true, cx);
    }

    /// Analyze a user-chosen root — the folder picker's entry point.
    /// The bare root volume is refused rather than walked: firmlinks
    /// double-count, and /System plus TCC would distort every figure
    /// (docs/disk-analysis.md's scope table) — the answer would be
    /// wrong, not merely slow.
    pub fn start_disk_analysis_at(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if root == Path::new("/") {
            self.cancel_disk_analysis_walk();
            self.disk_analysis_stack.clear();
            self.disk_analysis = DiskAnalysis::Failed(i18n::tr("disk.ana_root_unsupported"));
            cx.notify();
            return;
        }
        let scope = ScanScope::single(root);
        self.disk_analysis_root = Some(scope.clone());
        self.disk_analysis_stack.clear();
        self.disk_analysis_expanded = true;
        self.launch_disk_analysis(scope, true, cx);
    }

    /// Analyze the cache-set preset — the explicit cache roots merged
    /// into one ranked view (docs/disk-analysis.md's scope table). Same
    /// session semantics as a picked folder: re-analyze means this scope
    /// until the results are cleared.
    pub fn start_disk_analysis_caches(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = ScanScope::cache_set() else {
            self.disk_analysis = DiskAnalysis::Failed("HOME is not set".into());
            cx.notify();
            return;
        };
        self.disk_analysis_root = Some(scope.clone());
        self.disk_analysis_stack.clear();
        self.disk_analysis_expanded = true;
        self.launch_disk_analysis(scope, true, cx);
    }

    /// Drill into one ranked directory: park the current result on the
    /// stack and show that path as the new root. Served instantly from
    /// the finished scan's retained index when it can honestly answer;
    /// only folded interiors and below-floor corners fall back to a live
    /// walk. Only a finished result can be drilled — the rows are inert
    /// while a walk is running.
    pub fn drill_disk_analysis(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let DiskAnalysis::Ready(current) = &self.disk_analysis else {
            return;
        };
        let derived = diskscan::drill(current, &root);
        if let DiskAnalysis::Ready(current) = mem::take(&mut self.disk_analysis) {
            self.disk_analysis_stack.push(current);
        }
        self.disk_analysis_expanded = true;
        match derived {
            Some(result) => {
                self.disk_analysis = DiskAnalysis::Ready(result);
                cx.notify();
            }
            None => self.launch_disk_analysis(ScanScope::single(root), false, cx),
        }
    }

    /// Leave the current drill level and restore the parked outer result.
    pub fn pop_disk_analysis(&mut self, cx: &mut Context<Self>) {
        let Some(prev) = self.disk_analysis_stack.pop() else {
            return;
        };
        self.cancel_disk_analysis_walk();
        self.disk_analysis = DiskAnalysis::Ready(prev);
        cx.notify();
    }

    pub fn analysis_show_all_dirs(&self) -> bool {
        self.analysis_show_all_dirs
    }

    pub fn set_analysis_show_all_dirs(&mut self, show: bool, cx: &mut Context<Self>) {
        self.analysis_show_all_dirs = show;
        cx.notify();
    }

    pub fn disk_analysis_expanded(&self) -> bool {
        self.disk_analysis_expanded
    }

    pub fn set_disk_analysis_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        self.disk_analysis_expanded = expanded;
        cx.notify();
    }

    pub fn disk_analysis_can_back(&self) -> bool {
        !self.disk_analysis_stack.is_empty()
    }

    /// Dismiss the analysis entirely — straight to Off, whatever level
    /// is showing. This is a view action, not a disk one: nothing is
    /// touched on disk, and dropping the results also releases the
    /// retained drill index.
    pub fn clear_disk_analysis(&mut self, cx: &mut Context<Self>) {
        self.cancel_disk_analysis_walk();
        // Clean slate includes the saved result — otherwise the next
        // launch would resurrect what the user just dismissed. The
        // session's top-level scope is the outermost parked result, or
        // the current one when nothing is parked.
        let top_roots = self
            .disk_analysis_stack
            .first()
            .map(|r| r.roots.clone())
            .or_else(|| match &self.disk_analysis {
                DiskAnalysis::Ready(r) => Some(r.roots.clone()),
                _ => None,
            });
        if let Some(roots) = top_roots {
            diskscan::delete_cache(&roots);
        }
        // The baseline's file went with the cache; the flattened copy
        // must not outlive it.
        self.analysis_diff = None;
        self.disk_analysis_stack.clear();
        // Clean slate includes the picked scope: the next "Analyze"
        // means the default home tree again — this launch and the next.
        self.disk_analysis_root = None;
        prefs::set_analysis_roots(&[]);
        self.disk_analysis = DiskAnalysis::Off;
        cx.notify();
    }

    /// The walk itself. Runs on its own thread; everything this state
    /// learns — progress, completion, failure — arrives over the channel
    /// drained below, guarded by `run_id` so a superseded run's late
    /// events fall on the floor.
    fn launch_disk_analysis(&mut self, scope: ScanScope, persist: bool, cx: &mut Context<Self>) {
        self.cancel_disk_analysis_walk();
        self.disk_analysis_runs += 1;
        let run_id = self.disk_analysis_runs;
        let cancel = Arc::new(AtomicBool::new(false));
        self.disk_analysis = DiskAnalysis::Running {
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
                        state.disk_analysis,
                        DiskAnalysis::Running { run_id: id, .. } if id == run_id
                    );
                    if !owns {
                        return;
                    }
                    match event {
                        ScanEvent::Progress { dirs_done } => {
                            if let DiskAnalysis::Running { dirs_done: d, .. } =
                                &mut state.disk_analysis
                            {
                                *d = dirs_done;
                            }
                        }
                        ScanEvent::Partial(result) => {
                            if let DiskAnalysis::Running { partial, .. } = &mut state.disk_analysis
                            {
                                *partial = Some(*result);
                            }
                        }
                        ScanEvent::Done(result) => {
                            // Only finished top-level walks reach the cache;
                            // cancelled and failed runs never get here, so a
                            // half table cannot overwrite a full one.
                            if let DiskAnalysis::Running { persist: true, .. } = state.disk_analysis
                            {
                                // The save rotated the displaced run into
                                // `.prev` — read it back as the Δ baseline.
                                diskscan::save_cache(&result);
                                state.analysis_diff = diskscan::load_prev_cache(&result.roots)
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
                            }
                            state.disk_analysis = DiskAnalysis::Ready(*result);
                        }
                        ScanEvent::Failed(e) => {
                            state.disk_analysis = DiskAnalysis::Failed(e);
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
    /// results are never kept. A cancelled drill falls back to the outer
    /// result it came from; a cancelled top-level run goes to Off.
    pub fn cancel_disk_analysis(&mut self, cx: &mut Context<Self>) {
        self.cancel_disk_analysis_walk();
        self.disk_analysis = match self.disk_analysis_stack.pop() {
            Some(prev) => DiskAnalysis::Ready(prev),
            None => DiskAnalysis::Off,
        };
        cx.notify();
    }

    fn cancel_disk_analysis_walk(&self) {
        if let DiskAnalysis::Running { cancel, .. } = &self.disk_analysis {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    // ---- banner snooze -------------------------------------------------

    /// Quiet this episode's banners for `hours`. Suppression is delivery-
    /// layer only: the engine keeps evaluating and the Alerts list keeps
    /// recording — the interruption is what stops.
    pub fn snooze_banners(&mut self, event: &AlertEvent, hours: u64, cx: &mut Context<Self>) {
        let until_label = jiff::Zoned::now()
            .checked_add(jiff::Span::new().hours(hours as i64))
            .map(|z| z.strftime("%H:%M").to_string())
            .unwrap_or_default();
        self.snoozed.insert(
            Episode::of(event),
            Snooze {
                until: Instant::now() + Duration::from_secs(hours * 3600),
                until_label,
            },
        );
        cx.notify();
    }

    pub fn unsnooze_banners(&mut self, event: &AlertEvent, cx: &mut Context<Self>) {
        self.snoozed.remove(&Episode::of(event));
        cx.notify();
    }

    /// Whether this event's banner is muted right now. Runs on every fresh
    /// event, which is also where expired entries get dropped — the map
    /// never outlives its deadlines by more than one alert.
    pub fn banner_snoozed(&mut self, event: &AlertEvent) -> bool {
        let now = Instant::now();
        self.snoozed.retain(|_, s| s.until > now);
        self.snoozed.contains_key(&Episode::of(event))
    }

    /// The "muted until 14:32" label for a card, if its episode is muted.
    pub fn snoozed_until(&self, event: &AlertEvent) -> Option<&str> {
        let snooze = self.snoozed.get(&Episode::of(event))?;
        (snooze.until > Instant::now()).then_some(snooze.until_label.as_str())
    }

    pub fn show_all_sensors(&self) -> bool {
        self.show_all_sensors
    }

    pub fn toggle_all_sensors(&mut self, cx: &mut Context<Self>) {
        self.show_all_sensors = !self.show_all_sensors;
        cx.notify();
    }

    /// How long we have observed this pid as abnormal. Always a lower bound:
    /// it may well have been in that state before the app started.
    pub fn abnormal_observed(&self, pid: u32) -> Option<Duration> {
        self.abnormal.observed(pid)
    }

    pub fn set_settings(&mut self, settings: FileConfig) {
        self.settings = Some(settings);
    }

    /// Persist one `zstats -add` key and tell the collector. `[alerts]`
    /// reloads in place; everything else rebuilds the `Monitor` (rate
    /// baselines start over).
    pub fn apply_setting(
        &mut self,
        key: &str,
        value: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let file = persist_setting(&zstats::settings::default_dir(), key, value)?;
        self.settings = Some(file);
        if setting_rebuilds_collector(key) {
            metrics::request_rebuild();
        } else {
            metrics::request_reload();
        }
        if let Some(pace) = cx.try_global::<metrics::CollectorPace>() {
            pace.wake();
        }
        cx.notify();
        Ok(())
    }

    /// Write a per-subject `[alerts]` override (or a global pressure
    /// setting when `name` is empty), persist `config.toml`, and ask the
    /// collector to pick it up. Same keys as the zstats CLI `-add`.
    pub fn apply_alert_override(
        &mut self,
        key: &str,
        name: &str,
        value: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let payload = if name.is_empty() {
            value.to_string()
        } else {
            format!("{name}={value}")
        };
        self.apply_setting(key, &payload, cx)
    }

    /// Replace `config.toml` with zstats builtins. Language and theme live
    /// in `app.toml` and are left alone. Collector fields are baked in at
    /// construction, so this rebuilds the `Monitor`.
    pub fn reset_settings(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let file = reset_config(&zstats::settings::default_dir())?;
        self.settings = Some(file);
        metrics::request_rebuild();
        if let Some(pace) = cx.try_global::<metrics::CollectorPace>() {
            pace.wake();
        }
        cx.notify();
        Ok(())
    }

    // ---- view selection ------------------------------------------------

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn settings_window(&self) -> Option<gpui::AnyWindowHandle> {
        self.settings_window
    }

    pub fn set_settings_window(&mut self, handle: gpui::AnyWindowHandle) {
        self.settings_window = Some(handle);
    }

    /// This tab's scroll offset, held across frames so switching away and
    /// back returns to where the list was left.
    pub fn scroll_handle(&self, tab: Tab) -> &ScrollHandle {
        &self.scroll[tab.index()]
    }

    // ---- large files ---------------------------------------------------

    pub fn big_files(&self) -> &BigFiles {
        &self.big_files
    }

    /// Run (or re-run) the large-file query on the background executor.
    pub fn start_big_files(&mut self, cx: &mut Context<Self>) {
        if matches!(self.big_files, BigFiles::Running) {
            return;
        }
        self.big_files = BigFiles::Running;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let scanned = cx
                .background_executor()
                .spawn(async { bigfiles::scan() })
                .await;
            let _ = this.update(cx, |state, cx| {
                // Same landing guard as the full scans: a hide mid-query
                // reset this to Off, and the result must not undo that.
                if !matches!(state.big_files, BigFiles::Running) {
                    return;
                }
                state.big_files = match scanned {
                    Ok(scan) => BigFiles::Ready(scan),
                    Err(bigfiles::ScanError::IndexingOff) => {
                        BigFiles::Failed { indexing_off: true }
                    }
                    Err(bigfiles::ScanError::Other(e)) => {
                        eprintln!("large-file query failed: {e}");
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

    /// The delete button's confirmed action: move to the Trash, then drop
    /// the row. A failed trash leaves the row — a file that is still there
    /// must not vanish from the list.
    pub fn trash_big_file(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Err(e) = bigfiles::trash(path) {
            eprintln!("trash {}: {e}", path.display());
            return;
        }
        if let BigFiles::Ready(scan) = &mut self.big_files {
            scan.files.retain(|f| f.path != path);
            scan.total = scan.total.saturating_sub(1);
        }
        cx.notify();
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
                Err(e) => eprintln!("trash {}: {e}", path.display()),
            }
        }
        if gone.is_empty() {
            return;
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
        if let DiskAnalysis::Ready(result) = &mut self.disk_analysis {
            prune(result);
            // Keep the on-disk copy in step with the pruned rows — only
            // where this root already has one (drill views never do).
            diskscan::resave_if_cached(result);
        }
        for parked in &mut self.disk_analysis_stack {
            prune(parked);
            diskscan::resave_if_cached(parked);
        }
        cx.notify();
    }

    /// Back to a clean slate for the next open. The name filter and the
    /// one-shot full listings are "looking at something right now" state:
    /// a panel reopened hours later with yesterday's query looks broken,
    /// not remembered. Scroll positions and the selected tab survive —
    /// those are orientation, not a question being asked.
    pub fn reset_transient_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.proc_filter_open {
            // The close arm clears the input, the lowercased mirror and
            // the full-scan cuts in one place.
            self.toggle_proc_filter(window, cx);
        }
        self.full_scan = FullScan::Off;
        self.full_app_scan = FullAppScan::Off;
        self.big_files = BigFiles::Off;
        cx.notify();
    }

    pub fn proc_rows_scroll(&self) -> &ScrollHandle {
        &self.proc_rows_scroll
    }

    pub fn app_rows_scroll(&self) -> &ScrollHandle {
        &self.app_rows_scroll
    }

    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            // Every fresh visit to Hardware starts from the summary —
            // see `disk_analysis_expanded`.
            if tab == Tab::Hardware {
                self.disk_analysis_expanded = false;
                self.analysis_show_all_dirs = false;
                self.ensure_space_info(cx);
            }
            // Opening History is what pays for reading it. Re-read on every
            // visit rather than caching: the file grows a line a minute, and
            // a stale "today" is worse than a moment's wait.
            if tab == Tab::History {
                self.load_history(cx);
            }
            cx.notify();
        }
    }

    /// Today's biggest CPU-time spenders, or `None` while the read is in
    /// flight or before the tab has ever been opened.
    pub fn history(&self) -> Option<&[Spender]> {
        self.history.as_deref()
    }

    pub fn update_status(&self) -> Option<&UpdateStatus> {
        self.update_status.as_ref()
    }

    /// Ask GitHub for the latest release on the background executor.
    /// One at a time, same as the hints fetch.
    pub fn check_update(&mut self, cx: &mut Context<Self>) {
        if matches!(self.update_status, Some(UpdateStatus::Checking)) {
            return;
        }
        self.update_status = Some(UpdateStatus::Checking);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async { updater::check() })
                .await;
            let _ = this.update(cx, |state, cx| {
                state.update_status = Some(UpdateStatus::Done(outcome));
                cx.notify();
            });
        })
        .detach();
    }

    /// Download `version`'s DMG on the background executor, with a
    /// progress pump back to this state. One at a time.
    pub fn download_update(&mut self, version: String, cx: &mut Context<Self>) {
        if matches!(self.update_status, Some(UpdateStatus::Downloading { .. })) {
            return;
        }
        let (url, notes) = match &self.update_status {
            Some(UpdateStatus::Done(updater::UpdateCheck::Newer { url, notes, .. })) => {
                (url.clone(), notes.clone())
            }
            Some(UpdateStatus::DownloadFailed { url, notes, .. }) => (url.clone(), notes.clone()),
            _ => (String::new(), String::new()),
        };
        self.update_status = Some(UpdateStatus::Downloading {
            received: 0,
            total: 0,
            url,
            notes,
        });
        cx.notify();

        let (tx, rx) = smol::channel::unbounded::<(u64, u64)>();
        cx.spawn(async move |this, cx| {
            while let Ok((received, total)) = rx.recv().await {
                let _ = this.update(cx, |state, cx| {
                    if let Some(UpdateStatus::Downloading {
                        received: r,
                        total: t,
                        ..
                    }) = &mut state.update_status
                    {
                        *r = received;
                        *t = total;
                        cx.notify();
                    }
                });
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            let tag = version.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    // Throttled to whole-MB steps: every 64 KB chunk
                    // would repaint the settings window for nothing.
                    let mut last_mb = u64::MAX;
                    updater::download_and_open(&tag, move |received, total| {
                        let mb = received / (1024 * 1024);
                        if mb != last_mb || received == total {
                            last_mb = mb;
                            let _ = tx.try_send((received, total));
                        }
                    })
                })
                .await;
            let _ = this.update(cx, |state, cx| {
                let (url, notes) = match &state.update_status {
                    Some(UpdateStatus::Downloading { url, notes, .. }) => {
                        (url.clone(), notes.clone())
                    }
                    _ => (String::new(), String::new()),
                };
                state.update_status = Some(match outcome {
                    Ok(_path) => UpdateStatus::Installed,
                    Err(error) => UpdateStatus::DownloadFailed {
                        version,
                        error,
                        url,
                        notes,
                    },
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub fn hints_sync(&self) -> Option<&HintsSync> {
        self.hints_sync.as_ref()
    }

    /// Fetch the published rules on the background executor. One at a
    /// time — a second press while one runs is a no-op, not a queue.
    pub fn update_cleanhints(&mut self, cx: &mut Context<Self>) {
        if matches!(self.hints_sync, Some(HintsSync::Running)) {
            return;
        }
        self.hints_sync = Some(HintsSync::Running);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async { cleanhints::update_from_remote() })
                .await;
            let _ = this.update(cx, |state, cx| {
                state.hints_sync = Some(HintsSync::Done(outcome));
                cx.notify();
            });
        })
        .detach();
    }

    pub fn history_sort(&self) -> HistorySort {
        self.history_sort
    }

    /// One button, two orders — cycle like the process sort chip.
    pub fn cycle_history_sort(&mut self, cx: &mut Context<Self>) {
        self.history_sort = self.history_sort.next();
        cx.notify();
    }

    pub fn history_range(&self) -> HistoryRange {
        self.history_range
    }

    /// Switch the window and re-read. The rows drop to the loading state
    /// first — stale today-rows under a "30 days" title would be a lie.
    pub fn set_history_range(&mut self, range: HistoryRange, cx: &mut Context<Self>) {
        if self.history_range == range {
            return;
        }
        self.history_range = range;
        self.history = None;
        self.load_history(cx);
    }

    /// Re-read the selected window's history files on the background
    /// executor. Guarded by the range it was started for: quickly
    /// flipping ranges must not let a slow wide read land under a
    /// narrower title.
    pub fn load_history(&mut self, cx: &mut Context<Self>) {
        let range = self.history_range;
        cx.spawn(async move |this, cx| {
            let rows = cx
                .background_executor()
                .spawn(async move {
                    history::spenders(&zstats::settings::default_dir(), range.days())
                        .unwrap_or_else(|e| {
                            eprintln!("could not read history: {e}");
                            Vec::new()
                        })
                })
                .await;
            let _ = this.update(cx, |state, cx| {
                if state.history_range == range {
                    state.history = Some(rows);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The whole-table listing, or what is happening to it.
    pub fn full_scan(&self) -> &FullScan {
        &self.full_scan
    }

    /// Ask for the whole process table, or drop the answer and go back to
    /// the collector's list.
    pub fn toggle_full_scan(&mut self, cx: &mut Context<Self>) {
        match self.full_scan {
            // A failed scan retries rather than latching.
            FullScan::Off | FullScan::Failed => self.start_full_scan(cx),
            // Already in flight. A second click must not spawn a second
            // scan — they would land in an order nobody controls.
            FullScan::Running => {}
            FullScan::Ready(_) => {
                self.full_scan = FullScan::Off;
                cx.notify();
            }
        }
    }

    pub fn full_app_scan(&self) -> &FullAppScan {
        &self.full_app_scan
    }

    pub fn toggle_full_app_scan(&mut self, cx: &mut Context<Self>) {
        match self.full_app_scan {
            FullAppScan::Off | FullAppScan::Failed => self.start_full_app_scan(cx),
            FullAppScan::Running => {}
            FullAppScan::Ready(_) => {
                self.full_app_scan = FullAppScan::Off;
                cx.notify();
            }
        }
    }

    /// Collect every process tree on the background executor. Same reasons
    /// as [`Self::start_full_scan`] not to widen the resident collector.
    pub fn start_full_app_scan(&mut self, cx: &mut Context<Self>) {
        self.full_app_scan = FullAppScan::Running;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let scanned = cx
                .background_executor()
                .spawn(async { fullscan::scan_groups() })
                .await;
            let _ = this.update(cx, |state, cx| {
                // Land only into a scan someone is still waiting for — the
                // panel hiding mid-scan resets to Off, and a result nobody
                // asked for anymore must not push the tab back into the
                // full listing on the next open.
                if !matches!(state.full_app_scan, FullAppScan::Running) {
                    return;
                }
                state.full_app_scan = match scanned {
                    Ok(GroupScan {
                        groups,
                        total,
                        window,
                    }) => {
                        let visible = filtered_group_indices(&groups, &state.proc_filter_text);
                        FullAppScan::Ready(FullAppScanData {
                            list: ListState::new(visible.len(), ListAlignment::Top, px(400.)),
                            visible,
                            groups,
                            total,
                            window,
                            at: Instant::now(),
                        })
                    }
                    Err(e) => {
                        eprintln!("full application scan failed: {e}");
                        FullAppScan::Failed
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Collect every process on the background executor. Blocking and slow
    /// by design (it sleeps to get a CPU baseline), which is exactly why it
    /// does not run on the collector thread: that one has a cadence to keep.
    pub fn start_full_scan(&mut self, cx: &mut Context<Self>) {
        self.full_scan = FullScan::Running;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let scanned = cx
                .background_executor()
                .spawn(async { fullscan::scan() })
                .await;
            let _ = this.update(cx, |state, cx| {
                // Same landing guard as the app scan: a hide mid-scan reset
                // this to Off, and the result must not undo that.
                if !matches!(state.full_scan, FullScan::Running) {
                    return;
                }
                state.full_scan = match scanned {
                    Ok(Scan {
                        processes,
                        total,
                        window,
                    }) => {
                        // A filter typed while the scan ran applies to it too.
                        let visible = filtered_indices(&processes, &state.proc_filter_text);
                        FullScan::Ready(FullScanData {
                            // Overdraw of roughly one panel: rows near the
                            // viewport are pre-measured so scrolling does
                            // not pop as estimates get corrected.
                            list: ListState::new(visible.len(), ListAlignment::Top, px(400.)),
                            visible,
                            processes,
                            total,
                            window,
                            at: Instant::now(),
                        })
                    }
                    Err(e) => {
                        eprintln!("full process scan failed: {e}");
                        FullScan::Failed
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    pub fn proc_filter_open(&self) -> bool {
        self.proc_filter_open
    }

    pub fn proc_filter_input(&self) -> Option<&Entity<InputState>> {
        self.proc_filter.as_ref()
    }

    /// The lowercased filter query; empty while the filter is closed.
    pub fn proc_filter_text(&self) -> &str {
        &self.proc_filter_text
    }

    /// Show or hide the name filter, creating the input on first use.
    pub fn toggle_proc_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.proc_filter_open {
            self.proc_filter_open = false;
            // `set_value` deliberately emits no Change event, so the mirror
            // and the full-scan rows are brought back by hand.
            if let Some(input) = &self.proc_filter {
                input.update(cx, |input, cx| input.set_value("", window, cx));
            }
            self.proc_filter_text.clear();
            self.refresh_full_scan_filter();
        } else {
            self.proc_filter_open = true;
            if self.proc_filter.is_none() {
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(i18n::tr("processes.filter_placeholder"))
                        // Esc clears. Goes through `replace_text`, which —
                        // unlike `set_value` — emits Change, so the mirror
                        // and the full-scan rows follow without extra wiring.
                        .clean_on_escape()
                });
                cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.proc_filter_text = input.read(cx).value().to_lowercase();
                        this.refresh_full_scan_filter();
                        cx.notify();
                    }
                })
                .detach();
                self.proc_filter = Some(input);
            }
            // Focus so typing can start without a second click.
            if let Some(input) = &self.proc_filter {
                input.read(cx).focus_handle(cx).focus(window, cx);
            }
        }
        cx.notify();
    }

    /// Recompute which full-scan rows the filter keeps, and rebuild the
    /// list state to match — `gpui::list` is told its row count up front,
    /// so a changed row set is a new list, scrolled back to the top.
    fn refresh_full_scan_filter(&mut self) {
        let filter = mem::take(&mut self.proc_filter_text);
        if let FullScan::Ready(data) = &mut self.full_scan {
            data.visible = filtered_indices(&data.processes, &filter);
            data.list = ListState::new(data.visible.len(), ListAlignment::Top, px(400.));
        }
        if let FullAppScan::Ready(data) = &mut self.full_app_scan {
            data.visible = filtered_group_indices(&data.groups, &filter);
            data.list = ListState::new(data.visible.len(), ListAlignment::Top, px(400.));
        }
        self.proc_filter_text = filter;
    }

    pub fn selected_pid(&self) -> Option<u32> {
        self.selected_pid
    }

    /// Clicking the open row closes it, as in the design.
    pub fn toggle_pid(&mut self, pid: u32, cx: &mut Context<Self>) {
        self.selected_pid = if self.selected_pid == Some(pid) {
            None
        } else {
            Some(pid)
        };
        cx.notify();
    }

    pub fn selected_app(&self) -> Option<u32> {
        self.selected_app
    }

    pub fn toggle_app(&mut self, root_pid: u32, cx: &mut Context<Self>) {
        self.selected_app = if self.selected_app == Some(root_pid) {
            None
        } else {
            Some(root_pid)
        };
        cx.notify();
    }

    pub fn selected_alert(&self) -> Option<&(String, String)> {
        self.selected_alert.as_ref()
    }

    /// Clicking the open card closes it, as with process rows.
    pub fn toggle_alert(&mut self, key: &str, name: &str, cx: &mut Context<Self>) {
        let id = (key.to_string(), name.to_string());
        self.selected_alert = if self.selected_alert.as_ref() == Some(&id) {
            None
        } else {
            Some(id)
        };
        cx.notify();
    }

    // ---- window --------------------------------------------------------

    /// Where the main window was last seen. Used when reopening without a
    /// tray anchor (the menu's "Show Window"), so it doesn't jump to centre.
    pub fn window_bounds(&self) -> Option<Bounds<Pixels>> {
        self.window_bounds
    }

    /// Last known display scale factor, mirrored from the main window. Only
    /// a fallback: on macOS the menu bar's own factor is read from
    /// `NSScreen`, which is both more accurate and available with no window.
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    /// Called from the root view's `render` on every frame — only notify
    /// when something changed, or observers would wake up continuously.
    pub fn set_window_metrics(
        &mut self,
        bounds: Bounds<Pixels>,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) {
        if self.window_bounds != Some(bounds) || self.scale_factor != scale_factor {
            self.window_bounds = Some(bounds);
            self.scale_factor = scale_factor;
            cx.notify();
        }
    }

    /// Record that the window just closed itself because it lost focus.
    pub fn mark_auto_hidden(&mut self) {
        self.last_auto_hide = Some(Instant::now());
    }

    /// Did an auto-hide happen within `window`? Consumes the mark, so it
    /// only ever answers `true` once — see `TOGGLE_GRACE` in `main.rs`.
    pub fn took_recent_auto_hide(&mut self, window: Duration) -> bool {
        self.last_auto_hide
            .take()
            .is_some_and(|at| at.elapsed() < window)
    }
}

/// Indices of the processes whose name contains `filter`, matched with
/// both sides lowercased; the whole range when the filter is empty.
fn filtered_indices(processes: &[ProcessSnapshot], filter: &str) -> Vec<usize> {
    processes
        .iter()
        .enumerate()
        .filter(|(_, p)| filter.is_empty() || p.name.to_lowercase().contains(filter))
        .map(|(i, _)| i)
        .collect()
}

fn filtered_group_indices(groups: &[ProcessGroupSnapshot], filter: &str) -> Vec<usize> {
    groups
        .iter()
        .enumerate()
        .filter(|(_, g)| filter.is_empty() || g.name.to_lowercase().contains(filter))
        .map(|(i, _)| i)
        .collect()
}

/// `Global` wrapper around the state entity: `cx.global::<ZStatsGlobalStore>()`
/// reaches it from anywhere that holds an `App`, including the tray handler
/// and the collection task, which both run with no window at all.
#[derive(Clone)]
pub struct ZStatsGlobalStore(Entity<ZStatsAppState>);

impl ZStatsGlobalStore {
    pub fn new(state: Entity<ZStatsAppState>) -> Self {
        Self(state)
    }
}

impl Global for ZStatsGlobalStore {}

impl Deref for ZStatsGlobalStore {
    type Target = Entity<ZStatsAppState>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Write one `zstats -add` key into `<dir>/config.toml` and return the
/// saved file. The Config tab and the Alerts chips both go through this
/// so they share the CLI's validation.
pub(crate) fn persist_setting(dir: &Path, key: &str, value: &str) -> Result<FileConfig, String> {
    let mut file = zstats::settings::load(dir).map_err(|e| e.to_string())?;
    zstats::settings::apply_add(&mut file, key, value)?;
    zstats::settings::save(dir, &file).map_err(|e| e.to_string())?;
    Ok(file)
}

/// Write a default `config.toml`. Absent keys are zstats builtins; any
/// per-subject override in the previous file is gone.
pub(crate) fn reset_config(dir: &Path) -> Result<FileConfig, String> {
    let file = FileConfig::default();
    zstats::settings::save(dir, &file).map_err(|e| e.to_string())?;
    Ok(file)
}

/// `[collector]` and `[daemon]` are baked into `LocalCollector` at
/// construction. `[alerts]` is the one section `reload_settings` re-reads.
fn setting_rebuilds_collector(key: &str) -> bool {
    !matches!(
        key,
        "alert-cpu"
            | "alert-mem"
            | "alert-app-cpu"
            | "alert-app-mem"
            | "alert-disk"
            | "alert-cooldown"
            | "alert-pressure"
            | "alert-template"
    )
}

/// Screen rectangle of the tray icon, in **physical** pixels with a top-left
/// origin — that's what `tray_icon` reports. Converting to gpui's logical
/// `Pixels` needs the scale factor, see [`ZStatsAppState::scale_factor`].
#[derive(Clone, Copy, Debug)]
pub struct TrayAnchor {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::env;
    use std::fs;
    use std::process;
    use zstats::AlertDetail;

    fn snap(pid: u32, name: &str) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            name: name.into(),
            cmd: String::new(),
            cpu_usage_percent: 0.0,
            cpu_time_ms: 0,
            memory_bytes: 0,
            phys_footprint_bytes: None,
            virtual_memory_bytes: 0,
            run_time_secs: 0,
            parent_pid: None,
            user_id: None,
            status: String::new(),
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    /// The query arrives lowercased (the store lowers it on every change);
    /// names of any case must still match, and an empty query keeps all.
    #[test]
    fn name_filter_is_case_insensitive() {
        let procs = [
            snap(1, "WeChat"),
            snap(2, "kernel_task"),
            snap(3, "wechatwebview"),
        ];
        assert_eq!(filtered_indices(&procs, ""), vec![0, 1, 2]);
        assert_eq!(filtered_indices(&procs, "wechat"), vec![0, 2]);
        assert_eq!(filtered_indices(&procs, "task"), vec![1]);
        assert!(filtered_indices(&procs, "xcode").is_empty());
    }

    #[test]
    fn snooze_mutes_by_episode_and_expires() {
        let mut state = ZStatsAppState::new();
        let event = cpu_alert(7);

        // Active snooze mutes this episode, and only this episode: the
        // same pid's MEMORY alert is a different story and stays loud.
        state.snoozed.insert(
            Episode::of(&event),
            Snooze {
                until: Instant::now() + Duration::from_secs(3600),
                until_label: "14:32".into(),
            },
        );
        assert!(state.banner_snoozed(&event));
        assert!(!state.banner_snoozed(&mem_alert(7)));
        assert_eq!(state.snoozed_until(&event), Some("14:32"));

        // Past the deadline the entry is pruned on the next check.
        state.snoozed.get_mut(&Episode::of(&event)).unwrap().until =
            Instant::now() - Duration::from_secs(1);
        assert!(!state.banner_snoozed(&event));
        assert!(state.snoozed.is_empty(), "expired snooze should be pruned");
    }

    fn cpu_alert(pid: u32) -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::Process {
                pid,
                name: format!("p{pid}"),
            },
            detail: AlertDetail::Cpu {
                avg_percent: 90.0,
                threshold_percent: 30.0,
                window: Duration::from_secs(60),
                runaway: false,
            },
            repeat_after: None,
        }
    }

    fn mem_alert(pid: u32) -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::Process {
                pid,
                name: format!("p{pid}"),
            },
            detail: AlertDetail::Memory {
                avg_bytes: 1 << 30,
                share_percent: 40.0,
                threshold_percent: 25.0,
                threshold_bytes: 4 << 30,
                window: Duration::from_secs(60),
            },
            repeat_after: None,
        }
    }

    /// zstats reports a crossing once and follows up once 30 minutes later.
    /// Both describe the same episode, and a list that appends a card per
    /// event turns one problem into two — then lets a flapping process crowd
    /// everything else out of the 20 slots.
    #[test]
    fn repeat_reports_merge_into_one_episode() {
        let mut state = ZStatsAppState::new();
        let t0 = Instant::now();

        state.record_alert(cpu_alert(7), t0);
        state.record_alert(cpu_alert(7), t0 + Duration::from_secs(1800));
        assert_eq!(state.alerts().len(), 1, "same process, same measure");
        assert_eq!(state.alerts()[0].reports, 2);
        assert_eq!(state.alerts()[0].span(), Some(Duration::from_secs(1800)));

        // Same process over on a *different* measure is a separate story.
        state.record_alert(mem_alert(7), t0);
        // A different process likewise.
        state.record_alert(cpu_alert(8), t0);
        assert_eq!(state.alerts().len(), 3);

        // Resurfacing moves an episode back to the front without duplicating.
        state.record_alert(cpu_alert(7), t0 + Duration::from_secs(3600));
        assert_eq!(state.alerts().len(), 3);
        assert_eq!(state.alerts()[0].reports, 3);
        assert_eq!(state.alerts()[0].seq, 1, "still the episode opened first");
    }

    /// The id has to outlive reordering — it is what element state (hover,
    /// the expanded editor) is keyed on.
    #[test]
    fn episode_ids_are_unique_and_stable() {
        let mut state = ZStatsAppState::new();
        let t0 = Instant::now();
        for pid in 1..=3 {
            state.record_alert(cpu_alert(pid), t0);
        }
        let before: Vec<_> = state
            .alerts()
            .iter()
            .map(|a| (a.seq, a.event.kind()))
            .collect();
        // Push the oldest back to the front.
        state.record_alert(cpu_alert(1), t0 + Duration::from_secs(60));
        let after: Vec<_> = state.alerts().iter().map(|a| a.seq).collect();
        assert_eq!(after, vec![1, 3, 2], "order changes, ids do not");
        assert_eq!(before.len(), 3);
        let unique: HashSet<u64> = after.iter().copied().collect();
        assert_eq!(unique.len(), 3, "ids must not collide");
    }

    #[test]
    fn sustained_bar_follows_the_configured_alert_threshold() {
        let state = ZStatsAppState::new();
        // No config loaded yet: zstats' own default of 30%, thirded.
        assert!((state.sustained_bar() - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn sort_cycles_through_every_option_and_returns() {
        // One button cycles the list, so the cycle must be closed — otherwise
        // an ordering becomes unreachable.
        let mut seen = vec![ProcSort::default()];
        let mut cur = ProcSort::default();
        for _ in 0..8 {
            cur = cur.next();
            if cur == ProcSort::default() {
                break;
            }
            seen.push(cur);
        }
        assert_eq!(cur, ProcSort::default(), "cycle should return to start");
        assert_eq!(seen.len(), 3, "every ordering should be reachable");
    }

    #[test]
    fn every_tab_has_a_distinct_index() {
        // Scroll state is keyed by this, so a collision would make two tabs
        // share a scroll position.
        let mut seen: Vec<usize> = Tab::ALL.iter().map(|t| t.index()).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), Tab::ALL.len());
    }

    #[test]
    fn collector_keys_rebuild_and_alert_keys_reload() {
        assert!(setting_rebuilds_collector("collect-processes"));
        assert!(setting_rebuilds_collector("process-disk-io"));
        assert!(setting_rebuilds_collector("process-interval"));
        assert!(setting_rebuilds_collector("max-processes"));
        assert!(setting_rebuilds_collector("interval"));
        assert!(!setting_rebuilds_collector("alert-cpu"));
        assert!(!setting_rebuilds_collector("alert-mem"));
        assert!(!setting_rebuilds_collector("alert-cooldown"));
        assert!(!setting_rebuilds_collector("alert-pressure"));
        assert!(!setting_rebuilds_collector("alert-template"));
    }

    fn scratch(name: &str) -> PathBuf {
        env::temp_dir().join(format!("zstats-app-settings-{name}-{}", process::id()))
    }

    #[test]
    fn persist_setting_round_trips_collector_and_alerts() {
        let dir = scratch("roundtrip");
        let _ = fs::remove_dir_all(&dir);

        let file = persist_setting(&dir, "process-disk-io", "true").unwrap();
        assert!(file.collector.as_ref().unwrap().collect_process_disk_io);

        // A second write must not clobber the first section.
        let file = persist_setting(&dir, "alert-cpu", "50").unwrap();
        assert_eq!(file.alerts.cpu, Some(50.0));
        assert!(file.collector.as_ref().unwrap().collect_process_disk_io);

        persist_setting(&dir, "collect-processes", "false").unwrap();
        let reloaded = zstats::settings::load(&dir).unwrap();
        assert!(!reloaded.collector.as_ref().unwrap().collect_processes);
        assert!(reloaded.collector.as_ref().unwrap().collect_process_disk_io);
        assert_eq!(reloaded.alerts.cpu, Some(50.0));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_setting_rejects_unknown_keys() {
        let dir = scratch("unknown");
        let _ = fs::remove_dir_all(&dir);
        assert!(persist_setting(&dir, "not-a-key", "true").is_err());
        assert!(
            !dir.join("config.toml").exists(),
            "a rejected key must not create the file"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_config_clears_overrides_and_collector() {
        let dir = scratch("reset");
        let _ = fs::remove_dir_all(&dir);

        persist_setting(&dir, "process-disk-io", "true").unwrap();
        persist_setting(&dir, "alert-cpu", "50").unwrap();
        persist_setting(&dir, "alert-cpu", "ghostty=100").unwrap();
        persist_setting(&dir, "collect-processes", "false").unwrap();

        let file = reset_config(&dir).unwrap();
        assert!(file.collector.is_none());
        assert!(file.alerts.cpu.is_none());
        assert!(file.alerts.cpu_overrides.is_empty());

        let reloaded = zstats::settings::load(&dir).unwrap();
        assert!(reloaded.collector.is_none());
        assert!(reloaded.alerts.cpu.is_none());
        assert!(reloaded.alerts.cpu_overrides.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }
}
