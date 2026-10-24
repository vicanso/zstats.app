//! The disk-space window's content as an NSH Rust root: a placeholder that
//! names the window and the tab it is on. The directory map and the
//! duplicates list arrive in phase 4 of docs/nsh-migration.md.
//!
//! Its keys are the gpui window's: `mod+1` and `mod+2` pick the tab,
//! `mod+r` runs the tab's walk again, Escape closes (and `mod+w`, through
//! the host's Window menu).

use crate::nsh::root::{self, AuxWindow, CARD_PAD, GUTTER, HostAction, Live};
use crate::nsh::{store, theme};
use crate::state::{StorageTab, ZStatsAppState};
use crate::{i18n, palette};
use nsh_core::ui::prelude::*;

/// Not translated: a development build's line.
fn tab_line(state: &ZStatsAppState) -> String {
    format!(
        "Tab {:?} — phase 4 of docs/nsh-migration.md",
        state.storage_tab()
    )
}

#[component]
pub fn Storage() -> impl IntoElement {
    // The window is there to be drawn for as long as it exists.
    let live = Live::follow(|_| true);
    for (key, tab) in [
        ("mod+1", StorageTab::Analysis),
        ("mod+2", StorageTab::Duplicates),
    ] {
        on_key(key, move |_| {
            store::update(|state, fx| state.set_storage_tab(tab, fx));
        });
    }
    on_key("mod+r", |_| {
        store::update(|state, fx| state.rerun_storage_tab(fx))
    });
    on_key("escape", |_| {
        root::ask(HostAction::Close(AuxWindow::Storage))
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
                    text(i18n::tr("disk.storage_title"))
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::color(palette::text)),
                )
                .child(
                    text(live.text(tab_line))
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(theme::color(palette::text_muted)),
                ),
        )
}
