//! The colour every shared module paints with: straight RGBA in 0–1, the
//! same shape and the same hex constructors as gpui's `Rgba` (and NSH's),
//! so the palette (`palette.rs`) and the Omarchy reader (`omarchy.rs`)
//! read as they did when they returned gpui's type. Each front end converts
//! at its own edge, through the `From` impls below (docs/nsh-migration.md,
//! phase 1b).

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

#[cfg(feature = "gpui")]
impl From<Rgba> for gpui::Rgba {
    fn from(c: Rgba) -> Self {
        gpui::Rgba {
            r: c.r,
            g: c.g,
            b: c.b,
            a: c.a,
        }
    }
}

#[cfg(feature = "gpui")]
impl From<gpui::Rgba> for Rgba {
    fn from(c: gpui::Rgba) -> Self {
        Rgba {
            r: c.r,
            g: c.g,
            b: c.b,
            a: c.a,
        }
    }
}

#[cfg(feature = "nsh")]
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

    /// The same channels gpui's own constructors give, so a palette moved
    /// off gpui's type paints exactly as it did on it.
    #[cfg(feature = "gpui")]
    #[test]
    fn the_constructors_match_gpuis() {
        for hex in [0x000000, 0xffffff, 0xd9ae7a, 0x1c1c1e, 0xec3013] {
            assert_eq!(gpui::Rgba::from(rgb(hex)), gpui::rgb(hex));
        }
        for hex in [0x00000000, 0xffffff0a, 0x7aa2f715, 0xd9ae7a29] {
            assert_eq!(gpui::Rgba::from(rgba(hex)), gpui::rgba(hex));
        }
    }

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
