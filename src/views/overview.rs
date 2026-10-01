//! Overview: processor, top application trees, memory.

use super::processes;
use super::widgets::{self, card};
use crate::font;
use crate::format;
use crate::i18n;
use crate::series::{self, Point};
use crate::state::{MemoryCreep, Tab, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use crate::trend;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Bounds, Hsla, InteractiveElement, IntoElement, ParentElement, PathBuilder, Pixels,
    Rgba, StatefulInteractiveElement, Styled, Window, div, point, px, size,
};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use zstats::snapshot::{
    Capabilities, CpuSnapshot, IoTotalsSnapshot, LoadSnapshot, MemorySnapshot, ProcessGroupSnapshot,
};

/// How many trees the first panel names. Enough to answer "who's
/// hot" without turning Overview into a second Apps tab.
const TOP_N: usize = 5;

/// Per-core bar turns accent past this.
const CORE_HOT: f32 = 85.0;
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
const SWAP_HOT: f32 = 50.0;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(snapshot) = state.latest().map(|t| &t.snapshot) else {
        return vec![widgets::empty_card(
            i18n::tr("common.waiting_sample"),
            i18n::tr("common.waiting_sample_body"),
        )];
    };
    let watts = snapshot
        .battery
        .as_ref()
        .and_then(|b| b.power_watts)
        .and_then(format::whole_watts);
    // One `now` for every line, so a point sits on the same x in each.
    let now = Instant::now();
    let recent = Recent {
        cpu: state.cpu_series(),
        memory: state.memory_series(),
        net_down: state.net_down_series(),
        net_up: state.net_up_series(),
        now,
    };
    vec![
        processor(
            &snapshot.cpu,
            &snapshot.load,
            snapshot.host.uptime_secs,
            watts,
            recent,
        ),
        top_apps(state),
        memory(
            &snapshot.memory,
            &snapshot.io_totals,
            snapshot.capabilities,
            state.memory_climbers(),
            recent,
        ),
    ]
}

/// The half-hour series behind the three charts, stamped once per paint.
#[derive(Clone, Copy)]
struct Recent<'a> {
    cpu: &'a series::Series,
    memory: &'a series::Series,
    net_down: &'a series::Series,
    net_up: &'a series::Series,
    now: Instant,
}

/// A tree's recent minutes must sit this many percent-of-one-core
/// points above its earlier-hour average before the card calls it
/// climbing. Below ~an eighth of a core the delta is scheduler mood and
/// short blips — ranking by it would be ranking noise. Display only,
/// like every threshold in `views/`: a climb fires nothing.
const RISE_FLOOR: f32 = 15.0;

/// Compact "who changed" list — whole trees, ranked by their hour-window
/// climb (`trend.rs`) when anything is climbing, by current CPU when
/// nothing is. The panel usually gets opened because the machine *got*
/// loud, and the instantaneous top cannot answer that: the resident
/// that is always first is normal, the tree that climbed out of nowhere
/// is the reason — and a steady 30% outranks a 2%→21% climber in any
/// snapshot ranking. Always [`TOP_N`] rows: climbers take the top, the
/// rest of the slots keep the current CPU ranking so a quiet climb
/// (two trees) does not leave the card three rows short of the window.
/// All still opens the full Apps list.
fn top_apps(state: &ZStatsAppState) -> AnyElement {
    let Some(tick) = state.latest() else {
        return widgets::empty_card(
            i18n::tr("overview.top_cpu"),
            i18n::tr("common.waiting_sample"),
        );
    };
    let Some(groups) = tick.snapshot.process_groups.as_deref() else {
        return widgets::empty_card(
            i18n::tr("overview.top_cpu_off"),
            i18n::tr("overview.top_cpu_off_body"),
        );
    };
    let processes = tick
        .snapshot
        .processes
        .as_deref()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let topology = state.member_processes().unwrap_or(processes);
    let mut risers: Vec<_> = groups
        .iter()
        .filter_map(|g| {
            state
                .app_rise(trend::tree_key(g))
                .filter(|delta| *delta >= RISE_FLOOR)
                .map(|delta| (g, delta))
        })
        .collect();
    // Two modes, one card: risers when there are any, the current top
    // otherwise — a quiet hour ranked by ±noise deltas would be worse
    // than the ranking it replaced. The header says which question is
    // being answered.
    let (title_key, tip_key, rows): (_, _, Vec<_>) = if risers.is_empty() {
        (
            "overview.top_cpu",
            "overview.top_apps_tip",
            pad_rising(Vec::new(), groups, TOP_N),
        )
    } else {
        risers.sort_by(|a, b| b.1.total_cmp(&a.1));
        (
            "overview.rising_title",
            "overview.rising_tip",
            pad_rising(risers, groups, TOP_N),
        )
    };
    let n = rows.len();

    widgets::list_shell()
        .child(widgets::list_header(
            h_flex()
                .items_center()
                .gap(px(4.))
                .min_w_0()
                .child(div().min_w_0().truncate().child(i18n::tr(title_key)))
                .child(widgets::info_icon("overview-top-apps", i18n::tr(tip_key))),
            Some(top_apps_all()),
        ))
        .children(rows.into_iter().enumerate().map(|(i, (g, delta))| {
            // Burst only: a tree has no sustained flag of its own, and
            // walking members here would make Overview a second Apps tab.
            let hot = processes::is_hot(f64::from(g.cpu_usage_percent), false);
            let root_pid = g.root_pid;
            h_flex()
                .id(("top-app-row", root_pid as usize))
                .items_center()
                .justify_between()
                .gap(px(8.))
                .px(px(13.))
                .py(px(5.))
                .when(i + 1 < n, |d| {
                    d.border_b(px(1.)).border_color(theme::border_subtle())
                })
                // The row answers "who" — the Apps tab answers the next
                // question, so a click lands there with this tree open
                // and scrolled into view. Hover fill is the affordance,
                // same as the Apps rows the click leads to.
                .hover(|d| d.bg(theme::surface_raised()))
                .on_click(move |_, _window, cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.reveal_app(root_pid, cx));
                })
                .child({
                    // Face, not [`trend::tree_key`]: a login compile
                    // shows as cargo, a compile in Zed's terminal as
                    // `Zed · cargo`. The rise is still looked up on the
                    // launchd child so the hour does not split.
                    let face = trend::tree_face(g, topology, processes, state.member_pgids());
                    widgets::truncating_name_tailed(
                        ("top-app-name", g.root_pid as usize),
                        face.title,
                        face.job.map(gpui::SharedString::from),
                        12.,
                        gpui::FontWeight::MEDIUM,
                        Hsla::from(theme::text()),
                    )
                })
                // Climb + current as one right-hand cluster. Three
                // `justify_between` children parked the rise in the
                // leftover middle, so a short name left a hole and a
                // long one shoved the arrow into the percent. Muted
                // on purpose: a rise is news, not a threshold, and
                // accent stays reserved for over-the-line (`theme.rs`).
                .child(
                    h_flex()
                        .flex_none()
                        .items_baseline()
                        .gap(px(6.))
                        .children(delta.map(|delta| {
                            div()
                                .id(("top-app-rise", g.root_pid as usize))
                                .flex_none()
                                .font_family(font::MONO)
                                .text_size(px(10.))
                                .text_color(theme::text_muted())
                                .tooltip(widgets::wrap_tooltip(
                                    t!("overview.rise_row_tip", delta = format::pct(delta))
                                        .to_string(),
                                ))
                                .child(format!("↑{}", format::pct(delta)))
                        }))
                        .child(
                            div()
                                .id(("top-app-pct", g.root_pid as usize))
                                .flex_none()
                                .font_family(font::MONO)
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(theme::text_for(hot))
                                .tooltip(widgets::wrap_tooltip(i18n::tr(
                                    "overview.top_app_pct_tip",
                                )))
                                .child(format::pct_col(g.cpu_usage_percent)),
                        ),
                )
        }))
        .into_any_element()
}

/// Climbers first (with their delta), then current CPU to fill `n`.
/// A two-tree climb used to leave the card three rows short of the
/// window it was sized for.
fn pad_rising<'a>(
    risers: Vec<(&'a ProcessGroupSnapshot, f32)>,
    groups: &'a [ProcessGroupSnapshot],
    n: usize,
) -> Vec<(&'a ProcessGroupSnapshot, Option<f32>)> {
    let mut rows: Vec<_> = risers
        .into_iter()
        .take(n)
        .map(|(g, d)| (g, Some(d)))
        .collect();
    if rows.len() >= n {
        return rows;
    }
    let taken: HashSet<&str> = rows.iter().map(|(g, _)| trend::tree_key(g)).collect();
    let mut rest: Vec<_> = groups
        .iter()
        .filter(|g| !taken.contains(trend::tree_key(g)))
        .collect();
    rest.sort_by(|a, b| b.cpu_usage_percent.total_cmp(&a.cpu_usage_percent));
    rows.extend(rest.into_iter().take(n - rows.len()).map(|g| (g, None)));
    rows
}

