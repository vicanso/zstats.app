//! Squarified treemap layout (Bruls, Huizing & van Wijk, 2000) — the
//! arithmetic behind the disk window's directory map, kept free of gpui
//! so it can be tested on plain numbers.
//!
//! Squarified rather than slice-and-dice: alternating strips give a
//! folder with forty subfolders forty hairlines, where a reader compares
//! areas only between shapes that are close to square. The algorithm
//! fills the shorter side of what is left with a row, adding items while
//! that row's worst aspect ratio improves, then starts the next row in
//! the remainder. Input order is kept, so callers pass sizes largest
//! first — the order the method assumes and the one a reader scans in.
//!
//! [`colour`] is the other half: which hue each laid-out tile wears, so
//! that no two tiles sharing an edge wear the same one.

/// An axis-aligned rectangle, in whatever unit the caller lays out in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Shrunk by `by` on every side, never below zero.
    pub fn inset(self, by: f32) -> Self {
        let w = (self.w - 2. * by).max(0.);
        let h = (self.h - 2. * by).max(0.);
        Self::new(self.x + by, self.y + by, w, h)
    }
}

/// One rectangle per size, in input order, together tiling `bounds` in
/// proportion to the sizes. A zero size gets an empty rectangle at the
/// corner of what was left; an empty or zero-area input gets nothing to
/// draw rather than a division by zero.
pub fn layout(sizes: &[u64], bounds: Rect) -> Vec<Rect> {
    let total: f64 = sizes.iter().map(|&s| s as f64).sum();
    let area = f64::from(bounds.w) * f64::from(bounds.h);
    let empty = Rect::new(bounds.x, bounds.y, 0., 0.);
    if total <= 0. || area <= 0. {
        return vec![empty; sizes.len()];
    }
    let scale = area / total;
    let areas: Vec<f64> = sizes.iter().map(|&s| s as f64 * scale).collect();
    let (mut x, mut y) = (f64::from(bounds.x), f64::from(bounds.y));
    let (mut w, mut h) = (f64::from(bounds.w), f64::from(bounds.h));
    let mut out = Vec::with_capacity(sizes.len());
    let mut start = 0;
    while start < areas.len() {
        let side = w.min(h);
        let mut end = start + 1;
        let mut best = worst(&areas[start..end], side);
        while end < areas.len() {
            let next = worst(&areas[start..=end], side);
            if next > best {
                break;
            }
            best = next;
            end += 1;
        }
        let row = &areas[start..end];
        let row_area: f64 = row.iter().sum();
        if w >= h {
            // A column down the left of what is left.
            let strip = if h > 0. { row_area / h } else { 0. };
            let mut top = y;
            for &a in row {
                let tall = if strip > 0. { a / strip } else { 0. };
                out.push(rect(x, top, strip, tall));
                top += tall;
            }
            x += strip;
            w = (w - strip).max(0.);
        } else {
            // A row along the top.
            let strip = if w > 0. { row_area / w } else { 0. };
            let mut left = x;
            for &a in row {
                let wide = if strip > 0. { a / strip } else { 0. };
                out.push(rect(left, y, wide, strip));
                left += wide;
            }
            y += strip;
            h = (h - strip).max(0.);
        }
        start = end;
    }
    out
}

/// The worst aspect ratio in a row of `areas` laid along `side`.
fn worst(areas: &[f64], side: f64) -> f64 {
    let sum: f64 = areas.iter().sum();
    let max = areas.iter().copied().fold(0., f64::max);
    let min = areas.iter().copied().fold(f64::INFINITY, f64::min);
    if sum <= 0. || min <= 0. || side <= 0. {
        return f64::INFINITY;
    }
    let side2 = side * side;
    let sum2 = sum * sum;
    (side2 * max / sum2).max(sum2 / (side2 * min))
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect::new(x as f32, y as f32, w as f32, h as f32)
}

/// How close two edges must be to count as shared — layout arithmetic
/// leaves them a rounding error apart, never a gap.
const TOUCH: f32 = 0.5;

/// How much of an edge two tiles must share to be neighbours. Tiles that
/// meet only at a corner are not: the 2px gaps keep a corner from
/// reading as a border, and counting them would run out of hues sooner.
const SHARED_MIN: f32 = 1.;

