//! UI preferences — language, theme, panel opacity, the outbound proxy,
//! and the analyser's last scope — persisted in `app.toml`.
//!
//! Deliberately *not* in the shared `config.toml`: `zstats::settings::save`
//! serialises only the sections the CLI models (`collector` / `daemon` /
//! `alerts`), so an extra key written there would survive exactly until the
//! next `apply_add` round-trip — ours or the CLI's — and then silently
//! vanish. The overrides live in their own file in the same `~/.zstats`
//! directory instead, which nothing but this app touches.
//!
//! An absent key means "follow the system". That makes a missing file the
//! correct default, and keeps the file empty for anyone who never touches
//! the setting.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU8, Ordering};

/// The user's language choice. `System` defers to `i18n::detect`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LanguagePref {
    #[default]
    System,
    English,
    Chinese,
}

/// The user's theme choice. `System` defers to the window appearance.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ThemePref {
    #[default]
    System,
    Light,
    Dark,
}

impl LanguagePref {
    /// The value written to / read from `app.toml`; `None` for `System`,
    /// which is expressed by leaving the key out.
    fn key(self) -> Option<&'static str> {
        match self {
            LanguagePref::System => None,
            LanguagePref::English => Some("en"),
            LanguagePref::Chinese => Some("zh"),
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "en" => LanguagePref::English,
            "zh" => LanguagePref::Chinese,
            // An unrecognised value reads as "follow the system" rather
            // than an error — same posture as a missing file.
            _ => LanguagePref::System,
        }
    }

    /// The rust-i18n locale this preference pins, or `None` for `System`.
    pub fn locale(self) -> Option<&'static str> {
        self.key()
    }
}

impl ThemePref {
    fn key(self) -> Option<&'static str> {
        match self {
            ThemePref::System => None,
            ThemePref::Light => Some("light"),
            ThemePref::Dark => Some("dark"),
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "light" => ThemePref::Light,
            "dark" => ThemePref::Dark,
            _ => ThemePref::System,
        }
    }
}

// Held in statics rather than app state: the theme resolves before the
// first frame and the locale pins before the first `t!`, both ahead of the
// state entity existing. Same pattern as `theme::DARK`.
static LANGUAGE: AtomicU8 = AtomicU8::new(0);
static THEME: AtomicU8 = AtomicU8::new(0);
/// Hundredths of opacity. `0` means "unset — use the mode default".
/// One copy, read per frame by the root view's wash — a picker change
/// lands on the very next repaint. (Hand-edits to `app.toml` still wait
/// for a restart: nothing watches the file.)
static OPACITY: AtomicU8 = AtomicU8::new(0);
/// Outbound-proxy override for the clean-hints fetch: "" follows the
/// environment / OS system proxy, "none" forces direct, anything else
/// is a proxy URI. Mirrored into `proxy::set_configured_proxy` on load
/// and on every set, so the module the background fetch reads is never
/// stale.
static PROXY: RwLock<String> = RwLock::new(String::new());
/// The analysis scope a fresh launch restores — the roots of the last
/// finished top-level walk. Empty means the default home walk, expressed
/// by leaving the key out (same posture as every other pref here).
static ANALYSIS_ROOTS: RwLock<Vec<String>> = RwLock::new(Vec::new());
/// Directories the disk-space window leaves alone, as written in the
/// file — expanded on read, never by the writer, so the file keeps the
/// `~` the user typed.
static ANALYSIS_EXCLUDE: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Floor accepted from the file or the picker. Below this the built-in
/// dark/light default is used instead.
pub const OPACITY_MIN: f32 = 0.5;
pub const OPACITY_MAX: f32 = 1.0;

fn encode_language(pref: LanguagePref) -> u8 {
    match pref {
        LanguagePref::System => 0,
        LanguagePref::English => 1,
        LanguagePref::Chinese => 2,
    }
}

fn decode_language(raw: u8) -> LanguagePref {
    match raw {
        1 => LanguagePref::English,
        2 => LanguagePref::Chinese,
        _ => LanguagePref::System,
    }
}

