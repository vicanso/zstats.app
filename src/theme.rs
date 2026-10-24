//! The palette (`palette.rs`) as gpui colours, and the theme files
//! gpui-kit's own widgets are painted with.
//!
//! The tokens — which scheme is on screen and what every role is in each,
//! with the reasoning behind them — are shared with the NSH front end and
//! live in `palette.rs` (docs/nsh-migration.md, phase 1b). This module is
//! the gpui front end's edge: each token converted to gpui's `Rgba`,
//! [`GOLDEN_JSON`], and the Omarchy theme generated for gpui-kit
//! ([`omarchy_theme`]).

use crate::color;
use crate::omarchy::Palette;
use crate::palette::{self, OmarchyTokens};
use gpui::Rgba;

pub use crate::palette::{omarchy_palette_is, set_dark, set_golden, set_omarchy, window_opacity};

/// gpui-kit theme file for the golden pair. Loaded into `ThemeRegistry`
/// when the preference is Golden; the names `Golden Light` / `Golden Dark`
/// are what [`crate::gpui_app::apply_appearance`] looks up.
pub const GOLDEN_JSON: &str = include_str!("../themes/golden.json");

/// Tokens with nothing to choose: `palette.rs` documents each.
macro_rules! tokens {
    ($($name:ident),* $(,)?) => {$(
        #[inline]
        pub fn $name() -> Rgba {
            palette::$name().into()
        }
    )*};
}

tokens!(
    surface,
    trough,
    chip,
    surface_raised,
    inset,
    border,
    border_subtle,
    text,
    text_muted,
    text_dim,
    text_faint,
    ink,
    accent,
    accent_light,
    gauge,
    track,
    card_line,
);

/// `palette::tiny_label`.
#[inline]
pub fn tiny_label(base: Rgba) -> Rgba {
    palette::tiny_label(base.into()).into()
}

/// `palette::accent_wash`.
#[inline]
pub fn accent_wash(alpha_percent: u32) -> Rgba {
    palette::accent_wash(alpha_percent).into()
}

/// `palette::mark`.
#[inline]
pub fn mark() -> Option<(Rgba, Rgba)> {
    palette::mark().map(|(fill, ink)| (fill.into(), ink.into()))
}

/// `palette::selected`.
#[inline]
pub fn selected() -> Option<(Rgba, Rgba)> {
    palette::selected().map(|(fill, ink)| (fill.into(), ink.into()))
}

/// `palette::gauge_for`.
#[inline]
pub fn gauge_for(over: bool) -> Rgba {
    palette::gauge_for(over).into()
}

/// `palette::choice_fill`.
#[inline]
pub fn choice_fill(on: bool) -> Rgba {
    palette::choice_fill(on).into()
}

/// `palette::choice_line`.
#[inline]
pub fn choice_line(on: bool) -> Rgba {
    palette::choice_line(on).into()
}

/// `palette::choice_ink`.
#[inline]
pub fn choice_ink(on: bool) -> Rgba {
    palette::choice_ink(on).into()
}

/// `palette::choice_hover`.
#[inline]
pub fn choice_hover(on: bool) -> Rgba {
    palette::choice_hover(on).into()
}

/// `palette::fill_for`.
#[inline]
pub fn fill_for(over: bool) -> Rgba {
    palette::fill_for(over).into()
}

/// `palette::text_for`.
#[inline]
pub fn text_for(over: bool) -> Rgba {
    palette::text_for(over).into()
}

