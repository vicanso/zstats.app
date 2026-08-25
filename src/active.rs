//! Who the user is actually working in, and how long ago.
//!
//! The observers in `watch.rs` and `trend.rs` can say a tree is burning
//! CPU or growing by the gigabyte; none of them can say whether the
//! person is *using* it. That distinction is the difference between a
//! finding and a false alarm: a compile in the editor you are typing in
//! is work, and the same load in an app untouched since morning is
//! news. The kernel has no opinion on this — "frontmost" is an AppKit
//! concept — so the panel asks AppKit, and the answer rides on the
//! banners the slow-burn watchers already post.
//!
//! **Event-driven, and deliberately so.** `NSWorkspace` posts
//! `didActivateApplication` on every app switch; the handler is one
//! hash-map write at human frequency (a few times a minute at most),
//! there is no polling anywhere, and nothing here is asked outside the
//! moment a banner is being composed. A frontmost *poll* was the
//! alternative and it is strictly worse: a sample every tick would
//! still miss switches between ticks, and would burn work per tick
//! forever to answer a question asked twice an hour.
//!
//! Activation is an **application**-level event, which shapes both
//! answers this module gives. Switching between two windows of one app
//! does not fire it, so the table alone would report an app used
//! continuously for hours — never switched away from, never switched
//! back to — as untouched; the app in front is therefore asked
//! directly at question time and always reads as in use. And an app
//! that can never become active (a background agent, a bare process)
//! never enters the table at all, which is exactly why it never earns
//! the line.
//!
//! Public API, no entitlement, no TCC prompt — the notification's
//! `userInfo` carries the `NSRunningApplication` that just came
//! forward, and pids are all this module ever reads off it. (Window
//! *titles* would need screen recording; window ownership and
//! activation do not.)
//!
//! What this cannot answer, and does not pretend to: App Nap and
//! RunningBoard's own foreground/background roles are private
//! (`task_policy_get` on another process needs its task port). "Not
//! active" here means "not the frontmost application", which is the
//! question the user actually asks about an app they have not touched.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

/// Last time each pid was the frontmost application.
///
/// Two things bound it, and the second is the load-bearing one.
/// [`MAX_TRACKED`] caps the size; **termination clears the entry**
/// (`forget`), because a pid outliving its process is not merely stale
/// — macOS reissues low pids, and an inherited stamp would have a
/// banner claim hours of disuse for a process minutes old. Session
/// scope by design: this is what *this run* watched happen, and
/// nothing is written to disk.
static LAST_ACTIVE: OnceLock<Mutex<HashMap<u32, Instant>>> = OnceLock::new();

/// Enough for every app a person switches between in a session; the
/// oldest entries are dropped past it. A day of activations on a busy
/// machine is dozens, not hundreds, and the map is only read for trees
/// that are already producing a banner.
const MAX_TRACKED: usize = 256;

/// How long an app must have gone untouched before a banner says so.
/// Under this the phrase would fire for the app the reader just
/// switched away from to look at the banner — technically true, and
/// noise. An hour is also the window the slow-burn watchers measure
/// over, so the two halves of the sentence describe the same stretch
/// of time.
pub const UNUSED_AFTER: Duration = Duration::from_secs(60 * 60);

fn table() -> &'static Mutex<HashMap<u32, Instant>> {
    LAST_ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record that `pid` came forward. Called from the workspace observer,
/// and directly by tests.
fn note_active(pid: u32, at: Instant) {
    if let Ok(mut map) = table().lock() {
        insert_bounded(&mut map, pid, at);
    }
}

/// The bounded insert, kept free of the global so its own test needs
/// no shared state — the tests run in one process, and one that reset
/// the real table would pull the ground out from the others.
fn insert_bounded(map: &mut HashMap<u32, Instant>, pid: u32, at: Instant) {
    if map.len() >= MAX_TRACKED && !map.contains_key(&pid) {
        // Drop the least recently active rather than clearing: the
        // entries worth keeping are exactly the recent ones.
        if let Some(&stalest) = map
            .iter()
            .min_by_key(|(_, when)| **when)
            .map(|(pid, _)| pid)
        {
            map.remove(&stalest);
        }
    }
    map.insert(pid, at);
}

