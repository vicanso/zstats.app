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
//! folder tile wears one of the scheme's map hues ([`theme::map_hues`]),
//! chosen so no two tiles sharing an edge match ([`squarify::colour`]),
//! and the subfolders inside a tile are shades of its colour. The names
//! on the tiles say which folder; the hue only has to separate. Only
//! "files and small folders" is grey. No accent — nothing here crosses a
//! threshold. Labels pick black or white by the tile's luminance, so
//! every scheme stays legible. Hues follow the layout, so a resize or a
//! new walk can recolour a folder; a folder's colour was never meant to
//! be remembered.

use super::{META_PT, ROW_PT, reveal_tip, scope_word, size_text};
use crate::bigfiles;
use crate::diskscan::ScanResult;
use crate::font;
use crate::format;
use crate::i18n;
use crate::squarify::{self, Rect};
use crate::state::{Expansion, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use crate::views::widgets;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, SharedString, Size,
    StatefulInteractiveElement, Styled, div, hsla, px,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{IconName, Sizable, h_flex, v_flex};
use rust_i18n::t;
use std::cell::Cell;
use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Tiles in a level before the rest join "files and small folders".
/// Past forty a home folder's tail is slivers too thin to label or to
/// hit, and every one of them is still in the tooltip-less remainder.
const MAX_TILES: usize = 40;

/// Subfolders drawn inside one tile. The inner level is a hint of what a
/// click will open, not the level itself; sixteen keeps it legible.
const MAX_INNER: usize = 16;

/// Half the gap between tiles: every tile is inset by it, so neighbours
/// stand 2px apart on the map's own recessed fill.
const GAP: f32 = 1.;

/// A tile at least this large shows the level under it; a smaller one
/// is a single block with its name. Below it the inner tiles would be
/// too small to carry a name, and an unlabelled mosaic is noise.
const NEST_MIN_W: f32 = 120.;
const NEST_MIN_H: f32 = 84.;

/// The strip a nesting tile keeps for its own name and size.
const HEADER_H: f32 = 20.;

/// A tile smaller than this carries no label — a name cut to one letter
/// says nothing, and the tooltip still names it.
const LABEL_MIN_W: f32 = 40.;
const LABEL_MIN_H: f32 = 18.;

/// Room a tile needs to show its size under its name. Narrower, the
/// figure was cut to "760…", which is a worse answer than none — the
/// tooltip has it whole.
const SIZE_MIN_H: f32 = 36.;
const SIZE_MIN_W: f32 = 58.;

/// The map's height when it sits above the lists rather than beside
/// them — the window un-zoomed. What 720×480 leaves under the tabs and
/// the card's head, so the map is whole before the lists begin.
pub(super) const NARROW_MAP_H: f32 = 240.;

/// Lightness steps for the subfolders inside a tile: its colour, nudged,
/// so a group reads as one folder and its parts still separate.
const INNER_SHADE: [f32; 5] = [0.0, 0.07, -0.06, 0.12, -0.1];

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
    /// [`NARROW_MAP_H`] (above them).
    pub fill: bool,
}

/// What a level holds, as far as the map can tell.
enum Kids {
    Ready(Vec<(PathBuf, u64)>),
    Walking,
    Failed,
    Unknown,
}

pub(super) fn map(m: Map) -> AnyElement {
    let (path, total, kids) = level(m.state, m.result);
    v_flex()
        .gap(px(6.))
        .when(m.fill, |d| d.flex_1().min_h_0())
        .child(crumbs(&m, &path, total))
        .child(map_box(&m, &path, total, kids))
        .into_any_element()
}

