//! macOS native desktop lock engine:
//! - Creates a session-level `CGEventTap` FIRST (before any window or
//!   presentation change) so a failure aborts cleanly without trapping the user.
//! - Covers every display with a `CGShieldingWindowLevel` borderless window
//!   using `NSVisualEffectMaterial::FullScreenUI`.
//! - Applies `NSApplicationPresentationOptions` to disable Dock, Menu Bar,
//!   `⌘Tab`, Force Quit, and Hide.
//! - Rebuilds monitor overlays dynamically if displays change while locked.

use crate::i18n;
use crate::nsh::lockscreen::LockOverlay;
use crate::nsh::{host, root, store, theme, windows};
use crate::screenlock::{self, KeyInput, KeyOutcome};
use block2::RcBlock;
use nsh_platform_macos::{AppOptions, AppView};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationDidChangeScreenParametersNotification,
    NSApplicationPresentationOptions, NSAutoresizingMaskOptions, NSBackingStoreType, NSColor,
    NSCursor, NSScreen, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowCollectionBehavior,
    NSWindowSharingType, NSWindowStyleMask,
};
use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType, CGShieldingWindowLevel,
};
use objc2_foundation::{
    NSError, NSNotification, NSNotificationCenter, NSObject, NSObjectProtocol, NSRect, NSString,
};
use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[link(name = "LocalAuthentication", kind = "framework")]
unsafe extern "C" {}

/// Popup menu window level: sits above Menu Bar (24), Dock (20), and all
/// normal windows while letting the system Touch ID sheet appear on top.
const TOUCH_ID_OVERLAY_LEVEL: isize = 101;
const DISPLAY_IDLE_DIM_SECS: u64 = 15;
static DIM_GEN: AtomicU64 = AtomicU64::new(0);

// Leave `MouseMoved` untapped: a `SessionEventTap` cannot freeze the hardware
// cursor anyway, while swallowing `MouseMoved` starves macOS Universal Control's
// edge-push detection and traps the pointer on the locked Mac. Clicks, drags,
// scroll, tablet, and keyboard/system events remain intercepted.
const EVENT_MASK: CGEventMask = (1 << CGEventType::LeftMouseDown.0)
    | (1 << CGEventType::LeftMouseUp.0)
    | (1 << CGEventType::RightMouseDown.0)
    | (1 << CGEventType::RightMouseUp.0)
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
        window.setSharingType(if screenlock::ai_friendly() {
            NSWindowSharingType::None
        } else {
            NSWindowSharingType::ReadOnly
        });
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
    secondary_tints: Vec<Retained<NSView>>,
    observer: Retained<ScreenObserver>,
    touch_id_ctx: Option<Retained<AnyObject>>,
    cursor_hidden: bool,
}

struct RetiredOverlay {
    _windows: Vec<Retained<LockOverlayWindow>>,
    _primary_view: Option<Rc<AppView>>,
    _secondary_tints: Vec<Retained<NSView>>,
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

    let dim = screenlock::dim_display_enabled();
    if dim {
        screenlock::set_dimmed(true);
    }

    let overlay = build_overlay_windows(mtm, dim);
    let app = NSApplication::sharedApplication(mtm);
    app.activate();
    if let Some(primary) = overlay.windows.first() {
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
            windows: overlay.windows,
            primary_view: overlay.primary_view,
            secondary_tints: overlay.secondary_tints,
            observer,
            touch_id_ctx: None,
            cursor_hidden: false,
        });
    });

    store::changed();
    if dim {
        set_cursor_hidden(true);
        crate::displaydim::dim_async();
    }
    true
}

struct OverlayBundle {
    windows: Vec<Retained<LockOverlayWindow>>,
    primary_view: Option<Rc<AppView>>,
    secondary_tints: Vec<Retained<NSView>>,
}

fn build_overlay_windows(mtm: MainThreadMarker, dimmed: bool) -> OverlayBundle {
    let screens = NSScreen::screens(mtm);
    let mut windows = Vec::with_capacity(screens.count().max(1));
    let mut primary_view = None;
    let mut secondary_tints = Vec::new();
    let secondary_alpha = if dimmed { 1.0 } else { 0.52 };

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
                let dark =
                    NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, secondary_alpha);
                layer.setBackgroundColor(Some(&dark.CGColor()));
            }
            glass.addSubview(&tint);
            window.setContentView(Some(&glass));
            secondary_tints.push(tint);
        }

        window.orderFrontRegardless();
        windows.push(window);
    }

    OverlayBundle {
        windows,
        primary_view,
        secondary_tints,
    }
}

fn set_secondary_tints_blackout(blackout: bool) {
    let alpha = if blackout { 1.0 } else { 0.52 };
    let color = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, alpha);
    let cg = color.CGColor();
    LOCK.with_borrow(|slot| {
        if let Some(lock) = slot {
            for tint in &lock.secondary_tints {
                if let Some(layer) = tint.layer() {
                    layer.setBackgroundColor(Some(&cg));
                }
            }
        }
    });
}

