//! The NSH front end's AppKit side: the application, the status item and
//! the panel window, all on the main thread. NSH's `AppView` is one
//! `NSView` inside the panel, running the Rust root in `panel.rs` on an
//! engine thread of its own.
//!
//! Each gpui workaround in the gpui front end has its counterpart here,
//! and most of them are simply absent (`~/github/nsh-panel-spike`'s
//! FINDINGS.md measured this shape): the activation policy is set once
//! rather than swizzled (`dock.rs`), the window is created with the style
//! mask it needs rather than patched after its first frame
//! (`window_ext.rs`), and show and hide are plain AppKit calls with no App
//! borrow to step around (`after_app_borrow`).
//!
//! The status item is `tray.rs`'s, shared with the gpui front end: the
//! same faces, menu and click handling, with what it asks for handed to
//! [`on_tray`] here.
//!
//! Where the panel goes is `placement.rs`'s, shared with the gpui front
//! end: the same sizes, and the same hang under the icon with its
//! multi-display, mixed-DPI scale picking. This side only lists AppKit's
//! screens in the space that module works in, and turns its answer back
//! into AppKit's bottom-left one.

use crate::geom::{Rect, point, size};
use crate::nsh::panel::{self, HostAction};
#[cfg(debug_assertions)]
use crate::nsh::probe;
use crate::placement::{self, Dragged, ScreenGeom, TrayAnchor};
use crate::tray::{self, TrayAction};
use crate::{i18n, logger, palette, prefs};
use nsh_platform_macos::{AppOptions, AppView, Appearance, Root};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSPanel, NSScreen,
    NSWindowButton, NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask,
    NSWindowTitleVisibility,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize};
use std::cell::{Cell, OnceCell};
use std::rc::Rc;

/// `NSPopUpMenuWindowLevel` (`kCGPopUpMenuWindowLevel`): above ordinary
/// windows, as a status-item popover sits.
const POPUP_LEVEL: isize = 101;

/// What the main thread holds. AppKit objects are main-thread only, so
/// they live here rather than in a static.
pub struct Host {
    pub panel: Retained<NSPanel>,
    /// Kept for the engine it runs: dropping the view stops it.
    _view: Rc<AppView>,
    _delegate: Retained<PanelDelegate>,
}

thread_local! {
    static HOST: OnceCell<Rc<Host>> = const { OnceCell::new() };
    /// True while a click on the status item is being handled: the panel
    /// losing key status to that very click must not hide it first.
    static TOGGLING: Cell<bool> = const { Cell::new(false) };
}

pub fn with_host<R>(f: impl FnOnce(&Host) -> R) -> Option<R> {
    HOST.with(|h| h.get().map(|h| f(h)))
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the delegate keeps
    // no state of its own.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ZStatsNshPanelDelegate"]
    pub struct PanelDelegate;

    unsafe impl NSObjectProtocol for PanelDelegate {}

    unsafe impl NSWindowDelegate for PanelDelegate {
        /// The popover's auto-hide: the panel goes when it stops being the
        /// key window — a click anywhere else.
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            if TOGGLING.with(Cell::get) || !auto_hides() {
                return;
            }
            with_host(hide);
        }
    }
);

impl PanelDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// The footer pin keeps a focused-away panel up, as in the gpui front end,
/// and so do debug builds, so the panel can be inspected while the
/// debugger or a terminal has focus.
fn auto_hides() -> bool {
    !cfg!(debug_assertions) && !prefs::pinned()
}

pub fn run() {
    // The guard flushes the rolling file's worker; dropping it at the end
    // of `run` is what makes the last lines land.
    let _log_guard = logger::init();
    let mtm = MainThreadMarker::new().expect("the NSH front end starts on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // No Dock icon, no ⌘Tab entry. One call: nothing here resets it, which
    // is what `dock.rs` swizzles `setActivationPolicy:` against for gpui.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    prefs::load();
    i18n::init();
    // Golden Dark until the NSH front end follows the theme preference
    // and the appearance (phase 3 of docs/nsh-migration.md).
    palette::set_golden(true);
    tracing::info!("NSH front end starting (docs/nsh-migration.md, phase 0)");

    panel::connect(|action| {
        // The engine thread asks; AppKit answers on the main queue.
        dispatch2::DispatchQueue::main().exec_async(move || match action {
            HostAction::Quit => quit(),
        })
    });

    let (panel, view) = build_panel(mtm);
    let delegate = PanelDelegate::new(mtm);
    panel.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let host = Rc::new(Host {
        panel,
        _view: view,
        _delegate: delegate,
    });
    HOST.with(|h| {
        let _ = h.set(host.clone());
    });
    // Without a tray the debug build still opens the panel by itself.
    tray::start(on_tray);

    // Release builds start with no window, in the menu bar until the icon
    // is clicked; debug builds open the panel at once, as the gpui front
    // end's do — without activating, so launching steals no focus.
    #[cfg(debug_assertions)]
    {
        show(&host, None, false);
        probe::start();
    }
    app.run();
}

/// The panel: a titled, non-activating panel whose title bar is hidden and
/// whose content runs to the top edge — titled so it can become key (a
/// field in it takes keystrokes), non-activating so it can do that without
/// pulling the app in front of the one being used.
fn build_panel(mtm: MainThreadMarker) -> (Retained<NSPanel>, Rc<AppView>) {
    let (width, height) = panel_size();
    let frame = NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(f64::from(width), f64::from(height)),
    );
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::FullSizeContentView
        | NSWindowStyleMask::NonactivatingPanel;
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        style,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: the host keeps the panel for the life of the process.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setTitlebarAppearsTransparent(true);
    panel.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    for button in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        if let Some(button) = panel.standardWindowButton(button) {
            button.setHidden(true);
        }
    }
    panel.setMovable(false);
    panel.setHidesOnDeactivate(false);
    // Key only when a click lands on something that takes the keyboard, or
    // when the tray click asks for it. Without this, ordering the panel
    // front made it key by itself — and a key non-activating panel takes
    // keystrokes meant for the app in front (the prototype's finding 3).
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setLevel(POPUP_LEVEL);
    // `window_ext::join_all_spaces`: the panel opens on the Space the menu
    // bar was clicked in, full-screen ones included.
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    // Golden Dark's opaque ground (`theme::default_opacity`), under the
    // page's own until the first frame lands.
    panel.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        28.0 / 255.0,
        28.0 / 255.0,
        30.0 / 255.0,
        1.0,
    )));

    let view = AppView::new(
        mtm,
        AppOptions {
            app_id: "com.github.vicanso.zstats".to_owned(),
            size: (width as i32, height as i32),
            appearance: Appearance::Dark,
            ..AppOptions::default()
        },
    );
    panel.setContentView(Some(&view.view()));
    view.load(Root::of::<panel::Panel>().into());
    (panel, view)
}

