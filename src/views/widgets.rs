//! Pieces the design repeats across all eight views.
//!
//! Everything here is a plain builder returning an element — the design's
//! cards, pills, meters and key/value rows are div + radius + a width
//! percentage, so there is nothing to gain from real components.

use crate::appicon;
use crate::font;
use crate::format;
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, AnyView, App, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px, relative,
};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, IconName, Sizable, Size, h_flex, v_flex};

/// How much of the ink a tip's second tier keeps. Readable, but
/// plainly the smaller print under the sentence above it.
const TIP_DETAIL_OPACITY: f32 = 0.66;

/// Long-copy tooltip: wraps inside the 320px panel and uses caption size
/// instead of the default `text_sm` one-liner.
///
/// A tip may be two tiers, split by a blank line in the locale string
/// (`\n\n`): first a sentence anyone can read — what this is, what to
/// do about it — then the precise account in dimmer ink. Until
/// 2026-10-12 every tip was only its second tier, and the first words
/// someone met on hovering "Top apps" were "Each row is a launchd child
/// plus every descendant". The split is in the string so a translator
/// decides where it falls; separate children because gpui lays one
/// div's text out as a single run and would draw the break as a space.
pub fn wrap_tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let tiers: Vec<SharedString> = text
        .into()
        .split("\n\n")
        .map(|tier| SharedString::from(tier.to_string()))
        .collect();
    move |window, cx| {
        let tiers = tiers.clone();
        Tooltip::element(move |_, _| {
            v_flex()
                .max_w(px(220.))
                .gap(px(6.))
                .text_size(px(11.))
                .line_height(relative(1.35))
                .children(tiers.iter().enumerate().map(|(i, tier)| {
                    div()
                        .whitespace_normal()
                        .when(i > 0, |d| d.opacity(TIP_DETAIL_OPACITY))
                        .child(tier.clone())
                }))
        })
        .max_w(px(232.))
        .text_size(px(11.))
        .py(px(4.))
        .build(window, cx)
    }
}

