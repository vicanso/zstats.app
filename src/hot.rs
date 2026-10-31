//! Whether a process or a tree wears a row's only colour: a burst over one
//! core, or a long low hold the sustained watcher saw (`watch.rs`) — and
//! never this app's own CPU while the daily disk check explains it. Beside
//! it the other lines a figure turns colour at: a core's, swap's, a full
//! volume's, a busy GPU's and a sensor near its own ceiling. Display only,
//! like every threshold the pages keep: nothing here fires an alert. Every
//! page reads it, so a pid is red in all of them or in none (moved out of
//! the gpui pages for the NSH Overview, phases 4b and 4c of
//! docs/nsh-migration.md).

use crate::state::ZStatsAppState;
use std::process;
use std::time::Instant;
use zstats::snapshot::{DiskSnapshot, MemorySnapshot, TemperatureSnapshot};

/// A burst this big is worth colour — one whole core.
///
/// Used to be two cores (200%). On a 12-core machine that is a sixth of
/// the box, so the list stayed ink all day and the bar lost its only
/// severity cue. One core is still a fixed percentage, not scaled by
/// hardware: the same 110% is red here and there.
pub const HOT_PERCENT: f64 = 100.0;

/// A core cluster's bar turns accent past this (Overview's P/E rows).
pub const CORE_HOT: f32 = 85.0;

/// Swap past this share of **physical memory** is worth colour — display
/// only. The kernel can still report pressure Normal while a lot of the
/// working set has been pushed to disk; that is the signal the badge
/// will not give, and it is what this is for.
///
/// Measured against RAM, and not against swap's own allocation, because
/// macOS sizes swap on demand: `/System/Volumes/VM/` holds uniform 1 GB
/// files and the kernel adds one whenever free swap drops to roughly a
/// file's worth. Steady state is therefore `used/total ≈ (N-1)/N` for N
/// files, which *rises toward 100% as the machine swaps more* — 80% at
/// five files, 86% at seven, 93% at fourteen. A share-of-allocation bar
/// is consequently permanently crossed on any Mac that ever grew past
/// about five swapfiles, and was observed painting red on a machine
/// sitting at 55% memory free. Against RAM the same reading is 24%, and
/// the number means the same thing on a 16 GB laptop as on a 128 GB
/// desktop.
///
/// 50 is a judgement, not a derivation: half the physical memory's worth
/// of pages living on disk. Apple Silicon swaps readily well below that
/// without anything being wrong.
pub const SWAP_HOT: f32 = 50.0;

/// Whether the Swap figure wears colour ([`SWAP_HOT`]).
///
/// Deliberately not `mem.swap_used_percent`: that field *is* swap
/// against its own allocation, the ratio [`SWAP_HOT`] explains is
/// unusable here. Dividing those two bytes locally would also have been
/// a second copy of a figure zstats already reports, which AGENTS.md
/// forbids. This asks something zstats does not answer, and only ever
/// to pick a colour. The `total_bytes > 0` is a guard against a
/// collector reporting no memory at all, where any swap would otherwise
/// be a huge percentage and paint red.
pub fn swap_hot(mem: &MemorySnapshot) -> bool {
    mem.total_bytes > 0 && (mem.swap_used_bytes as f32 / mem.total_bytes as f32) * 100.0 >= SWAP_HOT
}

/// The design's default disk alert bar: a volume's bar wears the accent
/// past it (Hardware's volume cards).
pub const FULL_PERCENT: f32 = 90.0;

/// Whether a volume's bar wears the accent: past [`FULL_PERCENT`], and
/// not removable — a full removable drive is normal, a full boot volume
/// is not.
pub fn volume_full(disk: &DiskSnapshot) -> bool {
    disk.used_percent > FULL_PERCENT && !disk.is_removable
}

/// A GPU's busy bar turns accent here. The same line as a CPU core
/// ([`CORE_HOT`]): a GPU pinned near full is the same kind of fact as a
/// core pinned near full. zstats has no GPU rule, and this fires nothing.
pub const GPU_HOT: f32 = 85.0;

