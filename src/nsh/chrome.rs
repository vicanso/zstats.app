//! The panel's frame on NSH: the tab strip above the page and the footer
//! under it — `views::tab_strip` and `views::footer` on the gpui front end,
//! whose comments say why each piece is where it is and looks as it does;
//! the sizes, inks and words are theirs.
//!
//! NSH makes a component once and binds what changes, so nothing here is
//! rebuilt on a tick: a selected tab, a lit pin, the cup's time left are
//! bindings over the store's memos (`Live::memo`, `Live::watch`). That is
//! also what keeps the keep-awake menu open across a tick — a footer made
//! again would have closed it, as a gpui dropdown in a hidden panel stayed
//! open (`panel_visit`).

use crate::assets::CustomIconName;
use crate::color::Rgba;
use crate::nsh::root::{self, AuxWindow, CARD_GAP, GUTTER, HostAction, Live};
use crate::nsh::widgets::{self, CLEAR};
use crate::nsh::{store, theme};
use crate::state::{Tab, ZStatsAppState};
use crate::{awake, format, i18n, palette, prefs};
use nsh_base::{TooltipElement, button, menu, menu_item, menu_separator};
use nsh_core::ui::prelude::*;
use rust_i18n::t;
use std::time::SystemTime;

/// `views::TAB_ICON_ON` and `TAB_FACE_GAP`: the selected tab's icon, and
/// the space between it and the name.
const TAB_ICON_ON: f32 = 15.;
const TAB_FACE_GAP: f32 = 6.;

/// A tab cell's height, and a footer button's (`views::FOOTER_BUTTON`).
const CELL: f32 = 30.;

/// `views::FOOTER_HEIGHT`.
const FOOTER_HEIGHT: f32 = 44.;

fn tab_icon(tab: Tab) -> CustomIconName {
    match tab {
        Tab::Overview => CustomIconName::LayoutDashboard,
        Tab::Apps => CustomIconName::AppWindow,
        Tab::Processes => CustomIconName::Cpu,
        Tab::Hardware => CustomIconName::HardDrive,
        Tab::Net => CustomIconName::Network,
        Tab::Alerts => CustomIconName::Bell,
        Tab::History => CustomIconName::History,
    }
}

/// How the tab shortcuts are spelled here: `mod` is ⌘ on macOS and Ctrl
/// elsewhere (`views::shortcut_hint`).
fn shortcut_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl+"
    }
}

/// A tab cell's fill and ink (`views::tab_strip`): the theme's selected
/// look on the tab on screen, and red on Alerts while an alert of this
/// session stands — that colour is the alarm, and wins over the theme.
fn cell_paint(on: bool, alarm: bool) -> (Rgba, Rgba) {
    match (on, alarm) {
        (true, true) => (palette::accent_wash(16), palette::accent_light()),
        (false, true) => (CLEAR, palette::accent_light()),
        (true, false) => match (palette::selected(), palette::mark()) {
            (Some((fill, ink)), _) | (None, Some((fill, ink))) => (fill, ink),
            (None, None) => (palette::chip(), palette::text()),
        },
        (false, false) => (CLEAR, palette::text_dim()),
    }
}

/// A tab's name as the selected cell wears it.
fn tab_name(tab: Tab) -> Text {
    text(tab.title())
        .flex_none()
        .text_size(px(13.))
        .font_weight(FontWeight::SEMIBOLD)
}

/// As wide as `tab`'s selected face, no height, never painted: the
/// selected cell holds one for every tab and so is as wide as the widest
/// face in this language and this font (`views::tab_strut`).
fn tab_strut(tab: Tab) -> View {
    view()
        .flex_row()
        .flex_none()
        .h(px(0.))
        .opacity(0.)
        .gap(px(TAB_FACE_GAP))
        .child(view().flex_none().w(px(TAB_ICON_ON)))
        .child(tab_name(tab))
}

/// Every view in one row of icons, the one on screen wearing its name.
pub fn tab_strip(live: Live, active: Memo<Tab>) -> View {
    let alerting = live.memo(|state: &ZStatsAppState| state.alerts().iter().any(|seen| seen.live));
    let cells: Vec<AnyElement> = Tab::ALL
        .into_iter()
        .map(|tab| tab_cell(tab, active, alerting))
        .collect();
    view()
        .flex_none()
        .px(px(GUTTER))
        .pt(px(GUTTER))
        .pb(px(CARD_GAP))
        .child(
            view()
                .flex_row()
                .justify_between()
                .p(px(4.))
                .rounded(px(10.))
                .bg(theme::color(palette::trough))
                .children(cells),
        )
}

