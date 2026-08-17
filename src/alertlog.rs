//! Today's alert episodes, remembered across restarts.
//!
//! The Alerts tab is the one list that used to forget everything on
//! quit: History keeps months of CPU records, the analyser keeps its
//! last walk, but a restart erased what fired this morning. This is
//! the side file that closes that gap — `~/.zstats/alerts.toml`, read
//! once at launch, rewritten whenever an episode lands.
//!
//! **Memory, not judgement.** Nothing here evaluates a condition or
//! re-delivers a banner: the file holds episodes zstats' rule engine
//! already decided on, exactly as `state.rs` merged them (CLAUDE.md's
//! hard rule). A restored episode is a card to read, and — because it
//! keys on the same `(subject, kind)` — the episode a re-crossing
//! merges into rather than opening a duplicate beside.
//!
//! Scope is **today**, the same day boundary History draws: an alert
//! from yesterday morning answers no question you would ask now, and
//! an unbounded log would need pruning rules nobody asked for. Entries
//! that fail to parse are dropped individually — a file half-written
//! by a crash costs the entries it truncated, not the list.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zstats::AlertEvent;

/// Bumped only if the shape changes incompatibly; a mismatch reads as
/// "no history", never as garbage.
const FILE_VERSION: i64 = 1;

/// Mirrors `state::MAX_ALERTS`. A hand-edited file must not inflate the
/// list beyond what the list itself would keep.
const KEEP: usize = 20;

/// One episode as it survives a restart. Wall clock, not `Instant`:
/// monotonic time restarts with the machine, and "3h ago" has to
/// still mean three hours after a reboot.
pub struct Restored {
    pub event: AlertEvent,
    pub first_at: SystemTime,
    pub at: SystemTime,
    pub reports: u32,
}

fn file_path(dir: &Path) -> PathBuf {
    dir.join("alerts.toml")
}

/// Today's episodes, most recent first (the order they were written).
pub fn load() -> Vec<Restored> {
    load_in(&zstats::settings::default_dir(), SystemTime::now())
}

/// Replace the file with `episodes`. Called after an episode lands;
/// the whole list is small and rewriting it keeps the file a plain
/// mirror of what the panel shows.
pub fn save(episodes: &[Restored]) {
    save_in(&zstats::settings::default_dir(), episodes);
}

fn load_in(dir: &Path, now: SystemTime) -> Vec<Restored> {
    let Ok(text) = fs::read_to_string(file_path(dir)) else {
        return Vec::new();
    };
    let Ok(doc) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    if doc.get("version").and_then(toml::Value::as_integer) != Some(FILE_VERSION) {
        return Vec::new();
    }
    let today = local_date(now);
    doc.get("episode")
        .and_then(toml::Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| parse_episode(row, today.as_deref()))
                .take(KEEP)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_episode(row: &toml::Value, today: Option<&str>) -> Option<Restored> {
    let secs = |key: &str| -> Option<u64> { Some(row.get(key)?.as_integer()?.max(0) as u64) };
    let at_secs = secs("at_unix")?;
    let at = UNIX_EPOCH + Duration::from_secs(at_secs);
    // A stamp we cannot place on a calendar is dropped rather than
    // guessed onto today.
    if local_date(at).as_deref() != today || today.is_none() {
        return None;
    }
    let first_at = UNIX_EPOCH + Duration::from_secs(secs("first_at_unix").unwrap_or(at_secs));
    Some(Restored {
        event: row.get("event")?.clone().try_into().ok()?,
        first_at,
        at,
        reports: secs("reports").unwrap_or(1).clamp(1, u32::MAX as u64) as u32,
    })
}

fn save_in(dir: &Path, episodes: &[Restored]) {
    let mut doc = toml::Table::new();
    doc.insert("version".into(), toml::Value::Integer(FILE_VERSION));
    let rows: Vec<toml::Value> = episodes
        .iter()
        .filter_map(|e| {
            let mut row = toml::Table::new();
            row.insert(
                "first_at_unix".into(),
                toml::Value::Integer(unix(e.first_at)),
            );
            row.insert("at_unix".into(), toml::Value::Integer(unix(e.at)));
            row.insert("reports".into(), toml::Value::Integer(i64::from(e.reports)));
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
    let path = file_path(dir);
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
    let _ = fs::rename(&tmp, &path);
}

fn unix(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

/// The local calendar date of a wall-clock instant, as `2026-08-17`.
/// Local, not UTC: "today" means the user's day, the same boundary the
/// History tab draws.
fn local_date(t: SystemTime) -> Option<String> {
    let secs = t.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let stamp = jiff::Timestamp::from_second(secs.min(i64::MAX as u64) as i64).ok()?;
    Some(
        stamp
            .to_zoned(jiff::tz::TimeZone::system())
            .date()
            .to_string(),
    )
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

    #[test]
    fn episodes_round_trip_and_yesterday_is_dropped() {
        let dir = scratch("roundtrip");
        let _ = fs::remove_dir_all(&dir);
        let now = SystemTime::now();
        let episodes = vec![
            Restored {
                event: cpu_event(7),
                first_at: now - Duration::from_secs(1800),
                at: now,
                reports: 2,
            },
            Restored {
                event: cpu_event(8),
                // Comfortably a different calendar day, whatever the
                // hour the test runs at.
                first_at: now - Duration::from_secs(3 * 86_400),
                at: now - Duration::from_secs(3 * 86_400),
                reports: 1,
            },
        ];
        save_in(&dir, &episodes);

        let loaded = load_in(&dir, now);
        assert_eq!(loaded.len(), 1, "only today's episode comes back");
        assert_eq!(loaded[0].reports, 2);
        assert_eq!(
            loaded[0].at.duration_since(loaded[0].first_at).unwrap(),
            Duration::from_secs(1800),
            "the episode keeps its span across the restart"
        );
        match &loaded[0].event.subject {
            AlertSubject::Process { pid, name } => {
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

        // 0600 on purpose: an alert names the programs you run.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(file_path(&dir)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_or_foreign_file_reads_as_no_history() {
        let dir = scratch("broken");
        let _ = fs::remove_dir_all(&dir);
        let now = SystemTime::now();
        assert!(load_in(&dir, now).is_empty(), "no file at all");

        fs::create_dir_all(&dir).unwrap();
        fs::write(file_path(&dir), "not [valid toml").unwrap();
        assert!(load_in(&dir, now).is_empty(), "unparsable");

        fs::write(file_path(&dir), "version = 99\n").unwrap();
        assert!(load_in(&dir, now).is_empty(), "a future version");

        // One unreadable entry costs that entry, not the list.
        save_in(
            &dir,
            &[Restored {
                event: cpu_event(7),
                first_at: now,
                at: now,
                reports: 1,
            }],
        );
        let text = fs::read_to_string(file_path(&dir)).unwrap();
        fs::write(
            file_path(&dir),
            format!("{text}\n[[episode]]\nat_unix = 0\nreports = 1\n"),
        )
        .unwrap();
        assert_eq!(load_in(&dir, now).len(), 1, "the good entry survives");
        let _ = fs::remove_dir_all(&dir);
    }
}
