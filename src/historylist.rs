//! The History tab's rules, on either front end: how the period's
//! spenders are ordered under each lens and how many are named, what a
//! row is titled and when it says its pid, the words and figures on its
//! two lines, and Today's band — which part of the day it covers, how
//! dark a cell is, the clock under it and what hovering it spells out.
//! Every figure is zstats' record, read by `history.rs`; this orders and
//! words. Shared since the NSH History page (phase 4i of
//! docs/nsh-migration.md); `views/history.rs` reads it too, and says why
//! the page looks the way it does.

use crate::format;
use crate::history::{self, Band, HistoryShape, Spender, Stretch};
use crate::i18n;
use crate::state::HistorySort;
use rust_i18n::t;
use std::cmp::Reverse;
use std::collections::HashMap;

/// How many rows to name. Beyond this the tail is all daemons doing their job.
pub const TOP_N: usize = 12;

/// The rows on screen under a lens.
pub struct Ranked<'a> {
    pub shown: Vec<&'a Spender>,
    /// Rows past [`TOP_N`] while the list is not expanded — the chip's count
    pub hidden: usize,
    /// The biggest amount under the lens: a share meter's full track
    pub top: u64,
    /// How many shown rows carry each name ([`says_pid`])
    names: HashMap<&'a str, usize>,
}

/// The rows in the lens's order, the first [`TOP_N`] unless `show_all`.
/// Re-ordered here rather than read: `history::rank` ships CPU-time order,
/// and the memory orders are lenses over the same rows, not a second
/// dataset. Growth is signed: a process that freed memory sinks below the
/// ones that held steady, which is the order the question implies.
pub fn ranked(rows: &[Spender], sort: HistorySort, show_all: bool) -> Ranked<'_> {
    let mut ordered: Vec<&Spender> = rows.iter().collect();
    match sort {
        HistorySort::CpuTime => {}
        HistorySort::PeakMemory => ordered.sort_by_key(|s| Reverse(s.peak_memory_bytes)),
        HistorySort::MemoryGrowth => ordered.sort_by_key(|s| Reverse(s.memory_growth_bytes)),
    }
    // The bar is relative to the period's biggest — an absolute scale
    // would be meaningless (a day has 86 400 core-seconds per core).
    let top = ordered.first().map_or(1, |s| amount(s, sort)).max(1);
    let hidden = rows.len().saturating_sub(TOP_N);
    ordered.truncate(if show_all { rows.len() } else { TOP_N });
    let mut names: HashMap<&str, usize> = HashMap::new();
    for s in &ordered {
        *names.entry(shown_name(s)).or_default() += 1;
    }
    Ranked {
        shown: ordered,
        hidden,
        top,
        names,
    }
}

impl Ranked<'_> {
    /// Whether a row says its pid beside its name: only where another
    /// shown row carries the same name. Elsewhere it was a number beside
    /// every name on a page about whole days, where the pid is history;
    /// between two "Google Chrome Helper" rows it is the one thing that
    /// tells them apart.
    pub fn says_pid(&self, s: &Spender) -> bool {
        self.names.get(shown_name(s)).copied().unwrap_or(0) > 1
    }
}

/// What a row is titled: the record's display name where it carries one
/// (zstats 0.5.3 — "a history that cannot be read against the
/// notifications it explains is a worse history"), the matched name
/// otherwise. The ambiguity pids key on this too: ambiguity is about
/// what is on screen.
pub fn shown_name(s: &Spender) -> &str {
    s.display_name.as_deref().unwrap_or(&s.name)
}

/// What the lens ranks by, as an amount a share meter can divide.
fn amount(s: &Spender, sort: HistorySort) -> u64 {
    match sort {
        HistorySort::CpuTime => s.cpu_time_ms,
        HistorySort::PeakMemory => s.peak_memory_bytes,
        HistorySort::MemoryGrowth => s.memory_growth_bytes.max(0) as u64,
    }
}

/// The wider ranges' share meter: the row against the period's biggest.
pub fn meter_fraction(s: &Spender, sort: HistorySort, top: u64) -> f32 {
    amount(s, sort) as f32 / top as f32
}

