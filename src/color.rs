//! The colour every module paints with: straight RGBA in 0–1, the shape
//! and the hex constructors gpui's `Rgba` had (and NSH's has), so the
//! palette (`palette.rs`) and the Omarchy reader (`omarchy.rs`) kept their
//! values when they left gpui's type (docs/nsh-migration.md, phase 1b).
//! NSH takes it through the `From` impl below.

/// A colour, each channel and the alpha in 0–1, not premultiplied.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// An opaque colour from `0xRRGGBB`.
pub const fn rgb(hex: u32) -> Rgba {
    Rgba {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// A colour from `0xRRGGBBAA`.
pub const fn rgba(hex: u32) -> Rgba {
    Rgba {
        r: ((hex >> 24) & 0xff) as f32 / 255.0,
        g: ((hex >> 16) & 0xff) as f32 / 255.0,
        b: ((hex >> 8) & 0xff) as f32 / 255.0,
        a: (hex & 0xff) as f32 / 255.0,
    }
}

impl From<Rgba> for nsh_core::color::Rgba {
    fn from(c: Rgba) -> Self {
        nsh_core::color::Rgba {
            r: c.r,
            g: c.g,
            b: c.b,
            a: c.a,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_come_out_of_the_hex_in_order() {
        let c = rgba(0x11223344);
        assert_eq!(
            (c.r, c.g, c.b, c.a),
            (
                0x11 as f32 / 255.0,
                0x22 as f32 / 255.0,
                0x33 as f32 / 255.0,
                0x44 as f32 / 255.0
            )
        );
        assert_eq!(rgb(0x112233).a, 1.0);
    }
}
