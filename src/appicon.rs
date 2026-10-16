//! The icon an application shows in the Dock, for a row that names an
//! app tree: Overview's top apps. A letter in a tile stood there before
//! (`overview::monogram`), and five letters in five grey tiles are read
//! one by one, where Chrome's or WeChat's own icon is recognised before
//! the name is.
//!
//! The icon is the running application's (`NSRunningApplication`, asked
//! by the tree's root pid), and only when that application is an `.app`
//! bundle that declares an icon of its own (`CFBundleIconFile` or
//! `CFBundleIconName`). A tree rooted in a shell, a command-line tool or
//! a bare daemon has none, and neither do the system's own agents
//! (`MenuBarAgent` was the one seen): AppKit hands back the generic
//! application glyph, or nothing. Those keep the letter — the same pale
//! blueprint repeated down the list said less than the letters it
//! replaced.
//!
//! Each icon is drawn once, as a small PNG, and kept for the session
//! ([`for_tree`]). The monogram's doc said why it did not do this: "the
//! icon itself is a file read per app per paint" — which a cache keyed
//! by tree makes once per app instead. A pid can be reused, so the key
//! carries the tree's name too; a recycled pid under another program
//! misses and asks again.

use gpui::Image;
use std::sync::Arc;

/// The icon for the tree rooted at `root_pid` and called `name`, or
/// `None` where the row keeps its letter (see the module doc). Cached for
/// the session, answers included that were `None`; a miss costs one
/// AppKit lookup and one small drawing.
#[cfg(target_os = "macos")]
pub fn for_tree(root_pid: u32, name: &str) -> Option<Arc<Image>> {
    use gpui::ImageFormat;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Trees the cache holds before it starts over. A session meets a
    /// few dozen apps; the cap only stops a machine that cycles through
    /// thousands of short-lived pids from growing it without end.
    const CACHE_CAP: usize = 256;

    /// A tree by its root pid and its name; the answer, `None` included.
    type Icons = HashMap<(u32, String), Option<Arc<Image>>>;

    thread_local! {
        static CACHE: RefCell<Icons> = RefCell::new(HashMap::new());
    }
    CACHE.with(|cache| {
        let key = (root_pid, name.to_string());
        if let Some(hit) = cache.borrow().get(&key) {
            return hit.clone();
        }
        let icon = native::icon_png(root_pid)
            .map(|png| Arc::new(Image::from_bytes(ImageFormat::Png, png)));
        let mut cache = cache.borrow_mut();
        if cache.len() >= CACHE_CAP {
            cache.clear();
        }
        cache.insert(key, icon.clone());
        icon
    })
}

/// No running-application registry to ask off macOS: every row keeps its
/// letter.
#[cfg(not(target_os = "macos"))]
pub fn for_tree(_root_pid: u32, _name: &str) -> Option<Arc<Image>> {
    None
}

#[cfg(target_os = "macos")]
mod native {
    use objc2::encode::{Encoding, RefEncode};
    use objc2::rc::autoreleasepool;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_app_kit::NSRunningApplication;
    use objc2_foundation::{NSBundle, NSPoint, NSRect, NSSize, ns_string};
    use std::ffi::c_void;
    use std::ptr;

    /// The drawing's edge in pixels: the rows show the icon at 22pt,
    /// which is 44px on a Retina screen and 66 at 3×.
    const ICON_PX: f64 = 64.;

    /// `NSBitmapImageFileTypePNG`.
    const PNG: usize = 4;

    /// `CGImageRef`'s pointee, opaque. Spelled out rather than borrowed
    /// as `c_void` because a debug build checks every message's types
    /// against the runtime's, and `^v` is not `^{CGImage=}` — the first
    /// build panicked on the first icon it drew.
    #[repr(C)]
    struct CGImage {
        _opaque: [u8; 0],
    }

    unsafe impl RefEncode for CGImage {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGImage", &[]));
    }

