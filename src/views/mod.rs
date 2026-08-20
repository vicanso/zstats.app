//! The popover's chrome: icon tab strip, scrolling content, footer.
//!
//! Views read the store directly and return elements, rather than being
//! gpui views of their own — there is one root view and the whole panel
//! repaints when a tick lands, which every 2–5s over 320px is not worth
//! splitting up.
//!
//! **Affordance rule.** macOS does not change the pointer over clickable
//! things: AppKit keeps the arrow over buttons, tabs and table rows alike,
//! and reserves `pointingHand` for links — WebKit uses it for hyperlinks and
//! NSTextView for `.link` runs, so on this platform the hand means "this
//! leaves where you are", not "this is clickable". Every clickable element
//! therefore needs a *visible* affordance: a hover fill, a border that lifts,
//! or real button chrome. Where the target is a whole card rather than
//! something button-shaped, hover is not enough and it gets an explicit
//! control instead — that is why the Alerts card carries a gear button rather
//! than being clickable along its title.
//!
//! `cursor_pointer` is consequently used exactly once, on the footer's GitHub
//! button — the only control here that opens something outside the app. Used
//! that sparingly it carries the platform's own meaning instead of reading as
//! a web page in a window.

mod alerts;
mod apps;
// Public: rendered by the settings window (main.rs), not by a tab.
pub mod config;
mod disk;
mod history;
mod net;
mod overview;
mod processes;
mod sensors;
pub mod storage;

pub mod widgets;

use crate::assets::CustomIconName;
use crate::i18n;
use crate::state::{Tab, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Hsla, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;

/// Height of the tab strip: 14 top pad + a 34-tall well (3 + 28 + 3) + 8 gap.
///
/// Written down, and applied as an explicit height below, because a view
/// that has to size something against the viewport has no other way to know
/// how much room the body was left — `uniform_list` cannot measure itself,
/// it needs a bound to virtualise against.
const TAB_STRIP_HEIGHT: f32 = 56.;

/// Height of the footer: 1px top border + 4 + a 22-tall icon row + 6.
const FOOTER_HEIGHT: f32 = 33.;

/// What the body keeps clear beneath its content — for every tab.
///
/// Removing it entirely was tried and was wrong: the tabs whose last
/// card paints rows to its own edge (Processes, Apps, History) then sat
/// flush against the footer's hairline. This gutter is what those pages
/// have, so it stays here.
///
/// What must not happen is a card adding its own on top: the stack that
/// looked slack at the bottom was Overview, where the memory card's 13
/// of inner padding landed under a footnote line that needed none. The
/// fix belongs to that card (`overview::memory`), not to this number —
/// trimming rows elsewhere to pay for a doubled gap, which is what the
/// previous attempt did, spends content on a spacing mistake.
///
const BODY_BOTTOM_PAD: f32 = 12.;

/// Space above the first card. The tab strip already ends with 8px, so
/// this 6 brings tab-well → card to 14 — the same as the side gutter —
/// without pulling the cards themselves 14px apart (that would ungroup
/// the stack). Lists that size against [`body_height`] subtract it too.
const CONTENT_TOP_PAD: f32 = 6.;

/// Space between cards. Less than the 14px frame so the stack still
/// reads as one group; more than the old 8px so it does not look
/// pinched against the sides.
const CARD_GAP: f32 = 12.;

/// Room the scrolling body actually has, or `None` until the first frame
/// has reported a window size.
pub(super) fn body_height(state: &ZStatsAppState) -> Option<f32> {
    state.window_bounds().map(|bounds| {
        f32::from(bounds.size.height)
            - TAB_STRIP_HEIGHT
            - FOOTER_HEIGHT
            - BODY_BOTTOM_PAD
            - CONTENT_TOP_PAD
    })
}

/// Build the whole panel.
pub fn root(cx: &App) -> AnyElement {
    let state = cx.global::<ZStatsGlobalStore>().read(cx);
    v_flex()
        .size_full()
        .child(tab_strip(state))
        .child(content(state))
        .child(footer(state))
        .into_any_element()
}

fn tab_icon(tab: Tab) -> Icon {
    match tab {
        Tab::Overview => Icon::new(IconName::LayoutDashboard),
        Tab::Apps => CustomIconName::AppWindow.into(),
        Tab::Processes => Icon::new(IconName::Cpu),
        Tab::Hardware => Icon::new(IconName::HardDrive),
        Tab::Net => Icon::new(IconName::Network),
        Tab::Alerts => Icon::new(IconName::Bell),
        Tab::History => CustomIconName::History.into(),
    }
}

/// Every view in a single icon row — Control Center / Stats, not a text grid.
/// Full names live on the tooltip.
fn tab_strip(state: &ZStatsAppState) -> AnyElement {
    let active = state.tab();
    let alert_count = state.alerts().len();

    let cell = |tab: Tab| {
        let on = tab == active;
        let alerting = tab == Tab::Alerts && alert_count > 0;
        let color = if alerting {
            theme::accent_light()
        } else if on {
            theme::text()
        } else {
            theme::text_dim()
        };
        let id = tab.label();
        let title = tab.title();
        div()
            .id(id)
            .flex_1()
            .h(px(28.))
            .rounded(px(7.))
            .when(on, |d| d.bg(theme::chip()))
            .hover(|d| d.bg(theme::chip()))
            .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
            .child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        tab_icon(tab)
                            .with_size(Size::Size(px(15.)))
                            .text_color(Hsla::from(color)),
                    ),
            )
            .on_click(move |_, _window, cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.set_tab(tab, cx));
            })
    };

    v_flex()
        .h(px(TAB_STRIP_HEIGHT))
        .flex_none()
        .px(px(14.))
        .pt(px(14.))
        .pb(px(8.))
        .child(
            h_flex()
                .gap(px(2.))
                .p(px(3.))
                .rounded(px(10.))
                .bg(theme::trough())
                .children(Tab::ALL.iter().copied().map(cell)),
        )
        .into_any_element()
}

