//! The Omarchy desktop's own colours, behind the Omarchy theme choice.
//!
//! Omarchy (basecamp/omarchy) points
//! `~/.local/state/omarchy/current/theme` at the active theme, and
//! `colors.toml` in it is the palette every Omarchy app paints from;
//! installs that predate the state directory keep it under
//! `~/.config/omarchy/current`. The locations, the key fallbacks and
//! the fallback to Tokyo Night all follow huacnlee/gpui-omarchy's
//! loader, so a theme reads the same here as in an app built on that
//! library. It is read here rather than through that crate: gpui-omarchy
//! is a whole component library pinned to `gpui-kit =0.7.0`, and both it
//! and gpui-component own gpui-base's global colours — two owners of
//! one global would repaint each other. The panel needs one file.
//!
//! Both palette formats parse: the semantic one Omarchy 4 ships
//! (`accent`, `red`, `lighter_background`, …) and the ANSI one before
//! it (`color0`–`color15`). A palette missing a required role, or with
//! any value that is not `#RRGGBB`, is rejected whole — a half-read
//! theme mixed with defaults would be a palette nobody designed.
//!
//! Read on demand, never watched: the panel re-reads it each time it
//! opens (`crate::refresh_omarchy_palette`), and on Wayland the panel is
//! rebuilt on every open anyway. A theme switch shows up the next time
//! the panel does, which is the only time anyone is looking.

use gpui::{Rgba, rgb};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One Omarchy theme, reduced to the roles the panel paints with.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// `theme.name`, or `Omarchy` when the theme has none.
    pub name: String,
    pub dark: bool,
    pub background: Rgba,
    pub foreground: Rgba,
    /// The theme's mark: the selected tab, a checked switch.
    pub accent: Rgba,
    /// Cards. `lighter_background`, or a step from the background
    /// toward the foreground when the theme has none.
    pub surface: Rgba,
    /// Meter tracks and chart wells. `dark_background`, or a step past
    /// the surface.
    pub inset: Rgba,
    pub selection: Rgba,
    /// Over-threshold. The theme's own red, so an alarm reads as red in
    /// the desktop's own hue rather than as a second brand beside it.
    pub red: Rgba,
    pub bright_red: Rgba,
    pub green: Rgba,
    pub yellow: Rgba,
}

impl Palette {
    /// Omarchy's default theme, and what every read failure becomes —
    /// the same fallback gpui-omarchy uses. The values are Omarchy 4's
    /// `themes/tokyo-night/colors.toml`.
    pub fn tokyo_night() -> Self {
        let background = rgb(0x1a1b26);
        let foreground = rgb(0xa9b1d6);
        Palette {
            name: "Tokyo Night".into(),
            dark: true,
            background,
            foreground,
            accent: rgb(0x7aa2f7),
            surface: rgb(0x24283b),
            inset: rgb(0x13141c),
            selection: rgb(0x292e42),
            red: rgb(0xf7768e),
            bright_red: rgb(0xff7a93),
            green: rgb(0x9ece6a),
            yellow: rgb(0xe0af68),
        }
    }

