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
//! The store is the shared one (`store.rs`): the collector's ticks are
//! folded in on a thread of their own, a banner's click asks for the
//! Alerts tab, and showing and hiding the panel do the bookkeeping the
//! gpui front end's `mark_panel_shown` and auto-hide do — the collector's
//! pace, the tab's entry work, the transient views, the toggle's grace.
//!
//! How big the panel is follows the gpui front end's rules, from the same
//! shared answers: Overview's form and its notice row
//! (`placement::window_size`, `notice::row_shown`), what a person dragged
//! it to (`prefs::panel_width` / `panel_height`, kept when a drag ends),
//! the form switching as a drag crosses the long form's height, and the
//! least height the short form needs. The window is resizable from its
//! creation rather than patched after its first frame
//! (`window_ext::let_size_be_dragged`), and the drag's start, middle and
//! end are the delegate's own notifications.
//!
//! Where the panel goes is `placement.rs`'s, shared with the gpui front
//! end: the same sizes, and the same hang under the icon with its
//! multi-display, mixed-DPI scale picking. This side only lists AppKit's
//! screens in the space that module works in, and turns its answer back
//! into AppKit's bottom-left one.

use crate::appkit;
use crate::color;
use crate::geom::{Rect, point, size};
use crate::metrics::{self, CollectorPace};
use crate::nsh::panel;
#[cfg(debug_assertions)]
use crate::nsh::probe;
use crate::nsh::root::{self, AuxWindow, HostAction};
use crate::nsh::windows;
use crate::nsh::{store, theme};
use crate::placement::{self, Dragged, ScreenGeom, TrayAnchor};
use crate::state::{self, Tab, ZStatsAppState};
use crate::tray::{self, TrayAction};
use crate::{active, autostart, i18n, logger, notice, notify, prefs, updater};
use nsh_platform_macos::{AppOptions, AppView, Appearance};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::sel;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSColor, NSMenu, NSMenuItem, NSPanel, NSScreen, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowButton,
    NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use objc2_quartz_core::CALayer;
use std::cell::{Cell, OnceCell};
use std::fs;
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

/// `NSPopUpMenuWindowLevel` (`kCGPopUpMenuWindowLevel`): above ordinary
/// windows, as a status-item popover sits.
const POPUP_LEVEL: isize = 101;

/// What the main thread holds. AppKit objects are main-thread only, so
/// they live here rather than in a static.
pub struct Host {
    pub panel: Retained<NSPanel>,
    /// The root's surface; kept for the engine it runs, too — dropping the
    /// view stops it.
    view: Rc<AppView>,
    _delegate: Retained<PanelDelegate>,
}

/// The collector's pace, for the show and hide paths (the store holds a
/// clone of its own).
static PACE: OnceLock<CollectorPace> = OnceLock::new();