/// The row's headline figure under the lens.
pub fn headline(s: &Spender, sort: HistorySort) -> String {
    match sort {
        HistorySort::CpuTime => format::core_time(s.cpu_time_ms),
        HistorySort::PeakMemory => format::memory(s.peak_memory_bytes),
        HistorySort::MemoryGrowth => format::memory_signed(s.memory_growth_bytes),
    }
}

/// The small word after the headline. The headline under CPU time is
/// core-time, which shares a unit *shape* with the wall-clock minutes
/// below — the CPU tag is what keeps "30m" from reading as a duration of
/// day.
pub fn headline_tag(sort: HistorySort) -> String {
    i18n::tr(match sort {
        HistorySort::CpuTime => "alerts.kind_cpu",
        HistorySort::PeakMemory => "history.peak_tag",
        HistorySort::MemoryGrowth => "history.growth_tag",
    })
}

/// The second line's figure: the metric the lens is not already
/// headlining. Under the growth order the peak is the figure that puts
/// the climb in proportion, same as under CPU time.
pub fn caption(s: &Spender, sort: HistorySort) -> String {
    match sort {
        HistorySort::CpuTime | HistorySort::MemoryGrowth => t!(
            "history.mem_only",
            mem = format::memory(s.peak_memory_bytes)
        )
        .to_string(),
        HistorySort::PeakMemory => {
            t!("history.cpu_only", cpu = format::core_time(s.cpu_time_ms)).to_string()
        }
    }
}

/// Peak beside total on purpose: a small peak next to a large total is
/// exactly the process this view exists to find, and the pairing is what
/// makes that legible.
pub fn peak_line(s: &Spender) -> String {
    t!(
        "history.peak",
        cpu = format::pct(s.peak_cpu_percent),
        minutes = s.minutes
    )
    .to_string()
}

/// The shape's word and its tooltip — steady is most rows on most days,
/// and a word every row wears says nothing, so only the exceptions have
/// one.
pub fn shape(s: &Spender) -> Option<(String, String)> {
    let shape = s.shape.filter(|shape| *shape != HistoryShape::Sustained)?;
    let label = i18n::tr(match shape {
        HistoryShape::Spike => "history.shape_spike",
        HistoryShape::Sustained => "history.shape_sustained",
        HistoryShape::Intermittent => "history.shape_intermittent",
    });
    let tip = t!(
        "history.shape_tip",
        peak = format::pct(s.peak_cpu_percent),
        span = format::span(s.span),
        minutes = s.minutes
    )
    .to_string();
    Some((label, tip))
}

/// How many of today's half-hour cells have begun at `now`: only the lived
/// part of the day is drawn. At nine in the morning a fixed 24-hour axis
/// would read as fifteen hours of mysterious quiet, and squeeze the real
/// morning into a quarter of the width.
pub fn lived(now: &jiff::Zoned) -> usize {
    let minutes = now.hour().max(0) as usize * 60 + now.minute().max(0) as usize;
    (minutes / history::BAND_BUCKET_MINUTES + 1).min(history::BAND_BUCKETS)
}

/// `now` as the band's readout writes its live edge.
pub fn now_clock(now: &jiff::Zoned) -> String {
    format!("{:02}:{:02}", now.hour(), now.minute())
}

/// The six-hour marks the lived day has reached, each at its fraction of
/// the axis. A mark too near either end would collide with its label.
pub fn axis_marks(lived: usize) -> Vec<(String, f32)> {
    [6usize, 12, 18]
        .into_iter()
        .filter_map(|hour| {
            let at = (hour * 60 / history::BAND_BUCKET_MINUTES) as f32 / lived as f32;
            (0.15..0.8)
                .contains(&at)
                .then(|| (format!("{hour}:00"), at))
        })
        .collect()
}

/// Each lived cell's ink opacity, `None` where no line was recorded — the
/// track showing through, which is not zero (records are conditional,
/// `history::Band`) and must not be painted as a value.
pub fn band_cells(cells: &Band, lived: usize) -> Vec<Option<f32>> {
    cells
        .iter()
        .take(lived)
        .map(|cell| cell.map(|cell| band_alpha(cell.peak)))
        .collect()
}

