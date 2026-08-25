//! Desktop notifications for alert events.
//!
//! System banners cannot take our palette or layout — the OS owns that
//! chrome. What we can do is deliver them, and treat a click as "open
//! the panel on Alerts". Threshold edits stay in the Alerts tab.
//!
//! macOS delivery is our own thin `UNUserNotificationCenter` layer:
//! `addNotificationRequest:` is an async post that returns immediately,
//! and a resident delegate receives clicks on the main run loop, which
//! gpui is already pumping. Fire-and-forget is the point, not a shortcut.
//! An earlier transport (notify-rust's `wait_for_action` on one delivery
//! thread) returned only when the user *dealt with* a banner — one left
//! sitting in Notification Center parked the thread for as long as it sat
//! there, every later banner queued behind it, and the 17th was silently
//! dropped. Attention is not a resource to serialise on: with delivery
//! decoupled from it there is no queue to fill and nothing to stall.
//!
//! UN requires a real `.app` bundle: outside one (`cargo run`) it
//! throws, so banners are honestly absent there, said once in the log.
//! The predecessor, deprecated `NSUserNotification`, was kept for years
//! *because* it worked unbundled — until macOS 26, where it became a
//! silent no-op: `deliverNotification:` returned, nothing showed, and
//! the system never even created the app's notification-settings entry
//! (measured, with an osascript banner as the working control). An API
//! that pretends to deliver is worse than one that refuses.
//!
//! Non-macOS keeps the notify-rust transport (one delivery thread, bounded
//! queue) unchanged — XDG banners auto-expire, so the wait there is
//! bounded by the server, not the user. See "Platform reality" in
//! CLAUDE.md for how much trust to put in that path.

use crate::active;
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

