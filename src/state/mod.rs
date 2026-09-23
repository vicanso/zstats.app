//! Application-level state.
//!
//! The window is only a rendering layer, and it spends most of its life off
//! screen: the panel is ordered out rather than destroyed (see `window_ext`),
//! and gpui discards element state for anything it did not paint this frame.
//! So everything that has to survive a hide → reveal round trip belongs here
//! rather than in the root view — window geometry, the selected tab, per-tab
//! scroll offsets. Collected metrics are the main tenant, and sampling runs
//! whether or not a window exists at all.
//!
//! Disk analysis lives in [`analysis`], alert episodes in [`alerts`]: both
//! are store-owned, but changing either used to mean editing this whole file.

mod alerts;
mod analysis;

pub use alerts::SeenAlert;
pub use analysis::{BigFiles, DiskAnalysis, Expansion};

use alerts::{AlertBook, keep_alert};
use analysis::Analysis;

use crate::alerttpl;
use crate::cachepreset;
use crate::cleanhints;
use crate::fullscan::{self, GroupScan, Scan};
use crate::history;
use crate::history::Spender;
use crate::i18n;
use crate::metrics;
use crate::prefs;
use crate::procscan;
use crate::spaceinfo::{self, SpaceInfo};
use crate::tray;
use crate::trend::{self, AppTrend, MIB};
use crate::updater;
pub use crate::watch::SustainedNotice;
use crate::watch::{AbnormalWatch, NetActivity, SustainedRule, SustainedWatch};
use gpui::{
    App, AppContext, Bounds, Context, Entity, Focusable, Global, ListAlignment, ListState, Pixels,
    ScrollHandle, Window, px,
};
use gpui_kit::component::input::{InputEvent, InputState};
use std::array;
use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::collections::hash_map::{DefaultHasher, Entry};
use std::hash::{Hash, Hasher};
use std::mem;
use std::ops::Deref;
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zstats::settings::FileConfig;
use zstats::snapshot::{ProcessGroupSnapshot, ProcessSnapshot};
use zstats::{AlertEvent, Tick};

/// Used when config.toml sets no `alert-cpu` — zstats' own default is 30%.
/// The sustained bar is that line divided by `prefs::sustained_divisor`
/// (3 unless app.toml says): derived rather than fixed so tightening
/// `alert-cpu` tightens this too.
const SUSTAINED_FALLBACK_ALERT: f64 = 30.0;

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
    /// Combined disk read+write rate. Same truncated-list contract as
    /// Memory: the collector already ranked by CPU then memory, so this
    /// only reorders that set. Name stays last — it ranks by no magnitude.
    DiskIo,
    Name,
}

impl ProcSort {
    /// Cycles through the orderings; the control is one button, not a menu.
    pub fn next(self) -> Self {
        match self {
            ProcSort::Cpu => ProcSort::Memory,
            ProcSort::Memory => ProcSort::DiskIo,
            ProcSort::DiskIo => ProcSort::Name,
            ProcSort::Name => ProcSort::Cpu,
        }
    }

    /// i18n key for the short label on the control.
    pub fn label_key(self) -> &'static str {
        match self {
            ProcSort::Cpu => "processes.sort_cpu",
            ProcSort::Memory => "processes.sort_memory",
            ProcSort::DiskIo => "processes.sort_io",
            ProcSort::Name => "processes.sort_name",
        }
    }

    /// i18n key for the tooltip. Memory / IO / name only reorder the already
    /// truncated list — that caveat has to live somewhere, and the chip
    /// itself is too short to carry it.
    pub fn tip_key(self) -> &'static str {
        match self {
            ProcSort::Cpu => "processes.sort_cpu_tip",
            ProcSort::Memory => "processes.sort_memory_tip",
            ProcSort::DiskIo => "processes.sort_io_tip",
            ProcSort::Name => "processes.sort_name_tip",
        }
    }

    /// Tooltip on the whole-table listing, where the same chip really
    /// does rank the machine — the truncated-list caveat would be a lie.
    pub fn full_tip_key(self) -> &'static str {
        match self {
            ProcSort::Cpu => "processes.sort_cpu_tip_full",
            ProcSort::Memory => "processes.sort_memory_tip_full",
            ProcSort::DiskIo => "processes.sort_io_tip_full",
            ProcSort::Name => "processes.sort_name_tip_full",
        }
    }
}

/// How to order the Apps list. Separate from [`ProcSort`]: the two tabs
/// are different sets, and carrying a sort across them would make
/// switching tabs look like the list had jumped.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AppSort {
    /// Live CPU of each process tree, the collector's own ranking.
    #[default]
    Cpu,
    Memory,
}

impl AppSort {
    pub fn next(self) -> Self {
        match self {
            AppSort::Cpu => AppSort::Memory,
            AppSort::Memory => AppSort::Cpu,
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            AppSort::Cpu => "apps.sort_cpu",
            AppSort::Memory => "apps.sort_memory",
        }
    }

    pub fn tip_key(self) -> &'static str {
        match self {
            AppSort::Cpu => "apps.sort_cpu_tip",
            AppSort::Memory => "apps.sort_memory_tip",
        }
    }

    pub fn full_tip_key(self) -> &'static str {
        match self {
            AppSort::Cpu => "apps.sort_cpu_tip_full",
            AppSort::Memory => "apps.sort_memory_tip_full",
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
    Apps,
    Processes,
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
        Tab::Apps,
        Tab::Processes,
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
            Tab::Apps => "Apps",
            Tab::Processes => "Processes",
            Tab::Hardware => "Hardware",
            Tab::Net => "Network",
            Tab::Alerts => "Alerts",
            Tab::History => "History",
        }
    }

    /// File key in `app.toml`; `None` for Overview, which is the default
    /// and is expressed by leaving the key out.
    pub fn pref_key(self) -> Option<&'static str> {
        match self {
            Tab::Overview => None,
            Tab::Apps => Some("apps"),
            Tab::Processes => Some("processes"),
            Tab::Hardware => Some("hardware"),
            Tab::Net => Some("network"),
            Tab::Alerts => Some("alerts"),
            Tab::History => Some("history"),
        }
    }

    /// Inverse of [`Self::pref_key`]. Unknown or missing → Overview.
    pub fn from_pref_key(key: Option<&str>) -> Self {
        match key {
            Some("apps") => Tab::Apps,
            Some("processes") => Tab::Processes,
            Some("hardware") => Tab::Hardware,
            Some("network") => Tab::Net,
            Some("alerts") => Tab::Alerts,
            Some("history") => Tab::History,
            _ => Tab::Overview,
        }
    }

    /// Tooltip / spoken name in the active locale.
    pub fn title(self) -> String {
        i18n::tr(match self {
            Tab::Overview => "tabs.overview",
            Tab::Apps => "tabs.apps",
            Tab::Processes => "tabs.processes",
            Tab::Hardware => "tabs.hardware",
            Tab::Net => "tabs.network",
            Tab::Alerts => "tabs.alerts",
            Tab::History => "tabs.history",
        })
    }
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
    /// Last recorded footprint minus the first — who grew through the
    /// day. The leak question at day scale; `trend.rs` asks it over an
    /// hour. Same caveat as the peak: recorded minutes only.
    MemoryGrowth,
}

impl HistorySort {
    pub fn next(self) -> Self {
        match self {
            HistorySort::CpuTime => HistorySort::PeakMemory,
            HistorySort::PeakMemory => HistorySort::MemoryGrowth,
            HistorySort::MemoryGrowth => HistorySort::CpuTime,
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            HistorySort::CpuTime => "history.sort_cpu",
            HistorySort::PeakMemory => "history.sort_mem",
            HistorySort::MemoryGrowth => "history.sort_growth",
        }
    }

    pub fn tip_key(self) -> &'static str {
        match self {
            HistorySort::CpuTime => "history.sort_cpu_tip",
            HistorySort::PeakMemory => "history.sort_mem_tip",
            HistorySort::MemoryGrowth => "history.sort_growth_tip",
        }
    }
}

/// The version check / assisted download, for the About page.
pub enum UpdateStatus {
    Checking,
    Done(updater::UpdateCheck),
    Downloading {
        received: u64,
        /// 0 while the server has not said.
        total: u64,
        notes: String,
    },
    /// Downloaded and verified; the image is being mounted and the
    /// bundle copied into place — a couple of seconds of hdiutil and
    /// ditto, distinct from Downloading so the bar never sits at 100%
    /// pretending bytes are still moving.
    Installing {
        notes: String,
    },
    /// The update landed. `manual` is the fallback path: nothing to
    /// replace in place (bare binary, unwritable target), so the image
    /// was opened for the classic drag and the caption still asks for
    /// it. `false` means the bundle was swapped under the running app
    /// and one restart finishes the update.
    Installed {
        manual: bool,
    },
    DownloadFailed {
        version: String,
        error: String,
        notes: String,
    },
}

