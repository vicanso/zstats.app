//! The disk-space window's directory map, laid out for either front end:
//! the level on screen and what it holds, the path back up as crumbs that
//! fold their middle when too long, and the tiles — a squarified treemap
//! ([`crate::squarify`]) of the level, the level under each tile drawn
//! inside it when the tile is large enough. Shared since the NSH
//! disk-space window (phase 4k of docs/nsh-migration.md);
//! `views/storage/dirmap.rs` draws it with gpui and says why the map is
//! the way it is.
//!
//! A tile's paint is a hue's index and a lightness step rather than a
//! colour: the hues are the scheme's (`palette::map_hues`), read in
//! whichever half the front end is painting.

use crate::color::Rgba;
use crate::diskscan::ScanResult;
use crate::squarify::{self, Rect};
use crate::state::{Expansion, ZStatsAppState};
use crate::storagepage::{self, META_PT, ROW_PT};
use crate::{format, i18n, palette};
use rust_i18n::t;
use std::cmp::Reverse;
use std::path::{Path, PathBuf};

/// Tiles in a level before the rest join "files and small folders".
const MAX_TILES: usize = 40;

/// Subfolders drawn inside one tile: a hint of what a click opens.
const MAX_INNER: usize = 16;

/// Half the gap between tiles: every tile is inset by it.
const GAP: f32 = 1.;

/// A tile at least this large shows the level under it.
const NEST_MIN_W: f32 = 120.;
const NEST_MIN_H: f32 = 84.;

/// The strip a nesting tile keeps for its own name and size.
pub const HEADER_H: f32 = 20.;

/// A tile smaller than this carries no label.
const LABEL_MIN_W: f32 = 40.;
const LABEL_MIN_H: f32 = 18.;

/// Room a tile needs to show its size under its name.
const SIZE_MIN_H: f32 = 36.;
const SIZE_MIN_W: f32 = 58.;

/// The map's height above the lists, the window un-zoomed.
pub const NARROW_MAP_H: f32 = 240.;

/// The widest a crumb may grow before it truncates; the level on screen
/// gets more.
pub const CRUMB_MAX_W: f32 = 170.;
pub const CRUMB_HERE_MAX_W: f32 = 260.;

/// What a crumb costs beside its text, and a separator with its gaps.
pub const CRUMB_PAD_W: f32 = 10.;
const CRUMB_SEP_W: f32 = 14.;

/// The "…" crumb that stands for the folded levels.
const CRUMB_FOLD_W: f32 = 22.;

/// What the path row keeps for the level's size, the reveal button and
/// the hint beside the path.
pub const CRUMB_ROW_RESERVE: f32 = 120.;

/// Lightness steps for the subfolders inside a tile.
const INNER_SHADE: [f32; 5] = [0.0, 0.07, -0.06, 0.12, -0.1];

/// What a level holds, as far as the map can tell.
#[derive(Clone, Debug, PartialEq)]
pub enum Kids {
    Ready(Vec<(PathBuf, u64)>),
    Walking,
    Failed,
    Unknown,
}

/// The level on screen: its path, its total when known, and its folders.
pub fn level(state: &ZStatsAppState, result: &ScanResult) -> (PathBuf, Option<u64>, Kids) {
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

/// What the map shows in place of tiles while a level has none to show:
/// the sentence and, where a walk would answer, the button's words.
pub fn kids_note(kids: &Kids, path: &Path) -> Option<(String, Option<String>)> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format::tilde(path));
    match kids {
        Kids::Ready(_) => None,
        Kids::Walking => Some((t!("disk.map_walking", name = name).to_string(), None)),
        Kids::Failed => Some((
            t!("disk.map_failed", name = name).to_string(),
            Some(i18n::tr("disk.map_retry")),
        )),
        Kids::Unknown => Some((
            t!("disk.map_unmeasured", name = name).to_string(),
            Some(i18n::tr("disk.map_measure")),
        )),
    }
}

/// One part of the path row.
#[derive(Clone, Debug, PartialEq)]
pub enum Crumb {
    /// A level: where a click goes back to (`state.map_back_to`), its
    /// words, its whole path for the tooltip, and whether it is the level
    /// on screen.
    Level {
        depth: usize,
        label: String,
        tip: String,
        here: bool,
    },
    Separator,
}

