//! The two ordinary windows beside the panel — settings and disk space —
//! on the NSH host, each an `NSWindow` with an `AppView` of its own (and
//! so an engine thread of its own) running its root (`settings.rs`,
//! `storage.rs`).
//!
//! The gpui front end's contract, kept: a second open raises the window
//! already open; closing really closes, and the next open builds a fresh
//! one. gpui could not afford that for the panel (its windows leaked about
//! 1 MB a cycle), but NSH can for these — the prototype measured a
//! rebuilt `AppView` levelling off after the first few. Both have a real
//! title bar and close with it, Escape (the roots' own key) or `mod+w`
//! (the host's Window menu). Both exist because the panel hides itself
//! the moment it loses focus, and neither a settings session nor a
//! minutes-long walk can live on a surface you cannot look away from.

use crate::nsh::host;
use crate::nsh::lockscreen::LockSetup;
use crate::nsh::root::{self, AuxWindow, FolderPick};
use crate::nsh::settings::{self, Settings};
use crate::nsh::storage::{self, Storage};
use crate::nsh::{store, theme};
use crate::{autostart, i18n, palette, placement};
use block2::RcBlock;
use nsh_platform_macos::{AppOptions, AppView};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSModalResponse, NSModalResponseOK, NSOpenPanel, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

/// One open window and what keeps it running.
struct Open {
    window: Retained<NSWindow>,
    /// Kept for the engine it runs (dropping the view stops it), and
    /// loaded again on a restyle.
    view: Rc<AppView>,
    delegate: Retained<AuxDelegate>,
}

thread_local! {
    /// Settings, disk space, then desktop lock setup ([`slot`]).
    static OPEN: RefCell<[Option<Open>; 3]> = const { RefCell::new([None, None, None]) };
    /// Windows that have just closed, kept until the main queue's next
    /// turn: the last reference to a window must not go inside its own
    /// `windowWillClose:`.
    static CLOSED: RefCell<Vec<Open>> = const { RefCell::new(Vec::new()) };
    /// Guards against opening a second folder sheet while one is already active.
    static PICKING: Cell<bool> = const { Cell::new(false) };
}

/// Whether the disk-space window is open, for the store
/// (`store::Host::storage_window_open`), which asks from any thread.
static STORAGE_OPEN: AtomicBool = AtomicBool::new(false);

fn slot(aux: AuxWindow) -> usize {
    match aux {
        AuxWindow::Settings => 0,
        AuxWindow::Storage => 1,
        AuxWindow::LockSetup => 2,
    }
}

pub fn storage_open() -> bool {
    STORAGE_OPEN.load(Ordering::Relaxed)
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the delegate keeps
    // only which window it is for.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ZStatsNshAuxDelegate"]
    #[ivars = AuxWindow]
    struct AuxDelegate;

    unsafe impl NSObjectProtocol for AuxDelegate {}

    unsafe impl NSWindowDelegate for AuxDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            closed(*self.ivars());
        }

        /// Launch at login, or Full Disk Access, may have been changed in
        /// System Settings while this window was behind it.
        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _notification: &NSNotification) {
            match *self.ivars() {
                AuxWindow::Settings => settings::became_key(),
                AuxWindow::LockSetup => {
                    crate::screenlock::refresh_accessibility();
                    store::changed();
                }
                AuxWindow::Storage => {}
            }
        }
    }
);

impl AuxDelegate {
    fn new(mtm: MainThreadMarker, aux: AuxWindow) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(aux);
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// Opens `aux`, or raises it if it is open. On the main thread.
pub fn open(aux: AuxWindow) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    // A menu-bar app is not active by itself, and a window opened behind
    // the one in use is not an answer to the click.
    NSApplication::sharedApplication(mtm).activate();
    let existing = OPEN.with_borrow(|open| open[slot(aux)].as_ref().map(|o| o.window.clone()));
    if let Some(window) = existing {
        window.makeKeyAndOrderFront(None);
        return;
    }
    match aux {
        // The moment launch at login can have moved without us.
        AuxWindow::Settings => {
            settings::opening();
            autostart::refresh();
        }
        // A fresh window, not a raised one: yesterday's opened rows are
        // not what someone reopening it is asking about.
        AuxWindow::Storage => store::update(|state, fx| state.reset_storage_views(fx)),
        AuxWindow::LockSetup => {
            crate::screenlock::clear_start_error();
            crate::screenlock::refresh_accessibility();
        }
    }
    let (size, title, root) = parts(aux);
    let window = build(mtm, size, &title, aux != AuxWindow::LockSetup);
    let view = AppView::new(
        mtm,
        AppOptions {
            app_id: "com.github.vicanso.zstats".to_owned(),
            size: (size.0 as i32, size.1 as i32),
            appearance: theme::appearance(),
            ..AppOptions::default()
        },
    );
    window.setContentView(Some(&view.view()));
    view.load(root.into());
    let delegate = AuxDelegate::new(mtm, aux);
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    window.makeKeyAndOrderFront(None);
    if aux == AuxWindow::Storage {
        STORAGE_OPEN.store(true, Ordering::Relaxed);
        // Fills the screen's visible area, menu bar and Dock kept: a map
        // is read by area. The zoom button brings back the size it was
        // built at.
        window.zoom(None);
    }
    OPEN.with_borrow_mut(|open| {
        open[slot(aux)] = Some(Open {
            window,
            view,
            delegate,
        });
    });
}

