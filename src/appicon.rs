//! The icon an application shows in the Dock, for a row that names a
//! program: Overview's top apps, the Apps and Processes tabs, the
//! Network tab's traffic card, and History (through [`icon_for_record`],
//! which checks the recorded pid still runs that program). A
//! letter in a tile stood there before (`widgets::monogram`), and five
//! letters in five grey tiles are read one by one, where Chrome's or
//! WeChat's own icon is recognised before the name is.
//!
//! The icon is the one of the outermost `.app` bundle the process runs
//! from — its executable's path (`proc_pidpath`) — and only when that
//! bundle declares an icon of its own (`CFBundleIconFile` or
//! `CFBundleIconName`). A bundle, not a name: a directory can be called
//! `*.app` and be none, as CI's checkout of this repository is.
//! Outermost, because a helper is a bundle of its own inside its app's
//! (`Google Chrome Helper (Renderer).app` deep in `Google Chrome.app`),
//! and the app is what the reader knows: on the Processes tab the icon
//! is what says whose a helper is, which its name often does not
//! ("MacPacketTunnel" is Shadowrocket's). The executable's path and not
//! the command line, which can name another app's bundle as an argument
//! (`open -a /Applications/X.app`). And not `NSRunningApplication`, which
//! the first version asked by the tree's root pid: it knows only what
//! LaunchServices launched, which was the Apps tab's roots and none of
//! the helpers. Like `active.rs`'s frontmost table, this asks the OS who
//! a process is, never how it is doing — no figure comes from here.
//!
//! A shell, a command-line tool or a daemon runs from no `.app`; the
//! system's own agents do, but declare no icon (`MenuBarAgent` was the
//! one seen), and AppKit would hand back the generic application glyph.
//! Those keep the letter — the same pale blueprint repeated down the
//! list said less than the letters it replaced.
//!
//! Each bundle's icon is drawn once, as a small PNG, and kept for the
//! session ([`icon_for_pid`]): six Chrome helpers share one drawing. The
//! monogram's doc said why it did not do this: "the icon itself is a
//! file read per app per paint" — which a cache by bundle makes once per
//! app. The bundle a pid runs from is kept under the pid and the
//! process's name: a pid can be reused, and a recycled pid under another
//! program misses and asks again.
//!
//! The drawing is a PNG, kept as the `data:` URI NSH shows, made once
//! with it. One cache for the process, behind a lock: NSH asks from each
//! window's engine thread, and the drawing needs no particular thread —
//! this module's test draws Finder's icon on a test runner's.

use crate::assets;
use std::sync::{Arc, OnceLock};

/// An application's icon, drawn once as a small PNG.
#[derive(Clone)]
pub struct Icon {
    /// The drawing as a `data:` URI, the way NSH takes an image — made
    /// once, not per row per frame.
    pub data_uri: Arc<str>,
}

/// The icon for the process `pid`, called `name` — or for the tree it
/// roots — or `None` where the row keeps its letter (see the module
/// doc). For a pid the caller has just seen running under that name.
/// Cached for the session, answers included that were `None`; a miss
/// costs one `proc_pidpath`, and a bundle not met before one AppKit
/// drawing.
pub fn icon_for_pid(pid: u32, name: &str) -> Option<Icon> {
    cached(pid, name, false)
}

/// [`icon_for_pid`] for a process read back from the records — History's
/// rows — whose pid may have exited since, or been handed to another
/// program. The icon is given only while the pid still runs an
/// executable of the recorded name (zstats' process name is the
/// executable's file name, as sysinfo reads it on macOS); otherwise the
/// row keeps its letter. A stranger's icon on a row would be a claim
/// about the record that nothing behind it makes.
pub fn icon_for_record(pid: u32, name: &str) -> Option<Icon> {
    cached(pid, name, true)
}

