//! The popover's chrome: icon tab strip, scrolling content, footer.
//!
//! Views read the store directly and return elements, rather than being
//! gpui views of their own — there is one root view and the whole panel
//! repaints when a tick lands, which at 1 Hz over 320px is not worth
//! splitting up.

mod alerts;
mod apps;
mod config;
mod disk;
mod net;
mod overview;
mod processes;
mod sensors;

pub mod widgets;

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

/// Build the whole panel.
pub fn root(cx: &App) -> AnyElement {
    let state = cx.global::<ZStatsGlobalStore>().read(cx);
    v_flex()
        .size_full()
        .child(tab_strip(state))
        .child(content(state))
        .child(footer())
        .into_any_element()
}

fn tab_icon(tab: Tab) -> IconName {
    match tab {
        Tab::Overview => IconName::LayoutDashboard,
        Tab::Processes => IconName::SquareTerminal,
        Tab::Apps => IconName::Frame,
        Tab::Disk => IconName::HardDrive,
        Tab::Net => IconName::Globe,
        Tab::Sensors => IconName::Sun,
        Tab::Alerts => IconName::Bell,
        Tab::Config => IconName::Settings2,
    }
}

/// Eight views in a single icon row — Control Center / Stats, not a 4×2
/// text grid. Full names live on the tooltip.
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
            .cursor_pointer()
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
                        Icon::new(tab_icon(tab))
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
        Tab::Processes => processes::render(state),
        Tab::Apps => apps::render(state),
        Tab::Disk => disk::render(state),
        Tab::Net => net::render(state),
        Tab::Sensors => sensors::render(state),
        Tab::Alerts => alerts::render(state),
        Tab::Config => config::render(state),
    };

    div()
        // Keyed per tab: gpui stores scroll offset against the element id, so
        // one shared id would carry the previous tab's position over to the
        // next. Distinct ids also mean returning to a tab restores where it
        // was left, rather than jumping to the top.
        .id(("panel-body", state.tab().index()))
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(14.))
        .pb(px(6.))
        .child(v_flex().gap(px(8.)).children(body))
        .into_any_element()
}

const REPO_URL: &str = "https://github.com/vicanso/zstats.app";

/// GitHub and Quit sit together on the right — a lone icon on the left
/// read as an unfinished row.
fn footer() -> AnyElement {
    let github_tip = i18n::tr("common.github");
    h_flex()
        .items_center()
        .justify_end()
        .gap(px(6.))
        .px(px(14.))
        .pt(px(4.))
        .pb(px(6.))
        .border_t(px(1.))
        .border_color(theme::border_subtle())
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
                .cursor_pointer()
                .flex_none()
                .p(px(4.))
                .rounded(px(6.))
                .tooltip(move |window, cx| Tooltip::new(quit_tip.clone()).build(window, cx))
                .hover(|d| d.bg(theme::surface_raised()))
                .child(
                    Icon::empty()
                        .path("icons/power.svg")
                        .with_size(Size::Size(px(14.)))
                        .text_color(Hsla::from(theme::text_dim())),
                )
                .on_click(|_, _window, cx| cx.quit())
        })
        .into_any_element()
}
