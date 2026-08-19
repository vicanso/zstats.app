//! Sensors: hardware temperatures, then the battery in full. Rendered as
//! the lower half of the Hardware tab, after the disk cards.

use super::widgets::{self, card};
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, div, px,
};
use gpui_component::{h_flex, v_flex};
use rust_i18n::t;

/// Above this a sensor reads as hot. Only a display rule — the real limits
/// come from the sensor itself when it reports them.
const HOT_CELSIUS: f32 = 80.0;
/// Bar scale when a sensor reports no critical point of its own.
const ASSUMED_MAX_CELSIUS: f32 = 110.0;
/// Rows shown before "show more". Apple Silicon firmware exposes dozens of
/// channels and most sit within a degree of each other; the hottest few
/// carry the story, and the full list would bury the disk and battery
/// cards that share this tab. Hot sensors are never hidden — the cap only
/// ever swallows quiet ones.
///
/// Four, not three, because this card is what fills the tab: with one
/// volume and a battery at the default window height, three rows left a
/// row's worth of empty panel under the last card, and this is the only
/// card here that can honestly grow into it. It is a floor, not a fit —
/// a second mounted volume or a taller battery card takes that room
/// back and the tab scrolls, which is the recoverable direction. A
/// height-derived count would need to know how many volume cards
/// `disk::render` produced, which this module cannot see.
const SENSOR_PREVIEW: usize = 4;

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
            // Hottest first, sorted here rather than trusted from the
            // collector: the preview below is a prefix of this list, so
            // the sort is what makes "the few that matter" and "the top
            // of the list" the same set.
            let mut sorted: Vec<_> = temps.iter().collect();
            sorted.sort_by(|a, b| b.celsius.total_cmp(&a.celsius));
            let show_all = state.show_all_sensors();
            // How many the collapse would hide, independent of the current
            // state — same rule as the Network chip, so the control stays
            // a toggle instead of going inert once expanded.
            let hot = sorted.iter().filter(|t| t.celsius > HOT_CELSIUS).count();
            let hideable = sorted.len().saturating_sub(SENSOR_PREVIEW.max(hot));
            if !show_all {
                sorted.truncate(SENSOR_PREVIEW.max(hot));
            }
            widgets::list_shell()
                .child(widgets::list_header(
                    i18n::tr("sensors.title"),
                    Some(more_chip(hideable, show_all)),
                ))
                .children({
                    // No separator under the last row — it would land on the
                    // container's own edge and read as a stray line.
                    let total = sorted.len();
                    sorted.into_iter().enumerate().map(move |(i, t)| {
                        let hot = t.celsius > HOT_CELSIUS;
                        let scale = t.critical_celsius.unwrap_or(ASSUMED_MAX_CELSIUS).max(1.0);
                        v_flex()
                            .px(px(13.))
                            // 9, not 10: the tab has to end above the footer
                            // with room to spare, and trading single pixels
                            // against a total that shifts with the volume
                            // count is a game with no end. Still the loosest
                            // row in the panel — a process row is 7 — which
                            // this one earns by carrying a meter under a
                            // two-part label.
                            .py(px(9.))
                            .when(i + 1 != total, |d| {
                                d.border_b(px(1.)).border_color(theme::border_subtle())
                            })
                            .child(
                                h_flex()
                                    .items_baseline()
                                    .justify_between()
                                    .gap(px(8.))
                                    .child(
                                        // Label and limits on one line. The
                                        // limits used to sit under the meter,
                                        // costing a third line per sensor — and
                                        // "51.8 °C" only means something beside
                                        // the "max 52" it is being read against,
                                        // so the two belong on the same line
                                        // anyway.
                                        h_flex()
                                            .items_baseline()
                                            .gap(px(6.))
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .text_size(px(11.))
                                                    .text_color(theme::text_muted())
                                                    .truncate()
                                                    // Raw firmware labels;
                                                    // deliberately not prettified,
                                                    // so they match other tools.
                                                    .child(t.label.clone()),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .text_size(px(10.))
                                                    .text_color(theme::text_faint())
                                                    .child(limits_label(t)),
                                            ),
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
                            // 4, matching the meter's own height: it binds
                            // the bar to the label above it while giving the
                            // tab back exactly what the bottom gutter took
                            // (four rows × 1pt = the 4 that `BODY_BOTTOM_PAD`
                            // grew by, which is what put this tab back over
                            // the fold).
                            .child(div().mt(px(4.)).child(widgets::meter(
                                t.celsius / scale,
                                Hsla::from(theme::fill_for(hot)),
                                4.,
                            )))
                    })
                })
                .pb_3()
                .into_any_element()
        }
    };

    with_battery_card(temps_card, tick)
}