/// The level on screen: its path, its total when known, and its folders.
fn level(state: &ZStatsAppState, result: &ScanResult) -> (PathBuf, Option<u64>, Kids) {
    let index = result.index.as_ref();
    let Some((path, bytes)) = state.map_trail().last() else {
        let kids = if result.roots.len() > 1 {
            // The Caches preset: its walk roots, merged under home.
            match index {
                Some(index) => {
                    let mut roots: Vec<(PathBuf, u64)> = result
                        .roots
                        .iter()
                        .filter_map(|root| Some((root.clone(), index.total_of(root)?)))
                        .collect();
                    roots.sort_by_key(|(_, bytes)| Reverse(*bytes));
                    roots
                }
                None => hits(result),
            }
        } else {
            index
                .and_then(|index| index.children_of(&result.root))
                .unwrap_or_else(|| hits(result))
        };
        return (result.root.clone(), result.total, Kids::Ready(kids));
    };
    let total = index
        .and_then(|index| index.total_of(path))
        .or(Some(*bytes));
    let indexed = index
        .and_then(|index| index.children_of(path))
        .filter(|kids| !kids.is_empty());
    let kids = match indexed {
        Some(kids) => Kids::Ready(kids),
        None => match state.expansion(path) {
            Some(Expansion::Ready(dirs)) => {
                Kids::Ready(dirs.iter().map(|d| (d.path.clone(), d.bytes)).collect())
            }
            Some(Expansion::Walking) => Kids::Walking,
            Some(Expansion::Failed) => Kids::Failed,
            None => Kids::Unknown,
        },
    };
    (path.clone(), total, kids)
}

/// A result without its index — restored from the cache, or a partial —
/// still has its ranked level-1 table.
fn hits(result: &ScanResult) -> Vec<(PathBuf, u64)> {
    result
        .dirs
        .iter()
        .map(|d| (d.path.clone(), d.bytes))
        .collect()
}

