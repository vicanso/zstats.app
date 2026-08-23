//! Desktop notifications for alert events.
//!
//! System banners cannot take our palette or layout — the OS owns that
//! chrome. What we can do is deliver them, and treat a click as "open
//! the panel on Alerts". Threshold edits stay in the Alerts tab.
//!
//! macOS delivery is our own thin `NSUserNotificationCenter` layer:
//! `deliverNotification:` is an async XPC post that returns immediately,
//! and a resident delegate receives clicks on the main run loop, which
//! gpui is already pumping. Fire-and-forget is the point, not a shortcut.
//! The previous transport (notify-rust's `wait_for_action` on one delivery
//! thread) returned only when the user *dealt with* a banner — one left
//! sitting in Notification Center parked the thread for as long as it sat
//! there, every later banner queued behind it, and the 17th was silently
//! dropped. Attention is not a resource to serialise on: with delivery
//! decoupled from it there is no queue to fill and nothing to stall.
//!
//! Non-macOS keeps the notify-rust transport (one delivery thread, bounded
//! queue) unchanged — XDG banners auto-expire, so the wait there is
//! bounded by the server, not the user. See "Platform reality" in
//! CLAUDE.md for how much trust to put in that path.

use crate::format;
use crate::i18n;
use crate::state;
use rust_i18n::t;
use std::sync::OnceLock;
use std::time::Duration;
use zstats::{AlertDetail, AlertEvent, AlertKind, AlertSubject, Severity};

#[cfg(not(target_os = "macos"))]
use crate::APP_NAME;
#[cfg(not(target_os = "macos"))]
use notify_rust::Notification;
#[cfg(not(target_os = "macos"))]
use std::sync::mpsc;
#[cfg(not(target_os = "macos"))]
use std::thread;

/// Click on a banner: open the panel on the Alerts tab.
static CLICK: OnceLock<smol::channel::Sender<()>> = OnceLock::new();

/// One banner to deliver. Owned data — on non-macOS it crosses to the
/// delivery thread.
struct Banner {
    title: String,
    subtitle: String,
    body: String,
    /// Slow-burn findings arrive silently; threshold crossings make a sound.
    silent: bool,
}

/// Hand one banner to the platform transport.
fn dispatch(banner: Banner) {
    #[cfg(target_os = "macos")]
    native::deliver(&banner);
    #[cfg(not(target_os = "macos"))]
    enqueue(banner);
}

/// Fire-and-forget delivery over `NSUserNotificationCenter`.
///
/// The API has been deprecated since 10.14, but it is the only banner API a
/// bare `cargo run` binary can use at all — the replacement
/// `UNUserNotificationCenter` throws unless the process runs from a real
/// bundle, and half this app's life is spent unbundled under a debugger.
/// Hence the module-wide `allow`.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
mod native {
    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, ProtocolObject};
    use objc2::{AnyThread, define_class, msg_send};
    use objc2_foundation::{
        NSObjectProtocol, NSString, NSUserNotification, NSUserNotificationCenter,
        NSUserNotificationCenterDelegate, NSUserNotificationDefaultSoundName,
    };
    use std::mem;

    define_class!(
        /// Receives activations for every banner this process posted. One
        /// banner vocabulary, one action: any click opens the panel on
        /// Alerts, so nothing per-notification needs to be carried here.
        #[unsafe(super(NSObject))]
        #[name = "ZStatsNotifyDelegate"]
        struct NotifyDelegate;

        unsafe impl NSObjectProtocol for NotifyDelegate {}
        unsafe impl NSUserNotificationCenterDelegate for NotifyDelegate {}

        impl NotifyDelegate {
            #[unsafe(method(userNotificationCenter:didActivateNotification:))]
            fn did_activate(
                &self,
                _center: &NSUserNotificationCenter,
                _note: &NSUserNotification,
            ) {
                super::signal_click();
            }

            // Present even while this app is active. The default suppresses
            // banners from the frontmost app — reasonable for a document
            // app, but for a menu-bar accessory "frontmost" means the panel
            // is open, which is exactly when an alert banner is being asked
            // for context, not redundant.
            #[unsafe(method(userNotificationCenter:shouldPresentNotification:))]
            fn should_present(
                &self,
                _center: &NSUserNotificationCenter,
                _note: &NSUserNotification,
            ) -> bool {
                true
            }
        }
    );

    /// Install the resident delegate. Call once from `start`, before the
    /// first banner; a banner posted earlier still shows, its click is just
    /// nobody's to hear.
    pub(super) fn install() {
        let delegate: Retained<NotifyDelegate> = {
            let this = NotifyDelegate::alloc().set_ivars(());
            unsafe { msg_send![super(this), init] }
        };
        let center = NSUserNotificationCenter::defaultUserNotificationCenter();
        // SAFETY: `setDelegate:` stores an unretained pointer, so the
        // referent must outlive it — the `forget` below makes ours immortal.
        unsafe { center.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
        mem::forget(delegate);
    }

    /// Post one banner and return. Whether and when it shows is the
    /// notification centre's business; a banner the user never touches
    /// costs nothing here.
    pub(super) fn deliver(banner: &super::Banner) {
        let note = NSUserNotification::new();
        note.setTitle(Some(&NSString::from_str(&banner.title)));
        note.setSubtitle(Some(&NSString::from_str(&banner.subtitle)));
        note.setInformativeText(Some(&NSString::from_str(&banner.body)));
        if !banner.silent {
            // The exported constant, not a string that names it — a
            // `soundName` that matches no installed sound plays nothing.
            // SAFETY: reading Foundation's exported constant.
            note.setSoundName(Some(unsafe { NSUserNotificationDefaultSoundName }));
        }
        NSUserNotificationCenter::defaultUserNotificationCenter().deliverNotification(&note);
    }
}