/// How long since `pid` was last the frontmost application, or `None`
/// if this session never saw it come forward.
///
/// `None` is not "never used" — it is "not since this app started",
/// which is why every caller treats it as *no answer* rather than as a
/// long idle time. A panel launched ten minutes ago knows nothing about
/// the morning, and a banner claiming "unused for 8h" on that evidence
/// would be a lie the reader cannot check.
pub fn idle_for(pid: u32) -> Option<Duration> {
    let map = table().lock().ok()?;
    map.get(&pid).map(Instant::elapsed)
}

/// How long the user has gone without touching `pid`'s application,
/// once that is past [`UNUSED_AFTER`] — `None` whenever the answer is
/// unknown or shorter, so an unproven claim is never made.
///
/// The activation notification only fires on an app *switch*, which
/// makes the table alone wrong in the one case this feature exists to
/// avoid: an app used continuously for hours without switching away
/// still carries the timestamp from when it was switched *to*, and
/// would be announced as untouched while the reader types in it. So
/// the app in front right now is asked directly (one call, at most
/// twice an hour) and always reads as in use.
pub fn unused_for_a_while(pid: u32) -> Option<Duration> {
    if frontmost_pid() == Some(pid) {
        return None;
    }
    idle_for(pid).filter(|idle| *idle >= UNUSED_AFTER)
}

/// The frontmost application's pid, or `None` off macOS and whenever
/// AppKit has no active application to name.
fn frontmost_pid() -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        native::frontmost_pid()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Forget an application that has quit. Without this a pid's entry
/// outlives the process, and macOS hands low pids straight back out —
/// a reused pid would inherit a stale "last active" stamp and the
/// banner would claim hours of disuse for a process minutes old. Same
/// reasoning as the alert cards' `SeenAlert::live` gate: a pid is only
/// an identity while its process is alive.
fn forget(pid: u32) {
    if let Ok(mut map) = table().lock() {
        map.remove(&pid);
    }
}

/// Subscribe to workspace activations and seed the table with whatever
/// is frontmost right now. Call once at startup, on the main thread.
#[cfg(target_os = "macos")]
pub fn start() {
    native::install();
}

#[cfg(not(target_os = "macos"))]
pub fn start() {}

#[cfg(target_os = "macos")]
mod native {
    use block2::RcBlock;
    use objc2_app_kit::{
        NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey,
        NSWorkspaceDidActivateApplicationNotification,
        NSWorkspaceDidTerminateApplicationNotification,
    };
    use objc2_foundation::NSNotification;
    use std::mem;
    use std::time::Instant;

    /// Who is in front right now. Asked only while a banner is being
    /// composed — the activation table answers everything else.
    pub(super) fn frontmost_pid() -> Option<u32> {
        let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        u32::try_from(app.processIdentifier()).ok()
    }

    /// The activated app rides in `userInfo` under
    /// `NSWorkspaceApplicationKey` — *not* as the notification's
    /// `object`, which is the shared `NSWorkspace` itself (measured: a
    /// standalone observer printed `object=<NSWorkspace: 0x…>`). Its
    /// pid is the whole payload; no window inspection, no bundle
    /// lookup, nothing that would need a permission.
    fn pid_of(note: &NSNotification) -> Option<u32> {
        let info = note.userInfo()?;
        let app = info.objectForKey(unsafe { NSWorkspaceApplicationKey })?;
        let running = app.downcast_ref::<NSRunningApplication>()?;
        u32::try_from(running.processIdentifier()).ok()
    }

