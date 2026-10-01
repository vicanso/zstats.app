//! Who is moving bytes, on the Network tab above the listener list.
//!
//! The numbers are `zstats::process_traffic()`, diffed in [`crate::traffic`]
//! while this tab is on screen. Nothing here asks for the read; views
//! only format. A row is a program, ranked by ↓+↑, and a program that
//! moved nothing is not a row — a machine has dozens of processes
//! holding an idle socket, and listing them is how the two that matter
//! get buried. The same shape as the interface card (rates, then a
//! shared bar) so the two cards read as one page.
//!
//! The bar's floor is the interface card's: 64 KiB/s is where traffic
//! starts being worth a full track, and a few kB/s of housekeeping must
//! not paint one. There is no threshold on this card, so nothing here
//! turns accent — accent is a crossed line, and a fast download is not
//! one.

use super::widgets;
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{TrafficReady, TrafficView, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use crate::traffic::ProgramRate;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px, relative,
};
use gpui_kit::component::{h_flex, v_flex};
use rust_i18n::t;
use zstats::OwnerCoverage;
use zstats::snapshot::Capabilities;

/// Busiest programs before "show more". The listening card's cap: six
/// is a screenful at 320px, and the rest stay one chip away. Idle
/// programs are not in this list.
const PREVIEW_ROWS: usize = 6;

/// Floor for the bar scale. Same value as the interface card, for the
/// same reason: below it the busiest row must not get to define "full".
const SCALE_FLOOR_BYTES: f32 = 64.0 * 1024.0;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    // No such counters on this platform: no card, rather than an
    // apology on every visit. Windows is the one zstats refuses.
    if !Capabilities::current().process_traffic {
        return Vec::new();
    }
    let body = match state.traffic() {
        None | Some(TrafficView::Reading) => vec![note_line(i18n::tr("traffic.reading"))],
        Some(TrafficView::Restricted) => vec![note_line(i18n::tr("traffic.restricted"))],
        Some(TrafficView::Failed(error)) => {
            vec![note_line(
                t!("traffic.failed", error = error.clone()).to_string(),
            )]
        }
        Some(TrafficView::Ready(ready)) => return vec![ready_card(state, ready)],
    };
    vec![
        widgets::list_shell()
            .child(header(None, None))
            .children(body)
            .into_any_element(),
    ]
}

fn ready_card(state: &ZStatsAppState, ready: &TrafficReady) -> AnyElement {
    let rows = &ready.rows;
    if rows.is_empty() {
        let text = if ready.process_count == 0 {
            i18n::tr("traffic.none")
        } else {
            i18n::tr("traffic.quiet")
        };
        return widgets::list_shell()
            .child(header(None, Some(ready.coverage)))
            .child(note_line(text))
            .into_any_element();
    }
    let hidden = rows.len().saturating_sub(PREVIEW_ROWS);
    let show_all = state.show_all_traffic();
    let shown = if show_all {
        rows.as_slice()
    } else {
        &rows[..rows.len().min(PREVIEW_ROWS)]
    };
    // The chip stays a toggle once expanded, including the sample where
    // the extras have gone quiet and `hidden` drops to zero.
    let chip = (hidden > 0 || show_all).then(|| more_chip(hidden, show_all));
    // Scale against the rows on screen. A hidden faster row would pin
    // every visible bar to a track the reader cannot see.
    let scale = scale_for(shown);
    let last = shown.len() - 1;
    widgets::list_shell()
        .child(header(chip, Some(ready.coverage)))
        .children(
            shown
                .iter()
                .enumerate()
                .map(|(i, row)| row_element(i, row, scale, i != last)),
        )
        .into_any_element()
}

fn header(chip: Option<AnyElement>, coverage: Option<OwnerCoverage>) -> AnyElement {
    widgets::list_header(
        h_flex()
            .items_center()
            .gap(px(4.))
            .child(i18n::tr("traffic.title"))
            .child(widgets::info_icon("traffic-tip", tip(coverage))),
        chip,
    )
}

/// What the number is, and the one way this platform's counters differ.
/// The platform sentence is always in both catalogues; which one is
/// appended depends on where this binary runs.
fn tip(coverage: Option<OwnerCoverage>) -> String {
    let mut tip = i18n::tr("traffic.tip");
    let platform = if cfg!(target_os = "linux") {
        i18n::tr("traffic.tip_linux")
    } else {
        i18n::tr("traffic.tip_macos")
    };
    tip = format!("{tip} {platform}");
    if coverage == Some(OwnerCoverage::OwnProcessesOnly) {
        tip = format!("{tip} {}", i18n::tr("traffic.tip_partial"));
    }
    tip
}

