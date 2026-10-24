//! The NSH front end (docs/nsh-migration.md): NSH's native backends instead
//! of gpui — AppKit on macOS, GTK 4 on Linux — around the same shared core.
//!
//! The hosts are per platform: on macOS the application, the status item
//! and the panel (`host.rs`) and the settings and disk-space windows
//! (`windows.rs`); on Linux all of that on GTK (`linux.rs`). The roots —
//! one per window, each a placeholder until phase 4 brings the pages —
//! are shared: `panel.rs`, `settings.rs`, `storage.rs`, with what they
//! have in common in `root.rs`, all reading the shared store
//! (`store.rs`).

mod alerts;
mod apps;
mod chart;
mod chrome;
mod gallery;
mod hardware;
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
