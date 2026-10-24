//! Listening: which process is waiting for connections on which address
//! and port. Rendered on the Network tab, below process traffic and
//! above the interfaces.
//!
//! The list is `zstats::listeners()` — a one-shot function outside the
//! snapshot, read by the store when this tab comes on screen and every
//! 15s while it stays (`state::ensure_listeners`). Nothing here asks for
//! it; views only format.
//!
//! **Exposure is the headline, not the port.** `*:6379` and
//! `localhost:6379` are the same program on the same port, but only the
//! first can be reached from another machine. So a row is a process, its
//! endpoints read `*:port` (every interface), `localhost:port` (this Mac)
//! or the specific address, and the rows that reach beyond this Mac come
//! first and in the brighter ink. Not accent: accent means a threshold was
//! crossed, and listening on every interface is a fact, not a verdict.
//!
//! A row is a process *name*, not a pid: a development machine runs
//! twenty `redis-server`s and five container runtimes, and one row per
//! pid (the first version) filled the panel with the same word. The row
//! says how many processes it stands for; its tooltip says which pid holds
//! which endpoint.
//!
//! A search (the header's magnifier) matches a process name, a pid, a
//! port or an address — `redis`, `17302`, `6379`, `0.0.0.0`, `localhost`,
//! `udp` — over the whole answer, UDP included and uncapped: a query is
//! the reader naming the rows they want, so no preview stands in front of
//! them. A name match keeps the row whole; otherwise only the endpoints
//! that matched stay, so `6379` shows the one container that holds it
//! rather than all five with their other ports.
//!
//! The preview is TCP servers only. Unconnected UDP is mostly noise for
//! this question — measured here, 20 of 66 such sockets were mDNS on 5353,
//! one per app — so it arrives with "show more", merged into the same
//! process rows.

use super::widgets;
use crate::confirm;
use crate::font;
use crate::gpui_ui;
use crate::i18n;
use crate::listening::{self, Chip, Row};
use crate::state::{ListenerView, ZStatsAppState, ZStatsGlobalStore};
use crate::terminate;
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};
use gpui_kit::component::input::Input;
use gpui_kit::component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;
use zstats::snapshot::Capabilities;
use zstats::{Listeners, OwnerCoverage};

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    // No such view on this platform: no card, rather than an apology on
    // every visit.
    if !Capabilities::current().listeners {
        return Vec::new();
    }
    let body = match state.listeners() {
        None => vec![note_line(i18n::tr("listen.reading"))],
        Some(ListenerView::Restricted) => vec![note_line(i18n::tr("listen.restricted"))],
        Some(ListenerView::Failed(error)) => {
            vec![note_line(
                t!("listen.failed", error = error.clone()).to_string(),
            )]
        }
        Some(ListenerView::Ready(listeners)) => return vec![ready_card(state, listeners)],
    };
    vec![
        widgets::list_shell()
            .child(header(None, None))
            .children(body)
            .into_any_element(),
    ]
}

fn ready_card(state: &ZStatsAppState, listeners: &Listeners) -> AnyElement {
    let query = state.listen_filter_text();
    let card = listening::card(listeners, query, state.show_all_listeners());
    let shown = card.rows;
    let chip = card.chip.map(more_chip);

    let controls = h_flex()
        .items_center()
        .gap(px(5.))
        .child(search_chip(state))
        .children(chip)
        .into_any_element();
    let shell = widgets::list_shell()
        .child(header(Some(controls), Some(listeners.coverage)))
        .children(search_row(state));
    if shown.is_empty() {
        let text = if query.is_empty() {
            i18n::tr("listen.none")
        } else {
            t!("listen.no_match", query = query).to_string()
        };
        return shell.child(note_line(text)).into_any_element();
    }
    let last = shown.len() - 1;
    shell
        .children(
            shown
                .iter()
                .enumerate()
                .map(|(i, row)| row_element(i, row, i == last)),
        )
        .into_any_element()
}

/// The header control that opens the search — the process filter's
/// magnifier, so the two read as the same idiom.
fn search_chip(state: &ZStatsAppState) -> AnyElement {
    let on = state.listen_filter_open();
    div()
        .id("listen-filter")
        .flex_none()
        .rounded(px(4.))
        .p(px(2.))
        .when(on, |d| d.bg(theme::chip()))
        .when(!on, |d| d.hover(|d| d.bg(theme::surface_raised())))
        .tooltip(widgets::wrap_tooltip(i18n::tr("listen.filter_tip")))
        .child(
            Icon::new(IconName::Search)
                .with_size(Size::Size(px(11.)))
                .text_color(Hsla::from(if on {
                    theme::text()
                } else {
                    theme::text_muted()
                })),
        )
        .on_click(|_, window, cx| gpui_ui::toggle_listen_filter(window, cx))
        .into_any_element()
}

/// The search input under the header, while open.
fn search_row(state: &ZStatsAppState) -> Option<AnyElement> {
    if !state.listen_filter_open() {
        return None;
    }
    let input = gpui_ui::listen_filter_input()?;
    Some(
        div()
            .px(px(widgets::CARD_PAD))
            .pb(px(6.))
            .child(Input::new(&input).xsmall().cleanable(true))
            .into_any_element(),
    )
}

fn header(chip: Option<AnyElement>, coverage: Option<OwnerCoverage>) -> AnyElement {
    widgets::list_header(
        h_flex()
            .items_center()
            .gap(px(4.))
            .child(i18n::tr("listen.title"))
            .child(widgets::info_icon("listen-tip", listening::tip(coverage))),
        chip,
    )
}

