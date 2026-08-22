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

/// A load-average figure for the Processor card's footnote. One decimal
/// is where the signal lives — a 10-core machine's 9.6 and 10.4 read on
/// opposite sides of the core count — and past 100 even that is noise.
pub fn load(v: f64) -> String {
    if v < 100.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.0}")
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

/// Single-core CPU time as an amount, not a rate.
///
/// The unit people actually reason in is core-minutes and core-hours: "this
/// spent 40 minutes of a core today" lands where "2 400 000 ms" does not.
/// Seconds below a minute, because a short-lived process reporting "0m" would
/// look like a bug.
pub fn core_time(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Whether a process state is worth showing.
///
/// At any instant almost every process is asleep — on a 12-core machine with
/// 700 processes, 696 were `S` and 6 `R`. "Sleeping" is the Unix default, not
/// information, and a sampled state says little anyway: a process using 50% of
/// a core is asleep half the time it is looked at. What does matter is a state
/// that should not persist.
///
/// Written as "not one of the ordinary ones" so an unrecognised state counts
/// as notable rather than being silently hidden.
pub fn notable_status(status: &str) -> bool {
    !matches!(
        status,
        "Sleeping" | "Sleep" | "Runnable" | "Running" | "Run" | "Idle" | "Parked" | "Waking"
    )
}

/// Per-process memory, which spans four orders of magnitude and mostly sits
/// at the bottom of it. Fixing the unit at GB collapses almost every row onto
/// "0.0 GB"/"0.1 GB": on a typical machine every process is under a gigabyte
/// and the median is around 10 MB.
pub fn memory(bytes: u64) -> String {
    let v = bytes as f64;
    if v >= GIB {
        format!("{:.1} GB", v / GIB)
    } else {
        format!("{:.0} MB", v / MIB)
    }
}

/// Disk-style capacity, promoting to TB past a terabyte.
///
/// Below 1 GiB this used to round to `0 GB`, so a 400 MB installer
/// painted "0 GB free of 0 GB · 36% used" — a used percent next to two
/// zeros looks like a broken collector, not a small volume. Megabytes
/// until a gigabyte, one decimal under 10 GB (same rule as [`gb`]),
/// whole GB after that.
pub fn capacity(bytes: u64) -> String {
    let v = bytes as f64;
    if v >= TIB {
        format!("{:.1} TB", v / TIB)
    } else if v >= GIB {
        let gb = v / GIB;
        if gb < 10.0 {
            format!("{gb:.1} GB")
        } else {
            format!("{gb:.0} GB")
        }
    } else {
        format!("{:.0} MB", v / MIB)
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

/// Uptime, at most two units: "3d 4h", "4h 12m", "9m". A zero second
/// unit is omitted — "2h", not "2h 00m": on a round figure the trailing
/// zeros read as a glitch, and the precision they claim ("exactly on
/// the hour") is not one a minute-granular label can honour anyway.
pub fn uptime(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    if d > 0 {
        if h > 0 {
            format!("{d}d {h}h")
        } else {
            format!("{d}d")
        }
    } else if h > 0 {
        if m > 0 {
            format!("{h}h {m:02}m")
        } else {
            format!("{h}h")
        }
    } else {
        format!("{m}m")
    }
}

/// Digit grouping for counts ("48213" → "48,213") — past four digits a
/// bare run of digits stops being readable at a glance.
pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A finished operation's duration. Seconds stay visible at minute scale
/// ("2m 34s") — that is the resolution a person compares two runs at,
/// which is what this figure is for.
pub fn took(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
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

/// How long something has been going on, in the locale's words.
///
/// Not [`uptime`], which is the `4h 12m` shorthand a monospaced column
/// needs. This one sits inside a sentence next to [`ago`] — an alert
/// head reads "3 hours 24 minutes ago · going 1 hour 19 minutes", and
/// having one half spelled out while the other wore `1h 19m` made the
/// two look like different kinds of measurement.
pub fn span(elapsed: Duration) -> String {
    use rust_i18n::t;
    let secs = elapsed.as_secs();
    if secs < 60 {
        return t!("time.under_a_minute").to_string();
    }
    let h = secs / 3_600;
    let m = (secs % 3_600) / 60;
    if h > 0 {
        t!("time.hours_minutes", h = h, m = format!("{m:02}")).to_string()
    } else {
        t!("time.minutes", m = m).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_keeps_one_decimal_until_it_stops_mattering() {
        assert_eq!(load(0.0), "0.0");
        assert_eq!(load(9.96), "10.0");
        assert_eq!(load(10.44), "10.4");
        assert_eq!(load(123.4), "123");
    }

    #[test]
    fn thousands_groups_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(48_213), "48,213");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    /// The History tab's unit. Core-hours is how you reason about a day's
    /// spending; milliseconds is not.
    #[test]
    fn core_time_reads_as_an_amount_at_every_scale() {
        assert_eq!(core_time(0), "0s");
        assert_eq!(core_time(45_000), "45s");
        // Rolls to minutes exactly at 60s, not at 59.
        assert_eq!(core_time(59_999), "59s");
        assert_eq!(core_time(60_000), "1m");
        assert_eq!(core_time(45 * 60_000), "45m");
        assert_eq!(core_time(3_600_000), "1h 0m");
        // 3h 20m — the shape a real day's top spender takes.
        assert_eq!(core_time(12_000_000), "3h 20m");
    }

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
    fn only_abnormal_states_are_notable() {
        for ordinary in ["Sleeping", "Runnable", "Running", "Idle"] {
            assert!(!notable_status(ordinary), "{ordinary} should be hidden");
        }
        // A zombie has exited but its parent never reaped it; a stopped
        // process is suspended. Both should stand out.
        for odd in ["Zombie", "Stopped", "Dead", "UninterruptibleDiskSleep"] {
            assert!(notable_status(odd), "{odd} should be shown");
        }
        // Unrecognised states are surfaced rather than swallowed.
        assert!(notable_status("SomethingNew"));
    }

    #[test]
    fn process_memory_stays_legible_at_every_scale() {
        // The common case: a median process is tens of megabytes, and in GB
        // it would read "0.0 GB" — indistinguishable from its neighbours.
        assert_eq!(memory((10.0 * MIB) as u64), "10 MB");
        assert_eq!(memory((885.0 * MIB) as u64), "885 MB");
        assert_eq!(memory((2.5 * GIB) as u64), "2.5 GB");
    }

    #[test]
    fn capacity_promotes_to_terabytes() {
        assert_eq!(capacity((994.0 * GIB) as u64), "994 GB");
        assert_eq!(capacity((2.0 * TIB) as u64), "2.0 TB");
    }

    #[test]
    fn capacity_does_not_collapse_small_volumes_to_zero_gb() {
        // The installer card that read "0 GB free of 0 GB · 36% used".
        assert_eq!(capacity((400.0 * MIB) as u64), "400 MB");
        assert_eq!(capacity(0), "0 MB");
        assert_eq!(capacity((2.5 * GIB) as u64), "2.5 GB");
        assert_eq!(capacity((23.4 * GIB) as u64), "23 GB");
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
        // Round figures drop the zero unit instead of wearing "00".
        assert_eq!(uptime(2 * 3_600), "2h");
        assert_eq!(uptime(3 * 86_400), "3d");
        assert_eq!(uptime(0), "0m");
    }

    #[test]
    fn ago_collapses_the_first_minute() {
        assert_eq!(ago(Duration::from_secs(5)), "just now");
        assert_eq!(ago(Duration::from_secs(12 * 60)), "12m ago");
        assert_eq!(ago(Duration::from_secs(3_600 + 4 * 60)), "1h 04m ago");
    }
}