/// Both lookups, cached apart: a record that failed the name check must
/// not answer for a live row under the same pid and name, or the other
/// way round.
#[cfg(target_os = "macos")]
fn cached(pid: u32, name: &str, recorded: bool) -> Option<Icon> {
    use std::collections::HashMap;
    use std::sync::{Mutex, PoisonError};

    /// Processes the cache holds before it starts over. The Processes
    /// tab's full listing scrolls through every pid on the machine, so
    /// this is reached; starting over costs one `proc_pidpath` per row
    /// in view, since the drawings are kept apart, by bundle.
    const PID_CAP: usize = 512;

    /// Bundles the cache holds before it starts over: one per app that
    /// has run, which a session counts in dozens. Only a backstop.
    const BUNDLE_CAP: usize = 256;

    #[derive(Default)]
    struct Cache {
        /// A process by its pid, its name and whether it came from the
        /// records; the bundle it runs from, `None` included.
        bundles: HashMap<(u32, String, bool), Option<String>>,
        /// A bundle's icon, `None` included.
        icons: HashMap<String, Option<Icon>>,
    }

    static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let cache = cache.get_or_insert_with(Cache::default);
    {
        let key = (pid, name.to_string(), recorded);
        let bundle = match cache.bundles.get(&key) {
            Some(hit) => hit.clone(),
            None => {
                let bundle = native::bundle_of(pid, recorded.then_some(name));
                if cache.bundles.len() >= PID_CAP {
                    cache.bundles.clear();
                }
                cache.bundles.insert(key, bundle.clone());
                bundle
            }
        }?;
        if let Some(hit) = cache.icons.get(&bundle) {
            return hit.clone();
        }
        let icon = native::icon_png(&bundle).map(|png| Icon {
            data_uri: Arc::from(format!("data:image/png;base64,{}", base64(&png))),
        });
        if cache.icons.len() >= BUNDLE_CAP {
            cache.icons.clear();
        }
        cache.icons.insert(bundle, icon.clone());
        icon
    }
}

/// This app's own icon — the bundle's artwork, embedded as
/// `zstats-icon.png` — as a `data:` URI, for the About page; made once.
pub fn own_data_uri() -> Arc<str> {
    static URI: OnceLock<Arc<str>> = OnceLock::new();
    URI.get_or_init(|| {
        let png = assets::get("zstats-icon.png").unwrap_or_default();
        Arc::from(format!("data:image/png;base64,{}", base64(&png)))
    })
    .clone()
}

/// Standard base64 with padding (RFC 4648 §4), for [`Icon::data_uri`].
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// No bundles to read an icon from off macOS: every row keeps its
/// letter.
#[cfg(not(target_os = "macos"))]
fn cached(_pid: u32, _name: &str, _recorded: bool) -> Option<Icon> {
    None
}

