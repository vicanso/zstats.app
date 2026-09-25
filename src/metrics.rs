//! Embedded metrics collection.
//!
//! Collection runs for the life of the process, deliberately independent of
//! any window. `Monitor` accumulates the previous-sample baselines that disk,
//! network and per-process IO rates are diffed against, so tearing it down
//! with the popover would reset every rate to "unknown" on each reopen.
//!
//! GPUs and physical drives are the exception to "collect for the life of
//! the process". zstats reads both through an `ioreg` child process, and at
//! this app's cadence one read costs ~45 ms of CPU (both ~66 ms, measured
//! 2026-09-25 on an M4 Pro): at zstats' 10s default that is ~0.66% of a
//! core, about what the whole app costs with the panel hidden. Nothing reads
//! those figures while the panel is hidden, so the collector thread switches
//! both channels on only while the panel is on screen *and* showing a tab
//! that displays them ([`tab_reads_registry`]), through zstats' runtime
//! switch — a rebuild would reset every other channel's rate baseline.

use crate::notify;
use crate::prefs;
use crate::procscan;
use crate::state::{Tab, ZStatsGlobalStore};
use crate::tray;
use gpui::{App, Global};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use zstats::settings::FileConfig;
use zstats::{CollectorConfig, Monitor};

/// Set by the Alerts tab after it writes `[alerts]` overrides. The
/// collector thread consumes it on the next loop so windows and
/// cooldowns stay intact (`Monitor::reload_settings`).
static RELOAD: AtomicBool = AtomicBool::new(false);

/// Set when `[collector]` (or `[daemon] interval`) changes. Those are
/// baked into `LocalCollector` at construction, so the only honest
/// apply is a new `Monitor`. Rate baselines start over; the first
/// sample after a rebuild legitimately reads `—`.
static REBUILD: AtomicBool = AtomicBool::new(false);

/// Ask the collector to re-read `[alerts]` on its next pass.
pub fn request_reload() {
    RELOAD.store(true, Ordering::Release);
}

/// Ask the collector thread to throw away the running `Monitor` and
/// build another from the file.
pub fn request_rebuild() {
    REBUILD.store(true, Ordering::Release);
}

/// App defaults for channels the Config tab exposes as a cadence.
/// zstats itself uses 0 (every tick); 0 in the file therefore means
/// "this app's default", not "hammer the process table every 2s".
pub(crate) const PANEL_PROCESS_INTERVAL: Duration = Duration::from_secs(15);
pub(crate) const PANEL_DISK_IO_INTERVAL: Duration = Duration::from_secs(15);
pub(crate) const PANEL_NETWORK_INTERVAL: Duration = Duration::from_secs(15);

/// GPU and drive cadence. Those channels run only while a tab showing
/// them is on screen ([`registry_channels`]), so this is a live view's
/// cadence — zstats' own foreground view reads both every beat — not the
/// daemon's 10s the file's default is written for. At 5s the GPU gauge
/// stays within a few seconds of true, and drive rates reach the screen
/// one refresh after the tab opens (the switch restarts their baseline;
/// at the visible 2s tick that is ~6s, where 10s left the tab showing
/// `—` for ten). Cost while someone is looking: one ~66 ms pair of
/// `ioreg` reads per refresh, ~1.1% of a core. Applied over whatever the
/// file says, unlike the other cadences — the file's value here is
/// zstats' non-zero default, indistinguishable from a deliberate one.
pub(crate) const PANEL_REGISTRY_INTERVAL: Duration = Duration::from_secs(5);

/// Sensors, per-core CPU, battery, process groups and process-disk-io
/// have no off switch. CPU% and memory are already unconditional in
/// zstats. A zero cadence in the file is this app's 15s default.
///
/// GPU and drives keep the file's on/off: the collector thread switches
/// them per pass ([`registry_channels`]), and the file's value is the
/// ceiling it reads from `Monitor::settings`. A `collect-gpu false`
/// written with the CLI must stay off here too. Their cadence is ours
/// ([`PANEL_REGISTRY_INTERVAL`]).
fn with_always_on(mut settings: FileConfig) -> FileConfig {
    let mut collector = settings.collector.unwrap_or_default();
    collector.collect_temperatures = true;
    collector.collect_battery = true;
    collector.per_core_cpu = true;
    collector.collect_processes = true;
    collector.collect_process_groups = true;
    collector.collect_process_disk_io = true;
    collector.collect_disks = true;
    collector.collect_networks = true;
    collector.process_refresh_interval =
        panel_interval(collector.process_refresh_interval, PANEL_PROCESS_INTERVAL);
    collector.disk_io_refresh_interval =
        panel_interval(collector.disk_io_refresh_interval, PANEL_DISK_IO_INTERVAL);
    collector.network_refresh_interval =
        panel_interval(collector.network_refresh_interval, PANEL_NETWORK_INTERVAL);
    collector.gpu_refresh_interval = PANEL_REGISTRY_INTERVAL;
    collector.drive_refresh_interval = PANEL_REGISTRY_INTERVAL;
    settings.collector = Some(collector);
    settings
}

