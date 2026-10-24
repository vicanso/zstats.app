// Release builds detach from the console on Windows — otherwise launching the
// app pops an empty terminal behind the window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// The macOS host uses all of what the shared modules carry; the Linux one
// a part — no tray rect to hang the panel from (the compositor places
// it), no dragged size to remember. That geometry still compiles and is
// still tested there; it is linted for dead code on macOS, where it is
// called.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

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

// What the interface stands on: none of these draws.
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

// The interface (docs/nsh-migration.md: gpui-kit until 2026-10-25).
mod nsh;

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

fn main() {
    nsh::run();
}
