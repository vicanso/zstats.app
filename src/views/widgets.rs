//! Pieces the design repeats across all eight views.
//!
//! Everything here is a plain builder returning an element — the design's
//! cards, pills, meters and key/value rows are div + radius + a width
//! percentage, so there is nothing to gain from real components.

use crate::font;
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AnyView, App, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px, relative,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName, Sizable, Size, h_flex, v_flex};

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

/// Give a control a *wrapping* tooltip when it cannot build one itself.
///
/// gpui-component's `Button::tooltip` takes a plain string and renders
/// it on a single line. That is right for "Reveal in Finder" and wrong
/// for a sentence: at fifty-odd characters the tooltip grows wider than
/// the 320px panel it is explaining, and the tail runs off screen. The
/// button keeps its own chrome; the tooltip moves to a wrapper that can
/// carry [`wrap_tooltip`].
pub fn with_wrap_tooltip(
    id: impl Into<ElementId>,
    text: impl Into<SharedString> + 'static,
    control: impl IntoElement + 'static,
) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .tooltip(wrap_tooltip(text))
        .child(control)
        .into_any_element()
}

/// Light mode's 1px card outline, painted as a zero-blur inset shadow
/// instead of a border. Not cosmetic hair-splitting: gpui borders occupy
/// layout space, so a real border made every light card 2px taller than
/// its dark twin — a window sized snugly under one theme scrolled under
/// the other. A shadow costs no layout, and `inset` draws it exactly
/// where the old border sat; the two themes now produce pixel-identical
/// geometry.
fn light_outline(d: Div) -> Div {
    d.shadow(vec![gpui::BoxShadow {
        color: gpui::Hsla::from(theme::border()),
        offset: gpui::point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(1.),
        inset: true,
    }])
}

/// Settings-style grouped container: fill, no hard outline.
pub fn card() -> Div {
    v_flex()
        .rounded(px(12.))
        .bg(theme::surface())
        .when(!theme::is_dark(), light_outline)
        .px(px(13.))
        .pt(px(12.))
        .pb(px(13.))
}