fn top_apps_all() -> AnyElement {
    let tip = Tab::Apps.title();
    h_flex()
        .id("top-apps-all")
        .items_center()
        .gap(px(1.))
        .rounded(px(5.))
        .px(px(6.))
        .py(px(2.))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .hover(|d| d.bg(theme::chip()))
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.set_tab(Tab::Apps, cx));
        })
        .child(
            div()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::text_dim())
                .child(i18n::tr("overview.top_cpu_all")),
        )
        .child(
            Icon::new(IconName::ChevronRight)
                .with_size(Size::Size(px(12.)))
                .text_color(Hsla::from(theme::text_dim())),
        )
        .into_any_element()
}

fn processor(
    cpu: &CpuSnapshot,
    load: &LoadSnapshot,
    uptime_secs: u64,
    watts: Option<u32>,
    recent: Recent<'_>,
) -> AnyElement {
    let header_right = processor_caption(cpu, watts);
    let mut body = card()
        .child(widgets::card_header(
            i18n::tr("overview.processor"),
            Some(header_right),
        ))
        .child(
            // Baseline-aligned so the footnote sits on the headline's
            // line, not on the bottom of its taller box. Usage + load
            // stay a left-hand pair; uptime hugs the right so the
            // three-number load caption has room to breathe.
            h_flex()
                .w_full()
                .items_baseline()
                .justify_between()
                .gap(px(10.))
                .mt(px(4.))
                .child(
                    h_flex()
                        .items_baseline()
                        .gap(px(10.))
                        .min_w_0()
                        .child(
                            div()
                                .id("cpu-usage")
                                .tooltip(widgets::wrap_tooltip(i18n::tr("overview.usage_tip")))
                                .child(widgets::big_number(
                                    format::whole_pct(cpu.usage_percent),
                                    "%",
                                    20.,
                                )),
                        )
                        .child(load_caption(load, cpu.logical_cores)),
                )
                .child(uptime_caption(uptime_secs)),
        )
        .children(recent_line(
            Chart {
                id: "cpu-curve",
                lines: vec![ChartLine {
                    series: recent.cpu,
                    arrow: None,
                }],
                scale: cpu_axis_top(&chart_buckets(recent.cpu.points(), recent.now)),
                tip: i18n::tr("overview.cpu_curve_tip"),
                // Each point is a 20s average. The cubic follows those and
                // does not rise above one that was the peak.
                stroke: CurveStroke::Smooth,
                unit: ChartUnit::Percent,
                icon: None,
            },
            recent.now,
        ));

    // Apple Silicon and friends: usage split by performance cluster.
    if let Some(levels) = cpu.perf_levels.as_ref().filter(|l| l.len() > 1) {
        body = body.child(
            v_flex()
                .pt(px(8.))
                .gap(px(6.))
                .children(levels.iter().map(|level| {
                    let over = level.usage_percent > CORE_HOT;
                    h_flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .w(px(96.))
                                .flex_none()
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::text_muted())
                                .truncate()
                                .child(cluster_label(&level.name, level.logical_cores)),
                        )
                        .child(div().flex_1().child(widgets::meter(
                            level.usage_percent / 100.0,
                            Hsla::from(theme::fill_for(over)),
                            5.,
                        )))
                        .child(
                            div()
                                .w(px(44.))
                                .flex_none()
                                .font_family(font::MONO)
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight::NORMAL)
                                .text_color(theme::text())
                                .child(format::pct_col(level.usage_percent)),
                        )
                })),
        );
    }

    body.into_any_element()
}

/// Brand plus the reported clock, e.g. "Apple M4 Pro (4.5 GHz)", and
/// the live draw when the battery reports one.
///
/// Apple Silicon does not expose live per-cluster MHz through sysinfo;
/// this is the rated clock and usually never moves. zstats still only
/// *asks* for it every 30s (cheaper than every usage sample) — that is
/// a collect cadence, not a claim that the number changes.
///
/// Watts sit here rather than beside CPU%: identity (which chip, what
/// it is drawing) vs activity (how busy, how long). The brand is the
/// one that ellipsises — watts is `flex_none`, and the text has to live
/// on the truncating div itself (`note()` wrapping it once shoved the
/// figure off the card).
fn processor_caption(cpu: &CpuSnapshot, watts: Option<u32>) -> AnyElement {
    let freq = cpu
        .frequency_mhz
        .map(|mhz| format!("{:.1} GHz", mhz as f64 / 1000.0));
    let text = match (cpu.brand.as_deref(), freq.as_deref()) {
        (Some(brand), Some(freq)) => {
            t!("overview.brand_freq", brand = brand, freq = freq).to_string()
        }
        (Some(brand), None) => brand.to_string(),
        (None, Some(freq)) => freq.to_string(),
        (None, None) => i18n::tr("overview.freq_unknown"),
    };
    let tip = i18n::tr("overview.freq_tip");
    h_flex()
        .min_w_0()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .id("cpu-brand")
                .min_w_0()
                .truncate()
                .text_size(px(10.))
                .text_color(theme::text_dim())
                .tooltip(widgets::wrap_tooltip(tip))
                .child(text),
        )
        .children(watts.map(watts_caption))
        .into_any_element()
}

/// The 1 / 5 / 15-minute load averages beside the usage figure. Usage
/// says how busy the cores are; load says how much work is *waiting*
/// for one — a machine pinned at 100% with load 3 and one at 100% with
/// load 30 are two different conditions, and only the count tells them
/// apart. Footnote-sized on purpose: it qualifies the headline, it does
/// not compete with it. The tooltip carries the core count so the
/// reader has the line to compare against. Display only — zstats has
/// no load rule, and this adds none.
fn load_caption(load: &LoadSnapshot, cores: u32) -> AnyElement {
    let text = t!(
        "overview.load",
        one = format::load(load.load1),
        five = format::load(load.load5),
        fifteen = format::load(load.load15)
    )
    .to_string();
    let tip = t!("overview.load_tip", cores = cores).to_string();
    div()
        .id("cpu-load")
        .font_family(font::MONO)
        .text_size(px(10.))
        .text_color(theme::text_dim())
        .tooltip(widgets::wrap_tooltip(tip))
        .child(text)
        .into_any_element()
}

/// Live draw, beside the chip name. Whole watts from [`format::whole_watts`];
/// the caller already dropped zero. Absent on desktops and VMs.
fn watts_caption(watts: u32) -> AnyElement {
    div()
        .id("cpu-watts")
        .flex_none()
        .font_family(font::MONO)
        .text_size(px(10.))
        .text_color(theme::text_dim())
        .tooltip(widgets::wrap_tooltip(i18n::tr("overview.watts_tip")))
        .child(format!("{watts} W"))
        .into_any_element()
}

/// Time since boot. Always present — even a just-booted machine has a
/// number — and it is the sentence Overview was missing: how long this
/// load has had to build.
fn uptime_caption(secs: u64) -> AnyElement {
    div()
        .id("cpu-uptime")
        .flex_none()
        .font_family(font::MONO)
        .text_size(px(10.))
        .text_color(theme::text_dim())
        .tooltip(widgets::wrap_tooltip(i18n::tr("overview.uptime_tip")))
        .child(t!("overview.uptime", time = format::uptime_short(secs)).to_string())
        .into_any_element()
}

/// Activity Monitor / Stats wording: "P-cores · 8", not "P·8".
fn cluster_label(name: &str, cores: u32) -> String {
    let pretty = match name {
        "P" | "p" | "Performance" | "performance" => i18n::tr("overview.p_cores"),
        "E" | "e" | "Efficiency" | "efficiency" => i18n::tr("overview.e_cores"),
        other => other.to_string(),
    };
    format!("{pretty} · {cores}")
}

/// How many climbers the strip names. Three fit one line at 320px
/// beside the lead; a fourth would be a second line for a glance.
const MEM_RISE_N: usize = 3;

