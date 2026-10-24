//! Rectangles in logical points, top-left origin with y growing down — the
//! space `placement.rs` does its arithmetic in (gpui's, which it was moved
//! off in phase 1b of docs/nsh-migration.md); the AppKit host converts at
//! its edge.

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
}