/// The path back up, with its middle folded into one "…" when `room`
/// (the row's width, less [`CRUMB_ROW_RESERVE`]) cannot hold it all.
pub fn crumbs(state: &ZStatsAppState, result: &ScanResult, room: f32) -> Vec<Crumb> {
    let trail = state.map_trail();
    let mut levels: Vec<(String, &Path)> = vec![(
        storagepage::scope_word(&result.roots, &result.root),
        result.root.as_path(),
    )];
    for (step, _) in trail {
        let parent = levels.last().map_or(result.root.as_path(), |(_, p)| *p);
        levels.push((relative_name(step, parent), step.as_path()));
    }
    let last = levels.len() - 1;
    let widths: Vec<f32> = levels
        .iter()
        .enumerate()
        .map(|(depth, (name, _))| crumb_width(name, depth == last))
        .collect();
    let tail = tail_start(&widths, room);
    let mut parts = Vec::new();
    for (depth, (name, level)) in levels.iter().enumerate() {
        if depth > 0 && depth < tail {
            continue;
        }
        if depth > 0 {
            parts.push(Crumb::Separator);
        }
        if depth == tail && tail > 1 {
            // The folded levels as one crumb: it names the deepest of
            // them and a click goes there, where more of the path fits.
            parts.push(Crumb::Level {
                depth: tail - 1,
                label: "…".into(),
                tip: format::tilde(levels[tail - 1].1),
                here: false,
            });
            parts.push(Crumb::Separator);
        }
        parts.push(Crumb::Level {
            depth,
            label: name.clone(),
            tip: format::tilde(level),
            here: depth == last,
        });
    }
    parts
}

/// A crumb's width, near enough to decide what fits, capped where the
/// crumb itself truncates: lower case about 6.2 a character at 12pt,
/// capitals and digits 7.8, anything outside ASCII a full em.
fn crumb_width(name: &str, here: bool) -> f32 {
    let text: f32 = name
        .chars()
        .map(|c| {
            if !c.is_ascii() {
                ROW_PT
            } else if c.is_ascii_uppercase() || c.is_ascii_digit() {
                7.8
            } else {
                6.2
            }
        })
        .sum();
    let cap = if here { CRUMB_HERE_MAX_W } else { CRUMB_MAX_W };
    (text + CRUMB_PAD_W).min(cap)
}

/// Where the path's visible tail starts: crumbs `1..start` fold into
/// "…", and `1` folds nothing. The root and the level on screen always
/// stay; parents are added back from the nearest one while they fit.
fn tail_start(widths: &[f32], room: f32) -> usize {
    let count = widths.len();
    let all: f32 = widths.iter().sum::<f32>() + CRUMB_SEP_W * count.saturating_sub(1) as f32;
    if count <= 2 || all <= room {
        return 1;
    }
    let mut used = widths[0] + CRUMB_SEP_W + CRUMB_FOLD_W + CRUMB_SEP_W + widths[count - 1];
    let mut start = count - 1;
    while start > 1 {
        let parent = widths[start - 1] + CRUMB_SEP_W;
        if used + parent > room {
            break;
        }
        used += parent;
        start -= 1;
    }
    // Folding one level into "…" shows no more than the level would.
    if start == 2 { 1 } else { start }
}

/// A tile's paint: one of the scheme's map hues, or the grey of "files
/// and small folders", nudged in lightness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Paint {
    /// The hue's place in `palette::map_hues`; `None` is `map_rest`.
    pub hue: Option<usize>,
    pub shade: f32,
}

impl Paint {
    /// The colour, in the palette's half now in force.
    pub fn color(self) -> Rgba {
        let base = match self.hue {
            Some(i) => {
                let hues = palette::map_hues();
                hues[i % hues.len()]
            }
            None => palette::map_rest(),
        };
        shade(base, self.shade)
    }

    fn by(self, step: f32) -> Paint {
        Paint {
            shade: self.shade + step,
            ..self
        }
    }
}

/// One tile, as laid out.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub rect: Rect,
    pub paint: Paint,
    pub name: String,
    pub bytes: u64,
    pub tip: Vec<String>,
    /// What a click opens, outermost first; `None` for the remainder.
    pub open: Option<Vec<(PathBuf, u64)>>,
    /// Shows the level under it: the name moves to a strip at the top.
    pub nests: bool,
    pub name_pt: f32,
}

impl Tile {
    /// The tile's fill: a nesting tile is a step darker, so the folders
    /// inside it stand on it.
    pub fn fill(&self) -> Paint {
        if self.nests {
            self.paint.by(-0.12)
        } else {
            self.paint
        }
    }

