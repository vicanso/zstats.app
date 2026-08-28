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

/// A sensor reads as hot at this fraction of its own ceiling — the same
/// `celsius / meter_scale` the bar paints, so red always means "bar
/// nearly full". Only a display rule, like every threshold in `views/`.
///
/// A fraction, not degrees, because a flat line cannot rank hardware
/// with different ceilings. The old absolute 80 °C did two things wrong
/// at once: on an Apple Silicon machine working hard, *every* pACC/eACC
/// channel sits at 81–84 °C — 34 red rows carry exactly as much
/// information as zero — while a battery cell at 55 °C, five degrees
/// from its own 60 °C limit, stayed neutral. Ninety percent of the
/// firmware's own number colours the one that is actually near its
/// line and lets the merely-warm stay quiet.
const HOT_FRACTION: f32 = 0.9;
/// Bar scale when a sensor reports no critical point of its own. With
/// [`HOT_FRACTION`] this puts the no-crit colour line at 99 °C — far
/// above the old 80, deliberately: for hardware whose ceiling we do not
/// know, "hot" was a guess, and a CPU genuinely at 99 °C is past any
/// plausible one.
const ASSUMED_MAX_CELSIUS: f32 = 110.0;
/// Rows shown before "show more". Apple Silicon firmware exposes dozens of
/// channels and most sit within a degree of each other; the few nearest
/// their own limits carry the story, and the full list would bury the disk
/// and battery cards that share this tab. Hot sensors are never hidden —
/// the cap only ever swallows quiet ones, and since the sort key and the
/// colour key are the same fraction, the hot rows are always a prefix the
/// truncation cannot reach into.
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
            // Closest to its own limit first — the fraction the bar
            // paints, not raw degrees. Raw degrees cannot rank hardware
            // with different ceilings: an 84 °C core with crit 110 has
            // a quarter of its range left, a 55 °C battery cell with
            // crit 60 has five degrees. Sorting by the bar's own
            // fraction puts the second one on top where it belongs,
            // and makes the bars a descending staircase — the order
            // explains itself. It also makes "hot rows are a prefix"
            // structural: sort key and colour key are the same number,
            // where the old absolute sort could push a near-its-limit
            // sensor below the preview cut and hide it.
            let mut sorted: Vec<_> = temps.iter().collect();
            sorted.sort_by(|a, b| crit_fraction(b).total_cmp(&crit_fraction(a)));
            let show_all = state.show_all_sensors();
            // How many the collapse would hide, independent of the current
            // state — same rule as the Network chip, so the control stays
            // a toggle instead of going inert once expanded.
            let hot = sorted
                .iter()
                .filter(|t| sensor_hot(t.celsius, t.critical_celsius))
                .count();
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
                        let hot = sensor_hot(t.celsius, t.critical_celsius);
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
                                                    .id(("sensor-limit", i))
                                                    .flex_none()
                                                    .text_size(px(10.))
                                                    .text_color(theme::text_faint())
                                                    .tooltip(widgets::wrap_tooltip(i18n::tr(
                                                        "sensors.limit_tip",
                                                    )))
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
                                crit_fraction(t),
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

/// What the firmware reports beside the reading.
///
/// zstats' `max_celsius` is the **highest reading observed**, not a
/// thermal limit — painting "max 52" next to 51.8 °C read as "at the
/// ceiling" when the bar (drawn against 110 °C / crit) was only half
/// full. The label now says *peak*. The bar uses [`meter_scale`].
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

/// The number the meter is drawn against.
///
/// Critical when the firmware reports one — that *is* a ceiling.
/// Otherwise [`ASSUMED_MAX_CELSIUS`], never the observed peak: the
/// hottest sensor on the page *is* the peak, so a bar scaled to it
/// would be full by construction.
fn meter_scale(crit: Option<f32>) -> f32 {
    crit.unwrap_or(ASSUMED_MAX_CELSIUS).max(1.0)
}

/// How much of its own range this sensor has used — the bar's fill, the
/// list's sort key, and (against [`HOT_FRACTION`]) the colour's input.
/// One definition so the three can never disagree: the row with the
/// longest bar is the top row is the first to turn red.
fn crit_fraction(t: &zstats::snapshot::TemperatureSnapshot) -> f32 {
    t.celsius / meter_scale(t.critical_celsius)
}