/// One muted line under the memory figures: who has been climbing this
/// hour and is still up — `Chrome +1.2 GB · Code +420 MB`. The hour's
/// answer to the question the History tab asks of the day, and the
/// one the memory rules cannot ask at all (a climb crosses no line
/// until it is too late). Absent when nothing is climbing, so a quiet
/// hour costs the card no height. `None`, not an empty strip.
fn mem_climb_strip(climbers: &[MemoryCreep], total_bytes: u64) -> Option<AnyElement> {
    let floor = trend::mem_rise_floor(total_bytes);
    let named: Vec<String> = climbers
        .iter()
        .filter(|c| c.climb_bytes >= floor)
        .take(MEM_RISE_N)
        .map(|c| {
            t!(
                "overview.mem_rise_item",
                name = c.name.clone(),
                delta = format::memory(c.climb_bytes)
            )
            .to_string()
        })
        .collect();
    if named.is_empty() {
        return None;
    }
    Some(
        h_flex()
            .id("mem-climbers")
            .items_baseline()
            .gap(px(6.))
            .mt(px(8.))
            .min_w_0()
            .text_size(px(10.))
            .tooltip(widgets::wrap_tooltip(i18n::tr("overview.mem_rise_tip")))
            .child(
                div()
                    .flex_none()
                    .text_color(theme::text_dim())
                    .child(i18n::tr("overview.mem_rise_lead")),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(font::MONO)
                    .text_color(theme::text_muted())
                    .child(named.join(" · ")),
            )
            .into_any_element(),
    )
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
pub(crate) fn swap_moving(ins: Option<u64>, outs: Option<u64>) -> Option<(u64, u64)> {
    match (outs, ins) {
        (Some(outs), Some(ins)) if outs > 0 || ins > 0 => Some((outs, ins)),
        _ => None,
    }
}

/// One line under Swap / Compressed while the compressor is swapping:
/// `Swapping  out 120/s · in 40/s`. The badge says memory is tight; this
/// says how hard macOS is working to stay there — a warning with nothing
/// moving is a machine that has settled, one at hundreds a second is
/// still fighting. Absent otherwise, like the climb strip below it, so a
/// quiet machine pays no height.
///
/// Segments are the kernel's unit — bundles of compressed pages, no fixed
/// size — so they are shown as activity and never converted to bytes.
/// `swap_thrashing` is the kernel's own detector (segments written out
/// only to be read straight back): a verdict, so it recolours the lead
/// in accent and is never compared to anything here.
fn swap_activity_strip(mem: &MemorySnapshot) -> Option<AnyElement> {
    let thrashing = mem.swap_thrashing == Some(true);
    let moving = swap_moving(mem.swap_ins_per_sec, mem.swap_outs_per_sec);
    if !thrashing && moving.is_none() {
        return None;
    }
    let (outs, ins) = moving.unwrap_or((0, 0));
    let (lead, lead_color) = if thrashing {
        (i18n::tr("overview.swap_thrashing"), theme::accent())
    } else {
        (i18n::tr("overview.swap_activity_lead"), theme::text_dim())
    };
    Some(
        h_flex()
            .id("mem-swapping")
            .items_baseline()
            .gap(px(6.))
            .mt(px(8.))
            .min_w_0()
            .text_size(px(10.))
            .tooltip(widgets::wrap_tooltip(i18n::tr(
                "overview.swap_activity_tip",
            )))
            .child(div().flex_none().text_color(lead_color).child(lead))
            .child(
                font::mono_unless_cjk(div())
                    .min_w_0()
                    .truncate()
                    .text_color(theme::text_muted())
                    .child(
                        t!(
                            "overview.swap_activity",
                            outs = format::thousands(outs as usize),
                            ins = format::thousands(ins as usize)
                        )
                        .to_string(),
                    ),
            )
            .into_any_element(),
    )
}

/// `caps` decides how an absent figure reads: this build cannot measure
/// it, or it can and has not yet. On macOS every capability is true, so
/// every branch here resolves exactly as it did before 0.5.2.
fn memory(
    mem: &MemorySnapshot,
    io: &IoTotalsSnapshot,
    caps: Capabilities,
    climbers: Vec<MemoryCreep>,
    recent: Recent<'_>,
) -> AnyElement {
    // The kernel's own verdict, not a number we derive: 1 normal, 2 warning,
    // 4 critical. Absent has two readings and they are not the same
    // sentence: this build cannot measure pressure at all, or it can and
    // has nothing yet. zstats 0.5.2 answers the first through
    // `capabilities`, so the panel stops inferring it from a `None` —
    // on macOS the capability is true and every arm below reads exactly
    // as it did.
    let supported = caps.memory_pressure;
    let (label, tip, fg, bg, line) = match mem.pressure_level {
        Some(l) if l >= 4 => (
            i18n::tr("overview.pressure_critical"),
            i18n::tr("overview.pressure_tip_critical"),
            gpui::white(),
            Hsla::from(theme::accent()),
            Hsla::from(theme::accent()),
        ),
        Some(l) if l >= 2 => (
            i18n::tr("overview.pressure_warning"),
            i18n::tr("overview.pressure_tip_warning"),
            Hsla::from(theme::accent_light()),
            Hsla::from(theme::accent_wash(14)),
            Hsla::from(theme::accent_wash(40)),
        ),
        Some(_) => (
            i18n::tr("overview.pressure_normal"),
            i18n::tr("overview.pressure_tip_normal"),
            Hsla::from(theme::text_muted()),
            Hsla::from(theme::inset()),
            Hsla::from(theme::border()),
        ),
        None if !supported => (
            i18n::tr("overview.pressure_none"),
            i18n::tr("overview.pressure_tip_none"),
            Hsla::from(theme::text_muted()),
            Hsla::from(theme::inset()),
            Hsla::from(theme::border()),
        ),
        // Measurable here, just not measured yet — the same `—` every
        // other waiting figure shows, not a claim about the platform.
        None => (
            format::PLACEHOLDER.to_string(),
            i18n::tr("overview.pressure_tip_waiting"),
            Hsla::from(theme::text_muted()),
            Hsla::from(theme::inset()),
            Hsla::from(theme::border()),
        ),
    };
    // The figure the verdict is derived from (`kern.memorystatus_level`),
    // printed in the badge beside the verdict — "Normal · 62% available" —
    // not as a row next to Used, where a lower "available" would read as a
    // contradiction rather than as the kernel's narrower definition. The
    // word stays in the badge: a bare "62%" beside a memory card reads as
    // used. The tooltip says why it differs from free GB. macOS only.
    let (label, tip) = match badge_available(mem.pressure_level, mem.kernel_available_percent) {
        Some(pct) => (
            t!("overview.pressure_badge", level = label, pct = pct).to_string(),
            format!("{tip} {}", t!("overview.pressure_tip_kernel", pct = pct)),
        ),
        None => (label, tip),
    };

    let total = mem.total_bytes.max(1) as f32;
    let compressed = mem.compressed_bytes.unwrap_or(0).min(mem.used_bytes);
    let resident = mem.used_bytes.saturating_sub(compressed);
    let resident_w = resident as f32 / total;
    let comp_w = compressed as f32 / total;
    // Used and compressed are painted slices. Free is the unfilled
    // trough — leftover, not a third colour that has to fight the badge.
    let used_fill = Hsla::from(theme::ink());
    let compressed_fill = Hsla::from(theme::text_muted());

    // Total is already in the hero caption ("used of 24 GB"). The bar
    // shows compressed as a slice but not how many GB that is — and
    // that number is the early pressure signal. Swap is the other
    // fact the hero and the bar both omit.
    let compressed_label = match mem.compressed_bytes {
        Some(b) => format::gb(b),
        None if !supported => i18n::tr("common.n_a"),
        None => format::PLACEHOLDER.to_string(),
    };
    // Deliberately not `mem.swap_used_percent`: that field *is* swap
    // against its own allocation, the ratio [`SWAP_HOT`] explains is
    // unusable here. Dividing those two bytes locally would also have
    // been a second copy of a figure zstats already reports, which
    // CLAUDE.md forbids. This asks something zstats does not answer, and
    // only ever to pick a colour.
    //
    // The `> 0` is not division safety — `total` is already floored at 1
    // — but a guard against a collector reporting no memory at all, where
    // that floor would turn any swap into a huge percentage and paint red.
    let swap_hot = mem.total_bytes > 0 && (mem.swap_used_bytes as f32 / total) * 100.0 >= SWAP_HOT;
    let rows = vec![
        (
            i18n::tr("overview.swap"),
            format!(
                "{} / {}",
                format::gb(mem.swap_used_bytes),
                format::gb(mem.swap_total_bytes)
            ),
            swap_hot,
        ),
        (i18n::tr("overview.compressed"), compressed_label, false),
    ];

    let mut legend = vec![(
        widgets::LegendMark::Fill(used_fill),
        i18n::tr("overview.used").into(),
        i18n::tr("overview.used_tip").into(),
    )];
    if compressed > 0 {
        legend.push((
            widgets::LegendMark::Fill(compressed_fill),
            i18n::tr("overview.compressed").into(),
            i18n::tr("overview.compressed_tip").into(),
        ));
    }
    legend.push((
        widgets::LegendMark::Hollow,
        i18n::tr("overview.free").into(),
        i18n::tr("overview.free_tip").into(),
    ));

    card()
        .child(widgets::card_header(
            i18n::tr("overview.memory"),
            Some(
                div()
                    .id("mem-pressure")
                    .flex_none()
                    .rounded_full()
                    .border_1()
                    .border_color(line)
                    .bg(bg)
                    .px(px(8.))
                    .py(px(2.))
                    .text_size(px(10.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .tooltip(widgets::wrap_tooltip(tip))
                    .child(label)
                    .into_any_element(),
            ),
        ))
        .child(
            h_flex()
                .items_baseline()
                .gap(px(6.))
                .mt(px(8.))
                .child(
                    div()
                        .font_family(font::MONO)
                        .text_size(px(20.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::text())
                        .child(format::gb(mem.used_bytes)),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::text_muted())
                        .child(
                            t!("overview.used_of", total = format::gb(mem.total_bytes)).to_string(),
                        ),
                ),
        )
        .children(recent_line(
            Chart {
                id: "mem-curve",
                lines: vec![ChartLine {
                    series: recent.memory,
                    arrow: None,
                }],
                scale: MEM_AXIS_TOP,
                tip: i18n::tr("overview.mem_curve_tip"),
                stroke: CurveStroke::Bars,
                unit: ChartUnit::Percent,
                icon: None,
            },
            recent.now,
        ))
        .child(div().mt(px(10.)).child(widgets::stacked_meter(
            vec![(resident_w, used_fill), (comp_w, compressed_fill)],
            6.,
        )))
        .child(div().mt(px(8.)).child(widgets::legend(legend)))
        .child(widgets::kv_packed(rows))
        .children(swap_activity_strip(mem))
        .children(mem_climb_strip(&climbers, mem.total_bytes))
        .child(io_strip(io))
        .children(recent_line(
            Chart {
                id: "net-curve",
                // Download leads: it is the line most people read. Upload
                // stays on the chart, dimmer, because a backup or a sync
                // pushing out is just as often why the network is busy.
                lines: vec![
                    ChartLine {
                        series: recent.net_down,
                        arrow: Some("↓"),
                    },
                    ChartLine {
                        series: recent.net_up,
                        arrow: Some("↑"),
                    },
                ],
                // One ceiling for both, so the two heights compare.
                scale: net_scale(
                    &[
                        chart_buckets(recent.net_down.points(), recent.now),
                        chart_buckets(recent.net_up.points(), recent.now),
                    ]
                    .concat(),
                ),
                tip: i18n::tr("overview.net_curve_tip"),
                // Same cubic as CPU. A rate that holds between the 15s
                // refreshes has a flat tangent, so the hold stays a hold;
                // the bend is only where two readings differ.
                stroke: CurveStroke::Smooth,
                unit: ChartUnit::Rate,
                // The row above carries disk rates too, with the same
                // arrows; the glyph says which pair this chart is.
                icon: Some(IconName::Network),
            },
            recent.now,
        ))
        .into_any_element()
}

/// Height of an Overview chart. CPU and network are curves; memory is
/// bars. Three of them, each with [`CURVE_TOP`] above and a
/// [`CHART_CAPTION`] under it, are what `placement::DEFAULT_WINDOW_SIZE`
/// grew by so this tab still ends a few pixels above the footer. The
/// well stays this tall in both themes: a ceiling change moves the
/// mark inside the slot and does not resize the card.
const CURVE_H: f32 = 40.;

/// Air above a curve. Part of the window-height budget, with [`CURVE_H`].
const CURVE_TOP: f32 = 4.;

/// Air between a chart and the caption under it. Same budget.
const CHART_CAPTION_GAP: f32 = 4.;

/// The caption itself: peak, latest sample, and collected span, in one
/// line. 10px type in 14px, so the line does not clip. Three charts
/// add this plus [`CHART_CAPTION_GAP`] to the panel height.
const CHART_CAPTION: f32 = 14.;

/// Stroke. [`CHART_PAD`] is wider than half of this, so a peak on the
/// top of the drawing is not cut by the well.
const CURVE_STROKE: f32 = 1.5;

/// Radius of the chart well. Nested inside the card's 12px corner, and
/// the same in both themes: only the fill changes with the theme.
const CHART_WELL_RADIUS: f32 = 8.;

/// How far the marks sit inside the well. The row stays [`CURVE_H`];
/// this only keeps a bar or a peak off the rounded edge.
const CHART_PAD: f32 = 4.;

/// How one run of samples is drawn.
#[derive(Clone, Copy)]
enum CurveStroke {
    /// Monotone cubic through the samples ([`smooth_cubics`]). CPU and
    /// the network rates are a new point every tick, and a polyline of
    /// those is a run of corners. The cubic bends between readings that
    /// differ and does not rise above a sample that was the peak. A
    /// network total that holds still has a flat tangent, so the hold
    /// stays a hold.
    Smooth,
    /// One bar per reading, from the axis up to the sample. Memory's
    /// percent moves slowly; a line through it reads as flat, and the
    /// bars keep each reading's share of the 40px slot.
    Bars,
}

/// CPU axis while every reading in the window stays at or under this.
/// A machine at 8% drawn on 0–100 is a stripe on the floor; 0–30 lets
/// that range use the slot. 12% then 14% stays on this top, so the
/// line does not rescale while the machine is in the quiet band.
const CPU_AXIS_LOW: f64 = 30.0;

/// Memory axis. The series is `used_percent`, so the top is the whole
/// machine. It does not zoom to the recent band — a line that grew
/// because the percent wobbled would read as the machine filling up.
const MEM_AXIS_TOP: f64 = 100.0;

/// Network ceiling while every point stays at or under this — the
/// [`CPU_AXIS_LOW`] of the network line. It was 64 KiB/s (the Network
/// tab's per-program floor), and everyday traffic swinging between
/// 100 and 300 kB/s in the first minutes doubled it 128 → 256 → 512 KiB
/// with each new high, so the line rescaled inside the well while the
/// window was still filling. 1 MB/s holds still across ordinary use;
/// past it the doubling resumes (2, 4, 8 MB/s), and a few kB/s of
/// background traffic stays on the floor. Not shared with the program
/// curves on purpose: those draw each program's own shape, this is the
/// whole machine.
const NET_SCALE_FLOOR: f64 = 1024.0 * 1024.0;

/// How many points a curve draws. The ring keeps every tick; this is
/// only the display. 30 minutes / 90 is 20s.
const CHART_BUCKETS: usize = 90;

/// How many memory bars the chart draws. Same 30-minute axis as the
/// curves, coarser so a bar stays a bar: 30 minutes / 60 is 30s.
const BAR_BUCKETS: usize = 60;

/// Share of each of the [`BAR_BUCKETS`] slots left as the gap between
/// bars. The rest is the bar. A quarter keeps a gap on the minimum
/// window and stays a hairline on the default panel.
const BAR_GAP_SHARE: f32 = 0.25;

// One slice is wider than the raw missed-sample gap, so an empty slice
// is a break and not a tick that merely arrived late. Both grids.
const _: () = assert!(series::WINDOW.as_secs() / CHART_BUCKETS as u64 > series::GAP.as_secs());
const _: () = assert!(series::WINDOW.as_secs() / BAR_BUCKETS as u64 > series::GAP.as_secs());

/// One display slice. [`series::WINDOW`] divides evenly by both grids.
fn chart_slice(buckets: usize) -> Duration {
    series::WINDOW / buckets as u32
}

/// At most [`CHART_BUCKETS`] points, oldest first. CPU and network.
fn chart_buckets(points: &[Point], now: Instant) -> Vec<Point> {
    bucketed(points, now, CHART_BUCKETS)
}

/// At most [`BAR_BUCKETS`] bars, oldest first. Memory.
fn bar_buckets(points: &[Point], now: Instant) -> Vec<Point> {
    bucketed(points, now, BAR_BUCKETS)
}

/// Averages `points` into `buckets` equal slices of [`series::WINDOW`].
///
/// A slice before the first sample is left out, so the left of the
/// 30-minute axis stays empty and the slice does not dilute the first
/// average. A slice with no reading between two that have one becomes
/// a `None`, so the line breaks there and a bar slot stays empty. A
/// `None` beside a reading in the same slice is skipped; it is not
/// counted as zero.
///
/// Averaged from the first sample, not only once the ring outgrows the
/// grid. At the 2s open cadence 90 raw samples cover three minutes, the
/// newest tenth of the axis — about 30px, a third of a pixel apiece, so
/// raw points there are noise rather than detail — and switching to
/// averages at the 91st would drop the peaks in one frame and rescale
/// the CPU top and the network ceiling with them.
fn bucketed(points: &[Point], now: Instant, buckets: usize) -> Vec<Point> {
    if buckets == 0 {
        return Vec::new();
    }
    let bucket = chart_slice(buckets);
    let secs = bucket.as_secs();
    if secs == 0 {
        return Vec::new();
    }
    struct Acc {
        sum: f64,
        n: u32,
        saw_none: bool,
    }
    let mut acc = Vec::with_capacity(buckets);
    for _ in 0..buckets {
        acc.push(Acc {
            sum: 0.0,
            n: 0,
            saw_none: false,
        });
    }
    for point in points {
        let age = now.saturating_duration_since(point.at);
        if age >= series::WINDOW {
            continue;
        }
        let mut index = (age.as_secs() / secs) as usize;
        if index >= buckets {
            index = buckets - 1;
        }
        match point.value {
            Some(value) => {
                acc[index].sum += value;
                acc[index].n += 1;
            }
            None => acc[index].saw_none = true,
        }
    }
    let Some(oldest) = acc.iter().rposition(|slot| slot.n > 0 || slot.saw_none) else {
        return Vec::new();
    };
    let Some(newest) = acc.iter().position(|slot| slot.n > 0 || slot.saw_none) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for index in (newest..=oldest).rev() {
        let age = bucket * index as u32 + bucket / 2;
        let at = now.checked_sub(age).unwrap_or(now);
        let value = if acc[index].n > 0 {
            Some(acc[index].sum / f64::from(acc[index].n))
        } else {
            None
        };
        out.push(Point { at, value });
    }
    out
}

/// Runs the chart stroke can draw. A `None` bucket is the only break:
/// neighbouring buckets are one slice apart (20s on a curve, 30s on
/// the bars), and that step is the grid, not a missed sample. One
/// occupied bucket is still a reading.
fn chart_runs(points: &[Point]) -> Vec<Vec<(Instant, f64)>> {
    let mut out = Vec::new();
    let mut run = Vec::new();
    for point in points {
        match point.value {
            Some(value) => run.push((point.at, value)),
            None => {
                if !run.is_empty() {
                    out.push(run);
                    run = Vec::new();
                }
            }
        }
    }
    if !run.is_empty() {
        out.push(run);
    }
    out
}

/// 30 while every reading stays at or under 30. Once any reading goes
/// over that, the top is the window's own highest reading, so a 40%
/// peak fills the slot instead of sitting low on a 0–100 axis. Callers
/// pass [`chart_buckets`], so the top matches the points on screen.
fn cpu_axis_top(points: &[Point]) -> f64 {
    let peak = points
        .iter()
        .filter_map(|point| point.value)
        .fold(0.0, f64::max);
    peak.max(CPU_AXIS_LOW)
}

/// Where `age` sits on the 30-minute axis. `1` is now, the right edge.
/// `0` is [`series::WINDOW`] ago, the left edge. A short history stays
/// on the right. The time before the first sample is an empty stretch
/// of this axis, not a zero line.
fn axis_t(age: Duration) -> f64 {
    let window = series::WINDOW.as_secs_f64();
    if window <= 0.0 {
        return 1.0;
    }
    (1.0 - age.as_secs_f64() / window).clamp(0.0, 1.0)
}

/// The row's own peak, but never under the floor, and only on a
/// doubling. The exact peak would move the ceiling on every new high
/// and the whole line would jump inside the slot.
fn net_scale(points: &[Point]) -> f64 {
    let peak = points
        .iter()
        .filter_map(|point| point.value)
        .fold(NET_SCALE_FLOOR, f64::max);
    let mut step = NET_SCALE_FLOOR;
    // 64 KiB/s doubled forty times is 64 TiB/s. Past that, keep the
    // last step rather than loop.
    for _ in 0..40 {
        if step >= peak {
            return step;
        }
        step *= 2.0;
    }
    step
}

/// What a chart's samples are, so the readout formats them the way the
/// rest of the card does.
#[derive(Clone, Copy)]
enum ChartUnit {
    /// CPU and memory, `0–100`.
    Percent,
    /// Network download or upload, bytes per second.
    Rate,
}

/// Highest raw reading in the window. The drawn mark is a 20s average,
/// so it can stay under this. There is no "now" beside it: the latest
/// reading is the card's own headline (CPU %, memory, the rates row),
/// and repeating it under the chart was the same number twice.
fn chart_peak(points: &[Point]) -> Option<f64> {
    points
        .iter()
        .filter_map(|point| point.value)
        .reduce(f64::max)
}

fn chart_value(unit: ChartUnit, value: Option<f64>) -> String {
    let Some(value) = value else {
        return format::PLACEHOLDER.to_string();
    };
    match unit {
        ChartUnit::Percent => format::pct(value as f32),
        ChartUnit::Rate => format::rate(Some(value.max(0.0).round() as u64)),
    }
}

/// One Overview chart: what it draws and how it reads.
struct Chart<'a> {
    id: &'static str,
    /// The lead series first, in the full [`theme::ink`]. A second one
    /// (network upload) is drawn behind it in [`dim_ink`], on the same
    /// ceiling.
    lines: Vec<ChartLine<'a>>,
    scale: f64,
    tip: String,
    stroke: CurveStroke,
    unit: ChartUnit,
    /// Glyph at the head of the caption, for a chart whose card does
    /// not already say what it is.
    icon: Option<IconName>,
}

/// One series on a [`Chart`].
#[derive(Clone, Copy)]
struct ChartLine<'a> {
    series: &'a series::Series,
    /// Arrow before this line's peak in the caption, when a chart has
    /// two lines and the caption has to say which is which.
    arrow: Option<&'static str>,
}

/// One chart, once a ring holds a point. The slot is reserved from
/// that first point, so the card does not jump when the second sample
/// turns it into a mark. Under a second the time reads — and nothing
/// is stroked. The tooltip carries the scale and the tray cadence; the
/// canvas itself is not the hover target.
fn recent_line(chart: Chart, now: Instant) -> Option<AnyElement> {
    if chart
        .lines
        .iter()
        .all(|line| line.series.points().is_empty())
    {
        return None;
    }
    let span = chart
        .lines
        .iter()
        .filter_map(|line| line.series.span(now))
        .max();
    let marks: Vec<Vec<Point>> = chart
        .lines
        .iter()
        .map(|line| match chart.stroke {
            CurveStroke::Bars => bar_buckets(line.series.points(), now),
            CurveStroke::Smooth => chart_buckets(line.series.points(), now),
        })
        .collect();
    let peaks: Vec<(Option<&'static str>, Option<f64>)> = chart
        .lines
        .iter()
        .map(|line| (line.arrow, chart_peak(line.series.points())))
        .collect();
    Some(
        // The well stays [`CURVE_H`]. A ceiling change (CPU crossing
        // 30, a network step) moves the mark inside it. The caption
        // under the well is a fixed line, so the card does not jump.
        div()
            .id(chart.id)
            .mt(px(CURVE_TOP))
            .flex_none()
            .tooltip(widgets::wrap_tooltip(chart.tip))
            .child(
                // Recessed track, the same fill as a meter. It marks
                // the chart off the card. The marks are inset in
                // [`paint_curve`].
                div()
                    .w_full()
                    .h(px(CURVE_H))
                    .rounded(px(CHART_WELL_RADIUS))
                    .bg(theme::inset())
                    .overflow_hidden()
                    .child(sparkline(marks, span, now, chart.scale, chart.stroke)),
            )
            .child(chart_readout(&peaks, span, chart.unit, chart.icon))
            .into_any_element(),
    )
}

/// The highest reading (one per line, arrowed when there are two) and
/// how long the window has been collecting, one line under the chart.
/// The time stays — until the series covers a second, which is when
/// [`series::span`] starts answering.
fn chart_readout(
    peaks: &[(Option<&'static str>, Option<f64>)],
    span: Option<Duration>,
    unit: ChartUnit,
    icon: Option<IconName>,
) -> AnyElement {
    let span = span
        .map(series::span_label)
        .unwrap_or_else(|| format::PLACEHOLDER.to_string());
    let peaks = peaks.iter().enumerate().map(|(index, (arrow, value))| {
        let value = chart_value(unit, *value);
        let text = match arrow {
            Some(arrow) => format!("{arrow} {value}"),
            None => value,
        };
        // The dimmed line's figure is dimmed too: the caption doubles
        // as the legend for which stroke is which.
        let ink = if index == 0 {
            theme::text()
        } else {
            theme::text_muted()
        };
        div()
            .flex_none()
            .font_family(font::MONO)
            .text_size(px(10.))
            .line_height(px(CHART_CAPTION))
            .text_color(ink)
            .child(text)
    });
    h_flex()
        .mt(px(CHART_CAPTION_GAP))
        .h(px(CHART_CAPTION))
        .w_full()
        .items_center()
        .justify_between()
        .child(
            h_flex()
                .items_center()
                .gap(px(4.))
                .when_some(icon, |row, icon| {
                    row.child(
                        Icon::new(icon)
                            .with_size(Size::Size(px(10.)))
                            .text_color(Hsla::from(theme::text_dim())),
                    )
                })
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.))
                        .line_height(px(CHART_CAPTION))
                        .text_color(theme::text_dim())
                        .child(i18n::tr("overview.chart_peak")),
                )
                .child(h_flex().items_center().gap(px(8.)).children(peaks)),
        )
        .child(chart_stat(i18n::tr("overview.chart_span"), span))
        .into_any_element()
}

fn chart_stat(label: String, value: String) -> AnyElement {
    h_flex()
        .items_center()
        .gap(px(4.))
        .child(
            div()
                .flex_none()
                .text_size(px(10.))
                .line_height(px(CHART_CAPTION))
                .text_color(theme::text_dim())
                .child(label),
        )
        .child(
            div()
                .flex_none()
                .font_family(font::MONO)
                .text_size(px(10.))
                .line_height(px(CHART_CAPTION))
                .text_color(theme::text())
                .child(value),
        )
        .into_any_element()
}

/// The slot is always the same height. Nothing is stroked until the
/// series covers a second. The well behind it is already up, and the
/// axis under the stroke is the full 30 minutes, marked off by
/// [`paint_guides`] from the first frame. `marks` is one bucketed
/// series per line, the lead first.
fn sparkline(
    marks: Vec<Vec<Point>>,
    span: Option<Duration>,
    now: Instant,
    scale: f64,
    stroke: CurveStroke,
) -> AnyElement {
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            paint_guides(bounds, window);
            if span.is_none() {
                return;
            }
            // Behind first, so the lead line is painted on top.
            for (index, points) in marks.iter().enumerate().rev() {
                let ink = if index == 0 { theme::ink() } else { dim_ink() };
                paint_curve(bounds, points, now, scale, stroke, ink, window);
            }
        },
    )
    .size_full()
    .into_any_element()
}

