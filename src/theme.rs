//! Design tokens for a macOS menu-bar panel.
//!
//! Fills are translucent so the Popover vibrancy shows through. The classic
//! pair — dark (white washes) and light (black washes) — is what gpui-kit's
//! default theme paints. Golden is a second pair, the warm bone / champagne
//! tokens published as CSS variables on <https://notchclip.buildmac.app/>
//! (`--bone`, `--paper`, `--ink`, …). The gold is `#d9b98c`
//! (`rgb(217 185 140)`), the dark half of `--accent`, on both variants.
//! The site's light half `#8a5a2b` is the contrast colour for text on
//! bone; painted as a checked switch it reads as brown, so the light
//! variant uses the champagne too, with the dark `--on-accent`
//! (`#14110e`) on that fill. The same hexes live in [`GOLDEN_JSON`] for
//! gpui-kit's own widgets (the window ground, switches, focus rings).
//! These functions paint the panel's cards, which that file does not.
//!
//! Brand red stays the over-threshold colour in every scheme, golden
//! included. Champagne is the theme's own mark — a checked switch, a
//! selected chip, the active tab — because a hot bar that turned gold
//! would read as the theme's brand rather than a crossed line.
//!
//! Selected by [`set_dark`] / [`set_golden`] after every `Theme::change`.

use crate::i18n;
use gpui::{Rgba, rgb, rgba};
use std::sync::atomic::{AtomicU8, Ordering};

/// gpui-kit theme file for the golden pair. Loaded into `ThemeRegistry`
/// when the preference is Golden; the names `Golden Light` / `Golden Dark`
/// are what [`crate::apply_appearance`] looks up.
pub const GOLDEN_JSON: &str = include_str!("../themes/golden.json");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scheme {
    ClassicDark = 0,
    ClassicLight = 1,
    GoldenDark = 2,
    GoldenLight = 3,
}

impl Scheme {
    fn is_dark(self) -> bool {
        matches!(self, Scheme::ClassicDark | Scheme::GoldenDark)
    }
}

// Classic dark is the untouched default, matching the old `DARK = true`
// before the first `apply_appearance`.
static SCHEME: AtomicU8 = AtomicU8::new(0);

fn scheme() -> Scheme {
    match SCHEME.load(Ordering::Relaxed) {
        1 => Scheme::ClassicLight,
        2 => Scheme::GoldenDark,
        3 => Scheme::GoldenLight,
        _ => Scheme::ClassicDark,
    }
}

fn set_scheme(scheme: Scheme) {
    SCHEME.store(scheme as u8, Ordering::Relaxed);
}

/// Classic palette. Call after every `Theme::change`.
pub fn set_dark(dark: bool) {
    set_scheme(if dark {
        Scheme::ClassicDark
    } else {
        Scheme::ClassicLight
    });
}

/// Golden palette, light or dark to match the appearance `Theme::change`
/// just applied. Same call site as [`set_dark`].
pub fn set_golden(dark: bool) {
    set_scheme(if dark {
        Scheme::GoldenDark
    } else {
        Scheme::GoldenLight
    });
}

pub fn is_dark() -> bool {
    scheme().is_dark()
}

/// `classic_dark`, `classic_light`, `golden_dark`, `golden_light`, each
/// `0xRRGGBBAA`.
fn tone(classic_dark: u32, classic_light: u32, golden_dark: u32, golden_light: u32) -> Rgba {
    rgba(match scheme() {
        Scheme::ClassicDark => classic_dark,
        Scheme::ClassicLight => classic_light,
        Scheme::GoldenDark => golden_dark,
        Scheme::GoldenLight => golden_light,
    })
}

/// Card / grouped fill. Dark: faint white lift over the glass. Light:
/// nearly solid white, so cards sit on the grey panel the way Settings
/// groups do.
///
/// The dark lift is safe to stay translucent because the ground under it
/// is guaranteed: `use_popover_material` (main.rs) puts a stock
/// popover-material `NSVisualEffectView` under the content, whose
/// luminosity clamp keeps the backdrop dark over any wallpaper — the
/// same mechanism that keeps a system menu readable on a white desktop.
/// A brief detour made these cards nearly solid instead; it fixed white
/// wallpapers by killing the glass everywhere else, which is the wrong
/// trade when the OS offers the right one.
#[inline]
pub fn surface() -> Rgba {
    // Golden light is `--paper` at the same near-solid alpha the classic
    // light card uses. Golden dark is `--wash` (`#f5efe612`), the warm
    // equivalent of the white lift — still translucent, so the glass stays.
    tone(0xffffff12, 0xfffffff2, 0xf5efe612, 0xfbf8f3f2)
}