/// Whether the panel's height includes the short Overview's notice row,
/// as of the last time it was sized: what [`fit_panel`] compares the page
/// against. Window bookkeeping, as in the gpui front end.
static NOTICE_FITTED: AtomicBool = AtomicBool::new(false);

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
            // The tray click that caused this arrives next, and must not
            // read the panel it just hid as one to show (`toggle`).
            store::update(|state, _| state.mark_auto_hidden());
            tracing::info!("panel auto-hidden");
            with_host(hide);
        }

        /// A person dragging an edge, or the panel's own resize: the
        /// store learns the frame, and a drag carries Overview's form
        /// across the long form's height.
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _notification: &NSNotification) {
            with_host(|host| {
                if host.panel.inLiveResize() {
                    follow_dragged_height(host);
                }
                record_frame(host);
            });
        }

        #[unsafe(method(windowDidMove:))]
        fn window_did_move(&self, _notification: &NSNotification) {
            with_host(record_frame);
        }

        /// Only a person's drag ends a live resize; the panel's own
        /// resizes never start one.
        #[unsafe(method(windowDidEndLiveResize:))]
        fn window_did_end_live_resize(&self, _notification: &NSNotification) {
            with_host(remember_dragged_size);
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
    // A forced theme pins AppKit too, so a title bar and the system's own
    // controls agree with it.
    appkit::pin_appearance();
    // Dark until the panel's engine says which (`follow_appearance`) —
    // unless the preference already does.
    let dark = theme::appearance() != Appearance::Light;
    LAST_DARK.store(dark, Ordering::Relaxed);
    theme::follow(dark);
    tracing::info!("NSH front end starting (docs/nsh-migration.md, phase 3)");

    let mut state = ZStatsAppState::new();
    // Today's alerts survive a restart, and the tab is the session's —
    // its entry work waits for the panel to be shown (`mark_shown`).
    state.restore_alerts();
    state.restore_session_tab();
    let tab = state.tab();
    store::install(
        state,
        store::Host {
            main: on_main,
            fit_panel,
            storage_window_open: windows::storage_open,
        },
    );

    root::connect(|action| {
        // An engine thread asks; AppKit answers on the main queue.
        dispatch2::DispatchQueue::main().exec_async(move || match action {
            HostAction::Quit => quit(),
            HostAction::Open(aux) => windows::open(aux),
            HostAction::Close(aux) => windows::close(aux),
            HostAction::Appearance(dark) => follow_appearance(dark),
            HostAction::FitForm => fit_form(),
            HostAction::Wake => {
                if let Some(pace) = PACE.get() {
                    pace.wake();
                }
            }
            HostAction::Restyle => restyle(),
        })
    });
    install_menu(mtm);

    let (panel, view) = build_panel(mtm);
    let delegate = PanelDelegate::new(mtm);
    panel.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let host = Rc::new(Host {
        panel,
        view,
        _delegate: delegate,
    });
    HOST.with(|h| {
        let _ = h.set(host.clone());
    });
    // Without a tray the debug build still opens the panel by itself.
    tray::start(on_tray);
    let clicks = notify::start();
    spawn_named("banner-clicks", move || {
        while clicks.recv_blocking().is_ok() {
            on_main(Box::new(show_alerts));
        }
    });
    // Before the collector, as in the gpui front end: the login-time
    // status goes in the log, and the first slow-burn banner can already
    // say whether its subject is in use.
    autostart::refresh();
    active::start();
    start_collector(tab);
    // Child processes that block for seconds, off the main thread.
    spawn_named("installer-sweep", updater::sweep_installer_mounts);
    // The Large files tab's last listing, which nothing reads since the
    // tab went (2026-10-16) — the gpui front end's cleanup, kept here
    // because this is the front end that stays.
    let _ = fs::remove_file(zstats::settings::default_dir().join("bigfiles.toml"));

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

/// The collector, and the thread that folds its ticks into the store.
/// `tab` is the session's, which seeds the registry channels.
fn start_collector(tab: Tab) {
    let metrics::Collector {
        pace,
        ticks,
        settings,
    } = metrics::spawn(tab);
    store::update(|state, fx| {
        if let Some(settings) = settings {
            state.set_settings(settings);
        }
        state.set_pace(pace.clone());
        state.start_abnormal_scan(fx);
    });
    let _ = PACE.set(pace);
    spawn_named("ticks", move || {
        while let Ok(tick) = ticks.recv_blocking() {
            store::update(|state, fx| state.on_tick(tick, fx));
        }
    });
}

fn spawn_named(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = thread::Builder::new().name(name.into()).spawn(f) {
        tracing::error!("could not start the {name} thread: {e}");
    }
}

/// What the panel's engine resolved, dark or not: the palette follows it
/// (`theme::follow`), and so does the ground AppKit paints under a window
/// before its root's first frame.
fn follow_appearance(dark: bool) {
    LAST_DARK.store(dark, Ordering::Relaxed);
    theme::follow(dark);
    // NSH has just walled the glass off again for the new appearance.
    with_host(clear_surface_ground);
}

/// The appearance the panel's engine last resolved, for a change of theme
/// that has to set the palette's family before any root reports again.
static LAST_DARK: AtomicBool = AtomicBool::new(true);

/// A preference every root was made with changed — the theme, the
/// language, the panel's opacity (`HostAction::Restyle`). The locale and
/// AppKit's appearance follow it, the palette takes the theme's family,
/// the menus are made again in the new words and the tray re-faces, and
/// every window's root is loaded again: a root reads its colours and its
/// words as it is made.
fn restyle() {
    i18n::init();
    appkit::pin_appearance();
    theme::follow(LAST_DARK.load(Ordering::Relaxed));
    if let Some(mtm) = MainThreadMarker::new() {
        install_menu(mtm);
    }
    tray::rebuild_menu();
    store::read(tray::sync);
    with_host(|host| {
        host.view.load(root::make::<panel::Panel>().into());
        clear_surface_ground(host);
    });
    windows::reload();
}

/// NSH fills the view that holds a root with an opaque colour — the
/// root's own when that is opaque, otherwise near-black or white by the
/// appearance — and sets it again only when that colour changes, which is
/// when the appearance turns (`follow_background` in its `system.rs`).
/// Under a translucent ground that fill is a wall in front of the glass,
/// so it is cleared after each time it can have been set: the panel shown,
/// and the appearance followed — NSH sets it on the main thread before its
/// engine hears of the change, and the panel's root reports it from the
/// engine. The prototype's finding 2; an NSH option would be the right
/// fix.
fn clear_surface_ground(host: &Host) {
    if let Some(layer) = host.view.view().layer() {
        layer.setBackgroundColor(None);
    }
}

/// NSH draws every canvas with `drawsAsynchronously` (its `view.rs`, a
/// main-thread saving measured on its Stocks demo). On macOS 27 that cost
/// the prototype ~97 MB of GPU memory for a moment on every redraw,
/// however little the canvas drew — the panel's footprint swung between
/// 26 and 126 MB — and in place it drew for less CPU, not more
/// (`~/github/nsh-panel-spike/FINDINGS.md`, finding 1). This walks the
/// panel's layers and turns it off: on show, and after every tick (the
/// store's `fit_panel`), so a chart that mounts later — a tab switched to,
/// a page's first sample — draws in place by the next one. A walk of the
/// panel's few dozen layers.
///
/// A canvas that mounts is still drawn once asynchronously first, and
/// nothing out here can prevent it: NSH creates the layer while applying
/// the engine's batch inside a Core Animation transaction of its own, and
/// that commit draws it (`surface.rs`, `drain`). Measured on Overview's
/// form switch, 2026-10-24: about 100 MB of "owned physical footprint
/// (graphics)" for a second, until the next tick's walk. Walking when the
/// canvas is first laid out was tried, from its `on_layout`: the walk runs
/// before the batch is applied, or after it was drawn, never between.
fn draw_canvases_in_place(host: &Host) {
    fn walk(layer: &CALayer) {
        if layer.drawsAsynchronously() {
            layer.setDrawsAsynchronously(false);
        }
        // SAFETY: read on the main thread, where the layer tree is
        // changed; the array is a snapshot of it.
        if let Some(children) = unsafe { layer.sublayers() } {
            for child in children.iter() {
                walk(&child);
            }
        }
    }
    if let Some(layer) = host.view.view().layer() {
        walk(&layer);
    }
}

/// A palette colour as AppKit's.
pub(super) fn ns_color(c: color::Rgba) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        f64::from(c.r),
        f64::from(c.g),
        f64::from(c.b),
        f64::from(c.a),
    )
}

