//! AppKit window calls both front ends make on the panel. The gpui front
//! end reaches its `NSWindow` past gpui (`window_ext.rs`); the NSH one
//! builds its `NSPanel` itself. Either way what is asked of the window is
//! the same, and is here.

use crate::placement;
use objc2_app_kit::NSWindow;
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
/// Fires `windowDidResize` synchronously — the gpui front end hops it
/// off its App borrow (`window_ext::show_retained_at` says why).
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