/// How far apart the time guides are. Two of them, at 10 and 20
/// minutes ago, split the 30-minute axis into thirds.
const GUIDE_EVERY: Duration = Duration::from_secs(10 * 60);

/// Hairlines across the well at every [`GUIDE_EVERY`]. Until the window
/// fills, the marks sit on the right and the rest of the well is empty;
/// without these that emptiness read as a chart that failed to draw
/// rather than as half an hour not yet collected. Painted under the
/// marks, in the row-separator ink, full well height. On the bar chart
/// a guide falls on a slot boundary, in the gap between two bars.
fn paint_guides(bounds: Bounds<Pixels>, window: &mut Window) {
    let pad = px(CHART_PAD);
    let left = bounds.left() + pad;
    let width = bounds.size.width - pad * 2.;
    if width <= px(0.) {
        return;
    }
    let mut age = GUIDE_EVERY;
    while age < series::WINDOW {
        // Whole pixels: a 1px hairline straddling two columns paints
        // as two faint ones.
        let x = (f32::from(left) + f32::from(width) * axis_t(age) as f32).round();
        window.paint_quad(gpui::fill(
            Bounds::new(point(px(x), bounds.top()), size(px(1.), bounds.size.height)),
            theme::border_subtle(),
        ));
        age += GUIDE_EVERY;
    }
}

