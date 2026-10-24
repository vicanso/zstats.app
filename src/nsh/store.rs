//! The store: one [`ZStatsAppState`] behind a lock,
//! taken by every thread that has business with it.
//!
//! gpui kept the store on its main thread and ran everything there. NSH
//! has more threads than that: each `AppView` runs its interface on an
//! engine thread of its own, the collector's ticks land on a thread, and
//! a store job ([`Effects::background`]) reports from another. So the
//! store is a `Mutex` in a static (the reason phase 1c held the store
//! `Send`), and the engines learn that it changed from a channel
//! ([`subscribe`]) rather than by owning it.
//!
//! What a store method asks for on the way ([`Effects`]) is collected
//! while the lock is held and done once it is released, in [`update`]:
//! the subscribers are told, the tray is re-dressed on the main thread
//! with what was read under the lock, and the panel's height follows. A
//! store job's changes are applied on the job's own thread, under the
//! same lock, in the order the job sent them.
//!
//! The one rule a caller must keep: never [`update`] or [`read`] from
//! inside either. A `std` mutex is not re-entrant, so that would be a
//! deadlock; [`enter`] turns it into a panic that names the cause.

use crate::state::{Effects, Reply, ZStatsAppState};
use crate::tray;
use smol::channel::{Receiver, Sender, TrySendError};
use std::cell::Cell;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;

static STORE: OnceLock<Mutex<ZStatsAppState>> = OnceLock::new();

/// One sender per engine that reads the store ([`subscribe`]).
static SUBSCRIBERS: Mutex<Vec<Sender<()>>> = Mutex::new(Vec::new());

/// What the platform host does for the store, set once at startup.
static HOST: OnceLock<Host> = OnceLock::new();

thread_local! {
    /// This thread holds the store's lock (see the module doc).
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

/// The platform host's part of the store's side effects.
pub struct Host {
    /// Runs `job` on the main thread, after whatever is running there now.
    pub main: fn(Box<dyn FnOnce() + Send>),
    /// The panel's height follows Overview's notice row. Called on the
    /// main thread.
    pub fit_panel: fn(),
    /// Whether the disk-space window is open. Called under the lock, from
    /// any thread.
    pub storage_window_open: fn() -> bool,
}

/// Puts `state` behind the lock and `host` beside it. Once, at startup,
/// before anything reads either.
pub fn install(state: ZStatsAppState, host: Host) {
    if STORE.set(Mutex::new(state)).is_err() || HOST.set(host).is_err() {
        tracing::error!("the NSH store was installed twice; keeping the first");
    }
}

fn host() -> &'static Host {
    HOST.get()
        .expect("the NSH store's host is installed at startup")
}

/// The lock, marking this thread as inside it.
fn enter() -> Inside {
    assert!(
        !INSIDE.with(Cell::get),
        "the NSH store was taken again from inside itself (a deadlock with a std mutex)"
    );
    let store = STORE.get().expect("the NSH store is installed at startup");
    // A store method that panicked left the state as far as it got; carry
    // on from there rather than fail every read after it.
    let guard = store.lock().unwrap_or_else(PoisonError::into_inner);
    INSIDE.with(|inside| inside.set(true));
    Inside(guard)
}

struct Inside(MutexGuard<'static, ZStatsAppState>);

impl Drop for Inside {
    fn drop(&mut self) {
        INSIDE.with(|inside| inside.set(false));
    }
}

/// `f` of the store, read under the lock.
pub fn read<R>(f: impl FnOnce(&ZStatsAppState) -> R) -> R {
    let inside = enter();
    f(&inside.0)
}

/// `f` of the store, changed under the lock; what it asked for on the way
/// is done once the lock is released.
pub fn update<R>(f: impl FnOnce(&mut ZStatsAppState, &mut dyn Effects) -> R) -> R {
    let mut fx = Fx::default();
    let result = {
        let mut inside = enter();
        f(&mut inside.0, &mut fx)
    };
    fx.flush();
    result
}

/// A channel that says the store changed. Bounded at one and sent without
/// waiting, so a burst of changes is one wake-up and a busy engine never
/// holds a sender back; the engine reads the store afresh when it wakes.
pub fn subscribe() -> Receiver<()> {
    let (tx, rx) = smol::channel::bounded(1);
    SUBSCRIBERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(tx);
    rx
}

/// Tells every subscriber, and forgets the ones whose engine is gone.
pub fn changed() {
    SUBSCRIBERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|tx| !matches!(tx.try_send(()), Err(TrySendError::Closed(()))));
}

/// What one [`update`] asked for.
#[derive(Default)]
struct Fx {
    changed: bool,
    /// The last tray look asked for: an earlier one in the same update is
    /// already out of date.
    tray: Option<tray::Look>,
    fit_panel: bool,
}

impl Fx {
    fn flush(self) {
        if self.changed {
            changed();
        }
        if let Some(look) = self.tray {
            (host().main)(Box::new(move || tray::wear(look)));
        }
        if self.fit_panel {
            let fit = host().fit_panel;
            (host().main)(Box::new(fit));
        }
    }
}

impl Effects for Fx {
    fn changed(&mut self) {
        self.changed = true;
    }

    fn background(&mut self, job: Box<dyn FnOnce(Reply) + Send>) {
        // Applied on the job's thread: the lock is what orders it against
        // everything else, and the store outlives every job.
        let reply = Reply::new(|change| {
            update(change);
            true
        });
        if let Err(e) = thread::Builder::new()
            .name("store-job".into())
            .spawn(move || job(reply))
        {
            tracing::error!("could not start a background job: {e}");
        }
    }

    fn sync_tray(&mut self, state: &ZStatsAppState) {
        self.tray = Some(tray::look(state));
    }

    fn fit_panel(&mut self) {
        self.fit_panel = true;
    }

    fn storage_window_open(&self) -> bool {
        (host().storage_window_open)()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic;
    use std::sync::Once;

    /// The store, once per test process. Every job the host is asked to
    /// run on "the main thread" runs at once, here.
    fn installed() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            install(
                ZStatsAppState::new(),
                Host {
                    main: |job| job(),
                    fit_panel: || {},
                    storage_window_open: || false,
                },
            );
        });
    }

    /// A std mutex taken twice on one thread hangs that thread for good,
    /// with nothing in the log; the store says why instead, and stays
    /// usable after.
    #[test]
    fn taking_the_store_from_inside_itself_panics_rather_than_hangs() {
        installed();
        let inner = panic::catch_unwind(|| update(|_, _| read(|_| ())));
        assert!(inner.is_err());
        read(|_| ());
    }

    /// Three changes before the engine wakes are one wake-up, and an
    /// engine that is gone stops being told.
    #[test]
    fn a_burst_of_changes_is_one_wake_up() {
        let rx = subscribe();
        changed();
        changed();
        changed();
        assert_eq!(rx.len(), 1);
        drop(rx);
        let before = SUBSCRIBERS.lock().unwrap().len();
        changed();
        assert!(SUBSCRIBERS.lock().unwrap().len() < before);
    }
}
