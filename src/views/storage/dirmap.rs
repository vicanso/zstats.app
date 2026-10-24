//! The analysis tab's directory map: one level of the finished walk as a
//! squarified treemap ([`crate::squarify`]), each tile a folder sized by
//! what it holds, and inside every tile large enough the level under it.
//!
//! It replaced the big-directories table and the composition bar above
//! it. The table answered "which folders are big" one row at a time and
//! the bar "what share is each" for six of them; the map answers both at
//! once and keeps going down — a click opens a tile, the path above it
//! goes back. Every figure is the walk's own: the retained index serves a
//! level instantly, and where it recorded nothing (a result restored from
//! its cache, a folded tree) the same one-at-a-time walk a row's
//! expansion takes measures it.
//!
//! Colour keeps neighbours apart and says nothing about size: every
//! folder tile wears one of the scheme's map hues ([`palette::map_hues`]),
//! chosen so no two tiles sharing an edge match ([`squarify::colour`]),
//! and the subfolders inside a tile are shades of its colour. The names
//! on the tiles say which folder; the hue only has to separate. Only
//! "files and small folders" is grey. No accent — nothing here crosses a
//! threshold. Labels pick black or white by the tile's luminance, so
//! every scheme stays legible. Hues follow the layout, so a resize or a
//! new walk can recolour a folder; a folder's colour was never meant to
//! be remembered.
//!
//! The layout — the level, the crumbs, the tiles and their paint — is
//! `crate::dirmap`'s, shared with the NSH window; this file draws it.

use super::size_text;
use crate::dirmap::{self, Crumb, Kids, NARROW_MAP_H, Paint, Tile};
use crate::diskscan::ScanResult;
use crate::finder;
use crate::font;
use crate::i18n;
use crate::state::{ZStatsAppState, ZStatsGlobalStore};
use crate::storagepage::{self, META_PT, ROW_PT};
use crate::theme;
use crate::views::widgets;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, SharedString, Size,
    StatefulInteractiveElement, Styled, div, px,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{IconName, Sizable, h_flex, v_flex};
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

/// Everything the map needs beside the store.
pub(super) struct Map<'a> {
    pub state: &'a ZStatsAppState,
    pub result: &'a ScanResult,
    /// A finished result: tiles open, the path goes back. A partial one
    /// is lower bounds drawn as they land, with nothing to open yet.
    pub interactive: bool,
    /// The map's box as last laid out. Tiles are placed in pixels, and
    /// the box's size is only known after layout, so each frame lays out
    /// against the previous one's and a change asks for one more frame.
    pub frame: Rc<Cell<Option<Size<Pixels>>>>,
    /// Fill the height it is given (beside the lists) or keep
    /// [`NARROW_MAP_H`] (above them) — the map's height when it sits
    /// above the lists, what 720×480 leaves under the tabs and the card's
    /// head, so the map is whole before the lists begin.
    pub fill: bool,
}

pub(super) fn map(m: Map) -> AnyElement {
    let (path, total, kids) = dirmap::level(m.state, m.result);
    v_flex()
        .gap(px(6.))
        .when(m.fill, |d| d.flex_1().min_h_0())
        .child(crumbs(&m, &path, total))
        .child(map_box(&m, &path, total, kids))
        .into_any_element()
}

/// The way down as a path to click back along, and on the right the
/// level's size and its reveal.
///
/// Every crumb says its whole path on hover. When the path is too long
/// for the row the middle folds into one "…" — the root, the level on
/// screen and as many of its parents as fit stay whole. Letting every
/// crumb shrink instead cut all of them at once: ten levels down the row
/// read "Ho… › Libr… › Contai… › D…", eleven names and none legible.
/// A crumb's width is guessed from its characters (`dirmap::crumb_width`
/// — erring wide only folds a level early; erring narrow would bring
/// back the row where every crumb is cut).
fn crumbs(m: &Map, path: &Path, total: Option<u64>) -> AnyElement {
    // The row is as wide as the map under it; before the first layout
    // there is no width to fold against.
    let room = m.frame.get().map_or(f32::INFINITY, |size| {
        f32::from(size.width) - dirmap::CRUMB_ROW_RESERVE
    });
    let parts: Vec<AnyElement> = dirmap::crumbs(m.state, m.result, room)
        .into_iter()
        .map(|part| match part {
            Crumb::Separator => div()
                .flex_none()
                .text_size(px(ROW_PT))
                .text_color(theme::text_dim())
                .child("›")
                .into_any_element(),
            Crumb::Level {
                depth,
                label,
                tip,
                here,
            } => div()
                .id(("map-crumb", depth))
                .min_w_0()
                .max_w(px(if here {
                    dirmap::CRUMB_HERE_MAX_W
                } else {
                    dirmap::CRUMB_MAX_W
                }))
                .truncate()
                .px(px(dirmap::CRUMB_PAD_W / 2.))
                .py(px(1.))
                .rounded(px(4.))
                .text_size(px(ROW_PT))
                .when(here, |d| {
                    d.font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                })
                .when(!here, |d| d.text_color(theme::text_muted()))
                .when(!here && m.interactive, |d| {
                    d.hover(|s| s.bg(theme::surface_raised()).text_color(theme::text()))
                        .on_click(move |_, _window, cx| {
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| state.map_back_to(depth, cx));
                        })
                })
                .tooltip(widgets::wrap_tooltip(tip))
                .child(label)
                .into_any_element(),
        })
        .collect();
    let reveal = path.to_path_buf();
    h_flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap(px(10.))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(2.))
                .children(parts)
                .child(
                    div()
                        .pl(px(4.))
                        .child(widgets::info_icon("map-hint", i18n::tr("disk.map_hint"))),
                ),
        )
        .child(
            h_flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .children(total.map(size_text))
                .child(
                    Button::new("map-reveal")
                        .icon(IconName::Folder)
                        .ghost()
                        .xsmall()
                        .tooltip(storagepage::reveal_tip())
                        .on_click(move |_, _, _| finder::reveal(&reveal)),
                ),
        )
        .into_any_element()
}