/// Whether `a` and `b` share a stretch of edge.
fn touching(a: &Rect, b: &Rect) -> bool {
    let overlap = |a0: f32, a1: f32, b0: f32, b1: f32| a1.min(b1) - a0.max(b0);
    let side_by_side = ((a.x + a.w) - b.x).abs() < TOUCH || ((b.x + b.w) - a.x).abs() < TOUCH;
    let stacked = ((a.y + a.h) - b.y).abs() < TOUCH || ((b.y + b.h) - a.y).abs() < TOUCH;
    (side_by_side && overlap(a.y, a.y + a.h, b.y, b.y + b.h) > SHARED_MIN)
        || (stacked && overlap(a.x, a.x + a.w, b.x, b.x + b.w) > SHARED_MIN)
}

/// Search steps before [`colour`] settles for its fallback. Five hues
/// always colour a map (it is planar), and the search finds one in a few
/// dozen steps; the budget is for a palette too short to — Omarchy's
/// three — where an exhaustive search would run every frame for nothing.
const SEARCH_BUDGET: usize = 4_000;

/// One hue index per tile of a [`layout`], in `0..hues`, never the hue
/// of a tile sharing its edge. Tiles are taken in order — largest first,
/// as laid out — so the largest always wears hue 0, and each tries the
/// allowed hues least used so far first, which spreads the palette across
/// the map instead of piling it on the first few. A depth-first search
/// backs up when a tile is boxed in; a palette too short for the map
/// gets the assignment breaking the fewest edges instead.
pub fn colour(rects: &[Rect], hues: usize) -> Vec<usize> {
    if hues == 0 {
        return vec![0; rects.len()];
    }
    let near: Vec<Vec<usize>> = (0..rects.len())
        .map(|i| (0..i).filter(|&j| touching(&rects[i], &rects[j])).collect())
        .collect();
    let mut search = Search {
        near: &near,
        hues,
        picks: vec![0; rects.len()],
        used: vec![0; hues],
        budget: SEARCH_BUDGET,
    };
    if search.place(0) {
        return search.picks;
    }
    fewest_clashes(&near, hues)
}

struct Search<'a> {
    near: &'a [Vec<usize>],
    hues: usize,
    picks: Vec<usize>,
    used: Vec<usize>,
    budget: usize,
}

impl Search<'_> {
    fn place(&mut self, i: usize) -> bool {
        if i == self.near.len() {
            return true;
        }
        if self.budget == 0 {
            return false;
        }
        self.budget -= 1;
        let mut allowed: Vec<usize> = (0..self.hues)
            .filter(|&hue| self.near[i].iter().all(|&j| self.picks[j] != hue))
            .collect();
        allowed.sort_by_key(|&hue| (self.used[hue], hue));
        for hue in allowed {
            self.picks[i] = hue;
            self.used[hue] += 1;
            if self.place(i + 1) {
                return true;
            }
            self.used[hue] -= 1;
        }
        false
    }
}

