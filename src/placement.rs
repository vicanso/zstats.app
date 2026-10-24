//! Where the panel goes: sizes, and the geometry that hangs it off the tray.
//!
//! Split from `main.rs` because it is self-contained and the only part of the
//! window story that can be tested — everything else there needs a running
//! host, while this is arithmetic on rectangles. Mixed-DPI scale
//! picking (`resolve_icon`) lives here for the same reason: it is a
//! function of the tray's physical rect and a list of (frame, scale),
//! not of AppKit.
//!
//! The arithmetic is on `geom.rs`'s rectangles, and [`below_tray`] takes
//! the screens and the fallbacks as arguments: asking AppKit for those is
//! the host's business (`nsh/host.rs`'s `Screens`).

use crate::geom::{Point, Rect, Size, point, size};
use std::env;

/// Screen rectangle of the tray icon, in **physical** pixels with a top-left
/// origin — that's what `tray_icon` reports. It multiplied the AppKit
/// logical frame by *that status item's* `backingScaleFactor`, so converting
/// back has to pick the matching screen ([`resolve_icon`]), not the
/// window's last-known scale factor.
#[derive(Clone, Copy, Debug)]
pub struct TrayAnchor {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Menu-bar panel: 320px matches Control Center / Stats combined popovers
/// and lets the icon tab strip breathe. Height covers the icon strip,
/// Processor + Top apps + Memory + the disk-and-network card, and the
/// footer, with a few pixels under the last card.
///
/// 778 is the 2026-09-28 fit (653) plus the three Overview charts and
/// the caption under each, less two rows of the apps card. A chart is
/// 40px with 4px above it, 132 for three (`overview`'s `CURVE_H` and
/// `CURVE_TOP`). The caption is 14px with 4px above it, 54 for three
/// (`CHART_CAPTION` and `CHART_CAPTION_GAP`). The apps card going from
/// five rows to three (`overview::TOP_N`) took 61 back, measured: the
/// Memory card's bottom edge moved 122 device pixels up at @2x, and the
/// 16pt under it is the same at 778 as it was at 839 with five rows.
/// 792 is that plus 14 for moving the disk/network rates and the
/// network chart out of the Memory card into a card of their own
/// (`overview::io_card`) — the second card's padding and the gap
/// between cards, less the hairline row it replaced; measured as the
/// last card's bottom edge moving 28 device pixels down at @2x.
/// 828 is the 2026-10-12 redesign, measured again from scratch rather
/// than added to the above: the four cards, the title the
/// disk-and-network card gained, and the panel's new spacing scale (a
/// 12 gutter, cards 10 apart with 12 of padding, a 44-tall footer). It
/// was 908 for a few hours, with a status row and a line of facts above
/// the cards — 80 of height for things the cards and the Hardware tab
/// already say — and then 854 with one line for the link back to the
/// short form, which is the footer's toggle now. At 828 the page fits
/// under the menu bar of a 13-inch screen at its default scaling (876).
/// The last card's bottom edge is at 774pt and the body keeps 10 under
/// it, above that footer. This is the long form's height; the short
/// form has its own ([`SIMPLE_WINDOW_HEIGHT`]). Overview is sized not
/// to scroll; the other tabs scroll either way.
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_WINDOW_SIZE: (f32, f32) = (358., 828.);
/// Linux: 53 shorter than macOS, for the same fit, then the same chart
/// and caption budget. Overview's cards measured 543 tall on macOS and
/// 489 on Omarchy (2026-09-28, before the charts): the gap is the
/// Processor card's P/E cluster rows, which zstats reports on macOS only
/// (`perf_levels`). At 653 that left 57 of empty panel under Memory, where
/// macOS keeps 4; 600 kept the same 4. 725 / 778 keep that remainder
/// after the charts, the caption under each, and the two apps rows
/// dropped, and 14 more for the disk-and-network card, as on macOS.
/// Revisit when zstats grows
/// Linux perf levels (hybrid Intel parts have them in sysfs) — the rows
/// come back and so do the 53. 775 is macOS's 828 less those 53 —
/// carried over, not measured: no Linux machine has drawn this page
/// since the redesign.
#[cfg(target_os = "linux")]
pub const DEFAULT_WINDOW_SIZE: (f32, f32) = (358., 775.);
/// The short Overview's height (`prefs::overview_detail` off, the
/// default) on an ordinary day: the card of the processor's figure
/// and chart over four facts (memory, disk, network, the hottest
/// sensor), and the apps card at five rows. The same on both
/// platforms — what made macOS taller was the P/E cluster rows, which
/// the short form does not show. Measured on macOS, and then moved
/// four times on 2026-10-12 by what the page lost and gained, each
/// seen on screen: 665 with a status row that was always there; 619
/// without it ([`NOTICE_HEIGHT`], now only while it has something to
/// say); 649 with the processor row's chart, a 36-tall well where a
/// 6-tall bar was (`overview::cpu_pulse`); 573 once memory and the
/// disk gave up their bars and the line of small print under each,
/// 38 a row; 539 when those two rows became facts beside the network's
/// (`overview::facts`), two 24-tall rows and a gap for one 34-tall row
/// of the grid.
pub const SIMPLE_WINDOW_HEIGHT: f32 = 539.;

/// What the short form's notice row adds while it is shown: the row's
/// 36 and the 10 between cards (`overview::NOTICE_ROW` asserts the sum).
pub const NOTICE_HEIGHT: f32 = 46.;

/// Where the reader dragged the panel to (`prefs::panel_width`,
/// `prefs::panel_height`): `None` is the design's.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Dragged {
    pub width: Option<f32>,
    /// The height of the form being sized, without the notice row.
    pub height: Option<f32>,
}