/// Share of [`theme::ink`]'s alpha kept by the marks behind the lead:
/// network upload under download, and every memory bar but the newest.
const CHART_DIM: f32 = 0.45;

/// [`theme::ink`] at [`CHART_DIM`]. Paint only, so a theme switch moves
/// no geometry.
fn dim_ink() -> Rgba {
    let ink = theme::ink();
    Rgba {
        a: ink.a * CHART_DIM,
        ..ink
    }
}

fn paint_curve(
    bounds: Bounds<Pixels>,
    points: &[Point],
    now: Instant,
    scale: f64,
    stroke: CurveStroke,
    ink: Rgba,
    window: &mut Window,
) {
    if scale <= 0.0 {
        return;
    }
    let pad = px(CHART_PAD);
    let top = bounds.top() + pad;
    let bottom = bounds.bottom() - pad;
    let left = bounds.left() + pad;
    let width = bounds.size.width - pad * 2.;
    if bottom <= top || width <= px(0.) {
        return;
    }
    let height = bottom - top;
    let x_of = |at: Instant| {
        let t = axis_t(now.saturating_duration_since(at));
        left + width * (t as f32)
    };
    let y_of = |value: f64| {
        let h = (value / scale).clamp(0.0, 1.0);
        bottom - height * (h as f32)
    };
    if let CurveStroke::Bars = stroke {
        paint_bars(points, left, width, now, bottom, &y_of, window);
        return;
    }
    let mut builder = PathBuilder::stroke(px(CURVE_STROKE));
    let mut drew = false;
    for segment in chart_runs(points) {
        let plotted: Vec<(f32, f32)> = segment
            .iter()
            .map(|(at, value)| (f32::from(x_of(*at)), f32::from(y_of(*value))))
            .collect();
        if plotted.is_empty() {
            continue;
        }
        // One bucket has nothing to bend through: the first 20s, or a
        // run cut off on both sides by breaks. See [`lone_reading`].
        if plotted.len() == 1 {
            let (x, y) = plotted[0];
            let slice = f32::from(width) / CHART_BUCKETS as f32;
            let (from, to) = lone_reading(x, slice, f32::from(left), f32::from(left + width));
            builder.move_to(point(px(from), px(y)));
            builder.line_to(point(px(to), px(y)));
            drew = true;
            continue;
        }
        builder.move_to(point(px(plotted[0].0), px(plotted[0].1)));
        for cubic in smooth_cubics(&plotted) {
            builder.cubic_bezier_to(
                point(px(cubic.to.0), px(cubic.to.1)),
                point(px(cubic.ctrl_a.0), px(cubic.ctrl_a.1)),
                point(px(cubic.ctrl_b.0), px(cubic.ctrl_b.1)),
            );
        }
        drew = true;
    }
    if !drew {
        return;
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, ink);
    }
}

