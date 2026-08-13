//! Pieces the design repeats across all eight views.
//!
//! Everything here is a plain builder returning an element — the design's
//! cards, pills, meters and key/value rows are div + radius + a width
//! percentage, so there is nothing to gain from real components.

use crate::font;
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AnyView, App, Div, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px, relative,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{h_flex, v_flex};

/// Long-copy tooltip: wraps inside the 320px panel and uses caption size
/// instead of the default `text_sm` one-liner.
pub fn wrap_tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let text = text.into();
    move |window, cx| {
        let text = text.clone();
        Tooltip::element(move |_, _| {
            div()
                .max_w(px(220.))
                .text_size(px(11.))
                .line_height(relative(1.35))
                .whitespace_normal()
                .child(text.clone())
        })
        .max_w(px(232.))
        .text_size(px(11.))
        .py(px(4.))
        .build(window, cx)
    }
}

/// Settings-style grouped container: fill, no hard outline.
pub fn card() -> Div {
    v_flex()
        .rounded(px(12.))
        .bg(theme::surface())
        .when(!theme::is_dark(), |d| {
            d.border_1().border_color(theme::border())
        })
        .px(px(13.))
        .pt(px(12.))
        .pb(px(13.))
}

/// Same grouping, no inner padding — lists paint their own rows to the edges.
pub fn list_shell() -> Div {
    v_flex()
        .rounded(px(12.))
        .bg(theme::surface())
        .when(!theme::is_dark(), |d| {
            d.border_1().border_color(theme::border())
        })
        .overflow_hidden()
}

/// Card title on the left, an optional status pill on the right.
pub fn card_header(title: impl Into<SharedString>, right: Option<AnyElement>) -> AnyElement {
    h_flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title.into()),
        )
        .children(right)
        .into_any_element()
}

/// Title row for a [`list_shell`]: padded, no divider (rows carry the hairlines).
pub fn list_header(title: impl Into<SharedString>, right: Option<AnyElement>) -> AnyElement {
    h_flex()
        .items_center()
        .justify_between()
        .px(px(13.))
        .pt(px(11.))
        .pb(px(9.))
        .child(
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title.into()),
        )
        .children(right)
        .into_any_element()
}