/// Same grouping, no inner padding — lists paint their own rows to the edges.
pub fn list_shell() -> Div {
    v_flex()
        .rounded(px(12.))
        .bg(theme::surface())
        .when(!theme::is_dark(), light_outline)
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
///
/// The title is an element, not a string, so a card can put an
/// [`info_icon`] beside its own name; `String`, `&str` and
/// `SharedString` all satisfy it unchanged.
///
/// The title shrinks and the right slot does not: History used to clip
/// `Reload` because this row had no `min_w_0` and the shell clips overflow.
pub fn list_header(title: impl IntoElement, right: Option<AnyElement>) -> AnyElement {
    h_flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .px(px(13.))
        .pt(px(11.))
        .pb(px(9.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title),
        )
        .children(right.map(|el| div().flex_none().child(el)))
        .into_any_element()
}

/// A quiet ⓘ next to a card's title, carrying an explanation on hover.
///
/// For the fact a reader only needs once — what a number *means*, why
/// two cards can disagree about the same program — where a permanent
/// note block would be a paragraph of chrome on every open. Same
/// wrapping tooltip as everywhere else, so a sentence stays inside the
/// panel.
pub fn info_icon(id: impl Into<ElementId>, tip: impl Into<SharedString> + 'static) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .tooltip(wrap_tooltip(tip))
        .child(
            Icon::new(IconName::Info)
                .with_size(Size::Size(px(11.)))
                .text_color(gpui::Hsla::from(theme::text_dim())),
        )
        .into_any_element()
}

/// A name that may ellipsis at 320px. Short names skip the tooltip —
/// hovering "Finder" to read "Finder" is noise — and the cutoff is
/// characters, the same proxy [`KV_TIP_FROM`] uses.
const NAME_TIP_FROM: usize = 16;

/// Truncating label with a hover-to-read-the-tail tooltip when the
/// string is long enough that the ellipsis is likely real.
pub fn truncating_name(
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    size: f32,
    weight: FontWeight,
    color: gpui::Hsla,
) -> AnyElement {
    let name = name.into();
    let long = name.chars().count() >= NAME_TIP_FROM;
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .text_size(px(size))
        .font_weight(weight)
        .text_color(color)
        .truncate()
        .when(long, |d| d.tooltip(wrap_tooltip(name.clone())))
        .child(name)
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

/// How a legend chip is drawn. Fill matches a painted meter slice;
/// Hollow matches leftover track — empty, not another colour.
pub enum LegendMark {
    Fill(gpui::Hsla),
    Hollow,
}

/// Colour key under a stacked meter: a short bar plus a caption.
/// `tip` is shown on hover.
pub fn legend(items: Vec<(LegendMark, SharedString, SharedString)>) -> AnyElement {
    h_flex()
        .gap(px(12.))
        .children(
            items
                .into_iter()
                .enumerate()
                .map(|(i, (mark, label, tip))| {
                    h_flex()
                        .id(("legend", i))
                        .items_center()
                        .gap(px(5.))
                        .tooltip(wrap_tooltip(tip))
                        .child(legend_swatch(mark))
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

fn legend_swatch(mark: LegendMark) -> AnyElement {
    match mark {
        LegendMark::Fill(color) => div()
            .w(px(10.))
            .h(px(6.))
            .rounded(px(2.))
            .flex_none()
            .bg(color)
            .into_any_element(),
        LegendMark::Hollow => div()
            .w(px(10.))
            .h(px(6.))
            .rounded(px(2.))
            .flex_none()
            .border_1()
            .border_color(theme::text_dim())
            .into_any_element(),
    }
}

/// Above this many characters a kv value gets a hover tooltip with the
/// full text. A proxy for "might be truncated", and a sound one: the
/// value renders in the mono face, so character count maps linearly to
/// pixels. Calibrated for the WORST case, not the default: at the
/// panel's minimum width (320px) a half column beside the widest label
/// fits ~12–13 mono chars. The error direction is safe by construction
/// — a long-but-fitting value shows a redundant tooltip (harmless); a
/// truncated value without one would lose the only path to the tail.
const KV_TIP_FROM: usize = 12;

/// One "label ……… value" line with the design's hairline underneath.
/// `last` suppresses the rule — a separator on the final row has
/// nothing to separate and lands on the block's own edge.
///
/// The value is right-aligned in the leftover width so two side-by-side
/// cells with different value lengths (Swap's `6.0 GB / 7.0 GB` next to
/// Compressed's `6.4 GB`) still share an edge, instead of the short one
/// sitting next to its label while the long one fills the column.
pub fn kv_row(
    k: impl Into<SharedString>,
    v: impl Into<SharedString>,
    last: bool,
    hot: bool,
) -> AnyElement {
    let k = k.into();
    let v = v.into();
    let long = v.chars().count() >= KV_TIP_FROM;
    h_flex()
        .justify_between()
        .gap(px(8.))
        .py(px(5.))
        .when(!last, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .text_size(px(11.))
        .text_color(theme::text_muted())
        .child(div().flex_none().child(k.clone()))
        .child(
            h_flex().flex_1().min_w_0().justify_end().child(
                // A value that outgrows its half-width column (a parent
                // process name, a long user) truncates instead of spilling
                // over the label and the column edge; the tooltip carries
                // what the ellipsis ate.
                div()
                    .id(SharedString::from(format!("kv-{k}")))
                    .min_w_0()
                    .truncate()
                    .font_family(font::MONO)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme::text_for(hot))
                    .when(long, |d| d.tooltip(wrap_tooltip(v.clone())))
                    .child(v),
            ),
        )
        .into_any_element()
}

/// The design lays key/value pairs out two-up. gpui has no CSS grid, so this
/// splits the list into two columns by hand.
pub fn kv_columns(rows: Vec<(String, String)>) -> AnyElement {
    kv_pairs(rows.into_iter().map(|(k, v)| (k, v, false)).collect())
}

/// [`kv_columns`] with a per-cell colour flag. Display only — `hot` feeds
/// [`theme::text_for`], it does not produce an alert.
pub fn kv_pairs(rows: Vec<(String, String, bool)>) -> AnyElement {
    let mid = rows.len().div_ceil(2);
    let (left, right) = rows.split_at(mid.min(rows.len()));
    h_flex()
        .gap(px(14.))
        .mt(px(10.))
        .child(
            v_flex().flex_1().min_w_0().children(
                left.iter().enumerate().map(|(i, (k, v, hot))| {
                    kv_row(k.clone(), v.clone(), i + 1 == left.len(), *hot)
                }),
            ),
        )
        .child(
            v_flex().flex_1().min_w_0().children(
                right.iter().enumerate().map(|(i, (k, v, hot))| {
                    kv_row(k.clone(), v.clone(), i + 1 == right.len(), *hot)
                }),
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
