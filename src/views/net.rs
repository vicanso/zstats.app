//! Network: interfaces ranked by current throughput.
//!
//! Interfaces that have carried nothing recently are hidden — a machine lists
//! dozens (unused Ethernet, tunnels, bridges, VM adapters) and they bury the
//! two or three that matter. Measured here: 32 reported, 5 actually moving
//! bytes. A header chip says how many are hidden and reveals them.
//!
//! Loopback (`NetworkSnapshot::is_loopback`, zstats ≥ 0.7.1) is listed and
//! labelled "local", after the interfaces that reach a wire, and never
//! sets the bars' scale. It is programs on this machine talking to each
//! other at memory-copy speed — measured here, 628 GB against the Wi-Fi's
//! 0.88 GB over one uptime — so ranked by throughput it was always the
//! first row and every real interface's bar beside it was empty. zstats
//! leaves it out of the totals Overview shows; hiding the row here too
//! would leave no place that says where a local proxy's traffic went.

use super::widgets;
use crate::font;
use crate::format;
use crate::i18n;
use crate::net::{self, Ifaces};
use crate::state::{ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px, relative,
};
use gpui_kit::component::{h_flex, v_flex};

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

    let Ifaces {
        full,
        compact,
        hideable,
        scale,
    } = net::ifaces(state, nets);
    let show_all = state.show_unused_nets();
    // Nothing recent: the card keeps its header and its chip, with one
    // line under them. It sits under the listener list now, so a
    // full-size empty card was height spent saying nothing — and the chip
    // is the only way to see the idle interfaces, which the old empty card
    // left no way to reach.
    if full.is_empty() && compact.is_empty() {
        return vec![
            widgets::list_shell()
                .child(widgets::list_header(
                    i18n::tr("net.title"),
                    Some(more_chip(hideable, show_all)),
                ))
                .child(
                    div()
                        .px(px(widgets::CARD_PAD))
                        .pb(px(11.))
                        .text_size(px(11.))
                        .text_color(theme::text_dim())
                        .child(i18n::tr("net.idle_body")),
                )
                .into_any_element(),
        ];
    }
    let last = full.len().saturating_sub(1);
    let has_compact = !compact.is_empty();
    let list = widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("net.title"),
            Some(more_chip(hideable, show_all)),
        ))
        .children(
            full.into_iter()
                .enumerate()
                .map(|(i, n)| iface_row(n, scale, i != last || has_compact)),
        )
        .children(idle_compact(&compact));

    vec![list.into_any_element()]
}

fn iface_row(n: &zstats::snapshot::NetworkSnapshot, scale: f32, rule: bool) -> AnyElement {
    let active = net::active(n);
    // Loopback's figures are real and stay, a step back: they are not
    // the wire's, and beside it they are usually the largest on the card.
    let fg = if !active {
        theme::text_faint()
    } else if n.is_loopback {
        theme::text_muted()
    } else {
        theme::text()
    };
    let (down_fill, up_fill) = if n.is_loopback {
        (theme::border(), theme::border())
    } else {
        (theme::ink(), theme::text_dim())
    };
    h_flex()
        .items_center()
        .gap(px(10.))
        .px(px(widgets::CARD_PAD))
        .py(px(10.))
        .when(rule, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .child(
            v_flex()
                .id(gpui::SharedString::from(format!(
                    "net-name-{}",
                    n.interface
                )))
                // 64, not 42: `vmenet0` needs 43 and `bridge100`
                // needs ~58, so the old width turned three distinct
                // VM adapters into three rows all reading `vmen…`.
                // A truncation that erases the difference between
                // rows is worse than a narrower bar beside it.
                .w(px(64.))
                .flex_none()
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(fg)
                        .truncate()
                        .child(n.interface.clone()),
                )
                // Under the name, inside the row's two lines: the word
                // that says why this row is missing from Overview's total.
                .when(n.is_loopback, |d| {
                    d.child(
                        div()
                            .text_size(px(9.))
                            .text_color(theme::text_dim())
                            .child(i18n::tr("net.local")),
                    )
                    .tooltip(widgets::wrap_tooltip(i18n::tr("net.local_tip")))
                }),
        )
        .child({
            let rates = v_flex()
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
                        .child(bar(n.received_bytes_per_sec, scale, down_fill))
                        .child(bar(n.transmitted_bytes_per_sec, scale, up_fill)),
                )
                .children(errors_line(n));
            match net::packets_tip(n) {
                Some(tip) => div()
                    .id(gpui::SharedString::from(format!(
                        "net-pkts-{}",
                        n.interface
                    )))
                    .flex_1()
                    .min_w_0()
                    .tooltip(widgets::wrap_tooltip(tip))
                    .child(rates)
                    .into_any_element(),
                None => rates.into_any_element(),
            }
        })
        .into_any_element()
}

/// Error rates only when something is actually erroring. A healthy
/// interface is not a row of zeros. Display only — zstats has no
/// error-rate alert.
fn errors_line(n: &zstats::snapshot::NetworkSnapshot) -> Option<AnyElement> {
    let text = net::errors_label(n)?;
    Some(
        div()
            .mt(px(4.))
            .font_family(font::MONO)
            .text_size(px(9.))
            .text_color(theme::accent_light())
            .child(text)
            .into_any_element(),
    )
}

fn idle_compact(idle: &[&zstats::snapshot::NetworkSnapshot]) -> Option<AnyElement> {
    let line = net::idle_line(idle)?;
    Some(
        div()
            .px(px(widgets::CARD_PAD))
            .py(px(8.))
            .child(widgets::note(line))
            .into_any_element(),
    )
}

/// `hideable` is how many rows the filter *would* hide, whether or not they
/// are currently on screen — so the control stays a toggle in both states.
fn more_chip(hideable: usize, showing: bool) -> AnyElement {
    let Some(label) = net::more_label(hideable, showing) else {
        return widgets::note(i18n::tr("net.all_shown"));
    };
    div()
        .id("net-more")
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
                .update(cx, |state, cx| state.toggle_unused_nets(cx));
        })
        .into_any_element()
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
                .w(relative(net::fraction(bytes_per_sec, scale)))
                .rounded_full()
                .bg(fill),
        )
        .into_any_element()
}
