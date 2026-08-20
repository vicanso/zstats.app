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

/// Floor for the bar scale, below which the busiest row does not get to
/// define "full".
///
/// The track used to be a fixed 10 MB/s, which is a rate a laptop link
/// almost never sustains: at a normal 11 kB/s the fill was 0.1% — every
/// bar on the page painted empty, so fourteen tracks carried no
/// information at all. The scale is the page's own maximum now, the same
/// shape as the process list's, and this floor is what stops a machine
/// doing 3 kB/s of housekeeping from painting a full bar and reading as
/// saturated. 64 KiB/s is roughly where traffic starts being worth
/// seeing.
const SCALE_FLOOR_BYTES: f32 = 64.0 * 1024.0;

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

    let scale = scale_for(&rows);
    // Expanding used to dump every silent bridge and VM adapter as a
    // full 0 B/s row, burying the two or three that actually move.
    // Full rows stay on interfaces that are live *or* recently so; the
    // rest collapse to a name line.
    let (full, compact): (Vec<_>, Vec<_>) = if show_all {
        rows.into_iter().partition(|n| {
            n.received_bytes_per_sec + n.transmitted_bytes_per_sec > 0
                || state.net_is_recent(&n.interface)
        })
    } else {
        (rows, Vec::new())
    };
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
        .when(rule, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .child(
            div()
                // 64, not 42: `vmenet0` needs 43 and `bridge100`
                // needs ~58, so the old width turned three distinct
                // VM adapters into three rows all reading `vmen…`.
                // A truncation that erases the difference between
                // rows is worse than a narrower bar beside it.
                .w(px(64.))
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
                        .child(bar(n.received_bytes_per_sec, scale, theme::ink()))
                        .child(bar(n.transmitted_bytes_per_sec, scale, theme::text_dim())),
                ),
        )
        .into_any_element()
}

/// How many idle names the compact line keeps before an ellipsis.
/// Enough to show the usual suspects (awdl, bridges, a couple of
/// vmenet) without becoming a second table.
const IDLE_NAMES: usize = 8;

fn idle_compact(idle: &[&zstats::snapshot::NetworkSnapshot]) -> Option<AnyElement> {
    if idle.is_empty() {
        return None;
    }
    let mut names: Vec<&str> = idle
        .iter()
        .take(IDLE_NAMES)
        .map(|n| n.interface.as_str())
        .collect();
    if idle.len() > IDLE_NAMES {
        names.push("…");
    }
    Some(
        div()
            .px(px(13.))
            .py(px(8.))
            .child(widgets::note(
                t!("net.idle_compact", names = names.join(" · ")).to_string(),
            ))
            .into_any_element(),
    )
}

/// `hideable` is how many rows the filter *would* hide, whether or not they
/// are currently on screen — so the control stays a toggle in both states.
fn more_chip(hideable: usize, showing: bool) -> AnyElement {
    if hideable == 0 {
        return widgets::note(i18n::tr("net.all_shown"));
    }
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

/// Both directions share one scale, so ↓ and ↑ can be read against each
/// other as well as against the other rows.
fn scale_for(rows: &[&zstats::snapshot::NetworkSnapshot]) -> f32 {
    rows.iter()
        .flat_map(|n| [n.received_bytes_per_sec, n.transmitted_bytes_per_sec])
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
    use zstats::snapshot::NetworkSnapshot;

    fn net(rx: u64, tx: u64) -> NetworkSnapshot {
        NetworkSnapshot {
            interface: "en0".into(),
            received_bytes_per_sec: rx,
            transmitted_bytes_per_sec: tx,
            received_packets_per_sec: None,
            transmitted_packets_per_sec: None,
            received_errors_per_sec: None,
            transmitted_errors_per_sec: None,
        }
    }

    /// The track means "against the busiest thing on this page", but not
    /// below a rate worth drawing: a quiet machine must not paint a full
    /// bar for a trickle, and a real download must not peg every row.
    #[test]
    fn the_track_follows_the_page_but_never_below_the_floor() {
        let quiet = [net(11_000, 9_000), net(3_000, 7_000)];
        let scale = scale_for(&quiet.iter().collect::<Vec<_>>());
        assert_eq!(scale, SCALE_FLOOR_BYTES, "11 kB/s does not get to be full");
        assert!(11_000.0 / scale > 0.1, "and is still visible");

        let busy = [net(5 * 1024 * 1024, 0), net(1024 * 1024, 0)];
        let scale = scale_for(&busy.iter().collect::<Vec<_>>());
        assert_eq!(scale, 5.0 * 1024.0 * 1024.0, "the busiest row defines full");

        // An empty page still divides by something.
        assert_eq!(scale_for(&[]), SCALE_FLOOR_BYTES);
    }
}
