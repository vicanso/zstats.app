//! Embedded metrics collection.
//!
//! Collection runs for the life of the process, deliberately independent of
//! any window. `Monitor` accumulates the previous-sample baselines that disk,
//! network and per-process IO rates are diffed against, so tearing it down
//! with the popover would reset every rate to "unknown" on each reopen.

use crate::state::ZStatsGlobalStore;
#[cfg(not(target_os = "linux"))]
use crate::tray;
use gpui::App;
use std::time::Duration;
use zstats::Monitor;

/// Sampling cadence. The design labels the panel "sample 1 s".
///
/// This is only how often we *ask*; `LocalCollector` applies each subsystem's
/// own interval from config.toml internally, so a 1s call does not mean the
/// process table is walked every second.
const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Spawn the collector and the task that folds its output into the store.
pub fn start(cx: &mut App) {
    let dir = zstats::settings::default_dir();

    // Read the config once, for the read-only Config tab. Doing it on the UI
    // thread is fine — one small TOML file — and it means the tab has content
    // before the first sample lands.
    match zstats::settings::load(&dir) {
        Ok(settings) => cx
            .global::<ZStatsGlobalStore>()
            .clone()
            .update(cx, |state, _| state.set_settings(settings)),
        Err(e) => eprintln!("could not read {}/config.toml: {e}", dir.display()),
    }

    let (tx, rx) = smol::channel::unbounded::<zstats::Tick>();

    // `tick()` is a pile of syscalls and blocks — keep it off the UI thread.
    std::thread::spawn(move || {
        // Shared with the zstats CLI on purpose: same config.toml, same
        // thresholds, same history. See README about running `zstats serve`
        // at the same time.
        let mut monitor = match Monitor::new(&dir) {
            Ok(monitor) => monitor,
            Err(e) => {
                eprintln!("metrics collection unavailable ({}): {e}", dir.display());
                return;
            }
        };
        loop {
            match monitor.tick() {
                Ok(tick) => {
                    if tx.send_blocking(tick).is_err() {
                        return; // receiver dropped — the app is going away
                    }
                }
                // One failed sample shouldn't end sampling.
                Err(e) => eprintln!("collect failed: {e}"),
            }
            std::thread::sleep(TICK_INTERVAL);
        }
    });

    cx.spawn(async move |cx| {
        while let Ok(tick) = rx.recv().await {
            cx.update(|cx| {
                #[cfg(not(target_os = "linux"))]
                tray::set_cpu_title(cx, tick.snapshot.cpu.usage_percent);

                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.ingest(tick, cx));
            });
        }
    })
    .detach();
}