fn tab_cell(tab: Tab, active: Memo<Tab>, alerting: Memo<bool>) -> AnyElement {
    let alarm = move || tab == Tab::Alerts && alerting.get();
    let ink = move || cell_paint(active.get() == tab, alarm()).1;
    let tip = t!(
        "tabs.shortcut",
        name = tab.title(),
        hint = shortcut_hint(),
        n = tab.index() + 1
    )
    .to_string();
    // The tooltip on a view around the button: `nsh_base`'s button element
    // takes none of its own.
    view()
        .flex_none()
        .tooltip(widgets::wrap_tooltip(tip))
        .child(
            button()
                .flex_none()
                .on_press(move || store::update(|state, fx| state.set_tab(tab, fx)))
                .child(
                    when(
                        move || active.get() == tab,
                        move || {
                            // A column, so the cell is as wide as the widest
                            // thing in it while its face stays centred.
                            view()
                                .h(px(CELL))
                                .px(px(12.))
                                .items_center()
                                .justify_center()
                                .rounded(px(7.))
                                .bg(move || theme::color(move || cell_paint(true, alarm()).0))
                                .child(
                                    view()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(TAB_FACE_GAP))
                                        .child(widgets::icon(tab_icon(tab), TAB_ICON_ON, ink))
                                        .child(tab_name(tab).text_color(move || theme::color(ink))),
                                )
                                .children(Tab::ALL.into_iter().map(tab_strut).collect::<Vec<_>>())
                        },
                    )
                    .otherwise(move || {
                        view()
                            .w(px(34.))
                            .h(px(CELL))
                            .items_center()
                            .justify_center()
                            .rounded(px(7.))
                            .hover(|s| s.bg(theme::color(palette::chip)))
                            .child(widgets::icon(tab_icon(tab), 16., ink))
                    }),
                ),
        )
        .into_element()
}

/// One of the footer's icon buttons: a 32×30 box, the icon at 16, lit (the
/// chip's fill, the full ink) while `lit` says so.
fn footer_button(
    icon: impl IntoElement,
    lit: impl Fn() -> bool + Copy + 'static,
    tip: TooltipElement,
    on_press: impl Fn() + 'static,
) -> AnyElement {
    view()
        .flex_none()
        .tooltip(tip)
        .child(
            button()
                .flex_none()
                .w(px(32.))
                .h(px(CELL))
                .items_center()
                .justify_center()
                .rounded(px(7.))
                .bg(move || theme::color(move || if lit() { palette::chip() } else { CLEAR }))
                .hover(|s| s.bg(theme::color(palette::surface_raised)))
                .on_press(on_press)
                .child(icon),
        )
        .into_element()
}

/// The ink of a footer glyph, lit or not.
fn footer_ink(lit: bool) -> Rgba {
    if lit {
        palette::text()
    } else {
        palette::text_dim()
    }
}

/// What the panel does to itself on the left — Pin, its form, Settings,
/// the keep-awake cup — and, alone on the right and in words, Quit.
pub fn footer(live: Live) -> View {
    let pinned = live.watch(prefs::pinned);
    let detail = live.watch(prefs::overview_detail);
    let nudge = live.memo(|state: &ZStatsAppState| state.update_nudge().map(str::to_owned));
    let template_nudge = live.memo(ZStatsAppState::template_nudge);
    let dotted = move || nudge.get().is_some() || template_nudge.get();

    let pin = footer_button(
        widgets::icon(CustomIconName::Pin, 16., move || footer_ink(pinned.get())),
        move || pinned.get(),
        widgets::live_tooltip(move || {
            i18n::tr(if pinned.get() {
                "common.unpin"
            } else {
                "common.pin"
            })
        }),
        move || {
            // Pin only stops auto-hide — a tray click still toggles.
            prefs::set_pinned(!prefs::pinned());
            store::changed();
        },
    );
    let form = footer_button(
        dynamic(move || {
            let glyph = if detail.get() {
                CustomIconName::Collapse
            } else {
                CustomIconName::Expand
            };
            widgets::icon(glyph, 16., palette::text_dim)
        }),
        || false,
        widgets::live_tooltip(move || {
            i18n::tr(if detail.get() {
                "overview.collapse_tip"
            } else {
                "overview.expand_tip"
            })
        }),
        move || {
            prefs::set_overview_detail(!prefs::overview_detail());
            store::changed();
            root::ask(HostAction::FitForm);
        },
    );
    let settings = footer_button(
        view()
            .w(px(32.))
            .h(px(CELL))
            .items_center()
            .justify_center()
            .child(widgets::icon(
                CustomIconName::Settings,
                16.,
                palette::text_dim,
            ))
            // One dot, two possible reasons; paint only, the gear does
            // not move when it comes.
            .child(when(dotted, || {
                view()
                    .absolute()
                    .top(px(6.))
                    .right(px(6.))
                    .w(px(6.))
                    .h(px(6.))
                    .rounded_full()
                    .bg(theme::color(palette::accent))
            })),
        || false,
        widgets::live_tooltip(move || match (nudge.get(), template_nudge.get()) {
            (Some(v), true) => t!("config.both_nudge_tip", v = v).to_string(),
            (Some(v), false) => t!("config.update_nudge_tip", v = v).to_string(),
            (None, true) => i18n::tr("config.template_nudge_tip"),
            (None, false) => i18n::tr("tabs.settings"),
        }),
        || root::ask(HostAction::Open(AuxWindow::Settings)),
    );
    view()
        .flex_row()
        .flex_none()
        .h(px(FOOTER_HEIGHT))
        .items_center()
        .justify_between()
        .px(px(8.))
        .border_t(px(1.))
        .border_color(theme::color(palette::border_subtle))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .child(pin)
                .child(form)
                .child(settings)
                .child(keep_awake(live)),
        )
        .child(
            view()
                .flex_none()
                .tooltip(widgets::wrap_tooltip(i18n::tr("common.quit_app")))
                .child(
                    button()
                        .flex_row()
                        .flex_none()
                        .h(px(CELL))
                        .items_center()
                        .gap(px(6.))
                        .px(px(10.))
                        .rounded(px(7.))
                        .hover(|s| s.bg(theme::color(palette::surface_raised)))
                        .on_press(|| root::ask(HostAction::Quit))
                        .child(widgets::icon(CustomIconName::Power, 14., palette::text_dim))
                        .child(
                            text(i18n::tr("common.quit"))
                                .text_size(px(12.))
                                .text_color(theme::color(palette::text_dim)),
                        ),
                ),
        )
}