    /// The fill under the pointer.
    pub fn hover(&self) -> Paint {
        self.fill().by(0.06)
    }

    /// Large enough to carry its name.
    pub fn labelled(&self) -> bool {
        self.rect.w >= LABEL_MIN_W && self.rect.h >= LABEL_MIN_H
    }

    /// Large enough for its size under its name (a nesting tile's strip
    /// always carries it).
    pub fn shows_size(&self) -> bool {
        self.rect.h >= SIZE_MIN_H && self.rect.w >= SIZE_MIN_W
    }
}

/// The level's folders as tiles in a `width` × `height` box, the level
/// under each large one inside it, and the remainder last.
pub fn tiles(
    state: &ZStatsAppState,
    result: &ScanResult,
    path: &Path,
    total: Option<u64>,
    mut kids: Vec<(PathBuf, u64)>,
    (width, height): (f32, f32),
    interactive: bool,
) -> Vec<Tile> {
    kids.retain(|(_, bytes)| *bytes > 0);
    let all: u64 = kids.iter().map(|(_, bytes)| bytes).sum();
    kids.truncate(MAX_TILES);
    let shown: u64 = kids.iter().map(|(_, bytes)| bytes).sum();
    // Files directly in the level, folders under the index's floor, and
    // the tail past MAX_TILES. With no total (a partial) only the tail.
    let rest = total.map_or(all - shown, |total| total.saturating_sub(shown));
    let whole = shown + rest;
    let parent_name = if state.map_trail().is_empty() {
        storagepage::scope_word(&result.roots, &result.root)
    } else {
        display_name(path)
    };
    let bounds = Rect::new(0., 0., width, height);
    let mut sizes: Vec<u64> = kids.iter().map(|(_, bytes)| *bytes).collect();
    if rest > 0 {
        sizes.push(rest);
    }
    let rects = squarify::layout(&sizes, bounds);
    let index = result.index.as_ref();
    let hues = palette::map_hues().len();
    // The folder tiles only: the remainder, laid out last, is grey and
    // never a neighbour that a hue must avoid.
    let picks = squarify::colour(&rects[..kids.len()], hues);
    let mut out: Vec<Tile> = Vec::new();
    for (i, ((kid, bytes), rect)) in kids.iter().zip(&rects).enumerate() {
        let rect = rect.inset(GAP);
        let paint = Paint {
            hue: Some(picks[i]),
            shade: 0.,
        };
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
            paint,
            name: name.clone(),
            bytes: *bytes,
            tip: tip(kid, *bytes, whole, &parent_name, interactive),
            open: Some(vec![(kid.clone(), *bytes)]),
            nests,
            name_pt: ROW_PT,
        });
        if !nests {
            continue;
        }
        // The level under this tile, below its strip. What the subfolders
        // leave of it — the folder's own files — is a tile of its own.
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
                paint: paint.by(INNER_SHADE[j % INNER_SHADE.len()]),
                name: relative_name(sub, kid),
                bytes: *sub_bytes,
                tip: tip(sub, *sub_bytes, *bytes, &name, interactive),
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
                paint: paint.by(-0.05),
                name: i18n::tr("disk.map_rest"),
                bytes: rest,
                tip: vec![
                    t!("disk.map_rest_tip", parent = name.clone()).to_string(),
                    share_line(rest, *bytes, &name),
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
            paint: Paint {
                hue: None,
                shade: 0.,
            },
            name: i18n::tr("disk.map_rest"),
            bytes: rest,
            tip: vec![
                t!("disk.map_rest_tip", parent = parent_name.clone()).to_string(),
                share_line(rest, whole, &parent_name),
            ],
            open: None,
            nests: false,
            name_pt: META_PT + 1.,
        });
    }
    out
}

