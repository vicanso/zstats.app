//! Whether a process or a tree wears a row's only colour: a burst over
//! one core, or a long low hold the sustained watcher saw (`watch.rs`) —
//! and never this app's own CPU while the daily disk check explains it.
//! Display only, like every threshold the views keep: nothing here fires
//! an alert. Shared by both front ends since the NSH Overview (phase 4b
//! of docs/nsh-migration.md); the gpui Processes, Apps and Overview pages
//! read it too, so a pid is red in all of them or in none.

use crate::state::ZStatsAppState;
use std::process;
use std::time::Instant;

/// A burst this big is worth colour — one whole core.
///
/// Used to be two cores (200%). On a 12-core machine that is a sixth of
/// the box, so the list stayed ink all day and the bar lost its only
/// severity cue. One core is still a fixed percentage, not scaled by
/// hardware: the same 110% is red here and there.
pub const HOT_PERCENT: f64 = 100.0;

/// Whether a row gets the page's only colour. Overview's top trees use
/// the burst half of this (`sustained = false`) so a 110% tree is red
/// here and there.
///
/// Two ways in, because "worth a look" has two shapes. A burst over
/// [`HOT_PERCENT`] is the obvious one. The other is a process holding a
/// low-but-real share for a long time (`watch.rs`), which **by
/// definition never crosses a threshold** — it was the whole reason that
/// watcher exists, and the colour used to ignore it: a compile pegged
/// at 110% lit up, while a daemon sitting on 60% for four hours looked
/// exactly like one at 3%.
///
/// Display only, like every threshold in the views — the alert engine is
/// untouched, and the sustained watcher still notifies on its own terms.
pub fn is_hot(cpu: f64, sustained: bool) -> bool {
    cpu > HOT_PERCENT || sustained
}

/// Whether `pid` is this app and its CPU the daily disk check's
/// (`diskwatch::explains_own_cpu`) — while it walks and for the engine's
/// averaging tail after. That row, or this app's tree, then keeps the
/// ink whatever [`is_hot`] says. The engine's report about that CPU is
/// dropped as not news for the same window (`state::alerts::keep_alert`),
/// and a red row said what the dropped banner would have: the colour of
/// an alert on a page with none, for housekeeping the badge beside it
/// names (`disk_check_badge`). Same narrowness as the alert's gate —
/// this pid only, the scheduled check only — and the same cost: a real
/// runaway of ours that starts in that window is not painted.
pub fn own_check_cpu(pid: u32, state: &ZStatsAppState) -> bool {
    pid == process::id() && state.disk_check_explains_cpu(Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The colour has two ways in, and the second one is the point: a
    /// process that never crosses the burst line still earns it by
    /// holding a share for long enough. The old rule could only see
    /// bursts, so the daemon that had been at 60% all afternoon looked
    /// exactly like one at 3%.
    #[test]
    fn a_long_hold_is_hot_even_though_it_never_crosses_the_burst_line() {
        assert!(is_hot(110.0, false), "one core and a bit is a burst");
        assert!(
            !is_hot(100.0, false),
            "exactly one core is the line, not over it"
        );
        assert!(!is_hot(60.0, false), "a moment at 60% is just work");
        assert!(is_hot(60.0, true), "the same 60% held long enough is not");
        assert!(is_hot(0.4, true), "the watcher's own bar is well under 100");
    }
}