fn encode_theme(pref: ThemePref) -> u8 {
    match pref {
        ThemePref::System => 0,
        ThemePref::Light => 1,
        ThemePref::Dark => 2,
    }
}

fn decode_theme(raw: u8) -> ThemePref {
    match raw {
        1 => ThemePref::Light,
        2 => ThemePref::Dark,
        _ => ThemePref::System,
    }
}

pub fn language() -> LanguagePref {
    decode_language(LANGUAGE.load(Ordering::Relaxed))
}

pub fn theme() -> ThemePref {
    decode_theme(THEME.load(Ordering::Relaxed))
}

/// The panel opacity — what the Interface page shows and what the root
/// view paints, one and the same. `None` → the mode default.
pub fn opacity() -> Option<f32> {
    decode_opacity(OPACITY.load(Ordering::Relaxed))
}

pub fn proxy() -> String {
    PROXY.read().expect("proxy pref lock poisoned").clone()
}

/// Remember, persist and apply a proxy setting. Callers validate first
/// (`proxy::is_valid_proxy_setting`); a hand-edited file may still hold
/// junk, which the resolver degrades to system behavior.
pub fn set_proxy(value: &str) {
    *PROXY.write().expect("proxy pref lock poisoned") = value.trim().to_string();
    crate::proxy::set_configured_proxy(value);
    persist();
}

/// The analyser scope to restore at launch; empty = the default (~).
pub fn analysis_roots() -> Vec<PathBuf> {
    ANALYSIS_ROOTS
        .read()
        .expect("analysis roots pref lock poisoned")
        .iter()
        .map(PathBuf::from)
        .collect()
}

/// Directories the analyser must not walk and the large-file listing
/// must not show.
///
/// Hand-written into `app.toml` (`analysis_exclude = ["~/github"]`) —
/// there is no editor for it yet, which is why the writer carries the
/// key through untouched. `~/` expands against HOME here rather than in
/// the file, so what the user typed is what stays on disk.
pub fn analysis_exclude() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    ANALYSIS_EXCLUDE
        .read()
        .expect("analysis exclude pref lock poisoned")
        .iter()
        .map(|raw| match raw.strip_prefix("~/") {
            Some(rest) if !home.is_empty() => Path::new(&home).join(rest),
            _ => PathBuf::from(raw),
        })
        .collect()
}

/// The exclusion list exactly as stored — what the editor shows and
/// edits, `~` and all. [`analysis_exclude`] is the expanded form the
/// walk uses.
pub fn analysis_exclude_raw() -> Vec<String> {
    ANALYSIS_EXCLUDE
        .read()
        .expect("analysis exclude pref lock poisoned")
        .clone()
}

/// Replace the exclusion list. Entries are stored as written, with one
/// normalisation: a path under HOME is collapsed to `~/…` so a list
/// built by clicking reads like one written by hand, and survives a
/// change of user name.
pub fn set_analysis_exclude(paths: &[String]) {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut seen: Vec<String> = Vec::new();
    for raw in paths {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let stored = match trimmed.strip_prefix(&home) {
            Some(rest) if !home.is_empty() && rest.starts_with('/') => format!("~{rest}"),
            _ => trimmed.to_string(),
        };
        if !seen.contains(&stored) {
            seen.push(stored);
        }
    }
    *ANALYSIS_EXCLUDE
        .write()
        .expect("analysis exclude pref lock poisoned") = seen;
    persist();
}

/// Remember and persist the scope the next launch should restore. Pass
/// an empty slice for the default home walk — the key is then omitted.
pub fn set_analysis_roots(roots: &[PathBuf]) {
    *ANALYSIS_ROOTS
        .write()
        .expect("analysis roots pref lock poisoned") =
        roots.iter().map(|r| r.display().to_string()).collect();
    persist();
}

fn encode_opacity(value: Option<f32>) -> u8 {
    match value {
        Some(v) if v >= OPACITY_MIN => (v.min(OPACITY_MAX) * 100.0).round() as u8,
        _ => 0,
    }
}