/// The way down as a path to click back along, and on the right the
/// level's size and its reveal.
fn crumbs(m: &Map, path: &Path, total: Option<u64>) -> AnyElement {
    let trail = m.state.map_trail();
    let mut names = vec![scope_word(&m.result.roots, &m.result.root)];
    let mut parent = m.result.root.as_path();
    for (step, _) in trail {
        names.push(relative_name(step, parent));
        parent = step.as_path();
    }
    let last = names.len() - 1;
    let mut parts: Vec<AnyElement> = Vec::new();
    for (depth, name) in names.into_iter().enumerate() {
        if depth > 0 {
            parts.push(
                div()
                    .flex_none()
                    .text_size(px(ROW_PT))
                    .text_color(theme::text_dim())
                    .child("›")
                    .into_any_element(),
            );
        }
        let here = depth == last;
        parts.push(
            div()
                .id(("map-crumb", depth))
                .min_w_0()
                .truncate()
                .px(px(5.))
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
                .child(name)
                .into_any_element(),
        );
    }
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
                        .tooltip(reveal_tip())
                        .on_click(move |_, _, _| bigfiles::reveal(&reveal)),
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
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format::tilde(path));
    let body: Vec<AnyElement> = match kids {
        Kids::Walking => vec![note(t!("disk.map_walking", name = name).to_string(), None)],
        Kids::Failed => vec![note(
            t!("disk.map_failed", name = name).to_string(),
            Some(measure_button(i18n::tr("disk.map_retry"))),
        )],
        Kids::Unknown => vec![note(
            t!("disk.map_unmeasured", name = name).to_string(),
            Some(measure_button(i18n::tr("disk.map_measure"))),
        )],
        Kids::Ready(kids) => match m.frame.get() {
            Some(size) => tiles(m, path, total, kids, size),
            None => Vec::new(),
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

/// One tile, as laid out.
struct Tile {
    rect: Rect,
    color: Hsla,
    name: String,
    bytes: u64,
    tip: Vec<SharedString>,
    /// What a click opens, outermost first; `None` for the remainder.
    open: Option<Vec<(PathBuf, u64)>>,
    /// Shows the level under it: the name moves to a strip at the top.
    nests: bool,
    name_pt: f32,
}

fn tiles(
    m: &Map,
    path: &Path,
    total: Option<u64>,
    mut kids: Vec<(PathBuf, u64)>,
    size: Size<Pixels>,
) -> Vec<AnyElement> {
    kids.retain(|(_, bytes)| *bytes > 0);
    let all: u64 = kids.iter().map(|(_, bytes)| bytes).sum();
    kids.truncate(MAX_TILES);
    let shown: u64 = kids.iter().map(|(_, bytes)| bytes).sum();
    // Files directly in the level, folders under the index's floor, and
    // the tail past MAX_TILES. With no total (a partial) only the tail.
    let rest = total.map_or(all - shown, |total| total.saturating_sub(shown));
    let whole = shown + rest;
    let parent_name = if m.state.map_trail().is_empty() {
        scope_word(&m.result.roots, &m.result.root)
    } else {
        display_name(path)
    };
    let bounds = Rect::new(0., 0., f32::from(size.width), f32::from(size.height));
    let mut sizes: Vec<u64> = kids.iter().map(|(_, bytes)| *bytes).collect();
    if rest > 0 {
        sizes.push(rest);
    }
    let rects = squarify::layout(&sizes, bounds);
    let index = m.result.index.as_ref();
    let hues = theme::map_hues();
    // The folder tiles only: the remainder, laid out last, is grey and
    // never a neighbour that a hue must avoid.
    let picks = squarify::colour(&rects[..kids.len()], hues.len());
    let mut out: Vec<Tile> = Vec::new();
    for (i, ((kid, bytes), rect)) in kids.iter().zip(&rects).enumerate() {
        let rect = rect.inset(GAP);
        let color: Hsla = hues[picks[i]].into();
        let name = relative_name(kid, path);
        let inner: Vec<(PathBuf, u64)> = index
            .and_then(|index| index.children_of(kid))
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, b)| *b > 0)
            .take(MAX_INNER)
            .collect();
        let nests = !inner.is_empty() && rect.w >= NEST_MIN_W && rect.h >= NEST_MIN_H;
        out.push(Tile {
            rect,
            color,
            name: name.clone(),
            bytes: *bytes,
            tip: tip(kid, *bytes, whole, &parent_name, m.interactive),
            open: Some(vec![(kid.clone(), *bytes)]),
            nests,
            name_pt: ROW_PT,
        });
        if !nests {
            continue;
        }
        // The level under this tile, in the tile below its strip. What
        // the subfolders leave of it — the folder's own files — is a tile
        // of its own: left as the parent's darker ground, a folder of
        // mostly files read as a hole.
        let area = Rect::new(
            rect.x + 2.,
            rect.y + HEADER_H,
            (rect.w - 4.).max(0.),
            (rect.h - HEADER_H - 2.).max(0.),
        );
        let inner_sum: u64 = inner.iter().map(|(_, b)| b).sum();
        let mut inner_sizes: Vec<u64> = inner.iter().map(|(_, b)| *b).collect();
        if *bytes > inner_sum {
            inner_sizes.push(bytes - inner_sum);
        }
        let inner_rects = squarify::layout(&inner_sizes, area);
        for (j, ((sub, sub_bytes), sub_rect)) in inner.iter().zip(&inner_rects).enumerate() {
            out.push(Tile {
                rect: sub_rect.inset(GAP),
                color: shade(color, INNER_SHADE[j % INNER_SHADE.len()]),
                name: relative_name(sub, kid),
                bytes: *sub_bytes,
                tip: tip(sub, *sub_bytes, *bytes, &name, m.interactive),
                open: Some(vec![(kid.clone(), *bytes), (sub.clone(), *sub_bytes)]),
                nests: false,
                name_pt: META_PT + 1.,
            });
        }
        if *bytes > inner_sum
            && let Some(rest_rect) = inner_rects.get(inner.len())
        {
            let rest = bytes - inner_sum;
            out.push(Tile {
                rect: rest_rect.inset(GAP),
                color: shade(color, -0.05),
                name: i18n::tr("disk.map_rest"),
                bytes: rest,
                tip: vec![
                    t!("disk.map_rest_tip", parent = name.clone())
                        .to_string()
                        .into(),
                    share_line(rest, *bytes, &name).into(),
                ],
                // Part of the folder: a click opens the folder.
                open: Some(vec![(kid.clone(), *bytes)]),
                nests: false,
                name_pt: META_PT + 1.,
            });
        }
    }
    if rest > 0
        && let Some(rect) = rects.last()
    {
        out.push(Tile {
            rect: rect.inset(GAP),
            color: theme::map_rest().into(),
            name: i18n::tr("disk.map_rest"),
            bytes: rest,
            tip: vec![
                t!("disk.map_rest_tip", parent = parent_name.clone())
                    .to_string()
                    .into(),
                share_line(rest, whole, &parent_name).into(),
            ],
            open: None,
            nests: false,
            name_pt: META_PT + 1.,
        });
    }
    out.into_iter()
        .enumerate()
        .map(|(n, tile)| tile_element(n, tile, m.interactive))
        .collect()
}