/// [`wrap_tooltip`] with one line per entry, for a readout that is a
/// list rather than a sentence — the band's stretches, where three
/// ranges joined by separators read as one run of digits.
///
/// Separate children, not `\n` in one string: gpui lays a div's text
/// out as a single run and would render the newline as a space.
pub fn wrap_tooltip_lines(
    lines: Vec<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + use<> {
    move |window, cx| {
        let lines = lines.clone();
        Tooltip::element(move |_, _| {
            v_flex()
                .max_w(px(220.))
                .gap(px(2.))
                .text_size(px(11.))
                .line_height(relative(1.35))
                .children(
                    lines
                        .iter()
                        .map(|line| div().whitespace_normal().child(line.clone())),
                )
        })
        .max_w(px(232.))
        .text_size(px(11.))
        .py(px(4.))
        .build(window, cx)
    }
}

/// Give a control a *wrapping* tooltip when it cannot build one itself.
///
/// gpui-kit's `Button::tooltip` takes a plain string and renders
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

/// The 1px card outline, painted as a zero-blur inset shadow instead of
/// a border. Not cosmetic hair-splitting: gpui borders occupy layout
/// space, so a real border made every light card 2px taller than its
/// dark twin — a window sized snugly under one theme scrolled under the
/// other. A shadow costs no layout, and `inset` draws it exactly where
/// a border would sit; the two themes produce pixel-identical geometry.
///
/// Both themes now, not just light: since the cards went nearly solid,
/// their separation from the glass frame is a colour difference, and on
/// a wallpaper that happens to match the card (black, for dark) that
/// difference vanishes. The hairline is the separation that owes the
/// wallpaper nothing. `theme::card_line()` picks the ink per theme —
/// and is no ink at all in golden dark, whose ground is opaque and
/// whose cards are told from it by lightness.
fn outline(d: Div) -> Div {
    d.shadow(vec![gpui::BoxShadow {
        color: gpui::Hsla::from(theme::card_line()),
        offset: gpui::point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(1.),
        inset: true,
    }])
}

/// A card's inner margin, and the side margin of every row in a
/// [`list_shell`], which has none of its own so a row's hover fill can
/// reach the card's edge. One number so the two cannot drift: the rows
/// were written as 13 when [`card`] was 13 and stayed there when it
/// went to 12, and on Hardware the Drives title stood a point right of
/// the GPU title under it.
pub const CARD_PAD: f32 = 12.;

/// Settings-style grouped container: fill, hairline outline.
pub fn card() -> Div {
    outline(v_flex().rounded(px(12.)).bg(theme::surface())).p(px(CARD_PAD))
}

/// Same grouping, no inner padding — lists paint their own rows to the edges.
pub fn list_shell() -> Div {
    outline(v_flex().rounded(px(12.)).bg(theme::surface())).overflow_hidden()
}

/// Card title on the left, an optional status pill on the right.
///
/// The title does not shrink; the right slot does (`min_w_0`). Processor
/// puts a long chip name there, and without this the name shoved the
/// live-watts figure off the card — `overflow_y_scroll` on the panel
/// body also clips x.
pub fn card_header(title: impl Into<SharedString>, right: Option<AnyElement>) -> AnyElement {
    h_flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .child(
            div()
                .flex_none()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title.into()),
        )
        .children(right.map(|el| div().min_w_0().child(el)))
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
        .px(px(CARD_PAD))
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

/// [`info_icon`] carrying more than one paragraph — one ⓘ per card
/// header, with each note its own block, rather than two icons a reader
/// has to tell apart.
pub fn info_icon_lines(id: impl Into<ElementId>, lines: Vec<SharedString>) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .tooltip(wrap_tooltip_lines(lines))
        .child(
            Icon::new(IconName::Info)
                .with_size(Size::Size(px(11.)))
                .text_color(gpui::Hsla::from(theme::text_dim())),
        )
        .into_any_element()
}

/// A row's mark: the icon of the application the process runs from
/// (`appicon::for_pid`, drawn once per app and kept) — for an app's
/// tree, its root — and the name's first letter in a tile where there
/// is none: a shell, a command-line tool, a daemon. Either way the rows
/// have different left edges to find again on the next glance.
/// Overview's apps card, the Apps tab and the Processes tab lead their
/// rows with it. A gear for the daemons and a terminal for the tools was
/// proposed for the letter's place and not taken: nothing here can tell
/// one from the other reliably, and a glyph that guesses is worse than a
/// letter that does not.
pub fn app_mark(pid: u32, name: &str, title: &str) -> AnyElement {
    match appicon::for_pid(pid, name) {
        Some(icon) => gpui::img(icon)
            .flex_none()
            .size(px(MARK_SIZE))
            .into_any_element(),
        None => monogram(title),
    }
}

/// The edge of a row's mark, icon or tile.
const MARK_SIZE: f32 = 22.;

/// The letter a row falls back to: the name's first letter in a small
/// tile, where an app icon would be.
fn monogram(name: &str) -> AnyElement {
    let letter: String = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    div()
        .flex()
        .flex_none()
        .size(px(MARK_SIZE))
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .bg(theme::track())
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::text_muted())
        .child(letter)
        .into_any_element()
}

/// The mark that says a row opens: a chevron at the start of the row's
/// second line, turned down while it is open. A row's only sign that a
/// click did something was the hover fill, which shows where the
/// pointer is and nothing about what a click would do — the expansion,
/// with the thresholds and the Quit in it, was found by accident or not
/// at all. Dim on purpose: fifty of them down a list must not out-shout
/// the figures. It stood beside the figure until 2026-10-16, where
/// "› 34.4%" read as "more than 34.4%"; at the head of "26 processes"
/// it sits where a disclosure sits in Finder, beside what it discloses.
pub fn expand_mark(open: bool) -> AnyElement {
    Icon::new(if open {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    })
    .with_size(Size::Size(px(10.)))
    .text_color(gpui::Hsla::from(if open {
        theme::text_muted()
    } else {
        theme::text_dim()
    }))
    .into_any_element()
}

