//! The building blocks every page is made of: the gpui front end's widgets
//! (`views/widgets.rs`, `views::MenuTrigger`, the option chips), ported
//! with their sizes, inks and rules — `.claude/skills/ui-polish`, and the
//! comments on the gpui originals, which say why each is the way it is
//! and are not repeated here. What differs is how NSH draws, not what is
//! drawn:
//!
//! - **Colours carry both halves** (`theme::color`), so a widget made once
//!   follows the appearance; a colour that depends on a value the widget
//!   is told reactively (a chip's `on`) is a closure NSH re-reads.
//! - **The card's hairline is an outline**, which takes no layout, where
//!   gpui needed a zero-blur inset shadow for the same reason: a theme
//!   switch changes paint, never geometry.
//! - **Icons are SVGs drawn as images and tinted** — the bundled ones
//!   (`assets::CustomIconName`), as `data:` URIs NSH renders at the
//!   view's size. NSH has no icon set; a page brings the glyphs it uses
//!   into `assets/icons`.
//! - **The mono face is registered with each root** (`root::make`), from
//!   the bundled files ([`fonts`]).

use crate::appicon::{self, Icon};
use crate::assets::{self, CustomIconName};
use crate::color::{Rgba, rgba};
use crate::nsh::root::{self, CARD_PAD};
use crate::nsh::theme;
use crate::{format, i18n, palette};
use nsh_base::{MenuEvent, TooltipElement, alert_dialog, menu, menu_item, tooltip};
use nsh_core::ui::prelude::*;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex};

/// The bundled mono face: rates and code-like identifiers (gpui's
/// `font.rs` had the rule, which design.md keeps).
pub const MONO: &str = "JetBrains Mono";

/// The bundled faces' files, for a root to register (`Root::font`).
pub fn fonts() -> Vec<Arc<[u8]>> {
    [
        "fonts/JetBrainsMono-Regular.ttf",
        "fonts/JetBrainsMono-Bold.ttf",
    ]
    .into_iter()
    .filter_map(|path| {
        let bytes = assets::get(path);
        if bytes.is_none() {
            tracing::error!("missing bundled font: {path}");
        }
        bytes.map(|bytes| Arc::from(bytes.as_ref()))
    })
    .collect()
}

/// A bundled SVG as a `data:` URI, made once per glyph.
fn icon_uri(name: CustomIconName) -> SharedString {
    static URIS: LazyLock<Mutex<HashMap<CustomIconName, SharedString>>> =
        LazyLock::new(Mutex::default);
    let mut uris = URIS.lock().unwrap_or_else(|e| e.into_inner());
    uris.entry(name)
        .or_insert_with(|| {
            let svg = assets::get(name.path()).unwrap_or_default();
            format!("data:image/svg+xml,{}", percent_encode(&svg)).into()
        })
        .clone()
}

/// RFC 3986's unreserved bytes as they are, every other byte escaped.
fn percent_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A bundled glyph, `size` points square, in `ink` — read again when
/// what it reads changes (a tab turning red).
pub fn icon(name: CustomIconName, size: f32, ink: impl Fn() -> Rgba + 'static) -> AnyElement {
    image(icon_uri(name))
        .flex_none()
        .w(px(size))
        .h(px(size))
        .tint_color(move || theme::color(&ink))
        .into_element()
}

/// The edge of a row's mark, icon or tile (`widgets::MARK_SIZE`).
const MARK_SIZE: f32 = 22.;

/// A row's mark: the icon of the application the process runs from
/// (`appicon`), or the name's first letter in a tile where there is none
/// — a shell, a command-line tool, a daemon (`widgets::app_mark`).
pub fn app_mark(pid: u32, name: &str, title: &str) -> AnyElement {
    mark(appicon::icon_for_pid(pid, name), title)
}