/// `0` in config.toml is zstats' "every collect". This panel treats that
/// as unset and substitutes its own default.
pub(crate) fn panel_interval(file: Duration, fallback: Duration) -> Duration {
    if file.is_zero() { fallback } else { file }
}

/// Fallback cadence, used only when config.toml sets no `[daemon] interval`.
/// Matches zstats' own builtin default, so the app and the CLI agree.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(2);

/// Cadence while the panel is closed. The expensive part of a sample is
/// the full process-table walk plus the process-group tree aggregation
/// (already on its own 15s wall clock), and with the panel closed nobody
/// is reading that. CPU% and memory still feed the tray, so this cannot
/// be the process cadence — at 15s the title reads as frozen.
///
/// Not gated on load: a compile used to pin the 2s interval the whole
/// time the panel was hidden, for ~0.6% of one core (2s = 1.6%, 5s =
/// 1.0%) on a machine that is already busy. The tray is the only reader
/// then, and 5s is the line where it does not look stuck.
const IDLE_INTERVAL: Duration = Duration::from_secs(5);

/// How often to sweep for abnormal processes.
///
/// Far cheaper than a metrics sample — one `sysctl` on macOS, one `/proc`
/// walk on Linux, no per-process CPU/memory accounting — and what it looks
/// for changes on the scale of minutes or days, not seconds.
const ABNORMAL_SCAN_INTERVAL: Duration = Duration::from_secs(30);

/// Shared "the panel is on screen" flag, plus a way to wake the collector.
///
/// Two things hang off visibility: the sampling cadence, and whether a landing
/// tick repaints. The window is never destroyed now — it is ordered off screen
/// — so gpui has no idea it is invisible and would happily keep rendering it.
#[derive(Clone)]
pub struct CollectorPace {
    visible: Arc<AtomicBool>,
    /// The selected tab displays GPU or drive figures. Kept apart from
    /// `visible` so the seven hide paths need not know about it: the
    /// collector reads the two together, and a hidden panel wants
    /// nothing whatever its tab.
    registry_tab: Arc<AtomicBool>,
    wake: mpsc::Sender<()>,
}

impl Global for CollectorPace {}

impl CollectorPace {
    /// The panel came on screen: sample now, and hold the fast cadence.
    ///
    /// Sampling immediately matters — the collector may be seconds into an
    /// idle wait when the tray is clicked, and opening onto stale numbers
    /// would read as broken. That tick always refreshes whole-machine CPU
    /// (the tray / Processor headline). Process trees stay on their own
    /// cadence ([`PANEL_PROCESS_INTERVAL`]): busting zstats' cache means
    /// rebuilding the collector, and the first sample after that is `—`.
    /// Overview's copy says so, rather than substituting a second listing.
    pub fn shown(&self) {
        self.visible.store(true, Ordering::Relaxed);
        let _ = self.wake.send(());
    }

    /// The panel went off screen: back to the idle cadence.
    pub fn hidden(&self) {
        self.visible.store(false, Ordering::Relaxed);
    }

    /// Whether a landing tick should trigger a repaint.
    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }

    /// Interrupt an idle wait so a just-written setting is picked up
    /// on the next loop, not up to [`IDLE_INTERVAL`] later.
    pub fn wake(&self) {
        let _ = self.wake.send(());
    }

    /// The selected tab changed. Switching *to* a tab that shows GPUs or
    /// drives while the panel is up wakes the collector, so the GPU gauge
    /// is there on the first paint rather than a visible tick later.
    pub fn tab_selected(&self, tab: Tab) {
        let reads = tab_reads_registry(tab);
        let was = self.registry_tab.swap(reads, Ordering::Relaxed);
        if reads && !was && self.is_visible() {
            let _ = self.wake.send(());
        }
    }
}