/// A sensor reads as hot at this fraction of its own ceiling — the same
/// `celsius / ceiling` the list sorts by, so the red readings are always
/// the top rows.
///
/// A fraction, not degrees, because a flat line cannot rank hardware
/// with different ceilings. The old absolute 80 °C did two things wrong
/// at once: on an Apple Silicon machine working hard, *every* pACC/eACC
/// channel sits at 81–84 °C — 34 red rows carry exactly as much
/// information as zero — while a battery cell at 55 °C, five degrees
/// from its own 60 °C limit, stayed neutral. Ninety percent of the
/// firmware's own number colours the one that is actually near its
/// line and lets the merely-warm stay quiet.
pub const HOT_FRACTION: f32 = 0.9;

/// Ceiling assumed when a sensor reports no critical point of its own.
/// With [`HOT_FRACTION`] this puts the no-crit colour line at 99 °C — far
/// above the old 80, deliberately: for hardware whose ceiling we do not
/// know, "hot" was a guess, and a CPU genuinely at 99 °C is past any
/// plausible one.
pub const ASSUMED_MAX_CELSIUS: f32 = 110.0;

/// The number a reading is measured against, for both order and colour.
///
/// Critical when the firmware reports one — that *is* a ceiling.
/// Otherwise [`ASSUMED_MAX_CELSIUS`], never the observed peak: the
/// hottest sensor on the page *is* the peak, so against it the top row
/// would be at its limit by construction.
pub fn ceiling(crit: Option<f32>) -> f32 {
    crit.unwrap_or(ASSUMED_MAX_CELSIUS).max(1.0)
}

/// How much of its own range a sensor has used — the sensor list's sort
/// key and (against [`HOT_FRACTION`]) the colour's input. One definition
/// so the two can never disagree: the top row is the first to turn red.
pub fn crit_fraction(t: &TemperatureSnapshot) -> f32 {
    t.celsius / ceiling(t.critical_celsius)
}

/// Colour turns at [`HOT_FRACTION`] of the sensor's own ceiling — never
/// against the observed peak, which is not a danger line, and never
/// against a flat degree count, which cannot tell a core five degrees
/// into its range from a battery cell five degrees from its limit.
pub fn sensor_hot(celsius: f32, crit: Option<f32>) -> bool {
    celsius / ceiling(crit) >= HOT_FRACTION
}

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
    fn the_ceiling_is_never_the_observed_peak() {
        // The hottest sensor *is* the peak. Measured against it, the top
        // row would sit at its limit by construction — which is what
        // "max 52" next to 51.8 °C looked like, when 51.8 / 110 is half.
        assert_eq!(ceiling(None), ASSUMED_MAX_CELSIUS);
        assert_eq!(ceiling(Some(100.0)), 100.0);
    }

    fn temp(celsius: f32, crit: Option<f32>) -> TemperatureSnapshot {
        TemperatureSnapshot {
            label: "t".into(),
            celsius,
            max_celsius: None,
            critical_celsius: crit,
        }
    }

    #[test]
    fn hot_means_near_its_own_ceiling_not_past_a_flat_line() {
        // The all-red machine: every cluster channel at 81–84 °C against
        // a 110 °C assumed ceiling is warm, not near a limit. Under the
        // old flat 80 °C line all 34 rows coloured and the colour said
        // nothing.
        assert!(!sensor_hot(84.0, None));
        assert!(!sensor_hot(84.0, Some(110.0)));
        // The one the flat line missed: five degrees from its own limit.
        assert!(sensor_hot(55.0, Some(60.0)));
        assert!(sensor_hot(92.0, Some(100.0)), "90% of crit colours");
        assert!(!sensor_hot(70.0, Some(100.0)));
        assert!(sensor_hot(99.5, None), "no-crit line sits at 90% of 110");
    }

    #[test]
    fn the_ranking_puts_the_endangered_sensor_above_the_merely_warm() {
        // 55/60 has used 92% of its range; 84/110 has used 76%. Raw
        // degrees would bury the battery cell under every cluster
        // channel — the exact row the page exists to surface.
        let battery = temp(55.0, Some(60.0));
        let core = temp(84.0, Some(110.0));
        assert!(crit_fraction(&battery) > crit_fraction(&core));
        // And the sort key IS the colour key, so the hot set is always
        // a prefix of the list — the preview truncation can only ever
        // swallow quiet rows.
        assert!(sensor_hot(battery.celsius, battery.critical_celsius));
        assert!(!sensor_hot(core.celsius, core.critical_celsius));
    }

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
