//! Hour-window CPU trend per application tree — the question the top
//! list is structurally blind to.
//!
//! Same class as [`crate::watch`]: a display-layer observer answering
//! something zstats cannot. CPU% is a rate, and a rate has no memory —
//! the snapshot cannot tell "always 30%" from "was 2% ten minutes ago",
//! and zstats' rolling averages reach back only 60 seconds. Yet "who
//! *changed*" is usually why the panel got opened at all: the resident
//! that is always on top is normal, the tree that climbed out of nowhere
//! is the news. Answering that takes an hour of history nobody else
//! keeps, so it is kept here — fed from the resident collector's ticks,
//! alive while the panel is closed, and **display only**: no
//! `AlertEvent`, no notification, colour untouched (a rise is news, not
//! a threshold).
//!
//! Storage is sixty minute-slots per tree, `u16` percent-of-one-core —
//! the unit every CPU figure in the app already speaks. A minute keeps
//! the **max** of its samples: "rising" asks what a tree reached, and a
//! mean would let one idle sample inside the minute talk a real climb
//! back down. Whole percent is plenty for a trend, and the headroom
//! (65 534% ≈ 655 cores) means saturation is theoretical. ~100 trees ×
//! 60 × 2 B lands under 20 KB with the map around it.
//!
//! Two kinds of absence, kept distinct because they mean different
//! things (the same honesty rule that keeps History off line charts):
//!
//! - **The collector did not report a minute** (machine asleep, panel
//!   process just launched): unknowable — the minute is excluded from
//!   both sides of the comparison. A global ring of reported minutes is
//!   what tells this case apart.
//! - **The collector reported, but this tree was not in the groups**:
//!   the groups are the top-`max-processes` by CPU, so absence means
//!   "below that cut" — genuinely quiet, and it counts as zero. This is
//!   what lets a tree that climbs out of *nothing* register the full
//!   climb instead of having no baseline.
//!
//! Keyed by tree name, accepting that two same-named trees merge to
//! their max: a root pid key would be broken by every app restart, and
//! pid reuse would splice two different programs into one curve — the
//! same reason the alert cards gate their buttons on `SeenAlert::live`.
//!
//! No gpui types, minutes handed in by the caller — testable against
//! hand-built sequences, like `watch.rs`.

use std::collections::HashMap;

/// Minutes of history per tree: the hour the card talks about.
const SLOTS: usize = 60;

/// Slot value for "no reading survived for this minute".
const NO_DATA: u16 = u16::MAX;

/// Highest storable reading, % of one core (the value below the
/// sentinel). 655 cores' worth — saturation is a formality.
const MAX_PCT: u16 = u16::MAX - 1;

/// The "now" side of the comparison: mean of the newest reported
/// minutes. Five, not one — a single minute is one scheduler mood, and
/// the card should say "has been climbing", not "just blinked".
const RECENT_MINUTES: u64 = 5;

/// Reported minutes the baseline needs before a rise is worth stating.
/// Below this the "earlier hour" is a handful of samples and the delta
/// is mostly noise; the card simply has no verdict yet (first minutes
/// after launch), which is honest.
const BASELINE_MIN: usize = 5;

/// One ring of sixty minute-slots addressed by absolute minute number.
/// Skipped minutes are cleared on advance, so a slot can never leak a
/// reading from an hour ago into the current window.
struct Ring {
    slots: [u16; SLOTS],
    /// Absolute minute of the newest written slot.
    head: u64,
}

impl Ring {
    fn new(minute: u64, value: u16) -> Self {
        let mut ring = Ring {
            slots: [NO_DATA; SLOTS],
            head: minute,
        };
        ring.slots[(minute % SLOTS as u64) as usize] = value;
        ring
    }

    /// Advance to `minute` (clearing everything skipped) and merge
    /// `value` in by max — see the module doc for why max.
    fn record(&mut self, minute: u64, value: u16) {
        if minute < self.head {
            // A clock that went backwards is not a reading.
            return;
        }
        if minute > self.head {
            let gap = (minute - self.head).min(SLOTS as u64);
            for step in 1..=gap {
                self.slots[((self.head + step) % SLOTS as u64) as usize] = NO_DATA;
            }
            self.head = minute;
        }
        let slot = &mut self.slots[(minute % SLOTS as u64) as usize];
        *slot = if *slot == NO_DATA {
            value
        } else {
            (*slot).max(value)
        };
    }