/// Tab-strip well. Dark: same language as a card. Light: a grey trough so
/// the selected chip can be white.
#[inline]
pub fn trough() -> Rgba {
    // Golden light: `--bone-deep`, the well a paper chip sits in.
    tone(0xffffff12, 0x0000000a, 0xf5efe612, 0xe8dfd2ff)
}

/// Selected tab chip.
#[inline]
pub fn chip() -> Rgba {
    // Golden dark chip is `--line` (`#f5efe61f`). Golden light is `--paper`.
    tone(0xffffff1f, 0xffffffff, 0xf5efe61f, 0xfbf8f3ff)
}

/// Hover / expanded rows. Dark: lift. Light: a grey wash on the white card.
#[inline]
pub fn surface_raised() -> Rgba {
    // Golden: `--wash-strong` on dark (`#ffffff1a`), `--wash` on light.
    tone(0xffffff1f, 0x0000000e, 0xffffff1a, 0x1a17140f)
}

/// Recessed fill: nested detail blocks, meter tracks.
#[inline]
pub fn inset() -> Rgba {
    // Golden light recess is `--bone-deep`. Golden dark keeps the classic
    // black wash: the site's dark bone-deep (`#0e0c0a`) is a solid, and a
    // solid recess would turn every meter track into a black bar.
    tone(0x00000040, 0x0000000c, 0x00000040, 0xe8dfd2ff)
}

/// Hairline outlines and meter tracks.
#[inline]
pub fn border() -> Rgba {
    // Golden dark hairline is `--line`. Golden light is `--line-solid`,
    // the site's card edge, which is a real warm line rather than a
    // faint black wash.
    tone(0xffffff22, 0x00000012, 0xf5efe61f, 0xe2d8c9ff)
}

/// Row separators — dimmer than [`border`].
#[inline]
pub fn border_subtle() -> Rgba {
    // `--line-soft` on dark, `--line` on light.
    tone(0xffffff14, 0x00000014, 0xf5efe612, 0x1a171424)
}

/// Primary text.
#[inline]
pub fn text() -> Rgba {
    // Golden is `--ink`.
    tone(0xfafafaff, 0x1d1d1fff, 0xf5efe6ff, 0x1a1714ff)
}

/// Secondary text: units, captions, inactive tabs.
#[inline]
pub fn text_muted() -> Rgba {
    // Golden is `--ink-soft`.
    tone(0xa1a1aaff, 0x6e6e73ff, 0xa89f93ff, 0x5a534bff)
}

/// Tertiary text: field labels, footnotes.
#[inline]
pub fn text_dim() -> Rgba {
    // Classic keeps one grey in both modes. Golden steps down to
    // `--ink-faint`, the site's last published ink.
    tone(0x8e8e93ff, 0x8e8e93ff, 0x968d81ff, 0x645d54ff)
}

/// Ink for a tiny fixed label — the ≤9.5px pills, tags and footnote
/// sentences: `base` in a Latin locale, full [`text`] in a CJK one.
///
/// The concession exists because small Han glyphs lose on four fronts at
/// once: the UI family (SF Pro) has no Han glyphs, so Chinese renders in
/// PingFang — a fallback with no optical-size axis, meaning the English
/// half of a line gets small-size stem thickening and the Chinese half
/// does not; a Han glyph packs ~20 strokes into the box a Latin letter
/// spends on three, so at 9px @2x single strokes antialias into grey;
/// gpui rasterises grayscale-only on macOS (no subpixel sharpening); and
/// the panel is glass, so those grey edges composite against wallpaper.
/// Raising the *size* was tried and reverted — one point times gpui's
/// 1.618 line box moved the whole panel's rhythm. Contrast is the lever
/// that costs no layout: muted grey Chinese at 9px is mush, full-ink
/// Chinese at 9px is small but sharp. Latin keeps the designed hierarchy.
///
/// **Labels only, never controls.** A toggle's resting colour is its
/// state (`showing ? text() : muted`), a sort chip's muted resting tone
/// is what its hover lifts from — routing those through here would lie
/// about state, so they stay on their own colours in every locale.
#[inline]
pub fn tiny_label(base: Rgba) -> Rgba {
    if i18n::is_cjk() { text() } else { base }
}

/// Quaternary text: limits, tick marks.
#[inline]
pub fn text_faint() -> Rgba {
    // Golden has no ink past `--ink-faint`, so faint and dim share it.
    tone(0x636366ff, 0xa1a1a6ff, 0x968d81ff, 0x645d54ff)
}

/// Neutral fill for bars and meters — the "nothing is wrong" state.
#[inline]
pub fn ink() -> Rgba {
    // Golden meters use `--ink`, the same role as the classic near-black
    // / near-white neutral.
    tone(0xe4e4e7ff, 0x1d1d1fff, 0xf5efe6ff, 0x1a1714ff)
}