/// A length as the menu words it (`views::hold_label`).
fn hold_label(minutes: u32) -> String {
    match minutes {
        m if m < 60 => t!("common.keep_awake_minutes", n = m).to_string(),
        60 => i18n::tr("common.keep_awake_hour"),
        m => t!("common.keep_awake_hours", n = m / 60).to_string(),
    }
}

/// Keep the machine awake for a while (`awake.rs`): the cup, lit while a
/// hold runs with the time left beside it, opens a menu of lengths with
/// the running one checked, and Off (`views::keep_awake_menu`).
fn keep_awake(live: Live) -> AnyElement {
    let hold = live.watch(awake::held);
    let held = move || hold.get().is_some();
    let mac = cfg!(target_os = "macos");
    let tip = move || match hold.get() {
        Some(hold) => t!(
            if mac {
                "common.keep_awake_on"
            } else {
                "common.keep_awake_on_linux"
            },
            time = format::clock(hold.until)
        )
        .to_string(),
        None => i18n::tr(if mac {
            "common.keep_awake"
        } else {
            "common.keep_awake_linux"
        }),
    };
    let left = move || {
        hold.get().map_or_else(String::new, |hold| {
            format::uptime(u64::from(hold.minutes_left(SystemTime::now())) * 60)
        })
    };
    let item = move |label: String, minutes: u32| {
        let on = move || hold.get().map_or(0, |hold| hold.minutes) == minutes;
        menu_item()
            .on_select(move || {
                awake::hold_for(minutes);
                store::changed();
            })
            .flex_row()
            .items_center()
            .gap(px(6.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(5.))
            .hover(|s| s.bg(theme::color(palette::popover_hover)))
            .child(
                text(move || if on() { "✓" } else { "" })
                    .flex_none()
                    .w(px(14.))
                    .text_size(px(12.))
                    .text_color(theme::color(palette::gauge)),
            )
            .child(
                text(label)
                    .text_size(px(12.))
                    .text_color(theme::color(palette::text)),
            )
    };
    // No tooltip under the open menu: it came up over the items it
    // describes.
    let open = signal(false);
    let mut popup = menu()
        .offset(px(4.))
        .on_open(move || open.set(true))
        .on_close(move || open.set(false))
        .popup_style(|s| {
            s.min_w(px(170.))
                .p(px(4.))
                .rounded(px(8.))
                .bg(theme::color(palette::popover))
                .outline(px(1.))
                .outline_offset(px(-1.))
                .outline_color(theme::color(palette::border))
                .shadow_lg()
        })
        .trigger(
            view()
                .flex_row()
                .flex_none()
                .h(px(CELL))
                .min_w(px(32.))
                .items_center()
                .justify_center()
                .gap(px(4.))
                .px(px(8.))
                .rounded(px(7.))
                .bg(move || theme::color(move || if held() { palette::chip() } else { CLEAR }))
                .hover(|s| s.bg(theme::color(palette::surface_raised)))
                .tooltip(widgets::live_tooltip(tip).disabled(move || open.get()))
                .child(widgets::icon(CustomIconName::Coffee, 16., move || {
                    footer_ink(held())
                }))
                .child(when(held, move || {
                    text(left)
                        .pr(px(2.))
                        .tabular_nums()
                        .text_size(px(10.))
                        .line_height(px(14.))
                        .text_color(theme::color(palette::text))
                })),
        )
        // The menu's first line: until when, or what it is for.
        .child(
            text(move || match hold.get() {
                Some(hold) => {
                    t!("common.keep_awake_until", time = format::clock(hold.until)).to_string()
                }
                None => i18n::tr("common.keep_awake_for"),
            })
            .px(px(8.))
            .py(px(4.))
            .text_size(px(12.))
            .text_color(theme::color(palette::text_dim)),
        );
    for minutes in awake::HOLD_MINUTES {
        popup = popup.child(item(hold_label(minutes), minutes));
    }
    popup
        .child(
            menu_separator()
                .h(px(1.))
                .my(px(4.))
                .bg(theme::color(palette::border_subtle)),
        )
        .child(item(i18n::tr("common.keep_awake_off"), 0))
        .into_element()
}
