//! Desktop notifications for alert events, via [`notify_rust`].
//!
//! System banners cannot take our palette or layout — the OS owns that
//! chrome. What we can do is deliver them, and treat a click as "open
//! the panel on Alerts". Threshold edits stay in the Alerts tab.
//!
//! `show()` returns a handle; `wait_for_action` is what actually
//! delivers on macOS and then blocks until the user clicks or dismisses.
//! That wait lives on a helper thread so the gpui run loop stays free.
//! macOS `NSUserNotificationCenter` still delivers the click on the
//! main thread — gpui is already pumping that.

use crate::APP_NAME;
use crate::format;
use crate::i18n;
use notify_rust::Notification;
use rust_i18n::t;
use std::sync::OnceLock;
use std::time::Duration;
use zstats::{AlertDetail, AlertEvent, AlertKind, AlertSubject, Severity};

/// Click on a banner: open the panel on the Alerts tab.
static CLICK: OnceLock<smol::channel::Sender<()>> = OnceLock::new();

/// Must match `[package.metadata.bundle] identifier` in Cargo.toml — it is
/// what macOS attributes the notification to.
const BUNDLE_ID: &str = "com.github.vicanso.zstats";

/// Listen for banner clicks. Call once from `main`, on the UI thread,
/// before the first tick.
pub fn start(cx: &mut gpui::App) {
    let (tx, rx) = smol::channel::unbounded();
    let _ = CLICK.set(tx);

    // Claim our own bundle id up front, or `mac-notification-sys` will do it
    // for us — badly. Its `ensure_application_set()` runs before every
    // delivery and, when nothing has been set, calls
    // `get_bundle_identifier_or_default("use_default")`, which is an
    // AppleScript lookup *by application name*. No app is called
    // "use_default", so macOS puts up a "which application?" picker and the
    // library then falls back to `com.apple.Finder`.
    //
    // The literal avoids AppleScript entirely; it has to stay in step with
    // `[package.metadata.bundle] identifier` in Cargo.toml. Outside a bundle
    // (`cargo run`) there is no matching .app, so banners may not appear —
    // but nothing prompts, which is the point.
    if let Err(e) = notify_rust::set_application(BUNDLE_ID) {
        eprintln!("could not claim notification identity: {e}");
    }

    cx.spawn(async move |cx| {
        while rx.recv().await.is_ok() {
            // `update` is `()` in this gpui pin — a dropped app just
            // stops polling this task.
            cx.update(crate::show_alerts_window);
        }
    })
    .detach();
}

/// Show one system notification for a freshly fired alert.
pub fn post(event: &AlertEvent) {
    let title = subject_title(&event.subject);
    let subtitle = notify_subtitle(event);
    let body = notify_body(event);

    std::thread::spawn(move || {
        let mut n = Notification::new();
        n.appname(APP_NAME)
            .summary(&title)
            .subtitle(&subtitle)
            .body(&body)
            .sound_name("default");

        match n.show() {
            Ok(handle) => handle.wait_for_action(|action| {
                // Banner click is `"default"`. Dismiss is `"__closed"`.
                if action != "__closed" {
                    signal_click();
                }
            }),
            Err(e) => eprintln!("notification failed: {e}"),
        }
    });
}

/// Announce a process that has been holding a low-but-real CPU share for a
/// long time.
///
/// Separate from [`post`] because it is not a `zstats` alert and never will
/// be: alerting asks whether something is over the line right now, and this
/// is by definition always under it. Without a banner the finding would only
/// exist inside a panel nobody has a reason to open.
pub fn post_sustained(notice: &crate::state::SustainedNotice) {
    let title = notice.name.clone();
    let subtitle = t!(
        "alerts.sustained_subtitle",
        cpu = format::pct(notice.cpu_avg as f32),
        duration = format::uptime(notice.duration.as_secs())
    )
    .to_string();
    let body = t!("alerts.sustained_body", pid = notice.pid).to_string();

    std::thread::spawn(move || {
        let mut n = Notification::new();
        n.appname(APP_NAME)
            .summary(&title)
            .subtitle(&subtitle)
            .body(&body)
            // No sound: this is a slow-burn finding, not something that
            // needs attention this second.
            .sound_name("");

        match n.show() {
            Ok(handle) => handle.wait_for_action(|action| {
                if action != "__closed" {
                    signal_click();
                }
            }),
            Err(e) => eprintln!("notification failed: {e}"),
        }
    });
}