/// The tabs that display GPU or drive figures — the one place that
/// decides when `ioreg` runs, so adding those figures to another tab is
/// a change here and nowhere else.
pub(crate) fn tab_reads_registry(tab: Tab) -> bool {
    tab == Tab::Hardware
}

/// What the two `ioreg` channels should do this pass: (gpu, drives).
///
/// On only while the panel is on screen at a tab that shows them, and
/// never past the file's own `collect-gpu` / `collect-drives`. zstats'
/// switch is a no-op on a repeat, so applying this every pass costs two
/// comparisons; turning drives on restarts their rates from no baseline,
/// which is the honest answer after a stretch nobody was sampling.
fn registry_channels(on_screen: bool, file: Option<&CollectorConfig>) -> (bool, bool) {
    let gpu = file.is_none_or(|c| c.collect_gpu);
    let drives = file.is_none_or(|c| c.collect_drives);
    (on_screen && gpu, on_screen && drives)
}

/// Spawn the collector and the task that folds its output into the store.
pub fn start(cx: &mut App) {
    let dir = zstats::settings::default_dir();

    // Read the config once: it seeds the Config tab and the sampling
    // cadence. Later writes go through `apply_setting` → rebuild / reload.
    // Sharing ~/.zstats with the CLI means sharing its `[daemon] interval`
    // too — running at our own rate would have the two processes disagree
    // about a setting the user wrote down once.
    let mut interval = DEFAULT_INTERVAL;
    match zstats::settings::load(&dir) {
        Ok(settings) => {
            interval = settings.daemon.interval.unwrap_or(DEFAULT_INTERVAL);
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, _| state.set_settings(settings));
        }
        Err(e) => tracing::error!("could not read {}/config.toml: {e}", dir.display()),
    }

    let visible = Arc::new(AtomicBool::new(false));
    // Seeded from the store: the session's tab was restored before this
    // global existed, so `tab_selected` never saw it.
    let registry_tab = Arc::new(AtomicBool::new(tab_reads_registry(
        cx.global::<ZStatsGlobalStore>().read(cx).tab(),
    )));
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    cx.set_global(CollectorPace {
        visible: visible.clone(),
        registry_tab: registry_tab.clone(),
        wake: wake_tx,
    });

    let (tx, rx) = smol::channel::unbounded::<zstats::Tick>();

    // `tick()` is a pile of syscalls and blocks — keep it off the UI thread.
    thread::spawn(move || {
        // Shared with the zstats CLI on purpose: same config.toml, same
        // thresholds, same history. See docs/design.md about running `zstats serve`
        // at the same time.
        // `with_settings` is fallible since zstats 0.4: it also reads
        // `<config-dir>/template.toml`, and a template that failed to load
        // would be a rule set that silently did not apply — so refusing to
        // collect is the correct posture, same as a malformed config.toml.
        let mut monitor = match zstats::settings::load(&dir)
            .and_then(|settings| Monitor::with_settings(&dir, with_always_on(settings)))
        {
            Ok(monitor) => monitor,
            Err(e) => {
                tracing::error!("metrics collection unavailable ({}): {e}", dir.display());
                return;
            }
        };
        loop {
            if REBUILD.swap(false, Ordering::AcqRel) {
                // Rebuild wins over a pending reload: a new Monitor
                // already re-reads [alerts] from the file.
                let _ = RELOAD.swap(false, Ordering::AcqRel);
                match zstats::settings::load(&dir) {
                    Ok(settings) => {
                        interval = settings.daemon.interval.unwrap_or(DEFAULT_INTERVAL);
                        match Monitor::with_settings(&dir, with_always_on(settings)) {
                            Ok(next) => monitor = next,
                            Err(e) => tracing::error!("rebuild collector failed: {e}"),
                        }
                    }
                    Err(e) => tracing::error!("rebuild collector failed: {e}"),
                }
            } else if RELOAD.swap(false, Ordering::AcqRel)
                && let Err(e) = monitor.reload_settings()
            {
                tracing::error!("reload_settings failed: {e}");
            }
            // Every pass, and after the rebuild above: a fresh Monitor
            // comes up with the file's flags, which are on by default.
            let on_screen = visible.load(Ordering::Relaxed) && registry_tab.load(Ordering::Relaxed);
            let (gpu, drives) = registry_channels(on_screen, monitor.settings().collector.as_ref());
            monitor.set_collect_gpu(gpu);
            monitor.set_collect_drives(drives);
            match monitor.tick() {
                Ok(tick) => {
                    if tx.send_blocking(tick).is_err() {
                        return; // receiver dropped — the app is going away
                    }
                }
                // One failed sample shouldn't end sampling.
                Err(e) => tracing::warn!("collect failed: {e}"),
            }
            let wait = if visible.load(Ordering::Relaxed) {
                interval
            } else {
                IDLE_INTERVAL
            };
            match wake_rx.recv_timeout(wait) {
                Ok(()) => {
                    // Woken early. Drain the backlog so a burst of opens
                    // doesn't become a burst of samples.
                    while wake_rx.try_recv().is_ok() {}
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });

    spawn_abnormal_scan(cx);

    cx.spawn(async move |cx| {
        while let Ok(tick) = rx.recv().await {
            cx.update(|cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| {
                        let now = Instant::now();
                        // The user's master switch, ahead of the
                        // per-episode gates: while banners are off
                        // nothing interrupts, so neither the snooze
                        // clock nor the auto-quiet window should
                        // record a delivery that never happened.
                        let muted = !prefs::notifications();
                        for event in state.ingest(tick, cx) {
                            // Quiet subjects still land in the Alerts list
                            // above — only the banner stays quiet. Three
                            // gates: the global switch, the snooze the
                            // user asked for, and the auto-quiet for a
                            // subject that has already interrupted twice
                            // this hour.
                            //
                            // Each verdict is logged: a banner that
                            // silently stayed away is indistinguishable
                            // from a rule that stopped firing (the same
                            // reason the card wears a pill), and the log
                            // is where that question gets answered a day
                            // later.
                            let snoozed = !muted && state.banner_snoozed(&event);
                            let damped = !muted && !snoozed && state.banner_damped(&event, now);
                            tracing::info!(
                                kind = ?event.kind(),
                                subject = ?event.subject,
                                banner = if muted {
                                    "muted"
                                } else if snoozed {
                                    "snoozed"
                                } else if damped {
                                    "auto-quieted"
                                } else {
                                    "delivered"
                                },
                                "alert reported"
                            );
                            if muted || snoozed || damped {
                                continue;
                            }
                            notify::post(&event);
                        }
                        for notice in state.take_sustained_notices() {
                            tracing::info!(
                                pid = notice.pid,
                                name = %notice.name,
                                banner = if muted { "muted" } else { "delivered" },
                                "sustained-load notice"
                            );
                            if !muted {
                                notify::post_sustained(&notice);
                            }
                        }
                        for creep in state.take_memory_creep_notices() {
                            tracing::info!(
                                name = %creep.name,
                                climb_bytes = creep.climb_bytes,
                                now_bytes = creep.now_bytes,
                                banner = if muted { "muted" } else { "delivered" },
                                "memory creep notice"
                            );
                            if !muted {
                                notify::post_memory_creep(&creep);
                            }
                        }
                        // After ingest, not before: the tray's auto
                        // mode reads the episode list this tick just
                        // merged into, and a memory alert should turn
                        // the face on the sample that reported it.
                        tray::sync(cx, state);
                    });
            });
        }
    })
    .detach();
}

