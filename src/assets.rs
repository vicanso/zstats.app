//! Bundled static files: fonts, locales, and app icons.
//!
//! rust-embed's `compression` feature zstd-compresses each file into the
//! binary; [`get`] inflates on first read. Also implements [`AssetSource`]
//! so gpui can load `icons/power.svg` the same way it loads `IconName` SVGs
//! from gpui-component.

use anyhow::anyhow;
use gpui::{AssetSource, Result, SharedString};
use gpui_component::Icon;
use gpui_component_assets::Assets as ComponentAssets;
use rust_embed::RustEmbed;
use std::borrow::Cow;

/// Embedded as an allowlist, not "the whole folder minus dotfiles".
///
/// rust-embed walks the directory and keeps every regular file it finds — it
/// does **not** skip dotfiles, so the Finder's `.DS_Store` droppings (22 KB
/// across three directories here) would be compressed into the binary and
/// listed by [`AssetSource::list`], which is what gpui enumerates icons with.
/// Naming the extensions instead of excluding known junk means nothing new
/// can be embedded by accident.
///
/// The `include` attributes need rust-embed's `include-exclude` feature. It
/// is declared in `Cargo.toml` rather than left to feature unification with
/// gpui-component-assets, which happens to enable it today.
#[derive(RustEmbed)]
#[folder = "assets"]
#[include = "icons/*.svg"]
#[include = "fonts/*.ttf"]
#[include = "locales/*.toml"]
#[include = "cleanhints-*.toml"]
#[include = "zstats-icon.png"]
pub struct Assets;

/// Bytes for an embedded path (`fonts/JetBrainsMono-Regular.ttf`).
pub fn get(path: &str) -> Option<Cow<'static, [u8]>> {
    Assets::get(path).map(|f| f.data)
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        if let Some(file) = ComponentAssets::get(path) {
            return Ok(Some(file.data));
        }
        // An unknown path is an error rather than `Ok(None)`: gpui renders a
        // missing SVG as empty space either way, but only the error reaches
        // the log and says which path it was.
        Self::get(path)
            .map(|f| Some(f.data))
            .ok_or_else(|| anyhow!(r#"could not find asset at path "{path}""#))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut files: Vec<SharedString> = ComponentAssets::iter()
            .filter_map(|p| p.starts_with(path).then(|| p.into()))
            .collect();
        files.extend(Self::iter().filter_map(|p| p.starts_with(path).then(|| p.into())));
        Ok(files)
    }
}

/// The SVGs this app ships itself, because gpui-component's `IconName` has no
/// equivalent for them.
///
/// Named rather than written out as paths at the call site: the filename then
/// exists once, so renaming or dropping an SVG fails to compile here instead
/// of silently leaving a hole in the tab strip. `Icon::path()` resolves
/// through [`AssetSource`] at paint time and cannot be checked any earlier.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CustomIconName {
    AppWindow,
    Cpu,
    History,
    LogOut,
    /// The tray's memory face (`tray.rs`); lucide `memory-stick`.
    MemoryStick,
    Power,
    RefreshCw,
    Shield,
}

impl CustomIconName {
    pub fn path(self) -> SharedString {
        match self {
            CustomIconName::AppWindow => "icons/app-window.svg",
            CustomIconName::Cpu => "icons/cpu.svg",
            CustomIconName::History => "icons/history.svg",
            CustomIconName::LogOut => "icons/log-out.svg",
            CustomIconName::MemoryStick => "icons/memory-stick.svg",
            CustomIconName::Power => "icons/power.svg",
            CustomIconName::RefreshCw => "icons/refresh-cw.svg",
            CustomIconName::Shield => "icons/shield.svg",
        }
        .into()
    }
}

impl From<CustomIconName> for Icon {
    fn from(val: CustomIconName) -> Self {
        Icon::empty().path(val.path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanhints::FILE as HINTS_FILE;

    /// Every name resolves to a file that is actually embedded. Without this
    /// a typo — or an `include` pattern that stops matching — shows up as a
    /// blank icon at runtime and nothing else.
    #[test]
    fn every_custom_icon_is_embedded() {
        for icon in [
            CustomIconName::AppWindow,
            CustomIconName::Cpu,
            CustomIconName::History,
            CustomIconName::LogOut,
            CustomIconName::MemoryStick,
            CustomIconName::Power,
            CustomIconName::RefreshCw,
            CustomIconName::Shield,
        ] {
            let path = icon.path();
            assert!(get(&path).is_some(), "{path} is not embedded");
        }
    }

    /// The allowlist has to keep covering the other two asset kinds, which
    /// are read through [`get`] rather than through [`AssetSource`].
    #[test]
    fn fonts_and_locales_are_embedded() {
        assert!(get("fonts/JetBrainsMono-Regular.ttf").is_some());
        assert!(get("locales/en.toml").is_some());
        // Through the constant, not the literal: the hints file is named
        // per platform, and the allowlist glob has to keep matching it.
        assert!(get(HINTS_FILE).is_some());
        assert!(get("zstats-icon.png").is_some());
        assert!(
            !Assets::iter().any(|p| p.ends_with(".DS_Store")),
            "the allowlist let a dotfile through"
        );
    }
}