/// Four intensity steps for a cell's loudest minute. Quantized rather
/// than continuous so adjacent cells read as "same-ish" or "different"
/// instead of a smear; the breakpoints are a quarter of a core, one
/// core, three cores — the same one-core unit every CPU figure in the
/// app speaks. Display only, like every threshold in `views/`.
fn band_alpha(peak: f32) -> f32 {
    match peak {
        p if p < 25.0 => 0.30,
        p if p < 100.0 => 0.55,
        p if p < 300.0 => 0.80,
        _ => 1.0,
    }
}

/// How many stretches the readout names before folding the rest into a
/// count: enough for a real day's rhythm, short of a tooltip taller
/// than the panel for a process that flickered all day.
const TIP_STRETCHES: usize = 8;

/// Which stretches the readout names when a day has more than
/// [`TIP_STRETCHES`]: the longest, shown in day order. A bursty day is
/// a pile of single cells around a few real stretches, and taking the
/// first N by clock spent the whole budget on the morning's noise while
/// hiding the afternoon's two-hour block — the one the reader hovered
/// to find. Ties keep the earlier stretch, and what "+N" folds away is
/// then never longer than anything shown.
fn tip_stretches(runs: &[Stretch]) -> Vec<Stretch> {
    if runs.len() <= TIP_STRETCHES {
        return runs.to_vec();
    }
    let mut by_length = runs.to_vec();
    by_length.sort_by_key(|run| (Reverse(run.end - run.start), run.start));
    let mut chosen = by_length[..TIP_STRETCHES].to_vec();
    chosen.sort_by_key(|run| run.start);
    chosen
}

/// What hovering a band spells out — the lit cells in clock time, since
/// a ~6px cell can place "around three" but never answer 14:30; empty for
/// a band with nothing lit, which stays a plain strip.
///
/// One line per stretch: "09:00–11:30 · 42 min". Half-hour precision on
/// the clock, because that is the resolution the band actually has — a
/// tooltip more precise than its picture would claim minutes the cells
/// cannot show. The one minute-precise clock figure is the live edge: a
/// run still going ends at the wall clock itself, because its cell's
/// nominal end is in the future and a placeholder word would be the one
/// entry the reader cannot put on a clock.
///
/// The minute count is the half that makes the range mean anything. At
/// this resolution a laptop waking once every thirty minutes lights
/// the same cells as one that never stopped (measured overnight: 25
/// maintenance wakes, every cell from midnight to morning lit, 48
/// recorded minutes out of 440) — "00:30–07:00 · 22 min" says which of
/// the two happened and the range alone never could.
///
/// One line per stretch rather than a separator-joined sentence: three
/// ranges in a row read as one run of digits.
pub fn band_tip(cells: &Band, lived: usize, now_clock: &str) -> Vec<String> {
    let runs = history::stretches(cells, lived);
    let shown = tip_stretches(&runs);
    let mut lines: Vec<String> = shown
        .iter()
        .map(|run| {
            let end = if run.end == lived {
                now_clock.to_string()
            } else {
                bucket_clock(run.end)
            };
            t!(
                "history.band_line",
                start = bucket_clock(run.start),
                end = end,
                minutes = run.minutes
            )
            .to_string()
        })
        .collect();
    if runs.len() > shown.len() {
        lines.push(t!("history.band_more", count = runs.len() - shown.len()).to_string());
    }
    lines
}

