//! Drives: the physical disks under the volume cards (`disk0`, an external
//! SSD, a mounted image), with the figures the volume layer cannot carry —
//! operations per second, time per operation, and how many are in flight.
//! Rendered on the Hardware tab straight after the volumes: those answer
//! "is it full", these answer "is it slow".
//!
//! Every figure is zstats' own (`SystemSnapshot::drives`), read through an
//! `ioreg` child process that the collector runs only while this tab is on
//! screen (`metrics::registry_channels`). Turning the channel on restarts
//! the rates from no baseline, so a visit opens on `—` and the numbers
//! arrive one refresh later — which the card's ⓘ says.

use super::widgets;
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::ZStatsAppState;
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px,
};
use gpui_kit::component::{h_flex, v_flex};
use rust_i18n::t;
use zstats::snapshot::DriveSnapshot;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    // No sample yet: the volume renderer above already says so, once.
    let Some(tick) = state.latest() else {
        return Vec::new();
    };
    let snapshot = &tick.snapshot;
    // A platform without the concept (Linux, Windows today) and a file
    // that turned the channel off both get no card: neither is a state
    // this tab can do anything about, and "unavailable" in a card would
    // be chrome on every visit.
    let file_allows = state
        .settings()
        .and_then(|s| s.collector.as_ref())
        .is_none_or(|c| c.collect_drives);
    if !snapshot.capabilities.drive_io || !file_allows {
        return Vec::new();
    }

    let shell = widgets::list_shell().child(widgets::list_header(
        h_flex()
            .items_center()
            .gap(px(4.))
            .child(i18n::tr("drives.title"))
            .child(widgets::info_icon("drives-tip", i18n::tr("drives.tip"))),
        None,
    ));
    let card = match snapshot.drives.as_deref() {
        // The registry answered and found no whole disk: nothing to list,
        // and "reading" would be a promise the next tick cannot keep.
        Some([]) => return Vec::new(),
        // The tab just opened and the first read has not landed (the
        // collector was woken for it), or the registry did not answer
        // this round. Either way the next tick is due in seconds.
        None => shell.child(
            div()
                .px(px(13.))
                .pb(px(11.))
                .text_size(px(11.))
                .text_color(theme::text_dim())
                .child(i18n::tr("drives.reading")),
        ),
        Some(drives) => {
            let last = drives.len().saturating_sub(1);
            shell.children(
                drives
                    .iter()
                    .enumerate()
                    .map(|(i, drive)| drive_row(i, drive, i == last)),
            )
        }
    };
    vec![card.into_any_element()]
}

/// Name and model on top with the queue depth pinned right; the two
/// directions underneath; errors only when there are any.
fn drive_row(index: usize, drive: &DriveSnapshot, last: bool) -> AnyElement {
    let identity = match (&drive.model, drive.size_bytes) {
        (Some(model), Some(size)) => format!("{model} · {}", format::capacity(size)),
        (Some(model), None) => model.clone(),
        (None, Some(size)) => format::capacity(size),
        (None, None) => String::new(),
    };
    let errors = drive.read_errors + drive.write_errors;
    v_flex()
        .px(px(13.))
        .py(px(8.))
        .when(!last, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .child(
            h_flex()
                .items_baseline()
                .justify_between()
                .gap(px(8.))
                .child(
                    h_flex()
                        .items_baseline()
                        .gap(px(6.))
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .child(drive.name.clone()),
                        )
                        .child(widgets::truncating_name(
                            ("drive-model", index),
                            identity,
                            10.,
                            gpui::FontWeight::NORMAL,
                            theme::text_dim().into(),
                        )),
                )
                .child(
                    // A depth, not a percentage: the driver keeps no
                    // busy-time clock, so a 0-100 bar cannot be derived
                    // honestly and is deliberately not drawn.
                    font::mono_unless_cjk(div())
                        .id(("drive-queue", index))
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(theme::text_muted())
                        .tooltip(widgets::wrap_tooltip(i18n::tr("drives.queue_tip")))
                        .child(
                            t!(
                                "drives.queue",
                                depth = drive
                                    .queue_depth
                                    .map_or(format::PLACEHOLDER.to_string(), |q| format!("{q:.2}"))
                            )
                            .to_string(),
                        ),
                ),
        )
        .child(
            h_flex()
                .justify_between()
                .gap(px(8.))
                .mt(px(4.))
                .text_size(px(10.))
                .text_color(theme::text_muted())
                .child(
                    font::mono_unless_cjk(div()).child(
                        t!(
                            "drives.read",
                            ops = format::ops(drive.read_ops_per_sec),
                            lat = format::millis(drive.read_latency_ms)
                        )
                        .to_string(),
                    ),
                )
                .child(
                    font::mono_unless_cjk(div()).child(
                        t!(
                            "drives.write",
                            ops = format::ops(drive.write_ops_per_sec),
                            lat = format::millis(drive.write_latency_ms)
                        )
                        .to_string(),
                    ),
                ),
        )
        // Cumulative since boot, and rare enough that any count is news:
        // accent, never a comparison against a bar.
        .when(errors > 0, |row| {
            row.child(
                div()
                    .mt(px(4.))
                    .text_size(px(10.))
                    .text_color(theme::text_for(true))
                    .child(
                        t!(
                            "drives.errors",
                            read = drive.read_errors,
                            write = drive.write_errors
                        )
                        .to_string(),
                    ),
            )
        })
        .into_any_element()
}