/// The panel's size for Overview's form. A height per form, so neither
/// scrolls and neither ends in empty panel — or where the reader dragged
/// it (`dragged`), which is theirs to leave room under. The form is
/// what the height says it is (`detail_fits`): a short form is held
/// under the long form's height and a long one at it or over, so a
/// file that says otherwise cannot open a panel that would switch form
/// the first time it is dragged. Never under [`MIN_WINDOW_SIZE`]. Two
/// things resize the window: switching form
/// (`crate::set_overview_detail_pref`), and the short form's notice row
/// coming or going (`crate::sync_panel_notice`) — the long form has no
/// such row and `notice` does not move it, and the row is added on top
/// of a dragged height as on the form's own.
pub fn window_size(detail: bool, notice: bool, dragged: Dragged) -> (f32, f32) {
    let (design_width, full) = DEFAULT_WINDOW_SIZE;
    let width = dragged
        .width
        .map_or(design_width, |w| w.max(MIN_WINDOW_SIZE.0));
    let height = if detail {
        dragged.height.map_or(full, |h| h.max(full))
    } else {
        dragged.height.map_or(SIMPLE_WINDOW_HEIGHT, |h| {
            h.clamp(MIN_WINDOW_SIZE.1, full - 1.)
        })
    };
    let row = if notice && !detail { NOTICE_HEIGHT } else { 0. };
    (width, height + row)
}

/// Whether a panel this tall shows Overview's long form: once it is as
/// tall as that page was measured for, the page fits without scrolling,
/// and under that height the short form is what fits. Asked while a
/// person drags the panel's edge (the panel delegate's `windowDidResize:`) —
/// never of a height the panel gave itself, or the notice row's 46
/// could carry a short panel over the line and the long form, which has
/// no such row, would take them back.
pub fn detail_fits(height: f32) -> bool {
    height >= DEFAULT_WINDOW_SIZE.1
}

