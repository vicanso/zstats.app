//! History: what actually burned the CPU today.
//!
//! The only view here ranked by an *amount* rather than a rate. Everything
//! else answers "how busy is it right now", which structurally cannot see the
//! process that never looks busy and yet costs the most over a day — see
//! [`crate::history`].

use super::widgets::{self, card};
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, div, px,
};
use gpui_component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;

/// How many rows to name. Beyond this the tail is all daemons doing their job.
const TOP_N: usize = 12;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(rows) = state.history() else {
        // The read is on the background executor; this is the frame or two
        // before it lands.
        return vec![widgets::empty_card(
            i18n::tr("history.title"),
            i18n::tr("history.loading"),
        )];
    };
    if rows.is_empty() {
        return vec![widgets::empty_card(
            i18n::tr("history.empty_title"),
            i18n::tr("history.empty_body"),
        )];
    }

    // The bar is relative to the day's biggest spender — an absolute scale
    // would be meaningless (a day has 86 400 core-seconds per core).
    let top = rows.first().map_or(1, |s| s.cpu_time_ms).max(1);
    let shown: Vec<_> = rows.iter().take(TOP_N).collect();
    let last = shown.len().saturating_sub(1);

    let list = widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("history.title"),
            Some(refresh_control()),
        ))
        .children(shown.into_iter().enumerate().map(|(i, s)| {
            v_flex()
                .px(px(13.))
                .py(px(9.))
                .when(i != last, |d| {
                    d.border_b(px(1.)).border_color(theme::border_subtle())
                })
                .child(
                    h_flex()
                        .items_baseline()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::text())
                                .truncate()
                                .child(s.name.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family(font::MONO)
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(theme::text())
                                .child(format::core_time(s.cpu_time_ms)),
                        ),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .gap(px(8.))
                        .mt(px(2.))
                        .text_size(px(10.))
                        .text_color(theme::text_dim())
                        .child(
                            div().child(
                                t!(
                                    "history.pid_mem",
                                    pid = s.pid,
                                    mem = format::memory(s.peak_memory_bytes)
                                )
                                .to_string(),
                            ),
                        )
                        // Peak beside total on purpose: a small peak next to a
                        // large total is exactly the process this view exists
                        // to find, and the pairing is what makes that legible.
                        .child(
                            div().font_family(font::MONO).child(
                                t!(
                                    "history.peak",
                                    cpu = format::pct(s.peak_cpu_percent),
                                    minutes = s.minutes
                                )
                                .to_string(),
                            ),
                        ),
                )
                .child(div().mt(px(6.)).child(widgets::meter(
                    s.cpu_time_ms as f32 / top as f32,
                    Hsla::from(theme::ink()),
                    4.,
                )))
                .into_any_element()
        }));

    let mut out = vec![list.into_any_element()];
    if rows.len() > TOP_N {
        out.push(widgets::note(
            t!("history.more", count = rows.len() - TOP_N).to_string(),
        ));
    }
    out.push(explainer());
    out
}

/// Why this list disagrees with every other tab, said once where it is read.
fn explainer() -> AnyElement {
    card()
        .child(widgets::card_header(i18n::tr("history.about"), None))
        .child(
            div()
                .mt(px(4.))
                .text_size(px(10.5))
                .line_height(gpui::relative(1.4))
                .text_color(theme::text_muted())
                .child(i18n::tr("history.about_body")),
        )
        .into_any_element()
}

fn refresh_control() -> AnyElement {
    h_flex()
        .id("history-refresh")
        .items_center()
        .gap(px(3.))
        .rounded(px(4.))
        .px(px(5.))
        .py(px(1.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .hover(|d| d.bg(theme::surface_raised()).text_color(theme::text()))
        .child(
            Icon::new(IconName::Redo)
                .with_size(Size::Size(px(10.)))
                .text_color(Hsla::from(theme::text_dim())),
        )
        .child(i18n::tr("history.refresh"))
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.load_history(cx));
        })
        .into_any_element()
}