/// The scrolling body. Each tab renders a stack of cards.
fn content(state: &ZStatsAppState) -> AnyElement {
    let body = match state.tab() {
        Tab::Overview => overview::render(state),
        Tab::Apps => apps::render(state),
        Tab::Processes => processes::render(state),
        // One tab for the machine's physical substrate: volumes, then
        // temperatures, then the battery. Both renderers return card
        // stacks, so merging is concatenation — no layout marriage.
        Tab::Hardware => {
            let mut cards = disk::render(state);
            cards.extend(sensors::render(state));
            cards
        }
        Tab::Net => net::render(state),
        Tab::Alerts => alerts::render(state),
        Tab::History => history::render(state),
    };

    let tab = state.tab();
    div()
        // Keyed per tab, or one shared id would carry the previous tab's
        // offset over to the next. The id alone does not remember anything
        // though — gpui drops element state it did not paint, and only the
        // active tab is ever painted — so the handle comes from the store.
        .id(("panel-body", tab.index()))
        .track_scroll(state.scroll_handle(tab))
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(14.))
        .pt(px(CONTENT_TOP_PAD))
        .pb(px(BODY_BOTTOM_PAD))
        .child(v_flex().gap(px(CARD_GAP)).children(body))
        .into_any_element()
}

const REPO_URL: &str = "https://github.com/vicanso/zstats.app";

/// Config, GitHub and Quit sit together on the right — a lone icon on
/// the left read as an unfinished row. Quit stays last so it is the
/// edge action.
fn footer(state: &ZStatsAppState) -> AnyElement {
    let github_tip = i18n::tr("common.github");
    let nudge = state.update_nudge().map(str::to_string);
    h_flex()
        .h(px(FOOTER_HEIGHT))
        .flex_none()
        .items_center()
        .justify_end()
        .gap(px(6.))
        .px(px(14.))
        .pt(px(4.))
        .pb(px(6.))
        .border_t(px(1.))
        .border_color(theme::border_subtle())
        .child({
            // Config lives in its own window, not a tab: a settings
            // session should not be cut short by the popover auto-hiding
            // on focus loss. The gear opens (or refocuses) it.
            let settings_tip = match &nudge {
                // The dot's words: a silent check found a newer release;
                // the About page has the download.
                Some(v) => t!("config.update_nudge_tip", v = v).to_string(),
                None => i18n::tr("tabs.config"),
            };
            div()
                .id("settings")
                .flex_none()
                .p(px(4.))
                .rounded(px(6.))
                .tooltip(move |window, cx| Tooltip::new(settings_tip.clone()).build(window, cx))
                .hover(|d| d.bg(theme::surface_raised()))
                .child(
                    div()
                        .relative()
                        .child(
                            Icon::new(IconName::Settings2)
                                .with_size(Size::Size(px(14.)))
                                .text_color(Hsla::from(theme::text_dim())),
                        )
                        // The nudge dot: paint only, no layout — the
                        // gear must not shift when it appears.
                        .when(nudge.is_some(), |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .top(px(-1.))
                                    .right(px(-1.))
                                    .w(px(6.))
                                    .h(px(6.))
                                    .rounded_full()
                                    .bg(Hsla::from(theme::accent())),
                            )
                        }),
                )
                .on_click(|_, _window, cx| crate::open_settings_window(cx))
        })
        .child(
            div()
                .id("github")
                .cursor_pointer()
                .flex_none()
                .p(px(4.))
                .rounded(px(6.))
                .text_color(theme::text_muted())
                .tooltip(move |window, cx| Tooltip::new(github_tip.clone()).build(window, cx))
                .hover(|d| d.bg(theme::surface_raised()).text_color(theme::text()))
                .child(
                    Icon::new(IconName::Github)
                        .with_size(Size::Size(px(14.)))
                        .text_color(Hsla::from(theme::text_dim())),
                )
                .on_click(|_, _window, cx| {
                    cx.open_url(REPO_URL);
                }),
        )
        .child({
            let quit_tip = i18n::tr("common.quit");
            div()
                .id("quit")
                .flex_none()
                .p(px(4.))
                .rounded(px(6.))
                .tooltip(move |window, cx| Tooltip::new(quit_tip.clone()).build(window, cx))
                .hover(|d| d.bg(theme::surface_raised()))
                .child(
                    Icon::from(CustomIconName::Power)
                        .with_size(Size::Size(px(14.)))
                        .text_color(Hsla::from(theme::text_dim())),
                )
                .on_click(|_, _window, cx| cx.quit())
        })
        .into_any_element()
}
