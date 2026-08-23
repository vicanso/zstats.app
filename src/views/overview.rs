//! Overview: processor, top application trees, memory.

use super::processes;
use super::widgets::{self, card};
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{Tab, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use crate::trend;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, div, px,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;
use std::collections::HashSet;
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
    vec![
        processor(&snapshot.cpu, &snapshot.load),
        top_apps(state),
        memory(&snapshot.memory, &snapshot.io_totals, snapshot.capabilities),
    ]
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
/// All still opens the full Apps list. Battery / watts stay on Sensors.
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
            h_flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .px(px(13.))
                .py(px(5.))
                .when(i + 1 < n, |d| {
                    d.border_b(px(1.)).border_color(theme::border_subtle())
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
                // The climb, next to the level it led to. Muted on
                // purpose: a rise is news, not a threshold, and accent
                // stays reserved for over-the-line (`theme.rs`).
                .children(delta.map(|delta| {
                    div()
                        .id(("top-app-rise", g.root_pid as usize))
                        .flex_none()
                        .font_family(font::MONO)
                        .text_size(px(10.))
                        .text_color(theme::text_muted())
                        .tooltip(widgets::wrap_tooltip(
                            t!("overview.rise_row_tip", delta = format::pct(delta)).to_string(),
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
                        .child(format::pct_col(g.cpu_usage_percent)),
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

fn processor(cpu: &CpuSnapshot, load: &LoadSnapshot) -> AnyElement {
    let header_right = processor_caption(cpu);
    let mut body = card()
        .child(widgets::card_header(
            i18n::tr("overview.processor"),
            Some(header_right),
        ))
        .child(
            // Baseline-aligned so the footnote sits on the headline's
            // line, not on the bottom of its taller box. Two hit areas,
            // two tooltips: the figure explains its weighting, the load
            // explains what a count means against this core count.
            h_flex()
                .items_baseline()
                .mt(px(4.))
                .gap(px(10.))
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
        );

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

/// Brand plus the reported clock, e.g. "Apple M4 Pro (4.5 GHz)".
///
/// Apple Silicon does not expose live per-cluster MHz through sysinfo;
/// this is the rated clock and usually never moves. zstats still only
/// *asks* for it every 30s (cheaper than every usage sample) — that is
/// a collect cadence, not a claim that the number changes.
fn processor_caption(cpu: &CpuSnapshot) -> AnyElement {
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
    div()
        .id("cpu-brand")
        .max_w(px(176.))
        .truncate()
        .tooltip(widgets::wrap_tooltip(tip))
        .child(widgets::note(text))
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

/// Activity Monitor / Stats wording: "P-cores · 8", not "P·8".
fn cluster_label(name: &str, cores: u32) -> String {
    let pretty = match name {
        "P" | "p" | "Performance" | "performance" => i18n::tr("overview.p_cores"),
        "E" | "e" | "Efficiency" | "efficiency" => i18n::tr("overview.e_cores"),
        other => other.to_string(),
    };
    format!("{pretty} · {cores}")
}

/// `caps` decides how an absent figure reads: this build cannot measure
/// it, or it can and has not yet. On macOS every capability is true, so
/// every branch here resolves exactly as it did before 0.5.2.
fn memory(mem: &MemorySnapshot, io: &IoTotalsSnapshot, caps: Capabilities) -> AnyElement {
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
        .child(div().mt(px(10.)).child(widgets::stacked_meter(
            vec![(resident_w, used_fill), (comp_w, compressed_fill)],
            6.,
        )))
        .child(div().mt(px(8.)).child(widgets::legend(legend)))
        .child(widgets::kv_packed(rows))
        .child(io_strip(io))
        .into_any_element()
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
