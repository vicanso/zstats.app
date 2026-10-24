//! The gpui front end's widgets (`views/widgets.rs`, `views::MenuTrigger`,
//! the option chips) on NSH, for the pages phase 4 brings over: the same
//! sizes, inks and rules — `.claude/skills/ui-polish`, and the comments
//! on the gpui originals, which say why each is the way it is and are not
//! repeated here. What differs is how NSH draws, not what is drawn:
//!
//! - **Colours carry both halves** (`theme::color`), so a widget made once
//!   follows the appearance; a colour that depends on a value the widget
//!   is told reactively (a chip's `on`) is a closure NSH re-reads.
//! - **The card's hairline is an outline**, which takes no layout, where
//!   gpui needed a zero-blur inset shadow for the same reason: a theme
//!   switch changes paint, never geometry.
//! - **Icons are SVGs drawn as images and tinted** — the bundled ones
//!   (`assets::CustomIconName`), as `data:` URIs NSH renders at the
//!   view's size. gpui-kit's lucide set is not here; a page brings the
//!   glyphs it uses into `assets/icons`.
//! - **The mono face is registered with each root** (`root::make`), from
//!   the same bundled files `font::register` gives gpui.

use crate::assets::{self, CustomIconName};
use crate::color::Rgba;
use crate::nsh::root::CARD_PAD;
use crate::nsh::theme;
use crate::palette;
use nsh_base::{menu, menu_item};
use nsh_core::ui::prelude::*;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex};

/// The bundled mono face (`font::MONO`): rates and code-like identifiers.
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

/// A bundled glyph, `size` points square, in `ink`.
pub fn icon(name: CustomIconName, size: f32, ink: impl Fn() -> Rgba) -> AnyElement {
    image(icon_uri(name))
        .w(px(size))
        .h(px(size))
        .tint_color(theme::color(ink))
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
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::color(palette::text)),
        )
        .child(
            text(unit)
                .text_size(px(HEADLINE_UNIT))
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
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || theme::color(|| palette::choice_ink(on()))),
        )
        .into_element()
}

/// How far a tooltip may run before it wraps: inside the panel it
/// explains (`widgets::wrap_tooltip`).
const TIP_WIDTH: f32 = 220.;

/// The second tier's opacity (`widgets::TIP_DETAIL_OPACITY`).
const TIP_DETAIL_OPACITY: f32 = 0.66;

/// A tooltip that wraps inside the panel, in two tiers split by a blank
/// line in the string: a sentence anyone can read, then the precise
/// account, dimmer (CLAUDE.md, i18n). For an element's `.tooltip(..)`:
/// the bubble itself, which NSH puts in a hint overlay with its own delay
/// and placement — `nsh_base`'s `Tooltip` *is* such an overlay, and
/// handed over as an element it ends up inside a second one that never
/// shows.
pub fn wrap_tooltip(tip: impl AsRef<str>) -> AnyElement {
    let tiers: Vec<String> = tip.as_ref().split("\n\n").map(str::to_owned).collect();
    let mut bubble = view().max_w(px(TIP_WIDTH)).gap(px(6.));
    for (i, tier) in tiers.into_iter().enumerate() {
        let line = text(tier)
            .text_size(px(11.))
            .line_height(relative(1.35))
            .text_color(theme::color(palette::text));
        bubble = bubble.child(if i > 0 {
            line.opacity(TIP_DETAIL_OPACITY)
        } else {
            line
        });
    }
    bubble
        .px(px(8.))
        .py(px(4.))
        .rounded(px(6.))
        .bg(theme::color(palette::popover))
        .outline(px(1.))
        .outline_offset(px(-1.))
        .outline_color(theme::color(palette::border))
        .into_element()
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
/// chip — the outline and a chevron after it — and `choices` in the menu
/// it opens, the one in force checked.
pub fn menu_trigger(label: impl IntoReactive<SharedString>, choices: Vec<Choice>) -> AnyElement {
    let mut popup = menu()
        .offset(px(4.))
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
        .trigger(
            view()
                .flex_row()
                .items_center()
                .gap(px(3.))
                .h(px(20.))
                .px(px(8.))
                .rounded_full()
                .border(px(1.))
                .border_color(theme::color(palette::border_subtle))
                .hover(|s| {
                    s.bg(theme::color(palette::surface_raised))
                        .border_color(theme::color(palette::border))
                })
                .child(
                    text(label)
                        .text_size(px(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::color(palette::text_muted)),
                )
                .child(icon(CustomIconName::ChevronDown, 8., palette::text_dim)),
        );
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
    popup.into_element()
}
