//! macOS native desktop lock engine:
//! - Creates a session-level `CGEventTap` FIRST (before any window or
//!   presentation change) so a failure aborts cleanly without trapping the user.
//! - Covers every display with a `CGShieldingWindowLevel` borderless window
//!   using `NSVisualEffectMaterial::FullScreenUI`.
//! - Applies `NSApplicationPresentationOptions` to disable Dock, Menu Bar,
//!   `⌘Tab`, Force Quit, and Hide.
//! - Rebuilds monitor overlays dynamically if displays change while locked.

use crate::nsh::lockscreen::LockOverlay;
use crate::nsh::{host, root, store, theme, windows};
use crate::screenlock::{self, KeyInput, KeyOutcome};
use nsh_platform_macos::{AppOptions, AppView};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationDidChangeScreenParametersNotification,
    NSApplicationPresentationOptions, NSAutoresizingMaskOptions, NSBackingStoreType, NSColor,
    NSScreen, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
    NSVisualEffectView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType, CGShieldingWindowLevel,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObject, NSObjectProtocol, NSRect};
use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::rc::Rc;

const EVENT_MASK: CGEventMask = (1 << CGEventType::LeftMouseDown.0)
    | (1 << CGEventType::LeftMouseUp.0)
    | (1 << CGEventType::RightMouseDown.0)
    | (1 << CGEventType::RightMouseUp.0)
    | (1 << CGEventType::MouseMoved.0)
    | (1 << CGEventType::LeftMouseDragged.0)
    | (1 << CGEventType::RightMouseDragged.0)
    | (1 << CGEventType::KeyDown.0)
    | (1 << CGEventType::KeyUp.0)
    | (1 << CGEventType::FlagsChanged.0)
    | (1 << 14) // NX_SYSDEFINED (media / Mission Control / special keys)
    | (1 << CGEventType::ScrollWheel.0)
    | (1 << CGEventType::TabletPointer.0)
    | (1 << CGEventType::TabletProximity.0)
    | (1 << CGEventType::OtherMouseDown.0)
    | (1 << CGEventType::OtherMouseUp.0)
    | (1 << CGEventType::OtherMouseDragged.0);

define_class!(
    // SAFETY: `NSWindow` subclass with no extra ivars; overrides only key/main
    // eligibility so a borderless shielding window can take key status.
    #[unsafe(super(NSWindow))]
    #[thread_kind = MainThreadOnly]
    #[name = "ZStatsLockOverlayWindow"]
    struct LockOverlayWindow;

    impl LockOverlayWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            true
        }
    }
);

impl LockOverlayWindow {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let alloc = Self::alloc(mtm).set_ivars(());
        // SAFETY: `initWithContentRect:styleMask:backing:defer:` is the
        // designated initializer for `NSWindow`.
        let window: Retained<Self> = unsafe {
            msg_send![
                super(alloc),
                initWithContentRect: frame,
                styleMask: NSWindowStyleMask::Borderless,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        // SAFETY: owned by `ActiveLock` and explicitly hidden/dropped on disengage.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setLevel(CGShieldingWindowLevel() as isize);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window
    }
}

define_class!(
    // SAFETY: `NSObject` subclass with no extra ivars; invoked on the main
    // thread by `NSNotificationCenter`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ZStatsLockScreenObserver"]
    struct ScreenObserver;

    unsafe impl NSObjectProtocol for ScreenObserver {}

    impl ScreenObserver {
        #[unsafe(method(screensChanged:))]
        fn screens_changed(&self, _notification: &NSNotification) {
            rebuild_overlays();
        }
    }
);

impl ScreenObserver {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `NSObject`'s designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

struct ActiveLock {
    tap_port: CFRetained<CFMachPort>,
    run_loop_source: CFRetained<CFRunLoopSource>,
    windows: Vec<Retained<LockOverlayWindow>>,
    primary_view: Option<Rc<AppView>>,
    observer: Retained<ScreenObserver>,
}

struct RetiredOverlay {
    _windows: Vec<Retained<LockOverlayWindow>>,
    _primary_view: Option<Rc<AppView>>,
}

thread_local! {
    static LOCK: RefCell<Option<ActiveLock>> = const { RefCell::new(None) };
    static RETIRED: RefCell<Vec<RetiredOverlay>> = const { RefCell::new(Vec::new()) };
}

/// Engages the macOS desktop lock on the main thread. Returns `false` and
/// rolls back `screenlock` state if Accessibility is missing or `CGEventTap`
/// creation fails.
pub fn engage() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        screenlock::abort_arm();
        store::changed();
        return false;
    };
    if !screenlock::is_active() {
        return false;
    }
    if LOCK.with_borrow(|l| l.is_some()) {
        return true;
    }
    if !screenlock::refresh_accessibility() {
        screenlock::abort_arm();
        store::changed();
        return false;
    }

