//! Sensors: hardware temperatures, then the battery in full. Rendered as
//! the lower half of the Hardware tab, after the disk cards.

use super::widgets::{self, card};
use crate::hardware;
use crate::hot;
use crate::i18n;
use crate::state::{ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px,
};
use gpui_kit::component::{h_flex, v_flex};
use rust_i18n::t;

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
        Some(temps) => {
            // Closest to its own limit first, never hiding a hot one, and
            // the hottest in degrees for the header (`hardware::sensors`).
            let list = hardware::sensors(temps, state.show_all_sensors());
            let (hottest, sorted, hideable) = (list.hottest, list.rows, list.hideable);
            let show_all = state.show_all_sensors();
            // Same card and the same two-up cells as the Battery card
            // under it, so the two read as one kind of block.
            card()
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            h_flex()
                                .items_center()
                                .gap(px(6.))
                                .min_w_0()
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(px(12.))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme::text())
                                        .child(i18n::tr("sensors.title")),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(10.))
                                        .text_color(theme::text_muted())
                                        .child(
                                            t!("sensors.hottest", t = format!("{hottest:.1} °C"))
                                                .to_string(),
                                        ),
                                )
                                .child(widgets::info_icon(
                                    "sensors-names",
                                    i18n::tr("sensors.names_tip"),
                                )),
                        )
                        .child(more_chip(hideable, show_all)),
                )
                .child(sensor_grid(&sorted))
                .into_any_element()
        }
    };

    with_battery_card(temps_card, tick)
}

/// The readings two to a row, in rank order read left to right: the two
/// nearest their limits share the first row. Row-major, where the Battery
/// grid below splits its list into a left and a right column, because
/// this list is a ranking and a ranking reads across first.
///
/// Two columns cost the inline "peak 52" — a label, a limit and a reading
/// do not fit in half of 320px — so each cell carries them on hover, with
/// the full label, which the narrower cell now truncates sooner. The
/// order and the colour still say what the limit meant on the page.
fn sensor_grid(sorted: &[&zstats::snapshot::TemperatureSnapshot]) -> AnyElement {
    let rows = sorted.len().div_ceil(2);
    v_flex()
        .mt(px(10.))
        .children(sorted.chunks(2).enumerate().map(|(r, pair)| {
            // No separator under the last row — it would land on the
            // card's own edge and read as a stray line.
            let last = r + 1 == rows;
            h_flex()
                .gap(px(14.))
                .children(
                    pair.iter()
                        .enumerate()
                        .map(|(c, t)| sensor_cell(r * 2 + c, t, last)),
                )
                // An odd count leaves the right half empty rather than
                // stretching the last reading across the row.
                .when(pair.len() == 1, |row| row.child(div().flex_1()))
        }))
        .into_any_element()
}

fn sensor_cell(index: usize, t: &zstats::snapshot::TemperatureSnapshot, last: bool) -> AnyElement {
    let hot = hot::sensor_hot(t.celsius, t.critical_celsius);
    div()
        .id(("sensor", index))
        .flex_1()
        .min_w_0()
        .tooltip(widgets::wrap_tooltip_lines(vec![
            // Raw firmware labels; deliberately not prettified, so they
            // match other tools.
            t.label.clone().into(),
            hardware::limits_label(t).into(),
            i18n::tr("sensors.limit_tip").into(),
        ]))
        .child(widgets::kv_row(
            t.label.clone(),
            format!("{:.1} °C", t.celsius),
            last,
            hot,
        ))
        .into_any_element()
}

/// The expand/collapse control in the header, borrowed from the Network
/// tab's chip so the two filters read as the same idiom. `hideable` is how
/// many rows the collapse *would* hide, whether or not they are currently
/// on screen.
fn more_chip(hideable: usize, showing: bool) -> AnyElement {
    if hideable == 0 {
        return widgets::note(i18n::tr("sensors.hottest_first"));
    }
    div()
        .id("sensors-more")
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(if showing {
            theme::border()
        } else {
            theme::border_subtle()
        })
        .when(showing, |d| d.bg(theme::chip()))
        // Only while off: the "on" fill is the state, and a hover that
        // repainted it would read as the toggle having flipped.
        .when(!showing, |d| {
            d.hover(|d| d.bg(theme::surface_raised()).border_color(theme::border()))
        })
        .px(px(7.))
        .py(px(1.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(if showing {
            theme::text()
        } else {
            theme::text_dim()
        })
        .child(if showing {
            i18n::tr("sensors.hide_more")
        } else {
            t!("sensors.show_more", count = hideable).to_string()
        })
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.toggle_all_sensors(cx));
        })
        .into_any_element()
}

fn with_battery_card(temps_card: AnyElement, tick: &zstats::Tick) -> Vec<AnyElement> {
    let battery_card = match tick.snapshot.battery.as_ref() {
        None => widgets::empty_card(i18n::tr("sensors.battery"), i18n::tr("sensors.no_battery")),
        Some(b) => card()
            .child(widgets::card_header(
                i18n::tr("sensors.battery"),
                hardware::battery_status(b).map(widgets::note),
            ))
            .child(widgets::kv_columns(hardware::battery_cells(b)))
            .pb_2()
            .into_any_element(),
    };

    vec![temps_card, battery_card]
}
