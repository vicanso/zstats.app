//! The NSH front end's GTK side on Linux: the panel, the two ordinary
//! windows, and the ways in (the tray, the socket a keybinding talks to,
//! a banner's click), all on GTK's main loop. NSH's `AppView` is one GTK
//! widget per window, running its root on an engine thread of its own;
//! each commit is one hop back to this loop.
//!
//! What it does, and why, is the gpui front end's Linux path
//! (`gpui_app.rs`, docs/omarchy-port.md), from the same shared parts: the
//! panel is a wlr-layer-shell surface the compositor anchors top-right
//! (`panel_kind` there, with the margins and namespace in `placement.rs`),
//! or a plain window when the compositor has no layer shell; it dismisses
//! itself when the keyboard or the pointer leaves, past the same two graces
//! (`state::REVEAL_GRACE`, `state::POINTER_LEAVE_GRACE`); the tray is
//! ksni's (`tray.rs`) and the socket `ipc.rs`'s. One thing GTK does that
//! gpui could not: the panel is hidden and shown again rather than
//! destroyed, since an unmapped layer surface maps again without a leak to
//! work around. The panel is not dragged — a layer surface cannot be —
//! so it takes the design's size for Overview's form.
//!
//! **Compiled, never seen.** CI and `make linux-check` build it and there
//! is no compositor in either (CLAUDE.md on Linux); Omarchy is where it
//! gets looked at.

use crate::metrics::{self, CollectorPace};
use crate::nsh::panel::Panel;
use crate::nsh::root::{self, AuxWindow, HostAction};
use crate::nsh::settings::Settings;
use crate::nsh::storage::Storage;
use crate::nsh::{store, theme};
use crate::state::{self, Tab, ZStatsAppState};
use crate::tray::{self, TrayAction};
use crate::{
    APP_NAME, active, autostart, i18n, ipc, logger, notice, notify, palette, placement, prefs,
    updater,
};
use gtk::glib;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use nsh_platform_linux::gtk4 as gtk;
use nsh_platform_linux::{AppOptions, AppView, Appearance};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use std::{fs, process, thread};

/// The application id: Wayland's app id, and the name a `.desktop` file
/// and a compositor rule match on — the gpui front end's `LINUX_APP_ID`.
const APP_ID: &str = "com.github.vicanso.zstats";

/// The collector's pace, for the show and hide paths.
static PACE: OnceLock<CollectorPace> = OnceLock::new();

/// Whether the panel's height includes the short Overview's notice row,
/// as of the last time it was sized ([`fit_panel`]).
static NOTICE_FITTED: AtomicBool = AtomicBool::new(false);

/// Whether the disk-space window is open, for the store, which asks from
/// any thread.
static STORAGE_OPEN: AtomicBool = AtomicBool::new(false);

/// The panel, and what its two dismissals need to remember.
struct PanelHost {
    window: gtk::Window,
    /// Where the pointer is, as far as the panel knows.
    motion: gtk::EventControllerMotion,
    /// Kept for the engine it runs.
    _view: Rc<AppView>,
    /// When it was last mapped: inside [`state::REVEAL_GRACE`] neither
    /// dismissal is believed.
    opened_at: Cell<Instant>,
    /// The pointer has been inside since it was mapped. Leaving is what
    /// dismisses it; a panel opened from the tray, with the cursor still
    /// on the bar, must not go on the first frame.
    pointer_was_inside: Cell<bool>,
    /// A pointer-leave is already waiting out its grace.
    leave_pending: Cell<bool>,
}

/// One open ordinary window.
struct Aux {
    window: gtk::Window,
    view: Rc<AppView>,
}

thread_local! {
    static PANEL: OnceCell<Rc<PanelHost>> = const { OnceCell::new() };
    /// Settings, then disk space.
    static AUX: RefCell<[Option<Aux>; 2]> = const { RefCell::new([None, None]) };
    static MAIN_LOOP: OnceCell<glib::MainLoop> = const { OnceCell::new() };
}

fn with_panel<R>(f: impl FnOnce(&PanelHost) -> R) -> Option<R> {
    PANEL.with(|panel| panel.get().map(|panel| f(panel)))
}

