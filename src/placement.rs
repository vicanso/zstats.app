//! Where the panel goes: sizes, and the geometry that hangs it off the tray.
//!
//! Split from `main.rs` because it is self-contained and the only part of the
//! window story that can be tested — everything else there needs a running
//! gpui app, while this is arithmetic on rectangles. Mixed-DPI scale
//! picking (`resolve_icon`) lives here for the same reason: it is a
//! function of the tray's physical rect and a list of (frame, scale),
//! not of AppKit.

use crate::state::{TrayAnchor, ZStatsGlobalStore};
use crate::window_ext;
use gpui::{App, Bounds, Pixels, Point, Size, point, px, size};
use std::env;

/// Menu-bar panel: 320px matches Control Center / Stats combined popovers
/// and lets the icon tab strip breathe. Height covers the icon strip,
/// Processor + Top CPU + Memory, and the footer without clipping.
pub const DEFAULT_WINDOW_SIZE: (f32, f32) = (358., 653.);
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

/// One display in gpui space, plus the backing scale tray-icon used for it.
///
/// `frame` is the full screen (menu bar included) so a status item sitting
/// in the menu bar still `contains`s; `visible` is what the panel clamps to.
#[derive(Clone, Copy, Debug)]
struct ScreenGeom {
    frame: Bounds<Pixels>,
    visible: Bounds<Pixels>,
    scale: f32,
}

/// Status-item windows are the menu-bar strip: ~22–24pt tall. tray-icon
/// reports `logical × that screen's backingScaleFactor`, so recovering
/// with the *wrong* screen's scale yields half or double this height —
/// which is how a 1x icon whose physical x still lands inside the 2x
/// primary is rejected. Width is not a signal: Both mode's title makes
/// the item arbitrarily wide, the height does not change.
const EXPECTED_ICON_HEIGHT: f32 = 24.0;

/// tray-icon's physical rect → gpui logical, under one screen's scale.
fn logical_icon(anchor: TrayAnchor, scale: f32) -> Bounds<Pixels> {
    let scale = if scale > 0. { scale } else { 1. };
    let to_px = |v: f64| px(v as f32 / scale);
    Bounds {
        origin: point(to_px(anchor.x), to_px(anchor.y)),
        size: size(to_px(anchor.width), to_px(anchor.height)),
    }
}

struct ResolvedIcon {
    icon: Bounds<Pixels>,
    visible: Option<Bounds<Pixels>>,
    scale: f32,
}

/// Try each screen's scale, keep the conversion whose recovered icon
/// sits on *that* screen. Mixed DPI can make two screens consistent
/// that way (physical x of a 1x icon, divided by 2, still lands on the
/// 2x primary) — then the recovered height picks the real one.
fn resolve_icon(anchor: TrayAnchor, screens: &[ScreenGeom]) -> ResolvedIcon {
    debug_assert!(
        !screens.is_empty(),
        "caller falls back to the window scale when AppKit has no list"
    );
    let mut best: Option<(f32, usize)> = None;
    for (i, screen) in screens.iter().enumerate() {
        let icon = logical_icon(anchor, screen.scale);
        if !screen.frame.contains(&icon.center()) {
            continue;
        }
        let score = (f32::from(icon.size.height) - EXPECTED_ICON_HEIGHT).abs();
        match best {
            Some((s, _)) if score >= s => {}
            _ => best = Some((score, i)),
        }
    }
    let i = best.map(|(_, i)| i).unwrap_or(0);
    let scale = screens[i].scale;
    ResolvedIcon {
        icon: logical_icon(anchor, scale),
        visible: Some(screens[i].visible),
        scale,
    }
}