/// The panel's height once the notice row has appeared (`shown`) or
/// gone, from the height it has now. A step and not [`window_size`]'s
/// figure, so a dragged panel keeps what it was given; it never goes
/// under the short form's height (`dragged`, as for [`window_size`]),
/// which is all a panel cut to a short screen can be given back.
pub fn notice_step(current: f32, shown: bool, dragged: Option<f32>) -> f32 {
    let short = Dragged {
        width: None,
        height: dragged,
    };
    let (_, fitted) = window_size(false, shown, short);
    if shown {
        current + NOTICE_HEIGHT
    } else {
        (current - NOTICE_HEIGHT).max(fitted)
    }
}

/// The smallest the panel can be dragged. The width's floor is the
/// width the layout was designed at: narrower, and the tab strip's
/// longest name no longer fits beside the six icons. Wider only gives
/// every row more room. The height's floor is the short Overview's own
/// height — the page the panel opens on, whole — and while that page
/// shows its notice row, the row's height more ([`min_height`]). It was
/// 320, which let the panel be dragged to two cards cut through the
/// middle with the footer under them; a screen too short for the panel
/// still cuts it further ([`CUT_FLOOR`]).
pub const MIN_WINDOW_SIZE: (f32, f32) = (358., SIMPLE_WINDOW_HEIGHT);

/// The least height the panel can be dragged to now: the short
/// Overview's, and its notice row's on top while that is shown — the
/// page is that much taller then, and a drag that ended under it left
/// the row and two cards scrolling.
pub fn min_height(notice: bool) -> f32 {
    MIN_WINDOW_SIZE.1 + if notice { NOTICE_HEIGHT } else { 0. }
}

/// The least a screen too short for the panel cuts it to. Not
/// [`MIN_WINDOW_SIZE`]: that is what a drag may reach, and a floor over
/// the screen's own height would leave the footer hanging off it. Under
/// this a screen is not one the panel can be right on.
const CUT_FLOOR: f32 = 320.;
/// Gap between the panel's right edge and the screen edge on Wayland.
/// A surface flush against the bezel reads as clipped rather than
/// placed, and macOS never puts one there either — `placement.rs`
/// centres the window under its tray icon, which leaves the menu bar's
/// own inset. 8 was reported as no gap at all on Omarchy next to a
/// 358pt-wide panel; this is the value to move if the panel should sit
/// on the same rhythm as the compositor's own window gaps (`gaps_out`
/// in hyprland.conf), which is a per-desktop number nothing reports.
#[cfg(target_os = "linux")]
pub const PANEL_MARGIN_RIGHT: f32 = 10.;

/// Gap between the panel's top edge and the bar above it. Matches
/// [`PANEL_MARGIN_RIGHT`]: a popover with the same inset on both edges
/// it hangs off reads as placed, and the right edge is the one that
/// already looks right.
///
/// This was 0 for a while, and the reason is worth keeping because it
/// was a measurement taken through a bug. gpui-component's client-side
/// frame was inflating the gpui front end's surface by 20px a side (a
/// vendored gpui patch ended it, docs/omarchy-port.md), so the
/// first pixel of content sat 20px below the bar and the panel looked
/// over-gapped; 0 was the value that made *that* look right. With the
/// inflation gone, 0 is what it says — flush against the bar, and too
/// tight. The lesson is not about the number: a geometry constant tuned
/// against a build with a layout bug in it encodes the bug.
///
/// Measured from what the bar left free, **not** from the top of the
/// screen: an exclusive zone of 0 — gtk4-layer-shell's default, which the
/// panel keeps — asks the compositor to move the surface clear of every bar
/// that reserves space, and it has already done that before this margin
/// applies. A bar that reserves more than it paints therefore still
/// leaves a visible gap here, and closing *that* one means opting out of
/// the reservation entirely (an exclusive zone of -1) and
/// subtracting the bar's height by hand — a number no protocol reports,
/// so it would have to be configured rather than measured.
#[cfg(target_os = "linux")]
pub const PANEL_MARGIN_TOP: f32 = 10.;