pub fn run() {
    // Arguments before anything is initialised, as in the gpui front end:
    // `--help`, `--version` and a refused flag end the process here.
    let invocation = match ipc::Invocation::from_args() {
        Ok(invocation) => invocation,
        Err(message) => {
            println!("{}", message.text);
            process::exit(message.code);
        }
    };
    let _log_guard = logger::init();
    // Before anything is built: a launch that is a keypress for the
    // process already running must not build a second one of anything.
    let listener = match ipc::claim(invocation) {
        ipc::Role::Delivered => return,
        ipc::Role::Instance(listener) => Some(listener),
        ipc::Role::Alone => None,
    };
    // X11's WM_CLASS is the program name; Wayland's app id the window's.
    glib::set_prgname(Some(APP_ID));
    if let Err(e) = gtk::init() {
        tracing::error!("GTK could not start: {e}");
        return;
    }
    prefs::load();
    i18n::init();
    // Dark until the panel's engine says which (`follow_appearance`) —
    // unless the preference already does.
    theme::follow(theme::appearance() != Appearance::Light);
    tracing::info!("NSH front end starting on GTK (docs/nsh-migration.md, phase 3)");

    let mut state = ZStatsAppState::new();
    state.restore_alerts();
    state.restore_session_tab();
    let tab = state.tab();
    store::install(
        state,
        store::Host {
            main: on_main,
            fit_panel,
            storage_window_open: || STORAGE_OPEN.load(Ordering::Relaxed),
        },
    );
    root::connect(|action| {
        on_main(Box::new(move || match action {
            HostAction::Quit => quit(),
            HostAction::Open(aux) => open_aux(aux),
            HostAction::Close(aux) => close_aux(aux),
            HostAction::Appearance(dark) => follow_appearance(dark),
        }));
    });

    let panel = build_panel();
    PANEL.with(|cell| {
        let _ = cell.set(panel);
    });
    // ksni reports on its own thread; the panel is this one's.
    tray::start(|action| on_main(Box::new(move || on_tray(action))));
    let clicks = notify::start();
    spawn_named("banner-clicks", move || {
        while clicks.recv_blocking().is_ok() {
            on_main(Box::new(show_alerts));
        }
    });
    autostart::refresh();
    active::start();
    start_collector(tab);
    spawn_named("installer-sweep", updater::sweep_installer_mounts);
    let _ = fs::remove_file(zstats::settings::default_dir().join("bigfiles.toml"));
    if let Some(listener) = listener {
        ipc::serve(listener, |invocation| {
            on_main(Box::new(move || match invocation {
                ipc::Invocation::Toggle => toggle(),
                ipc::Invocation::Launch => show(),
            }));
        });
    }
    // The first press of the keybinding has nothing to toggle: the process
    // it would have talked to is this one, starting, and a key that
    // silently starts a background app reads as a key that did nothing.
    // Debug builds open the panel too, as the gpui front end's do.
    if cfg!(debug_assertions) || invocation.opens_the_panel() {
        show();
    }

    let main_loop = glib::MainLoop::new(None, false);
    MAIN_LOOP.with(|cell| {
        let _ = cell.set(main_loop.clone());
    });
    main_loop.run();
}

/// The collector, and the thread that folds its ticks into the store.
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
/// (`theme::follow`), and so does the tray, whose glyph is inked from it
/// here (`tray::ink` — SNI has no template images).
fn follow_appearance(dark: bool) {
    theme::follow(dark);
    store::read(tray::sync);
}

/// The store's way onto the main loop (`store::Host::main`), and every
/// other thread's: an idle callback, so it runs after whatever is running
/// there now even when asked from the loop itself.
fn on_main(job: Box<dyn FnOnce() + Send>) {
    let _ = glib::idle_add_once(job);
}

fn quit() {
    MAIN_LOOP.with(|main_loop| {
        if let Some(main_loop) = main_loop.get() {
            main_loop.quit();
        }
    });
}

/// The size Overview's form wants now — the design's, since a layer
/// surface is not dragged — noting whether the notice row is in it.
fn panel_size() -> (i32, i32) {
    let notice = store::read(notice::row_shown);
    NOTICE_FITTED.store(notice, Ordering::Relaxed);
    let (width, height) = placement::window_size(
        prefs::overview_detail(),
        notice,
        placement::Dragged::default(),
    );
    (width as i32, height as i32)
}

/// The store's `fit_panel`, after every tick: the notice row came or went,
/// so the panel steps by its height. A layer surface is anchored by its
/// top edge, so the compositor keeps that edge where it was.
fn fit_panel() {
    let notice = store::read(notice::row_shown);
    if NOTICE_FITTED.swap(notice, Ordering::Relaxed) == notice {
        return;
    }
    with_panel(|panel| {
        let (width, height) = panel.window.default_size();
        let height = placement::notice_step(height as f32, notice, None);
        panel.window.set_default_size(width, height as i32);
    });
}

