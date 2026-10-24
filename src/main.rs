// Release builds detach from the console on Windows — otherwise launching the
// app pops an empty terminal behind the window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// While the interface moves from gpui to NSH (docs/nsh-migration.md), a
// shared module carries what either front end calls, and a build with one
// of them compiles all of it: the NSH-only build has the store's
// re-exports only gpui's views import, the gpui-only build the palette's
// halves only NSH reads. Dead code is reported where both are built —
// the configuration `make lint` checks, where nothing shared is unused
// unless it really is.
#![cfg_attr(
    not(all(feature = "gpui", feature = "nsh")),
    allow(dead_code, unused_imports)
)]

// macOS and Linux. Anything else fails here rather than six files later,
// with the same reasoning the macOS-only guard carried before: a scatter
// of unresolved imports says nothing about why. The Linux side is a port
// in progress — see docs/omarchy-port.md for what each phase covers and
// what is deliberately absent — and every gate below is either a real
// implementation or an honest empty state, never scaffolding.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!(
    "zstats-app builds on macOS and Linux. See docs/omarchy-port.md — porting \
     to a third platform means implementing the tray, window show/hide, \
     positioning and process scanning, not just satisfying the compiler."
);

#[cfg(not(any(feature = "gpui", feature = "nsh")))]
compile_error!(
    "zstats-app needs a front end: the `gpui` feature (the default) or `nsh` \
     (docs/nsh-migration.md)."
);

// Shared by both front ends: none of these knows which one is drawing.
mod about;
mod active;
mod alertbars;
mod alertlist;
mod alertlog;
mod alerttpl;
mod appicon;
#[cfg(target_os = "macos")]
mod appkit;
mod applist;
mod assetinfo;
mod assets;
mod autostart;
mod awake;
mod cachepreset;
mod chart;
mod cleanhints;
mod color;
mod dirmap;
mod diskscan;
mod diskwatch;
mod dupes;
mod finder;
mod format;
mod fullscan;
mod geom;
mod glyph;
mod hardware;
mod history;
mod historylist;
mod hot;
mod i18n;
mod i18n_loader;
#[cfg(target_os = "linux")]
mod ipc;
mod listening;
mod logger;
mod metrics;
mod net;
mod notice;
mod notify;
mod omarchy;
mod opener;
mod palette;
mod placement;
mod prefs;
mod pressure;
mod proclist;
mod procscan;
mod proxy;
mod rows;
mod series;
mod settingspage;
mod spaceinfo;
mod squarify;
mod state;
mod storagepage;
mod terminate;
mod traffic;
mod tray;
mod trend;
mod updater;
mod volflag;
mod watch;
mod webhook;

// The gpui front end. Phase 1 of docs/nsh-migration.md moves what is not
// about drawing — the store, the tray's face, the colours, the placement
// geometry — out of these and into the shared half.
#[cfg(feature = "gpui")]
mod confirm;
#[cfg(all(feature = "gpui", target_os = "macos"))]
mod dock;
#[cfg(feature = "gpui")]
mod font;
#[cfg(feature = "gpui")]
mod gpui_app;
#[cfg(feature = "gpui")]
mod gpui_ui;
#[cfg(feature = "gpui")]
mod theme;
#[cfg(feature = "gpui")]
mod views;
#[cfg(all(feature = "gpui", target_os = "macos"))]
mod window_ext;

// The NSH front end (docs/nsh-migration.md).
#[cfg(feature = "nsh")]
mod nsh;

#[cfg(all(feature = "gpui", feature = "nsh"))]
use std::env;

// Pointed at the empty `locales_stub/` so the macro embeds no translations.
// Real files live in `assets/locales/` and inflate via `i18n_loader`.
rust_i18n::i18n!(
    "locales_stub",
    fallback = "en",
    backend = crate::i18n_loader::runtime_backend()
);

/// Shown in the app menu, the tray tooltip, the task switcher and the Linux
/// title bar.
pub const APP_NAME: &str = "zstats";

// What other modules call on the gpui front end, at the paths they used
// while it was this file.
#[cfg(feature = "gpui")]
pub(crate) use gpui_app::{
    open_settings_window, open_storage_window, reset_panel_size_pref, set_autostart_pref,
    set_disk_watch_pref, set_keep_awake, set_language_pref, set_notifications_pref,
    set_opacity_pref, set_overview_detail_pref, set_sustained_after_pref,
    set_sustained_divisor_pref, set_theme_pref, set_tray_pref, sync_panel_notice,
};

fn main() {
    // Both front ends built in: gpui unless asked otherwise, so a build
    // with everything on behaves as the release does.
    #[cfg(all(feature = "gpui", feature = "nsh"))]
    if env::var_os("ZSTATS_UI").is_some_and(|ui| ui == "nsh") {
        nsh::run();
        return;
    }
    #[cfg(feature = "gpui")]
    gpui_app::run();
    #[cfg(all(feature = "nsh", not(feature = "gpui")))]
    nsh::run();
}
