//! Where the panel goes: sizes, and the geometry that hangs it off the tray.
//!
//! Split from `main.rs` because it is self-contained and the only part of the
//! window story that can be tested — everything else there needs a running
//! gpui app, while this is arithmetic on rectangles.

use crate::state::{TrayAnchor, ZStatsGlobalStore};
use crate::window_ext;
use gpui::{App, Bounds, Pixels, Point, Size, point, px, size};
use std::env;

/// Menu-bar panel: 320px matches Control Center / Stats combined popovers
/// and lets the icon tab strip breathe. Height covers the icon strip,
/// Processor + Top CPU + Memory, and the footer without clipping.
pub const DEFAULT_WINDOW_SIZE: (f32, f32) = (358., 678.);
/// Fixed width — the layout is built for exactly this and nothing reflows.
pub const MIN_WINDOW_SIZE: (f32, f32) = (320., 320.);
/// Gap between the tray icon and the top of the window.
const TRAY_GAP: f32 = 6.;
/// Hang the window under the tray icon: horizontally centred on it, `TRAY_GAP`
/// below it. Centring is the default; the clamp only kicks in when the window
/// would run past a screen edge, in which case it sits flush against that edge.
///
/// Pure geometry, all in logical pixels, so it's testable without an `App`.
fn anchored_origin(
    icon: Bounds<Pixels>,
    window_size: Size<Pixels>,
    screen: Bounds<Pixels>,
) -> Point<Pixels> {
    let mut origin = point(
        icon.origin.x + icon.size.width / 2. - window_size.width / 2.,
        icon.origin.y + icon.size.height + px(TRAY_GAP),
    );
    // `.max(origin)` guards the degenerate case of a window wider than the
    // screen, where the upper clamp bound would fall below the lower one.
    let max_x = (screen.origin.x + screen.size.width - window_size.width).max(screen.origin.x);
    let max_y = (screen.origin.y + screen.size.height - window_size.height).max(screen.origin.y);
    origin.x = origin.x.clamp(screen.origin.x, max_x);
    origin.y = origin.y.clamp(screen.origin.y, max_y);
    origin
}

/// Scale factor of the display that owns the menu bar — `screens()[0]` is
/// always that one, unlike `mainScreen`, which follows the key window.
///
/// gpui's `PlatformDisplay` exposes no scale factor, and mirroring the main
/// window's isn't an option here: the tray fires with no window open at all.
#[cfg(target_os = "macos")]
fn menu_bar_scale_factor() -> Option<f32> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;

    let mtm = MainThreadMarker::new()?;
    let screen = NSScreen::screens(mtm).firstObject()?;
    Some(screen.backingScaleFactor() as f32)
}

#[cfg(not(target_os = "macos"))]
fn menu_bar_scale_factor() -> Option<f32> {
    None
}

/// [`anchored_origin`] plus the two things that need an `App`: converting the
/// tray's physical pixels to logical ones, and finding the icon's display.
pub fn bounds_below_tray(
    anchor: TrayAnchor,
    window_size: Size<Pixels>,
    cx: &App,
) -> Bounds<Pixels> {
    let scale = menu_bar_scale_factor()
        // Fallback for platforms without the AppKit path: whatever the main
        // window last reported.
        .unwrap_or_else(|| cx.global::<ZStatsGlobalStore>().read(cx).scale_factor());
    let scale = if scale > 0. { scale } else { 1. };
    let to_px = |v: f64| px(v as f32 / scale);
    let icon = Bounds {
        origin: point(to_px(anchor.x), to_px(anchor.y)),
        size: size(to_px(anchor.width), to_px(anchor.height)),
    };

    // Resolved through AppKit rather than `cx.displays()`, which reports
    // every screen at the same origin — see `window_ext`.
    #[cfg(target_os = "macos")]
    let screen = window_ext::visible_bounds_containing(icon.origin);
    #[cfg(not(target_os = "macos"))]
    let screen = cx
        .displays()
        .into_iter()
        .find(|d| d.bounds().contains(&icon.origin))
        .or_else(|| cx.primary_display())
        .map(|d| d.visible_bounds());
    let origin = match screen {
        Some(screen) => anchored_origin(icon, window_size, screen),
        // No display info to clamp against — centre and hope for the best.
        None => point(
            icon.origin.x + icon.size.width / 2. - window_size.width / 2.,
            icon.origin.y + icon.size.height + px(TRAY_GAP),
        ),
    };

    let bounds = Bounds {
        origin,
        size: window_size,
    };

    // Multi-display positioning has several places to go wrong and no visible
    // symptom beyond "it opened on the wrong screen". `ZSTATS_DEBUG_POSITION=1`
    // prints the whole chain so a bad step can be identified rather than
    // guessed at.
    if env::var_os("ZSTATS_DEBUG_POSITION").is_some() {
        eprintln!(
            "POS tray_physical=({:.0},{:.0} {:.0}x{:.0}) scale={scale} \
             icon_logical=({:.0},{:.0}) screen={} window=({:.0},{:.0} {:.0}x{:.0})",
            anchor.x,
            anchor.y,
            anchor.width,
            anchor.height,
            f32::from(icon.origin.x),
            f32::from(icon.origin.y),
            match screen {
                Some(s) => format!(
                    "({:.0},{:.0} {:.0}x{:.0})",
                    f32::from(s.origin.x),
                    f32::from(s.origin.y),
                    f32::from(s.size.width),
                    f32::from(s.size.height)
                ),
                None => "none".to_string(),
            },
            f32::from(bounds.origin.x),
            f32::from(bounds.origin.y),
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
        );
    }
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1440×900 display with the 38px menu bar taken off the top, i.e. what
    /// `visible_bounds()` reports on macOS.
    fn screen() -> Bounds<Pixels> {
        Bounds {
            origin: point(px(0.), px(38.)),
            size: size(px(1440.), px(862.)),
        }
    }

    /// A 24×24 menu bar icon with its left edge at `x`.
    fn icon_at(x: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(x), px(0.)),
            size: size(px(24.), px(24.)),
        }
    }

    fn window() -> Size<Pixels> {
        let (w, h) = DEFAULT_WINDOW_SIZE;
        size(px(w), px(h))
    }

    #[test]
    fn centers_under_the_icon_when_there_is_room() {
        let icon = icon_at(700.);
        let origin = anchored_origin(icon, window(), screen());
        // Icon centre 712, window 358 wide → 712 - 179.
        assert_eq!(origin.x, px(533.));
        // Icon bottom 24 + 6px gap, which clears the 38px menu bar.
        assert_eq!(origin.y, px(38.));
    }

    #[test]
    fn sticks_to_the_right_edge_when_the_icon_is_near_it() {
        // Centring would put the window at 1416 - 179 = 1237, whose right
        // edge (1595) overflows the 1440 screen.
        let origin = anchored_origin(icon_at(1404.), window(), screen());
        assert_eq!(origin.x, px(1082.)); // 1440 - 358
    }

    #[test]
    fn sticks_to_the_left_edge_when_the_icon_is_near_it() {
        let origin = anchored_origin(icon_at(4.), window(), screen());
        assert_eq!(origin.x, px(0.));
    }

    #[test]
    fn never_overflows_the_bottom() {
        let tall = size(px(320.), px(2000.));
        let origin = anchored_origin(icon_at(700.), tall, screen());
        // Taller than the screen: pinned to the top of the visible area rather
        // than to a negative coordinate.
        assert_eq!(origin.y, px(38.));
    }
}