fn decode_opacity(raw: u8) -> Option<f32> {
    (raw >= 50).then_some((raw as f32 / 100.0).min(OPACITY_MAX))
}

/// Read `app.toml` into the statics. Call once at startup, before the
/// theme is first resolved and the locale first pinned.
pub fn load() {
    let prefs = read(&zstats::settings::default_dir());
    LANGUAGE.store(encode_language(prefs.language), Ordering::Relaxed);
    THEME.store(encode_theme(prefs.theme), Ordering::Relaxed);
    OPACITY.store(encode_opacity(prefs.opacity), Ordering::Relaxed);
    crate::proxy::set_configured_proxy(&prefs.proxy);
    *PROXY.write().expect("proxy pref lock poisoned") = prefs.proxy;
    *ANALYSIS_ROOTS
        .write()
        .expect("analysis roots pref lock poisoned") = prefs.analysis_roots;
    *ANALYSIS_EXCLUDE
        .write()
        .expect("analysis exclude pref lock poisoned") = prefs.analysis_exclude;
}

/// Remember and persist a language choice. Only the store is infallible —
/// a failed write is reported and the in-memory choice still applies for
/// this run.
pub fn set_language(pref: LanguagePref) {
    LANGUAGE.store(encode_language(pref), Ordering::Relaxed);
    persist();
}

/// Remember and persist a theme choice.
pub fn set_theme(pref: ThemePref) {
    THEME.store(encode_theme(pref), Ordering::Relaxed);
    persist();
}

/// Remember, persist and apply a panel opacity — the root view reads
/// it per frame, so the caller's repaint makes it land immediately.
pub fn set_opacity(value: Option<f32>) {
    OPACITY.store(encode_opacity(value), Ordering::Relaxed);
    persist();
}

fn persist() {
    let dir = zstats::settings::default_dir();
    let prefs = Prefs {
        language: language(),
        theme: theme(),
        opacity: opacity(),
        proxy: proxy(),
        analysis_roots: ANALYSIS_ROOTS
            .read()
            .expect("analysis roots pref lock poisoned")
            .clone(),
        analysis_exclude: ANALYSIS_EXCLUDE
            .read()
            .expect("analysis exclude pref lock poisoned")
            .clone(),
    };
    if let Err(e) = write(&dir, &prefs) {
        eprintln!("could not write {}: {e}", file_path(&dir).display());
    }
}

fn file_path(dir: &Path) -> PathBuf {
    dir.join("app.toml")
}

/// Everything `app.toml` models — and therefore everything that
/// survives a write. A struct rather than a widening tuple, because
/// every field here has to make the round trip and a positional list of
/// six is where that starts going wrong.
///
/// Anything NOT modelled here is dropped the next time a preference
/// changes, since [`write`] rebuilds the file from these fields alone.
/// That is why a hand-edited key still has to be read *and* written back
/// (`analysis_exclude` is exactly that case).
#[derive(Default, Debug, PartialEq)]
struct Prefs {
    language: LanguagePref,
    theme: ThemePref,
    opacity: Option<f32>,
    proxy: String,
    analysis_roots: Vec<String>,
    analysis_exclude: Vec<String>,
}