/// The clean-hints update fetch, for the Config page's status line.
pub enum HintsSync {
    Running,
    Done(cleanhints::RemoteUpdate),
}

/// The Caches-preset roots fetch — same question, different file.
pub enum CachesSync {
    Running,
    Done(cachepreset::RemoteUpdate),
}

/// The alert-template fetch, and the revert beside it, for the Config
/// page's status line. Both land here because they are the same
/// question to the reader — "what did that button just do to the table
/// zstats is running with" — and only one of them can be in flight.
pub enum TemplateSync {
    Running,
    Done(alerttpl::RemoteUpdate),
    /// The override was deleted and the compiled-in table is live again.
    Reverted,
    /// There was no override to delete — the built-in table was already
    /// what zstats was using.
    NothingToRevert,
    RevertFailed(String),
}

/// A successfully ejected volume stays hidden at most this long.
///
/// The normal exit is the snapshot dropping it, which happens on the
/// collector's disk cadence. This is the backstop for the case that
/// never resolves that way: the user replugs the drive and it mounts on
/// the same path, so the volume never disappears and the hide would
/// otherwise be permanent. A minute is several disk refreshes at the
/// panel's default cadence, so it only ever fires for a replug.
const EJECT_HIDE_MAX: Duration = Duration::from_secs(60);

/// History rows younger than this survive a reveal onto the tab. The
/// records file gains one line a minute, so a re-read inside that
/// minute can only return what is already on screen.
const HISTORY_FRESH: Duration = Duration::from_secs(60);

/// Whether the panel is on screen. Tab-entry work (History read,
/// `tmutil` probe, topology fetch) keys off this rather than the
/// selected tab alone: the tab outlives a hide and, since it is
/// remembered, a restart too.
fn panel_visible(cx: &App) -> bool {
    cx.try_global::<metrics::CollectorPace>()
        .is_some_and(|p| p.is_visible())
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

/// Full process table, for naming every member of an expanded Apps tree.
/// A tree whose memory footprint has climbed through the hour and is
/// still at its high — the leak shape (`trend::climb`). Display and a
/// silent banner, never an `AlertEvent`: a climb crosses no line by
/// definition, which is exactly why it has to be said somewhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryCreep {
    pub name: String,
    /// The tree's root — the pid AppKit knows the application by, so
    /// the banner can ask `active.rs` whether anyone is using it. Not
    /// an identity: trees are keyed by name (`trend::tree_key`),
    /// because a root pid changes across a restart.
    pub root_pid: u32,
    /// Newest minutes against the earliest reported ones in the hour.
    pub climb_bytes: u64,
    /// What the tree holds on the latest sample.
    pub now_bytes: u64,
}

/// The resident tick only keeps `max-processes`, so a group's
/// `process_count` can be 37 while the live table names four of them.
///
/// CPU on this table is unusable (one pass, no baseline) — the expansion
/// paints rates from the tick when the pid is there, and `—` otherwise.
/// Same photograph feeds the job faces (`login` → `cargo`): the tick
/// drops the idle shell and carries no process groups, so a collapsed
/// row cannot name its job until this lands. Hide still drops it;
/// collapse does not, or folding would rename the row back to `login`.
/// While Apps or Overview is on screen (or a row is held open) it
/// refreshes on the process cadence ([`metrics::PANEL_PROCESS_INTERVAL`]),
/// not the 2s CPU tick.
#[derive(Default)]
pub enum MemberTable {
    #[default]
    Off,
    Running,
    Ready {
        processes: Arc<Vec<ProcessSnapshot>>,
        /// pid → process group, taken in the same breath as the table
        /// (`procscan::process_groups`): the kernel's job boundaries
        /// that `trend::tree_face` names a bare tree by.
        pgids: Arc<HashMap<u32, u32>>,
        at: Instant,
        /// A refresh in flight keeps the last photograph on screen.
        refreshing: bool,
    },
    /// First photograph failed. Retried after
    /// [`metrics::PANEL_PROCESS_INTERVAL`], same clock as a Ready
    /// refresh — without this, Overview's job face stays wrong until
    /// hide resets the table to Off.
    Failed {
        at: Instant,
    },
}

/// What [`ZStatsAppState::ensure_member_table`] should do with the
/// current photograph. Extracted so the retry clock can be tested
/// without spawning a collector.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MemberTableNext {
    /// In flight, or the last attempt is still fresh.
    Hold,
    /// Start (or refresh) now.
    Restart,
    /// `Off`: fall through to the "do we even need a table" checks.
    FallThrough,
}

fn member_table_next(table: &MemberTable, now: Instant, interval: Duration) -> MemberTableNext {
    match table {
        MemberTable::Running => MemberTableNext::Hold,
        MemberTable::Ready {
            refreshing: true, ..
        } => MemberTableNext::Hold,
        MemberTable::Ready { at, .. } if now.saturating_duration_since(*at) < interval => {
            MemberTableNext::Hold
        }
        MemberTable::Ready { .. } => MemberTableNext::Restart,
        MemberTable::Failed { at } if now.saturating_duration_since(*at) < interval => {
            MemberTableNext::Hold
        }
        MemberTable::Failed { .. } => MemberTableNext::Restart,
        MemberTable::Off => MemberTableNext::FallThrough,
    }
}

pub struct ZStatsAppState {
    window_bounds: Option<Bounds<Pixels>>,
    scale_factor: f32,
    last_auto_hide: Option<Instant>,
    pub(crate) latest: Option<Tick>,
    pub(crate) alert_book: AlertBook,
    pub(crate) tab: Tab,
    selected_pid: Option<u32>,
    selected_app: Option<u32>,
    settings: Option<FileConfig>,
    /// UI filter: show only the abnormal entries, not the whole table.
    only_abnormal: bool,
    /// UI filter: reveal the interfaces the recency filter would hide.
    show_unused_nets: bool,
    /// UI filter: reveal every temperature sensor, not just the preview.
    show_all_sensors: bool,
    /// Directory analyser + large-file query. Survives hide for the
    /// analyser (see [`DiskAnalysis`]); the listing is query-like and
    /// reset on hide.
    pub(crate) analysis: Analysis,
    /// The boot volume's purgeable-space / snapshot readout, refreshed
    /// lazily while Hardware is the visible tab (throttled below) — a
    /// panel-owned query, deliberately not a Monitor metric.
    space: Option<SpaceInfo>,
    space_at: Option<Instant>,
    space_inflight: bool,
    /// The popover panel. Hidden rather than destroyed on macOS, so this
    /// outlives hide → show. Settings and storage already keep theirs;
    /// `cx.windows().first()` is SlotMap insertion order, not "the
    /// panel", and would `orderOut` the wrong window once another exists.
    panel_window: Option<gpui::AnyWindowHandle>,
    /// The settings window, if one was ever opened. Kept so a second
    /// click focuses the existing window; a handle whose window the user
    /// closed fails its update and a fresh window is built instead.
    settings_window: Option<gpui::AnyWindowHandle>,
    /// The disk-space window (large files + the analyser), same
    /// reuse-or-rebuild contract as [`Self::settings_window`].
    storage_window: Option<gpui::AnyWindowHandle>,
    /// Bumped by the Interface page's pref setters ([`crate::repaint`]).
    /// Prefs live in `app.toml`, not this struct, so a collector tick
    /// must not look like a chip click to the settings window's observer.
    ui_epoch: u64,
    /// Volumes this session has successfully ejected, and when. They
    /// are hidden from the Hardware tab until the snapshot stops
    /// listing them — see [`Self::mark_ejected`].
    pub(crate) ejected: HashMap<String, Instant>,
    proc_sort: ProcSort,
    app_sort: AppSort,
    /// The three observers that answer questions zstats' own rules cannot —
    /// see [`crate::watch`]. They own their clocks and thresholds; this type
    /// only feeds them samples and reads the verdicts back out.
    sustained: SustainedWatch,
    abnormal: AbnormalWatch,
    net: NetActivity,
    /// The hour of per-tree CPU history behind Overview's climbing rows
    /// — same observer class as the three above (see `trend.rs`).
    trend: AppTrend,
    /// The same hour of rings, fed with each tree's memory footprint
    /// in MB instead of CPU% — the leak question. A footprint that
    /// went 300 MB → 1.5 GB over an hour has crossed nothing, and
    /// zstats' rules ask only "over the line now"; this is the shape
    /// that is too late by the time it is. Display plus one silent
    /// banner, never an `AlertEvent`. `u16` MB caps at ~64 GB per
    /// tree, which is the whole machine.
    mem_trend: AppTrend,
    /// Trees whose climb has been announced within the last
    /// [`trend::CREEP_REARM`] — the re-arm set, pruned by that clock
    /// and never by the figure, so a creep is one banner an hour, not
    /// one per crossing of the bar (`take_memory_creep_notices`). The
    /// value is when the climb was first named: the Alerts tab's
    /// read-only card sorts into the live list by it (`creeps_active`),
    /// same as the sustained card sorts by its notice age.
    creep_notified: HashMap<String, Instant>,
    /// Today's history, ranked. `None` until the tab is first opened — the
    /// read walks a day of JSONL and there is no reason to pay for it before
    /// somebody asks.
    history: Option<Vec<Spender>>,
    /// When `history` last landed, so a reveal onto History inside
    /// [`HISTORY_FRESH`] keeps the rows instead of re-reading.
    history_loaded_at: Option<Instant>,
    /// The window `history` was (or is being) read for.
    history_range: HistoryRange,
    /// The order the History list shows.
    history_sort: HistorySort,
    /// The last (or in-flight) clean-hints update fetch.
    hints_sync: Option<HintsSync>,
    /// The last (or in-flight) Caches-preset roots fetch.
    caches_sync: Option<CachesSync>,
    template_sync: Option<TemplateSync>,
    /// A newer release a silent check found (its tag) — the settings
    /// gear's dot. Loaded from the check file at launch, refreshed by
    /// every check, cleared by comparison once the update is installed.
    update_nudge: Option<String>,
    /// The gear dot's other half: a probe found a published alert table
    /// that differs from the one in force (`alerttpl::nudge`).
    template_nudge: bool,
    /// The version the user chose to skip, while it still applies. The
    /// About page states it rather than going blank, and offers the way
    /// back — a choice with no visible record reads as a dead button.
    update_ignored: Option<String>,
    /// Throttles the *probe* (a tiny file read) to once an hour; the
    /// check itself is throttled to days by the file's timestamp.
    auto_check_probe_at: Option<Instant>,
    auto_check_inflight: bool,
    /// The last (or in-flight) version check.
    update_status: Option<UpdateStatus>,
    /// The whole-table listing, only ever populated on request.
    full_scan: FullScan,
    /// The whole-tree listing for the Apps tab, only ever populated on request.
    full_app_scan: FullAppScan,
    /// Full process table for Apps expansions. Separate from [`full_scan`]:
    /// opening All on Processes must not be how you get Chrome's helpers,
    /// and landing this must not flip that tab into its full listing.
    member_table: MemberTable,
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
    history_rows_scroll: ScrollHandle,
    app_rows_scroll: ScrollHandle,
    /// One-shot "scroll the Apps list to the selected row on the next
    /// paint", armed by [`Self::reveal_app`]. A `Cell` because the
    /// consumer is the render pass, which holds `&self`; taken once,
    /// so the reader's own scrolling wins from the frame after.
    app_reveal: Cell<bool>,
    /// Same one-shot for the Processes list, armed by [`Self::reveal_pid`].
    proc_reveal: Cell<bool>,
}

