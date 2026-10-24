//! Bundled static files: fonts, locales, and app icons.
//!
//! rust-embed's `compression` feature zstd-compresses each file into the
//! binary; [`get`] inflates on first read. With the gpui front end it also
//! implements gpui's `AssetSource`, so gpui can load `icons/power.svg` the
//! same way it loads `IconName` SVGs from gpui-kit.

#[cfg(feature = "gpui")]
use anyhow::anyhow;
#[cfg(feature = "gpui")]
use gpui::{AssetSource, Result, SharedString};
#[cfg(feature = "gpui")]
use gpui_kit::assets::Assets as ComponentAssets;
#[cfg(feature = "gpui")]
use gpui_kit::component::Icon;
use rust_embed::RustEmbed;
use std::borrow::Cow;

/// Embedded as an allowlist, not "the whole folder minus dotfiles".
///
/// rust-embed walks the directory and keeps every regular file it finds — it
/// does **not** skip dotfiles, so the Finder's `.DS_Store` droppings (22 KB
/// across three directories here) would be compressed into the binary and
/// listed by gpui's `AssetSource::list`, which is what gpui enumerates icons
/// with.
/// Naming the extensions instead of excluding known junk means nothing new
/// can be embedded by accident.
///
/// The `include` attributes need rust-embed's `include-exclude` feature. It
/// is declared in `Cargo.toml` rather than left to feature unification with
/// gpui-kit-assets, which happens to enable it today.
#[derive(RustEmbed)]
#[folder = "assets"]
#[include = "icons/*.svg"]
#[include = "fonts/*.ttf"]
#[include = "locales/*.toml"]
#[include = "cleanhints-*.toml"]
#[include = "caches-*.toml"]
#[include = "zstats-icon.png"]
pub struct Assets;

/// Bytes for an embedded path (`fonts/JetBrainsMono-Regular.ttf`).
pub fn get(path: &str) -> Option<Cow<'static, [u8]>> {
    Assets::get(path).map(|f| f.data)
}

#[cfg(feature = "gpui")]
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

/// The SVGs this app ships itself, because gpui-kit's `IconName` has no
/// equivalent for them.
///
/// Named rather than written out as paths at the call site: the filename then
/// exists once, so renaming or dropping an SVG fails to compile here instead
/// of silently leaving a hole in the tab strip. gpui's `Icon::path()`
/// resolves through `AssetSource` at paint time and cannot be checked any
/// earlier.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CustomIconName {
    AppWindow,
    Cpu,
    History,
    LogOut,
    /// The tray's memory face (`tray.rs`); lucide `memory-stick`.
    MemoryStick,
    /// The tray's disk face (`tray.rs`); lucide `hard-drive`. Not the
    /// Hardware tab's `IconName::HardDrive` — the tray rasterises its
    /// own glyphs as templates.
    HardDrive,
    Power,
    RefreshCw,
    Shield,
    /// Footer pin: keeps the panel from auto-hiding on focus loss.
    Pin,
    /// Footer keep-awake indicator; lucide `coffee`. gpui-kit has `Sun`
    /// and `Moon`, but both read as the theme picker's glyphs — this
    /// one has to say "the Mac is being held awake" and nothing else.
    Coffee,
    /// Move-to-Trash on the disk-space window's rows; lucide `trash-2`.
    /// gpui-kit's `IconName::Trash` names the same glyph but is not in its
    /// default embedded set, and its `Delete` is a backspace key — which
    /// read as "clear this field" on a control that moves files.
    Trash,
    /// The footer's form toggle while the panel is short: lucide
    /// `chevrons-up-down`, two chevrons pulling apart — open it out.
    Expand,
    /// The same toggle while the panel is long: lucide
    /// `chevrons-down-up`, the two chevrons closing.
    Collapse,
    /// The NSH front end's disclosure and menu chevrons (`nsh/widgets.rs`),
    /// lucide's, which the gpui front end takes from gpui-kit's own set
    /// (`IconName::ChevronDown`, `ChevronRight`). NSH has no icon set; it
    /// draws the glyphs this file names.
    ChevronDown,
    ChevronRight,
    /// The NSH panel's tab strip and footer (`nsh/chrome.rs`), lucide's:
    /// Overview, the Network and Alerts tabs, Settings. gpui takes these
    /// from gpui-kit (`IconName::LayoutDashboard`, `Network`, `Bell`,
    /// `Settings2`); Processes and Hardware reuse [`Self::Cpu`] and
    /// [`Self::HardDrive`], the same lucide glyphs.
    LayoutDashboard,
    Network,
    Bell,
    Settings,
}