fn tile_element(n: usize, tile: Tile, interactive: bool) -> AnyElement {
    let Tile {
        rect,
        color,
        name,
        bytes,
        tip,
        open,
        nests,
        name_pt,
    } = tile;
    let base = if nests { shade(color, -0.12) } else { color };
    let ink = ink_on(base);
    let labelled = rect.w >= LABEL_MIN_W && rect.h >= LABEL_MIN_H;
    let label = labelled.then(|| {
        if nests {
            // The strip: name left, size right, one line.
            h_flex()
                .h(px(HEADER_H - 4.))
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
                        .child(format::memory(bytes)),
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
                .when(rect.h >= SIZE_MIN_H && rect.w >= SIZE_MIN_W, |d| {
                    d.child(
                        div()
                            .truncate()
                            .font_family(font::MONO)
                            .text_size(px(META_PT))
                            .text_color(ink.opacity(0.75))
                            .child(format::memory(bytes)),
                    )
                })
                .into_any_element()
        }
    });
    let hover = shade(base, 0.06);
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
        .tooltip(widgets::wrap_tooltip_lines(tip))
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

/// Where it is, what it holds and what share of its parent that is —
/// and, on a finished map, that a click opens it.
fn tip(path: &Path, bytes: u64, whole: u64, parent: &str, interactive: bool) -> Vec<SharedString> {
    let mut lines: Vec<SharedString> = vec![
        format::tilde(path).into(),
        share_line(bytes, whole, parent).into(),
    ];
    if interactive {
        lines.push(i18n::tr("disk.map_open").into());
    }
    lines
}

fn share_line(bytes: u64, whole: u64, parent: &str) -> String {
    let share = if whole == 0 {
        0.
    } else {
        bytes as f64 * 100. / whole as f64
    };
    let share = if share < 10. {
        format!("{share:.1}%")
    } else {
        format!("{share:.0}%")
    };
    t!(
        "disk.map_share",
        size = format::memory(bytes),
        share = share,
        parent = parent
    )
    .to_string()
}

/// `path` as seen from `parent` — `Caches/go-build` under `Library` for a
/// chased level, the bare name for a direct child.
fn relative_name(path: &Path, parent: &Path) -> String {
    path.strip_prefix(parent)
        .ok()
        .map(|rel| rel.display().to_string())
        .filter(|rel| !rel.is_empty())
        .unwrap_or_else(|| display_name(path))
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format::tilde(path))
}

fn shade(color: Hsla, by: f32) -> Hsla {
    Hsla {
        l: (color.l + by).clamp(0.08, 0.92),
        ..color
    }
}

/// Black or white, whichever stands further from the tile: the WCAG
/// crossover is at a relative luminance of about 0.18.
fn ink_on(color: Hsla) -> Hsla {
    let rgb = color.to_rgb();
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(rgb.r) + 0.7152 * linear(rgb.g) + 0.0722 * linear(rgb.b);
    if luminance > 0.18 {
        hsla(0., 0., 0.06, 0.9)
    } else {
        hsla(0., 0., 1., 0.94)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_take_the_ink_that_stands_out() {
        // Classic dark's amber hue, and its grey for the rest.
        let amber = gpui::rgb(0xc98500);
        let grey = gpui::rgb(0x46464c);
        assert!(ink_on(amber.into()).l < 0.5, "dark ink on the amber");
        assert!(ink_on(grey.into()).l > 0.5, "white ink on the grey");
    }

    #[test]
    fn a_name_is_read_from_the_level_above() {
        let parent = Path::new("/Users/me/Library");
        assert_eq!(
            relative_name(Path::new("/Users/me/Library/Caches/go-build"), parent),
            "Caches/go-build"
        );
        assert_eq!(
            relative_name(Path::new("/Users/me/Library/Mail"), parent),
            "Mail"
        );
        assert_eq!(relative_name(Path::new("/opt/x"), parent), "x");
    }
}
