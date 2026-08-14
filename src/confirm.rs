//! In-window confirm sheet.
//!
//! Destructive actions (eject, kill) open this before they run. Built on
//! gpui-component's `AlertDialog`, sized to the 320px panel — the stock
//! title / `text_sm` body / default buttons read as a full-size window
//! dialog and swamp the popover.

use crate::i18n;
use crate::theme;
use gpui::prelude::*;
use gpui::{App, FontWeight, Window, div, px, relative};
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::dialog::DialogFooter;
use gpui_component::{Sizable, h_flex};
use std::rc::Rc;

/// Ask, then run `on_ok` only if the user confirms.
pub fn ask(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<String>,
    body: impl Into<String>,
    ok: impl Into<String>,
    on_ok: impl Fn(&mut App) + 'static,
) {
    let title = title.into();
    let body = body.into();
    let ok = ok.into();
    let cancel = i18n::tr("common.cancel");
    let on_ok = Rc::new(on_ok);
    window.open_alert_dialog(cx, move |alert, _, _| {
        let on_ok = on_ok.clone();
        let ok = ok.clone();
        let cancel = cancel.clone();
        alert
            .width(px(252.))
            .overlay_closable(true)
            .title(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(title.clone()),
            )
            .description(
                div()
                    .mt(px(2.))
                    .text_size(px(11.))
                    .line_height(relative(1.35))
                    .text_color(theme::text_muted())
                    .child(body.clone()),
            )
            .footer(
                DialogFooter::new().child(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap(px(6.))
                        .child(
                            Button::new("confirm-cancel")
                                .xsmall()
                                .label(cancel)
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("confirm-ok")
                                .xsmall()
                                .danger()
                                .label(ok)
                                .on_click(move |_, window, cx| {
                                    on_ok(cx);
                                    window.close_dialog(cx);
                                }),
                        ),
                ),
            )
    });
}