fn signal_click() {
    if let Some(tx) = CLICK.get() {
        let _ = tx.try_send(());
    }
}

fn subject_title(subject: &AlertSubject) -> String {
    match subject {
        AlertSubject::Process { name, .. } | AlertSubject::App { name, .. } => name.clone(),
        AlertSubject::Volume { mount_point } => {
            t!("alerts.volume", mount = mount_point.clone()).to_string()
        }
        AlertSubject::System => i18n::tr("alerts.system"),
    }
}

fn notify_subtitle(event: &AlertEvent) -> String {
    let sev = if event.severity() == Severity::Critical {
        i18n::tr("alerts.critical")
    } else {
        i18n::tr("alerts.warning")
    };
    let kind = match event.kind() {
        AlertKind::Cpu | AlertKind::AppCpu => i18n::tr("alerts.kind_cpu"),
        AlertKind::Memory | AlertKind::AppMemory => i18n::tr("alerts.kind_mem"),
        AlertKind::Disk => i18n::tr("alerts.kind_disk"),
        AlertKind::Pressure => i18n::tr("alerts.kind_pressure"),
    };
    format!("{sev} · {kind}")
}

fn notify_body(event: &AlertEvent) -> String {
    let mut text = match &event.detail {
        AlertDetail::Cpu {
            avg_percent,
            threshold_percent,
            window,
            runaway,
        } => {
            let avg = format!("{avg_percent:.0}");
            let threshold = format!("{threshold_percent:.0}");
            let window = window_mins(*window);
            if *runaway {
                t!(
                    "alerts.notify_cpu_runaway",
                    avg = avg,
                    threshold = threshold,
                    window = window
                )
                .to_string()
            } else {
                t!(
                    "alerts.notify_cpu",
                    avg = avg,
                    threshold = threshold,
                    window = window
                )
                .to_string()
            }
        }
        AlertDetail::Memory {
            share_percent,
            threshold_percent,
            window,
            ..
        } => t!(
            "alerts.notify_mem",
            share = format!("{share_percent:.0}"),
            threshold = format!("{threshold_percent:.0}"),
            window = window_mins(*window)
        )
        .to_string(),
        AlertDetail::Disk {
            used_percent,
            available_bytes,
            ..
        } => t!(
            "alerts.notify_disk",
            used = format!("{used_percent:.0}"),
            free = format::gb(*available_bytes)
        )
        .to_string(),
        AlertDetail::Pressure {
            level, sustained, ..
        } => {
            let level = if *level >= 4 {
                i18n::tr("alerts.critical")
            } else {
                i18n::tr("alerts.warning")
            };
            t!(
                "alerts.notify_pressure",
                level = level,
                minutes = sustained.as_secs() / 60
            )
            .to_string()
        }
    };
    if event.repeat_after.is_some() {
        text.push(' ');
        text.push_str(&i18n::tr("alerts.notify_repeat"));
    }
    text
}

fn window_mins(window: Duration) -> String {
    let n = (window.as_secs() / 60).max(1);
    t!("alerts.notify_window", n = n).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_id_matches_the_manifest() {
        // These two have to agree: macOS attributes the banner to whatever
        // identifier we claim, and a mismatch fails silently at runtime —
        // notifications simply show up under the wrong application.
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest.contains(&format!("identifier = \"{BUNDLE_ID}\"")),
            "BUNDLE_ID ({BUNDLE_ID}) is not the identifier in Cargo.toml"
        );
    }
}