    /// Parse a `colors.toml`. `name` is the theme's display name.
    pub fn from_colors_toml(name: &str, contents: &str) -> Result<Self, String> {
        let table: toml::Table = contents
            .parse()
            .map_err(|e: toml::de::Error| e.to_string())?;
        // The first key present wins; a present key that is not a
        // `#RRGGBB` string is an error, not a reason to try the next.
        let color = |keys: &[&str]| -> Result<Option<Rgba>, String> {
            for key in keys {
                let Some(value) = table.get(*key) else {
                    continue;
                };
                let text = value
                    .as_str()
                    .ok_or_else(|| format!("{key} must be a hex colour"))?;
                return parse_hex(text)
                    .map(Some)
                    .ok_or_else(|| format!("{key} must be #RRGGBB, got {text}"));
            }
            Ok(None)
        };
        let required = |keys: &[&str]| -> Result<Rgba, String> {
            color(keys)?.ok_or_else(|| format!("missing {}", keys[0]))
        };
        let background = required(&["background"])?;
        let foreground = required(&["foreground"])?;
        let accent = required(&["accent"])?;
        let red = required(&["red", "color1"])?;
        let dark = match table.get("mode") {
            None => luminance(background) <= luminance(foreground),
            Some(toml::Value::String(mode)) if mode == "dark" => true,
            Some(toml::Value::String(mode)) if mode == "light" => false,
            Some(_) => return Err("mode must be dark or light".into()),
        };
        Ok(Palette {
            name: name.to_owned(),
            dark,
            background,
            foreground,
            accent,
            surface: color(&["lighter_background"])?
                .unwrap_or_else(|| mix(background, foreground, 0.05)),
            inset: color(&["dark_background"])?
                .unwrap_or_else(|| mix(background, foreground, 0.08)),
            selection: color(&["selection", "selection_background"])?
                .unwrap_or_else(|| mix(background, accent, 0.2)),
            red,
            bright_red: color(&["bright_red", "color9"])?.unwrap_or(red),
            green: color(&["green", "color2"])?.unwrap_or(accent),
            yellow: color(&["yellow", "color3"])?.unwrap_or(accent),
        })
    }
}

/// The `current` directory Omarchy keeps under `home`, if any. The
/// state directory wins whenever it exists — even broken, it must not
/// revive a stale theme left in the legacy one by an upgrade.
fn current_dir(home: &Path) -> Option<PathBuf> {
    let state = home.join(".local/state/omarchy/current");
    match fs::symlink_metadata(&state) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let legacy = home.join(".config/omarchy/current");
            fs::symlink_metadata(&legacy).is_ok().then_some(legacy)
        }
        _ => Some(state),
    }
}

fn home() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Whether this session runs on Omarchy: Linux, with Omarchy's current
/// theme directory in place. That directory is what the choice paints
/// from, so its presence is the honest test — a hostname or a package
/// name would say nothing about whether there is a palette to read.
pub fn installed() -> bool {
    cfg!(target_os = "linux") && home().is_some_and(|home| current_dir(&home).is_some())
}

/// Whether the Omarchy choice is offered. On Omarchy, and in every
/// development build, so the scheme can be exercised from any machine —
/// where it paints [`Palette::tokyo_night`], the same fallback a broken
/// Omarchy theme gets.
pub fn selectable() -> bool {
    cfg!(debug_assertions) || installed()
}

/// The current Omarchy palette, or Tokyo Night when there is none to
/// read. A failure is logged once per distinct message: the panel
/// re-reads on every open, and a broken file would otherwise log on
/// each one.
pub fn palette() -> Palette {
    match home().as_deref().map(load) {
        Some(Ok(palette)) => palette,
        Some(Err(err)) => {
            warn_once(&err);
            Palette::tokyo_night()
        }
        None => Palette::tokyo_night(),
    }
}

fn load(home: &Path) -> Result<Palette, String> {
    let dir = current_dir(home).ok_or_else(|| "no Omarchy theme directory".to_string())?;
    let file = dir.join("theme/colors.toml");
    let contents = fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let name = fs::read_to_string(dir.join("theme.name"))
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Omarchy".into());
    Palette::from_colors_toml(&name, &contents).map_err(|e| format!("{}: {e}", file.display()))
}

/// Log a read failure the first time it is seen. "No Omarchy theme
/// directory" is the expected answer off Omarchy (a development build
/// testing the scheme), so it stays at debug.
fn warn_once(err: &str) {
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_deref() == Some(err) {
        return;
    }
    if installed() {
        tracing::warn!("omarchy theme: {err}; painting Tokyo Night");
    } else {
        tracing::debug!("omarchy theme: {err}; painting Tokyo Night");
    }
    *last = Some(err.to_owned());
}

fn parse_hex(text: &str) -> Option<Rgba> {
    let hex = text.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok().map(rgb)
}

