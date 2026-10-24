//! The panel's root on NSH: the tab strip, the page of the tab on screen
//! in a scroll view, and the footer (`chrome.rs`) — `views::root` on the
//! gpui front end. The pages arrive one by one in phase 4 of
//! docs/nsh-migration.md; a tab whose page has not is a card that says so,
//! and Overview's place holds the building blocks on live figures
//! (`sample.rs`) until its own page does.
//!
//! The page is made again only when the tab changes (a memo of it), and
//! keeps its own bindings across ticks; the gpui front end repaints the
//! whole panel per tick.
//!
//! Its keys are the gpui panel's: `mod+1` … `mod+7` pick a tab and
//! `mod+p` pins the panel — `mod` is ⌘ on macOS and Ctrl elsewhere, the
//! gpui front end's `secondary-` for the same reason (Super is the
//! compositor's on a Hyprland desktop).

use crate::nsh::root::{self, CARD_GAP, GUTTER, HostAction, Live};
use crate::nsh::{chrome, sample, store, theme, widgets};
use crate::palette;
use crate::prefs;
use crate::state::{Tab, ZStatsAppState};
use nsh_core::ui::prelude::*;

#[component]
pub fn Panel() -> impl IntoElement {
    let live = Live::follow(ZStatsAppState::panel_visible);
    // The appearance this engine resolved, for the palette outside it: at
    // the first frame, and again whenever the system's turns.
    effect(|| root::ask(HostAction::Appearance(dark())));
    for (i, tab) in Tab::ALL.into_iter().enumerate() {
        on_key(&format!("mod+{}", i + 1), move |_| {
            store::update(|state, fx| state.set_tab(tab, fx));
        });
    }
    on_key("mod+p", |_| {
        prefs::set_pinned(!prefs::pinned());
        store::changed();
    });
    let tab = live.memo(ZStatsAppState::tab);
    view()
        .size_full()
        .bg(theme::color(palette::panel_ground))
        .child(chrome::tab_strip(live, tab))
        .child(
            scroll().flex_1().child(
                view()
                    .px(px(GUTTER))
                    .pb(px(CARD_GAP))
                    .child(dynamic(move || page(tab.get(), live))),
            ),
        )
        .child(chrome::footer(live))
}

/// The page of `tab`.
fn page(tab: Tab, live: Live) -> AnyElement {
    match tab {
        Tab::Overview => sample::page(live),
        other => not_yet(other),
    }
}

/// A tab whose page has not reached this front end yet. Not translated —
/// a development build's card, gone with the last page.
fn not_yet(tab: Tab) -> AnyElement {
    widgets::card()
        .gap(px(6.))
        .child(widgets::card_header(tab.title(), None))
        .child(
            text("Not on the NSH front end yet — phase 4 of docs/nsh-migration.md.")
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(theme::color(palette::text_muted)),
        )
        .into_element()
}
