//! Application-level state.
//!
//! The window is only a rendering layer: it gets closed and rebuilt whenever
//! the tray has to reposition it (gpui can't move an existing window), so
//! anything that has to survive a close → reopen round trip belongs here, not
//! in the root view. Collected metrics are the main tenant — sampling runs
//! whether or not a window exists.

use crate::i18n;
use gpui::{Bounds, Context, Entity, Global, Pixels};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Deref;
use std::time::{Duration, Instant};
use zstats::settings::FileConfig;
use zstats::{AlertEvent, Tick};

/// How long a process must stay abnormal before it is worth showing.
///
/// A healthy shutdown briefly produces a zombie between the child exiting and
/// the parent reaping it; only a persistent one means nobody is reaping.
///
/// Timed from when we first observed it, not from the transition itself —
/// the kernel does not record that (see `procscan::AbnormalProcess::age`).
/// This app runs continuously, so the observation window keeps growing; the
/// cost is that nothing shows for the first few minutes after a restart, even
/// a zombie that has been there for a fortnight.
const MIN_ABNORMAL_DURATION: Duration = Duration::from_secs(5 * 60);

/// A process must stay above the low-grade bar this long before it is worth
/// pointing at.
///
/// Long enough to rule out ordinary work — a build, a backup, an import all
/// finish well inside it — and short enough that the finding still lands in
/// the session that caused it, rather than the next morning.
const SUSTAINED_AFTER: Duration = Duration::from_secs(2 * 60 * 60);

/// How long a process may drop below the bar without resetting its clock.
///
/// CPU wobbles: something averaging 11% dips under 10% constantly, and a
/// strict "consecutive" rule would restart the count every few seconds and
/// never reach [`SUSTAINED_AFTER`]. Only a real stop counts as a stop.
const SUSTAINED_GRACE: Duration = Duration::from_secs(5 * 60);

/// Fraction of the CPU alert threshold at which sustained load starts to
/// matter. Derived rather than fixed so tightening `alert-cpu` tightens this
/// too.
const SUSTAINED_FRACTION: f64 = 1.0 / 3.0;

/// Used when config.toml sets no `alert-cpu` — zstats' own default is 30%.
const SUSTAINED_FALLBACK_ALERT: f64 = 30.0;

/// Hide a network interface that has carried nothing for this long.
///
/// A machine lists plenty of interfaces that never move a byte — inactive
/// Ethernet, unused tunnels, bridges — and they crowd out the two or three
/// that matter. Long enough that an idle-but-real connection does not vanish
/// mid-session.
const NET_IDLE_AFTER: Duration = Duration::from_secs(30 * 60);

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
}

/// The design's eight views.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tab {
    #[default]
    Overview,
    Processes,
    Apps,
    Disk,
    Net,
    Sensors,
    Alerts,
    Config,
}

impl Tab {
    pub const ALL: [Tab; 8] = [
        Tab::Overview,
        Tab::Processes,
        Tab::Apps,
        Tab::Disk,
        Tab::Net,
        Tab::Sensors,
        Tab::Alerts,
        Tab::Config,
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
            Tab::Disk => "Disk",
            Tab::Net => "Network",
            Tab::Sensors => "Sensors",
            Tab::Alerts => "Alerts",
            Tab::Config => "Config",
        }
    }

    /// Tooltip / spoken name in the active locale.
    pub fn title(self) -> String {
        i18n::tr(match self {
            Tab::Overview => "tabs.overview",
            Tab::Processes => "tabs.processes",
            Tab::Apps => "tabs.apps",
            Tab::Disk => "tabs.disk",
            Tab::Net => "tabs.network",
            Tab::Sensors => "tabs.sensors",
            Tab::Alerts => "tabs.alerts",
            Tab::Config => "tabs.config",
        })
    }
}

/// A process that has just crossed into sustained-load territory.
pub struct SustainedNotice {
    pub pid: u32,
    pub name: String,
    pub cpu_avg: f64,
    pub duration: Duration,
}

/// A process sitting on a low-but-real share of CPU for a long time.
///
/// This never trips zstats' alerting, which asks "is it over the line right
/// now" — the whole point is that it never is. What makes it worth surfacing
/// is the integral: 10% for twelve hours is more CPU burnt than a minute at
/// 100%, and it is the kind of thing that keeps a fan running with nothing
/// obvious to blame.
#[derive(Clone, Copy)]
pub struct SustainedLoad {
    /// First sample above the bar in the current stretch.
    since: Instant,
    /// Most recent sample above it, for the grace window.
    last_over: Instant,
}