/// Horizontal extent of a run that holds one bucket: a flat dash at the
/// reading's height, one slice wide, centred on the bucket and kept
/// inside the drawing. It used to be a vertical from the axis up to
/// the reading, which is how a bar is read — "from 0 to here", beside
/// the memory card's real bars — when it is one level, the first piece
/// of the line the next bucket extends.
fn lone_reading(x: f32, slice: f32, left: f32, right: f32) -> (f32, f32) {
    let half = slice / 2.0;
    ((x - half).max(left), (x + half).min(right))
}

/// Bars for [`CurveStroke::Bars`]. One rectangle per reading that has a
/// value, from the axis up to the sample. An empty slot stays empty,
/// on the left before the first sample and in a hole alike, so a bar
/// does not grow across it.
///
/// Only the newest bar is in the full ink; the history behind it is
/// [`dim_ink`]. Memory in use barely moves in half an hour, so sixty
/// full-ink bars were a solid block at ~70% height — the brightest
/// thing in the panel, for the least news on it.
fn paint_bars(
    points: &[Point],
    left: Pixels,
    width: Pixels,
    now: Instant,
    bottom: Pixels,
    y_of: &impl Fn(f64) -> Pixels,
    window: &mut Window,
) {
    let left = f32::from(left);
    let width = f32::from(width);
    if width <= 0.0 {
        return;
    }
    let floor = f32::from(bottom);
    // Oldest first, so the newest reading is the last one with a value.
    let newest = points.iter().rposition(|sample| sample.value.is_some());
    let mut history = PathBuilder::fill();
    let mut latest = PathBuilder::fill();
    let (mut drew_history, mut drew_latest) = (false, false);
    for (position, sample) in points.iter().enumerate() {
        let Some(value) = sample.value else {
            continue;
        };
        let Some(index) = bucket_index(sample.at, now) else {
            continue;
        };
        let top = f32::from(y_of(value));
        if top >= floor {
            continue;
        }
        let (bar_left, bar_right) = bar_edges(index, left, width);
        if bar_right <= bar_left {
            continue;
        }
        let builder = if Some(position) == newest {
            drew_latest = true;
            &mut latest
        } else {
            drew_history = true;
            &mut history
        };
        builder.move_to(point(px(bar_left), px(floor)));
        builder.line_to(point(px(bar_left), px(top)));
        builder.line_to(point(px(bar_right), px(top)));
        builder.line_to(point(px(bar_right), px(floor)));
        builder.close();
    }
    if drew_history && let Ok(path) = history.build() {
        window.paint_path(path, dim_ink());
    }
    if drew_latest && let Ok(path) = latest.build() {
        window.paint_path(path, theme::ink());
    }
}

/// Which of the [`BAR_BUCKETS`] slots `at` falls in. `0` is the newest,
/// on the right. A bar is drawn at its centre, and the centre of slot
/// `n` is `n * 30s + 15s`, which divides back to `n`.
fn bucket_index(at: Instant, now: Instant) -> Option<usize> {
    let age = now.saturating_duration_since(at);
    if age >= series::WINDOW {
        return None;
    }
    let secs = chart_slice(BAR_BUCKETS).as_secs();
    if secs == 0 {
        return None;
    }
    Some(((age.as_secs() / secs) as usize).min(BAR_BUCKETS - 1))
}

/// Edges of the bar in slot `index` (`0` is the rightmost). The chart
/// is split into [`BAR_BUCKETS`] equal slots, and [`BAR_GAP_SHARE`] of
/// each slot is the gap, half on either side of the bar. Sixty bars
/// and their gaps fill `width` exactly.
fn bar_edges(index: usize, left: f32, width: f32) -> (f32, f32) {
    let pitch = width / BAR_BUCKETS as f32;
    let inset = pitch * BAR_GAP_SHARE / 2.0;
    let slot_right = left + width - index as f32 * pitch;
    (slot_right - pitch + inset, slot_right - inset)
}

/// One cubic of [`smooth_cubics`]. `from` is where the previous cubic
/// ended; the stroke starts there and does not move again.
struct Cubic {
    ctrl_a: (f32, f32),
    ctrl_b: (f32, f32),
    to: (f32, f32),
}

/// Monotone cubic through `points`, as Bézier controls.
///
/// Fritsch–Carlson: the tangent at a sample is zero wherever the
/// slope changes sign, and each segment's tangents are shortened
/// until the curve stays between that segment's two samples. A plain
/// spline overshoots, and on a 0–100 axis an overshoot is a reading
/// the machine did not take. Two samples come back as the straight
/// chord. X is the pixel position, so a 5s tray step and a 2s open
/// step keep their own slopes.
fn smooth_cubics(points: &[(f32, f32)]) -> Vec<Cubic> {
    let n = points.len();
    if n < 2 {
        return Vec::new();
    }
    let slope = monotone_slopes(points);
    let mut out = Vec::with_capacity(n - 1);
    for i in 0..n - 1 {
        let (x0, y0) = points[i];
        let (x1, y1) = points[i + 1];
        let dx = (x1 - x0) / 3.0;
        out.push(Cubic {
            ctrl_a: (x0 + dx, y0 + slope[i] * dx),
            ctrl_b: (x1 - dx, y1 - slope[i + 1] * dx),
            to: (x1, y1),
        });
    }
    out
}

