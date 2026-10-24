//! Memory's verdicts as the panel words them: the kernel's pressure level
//! as the long Overview's badge says it, and whether the compressor is
//! moving segments. Words over zstats' own fields — nothing here reads a
//! figure zstats does not report or draws a line of its own. Shared since
//! the NSH Overview's long form (phase 4c of docs/nsh-migration.md); the
//! gpui Overview and Alerts read it too.

use crate::format;
use crate::i18n;
use rust_i18n::t;
use zstats::snapshot::MemorySnapshot;

/// The kernel's memory-pressure level at critical (1 is normal, 2
/// warning). The only tier that earns a word of its own, beside the
/// memory row's name and in the long form's badge.
pub const PRESSURE_CRITICAL: u32 = 4;

/// The pressure badge's words and its tooltip: the kernel's own verdict,
/// not a number we derive — 1 normal, 2 warning, 4 critical. Absent has
/// two readings and they are not the same sentence: this build cannot
/// measure pressure at all (`supported`, zstats' `capabilities` since
/// 0.5.2, so the panel stops inferring it from a `None`), or it can and
/// has nothing yet. On macOS the capability is true.
///
/// The figure the verdict is derived from (`kern.memorystatus_level`) is
/// printed in the badge beside the verdict — "Normal · 62% available" —
/// not as a row next to Used, where a lower "available" would read as a
/// contradiction rather than as the kernel's narrower definition. The
/// word stays in the badge: a bare "62%" beside a memory card reads as
/// used. The tooltip says why it differs from free GB. macOS only.
pub fn badge(mem: &MemorySnapshot, supported: bool) -> (String, String) {
    let (label, tip) = match mem.pressure_level {
        Some(l) if l >= PRESSURE_CRITICAL => (
            i18n::tr("overview.pressure_critical"),
            i18n::tr("overview.pressure_tip_critical"),
        ),
        Some(l) if l >= 2 => (
            i18n::tr("overview.pressure_warning"),
            i18n::tr("overview.pressure_tip_warning"),
        ),
        Some(_) => (
            i18n::tr("overview.pressure_normal"),
            i18n::tr("overview.pressure_tip_normal"),
        ),
        None if !supported => (
            i18n::tr("overview.pressure_none"),
            i18n::tr("overview.pressure_tip_none"),
        ),
        // Measurable here, just not measured yet — the same `—` every
        // other waiting figure shows, not a claim about the platform.
        None => (
            format::PLACEHOLDER.to_string(),
            i18n::tr("overview.pressure_tip_waiting"),
        ),
    };
    match badge_available(mem.pressure_level, mem.kernel_available_percent) {
        Some(pct) => (
            t!("overview.pressure_badge", level = label, pct = pct).to_string(),
            format!("{tip} {}", t!("overview.pressure_tip_kernel", pct = pct)),
        ),
        None => (label, tip),
    }
}

/// The kernel's available percent for the pressure badge — only beside a
/// verdict. Without a level the badge reads `—` or "not measured", and a
/// percentage hung on either would be a figure with nothing to explain.
fn badge_available(level: Option<u32>, kernel_available: Option<u32>) -> Option<u32> {
    level.and(kernel_available)
}

/// Compressor segments moved per second, as (out, in) — `None` while both
/// are still, not yet measured (the first sample), or not measurable (off
/// macOS). A settled machine gets no clause rather than a pair of zeros:
/// zstats' own pressure line makes the same call, and it is what lets the
/// memory card and the pressure alert's sentence grow only when there is
/// something to read.
pub fn swap_moving(ins: Option<u64>, outs: Option<u64>) -> Option<(u64, u64)> {
    match (outs, ins) {
        (Some(outs), Some(ins)) if outs > 0 || ins > 0 => Some((outs, ins)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kernel_percent_rides_the_badge_only_beside_a_verdict() {
        assert_eq!(badge_available(Some(1), Some(62)), Some(62));
        assert_eq!(badge_available(Some(4), Some(8)), Some(8));
        // Waiting for a first sample, or a platform with neither figure
        assert_eq!(badge_available(None, Some(62)), None);
        assert_eq!(badge_available(Some(1), None), None);
    }

    #[test]
    fn swap_activity_is_news_only_while_something_moves() {
        assert_eq!(swap_moving(Some(40), Some(120)), Some((120, 40)));
        assert_eq!(swap_moving(Some(0), Some(3)), Some((3, 0)));
        // Settled, first sample, off macOS: no clause at all
        assert_eq!(swap_moving(Some(0), Some(0)), None);
        assert_eq!(swap_moving(None, None), None);
        assert_eq!(swap_moving(Some(5), None), None);
    }
}
