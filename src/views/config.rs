//! Config: a read-only window onto config.toml.
//!
//! Deliberately not editable. Two of the three sections could not be applied
//! in place anyway: `reload_settings()` only re-reads `[alerts]`, while the
//! `[collector]` switches are baked into a running collector whose rate
//! baselines would be lost by rebuilding it. Changes go through the zstats
//! CLI, or the file itself.

use super::widgets::{self, card};
use crate::state::ZStatsAppState;
use crate::font;
use crate::i18n;
use crate::theme;
use rust_i18n::t;
use gpui::prelude::FluentBuilder;
use gpui::{AnyElement, IntoElement, ParentElement, Styled, div, px};
use gpui_component::{h_flex, v_flex};
use std::time::Duration;
use zstats::CollectorConfig;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(file) = state.settings() else {
        return vec![widgets::empty_card(
            i18n::tr("config.unavailable"),
            i18n::tr("config.unavailable_body"),
        )];
    };
    let collector = file.collector.clone().unwrap_or_default();

    vec![
        collection_card(&collector),
        cadence_card(&collector),
        thresholds_card(file),
    ]
}

/// The collect-* switches, rendered as the design's pill toggles but inert.
fn collection_card(c: &CollectorConfig) -> AnyElement {
    let switches = [
        ("collect-processes", c.collect_processes),
        ("collect-disks", c.collect_disks),
        ("collect-networks", c.collect_networks),
        ("collect-temperatures", c.collect_temperatures),
        ("collect-battery", c.collect_battery),
        ("process-groups", c.collect_process_groups),
        ("process-disk-io", c.collect_process_disk_io),
        ("per-core-cpu", c.per_core_cpu),
        ("dedupe-disks", c.dedupe_disks),
    ];

    widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("config.collection"),
            Some(widgets::note(i18n::tr("config.readonly"))),
        ))
        .children(switches.into_iter().map(|(label, on)| {
            h_flex()
                .items_center()
                .justify_between()
                .px(px(13.))
                .py(px(8.))
                .border_b(px(1.))
                .border_color(theme::border_subtle())
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::ink())
                        .child(label),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(28.))
                        .h(px(16.))
                        .rounded_full()
                        .p(px(2.))
                        .bg(if on { theme::accent() } else { theme::border() })
                        .child(
                            h_flex()
                                .size_full()
                                .when(on, |d| d.justify_end())
                                .child(
                                    div()
                                        .size(px(12.))
                                        .rounded_full()
                                        .bg(if on { gpui::white().into() } else { theme::text_dim() }),
                                ),
                        ),
                )
        }))
        .into_any_element()
}

fn cadence_card(c: &CollectorConfig) -> AnyElement {
    let cells = [
        (i18n::tr("config.cadence_process"), c.process_refresh_interval),
        (i18n::tr("config.cadence_disk_io"), c.disk_io_refresh_interval),
        (
            i18n::tr("config.cadence_disk_capacity"),
            c.disk_storage_refresh_interval,
        ),
        (i18n::tr("config.cadence_network"), c.network_refresh_interval),
        (
            i18n::tr("config.cadence_temperature"),
            c.temperature_refresh_interval,
        ),
        (
            i18n::tr("config.cadence_cpu_freq"),
            c.cpu_frequency_refresh_interval,
        ),
        (i18n::tr("config.cadence_battery"), c.battery_refresh_interval),
        (i18n::tr("config.cadence_timeout"), c.collect_timeout),
    ];

    card()
        .child(
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(i18n::tr("config.cadences")),
        )
        .child(
            v_flex()
                .mt(px(8.))
                .gap(px(6.))
                .children(cells.chunks(2).map(|pair| {
                    h_flex()
                        .gap(px(6.))
                        .children(pair.iter().map(|(k, v)| {
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .rounded(px(8.))
                                .bg(theme::inset())
                                .px(px(8.))
                                .py(px(6.))
                                .child(
                                    div()
                                        .text_size(px(8.5))
                                        .text_color(theme::text_dim())
                                        .child(k.clone()),
                                )
                                .child(
                                    div()
                                        .mt(px(1.))
                                        .font_family(font::MONO)
                                        .text_size(px(11.))
                                        .font_weight(gpui::FontWeight::NORMAL)
                                        .text_color(theme::text())
                                        .child(humanize(*v)),
                                )
                        }))
                })),
        )
        .into_any_element()
}

fn thresholds_card(file: &zstats::settings::FileConfig) -> AnyElement {
    let a = &file.alerts;
    let rows: Vec<(String, String, usize)> = vec![
        (
            "alert-cpu".into(),
            a.cpu.map_or(i18n::tr("config.unset"), |v| format!("{v:.0}%")),
            a.cpu_overrides.len(),
        ),
        (
            "alert-mem".into(),
            a.mem.map_or(i18n::tr("config.unset"), |v| format!("{v:.0}%")),
            a.mem_overrides.len(),
        ),
        (
            "alert-app-cpu".into(),
            a.app_cpu.map_or(i18n::tr("config.unset"), |v| format!("{v:.0}%")),
            a.app_cpu_overrides.len(),
        ),
        (
            "alert-app-mem".into(),
            a.app_mem.map_or(i18n::tr("config.unset"), |v| format!("{v:.0}%")),
            a.app_mem_overrides.len(),
        ),
        (
            "alert-disk".into(),
            a.disk.map_or(i18n::tr("config.unset"), |v| format!("{v:.0}%")),
            a.disk_overrides.len(),
        ),
        (
            "alert-cooldown".into(),
            a.cooldown.map_or(i18n::tr("config.default"), humanize),
            0,
        ),
    ];

    widgets::list_shell()
        .child(widgets::list_header(i18n::tr("config.thresholds"), None))
        .children(rows.into_iter().map(|(k, v, overrides)| {
            h_flex()
                .items_center()
                .justify_between()
                .px(px(13.))
                .py(px(7.))
                .border_b(px(1.))
                .border_color(theme::border_subtle())
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::ink())
                        .child(k),
                )
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(6.))
                        .when(overrides > 0, |d| {
                            d.child(
                                div()
                                    .rounded_full()
                                    .border_1()
                                    .border_color(theme::accent_wash(45))
                                    .px(px(6.))
                                    .text_size(px(8.5))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme::accent_light())
                                    .child(t!("config.override", count = overrides).to_string()),
                            )
                        })
                        .child(
                            div()
                                .font_family(font::MONO)
                                .text_size(px(10.))
                                .font_weight(gpui::FontWeight::NORMAL)
                                .text_color(theme::text())
                                .child(v),
                        ),
                )
        }))
        .child(
            div()
                .px(px(13.))
                .pt(px(9.))
                .pb(px(11.))
                .child(widgets::note(i18n::tr("config.thresholds_note"))),
        )
        .into_any_element()
}

/// Durations the way the config file writes them.
fn humanize(d: Duration) -> String {
    let ms = d.as_millis();
    if ms >= 60_000 && ms.is_multiple_of(60_000) {
        format!("{}m", ms / 60_000)
    } else if ms.is_multiple_of(1_000) {
        format!("{}s", ms / 1_000)
    } else {
        format!("{ms}ms")
    }
}
