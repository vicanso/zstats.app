//! The NSH front end (docs/nsh-migration.md): NSH's native backends instead
//! of gpui — AppKit on macOS, GTK 4 on Linux — around the same shared core.
//!
//! On macOS the application, the status item and the panel window
//! (`host.rs`), the settings and disk-space windows (`windows.rs`), and a
//! placeholder root in each (`panel.rs`, `settings.rs`, `storage.rs`,
//! with what they share in `root.rs`) that reads the shared store
//! (`store.rs`) — the pages replace them in phase 4.
//! The Linux host is phase 2d; until then a Linux build with this front
//! end says so and exits.

#[cfg(target_os = "macos")]
mod host;
#[cfg(target_os = "macos")]
mod panel;
#[cfg(all(target_os = "macos", debug_assertions))]
mod probe;
#[cfg(target_os = "macos")]
mod root;
#[cfg(target_os = "macos")]
mod settings;
#[cfg(target_os = "macos")]
mod storage;
#[cfg(target_os = "macos")]
mod windows;
// Not the host's: the Linux host (phase 2d) shares it.
mod store;

pub fn run() {
    #[cfg(target_os = "macos")]
    host::run();
    #[cfg(target_os = "linux")]
    {
        // A person at a terminal asked for this front end; the log is not
        // where they will look (the same reasoning `ipc.rs` gives for
        // printing its answers).
        println!(
            "zstats: the NSH front end has no Linux host yet (phase 2 of \
             docs/nsh-migration.md); build with the default `gpui` feature"
        );
    }
}