/// The fallback: each tile, in order, the hue fewest of its placed
/// neighbours wear, least used on a tie.
fn fewest_clashes(near: &[Vec<usize>], hues: usize) -> Vec<usize> {
    let mut picks: Vec<usize> = Vec::with_capacity(near.len());
    let mut used = vec![0usize; hues];
    for tile in near {
        let hue = (0..hues)
            .min_by_key(|&hue| {
                let same = tile.iter().filter(|&&j| picks[j] == hue).count();
                (same, used[hue], hue)
            })
            .unwrap_or(0);
        used[hue] += 1;
        picks.push(hue);
    }
    picks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(r: &Rect) -> f32 {
        r.w * r.h
    }

    fn inside(r: &Rect, b: &Rect) -> bool {
        let eps = 0.01;
        r.x >= b.x - eps
            && r.y >= b.y - eps
            && r.x + r.w <= b.x + b.w + eps
            && r.y + r.h <= b.y + b.h + eps
    }

    fn overlap(a: &Rect, b: &Rect) -> bool {
        let eps = 0.01;
        a.x + eps < b.x + b.w
            && b.x + eps < a.x + a.w
            && a.y + eps < b.y + b.h
            && b.y + eps < a.y + a.h
    }

    #[test]
    fn areas_follow_the_sizes_and_tile_the_bounds() {
        let bounds = Rect::new(10., 20., 600., 400.);
        let sizes = [500, 300, 120, 50, 20, 10];
        let rects = layout(&sizes, bounds);
        assert_eq!(rects.len(), sizes.len());
        let total: u64 = sizes.iter().sum();
        for (rect, size) in rects.iter().zip(sizes) {
            let want = area(&bounds) * size as f32 / total as f32;
            assert!((area(rect) - want).abs() < 1., "{rect:?} for {size}");
            assert!(inside(rect, &bounds), "{rect:?} leaves {bounds:?}");
        }
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(!overlap(a, b), "{a:?} overlaps {b:?}");
            }
        }
        let covered: f32 = rects.iter().map(area).sum();
        assert!((covered - area(&bounds)).abs() < 1.);
    }

    #[test]
    fn equal_sizes_come_out_close_to_square() {
        let rects = layout(&[1; 16], Rect::new(0., 0., 400., 400.));
        for rect in &rects {
            let ratio = rect.w.max(rect.h) / rect.w.min(rect.h);
            assert!(ratio < 1.5, "{rect:?} is a strip, not a tile");
        }
    }

    #[test]
    fn nothing_to_divide_draws_nothing() {
        assert!(
            layout(&[0, 0], Rect::new(0., 0., 100., 100.))
                .iter()
                .all(|r| area(r) == 0.)
        );
        assert!(
            layout(&[5, 3], Rect::new(0., 0., 0., 100.))
                .iter()
                .all(|r| area(r) == 0.)
        );
        assert!(layout(&[], Rect::new(0., 0., 100., 100.)).is_empty());
        let rects = layout(&[10, 0], Rect::new(0., 0., 100., 50.));
        assert!((area(&rects[0]) - 5000.).abs() < 1.);
        assert_eq!(area(&rects[1]), 0.);
    }

    fn neighbours_sharing_a_hue(rects: &[Rect], picks: &[usize]) -> usize {
        let mut shared = 0;
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                if touching(&rects[i], &rects[j]) && picks[i] == picks[j] {
                    shared += 1;
                }
            }
        }
        shared
    }

    #[test]
    fn neighbours_never_share_a_hue() {
        for (count, w, h) in [(30, 900., 600.), (41, 1300., 760.), (12, 400., 300.)] {
            let sizes: Vec<u64> = (1..=count).rev().map(|n| n * n + 7).collect();
            let rects = layout(&sizes, Rect::new(0., 0., w, h));
            let picks = colour(&rects, 5);
            assert_eq!(picks[0], 0, "the largest tile wears the first hue");
            assert_eq!(neighbours_sharing_a_hue(&rects, &picks), 0);
            let mut seen = picks.clone();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(
                seen.len(),
                5,
                "the whole palette is used, not the first hues"
            );
        }
    }

    #[test]
    fn a_short_palette_still_colours_every_tile() {
        let sizes: Vec<u64> = (1..=41).rev().map(|n| n * n).collect();
        let rects = layout(&sizes, Rect::new(0., 0., 1300., 760.));
        let picks = colour(&rects, 3);
        assert_eq!(picks.len(), rects.len());
        assert!(picks.iter().all(|&hue| hue < 3));
        assert_eq!(colour(&rects, 0), vec![0; rects.len()]);
    }

    #[test]
    fn a_corner_is_not_an_edge() {
        let a = Rect::new(0., 0., 10., 10.);
        assert!(touching(&a, &Rect::new(10., 2., 10., 10.)));
        assert!(touching(&a, &Rect::new(3., 10., 4., 4.)));
        assert!(!touching(&a, &Rect::new(10., 10., 5., 5.)), "corner only");
        assert!(!touching(&a, &Rect::new(12., 0., 5., 5.)), "a gap apart");
    }

    #[test]
    fn inset_never_goes_negative() {
        assert_eq!(
            Rect::new(0., 0., 10., 4.).inset(3.),
            Rect::new(3., 3., 4., 0.)
        );
    }
}