/// The store's way onto the main thread (`store::Host::main`).
fn on_main(job: Box<dyn FnOnce() + Send>) {
    dispatch2::DispatchQueue::main().exec_async(job);
}

/// Where the panel was last dragged to: its width, and the height of
/// this form of Overview.
fn dragged(detail: bool) -> Dragged {
    Dragged {
        width: prefs::panel_width(),
        height: prefs::panel_height(detail),
    }
}

/// The size Overview's form wants now — its page, the notice row while
/// the short form shows one, or what a person dragged it to — noting
/// whether the row is in it.
fn panel_size() -> (f32, f32) {
    let notice = store::read(notice::row_shown);
    NOTICE_FITTED.store(notice, Ordering::Relaxed);
    let detail = prefs::overview_detail();
    placement::window_size(detail, notice, dragged(detail))
}

/// The least height a drag can reach: the short Overview's page, and the
/// notice row while [`NOTICE_FITTED`] says it is shown.
fn apply_min_height(host: &Host) {
    let height = placement::min_height(NOTICE_FITTED.load(Ordering::Relaxed));
    host.panel.setContentMinSize(NSSize::new(
        f64::from(placement::MIN_WINDOW_SIZE.0),
        f64::from(height),
    ));
}

/// The footer's form toggle: the panel takes the height of Overview's
/// form, top edge held (`set_overview_detail_pref` on the gpui front end).
fn fit_form() {
    let (width, height) = panel_size();
    with_host(|host| {
        apply_min_height(host);
        appkit::set_size_keeping_top(&host.panel, width, height, placement::TRAY_GAP);
    });
}

