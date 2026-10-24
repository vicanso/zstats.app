//! The menu bar's glyphs as bitmaps: the bundled SVGs rasterised to the
//! straight RGBA `tray-icon` takes, and the alert spec stamped onto them.
//!
//! Which glyph the bar wears is a decision about the data
//! (`tray::face_for`), how it is drawn is this, and neither needs to know
//! which toolkit draws the panel. The ink comes in from the caller, because
//! only the caller knows the theme (`tray::ink`).
//!
//! `tray-icon` takes raw RGBA only — it has no SVG support and just
//! re-encodes whatever it is given as a PNG for `NSImage`. macOS then scales
//! that to 18pt tall (`platform_impl/macos/mod.rs`), so the bitmap is
//! produced at 2x to stay sharp on Retina.

use crate::assets::{self, CustomIconName};
use resvg::{tiny_skia, usvg};
use std::str;

/// Rendered at 2x the 18pt macOS uses, so no upscaling happens.
pub const ICON_SIZE: u32 = 36;

/// Share of the canvas the glyph occupies; the rest is transparent margin.
///
/// lucide draws to the edges of its 24x24 viewBox, and macOS scales the whole
/// bitmap to 18pt — so at 1.0 the glyph is a full 18pt tall and outweighs the
/// ~12pt title beside it. System menu bar icons inset their artwork instead.
const GLYPH_SCALE: f32 = 0.78;

/// 4 pt at 2×, top-left — away from the percent title on the right.
const DOT_DIAMETER: u32 = 8;
const DOT_INSET: u32 = 1;

/// Rasterise the bundled SVG to straight RGBA at [`ICON_SIZE`], every pixel
/// in `ink`. Kept apart from the `Icon` it becomes so the result can be
/// inspected in a test — a silently empty bitmap would otherwise just look
/// like a missing icon at runtime.
pub fn rasterise(glyph: CustomIconName, ink: [u8; 3]) -> Option<Vec<u8>> {
    let size = ICON_SIZE;
    // From the shared embed rather than `include_str!`, which would compile a
    // second, uncompressed copy of the same file into the binary.
    let raw = assets::get(glyph.path())?;
    // lucide ships `stroke="currentColor"`, which is a CSS-context keyword
    // usvg cannot resolve on its own. Which colour does not matter: the
    // channels are overwritten with `ink` below, and only alpha survives
    // this function.
    let svg = str::from_utf8(&raw)
        .ok()?
        .replace("currentColor", "#000000");

    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let source = tree.size();
    let longest = source.width().max(source.height());
    let scale = size as f32 * GLYPH_SCALE / longest;
    // Centre what is left over, so the margin is even on all four sides.
    let inset = (size as f32 - longest * scale) / 2.0;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_translate(inset, inset).pre_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    let mut rgba = pixmap.take();
    // Keep only alpha and repaint the colour channels. tiny-skia hands back
    // premultiplied colour, which `Icon::from_rgba` would read as straight,
    // so those channels have to be rewritten whatever happens — `ink` is
    // what decides to what. On macOS the template discards it and works off
    // alpha alone; off macOS it *is* the glyph.
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel[..3].copy_from_slice(&ink);
    }
    Some(rgba)
}

/// A filled spec in the top-left of a [`rasterise`]d glyph, in the glyph's
/// own ink rather than a colour of its own. On macOS that means extra alpha
/// on the same template, so the spec takes the menu bar's ink along with
/// the glyph (white on dark, black on light) — a painted colour there would
/// have to drop template mode and become a black block. Off macOS it is the
/// colour the strokes carry.
pub fn stamp_hot_dot(rgba: &mut [u8], ink: [u8; 3]) {
    let size = ICON_SIZE;
    let radius = DOT_DIAMETER as f32 / 2.0;
    let cx = DOT_INSET as f32 + radius;
    let cy = DOT_INSET as f32 + radius;
    let r2 = radius * radius;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= r2 {
                let i = ((y * size + x) * 4) as usize;
                rgba[i..i + 3].copy_from_slice(&ink);
                rgba[i + 3] = 255;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INK: [u8; 3] = [0x1a; 3];

    /// Every face, not just the one an item is built with: the memory glyph
    /// is applied only on a swap, so a broken SVG there would show up as
    /// "the icon vanished the first time memory got tight".
    #[test]
    fn every_face_rasterises_to_a_visible_glyph() {
        for glyph in [
            CustomIconName::Cpu,
            CustomIconName::MemoryStick,
            CustomIconName::HardDrive,
        ] {
            let rgba = rasterise(glyph, INK).expect("icon should rasterise");
            assert_eq!(rgba.len() as u32, ICON_SIZE * ICON_SIZE * 4);

            let opaque = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|px| px[3] > 32)
                .count();
            let total = (ICON_SIZE * ICON_SIZE) as usize;
            let coverage = opaque as f32 / total as f32;

            // A failed parse (lucide's `currentColor`, a bad viewBox, a
            // scale of zero) yields a fully transparent bitmap that raises
            // no error and simply shows as a missing icon. A stroke-only
            // glyph covers a modest share of its box, so bracket it on
            // both sides.
            assert!(
                (0.02..0.50).contains(&coverage),
                "{glyph:?}: unexpected glyph coverage {coverage:.3} — SVG likely failed to render"
            );
        }
    }

    #[test]
    fn an_alert_spec_is_extra_alpha_in_the_glyphs_own_ink() {
        for glyph in [
            CustomIconName::Cpu,
            CustomIconName::MemoryStick,
            CustomIconName::HardDrive,
        ] {
            let mut rgba = rasterise(glyph, INK).expect("glyph");
            let cx = (DOT_INSET + DOT_DIAMETER / 2) as usize;
            let i = (cx * ICON_SIZE as usize + cx) * 4;
            let before = rgba[i + 3];
            stamp_hot_dot(&mut rgba, INK);
            assert_eq!(rgba[i + 3], 255, "{glyph:?}: the spec must be opaque");
            assert!(rgba[i + 3] >= before);
            assert_eq!(
                &rgba[i..i + 3],
                &INK,
                "{glyph:?}: the glyph's own ink, never a colour of its own"
            );
        }
    }
}