/// gpui-kit's own widgets — the window ground, switches, inputs, focus
/// rings — painted from an Omarchy palette: the role [`GOLDEN_JSON`]
/// plays for Golden, generated instead of written. Returns the theme's
/// name and the theme-set JSON to load.
///
/// The name carries a fingerprint of the colours because
/// `ThemeRegistry::load_themes_from_str` keeps the first theme of a name
/// and silently skips a later one: under a fixed name, a theme switch on
/// the desktop would keep painting the first palette of the session.
/// Each distinct palette is one small registry entry; the same palette
/// read again is the same name and loads nothing.
pub fn omarchy_theme(p: &Palette) -> (String, String) {
    let t = OmarchyTokens::from_palette(p);
    let on = |fill: color::Rgba| {
        if palette::contrast(fill, color::rgb(0x000000))
            > palette::contrast(fill, color::rgb(0xffffff))
        {
            color::rgb(0x000000)
        } else {
            color::rgb(0xffffff)
        }
    };
    let active_wash = color::Rgba {
        a: 0x15 as f32 / 255.0,
        ..p.accent
    };
    let colors = [
        ("background", p.background),
        ("foreground", p.foreground),
        ("border", t.border),
        ("ring", p.accent),
        ("caret", p.foreground),
        ("link", p.accent),
        // A step off the surface, never the surface itself: gpui-kit
        // paints a menu's hovered item in this, on the popover fill
        // below. The same step the classic dark theme takes (≈12%
        // toward the ink) — the chip's.
        ("accent.background", t.chip),
        ("accent.foreground", p.foreground),
        ("primary.background", p.accent),
        ("primary.foreground", t.on_mark),
        ("danger.background", p.red),
        ("danger.foreground", on(p.red)),
        ("success.background", p.green),
        ("warning.background", p.yellow),
        ("info.background", p.accent),
        ("muted.background", p.surface),
        ("muted.foreground", t.text_muted),
        ("popover.background", p.surface),
        ("popover.foreground", p.foreground),
        ("secondary.background", p.surface),
        ("secondary.foreground", p.foreground),
        ("secondary.hover.background", t.surface_raised),
        ("secondary.active.background", t.border),
        ("list.hover.background", t.surface_raised),
        ("list.even.background", p.surface),
        ("list.active.background", active_wash),
        ("list.active.border", p.accent),
        ("selection.background", p.selection),
        ("input.border", t.border),
        ("switch.background", t.border),
        ("switch.thumb.background", p.background),
        ("tab.background", p.inset),
        ("tab.foreground", t.text_muted),
        ("tab.active.background", p.background),
        ("tab.active.foreground", p.foreground),
        ("tab_bar.background", p.inset),
        ("title_bar.background", p.inset),
        ("title_bar.border", t.border),
        ("status_bar.background", p.inset),
        ("scrollbar.background", color::rgba(0x00000000)),
        ("scrollbar.thumb.background", t.text_dim),
        ("base.red", p.red),
        ("base.green", p.green),
        ("base.yellow", p.yellow),
        ("base.blue", p.accent),
        ("base.cyan", p.accent),
        ("base.magenta", p.accent),
    ];
    let body = colors
        .iter()
        .map(|(key, color)| format!("\"{key}\": \"{}\"", hex(*color)))
        .collect::<Vec<_>>()
        .join(", ");
    let mode = if p.dark { "dark" } else { "light" };
    // FNV-1a over everything the theme paints, mode included.
    let fingerprint = format!("{mode} {body}")
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    let name = format!("Omarchy {fingerprint:016x}");
    let json = format!(
        "{{\"name\": \"Omarchy\", \"author\": \"zstats-app\", \"themes\": [{{\"name\": \"{name}\", \"mode\": \"{mode}\", \"shadow\": false, \"colors\": {{{body}}}}}]}}"
    );
    (name, json)
}