impl Default for ZStatsAppState {
    fn default() -> Self {
        Self {
            window_bounds: None,
            scale_factor: 1.0,
            last_auto_hide: None,
            latest: None,
            alert_book: AlertBook::default(),
            tab: Tab::default(),
            selected_pid: None,
            selected_app: None,
            settings: None,
            only_abnormal: false,
            show_unused_nets: false,
            show_all_sensors: false,
            analysis: Analysis::default(),
            space: None,
            space_at: None,
            space_inflight: false,
            panel_window: None,
            settings_window: None,
            storage_window: None,
            ui_epoch: 0,
            ejected: HashMap::new(),
            proc_sort: ProcSort::default(),
            app_sort: AppSort::default(),
            sustained: SustainedWatch::default(),
            abnormal: AbnormalWatch::default(),
            net: NetActivity::default(),
            trend: AppTrend::default(),
            mem_trend: AppTrend::default(),
            creep_notified: HashMap::new(),
            history: None,
            history_loaded_at: None,
            history_range: HistoryRange::default(),
            history_sort: HistorySort::default(),
            hints_sync: None,
            caches_sync: None,
            template_sync: None,
            update_status: None,
            update_nudge: updater::nudge(),
            template_nudge: alerttpl::nudge(),
            update_ignored: updater::ignored(),
            auto_check_probe_at: None,
            auto_check_inflight: false,
            full_scan: FullScan::default(),
            full_app_scan: FullAppScan::default(),
            member_table: MemberTable::default(),
            proc_filter: None,
            proc_filter_open: false,
            proc_filter_text: String::new(),
            scroll: array::from_fn(|_| ScrollHandle::new()),
            proc_rows_scroll: ScrollHandle::new(),
            history_rows_scroll: ScrollHandle::new(),
            app_rows_scroll: ScrollHandle::new(),
            app_reveal: Cell::new(false),
            proc_reveal: Cell::new(false),
        }
    }
}

/// A live tree whose matchable `name` is this one. Name, not pid: a
/// restart changes the root, and a recycled pid must not inherit a
/// dead program's jump.
fn live_group_root(groups: &[ProcessGroupSnapshot], name: &str) -> Option<u32> {
    groups.iter().find(|g| g.name == name).map(|g| g.root_pid)
}

/// A live process still holding this history identity. Prefer the same
/// pid only while it still has that `name`; otherwise the first live
/// process with the name (restarted). Pid-only is rejected.
fn live_process_pid(processes: &[ProcessSnapshot], name: &str, pid: u32) -> Option<u32> {
    if processes.iter().any(|p| p.pid == pid && p.name == name) {
        return Some(pid);
    }
    processes.iter().find(|p| p.name == name).map(|p| p.pid)
}

impl ZStatsAppState {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- metrics -------------------------------------------------------

    /// Remember that `mount` was ejected, so the Hardware tab can drop
    /// its card now rather than at the end of the disk cadence.
    ///
    /// The one place the panel shows a machine state ahead of zstats,
    /// and it is bounded on both ends: it is only ever called after
    /// `diskutil eject` **returned success** — the OS itself saying the
    /// volume is gone, not the panel guessing — and the entry is
    /// dropped the moment a snapshot agrees (or after
    /// [`EJECT_HIDE_MAX`]). Without it the card outlives the volume by
    /// up to a full `disk_io_refresh_interval`, because zstats serves
    /// the whole disk list from cache between refreshes; waking the
    /// collector does not help, since that cadence is wall-clock.
    pub fn mark_ejected(&mut self, mount: String, cx: &mut Context<Self>) {
        self.ejected.insert(mount, Instant::now());
        cx.notify();
    }

    /// Whether a volume card should be withheld this frame.
    pub fn is_ejected(&self, mount: &str) -> bool {
        self.ejected.contains_key(mount)
    }

    /// Retire hide entries that have done their job — the volume is
    /// gone from the snapshot — or that have waited long enough.
    fn prune_ejected(&mut self, listed: &[String], now: Instant) {
        self.ejected.retain(|mount, at| {
            listed.iter().any(|m| m == mount) && now.duration_since(*at) < EJECT_HIDE_MAX
        });
    }

    /// Fold one collection round into the state. Returns the events that
    /// arrived this tick so the caller can deliver desktop notifications
    /// without walking the accumulated list.
    pub fn ingest(&mut self, tick: Tick, cx: &mut Context<Self>) -> Vec<AlertEvent> {
        let now = Instant::now();
        let wall = SystemTime::now();
        let fresh: Vec<AlertEvent> = tick
            .alerts
            .iter()
            .filter(|event| keep_alert(event))
            .cloned()
            .collect();
        if !fresh.is_empty() {
            for event in &fresh {
                self.record_alert(event.clone(), wall);
            }
            // Looking at the list as it arrives is the same as switching
            // to it: the spec is "you have not opened that tab", not a
            // count of undismissed cards. Hidden, or on another tab,
            // `record_alert` has already lit it.
            if self.alerts_are_showing(cx) {
                self.see_alerts();
            }
            // The file mirrors the list, so it is rewritten where the
            // list changes — which is only ever here. Tests drive
            // `record_alert` directly and touch no disk.
            self.persist_alerts();
        }

        if let Some(processes) = tick.snapshot.processes.as_deref() {
            self.sustained
                .record(processes, &tick.process_stats, self.sustained_rule(), now);
        }
        if let Some(nets) = tick.snapshot.networks.as_deref() {
            self.net.record(nets, now);
        }
        if let Some(groups) = tick.snapshot.process_groups.as_deref() {
            // Wall clock, not `Instant`: the trend's minute slots must
            // line up across a sleep, which a monotonic clock spans
            // inconsistently across platforms.
            let minute = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() / 60);
            self.trend.sample(
                minute,
                groups
                    .iter()
                    .map(|g| (trend::tree_key(g), g.cpu_usage_percent)),
            );
            // Same ring, the footprint in MB — the figure the memory rules
            // measure, RSS where the kernel refused one (same fallback as
            // every memory figure in the app).
            self.mem_trend.sample(
                minute,
                groups.iter().map(|g| {
                    let bytes = g.phys_footprint_bytes.unwrap_or(g.memory_bytes);
                    (trend::tree_key(g), (bytes / MIB) as f32)
                }),
            );
        }

