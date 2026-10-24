//! The settings window's content as an NSH Rust root: a placeholder that
//! names the window and its pages. The pages themselves — Interface,
//! Alerts, Advanced, Permissions, About — arrive in phase 4 of
//! docs/nsh-migration.md.
//!
//! Its keys are the gpui settings window's dismissals: Escape, and `mod+w`
//! through the host's Window menu.

use crate::nsh::root::{self, AuxWindow, CARD_PAD, GUTTER, HostAction};
use crate::nsh::theme;
use crate::{i18n, palette};
use nsh_core::ui::prelude::*;

#[component]
pub fn Settings() -> impl IntoElement {
    on_key("escape", |_| {
        root::ask(HostAction::Close(AuxWindow::Settings))
    });
    view()
        .size_full()
        .bg(theme::color(palette::ground))
        .p(px(GUTTER))
        .child(
            view()
                .rounded(px(12.))
                .bg(theme::color(palette::surface))
                .p(px(CARD_PAD))
                .gap(px(6.))
                .child(
                    text(i18n::tr("tabs.settings"))
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::color(palette::text)),
                )
                // Not translated: a development build's placeholder.
                .child(
                    text("Interface · Alerts · Advanced · Permissions · About — phase 4 of docs/nsh-migration.md")
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(theme::color(palette::text_muted)),
                ),
        )
}
