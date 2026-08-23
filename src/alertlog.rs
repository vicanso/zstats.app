//! Alert episodes, remembered across restarts — today's for the list,
//! the last month's for the record.
//!
//! The Alerts tab is the one list that used to forget everything on
//! quit: History keeps months of CPU records, the analyser keeps its
//! last walk, but a restart erased what fired this morning. These are
//! the side files that close that gap — one per local day,
//! `~/.zstats/alerts-YYYY-MM-DD.toml`, today's read once at launch and
//! rewritten whenever an episode lands, the older ones read for the
//! "past 7 days" block and swept after [`RETENTION_DAYS`] (the same
//! month zstats keeps its own daily records).
//!
//! **Memory, not judgement.** Nothing here evaluates a condition or
//! re-delivers a banner: a file holds episodes zstats' rule engine
//! already decided on, exactly as `state.rs` merged them (CLAUDE.md's
//! hard rule). A restored episode is a card to read, and — because it
//! keys on the same `(subject, kind)` — the episode a re-crossing
//! merges into rather than opening a duplicate beside.
//!
//! The day boundary is the one History draws: today's file feeds the
//! live list, and when the local day turns the list retires what is no
//! longer today's — which is exactly what yesterday's file already
//! holds. A dismissed card leaves the list but not the day's file: it
//! is written back with `dismissed = true`, because "this fired and I
//! acknowledged it" is still what happened, and the week's record would
//! be a lie without it. Entries that fail to parse are dropped
//! individually — a file half-written by a crash costs the entries it
//! truncated, not the list.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zstats::AlertEvent;

/// Bumped only if the shape changes incompatibly; a mismatch reads as
/// "no history", never as garbage.
const FILE_VERSION: i64 = 1;

/// Mirrors `state::MAX_ALERTS`. A hand-edited file must not inflate the
/// list beyond what the list itself would keep.
const KEEP: usize = 20;

/// Days of files kept — the month zstats keeps its daily records, so
/// the two histories answer the same window.
pub const RETENTION_DAYS: i64 = 30;

/// The file that preceded the per-day ones. Read once, as today's
/// file, when no day file exists yet; removed on the first save after
/// that. Only this app ever wrote it.
const LEGACY_FILE: &str = "alerts.toml";

/// One episode as it survives a restart. Wall clock, not `Instant`:
/// monotonic time restarts with the machine, and "3h ago" has to
/// still mean three hours after a reboot.
pub struct Restored {
    pub event: AlertEvent,
    pub first_at: SystemTime,
    pub at: SystemTime,
    pub reports: u32,
    /// Acknowledged with the card's ✕. Kept in the day's file for the
    /// record; never restored into the live list.
    pub dismissed: bool,
}

/// One past day's episodes, for the read-only block.
pub struct DayLog {
    /// `2026-08-22`, local calendar.
    pub date: String,
    /// Most recent first, dismissed ones included.
    pub episodes: Vec<Restored>,
}

fn day_file(dir: &Path, date: jiff::civil::Date) -> PathBuf {
    dir.join(format!("alerts-{date}.toml"))
}

/// Today's live episodes, most recent first (the order they were
/// written). Dismissed ones stay in the file and out of the list.
pub fn load() -> Vec<Restored> {
    load_in(&zstats::settings::default_dir(), local_today())
}

/// Replace today's file with today's share of `episodes`. Called after
/// an episode lands or a card is dismissed; the list is small and
/// rewriting it keeps the file a plain mirror of what the panel shows.
/// Episodes from another day are left to that day's file, which was
/// written when they landed.
pub fn save(episodes: &[Restored]) {
    save_in(&zstats::settings::default_dir(), episodes, local_today());
}

/// The last `days` days before today, newest first, days with nothing
/// recorded omitted. Today is not in here — it is the list itself.
pub fn recent(days: u16) -> Vec<DayLog> {
    recent_in(&zstats::settings::default_dir(), local_today(), days)
}

fn local_today() -> jiff::civil::Date {
    jiff::Zoned::now().date()
}

