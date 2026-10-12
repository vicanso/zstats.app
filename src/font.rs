//! The two faces a figure can wear, and which one.
//!
//! **Figures are in the UI face with fixed-width digits** ([`tnum`],
//! [`tabular`]): percentages, bytes, counts, durations, thresholds. They
//! were all on [`MONO`] until 2026-10-12, for the one thing mono buys —
//! a ticking value that does not change width — and the price was that
//! every number in the panel read as code. The `tnum` feature buys the
//! same stillness in the page's own type, with real Medium and Semibold
//! weights (the bundle ships Regular and Bold only).
//!
//! **Where MONO still goes**: the rates — network and disk throughput,
//! a program's traffic, a drive's operations and latency, swap moving —
//! and code-like identifiers (an executable's name, a listening
//! endpoint). A rate rewrites its digits *and* its unit every
//! couple of seconds; there the fixed advance of every glyph is what
//! keeps the line from shimmering, which fixed digits alone do not.
//!
//! Everything else stays on the UI face: names, labels, and sentences
//! that happen to contain numbers ("pid 51244 · peak 1.2 GB"). Names in
//! mono run ~25% wider — fatal at 320px.
//!
//! Bundled JetBrains Mono is registered at startup via [`register`] so
//! Regular/Bold resolve the same on every platform, instead of leaning
//! on Menlo / Consolas / DejaVu.

use gpui::{App, Div, FontFeatures, SharedString, Styled};
use gpui_kit::component::Theme;
use std::sync::{Arc, LazyLock};

use crate::assets;
use crate::i18n;

/// Family name inside the bundled TTF. Must match the name table exactly.
///
/// The bundle only ships Regular + Bold (`JetBrainsMono-*.ttf`). Medium /
/// Semibold have no face — gpui will silently pick another family. Headline
/// figures use Bold; everything else stays Regular.
pub const MONO: &str = "JetBrains Mono";

/// [`MONO`], unless the element's text is a translated sentence.
///
/// JetBrains Mono has no Han glyphs at all, so "读 12 MB/s · 写 3 MB/s"
/// in it is the worst text in the app: every Han character falls back to
/// PingFang at a size chosen for digits, inside a face picked for
/// tabular figures it cannot supply. The sites that render a whole
/// translated string as one value (the volume card's R/W line, History's
/// peak line) hand the element back to the UI face in a CJK locale and
/// keep mono everywhere else, where the alignment it buys is real.
/// Layout-neutral by construction: the line box is size × line-height,
/// independent of family — only widths shift.
///
/// Not a general escape hatch: a standalone figure stays on [`MONO`]
/// unconditionally — `format` emits ASCII only.
pub fn mono_unless_cjk(d: Div) -> Div {
    if i18n::is_cjk() {
        d
    } else {
        d.font_family(MONO)
    }
}

/// Fixed-width digits in whatever face the element has. Built once: a
/// frame asks for it a couple of hundred times and the set never changes.
static TNUM: LazyLock<FontFeatures> =
    LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".to_string(), 1)])));

/// The feature set of a figure, for a chain that is already styling an
/// element: `.font_features(font::tnum())`. The module doc says which
/// figures take it and which stay on [`MONO`].
pub fn tnum() -> FontFeatures {
    TNUM.clone()
}

/// A `div` for a figure: the UI face with fixed-width digits, so a value
/// ticking over does not shove what sits beside it.
pub fn tabular(d: Div) -> Div {
    d.font_features(tnum())
}

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
            tracing::error!("missing bundled font: {p}");
        }
        bytes
    })
    .collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        tracing::error!("failed to register JetBrains Mono: {e}");
    }
}

/// Point gpui-kit's theme at the bundled family so its own widgets
/// (and anything reading `Theme::mono_font_family`) match ours.
///
/// `Theme::change` rebuilds Theme from stock defaults (Menlo on macOS), so
/// this has to run *after* every theme switch.
///
/// Through `Theme::update`, not `global_mut`: since gpui-kit 0.7 the Base
/// layer keeps its own projection of the theme, mono family included
/// (`tokens.mono`), and a `global_mut` edit leaves that projection on the
/// old family until someone rebuilds it. `update` rebuilds it and
/// refreshes the windows. Its font resolution leaves a non-default family
/// alone, so the bundled face is not swapped for an "installed" stand-in.
pub fn apply(cx: &mut App) {
    Theme::update(cx, |theme| {
        theme.mono_font_family = SharedString::from(MONO);
    });
}
