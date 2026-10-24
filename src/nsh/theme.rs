//! The theme on the NSH front end: the same preference and the same
//! tokens as the gpui one (`prefs::theme_in_force`, `palette.rs`), handed
//! to NSH the way it paints.
//!
//! The gpui front end repaints every token when the appearance turns
//! (`apply_appearance`, then `Theme::change`). NSH does not need asking:
//! a colour given to it carries a light and a dark value
//! (`AdaptiveColor`), and its engine picks one as the appearance turns,
//! the system's or the one a root was loaded in. So a token is read here
//! in both halves of the pair on screen ([`color`], `palette::in_mode`),
//! and only a change of pair — classic, Golden, Omarchy, which is the
//! Interface page's to make — needs the roots loaded again.
//!
//! What NSH resolved comes back to the host from the panel's root
//! ([`follow`]): the palette's own idea of light or dark is read outside
//! any engine too — the tray's ink on Linux, the opacity's default.

use crate::color::Rgba;
use crate::prefs::{self, ThemePref};
use crate::{omarchy, palette};
use nsh_core::ui::prelude::*;

/// The appearance a root is loaded in: the one the theme preference pins
/// (Light, Dark, or an Omarchy palette's own), the system's otherwise —
/// Golden is a pair, not a third appearance, and follows it as System
/// does.
pub fn appearance() -> Appearance {
    match prefs::theme_in_force() {
        ThemePref::Light => Appearance::Light,
        ThemePref::Dark => Appearance::Dark,
        ThemePref::Omarchy if omarchy::palette().dark => Appearance::Dark,
        ThemePref::Omarchy => Appearance::Light,
        ThemePref::System | ThemePref::Golden => Appearance::System,
    }
}

/// Paints the palette for `dark`, the appearance NSH resolved: Golden's
/// half or the classic pair's, or the Omarchy palette, which is light or
/// dark by itself.
pub fn follow(dark: bool) {
    match prefs::theme_in_force() {
        ThemePref::Golden => palette::set_golden(dark),
        ThemePref::Omarchy => palette::set_omarchy(&omarchy::palette()),
        ThemePref::System | ThemePref::Light | ThemePref::Dark => palette::set_dark(dark),
    }
}

/// `token` in both halves of the pair on screen, for NSH to choose from.
pub fn color(token: fn() -> Rgba) -> AdaptiveColor {
    let half = |dark| -> Hsla {
        let rgba: nsh_core::Rgba = palette::in_mode(dark, token).into();
        rgba.into()
    };
    AdaptiveColor {
        light: half(false),
        dark: half(true),
    }
}