    pub(super) fn install() {
        let workspace = NSWorkspace::sharedWorkspace();

        // Seed: the app in front at launch has been in front since
        // before this observer existed, and without this it would look
        // unknown until the user switched away and back.
        if let Some(app) = workspace.frontmostApplication()
            && let Ok(pid) = u32::try_from(app.processIdentifier())
        {
            super::note_active(pid, Instant::now());
        }

        let center = workspace.notificationCenter();
        let on_activate = RcBlock::new(|note: std::ptr::NonNull<NSNotification>| {
            let note = unsafe { note.as_ref() };
            if let Some(pid) = pid_of(note) {
                super::note_active(pid, Instant::now());
            }
        });
        // Termination is the table's own expiry: entries are dropped
        // when the application they name goes away, so a recycled pid
        // starts unknown rather than inheriting a stranger's history.
        let on_terminate = RcBlock::new(|note: std::ptr::NonNull<NSNotification>| {
            let note = unsafe { note.as_ref() };
            if let Some(pid) = pid_of(note) {
                super::forget(pid);
            }
        });
        // `None` queue means "post on the thread that raised it" — the
        // main thread for workspace notifications, which is where gpui
        // already is.
        let tokens = unsafe {
            (
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWorkspaceDidActivateApplicationNotification),
                    None,
                    None,
                    &on_activate,
                ),
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWorkspaceDidTerminateApplicationNotification),
                    None,
                    None,
                    &on_terminate,
                ),
            )
        };
        // The observers live as long as the process; leaking the tokens
        // is what keeps them registered (the same posture as the
        // notification delegate in notify.rs). Nothing ever
        // unsubscribes — an app that stopped tracking activations
        // mid-run would silently start answering "unknown".
        mem::forget(tokens);
        mem::forget(on_activate);
        mem::forget(on_terminate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pid table answers in elapsed time, and only for what it
    /// actually saw: an unknown pid is no answer at all, never a long
    /// idle. A banner is allowed to say nothing; it is not allowed to
    /// invent a stretch of time the reader cannot check.
    #[test]
    fn an_unseen_pid_has_no_answer() {
        assert!(idle_for(4_294_967_290).is_none());
        assert!(unused_for_a_while(4_294_967_290).is_none());
    }

    #[test]
    fn a_just_activated_app_is_not_idle() {
        note_active(424_242, Instant::now());
        let idle = idle_for(424_242).expect("seen");
        assert!(idle < Duration::from_secs(5));
        assert!(
            unused_for_a_while(424_242).is_none(),
            "the app in front is not the one to name"
        );
    }

    /// The whole point of the gate: only a stretch past the bar earns
    /// the line. Guarded like the creep test — `Instant` cannot reach
    /// past boot, so a machine up less than the bar skips it.
    #[test]
    fn an_app_untouched_past_the_bar_reports_its_idle_time() {
        let Some(then) = Instant::now().checked_sub(UNUSED_AFTER + Duration::from_secs(60)) else {
            return;
        };
        note_active(424_243, then);
        let idle = unused_for_a_while(424_243).expect("past the bar");
        assert!(idle >= UNUSED_AFTER);
    }

    /// A quit application's entry goes, so the next process to be
    /// handed that pid starts unknown. Without this the reused pid
    /// would inherit a stale stamp and the banner would claim hours of
    /// disuse for a process minutes old — the same class of mistake
    /// the alert cards' `live` gate exists to prevent.
    #[test]
    fn termination_clears_the_entry_so_a_reused_pid_starts_unknown() {
        let Some(then) = Instant::now().checked_sub(UNUSED_AFTER + Duration::from_secs(60)) else {
            return;
        };
        note_active(424_244, then);
        assert!(unused_for_a_while(424_244).is_some(), "stale before");
        forget(424_244);
        assert!(
            idle_for(424_244).is_none(),
            "a recycled pid must not inherit the dead app's history"
        );
    }

    /// The table is bounded, and what it drops is the stalest entry —
    /// the recent activations are exactly the ones worth keeping.
    #[test]
    fn the_table_is_bounded_and_drops_the_stalest_first() {
        // A local map: the real one is shared with every other test in
        // this binary, and resetting it here would make theirs flaky.
        let mut map = HashMap::new();
        let base = Instant::now();
        for i in 0..MAX_TRACKED {
            // Oldest first, so pid 900_000 is the stalest.
            let at = base
                .checked_sub(Duration::from_secs((MAX_TRACKED - i) as u64))
                .unwrap_or(base);
            insert_bounded(&mut map, 900_000 + i as u32, at);
        }
        insert_bounded(&mut map, 999_999, base);
        assert_eq!(map.len(), MAX_TRACKED);
        assert!(map.contains_key(&999_999), "the newcomer is kept");
        assert!(!map.contains_key(&900_000), "the stalest one made room");
        // A pid already present refreshes in place rather than
        // evicting anybody: a full table must not forget an app just
        // because the user switched back to it.
        insert_bounded(&mut map, 999_999, base);
        assert_eq!(map.len(), MAX_TRACKED);
    }
}