/// The panel: a layer surface anchored top-right on the overlay layer,
/// taking the keyboard when it is clicked into (a field in it takes
/// keystrokes), under the namespace a Hyprland `layerrule` matches — or,
/// with no layer shell (a GNOME session, say), a plain window, which
/// still shows every number and lands where the compositor puts it.
fn build_panel() -> Rc<PanelHost> {
    let window = gtk::Window::new();
    window.set_title(Some(APP_NAME));
    window.set_resizable(false);
    let (width, height) = panel_size();
    window.set_default_size(width, height);
    if gtk4_layer_shell::is_supported() {
        window.init_layer_shell();
        // Changing it breaks every user's blur rule: a fixed name, not the
        // app id.
        window.set_namespace(Some("zstats"));
        window.set_layer(Layer::Overlay);
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Right, true);
        window.set_margin(Edge::Top, placement::PANEL_MARGIN_TOP as i32);
        window.set_margin(Edge::Right, placement::PANEL_MARGIN_RIGHT as i32);
        window.set_keyboard_mode(KeyboardMode::OnDemand);
    } else {
        tracing::warn!("layer-shell unavailable; the panel is a plain window");
    }
    let view = AppView::new(AppOptions {
        app_id: APP_ID.to_owned(),
        title: APP_NAME.to_owned(),
        size: (width, height),
        appearance: theme::appearance(),
        ..AppOptions::default()
    });
    window.set_child(Some(&view.widget()));
    view.load(root::make::<Panel>().into());

    window.connect_is_active_notify(|window| {
        if !window.is_active() {
            keyboard_left();
        }
    });
    let motion = gtk::EventControllerMotion::new();
    motion.connect_enter(|_, _, _| pointer_entered());
    motion.connect_leave(|_| pointer_left());
    window.add_controller(motion.clone());
    // A close from the compositor (a keybinding that closes the focused
    // window) is a hide, as every other way out is.
    window.connect_close_request(|_| {
        hide();
        glib::Propagation::Stop
    });

    Rc::new(PanelHost {
        window,
        motion,
        _view: view,
        opened_at: Cell::new(Instant::now()),
        pointer_was_inside: Cell::new(false),
        leave_pending: Cell::new(false),
    })
}

/// Keyboard focus left the panel. Not believed inside the reveal grace,
/// nor before the pointer has been inside: on the way down from the tray
/// the cursor crosses whatever is under the bar, and Hyprland hands
/// keyboard focus back before the panel was ever reached.
fn keyboard_left() {
    let dismiss = with_panel(|panel| {
        !prefs::pinned()
            && panel.window.is_visible()
            && panel.opened_at.get().elapsed() >= state::REVEAL_GRACE
            && panel.pointer_was_inside.get()
    });
    if dismiss == Some(true) {
        auto_hide("keyboard-leave");
    }
}

/// The pointer arrived. Inside the reveal grace it does not count: an
/// enter within a frame or two of the map is the surface appearing under
/// a cursor that has not moved.
fn pointer_entered() {
    with_panel(|panel| {
        if panel.opened_at.get().elapsed() >= state::REVEAL_GRACE {
            panel.pointer_was_inside.set(true);
        }
    });
}

/// The pointer left. Once it has been inside, and if it stays out for
/// [`state::POINTER_LEAVE_GRACE`]: a panel mapped flush under the bar
/// gets an enter it was not reached for, and the small move back into the
/// bar is a leave.
fn pointer_left() {
    let wait = with_panel(|panel| {
        let wait = !prefs::pinned()
            && panel.pointer_was_inside.get()
            && panel.opened_at.get().elapsed() >= state::REVEAL_GRACE
            && !panel.leave_pending.get();
        if wait {
            panel.leave_pending.set(true);
        }
        wait
    });
    if wait != Some(true) {
        return;
    }
    let _ = glib::timeout_add_local_once(state::POINTER_LEAVE_GRACE, || {
        let gone = with_panel(|panel| {
            panel.leave_pending.set(false);
            !prefs::pinned() && panel.window.is_visible() && !panel.motion.contains_pointer()
        });
        if gone == Some(true) {
            auto_hide("pointer-leave");
        }
    });
}

/// One of the two dismissals took the panel down. Which, and after how
/// long, is logged: "it vanished before I could reach it" is asked about
/// the installed build.
fn auto_hide(path: &str) {
    let up_for = with_panel(|panel| panel.opened_at.get().elapsed().as_millis());
    tracing::info!(path, up_for_ms = up_for, "panel auto-hidden");
    store::update(|state, _| state.mark_auto_hidden());
    hide();
}