/// What the firmware says this sensor's ceiling is — the number the
/// reading is judged against, and the scale the meter is drawn on.
/// "Nothing reported" is said out loud rather than left blank: the bar
/// is then drawn against [`ASSUMED_MAX_CELSIUS`], and a silent bar would
/// let that assumption pass for a measurement.
fn limits_label(t: &zstats::snapshot::TemperatureSnapshot) -> String {
    match (t.max_celsius, t.critical_celsius) {
        (Some(m), Some(c)) => t!(
            "sensors.max_crit",
            max = format!("{m:.0}"),
            crit = format!("{c:.0}")
        )
        .to_string(),
        (Some(m), None) => t!("sensors.max_only", max = format!("{m:.0}")).to_string(),
        (None, Some(c)) => t!("sensors.crit_only", crit = format!("{c:.0}")).to_string(),
        (None, None) => i18n::tr("sensors.no_limits"),
    }
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

/// The one time figure that applies right now, or none at all.
///
/// A battery is either emptying or filling, so the old fixed pair of
/// "Time left" and "To full" guaranteed that one of them was a dash —
/// and on a full, idle machine both were. One adaptive cell says the
/// same thing when there is something to say, and a full battery gets
/// the card back a whole row (the grid pairs cells, so six entries take
/// three rows where eight took four).
fn battery_time(b: &zstats::snapshot::BatterySnapshot) -> Option<(String, String)> {
    match (b.time_to_empty_secs, b.time_to_full_secs) {
        (Some(secs), _) => Some((i18n::tr("sensors.empty_in"), format::uptime(secs))),
        (_, Some(secs)) => Some((i18n::tr("sensors.to_full"), format::uptime(secs))),
        (None, None) => None,
    }
}

fn with_battery_card(temps_card: AnyElement, tick: &zstats::Tick) -> Vec<AnyElement> {
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
            .child(widgets::kv_columns(
                vec![
                    (i18n::tr("sensors.state"), b.state.clone()),
                    (i18n::tr("sensors.charge"), format::pct(b.charge_percent)),
                    (
                        i18n::tr("sensors.draw"),
                        b.power_watts
                            .map_or(format::PLACEHOLDER.to_string(), |w| format!("{w:.1} W")),
                    ),
                    (
                        i18n::tr("sensors.health"),
                        b.health_percent
                            .map_or(format::PLACEHOLDER.into(), format::pct),
                    ),
                    (
                        i18n::tr("sensors.cycles"),
                        b.cycle_count
                            .map_or(format::PLACEHOLDER.to_string(), |c| c.to_string()),
                    ),
                    (
                        i18n::tr("sensors.cell_temp"),
                        b.temperature_celsius
                            .map_or(format::PLACEHOLDER.to_string(), |c| format!("{c:.1} °C")),
                    ),
                ]
                .into_iter()
                .chain(battery_time(b))
                .collect(),
            ))
            .into_any_element(),
    };

    vec![temps_card, battery_card]
}

#[cfg(test)]
mod tests {
    use super::*;
    use zstats::snapshot::BatterySnapshot;

    fn battery(to_empty: Option<u64>, to_full: Option<u64>) -> BatterySnapshot {
        BatterySnapshot {
            state: "Full".into(),
            charge_percent: 100.0,
            health_percent: None,
            cycle_count: None,
            temperature_celsius: None,
            power_watts: None,
            time_to_full_secs: to_full,
            time_to_empty_secs: to_empty,
        }
    }

    /// A battery is either emptying or filling. The card used to reserve
    /// a cell for each, so one of them was always a dash — and on a full,
    /// idle machine both were, spending a whole row of the card on two
    /// dashes.
    #[test]
    fn the_battery_shows_the_one_time_that_applies_and_otherwise_none() {
        assert_eq!(
            battery_time(&battery(Some(3600), None)).map(|(_, v)| v),
            Some(format::uptime(3600)),
            "discharging shows time left"
        );
        assert_eq!(
            battery_time(&battery(None, Some(900))).map(|(_, v)| v),
            Some(format::uptime(900)),
            "charging shows time to full"
        );
        assert!(
            battery_time(&battery(None, None)).is_none(),
            "full and idle has nothing to say, so it says nothing"
        );
    }
}
