//! Application-level state.
//!
//! The window is only a rendering layer, and it spends most of its life off
//! screen: the panel is ordered out rather than destroyed (see `window_ext`),
//! and gpui discards element state for anything it did not paint this frame.
//! So everything that has to survive a hide → reveal round trip belongs here
//! rather than in the root view — window geometry, the selected tab, per-tab
//! scroll offsets. Collected metrics are the main tenant, and sampling runs
//! whether or not a window exists at all.

use crate::alertlog;
use crate::alerttpl;
use crate::bigfiles;
use crate::bigfiles::BigFilesScan;
use crate::cachepreset;
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
#[cfg(not(target_os = "linux"))]
use crate::tray;
use crate::trend::{self, AppTrend, MIB};
use crate::updater;
use crate::volflag;
pub use crate::watch::SustainedNotice;
use crate::watch::{AbnormalWatch, NetActivity, SustainedRule, SustainedWatch};
use gpui::{
    AppContext, Bounds, Context, Entity, Focusable, Global, ListAlignment, ListState, Pixels,
    ScrollHandle, Window, px,
};
use gpui_kit::component::input::{InputEvent, InputState};
use std::array;
use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::mem;
use std::ops::Deref;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zstats::settings::FileConfig;
use zstats::snapshot::SystemSnapshot;
use zstats::snapshot::{ProcessGroupSnapshot, ProcessSnapshot};
use zstats::{AlertDetail, AlertEvent, AlertKind, AlertSubject, Severity, Tick};

/// Used when config.toml sets no `alert-cpu` — zstats' own default is 30%.
/// The sustained bar is that line divided by `prefs::sustained_divisor`
/// (3 unless app.toml says): derived rather than fixed so tightening
/// `alert-cpu` tightens this too.
const SUSTAINED_FALLBACK_ALERT: f64 = 30.0;

/// How many past alerts the Alerts tab can show.
const MAX_ALERTS: usize = 20;

/// Days the Alerts tab's read-only record reaches back — a week, the
/// span "how often did this fire" is usually asked over. The files
/// keep a month (`alertlog::RETENTION_DAYS`); the tab shows the part
/// that fits a glance.
const ALERT_HISTORY_DAYS: u16 = 7;

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

/// How far back the auto-quiet rule looks, and how many banners it lets
/// through in that span before it stops interrupting.
///
/// Aimed at a condition that keeps crossing, clearing and crossing again:
/// zstats already spaces the reminders *inside* one episode (the pressure
/// rule backs off 30m/1h/2h/4h), but a flapping subject opens a fresh
/// episode each time and each one is news. Two banners is enough to have
/// said it; a third within the hour is the same sentence again.
///
/// Deliberately per episode, not global: a second, different subject
/// crossing its line is new information and must still arrive.
const NOISY_WINDOW: Duration = Duration::from_secs(3600);
const NOISY_AFTER: usize = 2;

/// How long a memory-class episode must look recovered before Auto
/// puts the tray back on CPU. Same five minutes zstats waits to end a
/// pressure episode (`PRESSURE_REARM` = `SLOW_WINDOW`): a one-sample
/// dip must not flip the icon, and the face turns back when the engine
/// would have cleared that episode, not five minutes after. Process
/// and app memory get the same hold so a leak that just went under
/// its own bar does not flicker the menu bar. The card stays.
const TRAY_RECOVER: Duration = Duration::from_secs(5 * 60);

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

/// A disk alert the user cannot act on is not news: a read-only extra
/// volume (an installer DMG under `/Volumes`) is full by construction.
/// Other kinds, and the boot disk, pass. `statfs` failing is fail-open
/// — see [`volflag`].
fn keep_alert(event: &AlertEvent) -> bool {
    let AlertSubject::Volume { mount_point } = &event.subject else {
        return true;
    };
    if event.kind() != AlertKind::Disk {
        return true;
    }
    if !volflag::skips_disk_alert(mount_point) {
        return true;
    }
    tracing::info!(
        kind = ?event.kind(),
        subject = ?event.subject,
        banner = "skipped",
        "disk alert skipped: volume is read-only"
    );
    false
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
    /// When this episode first crossed. Wall clock, not `Instant`:
    /// these outlive the process now ([`crate::alertlog`]), and a
    /// monotonic clock restarts with the machine.
    pub first_at: SystemTime,
    /// Most recent report within the episode.
    pub at: SystemTime,
    /// How many times zstats has reported it — 1 on the crossing, 2 once the
    /// 30-minute follow-up lands.
    pub reports: u32,
    /// Whether this episode has been reported *in this session*.
    ///
    /// The gate on every acting control the card carries. A card
    /// restored from yesterday names a pid, and after a reboot macOS
    /// hands low pids straight back out — so "quit Google Chrome ·
    /// 923" could deliver SIGTERM to whatever holds 923 now. Nothing in
    /// `terminate::can_quit` catches that: `kill(pid, 0)` answers "may
    /// I signal this pid", never "is this still that program". So the
    /// buttons appear only once a live report has confirmed the pid
    /// during this run — restored cards are records to read, and the
    /// Processes tab still offers a quit for anything actually running.
    pub live: bool,
    /// When this memory-class episode last started looking recovered,
    /// for the tray's Auto face. `None` while it still holds. The card
    /// stays on the Alerts tab; the menu bar goes back to CPU after
    /// [`TRAY_RECOVER`]. Display-layer only — the engine is not asked.
    recovered_since: Option<SystemTime>,
    pub event: AlertEvent,
}

impl SeenAlert {
    /// Time since the most recent report. A clock stepped backwards
    /// (NTP, a manual change) reads as "just now" rather than a
    /// negative age.
    pub fn age(&self) -> Duration {
        self.at.elapsed().unwrap_or_default()
    }

    /// First report to last report, once that differs from [`age`] by
    /// enough to be worth a second timestamp. Not "still happening":
    /// zstats goes quiet after the follow-up, so this span can end
    /// hours before the card is read.
    pub fn span(&self) -> Option<Duration> {
        let span = self.at.duration_since(self.first_at).unwrap_or_default();
        (span >= Duration::from_secs(60)).then_some(span)
    }

    fn recovered_for(&self, now: SystemTime) -> bool {
        self.recovered_since
            .is_some_and(|at| now.duration_since(at).unwrap_or_default() >= TRAY_RECOVER)
    }
}

fn is_memory_class(kind: AlertKind) -> bool {
    matches!(
        kind,
        AlertKind::Memory | AlertKind::AppMemory | AlertKind::Pressure
    )
}