impl CustomIconName {
    pub fn path(self) -> &'static str {
        match self {
            CustomIconName::AppWindow => "icons/app-window.svg",
            CustomIconName::Cpu => "icons/cpu.svg",
            CustomIconName::History => "icons/history.svg",
            CustomIconName::LogOut => "icons/log-out.svg",
            CustomIconName::MemoryStick => "icons/memory-stick.svg",
            CustomIconName::HardDrive => "icons/hard-drive.svg",
            CustomIconName::Power => "icons/power.svg",
            CustomIconName::RefreshCw => "icons/refresh-cw.svg",
            CustomIconName::Shield => "icons/shield.svg",
            CustomIconName::Pin => "icons/pin.svg",
            CustomIconName::Coffee => "icons/coffee.svg",
            CustomIconName::Trash => "icons/trash.svg",
            CustomIconName::Expand => "icons/chevrons-up-down.svg",
            CustomIconName::Collapse => "icons/chevrons-down-up.svg",
            CustomIconName::ChevronDown => "icons/chevron-down.svg",
            CustomIconName::ChevronRight => "icons/chevron-right.svg",
            CustomIconName::LayoutDashboard => "icons/layout-dashboard.svg",
            CustomIconName::Network => "icons/network.svg",
            CustomIconName::Bell => "icons/bell.svg",
            CustomIconName::Settings => "icons/settings-2.svg",
        }
    }
}

#[cfg(feature = "gpui")]
impl From<CustomIconName> for Icon {
    fn from(val: CustomIconName) -> Self {
        Icon::empty().path(val.path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use crate::cachepreset::FILE as CACHES_FILE;
    #[cfg(target_os = "macos")]
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
            CustomIconName::HardDrive,
            CustomIconName::Power,
            CustomIconName::RefreshCw,
            CustomIconName::Shield,
            CustomIconName::Pin,
            CustomIconName::Coffee,
            CustomIconName::Trash,
            CustomIconName::Expand,
            CustomIconName::Collapse,
            CustomIconName::ChevronDown,
            CustomIconName::ChevronRight,
            CustomIconName::LayoutDashboard,
            CustomIconName::Network,
            CustomIconName::Bell,
            CustomIconName::Settings,
        ] {
            let path = icon.path();
            assert!(get(path).is_some(), "{path} is not embedded");
        }
    }

    /// The allowlist has to keep covering the other two asset kinds, which
    /// are read through [`get`] rather than through gpui's `AssetSource`.
    #[test]
    fn fonts_and_locales_are_embedded() {
        assert!(get("fonts/JetBrainsMono-Regular.ttf").is_some());
        assert!(get("locales/en.toml").is_some());
        // Through the constant, not the literal: the hints file is named
        // per platform, and the allowlist glob has to keep matching it.
        // Only macOS ships one today — Linux gets its own in phase 6 of
        // docs/omarchy-port.md, and until then the runtime degrades to
        // "no hints" rather than failing, so there is nothing to assert.
        #[cfg(target_os = "macos")]
        {
            assert!(get(HINTS_FILE).is_some());
            assert!(get(CACHES_FILE).is_some());
        }
        assert!(get("zstats-icon.png").is_some());
        assert!(
            !Assets::iter().any(|p| p.ends_with(".DS_Store")),
            "the allowlist let a dotfile through"
        );
    }
}