/// Where the settings window opens. Disk space shared it until its
/// paths needed the width ([`STORAGE_WINDOW_SIZE`]); the two are now
/// different shapes, which reads as a decision, where two nearly-equal
/// sizes read as an accident. Still resizable — this is where it opens,
/// not where it must stay.
pub const SETTINGS_WINDOW_SIZE: (f32, f32) = (507., 620.);
/// The disk-space window's size before it zooms to fill the screen, and
/// the size the zoom button returns it to. It opens zoomed because the
/// analysis tab is a directory map beside its lists, and a map is read
/// by area — 720×480 left it a postcard. Wide rather than tall when
/// un-zoomed: at 507 a path got what was left after its size, pills and
/// two buttons; 720 reads `Library/Application Support/GIMP/2.10/cache/
/// fontconfig` whole. Sizing to the content was weighed and dropped:
/// once run, every tab's list outgrows any cap, so the window would only
/// move in the empty and running states — growing when a walk lands,
/// shrinking on a tab switch.
pub const STORAGE_WINDOW_SIZE: (f32, f32) = (720., 480.);
/// Floor for both auxiliary windows. Has to stay under both opening
/// sizes: a minimum wider than the opening width would silently widen
/// the window.
pub const AUX_MIN_WINDOW_SIZE: (f32, f32) = (460., 420.);

/// Gap between the tray icon and the top of the window.
pub const TRAY_GAP: f32 = 6.;
/// Hang the window under the tray icon: horizontally centred on it, `TRAY_GAP`
/// below it. Centring is the default; the clamp only kicks in when the window
/// would run past a screen edge, in which case it sits flush against that edge.
///
/// Pure geometry, all in logical pixels, so it's testable without an `App`.
fn anchored_origin(icon: Rect, window_size: Size, screen: Rect) -> Point {
    let mut origin = point(
        icon.origin.x + icon.size.width / 2. - window_size.width / 2.,
        icon.origin.y + icon.size.height + TRAY_GAP,
    );
    // `.max(origin)` guards the degenerate case of a window wider than the
    // screen, where the upper clamp bound would fall below the lower one.
    let max_x = (screen.origin.x + screen.size.width - window_size.width).max(screen.origin.x);
    let max_y = (screen.origin.y + screen.size.height - window_size.height).max(screen.origin.y);
    origin.x = origin.x.clamp(screen.origin.x, max_x);
    origin.y = origin.y.clamp(screen.origin.y, max_y);
    origin
}

/// One display in `geom.rs`'s space, plus the backing scale tray-icon used
/// for it.
///
/// `frame` is the full screen (menu bar included) so a status item sitting
/// in the menu bar still `contains`s; `visible` is what the panel clamps to.
#[derive(Clone, Copy, Debug)]
pub struct ScreenGeom {
    pub frame: Rect,
    pub visible: Rect,
    pub scale: f32,
}

/// Status-item windows are the menu-bar strip: ~22–24pt tall. tray-icon
/// reports `logical × that screen's backingScaleFactor`, so recovering
/// with the *wrong* screen's scale yields half or double this height —
/// which is how a 1x icon whose physical x still lands inside the 2x
/// primary is rejected. Width is not a signal: Both mode's title makes
/// the item arbitrarily wide, the height does not change.
const EXPECTED_ICON_HEIGHT: f32 = 24.0;

/// tray-icon's physical rect → logical points, under one screen's scale.
fn logical_icon(anchor: TrayAnchor, scale: f32) -> Rect {
    let scale = if scale > 0. { scale } else { 1. };
    let to_points = |v: f64| v as f32 / scale;
    Rect {
        origin: point(to_points(anchor.x), to_points(anchor.y)),
        size: size(to_points(anchor.width), to_points(anchor.height)),
    }
}