#[cfg(target_os = "macos")]
fn load_screens() -> Vec<ScreenGeom> {
    window_ext::screens()
        .into_iter()
        .map(|(frame, visible, scale)| ScreenGeom {
            frame,
            visible,
            scale,
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn load_screens() -> Vec<ScreenGeom> {
    Vec::new()
}

/// [`anchored_origin`] plus the two things that need an `App`: converting the
/// tray's physical pixels to logical ones, and finding the icon's display.
pub fn bounds_below_tray(
    anchor: TrayAnchor,
    window_size: Size<Pixels>,
    cx: &App,
) -> Bounds<Pixels> {
    let screens = load_screens();
    let resolved = if screens.is_empty() {
        // No AppKit list (off the main thread, or not macOS): whatever
        // the main window last reported, which is the pre-fix fallback.
        let scale = cx.global::<ZStatsGlobalStore>().read(cx).scale_factor();
        ResolvedIcon {
            icon: logical_icon(anchor, scale),
            visible: None,
            scale: if scale > 0. { scale } else { 1. },
        }
    } else {
        resolve_icon(anchor, &screens)
    };
    let icon = resolved.icon;
    let scale = resolved.scale;

    // Resolved through AppKit rather than `cx.displays()`, which reports
    // every screen at the same origin — see `window_ext`.
    #[cfg(target_os = "macos")]
    let screen = resolved
        .visible
        .or_else(|| window_ext::visible_bounds_containing(icon.origin));
    #[cfg(not(target_os = "macos"))]
    let screen = resolved.visible.or_else(|| {
        cx.displays()
            .into_iter()
            .find(|d| d.bounds().contains(&icon.origin))
            .or_else(|| cx.primary_display())
            .map(|d| d.visible_bounds())
    });
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
        let screens_dbg: String = screens
            .iter()
            .map(|s| {
                format!(
                    "{:.0}@({:.0},{:.0} {:.0}x{:.0})",
                    s.scale,
                    f32::from(s.frame.origin.x),
                    f32::from(s.frame.origin.y),
                    f32::from(s.frame.size.width),
                    f32::from(s.frame.size.height)
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!(
            "POS tray_physical=({:.0},{:.0} {:.0}x{:.0}) scale={scale} \
             screens=[{screens_dbg}] icon_logical=({:.0},{:.0} {:.0}x{:.0}) \
             screen={} window=({:.0},{:.0} {:.0}x{:.0})",
            anchor.x,
            anchor.y,
            anchor.width,
            anchor.height,
            f32::from(icon.origin.x),
            f32::from(icon.origin.y),
            f32::from(icon.size.width),
            f32::from(icon.size.height),
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
        for candidate in &screens {
            let try_icon = logical_icon(anchor, candidate.scale);
            let on = candidate.frame.contains(&try_icon.center());
            let score = (f32::from(try_icon.size.height) - EXPECTED_ICON_HEIGHT).abs();
            eprintln!(
                "POS try scale={} icon=({:.0},{:.0} {:.0}x{:.0}) on_screen={on} score={score:.0}",
                candidate.scale,
                f32::from(try_icon.origin.x),
                f32::from(try_icon.origin.y),
                f32::from(try_icon.size.width),
                f32::from(try_icon.size.height),
            );
        }
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

    /// Primary 1440×900 @2x, secondary 1920×1080 @1x to its right — the
    /// mixed-DPI layout the old `screens()[0]` scale got wrong.
    fn mixed_dpi() -> Vec<ScreenGeom> {
        vec![
            ScreenGeom {
                frame: Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(1440.), px(900.)),
                },
                visible: Bounds {
                    origin: point(px(0.), px(24.)),
                    size: size(px(1440.), px(876.)),
                },
                scale: 2.0,
            },
            ScreenGeom {
                frame: Bounds {
                    origin: point(px(1440.), px(0.)),
                    size: size(px(1920.), px(1080.)),
                },
                visible: Bounds {
                    origin: point(px(1440.), px(24.)),
                    size: size(px(1920.), px(1056.)),
                },
                scale: 1.0,
            },
        ]
    }

    fn anchor(x: f64, y: f64, w: f64, h: f64) -> TrayAnchor {
        TrayAnchor {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn mixed_dpi_icon_on_the_1x_screen_is_not_mapped_onto_the_2x_one() {
        // Logical (1600, 0) 24×24 on the 1x display. tray-icon reports
        // physical = logical × 1. Dividing by the primary's 2 instead
        // lands at 800 — still on the 2x screen, which is the false
        // positive this picker exists to reject.
        let physical = anchor(1600., 0., 24., 24.);
        let naive = logical_icon(physical, 2.0);
        assert!(
            mixed_dpi()[0].frame.contains(&naive.center()),
            "the old screens[0] scale puts this icon on the primary"
        );
        let resolved = resolve_icon(physical, &mixed_dpi());
        assert_eq!(resolved.scale, 1.0);
        assert_eq!(resolved.icon.origin, point(px(1600.), px(0.)));
        assert_eq!(resolved.icon.size, size(px(24.), px(24.)));
        assert_eq!(
            resolved.visible.map(|v| v.origin.x),
            Some(px(1440.)),
            "clamp against the screen the icon is actually on"
        );
    }

    #[test]
    fn mixed_dpi_icon_on_the_2x_screen_keeps_the_retina_scale() {
        // Logical (700, 0) 24×24 on the 2x display → physical 1400, 48×48.
        let resolved = resolve_icon(anchor(1400., 0., 48., 48.), &mixed_dpi());
        assert_eq!(resolved.scale, 2.0);
        assert_eq!(resolved.icon.origin, point(px(700.), px(0.)));
        assert_eq!(resolved.icon.size.height, px(24.));
    }

    #[test]
    fn same_dpi_two_screens_picks_the_one_that_contains_the_icon() {
        let screens = vec![
            ScreenGeom {
                frame: Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(1440.), px(900.)),
                },
                visible: Bounds {
                    origin: point(px(0.), px(24.)),
                    size: size(px(1440.), px(876.)),
                },
                scale: 2.0,
            },
            ScreenGeom {
                frame: Bounds {
                    origin: point(px(1440.), px(0.)),
                    size: size(px(1920.), px(1080.)),
                },
                visible: Bounds {
                    origin: point(px(1440.), px(24.)),
                    size: size(px(1920.), px(1056.)),
                },
                scale: 2.0,
            },
        ];
        // Logical (1600, 0) 24×24 on the secondary, both @2x.
        let resolved = resolve_icon(anchor(3200., 0., 48., 48.), &screens);
        assert_eq!(resolved.scale, 2.0);
        assert_eq!(resolved.icon.origin.x, px(1600.));
        assert_eq!(resolved.visible.map(|v| v.origin.x), Some(px(1440.)));
    }

    #[test]
    fn a_2x_secondary_to_the_right_of_a_1x_primary() {
        // Menu bar moved onto the 1x display; the Retina laptop is
        // secondary. Logical (2000, 0) 24×24 on the laptop → physical ×2.
        let screens = vec![
            ScreenGeom {
                frame: Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(1920.), px(1080.)),
                },
                visible: Bounds {
                    origin: point(px(0.), px(24.)),
                    size: size(px(1920.), px(1056.)),
                },
                scale: 1.0,
            },
            ScreenGeom {
                frame: Bounds {
                    origin: point(px(1920.), px(0.)),
                    size: size(px(1440.), px(900.)),
                },
                visible: Bounds {
                    origin: point(px(1920.), px(24.)),
                    size: size(px(1440.), px(876.)),
                },
                scale: 2.0,
            },
        ];
        let resolved = resolve_icon(anchor(4000., 0., 48., 48.), &screens);
        assert_eq!(resolved.scale, 2.0);
        assert_eq!(resolved.icon.origin, point(px(2000.), px(0.)));
    }

    #[test]
    fn mixed_dpi_stacked_laptop_below_the_monitor() {
        // 1x monitor on top, 2x laptop below. Icon on the laptop at
        // logical (700, 1080) — physical y is doubled too.
        let screens = vec![
            ScreenGeom {
                frame: Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(1920.), px(1080.)),
                },
                visible: Bounds {
                    origin: point(px(0.), px(24.)),
                    size: size(px(1920.), px(1056.)),
                },
                scale: 1.0,
            },
            ScreenGeom {
                frame: Bounds {
                    origin: point(px(0.), px(1080.)),
                    size: size(px(1440.), px(900.)),
                },
                visible: Bounds {
                    origin: point(px(0.), px(1104.)),
                    size: size(px(1440.), px(876.)),
                },
                scale: 2.0,
            },
        ];
        let resolved = resolve_icon(anchor(1400., 2160., 48., 48.), &screens);
        assert_eq!(resolved.scale, 2.0);
        assert_eq!(resolved.icon.origin, point(px(700.), px(1080.)));
    }
}
