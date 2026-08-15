//! UI preferences: language, theme, and panel opacity, persisted in `app.toml`.
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

use std::path::{Path, PathBuf};
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
/// The *applied* copy is frozen at `load()` so a mid-session edit only
/// lands after restart, matching the Interface copy.
static OPACITY_APPLIED: AtomicU8 = AtomicU8::new(0);
static OPACITY_SAVED: AtomicU8 = AtomicU8::new(0);

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

/// The opacity the Interface page shows (may differ from what is painted
/// until the next launch).
pub fn opacity() -> Option<f32> {
    decode_opacity(OPACITY_SAVED.load(Ordering::Relaxed))
}

/// Opacity frozen at startup. `None` → caller uses the mode default.
pub fn applied_opacity() -> Option<f32> {
    decode_opacity(OPACITY_APPLIED.load(Ordering::Relaxed))
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
    let (language, theme, opacity) = read(&zstats::settings::default_dir());
    LANGUAGE.store(encode_language(language), Ordering::Relaxed);
    THEME.store(encode_theme(theme), Ordering::Relaxed);
    let encoded = encode_opacity(opacity);
    OPACITY_APPLIED.store(encoded, Ordering::Relaxed);
    OPACITY_SAVED.store(encoded, Ordering::Relaxed);
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

/// Remember and persist a panel opacity. Does not change what this
/// process paints — `load()` is the only thing that fills the applied
/// copy.
pub fn set_opacity(value: Option<f32>) {
    OPACITY_SAVED.store(encode_opacity(value), Ordering::Relaxed);
    persist();
}

fn persist() {
    let dir = zstats::settings::default_dir();
    if let Err(e) = write(&dir, language(), theme(), opacity()) {
        eprintln!("could not write {}: {e}", file_path(&dir).display());
    }
}

fn file_path(dir: &Path) -> PathBuf {
    dir.join("app.toml")
}

fn read(dir: &Path) -> (LanguagePref, ThemePref, Option<f32>) {
    let Ok(text) = std::fs::read_to_string(file_path(dir)) else {
        return Default::default();
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return Default::default();
    };
    let get = |key: &str| table.get(key).and_then(|v| v.as_str());
    (
        get("language").map_or_else(Default::default, LanguagePref::from_key),
        get("theme").map_or_else(Default::default, ThemePref::from_key),
        table.get("opacity").and_then(parse_opacity),
    )
}

/// `None` when the key is missing, unparsable, or below [`OPACITY_MIN`].
fn parse_opacity(value: &toml::Value) -> Option<f32> {
    let n = value
        .as_float()
        .or_else(|| value.as_integer().map(|i| i as f64))? as f32;
    (n >= OPACITY_MIN).then_some(n.min(OPACITY_MAX))
}

fn write(
    dir: &Path,
    language: LanguagePref,
    theme: ThemePref,
    opacity: Option<f32>,
) -> std::io::Result<()> {
    let mut out =
        String::from("# UI preferences for zstats.app. An absent key follows the system.\n");
    if let Some(key) = language.key() {
        out.push_str(&format!("language = \"{key}\"\n"));
    }
    if let Some(key) = theme.key() {
        out.push_str(&format!("theme = \"{key}\"\n"));
    }
    if let Some(value) = opacity.filter(|v| *v >= OPACITY_MIN) {
        out.push_str(&format!("opacity = {:.2}\n", value.min(OPACITY_MAX)));
    }
    std::fs::create_dir_all(dir)?;
    std::fs::write(file_path(dir), out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("zstats-app-prefs-{name}-{}", std::process::id()))
    }

    #[test]
    fn round_trips_through_app_toml() {
        let dir = scratch("roundtrip");
        write(&dir, LanguagePref::Chinese, ThemePref::Dark, Some(0.8)).unwrap();
        assert_eq!(
            read(&dir),
            (LanguagePref::Chinese, ThemePref::Dark, Some(0.8))
        );

        // Both back to System: the keys disappear rather than being written
        // as a third value. Same for an unset opacity.
        write(&dir, LanguagePref::System, ThemePref::System, None).unwrap();
        let text = std::fs::read_to_string(file_path(&dir)).unwrap();
        assert!(!text.contains("language"), "System should omit the key");
        assert!(
            !text.contains("opacity"),
            "unset opacity should omit the key"
        );
        assert_eq!(read(&dir), (LanguagePref::System, ThemePref::System, None));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_broken_file_reads_as_system() {
        let dir = scratch("missing");
        assert_eq!(read(&dir), (LanguagePref::System, ThemePref::System, None));

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(file_path(&dir), "not [valid toml").unwrap();
        assert_eq!(read(&dir), (LanguagePref::System, ThemePref::System, None));

        std::fs::write(file_path(&dir), "language = \"ja\"\ntheme = \"sepia\"\n").unwrap();
        assert_eq!(read(&dir), (LanguagePref::System, ThemePref::System, None));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opacity_below_the_floor_reads_as_unset() {
        let dir = scratch("opacity");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(file_path(&dir), "opacity = 0.3\n").unwrap();
        assert_eq!(read(&dir).2, None, "below 0.5 is the built-in default");

        std::fs::write(file_path(&dir), "opacity = 1\n").unwrap();
        assert_eq!(read(&dir).2, Some(1.0));

        std::fs::write(file_path(&dir), "opacity = 1.4\n").unwrap();
        assert_eq!(read(&dir).2, Some(1.0), "above 1.0 clamps");

        write(&dir, LanguagePref::System, ThemePref::System, Some(0.4)).unwrap();
        let text = std::fs::read_to_string(file_path(&dir)).unwrap();
        assert!(!text.contains("opacity"), "the picker cannot persist < 0.5");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