/// `#rrggbb`, or `#rrggbbaa` when not opaque.
fn hex(color: color::Rgba) -> String {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let (r, g, b, a) = (byte(color.r), byte(color.g), byte(color.b), byte(color.a));
    if a == 0xff {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every hex in the theme file is a Notchclip `:root` token (or
    /// transparent black). An alpha suffix is allowed — macos-classic
    /// writes list selection the same way — but the RGB is not.
    #[test]
    fn golden_json_uses_notchclip_tokens_only() {
        // light-dark() pairs from notchclip.buildmac.app, plus black and
        // white which the site itself uses for washes and shadows.
        let allowed = [
            "f3eee6", "e8dfd2", "fbf8f3", "1a1714", "5a534b", "645d54", "8a5a2b", "2f6f3e",
            "7a5a0a", "e2d8c9", "a8321e", "14110e", "0e0c0a", "1c1814", "f5efe6", "a89f93",
            "968d81", "d9b98c", "5cc98a", "e8c26a", "332c25", "ff9a80", "ffffff", "000000",
            // The dark half's own palette (`.claude/skills/ui-polish`).
            "1c1c1e", "252528", "36363a", "f2f0eb", "a19d96", "8f8b85", "d9ae7a", "6ec48a",
        ];
        let json = GOLDEN_JSON.to_ascii_lowercase();
        let mut i = 0;
        while let Some(rel) = json[i..].find('#') {
            let start = i + rel + 1;
            let hex_len = json[start..]
                .chars()
                .take_while(|c| c.is_ascii_hexdigit())
                .count();
            let hex = &json[start..start + hex_len];
            let rgb = match hex.len() {
                6 => hex,
                8 => &hex[..6],
                other => panic!("#{hex} has {other} digits; want 6 or 8"),
            };
            assert!(allowed.contains(&rgb), "#{hex} is not a published token");
            i = start + hex_len;
        }
        // `#8a5a2b` is a published token (the light half of `--accent`)
        // and stays in the allowlist, but the file must not use it: filled
        // in, it reads as brown rather than the champagne above.
        assert!(
            !json.contains("8a5a2b"),
            "golden light's mark is the champagne, not the bronze"
        );
    }

    #[test]
    fn golden_json_loads_as_a_light_and_dark_pair() {
        let mut registry = gpui_kit::component::ThemeRegistry::default();
        registry
            .load_themes_from_str(GOLDEN_JSON)
            .expect("golden.json is a gpui-kit theme set");
        let mut light = false;
        let mut dark = false;
        for theme in registry.themes().values() {
            match theme.name.as_ref() {
                "Golden Light" => {
                    assert!(!theme.mode.is_dark());
                    assert_eq!(theme.colors.background.as_deref(), Some("#f3eee6"));
                    // The light half of the site's `--accent` is bronze.
                    // The mark on both variants is the champagne the page
                    // shows as its gold.
                    assert_eq!(theme.colors.primary.as_deref(), Some("#d9b98c"));
                    assert_eq!(theme.colors.primary_foreground.as_deref(), Some("#14110e"));
                    light = true;
                }
                "Golden Dark" => {
                    assert!(theme.mode.is_dark());
                    // The dark half is the ui-polish palette, not the
                    // site's: its ground and its one accent.
                    assert_eq!(theme.colors.background.as_deref(), Some("#1c1c1e"));
                    assert_eq!(theme.colors.primary.as_deref(), Some("#d9ae7a"));
                    dark = true;
                }
                other => panic!("unexpected theme {other}"),
            }
        }
        assert!(light && dark, "the file is a pair, like macos-classic");
    }

    /// gpui-kit paints a menu's hovered item in `accent.background`, on
    /// `popover.background`. Golden Dark had both at `#1c1814` and the
    /// generated Omarchy theme had both at the palette's surface, so the
    /// footer menu's highlight was drawn and could not be seen.
    #[test]
    fn a_hovered_menu_item_differs_from_the_menu_under_it() {
        let mut registry = gpui_kit::component::ThemeRegistry::default();
        registry.load_themes_from_str(GOLDEN_JSON).unwrap();
        let (omarchy, json) = omarchy_theme(&Palette::tokyo_night());
        registry.load_themes_from_str(&json).unwrap();
        for name in ["Golden Light", "Golden Dark", omarchy.as_str()] {
            let colors = &registry.themes()[name].colors;
            let (hover, menu) = (colors.accent.as_deref(), colors.popover.as_deref());
            assert!(hover.is_some() && menu.is_some(), "{name} sets both");
            assert_ne!(hover, menu, "{name}: the highlight is the menu's fill");
        }
    }

    #[test]
    fn an_omarchy_theme_loads_and_is_named_by_its_colours() {
        let dark = Palette::tokyo_night();
        let (name, json) = omarchy_theme(&dark);
        let mut registry = gpui_kit::component::ThemeRegistry::default();
        registry
            .load_themes_from_str(&json)
            .expect("generated theme is a gpui-kit theme set");
        let theme = registry
            .themes()
            .get(name.as_str())
            .expect("loaded by name");
        assert!(theme.mode.is_dark());
        assert_eq!(theme.colors.background.as_deref(), Some("#1a1b26"));
        assert_eq!(theme.colors.primary.as_deref(), Some("#7aa2f7"));

        // The same palette is the same name, so a re-read loads nothing;
        // a changed one is a new name, because the registry would skip
        // a second theme under the old one.
        assert_eq!(omarchy_theme(&dark).0, name);
        let light = Palette {
            dark: false,
            background: color::rgb(0xfffcf0),
            foreground: color::rgb(0x100f0f),
            ..dark.clone()
        };
        let (light_name, light_json) = omarchy_theme(&light);
        assert_ne!(light_name, name);
        registry.load_themes_from_str(&light_json).unwrap();
        assert!(!registry.themes()[light_name.as_str()].mode.is_dark());
    }

    #[test]
    fn hex_writes_alpha_only_when_translucent() {
        assert_eq!(hex(color::rgb(0x1a1b26)), "#1a1b26");
        assert_eq!(hex(color::rgba(0x7aa2f715)), "#7aa2f715");
    }
}