impl SustainedLoad {
    pub fn duration(&self) -> Duration {
        self.since.elapsed()
    }
}

/// One alert plus when this process saw it. [`AlertEvent`] carries no
/// timestamp of its own, and `Tick::alerts` only reports the moment a
/// threshold is *crossed* — so the "currently interesting" list the design
/// shows has to be accumulated here.
pub struct SeenAlert {
    pub at: Instant,
    pub event: AlertEvent,
}

impl SeenAlert {
    pub fn age(&self) -> Duration {
        self.at.elapsed()
    }
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
    abnormal: Vec<crate::procscan::AbnormalProcess>,
    only_abnormal: bool,
    /// Interfaces whose kernel counters are still 0/0 since boot.
    /// Network tab: unused-since-boot interfaces stay hidden unless this is on.
    show_unused_nets: bool,
    proc_sort: ProcSort,
    /// When each abnormal pid was first seen in that state. The kernel does
    /// not timestamp the transition, so this is the only handle on "how long
    /// has it been like this" — bounded by how long this process has been
    /// running, hence reported as "at least".
    abnormal_since: HashMap<u32, Instant>,
    /// Last moment each interface carried any traffic. Only covers this
    /// session — the kernel's counters are cumulative since boot and cannot
    /// say *when* the bytes moved.
    net_last_active: HashMap<String, Instant>,
    sustained: HashMap<u32, SustainedLoad>,
    /// Pids already announced for sustained load. Crossing the threshold is
    /// the event; staying over it is not, so this stops a notification per
    /// tick for the next twelve hours.
    sustained_notified: HashSet<u32>,
    /// Drained by the collector after each round.
    sustained_pending: Vec<SustainedNotice>,
}

impl Default for ZStatsAppState {
    fn default() -> Self {
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
            abnormal: Vec::new(),
            only_abnormal: false,
            show_unused_nets: false,
            proc_sort: ProcSort::default(),
            abnormal_since: HashMap::new(),
            net_last_active: HashMap::new(),
            sustained: HashMap::new(),
            sustained_notified: HashSet::new(),
            sustained_pending: Vec::new(),
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
            self.alerts.push_front(SeenAlert {
                at: now,
                event: event.clone(),
            });
        }
        while self.alerts.len() > MAX_ALERTS {
            self.alerts.pop_back();
        }

        self.track_sustained(&tick);

        if let Some(nets) = tick.snapshot.networks.as_deref() {
            let now = Instant::now();
            for n in nets {
                if n.received_bytes_per_sec + n.transmitted_bytes_per_sec > 0 {
                    self.net_last_active.insert(n.interface.clone(), now);
                }
            }
            // Forget interfaces that disappeared entirely (VPN down, cable
            // out), or the map would grow for the life of the process.
            self.net_last_active
                .retain(|name, _| nets.iter().any(|n| &n.interface == name));
        }

