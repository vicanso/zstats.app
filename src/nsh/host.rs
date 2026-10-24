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
//! Phase 0 of docs/nsh-migration.md: positioning is the prototype's
//! one-screen version of `placement::anchored_origin`, until phase 1b
//! makes that module's geometry shared.

use crate::assets::CustomIconName;
use crate::nsh::panel::{self, HostAction};
#[cfg(debug_assertions)]
use crate::nsh::probe;
use crate::{APP_NAME, glyph, i18n, logger, prefs};
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
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// The short Overview's size (`placement::DEFAULT_WINDOW_SIZE`'s width,
/// `placement::SIMPLE_WINDOW_HEIGHT`).
pub const PANEL_SIZE: (f64, f64) = (358.0, 539.0);

/// `placement::TRAY_GAP`.
const TRAY_GAP: f64 = 6.0;

/// `NSPopUpMenuWindowLevel` (`kCGPopUpMenuWindowLevel`): above ordinary
/// windows, as a status-item popover sits.
const POPUP_LEVEL: isize = 101;

const MENU_ID_SHOW: &str = "show";
const MENU_ID_QUIT: &str = "quit";

/// What the main thread holds. AppKit objects are main-thread only, so
/// they live here rather than in a static.
pub struct Host {
    pub panel: Retained<NSPanel>,
    /// Kept for the engine it runs: dropping the view stops it.
    _view: Rc<AppView>,
    _delegate: Retained<PanelDelegate>,
    _tray: Option<TrayIcon>,
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
        _tray: build_tray(),
    });
    HOST.with(|h| {
        let _ = h.set(host.clone());
    });

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
    let frame = NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(PANEL_SIZE.0, PANEL_SIZE.1),
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
            size: (PANEL_SIZE.0 as i32, PANEL_SIZE.1 as i32),
            appearance: Appearance::Dark,
            ..AppOptions::default()
        },
    );
    panel.setContentView(Some(&view.view()));
    view.load(Root::of::<panel::Panel>().into());
    (panel, view)
}

/// Shows the panel, under the status item's `anchor` (tray-icon's rect,
/// physical pixels from the top left of the primary screen) or in the main
/// screen's top-right corner.
pub fn show(host: &Host, anchor: Option<tray_icon::Rect>, make_key: bool) {
    let mtm = MainThreadMarker::from(&*host.panel);
    let origin = anchor
        .and_then(|rect| origin_under(mtm, rect))
        .or_else(|| corner_origin(mtm));
    if let Some(origin) = origin {
        host.panel.setFrameOrigin(origin);
    }
    host.panel.orderFrontRegardless();
    if make_key {
        host.panel.makeKeyWindow();
    }
}

pub fn hide(host: &Host) {
    host.panel.orderOut(None);
}

fn toggle(rect: Option<tray_icon::Rect>) {
    TOGGLING.with(|t| t.set(true));
    with_host(|host| {
        if host.panel.isVisible() {
            hide(host);
        } else {
            show(host, rect, true);
        }
    });
    TOGGLING.with(|t| t.set(false));
}

fn quit() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).terminate(None);
    }
}

/// `placement::anchored_origin` for one screen: centred under the icon,
/// `TRAY_GAP` below it, kept inside the visible frame.
fn origin_under(mtm: MainThreadMarker, rect: tray_icon::Rect) -> Option<NSPoint> {
    let screens = NSScreen::screens(mtm);
    let primary_h = screens.firstObject()?.frame().size.height;
    for screen in screens.iter() {
        let scale = screen.backingScaleFactor();
        let (x, y) = (rect.position.x / scale, rect.position.y / scale);
        let (w, h) = (
            f64::from(rect.size.width) / scale,
            f64::from(rect.size.height) / scale,
        );
        let frame = screen.frame();
        // AppKit's y grows upwards from the primary screen's bottom.
        let top = primary_h - (y + h);
        let inside = x >= frame.origin.x
            && x <= frame.origin.x + frame.size.width
            && top <= frame.origin.y + frame.size.height
            && top >= frame.origin.y;
        if !inside {
            continue;
        }
        let visible = screen.visibleFrame();
        let left = (x + w / 2.0 - PANEL_SIZE.0 / 2.0).clamp(
            visible.origin.x,
            (visible.origin.x + visible.size.width - PANEL_SIZE.0).max(visible.origin.x),
        );
        let panel_top = (top - TRAY_GAP).min(visible.origin.y + visible.size.height);
        return Some(NSPoint::new(left, panel_top - PANEL_SIZE.1));
    }
    None
}

fn corner_origin(mtm: MainThreadMarker) -> Option<NSPoint> {
    let visible = NSScreen::mainScreen(mtm)?.visibleFrame();
    Some(NSPoint::new(
        visible.origin.x + visible.size.width - PANEL_SIZE.0 - 8.0,
        visible.origin.y + visible.size.height - PANEL_SIZE.1 - TRAY_GAP,
    ))
}

/// The status item: the processor glyph as a template, the panel on a left
/// click, Show and Quit on a right click. tray-icon calls both handlers on
/// the main thread (the status item's own action), so they reach the host
/// directly — the gpui front end hops through two threads and a task to
/// get the same click to `toggle_main_window`.
fn build_tray() -> Option<TrayIcon> {
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::with_id(
        MENU_ID_SHOW,
        i18n::tr("common.show_window"),
        true,
        None,
    ));
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id(
        MENU_ID_QUIT,
        i18n::tr("common.quit"),
        true,
        None,
    ));
    MenuEvent::set_event_handler(Some(|event: MenuEvent| match event.id().0.as_str() {
        MENU_ID_SHOW => {
            with_host(|host| show(host, None, true));
        }
        MENU_ID_QUIT => quit(),
        _ => {}
    }));
    TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
        // macOS emits Click on both mouseDown and mouseUp — keying off Up
        // means a press-and-hold does not fire until the button is released.
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            rect,
            ..
        } = event
        {
            toggle(Some(rect));
        }
    }));
    // The template keeps alpha and throws the ink away, so any ink will do.
    let icon = glyph::rasterise(CustomIconName::Cpu, [0, 0, 0])
        .and_then(|rgba| Icon::from_rgba(rgba, glyph::ICON_SIZE, glyph::ICON_SIZE).ok())?;
    match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(APP_NAME)
        .with_icon(icon)
        .with_icon_as_template(true)
        .build()
    {
        Ok(tray) => Some(tray),
        Err(e) => {
            // Not fatal: the debug build still opens the panel by itself.
            tracing::error!("failed to create tray icon: {e}");
            None
        }
    }
}