fn note_line(text: String) -> AnyElement {
    div()
        .px(px(widgets::CARD_PAD))
        .pb(px(11.))
        .text_size(px(11.))
        .text_color(theme::text_dim())
        .child(text)
        .into_any_element()
}

/// Two lines: who (name, pid or `×N`, and how long it has been up), then
/// where (the endpoints, with Quit at the end where it can act). On one line the endpoints
/// shared the width with the name and both truncated.
fn row_element(index: usize, row: &Row, last: bool) -> AnyElement {
    let tip: Vec<SharedString> = row
        .endpoint_lines()
        .into_iter()
        .map(SharedString::from)
        .collect();
    // The row is the hover group its Quit turns red in.
    let group = SharedString::from(format!("listen-row-{index}"));
    let quit = listening::quit_target(row).map(|(pid, name)| {
        let body = row.quit_body(pid, &name);
        quit_button(index, &group, pid, name, body)
    });
    v_flex()
        .group(group)
        .px(px(widgets::CARD_PAD))
        .py(px(7.))
        .when(!last, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    h_flex()
                        .items_baseline()
                        .gap(px(6.))
                        .flex_1()
                        .min_w_0()
                        .child(widgets::truncating_name(
                            ("listen-owner", index),
                            row.owner_label(),
                            11.,
                            gpui::FontWeight::NORMAL,
                            theme::text().into(),
                        ))
                        .children(row.pid_label().map(|label| {
                            div()
                                .flex_none()
                                .text_size(px(10.))
                                .text_color(theme::text_dim())
                                .child(label)
                        })),
                )
                .children(row.age_label().map(|label| {
                    div()
                        .id(("listen-age", index))
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(theme::text_dim())
                        .tooltip(widgets::wrap_tooltip(row.age_tip()))
                        .child(label)
                })),
        )
        .child(
            // Quit sits at the end of the endpoints line: what the button
            // would silence, then the button. Every second line is held to
            // the button's height, so a row without one is not shorter
            // than its neighbours.
            h_flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .mt(px(3.))
                .min_h(px(QUIT_HEIGHT))
                .child(
                    font::mono_unless_cjk(div())
                        .id(("listen-endpoints", index))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(10.))
                        .text_color(if row.exposed() {
                            theme::text()
                        } else {
                            theme::text_dim()
                        })
                        .tooltip(widgets::wrap_tooltip_lines(tip))
                        .child(row.endpoints_line()),
                )
                .children(quit),
        )
        .into_any_element()
}

/// The Quit button's height, and so every row's second line — 16 keeps it
/// a control without making each row a line taller than its text.
const QUIT_HEIGHT: f32 = 16.;

/// The process page's Quit, sized for a row's first line. SIGTERM behind
/// the confirm sheet, whose `body` is `Row::quit_body`.
///
/// In the second ink at rest, and red only while its row is hovered —
/// the alarm's colour on the one row the pointer chose. Red at rest,
/// even receded to 40% (until 2026-10-16), was three or four red
/// outlines down a list of sockets: the loudest thing on the Network
/// tab, on a page with nothing wrong, beside `rapportd` and other
/// programs nobody opened the tab to stop.
fn quit_button(
    index: usize,
    group: &SharedString,
    pid: u32,
    name: String,
    body: String,
) -> AnyElement {
    div()
        .id(("listen-quit", index))
        .flex_none()
        .h(px(QUIT_HEIGHT))
        .px(px(7.))
        .rounded(px(5.))
        .border_1()
        .border_color(theme::border())
        .text_color(theme::text_muted())
        // On this element, not on the label inside it: a text's colour
        // is fixed when it is laid out, and only an element with an id
        // remembers at layout that its group is hovered — on the label
        // the outline turned red around a grey word.
        .group_hover(group.clone(), |d| {
            d.border_color(theme::accent_wash(45))
                .text_color(theme::accent_light())
        })
        .flex()
        .items_center()
        .justify_center()
        .hover(|d| d.bg(theme::accent_wash(10)))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let name = name.clone();
            confirm::ask(
                window,
                cx,
                i18n::tr("processes.kill_title"),
                body.clone(),
                i18n::tr("processes.kill_ok"),
                move |cx| {
                    // Checked against the name it was shown under: this
                    // list can be 15s old, and the pid may belong to
                    // something else by now.
                    if terminate::request_term_named(pid, &name) {
                        // Re-read on the next tick rather than 15s on, so
                        // the row goes once the process has.
                        cx.global::<ZStatsGlobalStore>()
                            .clone()
                            .update(cx, |state, _| state.expire_listeners());
                    }
                },
            );
        })
        .child(
            div()
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(i18n::tr("processes.kill_ok")),
        )
        .into_any_element()
}

/// The Network tab's chip idiom, for this card's own filter.
fn more_chip(chip: Chip) -> AnyElement {
    let showing = chip.showing;
    div()
        .id("listen-more")
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(if showing {
            theme::border()
        } else {
            theme::border_subtle()
        })
        .when(showing, |d| d.bg(theme::chip()))
        // Only while off: the "on" fill is the state, and a hover that
        // repainted it would read as the toggle having flipped.
        .when(!showing, |d| {
            d.hover(|d| d.bg(theme::surface_raised()).border_color(theme::border()))
        })
        .px(px(7.))
        .py(px(1.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(if showing {
            theme::text()
        } else {
            theme::text_dim()
        })
        .child(chip.label())
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.toggle_all_listeners(cx));
        })
        .into_any_element()
}