/// The short Overview's size, the design's (`placement::window_size`).
fn panel_size() -> (f32, f32) {
    placement::window_size(false, false, Dragged::default())
}

/// Shows the panel, under the status item's `anchor` (physical pixels from
/// the top left of the primary screen) or in the main screen's top-right
/// corner.
pub fn show(host: &Host, anchor: Option<TrayAnchor>, make_key: bool) {
    let mtm = MainThreadMarker::from(&*host.panel);
    let (width, height) = panel_size();
    let screens = Screens::list(mtm);
    let rect = match anchor {
        Some(anchor) => Some(placement::below_tray(
            anchor,
            size(width, height),
            &screens.geoms,
            screens.main_scale,
            |_| None,
        )),
        None => screens.corner(width, height),
    };
    if let Some(rect) = rect {
        host.panel.setFrame_display(screens.to_appkit(rect), true);
    }
    host.panel.orderFrontRegardless();
    if make_key {
        host.panel.makeKeyWindow();
    }
}

pub fn hide(host: &Host) {
    host.panel.orderOut(None);
}

fn toggle(anchor: Option<TrayAnchor>) {
    TOGGLING.with(|t| t.set(true));
    with_host(|host| {
        if host.panel.isVisible() {
            hide(host);
        } else {
            show(host, anchor, true);
        }
    });
    TOGGLING.with(|t| t.set(false));
}

/// What the status item asks for. tray-icon reports both of its events
/// from AppKit's own action, on the main thread, so they reach the host
/// directly — the gpui front end queues them for a task to pick up.
fn on_tray(action: TrayAction) {
    match action {
        TrayAction::Toggle(anchor) => toggle(anchor),
        // Under the icon, as a click would put it (`show_main_window`).
        TrayAction::Show => {
            with_host(|host| show(host, tray::anchor(), true));
        }
        TrayAction::Storage => {
            tracing::info!("the disk-space window is not in the NSH front end yet");
        }
        TrayAction::Quit => quit(),
    }
}

fn quit() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).terminate(None);
    }
}

/// AppKit's screens in `placement.rs`'s space: top-left origin, y growing
/// down from the primary screen's top edge (AppKit's grows up from its
/// bottom). The same conversion `window_ext::screens` makes for gpui.
struct Screens {
    geoms: Vec<ScreenGeom>,
    primary_height: f64,
    main_scale: f32,
    main_visible: Option<Rect>,
}

impl Screens {
    fn list(mtm: MainThreadMarker) -> Screens {
        let screens = NSScreen::screens(mtm);
        let primary_height = screens.firstObject().map_or(0.0, |s| s.frame().size.height);
        let flip = |r: NSRect| Rect {
            origin: point(
                r.origin.x as f32,
                (primary_height - (r.origin.y + r.size.height)) as f32,
            ),
            size: size(r.size.width as f32, r.size.height as f32),
        };
        let geoms = screens
            .iter()
            .map(|s| ScreenGeom {
                frame: flip(s.frame()),
                visible: flip(s.visibleFrame()),
                scale: s.backingScaleFactor() as f32,
            })
            .collect();
        let main = NSScreen::mainScreen(mtm);
        Screens {
            geoms,
            primary_height,
            main_scale: main.as_ref().map_or(2.0, |s| s.backingScaleFactor() as f32),
            main_visible: main.map(|s| flip(s.visibleFrame())),
        }
    }

    /// The main screen's top-right corner, under the menu bar: where a
    /// panel opened without a tray click goes.
    fn corner(&self, width: f32, height: f32) -> Option<Rect> {
        let visible = self.main_visible?;
        Some(Rect {
            origin: point(
                visible.origin.x + visible.size.width - width - 8.,
                visible.origin.y + placement::TRAY_GAP,
            ),
            size: size(width, height),
        })
    }

    fn to_appkit(&self, r: Rect) -> NSRect {
        NSRect::new(
            NSPoint::new(
                f64::from(r.origin.x),
                self.primary_height - f64::from(r.origin.y) - f64::from(r.size.height),
            ),
            NSSize::new(f64::from(r.size.width), f64::from(r.size.height)),
        )
    }
}