/// Colour turns at [`HOT_FRACTION`] of the sensor's own ceiling — never
/// against the observed peak, which is not a danger line, and never
/// against a flat degree count, which cannot tell a core five degrees
/// into its range from a battery cell five degrees from its limit.
fn sensor_hot(celsius: f32, crit: Option<f32>) -> bool {
    celsius / meter_scale(crit) >= HOT_FRACTION
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
/// same thing when there is something to say. State lives in the title
/// so the grid is always three rows: charging fills six cells, full and
/// idle leaves the last right-hand cell empty rather than collapsing
/// the card (five cells in two-up is three-and-two, and the hairlines
/// no longer meet).
fn battery_time(b: &zstats::snapshot::BatterySnapshot) -> Option<(String, String)> {
    match (b.time_to_empty_secs, b.time_to_full_secs) {
        (Some(secs), _) => Some((i18n::tr("sensors.empty_in"), format::uptime(secs))),
        (_, Some(secs)) => Some((i18n::tr("sensors.to_full"), format::uptime(secs))),
        (None, None) => None,
    }
}

fn battery_grid(b: &zstats::snapshot::BatterySnapshot) -> AnyElement {
    let charge = (i18n::tr("sensors.charge"), format::pct(b.charge_percent));
    let draw = (
        i18n::tr("sensors.draw"),
        b.power_watts
            .map_or(format::PLACEHOLDER.to_string(), |w| format!("{w:.1} W")),
    );
    let health = (
        i18n::tr("sensors.health"),
        b.health_percent
            .map_or(format::PLACEHOLDER.into(), format::pct),
    );
    let cycles = (
        i18n::tr("sensors.cycles"),
        b.cycle_count
            .map_or(format::PLACEHOLDER.to_string(), |c| c.to_string()),
    );
    let temp = (
        i18n::tr("sensors.cell_temp"),
        b.temperature_celsius
            .map_or(format::PLACEHOLDER.to_string(), |c| format!("{c:.1} °C")),
    );
    // Six slots so two-up is always three-and-three. The time cell is
    // the one that comes and goes; an empty pair keeps the row height
    // when there is nothing to say.
    widgets::kv_columns(vec![
        charge,
        draw,
        health,
        cycles,
        temp,
        battery_time(b).unwrap_or_else(|| (String::new(), "\u{00a0}".into())),
    ])
}

fn with_battery_card(temps_card: AnyElement, tick: &zstats::Tick) -> Vec<AnyElement> {
    let battery_card = match tick.snapshot.battery.as_ref() {
        None => widgets::empty_card(i18n::tr("sensors.battery"), i18n::tr("sensors.no_battery")),
        Some(b) => card()
            .child(widgets::card_header(
                i18n::tr("sensors.battery"),
                Some(widgets::note(b.state.clone())),
            ))
            .child(battery_grid(b))
            .pb_2()
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

    #[test]
    fn the_bar_is_not_scaled_to_the_observed_peak() {
        // The hottest sensor *is* the peak. Scaling to it would paint a
        // full bar by construction — which is what "max 52" next to
        // 51.8 °C looked like, even though 51.8 / 110 is half a trough.
        assert_eq!(meter_scale(None), ASSUMED_MAX_CELSIUS);
        assert_eq!(meter_scale(Some(100.0)), 100.0);
    }

    fn temp(celsius: f32, crit: Option<f32>) -> zstats::snapshot::TemperatureSnapshot {
        zstats::snapshot::TemperatureSnapshot {
            label: "t".into(),
            celsius,
            max_celsius: None,
            critical_celsius: crit,
        }
    }

    #[test]
    fn hot_means_near_its_own_ceiling_not_past_a_flat_line() {
        // The all-red machine: every cluster channel at 81–84 °C against
        // a 110 °C assumed ceiling is warm, not near a limit. Under the
        // old flat 80 °C line all 34 rows coloured and the colour said
        // nothing.
        assert!(!sensor_hot(84.0, None));
        assert!(!sensor_hot(84.0, Some(110.0)));
        // The one the flat line missed: five degrees from its own limit.
        assert!(sensor_hot(55.0, Some(60.0)));
        assert!(sensor_hot(92.0, Some(100.0)), "90% of crit colours");
        assert!(!sensor_hot(70.0, Some(100.0)));
        assert!(sensor_hot(99.5, None), "no-crit line sits at 90% of 110");
    }

    #[test]
    fn the_ranking_puts_the_endangered_sensor_above_the_merely_warm() {
        // 55/60 has used 92% of its range; 84/110 has used 76%. Raw
        // degrees would bury the battery cell under every cluster
        // channel — the exact row the page exists to surface.
        let battery = temp(55.0, Some(60.0));
        let core = temp(84.0, Some(110.0));
        assert!(crit_fraction(&battery) > crit_fraction(&core));
        // And the sort key IS the colour key, so the hot set is always
        // a prefix of the list — the preview truncation can only ever
        // swallow quiet rows.
        assert!(sensor_hot(battery.celsius, battery.critical_celsius));
        assert!(!sensor_hot(core.celsius, core.critical_celsius));
    }
}
