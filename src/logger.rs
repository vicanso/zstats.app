//! Tracing to stdout and a daily-rolling file — the zedis logger,
//! adapted to the shared `~/.zstats`.
//!
//! Until this existed the app had ~30 bare `eprintln!`s and no
//! subscriber at all. Under LaunchServices stderr goes nowhere a person
//! looks, so every one of those lines — a failed collect, an
//! undelivered quit request, a refused template — vanished; and the
//! zstats crate *emits* tracing events (scheduler, alert engine,
//! records) that were being dropped on the floor for want of a
//! subscriber. `make debug`'s `RUST_LOG=debug` finally does something.
//!
//! What lands here and why it is worth a file: the alert-class trail —
//! every reported `AlertEvent` with its delivery verdict (banner shown,
//! snoozed, auto-quieted), every quit/term request the app delivers,
//! every move-to-Trash — is exactly the record a "why did/didn't it
//! tell me" question needs, and none of it is reconstructable later.
//! `alerts.toml` mirrors the *current* episode list for restoring the
//! tab; the log is the append-only account of what happened.
//!
//! The directory is `~/.zstats/logs/`, shared with the CLI like the
//! rest of the config dir, so the file name carries the writer:
//! `zstats-app.log.YYYY-MM-DD` — the same reason the clean-hints file
//! name carries the platform.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime};
use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{filter::LevelFilter, layer::SubscriberExt, util::SubscriberInitExt};

/// Rolling file name prefix; the appender appends `.YYYY-MM-DD`.
///
/// That date is **UTC** — `tracing-appender` rolls on
/// `OffsetDateTime::now_utc()` with no local option — so at UTC+8 the
/// lines written between midnight and 08:00 land in the previous day's
/// file, and the file turns over at 08:00 local. The line timestamps
/// *inside* are local (`OffsetTime::local_rfc_3339` below): search by
/// content time, not by file name. Not worth a hand-rolled roller —
/// same trade zedis made.
const FILE_PREFIX: &str = "zstats-app.log";

/// Delete our rolling files older than ~3 months so the shared logs
/// directory does not grow without bound. Best-effort: any error is
/// ignored — this runs **at startup only**, never on a timer: a
/// resident that goes unrestarted for months skips its pruning, and
/// that is fine at kilobytes a day with the file count capped near 90.
/// Age is judged by mtime, not the (UTC) file-name date, so the
/// timezone quirk above cannot mis-age a file. Only files with our own
/// prefix are touched: the directory is shared.
const MAX_LOG_AGE: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// `~/.zstats/logs/`, created if missing. `None` when it cannot be
/// created — file logging is then skipped, stdout still works.
pub fn logs_dir() -> Option<PathBuf> {
    let dir = zstats::settings::default_dir().join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn prune_old_logs(dir: &Path) {
    let Some(cutoff) = SystemTime::now().checked_sub(MAX_LOG_AGE) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let ours = entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(FILE_PREFIX));
        if !ours {
            continue;
        }
        if let Ok(modified) = entry.metadata().and_then(|m| m.modified())
            && modified < cutoff
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Install the subscriber: stdout always, the rolling file when the
/// directory can be made. INFO by default, `RUST_LOG` overrides (the
/// simple level form — `debug`, `warn` — which is all `make debug`
/// needs).
///
/// The returned guard flushes the non-blocking file writer and MUST be
/// kept alive for the whole run — `main` owns it. Call before the
/// collector thread spawns, or its first events race the subscriber.
pub fn init() -> Option<WorkerGuard> {
    let mut level = Level::INFO;
    if let Ok(raw) = std::env::var("RUST_LOG")
        && let Ok(parsed) = Level::from_str(&raw)
    {
        level = parsed;
    }
    // Local offset is detected once, up front, deliberately before the
    // appender spawns its worker thread: the time crate refuses to read
    // the environment once the process is multi-threaded.
    let timer = tracing_subscriber::fmt::time::OffsetTime::local_rfc_3339().unwrap_or_else(|_| {
        tracing_subscriber::fmt::time::OffsetTime::new(
            time::UtcOffset::UTC,
            time::format_description::well_known::Rfc3339,
        )
    });

    let (file_layer, guard) = match logs_dir() {
        Some(dir) => {
            prune_old_logs(&dir);
            let appender = tracing_appender::rolling::daily(&dir, FILE_PREFIX);
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false)
                .with_timer(timer.clone());
            (Some(layer), Some(guard))
        }
        None => {
            eprintln!("file logging disabled: could not create ~/.zstats/logs");
            (None, None)
        }
    };

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        // Colour only where a person is watching a terminal — a bundle's
        // stdout is not one.
        .with_ansi(cfg!(debug_assertions))
        .with_timer(timer);

    let registry = tracing_subscriber::registry()
        .with(LevelFilter::from_level(level))
        .with(stdout_layer);
    match file_layer {
        Some(file_layer) => registry.with(file_layer).init(),
        None => registry.init(),
    }
    guard
}
