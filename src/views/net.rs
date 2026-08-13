//! Network: interfaces ranked by current throughput.
//!
//! Interfaces that have carried nothing recently are hidden — a machine lists
//! dozens (unused Ethernet, tunnels, bridges, VM adapters) and they bury the
//! two or three that matter. Measured here: 32 reported, 5 actually moving
//! bytes. A header chip says how many are hidden and reveals them.

use super::widgets;
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px, relative,
};
use gpui_component::{h_flex, v_flex};
use rust_i18n::t;

/// Bar scale: 10 MB/s fills the track.
const SCALE_BYTES: f32 = 10.0 * 1024.0 * 1024.0;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(tick) = state.latest() else {
        return vec![widgets::empty_card(
            i18n::tr("net.title"),
            i18n::tr("common.waiting_sample"),
        )];
    };
    let Some(nets) = tick.snapshot.networks.as_deref() else {
        return vec![widgets::empty_card(
            i18n::tr("net.off"),
            i18n::tr("net.off_body"),
        )];
    };

    // Only interfaces that have actually moved bytes recently. A machine
    // lists a pile of permanently silent ones — unused Ethernet, tunnels,
    // bridges — and they bury the two that matter.
    // One rule, one count. Previously two independent filters ran in
    // sequence and the header chip only knew about the second — so it could
    // report "nothing hidden" while the first had quietly dropped 27 of 32
    // interfaces, with no way to see them.
    let show_all = state.show_unused_nets();
    // Counted independently of `show_all`: deriving it from what is currently
    // displayed makes it zero once expanded, and the chip would turn back into
    // static text with no way to collapse again.
    let hideable = nets
        .iter()
        .filter(|n| !state.net_is_recent(&n.interface))
        .count();
    let mut rows: Vec<_> = nets
        .iter()
        .filter(|n| show_all || state.net_is_recent(&n.interface))
        .collect();
    if rows.is_empty() {
        return vec![widgets::empty_card(
            i18n::tr("net.idle_title"),
            i18n::tr("net.idle_body"),
        )];
    }
    rows.sort_by(|a, b| {
        (b.received_bytes_per_sec + b.transmitted_bytes_per_sec)
            .cmp(&(a.received_bytes_per_sec + a.transmitted_bytes_per_sec))
            .then_with(|| a.interface.cmp(&b.interface))
    });

    let last = rows.len().saturating_sub(1);
    let list = widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("net.title"),
            Some(more_chip(hideable, show_all)),
        ))
        .children(rows.into_iter().enumerate().map(|(i, n)| {
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
                .when(i != last, |d| {
                    d.border_b(px(1.)).border_color(theme::border_subtle())
                })
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
        }));

    vec![list.into_any_element()]
}

/// `hideable` is how many rows the filter *would* hide, whether or not they
/// are currently on screen — so the control stays a toggle in both states.
fn more_chip(hideable: usize, showing: bool) -> AnyElement {
    if hideable == 0 {
        return widgets::note(i18n::tr("net.all_shown"));
    }
    div()
        .id("net-more")
        .cursor_pointer()
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(if showing {
            theme::border()
        } else {
            theme::border_subtle()
        })
        .when(showing, |d| d.bg(theme::chip()))
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
            i18n::tr("net.hide_more")
        } else {
            t!("net.show_more", count = hideable).to_string()
        })
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.toggle_unused_nets(cx));
        })
        .into_any_element()
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