fn load_in(dir: &Path, today: jiff::civil::Date) -> Vec<Restored> {
    let day = day_file(dir, today);
    let text = if day.exists() {
        fs::read_to_string(day)
    } else {
        fs::read_to_string(dir.join(LEGACY_FILE))
    };
    let Ok(text) = text else {
        return Vec::new();
    };
    let mut episodes = parse_file(&text);
    // Either source is read as "today's": the legacy file was never
    // anything else, and a day file is its day by name — but a line
    // that cannot be placed on today's calendar is not today's, and a
    // hand-edited or clock-skewed entry must not be guessed onto it.
    episodes.retain(|e| !e.dismissed && local_day(e.at) == Some(today));
    episodes.truncate(KEEP);
    episodes
}

fn recent_in(dir: &Path, today: jiff::civil::Date, days: u16) -> Vec<DayLog> {
    (1..=i64::from(days))
        .filter_map(|back| today.checked_sub(jiff::Span::new().days(back)).ok())
        .filter_map(|date| {
            let text = fs::read_to_string(day_file(dir, date)).ok()?;
            let episodes = parse_file(&text);
            (!episodes.is_empty()).then(|| DayLog {
                date: date.to_string(),
                episodes,
            })
        })
        .collect()
}

fn parse_file(text: &str) -> Vec<Restored> {
    let Ok(doc) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    if doc.get("version").and_then(toml::Value::as_integer) != Some(FILE_VERSION) {
        return Vec::new();
    }
    doc.get("episode")
        .and_then(toml::Value::as_array)
        .map(|rows| rows.iter().filter_map(parse_episode).collect())
        .unwrap_or_default()
}

fn parse_episode(row: &toml::Value) -> Option<Restored> {
    let secs = |key: &str| -> Option<u64> { Some(row.get(key)?.as_integer()?.max(0) as u64) };
    let at_secs = secs("at_unix")?;
    let at = UNIX_EPOCH + Duration::from_secs(at_secs);
    let first_at = UNIX_EPOCH + Duration::from_secs(secs("first_at_unix").unwrap_or(at_secs));
    Some(Restored {
        event: row.get("event")?.clone().try_into().ok()?,
        first_at,
        at,
        reports: secs("reports").unwrap_or(1).clamp(1, u32::MAX as u64) as u32,
        dismissed: row
            .get("dismissed")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false),
    })
}

/// The day a sweep last ran for, so the directory is read once per
/// day rather than once per save.
static SWEPT_FOR: Mutex<Option<jiff::civil::Date>> = Mutex::new(None);

fn save_in(dir: &Path, episodes: &[Restored], today: jiff::civil::Date) {
    let mut doc = toml::Table::new();
    doc.insert("version".into(), toml::Value::Integer(FILE_VERSION));
    let rows: Vec<toml::Value> = episodes
        .iter()
        .filter(|e| local_day(e.at) == Some(today))
        .filter_map(|e| {
            let mut row = toml::Table::new();
            row.insert(
                "first_at_unix".into(),
                toml::Value::Integer(unix(e.first_at)),
            );
            row.insert("at_unix".into(), toml::Value::Integer(unix(e.at)));
            row.insert("reports".into(), toml::Value::Integer(i64::from(e.reports)));
            if e.dismissed {
                row.insert("dismissed".into(), toml::Value::Boolean(true));
            }
            // zstats' own serde shape — the event round-trips as the
            // library defines it, so a field added upstream needs no
            // change here.
            row.insert("event".into(), toml::Value::try_from(&e.event).ok()?);
            Some(toml::Value::Table(row))
        })
        .collect();
    doc.insert("episode".into(), toml::Value::Array(rows));
    let Ok(text) = toml::to_string(&toml::Value::Table(doc)) else {
        return;
    };

    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = day_file(dir, today);
    let tmp = path.with_extension("toml.tmp");
    if fs::write(&tmp, text).is_err() {
        return;
    }
    // 0600: an alert names the programs you run.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    if fs::rename(&tmp, &path).is_err() {
        return;
    }
    // The day file now holds everything the legacy one did.
    let _ = fs::remove_file(dir.join(LEGACY_FILE));

    let mut swept = SWEPT_FOR.lock().unwrap_or_else(|e| e.into_inner());
    if *swept != Some(today) {
        sweep(dir, today);
        *swept = Some(today);
    }
}