struct ResolvedIcon {
    icon: Rect,
    visible: Option<Rect>,
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
        let score = (icon.size.height - EXPECTED_ICON_HEIGHT).abs();
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

/// The panel's height on a screen with `visible` of height to give:
/// what it asks for, or what fits under the tray gap. Never under
/// [`CUT_FLOOR`].
fn fitted_height(wanted: f32, visible: f32) -> f32 {
    wanted.min(visible - TRAY_GAP).max(CUT_FLOOR)
}

/// Where a panel's top edge and height land when it is given a new
/// height, in AppKit's coordinates (y grows upwards): `floor` and
/// `ceiling` are the bottom of the screen's usable area and the highest
/// the panel's top edge may sit — the tray gap under that area's top.
///
/// The top edge stays where it is while the new height fits under it.
/// When it does not, the panel rises first, as far as `ceiling`, and
/// only then gives up height. A top edge already above `ceiling` is
/// never moved down.
///
/// The rise is for a panel that was not hung from the tray — a debug
/// run opens it mid-screen, and so does "Show Window": it used to be
/// cut to what was left under wherever it happened to be, so a page
/// sized not to scroll scrolled with half a screen free above it. A
/// panel hung from the tray is at `ceiling` already and only the cut
/// applies.
pub fn resized(top: f64, height: f64, floor: f64, ceiling: f64) -> (f64, f64) {
    let top = if top - height < floor {
        (floor + height).min(ceiling.max(top))
    } else {
        top
    };
    (top, height.min(top - floor))
}

/// [`anchored_origin`] for a tray icon: its physical rect converted to
/// logical points under the right screen's scale (`screens`, from the
/// platform; with none, `fallback_scale` — the window's last-known one),
/// the screen to clamp against (that screen's visible area, or whatever
/// `visible_containing` says contains the icon), and the height cut to
/// what that screen has room for.
pub fn below_tray(
    anchor: TrayAnchor,
    window_size: Size,
    screens: &[ScreenGeom],
    fallback_scale: f32,
    visible_containing: impl FnOnce(Point) -> Option<Rect>,
) -> Rect {
    let resolved = if screens.is_empty() {
        // No list (off the main thread, or not macOS): whatever the main
        // window last reported, which is the pre-fix fallback.
        ResolvedIcon {
            icon: logical_icon(anchor, fallback_scale),
            visible: None,
            scale: if fallback_scale > 0. {
                fallback_scale
            } else {
                1.
            },
        }
    } else {
        resolve_icon(anchor, screens)
    };
    let icon = resolved.icon;
    let scale = resolved.scale;
    let screen = resolved.visible.or_else(|| visible_containing(icon.origin));
    // A panel the reader dragged taller, or a screen shorter than the
    // long Overview's 828: the body scrolls, the footer must not hang
    // off the screen.
    let window_size = match screen {
        Some(screen) => size(
            window_size.width,
            fitted_height(window_size.height, screen.size.height),
        ),
        None => window_size,
    };
    let origin = match screen {
        Some(screen) => anchored_origin(icon, window_size, screen),
        // No display info to clamp against — centre and hope for the best.
        None => point(
            icon.origin.x + icon.size.width / 2. - window_size.width / 2.,
            icon.origin.y + icon.size.height + TRAY_GAP,
        ),
    };

    let bounds = Rect {
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
                    s.frame.origin.x,
                    s.frame.origin.y,
                    s.frame.size.width,
                    s.frame.size.height
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
            icon.origin.x,
            icon.origin.y,
            icon.size.width,
            icon.size.height,
            match screen {
                Some(s) => format!(
                    "({:.0},{:.0} {:.0}x{:.0})",
                    s.origin.x, s.origin.y, s.size.width, s.size.height
                ),
                None => "none".to_string(),
            },
            bounds.origin.x,
            bounds.origin.y,
            bounds.size.width,
            bounds.size.height,
        );
        for candidate in screens {
            let try_icon = logical_icon(anchor, candidate.scale);
            let on = candidate.frame.contains(&try_icon.center());
            let score = (try_icon.size.height - EXPECTED_ICON_HEIGHT).abs();
            eprintln!(
                "POS try scale={} icon=({:.0},{:.0} {:.0}x{:.0}) on_screen={on} score={score:.0}",
                candidate.scale,
                try_icon.origin.x,
                try_icon.origin.y,
                try_icon.size.width,
                try_icon.size.height,
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
    fn screen() -> Rect {
        Rect {
            origin: point(0., 38.),
            size: size(1440., 862.),
        }
    }

    /// A 24×24 menu bar icon with its left edge at `x`.
    fn icon_at(x: f32) -> Rect {
        Rect {
            origin: point(x, 0.),
            size: size(24., 24.),
        }
    }

    fn window() -> Size {
        let (w, h) = DEFAULT_WINDOW_SIZE;
        size(w, h)
    }

    /// Given a new height, the panel keeps its top edge while that
    /// fits, rises when it does not, and is cut only when the screen
    /// itself is too short.
    #[test]
    fn a_resized_panel_rises_before_it_is_cut() {
        // A screen whose usable area runs from 0 to 1050, tray gap 6.
        let (floor, ceiling) = (0., 1044.);
        // Hung from the tray, growing: room below, nothing moves.
        assert_eq!(resized(1044., 854., floor, ceiling), (1044., 854.));
        // Shrinking never moves the top.
        assert_eq!(resized(700., 665., floor, ceiling), (700., 665.));
        // Mid-screen with 700 under the top edge: rises by what is short.
        assert_eq!(resized(700., 854., floor, ceiling), (854., 854.));
        // A screen too short even from the ceiling: top at the ceiling,
        // the rest cut.
        assert_eq!(resized(700., 854., floor, 800.), (800., 800.));
        // A top edge above the ceiling is never brought down.
        assert_eq!(resized(1050., 854., floor, ceiling), (1050., 854.));
        assert_eq!(resized(1050., 1100., floor, ceiling), (1050., 1050.));
    }

    /// A panel taller than the screen is cut to what fits under the
    /// tray gap; one that fits is left alone.
    #[test]
    fn a_panel_taller_than_the_screen_is_cut_to_fit() {
        assert_eq!(fitted_height(828., 1080.), 828.);
        assert_eq!(fitted_height(1000., 876.), 876. - TRAY_GAP);
        assert_eq!(fitted_height(828., 200.), CUT_FLOOR);
    }

    /// One width for both forms — the layout does not reflow — and the
    /// short form is the shorter window.
    #[test]
    fn the_short_overview_is_a_shorter_panel_of_the_same_width() {
        let (full_w, full_h) = window_size(true, false, Dragged::default());
        let (short_w, short_h) = window_size(false, false, Dragged::default());
        assert_eq!((full_w, full_h), DEFAULT_WINDOW_SIZE);
        assert_eq!(short_w, full_w);
        assert!(short_h < full_h);
        assert!(short_h >= MIN_WINDOW_SIZE.1);
    }

    /// The notice row is the short form's alone, and its coming and
    /// going is a step from whatever height the panel has.
    #[test]
    fn the_notice_row_is_a_step_in_the_short_form_only() {
        let (_, short) = window_size(false, false, Dragged::default());
        assert_eq!(
            window_size(false, true, Dragged::default()).1,
            short + NOTICE_HEIGHT
        );
        assert_eq!(
            window_size(true, true, Dragged::default()),
            window_size(true, false, Dragged::default())
        );
        // Fitted panel: there and back.
        assert_eq!(notice_step(short, true, None), short + NOTICE_HEIGHT);
        assert_eq!(notice_step(short + NOTICE_HEIGHT, false, None), short);
        // Dragged 100 taller: the 100 survives both ways.
        assert_eq!(
            notice_step(short + 100., true, None),
            short + 100. + NOTICE_HEIGHT
        );
        assert_eq!(
            notice_step(short + 100. + NOTICE_HEIGHT, false, None),
            short + 100.
        );
        // Cut to a short screen while the row was shown: never under
        // the form's own height.
        assert_eq!(notice_step(short + 20., false, None), short);
    }

    /// A dragged size replaces the design's: the width for both forms,
    /// the height for the form it was dragged in. The notice row still
    /// goes on top, and its going takes a panel dragged taller than the
    /// form back to where it was dragged.
    #[test]
    fn a_dragged_size_is_the_forms_size() {
        let short = |h: f32| Dragged {
            width: Some(420.),
            height: Some(h),
        };
        assert_eq!(window_size(false, false, short(650.)), (420., 650.));
        assert_eq!(
            window_size(false, true, short(650.)).1,
            650. + NOTICE_HEIGHT
        );
        assert_eq!(window_size(true, false, short(900.)), (420., 900.));
        assert_eq!(notice_step(650. + NOTICE_HEIGHT, false, Some(650.)), 650.);
        // A file that asks for less than the panel may be dragged to
        // gets the floor, which is the short Overview whole.
        let tiny = Dragged {
            width: Some(100.),
            height: Some(400.),
        };
        assert_eq!(window_size(false, false, tiny), MIN_WINDOW_SIZE);
        assert_eq!(MIN_WINDOW_SIZE.1, SIMPLE_WINDOW_HEIGHT);
        assert_eq!(min_height(true), SIMPLE_WINDOW_HEIGHT + NOTICE_HEIGHT);
    }

    /// The height decides the form: a short panel stays under the long
    /// form's height and a long one at it or over, whatever the file
    /// says — the same line a drag switches form at.
    #[test]
    fn the_long_form_starts_where_its_page_fits() {
        let full = DEFAULT_WINDOW_SIZE.1;
        assert!(detail_fits(full));
        assert!(!detail_fits(full - 1.));
        let tall = Dragged {
            width: None,
            height: Some(full + 200.),
        };
        let low = Dragged {
            width: None,
            height: Some(SIMPLE_WINDOW_HEIGHT),
        };
        assert!(!detail_fits(window_size(false, false, tall).1));
        assert!(detail_fits(window_size(true, false, low).1));
    }

    #[test]
    fn centers_under_the_icon_when_there_is_room() {
        let icon = icon_at(700.);
        let origin = anchored_origin(icon, window(), screen());
        // Icon centre 712, window 358 wide → 712 - 179.
        assert_eq!(origin.x, 533.);
        // Icon bottom 24 + 6px gap, which clears the 38px menu bar.
        assert_eq!(origin.y, 38.);
    }

    #[test]
    fn sticks_to_the_right_edge_when_the_icon_is_near_it() {
        // Centring would put the window at 1416 - 179 = 1237, whose right
        // edge (1595) overflows the 1440 screen.
        let origin = anchored_origin(icon_at(1404.), window(), screen());
        assert_eq!(origin.x, 1082.); // 1440 - 358
    }

    #[test]
    fn sticks_to_the_left_edge_when_the_icon_is_near_it() {
        let origin = anchored_origin(icon_at(4.), window(), screen());
        assert_eq!(origin.x, 0.);
    }

    #[test]
    fn never_overflows_the_bottom() {
        let tall = size(320., 2000.);
        let origin = anchored_origin(icon_at(700.), tall, screen());
        // Taller than the screen: pinned to the top of the visible area rather
        // than to a negative coordinate.
        assert_eq!(origin.y, 38.);
    }

    /// Primary 1440×900 @2x, secondary 1920×1080 @1x to its right — the
    /// mixed-DPI layout the old `screens()[0]` scale got wrong.
    fn mixed_dpi() -> Vec<ScreenGeom> {
        vec![
            ScreenGeom {
                frame: Rect {
                    origin: point(0., 0.),
                    size: size(1440., 900.),
                },
                visible: Rect {
                    origin: point(0., 24.),
                    size: size(1440., 876.),
                },
                scale: 2.0,
            },
            ScreenGeom {
                frame: Rect {
                    origin: point(1440., 0.),
                    size: size(1920., 1080.),
                },
                visible: Rect {
                    origin: point(1440., 24.),
                    size: size(1920., 1056.),
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
        assert_eq!(resolved.icon.origin, point(1600., 0.));
        assert_eq!(resolved.icon.size, size(24., 24.));
        assert_eq!(
            resolved.visible.map(|v| v.origin.x),
            Some(1440.),
            "clamp against the screen the icon is actually on"
        );
    }

    #[test]
    fn mixed_dpi_icon_on_the_2x_screen_keeps_the_retina_scale() {
        // Logical (700, 0) 24×24 on the 2x display → physical 1400, 48×48.
        let resolved = resolve_icon(anchor(1400., 0., 48., 48.), &mixed_dpi());
        assert_eq!(resolved.scale, 2.0);
        assert_eq!(resolved.icon.origin, point(700., 0.));
        assert_eq!(resolved.icon.size.height, 24.);
    }

    #[test]
    fn same_dpi_two_screens_picks_the_one_that_contains_the_icon() {
        let screens = vec![
            ScreenGeom {
                frame: Rect {
                    origin: point(0., 0.),
                    size: size(1440., 900.),
                },
                visible: Rect {
                    origin: point(0., 24.),
                    size: size(1440., 876.),
                },
                scale: 2.0,
            },
            ScreenGeom {
                frame: Rect {
                    origin: point(1440., 0.),
                    size: size(1920., 1080.),
                },
                visible: Rect {
                    origin: point(1440., 24.),
                    size: size(1920., 1056.),
                },
                scale: 2.0,
            },
        ];
        // Logical (1600, 0) 24×24 on the secondary, both @2x.
        let resolved = resolve_icon(anchor(3200., 0., 48., 48.), &screens);
        assert_eq!(resolved.scale, 2.0);
        assert_eq!(resolved.icon.origin.x, 1600.);
        assert_eq!(resolved.visible.map(|v| v.origin.x), Some(1440.));
    }

    #[test]
    fn a_2x_secondary_to_the_right_of_a_1x_primary() {
        // Menu bar moved onto the 1x display; the Retina laptop is
        // secondary. Logical (2000, 0) 24×24 on the laptop → physical ×2.
        let screens = vec![
            ScreenGeom {
                frame: Rect {
                    origin: point(0., 0.),
                    size: size(1920., 1080.),
                },
                visible: Rect {
                    origin: point(0., 24.),
                    size: size(1920., 1056.),
                },
                scale: 1.0,
            },
            ScreenGeom {
                frame: Rect {
                    origin: point(1920., 0.),
                    size: size(1440., 900.),
                },
                visible: Rect {
                    origin: point(1920., 24.),
                    size: size(1440., 876.),
                },
                scale: 2.0,
            },
        ];
        let resolved = resolve_icon(anchor(4000., 0., 48., 48.), &screens);
        assert_eq!(resolved.scale, 2.0);
        assert_eq!(resolved.icon.origin, point(2000., 0.));
    }

    #[test]
    fn mixed_dpi_stacked_laptop_below_the_monitor() {
        // 1x monitor on top, 2x laptop below. Icon on the laptop at
        // logical (700, 1080) — physical y is doubled too.
        let screens = vec![
            ScreenGeom {
                frame: Rect {
                    origin: point(0., 0.),
                    size: size(1920., 1080.),
                },
                visible: Rect {
                    origin: point(0., 24.),
                    size: size(1920., 1056.),
                },
                scale: 1.0,
            },
            ScreenGeom {
                frame: Rect {
                    origin: point(0., 1080.),
                    size: size(1440., 900.),
                },
                visible: Rect {
                    origin: point(0., 1104.),
                    size: size(1440., 876.),
                },
                scale: 2.0,
            },
        ];
        let resolved = resolve_icon(anchor(1400., 2160., 48., 48.), &screens);
        assert_eq!(resolved.scale, 2.0);
        assert_eq!(resolved.icon.origin, point(700., 1080.));
    }
}