    // Create and install the CGEventTap FIRST before presenting any overlay.
    // SAFETY: `event_tap_callback` matches `CGEventTapCallBack` and never
    // accesses `user_info`.
    let Some(tap_port) = (unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            EVENT_MASK,
            Some(event_tap_callback),
            ptr::null_mut(),
        )
    }) else {
        tracing::warn!("CGEventTapCreate returned None; aborting screen lock");
        screenlock::abort_arm();
        store::changed();
        return false;
    };

    let Some(run_loop_source) = CFMachPort::new_run_loop_source(None, Some(&tap_port), 0) else {
        tap_port.invalidate();
        screenlock::abort_arm();
        store::changed();
        return false;
    };

    let Some(run_loop) = CFRunLoop::current() else {
        tap_port.invalidate();
        screenlock::abort_arm();
        store::changed();
        return false;
    };

    // SAFETY: `kCFRunLoopCommonModes` is a static CFRunLoopMode string.
    run_loop.add_source(Some(&run_loop_source), unsafe { kCFRunLoopCommonModes });
    CGEvent::tap_enable(&tap_port, true);

    host::with_host(host::hide);
    windows::close(root::AuxWindow::LockSetup);

    let (windows, primary_view) = build_overlay_windows(mtm);
    let app = NSApplication::sharedApplication(mtm);
    app.activate();
    if let Some(primary) = windows.first() {
        primary.makeKeyAndOrderFront(None);
    }

    // Valid AppKit presentation options combination (`HideDock` without
    // `AutoHideDock`, `HideMenuBar` without `AutoHideMenuBar`).
    app.setPresentationOptions(
        NSApplicationPresentationOptions::HideDock
            | NSApplicationPresentationOptions::HideMenuBar
            | NSApplicationPresentationOptions::DisableProcessSwitching
            | NSApplicationPresentationOptions::DisableForceQuit
            | NSApplicationPresentationOptions::DisableHideApplication
            | NSApplicationPresentationOptions::DisableAppleMenu
            | NSApplicationPresentationOptions::DisableCursorLocationAssistance,
    );

    let observer = ScreenObserver::new(mtm);
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: `observer` implements `screensChanged:` and is removed in `disengage`.
    unsafe {
        center.addObserver_selector_name_object(
            &observer,
            sel!(screensChanged:),
            Some(NSApplicationDidChangeScreenParametersNotification),
            None,
        );
    }

    LOCK.with_borrow_mut(|slot| {
        *slot = Some(ActiveLock {
            tap_port,
            run_loop_source,
            windows,
            primary_view,
            observer,
        });
    });

    store::changed();
    true
}

fn build_overlay_windows(
    mtm: MainThreadMarker,
) -> (Vec<Retained<LockOverlayWindow>>, Option<Rc<AppView>>) {
    let screens = NSScreen::screens(mtm);
    let mut windows = Vec::with_capacity(screens.count().max(1));
    let mut primary_view = None;

    for (idx, screen) in screens.iter().enumerate() {
        let frame = screen.frame();
        let window = LockOverlayWindow::new(mtm, frame);
        let local = NSRect::new(objc2_foundation::NSPoint::new(0., 0.), frame.size);

        let glass = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), local);
        glass.setMaterial(NSVisualEffectMaterial::FullScreenUI);
        glass.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        glass.setState(NSVisualEffectState::Active);
        glass.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );

        if idx == 0 {
            let view = AppView::new(
                mtm,
                AppOptions {
                    app_id: "com.github.vicanso.zstats".to_owned(),
                    size: (frame.size.width as i32, frame.size.height as i32),
                    appearance: theme::appearance(),
                    ..AppOptions::default()
                },
            );
            let surface = view.view();
            surface.setFrame(local);
            surface.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            glass.addSubview(&surface);
            window.setContentView(Some(&glass));
            view.load(root::make::<LockOverlay>().into());
            clear_app_view_ground(&view);
            primary_view = Some(view);
        } else {
            let tint = NSView::initWithFrame(NSView::alloc(mtm), local);
            tint.setWantsLayer(true);
            tint.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            if let Some(layer) = tint.layer() {
                let dark = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 0.52);
                layer.setBackgroundColor(Some(&dark.CGColor()));
            }
            glass.addSubview(&tint);
            window.setContentView(Some(&glass));
        }

        window.orderFrontRegardless();
        windows.push(window);
    }

    (windows, primary_view)
}

fn rebuild_overlays() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if !screenlock::is_active() {
        return;
    }
    let old = LOCK.with_borrow_mut(|slot| {
        slot.as_mut().map(|lock| {
            let prev_windows = std::mem::take(&mut lock.windows);
            let prev_view = lock.primary_view.take();
            for w in &prev_windows {
                w.orderOut(None);
            }
            let (next_windows, next_view) = build_overlay_windows(mtm);
            if let Some(primary) = next_windows.first() {
                primary.makeKeyAndOrderFront(None);
            }
            lock.windows = next_windows;
            lock.primary_view = next_view;
            RetiredOverlay {
                _windows: prev_windows,
                _primary_view: prev_view,
            }
        })
    });
    if let Some(retired) = old {
        RETIRED.with_borrow_mut(|list| list.push(retired));
        dispatch2::DispatchQueue::main().exec_async(|| {
            RETIRED.with_borrow_mut(Vec::clear);
        });
    }
}

