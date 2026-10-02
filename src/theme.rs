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
//! Omarchy is the third, and the only one not written here: its tokens
//! are derived from the desktop's current palette (`omarchy.rs`) by
//! [`OmarchyTokens::from_palette`], and gpui-kit's widgets get a theme
//! file generated from the same palette ([`omarchy_theme_json`]). It
//! has one light-or-dark, the palette's own, rather than following the
//! window appearance. Its alarm is the palette's red, not brand red: a
//! pastel desktop with a saturated #ec3013 on it reads as a second brand
//! beside the theme, and the palette's red still says "crossed a line".
//!
//! Selected by [`set_dark`] / [`set_golden`] / [`set_omarchy`] after
//! every `Theme::change`.

use crate::i18n;
use crate::omarchy::{self, Palette};
use gpui::{Rgba, rgb, rgba};
use std::sync::RwLock;
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
    /// Light or dark is the palette's, in [`OmarchyTokens::dark`].
    Omarchy = 4,
}

// Classic dark is the untouched default, matching the old `DARK = true`
// before the first `apply_appearance`.
static SCHEME: AtomicU8 = AtomicU8::new(0);

fn scheme() -> Scheme {
    match SCHEME.load(Ordering::Relaxed) {
        1 => Scheme::ClassicLight,
        2 => Scheme::GoldenDark,
        3 => Scheme::GoldenLight,
        4 => Scheme::Omarchy,
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

/// Omarchy palette, derived into tokens. Same call site as [`set_dark`];
/// light or dark is the palette's own.
pub fn set_omarchy(palette: &Palette) {
    let tokens = OmarchyTokens::from_palette(palette);
    *OMARCHY.write().unwrap_or_else(|e| e.into_inner()) = Some((palette.clone(), tokens));
    set_scheme(Scheme::Omarchy);
}

/// Whether `palette` is the one already painted — the panel re-reads it
/// on every open, and only a change is worth a restyle.
pub fn omarchy_palette_is(palette: &Palette) -> bool {
    scheme() == Scheme::Omarchy
        && OMARCHY
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|(painted, _)| painted == palette)
}

pub fn is_dark() -> bool {
    match scheme() {
        Scheme::ClassicDark | Scheme::GoldenDark => true,
        Scheme::ClassicLight | Scheme::GoldenLight => false,
        Scheme::Omarchy => omarchy_tokens().is_none_or(|t| t.dark),
    }
}

/// The last palette [`set_omarchy`] painted, with its tokens.
static OMARCHY: RwLock<Option<(Palette, OmarchyTokens)>> = RwLock::new(None);

/// The Omarchy tokens, when that scheme is on screen.
fn omarchy_tokens() -> Option<OmarchyTokens> {
    if scheme() != Scheme::Omarchy {
        return None;
    }
    OMARCHY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|(_, tokens)| *tokens)
}

/// Every token, from one Omarchy palette. The neutral layers are steps
/// from the theme's background toward its foreground, so each one keeps
/// the theme's hue; nothing is borrowed from another scheme.
#[derive(Clone, Copy, Debug, PartialEq)]
struct OmarchyTokens {
    dark: bool,
    surface: Rgba,
    trough: Rgba,
    chip: Rgba,
    surface_raised: Rgba,
    inset: Rgba,
    border: Rgba,
    border_subtle: Rgba,
    text: Rgba,
    text_muted: Rgba,
    text_dim: Rgba,
    text_faint: Rgba,
    ink: Rgba,
    accent: Rgba,
    accent_light: Rgba,
    mark: Rgba,
    on_mark: Rgba,
}

impl OmarchyTokens {
    fn from_palette(p: &Palette) -> Self {
        let toward_ink = |amount: f32| omarchy::mix(p.surface, p.foreground, amount);
        let text_step = |amount: f32| omarchy::mix(p.background, p.foreground, amount);
        // Black or white on the accent, whichever reads.
        let black = rgb(0x000000);
        let white = rgb(0xffffff);
        let on_mark = if contrast(p.accent, black) > contrast(p.accent, white) {
            black
        } else {
            white
        };
        OmarchyTokens {
            dark: p.dark,
            surface: p.surface,
            // Same language as a card, as the classic dark trough is.
            trough: p.surface,
            chip: toward_ink(0.12),
            surface_raised: toward_ink(0.07),
            inset: p.inset,
            border: toward_ink(0.16),
            border_subtle: toward_ink(0.09),
            text: p.foreground,
            text_muted: text_step(0.72),
            text_dim: text_step(0.58),
            text_faint: text_step(0.42),
            ink: p.foreground,
            accent: p.red,
            accent_light: if p.dark { p.bright_red } else { p.red },
            mark: p.accent,
            on_mark,
        }
    }
}

fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (a, b) = (omarchy::luminance(a), omarchy::luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `classic_dark`, `classic_light`, `golden_dark`, `golden_light`, each
/// `0xRRGGBBAA`, and which Omarchy token stands in that role.
fn tone(
    classic_dark: u32,
    classic_light: u32,
    golden_dark: u32,
    golden_light: u32,
    omarchy: fn(&OmarchyTokens) -> Rgba,
) -> Rgba {
    match scheme() {
        Scheme::ClassicDark => rgba(classic_dark),
        Scheme::ClassicLight => rgba(classic_light),
        Scheme::GoldenDark => rgba(golden_dark),
        Scheme::GoldenLight => rgba(golden_light),
        // Unset only if the scheme was switched without a palette, which
        // `set_omarchy` never does; classic dark is the safe paint.
        Scheme::Omarchy => omarchy_tokens().map_or(rgba(classic_dark), |t| omarchy(&t)),
    }
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
    tone(0xffffff12, 0xfffffff2, 0xf5efe612, 0xfbf8f3f2, |t| {
        t.surface
    })
}

/// Tab-strip well. Dark: same language as a card. Light: a grey trough so
/// the selected chip can be white.
#[inline]
pub fn trough() -> Rgba {
    // Golden light: `--bone-deep`, the well a paper chip sits in.
    tone(0xffffff12, 0x0000000a, 0xf5efe612, 0xe8dfd2ff, |t| t.trough)
}

/// Selected tab chip.
#[inline]
pub fn chip() -> Rgba {
    // Golden dark chip is `--line` (`#f5efe61f`). Golden light is `--paper`.
    tone(0xffffff1f, 0xffffffff, 0xf5efe61f, 0xfbf8f3ff, |t| t.chip)
}

/// Hover / expanded rows. Dark: lift. Light: a grey wash on the white card.
#[inline]
pub fn surface_raised() -> Rgba {
    // Golden: `--wash-strong` on dark (`#ffffff1a`), `--wash` on light.
    tone(0xffffff1f, 0x0000000e, 0xffffff1a, 0x1a17140f, |t| {
        t.surface_raised
    })
}

/// Recessed fill: nested detail blocks, meter tracks.
#[inline]
pub fn inset() -> Rgba {
    // Golden light recess is `--bone-deep`. Golden dark keeps the classic
    // black wash: the site's dark bone-deep (`#0e0c0a`) is a solid, and a
    // solid recess would turn every meter track into a black bar.
    tone(0x00000040, 0x0000000c, 0x00000040, 0xe8dfd2ff, |t| t.inset)
}

/// Hairline outlines and meter tracks.
#[inline]
pub fn border() -> Rgba {
    // Golden dark hairline is `--line`. Golden light is `--line-solid`,
    // the site's card edge, which is a real warm line rather than a
    // faint black wash.
    tone(0xffffff22, 0x00000012, 0xf5efe61f, 0xe2d8c9ff, |t| t.border)
}

/// Row separators — dimmer than [`border`].
#[inline]
pub fn border_subtle() -> Rgba {
    // `--line-soft` on dark, `--line` on light.
    tone(0xffffff14, 0x00000014, 0xf5efe612, 0x1a171424, |t| {
        t.border_subtle
    })
}

/// Primary text.
#[inline]
pub fn text() -> Rgba {
    // Golden is `--ink`.
    tone(0xfafafaff, 0x1d1d1fff, 0xf5efe6ff, 0x1a1714ff, |t| t.text)
}

/// Secondary text: units, captions, inactive tabs.
#[inline]
pub fn text_muted() -> Rgba {
    // Golden is `--ink-soft`.
    tone(0xa1a1aaff, 0x6e6e73ff, 0xa89f93ff, 0x5a534bff, |t| {
        t.text_muted
    })
}

/// Tertiary text: field labels, footnotes.
#[inline]
pub fn text_dim() -> Rgba {
    // Classic dark keeps the system grey. Classic light is a step darker
    // than it: `#8e8e93` on the near-white card measured 3.26:1, under the
    // 4.5:1 that 10pt captions and path prefixes need, while every other
    // scheme cleared 4.9; `#747479` reads 4.65:1. Golden steps down to
    // `--ink-faint`, the site's last published ink.
    tone(0x8e8e93ff, 0x747479ff, 0x968d81ff, 0x645d54ff, |t| {
        t.text_dim
    })
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
    tone(0x636366ff, 0xa1a1a6ff, 0x968d81ff, 0x645d54ff, |t| {
        t.text_faint
    })
}

/// Neutral fill for bars and meters — the "nothing is wrong" state.
#[inline]
pub fn ink() -> Rgba {
    // Golden meters use `--ink`, the same role as the classic near-black
    // / near-white neutral.
    tone(0xe4e4e7ff, 0x1d1d1fff, 0xf5efe6ff, 0x1a1714ff, |t| t.ink)
}

/// Brand red. Reserved for over-threshold states and the primary action.
/// Omarchy's is the palette's red (see the module doc).
#[inline]
pub fn accent() -> Rgba {
    omarchy_tokens().map_or(rgb(0xec3013), |t| t.accent)
}

/// Accent *text*: lighter on dark surfaces, darker on light so contrast holds.
#[inline]
pub fn accent_light() -> Rgba {
    if let Some(t) = omarchy_tokens() {
        t.accent_light
    } else if is_dark() {
        rgb(0xff9783)
    } else {
        rgb(0xc41e0a)
    }
}

/// Accent washes used behind warning pills and alert rows.
#[inline]
pub fn accent_wash(alpha_percent: u32) -> Rgba {
    let alpha = alpha_percent.clamp(0, 100) * 255 / 100;
    Rgba {
        a: alpha as f32 / 255.0,
        ..accent()
    }
}

/// The theme's own mark and the ink that sits on it: Golden's champagne,
/// Omarchy's accent. `None` for the classic pair, whose selected chips
/// keep the faint brand wash. A selected tab, a selected option chip and
/// a checked switch wear it.
#[inline]
pub fn mark() -> Option<(Rgba, Rgba)> {
    if let Some(t) = omarchy_tokens() {
        Some((t.mark, t.on_mark))
    } else if is_golden() {
        Some((gold(), on_gold()))
    } else {
        None
    }
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
/// A scheme with a [`mark`] fills it solid.
#[inline]
pub fn choice_fill(on: bool) -> Rgba {
    match (on, mark()) {
        (false, _) => inset(),
        (true, Some((fill, _))) => fill,
        (true, None) => accent_wash(10),
    }
}

/// Border of a selected option chip. Same split as [`choice_fill`].
#[inline]
pub fn choice_line(on: bool) -> Rgba {
    match (on, mark()) {
        (false, _) => border(),
        (true, Some((fill, _))) => fill,
        (true, None) => accent_wash(45),
    }
}

/// Label on a selected option chip. With a [`mark`] it is the mark's own
/// ink, because the fill is the mark itself.
#[inline]
pub fn choice_ink(on: bool) -> Rgba {
    match (on, mark()) {
        (false, _) => text(),
        (true, Some((_, ink))) => ink,
        (true, None) => accent_light(),
    }
}

/// Hover fill for an option chip. A selected chip on a [`mark`] stays the
/// mark: its label is the mark's ink, and the usual raised wash would put
/// that ink on a card.
#[inline]
pub fn choice_hover(on: bool) -> Rgba {
    match (on, mark()) {
        (true, Some((fill, _))) => fill,
        _ => surface_raised(),
    }
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
    let on = |fill: Rgba| {
        if contrast(fill, rgb(0x000000)) > contrast(fill, rgb(0xffffff)) {
            rgb(0x000000)
        } else {
            rgb(0xffffff)
        }
    };
    let active_wash = Rgba {
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
        ("accent.background", p.surface),
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
        ("scrollbar.background", rgba(0x00000000)),
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
fn hex(color: Rgba) -> String {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let (r, g, b, a) = (byte(color.r), byte(color.g), byte(color.b), byte(color.a));
    if a == 0xff {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
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
    use std::sync::Mutex;

    /// The scheme is one global. Tests that switch it hold this, so two
    /// of them running in parallel cannot repaint each other mid-assert.
    static SCHEME_LOCK: Mutex<()> = Mutex::new(());

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
        let _scheme = SCHEME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    #[test]
    fn omarchy_paints_every_token_from_the_palette() {
        let _scheme = SCHEME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let palette = Palette::tokyo_night();
        set_omarchy(&palette);
        assert!(is_dark());
        assert!(omarchy_palette_is(&palette));
        assert_eq!(surface(), palette.surface);
        assert_eq!(inset(), palette.inset);
        assert_eq!(text(), palette.foreground);
        assert_eq!(ink(), palette.foreground);
        // The alarm is the palette's red; its washes keep that hue.
        assert_eq!(accent(), palette.red);
        assert_eq!(accent_light(), palette.bright_red);
        let wash = accent_wash(10);
        assert_eq!(
            (wash.r, wash.g, wash.b),
            (palette.red.r, palette.red.g, palette.red.b)
        );
        assert!((wash.a - 25.0 / 255.0).abs() < 1e-6);
        // The theme's mark is its accent, solid, with legible ink on it.
        let (fill, on) = mark().expect("Omarchy has a mark");
        assert_eq!(fill, palette.accent);
        assert!(contrast(fill, on) >= 4.5, "ink on the mark must read");
        assert_eq!(choice_fill(true), fill);
        assert_eq!(choice_ink(true), on);
        // Neutral steps keep the theme's hue: muted text sits between
        // the ground and the ink.
        let muted = omarchy::luminance(text_muted());
        assert!(muted > omarchy::luminance(palette.background));
        assert!(muted < omarchy::luminance(palette.foreground));

        set_dark(true);
        assert!(mark().is_none());
        assert_eq!(accent(), rgb(0xec3013));
        assert!(!omarchy_palette_is(&palette), "classic is on screen now");
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
            background: rgb(0xfffcf0),
            foreground: rgb(0x100f0f),
            ..dark.clone()
        };
        let (light_name, light_json) = omarchy_theme(&light);
        assert_ne!(light_name, name);
        registry.load_themes_from_str(&light_json).unwrap();
        assert!(!registry.themes()[light_name.as_str()].mode.is_dark());
    }

    #[test]
    fn hex_writes_alpha_only_when_translucent() {
        assert_eq!(hex(rgb(0x1a1b26)), "#1a1b26");
        assert_eq!(hex(rgba(0x7aa2f715)), "#7aa2f715");
    }
}