/// Brand red. Reserved for over-threshold states and the primary action.
#[inline]
pub fn accent() -> Rgba {
    rgb(0xec3013)
}

/// Accent *text*: lighter on dark surfaces, darker on light so contrast holds.
#[inline]
pub fn accent_light() -> Rgba {
    if is_dark() {
        rgb(0xff9783)
    } else {
        rgb(0xc41e0a)
    }
}

/// Accent washes used behind warning pills and alert rows.
#[inline]
pub fn accent_wash(alpha_percent: u32) -> Rgba {
    wash(0xec3013, alpha_percent)
}

/// Whether the golden pair is the one on screen.
#[inline]
pub fn is_golden() -> bool {
    matches!(scheme(), Scheme::GoldenDark | Scheme::GoldenLight)
}

/// Notchclip's champagne, `rgb(217 185 140)`.
///
/// The site publishes it as the dark half of `--accent`. Both golden
/// variants use it as the theme's mark: a wash of it would colour-pick
/// as a blend, and the light-mode bronze reads as brown.
#[inline]
pub fn gold() -> Rgba {
    rgb(0xd9b98c)
}

/// Dark `--on-accent`. Cream on champagne has no contrast.
#[inline]
pub fn on_gold() -> Rgba {
    rgb(0x14110e)
}

/// Fill of a selected option chip. Classic keeps the faint brand wash.
/// Golden is solid [`gold`].
#[inline]
pub fn choice_fill(on: bool) -> Rgba {
    if !on {
        inset()
    } else if is_golden() {
        gold()
    } else {
        accent_wash(10)
    }
}

/// Border of a selected option chip. Same split as [`choice_fill`].
#[inline]
pub fn choice_line(on: bool) -> Rgba {
    if !on {
        border()
    } else if is_golden() {
        gold()
    } else {
        accent_wash(45)
    }
}

/// Label on a selected option chip. Golden uses [`on_gold`] because the
/// fill is the champagne itself.
#[inline]
pub fn choice_ink(on: bool) -> Rgba {
    if !on {
        text()
    } else if is_golden() {
        on_gold()
    } else {
        accent_light()
    }
}

/// Hover fill for an option chip. A selected golden chip stays [`gold`]:
/// the label is [`on_gold`], and the usual raised wash would put that
/// near-black on a dark card.
#[inline]
pub fn choice_hover(on: bool) -> Rgba {
    if on && is_golden() {
        gold()
    } else {
        surface_raised()
    }
}

fn wash(rgb: u32, alpha_percent: u32) -> Rgba {
    let alpha = alpha_percent.clamp(0, 100) * 255 / 100;
    rgba((rgb << 8) | alpha)
}

/// The single rule the design applies everywhere: a bar, meter or number is
/// neutral until it crosses its threshold, and only then turns accent.
#[inline]
pub fn fill_for(over: bool) -> Rgba {
    if over { accent() } else { ink() }
}

/// Same rule for text, which needs the lighter/darker accent to stay readable.
#[inline]
pub fn text_for(over: bool) -> Rgba {
    if over { accent_light() } else { text() }
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
            assert!(allowed.contains(&rgb), "#{hex} is not a Notchclip token");
            i = start + hex_len;
        }
        // `#8a5a2b` is a published token (the light half of `--accent`)
        // and stays in the allowlist, but the file must not use it: filled
        // in, it reads as brown rather than the champagne above.
        assert!(
            !json.contains("8a5a2b"),
            "golden mark is #d9b98c on both variants"
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
                    assert_eq!(theme.colors.background.as_deref(), Some("#14110e"));
                    assert_eq!(theme.colors.primary.as_deref(), Some("#d9b98c"));
                    dark = true;
                }
                other => panic!("unexpected theme {other}"),
            }
        }
        assert!(light && dark, "the file is a pair, like macos-classic");
    }

    #[test]
    fn golden_light_paints_paper_and_keeps_the_alarm_red() {
        set_golden(false);
        assert!(!is_dark());
        assert_eq!(surface(), rgba(0xfbf8f3f2));
        assert_eq!(text(), rgba(0x1a1714ff));
        // The threshold colour stays brand red. The selected chip is the
        // champagne, solid, with the dark on-accent for its label.
        assert_eq!(accent(), rgb(0xec3013));
        assert_eq!(choice_fill(true), gold());
        assert_eq!(choice_ink(true), on_gold());
        assert_eq!(choice_hover(true), gold());
        assert_eq!(gold(), rgb(0xd9b98c));
        set_dark(false);
        assert!(!is_golden());
        assert_eq!(choice_fill(true), accent_wash(10));
        set_dark(true);
        assert!(is_dark());
        assert_eq!(surface(), rgba(0xffffff12));
    }
}