fn read(dir: &Path) -> Prefs {
    let Ok(text) = fs::read_to_string(file_path(dir)) else {
        return Prefs::default();
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return Prefs::default();
    };
    let get = |key: &str| table.get(key).and_then(|v| v.as_str());
    let list = |key: &str| {
        table
            .get(key)
            .and_then(toml::Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    Prefs {
        language: get("language").map_or_else(Default::default, LanguagePref::from_key),
        theme: get("theme").map_or_else(Default::default, ThemePref::from_key),
        opacity: table.get("opacity").and_then(parse_opacity),
        proxy: get("proxy").unwrap_or_default().trim().to_string(),
        analysis_roots: list("analysis_roots"),
        analysis_exclude: list("analysis_exclude"),
    }
}

/// `None` when the key is missing, unparsable, or below [`OPACITY_MIN`].
fn parse_opacity(value: &toml::Value) -> Option<f32> {
    let n = value
        .as_float()
        .or_else(|| value.as_integer().map(|i| i as f64))? as f32;
    (n >= OPACITY_MIN).then_some(n.min(OPACITY_MAX))
}

fn write(dir: &Path, prefs: &Prefs) -> io::Result<()> {
    // Serialised by the toml crate, never by hand: an analysis root is
    // a user-chosen path, and macOS allows every byte but `/` and NUL
    // in a filename — including the control characters that are illegal
    // raw inside a TOML basic string. A hand-rolled quote would emit
    // them as-is, the reader would fail to parse the whole file, and
    // `read`'s fallback would silently reset *every* preference here.
    let mut doc = toml::Table::new();
    if let Some(key) = prefs.language.key() {
        doc.insert("language".into(), toml::Value::String(key.into()));
    }
    if let Some(key) = prefs.theme.key() {
        doc.insert("theme".into(), toml::Value::String(key.into()));
    }
    if let Some(value) = prefs.opacity.filter(|v| *v >= OPACITY_MIN) {
        doc.insert(
            "opacity".into(),
            toml::Value::Float(f64::from((value.min(OPACITY_MAX) * 100.0).round() / 100.0)),
        );
    }
    if !prefs.proxy.is_empty() {
        doc.insert("proxy".into(), toml::Value::String(prefs.proxy.clone()));
    }
    let mut list = |key: &str, values: &[String]| {
        if !values.is_empty() {
            doc.insert(
                key.into(),
                toml::Value::Array(
                    values
                        .iter()
                        .map(|v| toml::Value::String(v.clone()))
                        .collect(),
                ),
            );
        }
    };
    list("analysis_roots", &prefs.analysis_roots);
    // Written back although nothing in the UI sets it: `write` rebuilds
    // the file from what it models, so a key it merely ignored would be
    // gone the first time someone changed the theme.
    list("analysis_exclude", &prefs.analysis_exclude);
    let body = toml::to_string(&toml::Value::Table(doc))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let out = format!("# UI preferences for zstats.app. An absent key follows the system.\n{body}");

    // Written through a temp file, like every other side file: a kill
    // mid-write must not leave a truncated app.toml, which reads as
    // "no preferences at all".
    fs::create_dir_all(dir)?;
    let path = file_path(dir);
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, out)?;
    fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::process;

    fn scratch(name: &str) -> PathBuf {
        env::temp_dir().join(format!("zstats-app-prefs-{name}-{}", process::id()))
    }

    #[test]
    fn round_trips_through_app_toml() {
        let dir = scratch("roundtrip");
        write(
            &dir,
            &Prefs {
                language: LanguagePref::Chinese,
                theme: ThemePref::Dark,
                opacity: Some(0.8),
                proxy: "http://127.0.0.1:7890".into(),
                analysis_roots: vec![
                    "/Users/x/Library".to_string(),
                    "/Users/x/.cache".to_string(),
                ],
                // Nothing in the UI writes this one; it still has to
                // come back out, or a theme change would eat it.
                analysis_exclude: vec!["~/github".to_string()],
            },
        )
        .unwrap();
        let back = read(&dir);
        assert_eq!(back.language, LanguagePref::Chinese);
        assert_eq!(back.theme, ThemePref::Dark);
        assert_eq!(back.opacity, Some(0.8));
        assert_eq!(back.proxy, "http://127.0.0.1:7890");
        assert_eq!(
            back.analysis_roots,
            vec![
                "/Users/x/Library".to_string(),
                "/Users/x/.cache".to_string()
            ]
        );
        assert_eq!(back.analysis_exclude, vec!["~/github".to_string()]);

        // Both back to System: the keys disappear rather than being written
        // as a third value. Same for an unset opacity.
        write(&dir, &Prefs::default()).unwrap();
        let text = fs::read_to_string(file_path(&dir)).unwrap();
        assert!(!text.contains("language"), "System should omit the key");
        assert!(
            !text.contains("opacity"),
            "unset opacity should omit the key"
        );
        assert!(!text.contains("proxy"), "empty proxy should omit the key");
        assert!(
            !text.contains("analysis_roots"),
            "the default scope should omit the key"
        );
        assert!(
            !text.contains("analysis_exclude"),
            "an empty exclusion list should omit the key"
        );
        let back = read(&dir);
        assert_eq!(back.language, LanguagePref::System);
        assert_eq!(back.theme, ThemePref::System);
        assert_eq!(back.opacity, None);
        assert!(back.proxy.is_empty());
        assert!(back.analysis_roots.is_empty());
        assert!(back.analysis_exclude.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_broken_file_reads_as_system() {
        let dir = scratch("missing");
        let system: Prefs = Default::default();
        assert_eq!(read(&dir), system);

        fs::create_dir_all(&dir).unwrap();
        fs::write(file_path(&dir), "not [valid toml").unwrap();
        assert_eq!(read(&dir), system);

        fs::write(file_path(&dir), "language = \"ja\"\ntheme = \"sepia\"\n").unwrap();
        assert_eq!(read(&dir), system);

        let _ = fs::remove_dir_all(&dir);
    }

    /// The exclusion list is the one key nothing in the UI writes, so it
    /// is also the one that a careless writer would drop — a theme
    /// change would silently un-exclude the reader's directories.
    #[test]
    fn a_hand_written_exclusion_survives_a_write_it_had_no_part_in() {
        let dir = scratch("exclude");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            file_path(&dir),
            "theme = \"dark\"\nanalysis_exclude = [\"~/github\", \"~/vicanso\"]\n",
        )
        .unwrap();

        // What a preference change does: read everything, write it back.
        let carried = read(&dir);
        write(
            &dir,
            &Prefs {
                theme: ThemePref::Light,
                ..carried
            },
        )
        .unwrap();

        let after = read(&dir);
        assert_eq!(after.theme, ThemePref::Light, "the change landed");
        assert_eq!(
            after.analysis_exclude,
            vec!["~/github".to_string(), "~/vicanso".to_string()],
            "and took nothing with it"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// macOS filenames may hold newlines and other control characters,
    /// which a hand-rolled TOML quote would emit raw — the file would
    /// then fail to parse and every preference in it would silently
    /// reset on the next launch.
    #[test]
    fn a_hostile_path_survives_the_round_trip() {
        let dir = scratch("hostile");
        let nasty = "/Users/x/we\nird \"quoted\"\\path\ttab";
        write(
            &dir,
            &Prefs {
                language: LanguagePref::Chinese,
                theme: ThemePref::Dark,
                opacity: Some(0.8),
                analysis_roots: vec![nasty.to_string()],
                ..Prefs::default()
            },
        )
        .unwrap();
        let back = read(&dir);
        assert_eq!(
            back.analysis_roots,
            vec![nasty.to_string()],
            "the path comes back whole"
        );
        assert_eq!(
            back.language,
            LanguagePref::Chinese,
            "and takes the rest with it"
        );
        assert_eq!(back.opacity, Some(0.8));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn opacity_below_the_floor_reads_as_unset() {
        let dir = scratch("opacity");
        fs::create_dir_all(&dir).unwrap();
        fs::write(file_path(&dir), "opacity = 0.3\n").unwrap();
        assert_eq!(
            read(&dir).opacity,
            None,
            "below 0.5 is the built-in default"
        );

        fs::write(file_path(&dir), "opacity = 1\n").unwrap();
        assert_eq!(read(&dir).opacity, Some(1.0));

        fs::write(file_path(&dir), "opacity = 1.4\n").unwrap();
        assert_eq!(read(&dir).opacity, Some(1.0), "above 1.0 clamps");

        write(
            &dir,
            &Prefs {
                opacity: Some(0.4),
                ..Prefs::default()
            },
        )
        .unwrap();
        let text = fs::read_to_string(file_path(&dir)).unwrap();
        assert!(!text.contains("opacity"), "the picker cannot persist < 0.5");

        let _ = fs::remove_dir_all(&dir);
    }
}
