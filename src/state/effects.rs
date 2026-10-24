//! What a store method asks of the host that holds the store.
//!
//! The store's methods took gpui's `Context<Self>` until phase 1c of
//! docs/nsh-migration.md, and used it for four things: saying the store
//! changed, running work off the main thread and folding its answer back
//! in, keeping the tray in step, and fitting the panel to Overview. Those
//! are this trait, which `nsh/store.rs` implements around the store it
//! shares between threads.
//!
//! A background job never touches the store itself. It runs on a thread of
//! its own and sends its changes back as closures ([`Reply::apply`]), which
//! are run under the store's lock, in the order they were sent. A job that
//! reports as it goes (a disk walk, a download) sends one per report.

use super::ZStatsAppState;
use std::sync::Arc;

/// One change to the store, made under its lock.
pub type Apply = Box<dyn FnOnce(&mut ZStatsAppState, &mut dyn Effects) + Send>;

/// How a background job hands its changes back. Cheap to clone, and
/// `Send`, so a job can pass it to threads of its own.
#[derive(Clone)]
pub struct Reply(Arc<dyn Fn(Apply) -> bool + Send + Sync>);

impl Reply {
    /// A reply that delivers each change with `deliver`, which answers
    /// whether the store is still there to take it.
    pub fn new(deliver: impl Fn(Apply) -> bool + Send + Sync + 'static) -> Reply {
        Reply(Arc::new(deliver))
    }

    /// Queues `change`. False once the store is gone, so a job that
    /// reports as it goes can stop.
    pub fn apply(
        &self,
        change: impl FnOnce(&mut ZStatsAppState, &mut dyn Effects) + Send + 'static,
    ) -> bool {
        (self.0)(Box::new(change))
    }
}

pub trait Effects {
    /// The store changed: whatever reads it paints again.
    fn changed(&mut self);

    /// Runs `job` on a thread of its own; what it sends through its
    /// [`Reply`] is applied to the store in the order sent.
    fn background(&mut self, job: Box<dyn FnOnce(Reply) + Send>);

    /// The menu bar's face and its spec follow the store (`tray::sync`).
    fn sync_tray(&mut self, state: &ZStatsAppState);

    /// The panel's height follows Overview's notice row, once the store
    /// has settled (`crate::sync_panel_notice`).
    fn fit_panel(&mut self);

    /// Whether the disk-space window is open.
    fn storage_window_open(&self) -> bool;
}

/// [`Effects::background`] for a closure, boxed here.
pub fn background(fx: &mut dyn Effects, job: impl FnOnce(Reply) + Send + 'static) {
    fx.background(Box::new(job));
}

// NSH runs a window's interface on a thread of its own (one per
// `AppView`) and the store is shared between them behind a lock, which
// needs the store to be `Send`. A handle with an `Rc` inside put into it
// would undo that, so it fails here, at compile time.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<ZStatsAppState>();
};