/// The store's `fit_panel`, after every tick: the short Overview's notice
/// row came or went, so the panel steps by its height, top edge held.
/// Nothing on all but the few ticks where the answer changed. A step from
/// the height the panel has (`placement::notice_step`), so a panel dragged
/// taller keeps what was added — the gpui front end's `sync_panel_notice`.
fn fit_panel() {
    with_host(draw_canvases_in_place);
    let notice = store::read(notice::row_shown);
    if NOTICE_FITTED.swap(notice, Ordering::Relaxed) == notice {
        return;
    }
    with_host(|host| {
        apply_min_height(host);
        let frame = host.panel.frame();
        let height =
            placement::notice_step(frame.size.height as f32, notice, dragged(false).height);
        appkit::set_size_keeping_top(
            &host.panel,
            frame.size.width as f32,
            height,
            placement::TRAY_GAP,
        );
    });
}

/// While a person drags the panel's edge, Overview's form follows the
/// height: the long form once the panel is as tall as that page needs
/// (`placement::detail_fits`), the short one under it. The height stays
/// the person's — the notice row the new form shows or hides is drawn
/// inside it, which is what [`NOTICE_FITTED`] is set to say.
fn follow_dragged_height(host: &Host) {
    let detail = placement::detail_fits(host.panel.frame().size.height as f32);
    if detail == prefs::overview_detail() {
        return;
    }
    prefs::set_overview_detail(detail);
    NOTICE_FITTED.store(store::read(notice::row_shown), Ordering::Relaxed);
    apply_min_height(host);
    // The pages read the form.
    store::changed();
}

/// A drag ended: its width is the panel's, and its height that of the
/// form on screen, from now on and after a restart — kept without the
/// short form's notice row, which comes and goes on top of it.
fn remember_dragged_size(host: &Host) {
    let size = host.panel.frame().size;
    let detail = prefs::overview_detail();
    let row = if !detail && NOTICE_FITTED.load(Ordering::Relaxed) {
        placement::NOTICE_HEIGHT
    } else {
        0.
    };
    prefs::set_panel_size(detail, size.width as f32, size.height as f32 - row);
}

/// The panel's frame and scale, for the store: the pages size their
/// lists from it, and placement falls back to the scale.
fn record_frame(host: &Host) {
    let screens = Screens::list(MainThreadMarker::from(&*host.panel));
    let bounds = screens.placement_rect(host.panel.frame());
    let scale = host.panel.backingScaleFactor() as f32;
    store::update(|state, fx| state.set_window_metrics(bounds, scale, fx));
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
        | NSWindowStyleMask::NonactivatingPanel
        | NSWindowStyleMask::Resizable;
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
    panel.setContentMinSize(NSSize::new(
        f64::from(placement::MIN_WINDOW_SIZE.0),
        f64::from(placement::min_height(NOTICE_FITTED.load(Ordering::Relaxed))),
    ));
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

    let view = AppView::new(
        mtm,
        AppOptions {
            app_id: "com.github.vicanso.zstats".to_owned(),
            size: (width as i32, height as i32),
            appearance: theme::appearance(),
            ..AppOptions::default()
        },
    );
    // The glass: the stock popover material behind the root, whose
    // luminosity clamp keeps the ground dark over a white wallpaper — the
    // gpui front end's middle layer (`use_popover_material`; CLAUDE.md on
    // vibrancy). The root paints the scheme's ground over it at the
    // panel's opacity (`palette::panel_ground`): all of it on Golden Dark,
    // a wash on the classic pair. gpui needed its own blur view under this
    // one and a material pinned against AppKit's repaints; a window that
    // is ours needs only the material.
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    let glass = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
    glass.setMaterial(NSVisualEffectMaterial::Popover);
    glass.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    glass.setState(NSVisualEffectState::Active);
    let surface = view.view();
    surface.setFrame(frame);
    surface.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    glass.addSubview(&surface);
    panel.setContentView(Some(&glass));
    view.load(root::make::<panel::Panel>().into());
    (panel, view)
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
    apply_min_height(host);
    match rect {
        Some(rect) => host.panel.setFrame_display(screens.to_appkit(rect), true),
        // No screen to place it on: where it was, at the size it wants.
        None => appkit::set_size_keeping_top(&host.panel, width, height, placement::TRAY_GAP),
    }
    host.panel.orderFrontRegardless();
    if make_key {
        host.panel.makeKeyWindow();
    }
    clear_surface_ground(host);
    draw_canvases_in_place(host);
    // A frame that did not change posts no notification.
    record_frame(host);
    mark_shown();
}

/// Panel on screen: the collector's fast cadence, the shown tab's entry
/// work (it is being visited), and if that tab is Alerts the tray's spec
/// has been seen. `shown` first — the entry work checks visibility.
fn mark_shown() {
    if let Some(pace) = PACE.get() {
        pace.shown();
    }
    store::update(|state, fx| {
        state.enter_shown_tab(fx);
        state.see_alerts_if_showing(fx);
    });
    // The panel's engine skipped every change while it was hidden; this
    // one brings it up to date before its first frame.
    store::changed();
}

