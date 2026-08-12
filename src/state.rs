//! Application-level state.
//!
//! The window is only a rendering layer: it gets closed and rebuilt whenever
//! the tray has to reposition it (gpui can't move an existing window), so
//! anything that has to survive a close → reopen round trip belongs here, not
//! in the root view. Collected metrics are the main tenant — sampling runs
//! whether or not a window exists.

use crate::i18n;
use gpui::{Bounds, Context, Entity, Global, Pixels};
use std::collections::VecDeque;
use std::ops::Deref;
use std::time::{Duration, Instant};
use zstats::settings::FileConfig;
use zstats::{AlertEvent, Tick};

/// How many past alerts the Alerts tab can show.
const MAX_ALERTS: usize = 20;

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
    settings: Option<FileConfig>,
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
            settings: None,
        }
    }
}

impl ZStatsAppState {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- metrics -------------------------------------------------------

    /// Fold one collection round into the state.
    pub fn ingest(&mut self, tick: Tick, cx: &mut Context<Self>) {
        let now = Instant::now();
        for event in &tick.alerts {
            self.alerts.push_front(SeenAlert {
                at: now,
                event: event.clone(),
            });
        }
        while self.alerts.len() > MAX_ALERTS {
            self.alerts.pop_back();
        }

        self.latest = Some(tick);
        cx.notify();
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

    pub fn set_settings(&mut self, settings: FileConfig) {
        self.settings = Some(settings);
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
