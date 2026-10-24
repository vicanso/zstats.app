//! The NSH front end (docs/nsh-migration.md): NSH's native backends instead
//! of gpui — AppKit on macOS, GTK 4 on Linux — around the same shared core.
//!
//! Phase 0 is the skeleton: on macOS the application, the status item and
//! the panel window (`host.rs`), with a placeholder page in it
//! (`panel.rs`). The core reaches it in phase 1, the pages in phase 4.
//! The Linux host is phase 2; until then a Linux build with this front end
//! says so and exits.

#[cfg(target_os = "macos")]
mod host;
#[cfg(target_os = "macos")]
mod panel;
#[cfg(all(target_os = "macos", debug_assertions))]
mod probe;

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
