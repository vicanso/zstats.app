//! Number → string helpers.
//!
//! Every string in the design goes through one of these, and they carry real
//! rules (when a decimal appears, what a missing rate looks like), so they
//! live apart from the views and are unit-tested. Lives at the crate root
//! rather than under `views` because the tray title needs [`pct`] too.

use std::time::Duration;

const KIB: f64 = 1024.0;
const MIB: f64 = 1024.0 * 1024.0;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const TIB: f64 = 1024.0 * 1024.0 * 1024.0 * 1024.0;

/// Stand-in for a value that does not exist yet. The first tick has no
/// previous sample to diff against, so every rate legitimately starts here.
pub const PLACEHOLDER: &str = "—";

/// Percentages: one decimal below 100, none above. Process CPU is in
/// single-core units and routinely exceeds 100, where a decimal is noise.
pub fn pct(v: f32) -> String {
    if v < 100.0 {
        format!("{v:.1}%")
    } else {
        format!("{v:.0}%")
    }
}

/// [`pct`] with a leading space when the integer part is one digit, so a
/// JetBrains Mono column of `8.0%` / `16.3%` stays put. Pads from the
/// *rendered* digits (`9.96` becomes `10.0%`, no space).
pub fn pct_col(v: f32) -> String {
    let s = pct(v);
    if integer_digits(&s) == 1 {
        format!(" {s}")
    } else {
        s
    }
}

/// Whole-number percent digits (no unit) for the Processor headline.
/// Same one-space pad as [`pct_col`], after rounding.
pub fn whole_pct(v: f32) -> String {
    let n = v.round().abs();
    if n < 10.0 {
        format!(" {n:.0}")
    } else {
        format!("{n:.0}")
    }
}

fn integer_digits(formatted: &str) -> usize {
    formatted
        .trim_start()
        .trim_end_matches('%')
        .split('.')
        .next()
        .map_or(0, str::len)
}

/// Memory-style capacity, always in GB: "23.4 GB" under ten, "112 GB" over.
pub fn gb(bytes: u64) -> String {
    let v = bytes as f64 / GIB;
    if v < 10.0 {
        format!("{v:.1} GB")
    } else {
        format!("{v:.0} GB")
    }
}

/// Disk-style capacity, promoting to TB past a terabyte.
pub fn capacity(bytes: u64) -> String {
    let v = bytes as f64;
    if v >= TIB {
        format!("{:.1} TB", v / TIB)
    } else {
        format!("{:.0} GB", v / GIB)
    }
}

/// Throughput. `None` means the collector had no previous sample.
pub fn rate(bytes_per_sec: Option<u64>) -> String {
    let Some(b) = bytes_per_sec else {
        return PLACEHOLDER.to_string();
    };
    let v = b as f64;
    if v >= MIB {
        format!("{:.1} MB/s", v / MIB)
    } else if v >= KIB {
        format!("{:.0} kB/s", v / KIB)
    } else {
        format!("{b} B/s")
    }
}

/// Uptime, at most two units: "3d 4h", "4h 12m", "9m".
pub fn uptime(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
}

/// How long ago something happened. Copy comes from the active locale.
pub fn ago(elapsed: Duration) -> String {
    use rust_i18n::t;
    let secs = elapsed.as_secs();
    if secs < 60 {
        return t!("time.just_now").to_string();
    }
    let h = secs / 3_600;
    let m = (secs % 3_600) / 60;
    if h > 0 {
        t!("time.hours_minutes_ago", h = h, m = format!("{m:02}")).to_string()
    } else {
        t!("time.minutes_ago", m = m).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentages_drop_the_decimal_once_past_100() {
        assert_eq!(pct(42.34), "42.3%");
        assert_eq!(pct(9.0), "9.0%");
        // Single-core units: a busy process reads well over 100.
        assert_eq!(pct(612.4), "612%");
        assert_eq!(pct(100.0), "100%");
    }

    #[test]
    fn column_percents_pad_a_space_below_ten() {
        assert_eq!(pct_col(8.0), " 8.0%");
        assert_eq!(pct_col(9.94), " 9.9%");
        assert_eq!(pct_col(9.96), "10.0%"); // rounds into two digits, no pad
        assert_eq!(pct_col(10.0), "10.0%");
        assert_eq!(pct_col(612.4), "612%");
        assert_eq!(whole_pct(9.4), " 9");
        assert_eq!(whole_pct(9.6), "10");
        assert_eq!(whole_pct(16.0), "16");
    }

    #[test]
    fn memory_keeps_one_decimal_only_under_ten_gb() {
        assert_eq!(gb((2.5 * GIB) as u64), "2.5 GB");
        assert_eq!(gb((23.4 * GIB) as u64), "23 GB");
        assert_eq!(gb(0), "0.0 GB");
    }

    #[test]
    fn capacity_promotes_to_terabytes() {
        assert_eq!(capacity((994.0 * GIB) as u64), "994 GB");
        assert_eq!(capacity((2.0 * TIB) as u64), "2.0 TB");
    }

    #[test]
    fn missing_rate_renders_as_the_placeholder() {
        // The first tick has nothing to diff against — this is the
        // collector's contract, not an error.
        assert_eq!(rate(None), PLACEHOLDER);
        assert_eq!(rate(Some(0)), "0 B/s");
        assert_eq!(rate(Some((8.4 * MIB) as u64)), "8.4 MB/s");
        assert_eq!(rate(Some((420.0 * KIB) as u64)), "420 kB/s");
    }

    #[test]
    fn uptime_shows_at_most_two_units() {
        assert_eq!(uptime(3 * 86_400 + 4 * 3_600 + 30 * 60), "3d 4h");
        assert_eq!(uptime(4 * 3_600 + 12 * 60), "4h 12m");
        assert_eq!(uptime(9 * 60), "9m");
    }

    #[test]
    fn ago_collapses_the_first_minute() {
        assert_eq!(ago(Duration::from_secs(5)), "just now");
        assert_eq!(ago(Duration::from_secs(12 * 60)), "12m ago");
        assert_eq!(ago(Duration::from_secs(3_600 + 4 * 60)), "1h 04m ago");
    }
}
