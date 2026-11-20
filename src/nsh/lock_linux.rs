//! Linux native desktop lock engine (GTK 4 + `gtk4-layer-shell`):
//! - On Wayland compositors supporting `wlr-layer-shell`, creates a
//!   `Layer::Overlay` surface covering every monitor (`Top | Bottom | Left |
//!   Right`), with `KeyboardMode::Exclusive` on the primary monitor so global
//!   keystrokes go only to the unlock screen.
//! - Falls back to undecorated fullscreen windows when layer-shell is not
//!   supported.
//! - Rebuilds monitor overlays if monitors are added/removed while locked.

use crate::nsh::lockscreen::LockOverlay;
use crate::nsh::{root, store, theme};
use crate::screenlock::{self, KeyInput, KeyOutcome};
use gtk::gdk;
use gtk::gio::prelude::ListModelExt;
use gtk::glib;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use nsh_platform_linux::gtk4 as gtk;
use nsh_platform_linux::{AppOptions, AppView};
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

const APP_ID: &str = "com.github.vicanso.zstats";

struct ActiveLock {
    windows: Vec<gtk::Window>,
    primary_view: Option<Rc<AppView>>,
    monitors_signal: Option<(gtk::gio::ListModel, glib::SignalHandlerId)>,
}

thread_local! {
    static LOCK: RefCell<Option<ActiveLock>> = const { RefCell::new(None) };
    static CSS_INSTALLED: OnceCell<()> = const { OnceCell::new() };
}

fn ensure_css() {
    CSS_INSTALLED.with(|cell| {
        cell.get_or_init(|| {
            if let Some(display) = gdk::Display::default() {
                let provider = gtk::CssProvider::new();
                provider.load_from_string(
                    ".zstats-lock-backdrop { background-color: rgba(10, 12, 16, 0.92); }",
                );
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
            }
        });
    });
}

/// Engages the GTK desktop lock overlay across all monitors.
pub fn engage() -> bool {
    if !screenlock::is_active() {
        return false;
    }
    if LOCK.with_borrow(|slot| slot.is_some()) {
        return true;
    }
    ensure_css();

    let lock = build_overlays();
    if lock.windows.is_empty() {
        screenlock::abort_arm();
        store::changed();
        return false;
    }

    LOCK.with_borrow_mut(|slot| {
        *slot = Some(lock);
    });
    store::changed();
    true
}

fn build_overlays() -> ActiveLock {
    let monitors = gdk::Display::default().map(|d| d.monitors());
    let count = monitors.as_ref().map_or(0, |m| m.n_items());
    let mut windows = Vec::with_capacity((count as usize).max(1));
    let mut primary_view = None;

    if count == 0 {
        let (window, view) = build_window(0, None);
        windows.push(window);
        primary_view = view;
    } else if let Some(ref model) = monitors {
        for idx in 0..count {
            let monitor = model
                .item(idx)
                .and_then(|obj| obj.downcast::<gdk::Monitor>().ok());
            let (window, view) = build_window(idx as usize, monitor.as_ref());
            windows.push(window);
            if idx == 0 {
                primary_view = view;
            }
        }
    }

    let monitors_signal = monitors.map(|model| {
        let id = model.connect_items_changed(|_, _, _, _| {
            rebuild_overlays();
        });
        (model, id)
    });

    ActiveLock {
        windows,
        primary_view,
        monitors_signal,
    }
}

fn build_window(idx: usize, monitor: Option<&gdk::Monitor>) -> (gtk::Window, Option<Rc<AppView>>) {
    let window = gtk::Window::new();
    window.add_css_class("zstats-lock-backdrop");

    if gtk4_layer_shell::is_supported() {
        window.init_layer_shell();
        if let Some(mon) = monitor {
            window.set_monitor(Some(mon));
        }
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("zstats-lock"));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(if idx == 0 {
            KeyboardMode::Exclusive
        } else {
            KeyboardMode::None
        });
    } else {
        window.set_decorated(false);
        if let Some(mon) = monitor {
            window.fullscreen_on_monitor(mon);
        } else {
            window.fullscreen();
        }
    }

    let view = if idx == 0 {
        let app_view = AppView::new(AppOptions {
            app_id: APP_ID.to_owned(),
            title: "zstats-lock".to_owned(),
            size: (1280, 720),
            appearance: theme::appearance(),
            ..AppOptions::default()
        });
        window.set_child(Some(&app_view.widget()));
        app_view.load(root::make::<LockOverlay>().into());
        Some(app_view)
    } else {
        let backdrop = gtk::Box::new(gtk::Orientation::Vertical, 0);
        backdrop.add_css_class("zstats-lock-backdrop");
        window.set_child(Some(&backdrop));
        None
    };

    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_key_pressed(|_, key, _keycode, state| {
        if !screenlock::is_active() {
            return glib::Propagation::Proceed;
        }
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let other_mods = state.intersects(
            gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SUPER_MASK
                | gdk::ModifierType::META_MASK,
        );

        let input = if ctrl && !other_mods && matches!(key, gdk::Key::u | gdk::Key::U) {
            Some(KeyInput::Clear)
        } else if ctrl || other_mods {
            None
        } else {
            match key {
                gdk::Key::Return | gdk::Key::KP_Enter => Some(KeyInput::Submit),
                gdk::Key::BackSpace | gdk::Key::Delete => Some(KeyInput::Backspace),
                gdk::Key::Escape => Some(KeyInput::Clear),
                _ => key
                    .to_unicode()
                    .filter(|c| !c.is_control())
                    .map(KeyInput::Char),
            }
        };

        if let Some(input) = input {
            match screenlock::handle_key(input) {
                KeyOutcome::Unlocked => {
                    let _ = glib::idle_add_once(disengage);
                }
                KeyOutcome::Updated | KeyOutcome::WrongPassword => store::changed(),
                KeyOutcome::RequestTouchId | KeyOutcome::Ignored => {}
            }
        }
        glib::Propagation::Stop
    });
    window.add_controller(controller);

    window.connect_close_request(|_| {
        if screenlock::is_active() {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });

    window.present();
    (window, view)
}

fn rebuild_overlays() {
    if !screenlock::is_active() {
        return;
    }
    teardown_windows_only();
    let lock = build_overlays();
    LOCK.with_borrow_mut(|slot| {
        *slot = Some(lock);
    });
}

fn teardown_windows_only() {
    let taken = LOCK.with_borrow_mut(Option::take);
    if let Some(lock) = taken {
        if let Some((model, id)) = lock.monitors_signal {
            model.disconnect(id);
        }
        if let Some(view) = lock.primary_view {
            view.stop();
        }
        for window in lock.windows {
            window.destroy();
        }
    }
}

/// Disengages the Linux desktop lock overlay and scrubs memory buffers.
pub fn disengage() {
    teardown_windows_only();
    screenlock::force_disarm();
    store::changed();
}
