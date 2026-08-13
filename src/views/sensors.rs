//! Sensors: hardware temperatures, then the battery in full.

use super::widgets::{self, card};
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::ZStatsAppState;
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{AnyElement, Hsla, IntoElement, ParentElement, Styled, div, px};
use gpui_component::{h_flex, v_flex};
use rust_i18n::t;

/// Above this a sensor reads as hot. Only a display rule — the real limits
/// come from the sensor itself when it reports them.
const HOT_CELSIUS: f32 = 80.0;
/// Bar scale when a sensor reports no critical point of its own.
const ASSUMED_MAX_CELSIUS: f32 = 110.0;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(tick) = state.latest() else {
        return vec![widgets::empty_card(
            i18n::tr("sensors.title"),
            i18n::tr("common.waiting_sample"),
        )];
    };

    let temps_card = match tick.snapshot.temperatures.as_deref() {
        // Off and "ran but found nothing" are genuinely different states and
        // the design words them separately.
        None => widgets::empty_card(i18n::tr("sensors.off"), i18n::tr("sensors.off_body")),
        Some([]) => widgets::empty_card(
            i18n::tr("sensors.nothing"),
            i18n::tr("sensors.nothing_body"),
        ),
        Some(temps) => widgets::list_shell()
            .child(widgets::list_header(
                i18n::tr("sensors.title"),
                Some(widgets::note(i18n::tr("sensors.hottest_first"))),
            ))
            .children({
                // No separator under the last row — it would land on the
                // container's own edge and read as a stray line.
                let total = temps.len();
                temps.iter().enumerate().map(move |(i, t)| {
                    let hot = t.celsius > HOT_CELSIUS;
                    let scale = t.critical_celsius.unwrap_or(ASSUMED_MAX_CELSIUS).max(1.0);
                    v_flex()
                        .px(px(13.))
                        .py(px(10.))
                        .when(i + 1 != total, |d| {
                            d.border_b(px(1.)).border_color(theme::border_subtle())
                        })
                        .child(
                            h_flex()
                                .items_baseline()
                                .justify_between()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(11.))
                                        .text_color(theme::text_muted())
                                        .truncate()
                                        // Raw firmware labels; deliberately not
                                        // prettified, so they match other tools.
                                        .child(t.label.clone()),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .font_family(font::MONO)
                                        .text_size(px(13.))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(theme::text_for(hot))
                                        .child(format!("{:.1} °C", t.celsius)),
                                ),
                        )
                        .child(div().mt(px(5.)).child(widgets::meter(
                            t.celsius / scale,
                            Hsla::from(theme::fill_for(hot)),
                            4.,
                        )))
                        .child(
                            div()
                                .mt(px(4.))
                                .text_size(px(10.))
                                .text_color(theme::text_faint())
                                .child(match (t.max_celsius, t.critical_celsius) {
                                    (Some(m), Some(c)) => t!(
                                        "sensors.max_crit",
                                        max = format!("{m:.0}"),
                                        crit = format!("{c:.0}")
                                    )
                                    .to_string(),
                                    (Some(m), None) => {
                                        t!("sensors.max_only", max = format!("{m:.0}")).to_string()
                                    }
                                    (None, Some(c)) => {
                                        t!("sensors.crit_only", crit = format!("{c:.0}"))
                                            .to_string()
                                    }
                                    (None, None) => i18n::tr("sensors.no_limits"),
                                }),
                        )
                })
            })
            .into_any_element(),
    };

    let battery_card = match tick.snapshot.battery.as_ref() {
        None => widgets::empty_card(i18n::tr("sensors.battery"), i18n::tr("sensors.no_battery")),
        Some(b) => card()
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(i18n::tr("sensors.battery")),
            )
            .child(widgets::kv_columns(vec![
                (i18n::tr("sensors.state"), b.state.clone()),
                (i18n::tr("sensors.charge"), format::pct(b.charge_percent)),
                (
                    i18n::tr("sensors.draw"),
                    b.power_watts
                        .map_or("—".to_string(), |w| format!("{w:.1} W")),
                ),
                (
                    i18n::tr("sensors.empty_in"),
                    b.time_to_empty_secs.map_or("—".to_string(), format::uptime),
                ),
                (
                    i18n::tr("sensors.health"),
                    b.health_percent.map_or("—".into(), format::pct),
                ),
                (
                    i18n::tr("sensors.cycles"),
                    b.cycle_count.map_or("—".to_string(), |c| c.to_string()),
                ),
                (
                    i18n::tr("sensors.cell_temp"),
                    b.temperature_celsius
                        .map_or("—".to_string(), |c| format!("{c:.1} °C")),
                ),
                (
                    i18n::tr("sensors.to_full"),
                    b.time_to_full_secs.map_or("—".to_string(), format::uptime),
                ),
            ]))
            .into_any_element(),
    };

    vec![temps_card, battery_card]
}