    /// The reading for an absolute minute, if it is inside the window
    /// and was actually written.
    fn at(&self, minute: u64) -> Option<u16> {
        if minute > self.head || self.head - minute >= SLOTS as u64 {
            return None;
        }
        let value = self.slots[(minute % SLOTS as u64) as usize];
        (value != NO_DATA).then_some(value)
    }
}

/// The hour of per-tree CPU history behind Overview's "climbing" rows.
#[derive(Default)]
pub struct AppTrend {
    /// Which minutes the collector reported at all — what separates
    /// "machine was asleep" from "tree was quiet".
    reported: Option<Ring>,
    apps: HashMap<String, Ring>,
}

impl AppTrend {
    /// Feed one tick's application trees. `minute` is minutes since the
    /// Unix epoch — wall clock, not `Instant`, because the slots must
    /// line up across a sleep.
    pub fn sample<'a>(&mut self, minute: u64, trees: impl Iterator<Item = (&'a str, f32)>) {
        match &mut self.reported {
            Some(ring) => ring.record(minute, 1),
            None => self.reported = Some(Ring::new(minute, 1)),
        }
        for (name, pct) in trees {
            let value = pct.max(0.0).round().min(f32::from(MAX_PCT)) as u16;
            match self.apps.get_mut(name) {
                Some(ring) => ring.record(minute, value),
                None => {
                    self.apps.insert(name.to_string(), Ring::new(minute, value));
                }
            }
        }
        // A tree silent for the whole window has nothing left to say —
        // every slot it could contribute is already out of range.
        self.apps
            .retain(|_, ring| minute.saturating_sub(ring.head) < SLOTS as u64);
    }

    /// Percent-of-one-core points this tree's recent minutes sit above
    /// its earlier-hour average. `None` until enough of the hour has
    /// been reported for the comparison to mean anything.
    pub fn rise(&self, name: &str) -> Option<f32> {
        let reported = self.reported.as_ref()?;
        let ring = self.apps.get(name)?;
        let now = reported.head;
        let (mut recent_sum, mut recent_n) = (0f32, 0u32);
        let (mut base_sum, mut base_n) = (0f32, 0u32);
        for minute in now.saturating_sub(SLOTS as u64 - 1)..=now {
            if reported.at(minute).is_none() {
                // Asleep / before launch: unknowable, on neither side.
                continue;
            }
            // Reported but absent from the groups = below the collector's
            // cut = quiet. Zero, and that is a statement, not a gap.
            let value = f32::from(ring.at(minute).unwrap_or(0));
            if now - minute < RECENT_MINUTES {
                recent_sum += value;
                recent_n += 1;
            } else {
                base_sum += value;
                base_n += 1;
            }
        }
        if recent_n == 0 || (base_n as usize) < BASELINE_MIN {
            return None;
        }
        Some(recent_sum / recent_n as f32 - base_sum / base_n as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample one tree at `pct` for every minute in `minutes`.
    fn feed(trend: &mut AppTrend, name: &str, minutes: std::ops::Range<u64>, pct: f32) {
        for m in minutes {
            trend.sample(m, [(name, pct)].into_iter());
        }
    }

    /// Report minutes with no tree in them at all.
    fn idle(trend: &mut AppTrend, minutes: std::ops::Range<u64>) {
        for m in minutes {
            trend.sample(m, std::iter::empty());
        }
    }

    #[test]
    fn a_minute_keeps_the_max_of_its_samples() {
        let mut trend = AppTrend::default();
        trend.sample(10, [("zed", 40.0)].into_iter());
        trend.sample(10, [("zed", 250.0)].into_iter());
        trend.sample(10, [("zed", 90.0)].into_iter());
        // 55 quiet baseline minutes so rise() has a verdict.
        idle(&mut trend, 11..66);
        feed(&mut trend, "zed", 66..71, 250.0);
        assert_eq!(trend.rise("zed"), Some(250.0));
    }

    #[test]
    fn a_climber_reads_as_its_climb() {
        let mut trend = AppTrend::default();
        feed(&mut trend, "make", 0..55, 5.0);
        feed(&mut trend, "make", 55..60, 305.0);
        let rise = trend.rise("make").expect("a full hour has a verdict");
        assert!((rise - 300.0).abs() < 1.0, "5% → 305% is a 300-point rise");
    }

    #[test]
    fn a_flat_tree_reads_as_flat() {
        let mut trend = AppTrend::default();
        feed(&mut trend, "WindowServer", 0..60, 28.0);
        let rise = trend.rise("WindowServer").expect("verdict");
        assert!(rise.abs() < 0.5, "steady 28% is not a rise: {rise}");
    }

    #[test]
    fn climbing_out_of_nothing_counts_the_whole_climb() {
        // The card's most valuable catch: a tree that was not even in
        // the groups an hour ago. Reported-but-absent minutes are a
        // quiet baseline of zero, not a missing one.
        let mut trend = AppTrend::default();
        idle(&mut trend, 0..55);
        feed(&mut trend, "softwareupdated", 55..60, 180.0);
        assert_eq!(trend.rise("softwareupdated"), Some(180.0));
    }

    #[test]
    fn sleep_gaps_are_on_neither_side_of_the_comparison() {
        // Ten reported minutes, forty asleep, ten reported. The gap
        // must not read as a quiet baseline — the machine was not
        // running, so nothing about the tree is known.
        let mut trend = AppTrend::default();
        feed(&mut trend, "zed", 0..10, 100.0);
        // minutes 10..50 never sampled at all
        feed(&mut trend, "zed", 50..60, 100.0);
        let rise = trend.rise("zed").expect("ten baseline minutes reported");
        assert!(
            rise.abs() < 0.5,
            "steady across a sleep is still steady: {rise}"
        );
    }

    #[test]
    fn too_little_history_has_no_verdict() {
        // Four reported minutes: everything inside RECENT_MINUTES, so
        // the baseline is empty — and a baseline of almost nothing must
        // say nothing rather than guess.
        let mut trend = AppTrend::default();
        feed(&mut trend, "zed", 0..4, 200.0);
        assert_eq!(trend.rise("zed"), None);
    }

    #[test]
    fn an_hour_of_silence_evicts_the_tree() {
        let mut trend = AppTrend::default();
        feed(&mut trend, "gone", 0..2, 300.0);
        idle(&mut trend, 2..62);
        assert_eq!(trend.rise("gone"), None, "evicted, not remembered");
        assert!(trend.apps.is_empty(), "the map must not grow for a day");
    }

    #[test]
    fn the_ring_never_leaks_last_hours_reading_into_this_one() {
        // Minute 5 and minute 65 share a slot. Writing 65 must clear the
        // old reading rather than max-merge with it — a stale 400%
        // surviving into this hour would manufacture a fall — and minute
        // 5 itself must fall out of the window (65 − 5 = 60 ≥ 60).
        let mut trend = AppTrend::default();
        trend.sample(5, [("zed", 400.0)].into_iter());
        trend.sample(65, [("zed", 10.0)].into_iter());
        let ring = trend.apps.get("zed").expect("still live");
        assert_eq!(ring.at(5), None, "minute 5 is out of the window");
        assert_eq!(ring.at(65), Some(10), "the shared slot holds only 65");
        // One minute earlier the same reading was still in range: the
        // window is exactly the last sixty minutes, not fifty-nine.
        let mut edge = AppTrend::default();
        edge.sample(5, [("zed", 400.0)].into_iter());
        edge.sample(64, [("zed", 10.0)].into_iter());
        assert_eq!(edge.apps.get("zed").unwrap().at(5), Some(400));
    }

    #[test]
    fn a_reading_saturates_instead_of_wrapping() {
        let mut trend = AppTrend::default();
        trend.sample(0, [("mega", 1.0e9)].into_iter());
        assert_eq!(trend.apps.get("mega").unwrap().at(0), Some(MAX_PCT));
    }
}