    /// The running application's icon at [`ICON_PX`], as PNG bytes —
    /// `None` when the pid is not a running `.app`, or is one that
    /// declares no icon of its own.
    pub(super) fn icon_png(pid: u32) -> Option<Vec<u8>> {
        let pid = i32::try_from(pid).ok()?;
        autoreleasepool(|_| {
            let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
            let bundle = app.bundleURL()?;
            if bundle.pathExtension()?.to_string() != "app" {
                return None;
            }
            let info = NSBundle::bundleWithURL(&bundle)?;
            let declared = [
                ns_string!("CFBundleIconFile"),
                ns_string!("CFBundleIconName"),
            ]
            .into_iter()
            .any(|key| info.objectForInfoDictionaryKey(key).is_some());
            if !declared {
                return None;
            }
            unsafe {
                let icon: *mut AnyObject = msg_send![&*app, icon];
                if icon.is_null() {
                    return None;
                }
                // The image's best representation for this size, as a
                // CGImage; AppKit picks among the icon's own sizes.
                let mut rect = NSRect::new(NSPoint::new(0., 0.), NSSize::new(ICON_PX, ICON_PX));
                let cg: *mut CGImage = msg_send![
                    icon,
                    CGImageForProposedRect: &mut rect,
                    context: ptr::null_mut::<AnyObject>(),
                    hints: ptr::null_mut::<AnyObject>()
                ];
                if cg.is_null() {
                    return None;
                }
                let rep: *mut AnyObject = msg_send![class!(NSBitmapImageRep), alloc];
                let rep: *mut AnyObject = msg_send![rep, initWithCGImage: cg];
                if rep.is_null() {
                    return None;
                }
                let properties: *mut AnyObject = msg_send![class!(NSDictionary), dictionary];
                let data: *mut AnyObject =
                    msg_send![rep, representationUsingType: PNG, properties: properties];
                let png = (!data.is_null()).then(|| {
                    let length: usize = msg_send![data, length];
                    let mut bytes = vec![0u8; length];
                    let _: () = msg_send![
                        data,
                        getBytes: bytes.as_mut_ptr().cast::<c_void>(),
                        length: length
                    ];
                    bytes
                });
                // `alloc` + `init` is ours to release; the rest is
                // autoreleased into the pool around this.
                let _: () = msg_send![rep, release];
                png.filter(|bytes| !bytes.is_empty())
            }
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use objc2_app_kit::NSWorkspace;

        /// Every message on the way to a PNG, sent for real: Finder is a
        /// running `.app` on any Mac with a session. A wrong type in one
        /// of them is a panic in a debug build, which is how the first
        /// version took the panel down the moment it drew an icon.
        #[test]
        fn a_running_app_gives_a_png_and_a_bare_process_does_not() {
            let finder = NSWorkspace::sharedWorkspace()
                .runningApplications()
                .iter()
                .find(|app| {
                    app.bundleIdentifier()
                        .is_some_and(|id| id.to_string() == "com.apple.finder")
                })
                .map(|app| app.processIdentifier());
            if let Some(pid) = finder.and_then(|pid| u32::try_from(pid).ok()) {
                let png = icon_png(pid).expect("Finder has an icon");
                assert!(png.starts_with(b"\x89PNG"), "PNG signature");
            }
            // A system agent whose bundle declares no icon keeps the
            // letter, where it is running.
            let agent = NSWorkspace::sharedWorkspace()
                .runningApplications()
                .iter()
                .find(|app| {
                    app.bundleIdentifier()
                        .is_some_and(|id| id.to_string() == "com.apple.MenuBarAgent")
                })
                .map(|app| app.processIdentifier());
            if let Some(pid) = agent.and_then(|pid| u32::try_from(pid).ok()) {
                assert!(icon_png(pid).is_none(), "the generic glyph is no icon");
            }
            // This test binary is a bare executable, not an `.app`.
            assert!(icon_png(std::process::id()).is_none());
        }
    }
}