/// Remove day files older than [`RETENTION_DAYS`]. Best-effort, like
/// zstats' own sweep: a file that will not go is retried tomorrow.
fn sweep(dir: &Path, today: jiff::civil::Date) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let Ok(cutoff) = today.checked_sub(jiff::Span::new().days(RETENTION_DAYS)) else {
        return removed;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return removed;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(date) = name
            .to_str()
            .and_then(|n| n.strip_prefix("alerts-"))
            .and_then(|n| n.strip_suffix(".toml"))
            .and_then(|stem| stem.parse::<jiff::civil::Date>().ok())
        else {
            continue;
        };
        if date < cutoff && fs::remove_file(entry.path()).is_ok() {
            removed.push(entry.path());
        }
    }
    removed
}

fn unix(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

/// The local calendar day of a wall-clock instant. Local, not UTC:
/// "today" means the user's day, the same boundary the History tab
/// draws.
pub(crate) fn local_day(t: SystemTime) -> Option<jiff::civil::Date> {
    let secs = t.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let stamp = jiff::Timestamp::from_second(secs.min(i64::MAX as u64) as i64).ok()?;
    Some(stamp.to_zoned(jiff::tz::TimeZone::system()).date())
}

/// [`local_day`] as `2026-08-17`.
pub(crate) fn local_date(t: SystemTime) -> Option<String> {
    local_day(t).map(|d| d.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::process;
    use zstats::{AlertDetail, AlertSubject};

    fn scratch(name: &str) -> PathBuf {
        env::temp_dir().join(format!("zstats-alertlog-{name}-{}", process::id()))
    }

    fn cpu_event(pid: u32) -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::Process {
                pid,
                name: "hog".into(),
                display_name: None,
            },
            detail: AlertDetail::Cpu {
                avg_percent: 190.0,
                threshold_percent: 150.0,
                window: Duration::from_secs(60),
                runaway: false,
            },
            repeat_after: None,
        }
    }

    fn restored(pid: u32, at: SystemTime, dismissed: bool) -> Restored {
        Restored {
            event: cpu_event(pid),
            first_at: at,
            at,
            reports: 1,
            dismissed,
        }
    }

    /// A wall-clock instant safely inside the local day `back` days ago —
    /// noon, so no timezone puts it on a neighbouring date.
    fn noon_days_ago(today: jiff::civil::Date, back: i64) -> (jiff::civil::Date, SystemTime) {
        let date = today.checked_sub(jiff::Span::new().days(back)).unwrap();
        let zoned = date
            .at(12, 0, 0, 0)
            .to_zoned(jiff::tz::TimeZone::system())
            .unwrap();
        let secs = zoned.timestamp().as_second().max(0) as u64;
        (date, UNIX_EPOCH + Duration::from_secs(secs))
    }

    #[test]
    fn episodes_round_trip_and_another_day_stays_out_of_todays_file() {
        let dir = scratch("roundtrip");
        let _ = fs::remove_dir_all(&dir);
        let today = local_today();
        let (_, now) = noon_days_ago(today, 0);
        let (_, three_days_ago) = noon_days_ago(today, 3);
        let episodes = vec![
            Restored {
                event: cpu_event(7),
                first_at: now - Duration::from_secs(1800),
                at: now,
                reports: 2,
                dismissed: false,
            },
            restored(8, three_days_ago, false),
        ];
        save_in(&dir, &episodes, today);

        let loaded = load_in(&dir, today);
        assert_eq!(loaded.len(), 1, "only today's episode is today's");
        assert_eq!(loaded[0].reports, 2);
        assert_eq!(
            loaded[0].at.duration_since(loaded[0].first_at).unwrap(),
            Duration::from_secs(1800),
            "the episode keeps its span across the restart"
        );
        match &loaded[0].event.subject {
            AlertSubject::Process { pid, name, .. } => {
                assert_eq!(*pid, 7);
                assert_eq!(name, "hog");
            }
            other => panic!("subject did not round-trip: {other:?}"),
        }
        match loaded[0].event.detail {
            AlertDetail::Cpu {
                avg_percent,
                threshold_percent,
                ..
            } => {
                assert_eq!(avg_percent, 190.0);
                assert_eq!(threshold_percent, 150.0);
            }
            ref other => panic!("detail did not round-trip: {other:?}"),
        }
        assert!(
            recent_in(&dir, today, 7).is_empty(),
            "the other day's episode was never written — it belongs to its own day's file"
        );

        // 0600 on purpose: an alert names the programs you run.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(day_file(&dir, today))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// The week's record: each day its own file, newest first, today
    /// excluded (it is the list), empty days omitted, and a dismissed
    /// card still counted — it fired.
    #[test]
    fn recent_days_read_newest_first_with_dismissals_kept() {
        let dir = scratch("recent");
        let _ = fs::remove_dir_all(&dir);
        let today = local_today();
        for back in [0, 1, 3] {
            let (date, at) = noon_days_ago(today, back);
            save_in(
                &dir,
                &[
                    restored(10 + back as u32, at, false),
                    restored(20, at, true),
                ],
                date,
            );
        }
        let days = recent_in(&dir, today, 7);
        assert_eq!(days.len(), 2, "yesterday and three days ago; not today");
        assert_eq!(days[0].date, noon_days_ago(today, 1).0.to_string());
        assert_eq!(days[1].date, noon_days_ago(today, 3).0.to_string());
        assert_eq!(
            days[0].episodes.len(),
            2,
            "the dismissed one is in the record"
        );
        assert!(days[0].episodes.iter().any(|e| e.dismissed));

        let live = load_in(&dir, today);
        assert_eq!(live.len(), 1, "but never in the live list");
        assert!(!live[0].dismissed);

        assert!(
            recent_in(&dir, today, 2).len() == 1,
            "the window is honoured"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sweep_keeps_a_month_and_spares_foreign_files() {
        let dir = scratch("sweep");
        let _ = fs::remove_dir_all(&dir);
        let today = local_today();
        for back in [RETENTION_DAYS - 1, RETENTION_DAYS + 1] {
            let (date, at) = noon_days_ago(today, back);
            save_in(&dir, &[restored(1, at, false)], date);
        }
        fs::write(dir.join("alerts-not-a-date.toml"), "").unwrap();
        fs::write(dir.join("app.toml"), "").unwrap();

        let removed = sweep(&dir, today);
        assert_eq!(removed.len(), 1);
        assert!(day_file(&dir, noon_days_ago(today, RETENTION_DAYS - 1).0).exists());
        assert!(!day_file(&dir, noon_days_ago(today, RETENTION_DAYS + 1).0).exists());
        assert!(dir.join("alerts-not-a-date.toml").exists());
        assert!(dir.join("app.toml").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// The file this replaced: read as today's until the first save,
    /// which moves its contents into the day file and removes it.
    #[test]
    fn the_legacy_file_is_read_once_and_retired_by_the_first_save() {
        let dir = scratch("legacy");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let today = local_today();
        let (_, now) = noon_days_ago(today, 0);
        // Write a day file, then rename it into the legacy name.
        save_in(&dir, &[restored(5, now, false)], today);
        fs::rename(day_file(&dir, today), dir.join(LEGACY_FILE)).unwrap();

        let loaded = load_in(&dir, today);
        assert_eq!(loaded.len(), 1, "the legacy file feeds today's list");

        save_in(&dir, &loaded, today);
        assert!(day_file(&dir, today).exists());
        assert!(
            !dir.join(LEGACY_FILE).exists(),
            "retired after the first save"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_or_foreign_file_reads_as_no_history() {
        let dir = scratch("broken");
        let _ = fs::remove_dir_all(&dir);
        let today = local_today();
        assert!(load_in(&dir, today).is_empty(), "no file at all");

        fs::create_dir_all(&dir).unwrap();
        fs::write(day_file(&dir, today), "not [valid toml").unwrap();
        assert!(load_in(&dir, today).is_empty(), "unparsable");

        fs::write(day_file(&dir, today), "version = 99\n").unwrap();
        assert!(load_in(&dir, today).is_empty(), "a future version");

        // One unreadable entry costs that entry, not the list.
        let (_, now) = noon_days_ago(today, 0);
        save_in(&dir, &[restored(7, now, false)], today);
        let text = fs::read_to_string(day_file(&dir, today)).unwrap();
        fs::write(
            day_file(&dir, today),
            format!("{text}\n[[episode]]\nat_unix = 0\nreports = 1\n"),
        )
        .unwrap();
        assert_eq!(load_in(&dir, today).len(), 1, "the good entry survives");
        let _ = fs::remove_dir_all(&dir);
    }
}