/// `a` moved `amount` of the way to `b`, opaque.
pub fn mix(a: Rgba, b: Rgba, amount: f32) -> Rgba {
    Rgba {
        r: a.r + (b.r - a.r) * amount,
        g: a.g + (b.g - a.g) * amount,
        b: a.b + (b.b - a.b) * amount,
        a: 1.0,
    }
}

/// WCAG relative luminance.
pub fn luminance(color: Rgba) -> f32 {
    let linear = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Omarchy 4's `themes/tokyo-night/colors.toml`, abridged.
    const SEMANTIC: &str = r##"mode = "dark"
accent = "#7aa2f7"
selection = "#292e42"
muted = "#414868"
background = "#1a1b26"
dark_background = "#13141c"
lighter_background = "#24283b"
foreground = "#a9b1d6"
red = "#f7768e"
yellow = "#e0af68"
green = "#9ece6a"
bright_red = "#ff7a93"
"##;

    fn scratch(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = env::temp_dir().join(format!("zstats-omarchy-{tag}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_semantic_palette_keeps_its_explicit_roles() {
        let palette = Palette::from_colors_toml("Tokyo Night", SEMANTIC).unwrap();
        assert_eq!(palette, Palette::tokyo_night());
    }

    #[test]
    fn an_ansi_palette_reads_its_numbered_colours_and_infers_light() {
        // Omarchy 3 style: no mode, no semantic red, a light ground.
        let ansi = r##"background = "#fffcf0"
foreground = "#100f0f"
accent = "#205ea6"
color1 = "#af3029"
color2 = "#526600"
color3 = "#855b00"
"##;
        let palette = Palette::from_colors_toml("Flexoki Light", ansi).unwrap();
        assert!(!palette.dark);
        assert_eq!(palette.red, rgb(0xaf3029));
        assert_eq!(palette.bright_red, palette.red);
        assert_eq!(palette.green, rgb(0x526600));
        // No `lighter_background`: the card steps toward the ink.
        assert_eq!(
            palette.surface,
            mix(palette.background, palette.foreground, 0.05)
        );
    }

    #[test]
    fn a_broken_palette_is_rejected_whole() {
        for bad in [
            "",
            "background = \"#000000\"",
            "background = \"#000\"\nforeground = \"#ffffff\"\naccent = \"#ff0000\"\nred = \"#ff0000\"",
            "background = 1\nforeground = \"#ffffff\"\naccent = \"#ff0000\"\nred = \"#ff0000\"",
            "mode = \"dim\"\nbackground = \"#000000\"\nforeground = \"#ffffff\"\naccent = \"#ff0000\"\nred = \"#ff0000\"",
        ] {
            assert!(Palette::from_colors_toml("x", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_state_directory_wins_and_a_broken_one_does_not_revive_the_legacy() {
        let home = scratch("home");
        assert_eq!(current_dir(&home), None);

        let legacy = home.join(".config/omarchy/current");
        fs::create_dir_all(legacy.join("theme")).unwrap();
        fs::write(legacy.join("theme/colors.toml"), SEMANTIC).unwrap();
        assert_eq!(current_dir(&home), Some(legacy.clone()));
        assert_eq!(load(&home).unwrap().name, "Omarchy");

        let state = home.join(".local/state/omarchy/current");
        fs::create_dir_all(state.join("theme")).unwrap();
        fs::write(state.join("theme.name"), "Kanagawa\n").unwrap();
        fs::write(state.join("theme/colors.toml"), "broken").unwrap();
        assert_eq!(current_dir(&home), Some(state.clone()));
        // Broken state theme: an error, not the legacy one behind it.
        assert!(load(&home).is_err());

        fs::write(state.join("theme/colors.toml"), SEMANTIC).unwrap();
        assert_eq!(load(&home).unwrap().name, "Kanagawa");
        fs::remove_dir_all(&home).unwrap();
    }
}