fn show() {
    let shown = with_panel(|panel| {
        // A panel already up keeps its graces: "Show Window" or a banner's
        // click over it is not a new map.
        if !panel.window.is_visible() {
            let (width, height) = panel_size();
            panel.window.set_default_size(width, height);
            panel.opened_at.set(Instant::now());
            panel.pointer_was_inside.set(false);
        }
        panel.window.present();
    });
    if shown.is_none() {
        return;
    }
    if let Some(pace) = PACE.get() {
        pace.shown();
    }
    store::update(|state, fx| {
        state.enter_shown_tab(fx);
        state.see_alerts_if_showing(fx);
    });
    store::changed();
}

fn hide() {
    with_panel(|panel| panel.window.set_visible(false));
    store::update(|state, fx| state.reset_transient_views(fx));
    if let Some(pace) = PACE.get() {
        pace.hidden();
    }
}

/// The tray's click and `zstats --toggle`. No toggle grace here, as in the
/// gpui front end's Linux path: a click on the bar is not a focus change
/// the panel sees first.
fn toggle() {
    if with_panel(|panel| panel.window.is_visible()) == Some(true) {
        hide();
    } else {
        show();
    }
}

/// A banner's click: the Alerts tab, then the panel.
fn show_alerts() {
    store::update(|state, fx| state.set_tab(Tab::Alerts, fx));
    show();
}

fn on_tray(action: TrayAction) {
    match action {
        TrayAction::Toggle(_) => toggle(),
        TrayAction::Show => show(),
        TrayAction::Storage => open_aux(AuxWindow::Storage),
        TrayAction::Quit => quit(),
        TrayAction::HostAppeared => tray::host_appeared(),
    }
}

fn slot(aux: AuxWindow) -> usize {
    match aux {
        AuxWindow::Settings => 0,
        AuxWindow::Storage => 1,
    }
}

/// Opens `aux`, or raises it if it is open. The panel is an overlay, so a
/// window opened under it could not be reached: unless pinned it goes, as
/// it does when focus leaves it.
fn open_aux(aux: AuxWindow) {
    if !prefs::pinned() && with_panel(|panel| panel.window.is_visible()) == Some(true) {
        hide();
    }
    let existing = AUX.with_borrow(|open| open[slot(aux)].as_ref().map(|a| a.window.clone()));
    if let Some(window) = existing {
        window.present();
        return;
    }
    match aux {
        AuxWindow::Settings => autostart::refresh(),
        AuxWindow::Storage => store::update(|state, fx| state.reset_storage_views(fx)),
    }
    let (size, title, root) = match aux {
        AuxWindow::Settings => (
            placement::SETTINGS_WINDOW_SIZE,
            i18n::tr("tabs.settings"),
            root::make::<Settings>(),
        ),
        AuxWindow::Storage => (
            placement::STORAGE_WINDOW_SIZE,
            i18n::tr("disk.storage_title"),
            root::make::<Storage>(),
        ),
    };
    let window = gtk::Window::new();
    window.set_title(Some(&title));
    window.set_default_size(size.0 as i32, size.1 as i32);
    let (min_w, min_h) = placement::AUX_MIN_WINDOW_SIZE;
    window.set_size_request(min_w as i32, min_h as i32);
    let view = AppView::new(AppOptions {
        app_id: APP_ID.to_owned(),
        title: title.clone(),
        size: (size.0 as i32, size.1 as i32),
        appearance: theme::appearance(),
        ..AppOptions::default()
    });
    window.set_child(Some(&view.widget()));
    view.load(root.into());
    if aux == AuxWindow::Settings {
        // Launch at login may have changed while it was behind something.
        window.connect_is_active_notify(|window| {
            if window.is_active() {
                autostart::refresh();
            }
        });
    }
    window.connect_close_request(move |_| {
        closed(aux);
        glib::Propagation::Proceed
    });
    if aux == AuxWindow::Storage {
        STORAGE_OPEN.store(true, Ordering::Relaxed);
        // A map is read by area.
        window.maximize();
    }
    window.present();
    AUX.with_borrow_mut(|open| open[slot(aux)] = Some(Aux { window, view }));
}

fn close_aux(aux: AuxWindow) {
    let window = AUX.with_borrow(|open| open[slot(aux)].as_ref().map(|a| a.window.clone()));
    if let Some(window) = window {
        window.close();
    }
}

/// `aux` is closing: its engine stops and the next open builds it again.
fn closed(aux: AuxWindow) {
    if aux == AuxWindow::Storage {
        STORAGE_OPEN.store(false, Ordering::Relaxed);
    }
    if let Some(open) = AUX.with_borrow_mut(|open| open[slot(aux)].take()) {
        open.view.stop();
    }
}