/// Fire-and-forget delivery over `UNUserNotificationCenter`.
///
/// Bundled processes only — see the module doc for why the unbundled
/// fallback (`NSUserNotification`) no longer exists.
#[cfg(target_os = "macos")]
mod native {
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, ProtocolObject};
    use objc2::{AnyThread, define_class, msg_send};
    use objc2_foundation::{NSBundle, NSError, NSObjectProtocol, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
        UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
        UNNotificationSound, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    use std::mem;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Whether this process runs from a real `.app`. UN throws an
    /// Objective-C exception for a bare binary, so the answer gates every
    /// call into it. The main bundle of a bare `cargo run` is the
    /// executable's directory; only a real bundle's path ends in `.app`.
    fn bundled() -> bool {
        NSBundle::mainBundle()
            .bundlePath()
            .to_string()
            .ends_with(".app")
    }

    define_class!(
        /// Receives activations for every banner this process posted. One
        /// banner vocabulary, one action: any click opens the panel on
        /// Alerts, so nothing per-notification needs to be carried here.
        #[unsafe(super(NSObject))]
        #[name = "ZStatsNotifyDelegate"]
        struct NotifyDelegate;

        unsafe impl NSObjectProtocol for NotifyDelegate {}
        unsafe impl UNUserNotificationCenterDelegate for NotifyDelegate {}

        impl NotifyDelegate {
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                _response: &UNNotificationResponse,
                completion: &block2::Block<dyn Fn()>,
            ) {
                super::signal_click();
                completion.call(());
            }

            // Present even while this app is active. UN's default
            // suppresses banners from the frontmost app — reasonable for
            // a document app, but for a menu-bar accessory "frontmost"
            // means the panel is open, which is exactly when an alert
            // banner is being asked for context, not redundant.
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _note: &UNNotification,
                completion: &block2::Block<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                completion.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List
                    | UNNotificationPresentationOptions::Sound,));
            }
        }
    );

    /// Install the resident delegate and ask for authorization. Call once
    /// from `start`, before the first banner. The request puts up the
    /// system's own "allow notifications?" prompt the first time this app
    /// ever asks; every run after that it resolves silently from the
    /// user's recorded answer.
    pub(super) fn install() {
        if !bundled() {
            tracing::info!(
                "banners unavailable outside a bundle (cargo run): \
                 UNUserNotificationCenter needs a real .app"
            );
            return;
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let delegate: Retained<NotifyDelegate> = {
            let this = NotifyDelegate::alloc().set_ivars(());
            unsafe { msg_send![super(this), init] }
        };
        // `setDelegate:` stores an unretained pointer, so the referent
        // must outlive it — the `forget` below makes ours immortal.
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        mem::forget(delegate);

        // The denial is logged, not surfaced: the user answered the
        // system's own dialog, and nagging past that answer is exactly
        // what an alerting app must not do. The Alerts tab still records
        // every episode either way.
        let done = RcBlock::new(|granted: objc2::runtime::Bool, error: *mut NSError| {
            if !granted.as_bool() {
                // The description, not the pointer: "not allowed" for an
                // unsigned bundle and "declined" by the user are different
                // problems, and this line is the only witness.
                let reason = unsafe { error.as_ref() }
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_else(|| "declined by the user".into());
                tracing::warn!("notification authorization declined: {reason}");
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &done,
        );
    }

    /// Post one banner and return. Whether and when it shows is the
    /// notification centre's business; a banner the user never touches
    /// costs nothing here.
    pub(super) fn deliver(banner: &super::Banner) {
        if !bundled() {
            return; // said once, at install
        }
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&banner.title));
        content.setSubtitle(&NSString::from_str(&banner.subtitle));
        content.setBody(&NSString::from_str(&banner.body));
        if !banner.silent {
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }
        // Unique per banner: UN treats a repeated identifier as an update
        // to the existing notification, and every alert here is its own.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let id = format!("zstats-banner-{}", SEQ.fetch_add(1, Ordering::Relaxed));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&id),
            &content,
            None,
        );
        let done = RcBlock::new(|error: *mut NSError| {
            if !error.is_null() {
                tracing::warn!("banner rejected by notification center: {error:?}");
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, Some(&done));
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

/// Listen for banner clicks. Call once from `main`, on the UI thread,
/// before the first tick.
///
/// No identity claim any more: `UNUserNotificationCenter` attributes
/// banners to the process's real bundle, so the old
/// `notify_rust::set_application` swizzle (and the `BUNDLE_ID` constant
/// it had to keep in step with Cargo.toml) retired with the
/// `NSUserNotification` path.
pub fn start(cx: &mut gpui::App) {
    let (tx, rx) = smol::channel::unbounded();
    let _ = CLICK.set(tx);

    #[cfg(target_os = "macos")]
    native::install();

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
        body: format!(
            "{}{}",
            t!("alerts.sustained_body", pid = notice.pid),
            unused_clause(notice.pid)
        ),
        // Silent: a slow-burn finding, not something that needs attention
        // this second.
        silent: true,
    });
}

/// " Unused for 3h." — appended to a slow-burn banner when the subject
/// is an application nobody has switched to in a while (`active.rs`).
///
/// This is the sentence that separates a finding from a false alarm:
/// the same load in the editor you are typing in is work. Empty
/// whenever the answer is not known *and proven* — an app active
/// recently, a bare process AppKit never reports, or a session too
/// young to have seen the switch. A banner may say less; it may not
/// claim a stretch of time the reader cannot check.
fn unused_clause(pid: u32) -> String {
    active::unused_for_a_while(pid).map_or_else(String::new, |idle| {
        format!(" {}", t!("alerts.unused_for", span = format::span(idle)))
    })
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
        body: format!(
            "{}{}",
            t!("alerts.creep_body", now = format::memory(creep.now_bytes)),
            unused_clause(creep.root_pid)
        ),
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

    /// The notification identity now IS the bundle — UN attributes by the
    /// real .app, no claimed id to keep in step with Cargo.toml. What is
    /// left to guard is that the manifest still declares one at all: an
    /// identifier-less bundle would fail authorization at runtime with
    /// nothing pointing at the cause.
    #[test]
    fn the_manifest_still_declares_a_bundle_identifier() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest.contains("identifier = \"com.github.vicanso.zstats\""),
            "Cargo.toml lost its [package.metadata.bundle] identifier"
        );
    }
}