/// dy/dx at each sample. Interior points use the weighted harmonic
/// mean of the neighbouring secants; a sign change is a real peak
/// and gets a flat tangent. Endpoints use the three-point estimate,
/// then every segment is clamped so its Bézier cannot leave the
/// range of its two samples.
fn monotone_slopes(points: &[(f32, f32)]) -> Vec<f32> {
    let n = points.len();
    let mut h = vec![0.0; n - 1];
    let mut secant = vec![0.0; n - 1];
    for i in 0..n - 1 {
        h[i] = points[i + 1].0 - points[i].0;
        secant[i] = if h[i].abs() <= f32::EPSILON {
            0.0
        } else {
            (points[i + 1].1 - points[i].1) / h[i]
        };
    }
    let mut slope = vec![0.0; n];
    if n == 2 {
        slope[0] = secant[0];
        slope[1] = secant[0];
    } else {
        slope[0] = endpoint_slope(h[0], h[1], secant[0], secant[1]);
        let last = n - 2;
        slope[n - 1] = endpoint_slope(h[last], h[last - 1], secant[last], secant[last - 1]);
        for i in 1..n - 1 {
            if secant[i - 1] * secant[i] <= 0.0 {
                slope[i] = 0.0;
            } else {
                let w1 = 2.0 * h[i] + h[i - 1];
                let w2 = h[i] + 2.0 * h[i - 1];
                slope[i] = (w1 + w2) / (w1 / secant[i - 1] + w2 / secant[i]);
            }
        }
    }
    for i in 0..n - 1 {
        if secant[i].abs() <= f32::EPSILON {
            slope[i] = 0.0;
            slope[i + 1] = 0.0;
            continue;
        }
        let a = slope[i] / secant[i];
        let b = slope[i + 1] / secant[i];
        let sum = a * a + b * b;
        if sum > 9.0 {
            let tau = 3.0 / sum.sqrt();
            slope[i] *= tau;
            slope[i + 1] *= tau;
        }
    }
    slope
}

/// Slope at an end sample. A sign that disagrees with the first
/// secant would leave the data immediately, so it becomes flat; a
/// slope steeper than three times that secant is the same overshoot
/// the per-segment clamp removes, caught here before it bends the end.
fn endpoint_slope(h0: f32, h1: f32, d0: f32, d1: f32) -> f32 {
    let width = h0 + h1;
    if width.abs() <= f32::EPSILON {
        return 0.0;
    }
    let slope = ((2.0 * h0 + h1) * d0 - h0 * d1) / width;
    if slope * d0 <= 0.0 {
        0.0
    } else if d0 * d1 <= 0.0 && slope.abs() > 3.0 * d0.abs() {
        3.0 * d0
    } else {
        slope
    }
}