pub fn hide(host: &Host) {
    host.panel.orderOut(None);
    // Yesterday's query must not greet the next open, and the collector
    // drops to the hidden cadence.
    store::update(|state, fx| state.reset_transient_views(fx));
    if let Some(pace) = PACE.get() {
        pace.hidden();
    }
}

/// A click on the status item: hide the panel if it is up, show it if
/// not — unless the click itself just auto-hid it, which is AppKit taking
/// key status away before it delivers the click (`state::TOGGLE_GRACE`).
fn toggle(anchor: Option<TrayAnchor>) {
    TOGGLING.with(|t| t.set(true));
    with_host(|host| {
        if host.panel.isVisible() {
            hide(host);
        } else if !store::update(|state, _| state.took_recent_auto_hide(state::TOGGLE_GRACE)) {
            show(host, anchor, true);
        }
    });
    TOGGLING.with(|t| t.set(false));
}

/// Shows the panel under the icon, whatever it was doing: the menu's
/// "Show Window" and a banner's click. An auto-hide mark left by the
/// click that opened the menu must not swallow it.
fn show_anchored() {
    store::update(|state, _| state.took_recent_auto_hide(state::TOGGLE_GRACE));
    with_host(|host| show(host, tray::anchor(), true));
}

/// A banner's click: the Alerts tab, then the panel — the tab first, so
/// the panel's first frame is that tab.
pub fn show_alerts() {
    store::update(|state, fx| state.set_tab(Tab::Alerts, fx));
    show_anchored();
}

/// What the status item asks for. tray-icon reports both of its events
/// from AppKit's own action, on the main thread, so they reach the host
/// directly — the gpui front end queues them for a task to pick up.
fn on_tray(action: TrayAction) {
    match action {
        TrayAction::Toggle(anchor) => toggle(anchor),
        // Under the icon, as a click would put it (`show_main_window`).
        TrayAction::Show => show_anchored(),
        TrayAction::Storage => windows::open(AuxWindow::Storage),
        TrayAction::Quit => quit(),
    }
}

fn quit() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).terminate(None);
    }
}

/// The app's menu. A menu-bar app never shows a menu bar, but its key
/// equivalents still answer: ⌘Q, ⌘W for the two ordinary windows (the
/// gpui front end binds it in their key contexts), and the Edit menu,
/// without which ⌘C / ⌘V / ⌘Z never reach a text field. The titles are
/// never seen, so only Quit's is translated, as the gpui front end's one
/// menu item is.
fn install_menu(mtm: MainThreadMarker) {
    let bar = NSMenu::new(mtm);
    let item = |menu: &NSMenu, title: &str, action: objc2::runtime::Sel, key: &str| {
        // SAFETY: AppKit's standard actions, sent down the responder
        // chain.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        };
        menu.addItem(&item);
    };
    let submenu = |title: &str| {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
        let holder = NSMenuItem::new(mtm);
        holder.setSubmenu(Some(&menu));
        bar.addItem(&holder);
        menu
    };
    let app = submenu(crate::APP_NAME);
    item(&app, &i18n::tr("common.quit"), sel!(terminate:), "q");
    let edit = submenu("Edit");
    item(&edit, "Undo", sel!(undo:), "z");
    // An upper-case key equivalent is Shift with it.
    item(&edit, "Redo", sel!(redo:), "Z");
    item(&edit, "Cut", sel!(cut:), "x");
    item(&edit, "Copy", sel!(copy:), "c");
    item(&edit, "Paste", sel!(paste:), "v");
    item(&edit, "Select All", sel!(selectAll:), "a");
    let window = submenu("Window");
    item(&window, "Close", sel!(performClose:), "w");
    NSApplication::sharedApplication(mtm).setMainMenu(Some(&bar));
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
        let flip = |r: NSRect| flip(primary_height, r);
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

    fn placement_rect(&self, r: NSRect) -> Rect {
        flip(self.primary_height, r)
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

/// An AppKit rect in `placement.rs`'s space, against the primary screen's
/// height.
fn flip(primary_height: f64, r: NSRect) -> Rect {
    Rect {
        origin: point(
            r.origin.x as f32,
            (primary_height - (r.origin.y + r.size.height)) as f32,
        ),
        size: size(r.size.width as f32, r.size.height as f32),
    }
}
