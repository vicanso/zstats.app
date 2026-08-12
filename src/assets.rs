//! Bundled static files: fonts, locales, and app icons.
//!
//! rust-embed's `compression` feature zstd-compresses each file into the
//! binary; [`get`] inflates on first read. Also implements [`AssetSource`]
//! so gpui can load `icons/power.svg` the same way it loads `IconName` SVGs
//! from gpui-component.

use gpui::{AssetSource, Result, SharedString};
use gpui_component_assets::Assets as ComponentAssets;
use rust_embed::RustEmbed;
use std::borrow::Cow;

#[derive(RustEmbed)]
#[folder = "assets"]
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
        Ok(Self::get(path).map(|f| f.data))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut files: Vec<SharedString> = ComponentAssets::iter()
            .filter_map(|p| p.starts_with(path).then(|| p.into()))
            .collect();
        files.extend(Self::iter().filter_map(|p| p.starts_with(path).then(|| p.into())));
        Ok(files)
    }
}
