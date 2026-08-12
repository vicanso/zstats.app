//! Bundled JetBrains Mono — tabular figures for ticking metrics.
//!
//! Registered at startup via [`register`] so Regular/Bold resolve the same
//! on every platform, instead of leaning on Menlo / Consolas / DejaVu.

use gpui::{App, SharedString};
use gpui_component::Theme;

use crate::assets;

/// Family name inside the bundled TTF. Must match the name table exactly.
///
/// The bundle only ships Regular + Bold (`JetBrainsMono-*.ttf`). Medium /
/// Semibold have no face — gpui will silently pick another family. Headline
/// figures use Bold; everything else stays Regular.
pub const MONO: &str = "JetBrains Mono";

/// Load the two faces into gpui's text system. Call once, before the first
/// frame that paints a metric. Bytes come from [`assets`] (compressed in
/// release); only Regular and Bold exist in the bundle.
pub fn register(cx: &App) {
    let fonts = [
        "fonts/JetBrainsMono-Regular.ttf",
        "fonts/JetBrainsMono-Bold.ttf",
    ]
    .into_iter()
    .filter_map(|p| {
        let bytes = assets::get(p);
        if bytes.is_none() {
            eprintln!("missing bundled font: {p}");
        }
        bytes
    })
    .collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        eprintln!("failed to register JetBrains Mono: {e}");
    }
}

/// Point gpui-component's theme at the bundled family so its own widgets
/// (and anything reading `Theme::mono_font_family`) match ours.
///
/// `Theme::change` rebuilds Theme from stock defaults (Menlo on macOS), so
/// this has to run *after* every theme switch.
pub fn apply(cx: &mut App) {
    Theme::global_mut(cx).mono_font_family = SharedString::from(MONO);
}