/// Disk + net rates, summed by zstats after its own dedupe. A footnote
/// under Memory, not a fourth card and not a second section — one
/// muted line so the memory card stays about memory.
///
/// Icons, not words: "Disk" / "Network" next to the rates read as one
/// sentence at this size. The glyphs are the same ones the tab strip
/// already uses for Hardware and Network, so they carry that meaning
/// here; the translated name sits on the tooltip.
fn io_strip(io: &IoTotalsSnapshot) -> AnyElement {
    let cells = [
        (
            "io-disk",
            IconName::HardDrive,
            i18n::tr("overview.io_disk"),
            io.disk_read_bytes_per_sec,
            io.disk_write_bytes_per_sec,
        ),
        (
            "io-net",
            IconName::Network,
            i18n::tr("overview.io_net"),
            io.network_received_bytes_per_sec,
            io.network_transmitted_bytes_per_sec,
        ),
    ];
    if cells
        .iter()
        .all(|(_, _, _, r, w)| r.is_none() && w.is_none())
    {
        return div().into_any_element();
    }

    h_flex()
        .mt(px(8.))
        // Extra padding above the rates, not margin: the hairline stays
        // with this row, and the extra air sits *inside* the footnote
        // instead of as a gap that read as the card ending early.
        .pt(px(14.))
        .gap(px(16.))
        .border_t(px(1.))
        .border_color(theme::border_subtle())
        .children(cells.into_iter().map(|(id, icon, label, read, write)| {
            h_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .id(id)
                        .flex_none()
                        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                        .child(
                            Icon::new(icon)
                                .with_size(Size::Size(px(12.)))
                                .text_color(Hsla::from(theme::text_dim())),
                        ),
                )
                .child(
                    h_flex()
                        .gap(px(6.))
                        .font_family(font::MONO)
                        .text_size(px(10.))
                        .text_color(theme::text_muted())
                        .child(format!("↓ {}", format::rate(read)))
                        .child(format!("↑ {}", format::rate(write))),
                )
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bezier_y(from_y: f32, cubic: &Cubic, t: f32) -> f32 {
        let u = 1.0 - t;
        u * u * u * from_y
            + 3.0 * u * u * t * cubic.ctrl_a.1
            + 3.0 * u * t * t * cubic.ctrl_b.1
            + t * t * t * cubic.to.1
    }

    #[test]
    fn the_cpu_curve_passes_through_the_samples_and_not_above_a_peak() {
        let peak = [(0.0, 10.0), (10.0, 80.0), (20.0, 12.0)];
        let cubics = smooth_cubics(&peak);
        assert_eq!(cubics.len(), 2);
        assert!((cubics[0].to.1 - 80.0).abs() < 1e-4);
        assert!((cubics[1].to.1 - 12.0).abs() < 1e-4);
        for (i, cubic) in cubics.iter().enumerate() {
            let low = peak[i].1.min(peak[i + 1].1);
            let high = peak[i].1.max(peak[i + 1].1);
            for step in 0..=8 {
                let y = bezier_y(peak[i].1, cubic, step as f32 / 8.0);
                assert!(
                    (low - 1e-3..=high + 1e-3).contains(&y),
                    "a sample of {high} must stay the high point, got {y}"
                );
            }
        }

        let chord = smooth_cubics(&[(0.0, 10.0), (9.0, 40.0)]);
        assert_eq!(chord.len(), 1);
        assert!(
            (chord[0].ctrl_a.1 - 20.0).abs() < 1e-3,
            "two samples stay a line"
        );
        assert!((chord[0].ctrl_b.1 - 30.0).abs() < 1e-3);

        let rising = [(0.0, 5.0), (4.0, 20.0), (14.0, 30.0)];
        let cubics = smooth_cubics(&rising);
        for (i, cubic) in cubics.iter().enumerate() {
            for step in 0..=8 {
                let y = bezier_y(rising[i].1, cubic, step as f32 / 8.0);
                assert!(
                    y + 1e-3 >= rising[i].1.min(rising[i + 1].1),
                    "a climb does not dip, got {y}"
                );
                assert!(
                    y <= rising[i].1.max(rising[i + 1].1) + 1e-3,
                    "a climb does not jump the next sample, got {y}"
                );
            }
        }
    }

    #[test]
    fn the_cpu_axis_stays_at_30_until_a_reading_goes_over_it() {
        let now = Instant::now();
        let point = |value: Option<f64>| Point { at: now, value };
        assert_eq!(cpu_axis_top(&[]), CPU_AXIS_LOW);
        assert_eq!(cpu_axis_top(&[point(None)]), CPU_AXIS_LOW);
        assert_eq!(cpu_axis_top(&[point(Some(12.0))]), CPU_AXIS_LOW);
        assert_eq!(cpu_axis_top(&[point(Some(29.9))]), CPU_AXIS_LOW);
        // Exactly 30 has not gone over the line, so the top stays 30.
        assert_eq!(cpu_axis_top(&[point(Some(30.0))]), CPU_AXIS_LOW);
        assert_eq!(cpu_axis_top(&[point(Some(8.0)), point(Some(64.0))]), 64.0);
        assert_eq!(cpu_axis_top(&[point(Some(30.1))]), 30.1);
    }

    #[test]
    fn a_short_history_stays_on_the_right_of_the_thirty_minute_axis() {
        assert!((axis_t(Duration::ZERO) - 1.0).abs() < 1e-9);
        assert!(axis_t(series::WINDOW).abs() < 1e-9);
        assert!((axis_t(series::WINDOW / 2) - 0.5).abs() < 1e-9);
        // 40s is the right edge of a 30-minute axis. The left stays empty.
        let t = axis_t(Duration::from_secs(40));
        assert!(t > 0.95, "40s sits on the right, got {t}");
    }

    #[test]
    fn sixty_bars_split_the_chart_and_a_new_bar_sits_on_the_right() {
        let left = 10.0;
        let width = 270.0;
        let pitch = width / BAR_BUCKETS as f32;
        let gap = pitch * BAR_GAP_SHARE;
        let (newest_left, newest_right) = bar_edges(0, left, width);
        let (oldest_left, oldest_right) = bar_edges(BAR_BUCKETS - 1, left, width);
        assert!((newest_right - (left + width - gap / 2.0)).abs() < 1e-3);
        assert!((oldest_left - (left + gap / 2.0)).abs() < 1e-3);
        assert!(((newest_right - newest_left) - (pitch - gap)).abs() < 1e-3);
        // The gap between the two newest bars is the share of one slot.
        let (_, next_right) = bar_edges(1, left, width);
        assert!((newest_left - next_right - gap).abs() < 1e-3);
        // Outer edges of the first and last slots meet the chart edges.
        let outer = (newest_right + gap / 2.0) - (oldest_left - gap / 2.0);
        assert!((outer - width).abs() < 1e-2);
        assert!(oldest_right < newest_left);
        let now = Instant::now();
        let at = now.checked_sub(chart_slice(BAR_BUCKETS) / 2).unwrap();
        assert_eq!(bucket_index(at, now), Some(0));
    }

    #[test]
    fn memory_draws_sixty_bars_of_thirty_seconds() {
        assert_eq!(chart_slice(BAR_BUCKETS), Duration::from_secs(30));
        assert_eq!(chart_slice(CHART_BUCKETS), Duration::from_secs(20));
        let now = Instant::now();
        // 5s and 20s share the newest 30s slice. 45s is the slice before.
        let bars = bar_buckets(
            &[
                Point {
                    at: ago(now, 45),
                    value: Some(6.0),
                },
                Point {
                    at: ago(now, 20),
                    value: Some(10.0),
                },
                Point {
                    at: ago(now, 5),
                    value: Some(30.0),
                },
            ],
            now,
        );
        assert_eq!(
            bars.iter().map(|point| point.value).collect::<Vec<_>>(),
            vec![Some(6.0), Some(20.0)]
        );
        let mut full = Vec::new();
        for step in 0..900 {
            full.push(Point {
                at: ago(now, step * 2),
                value: Some(1.0),
            });
        }
        assert_eq!(bar_buckets(&full, now).len(), BAR_BUCKETS);
        assert_eq!(chart_buckets(&full, now).len(), CHART_BUCKETS);
    }

    #[test]
    fn a_lone_reading_is_one_slice_wide_and_stays_in_the_drawing() {
        // Mid-axis: one slice, centred.
        assert_eq!(lone_reading(100.0, 4.0, 0.0, 300.0), (98.0, 102.0));
        // The newest bucket's centre is half a slice from the right
        // edge, so the dash ends on that edge rather than past it.
        assert_eq!(lone_reading(298.0, 4.0, 0.0, 300.0), (296.0, 300.0));
        assert_eq!(lone_reading(299.0, 4.0, 0.0, 300.0), (297.0, 300.0));
        assert_eq!(lone_reading(1.0, 4.0, 0.0, 300.0), (0.0, 3.0));
    }

    #[test]
    fn the_readout_is_the_highest_raw_sample() {
        let now = Instant::now();
        let peak = chart_peak(&[
            Point {
                at: ago(now, 30),
                value: Some(70.0),
            },
            Point {
                at: ago(now, 8),
                value: Some(10.0),
            },
            Point {
                at: ago(now, 2),
                value: Some(4.0),
            },
        ]);
        assert_eq!(peak, Some(70.0));
        // A network break ends nothing: the earlier peak remains.
        let broken = chart_peak(&[
            Point {
                at: ago(now, 8),
                value: Some(10.0),
            },
            Point {
                at: ago(now, 2),
                value: None,
            },
        ]);
        assert_eq!(broken, Some(10.0));
        assert_eq!(chart_peak(&[]), None);
        assert_eq!(chart_value(ChartUnit::Percent, Some(12.0)), "12.0%");
        assert_eq!(
            chart_value(ChartUnit::Rate, Some(2048.0)),
            format::rate(Some(2048))
        );
        assert_eq!(chart_value(ChartUnit::Percent, None), format::PLACEHOLDER);
    }

    fn ago(now: Instant, secs: u64) -> Instant {
        now.checked_sub(Duration::from_secs(secs)).unwrap()
    }

    #[test]
    fn the_chart_averages_twenty_second_slices_and_stops_at_ninety() {
        let now = Instant::now();
        // 10 and 30 in the newest slice average to 20. A slice with
        // only a missing rate stays a break, and it does not become 0.
        let sparse = chart_buckets(
            &[
                Point {
                    at: ago(now, 50),
                    value: Some(6.0),
                },
                Point {
                    at: ago(now, 30),
                    value: None,
                },
                Point {
                    at: ago(now, 8),
                    value: Some(10.0),
                },
                Point {
                    at: ago(now, 2),
                    value: Some(30.0),
                },
            ],
            now,
        );
        assert_eq!(
            sparse.iter().map(|point| point.value).collect::<Vec<_>>(),
            vec![Some(6.0), None, Some(20.0)]
        );
        let runs = chart_runs(&sparse);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].len(), 1);
        assert_eq!(runs[1][0].1, 20.0);
        // A slice with no sample at all, between two readings, is the
        // same break. It is not filled with zero.
        let hole = chart_buckets(
            &[
                Point {
                    at: ago(now, 50),
                    value: Some(6.0),
                },
                Point {
                    at: ago(now, 5),
                    value: Some(4.0),
                },
            ],
            now,
        );
        assert_eq!(
            hole.iter().map(|point| point.value).collect::<Vec<_>>(),
            vec![Some(6.0), None, Some(4.0)]
        );

        // A missing rate beside a real reading is skipped, not averaged in.
        let mixed = chart_buckets(
            &[
                Point {
                    at: ago(now, 6),
                    value: None,
                },
                Point {
                    at: ago(now, 2),
                    value: Some(10.0),
                },
            ],
            now,
        );
        assert_eq!(mixed.len(), 1);
        assert_eq!(mixed[0].value, Some(10.0));

        // The empty slices before the first sample are not points.
        let young = chart_buckets(
            &[Point {
                at: ago(now, 5),
                value: Some(4.0),
            }],
            now,
        );
        assert_eq!(young.len(), 1);

        let mut full = Vec::new();
        for step in 0..900 {
            full.push(Point {
                at: ago(now, step * 2),
                value: Some(1.0),
            });
        }
        let chart = chart_buckets(&full, now);
        assert_eq!(chart.len(), CHART_BUCKETS);
        assert!(chart.iter().all(|point| point.value == Some(1.0)));

        // The top follows the averaged points. 10 and 70 in one slice
        // are 40, so the axis is 40; 10 and 20 stay under 30.
        assert_eq!(
            cpu_axis_top(&chart_buckets(
                &[
                    Point {
                        at: ago(now, 2),
                        value: Some(10.0),
                    },
                    Point {
                        at: ago(now, 8),
                        value: Some(70.0),
                    },
                ],
                now,
            )),
            40.0
        );
        assert_eq!(
            cpu_axis_top(&chart_buckets(
                &[
                    Point {
                        at: ago(now, 2),
                        value: Some(10.0),
                    },
                    Point {
                        at: ago(now, 8),
                        value: Some(20.0),
                    },
                ],
                now,
            )),
            CPU_AXIS_LOW
        );
    }

    #[test]
    fn the_network_line_steps_by_doubling_and_never_below_the_floor() {
        let now = Instant::now();
        let point = |value: Option<f64>| Point { at: now, value };
        assert_eq!(
            net_scale(&[point(Some(11_000.0))]),
            NET_SCALE_FLOOR,
            "11 kB/s does not get to be full"
        );
        // 5 MiB sits between 4 MiB and 8 MiB. The ceiling is the next
        // doubling, so a few more bytes do not move the line.
        assert_eq!(
            net_scale(&[point(Some(5.0 * 1024.0 * 1024.0))]),
            8.0 * 1024.0 * 1024.0
        );
        // Everyday traffic swinging under 1 MiB keeps one ceiling, so
        // the line does not rescale while the window fills.
        assert_eq!(net_scale(&[point(Some(70_000.0))]), NET_SCALE_FLOOR);
        assert_eq!(net_scale(&[point(Some(263_000.0))]), NET_SCALE_FLOOR);
        assert_eq!(net_scale(&[point(Some(1024.0 * 1024.0))]), NET_SCALE_FLOOR);
        // Past the floor, nearby peaks share a step: 1.2 and 1.9 MiB
        // are both under 2 MiB.
        let step = net_scale(&[point(Some(1.2 * 1024.0 * 1024.0))]);
        assert_eq!(step, 2.0 * 1024.0 * 1024.0);
        assert_eq!(net_scale(&[point(Some(1.9 * 1024.0 * 1024.0))]), step);

        assert_eq!(net_scale(&[]), NET_SCALE_FLOOR);
        assert_eq!(net_scale(&[point(None)]), NET_SCALE_FLOOR);
    }

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

    fn group(pid: u32, name: &str, cpu: f32) -> ProcessGroupSnapshot {
        ProcessGroupSnapshot {
            root_pid: pid,
            name: name.into(),
            display_name: None,
            process_count: 1,
            cpu_usage_percent: cpu,
            memory_bytes: 0,
            phys_footprint_bytes: None,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    /// Two climbers used to be a two-row card in a window sized for five.
    #[test]
    fn a_short_climb_keeps_five_rows() {
        let groups = vec![
            group(1, "Zed", 0.7),
            group(2, "Ghostty", 6.5),
            group(3, "Chrome", 40.0),
            group(4, "Finder", 8.0),
            group(5, "WindowServer", 5.0),
        ];
        let risers = vec![(&groups[0], 216.0), (&groups[1], 22.8)];
        let rows = pad_rising(risers, &groups, 5);
        assert_eq!(rows.len(), 5);
        assert_eq!(trend::tree_key(rows[0].0), "Zed");
        assert_eq!(rows[0].1, Some(216.0));
        assert_eq!(trend::tree_key(rows[1].0), "Ghostty");
        assert_eq!(rows[1].1, Some(22.8));
        // Leftover slots are current CPU, skip the climbers already named.
        assert_eq!(trend::tree_key(rows[2].0), "Chrome");
        assert!(rows[2].1.is_none());
        assert_eq!(trend::tree_key(rows[3].0), "Finder");
        assert_eq!(trend::tree_key(rows[4].0), "WindowServer");
    }

    #[test]
    fn five_climbers_are_not_padded() {
        let groups: Vec<_> = (0..5)
            .map(|i| group(i, &format!("a{i}"), 10.0 + i as f32))
            .collect();
        let risers: Vec<_> = groups.iter().map(|g| (g, 20.0)).collect();
        let rows = pad_rising(risers, &groups, 5);
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|(_, d)| d.is_some()));
    }
}