/// Clears NSH's opaque root backing layer on the primary lock overlay view.
pub fn clear_surface_ground() {
    LOCK.with_borrow(|slot| {
        if let Some(lock) = slot
            && let Some(view) = &lock.primary_view
        {
            clear_app_view_ground(view);
        }
    });
}

fn clear_app_view_ground(view: &AppView) {
    if let Some(layer) = view.view().layer() {
        layer.setBackgroundColor(None);
    }
}

/// Disengages the macOS desktop lock on the main thread and restores system
/// presentation options.
pub fn disengage() {
    let taken = LOCK.with_borrow_mut(Option::take);
    if let Some(lock) = taken {
        CGEvent::tap_enable(&lock.tap_port, false);
        if let Some(run_loop) = CFRunLoop::current() {
            // SAFETY: `kCFRunLoopCommonModes` is a static CFRunLoopMode string.
            run_loop.remove_source(Some(&lock.run_loop_source), unsafe {
                kCFRunLoopCommonModes
            });
        }
        lock.tap_port.invalidate();

        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: `lock.observer` was registered in `engage`.
        unsafe {
            let obj: &AnyObject = &lock.observer;
            center.removeObserver(obj);
        }

        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm)
                .setPresentationOptions(NSApplicationPresentationOptions::Default);
        }

        for w in &lock.windows {
            w.orderOut(None);
        }

        RETIRED.with_borrow_mut(|list| {
            list.push(RetiredOverlay {
                _windows: lock.windows,
                _primary_view: lock.primary_view,
            });
        });
        dispatch2::DispatchQueue::main().exec_async(|| {
            RETIRED.with_borrow_mut(Vec::clear);
        });
    }
    screenlock::force_disarm();
    store::changed();
}

unsafe extern "C-unwind" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    _user_info: *mut c_void,
) -> *mut CGEvent {
    if event_type == CGEventType::TapDisabledByTimeout
        || event_type == CGEventType::TapDisabledByUserInput
    {
        LOCK.with_borrow(|slot| {
            if let Some(lock) = slot {
                CGEvent::tap_enable(&lock.tap_port, true);
            }
        });
        return event.as_ptr();
    }

    if !screenlock::is_active() {
        return event.as_ptr();
    }

    if event_type == CGEventType::KeyDown {
        // SAFETY: `event` is a valid non-null `CGEvent` provided by the tap.
        let event_ref = unsafe { event.as_ref() };
        let flags = CGEvent::flags(Some(event_ref));
        let keycode =
            CGEvent::integer_value_field(Some(event_ref), CGEventField::KeyboardEventKeycode);
        let cmd = flags.contains(CGEventFlags::MaskCommand);
        let ctrl = flags.contains(CGEventFlags::MaskControl);

        let mut outcome = KeyOutcome::Ignored;
        if cmd || ctrl {
            // Support Ctrl+U (32) and Cmd+Delete (51) to clear the entry; swallow all other shortcuts.
            if (ctrl && !cmd && keycode == 32) || (cmd && !ctrl && keycode == 51) {
                outcome = screenlock::handle_key(KeyInput::Clear);
            }
        } else {
            match keycode {
                36 | 76 => outcome = screenlock::handle_key(KeyInput::Submit),
                51 | 117 => outcome = screenlock::handle_key(KeyInput::Backspace),
                53 => outcome = screenlock::handle_key(KeyInput::Clear),
                _ => {
                    let mut len = 0;
                    let mut buf = [0u16; 8];
                    // SAFETY: `buf` holds 8 `UniChar` elements and `len` is a valid out-pointer.
                    unsafe {
                        CGEvent::keyboard_get_unicode_string(
                            Some(event_ref),
                            buf.len() as _,
                            &mut len,
                            buf.as_mut_ptr(),
                        );
                    }
                    let count = (len as usize).min(buf.len());
                    for ch in char::decode_utf16(buf[..count].iter().copied()).flatten() {
                        if !ch.is_control() {
                            let res = screenlock::handle_key(KeyInput::Char(ch));
                            if res != KeyOutcome::Ignored {
                                outcome = res;
                            }
                        }
                    }
                }
            }
        }

        match outcome {
            KeyOutcome::Unlocked => dispatch2::DispatchQueue::main().exec_async(disengage),
            KeyOutcome::Updated | KeyOutcome::WrongPassword => {
                dispatch2::DispatchQueue::main().exec_async(store::changed);
            }
            KeyOutcome::Ignored => {}
        }
    }

    // Swallow all mouse, scroll, system, and key events while locked.
    ptr::null_mut()
}
