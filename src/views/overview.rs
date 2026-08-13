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
use zstats::snapshot::{CpuSnapshot, MemorySnapshot};

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
        memory(&snapshot.memory),
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
            let hot = avg > processes::HOT_PERCENT;
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
                        .flex_none()
                        .font_family(font::MONO)
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::text_for(hot))
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
        .cursor_pointer()
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
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
        .hover(|d| d.text_color(theme::text()))
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.set_tab(Tab::Processes, cx));
        })
        .into_any_element()
}

fn processor(cpu: &CpuSnapshot) -> AnyElement {
    let header_right = match cpu.frequency_mhz {
        Some(mhz) => widgets::metric_pill(format!("{:.2} GHz", mhz as f64 / 1000.0)),
        None => widgets::note(i18n::tr("overview.freq_unknown")),
    };
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

/// Activity Monitor / Stats wording: "P-cores · 8", not "P·8".
fn cluster_label(name: &str, cores: u32) -> String {
    let pretty = match name {
        "P" | "p" | "Performance" | "performance" => i18n::tr("overview.p_cores"),
        "E" | "e" | "Efficiency" | "efficiency" => i18n::tr("overview.e_cores"),
        other => other.to_string(),
    };
    format!("{pretty} · {cores}")
}

fn memory(mem: &MemorySnapshot) -> AnyElement {
    // The kernel's own verdict, not a number we derive: 1 normal, 2 warning,
    // 4 critical. Absent means the platform has no pressure API at all —
    // which the design is careful to word differently from "fine".
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
        None => (
            i18n::tr("overview.pressure_none"),
            i18n::tr("overview.pressure_tip_none"),
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
    // Compressed is part of used, not a leftover. Accent only when the
    // kernel says pressure is actually up — a red tail on a healthy Mac
    // reads as an error.
    let pressure_hot = mem.pressure_level.is_some_and(|l| l >= 2);
    // Three rungs that stay distinct as both a 6px bar slice and a
    // legend chip: ink (used) → muted (compressed) → faint (free).
    // `text_dim` sat too close to `text_faint` and the two dots
    // collapsed into one grey.
    let used_fill = Hsla::from(theme::ink());
    let compressed_fill = Hsla::from(if pressure_hot {
        theme::accent()
    } else {
        theme::text_muted()
    });
    let free_fill = Hsla::from(theme::text_faint());
    let free_w = (1.0 - resident_w - comp_w).max(0.0);

    let mut rows = vec![
        (i18n::tr("overview.used"), format::gb(mem.used_bytes)),
        (
            i18n::tr("overview.swap"),
            format!(
                "{} / {}",
                format::gb(mem.swap_used_bytes),
                format::gb(mem.swap_total_bytes)
            ),
        ),
    ];
    rows.push((
        i18n::tr("overview.compressed"),
        match mem.compressed_bytes {
            Some(b) => format::gb(b),
            None => i18n::tr("common.n_a"),
        },
    ));
    rows.push((i18n::tr("overview.total"), format::gb(mem.total_bytes)));

    let mut legend = vec![(
        used_fill,
        i18n::tr("overview.used").into(),
        i18n::tr("overview.used_tip").into(),
    )];
    if compressed > 0 {
        legend.push((
            compressed_fill,
            i18n::tr("overview.compressed").into(),
            i18n::tr("overview.compressed_tip").into(),
        ));
    }
    legend.push((
        free_fill,
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
                        .child(format::gb(mem.available_bytes)),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::text_muted())
                        .child(
                            t!("overview.available_of", total = format::gb(mem.total_bytes))
                                .to_string(),
                        ),
                ),
        )
        .child(div().mt(px(10.)).child(widgets::stacked_meter(
            vec![
                (resident_w, used_fill),
                (comp_w, compressed_fill),
                (free_w, free_fill),
            ],
            6.,
        )))
        .child(div().mt(px(8.)).child(widgets::legend(legend)))
        .child(widgets::kv_columns(rows))
        .into_any_element()
}