        if !self.ejected.is_empty() {
            let listed: Vec<String> = tick
                .snapshot
                .disks
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|d| d.mount_point.clone())
                .collect();
            self.prune_ejected(&listed, now);
        }
        self.latest = Some(tick);
        self.note_memory_recovery(wall);
        // Piggyback on the tick rather than the render: views are pure
        // functions and cannot start work, and a fresh probe is only
        // interesting while someone is looking at the Hardware tab —
        // on screen, not merely selected: the tab survives hide (and a
        // restart), and the probe spawns `tmutil` once a minute.
        if self.tab == Tab::Hardware && panel_visible(cx) {
            self.ensure_space_info(cx);
        }
        self.prune_stale_alerts();
        self.maybe_auto_check_update(cx);
        // Views cannot start work. Hide drops the table. A still-open
        // expansion, or a job face the tick cannot name, asks from here
        // so the row title is `cargo` before anyone clicks, and a
        // 15s-old photograph is replaced.
        self.ensure_apps_topology(cx);
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

    /// The most recent collection, or `None` before the first one lands.
    pub fn latest(&self) -> Option<&Tick> {
        self.latest.as_ref()
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
        self.sustained.active(self.sustained_rule())
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
            / f64::from(prefs::sustained_divisor())
    }

    /// The sustained-load rule in force: the bar from `alert-cpu` and
    /// the panel's divisor, the duration from the panel's own file.
    /// Built per question rather than cached, so a picker change is
    /// in force on the next tick with no restart.
    pub fn sustained_rule(&self) -> SustainedRule {
        SustainedRule {
            bar: self.sustained_bar(),
            after: prefs::sustained_after(),
        }
    }

    /// Sustained-load notices raised by the last round, taken once.
    pub fn take_sustained_notices(&mut self) -> Vec<SustainedNotice> {
        self.sustained.take_notices()
    }

    /// How long this process has been holding a low-but-real CPU share, once
    /// that has gone on long enough to be worth saying.
    pub fn sustained_load(&self, pid: u32) -> Option<Duration> {
        self.sustained.duration_for(pid, self.sustained_rule())
    }

    /// Whether an interface has carried traffic recently enough for a row.
    pub fn net_is_recent(&self, interface: &str) -> bool {
        self.net.is_recent(interface)
    }

    /// How far this tree's recent minutes sit above its earlier-hour
    /// average, in percent-of-one-core points. `None` until the trend
    /// has enough reported history for a verdict.
    pub fn app_rise(&self, name: &str) -> Option<f32> {
        self.trend.rise(name)
    }

    /// How far this tree's footprint has climbed across the hour and
    /// is still holding, in bytes. `None` without enough history, or
    /// when the climb has already come back down (`trend::climb`).
    pub fn app_memory_climb(&self, name: &str) -> Option<u64> {
        self.mem_trend
            .climb(name)
            .filter(|mb| *mb > 0.0)
            .map(|mb| mb as u64 * MIB)
    }

    /// Every tree climbing at all this hour, biggest climb first, with
    /// what it holds now. The Overview strip applies its own floor;
    /// this is the raw answer.
    pub fn memory_climbers(&self) -> Vec<MemoryCreep> {
        let Some(groups) = self
            .latest
            .as_ref()
            .and_then(|t| t.snapshot.process_groups.as_deref())
        else {
            return Vec::new();
        };
        let mut climbers: Vec<MemoryCreep> = groups
            .iter()
            .filter_map(|g| {
                let name = trend::tree_key(g);
                let climb_bytes = self.app_memory_climb(name)?;
                Some(MemoryCreep {
                    name: name.to_string(),
                    root_pid: g.root_pid,
                    climb_bytes,
                    now_bytes: g.phys_footprint_bytes.unwrap_or(g.memory_bytes),
                })
            })
            .collect();
        climbers.sort_by_key(|c| Reverse(c.climb_bytes));
        climbers
    }

    /// Creeps that have crossed [`trend::creep_notify_bytes`] since they
    /// were last announced — one banner per climb, where "per climb"
    /// is kept by the clock, not by the figure: an announcement stands
    /// for [`trend::CREEP_REARM`] however the number moves underneath
    /// it. Re-arming the moment the climb fell under the bar was the
    /// first shape, and a GC sawtooth turned it into three Chrome
    /// banners in 29 minutes — every re-crossing of the bar read as a
    /// fresh leak (the constant's doc has the full story). Once the
    /// hour expires, a tree still climbing past the bar is measured
    /// against a baseline newer than the last banner: news again,
    /// once an hour, which was the intent all along.
    pub fn take_memory_creep_notices(&mut self) -> Vec<MemoryCreep> {
        self.creep_notified
            .retain(|_, named_at| named_at.elapsed() < trend::CREEP_REARM);
        let bar = trend::creep_notify_bytes(
            self.latest()
                .map(|t| t.snapshot.memory.total_bytes)
                .unwrap_or(0),
        );
        self.memory_climbers()
            .into_iter()
            .filter(|c| c.climb_bytes >= bar)
            .filter(|c| match self.creep_notified.entry(c.name.clone()) {
                Entry::Occupied(_) => false,
                Entry::Vacant(slot) => {
                    slot.insert(Instant::now());
                    true
                }
            })
            .collect()
    }

    /// The climbs whose banner is out — announced within the hour and
    /// still climbing — with how long ago each was first named. The
    /// Alerts tab's read-only card reads this: the card is the landing
    /// spot for the creep banner, so its rows mirror the standing
    /// announcements with live figures (a dip below the bar does not
    /// drop a row — the reader clicking a 20-minute-old banner must
    /// still land on its subject; only a climb that ended, or the
    /// hour turning over, retires one). Unannounced climbers stay on
    /// Overview's strip. Biggest climb first, from `memory_climbers`'
    /// own order.
    pub fn creeps_active(&self) -> Vec<(MemoryCreep, Duration)> {
        self.memory_climbers()
            .into_iter()
            .filter_map(|c| {
                let named_at = self.creep_notified.get(&c.name)?;
                Some((c, named_at.elapsed()))
            })
            .collect()
    }

    pub fn proc_sort(&self) -> ProcSort {
        self.proc_sort
    }

    pub fn cycle_proc_sort(&mut self, cx: &mut Context<Self>) {
        self.proc_sort = self.proc_sort.next();
        cx.notify();
    }

    pub fn app_sort(&self) -> AppSort {
        self.app_sort
    }

    pub fn cycle_app_sort(&mut self, cx: &mut Context<Self>) {
        self.app_sort = self.app_sort.next();
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

    /// Drop one per-subject `[alerts]` override, so that subject falls
    /// back to the base rule (or the template's line for it). Same
    /// `<key> <name>` shape as the CLI's `-remove`, and the same
    /// reload afterwards as writing one.
    ///
    /// The counterpart to [`apply_alert_override`](Self::apply_alert_override),
    /// and until it existed an override could be written from the panel
    /// but never taken back: the only way out was hand-editing
    /// config.toml, which is exactly the file this app exists to keep
    /// people out of.
    pub fn remove_alert_override(
        &mut self,
        key: &str,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let file = remove_setting(&zstats::settings::default_dir(), key, name)?;
        self.settings = Some(file);
        // Overrides live in `[alerts]`, the one section that reloads in
        // place — no collector rebuild, so no rate baselines are lost.
        metrics::request_reload();
        if let Some(pace) = cx.try_global::<metrics::CollectorPace>() {
            pace.wake();
        }
        cx.notify();
        Ok(())
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

    pub fn panel_window(&self) -> Option<gpui::AnyWindowHandle> {
        self.panel_window
    }

    pub fn set_panel_window(&mut self, handle: gpui::AnyWindowHandle) {
        self.panel_window = Some(handle);
    }

    pub fn settings_window(&self) -> Option<gpui::AnyWindowHandle> {
        self.settings_window
    }

    /// Interface-page prefs are not fields here. Bumping this is what
    /// makes the settings window's observer treat a chip click as news
    /// without also following every collector tick.
    pub fn bump_ui(&mut self) {
        self.ui_epoch = self.ui_epoch.wrapping_add(1);
    }

    /// What the settings window actually paints from this store. A
    /// collector tick changes `latest` and must not match; an update
    /// download, a template fetch, a config.toml write, or a pref chip
    /// must. Compared by the window's observer so it can skip `notify`.
    pub fn settings_paint_token(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.ui_epoch.hash(&mut h);
        self.settings
            .as_ref()
            .map(|s| s as *const FileConfig as usize)
            .unwrap_or(0)
            .hash(&mut h);
        self.update_nudge.hash(&mut h);
        self.update_ignored.hash(&mut h);
        self.template_nudge.hash(&mut h);
        hash_update_status(&self.update_status, &mut h);
        hash_template_sync(&self.template_sync, &mut h);
        match &self.hints_sync {
            None => 0u8.hash(&mut h),
            Some(HintsSync::Running) => 1u8.hash(&mut h),
            Some(HintsSync::Done(_)) => 2u8.hash(&mut h),
        }
        match &self.caches_sync {
            None => 0u8.hash(&mut h),
            Some(CachesSync::Running) => 1u8.hash(&mut h),
            Some(CachesSync::Done(_)) => 2u8.hash(&mut h),
        }
        h.finish()
    }

    pub fn set_settings_window(&mut self, handle: gpui::AnyWindowHandle) {
        self.settings_window = Some(handle);
    }

    pub fn storage_window(&self) -> Option<gpui::AnyWindowHandle> {
        self.storage_window
    }

    pub fn set_storage_window(&mut self, handle: gpui::AnyWindowHandle) {
        self.storage_window = Some(handle);
    }

    /// This tab's scroll offset, held across frames so switching away and
    /// back returns to where the list was left.
    pub fn scroll_handle(&self, tab: Tab) -> &ScrollHandle {
        &self.scroll[tab.index()]
    }

    /// Back to a clean slate for the next open. The name filter and the
    /// one-shot full listings are "looking at something right now" state:
    /// a panel reopened hours later with yesterday's query looks broken,
    /// not remembered. Scroll positions and the selected tab survive —
    /// those are orientation, not a question being asked.
    ///
    /// The large-file listing is deliberately **not** cleared here any
    /// more: it renders in the disk-space window, and opening that window
    /// takes focus off the panel — which is exactly what calls this. The
    /// same "not remembered" rule now runs on that window's own
    /// lifecycle ([`Self::reset_storage_views`]).
    pub fn reset_transient_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.proc_filter_open {
            // The close arm clears the input, the lowercased mirror and
            // the full-scan cuts in one place.
            self.toggle_proc_filter(window, cx);
        }
        self.full_scan = FullScan::Off;
        self.full_app_scan = FullAppScan::Off;
        self.member_table = MemberTable::Off;
        // The question goes with the photograph. `ensure_apps_topology`
        // keeps the member table fresh for a selected tree on every
        // tick, ahead of its visibility gate — an expansion left
        // selected across a hide kept the resident loop refetching the
        // full table (footprints and all) every 15s with no panel on
        // screen, which is how tray-resident CPU more than doubled.
        // Collapse still keeps the selection; hide is the reset.
        self.selected_app = None;
        cx.notify();
    }

    pub fn proc_rows_scroll(&self) -> &ScrollHandle {
        &self.proc_rows_scroll
    }

    pub fn history_rows_scroll(&self) -> &ScrollHandle {
        &self.history_rows_scroll
    }

    pub fn app_rows_scroll(&self) -> &ScrollHandle {
        &self.app_rows_scroll
    }

    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            prefs::set_last_tab_key(tab.pref_key());
            // A hidden switch (a banner click about to reveal Alerts)
            // leaves the entry work to `enter_shown_tab`, which the
            // reveal runs — doing it here too would read twice.
            if panel_visible(cx) {
                self.enter_tab(cx);
            }
            if tab == Tab::Alerts {
                self.see_alerts();
                tray::sync(cx, self);
            }
            cx.notify();
        }
    }

    /// Launch: put the last tab on and nothing else. Not a look at
    /// Alerts (the spec waits until the panel is actually shown), no
    /// `app.toml` rewrite, and none of the tab's entry work — a login
    /// launch may never open the panel, and the History rows read now
    /// would be what the first open showed hours later.
    /// [`Self::enter_shown_tab`] does that work when the panel appears.
    pub fn restore_session_tab(&mut self) {
        self.tab = Tab::from_pref_key(prefs::last_tab_key().as_deref());
    }

    /// The panel just came on screen (built, or revealed from the tray):
    /// the tab it shows is being visited, so it gets the same entry work
    /// a click on it would.
    pub fn enter_shown_tab(&mut self, cx: &mut Context<Self>) {
        self.enter_tab(cx);
    }

    fn enter_tab(&mut self, cx: &mut Context<Self>) {
        let tab = self.tab;
        if tab == Tab::Hardware {
            self.ensure_space_info(cx);
        }
        // Opening History is what pays for reading it. Re-read on every
        // visit rather than caching: the file grows a line a minute, and
        // a stale "today" is worse than a moment's wait. A reveal counts
        // as a visit, so rows younger than that minute are kept — tray
        // toggles would otherwise re-read a 30-day window each time.
        if tab == Tab::History
            && !self
                .history_loaded_at
                .is_some_and(|at| at.elapsed() < HISTORY_FRESH)
        {
            self.load_history(cx);
        }
        // The past week's files are small and change only at
        // midnight; re-reading them on the way into the tab is
        // what keeps a day-old photograph from being the record.
        if tab == Tab::Alerts {
            self.refresh_alert_history();
        }
        // Apps / Overview titles need the full ppid chain and the
        // process groups for a job face. Kick it here so the first
        // paint after the switch is not waiting on the next tick.
        if matches!(tab, Tab::Apps | Tab::Overview) {
            self.ensure_apps_topology(cx);
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

    /// The tag of a newer release a check has seen, if any — what the
    /// settings gear's dot means.
    pub fn update_nudge(&self) -> Option<&str> {
        self.update_nudge.as_deref()
    }

    pub fn template_nudge(&self) -> bool {
        self.template_nudge
    }

    /// Wave the offered table away: dot out for exactly this content,
    /// probes keep running, the card's button keeps telling the truth.
    pub fn ignore_template_offer(&mut self, cx: &mut Context<Self>) {
        alerttpl::ignore_offer();
        self.template_nudge = alerttpl::nudge();
        cx.notify();
    }

    /// A silent finding but no check this session: run one, so the
    /// About row carries the release notes the silent check does not
    /// retain. Solicited — the user just opened the update surface.
    pub fn refresh_update_for_about(&mut self, cx: &mut Context<Self>) {
        if self.update_status.is_none() && self.update_nudge.is_some() {
            self.check_update(cx);
        }
    }

    /// The version the user skipped, while it still applies.
    pub fn update_ignored(&self) -> Option<&str> {
        self.update_ignored.as_deref()
    }

    /// Take the skip back: the dot returns and the About row goes back
    /// to offering the download.
    pub fn unignore_update(&mut self, cx: &mut Context<Self>) {
        updater::unignore();
        self.update_ignored = None;
        self.update_nudge = updater::nudge();
        cx.notify();
    }

    /// "Skip this version": mute the gear's dot for `version` alone.
    /// Checks keep running, the About page keeps answering truthfully,
    /// and the next release re-arms the dot by itself.
    pub fn ignore_update(&mut self, version: &str, cx: &mut Context<Self>) {
        updater::ignore(version);
        self.update_nudge = updater::nudge();
        self.update_ignored = updater::ignored();
        // Clear the finding this session is showing, so the row falls
        // through to the "skipped" state below it. Without this the row
        // keeps rendering from `update_status`, which the skip does not
        // touch — the button would look inert while the only thing it
        // changed (the gear's dot) sits in another window. Checking
        // again still tells the truth: skipping silences the reminder,
        // never the answer.
        self.update_status = None;
        cx.notify();
    }

    /// The silent update check, riding the tick like the space probe.
    /// Three throttles deep: in-flight flag, an hourly probe of the
    /// check file, and the file's own days-scale cadence — so the
    /// steady state is one tiny file read per hour and one network
    /// round-trip per `AUTO_CHECK_EVERY`.
    fn maybe_auto_check_update(&mut self, cx: &mut Context<Self>) {
        const PROBE_EVERY: Duration = Duration::from_secs(3600);
        if self.auto_check_inflight
            || self
                .auto_check_probe_at
                .is_some_and(|at| at.elapsed() < PROBE_EVERY)
        {
            return;
        }
        self.auto_check_probe_at = Some(Instant::now());
        if !updater::auto_check_due(SystemTime::now()) {
            return;
        }
        self.auto_check_inflight = true;
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async {
                    let outcome = updater::check();
                    // The template probe rides the same two-day clock —
                    // one rhythm of unprompted network for the whole
                    // app, not one per feature. Compare-only: applying
                    // stays behind the card's button (`alerttpl`).
                    alerttpl::silent_check();
                    outcome
                })
                .await;
            let _ = this.update(cx, |state, cx| {
                state.auto_check_inflight = false;
                updater::record_outcome(SystemTime::now(), &outcome);
                state.update_nudge = updater::nudge();
                state.template_nudge = alerttpl::nudge();
                cx.notify();
            });
        })
        .detach();
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
                // A manual check answers the same question: stamp the
                // silent clock and refresh the gear's dot from it.
                updater::record_outcome(SystemTime::now(), &outcome);
                state.update_nudge = updater::nudge();
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
        let notes = match &self.update_status {
            Some(UpdateStatus::Done(updater::UpdateCheck::Newer { notes, .. }))
            | Some(UpdateStatus::DownloadFailed { notes, .. }) => notes.clone(),
            _ => String::new(),
        };
        self.update_status = Some(UpdateStatus::Downloading {
            received: 0,
            total: 0,
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
            let downloaded = cx
                .background_executor()
                .spawn(async move {
                    // Throttled to whole-MB steps: every 64 KB chunk
                    // would repaint the settings window for nothing.
                    let mut last_mb = u64::MAX;
                    updater::download(&tag, move |received, total| {
                        let mb = received / (1024 * 1024);
                        if mb != last_mb || received == total {
                            last_mb = mb;
                            let _ = tx.try_send((received, total));
                        }
                    })
                })
                .await;
            let path = match downloaded {
                Ok(path) => path,
                Err(error) => {
                    let _ = this.update(cx, |state, cx| {
                        let notes = state.update_notes_in_flight();
                        state.update_status = Some(UpdateStatus::DownloadFailed {
                            version,
                            error,
                            notes,
                        });
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |state, cx| {
                let notes = state.update_notes_in_flight();
                state.update_status = Some(UpdateStatus::Installing { notes });
                cx.notify();
            });
            let delivered = cx
                .background_executor()
                .spawn(async move { updater::install(&path) })
                .await;
            let _ = this.update(cx, |state, cx| {
                let notes = state.update_notes_in_flight();
                state.update_status = Some(match delivered {
                    Ok(updater::Delivery::Replaced) => UpdateStatus::Installed { manual: false },
                    #[cfg(target_os = "macos")]
                    Ok(updater::Delivery::OpenedForDrag) => {
                        UpdateStatus::Installed { manual: true }
                    }
                    Err(error) => UpdateStatus::DownloadFailed {
                        version,
                        error,
                        notes,
                    },
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// The release notes riding the in-flight update status, so a
    /// failure can keep showing them beside the retry button.
    fn update_notes_in_flight(&self) -> String {
        match &self.update_status {
            Some(UpdateStatus::Downloading { notes, .. })
            | Some(UpdateStatus::Installing { notes }) => notes.clone(),
            _ => String::new(),
        }
    }

    pub fn hints_sync(&self) -> Option<&HintsSync> {
        self.hints_sync.as_ref()
    }

    pub fn caches_sync(&self) -> Option<&CachesSync> {
        self.caches_sync.as_ref()
    }

    /// Fetch the published Caches roots on the background executor.
    /// One at a time, same as the clean hints.
    pub fn update_cachepreset(&mut self, cx: &mut Context<Self>) {
        if matches!(self.caches_sync, Some(CachesSync::Running)) {
            return;
        }
        self.caches_sync = Some(CachesSync::Running);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async { cachepreset::update_from_remote() })
                .await;
            let _ = this.update(cx, |state, cx| {
                state.caches_sync = Some(CachesSync::Done(outcome));
                cx.notify();
            });
        })
        .detach();
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

    pub fn template_sync(&self) -> Option<&TemplateSync> {
        self.template_sync.as_ref()
    }

    /// Fetch the published alert table on the background executor. One
    /// at a time, same as the clean hints — a second press while one
    /// runs is a no-op, not a queue.
    ///
    /// The collector reload is [`alerttpl`]'s own doing, next to the
    /// write: whether zstats has to re-read its thresholds is a fact
    /// about the file having changed, not about a view having asked.
    pub fn update_alert_template(&mut self, cx: &mut Context<Self>) {
        if matches!(self.template_sync, Some(TemplateSync::Running)) {
            return;
        }
        self.template_sync = Some(TemplateSync::Running);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async { alerttpl::update_from_remote() })
                .await;
            let _ = this.update(cx, |state, cx| {
                state.template_sync = Some(TemplateSync::Done(outcome));
                // Applying (or finding local already current) withdrew
                // the standing offer; the dot follows the file.
                state.template_nudge = alerttpl::nudge();
                cx.notify();
            });
        })
        .detach();
    }

    /// Drop the override and go back to the table compiled into zstats.
    /// Local file work only — no executor hop, unlike the fetch above.
    pub fn use_builtin_alert_template(&mut self, cx: &mut Context<Self>) {
        self.template_sync = Some(match alerttpl::use_builtin() {
            Ok(true) => TemplateSync::Reverted,
            Ok(false) => TemplateSync::NothingToRevert,
            Err(e) => TemplateSync::RevertFailed(e),
        });
        self.template_nudge = alerttpl::nudge();
        cx.notify();
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
        self.history_loaded_at = None;
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
                            tracing::error!("could not read history: {e}");
                            Vec::new()
                        })
                })
                .await;
            let _ = this.update(cx, |state, cx| {
                if state.history_range == range {
                    state.history = Some(rows);
                    state.history_loaded_at = Some(Instant::now());
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
                        tracing::error!("full application scan failed: {e}");
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
                        tracing::error!("full process scan failed: {e}");
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
        if self.selected_app == Some(root_pid) {
            self.selected_app = None;
            // Keep the photograph: collapsing must not rename a
            // job-faced row back to `login`. Hide still drops it.
        } else {
            self.selected_app = Some(root_pid);
            if matches!(self.member_table, MemberTable::Failed { .. }) {
                self.member_table = MemberTable::Off;
            }
            let expected = self.group_process_count(root_pid).unwrap_or(1);
            self.ensure_member_table(root_pid, expected, cx);
        }
        cx.notify();
    }

    /// Jump from a row elsewhere (Overview's top card) to the Apps tab
    /// with this tree selected and its expansion loading — the same
    /// state a click on the Apps row itself produces, minus the toggle:
    /// landing on an already-open tree must not fold it.
    pub fn reveal_app(&mut self, root_pid: u32, cx: &mut Context<Self>) {
        self.set_tab(Tab::Apps, cx);
        if self.selected_app != Some(root_pid) {
            self.selected_app = Some(root_pid);
            if matches!(self.member_table, MemberTable::Failed { .. }) {
                self.member_table = MemberTable::Off;
            }
            let expected = self.group_process_count(root_pid).unwrap_or(1);
            self.ensure_member_table(root_pid, expected, cx);
        }
        self.app_reveal.set(true);
        cx.notify();
    }

    /// True exactly once per [`Self::reveal_app`]: the Apps list scrolls
    /// the selected row into view on that paint and never steers again.
    pub fn take_app_reveal(&self) -> bool {
        self.app_reveal.take()
    }

    /// Jump from an alert card or History row to the Processes tab with
    /// this pid selected and expanded — same non-toggle as [`Self::reveal_app`].
    /// The abnormal-only filter would hide a normal target, so it comes off;
    /// a name filter that already hides the row is left alone (nowhere to
    /// scroll is the honest outcome, same as Apps).
    pub fn reveal_pid(&mut self, pid: u32, cx: &mut Context<Self>) {
        self.set_tab(Tab::Processes, cx);
        self.only_abnormal = false;
        if self.selected_pid != Some(pid) {
            self.selected_pid = Some(pid);
        }
        self.proc_reveal.set(true);
        cx.notify();
    }

    /// True exactly once per [`Self::reveal_pid`].
    pub fn take_proc_reveal(&self) -> bool {
        self.proc_reveal.take()
    }

    /// History names a process by `(pid, name)` from a file that outlives
    /// the process. Prefer a live tree with that matchable `name` (survives
    /// a restart); else a live process still holding that pid-and-name, or
    /// the same name under a new pid; else just open Processes. A pid-only
    /// hit is rejected: macOS recycles low pids.
    pub fn reveal_history_subject(&mut self, pid: u32, name: &str, cx: &mut Context<Self>) {
        let groups = self
            .latest
            .as_ref()
            .and_then(|t| t.snapshot.process_groups.as_deref());
        if let Some(groups) = groups
            && let Some(root) = live_group_root(groups, name)
        {
            self.reveal_app(root, cx);
            return;
        }
        let processes = self
            .latest
            .as_ref()
            .and_then(|t| t.snapshot.processes.as_deref());
        if let Some(processes) = processes
            && let Some(live) = live_process_pid(processes, name, pid)
        {
            self.reveal_pid(live, cx);
            return;
        }
        self.set_tab(Tab::Processes, cx);
    }

    /// The uncapped process table, once Apps/Overview needed a job face
    /// or an expansion asked for members.
    pub fn member_processes(&self) -> Option<&[ProcessSnapshot]> {
        match &self.member_table {
            MemberTable::Ready { processes, .. } => Some(processes.as_slice()),
            _ => None,
        }
    }

    /// The process groups from the same photograph — empty until it
    /// lands, which `trend::tree_face` reads as "keep the tree's own
    /// name".
    pub fn member_pgids(&self) -> &HashMap<u32, u32> {
        static NONE: LazyLock<HashMap<u32, u32>> = LazyLock::new(HashMap::new);
        match &self.member_table {
            MemberTable::Ready { pgids, .. } => pgids,
            _ => &NONE,
        }
    }

    pub fn member_table_running(&self) -> bool {
        matches!(self.member_table, MemberTable::Running)
    }

    fn group_process_count(&self, root: u32) -> Option<u32> {
        if let FullAppScan::Ready(data) = &self.full_app_scan
            && let Some(g) = data.groups.iter().find(|g| g.root_pid == root)
        {
            return Some(g.process_count);
        }
        self.latest
            .as_ref()?
            .snapshot
            .process_groups
            .as_deref()?
            .iter()
            .find(|g| g.root_pid == root)
            .map(|g| g.process_count)
    }

    /// Fetch the full table when the live top-N cannot name the tree
    /// (members *or* a job face), and again when a held photograph is
    /// older than the process cadence. A 2-process Finder is already
    /// complete; Chrome's helpers and a `login` compile whose `zsh` was
    /// ranked out are why the first fetch exists.
    fn ensure_apps_topology(&mut self, cx: &mut Context<Self>) {
        if let Some(pid) = self.selected_app
            && let Some(n) = self.group_process_count(pid)
        {
            self.ensure_member_table(pid, n, cx);
        }
        if !panel_visible(cx) || !matches!(self.tab, Tab::Apps | Tab::Overview) {
            return;
        }
        if let Some((root, n)) = self.tree_needing_topology() {
            self.ensure_member_table(root, n, cx);
        }
    }

    /// A tree with company and CPU: its face may be the job holding
    /// that CPU — the title of a bare tree, the tail of an application's
    /// (`Zed · cargo`) — and the job boundaries come only with the
    /// photograph: the tick carries no process groups, and usually not
    /// the idle shell either. Not gated on bundle or on "members missing
    /// from the tick": a tree fully present in the tick still has no
    /// pgids there. One fetch serves every tree, so the broader gate
    /// costs nothing extra.
    fn tree_needing_topology(&self) -> Option<(u32, u32)> {
        let tick = self.latest.as_ref()?;
        let groups = tick.snapshot.process_groups.as_deref()?;
        groups.iter().find_map(|g| {
            (g.cpu_usage_percent > 0.0 && g.process_count > 1)
                .then_some((g.root_pid, g.process_count))
        })
    }

    fn ensure_member_table(&mut self, root: u32, expected: u32, cx: &mut Context<Self>) {
        match member_table_next(
            &self.member_table,
            Instant::now(),
            metrics::PANEL_PROCESS_INTERVAL,
        ) {
            MemberTableNext::Hold => return,
            MemberTableNext::Restart => {
                self.start_member_table(cx);
                return;
            }
            MemberTableNext::FallThrough => {}
        }
        if expected <= 1 {
            return;
        }
        let processes = self
            .latest
            .as_ref()
            .and_then(|t| t.snapshot.processes.as_deref().map(Vec::as_slice))
            .unwrap_or(&[]);
        if fullscan::tree_members(root, processes).len() as u32 >= expected {
            return;
        }
        self.start_member_table(cx);
    }

    fn start_member_table(&mut self, cx: &mut Context<Self>) {
        match &mut self.member_table {
            MemberTable::Running => return,
            MemberTable::Ready {
                refreshing: true, ..
            } => return,
            MemberTable::Ready { refreshing, .. } => *refreshing = true,
            _ => {
                self.member_table = MemberTable::Running;
                cx.notify();
            }
        }
        cx.spawn(async move |this, cx| {
            // The process groups come from the same background pass, so
            // the face and the member rows describe one moment: a job
            // read a tick later could name a pid the table no longer
            // has, or miss the one it just gained.
            let listed = cx
                .background_executor()
                .spawn(async {
                    let processes = fullscan::list_processes()?;
                    Ok::<_, zstats::CollectError>((processes, Arc::new(procscan::process_groups())))
                })
                .await;
            let _ = this.update(cx, |state, cx| {
                if matches!(state.member_table, MemberTable::Off) {
                    return;
                }
                state.member_table = match listed {
                    Ok((processes, pgids)) => MemberTable::Ready {
                        processes,
                        pgids,
                        at: Instant::now(),
                        refreshing: false,
                    },
                    Err(e) => {
                        tracing::warn!("app member listing failed: {e}");
                        match &state.member_table {
                            MemberTable::Ready {
                                processes,
                                pgids,
                                at,
                                ..
                            } => MemberTable::Ready {
                                processes: Arc::clone(processes),
                                pgids: Arc::clone(pgids),
                                at: *at,
                                refreshing: false,
                            },
                            _ => MemberTable::Failed { at: Instant::now() },
                        }
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    // ---- window --------------------------------------------------------

    /// Where the main window was last seen. Used when reopening without a
    /// tray anchor (the menu's "Show Window"), so it doesn't jump to centre.
    pub fn window_bounds(&self) -> Option<Bounds<Pixels>> {
        self.window_bounds
    }

    /// Last known display scale factor, mirrored from the main window. Only
    /// a fallback for placement when AppKit has no screen list (off the
    /// main thread, or not macOS). On macOS the tray's scale is resolved
    /// per display — see `placement::resolve_icon`.
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

fn hash_update_status(status: &Option<UpdateStatus>, h: &mut impl Hasher) {
    match status {
        None => 0u8.hash(h),
        Some(UpdateStatus::Checking) => 1u8.hash(h),
        Some(UpdateStatus::Done(check)) => {
            2u8.hash(h);
            match check {
                updater::UpdateCheck::UpToDate => 0u8.hash(h),
                updater::UpdateCheck::Newer { version, .. } => {
                    1u8.hash(h);
                    version.hash(h);
                }
                updater::UpdateCheck::Failed(e) => {
                    2u8.hash(h);
                    e.hash(h);
                }
            }
        }
        Some(UpdateStatus::Downloading {
            received, total, ..
        }) => {
            3u8.hash(h);
            received.hash(h);
            total.hash(h);
        }
        Some(UpdateStatus::Installing { .. }) => 4u8.hash(h),
        Some(UpdateStatus::Installed { manual }) => {
            5u8.hash(h);
            manual.hash(h);
        }
        Some(UpdateStatus::DownloadFailed { version, error, .. }) => {
            6u8.hash(h);
            version.hash(h);
            error.hash(h);
        }
    }
}

fn hash_template_sync(sync: &Option<TemplateSync>, h: &mut impl Hasher) {
    match sync {
        None => 0u8.hash(h),
        Some(TemplateSync::Running) => 1u8.hash(h),
        Some(TemplateSync::Done(_)) => 2u8.hash(h),
        Some(TemplateSync::Reverted) => 3u8.hash(h),
        Some(TemplateSync::NothingToRevert) => 4u8.hash(h),
        Some(TemplateSync::RevertFailed(e)) => {
            5u8.hash(h);
            e.hash(h);
        }
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

/// Drop one `<key> <name>` override from `<dir>/config.toml` and return
/// the saved file. Mirrors [`persist_setting`] through the CLI's own
/// `apply_remove`, so the panel and `zstats -remove` can never disagree
/// about what removal means.
pub(crate) fn remove_setting(dir: &Path, key: &str, name: &str) -> Result<FileConfig, String> {
    let mut file = zstats::settings::load(dir).map_err(|e| e.to_string())?;
    zstats::settings::apply_remove(&mut file, key, Some(name))?;
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
/// origin — that's what `tray_icon` reports. It multiplied the AppKit
/// logical frame by *that status item's* `backingScaleFactor`, so converting
/// back has to pick the matching screen (see `placement::resolve_icon`),
/// not the window's last-known [`ZStatsAppState::scale_factor`].
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
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::process;

    fn snap(pid: u32, name: &str) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            name: name.into(),
            display_name: None,
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

    fn ready_table(at: Instant, refreshing: bool) -> MemberTable {
        MemberTable::Ready {
            processes: std::sync::Arc::new(vec![]),
            pgids: std::sync::Arc::new(std::collections::HashMap::new()),
            at,
            refreshing,
        }
    }

    #[test]
    fn a_failed_member_table_retries_after_the_process_cadence() {
        let interval = metrics::PANEL_PROCESS_INTERVAL;
        let t0 = Instant::now();
        assert_eq!(
            member_table_next(&MemberTable::Failed { at: t0 }, t0, interval),
            MemberTableNext::Hold,
            "just failed: do not hammer the full table"
        );
        assert_eq!(
            member_table_next(
                &MemberTable::Failed { at: t0 },
                t0 + interval + Duration::from_secs(1),
                interval,
            ),
            MemberTableNext::Restart,
        );
        assert_eq!(
            member_table_next(&MemberTable::Running, t0, interval),
            MemberTableNext::Hold,
        );
        assert_eq!(
            member_table_next(&ready_table(t0, true), t0 + interval + interval, interval),
            MemberTableNext::Hold,
            "a refresh in flight is not a second fetch"
        );
        assert_eq!(
            member_table_next(&MemberTable::Off, t0, interval),
            MemberTableNext::FallThrough,
        );
        assert_eq!(
            member_table_next(
                &ready_table(t0, false),
                t0 + interval + Duration::from_secs(1),
                interval,
            ),
            MemberTableNext::Restart,
        );
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

    /// The hide exists because zstats serves the disk list from cache
    /// between refreshes; it must end the moment the snapshot agrees,
    /// and it must not outlive a drive that came back.
    #[test]
    fn an_ejected_volume_is_hidden_until_the_snapshot_agrees() {
        let mut state = ZStatsAppState::new();
        let t0 = Instant::now();
        state.ejected.insert("/Volumes/USB".into(), t0);
        assert!(state.is_ejected("/Volumes/USB"));
        assert!(!state.is_ejected("/"), "only the volume that was ejected");

        // Still listed a moment later: the cache has not turned over yet,
        // so the card stays hidden.
        state.prune_ejected(
            &["/".into(), "/Volumes/USB".into()],
            t0 + Duration::from_secs(5),
        );
        assert!(state.is_ejected("/Volumes/USB"));

        // The snapshot drops it — the hide has done its job and goes.
        state.prune_ejected(&["/".into()], t0 + Duration::from_secs(6));
        assert!(!state.is_ejected("/Volumes/USB"));
    }

    /// A drive replugged onto the same path never disappears from the
    /// snapshot, so "hide until it is gone" alone would hide it forever.
    #[test]
    fn a_volume_that_never_leaves_stops_being_hidden() {
        let mut state = ZStatsAppState::new();
        let t0 = Instant::now();
        state.ejected.insert("/Volumes/USB".into(), t0);

        let listed = ["/".to_string(), "/Volumes/USB".to_string()];
        state.prune_ejected(&listed, t0 + EJECT_HIDE_MAX - Duration::from_secs(1));
        assert!(state.is_ejected("/Volumes/USB"), "still within the cap");

        state.prune_ejected(&listed, t0 + EJECT_HIDE_MAX);
        assert!(!state.is_ejected("/Volumes/USB"), "the cap releases it");
    }

    /// The creep re-arm is the clock, not the figure. With nothing
    /// over the bar this tick, the first shape read "climb gone" and
    /// re-armed — a GC sawtooth crossing 1 GB every few minutes became
    /// three Chrome banners in 29 minutes. A standing announcement now
    /// survives any dip; only [`trend::CREEP_REARM`] expiring prunes it.
    #[test]
    fn a_dip_under_the_bar_does_not_rearm_the_creep_banner() {
        let mut state = ZStatsAppState::new();
        state
            .creep_notified
            .insert("Google Chrome".into(), Instant::now());
        // No tick at all — as far as this pass can see, nothing is
        // over the bar, which is exactly what a low tooth looks like.
        assert!(state.take_memory_creep_notices().is_empty());
        assert!(
            state.creep_notified.contains_key("Google Chrome"),
            "the hour re-arms, a dip must not"
        );
        // The clock half: an announcement older than the ring goes.
        // Guarded because `Instant` cannot reach past boot — on a
        // machine (or CI runner) up less than the hour, only the
        // dip half above is checkable.
        if let Some(stale) = Instant::now().checked_sub(trend::CREEP_REARM + Duration::from_secs(1))
        {
            state.creep_notified.insert("old".into(), stale);
            let _ = state.take_memory_creep_notices();
            assert!(!state.creep_notified.contains_key("old"));
            assert!(state.creep_notified.contains_key("Google Chrome"));
        }
    }

    #[test]
    fn sustained_bar_follows_the_configured_alert_threshold() {
        let state = ZStatsAppState::new();
        // No config loaded yet: zstats' own default of 30%, thirded.
        assert!((state.sustained_bar() - 10.0).abs() < f64::EPSILON);
    }

    /// The settings window observes this token instead of every store
    /// notify. A collector tick (tab, selection, filters) must not
    /// match; a pref chip, a download byte, or a config.toml write must.
    #[test]
    fn settings_paint_token_ignores_collector_ticks() {
        let mut state = ZStatsAppState::new();
        let a = state.settings_paint_token();
        state.tab = Tab::Alerts;
        state.selected_pid = Some(1);
        state.only_abnormal = true;
        assert_eq!(
            state.settings_paint_token(),
            a,
            "panel selection is not settings"
        );
        state.bump_ui();
        assert_ne!(
            state.settings_paint_token(),
            a,
            "a pref chip must repaint settings"
        );
        let b = state.settings_paint_token();
        state.update_status = Some(UpdateStatus::Downloading {
            received: 10,
            total: 100,
            notes: String::new(),
        });
        assert_ne!(
            state.settings_paint_token(),
            b,
            "download progress must move the About bar"
        );
        let c = state.settings_paint_token();
        state.update_status = Some(UpdateStatus::Downloading {
            received: 50,
            total: 100,
            notes: String::new(),
        });
        assert_ne!(state.settings_paint_token(), c);
        let d = state.settings_paint_token();
        state.settings = Some(FileConfig::default());
        assert_ne!(
            state.settings_paint_token(),
            d,
            "a config.toml write must repaint Config"
        );
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
        assert_eq!(seen.len(), 4, "every ordering should be reachable");

        let mut app = AppSort::default();
        app = app.next();
        assert_eq!(app, AppSort::Memory);
        app = app.next();
        assert_eq!(app, AppSort::Cpu, "apps cycle is two-way");
    }

    fn group(root_pid: u32, name: &str) -> ProcessGroupSnapshot {
        ProcessGroupSnapshot {
            root_pid,
            name: name.into(),
            display_name: None,
            process_count: 1,
            cpu_usage_percent: 0.0,
            memory_bytes: 0,
            phys_footprint_bytes: None,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    #[test]
    fn history_jumps_to_a_live_tree_by_name_not_a_recycled_pid() {
        let groups = [group(10, "Chrome"), group(20, "code")];
        assert_eq!(
            live_group_root(&groups, "Chrome"),
            Some(10),
            "a restart that kept the name still lands on the live tree"
        );
        assert_eq!(
            live_group_root(&groups, "helper"),
            None,
            "a helper name is not a tree"
        );
    }

    #[test]
    fn history_jumps_to_a_live_process_only_while_the_name_matches() {
        let procs = [snap(7, "Chrome"), snap(8, "code")];
        assert_eq!(live_process_pid(&procs, "Chrome", 7), Some(7));
        assert_eq!(
            live_process_pid(&procs, "Chrome", 99),
            Some(7),
            "restarted: same name, new pid"
        );
        assert_eq!(
            live_process_pid(&procs, "Chrome", 8),
            Some(7),
            "pid 8 is code now — do not follow the recycled pid"
        );
        assert_eq!(live_process_pid(&procs, "gone", 7), None);
    }

    #[test]
    fn tab_pref_keys_round_trip_and_overview_is_absent() {
        for tab in Tab::ALL {
            assert_eq!(Tab::from_pref_key(tab.pref_key()), tab);
        }
        assert!(Tab::Overview.pref_key().is_none());
        assert_eq!(
            Tab::from_pref_key(Some("config")),
            Tab::Overview,
            "Config is a window"
        );
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

    /// Removal is the half that did not exist: an override could be
    /// written from the panel and never taken back. It has to drop one
    /// line and only that line — the other subjects under the same
    /// rule, and the base value, are somebody else's setting.
    #[test]
    fn remove_setting_drops_one_override_and_leaves_the_rest() {
        let dir = scratch("remove");
        let _ = fs::remove_dir_all(&dir);
        persist_setting(&dir, "alert-cpu", "40").unwrap();
        persist_setting(&dir, "alert-cpu", "Google Chrome=45").unwrap();
        persist_setting(&dir, "alert-cpu", "node=70").unwrap();
        persist_setting(&dir, "alert-mem", "Xcode=25").unwrap();

        let file = remove_setting(&dir, "alert-cpu", "node").unwrap();
        assert!(!file.alerts.cpu_overrides.contains_key("node"));
        // A name with a space is the common case (an application), and
        // the one most likely to be mangled on the way through.
        assert_eq!(
            file.alerts.cpu_overrides.get("Google Chrome").copied(),
            Some(45.0)
        );
        assert_eq!(file.alerts.mem_overrides.get("Xcode").copied(), Some(25.0));
        assert_eq!(file.alerts.cpu, Some(40.0), "the base rule is untouched");

        // Written through, not just returned.
        let reloaded = zstats::settings::load(&dir).unwrap();
        assert!(!reloaded.alerts.cpu_overrides.contains_key("node"));
        assert_eq!(reloaded.alerts.cpu_overrides.len(), 1);

        // Removing what is not there is an error rather than a silent
        // success: the row that asked has just gone stale.
        assert!(remove_setting(&dir, "alert-cpu", "node").is_err());
        // And a key with no per-name overrides at all says so.
        assert!(remove_setting(&dir, "alert-pressure", "node").is_err());
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
