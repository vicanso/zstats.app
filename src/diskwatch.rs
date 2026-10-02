//! The daily disk check: what grew this week, measured while nobody is
//! looking.
//!
//! The directory analyser answers "what is big now"; a disk fills up
//! over days, and the useful warning is "this folder put on 6 GB this
//! week" before the volume is full. That needs a measurement taken on
//! a schedule rather than when someone remembers to press Analyze, so
//! once a day the home tree is walked in the background
//! (`diskscan::Pace::Background` — one thread at background QoS, which
//! macOS throttles behind anything a person is doing), and each walk
//! leaves one small snapshot here: every directory of at least
//! [`RECORD_FLOOR`] with its total, one file per local day under
//! `~/.zstats/disk-history/`, kept [`KEEP_DAYS`].
//!
//! **Why a full walk and not FSEvents.** Replaying FSEvents history to
//! re-walk only what changed was measured on a development Mac
//! (2026-10-02): one day under `$HOME` was 2.1 M events across 469 k
//! directories (423 k already deleted — build output, app containers),
//! 199 s just to replay, then 18 s to re-read the 47 k still there —
//! against 25 s for the whole walk. A busy machine changes too much for
//! "only what changed" to be cheaper than "everything", and the gap
//! widens with every day since the last walk.
//!
//! **When it runs** ([`due`]): a home walk has finished before — the
//! user has seen the analyser and any one-time folder consent is
//! already answered, so a background walk never raises a prompt — the
//! last one is [`CHECK_EVERY`] old, the machine is on power, not busy,
//! and nobody has the panel or the disk-space window open. Any finished
//! home walk counts, the user's own included: a check is not repeated
//! the hour after someone pressed Analyze.
//!
//! **What it says.** Growth is today against the snapshot nearest
//! [`WINDOW`] old ([`baseline_in`]), deepest directory that carries the
//! growth ([`growth`]): a cache that put on 6 GB also lifted `Library`
//! and `~`, and three rows of the same 6 GB would read as 18. The disk
//! window lists it; a climb past [`NOTIFY_BYTES`] posts one silent
//! banner per directory per [`WINDOW`]. Display and a quiet banner only
//! — not an `AlertEvent`: zstats owns alerting, and this is an observer
//! of the same class as the memory-creep watcher.

use crate::alertlog;
use crate::diskscan::ScanResult;
use jiff::ToSpan;
use jiff::civil::Date;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How old the last home walk must be before the background repeats it.
/// A day: growth worth a warning is measured in days, and one walk a day
/// is a cost nobody notices (~25 s of throttled I/O on the measured Mac).
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The comparison span: "this week".
pub const WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Days of snapshots kept. Twice the window, so a machine that was off
/// for a few days still has a baseline near a week old.
const KEEP_DAYS: i64 = 14;

/// Directories recorded per snapshot. Growth below a gigabyte is not
/// ranked anyway; 100 MiB keeps a home tree's snapshot at a few
/// thousand lines, and lets a directory that was small last week show
/// up as new (absent from the baseline reads as under this floor).
pub const RECORD_FLOOR: u64 = 100 * 1024 * 1024;

/// Growth worth a row in the disk window.
pub const GROWTH_FLOOR: u64 = 1024 * 1024 * 1024;

/// Growth worth a banner: enough to matter on any disk this app runs on,
/// rare enough on a working machine that the banner is news.
pub const NOTIFY_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Rows the disk window lists.
pub const MAX_ROWS: usize = 5;

/// A parent is represented by a child that carries this share of its
/// growth — the same chase `diskscan` applies to sizes.
const DOMINANCE_PERCENT: u64 = 90;

/// Whole-machine CPU above which the check waits. A walk competes for
/// the disk more than the CPU, but a busy machine is a person working,
/// and the check can run in the next quiet tick.
const BUSY_CPU: f32 = 25.0;

/// A baseline younger than this says nothing about a week.
const MIN_BASELINE_AGE: Duration = Duration::from_secs(20 * 60 * 60);

const NOTIFIED_FILE: &str = "notified.toml";

/// One directory that grew, deepest first among its ancestors.
#[derive(Clone, Debug, PartialEq)]
pub struct Growth {
    pub path: PathBuf,
    pub grew: u64,
    pub now: u64,
}