fn rebuild_overlays() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if !screenlock::is_active() {
        return;
    }
    let dimmed = screenlock::is_dimmed();
    let old = LOCK.with_borrow_mut(|slot| {
        slot.as_mut().map(|lock| {
            let prev_windows = std::mem::take(&mut lock.windows);
            let prev_view = lock.primary_view.take();
            let prev_tints = std::mem::take(&mut lock.secondary_tints);
            for w in &prev_windows {
                w.orderOut(None);
            }
            let next = build_overlay_windows(mtm, dimmed);
            if lock.touch_id_ctx.is_some() {
                for w in &next.windows {
                    w.setLevel(TOUCH_ID_OVERLAY_LEVEL);
                }
            }
            if let Some(primary) = next.windows.first() {
                primary.makeKeyAndOrderFront(None);
            }
            lock.windows = next.windows;
            lock.primary_view = next.primary_view;
            lock.secondary_tints = next.secondary_tints;
            RetiredOverlay {
                _windows: prev_windows,
                _primary_view: prev_view,
                _secondary_tints: prev_tints,
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
    DIM_GEN.fetch_add(1, Ordering::Relaxed);
    crate::displaydim::release_async();
    let taken = LOCK.with_borrow_mut(Option::take);
    if let Some(lock) = taken {
        if let Some(mut ctx) = lock.touch_id_ctx {
            // SAFETY: `ctx` is an active `LAContext`; invalidates any pending Touch ID sheet.
            unsafe {
                let _: () = msg_send![&*ctx, invalidate];
            }
            let _ = &mut ctx;
        }
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
        if lock.cursor_hidden {
            NSCursor::unhide();
        }

        RETIRED.with_borrow_mut(|list| {
            list.push(RetiredOverlay {
                _windows: lock.windows,
                _primary_view: lock.primary_view,
                _secondary_tints: lock.secondary_tints,
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

    // SAFETY: `event` is a valid non-null `CGEvent` provided by the tap.
    let event_ref = unsafe { event.as_ref() };
    if screenlock::ai_friendly() {
        let source_pid =
            CGEvent::integer_value_field(Some(event_ref), CGEventField::EventSourceUnixProcessID);
        if source_pid > 0 && source_pid != i64::from(std::process::id()) {
            return event.as_ptr();
        }
    }

    if event_type == CGEventType::KeyDown || event_type == CGEventType::KeyUp {
        let flags = CGEvent::flags(Some(event_ref));
        let keycode =
            CGEvent::integer_value_field(Some(event_ref), CGEventField::KeyboardEventKeycode);
        // Preserve macOS Universal Control's escape shortcut (`⌃⌥⌘⌫`).
        if keycode == 51
            && flags.contains(
                CGEventFlags::MaskControl | CGEventFlags::MaskAlternate | CGEventFlags::MaskCommand,
            )
        {
            return event.as_ptr();
        }
    }

    if event_type == CGEventType::KeyDown {
        if screenlock::is_dimmed() {
            dispatch2::DispatchQueue::main().exec_async(wake_and_schedule_redim);
            return ptr::null_mut();
        }

        let flags = CGEvent::flags(Some(event_ref));
        let keycode =
            CGEvent::integer_value_field(Some(event_ref), CGEventField::KeyboardEventKeycode);
        let cmd = flags.contains(CGEventFlags::MaskCommand);
        let ctrl = flags.contains(CGEventFlags::MaskControl);

        let mut inputs = Vec::new();
        if cmd || ctrl {
            // Support Ctrl+U (32) and Cmd+Delete (51) to clear the entry; swallow all other shortcuts.
            if (ctrl && !cmd && keycode == 32) || (cmd && !ctrl && keycode == 51) {
                inputs.push(KeyInput::Clear);
            }
        } else {
            match keycode {
                36 | 76 => inputs.push(KeyInput::Submit),
                51 | 117 => inputs.push(KeyInput::Backspace),
                53 => inputs.push(KeyInput::Clear),
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
                            inputs.push(KeyInput::Char(ch));
                        }
                    }
                }
            }
        }

        if !inputs.is_empty() {
            dispatch2::DispatchQueue::main().exec_async(move || handle_key_on_main(inputs));
        }
    }

    // Swallow all mouse clicks, drags, scroll, system, and key events while locked.
    ptr::null_mut()
}

fn handle_key_on_main(inputs: Vec<KeyInput>) {
    if !screenlock::is_active() {
        return;
    }
    if screenlock::dim_display_enabled() {
        schedule_redim();
    }
    for input in inputs {
        if touch_id_active()
            && matches!(
                input,
                KeyInput::Char(_) | KeyInput::Backspace | KeyInput::Clear
            )
        {
            let was_empty = screenlock::snapshot().entered_len == 0;
            cancel_touch_id();
            if input == KeyInput::Clear && was_empty {
                return;
            }
        }
        match screenlock::handle_key(input) {
            KeyOutcome::Unlocked => {
                disengage();
                return;
            }
            KeyOutcome::RequestTouchId => {
                start_touch_id();
                return;
            }
            KeyOutcome::Updated | KeyOutcome::WrongPassword => store::changed(),
            KeyOutcome::Ignored => {}
        }
    }
}

fn wake_and_schedule_redim() {
    if !screenlock::is_active() {
        return;
    }
    if screenlock::set_dimmed(false) {
        set_cursor_hidden(false);
        set_secondary_tints_blackout(false);
        store::changed();
        crate::displaydim::wake_async();
    }
    schedule_redim();
}

fn schedule_redim() {
    let epoch = DIM_GEN.fetch_add(1, Ordering::Relaxed) + 1;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(DISPLAY_IDLE_DIM_SECS));
        dispatch2::DispatchQueue::main().exec_async(move || {
            if DIM_GEN.load(Ordering::Relaxed) == epoch
                && screenlock::is_active()
                && !touch_id_active()
                && !screenlock::is_dimmed()
                && screenlock::set_dimmed(true)
            {
                set_cursor_hidden(true);
                set_secondary_tints_blackout(true);
                store::changed();
                crate::displaydim::dim_async();
            }
        });
    });
}

fn set_cursor_hidden(hidden: bool) {
    LOCK.with_borrow_mut(|slot| {
        if let Some(lock) = slot
            && lock.cursor_hidden != hidden
        {
            lock.cursor_hidden = hidden;
            if hidden {
                NSCursor::hide();
            } else {
                NSCursor::unhide();
            }
        }
    });
}

fn touch_id_active() -> bool {
    LOCK.with_borrow(|slot| {
        slot.as_ref()
            .is_some_and(|lock| lock.touch_id_ctx.is_some())
    })
}

fn set_overlay_level(level: isize) {
    LOCK.with_borrow(|slot| {
        if let Some(lock) = slot {
            for w in &lock.windows {
                w.setLevel(level);
            }
        }
    });
}

fn start_touch_id() {
    let Some(cls) = AnyClass::get(c"LAContext") else {
        screenlock::finish_touch_id();
        store::changed();
        return;
    };
    // SAFETY: `LAContext` is a standard `LocalAuthentication` NSObject subclass.
    let ctx: Option<Retained<AnyObject>> = unsafe { msg_send![cls, new] };
    let Some(ctx) = ctx else {
        screenlock::finish_touch_id();
        store::changed();
        return;
    };

    let mut err: *mut NSError = ptr::null_mut();
    // Policy 1 = `LAPolicyDeviceOwnerAuthenticationWithBiometrics`.
    // SAFETY: `ctx` is a valid `LAContext`; `err` is a valid out-pointer.
    let can: bool = unsafe { msg_send![&*ctx, canEvaluatePolicy: 1_isize, error: &mut err] };
    if !can {
        screenlock::finish_touch_id();
        store::changed();
        return;
    }

    // Empty `localizedFallbackTitle` disables fallback to macOS login password.
    let empty = NSString::from_str("");
    // SAFETY: `setLocalizedFallbackTitle:` accepts an `NSString *`.
    unsafe {
        let _: () = msg_send![&*ctx, setLocalizedFallbackTitle: &*empty];
    }

    set_overlay_level(TOUCH_ID_OVERLAY_LEVEL);
    LOCK.with_borrow_mut(|slot| {
        if let Some(lock) = slot {
            lock.touch_id_ctx = Some(ctx.clone());
        }
    });
    store::changed();

    let reason = NSString::from_str(&i18n::tr("lock.touch_id_reason"));
    let reply = RcBlock::new(move |success: Bool, _err: *mut NSError| {
        let ok = success.as_bool();
        dispatch2::DispatchQueue::main().exec_async(move || on_touch_id_result(ok));
    });
    // SAFETY: `evaluatePolicy:localizedReason:reply:` retains the block and
    // invokes it asynchronously with `(BOOL, NSError *)`.
    unsafe {
        let _: () = msg_send![
            &*ctx,
            evaluatePolicy: 1_isize,
            localizedReason: &*reason,
            reply: &*reply
        ];
    }
}

fn on_touch_id_result(success: bool) {
    if !screenlock::is_active() {
        return;
    }
    LOCK.with_borrow_mut(|slot| {
        if let Some(lock) = slot {
            lock.touch_id_ctx = None;
        }
    });
    set_overlay_level(CGShieldingWindowLevel() as isize);

    if success && screenlock::unlock_by_biometrics() {
        disengage();
        return;
    }

    screenlock::finish_touch_id();
    if screenlock::dim_display_enabled() {
        schedule_redim();
    }
    LOCK.with_borrow(|slot| {
        if let Some(lock) = slot
            && let Some(primary) = lock.windows.first()
        {
            primary.makeKeyAndOrderFront(None);
        }
    });
    store::changed();
}

fn cancel_touch_id() {
    let ctx = LOCK.with_borrow_mut(|slot| slot.as_mut().and_then(|lock| lock.touch_id_ctx.take()));
    if let Some(ctx) = ctx {
        // SAFETY: `ctx` is an active `LAContext`; invalidates the pending sheet.
        unsafe {
            let _: () = msg_send![&*ctx, invalidate];
        }
    }
    set_overlay_level(CGShieldingWindowLevel() as isize);
    screenlock::finish_touch_id();
    if screenlock::dim_display_enabled() {
        schedule_redim();
    }
    store::changed();
}