/// [`app_mark`] for a row read back from the records — History's: the
/// icon only while the pid still runs a program of the recorded name
/// (`appicon::icon_for_record`), the letter otherwise.
pub fn record_mark(pid: u32, name: &str, title: &str) -> AnyElement {
    mark(appicon::icon_for_record(pid, name), title)
}

fn mark(icon: Option<Icon>, title: &str) -> AnyElement {
    match icon {
        Some(icon) => image(SharedString::from(icon.data_uri))
            .flex_none()
            .w(px(MARK_SIZE))
            .h(px(MARK_SIZE))
            .into_element(),
        None => monogram(title),
    }
}

/// The letter a row falls back to, in a small tile where an icon would be.
fn monogram(name: &str) -> AnyElement {
    let letter: String = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    view()
        .flex_none()
        .w(px(MARK_SIZE))
        .h(px(MARK_SIZE))
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .bg(theme::color(palette::track))
        .child(
            text(letter)
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::color(palette::text_muted)),
        )
        .into_element()
}

/// A quiet ⓘ beside a card's title, carrying an explanation on hover
/// (`widgets::info_icon`).
pub fn info_icon(tip: TooltipElement) -> AnyElement {
    view()
        .flex_none()
        .tooltip(tip)
        .child(icon(CustomIconName::Info, 11., palette::text_dim))
        .into_element()
}

/// A link in the page's own voice: a word and a chevron, in `ink`, with
/// the chip's fill under the pointer (`overview::quiet_link`).
pub fn quiet_link(
    label: impl IntoReactive<SharedString>,
    tip: TooltipElement,
    ink: impl Fn() -> Rgba + Copy + 'static,
    go: impl Fn() + 'static,
) -> AnyElement {
    view()
        .flex_none()
        .tooltip(tip)
        .child(
            nsh_base::button()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .rounded(px(7.))
                .px(px(8.))
                .py(px(4.))
                .hover(|s| s.bg(theme::color(palette::chip)))
                .on_press(go)
                .child(
                    text(label)
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(move || theme::color(ink)),
                )
                .child(icon(CustomIconName::ChevronRight, 12., ink)),
        )
        .into_element()
}

/// A card that says why there is nothing to show (`widgets::empty_card`).
pub fn empty_card(title: String, body: String) -> AnyElement {
    card()
        .pt(px(16.))
        .pb(px(16.))
        .child(
            text(title)
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::color(palette::text)),
        )
        .child(
            text(body)
                .mt(px(6.))
                .text_size(px(11.))
                .text_color(theme::color(palette::text_muted)),
        )
        .into_element()
}

/// The mark that says a row opens: a chevron at the head of its second
/// line, turned down while `open`, dim (`widgets::expand_mark`).
pub fn expand_mark(open: impl Fn() -> bool + 'static) -> AnyElement {
    dynamic(move || {
        if open() {
            icon(CustomIconName::ChevronDown, 10., palette::text_muted)
        } else {
            icon(CustomIconName::ChevronRight, 10., palette::text_dim)
        }
    })
}

/// A grouped container: the card fill, its hairline (`palette::card_line`,
/// none on Golden Dark), the card's padding.
pub fn card() -> View {
    shell().p(px(CARD_PAD))
}

/// [`card`] without the padding: a list's rows run to its edges and pad
/// themselves by [`CARD_PAD`].
pub fn list_shell() -> View {
    shell().overflow_hidden()
}

fn shell() -> View {
    view()
        .rounded(px(12.))
        .bg(theme::color(palette::surface))
        .outline(px(1.))
        .outline_offset(px(-1.))
        .outline_color(theme::color(palette::card_line))
}

/// The line height gpui gave a text that set none — the font's size times
/// φ — to the nearest whole point. NSH's own default is tighter, so an
/// element ported from one whose gpui twin sets no line height takes
/// this, or its rows come out shorter than the page they port: the long
/// Overview stood 46 short without it. Whole points because NSH rounds a
/// text's measured height up to one (`measure_text`), where gpui kept
/// the fraction: φ times 10 became 17 against gpui's 16.2, and over a
/// page of such rows the long Overview ran 5 past its height and
/// scrolled. In points rather than `relative(..)`, which NSH 0.2.0 drops
/// without a word (it is written out as a percentage, and the line
/// height's parser refuses one).
pub fn line(size: f32) -> Points {
    px((size * 1.618).round())
}