/// A name that may ellipsis at 320px. Short names skip the tooltip —
/// hovering "Finder" to read "Finder" is noise — and the cutoff is
/// characters, the same proxy [`KV_TIP_FROM`] uses.
const NAME_TIP_FROM: usize = 16;

/// Truncating label with a hover-to-read-the-tail tooltip when the
/// string is long enough that the ellipsis is likely real.
///
/// Every caller passes a process or app name, so a reverse-DNS one is
/// shown as its last label (`format::display_name`) and the tooltip gives
/// the whole identifier back.
pub fn truncating_name(
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    size: f32,
    weight: FontWeight,
    color: gpui::Hsla,
) -> AnyElement {
    let full: SharedString = name.into();
    let shown = format::display_name(&full);
    let shortened = shown.len() != full.len();
    let name = if shortened {
        SharedString::from(shown.to_string())
    } else {
        full.clone()
    };
    let long = shortened || name.chars().count() >= NAME_TIP_FROM;
    // Content-sized, not `flex_1`: every caller sits this at the left
    // of a `justify_between` row, where growing changes nothing — but a
    // sibling *inside* the left group (History's inline pid) must hug
    // the name, not be pushed to the group's far edge by a greedy name.
    // `min_w_0` + default shrink keep the truncation when space runs
    // out, and the long-name tooltip now triggers on the text itself
    // rather than on the empty stretch beside it.
    div()
        .id(id)
        .min_w_0()
        .text_size(px(size))
        .font_weight(weight)
        .text_color(color)
        .truncate()
        .when(long, |d| d.tooltip(wrap_tooltip(full.clone())))
        .child(name)
        .into_any_element()
}

/// [`truncating_name`] with a muted tail that survives the ellipsis:
/// `Zed · cargo` keeps `· cargo` when the title has to truncate, because
/// the tail is the news and the title is what the reader already knows.
/// Two runs in one row rather than one string, so the tail can take a
/// second colour — gpui lays nested divs out as blocks, not inline text.
pub fn truncating_name_tailed(
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    tail: Option<SharedString>,
    size: f32,
    weight: FontWeight,
    color: gpui::Hsla,
) -> AnyElement {
    let name: SharedString = name.into();
    let Some(tail) = tail else {
        return truncating_name(id, name, size, weight, color);
    };
    let full: SharedString = format!("{name} · {tail}").into();
    let shown = format::display_name(&name);
    let shortened = shown.len() != name.len();
    let name = SharedString::from(shown.to_string());
    let long = shortened || full.chars().count() >= NAME_TIP_FROM;
    h_flex()
        .id(id)
        .flex_1()
        .min_w_0()
        .items_baseline()
        .gap(px(4.))
        .text_size(px(size))
        .font_weight(weight)
        .when(long, |d| d.tooltip(wrap_tooltip(full.clone())))
        .child(div().min_w_0().truncate().text_color(color).child(name))
        .child(
            div()
                .flex_none()
                .font_weight(FontWeight::NORMAL)
                .text_color(theme::text_muted())
                .child(format!("· {tail}")),
        )
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
        .bg(theme::track())
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
        .bg(theme::track())
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
/// full text. A proxy for "might be truncated": the value is digits and
/// a unit, so character count tracks pixels closely enough. Calibrated
/// for the WORST case, not the default, and on the mono face these
/// values wore until 2026-10-12 — at the panel's minimum width (320px)
/// a half column beside the widest label fit ~12–13 of its characters;
/// the UI face is narrower, which only moves the error the safe way. The error direction is safe by construction
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
        // Label yields. A long flex_none label used to squeeze the
        // figure into an ellipsis. A long parent name still truncates
        // — the value is capped at half the cell.
        .child(div().flex_1().min_w_0().truncate().child(k.clone()))
        .child(
            div()
                .id(SharedString::from(format!("kv-{k}")))
                .flex_none()
                .max_w(relative(0.55))
                .min_w_0()
                .truncate()
                .font_features(font::tnum())
                .font_weight(FontWeight::NORMAL)
                .text_color(theme::text_for(hot))
                .when(long, |d| d.tooltip(wrap_tooltip(v.clone())))
                .child(v),
        )
        .into_any_element()
}