/// A bucket boundary as wall clock — bucket 19 is "09:30".
fn bucket_clock(bucket: usize) -> String {
    let minutes = bucket * history::BAND_BUCKET_MINUTES;
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::BandCell;
    use std::time::Duration;

    #[test]
    fn bucket_boundaries_read_as_wall_clock() {
        assert_eq!(bucket_clock(0), "00:00");
        assert_eq!(bucket_clock(19), "09:30");
        assert_eq!(bucket_clock(history::BAND_BUCKETS), "24:00");
    }

    /// A bursty day: a pile of single cells around two real stretches.
    /// The budget goes to the longest, in day order — first-N-by-clock
    /// would have spent it all on the morning's noise and hidden the
    /// afternoon block entirely.
    #[test]
    fn the_longest_stretches_survive_the_cap_in_day_order() {
        let run = |start: usize, end: usize| Stretch {
            start,
            end,
            minutes: (end - start) as u32,
        };
        // Nine singles (00:00 onward, every other cell)…
        let mut runs: Vec<Stretch> = (0..9).map(|i| run(i * 2, i * 2 + 1)).collect();
        // …then a four-cell block in the afternoon.
        runs.push(run(28, 32));
        let shown = tip_stretches(&runs);
        assert_eq!(shown.len(), TIP_STRETCHES);
        assert!(
            shown.contains(&run(28, 32)),
            "the block must not be dropped"
        );
        // Day order kept: the block is last because it is latest.
        assert_eq!(shown.last(), Some(&run(28, 32)));
        // Ties (the singles) keep the earlier ones.
        assert_eq!(shown[0], run(0, 1));
        // At or under the cap, nothing is reordered or dropped.
        let few = vec![run(4, 5), run(10, 12)];
        assert_eq!(tip_stretches(&few), few);
    }

    /// The readout is a list, one line per stretch — and the live edge
    /// takes the wall clock rather than its cell's nominal end.
    #[test]
    fn the_readout_is_one_line_per_stretch() {
        let mut cells: Band = [None; history::BAND_BUCKETS];
        cells[0] = Some(BandCell {
            peak: 50.0,
            minutes: 28,
        });
        cells[2] = Some(BandCell {
            peak: 50.0,
            minutes: 22,
        });
        let lines = band_tip(&cells, 3, "01:22");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("00:00") && lines[0].contains("00:30"));
        assert!(lines[0].contains("28"));
        // Ends at the clock, not at 01:30 — that half hour is not over.
        assert!(lines[1].contains("01:22"), "{}", lines[1]);
        assert!(lines[1].contains("22"));
        assert_eq!(
            band_cells(&cells, 3),
            [Some(0.55), None, Some(0.55)],
            "the unrecorded half hour is the track, not a zero"
        );
    }

    fn spender(pid: u32, name: &str, cpu: u64, peak_mem: u64) -> Spender {
        Spender {
            pid,
            name: name.into(),
            display_name: None,
            cpu_time_ms: cpu,
            peak_cpu_percent: 10.0,
            peak_memory_bytes: peak_mem,
            memory_growth_bytes: 0,
            minutes: 10,
            span: Duration::from_secs(600),
            shape: None,
            band: None,
        }
    }

    #[test]
    fn the_memory_lens_reorders_and_only_repeated_names_say_their_pid() {
        let rows = [
            spender(1, "mds", 9_000, 10),
            spender(2, "Chrome Helper", 5_000, 300),
            spender(3, "Chrome Helper", 1_000, 200),
        ];
        let cpu = ranked(&rows, HistorySort::CpuTime, false);
        assert_eq!(cpu.top, 9_000);
        let mem = ranked(&rows, HistorySort::PeakMemory, false);
        let pids: Vec<u32> = mem.shown.iter().map(|s| s.pid).collect();
        assert_eq!(pids, [2, 3, 1]);
        assert!(mem.says_pid(&rows[1]) && !mem.says_pid(&rows[0]));
        assert_eq!(
            meter_fraction(&rows[2], HistorySort::PeakMemory, mem.top),
            200. / 300.
        );
    }

    #[test]
    fn the_list_names_twelve_until_it_is_expanded() {
        let rows: Vec<Spender> = (0..20)
            .map(|i| spender(i, &format!("p{i}"), 100 - u64::from(i), 0))
            .collect();
        let short = ranked(&rows, HistorySort::CpuTime, false);
        assert_eq!((short.shown.len(), short.hidden), (TOP_N, 8));
        let all = ranked(&rows, HistorySort::CpuTime, true);
        assert_eq!(
            (all.shown.len(), all.hidden),
            (20, 8),
            "the chip stays a toggle"
        );
    }
}
