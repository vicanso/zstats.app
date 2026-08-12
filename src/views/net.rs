//! Network: one fixed row per interface.
//!
//! The design keeps idle interfaces in place rather than filtering them out —
//! a stable row order means nothing below jumps when traffic starts.

use super::widgets;
use crate::font;
use crate::format;
use crate::i18n;
use rust_i18n::t;
use crate::state::ZStatsAppState;
use crate::theme;
use gpui::{AnyElement, IntoElement, ParentElement, Styled, div, px, relative};
use gpui_component::{h_flex, v_flex};

/// Bar scale: 10 MB/s fills the track.
const SCALE_BYTES: f32 = 10.0 * 1024.0 * 1024.0;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(tick) = state.latest() else {
        return vec![widgets::empty_card(i18n::tr("net.title"), i18n::tr("common.waiting_sample"))];
    };
    let Some(nets) = tick.snapshot.networks.as_deref() else {
        return vec![widgets::empty_card(i18n::tr("net.off"), i18n::tr("net.off_body"))];
    };

    let mut rows: Vec<_> = nets.iter().collect();
    rows.sort_by(|a, b| {
        (b.received_bytes_per_sec + b.transmitted_bytes_per_sec)
            .cmp(&(a.received_bytes_per_sec + a.transmitted_bytes_per_sec))
            .then_with(|| a.interface.cmp(&b.interface))
    });

    let list = widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("net.title"),
            Some(widgets::note(
                t!("net.n_interfaces", count = rows.len()).to_string(),
            )),
        ))
        .children(rows.into_iter().map(|n| {
            let active = n.received_bytes_per_sec + n.transmitted_bytes_per_sec > 0;
            let fg = if active {
                theme::text()
            } else {
                theme::text_faint()
            };
            h_flex()
                .items_center()
                .gap(px(10.))
                .px(px(13.))
                .py(px(10.))
                .border_b(px(1.))
                .border_color(theme::border_subtle())
                .child(
                    div()
                        .w(px(42.))
                        .flex_none()
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(fg)
                        .truncate()
                        .child(n.interface.clone()),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            h_flex()
                                .justify_between()
                                .font_family(font::MONO)
                                .text_size(px(11.))
                                .text_color(theme::text_dim())
                                .child(div().text_color(fg).child(format!(
                                    "↓ {}",
                                    format::rate(Some(n.received_bytes_per_sec))
                                )))
                                .child(div().text_color(fg).child(format!(
                                    "↑ {}",
                                    format::rate(Some(n.transmitted_bytes_per_sec))
                                ))),
                        )
                        .child(
                            h_flex()
                                .gap(px(3.))
                                .mt(px(5.))
                                .child(bar(n.received_bytes_per_sec, theme::ink()))
                                .child(bar(n.transmitted_bytes_per_sec, theme::text_dim())),
                        ),
                )
        }))
        .child(
            div()
                .px(px(13.))
                .pt(px(9.))
                .pb(px(11.))
                .child(widgets::note(i18n::tr("net.idle_note"))),
        );

    vec![list.into_any_element()]
}

fn bar(bytes_per_sec: u64, fill: gpui::Rgba) -> AnyElement {
    div()
        .flex_1()
        .h(px(4.))
        .rounded_full()
        .bg(theme::inset())
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(relative(
                    (bytes_per_sec as f32 / SCALE_BYTES).clamp(0.0, 1.0),
                ))
                .rounded_full()
                .bg(fill),
        )
        .into_any_element()
}
