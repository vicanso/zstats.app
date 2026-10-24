//! Rectangles in logical points, top-left origin with y growing down — the
//! space gpui lays windows out in, and the one `placement.rs` does its
//! arithmetic in. Shared so the panel's sizes and the hang-it-off-the-tray
//! geometry serve both front ends (docs/nsh-migration.md, phase 1b); the
//! gpui one converts at its edge through the `From` impls below.

/// A point.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// A size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

/// A rectangle: its top-left corner and its size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

pub const fn point(x: f32, y: f32) -> Point {
    Point { x, y }
}

pub const fn size(width: f32, height: f32) -> Size {
    Size { width, height }
}

impl Rect {
    pub fn center(&self) -> Point {
        point(
            self.origin.x + self.size.width / 2.,
            self.origin.y + self.size.height / 2.,
        )
    }

    /// Whether `p` lies inside, the top and left edges included and the
    /// bottom and right excluded — gpui's `Bounds::contains`, so a point on
    /// the seam between two screens belongs to exactly one of them.
    pub fn contains(&self, p: &Point) -> bool {
        p.x >= self.origin.x
            && p.x < self.origin.x + self.size.width
            && p.y >= self.origin.y
            && p.y < self.origin.y + self.size.height
    }
}

#[cfg(feature = "gpui")]
mod gpui_edge {
    use super::{Point, Rect, Size, point, size};
    use gpui::{Bounds, Pixels, px};

    impl From<Point> for gpui::Point<Pixels> {
        fn from(p: Point) -> Self {
            gpui::point(px(p.x), px(p.y))
        }
    }

    impl From<gpui::Point<Pixels>> for Point {
        fn from(p: gpui::Point<Pixels>) -> Self {
            point(f32::from(p.x), f32::from(p.y))
        }
    }

    impl From<Size> for gpui::Size<Pixels> {
        fn from(s: Size) -> Self {
            gpui::size(px(s.width), px(s.height))
        }
    }

    impl From<gpui::Size<Pixels>> for Size {
        fn from(s: gpui::Size<Pixels>) -> Self {
            size(f32::from(s.width), f32::from(s.height))
        }
    }

    impl From<Rect> for Bounds<Pixels> {
        fn from(r: Rect) -> Self {
            Bounds {
                origin: r.origin.into(),
                size: r.size.into(),
            }
        }
    }

    impl From<Bounds<Pixels>> for Rect {
        fn from(b: Bounds<Pixels>) -> Self {
            Rect {
                origin: b.origin.into(),
                size: b.size.into(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_on_the_seam_belongs_to_the_screen_on_its_right() {
        let left = Rect {
            origin: point(0., 0.),
            size: size(1440., 900.),
        };
        let right = Rect {
            origin: point(1440., 0.),
            size: size(1920., 1080.),
        };
        let seam = point(1440., 10.);
        assert!(!left.contains(&seam));
        assert!(right.contains(&seam));
        assert_eq!(left.center(), point(720., 450.));
    }

    /// The same answers gpui's own rectangle gives, so geometry moved off
    /// gpui's types places the panel exactly where it did on them.
    #[cfg(feature = "gpui")]
    #[test]
    fn contains_and_center_match_gpuis() {
        use gpui::{Bounds, Pixels};
        let r = Rect {
            origin: point(10., 24.),
            size: size(358., 828.),
        };
        let b: Bounds<Pixels> = r.into();
        for p in [
            point(10., 24.),
            point(368., 24.),
            point(9.9, 30.),
            point(200., 852.),
            point(200., 851.9),
        ] {
            assert_eq!(r.contains(&p), b.contains(&p.into()), "{p:?}");
        }
        assert_eq!(Point::from(b.center()), r.center());
        assert_eq!(Rect::from(b), r);
    }
}
