//! The interface, on NSH's native backends — AppKit on macOS, GTK 4 on
//! Linux — around the shared core (docs/nsh-migration.md).
//!
//! The hosts are per platform: on macOS the application, the status item
//! and the panel (`host.rs`) and the settings and disk-space windows
//! (`windows.rs`); on Linux all of that on GTK (`linux.rs`). The roots —
//! one per window — are shared: `panel.rs`, `settings.rs`, `storage.rs`,
//! with what they have in common in `root.rs`, all reading the shared
//! store (`store.rs`).
//!
//! **The gpui front end these modules replaced was deleted in 269a17b.**
//! Each page was ported from its gpui twin and checked against it side by
//! side, and the twin's comments carry most of the reasons the port kept.
//! A comment here that names one of its files — `views/x.rs` or
//! `views::x`, `gpui_app.rs`, `gpui_ui.rs`, `window_ext.rs`, `dock.rs`,
//! `theme.rs`, `font.rs`, `confirm.rs` — means that file as it stood
//! there: `git show 269a17b^:src/views/x.rs`.

mod alerts;
mod apps;
mod chart;
mod chrome;
mod gallery;
mod hardware;
mod history;
#[cfg(target_os = "macos")]
mod host;
#[cfg(target_os = "linux")]
mod linux;
mod lists;
mod network;
mod overview;
mod panel;
#[cfg(all(target_os = "macos", debug_assertions))]
mod probe;
mod processes;
mod root;
mod settings;
mod storage;
mod store;
mod theme;
mod widgets;
#[cfg(target_os = "macos")]
mod windows;

pub fn run() {
    #[cfg(target_os = "macos")]
    host::run();
    #[cfg(target_os = "linux")]
    linux::run();
}
