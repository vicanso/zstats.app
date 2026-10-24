//! AppKit window calls the host makes on the panel, which it builds itself
//! (`nsh/host.rs`). They were shared with the gpui front end, which
//! reached its `NSWindow` past gpui (`window_ext.rs`), and stayed here.

use crate::{omarchy, placement, prefs};
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSWindow,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

/// Give a panel a new size, keeping its top edge where it is
/// while the screen has room for that, and its left edge where it is
/// (the next reveal centres it under the tray again).
///
/// AppKit sizes a window from its bottom-left corner, so a plain resize
/// would move the edge the panel hangs from: shrinking it would leave a
/// gap under the menu bar. The frame's origin moves down by what the
/// height loses instead.
///
/// When the new height does not fit under the top edge, the panel rises
/// first — as far as `gap` under the top of the screen's usable area —
/// and only then gives up height: the footer has to stay reachable, and
/// the body scrolls. [`placement::resized`] is that rule and says what
/// it is for.
///
/// Fires `windowDidResize` synchronously, so it is called on the main
/// queue, never from inside an engine's handler.
pub fn set_size_keeping_top(ns: &NSWindow, width: f32, height: f32, gap: f32) {
    let frame = ns.frame();
    let top = frame.origin.y + frame.size.height;
    let height = f64::from(height);
    let (top, height) = match ns.screen() {
        Some(screen) => {
            let visible = screen.visibleFrame();
            let floor = visible.origin.y;
            let ceiling = floor + visible.size.height - f64::from(gap);
            placement::resized(top, height, floor, ceiling)
        }
        None => (top, height),
    };
    ns.setFrame_display(
        NSRect::new(
            NSPoint::new(frame.origin.x, top - height),
            NSSize::new(f64::from(width), height),
        ),
        true,
    );
}

/// Pin (or release) AppKit's own appearance to match the theme preference.
///
/// Without this a forced theme would only recolour our tokens: the vibrancy
/// material underneath follows the *window's* appearance, and dark type over
/// a light Popover blur is exactly the grey fog the background opacities were
/// tuned to avoid. Pinning `NSApp.appearance` makes every window — and any
/// appearance AppKit resolves for it — agree with the forced theme; `None`
/// hands control back to System Settings.
pub fn pin_appearance() {
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let appearance = match prefs::theme_in_force() {
        // Golden follows the Mac, so the material has to as well. Pinning
        // Aqua here would freeze the cream variant on a dark desktop.
        prefs::ThemePref::System | prefs::ThemePref::Golden => None,
        // Omarchy is pinned to its palette's own mode (a development
        // build is the only way it reaches a Mac).
        prefs::ThemePref::Omarchy => NSAppearance::appearanceNamed(if omarchy::palette().dark {
            // SAFETY: reading AppKit's exported appearance-name constants.
            unsafe { NSAppearanceNameDarkAqua }
        } else {
            // SAFETY: as above.
            unsafe { NSAppearanceNameAqua }
        }),
        // SAFETY: reading AppKit's exported appearance-name constants.
        prefs::ThemePref::Light => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }),
        prefs::ThemePref::Dark => {
            NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua })
        }
    };
    app.setAppearance(appearance.as_deref());
}