/// A card's title, and on its right an optional element that gives way
/// before the title does.
pub fn card_header(title: impl Into<SharedString>, right: Option<AnyElement>) -> View {
    let row = view()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .child(
            text(title.into())
                .flex_none()
                .text_size(px(12.))
                .line_height(line(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::color(palette::text)),
        );
    match right {
        Some(right) => row.child(view().min_w(px(0.)).child(right)),
        None => row,
    }
}

/// `widgets::HEADLINE` and `HEADLINE_UNIT`.
const HEADLINE: f32 = 20.;
const HEADLINE_UNIT: f32 = 13.;

/// Nothing painted: a fill that comes and goes with a state.
pub const CLEAR: Rgba = rgba(0x00000000);

/// A [`list_shell`]'s title row: padded, no divider (rows carry the
/// hairlines). The title shrinks and the right slot does not
/// (`widgets::list_header`).
pub fn list_header(title: AnyElement, right: Option<AnyElement>) -> View {
    let row = view()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .px(px(CARD_PAD))
        .pt(px(11.))
        .pb(px(9.))
        .child(view().flex_1().min_w(px(0.)).child(title));
    match right {
        Some(right) => row.child(view().flex_none().child(right)),
        None => row,
    }
}

/// A list header's title: 12px, the first ink, cut short before its
/// neighbours are.
pub fn list_title(words: impl IntoReactive<SharedString>) -> Text {
    text(words)
        .number_of_lines(1u32)
        .text_size(px(12.))
        .line_height(line(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::color(palette::text))
}

/// A small caption at the foot of a card, or under a list's header
/// (`widgets::note`).
pub fn note(words: impl IntoReactive<SharedString>) -> Text {
    text(words)
        .text_size(px(10.))
        .line_height(line(10.))
        .text_color(theme::color(palette::text_dim))
}

/// A small filled tag on a row: "This app", "Daily disk check"
/// (`processes::pill`), in the accent's wash when `accent`.
pub fn pill(words: impl IntoReactive<SharedString>, accent: bool) -> View {
    view().flex_none().child(
        text(words)
            .rounded(px(3.))
            .px(px(4.))
            .bg(theme::color(move || {
                if accent {
                    palette::accent_wash(14)
                } else {
                    palette::inset()
                }
            }))
            .text_size(px(9.))
            .line_height(line(9.))
            .text_color(theme::color(move || {
                if accent {
                    palette::accent_light()
                } else {
                    palette::tiny_label(palette::text_muted())
                }
            })),
    )
}

/// `widgets::NAME_TIP_FROM`: a name this long is likely cut short, and
/// earns the whole of it on hover.
const NAME_TIP_FROM: usize = 16;

/// A process or app name that may be cut short, shown as
/// `format::display_name` gives it, with a muted tail that survives the
/// cut — `Zed · cargo` keeps `· cargo`, the news (`truncating_name` and
/// `truncating_name_tailed`); the whole name on hover when it is likely
/// cut. Bound to what `name` and `tail` read.
pub fn name_tailed(
    name: impl Fn() -> String + Clone + 'static,
    tail: impl Fn() -> Option<String> + Clone + 'static,
    size: f32,
    weight: FontWeight,
) -> View {
    let full = {
        let (name, tail) = (name.clone(), tail.clone());
        move || match tail() {
            Some(tail) => format!("{} · {tail}", name()),
            None => name(),
        }
    };
    let long = {
        let (name, full) = (name.clone(), full.clone());
        move || {
            let name = name();
            format::display_name(&name).len() != name.len()
                || full().chars().count() >= NAME_TIP_FROM
        }
    };
    let has_tail = {
        let tail = tail.clone();
        move || tail().is_some()
    };
    view()
        .flex_row()
        .flex_1()
        .min_w(px(0.))
        .items_baseline()
        .gap(px(4.))
        .tooltip(live_tooltip(full).disabled(move || !long()))
        .child(
            text(move || format::display_name(&name()).to_owned())
                .flex_shrink(1.)
                .number_of_lines(1u32)
                .text_size(px(size))
                .line_height(line(size))
                .font_weight(weight)
                .text_color(theme::color(palette::text)),
        )
        .child(when(has_tail, move || {
            let tail = tail.clone();
            text(move || format!("· {}", tail().unwrap_or_default()))
                .flex_none()
                .text_size(px(size))
                .line_height(line(size))
                .text_color(theme::color(palette::text_muted))
        }))
}

/// A name that may be cut short, as wide as its words (`truncating_name`):
/// what follows it on the line hugs it rather than the line's far edge.
/// The whole name on hover when it is likely cut.
pub fn name(words: impl Fn() -> String + Clone + 'static, size: f32, weight: FontWeight) -> View {
    let long = {
        let words = words.clone();
        move || {
            let words = words();
            format::display_name(&words).len() != words.len()
                || words.chars().count() >= NAME_TIP_FROM
        }
    };
    view()
        .flex_shrink(1.)
        .min_w(px(0.))
        .tooltip(live_tooltip(words.clone()).disabled(move || !long()))
        .child(
            text(move || format::display_name(&words()).to_owned())
                .number_of_lines(1u32)
                .text_size(px(size))
                .line_height(line(size))
                .font_weight(weight)
                .text_color(theme::color(palette::text)),
        )
}

/// A body line in the mono face, unless the locale's words are CJK, which
/// the bundled mono face cannot draw.
pub fn mono_unless_cjk(words: Text) -> Text {
    if i18n::is_cjk() {
        words
    } else {
        words.font_family(MONO)
    }
}

/// gpui-kit's ghost icon button at its smallest (`Button::ghost().xsmall()`,
/// the Alerts cards' controls): 20 square, the kit's 6 corner, the glyph at
/// 12 in the first ink, a fill only under the pointer — the kit's accent
/// background, at half strength on a dark scheme as the kit paints it.
pub fn ghost_icon(
    name: CustomIconName,
    tip: TooltipElement,
    on_press: impl Fn() + 'static,
) -> AnyElement {
    view()
        .flex_none()
        .tooltip(tip)
        .child(
            nsh_base::button()
                .w(px(20.))
                .h(px(20.))
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .hover(|s| s.bg(theme::color(ghost_hover)))
                .on_press(on_press)
                .child(icon(name, 12., palette::text)),
        )
        .into_element()
}

fn ghost_hover() -> Rgba {
    let fill = palette::chip();
    if palette::is_dark() {
        Rgba {
            a: fill.a * 0.5,
            ..fill
        }
    } else {
        fill
    }
}

/// A card's "show more" toggle (`sensors::more_chip` and its siblings):
/// outlined while off, the chip's fill while on, and a hover only while
/// off — the "on" fill is the state.
pub fn more_chip(label: String, showing: bool, toggle: fn()) -> AnyElement {
    nsh_base::button()
        .flex_none()
        .rounded_full()
        .border(px(1.))
        .border_color(theme::color(move || {
            if showing {
                palette::border()
            } else {
                palette::border_subtle()
            }
        }))
        .bg(theme::color(
            move || if showing { palette::chip() } else { CLEAR },
        ))
        .hover(move |s| {
            if showing {
                s
            } else {
                s.bg(theme::color(palette::surface_raised))
                    .border_color(theme::color(palette::border))
            }
        })
        .px(px(7.))
        .py(px(1.))
        .on_press(toggle)
        .child(
            text(label)
                .text_size(px(9.))
                .line_height(line(9.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(move || {
                    if showing {
                        palette::text()
                    } else {
                        palette::text_dim()
                    }
                })),
        )
        .into_element()
}

/// A headline figure with its sign against it: "42" and "%".
pub fn big_number(
    value: impl IntoReactive<SharedString>,
    unit: impl IntoReactive<SharedString>,
) -> View {
    headline(value, unit, 1.)
}

/// A headline figure `format` already joined to its unit ("19 GB"), split
/// at the space and nothing recomputed; the unit a word apart. Bound to
/// `figure`, which is read again as what it reads changes.
pub fn big_figure(figure: impl Fn() -> String + Clone + 'static) -> View {
    let unit = figure.clone();
    headline(
        move || split_figure(&figure()).0,
        move || split_figure(&unit()).1,
        4.,
    )
}

/// [`big_figure`] with a word after the unit, in the unit's size and ink:
/// "121 GB free".
pub fn big_figure_with(figure: impl Fn() -> String + Clone + 'static, word: String) -> View {
    let unit = figure.clone();
    headline(
        move || split_figure(&figure()).0,
        move || match split_figure(&unit()).1 {
            unit if unit.is_empty() => word.clone(),
            unit => format!("{unit} {word}"),
        },
        4.,
    )
}

/// "19 GB" as ("19", "GB"); a text with no unit (the placeholder) is the
/// figure alone.
fn split_figure(text: &str) -> (String, String) {
    match text.rsplit_once(' ') {
        Some((value, unit)) => (value.to_owned(), unit.to_owned()),
        None => (text.to_owned(), String::new()),
    }
}

fn headline(
    value: impl IntoReactive<SharedString>,
    unit: impl IntoReactive<SharedString>,
    gap: f32,
) -> View {
    view()
        .flex_row()
        .flex_none()
        .items_baseline()
        .gap(px(gap))
        .child(
            text(value)
                .tabular_nums()
                .text_size(px(HEADLINE))
                .line_height(line(HEADLINE))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::color(palette::text)),
        )
        .child(
            text(unit)
                .text_size(px(HEADLINE_UNIT))
                .line_height(line(HEADLINE_UNIT))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(palette::text_muted)),
        )
}

/// A neutral outlined pill: "30s", a quiet status.
pub fn outline_pill(label: impl IntoReactive<SharedString>) -> View {
    view().flex_none().child(
        text(label)
            .rounded_full()
            .border(px(1.))
            .border_color(theme::color(palette::border))
            .px(px(7.))
            .py(px(1.))
            .text_size(px(10.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::color(palette::text_muted)),
    )
}

/// A horizontal meter on the theme's track, `fraction` of it filled in
/// `fill`; a fraction past one fills it rather than overflowing.
pub fn meter(
    fraction: impl Fn() -> f32 + 'static,
    fill: impl Fn() -> Rgba + 'static,
    height: f32,
) -> View {
    view()
        .h(px(height))
        .w_full()
        .rounded_full()
        .bg(theme::color(palette::track))
        .overflow_hidden()
        .child(
            view()
                .h_full()
                .w(move || relative(fraction().clamp(0., 1.)))
                .rounded_full()
                .bg(move || theme::color(&fill)),
        )
}

/// One segment of a [`stacked_meter`]: its share of the whole track, and
/// its paint.
pub struct Segment {
    pub fraction: Box<dyn Fn() -> f32>,
    pub fill: fn() -> Rgba,
}

/// One track, several adjacent segments (memory: resident, then
/// compressed); what is left of the track is the leftover.
pub fn stacked_meter(segments: Vec<Segment>, height: f32) -> View {
    let parts: Vec<View> = segments
        .into_iter()
        .map(|Segment { fraction, fill }| {
            view()
                .h_full()
                .w(move || relative(fraction().clamp(0., 1.)))
                .bg(theme::color(fill))
        })
        .collect();
    view()
        .h(px(height))
        .w_full()
        .rounded_full()
        .bg(theme::color(palette::track))
        .overflow_hidden()
        .child(view().flex_row().size_full().children(parts))
}

/// How a legend's swatch is drawn: the paint of a meter's slice, or
/// hollow for the leftover track — empty, not another colour.
pub enum LegendMark {
    Fill(fn() -> Rgba),
    Hollow,
}

/// One entry of a colour key under a stacked meter: a short bar and its
/// caption, `tip` on hover. A row of them is the key (`gap` 12).
pub fn legend_item(
    mark: LegendMark,
    label: impl IntoReactive<SharedString>,
    tip: TooltipElement,
) -> View {
    let swatch = view().w(px(10.)).h(px(6.)).rounded(px(2.)).flex_none();
    let swatch = match mark {
        LegendMark::Fill(fill) => swatch.bg(theme::color(fill)),
        LegendMark::Hollow => swatch
            .border(px(1.))
            .border_color(theme::color(palette::text_dim)),
    };
    view()
        .flex_row()
        .items_center()
        .gap(px(5.))
        .tooltip(tip)
        .child(swatch)
        .child(
            text(label)
                .text_size(px(10.))
                .line_height(line(10.))
                .text_color(theme::color(palette::text_dim)),
        )
}

/// One pair of a packed key/value row (`widgets::kv_packed`): the label
/// beside its figure, the figure in the alarm's ink while `hot`.
pub fn kv_pair(
    key: impl Into<SharedString>,
    value: impl IntoReactive<SharedString>,
    hot: impl Fn() -> bool + 'static,
) -> View {
    view()
        .flex_row()
        .flex_none()
        .items_baseline()
        .gap(px(8.))
        .child(
            text(key.into())
                .flex_none()
                .text_size(px(11.))
                .line_height(line(11.))
                .text_color(theme::color(palette::text_muted)),
        )
        .child(
            text(value)
                .flex_none()
                .tabular_nums()
                .text_size(px(11.))
                .line_height(line(11.))
                .text_color(move || theme::color(|| palette::text_for(hot()))),
        )
}

/// An option chip: one of a row of choices, `on` while it is the one in
/// force — the theme's selected look on, the neutral chip off.
pub fn option_chip(
    label: impl Into<SharedString>,
    on: impl Fn() -> bool + Copy + 'static,
    on_press: impl Fn() + 'static,
) -> AnyElement {
    nsh_base::button()
        .flex_none()
        .rounded_full()
        .border(px(1.))
        .border_color(move || theme::color(|| palette::choice_line(on())))
        .bg(move || theme::color(|| palette::choice_fill(on())))
        .hover(move |s| s.bg(theme::color(|| palette::choice_hover(on()))))
        .px(px(8.))
        .py(px(2.))
        .on_press(on_press)
        .child(
            text(label.into())
                .text_size(px(10.))
                .line_height(line(10.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || theme::color(|| palette::choice_ink(on()))),
        )
        .into_element()
}

/// How far a tooltip may run before it wraps: inside the panel it
/// explains (`widgets::wrap_tooltip`).
const TIP_WIDTH: f32 = 220.;

/// A tooltip's line height, a multiple of its 11px (`widgets::wrap_tooltip`).
const TIP_LINE: f32 = 1.35;

/// The second tier's opacity (`widgets::TIP_DETAIL_OPACITY`).
const TIP_DETAIL_OPACITY: f32 = 0.66;

/// A tooltip that wraps inside the panel, in two tiers split by a blank
/// line in the string: a sentence anyone can read, then the precise
/// account, dimmer (CLAUDE.md, i18n). `nsh_base`'s `Tooltip`, which is the
/// hint overlay itself and so must be handed to `.tooltip(..)` as it is:
/// turned into a plain element first, NSH wraps it in a second overlay
/// that never shows. It can be `.disabled(..)` — under an open menu.
pub fn wrap_tooltip(tip: impl AsRef<str>) -> TooltipElement {
    let tiers: Vec<String> = tip.as_ref().split("\n\n").map(str::to_owned).collect();
    bubble().child(
        view()
            .max_w(px(TIP_WIDTH))
            .gap(px(6.))
            .children(tiers.into_iter().enumerate().map(tier).collect::<Vec<_>>()),
    )
}

/// [`wrap_tooltip`] whose words follow what `tip` reads: Pin's and
/// Unpin's, the gear's with or without its dot.
pub fn live_tooltip(tip: impl Fn() -> String + 'static) -> TooltipElement {
    bubble().child(
        view()
            .max_w(px(TIP_WIDTH))
            .gap(px(6.))
            .child(dynamic(move || {
                tip()
                    .split("\n\n")
                    .map(str::to_owned)
                    .enumerate()
                    .map(tier)
                    .collect::<Vec<_>>()
            })),
    )
}

/// A tooltip of several lines in one ink (`widgets::wrap_tooltip_lines`):
/// a list, where [`wrap_tooltip`]'s second tier would dim all but the
/// first. Its lines follow what `lines` reads.
pub fn tooltip_lines(lines: impl Fn() -> Vec<String> + 'static) -> TooltipElement {
    bubble().child(
        view()
            .max_w(px(TIP_WIDTH))
            .gap(px(2.))
            .child(dynamic(move || {
                lines()
                    .into_iter()
                    .map(|line| {
                        text(line)
                            .text_size(px(11.))
                            .line_height(px(11. * TIP_LINE))
                            .text_color(theme::color(palette::text))
                    })
                    .collect::<Vec<_>>()
            })),
    )
}

/// The tooltip's bubble: the opaque popover fill, a hairline.
fn bubble() -> TooltipElement {
    tooltip()
        .px(px(8.))
        .py(px(4.))
        .rounded(px(6.))
        .bg(theme::color(palette::popover))
        .outline(px(1.))
        .outline_offset(px(-1.))
        .outline_color(theme::color(palette::border))
}

/// One tier of a tooltip; every tier after the first dimmer.
fn tier((i, words): (usize, String)) -> Text {
    text(words)
        .text_size(px(11.))
        .line_height(px(11. * TIP_LINE))
        .text_color(theme::color(palette::text))
        .opacity(if i > 0 { TIP_DETAIL_OPACITY } else { 1. })
}

/// One entry of a [`menu_trigger`]'s menu: its label, whether it is the
/// one in force (checked), and what choosing it does.
pub struct Choice {
    pub label: String,
    pub on: Box<dyn Fn() -> bool>,
    pub choose: Box<dyn Fn()>,
}

/// A control with several states opens a menu rather than cycling on a
/// click (CLAUDE.md, "A control says what it is"): `label` worn as a
/// chip — the outline of the "All" chip beside it and a chevron after it,
/// at its size (`MenuTrigger::chip`) — `tip` on hover while the menu is
/// shut, and `choices` in the menu it opens, the one in force checked.
pub fn menu_trigger(
    label: impl IntoReactive<SharedString>,
    tip: impl Fn() -> String + 'static,
    choices: Vec<Choice>,
) -> AnyElement {
    // No tooltip under the open menu, and the hover's fill kept while it
    // is open — as on the cup.
    let open = signal(false);
    menu_of(
        open,
        view()
            .flex_row()
            .items_center()
            .gap(px(3.))
            .px(px(7.))
            .py(px(1.))
            .rounded_full()
            .border(px(1.))
            .border_color(move || {
                theme::color(move || {
                    if open.get() {
                        palette::border()
                    } else {
                        palette::border_subtle()
                    }
                })
            })
            .bg(move || {
                theme::color(move || {
                    if open.get() {
                        palette::surface_raised()
                    } else {
                        CLEAR
                    }
                })
            })
            .hover(|s| {
                s.bg(theme::color(palette::surface_raised))
                    .border_color(theme::color(palette::border))
            })
            .tooltip(live_tooltip(tip).disabled(move || open.get()))
            .child(
                text(label)
                    .text_size(px(9.))
                    .line_height(line(9.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::color(palette::text_muted)),
            )
            .child(icon(CustomIconName::ChevronDown, 8., palette::text_dim)),
        choices,
    )
}

/// [`menu_trigger`] worn as a card's title — History's lens, which names
/// the ordering and is the control that switches it: the title's words
/// and a chevron after them, padded for the hover's fill and pulled back
/// so the title still starts on the card's text line.
pub fn title_menu(
    label: impl IntoReactive<SharedString>,
    tip: impl Fn() -> String + 'static,
    choices: Vec<Choice>,
) -> AnyElement {
    let open = signal(false);
    menu_of(
        open,
        view()
            .flex_row()
            .items_center()
            .gap(px(3.))
            .min_w(px(0.))
            .px(px(4.))
            .ml(px(-4.))
            .rounded(px(4.))
            .bg(move || {
                theme::color(move || {
                    if open.get() {
                        palette::surface_raised()
                    } else {
                        CLEAR
                    }
                })
            })
            .hover(|s| s.bg(theme::color(palette::surface_raised)))
            .tooltip(live_tooltip(tip).disabled(move || open.get()))
            .child(list_title(label).flex_shrink(1.))
            .child(icon(CustomIconName::ChevronDown, 10., palette::text_dim)),
        choices,
    )
}

/// The menu a trigger opens: `choices`, the one in force checked, with
/// `open` following it.
fn menu_of(open: Signal<bool>, trigger: View, choices: Vec<Choice>) -> AnyElement {
    root::close_on_hide(open);
    let mut popup = menu()
        .offset(px(4.))
        .open(move || Some(open.get()))
        .on_open_change(move |event: &MenuEvent| {
            if let MenuEvent::OpenChange(next, _) = event {
                open.set(*next);
            }
        })
        .popup_style(|s| {
            s.min_w(px(140.))
                .p(px(4.))
                .rounded(px(8.))
                .bg(theme::color(palette::popover))
                .outline(px(1.))
                .outline_offset(px(-1.))
                .outline_color(theme::color(palette::border))
                .shadow_lg()
        })
        .trigger(trigger);
    for choice in choices {
        let Choice { label, on, choose } = choice;
        let on = Rc::new(on);
        let check = on.clone();
        popup = popup.child(
            menu_item()
                .on_select(choose)
                .flex_row()
                .items_center()
                .gap(px(6.))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(5.))
                .hover(|s| s.bg(theme::color(palette::popover_hover)))
                .child(
                    text(move || if check() { "✓" } else { "" })
                        .flex_none()
                        .w(px(14.))
                        .text_size(px(12.))
                        .text_color(theme::color(palette::gauge)),
                )
                .child(text(label).text_size(px(12.)).text_color(move || {
                    if on() {
                        theme::color(palette::text)
                    } else {
                        theme::color(palette::text_muted)
                    }
                })),
        );
    }
    view().flex_none().child(popup).into_element()
}

/// Ask before something that cannot be taken back, then run `on_ok` only
/// on a yes (`confirm::ask`): the action named on its button in the
/// destructive style, Cancel beside it, Escape as Cancel. The platform's
/// own alert (an `NSAlert` sheet) rather than the small panel the gpui
/// front end drew for itself — NSH has the native one,
/// and a quit or a move to the Trash asked in the system's words reads as
/// the system's question. Called from a handler, which owns the task the
/// answer is awaited in.
pub fn confirm(title: String, body: String, ok: String, on_ok: impl FnOnce() + 'static) {
    let answer = alert_dialog()
        .title(title)
        .description(body)
        .cancel(i18n::tr("common.cancel"))
        .destructive(ok.clone())
        .close_on_escape(true)
        .show();
    spawn(async move {
        if let Ok(Some(chosen)) = answer.await
            && chosen.as_str() == ok
        {
            on_ok();
        }
    });
}