fn map_box(m: &Map, path: &Path, total: Option<u64>, kids: Kids) -> AnyElement {
    let frame = m.frame.clone();
    // Records the box after layout; a size the tiles were not laid out
    // for asks for one more frame.
    let recorder = gpui::canvas(
        move |bounds, window, _| {
            if frame.get() != Some(bounds.size) {
                frame.set(Some(bounds.size));
                window.refresh();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full();
    let body: Vec<AnyElement> = match dirmap::kids_note(&kids, path) {
        Some((text, action)) => vec![note(text, action.map(measure_button))],
        None => match (kids, m.frame.get()) {
            (Kids::Ready(kids), Some(size)) => dirmap::tiles(
                m.state,
                m.result,
                path,
                total,
                kids,
                (f32::from(size.width), f32::from(size.height)),
                m.interactive,
            )
            .into_iter()
            .enumerate()
            .map(|(n, tile)| tile_element(n, tile, m.interactive))
            .collect(),
            _ => Vec::new(),
        },
    };
    div()
        .relative()
        .overflow_hidden()
        .rounded(px(6.))
        .bg(theme::inset())
        .when(m.fill, |d| d.flex_1().min_h_0())
        .when(!m.fill, |d| d.flex_none().h(px(NARROW_MAP_H)))
        .child(recorder)
        .children(body)
        .into_any_element()
}

fn note(text: String, action: Option<AnyElement>) -> AnyElement {
    v_flex()
        .absolute()
        .inset_0()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .child(
            div()
                .text_size(px(ROW_PT))
                .text_color(theme::text_muted())
                .child(text),
        )
        .children(action)
        .into_any_element()
}

fn measure_button(label: String) -> AnyElement {
    Button::new("map-measure")
        .small()
        .label(label)
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.measure_map_level(cx));
        })
        .into_any_element()
}

fn hsla(paint: Paint) -> Hsla {
    gpui::Rgba::from(paint.color()).into()
}

fn tile_element(n: usize, tile: Tile, interactive: bool) -> AnyElement {
    let base = tile.fill().color();
    let ink: Hsla = gpui::Rgba::from(dirmap::ink_on(base)).into();
    let hover = hsla(tile.hover());
    let labelled = tile.labelled();
    let shows_size = tile.shows_size();
    let Tile {
        rect,
        name,
        bytes,
        tip,
        open,
        nests,
        name_pt,
        ..
    } = tile;
    let label = labelled.then(|| {
        if nests {
            // The strip: name left, size right, one line.
            h_flex()
                .h(px(dirmap::HEADER_H - 4.))
                .items_center()
                .justify_between()
                .gap(px(6.))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(name_pt))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(ink)
                        .child(name),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(font::MONO)
                        .text_size(px(META_PT))
                        .text_color(ink.opacity(0.75))
                        .child(crate::format::memory(bytes)),
                )
                .into_any_element()
        } else {
            v_flex()
                .gap(px(1.))
                .child(
                    div()
                        .truncate()
                        .text_size(px(name_pt))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(ink)
                        .child(name),
                )
                .when(shows_size, |d| {
                    d.child(
                        div()
                            .truncate()
                            .font_family(font::MONO)
                            .text_size(px(META_PT))
                            .text_color(ink.opacity(0.75))
                            .child(crate::format::memory(bytes)),
                    )
                })
                .into_any_element()
        }
    });
    let base: Hsla = gpui::Rgba::from(base).into();
    div()
        .id(("map-tile", n))
        .absolute()
        .left(px(rect.x))
        .top(px(rect.y))
        .w(px(rect.w))
        .h(px(rect.h))
        // A tile on top takes the pointer from the one under it: a
        // subfolder's click opens the subfolder, not its parent.
        .occlude()
        .overflow_hidden()
        .rounded(px(3.))
        .bg(base)
        .px(px(6.))
        .py(px(if nests { 2. } else { 4. }))
        .children(label)
        .tooltip(widgets::wrap_tooltip_lines(
            tip.into_iter().map(SharedString::from).collect(),
        ))
        .when_some(open.filter(|_| interactive), |d, chain| {
            d.hover(move |s| s.bg(hover))
                .on_click(move |_, _window, cx| {
                    let chain = chain.clone();
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.open_in_map(chain, cx));
                })
        })
        .into_any_element()
}
