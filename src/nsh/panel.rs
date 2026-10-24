//! The panel's root on NSH: the tab strip, the page of the tab on screen
//! in a scroll view, and the footer (`chrome.rs`) — `views::root` on the
//! gpui front end; the pages arrived one by one in phase 4 of
//! docs/nsh-migration.md. A debug build run with `ZSTATS_GALLERY` set shows the building blocks
//! on live figures (`gallery.rs`) in Overview's place.
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
use crate::nsh::{
    alerts, apps, chrome, gallery, hardware, history, network, overview, processes, store, theme,
};
use crate::palette;
use crate::prefs;
use crate::state::{Tab, ZStatsAppState};
use nsh_core::ui::prelude::*;
use std::env;

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
    root::bind_window_keys(None);
    let tab = live.memo(ZStatsAppState::tab);
    // The page's height on screen, for the lists that end on a whole row
    // at the gap above the footer.
    let body = signal(0.0_f32);
    view()
        .size_full()
        .bg(theme::color(palette::panel_ground))
        .child(chrome::tab_strip(live, tab))
        .child(
            scroll()
                .flex_1()
                .on_layout(move |b: &Bounds<Points>| body.set(b.size.height.0))
                .child(
                    view()
                        .px(px(GUTTER))
                        .pb(px(CARD_GAP))
                        .child(dynamic(move || page(tab.get(), live, body.read_only()))),
                ),
        )
        .child(chrome::footer(live))
}

/// The page of `tab`; `body` is the height it has on screen.
fn page(tab: Tab, live: Live, body: ReadSignal<f32>) -> AnyElement {
    match tab {
        Tab::Overview if gallery_asked() => gallery::page(live),
        Tab::Overview => overview::page(live),
        Tab::Apps => apps::page(live, body),
        Tab::Processes => processes::page(live, body),
        Tab::Hardware => hardware::page(live),
        Tab::Net => network::page(live),
        Tab::Alerts => alerts::page(live),
        Tab::History => history::page(live, body),
    }
}

/// The building blocks instead of Overview: a debug build run with
/// `ZSTATS_GALLERY` set.
fn gallery_asked() -> bool {
    cfg!(debug_assertions) && env::var_os("ZSTATS_GALLERY").is_some()
}