/// Whether this episode is worth the menu bar changing face.
///
/// Every memory-class episode qualifies except **kernel pressure at
/// the warning tier**, and the exception is about what warning *means*
/// on this platform: a memory-heavy Mac sits at warning as its steady
/// state — zstats says so in the pressure rule's own comment, and
/// makes that tier wait five times as long before reporting for
/// exactly this reason. A face that spends half the day on memory has
/// stopped being a signal, so the tray waits for the kernel's
/// `critical` while the card and the banner still carry the warning.
///
/// The severity is `AlertEvent::severity()`, zstats' own field — the
/// panel is choosing *which verdict deserves the menu bar*, not
/// deciding when memory is a problem, and it reads no raw
/// `pressure_level` to do it. Process and application memory episodes
/// are Warning by construction in zstats (only pressure ≥ 4 and a
/// runaway CPU are Critical), so gating the whole class on severity
/// would have deleted the face's original job: naming the process or
/// tree that is eating the machine.
///
/// An episode that escalated from warning to critical turns the face
/// the moment the worsening is reported (`record_alert` keeps the
/// newest event), and keeps it through a fall back to warning — that
/// tail is still one unrecovered critical episode, and it ends the way
/// every other one does, on `TRAY_RECOVER` of the kernel calling the
/// machine normal again.
fn turns_the_face(event: &AlertEvent) -> bool {
    match event.kind() {
        AlertKind::Pressure => event.severity() == Severity::Critical,
        kind => is_memory_class(kind),
    }
}