/// Growth since a baseline, with how far apart the two walks are —
/// "over 7 days", or fewer while the history is still young.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub over: Duration,
    pub rows: Vec<Growth>,
}

/// One day's measurement.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub scanned_at: SystemTime,
    /// The walk's root — never a growth row of its own.
    pub root: PathBuf,
    pub dirs: HashMap<PathBuf, u64>,
}

/// What [`due`] decides on. Plain fields, so the policy is testable
/// without a running app.
pub struct Gate {
    /// `prefs::disk_watch`.
    pub enabled: bool,
    /// When the last home walk finished. `None` — never — keeps the
    /// check off entirely: the first walk is the user's own.
    pub last_walk: Option<SystemTime>,
    pub now: SystemTime,
    pub on_battery: bool,
    pub cpu_percent: Option<f32>,
    /// The panel or the disk-space window is on screen.
    pub someone_looking: bool,
    /// A walk — the user's or a previous check — is in flight.
    pub walk_running: bool,
}

/// Whether the background check should start now.
pub fn due(gate: &Gate) -> bool {
    if !gate.enabled || gate.on_battery || gate.someone_looking || gate.walk_running {
        return false;
    }
    if gate.cpu_percent.is_none_or(|cpu| cpu > BUSY_CPU) {
        return false;
    }
    let Some(last) = gate.last_walk else {
        return false;
    };
    // A clock set backwards is not a day passing.
    gate.now
        .duration_since(last)
        .is_ok_and(|age| age >= CHECK_EVERY)
}

/// `~/.zstats/disk-history`.
pub fn dir() -> PathBuf {
    zstats::settings::default_dir().join("disk-history")
}

/// Record a finished home walk as today's snapshot, replacing any
/// earlier one from today, and drop snapshots past [`KEEP_DAYS`].
pub fn record(result: &ScanResult) {
    record_in(&dir(), result);
}

fn record_in(dir: &Path, result: &ScanResult) {
    let Some(day) = alertlog::local_day(result.scanned_at) else {
        return;
    };
    let mut table = toml::Table::new();
    for (path, bytes) in result.dir_totals(RECORD_FLOOR) {
        if let Some(path) = path.to_str() {
            table.insert(path.into(), toml::Value::Integer(clamp(bytes)));
        }
    }
    let mut doc = toml::Table::new();
    doc.insert(
        "scanned_at_unix".into(),
        toml::Value::Integer(unix(result.scanned_at)),
    );
    if let Some(root) = result.root.to_str() {
        doc.insert("root".into(), toml::Value::String(root.into()));
    }
    doc.insert("dirs".into(), toml::Value::Table(table));
    let Ok(text) = toml::to_string(&doc) else {
        return;
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Err(e) = fs::write(dir.join(format!("{day}.toml")), text) {
        tracing::warn!("disk history: {e}");
        return;
    }
    sweep_in(dir, day);
}

fn sweep_in(dir: &Path, today: Date) {
    let Some(oldest) = today.checked_sub(KEEP_DAYS.days()).ok() else {
        return;
    };
    for (day, path) in days_in(dir) {
        if day < oldest {
            let _ = fs::remove_file(path);
        }
    }
}

/// Every snapshot file, by its day.
fn days_in(dir: &Path) -> Vec<(Date, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let day: Date = path.file_stem()?.to_str()?.parse().ok()?;
            (path.extension()? == "toml").then_some((day, path))
        })
        .collect()
}

/// The snapshot to compare `now` against: the newest one at least a
/// [`WINDOW`] old; failing that, the oldest one at least
/// [`MIN_BASELINE_AGE`] old (a young history still says what grew,
/// over the span it has). Chosen by file name, so only the one it picks
/// is read.
fn baseline_in(dir: &Path, now: SystemTime) -> Option<Snapshot> {
    let today = alertlog::local_day(now)?;
    let week_ago = today.checked_sub(7.days()).ok()?;
    let mut days = days_in(dir);
    days.retain(|(day, _)| *day < today);
    days.sort();
    let pick = days
        .iter()
        .rev()
        .find(|(day, _)| *day <= week_ago)
        .or_else(|| days.first())?;
    let snapshot = load(&pick.1)?;
    now.duration_since(snapshot.scanned_at)
        .is_ok_and(|age| age >= MIN_BASELINE_AGE)
        .then_some(snapshot)
}