/// Banners waiting to be shown, drained by the single delivery thread.
#[cfg(not(target_os = "macos"))]
static QUEUE: OnceLock<mpsc::SyncSender<Banner>> = OnceLock::new();

/// How many banners may wait before new ones are dropped.
///
/// Deep enough that a burst never loses anything in practice — zstats already
/// rate-limits per subject through `alert-cooldown` — and bounded because
/// `wait_for_action` holds the thread until the notification server resolves
/// the banner. Dropping is safe; the Alerts tab still has the event.
#[cfg(not(target_os = "macos"))]
const QUEUE_DEPTH: usize = 16;

/// Hand a banner to the delivery thread, or drop it if that thread is still
/// parked on one the server has not resolved.
#[cfg(not(target_os = "macos"))]
fn enqueue(banner: Banner) {
    let Some(tx) = QUEUE.get() else {
        return; // start() never ran — no window, no notifications
    };
    if let Err(mpsc::TrySendError::Full(dropped)) = tx.try_send(banner) {
        tracing::warn!("notification queue full, dropping: {}", dropped.title);
    }
}

/// Show one banner and block until the server resolves it. Serial on one
/// long-lived thread — the thread-per-banner alternative left a thread per
/// unattended banner during alert storms.
#[cfg(not(target_os = "macos"))]
fn deliver(banner: &Banner) {
    let mut n = Notification::new();
    n.appname(APP_NAME)
        .summary(&banner.title)
        .subtitle(&banner.subtitle)
        .body(&banner.body)
        .sound_name(if banner.silent { "" } else { "default" });

    match n.show() {
        Ok(handle) => handle.wait_for_action(|action| {
            // Banner click is `"default"`. Dismiss is `"__closed"`.
            if action != "__closed" {
                signal_click();
            }
        }),
        Err(e) => tracing::warn!("notification failed: {e}"),
    }
}

/// Must match `[package.metadata.bundle] identifier` in Cargo.toml — it is
/// what macOS attributes the notification to.
const BUNDLE_ID: &str = "com.github.vicanso.zstats";

/// Listen for banner clicks. Call once from `main`, on the UI thread,
/// before the first tick.
pub fn start(cx: &mut gpui::App) {
    let (tx, rx) = smol::channel::unbounded();
    let _ = CLICK.set(tx);

    #[cfg(target_os = "macos")]
    {
        // Claim our own bundle id before the first delivery. This is what
        // stamps our identity on the banner: `set_application` swizzles
        // `NSBundle.bundleIdentifier` process-wide at call time, so it
        // covers our own delivery path too. The literal has to stay in step
        // with `[package.metadata.bundle] identifier` in Cargo.toml (a test
        // guards that). Outside a bundle (`cargo run`) the id still resolves
        // to the *installed* .app, which is why banners work under a
        // debugger at all — and why they quietly don't when no zstats.app
        // is installed.
        if let Err(e) = notify_rust::set_application(BUNDLE_ID) {
            tracing::warn!("could not claim notification identity: {e}");
        }
        native::install();
    }

    // One delivery thread for the life of the process. It only ever blocks —
    // on the queue, or on a banner the server has not resolved — so it costs
    // nothing while idle.
    #[cfg(not(target_os = "macos"))]
    {
        let (banner_tx, banner_rx) = mpsc::sync_channel::<Banner>(QUEUE_DEPTH);
        let _ = QUEUE.set(banner_tx);
        thread::spawn(move || {
            while let Ok(banner) = banner_rx.recv() {
                deliver(&banner);
            }
        });
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
    dispatch(Banner {
        title: subject_title(&event.subject),
        subtitle: notify_subtitle(event),
        body: notify_body(event),
        silent: false,
    });
}

/// Announce a process that has been holding a low-but-real CPU share for a
/// long time.
///
/// Separate from [`post`] because it is not a `zstats` alert and never will
/// be: alerting asks whether something is over the line right now, and this
/// is by definition always under it. Without a banner the finding would only
/// exist inside a panel nobody has a reason to open.
pub fn post_sustained(notice: &state::SustainedNotice) {
    dispatch(Banner {
        title: notice.name.clone(),
        subtitle: t!(
            "alerts.sustained_subtitle",
            cpu = format::pct(notice.cpu_avg as f32),
            duration = format::uptime(notice.duration.as_secs())
        )
        .to_string(),
        body: t!("alerts.sustained_body", pid = notice.pid).to_string(),
        // Silent: a slow-burn finding, not something that needs attention
        // this second.
        silent: true,
    });
}

/// Announce a tree whose memory footprint has climbed a gigabyte within
/// the hour and is still holding it — the leak shape.
///
/// Same class as [`post_sustained`], same reasons: not a `zstats` alert
/// and never will be, because a climb crosses no line until it is too
/// late. Silent, and one per climb (`state::take_memory_creep_notices`
/// re-arms only once the climb is gone).
pub fn post_memory_creep(creep: &state::MemoryCreep) {
    dispatch(Banner {
        title: creep.name.clone(),
        subtitle: t!(
            "alerts.creep_subtitle",
            delta = format::memory(creep.climb_bytes)
        )
        .to_string(),
        body: t!("alerts.creep_body", now = format::memory(creep.now_bytes)).to_string(),
        silent: true,
    });
}

fn signal_click() {
    if let Some(tx) = CLICK.get() {
        let _ = tx.try_send(());
    }
}

fn subject_title(subject: &AlertSubject) -> String {
    match subject {
        // The banner is pure presentation — the display name where the
        // executable's own says nothing (zstats 0.5.3).
        AlertSubject::Process {
            name, display_name, ..
        }
        | AlertSubject::App {
            name, display_name, ..
        } => display_name.clone().unwrap_or_else(|| name.clone()),
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