#[cfg(target_os = "macos")]
mod native {
    use objc2::encode::{Encoding, RefEncode};
    use objc2::rc::autoreleasepool;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSBundle, NSPoint, NSRect, NSSize, NSString, ns_string};
    use std::ffi::c_void;
    use std::path::Path;
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

    /// The outermost `.app` bundle the process `pid` runs from, as a
    /// path — `None` for a process that runs from none, one whose path
    /// the OS will not give, or, given `named`, one whose executable is
    /// not called that.
    pub(super) fn bundle_of(pid: u32, named: Option<&str>) -> Option<String> {
        let pid = i32::try_from(pid).ok()?;
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: the buffer is as long as the size passed, and the call
        // writes at most that many bytes into it.
        let written = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        let written = usize::try_from(written).ok().filter(|&n| n > 0)?;
        buf.truncate(written);
        let exe = String::from_utf8(buf).ok()?;
        if named.is_some_and(|name| !runs(&exe, name)) {
            return None;
        }
        bundle_in(&exe).map(str::to_string)
    }

    /// Whether the executable at `exe` is the one called `name`.
    fn runs(exe: &str, name: &str) -> bool {
        Path::new(exe).file_name().is_some_and(|file| file == name)
    }

    /// The outermost of [`app_dirs`] that is a bundle — has its
    /// `Contents/Info.plist`. A name is not enough: a directory may be
    /// called `*.app` without being one, as this repository's checkout
    /// is on CI (`/Users/runner/work/zstats.app/…`), and an app built
    /// inside it would otherwise be passed over for the checkout.
    fn bundle_in(exe: &str) -> Option<&str> {
        app_dirs(exe).find(|dir| Path::new(dir).join("Contents/Info.plist").is_file())
    }

    /// Every directory on `exe`'s path whose name ends in `.app`,
    /// outermost first: the app a helper's own nested bundle belongs to
    /// comes before that helper's.
    fn app_dirs(exe: &str) -> impl Iterator<Item = &str> {
        exe.match_indices(".app")
            .map(|(at, _)| at + 4)
            .filter(|&end| matches!(exe.as_bytes().get(end), Some(b'/')))
            .filter_map(|end| {
                let start = exe[..end].rfind('/')? + 1;
                // A component called just ".app" names no app.
                (end - start > 4).then(|| &exe[..end])
            })
    }

    /// The bundle's icon at [`ICON_PX`], as PNG bytes — `None` when the
    /// path is no bundle, or one that declares no icon of its own.
    pub(super) fn icon_png(bundle: &str) -> Option<Vec<u8>> {
        autoreleasepool(|_| {
            let path = NSString::from_str(bundle);
            let info = NSBundle::bundleWithPath(&path)?;
            let declared = [
                ns_string!("CFBundleIconFile"),
                ns_string!("CFBundleIconName"),
            ]
            .into_iter()
            .any(|key| info.objectForInfoDictionaryKey(key).is_some());
            if !declared {
                return None;
            }
            let workspace = NSWorkspace::sharedWorkspace();
            unsafe {
                let icon: *mut AnyObject = msg_send![&*workspace, iconForFile: &*path];
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
        use std::env;
        use std::fs;
        use std::process;

        fn dirs(exe: &str) -> Vec<&str> {
            app_dirs(exe).collect()
        }

        #[test]
        fn a_helper_lists_its_app_first() {
            assert_eq!(
                dirs(
                    "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome \
                     Framework.framework/Versions/141.0/Helpers/Google Chrome Helper \
                     (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer)"
                )[0],
                "/Applications/Google Chrome.app"
            );
            assert_eq!(
                dirs("/Applications/企业微信.app/Contents/MacOS/企业微信"),
                ["/Applications/企业微信.app"]
            );
            assert!(dirs("/usr/sbin/cupsd").is_empty());
            // `.app` only as a whole directory name's end.
            assert!(dirs("/opt/tool.application/bin/tool").is_empty());
            assert!(dirs("/Users/me/my.apps/run").is_empty());
            assert!(dirs("/Users/me/run.app").is_empty());
            assert_eq!(
                dirs("/tmp/.app/Tool.app/Contents/MacOS/Tool"),
                ["/tmp/.app/Tool.app"]
            );
        }

        /// A directory named `*.app` that is no bundle is passed over,
        /// for the app inside it — CI checks this repository out into
        /// one, and the test binary lives under it.
        #[test]
        fn a_folder_named_like_an_app_is_not_one() {
            let root = env::temp_dir().join(format!("zstats-appicon-{}", process::id()));
            let checkout = root.join("zstats.app");
            let bundle = checkout.join("target/Tool.app");
            fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
            fs::write(bundle.join("Contents/Info.plist"), "").unwrap();
            let tool = bundle.join("Contents/MacOS/Tool");
            let built = checkout.join("target/debug/zstats");
            let found = bundle_in(tool.to_str().unwrap()).map(str::to_string);
            let bare = bundle_in(built.to_str().unwrap()).map(str::to_string);
            fs::remove_dir_all(&root).unwrap();
            assert_eq!(found.as_deref(), bundle.to_str());
            assert_eq!(bare, None);
        }

        /// Every message on the way to a PNG, sent for real: Finder's
        /// bundle is on every Mac. A wrong type in one of them is a panic
        /// in a debug build, which is how the first version took the
        /// panel down the moment it drew an icon.
        #[test]
        fn an_app_gives_a_png_and_an_agent_or_a_bare_process_does_not() {
            let png = icon_png("/System/Library/CoreServices/Finder.app").expect("Finder");
            assert!(png.starts_with(b"\x89PNG"), "PNG signature");
            // A system agent whose bundle declares no icon keeps the
            // letter: the generic glyph is no icon.
            let agent = "/System/Library/CoreServices/MenuBarAgent.app";
            if Path::new(agent).exists() {
                assert!(icon_png(agent).is_none());
            }
            assert!(icon_png("/usr/sbin").is_none());
            // This test binary is a bare executable, not inside an `.app`.
            assert_eq!(bundle_of(process::id(), None), None);
        }

        /// The path is asked of another process, not only this one:
        /// Finder runs on any Mac with a session.
        #[test]
        fn a_running_app_is_found_by_its_pid() {
            let finder = NSWorkspace::sharedWorkspace()
                .runningApplications()
                .iter()
                .find(|app| {
                    app.bundleIdentifier()
                        .is_some_and(|id| id.to_string() == "com.apple.finder")
                })
                .map(|app| app.processIdentifier());
            if let Some(pid) = finder.and_then(|pid| u32::try_from(pid).ok()) {
                assert_eq!(
                    bundle_of(pid, None).as_deref(),
                    Some("/System/Library/CoreServices/Finder.app")
                );
                // As a record: the name must still be the one running.
                assert_eq!(
                    bundle_of(pid, Some("Finder")).as_deref(),
                    Some("/System/Library/CoreServices/Finder.app")
                );
                assert_eq!(bundle_of(pid, Some("Safari")), None, "a reused pid");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648's own test vectors: every padding case.
    #[test]
    fn base64_is_the_standard_one() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain:?}");
        }
    }
}