fn load(path: &Path) -> Option<Snapshot> {
    let doc: toml::Table = fs::read_to_string(path).ok()?.parse().ok()?;
    let secs = doc.get("scanned_at_unix")?.as_integer()?;
    let root = PathBuf::from(doc.get("root").and_then(|v| v.as_str()).unwrap_or("/"));
    let dirs = doc
        .get("dirs")?
        .as_table()?
        .iter()
        .filter_map(|(path, bytes)| Some((PathBuf::from(path), bytes.as_integer()?.max(0) as u64)))
        .collect();
    Some(Snapshot {
        scanned_at: UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64),
        root,
        dirs,
    })
}

/// What grew under `root` between `base` and `now`, at least
/// [`GROWTH_FLOOR`], largest first, at most [`MAX_ROWS`].
///
/// A directory missing from the baseline counts from zero — it was
/// under [`RECORD_FLOOR`] or did not exist. The root itself is never a
/// row (it is the sum of everything else). A directory is dropped when
/// one of its own listed descendants carries [`DOMINANCE_PERCENT`] of
/// its growth: the descendant is the answer, the ancestor only rose
/// with it.
pub fn growth(root: &Path, now: &[(PathBuf, u64)], base: &Snapshot) -> Vec<Growth> {
    let candidates: Vec<Growth> = now
        .iter()
        .filter(|(path, _)| path != root && path.starts_with(root))
        .filter_map(|(path, bytes)| {
            let was = base.dirs.get(path).copied().unwrap_or(0);
            let grew = bytes.saturating_sub(was);
            (grew >= GROWTH_FLOOR).then(|| Growth {
                path: path.clone(),
                grew,
                now: *bytes,
            })
        })
        .collect();
    let mut rows: Vec<Growth> = candidates
        .iter()
        .filter(|parent| {
            !candidates.iter().any(|child| {
                child.path != parent.path
                    && child.path.starts_with(&parent.path)
                    && child.grew * 100 >= parent.grew * DOMINANCE_PERCENT
            })
        })
        .cloned()
        .collect();
    rows.sort_by(|a, b| b.grew.cmp(&a.grew).then_with(|| a.path.cmp(&b.path)));
    rows.truncate(MAX_ROWS);
    rows
}

/// What grew, read from the history alone: the newest snapshot against
/// its [`baseline_in`]. From files rather than from a scan in memory, so a
/// launch can say it before any walk of this session (a cached result
/// carries no per-directory totals), and the user's walks and the daily
/// check feed the same answer. Two small files are read.
pub fn latest_report() -> Option<Report> {
    latest_report_in(&dir())
}

fn latest_report_in(dir: &Path) -> Option<Report> {
    let (_, newest) = days_in(dir).into_iter().max()?;
    let newest = load(&newest)?;
    let base = baseline_in(dir, newest.scanned_at)?;
    let now: Vec<(PathBuf, u64)> = newest
        .dirs
        .iter()
        .map(|(path, bytes)| (path.clone(), *bytes))
        .collect();
    Some(Report {
        over: newest
            .scanned_at
            .duration_since(base.scanned_at)
            .unwrap_or_default(),
        rows: growth(&newest.root, &now, &base),
    })
}

/// The rows of `report` past [`NOTIFY_BYTES`] not announced within the
/// last [`WINDOW`], recorded as announced now. A directory that keeps
/// growing is news once a week, not once a day: the daily check would
/// otherwise re-announce the same climb against a baseline that still
/// predates it.
pub fn unannounced(report: &Report, now: SystemTime) -> Vec<Growth> {
    unannounced_in(&dir(), report, now)
}

