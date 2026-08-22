//! Bundled JetBrains Mono — tabular figures for ticking metrics.
//!
//! Registered at startup via [`register`] so Regular/Bold resolve the same
//! on every platform, instead of leaning on Menlo / Consolas / DejaVu.
//!
//! **Where MONO goes** — the rule, since font choice is per-element:
//! standalone data values (percentages, bytes, temperatures, rates,
//! core-time, current thresholds) and code-like identifiers (a pid tag
//! beside a duplicated name). Mono earns its place by keeping a ticking
//! figure from changing width and by lining values up across rows —
//! reasons that only apply to numbers standing on their own.
//!
//! Everything else stays on the UI face: names, labels, and sentences
//! that happen to contain numbers ("pid 51244 · peak 1.2 GB"). Names in
//! mono run ~25% wider — fatal at 320px — and a caption that switches
//! face mid-sentence is noise without an alignment payoff. The split is
//! also what keeps the panel reading as a native card, not a terminal.

use gpui::{App, Div, SharedString, Styled};
use gpui_component::Theme;

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

/// Point gpui-component's theme at the bundled family so its own widgets
/// (and anything reading `Theme::mono_font_family`) match ours.
///
/// `Theme::change` rebuilds Theme from stock defaults (Menlo on macOS), so
/// this has to run *after* every theme switch.
pub fn apply(cx: &mut App) {
    Theme::global_mut(cx).mono_font_family = SharedString::from(MONO);
}
