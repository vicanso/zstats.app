//! The panel's content as an NSH Rust root. Phase 0 of
//! docs/nsh-migration.md: a placeholder on Golden Dark's ground that says
//! what this build is, and Quit. Overview arrives in phase 4, once the core
//! (phase 1) can feed it.
//!
//! The root runs on its engine thread and never touches AppKit itself: what
//! it needs of the window or the process goes to the host as a
//! [`HostAction`], which `host.rs` carries to the main queue.

use crate::{APP_NAME, i18n};
use nsh_base::button;
use nsh_core::ui::prelude::*;
use std::sync::OnceLock;

/// What the panel asks of the AppKit side, which owns the window and the
/// process.
#[derive(Clone, Copy, Debug)]
pub enum HostAction {
    Quit,
}

static HOST: OnceLock<Box<dyn Fn(HostAction) + Send + Sync>> = OnceLock::new();

/// Wires the panel to the host, before the root mounts.
pub fn connect(host: impl Fn(HostAction) + Send + Sync + 'static) {
    let _ = HOST.set(Box::new(host));
}

fn host(action: HostAction) {
    if let Some(host) = HOST.get() {
        host(action);
    }
}

/// Golden Dark's tokens (`theme.rs`, `.claude/skills/ui-polish`), until
/// phase 1b makes the theme shared.
mod ink {
    pub const GROUND: u32 = 0x1c1c1eff;
    /// `theme::surface()`: a white lift that composites to `#252528`.
    pub const CARD: u32 = 0xffffff0a;
    pub const TEXT: u32 = 0xf2f0ebff;
    pub const MUTED: u32 = 0xa19d96ff;
    pub const DIM: u32 = 0x8f8b85ff;
    pub const CHIP: u32 = 0xffffff14;
}

/// `views::GUTTER` and `widgets::CARD_PAD`.
const GUTTER: f32 = 12.;
const CARD_PAD: f32 = 12.;

#[component]
pub fn Panel() -> impl IntoElement {
    view()
        .size_full()
        .bg(rgba(ink::GROUND))
        .p(px(GUTTER))
        .gap(px(10.))
        .child(
            view()
                .rounded(px(12.))
                .bg(rgba(ink::CARD))
                .p(px(CARD_PAD))
                .gap(px(6.))
                .child(
                    text(APP_NAME)
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgba(ink::TEXT)),
                )
                // Not translated: a development build's placeholder, gone
                // when Overview takes its place in phase 4.
                .child(
                    text("NSH front end — phase 0 of docs/nsh-migration.md")
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(rgba(ink::MUTED)),
                )
                .child(
                    text(env!("CARGO_PKG_VERSION"))
                        .text_size(px(11.))
                        .text_color(rgba(ink::DIM))
                        .tabular_nums(),
                ),
        )
        .child(view().flex_1())
        .child(
            view().flex_row().justify_end().child(
                button()
                    .h(px(24.))
                    .px(px(10.))
                    .rounded(px(6.))
                    .hover(|s| s.bg(rgba(ink::CHIP)))
                    .on_press(|| host(HostAction::Quit))
                    .child(
                        text(i18n::tr("common.quit"))
                            .text_size(px(12.))
                            .text_color(rgba(ink::MUTED)),
                    ),
            ),
        )
}
