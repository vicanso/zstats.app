//! Keeping the Dock icon away (macOS only).
//!
//! Two independent things put the icon there, so both have to be handled:
//!
//! 1. **LaunchServices at process start.** Handled by `LSUIElement` in
//!    Info.plist, which `make bundle` writes. Nothing in this module can help
//!    a bare `cargo run` binary — it has no Info.plist, so the icon is up for
//!    the ~50ms until [`hide_dock_icon`] runs.
//! 2. **gpui itself.** It calls `setActivationPolicy(Regular)` unconditionally
//!    in `applicationDidFinishLaunching` (`gpui_macos/src/platform.rs`), which
//!    overrides the plist. Setting the policy back afterwards is too late:
//!    the call is an IPC to the Dock, which starts its icon animation on
//!    receipt, and the animation plays out regardless of the policy flipping
//!    back microseconds later. [`suppress_regular_policy`] swallows that call
//!    instead, so the Dock is never told in the first place.
//!
//! The swizzle is deliberately narrow — it drops only `Regular` and forwards
//! everything else — but it is still a runtime patch of someone else's
//! behaviour. If gpui ever switches to a different API for this, it will
//! silently stop mattering rather than break anything.

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::sel;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use std::sync::atomic::{AtomicPtr, Ordering};

/// The implementation we displaced, called for every policy except `Regular`.
static ORIGINAL_IMP: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

type SetActivationPolicyFn =
    unsafe extern "C-unwind" fn(*mut AnyObject, Sel, NSApplicationActivationPolicy) -> Bool;

unsafe extern "C-unwind" fn set_activation_policy(
    this: *mut AnyObject,
    sel: Sel,
    policy: NSApplicationActivationPolicy,
) -> Bool {
    if policy == NSApplicationActivationPolicy::Regular {
        // Report success without telling the Dock anything.
        return Bool::YES;
    }
    let original = ORIGINAL_IMP.load(Ordering::Acquire);
    if original.is_null() {
        return Bool::NO;
    }
    // SAFETY: `original` is the IMP this method had before we replaced it, so
    // it has exactly this signature.
    let original: SetActivationPolicyFn = unsafe { std::mem::transmute(original) };
    unsafe { original(this, sel, policy) }
}

/// Make `-[NSApplication setActivationPolicy:]` ignore `Regular`.
///
/// Must run before gpui starts the run loop — call it at the top of `main`.
/// Swizzling the base class covers gpui's `NSApplication` subclass too, since
/// that subclass doesn't override this method.
pub fn suppress_regular_policy() {
    let Some(class) = AnyClass::get(c"NSApplication") else {
        return;
    };
    let Some(method) = class.instance_method(sel!(setActivationPolicy:)) else {
        return;
    };

    // Publish the old IMP *before* installing ours, so the replacement can
    // never observe a null original.
    let previous = method.implementation();
    ORIGINAL_IMP.store(previous as *mut (), Ordering::Release);

    let replacement: SetActivationPolicyFn = set_activation_policy;
    // SAFETY: `replacement` has the signature the runtime expects for this
    // selector, and forwards to the original for every policy it doesn't drop.
    unsafe {
        method.set_implementation(std::mem::transmute::<SetActivationPolicyFn, Imp>(replacement));
    }
}

/// Switch the app to `Accessory`: no Dock icon, no ⌘Tab entry.
///
/// Call from inside the `Application::run` callback. Needed even with the
/// swizzle in place, because `cargo run` binaries start out as `Regular` via
/// LaunchServices, which never goes through `setActivationPolicy:`.
pub fn hide_dock_icon() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app: Retained<NSApplication> = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
}