/// Where it is, what it holds and what share of its parent that is —
/// and, on a finished map, that a click opens it.
fn tip(path: &Path, bytes: u64, whole: u64, parent: &str, interactive: bool) -> Vec<String> {
    let mut lines = vec![format::tilde(path), share_line(bytes, whole, parent)];
    if interactive {
        lines.push(i18n::tr("disk.map_open"));
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

/// `color` with its HSL lightness moved by `by`, kept off pure black and
/// white.
fn shade(color: Rgba, by: f32) -> Rgba {
    if by == 0. {
        return color;
    }
    let (h, s, l) = to_hsl(color);
    from_hsl(h, s, (l + by).clamp(0.08, 0.92), color.a)
}

fn to_hsl(c: Rgba) -> (f32, f32, f32) {
    let max = c.r.max(c.g).max(c.b);
    let min = c.r.min(c.g).min(c.b);
    let l = (max + min) / 2.;
    if max == min {
        return (0., 0., l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2. - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == c.r {
        (c.g - c.b) / d + if c.g < c.b { 6. } else { 0. }
    } else if max == c.g {
        (c.b - c.r) / d + 2.
    } else {
        (c.r - c.g) / d + 4.
    };
    (h / 6., s, l)
}

fn from_hsl(h: f32, s: f32, l: f32, a: f32) -> Rgba {
    if s == 0. {
        return Rgba {
            r: l,
            g: l,
            b: l,
            a,
        };
    }
    let q = if l < 0.5 { l * (1. + s) } else { l + s - l * s };
    let p = 2. * l - q;
    let channel = |t: f32| {
        let t = t.rem_euclid(1.);
        if t < 1. / 6. {
            p + (q - p) * 6. * t
        } else if t < 0.5 {
            q
        } else if t < 2. / 3. {
            p + (q - p) * (2. / 3. - t) * 6.
        } else {
            p
        }
    };
    Rgba {
        r: channel(h + 1. / 3.),
        g: channel(h),
        b: channel(h - 1. / 3.),
        a,
    }
}

/// Black or white, whichever stands further from the tile: the WCAG
/// crossover is at a relative luminance of about 0.18.
pub fn ink_on(color: Rgba) -> Rgba {
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b);
    if luminance > 0.18 {
        Rgba {
            r: 0.06,
            g: 0.06,
            b: 0.06,
            a: 0.9,
        }
    } else {
        Rgba {
            r: 1.,
            g: 1.,
            b: 1.,
            a: 0.94,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::rgb;

    #[test]
    fn labels_take_the_ink_that_stands_out() {
        // Classic dark's amber hue, and its grey for the rest.
        assert!(ink_on(rgb(0xc98500)).r < 0.5, "dark ink on the amber");
        assert!(ink_on(rgb(0x46464c)).r > 0.5, "white ink on the grey");
    }

    /// A shade moves lightness and nothing else, and a zero step is the
    /// colour itself.
    #[test]
    fn a_shade_keeps_the_hue() {
        let blue = rgb(0x3987e5);
        assert_eq!(shade(blue, 0.), blue);
        let (h, s, l) = to_hsl(blue);
        let (h2, s2, l2) = to_hsl(shade(blue, 0.1));
        assert!((h - h2).abs() < 1e-4 && (s - s2).abs() < 1e-3);
        assert!((l2 - l - 0.1).abs() < 1e-4);
        let (r, g, b) = (blue.r, blue.g, blue.b);
        let back = from_hsl(h, s, l, 1.);
        assert!(
            (back.r - r).abs() < 1e-4 && (back.g - g).abs() < 1e-4 && (back.b - b).abs() < 1e-4
        );
    }

    #[test]
    fn a_long_path_folds_its_middle_and_keeps_both_ends() {
        // Eleven levels of about 80px each: far more than fits in 500.
        let widths = vec![80.; 11];
        let start = tail_start(&widths, 500.);
        assert!(start > 2, "several levels fold");
        let shown = 1 + (widths.len() - start);
        let used = 80. * shown as f32 + CRUMB_FOLD_W + CRUMB_SEP_W * shown as f32;
        assert!(used <= 500., "what stays fits: {used}");
        assert!(
            used + 80. + CRUMB_SEP_W > 500.,
            "and one more parent would not"
        );
        // Room for everything folds nothing; so does a two-level path.
        assert_eq!(tail_start(&widths, 2000.), 1);
        assert_eq!(tail_start(&[400., 400.], 100.), 1);
        // No room at all still keeps the root and the level on screen.
        assert_eq!(tail_start(&widths, 0.), widths.len() - 1);
    }

    #[test]
    fn a_crumb_is_measured_up_to_where_it_truncates() {
        assert!(crumb_width("data", false) < crumb_width("DATA", false));
        assert_eq!(crumb_width("大佬", false), 2. * ROW_PT + CRUMB_PAD_W);
        // A profile directory's id: wider than any crumb is allowed to be.
        let id = "1C621EFDC92004CCA1CE612C5AB01234";
        assert_eq!(crumb_width(id, false), CRUMB_MAX_W);
        // The level on screen is allowed more of it.
        let here = crumb_width(id, true);
        assert!(here > CRUMB_MAX_W && here <= CRUMB_HERE_MAX_W);
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