fn unannounced_in(dir: &Path, report: &Report, now: SystemTime) -> Vec<Growth> {
    let file = dir.join(NOTIFIED_FILE);
    let mut table: toml::Table = fs::read_to_string(&file)
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or_default();
    let cutoff = unix(now).saturating_sub(WINDOW.as_secs() as i64);
    table.retain(|_, at| at.as_integer().is_some_and(|at| at > cutoff));
    let fresh: Vec<Growth> = report
        .rows
        .iter()
        .filter(|row| row.grew >= NOTIFY_BYTES)
        .filter(|row| {
            row.path
                .to_str()
                .is_some_and(|path| !table.contains_key(path))
        })
        .cloned()
        .collect();
    for row in &fresh {
        if let Some(path) = row.path.to_str() {
            table.insert(path.into(), toml::Value::Integer(unix(now)));
        }
    }
    if (fs::create_dir_all(dir).is_ok())
        && let Ok(text) = toml::to_string(&table)
        && let Err(e) = fs::write(&file, text)
    {
        tracing::warn!("disk history: {e}");
    }
    fresh
}

fn unix(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

fn clamp(bytes: u64) -> i64 {
    bytes.min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::process;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn snapshot(dirs: &[(&str, u64)]) -> Snapshot {
        Snapshot {
            scanned_at: UNIX_EPOCH,
            root: p("/h"),
            dirs: dirs.iter().map(|(path, b)| (p(path), *b)).collect(),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("zstats-diskwatch-{tag}-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn gate() -> Gate {
        let now = SystemTime::now();
        Gate {
            enabled: true,
            last_walk: Some(now - CHECK_EVERY - Duration::from_secs(60)),
            now,
            on_battery: false,
            cpu_percent: Some(5.0),
            someone_looking: false,
            walk_running: false,
        }
    }

    #[test]
    fn the_check_waits_for_a_day_power_quiet_and_nobody_looking() {
        assert!(due(&gate()));
        assert!(!due(&Gate {
            enabled: false,
            ..gate()
        }));
        assert!(!due(&Gate {
            on_battery: true,
            ..gate()
        }));
        assert!(!due(&Gate {
            cpu_percent: Some(60.0),
            ..gate()
        }));
        assert!(!due(&Gate {
            cpu_percent: None,
            ..gate()
        }));
        assert!(!due(&Gate {
            someone_looking: true,
            ..gate()
        }));
        assert!(!due(&Gate {
            walk_running: true,
            ..gate()
        }));
        // Never analysed: the first walk is the user's own.
        assert!(!due(&Gate {
            last_walk: None,
            ..gate()
        }));
        // Twelve hours is not a day.
        let g = gate();
        assert!(!due(&Gate {
            last_walk: Some(g.now - Duration::from_secs(12 * 3600)),
            ..g
        }));
        // A clock set backwards is not a day passing.
        let g = gate();
        assert!(!due(&Gate {
            last_walk: Some(g.now + Duration::from_secs(3600)),
            ..g
        }));
    }

    #[test]
    fn growth_names_the_directory_that_grew_not_every_ancestor() {
        let base = snapshot(&[
            ("/h", 50 * GIB),
            ("/h/Library", 30 * GIB),
            ("/h/Library/Caches", 10 * GIB),
            ("/h/Library/Caches/sccache", 4 * GIB),
            ("/h/Movies", 8 * GIB),
        ]);
        let now = [
            (p("/h"), 60 * GIB),
            (p("/h/Library"), 36 * GIB),
            (p("/h/Library/Caches"), 16 * GIB),
            (p("/h/Library/Caches/sccache"), 10 * GIB),
            (p("/h/Movies"), 10 * GIB),
            // New this week: absent from the baseline counts from zero.
            (p("/h/Downloads/vm"), 3 * GIB),
            // Grew, but under the floor.
            (p("/h/.npm"), GIB / 2),
        ];
        let rows = growth(&p("/h"), &now, &base);
        let paths: Vec<&Path> = rows.iter().map(|r| r.path.as_path()).collect();
        // sccache carries all 6 GB of Caches and Library: one row, not three.
        assert_eq!(
            paths,
            [
                p("/h/Library/Caches/sccache").as_path(),
                p("/h/Downloads/vm").as_path(),
                p("/h/Movies").as_path(),
            ]
        );
        assert_eq!(rows[0].grew, 6 * GIB);
        assert_eq!(rows[0].now, 10 * GIB);
        assert_eq!(rows[1].grew, 3 * GIB);
    }

    #[test]
    fn diffuse_growth_keeps_the_parent() {
        // Two children of 3 GB each: neither carries 90% of the 6 GB, so
        // the parent is the honest answer, and both children still list.
        let base = snapshot(&[("/h/a", 0), ("/h/a/x", 0), ("/h/a/y", 0)]);
        let now = [
            (p("/h/a"), 6 * GIB),
            (p("/h/a/x"), 3 * GIB),
            (p("/h/a/y"), 3 * GIB),
        ];
        let rows = growth(&p("/h"), &now, &base);
        assert_eq!(rows[0].path, p("/h/a"));
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn the_baseline_is_the_newest_snapshot_a_week_old_or_the_oldest_younger_one() {
        let dir = scratch("baseline");
        fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        let day = |n: u64| now - Duration::from_secs(n * 24 * 3600);
        let write = |at: SystemTime, bytes: u64| {
            let name = alertlog::local_day(at).unwrap();
            fs::write(
                dir.join(format!("{name}.toml")),
                format!(
                    "scanned_at_unix = {}\n[dirs]\n\"/h/x\" = {bytes}\n",
                    unix(at)
                ),
            )
            .unwrap();
        };
        // Only young history: the oldest of it.
        write(day(3), 3);
        write(day(1), 1);
        assert_eq!(baseline_in(&dir, now).unwrap().dirs[&p("/h/x")], 3);
        // A week-old one wins; of two past the week, the newer.
        write(day(9), 9);
        write(day(7), 7);
        assert_eq!(baseline_in(&dir, now).unwrap().dirs[&p("/h/x")], 7);
        // Today's own snapshot is never its own baseline.
        let only_today = scratch("today");
        fs::create_dir_all(&only_today).unwrap();
        fs::write(
            only_today.join(format!("{}.toml", alertlog::local_day(now).unwrap())),
            format!("scanned_at_unix = {}\n[dirs]\n", unix(now)),
        )
        .unwrap();
        assert!(baseline_in(&only_today, now).is_none());
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&only_today);
    }

    #[test]
    fn a_climb_is_announced_once_a_week() {
        let dir = scratch("notified");
        let now = SystemTime::now();
        let report = Report {
            over: WINDOW,
            rows: vec![
                Growth {
                    path: p("/h/big"),
                    grew: 6 * GIB,
                    now: 20 * GIB,
                },
                Growth {
                    path: p("/h/small"),
                    grew: 2 * GIB,
                    now: 4 * GIB,
                },
            ],
        };
        let first = unannounced_in(&dir, &report, now);
        assert_eq!(first.len(), 1, "only past the banner bar");
        assert_eq!(first[0].path, p("/h/big"));
        // The next day's check: same climb, still inside the week.
        let next_day = now + Duration::from_secs(24 * 3600);
        assert!(unannounced_in(&dir, &report, next_day).is_empty());
        // A week on, it is news again.
        let week_on = now + WINDOW + Duration::from_secs(60);
        assert_eq!(unannounced_in(&dir, &report, week_on).len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_report_reads_the_newest_snapshot_against_a_week_before_it() {
        let dir = scratch("report");
        fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        let write = |at: SystemTime, sccache: u64| {
            let name = alertlog::local_day(at).unwrap();
            fs::write(
                dir.join(format!("{name}.toml")),
                format!(
                    "scanned_at_unix = {}\nroot = \"/h\"\n[dirs]\n\"/h\" = {}\n\"/h/sccache\" = {sccache}\n",
                    unix(at),
                    sccache + GIB,
                ),
            )
            .unwrap();
        };
        assert!(latest_report_in(&dir).is_none(), "no history, no report");
        write(now - Duration::from_secs(7 * 24 * 3600), 2 * GIB);
        write(now, 9 * GIB);
        let report = latest_report_in(&dir).unwrap();
        assert!(report.over >= Duration::from_secs(7 * 24 * 3600 - 60));
        // The root grew by the same 7 GB and is never a row.
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].path, p("/h/sccache"));
        assert_eq!(report.rows[0].grew, 7 * GIB);
        let _ = fs::remove_dir_all(&dir);
    }
}