/// The design lays key/value pairs out two-up. gpui has no CSS grid, so this
/// splits the list into two columns by hand.
pub fn kv_columns(rows: Vec<(String, String)>) -> AnyElement {
    kv_pairs(rows.into_iter().map(|(k, v)| (k, v, false)).collect())
}

/// One row of packed pairs: label sits next to its figure, then a gap,
/// then the next pair. For a single line there is no column of numbers
/// to align, so [`kv_row`]'s right-pin would leave a hole (Compressed
/// `8.8 GB` hanging on the card edge). Multi-row two-up still uses
/// [`kv_pairs`].
///
/// `trailing` hugs the right edge on the pairs' baseline and is what
/// gives way when the row is tight — a remark about the row that is
/// there some of the time, without costing a line when it is.
pub fn kv_packed(rows: Vec<(String, String, bool)>, trailing: Option<AnyElement>) -> AnyElement {
    h_flex()
        .w_full()
        .mt(px(10.))
        .items_baseline()
        .justify_between()
        .gap(px(12.))
        .child(kv_packed_pairs(rows))
        .children(trailing)
        .into_any_element()
}

fn kv_packed_pairs(rows: Vec<(String, String, bool)>) -> AnyElement {
    h_flex()
        .flex_none()
        .gap(px(16.))
        .children(rows.into_iter().enumerate().map(|(i, (k, v, hot))| {
            h_flex()
                .id(("kv-packed", i))
                .gap(px(8.))
                .items_baseline()
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(theme::text_muted())
                        .child(k),
                )
                .child(
                    div()
                        .flex_none()
                        .font_features(font::tnum())
                        .text_size(px(11.))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(theme::text_for(hot))
                        .child(v),
                )
        }))
        .into_any_element()
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

/// A card's headline figure and the unit beside it: the figure large,
/// what it is counted in a size and an ink down. They were one run in
/// one ink ("19 GB"), which gave the unit the figure's weight, and the
/// percent sign was a mono glyph at half size, a space away and below
/// the line.
const HEADLINE: f32 = 20.;
const HEADLINE_UNIT: f32 = 13.;

/// The headline figure with a sign trailing it, e.g. "42" + "%". The
/// sign sits against the figure, as a percent does in a sentence.
pub fn big_number(value: impl Into<SharedString>, unit: impl Into<SharedString>) -> AnyElement {
    headline(value.into(), unit.into(), 1.)
}

/// [`big_number`] for a figure `format` has already joined to its unit
/// ("19 GB", "4.5 TB"): split at the space, nothing recomputed, and the
/// unit a word apart. A text with no unit — the placeholder — is the
/// figure alone.
pub fn big_figure(text: &str) -> AnyElement {
    let (value, unit) = text.rsplit_once(' ').unwrap_or((text, ""));
    headline(value.to_string().into(), unit.to_string().into(), 4.)
}

fn headline(value: SharedString, unit: SharedString, gap: f32) -> AnyElement {
    h_flex()
        .flex_none()
        .items_baseline()
        .gap(px(gap))
        .child(
            div()
                .font_features(font::tnum())
                .text_size(px(HEADLINE))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(value),
        )
        .child(
            div()
                .text_size(px(HEADLINE_UNIT))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_muted())
                .child(unit),
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