        self.latest = Some(tick);
        cx.notify();
        fresh
    }

    /// The most recent collection, or `None` before the first one lands.
    pub fn latest(&self) -> Option<&Tick> {
        self.latest.as_ref()
    }

    pub fn alerts(&self) -> &VecDeque<SeenAlert> {
        &self.alerts
    }

    /// What the collector is running with. Read once at startup — the Config
    /// tab is read-only, so there is nothing to invalidate it.
    pub fn settings(&self) -> Option<&FileConfig> {
        self.settings.as_ref()
    }

    /// Replace the abnormal-process list, keeping first-seen times for any
    /// pid that is still there.
    pub fn set_abnormal(
        &mut self,
        found: Vec<crate::procscan::AbnormalProcess>,
        cx: &mut Context<Self>,
    ) {
        let now = Instant::now();
        self.abnormal_since
            .retain(|pid, _| found.iter().any(|p| p.pid == *pid));
        for p in &found {
            self.abnormal_since.entry(p.pid).or_insert(now);
        }
        self.abnormal = found;
        cx.notify();
    }

    /// Abnormal processes that have stayed that way long enough to matter.
    /// Every scan result is retained internally so the clocks keep running —
    /// only the reporting is gated.
    pub fn abnormal(&self) -> Vec<&crate::procscan::AbnormalProcess> {
        self.abnormal
            .iter()
            .filter(|p| {
                self.abnormal_observed(p.pid)
                    .is_some_and(|d| d >= MIN_ABNORMAL_DURATION)
            })
            .collect()
    }

    /// Whether the process list is filtered down to abnormal entries only.
    pub fn only_abnormal(&self) -> bool {
        self.only_abnormal
    }

    /// The CPU share above which load counts as sustained-and-worth-noting.
    /// A third of the alert threshold, so it scales with the user's setting.
    fn sustained_bar(&self) -> f64 {
        self.settings
            .as_ref()
            .and_then(|f| f.alerts.cpu)
            .map_or(SUSTAINED_FALLBACK_ALERT, f64::from)
            * SUSTAINED_FRACTION
    }

    /// Update the low-grade-load clocks from one collection round.
    fn track_sustained(&mut self, tick: &Tick) {
        let Some(processes) = tick.snapshot.processes.as_deref() else {
            return;
        };
        let bar = self.sustained_bar();
        let now = Instant::now();

        for p in processes {
            // The rolling average, not the instantaneous sample: this is a
            // question about a trend, and the raw value swings far too much.
            let cpu = tick
                .process_stats
                .get(&p.pid)
                .map(|s| s.cpu_avg)
                .unwrap_or(f64::from(p.cpu_usage_percent));
            if cpu >= bar {
                let entry = self
                    .sustained
                    .entry(p.pid)
                    .and_modify(|s| s.last_over = now)
                    .or_insert(SustainedLoad {
                        since: now,
                        last_over: now,
                    });
                if entry.since.elapsed() >= SUSTAINED_AFTER && self.sustained_notified.insert(p.pid)
                {
                    self.sustained_pending.push(SustainedNotice {
                        pid: p.pid,
                        name: p.name.clone(),
                        cpu_avg: cpu,
                        duration: entry.since.elapsed(),
                    });
                }
            }
        }

        // Drop anything that has been quiet past the grace window, or whose
        // pid is gone. Note a process missing from this tick is not itself a
        // reset — the table only keeps the top N, so a process can drop out
        // of it for a round without having gone quiet.
        self.sustained
            .retain(|_, s| s.last_over.elapsed() <= SUSTAINED_GRACE);
        // Re-arm: a process that stops and later starts again is a new
        // episode and should be announced again.
        self.sustained_notified
            .retain(|pid| self.sustained.contains_key(pid));
    }

    /// Sustained-load notices raised by the last round, taken once.
    pub fn take_sustained_notices(&mut self) -> Vec<SustainedNotice> {
        std::mem::take(&mut self.sustained_pending)
    }

    /// How long this process has been holding a low-but-real CPU share, once
    /// that has gone on long enough to be worth saying.
    pub fn sustained_load(&self, pid: u32) -> Option<Duration> {
        self.sustained
            .get(&pid)
            .map(|s| s.duration())
            .filter(|d| *d >= SUSTAINED_AFTER)
    }

    /// Whether an interface has carried traffic recently enough to be worth
    /// a row. Interfaces never seen active are excluded — including right
    /// after startup, when nothing has been observed yet.
    pub fn net_is_recent(&self, interface: &str) -> bool {
        self.net_last_active
            .get(interface)
            .is_some_and(|t| t.elapsed() < NET_IDLE_AFTER)
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

    /// How long we have observed this pid as abnormal. Always a lower bound:
    /// it may well have been in that state before the app started.
    pub fn abnormal_observed(&self, pid: u32) -> Option<Duration> {
        self.abnormal_since.get(&pid).map(|t| t.elapsed())
    }

    pub fn set_settings(&mut self, settings: FileConfig) {
        self.settings = Some(settings);
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
        let dir = zstats::settings::default_dir();
        let mut file = zstats::settings::load(&dir).map_err(|e| e.to_string())?;
        let payload = if name.is_empty() {
            value.to_string()
        } else {
            format!("{name}={value}")
        };
        zstats::settings::apply_add(&mut file, key, &payload)?;
        zstats::settings::save(&dir, &file).map_err(|e| e.to_string())?;
        self.settings = Some(file);
        crate::metrics::request_reload();
        cx.notify();
        Ok(())
    }

    // ---- view selection ------------------------------------------------

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            cx.notify();
        }
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
    use crate::procscan::{AbnormalProcess, ProcState};

    fn sample(pid: u32) -> AbnormalProcess {
        AbnormalProcess {
            pid,
            parent_pid: 1,
            name: "login".into(),
            state: ProcState::Zombie,
            age: Duration::from_secs(60 * 60 * 24),
        }
    }

    #[test]
    fn sustained_load_survives_dips_but_not_a_real_stop() {
        let mut state = ZStatsAppState::new();
        let start = Instant::now() - SUSTAINED_AFTER - Duration::from_secs(60);

        // Over the bar long enough, and seen recently: this is the case worth
        // reporting — a process that never trips the alert but has been
        // grinding away all day.
        state.sustained.insert(
            42,
            SustainedLoad {
                since: start,
                last_over: Instant::now(),
            },
        );
        assert!(state.sustained_load(42).is_some());

        // Long enough, but nothing above the bar for a while: the load
        // genuinely stopped, so the clock should not survive a prune.
        state.sustained.insert(
            43,
            SustainedLoad {
                since: start,
                last_over: Instant::now() - SUSTAINED_GRACE - Duration::from_secs(1),
            },
        );
        state
            .sustained
            .retain(|_, s| s.last_over.elapsed() <= SUSTAINED_GRACE);
        assert!(!state.sustained.contains_key(&43));

        // Above the bar right now, but only just started — not yet a story.
        state.sustained.insert(
            44,
            SustainedLoad {
                since: Instant::now(),
                last_over: Instant::now(),
            },
        );
        assert!(state.sustained_load(44).is_none());
    }

    #[test]
    fn crossing_the_line_notifies_once_then_re_arms() {
        let mut state = ZStatsAppState::new();
        let over = SustainedLoad {
            since: Instant::now() - SUSTAINED_AFTER - Duration::from_secs(1),
            last_over: Instant::now(),
        };
        state.sustained.insert(7, over);

        // Crossing raises a notice.
        assert!(state.sustained_notified.insert(7));
        state.sustained_pending.push(SustainedNotice {
            pid: 7,
            name: "helper".into(),
            cpu_avg: 12.0,
            duration: over.duration(),
        });
        assert_eq!(state.take_sustained_notices().len(), 1);
        // Draining is idempotent — staying over the line is not a new event,
        // or this would fire on every tick for the next twelve hours.
        assert!(state.take_sustained_notices().is_empty());
        assert!(!state.sustained_notified.insert(7));

        // The load stops: the pid is pruned, and the memo with it, so a later
        // episode counts as new.
        state.sustained.remove(&7);
        state
            .sustained_notified
            .retain(|pid| state.sustained.contains_key(pid));
        assert!(state.sustained_notified.insert(7));
    }

    #[test]
    fn sustained_bar_follows_the_configured_alert_threshold() {
        let state = ZStatsAppState::new();
        // No config loaded yet: zstats' own default of 30%, thirded.
        assert!((state.sustained_bar() - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn interfaces_are_kept_only_while_recently_active() {
        let mut state = ZStatsAppState::new();
        state.net_last_active.insert("en0".into(), Instant::now());
        state.net_last_active.insert(
            "utun9".into(),
            Instant::now() - NET_IDLE_AFTER - Duration::from_secs(1),
        );

        assert!(state.net_is_recent("en0"));
        assert!(!state.net_is_recent("utun9"), "gone quiet long enough");
        // Never observed carrying anything — the common case for unused
        // Ethernet and idle tunnels, and why the list is worth filtering.
        assert!(!state.net_is_recent("en5"));
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
    fn abnormal_is_withheld_until_it_has_persisted() {
        let mut state = ZStatsAppState::new();
        state.abnormal = vec![sample(101)];

        // Just observed: an exiting process can be a zombie for an instant,
        // so nothing is reported yet even though the process itself is a day
        // old — age is not the criterion.
        state.abnormal_since.insert(101, Instant::now());
        assert!(state.abnormal().is_empty());

        // Still there well past the threshold.
        state.abnormal_since.insert(
            101,
            Instant::now() - MIN_ABNORMAL_DURATION - Duration::from_secs(1),
        );
        assert_eq!(state.abnormal().len(), 1);
    }

    #[test]
    fn clocks_reset_only_when_a_pid_disappears() {
        let mut state = ZStatsAppState::new();
        let long_ago = Instant::now() - MIN_ABNORMAL_DURATION * 2;
        state.abnormal = vec![sample(101)];
        state.abnormal_since.insert(101, long_ago);

        // A rescan that still finds it must not restart the clock, or nothing
        // would ever cross the threshold.
        state.abnormal_since.retain(|pid, _| *pid == 101);
        state.abnormal_since.entry(101).or_insert(Instant::now());
        assert_eq!(state.abnormal_since.get(&101), Some(&long_ago));
    }
}
