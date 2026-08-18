//! Embedded metrics collection.
//!
//! Collection runs for the life of the process, deliberately independent of
//! any window. `Monitor` accumulates the previous-sample baselines that disk,
//! network and per-process IO rates are diffed against, so tearing it down
//! with the popover would reset every rate to "unknown" on each reopen.

use crate::notify;
use crate::procscan;
use crate::state::ZStatsGlobalStore;
#[cfg(not(target_os = "linux"))]
use crate::tray;
use gpui::{App, Global};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use zstats::Monitor;
use zstats::settings::FileConfig;

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

/// Sensors, per-core CPU, battery, process groups and process-disk-io
/// have no off switch. CPU% and memory are already unconditional in
/// zstats. A zero cadence in the file is this app's 15s default.
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

/// Cadence while the panel is closed and the machine is quiet. The expensive
/// part of a sample is the full process-table walk plus the process-group
/// tree aggregation, and with the panel closed nobody is reading it.
///
/// Not longer than this because the tray title rides on the same tick and is
/// the only thing still visible with the panel closed — at 15s it reads as
/// frozen. Measured idle cost: 2s = 1.6%, 5s = 1.0%, 15s = 0.3%; 5s is where
/// the curve bends without the number going stale.
const IDLE_INTERVAL: Duration = Duration::from_secs(5);

/// Overall CPU at or above which the machine counts as busy, holding the fast
/// cadence even with the panel closed.
///
/// This can only react to *sustained* load. CPU percent is the average
/// between two refreshes, so at [`IDLE_INTERVAL`] a 3-second spike is
/// flattened across the whole 5 seconds and may never reach the bar — the
/// mechanism suppresses its own trigger. Compilations and encodes are caught;
/// brief spikes are not.
const BUSY_CPU_PERCENT: f32 = 30.0;

/// How often to sweep for abnormal processes.
///
/// Far cheaper than a metrics sample — one `sysctl` and a scan of the result,
/// no per-process CPU/memory accounting — and what it looks for changes on the
/// scale of minutes or days, not seconds.
#[cfg(target_os = "macos")]
const ABNORMAL_SCAN_INTERVAL: Duration = Duration::from_secs(30);

/// Shared "the panel is on screen" flag, plus a way to wake the collector.
///
/// Two things hang off visibility: the sampling cadence, and whether a landing
/// tick repaints. The window is never destroyed now — it is ordered off screen
/// — so gpui has no idea it is invisible and would happily keep rendering it.
#[derive(Clone)]
pub struct CollectorPace {
    visible: Arc<AtomicBool>,
    wake: mpsc::Sender<()>,
}

impl Global for CollectorPace {}

impl CollectorPace {
    /// The panel came on screen: sample now, and hold the fast cadence.
    ///
    /// Sampling immediately matters — the collector may be seconds into an
    /// idle wait when the tray is clicked, and opening onto stale numbers
    /// would read as broken.
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
        Err(e) => eprintln!("could not read {}/config.toml: {e}", dir.display()),
    }

    let visible = Arc::new(AtomicBool::new(false));
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    cx.set_global(CollectorPace {
        visible: visible.clone(),
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
                eprintln!("metrics collection unavailable ({}): {e}", dir.display());
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
                            Err(e) => eprintln!("rebuild collector failed: {e}"),
                        }
                    }
                    Err(e) => eprintln!("rebuild collector failed: {e}"),
                }
            } else if RELOAD.swap(false, Ordering::AcqRel)
                && let Err(e) = monitor.reload_settings()
            {
                eprintln!("reload_settings failed: {e}");
            }
            // Derived fresh every round rather than carried across: a failed
            // sample says nothing about load, and a stale `true` would hold
            // the fast cadence indefinitely on a collector that has stopped
            // returning anything to be busy about.
            let busy = match monitor.tick() {
                Ok(tick) => {
                    let busy = tick.snapshot.cpu.usage_percent >= BUSY_CPU_PERCENT;
                    if tx.send_blocking(tick).is_err() {
                        return; // receiver dropped — the app is going away
                    }
                    busy
                }
                // One failed sample shouldn't end sampling.
                Err(e) => {
                    eprintln!("collect failed: {e}");
                    false
                }
            };
            let wait = if busy || visible.load(Ordering::Relaxed) {
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

    #[cfg(target_os = "macos")]
    spawn_abnormal_scan(cx);

    cx.spawn(async move |cx| {
        while let Ok(tick) = rx.recv().await {
            cx.update(|cx| {
                #[cfg(not(target_os = "linux"))]
                tray::set_cpu_title(cx, tick.snapshot.cpu.usage_percent);

                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| {
                        let now = Instant::now();
                        for event in state.ingest(tick, cx) {
                            // Quiet subjects still land in the Alerts list
                            // above — only the banner stays quiet. Two
                            // gates: the snooze the user asked for, and
                            // the auto-quiet for a subject that has
                            // already interrupted twice this hour.
                            if state.banner_snoozed(&event) || state.banner_damped(&event, now) {
                                continue;
                            }
                            notify::post(&event);
                        }
                        for notice in state.take_sustained_notices() {
                            notify::post_sustained(&notice);
                        }
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
#[cfg(target_os = "macos")]
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
            // stops polling this task.
            cx.update(|cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.set_abnormal(found, cx));
            });
            cx.background_executor().timer(ABNORMAL_SCAN_INTERVAL).await;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use zstats::CollectorConfig;

    #[test]
    fn zero_cadence_in_the_file_becomes_the_panel_default() {
        let file = with_always_on(FileConfig::default());
        let c = file.collector.unwrap();
        assert_eq!(c.process_refresh_interval, PANEL_PROCESS_INTERVAL);
        assert_eq!(c.disk_io_refresh_interval, PANEL_DISK_IO_INTERVAL);
        assert_eq!(c.network_refresh_interval, PANEL_NETWORK_INTERVAL);
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