/// Sweep for zombie / stopped processes on its own cadence.
///
/// Separate from the metrics collector on purpose: zstats keeps only the top N
/// processes by CPU then memory, and an abnormal process scores near zero on
/// both — on this machine they ranked 435th and below, so they can never
/// appear in the panel's process table.
fn spawn_abnormal_scan(cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            // `scan` walks the whole process table, so keep it off the UI
            // thread even though it is cheap.
            let found = cx
                .background_executor()
                .spawn(async { procscan::scan() })
                .await;
            // `update` returns `()` in this gpui pin; a dropped app simply
            // stops polling this task. A failed scan is not "no zombies":
            // skip the replace so observation clocks keep running.
            if let Some(found) = found {
                cx.update(|cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.set_abnormal(found, cx));
                });
            }
            cx.background_executor().timer(ABNORMAL_SCAN_INTERVAL).await;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_reads_run_only_on_screen_and_never_past_the_file() {
        let off = CollectorConfig {
            collect_gpu: false,
            collect_drives: false,
            ..CollectorConfig::default()
        };
        let gpu_only = CollectorConfig {
            collect_drives: false,
            ..CollectorConfig::default()
        };
        // Hidden, or on a tab that shows neither: nothing, whatever the file
        assert_eq!(registry_channels(false, None), (false, false));
        assert_eq!(
            registry_channels(false, Some(&CollectorConfig::default())),
            (false, false)
        );
        // On screen: zstats' defaults, or the file's own ceiling
        assert_eq!(registry_channels(true, None), (true, true));
        assert_eq!(
            registry_channels(true, Some(&CollectorConfig::default())),
            (true, true)
        );
        assert_eq!(registry_channels(true, Some(&gpu_only)), (true, false));
        assert_eq!(registry_channels(true, Some(&off)), (false, false));
    }

    #[test]
    fn the_file_ceiling_for_gpu_and_drives_survives_the_always_on_pass() {
        let file = with_always_on(FileConfig {
            collector: Some(CollectorConfig {
                collect_gpu: false,
                collect_drives: false,
                ..CollectorConfig::default()
            }),
            ..FileConfig::default()
        });
        let c = file.collector.unwrap();
        assert!(!c.collect_gpu, "a CLI `collect-gpu false` must stay off");
        assert!(!c.collect_drives);
        // And the default stays zstats' on, for the per-pass switch to
        // bring down while nobody is looking
        let c = with_always_on(FileConfig::default()).collector.unwrap();
        assert!(c.collect_gpu && c.collect_drives);
    }

    #[test]
    fn only_hardware_reads_the_registry() {
        for tab in Tab::ALL {
            assert_eq!(tab_reads_registry(tab), tab == Tab::Hardware, "{tab:?}");
        }
    }

    #[test]
    fn selecting_a_registry_tab_wakes_the_collector_only_when_it_matters() {
        let (wake, woken) = mpsc::channel();
        let pace = CollectorPace {
            visible: Arc::new(AtomicBool::new(false)),
            registry_tab: Arc::new(AtomicBool::new(false)),
            wake,
        };
        // Hidden: remembered, but nobody is waiting on a sample
        pace.tab_selected(Tab::Hardware);
        assert!(pace.registry_tab.load(Ordering::Relaxed));
        assert!(woken.try_recv().is_err());

        pace.tab_selected(Tab::Overview);
        assert!(!pace.registry_tab.load(Ordering::Relaxed));
        pace.visible.store(true, Ordering::Relaxed);
        pace.tab_selected(Tab::Hardware);
        assert!(woken.try_recv().is_ok(), "on screen: the gauge is due now");
        // Already there: no second wake for the same state
        pace.tab_selected(Tab::Hardware);
        assert!(woken.try_recv().is_err());
    }

    #[test]
    fn zero_cadence_in_the_file_becomes_the_panel_default() {
        let file = with_always_on(FileConfig::default());
        let c = file.collector.unwrap();
        assert_eq!(c.process_refresh_interval, PANEL_PROCESS_INTERVAL);
        assert_eq!(c.disk_io_refresh_interval, PANEL_DISK_IO_INTERVAL);
        assert_eq!(c.network_refresh_interval, PANEL_NETWORK_INTERVAL);
        assert_eq!(c.gpu_refresh_interval, PANEL_REGISTRY_INTERVAL);
        assert_eq!(c.drive_refresh_interval, PANEL_REGISTRY_INTERVAL);
        assert!(c.collect_processes);
        assert!(c.collect_process_groups);
        assert!(c.collect_process_disk_io);
        assert!(c.collect_disks);
        assert!(c.collect_networks);
        assert!(c.collect_temperatures);
        assert!(c.collect_battery);
        assert!(c.per_core_cpu);
    }

    #[test]
    fn an_explicit_cadence_is_kept() {
        let file = with_always_on(FileConfig {
            collector: Some(CollectorConfig {
                process_refresh_interval: Duration::from_secs(5),
                disk_io_refresh_interval: Duration::from_secs(30),
                network_refresh_interval: Duration::from_secs(10),
                ..CollectorConfig::default()
            }),
            ..FileConfig::default()
        });
        let c = file.collector.unwrap();
        assert_eq!(c.process_refresh_interval, Duration::from_secs(5));
        assert_eq!(c.disk_io_refresh_interval, Duration::from_secs(30));
        assert_eq!(c.network_refresh_interval, Duration::from_secs(10));
    }
}
