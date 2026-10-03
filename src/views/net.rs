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
use crate::state::{ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px, relative,
};
use gpui_kit::component::{h_flex, v_flex};
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
    // Nothing recent: the card keeps its header and its chip, with one
    // line under them. It sits under the listener list now, so a
    // full-size empty card was height spent saying nothing — and the chip
    // is the only way to see the idle interfaces, which the old empty card
    // left no way to reach.
    if rows.is_empty() {
        return vec![
            widgets::list_shell()
                .child(widgets::list_header(
                    i18n::tr("net.title"),
                    Some(more_chip(hideable, show_all)),
                ))
                .child(
                    div()
                        .px(px(13.))
                        .pb(px(11.))
                        .text_size(px(11.))
                        .text_color(theme::text_dim())
                        .child(i18n::tr("net.idle_body")),
                )
                .into_any_element(),
        ];
    }
    rank(&mut rows);

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

/// Busiest first among the interfaces that reach a wire, then loopback.
fn rank(rows: &mut [&zstats::snapshot::NetworkSnapshot]) {
    let total = |n: &zstats::snapshot::NetworkSnapshot| {
        n.received_bytes_per_sec
            .saturating_add(n.transmitted_bytes_per_sec)
    };
    rows.sort_by(|a, b| {
        a.is_loopback
            .cmp(&b.is_loopback)
            .then_with(|| total(b).cmp(&total(a)))
            .then_with(|| a.interface.cmp(&b.interface))
    });
}

fn iface_row(n: &zstats::snapshot::NetworkSnapshot, scale: f32, rule: bool) -> AnyElement {
    let active = n.received_bytes_per_sec + n.transmitted_bytes_per_sec > 0;
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
        .px(px(13.))
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
            match packets_tip(n) {
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
    let text = errors_label(n)?;
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

fn errors_label(n: &zstats::snapshot::NetworkSnapshot) -> Option<String> {
    match (n.received_errors_per_sec, n.transmitted_errors_per_sec) {
        (None, None) => None,
        (rx, tx) => {
            let rx = rx.unwrap_or(0);
            let tx = tx.unwrap_or(0);
            (rx + tx > 0).then(|| t!("net.errors", rx = rx, tx = tx).to_string())
        }
    }
}

fn packets_tip(n: &zstats::snapshot::NetworkSnapshot) -> Option<String> {
    match (n.received_packets_per_sec, n.transmitted_packets_per_sec) {
        (None, None) => None,
        (rx, tx) => Some(
            t!(
                "net.packets_tip",
                rx = rx.unwrap_or(0),
                tx = tx.unwrap_or(0)
            )
            .to_string(),
        ),
    }
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
        // Loopback does not get to define "full": at memory-copy speed
        // it would empty every bar that measures a real link.
        .filter(|n| !n.is_loopback)
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
            is_loopback: false,
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

    /// A build cache talking to itself over 127.0.0.1 is the busiest
    /// thing on the card and no part of the network: it goes last, and
    /// the real link beside it keeps a bar it can be read from.
    #[test]
    fn loopback_goes_last_and_does_not_set_the_scale() {
        let mut local = net(5_000_000_000, 5_000_000_000);
        local.interface = "lo0".into();
        local.is_loopback = true;
        let mut wifi = net(2 * 1024 * 1024, 100_000);
        wifi.interface = "en0".into();
        let mut tunnel = net(300_000, 50_000);
        tunnel.interface = "utun10".into();

        let mut rows = vec![&local, &tunnel, &wifi];
        rank(&mut rows);
        let names: Vec<&str> = rows.iter().map(|n| n.interface.as_str()).collect();
        assert_eq!(names, ["en0", "utun10", "lo0"]);
        assert_eq!(scale_for(&rows), 2.0 * 1024.0 * 1024.0, "en0 is full");
        assert_eq!(scale_for(&[&local]), SCALE_FLOOR_BYTES);
    }

    #[test]
    fn error_rates_stay_off_a_healthy_row() {
        let quiet = net(1000, 1000);
        assert!(errors_label(&quiet).is_none());
        assert!(packets_tip(&quiet).is_none());
        let mut noisy = net(1000, 1000);
        noisy.received_errors_per_sec = Some(12);
        noisy.transmitted_errors_per_sec = Some(0);
        assert!(errors_label(&noisy).unwrap().contains("12"));
        noisy.received_packets_per_sec = Some(800);
        noisy.transmitted_packets_per_sec = Some(90);
        assert!(packets_tip(&noisy).is_some());
    }
}