fn note_line(text: String) -> AnyElement {
    div()
        .px(px(13.))
        .pb(px(11.))
        .text_size(px(11.))
        .text_color(theme::text_dim())
        .child(text)
        .into_any_element()
}

fn row_element(index: usize, row: &ProgramRate, scale: f32, rule: bool) -> AnyElement {
    let name = row
        .name
        .clone()
        .unwrap_or_else(|| i18n::tr("traffic.unknown"));
    v_flex()
        .px(px(13.))
        .py(px(8.))
        .when(rule, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(widgets::truncating_name(
                    ("traffic-owner", index),
                    name,
                    11.,
                    gpui::FontWeight::MEDIUM,
                    theme::text().into(),
                ))
                .child(pid_label(index, row)),
        )
        .child(
            h_flex()
                .justify_between()
                .mt(px(3.))
                .font_family(font::MONO)
                .text_size(px(11.))
                .text_color(theme::text())
                .child(format!("↓ {}", format::rate(row.received_per_sec)))
                .child(format!("↑ {}", format::rate(row.transmitted_per_sec))),
        )
        .child(
            h_flex()
                .gap(px(3.))
                .mt(px(5.))
                .child(bar(row.received_per_sec.unwrap_or(0), scale, theme::ink()))
                .child(bar(
                    row.transmitted_per_sec.unwrap_or(0),
                    scale,
                    theme::text_dim(),
                )),
        )
        .into_any_element()
}

/// The pid for one process, `×N` when the row stands for several. The
/// several case is where the pids went, so the tooltip gives them back.
fn pid_label(index: usize, row: &ProgramRate) -> AnyElement {
    let label = match row.pids.len() {
        0 => String::new(),
        1 => row
            .pids
            .iter()
            .next()
            .map(u32::to_string)
            .unwrap_or_default(),
        n => format!("×{n}"),
    };
    let el = div()
        .flex_none()
        .text_size(px(10.))
        .text_color(theme::text_dim())
        .child(label);
    if row.pids.len() < 2 {
        return el.into_any_element();
    }
    let pids = row
        .pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    el.id(("traffic-pids", index))
        .tooltip(widgets::wrap_tooltip(
            t!("traffic.pids_tip", pids = pids).to_string(),
        ))
        .into_any_element()
}

fn more_chip(hidden: usize, showing: bool) -> AnyElement {
    let label = if showing {
        i18n::tr("traffic.hide")
    } else {
        t!("traffic.show_more", count = hidden).to_string()
    };
    div()
        .id("traffic-more")
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
        .child(label)
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.toggle_all_traffic(cx));
        })
        .into_any_element()
}

/// Both directions share one scale, so ↓ and ↑ can be read against each
/// other as well as against the other rows.
fn scale_for(rows: &[ProgramRate]) -> f32 {
    rows.iter()
        .flat_map(|row| {
            [
                row.received_per_sec.unwrap_or(0),
                row.transmitted_per_sec.unwrap_or(0),
            ]
        })
        .max()
        .map_or(SCALE_FLOOR_BYTES, |peak| {
            (peak as f32).max(SCALE_FLOOR_BYTES)
        })
}

fn bar(bytes_per_sec: u64, scale: f32, fill: gpui::Rgba) -> AnyElement {
    div()
        .flex_1()
        .h(px(4.))
        .rounded_full()
        .bg(theme::inset())
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(relative((bytes_per_sec as f32 / scale).clamp(0.0, 1.0)))
                .rounded_full()
                .bg(fill),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn program(name: &str, rx: u64, tx: u64) -> ProgramRate {
        ProgramRate {
            name: Some(name.into()),
            pids: BTreeSet::from([1]),
            received_per_sec: Some(rx),
            transmitted_per_sec: Some(tx),
        }
    }

    #[test]
    fn the_track_follows_the_rows_but_never_below_the_floor() {
        let quiet = [program("a", 11_000, 9_000), program("b", 3_000, 7_000)];
        let scale = scale_for(&quiet);
        assert_eq!(scale, SCALE_FLOOR_BYTES, "11 kB/s does not get to be full");

        let busy = [
            program("a", 5 * 1024 * 1024, 0),
            program("b", 1024 * 1024, 0),
        ];
        let scale = scale_for(&busy);
        assert_eq!(scale, 5.0 * 1024.0 * 1024.0, "the busiest row defines full");

        assert_eq!(scale_for(&[]), SCALE_FLOOR_BYTES);
    }
}
