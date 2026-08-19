//! Overview: processor, top CPU processes, memory.

use super::processes;
use super::widgets::{self, card};
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{Tab, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, div, px,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;
use zstats::snapshot::{Capabilities, CpuSnapshot, IoTotalsSnapshot, MemorySnapshot};

/// How many processes the first panel names. Enough to answer "who's
/// hot" without turning Overview into a second Processes tab.
const TOP_N: usize = 5;

/// Per-core bar turns accent past this.
const CORE_HOT: f32 = 85.0;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(snapshot) = state.latest().map(|t| &t.snapshot) else {
        return vec![widgets::empty_card(
            i18n::tr("common.waiting_sample"),
            i18n::tr("common.waiting_sample_body"),
        )];
    };
    vec![
        processor(&snapshot.cpu),
        top_cpu(state),
        memory(&snapshot.memory, &snapshot.io_totals, snapshot.capabilities),
    ]
}

/// Compact "who's using the CPU" list. Battery / watts stay on Sensors —
/// a glance at the menu bar almost never needs them.
fn top_cpu(state: &ZStatsAppState) -> AnyElement {
    let Some(tick) = state.latest() else {
        return widgets::empty_card(
            i18n::tr("overview.top_cpu"),
            i18n::tr("common.waiting_sample"),
        );
    };
    let Some(mut rows) = processes::ranked_live(tick) else {
        return widgets::empty_card(
            i18n::tr("overview.top_cpu_off"),
            i18n::tr("overview.top_cpu_off_body"),
        );
    };
    rows.truncate(TOP_N);
    let n = rows.len();

    widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("overview.top_cpu"),
            Some(top_cpu_all()),
        ))
        .children(rows.into_iter().enumerate().map(|(i, (p, avg))| {
            let sustained = state.sustained_load(p.pid);
            let hot = processes::is_hot(avg, sustained.is_some());
            // A red number with nothing beside it is a question. The
            // Processes row answers it with a visible "sustained" pill;
            // here, at 320px next to a truncating name, the tooltip is
            // the proportionate place to say which of the two rules
            // lit it.
            let why = state.sustained_load(p.pid).map(|dur| {
                t!(
                    "processes.sustained",
                    duration = format::uptime(dur.as_secs())
                )
                .to_string()
            });
            h_flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .px(px(13.))
                .py(px(5.))
                .when(i + 1 < n, |d| {
                    d.border_b(px(1.)).border_color(theme::border_subtle())
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::text())
                        .truncate()
                        .child(p.name.clone()),
                )
                .child(
                    div()
                        .id(("top-cpu-pct", i))
                        .flex_none()
                        .font_family(font::MONO)
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::text_for(hot))
                        .when_some(why, |d, text| d.tooltip(widgets::wrap_tooltip(text)))
                        .child(format::pct_col(avg as f32)),
                )
        }))
        .into_any_element()
}

fn top_cpu_all() -> AnyElement {
    let tip = Tab::Processes.title();
    h_flex()
        .id("top-cpu-all")
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
                .update(cx, |state, cx| state.set_tab(Tab::Processes, cx));
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

fn processor(cpu: &CpuSnapshot) -> AnyElement {
    let header_right = processor_caption(cpu);
    let mut body = card()
        .child(widgets::card_header(
            i18n::tr("overview.processor"),
            Some(header_right),
        ))
        .child(h_flex().items_end().mt(px(4.)).child(widgets::big_number(
            format::whole_pct(cpu.usage_percent),
            "%",
            20.,
        )));

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
    let rows = vec![
        (
            i18n::tr("overview.swap"),
            format!(
                "{} / {}",
                format::gb(mem.swap_used_bytes),
                format::gb(mem.swap_total_bytes)
            ),
        ),
        (i18n::tr("overview.compressed"), compressed_label),
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
        .child(widgets::kv_columns(rows))
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
        .pt(px(6.))
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