/// Neutral outlined pill — "30s", quiet status.
pub fn outline_pill(text: impl Into<SharedString>) -> AnyElement {
    div()
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(theme::border())
        .px(px(7.))
        .py(px(1.))
        .text_size(px(10.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

/// Outlined pill whose content is a ticking figure (`4.51 GHz`).
pub fn metric_pill(text: impl Into<SharedString>) -> AnyElement {
    div()
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(theme::border())
        .px(px(7.))
        .py(px(1.))
        .font_family(font::MONO)
        .text_size(px(10.))
        .font_weight(gpui::FontWeight::NORMAL)
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

/// Horizontal meter. `fraction` is clamped, so a process at 600% CPU fills
/// the bar instead of overflowing it.
pub fn meter(fraction: f32, fill: impl Into<gpui::Hsla>, height: f32) -> AnyElement {
    div()
        .h(px(height))
        .w_full()
        .rounded_full()
        .bg(theme::inset())
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(relative(fraction.clamp(0.0, 1.0)))
                .rounded_full()
                .bg(fill.into()),
        )
        .into_any_element()
}

/// One track, several adjacent segments (memory: resident + compressed).
pub fn stacked_meter(segments: Vec<(f32, gpui::Hsla)>, height: f32) -> AnyElement {
    div()
        .h(px(height))
        .w_full()
        .rounded_full()
        .bg(theme::inset())
        .overflow_hidden()
        .child(
            h_flex()
                .size_full()
                .children(segments.into_iter().filter_map(|(frac, fill)| {
                    let w = frac.clamp(0.0, 1.0);
                    (w > 0.0).then(|| div().h_full().w(relative(w)).bg(fill))
                })),
        )
        .into_any_element()
}

/// Colour key under a stacked meter: a 6px swatch plus a caption.
/// `tip` is shown on hover.
pub fn legend(items: Vec<(gpui::Hsla, SharedString, SharedString)>) -> AnyElement {
    h_flex()
        .gap(px(12.))
        .children(
            items
                .into_iter()
                .enumerate()
                .map(|(i, (color, label, tip))| {
                    h_flex()
                        .id(("legend", i))
                        .items_center()
                        .gap(px(5.))
                        .tooltip(wrap_tooltip(tip))
                        .child(
                            // Short bar, same height as the meter, so a mid grey
                            // and a dark grey stay distinguishable — 6px dots of
                            // `text_dim` / `text_faint` collapsed into one.
                            div()
                                .w(px(10.))
                                .h(px(6.))
                                .rounded(px(2.))
                                .flex_none()
                                .bg(color),
                        )
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(theme::text_dim())
                                .child(label),
                        )
                }),
        )
        .into_any_element()
}

/// One "label ……… value" line with the design's hairline underneath.
/// One key/value line. `last` suppresses the rule underneath — a separator on
/// the final row has nothing to separate and lands on the block's own edge.
pub fn kv_row(k: impl Into<SharedString>, v: impl Into<SharedString>, last: bool) -> AnyElement {
    h_flex()
        .justify_between()
        .py(px(5.))
        .when(!last, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .text_size(px(11.))
        .text_color(theme::text_muted())
        .child(div().child(k.into()))
        .child(
            div()
                .font_family(font::MONO)
                .font_weight(gpui::FontWeight::NORMAL)
                .text_color(theme::text())
                .child(v.into()),
        )
        .into_any_element()
}

/// The design lays key/value pairs out two-up. gpui has no CSS grid, so this
/// splits the list into two columns by hand.
pub fn kv_columns(rows: Vec<(String, String)>) -> AnyElement {
    let mid = rows.len().div_ceil(2);
    let (left, right) = rows.split_at(mid.min(rows.len()));
    h_flex()
        .gap(px(14.))
        .mt(px(10.))
        .child(
            v_flex().flex_1().min_w_0().children(
                left.iter()
                    .enumerate()
                    .map(|(i, (k, v))| kv_row(k.clone(), v.clone(), i + 1 == left.len())),
            ),
        )
        .child(
            v_flex().flex_1().min_w_0().children(
                right
                    .iter()
                    .enumerate()
                    .map(|(i, (k, v))| kv_row(k.clone(), v.clone(), i + 1 == right.len())),
            ),
        )
        .into_any_element()
}

/// The headline figure with its unit trailing, e.g. "42" + "%".
pub fn big_number(
    value: impl Into<SharedString>,
    unit: impl Into<SharedString>,
    size: f32,
) -> AnyElement {
    h_flex()
        .items_baseline()
        .gap(px(3.))
        .child(
            div()
                .font_family(font::MONO)
                .text_size(px(size))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::text())
                .child(value.into()),
        )
        .child(
            div()
                .font_family(font::MONO)
                .text_size(px(size * 0.55))
                .font_weight(gpui::FontWeight::NORMAL)
                .text_color(theme::text_dim())
                .child(unit.into()),
        )
        .into_any_element()
}

/// A card explaining why there is nothing to show. The design is careful to
/// distinguish "collection is off" from "collection ran and found nothing",
/// so callers pass their own wording.
pub fn empty_card(title: impl Into<SharedString>, body: impl Into<SharedString>) -> AnyElement {
    card()
        .pt(px(16.))
        .pb(px(16.))
        .child(
            div()
                .text_size(px(13.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title.into()),
        )
        .child(
            div()
                .mt(px(6.))
                .text_size(px(11.))
                .text_color(theme::text_muted())
                .child(body.into()),
        )
        .into_any_element()
}

/// Small caption used at the bottom of several cards.
pub fn note(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_size(px(10.))
        .text_color(theme::text_dim())
        .child(text.into())
        .into_any_element()
}