/// Whether this memory-class event still holds in `snapshot`.
///
/// `None` if this sample cannot say (no pressure level, process
/// collection off). `false` if the subject is gone or its figure is
/// under the bar the event itself recorded — not a new threshold.
fn memory_event_holds(event: &AlertEvent, snapshot: &SystemSnapshot) -> Option<bool> {
    match &event.detail {
        AlertDetail::Pressure { .. } => {
            // zstats: `level <= 1` is normal. No level → cannot say.
            Some(snapshot.memory.pressure_level? > 1)
        }
        AlertDetail::Memory {
            threshold_bytes,
            threshold_percent,
            ..
        } => {
            let held = match &event.subject {
                AlertSubject::Process { pid, name, .. } => {
                    let processes = snapshot.processes.as_deref()?;
                    let Some(p) = processes.iter().find(|p| p.pid == *pid) else {
                        return Some(false);
                    };
                    if p.name != *name {
                        return Some(false);
                    }
                    p.phys_footprint_bytes.unwrap_or(p.memory_bytes)
                }
                AlertSubject::App { root_pid, name, .. } => {
                    let groups = snapshot.process_groups.as_deref()?;
                    let Some(g) = groups.iter().find(|g| g.root_pid == *root_pid) else {
                        return Some(false);
                    };
                    if g.name != *name {
                        return Some(false);
                    }
                    g.phys_footprint_bytes.unwrap_or(g.memory_bytes)
                }
                _ => return Some(false),
            };
            if *threshold_bytes > 0 {
                Some(held >= *threshold_bytes)
            } else if *threshold_percent > 0.0 && snapshot.memory.total_bytes > 0 {
                let share = held as f64 / snapshot.memory.total_bytes as f64 * 100.0;
                Some(share >= *threshold_percent)
            } else {
                None
            }
        }
        _ => Some(false),
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
    Failed,
}

pub struct ZStatsAppState {
    window_bounds: Option<Bounds<Pixels>>,
    scale_factor: f32,
    last_auto_hide: Option<Instant>,
    latest: Option<Tick>,
    alerts: VecDeque<SeenAlert>,
    /// Today's episodes the user has acknowledged with ✕. Out of the
    /// list and off the tab's tint, but written back into today's file
    /// with `dismissed = true`: the record of the day must say what
    /// fired, not what was left unread. Retired with the list at
    /// midnight; bounded like it.
    dismissed_today: Vec<alertlog::Restored>,
    /// The past week's files, read at launch, on entering the Alerts
    /// tab and when the day turns — never per frame. Read-only on
    /// screen; nothing in it can be acted on (the pids are history).
    alert_history: Vec<alertlog::DayLog>,
    /// Tray corner spec: a live `AlertEvent` has landed since the user
    /// last looked at the Alerts tab. Display of "you have not opened
    /// that list", not a second threshold — the engine already decided
    /// the condition. Restored (`live = false`) cards do not count;
    /// session-only, like the snooze beside it.
    tray_alert_unseen: bool,
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
    /// Rows the reader opened in the analysis tables, and what is under
    /// each. Session state that belongs to the result on screen: a new
    /// walk, a cleared result or a fresh window drops it.
    expanded: HashMap<PathBuf, Expansion>,
    /// Monotonic id for expansion sub-walks. One runs at a time; a
    /// superseded one's events land nowhere, same guard as the main walk.
    expand_runs: u64,
    expand_cancel: Option<Arc<AtomicBool>>,
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
    /// Whether the dirs table shows every retained row (up to
    /// `TABLE_KEEP`) or the display default. A per-visit choice: reset
    /// when the disk-space window is built fresh, not persisted.
    analysis_show_all_dirs: bool,
    /// The settings window, if one was ever opened. Kept so a second
    /// click focuses the existing window; a handle whose window the user
    /// closed fails its update and a fresh window is built instead.
    settings_window: Option<gpui::AnyWindowHandle>,
    /// The disk-space window (large files + the analyser), same
    /// reuse-or-rebuild contract as [`Self::settings_window`].
    storage_window: Option<gpui::AnyWindowHandle>,
    /// Monotonic id for analyser runs, so a stale run's channel events
    /// can never land into a newer run's state.
    disk_analysis_runs: u64,
    /// Volumes this session has successfully ejected, and when. They
    /// are hidden from the Hardware tab until the snapshot stops
    /// listing them — see [`Self::mark_ejected`].
    ejected: HashMap<String, Instant>,
    /// When each episode's banners were actually delivered, newest last.
    /// Drives the auto-quiet rule ([`NOISY_AFTER`]); trimmed to
    /// [`NOISY_WINDOW`] on every read, so it cannot grow. Session-only,
    /// like the snooze beside it — a restart is a deliberate act and
    /// starts the count over.
    banner_sent: HashMap<Episode, Vec<Instant>>,
    /// Banner snoozes by episode: the user asked for quiet on this subject
    /// until a deadline. Delivery-layer only — events still land in the
    /// alerts list and the engine's rules are untouched. Deliberately not
    /// persisted: a snooze means "not now", and a restart is a new now.
    snoozed: HashMap<Episode, Snooze>,
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
    /// The window `history` was (or is being) read for.
    history_range: HistoryRange,
    /// The order the History list shows.
    history_sort: HistorySort,
    /// The last (or in-flight) clean-hints update fetch.
    hints_sync: Option<HintsSync>,
    /// The last (or in-flight) Caches-preset roots fetch.
    caches_sync: Option<CachesSync>,
    template_sync: Option<TemplateSync>,
    /// Throttle for the alert list's midnight sweep.
    alert_day_checked_at: Option<Instant>,
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
    history_rows_scroll: ScrollHandle,
    app_rows_scroll: ScrollHandle,
    /// One-shot "scroll the Apps list to the selected row on the next
    /// paint", armed by [`Self::reveal_app`]. A `Cell` because the
    /// consumer is the render pass, which holds `&self`; taken once,
    /// so the reader's own scrolling wins from the frame after.
    app_reveal: Cell<bool>,
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
            dismissed_today: Vec::new(),
            alert_history: Vec::new(),
            tray_alert_unseen: false,
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
            space: None,
            space_at: None,
            space_inflight: false,
            settings_window: None,
            storage_window: None,
            disk_analysis_runs: 0,
            ejected: HashMap::new(),
            banner_sent: HashMap::new(),
            snoozed: HashMap::new(),
            proc_sort: ProcSort::default(),
            app_sort: AppSort::default(),
            sustained: SustainedWatch::default(),
            abnormal: AbnormalWatch::default(),
            net: NetActivity::default(),
            trend: AppTrend::default(),
            mem_trend: AppTrend::default(),
            creep_notified: HashMap::new(),
            history: None,
            history_range: HistoryRange::default(),
            history_sort: HistorySort::default(),
            hints_sync: None,
            caches_sync: None,
            template_sync: None,
            update_status: None,
            alert_day_checked_at: None,
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
            next_seq: 0,
            scroll: array::from_fn(|_| ScrollHandle::new()),
            proc_rows_scroll: ScrollHandle::new(),
            history_rows_scroll: ScrollHandle::new(),
            app_rows_scroll: ScrollHandle::new(),
            app_reveal: Cell::new(false),
        }
    }
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
        // interesting while someone is looking at the Hardware tab.
        if self.tab == Tab::Hardware {
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

    /// Fold one alert into the list, merging into its episode if that episode
    /// is already there and moving it back to the front.
    fn record_alert(&mut self, event: AlertEvent, now: SystemTime) {
        // A live report is news the tray spec can show. Ingest clears
        // it again if the Alerts tab is already on screen; a follow-up
        // of an episode they already saw re-lights once they look away,
        // the same way it would a banner.
        self.tray_alert_unseen = true;
        let episode = Episode::of(&event);
        if let Some(i) = self
            .alerts
            .iter()
            .position(|seen| Episode::of(&seen.event) == episode)
            && let Some(mut seen) = self.alerts.remove(i)
        {
            seen.at = now;
            seen.reports += 1;
            // A live report just named this pid: the card may act again.
            seen.live = true;
            seen.recovered_since = None;
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
            live: true,
            recovered_since: None,
            event,
        });
        while self.alerts.len() > MAX_ALERTS {
            self.alerts.pop_back();
        }
    }

    /// Fill the list from today's saved episodes. Called once at
    /// startup rather than from `Default` so the startup order stays
    /// visible in `main` — and so tests construct an empty state
    /// instead of inheriting the developer's own alerts.
    pub fn restore_alerts(&mut self) {
        self.adopt_alerts(alertlog::load());
        self.refresh_alert_history();
    }

    /// The restore proper, minus the file read: episodes join the list
    /// with fresh ids from the same counter live ones use, so a later
    /// crossing of the same condition merges into the restored episode
    /// instead of opening a duplicate beside it.
    fn adopt_alerts(&mut self, saved: Vec<alertlog::Restored>) {
        for saved in saved {
            self.next_seq += 1;
            self.alerts.push_back(SeenAlert {
                seq: self.next_seq,
                first_at: saved.first_at,
                at: saved.at,
                reports: saved.reports,
                // Read-only until a live report confirms the subject —
                // see [`SeenAlert::live`].
                live: false,
                recovered_since: None,
                event: saved.event,
            });
        }
    }

    /// Drop one episode from the list and the file. Display-layer only,
    /// like the banner snooze: the engine keeps evaluating, and a
    /// condition that still holds re-opens the episode on its next
    /// report. Without this the list has no acknowledgement path at
    /// all — it outlives restarts now, so the tab's alert tint would
    /// otherwise stay lit for the rest of the day.
    pub fn dismiss_alert(&mut self, seq: u64, cx: &mut Context<Self>) {
        if self.drop_alert(seq) {
            self.persist_alerts();
            cx.notify();
        }
    }

    /// The removal proper, minus the file write — `true` when the list
    /// actually changed.
    fn drop_alert(&mut self, seq: u64) -> bool {
        let Some(index) = self.alerts.iter().position(|seen| seen.seq == seq) else {
            return false;
        };
        let Some(seen) = self.alerts.remove(index) else {
            return false;
        };
        // Out of the list, into the record: the day's file keeps it
        // with the acknowledgement, so the week still says it fired.
        self.dismissed_today.push(alertlog::Restored {
            event: seen.event,
            first_at: seen.first_at,
            at: seen.at,
            reports: seen.reports,
            dismissed: true,
        });
        if self.dismissed_today.len() > MAX_ALERTS {
            self.dismissed_today.remove(0);
        }
        true
    }

    /// Retire episodes that are no longer today's. The file already
    /// draws this boundary when it loads; a session that runs past
    /// midnight has to draw it too, or "today's alerts" would quietly
    /// mean "since this app started". Throttled — the check is a
    /// calendar conversion, not something to do 30 times a minute.
    fn prune_stale_alerts(&mut self) {
        const CHECK_EVERY: Duration = Duration::from_secs(60);
        if self
            .alert_day_checked_at
            .is_some_and(|at| at.elapsed() < CHECK_EVERY)
        {
            return;
        }
        self.alert_day_checked_at = Some(Instant::now());
        if self.retain_today(SystemTime::now()) {
            self.persist_alerts();
        }
    }

    /// Keep only `now`'s episodes — `true` when something was retired.
    /// A clock with no readable calendar (before the epoch) prunes
    /// nothing: dropping the list on a broken clock is worse than
    /// keeping it.
    fn retain_today(&mut self, now: SystemTime) -> bool {
        let Some(today) = alertlog::local_date(now) else {
            return false;
        };
        let before = self.alerts.len() + self.dismissed_today.len();
        self.alerts
            .retain(|seen| alertlog::local_date(seen.at).as_deref() == Some(today.as_str()));
        self.dismissed_today
            .retain(|e| alertlog::local_date(e.at).as_deref() == Some(today.as_str()));
        let retired = self.alerts.len() + self.dismissed_today.len() != before;
        if retired {
            // Yesterday is now a past day: its file was written as it
            // happened, so the record only needs re-reading.
            self.refresh_alert_history();
        }
        retired
    }

    fn persist_alerts(&self) {
        let episodes: Vec<alertlog::Restored> = self
            .alerts
            .iter()
            .map(|seen| alertlog::Restored {
                event: seen.event.clone(),
                first_at: seen.first_at,
                at: seen.at,
                reports: seen.reports,
                dismissed: false,
            })
            .chain(self.dismissed_today.iter().map(|e| alertlog::Restored {
                event: e.event.clone(),
                first_at: e.first_at,
                at: e.at,
                reports: e.reports,
                dismissed: true,
            }))
            .collect();
        alertlog::save(&episodes);
    }

    /// The past week's record, newest day first, today excluded.
    pub fn alert_history(&self) -> &[alertlog::DayLog] {
        &self.alert_history
    }

    /// Re-read the past days' files. A handful of small files, read on
    /// the events that can change what they say — launch, entering the
    /// tab, the day turning — never per frame.
    pub fn refresh_alert_history(&mut self) {
        self.alert_history = alertlog::recent(ALERT_HISTORY_DAYS);
    }

    /// The most recent collection, or `None` before the first one lands.
    pub fn latest(&self) -> Option<&Tick> {
        self.latest.as_ref()
    }

    pub fn alerts(&self) -> &VecDeque<SeenAlert> {
        &self.alerts
    }

    /// The one question the tray's auto mode asks the store: is memory
    /// what needs attention right now? A memory-class episode (process,
    /// application, or kernel pressure) reported *this session*, not
    /// yet dismissed, and not recovered for [`TRAY_RECOVER`]. Restored
    /// episodes do not count: they are yesterday-shaped records, and
    /// the tray is about now. Dismissing the card still switches back
    /// immediately; a condition that still holds re-opens it on the
    /// next report.
    ///
    /// Recovery is the event's own bar against this tick's numbers —
    /// `threshold_bytes` on the card, `pressure_level > 1` for the
    /// kernel verdict — not a second threshold. Turning *on* still
    /// waits for zstats to report: the raw level flaps, and reading it
    /// to face memory would put that flap on the menu bar. Turning
    /// *off* after five minutes of the same "normal" zstats uses to
    /// end a pressure episode is the clear side of that rule, which
    /// the list never heard.
    pub fn memory_needs_attention(&self) -> bool {
        self.memory_needs_attention_at(SystemTime::now())
    }

    fn memory_needs_attention_at(&self, now: SystemTime) -> bool {
        self.alerts
            .iter()
            .any(|seen| seen.live && turns_the_face(&seen.event) && !seen.recovered_for(now))
    }

    /// Start or reset each live memory episode's recovery clock from
    /// this tick. Unknown samples (no process table, no pressure
    /// level) leave the clock where it was.
    ///
    /// Both transitions are logged, and at INFO rather than DEBUG on
    /// purpose: the question they answer — "the episode looks over,
    /// why is the menu bar still on memory?" — is asked about the
    /// *installed* build, where DEBUG is not being captured. It was
    /// asked once with no record to answer it from, and the honest
    /// reply was a guess about the level flapping. A reset line with
    /// how long the clock had run says which of the two it was.
    /// Transitions only: the arm re-holds every tick the condition
    /// holds, and those would be a line every few seconds saying
    /// nothing changed.
    fn note_memory_recovery(&mut self, now: SystemTime) {
        let Some(tick) = self.latest.as_ref() else {
            return;
        };
        let snapshot = &tick.snapshot;
        for seen in &mut self.alerts {
            if !seen.live || !is_memory_class(seen.event.kind()) {
                continue;
            }
            match memory_event_holds(&seen.event, snapshot) {
                Some(true) => {
                    // Only a clock that was actually running is a reset;
                    // the arm holds on every tick the condition holds,
                    // and logging those would be a line every few
                    // seconds saying nothing changed.
                    if let Some(started) = seen.recovered_since.take() {
                        tracing::info!(
                            kind = ?seen.event.kind(),
                            subject = ?seen.event.subject,
                            ran_for = ?now.duration_since(started).unwrap_or_default(),
                            "memory recovery clock reset"
                        );
                    }
                }
                Some(false) if seen.recovered_since.is_none() => {
                    seen.recovered_since = Some(now);
                    tracing::info!(
                        kind = ?seen.event.kind(),
                        subject = ?seen.event.subject,
                        after = ?TRAY_RECOVER,
                        "memory recovery clock started"
                    );
                }
                Some(false) | None => {}
            }
        }
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

    /// The scope Analyze will walk: the session pick, or home when
    /// nothing has been chosen. The chips read this so Home lights up
    /// as the default rather than looking unselected.
    pub fn disk_analysis_scope(&self) -> Option<ScanScope> {
        self.disk_analysis_root
            .clone()
            .or_else(|| diskscan::default_root().map(ScanScope::single))
    }

    /// Remember a scope without walking it. Analyze is what starts the
    /// walk — picking a chip used to launch immediately, which made a
    /// mis-tap cost minutes and hid the selected state.
    pub fn set_disk_analysis_scope(&mut self, scope: ScanScope, cx: &mut Context<Self>) {
        self.disk_analysis_root = Some(scope);
        cx.notify();
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
            self.disk_analysis = DiskAnalysis::Failed(i18n::tr("disk.ana_root_unsupported"));
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
            self.disk_analysis = DiskAnalysis::Failed("HOME is not set".into());
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
        if self.expanded.remove(&path).is_some() {
            cx.notify();
            return;
        }
        let DiskAnalysis::Ready(current) = &self.disk_analysis else {
            return;
        };
        match diskscan::drill(current, &path) {
            Some(derived) => {
                self.expanded.insert(path, Expansion::Ready(derived.dirs));
                cx.notify();
            }
            None => self.walk_expansion(path, cx),
        }
    }

    /// The index had nothing under this row, so walk it. One at a time:
    /// a second open cancels the first, whose thread stops and whose
    /// events are dropped by the run-id guard either way.
    fn walk_expansion(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(cancel) = self.expand_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.expand_runs += 1;
        let run_id = self.expand_runs;
        let cancel = Arc::new(AtomicBool::new(false));
        self.expand_cancel = Some(cancel.clone());
        self.expanded.insert(path.clone(), Expansion::Walking);
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
                    if state.expand_runs != run_id
                        || !matches!(state.expanded.get(&path), Some(Expansion::Walking))
                    {
                        return;
                    }
                    state.expanded.insert(path.clone(), landed);
                    cx.notify();
                });
                break;
            }
        })
        .detach();
    }

    /// What is under an opened row, or `None` when it is closed.
    pub fn expansion(&self, path: &Path) -> Option<&Expansion> {
        self.expanded.get(path)
    }

    /// Every open row closes when the result they describe goes away —
    /// a new walk, a cleared card. Children of a replaced result would
    /// be figures from a scan that is no longer on screen.
    fn drop_expansions(&mut self) {
        self.expanded.clear();
        if let Some(cancel) = self.expand_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn analysis_show_all_dirs(&self) -> bool {
        self.analysis_show_all_dirs
    }

    pub fn set_analysis_show_all_dirs(&mut self, show: bool, cx: &mut Context<Self>) {
        self.analysis_show_all_dirs = show;
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
        let top_roots = match &self.disk_analysis {
            DiskAnalysis::Ready(r) => Some(r.roots.clone()),
            _ => None,
        };
        if let Some(roots) = top_roots {
            diskscan::delete_cache(&roots);
        }
        // The baseline's file went with the cache; the flattened copy
        // must not outlive it.
        self.analysis_diff = None;
        self.drop_expansions();
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
        self.drop_expansions();
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
    /// results are never kept, so this goes to Off rather than showing
    /// half a table.
    pub fn cancel_disk_analysis(&mut self, cx: &mut Context<Self>) {
        self.cancel_disk_analysis_walk();
        self.disk_analysis = DiskAnalysis::Off;
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
        self.resume_banners(event);
        cx.notify();
    }

    /// The un-mute proper, minus the repaint. "Resume" is unambiguous, so
    /// it clears the auto-quiet too — one left standing would keep the
    /// subject silent behind the user's back.
    fn resume_banners(&mut self, event: &AlertEvent) {
        let key = Episode::of(event);
        self.snoozed.remove(&key);
        self.banner_sent.remove(&key);
    }

    /// Whether this event's banner is muted right now. Runs on every fresh
    /// event, which is also where expired entries get dropped — the map
    /// never outlives its deadlines by more than one alert.
    pub fn banner_snoozed(&mut self, event: &AlertEvent) -> bool {
        let now = Instant::now();
        self.snoozed.retain(|_, s| s.until > now);
        self.snoozed.contains_key(&Episode::of(event))
    }

    /// Whether this event's banner is being held back because the same
    /// episode has already interrupted [`NOISY_AFTER`] times inside
    /// [`NOISY_WINDOW`]. Delivery-layer only, exactly like the snooze:
    /// the engine keeps evaluating, the list keeps recording and the card
    /// keeps counting reports — what stops is the interruption.
    ///
    /// Records the delivery it permits, so the window slides and the
    /// subject gets its voice back once it quiets down.
    pub fn banner_damped(&mut self, event: &AlertEvent, now: Instant) -> bool {
        let sent = self.banner_sent.entry(Episode::of(event)).or_default();
        sent.retain(|at| now.duration_since(*at) < NOISY_WINDOW);
        if sent.len() >= NOISY_AFTER {
            return true;
        }
        sent.push(now);
        false
    }

    /// Whether a card should say it has gone auto-quiet. Read-only — the
    /// count is only ever advanced by an actual delivery attempt.
    pub fn banner_auto_quiet(&self, event: &AlertEvent) -> bool {
        let now = Instant::now();
        self.banner_sent
            .get(&Episode::of(event))
            .is_some_and(|sent| {
                sent.iter()
                    .filter(|at| now.duration_since(**at) < NOISY_WINDOW)
                    .count()
                    >= NOISY_AFTER
            })
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

    /// The tray corner spec is on: a live report has landed since the
    /// Alerts tab was last on screen.
    pub fn tray_alert_unseen(&self) -> bool {
        self.tray_alert_unseen
    }

    /// The Alerts tab is (or is about to be) what the user is looking
    /// at: the spec has done its job. Does not touch the episode list.
    pub fn see_alerts(&mut self) {
        self.tray_alert_unseen = false;
    }

    /// Reveal path: opening the panel onto Alerts is the same as
    /// switching to it. Other tabs leave the spec — that is the
    /// reminder to go look.
    pub fn see_alerts_if_showing(&mut self, cx: &mut Context<Self>) {
        if self.tab == Tab::Alerts {
            self.see_alerts();
            #[cfg(not(target_os = "linux"))]
            tray::sync(cx, self);
        }
    }

    fn alerts_are_showing(&self, cx: &Context<Self>) -> bool {
        self.tab == Tab::Alerts
            && cx
                .try_global::<metrics::CollectorPace>()
                .is_some_and(|p| p.is_visible())
    }

    pub fn settings_window(&self) -> Option<gpui::AnyWindowHandle> {
        self.settings_window
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

    /// The delete button's confirmed action: move to the Trash, then drop
    /// the row. A failed trash leaves the row — a file that is still there
    /// must not vanish from the list.
    /// Put the listing away — back to "not asked yet", which is what the
    /// card shows before the first query. A view action only: the query
    /// costs seconds to repeat, and the stored baseline stays, so the
    /// next listing can still say what it added. Nothing on disk moves.
    pub fn clear_big_files(&mut self, cx: &mut Context<Self>) {
        self.big_files = BigFiles::Off;
        cx.notify();
    }

    pub fn trash_big_file(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Err(e) = bigfiles::trash(path) {
            tracing::warn!("trash {}: {e}", path.display());
            return;
        }
        if let BigFiles::Ready { scan, added, .. } = &mut self.big_files {
            scan.files.retain(|f| f.path != path);
            scan.total = scan.total.saturating_sub(1);
            added.remove(path);
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
                Err(e) => tracing::warn!("trash {}: {e}", path.display()),
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
        // The card always shows the session's top-level result now
        // (opening a row nests under it instead of replacing it), so the
        // one result on screen is exactly the one that owns a cache file.
        if let DiskAnalysis::Ready(result) = &mut self.disk_analysis {
            prune(result);
            diskscan::resave_if_cached(result);
        }
        // Opened rows are tables too: a trashed tree must not survive as
        // somebody's child row, and a row for the tree itself closes.
        self.expanded
            .retain(|path, _| !gone.iter().any(|g| path.starts_with(g)));
        for state in self.expanded.values_mut() {
            if let Expansion::Ready(rows) = state {
                rows.retain(|h| !gone.iter().any(|g| h.path.starts_with(g)));
            }
        }
        cx.notify();
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

    /// A freshly built disk-space window starts without yesterday's
    /// index query, and with the dirs table folded back to its default
    /// length. Only on a *new* window: raising one that is already open
    /// must not wipe what its owner is reading.
    ///
    /// The analysis result itself survives on purpose — it costs minutes
    /// to produce and is cached to disk across restarts; the caption says
    /// how old it is.
    pub fn reset_storage_views(&mut self, cx: &mut Context<Self>) {
        self.big_files = BigFiles::Off;
        self.analysis_show_all_dirs = false;
        // Opened rows are questions too — a window opened tomorrow should
        // show the result the way a finished scan leaves it, not a tree
        // somebody unfolded yesterday.
        self.drop_expansions();
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
            if tab == Tab::Hardware {
                self.ensure_space_info(cx);
            }
            // Opening History is what pays for reading it. Re-read on every
            // visit rather than caching: the file grows a line a minute, and
            // a stale "today" is worse than a moment's wait.
            if tab == Tab::History {
                self.load_history(cx);
            }
            // The past week's files are small and change only at
            // midnight; re-reading them on the way into the tab is
            // what keeps a day-old photograph from being the record.
            if tab == Tab::Alerts {
                self.refresh_alert_history();
                self.see_alerts();
                #[cfg(not(target_os = "linux"))]
                tray::sync(cx, self);
            }
            // Apps / Overview titles need the full ppid chain and the
            // process groups for a job face. Kick it here so the first
            // paint after the switch is not waiting on the next tick.
            if matches!(tab, Tab::Apps | Tab::Overview) {
                self.ensure_apps_topology(cx);
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
            if matches!(self.member_table, MemberTable::Failed) {
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
            if matches!(self.member_table, MemberTable::Failed) {
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
        let visible = cx
            .try_global::<metrics::CollectorPace>()
            .is_some_and(|p| p.is_visible());
        if !visible || !matches!(self.tab, Tab::Apps | Tab::Overview) {
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
        match &self.member_table {
            MemberTable::Running => return,
            MemberTable::Failed => return,
            MemberTable::Ready {
                refreshing: true, ..
            } => return,
            MemberTable::Ready { at, .. } if at.elapsed() < metrics::PANEL_PROCESS_INTERVAL => {
                return;
            }
            MemberTable::Ready { .. } => {
                self.start_member_table(cx);
                return;
            }
            MemberTable::Off => {}
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
                            _ => MemberTable::Failed,
                        }
                    }
                };
                cx.notify();
            });
        })
        .detach();
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

    #[test]
    fn a_writable_disk_still_alerts_and_statfs_failure_is_fail_open() {
        assert!(super::keep_alert(&cpu_alert(1)));
        assert!(
            super::keep_alert(&disk_alert("/")),
            "the boot volume must still alert"
        );
        assert!(
            super::keep_alert(&disk_alert("/Volumes/no-such-volume")),
            "a mount we cannot inspect is not silently exempted"
        );
    }

    fn disk_alert(mount: &str) -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::Volume {
                mount_point: mount.into(),
            },
            detail: AlertDetail::Disk {
                used_percent: 99.0,
                threshold_percent: 90.0,
                available_bytes: 0,
                total_bytes: 1 << 30,
            },
            repeat_after: None,
        }
    }

    fn cpu_alert(pid: u32) -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::Process {
                pid,
                name: format!("p{pid}"),
                display_name: None,
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
                display_name: None,
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

    fn pressure_alert(level: u32) -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::System,
            detail: AlertDetail::Pressure {
                level,
                sustained: Duration::from_secs(300),
                swap_used_bytes: 1 << 30,
                swap_total_bytes: 2 << 30,
                compressed_bytes: None,
                top_consumers: vec![],
            },
            repeat_after: None,
        }
    }

    fn empty_tick() -> Tick {
        use zstats::snapshot::{CpuSnapshot, HostInfo, LoadSnapshot, MemorySnapshot};
        Tick {
            snapshot: SystemSnapshot {
                timestamp: jiff::Timestamp::now(),
                host: HostInfo {
                    hostname: String::new(),
                    os_name: String::new(),
                    os_version: String::new(),
                    kernel_version: None,
                    arch: String::new(),
                    uptime_secs: 0,
                    labels: HashMap::new(),
                },
                cpu: CpuSnapshot {
                    usage_percent: 0.0,
                    per_core_usage: vec![],
                    logical_cores: 1,
                    physical_cores: None,
                    frequency_mhz: None,
                    per_core_frequency_mhz: vec![],
                    brand: None,
                    perf_levels: None,
                },
                memory: MemorySnapshot {
                    total_bytes: 16 << 30,
                    used_bytes: 0,
                    available_bytes: 16 << 30,
                    swap_total_bytes: 0,
                    swap_used_bytes: 0,
                    used_percent: 0.0,
                    swap_used_percent: 0.0,
                    compressed_bytes: None,
                    pressure_level: Some(1),
                },
                disks: None,
                networks: None,
                processes: None,
                process_groups: None,
                total_processes: None,
                battery: None,
                load: LoadSnapshot {
                    load1: 0.0,
                    load5: 0.0,
                    load15: 0.0,
                },
                temperatures: None,
                io_totals: Default::default(),
                capabilities: Default::default(),
                extras: HashMap::new(),
            },
            alerts: vec![],
            process_stats: HashMap::new(),
            records: vec![],
        }
    }

    /// zstats reports a crossing once and follows up once 30 minutes later.
    /// Both describe the same episode, and a list that appends a card per
    /// event turns one problem into two — then lets a flapping process crowd
    /// everything else out of the 20 slots.
    #[test]
    fn repeat_reports_merge_into_one_episode() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();

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

    /// A subject that keeps crossing, clearing and crossing again opens a
    /// fresh episode each time, and each one used to interrupt. Two is
    /// enough to have said it; the rest go to the list only.
    #[test]
    fn a_flapping_subject_stops_interrupting_after_two_banners() {
        let mut state = ZStatsAppState::new();
        let t0 = Instant::now();
        let event = cpu_alert(7);

        assert!(!state.banner_damped(&event, t0), "first one interrupts");
        assert!(
            !state.banner_damped(&event, t0 + Duration::from_secs(600)),
            "so does the second"
        );
        assert!(
            state.banner_damped(&event, t0 + Duration::from_secs(1200)),
            "the third within the hour does not"
        );
        assert!(state.banner_auto_quiet(&event), "and the card says so");

        // A different subject is different news — it must still arrive.
        assert!(!state.banner_damped(&mem_alert(9), t0 + Duration::from_secs(1200)));

        // Once the window has slid past both deliveries, it speaks again.
        let later = t0 + Duration::from_secs(3600 + 700);
        assert!(
            !state.banner_damped(&event, later),
            "quiet for an hour buys back a banner"
        );
    }

    /// "Resume" has to mean resume: an auto-quiet that outlived the
    /// explicit un-mute would keep the subject silent behind the user.
    #[test]
    fn resuming_a_snooze_also_clears_the_auto_quiet() {
        let mut state = ZStatsAppState::new();
        let t0 = Instant::now();
        let event = cpu_alert(7);
        assert!(!state.banner_damped(&event, t0));
        assert!(!state.banner_damped(&event, t0));
        assert!(state.banner_auto_quiet(&event));

        state.resume_banners(&event);
        assert!(!state.banner_auto_quiet(&event));
        assert!(
            !state.banner_damped(&event, t0),
            "and the next one interrupts again"
        );
    }

    /// A restart is not a new problem: an episode read back from the
    /// file is the same episode, so the next report merges into it and
    /// the count keeps climbing.
    #[test]
    fn a_restored_episode_is_continued_not_duplicated() {
        let mut state = ZStatsAppState::new();
        let morning = SystemTime::now() - Duration::from_secs(4 * 3600);
        state.adopt_alerts(vec![alertlog::Restored {
            event: cpu_alert(7),
            first_at: morning,
            at: morning,
            reports: 2,
            dismissed: false,
        }]);
        assert_eq!(state.alerts().len(), 1);

        state.record_alert(cpu_alert(7), SystemTime::now());
        assert_eq!(state.alerts().len(), 1, "same condition, same episode");
        assert_eq!(state.alerts()[0].reports, 3, "the count carries over");
        assert!(
            state.alerts()[0]
                .span()
                .is_some_and(|s| s >= Duration::from_secs(4 * 3600)),
            "the episode still knows it started this morning"
        );

        // A different condition opens its own card with its own id.
        state.record_alert(mem_alert(7), SystemTime::now());
        assert_eq!(state.alerts().len(), 2);
        assert_ne!(state.alerts()[0].seq, state.alerts()[1].seq);
    }

    /// The tray spec is "a live report you have not opened Alerts
    /// for", not a count of cards. Restored episodes are yesterday's
    /// news; a follow-up of one already seen re-lights once they look
    /// away.
    #[test]
    fn a_live_report_lights_the_tray_spec_until_alerts_are_shown() {
        let mut state = ZStatsAppState::new();
        assert!(!state.tray_alert_unseen());

        state.adopt_alerts(vec![alertlog::Restored {
            event: cpu_alert(7),
            first_at: SystemTime::now(),
            at: SystemTime::now(),
            reports: 1,
            dismissed: false,
        }]);
        assert!(
            !state.tray_alert_unseen(),
            "a restored card is not a new alert"
        );

        state.record_alert(cpu_alert(8), SystemTime::now());
        assert!(state.tray_alert_unseen(), "a live report lights the spec");

        state.see_alerts();
        assert!(!state.tray_alert_unseen());

        state.record_alert(cpu_alert(8), SystemTime::now());
        assert!(
            state.tray_alert_unseen(),
            "a follow-up while away re-lights"
        );
    }

    /// The acting controls on a card are gated on the pid having been
    /// confirmed *this session*: after a reboot the pid a restored card
    /// names may belong to something else entirely, and "quit Chrome"
    /// would deliver SIGTERM to whatever holds it now.
    #[test]
    fn a_restored_card_cannot_act_until_a_live_report_confirms_it() {
        let mut state = ZStatsAppState::new();
        state.adopt_alerts(vec![alertlog::Restored {
            event: mem_alert(923),
            first_at: SystemTime::now() - Duration::from_secs(7200),
            at: SystemTime::now() - Duration::from_secs(7200),
            reports: 1,
            dismissed: false,
        }]);
        assert!(!state.alerts()[0].live, "restored is read-only");

        // The same condition reported again names the pid live.
        state.record_alert(mem_alert(923), SystemTime::now());
        assert!(state.alerts()[0].live, "a live report re-arms the card");
        assert_eq!(state.alerts().len(), 1, "still one episode");
    }

    /// The list outlives restarts now, so it needs a way to be put down
    /// — otherwise the tab's alert tint stays lit for the rest of the
    /// day with nothing the user can do about it.
    #[test]
    fn dismiss_removes_one_episode_and_leaves_the_rest() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(cpu_alert(7), t0);
        state.record_alert(cpu_alert(8), t0);
        let doomed = state.alerts()[0].seq;

        assert!(state.drop_alert(doomed));
        assert_eq!(state.alerts().len(), 1);
        assert_ne!(state.alerts()[0].seq, doomed);
        assert!(
            !state.drop_alert(doomed),
            "dismissing twice changes nothing"
        );
    }

    /// Auto faces memory while a live memory episode still holds, and
    /// only after five minutes under the event's own bar — not on a
    /// one-sample dip, and not by evaluating a new threshold.
    #[test]
    fn auto_tray_returns_to_cpu_five_minutes_after_memory_recovers() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(mem_alert(7), t0);
        assert!(
            state.memory_needs_attention_at(t0),
            "a live memory episode faces memory"
        );

        let mut tick = empty_tick();
        let mut p = snap(7, "p7");
        p.phys_footprint_bytes = Some(100 << 20);
        tick.snapshot.processes = Some(Arc::new(vec![p]));
        state.latest = Some(tick);
        state.note_memory_recovery(t0);
        assert!(
            state.memory_needs_attention_at(t0),
            "just recovered is still memory"
        );
        assert!(
            state.memory_needs_attention_at(t0 + Duration::from_secs(4 * 60 + 59)),
            "four minutes under the bar is not five"
        );
        assert!(
            !state.memory_needs_attention_at(t0 + TRAY_RECOVER),
            "five minutes under the event's bar returns to CPU"
        );
    }

    #[test]
    fn auto_tray_stays_on_memory_while_the_process_is_still_over() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(mem_alert(7), t0);
        let mut tick = empty_tick();
        let mut p = snap(7, "p7");
        p.phys_footprint_bytes = Some(8 << 30);
        tick.snapshot.processes = Some(Arc::new(vec![p]));
        state.latest = Some(tick);
        state.note_memory_recovery(t0);
        assert!(state.memory_needs_attention_at(t0 + TRAY_RECOVER));
    }

    #[test]
    fn auto_tray_faces_memory_again_if_the_condition_returns() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(mem_alert(7), t0);
        let mut quiet = empty_tick();
        let mut p = snap(7, "p7");
        p.phys_footprint_bytes = Some(100 << 20);
        quiet.snapshot.processes = Some(Arc::new(vec![p.clone()]));
        state.latest = Some(quiet);
        state.note_memory_recovery(t0);

        p.phys_footprint_bytes = Some(8 << 30);
        let mut loud = empty_tick();
        loud.snapshot.processes = Some(Arc::new(vec![p]));
        state.latest = Some(loud);
        state.note_memory_recovery(t0 + Duration::from_secs(60));
        assert!(
            state.memory_needs_attention_at(t0 + TRAY_RECOVER + Duration::from_secs(60)),
            "crossing again resets the five minutes"
        );
    }

    #[test]
    fn auto_tray_pressure_returns_after_five_minutes_of_normal() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(pressure_alert(4), t0);
        let mut tick = empty_tick();
        tick.snapshot.memory.pressure_level = Some(4);
        state.latest = Some(tick);
        state.note_memory_recovery(t0);
        assert!(state.memory_needs_attention_at(t0));

        let mut normal = empty_tick();
        normal.snapshot.memory.pressure_level = Some(1);
        state.latest = Some(normal);
        state.note_memory_recovery(t0);
        assert!(state.memory_needs_attention_at(t0 + Duration::from_secs(60)));
        assert!(!state.memory_needs_attention_at(t0 + TRAY_RECOVER));
    }

    /// A memory-heavy Mac sits at the kernel's warning tier as its
    /// steady state, so that tier does not get the menu bar — the card
    /// and the banner still carry it. Critical does, and an episode
    /// that escalates turns the face on the report that says so.
    #[test]
    fn auto_tray_waits_for_critical_pressure_but_not_for_warning() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(pressure_alert(2), t0);
        let mut warned = empty_tick();
        warned.snapshot.memory.pressure_level = Some(2);
        state.latest = Some(warned);
        state.note_memory_recovery(t0);
        assert!(
            !state.memory_needs_attention_at(t0),
            "warning is this platform's normal, not news for the menu bar"
        );
        // The episode is still on the tab: only the face is withheld.
        assert_eq!(state.alerts().len(), 1);

        // Worsening is reported as a fresh event on the same episode.
        state.record_alert(pressure_alert(4), t0);
        let mut critical = empty_tick();
        critical.snapshot.memory.pressure_level = Some(4);
        state.latest = Some(critical);
        state.note_memory_recovery(t0);
        assert!(state.memory_needs_attention_at(t0));
    }

    /// The clock's two transitions are what the log reports, so the
    /// state they read from has to move exactly once per transition:
    /// the arm re-holds on every tick the condition holds, and a line
    /// per tick would drown the one that matters.
    #[test]
    fn the_recovery_clock_moves_only_on_a_transition() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(pressure_alert(4), t0);
        let normal = || {
            let mut tick = empty_tick();
            tick.snapshot.memory.pressure_level = Some(1);
            tick
        };
        state.latest = Some(normal());
        state.note_memory_recovery(t0);
        let started = state.alerts()[0].recovered_since.expect("clock started");
        // A second quiet tick must not restart it — that would push the
        // deadline out forever and log a line each time.
        state.latest = Some(normal());
        state.note_memory_recovery(t0 + Duration::from_secs(5));
        assert_eq!(state.alerts()[0].recovered_since, Some(started));

        // Back over the line: cleared, so the next quiet tick is a
        // genuine restart.
        let mut over = empty_tick();
        over.snapshot.memory.pressure_level = Some(4);
        state.latest = Some(over);
        state.note_memory_recovery(t0 + Duration::from_secs(60));
        assert!(state.alerts()[0].recovered_since.is_none());
        state.latest = Some(normal());
        state.note_memory_recovery(t0 + Duration::from_secs(65));
        assert_eq!(
            state.alerts()[0].recovered_since,
            Some(t0 + Duration::from_secs(65))
        );
    }

    /// A process over its memory bar is Warning in zstats — only
    /// pressure ≥ 4 and a runaway CPU are Critical — so gating the
    /// whole class on severity would have deleted the face's original
    /// job.
    #[test]
    fn auto_tray_still_turns_for_a_process_memory_episode() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(mem_alert(7), t0);
        assert_eq!(state.alerts()[0].event.severity(), Severity::Warning);
        assert!(state.memory_needs_attention_at(t0));
    }

    #[test]
    fn auto_tray_ignores_restored_memory_episodes() {
        let mut state = ZStatsAppState::new();
        state.adopt_alerts(vec![alertlog::Restored {
            event: mem_alert(7),
            first_at: SystemTime::now(),
            at: SystemTime::now(),
            reports: 1,
            dismissed: false,
        }]);
        assert!(!state.memory_needs_attention_at(SystemTime::now()));
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

    /// "Today's alerts" has to keep meaning today on a machine that
    /// never restarts — the file draws that boundary when it loads, and
    /// a session running past midnight has to draw it too.
    #[test]
    fn the_day_boundary_retires_yesterdays_episodes() {
        let mut state = ZStatsAppState::new();
        let now = SystemTime::now();
        state.record_alert(cpu_alert(7), now - Duration::from_secs(3 * 86_400));
        state.record_alert(cpu_alert(8), now);
        assert_eq!(state.alerts().len(), 2);

        assert!(state.retain_today(now), "the stale one is retired");
        assert_eq!(state.alerts().len(), 1);
        assert!(matches!(
            state.alerts()[0].event.subject,
            AlertSubject::Process { pid: 8, .. }
        ));
        assert!(!state.retain_today(now), "nothing left to retire");
    }

    /// The id has to outlive reordering — it is what element state (hover,
    /// the expanded editor) is keyed on.
    #[test]
    fn episode_ids_are_unique_and_stable() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
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

        let mut app = AppSort::default();
        app = app.next();
        assert_eq!(app, AppSort::Memory);
        app = app.next();
        assert_eq!(app, AppSort::Cpu, "apps cycle is two-way");
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
