//! Design tokens for a macOS menu-bar panel.
//!
//! Fills are translucent so the Popover vibrancy shows through. Two palettes
//! — dark (white washes) and light (black washes) — selected by [`set_dark`].
//! Brand red is reserved for over-threshold states.

use crate::i18n;
use gpui::{Rgba, rgb, rgba};
use std::sync::atomic::{AtomicBool, Ordering};

static DARK: AtomicBool = AtomicBool::new(true);

/// Pin the palette to the current appearance. Call after every
/// `Theme::change` so tokens match the window material.
pub fn set_dark(dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
}

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

/// Card / grouped fill. Dark: faint white lift. Light: nearly solid white,
/// so cards sit on the grey panel the way Settings groups do.
#[inline]
pub fn surface() -> Rgba {
    if is_dark() {
        rgba(0xffffff12)
    } else {
        rgba(0xfffffff2)
    }
}

/// Tab-strip well. Dark: same language as a card. Light: a grey trough so
/// the selected chip can be white.
#[inline]
pub fn trough() -> Rgba {
    if is_dark() {
        rgba(0xffffff12)
    } else {
        rgba(0x0000000a)
    }
}

/// Selected tab chip.
#[inline]
pub fn chip() -> Rgba {
    if is_dark() {
        rgba(0xffffff1f)
    } else {
        rgb(0xffffff)
    }
}

/// Hover / expanded rows. Dark: lift. Light: a grey wash on the white card.
#[inline]
pub fn surface_raised() -> Rgba {
    if is_dark() {
        rgba(0xffffff1f)
    } else {
        rgba(0x0000000e)
    }
}

/// Recessed fill: nested detail blocks, meter tracks.
#[inline]
pub fn inset() -> Rgba {
    if is_dark() {
        rgba(0x00000040)
    } else {
        rgba(0x0000000c)
    }
}

/// Hairline outlines and meter tracks.
#[inline]
pub fn border() -> Rgba {
    if is_dark() {
        rgba(0xffffff22)
    } else {
        rgba(0x00000012)
    }
}

/// Row separators — dimmer than [`border`].
#[inline]
pub fn border_subtle() -> Rgba {
    if is_dark() {
        rgba(0xffffff14)
    } else {
        rgba(0x00000014)
    }
}

/// Primary text.
#[inline]
pub fn text() -> Rgba {
    if is_dark() {
        rgb(0xfafafa)
    } else {
        rgb(0x1d1d1f)
    }
}

/// Secondary text: units, captions, inactive tabs.
#[inline]
pub fn text_muted() -> Rgba {
    if is_dark() {
        rgb(0xa1a1aa)
    } else {
        rgb(0x6e6e73)
    }
}

/// Tertiary text: field labels, footnotes.
#[inline]
pub fn text_dim() -> Rgba {
    rgb(0x8e8e93)
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
    if is_dark() {
        rgb(0x636366)
    } else {
        rgb(0xa1a1a6)
    }
}

/// Neutral fill for bars and meters — the "nothing is wrong" state.
#[inline]
pub fn ink() -> Rgba {
    if is_dark() {
        rgb(0xe4e4e7)
    } else {
        rgb(0x1d1d1f)
    }
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