/// `aux`'s size, title and root, made in the theme and words in force.
fn parts(aux: AuxWindow) -> ((f32, f32), String, nsh_core::element::Root) {
    match aux {
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
        AuxWindow::LockSetup => (
            placement::LOCK_SETUP_WINDOW_SIZE,
            i18n::tr("lock.title"),
            root::make::<LockSetup>(),
        ),
    }
}

/// Loads every open window's root again, in the theme and the words now
/// in force, and titles it again (`host::restyle`).
pub fn reload() {
    OPEN.with_borrow(|open| {
        for aux in [
            AuxWindow::Settings,
            AuxWindow::Storage,
            AuxWindow::LockSetup,
        ] {
            if let Some(open) = &open[slot(aux)] {
                let (_, title, root) = parts(aux);
                open.window.setTitle(&NSString::from_str(&title));
                open.window
                    .setBackgroundColor(Some(&host::ns_color(palette::ground())));
                open.view.load(root.into());
            }
        }
    });
}

/// The window server's number for `aux`, while it is open (the probe
/// addresses keystrokes with it).
#[cfg(debug_assertions)]
pub fn number(aux: AuxWindow) -> Option<isize> {
    OPEN.with_borrow(|open| open[slot(aux)].as_ref().map(|o| o.window.windowNumber()))
}

/// Sets `aux`'s content to `width` × `height` points, if it is open (the
/// probe's `resize`).
#[cfg(debug_assertions)]
pub fn resize(aux: AuxWindow, width: f64, height: f64) {
    OPEN.with_borrow(|open| {
        if let Some(open) = &open[slot(aux)] {
            open.window.setContentSize(NSSize::new(width, height));
        }
    });
}

/// The system's folder panel, for `pick`: one folder, or several for the
/// exclusions. Presented asynchronously as a sheet on the disk-space window
/// (or modelessly if no parent is open) so the main thread never blocks on
/// `runModal()`. On the main thread.
pub fn pick_folder(pick: FolderPick) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if PICKING.with(Cell::get) {
        return;
    }
    PICKING.with(|p| p.set(true));

    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(false);
    panel.setCanChooseDirectories(true);
    panel.setAllowsMultipleSelection(pick == FolderPick::Exclude);
    panel.setPrompt(Some(&NSString::from_str(&storage::pick_prompt(pick))));

    let panel_done = panel.clone();
    let handler = RcBlock::new(move |response: NSModalResponse| {
        PICKING.with(|p| p.set(false));
        if response != NSModalResponseOK {
            return;
        }
        let paths: Vec<PathBuf> = panel_done
            .URLs()
            .iter()
            .filter_map(|url| url.path().map(|p| PathBuf::from(p.to_string())))
            .collect();
        storage::picked(pick, paths);
    });

    let parent = OPEN.with_borrow(|open| {
        open[slot(AuxWindow::Storage)]
            .as_ref()
            .map(|o| o.window.clone())
    });
    if let Some(parent) = parent {
        panel.beginSheetModalForWindow_completionHandler(&parent, &handler);
    } else {
        panel.beginWithCompletionHandler(&handler);
    }
}

/// Closes `aux` if it is open. On the main thread.
pub fn close(aux: AuxWindow) {
    let window = OPEN.with_borrow(|open| open[slot(aux)].as_ref().map(|o| o.window.clone()));
    if let Some(window) = window {
        window.close();
    }
}

/// `aux` is closing: forget it, and let it go on the main queue's next
/// turn.
fn closed(aux: AuxWindow) {
    if aux == AuxWindow::Storage {
        STORAGE_OPEN.store(false, Ordering::Relaxed);
    }
    let Some(open) = OPEN.with_borrow_mut(|open| open[slot(aux)].take()) else {
        return;
    };
    CLOSED.with_borrow_mut(|closed| closed.push(open));
    dispatch2::DispatchQueue::main().exec_async(|| {
        for open in CLOSED.take() {
            // The window's delegate is not retained by it.
            open.window.setDelegate(None);
            drop(open.delegate);
        }
    });
}

/// A titled window that closes, minimises and resizes, centred, on the
/// theme's ground until its root's first frame lands.
fn build(
    mtm: MainThreadMarker,
    size: (f32, f32),
    title: &str,
    resizable: bool,
) -> Retained<NSWindow> {
    let mut style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
    if resizable {
        style |= NSWindowStyleMask::Miniaturizable | NSWindowStyleMask::Resizable;
    }
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(f64::from(size.0), f64::from(size.1)),
            ),
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is owned by `Open` and dropped on purpose.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    let min_size = if resizable {
        placement::AUX_MIN_WINDOW_SIZE
    } else {
        size
    };
    window.setContentMinSize(NSSize::new(f64::from(min_size.0), f64::from(min_size.1)));
    window.setBackgroundColor(Some(&host::ns_color(palette::ground())));
    window.center();
    window
}
