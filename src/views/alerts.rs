//! Alerts: conditions that crossed a threshold, newest first.
//!
//! `Tick::alerts` reports the *moment* a threshold is crossed, not a standing
//! list, so what is shown here is accumulated by the store (see
//! `state::SeenAlert`) along with when this process saw it.

use super::widgets::{self, card};
use crate::format;
use crate::i18n;
use rust_i18n::t;
use crate::state::ZStatsAppState;
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{AnyElement, Hsla, IntoElement, ParentElement, Styled, div, px};
use gpui_component::h_flex;
use zstats::{AlertSubject, Severity};

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    if state.alerts().is_empty() {
        return vec![widgets::empty_card(
            i18n::tr("alerts.empty_title"),
            i18n::tr("alerts.empty_body"),
        )];
    }

    let mut cards: Vec<AnyElement> = state
        .alerts()
        .iter()
        .map(|seen| {
            let critical = seen.event.severity() == Severity::Critical;
            let line = if critical {
                Hsla::from(theme::accent_wash(45))
            } else {
                Hsla::from(theme::border())
            };

            card()
                .border_color(line)
                .when(critical, |d| d.bg(theme::accent_wash(7)))
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_none()
                                .rounded_full()
                                .border_1()
                                .border_color(line)
                                .bg(if critical {
                                    Hsla::from(theme::accent())
                                } else {
                                    Hsla::from(theme::surface())
                                })
                                .px(px(8.))
                                .py(px(1.5))
                                .text_size(px(9.5))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(if critical {
                                    gpui::white()
                                } else {
                                    Hsla::from(theme::text_muted())
                                })
                                .child(if critical {
                                    i18n::tr("alerts.critical")
                                } else {
                                    i18n::tr("alerts.warning")
                                }),
                        )
                        .child(widgets::note(format::ago(seen.age()))),
                )
                .child(
                    div()
                        .mt(px(8.))
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .truncate()
                        .child(subject_label(&seen.event.subject)),
                )
                .child(
                    div()
                        .mt(px(3.))
                        .text_size(px(9.5))
                        .text_color(theme::text_muted())
                        // zstats renders the sentence from the same fields it
                        // evaluated, so no numbers are re-derived here.
                        .child(seen.event.summary()),
                )
                .when_some(seen.event.repeat_after, |d, after| {
                    d.child(div().mt(px(8.)).child(widgets::note(
                        t!(
                            "alerts.follow_up",
                            when = format::uptime(after.as_secs())
                        )
                        .to_string(),
                    )))
                })
                .into_any_element()
        })
        .collect();

    cards.push(widgets::note(i18n::tr("alerts.footer_note")));
    cards
}

/// Who the alert is about, in the design's compact form.
fn subject_label(subject: &AlertSubject) -> String {
    match subject {
        AlertSubject::Process { pid, name } => {
            t!("alerts.process", name = name.clone(), pid = *pid).to_string()
        }
        AlertSubject::App {
            name,
            process_count,
            ..
        } => t!("alerts.app", name = name.clone(), count = *process_count).to_string(),
        AlertSubject::Volume { mount_point } => {
            t!("alerts.volume", mount = mount_point.clone()).to_string()
        }
        AlertSubject::System => i18n::tr("alerts.system"),
    }
}
