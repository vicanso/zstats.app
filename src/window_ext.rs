//! Showing, hiding and moving the panel window through AppKit.
//!
//! gpui models a window as create-or-destroy: there is no way to hide one
//! (`PlatformWindow` has no visibility control) and no way to move one
//! (`resize` only). Both are needed here — the panel is a popover that
//! appears under whichever spot the tray icon occupies, and it appears often.
//!
//! Destroying and rebuilding it on every toggle, which is what those gaps
//! force, leaks roughly 1 MB a cycle inside gpui: a window that is created and
//! removed 12 times over grows the process by ~11 MB, and an empty window with
//! no views still accounts for half of that. So this reaches past gpui to the
//! `NSWindow` and drives it directly, and the window is built exactly once.

use gpui::{Bounds, Pixels, Point, Size, Window, px};
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSScreen, NSWindow, NSWindowCollectionBehavior};
use objc2_foundation::NSPoint;
use raw_window_handle::RawWindowHandle;

/// Borrow the `NSWindow` behind a gpui window.
///
/// # Safety
/// The reference borrows from the window handle and must not outlive the
/// gpui `Window` it came from.
fn ns_window(window: &Window) -> Option<&NSWindow> {
    // Fully qualified: gpui's inherent `Window::window_handle()` (returning
    // `AnyWindowHandle`) shadows the `HasWindowHandle` trait method.
    let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return None;
    };
    unsafe {
        let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
        let ptr: *mut NSWindow = objc2::msg_send![ns_view, window];
        ptr.as_ref()
    }
}

/// Let the panel appear on whichever desktop is active, and over full-screen
/// apps.
///
/// A plain window belongs to the Space it was created in, so revealing it from
/// another desktop makes macOS switch back to that one — jarring for something
/// summoned from the menu bar, which is present on every desktop. `PopUp`
/// windows get this from gpui already, but that window kind is a
/// non-activating panel and cannot hold keyboard focus, so this window is a
/// `Normal` one and has to ask for the behaviour itself.
///
/// Set once after creation; it is a persistent window property.
pub fn join_all_spaces(window: &Window) {
    if let Some(ns) = ns_window(window) {
        ns.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                // Without this, summoning the panel while something is
                // full-screen would drop out of full-screen first.
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
    }
}

/// Take the panel off screen without destroying it.
pub fn hide(window: &Window) {
    if let Some(ns) = ns_window(window) {
        ns.orderOut(None);
    }
}

/// Whether the panel is currently on screen.
pub fn is_visible(window: &Window) -> bool {
    ns_window(window).is_some_and(|ns| ns.isVisible())
}

/// Move the panel to `origin` and bring it forward, focused.
///
/// `origin` is in gpui's coordinate space — logical pixels, top-left of the
/// primary display, y growing downwards. AppKit places windows from the
/// bottom-left with y growing up, so the y axis is flipped against the height
/// of the screen that owns the menu bar (`screens()[0]`, the same one the tray
/// geometry is resolved against).
pub fn show_at(window: &Window, origin: Point<Pixels>) {
    let Some(ns) = ns_window(window) else {
        return;
    };
    if let Some(screen_height) = menu_bar_screen_height() {
        let height = ns.frame().size.height;
        let flipped = screen_height - f64::from(origin.y) - height;
        ns.setFrameOrigin(NSPoint::new(f64::from(origin.x), flipped));
    }
    ns.makeKeyAndOrderFront(None);
}

/// Height of the display that owns the menu bar.
fn menu_bar_screen_height() -> Option<f64> {
    let mtm = objc2::MainThreadMarker::new()?;
    let screen = NSScreen::screens(mtm).firstObject()?;
    Some(screen.frame().size.height)
}

/// Visible area of the screen containing `point`, in gpui's coordinate space.
///
/// Not `cx.displays()`: gpui's macOS `PlatformDisplay::bounds()` throws the
/// global origin away and reports every display at `(0, 0)`
/// (`gpui_macos/src/display.rs` — its own comment says `CGDisplayBounds` is in
/// global coordinates, then sets `origin: Default::default()`). With two
/// screens that makes them indistinguishable, so a lookup by position always
/// matches the first one and the panel is pinned to the primary display.
///
/// `visibleFrame` excludes the menu bar and the Dock, which is what the panel
/// should be clamped into.
pub fn visible_bounds_containing(point: Point<Pixels>) -> Option<Bounds<Pixels>> {
    let mtm = objc2::MainThreadMarker::new()?;
    let screens = NSScreen::screens(mtm);
    // AppKit's global origin is the bottom-left of the primary screen, which
    // is `screens[0]`; gpui's is its top-left.
    let primary_height = screens.firstObject()?.frame().size.height;

    let mut fallback = None;
    for screen in screens.iter() {
        let frame = to_gpui(screen.frame(), primary_height);
        let visible = to_gpui(screen.visibleFrame(), primary_height);
        if fallback.is_none() {
            fallback = Some(visible);
        }
        if frame.contains(&point) {
            return Some(visible);
        }
    }
    fallback
}

/// Flip an AppKit rect (bottom-left origin, y up) into gpui's space
/// (top-left origin, y down).
fn to_gpui(rect: objc2_foundation::NSRect, primary_height: f64) -> Bounds<Pixels> {
    let top = primary_height - rect.origin.y - rect.size.height;
    Bounds {
        origin: Point {
            x: px(rect.origin.x as f32),
            y: px(top as f32),
        },
        size: Size {
            width: px(rect.size.width as f32),
            height: px(rect.size.height as f32),
        },
    }
}
