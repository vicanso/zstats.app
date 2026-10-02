//! In-window confirm sheet.
//!
//! Destructive actions (eject, kill) open this before they run. Built on
//! gpui-kit's `AlertDialog`, sized to the 320px panel — the stock
//! title / `text_sm` body / default buttons read as a full-size window
//! dialog and swamp the popover.

use crate::font;
use crate::format;
use crate::i18n;
use crate::theme;
use gpui::prelude::*;
use gpui::{App, FontWeight, Window, div, px, relative};
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::{Disableable, Sizable, h_flex, v_flex};
use std::cell::RefCell;
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
        // No backdrop dismissal to ask for: gpui-kit deprecated
        // `AlertDialog::overlay_closable` into a no-op ("disabled by
        // design"), so the call only produced a warning. Esc still
        // closes (`keyboard` defaults true) and Cancel is always on the
        // sheet, so a destructive prompt never traps anyone — and for
        // this particular sheet, needing an explicit answer rather than
        // a stray click outside it is the better default anyway.
        alert
            .width(px(252.))
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

/// One row of [`ask_pick`].
pub struct PickItem {
    pub label: String,
    pub bytes: u64,
    /// A caution printed under the row — an app that is writing into it
    /// right now. Such a row starts unticked: the reader opts it in.
    pub caution: Option<String>,
}

/// A bulk action the reader can trim before it runs: every row it would
/// touch, each with its size and a tick. The one-line "N items, ~X GB"
/// sheet asked for trust in a number; this shows what the number is.
pub struct PickSheet {
    pub title: String,
    pub body: String,
    pub items: Vec<PickItem>,
    /// The confirm button's label for `n` ticked rows totalling `bytes`,
    /// so the button always says what it will do.
    pub ok: fn(usize, u64) -> String,
}

/// Rows this tall before the list scrolls — about eight, which keeps the
/// sheet inside the 620px disk-space window with its title and buttons.
const PICK_LIST_MAX_H: f32 = 240.;

/// Ask with a tickable list; run `on_ok` with the indices left ticked.
/// Wider than [`ask`]'s 252px: it opens in the disk-space window, and a
/// path needs the room.
pub fn ask_pick(
    window: &mut Window,
    cx: &mut App,
    sheet: PickSheet,
    on_ok: impl Fn(&[usize], &mut App) + 'static,
) {
    let picked: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(
        sheet
            .items
            .iter()
            .map(|item| item.caution.is_none())
            .collect(),
    ));
    let sheet = Rc::new(sheet);
    let on_ok = Rc::new(on_ok);
    let cancel = i18n::tr("common.cancel");
    window.open_alert_dialog(cx, move |alert, _, _| {
        // Rebuilt on every render, so a tick shows at once.
        let ticks = picked.borrow().clone();
        let chosen: Vec<usize> = (0..ticks.len()).filter(|&i| ticks[i]).collect();
        let total: u64 = chosen.iter().map(|&i| sheet.items[i].bytes).sum();
        let rows = sheet.items.iter().enumerate().map(|(i, item)| {
            let picked = picked.clone();
            v_flex()
                .py(px(3.))
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            Checkbox::new(("pick", i))
                                .xsmall()
                                .checked(ticks[i])
                                .on_click(move |on, window, _| {
                                    picked.borrow_mut()[i] = *on;
                                    window.refresh();
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(11.))
                                .text_color(theme::text())
                                .child(item.label.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family(font::MONO)
                                .text_size(px(10.5))
                                .text_color(theme::text_muted())
                                .child(format::memory(item.bytes)),
                        ),
                )
                .children(item.caution.clone().map(|caution| {
                    div()
                        .pl(px(20.))
                        .text_size(px(10.))
                        .text_color(theme::tiny_label(theme::text_muted()))
                        .child(caution)
                }))
        });
        let ok = (sheet.ok)(chosen.len(), total);
        let on_ok = on_ok.clone();
        let cancel = cancel.clone();
        alert
            .width(px(360.))
            .title(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(sheet.title.clone()),
            )
            .description(
                v_flex()
                    .gap(px(8.))
                    .mt(px(2.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .line_height(relative(1.35))
                            .text_color(theme::text_muted())
                            .child(sheet.body.clone()),
                    )
                    .child(
                        div()
                            .id("pick-list")
                            .max_h(px(PICK_LIST_MAX_H))
                            .overflow_y_scroll()
                            .children(rows),
                    ),
            )
            .footer(
                DialogFooter::new().child(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap(px(6.))
                        .child(Button::new("pick-cancel").xsmall().label(cancel).on_click(
                            |_, window, cx| {
                                window.close_dialog(cx);
                            },
                        ))
                        .child(
                            Button::new("pick-ok")
                                .xsmall()
                                .danger()
                                .label(ok)
                                .disabled(chosen.is_empty())
                                .on_click(move |_, window, cx| {
                                    on_ok(&chosen, cx);
                                    window.close_dialog(cx);
                                }),
                        ),
                ),
            )
    });
}
