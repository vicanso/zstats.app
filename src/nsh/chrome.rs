//! The panel's frame: the tab strip above the page and the footer under
//! it — a port of `views::tab_strip` and `views::footer` on the gpui front
//! end, whose comments say why each piece is where it is and looks as it
//! does; the sizes, inks and words are theirs.
//!
//! NSH makes a component once and binds what changes, so nothing here is
//! rebuilt on a tick: a selected tab, a lit pin, the cup's time left are
//! bindings over the store's memos (`Live::memo`, `Live::watch`). That is
//! also what keeps the keep-awake menu open across a tick — a footer made
//! again would have closed it. A hide is another matter: the panel is not
//! destroyed, so a menu open as it went away would come back open, and
//! the cup's is closed as the panel hides (`root::close_on_hide`).

use crate::assets::CustomIconName;
#[cfg(target_os = "macos")]
use crate::clamshell;
use crate::color::Rgba;
use crate::nsh::root::{self, AuxWindow, CARD_GAP, GUTTER, HostAction, Live};
use crate::nsh::widgets::{self, CLEAR};
use crate::nsh::{store, theme};
use crate::state::{Tab, ZStatsAppState};
use crate::{awake, format, i18n, palette, prefs};
use nsh_base::{MenuEvent, TooltipElement, button, menu, menu_separator, tab, tabs};
use nsh_core::prelude::*;
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
pub(super) fn shortcut_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl+"
    }
}

/// A tab cell's fill and ink: the theme's selected look on the tab on
/// screen, and red on Alerts while an episode of this session has not
/// been shown there (`SeenAlert::is_news`). Opening that tab is the end
/// of the colour for every episode then on it; a later episode, or a
/// graver report of one already shown, lights it again. The alarm wins
/// over the theme.
fn cell_paint(on: bool, alarm: bool) -> (Rgba, Rgba) {
    match (on, alarm) {
        (true, true) => (palette::accent_wash(16), palette::accent_light()),
        (false, true) => (CLEAR, palette::accent_light()),
        (true, false) => palette::active_item(),
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
    // News, not "a card exists". Opening this tab marks every episode
    // then on it as shown (`see_alerts`), and the bell returns to the
    // theme; the card underneath keeps its own red. A follow-up of one
    // already shown does not light it again.
    let alerting =
        live.memo(|state: &ZStatsAppState| state.alerts().iter().any(|seen| seen.is_news()));
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
            tabs()
                .flex_row()
                .justify_between()
                .p(px(4.))
                .rounded(px(10.))
                .bg(theme::color(palette::trough))
                .children(cells),
        )
}

fn tab_cell(this: Tab, active: Memo<Tab>, alerting: Memo<bool>) -> AnyElement {
    let alarm = move || this == Tab::Alerts && alerting.get();
    let ink = move || cell_paint(active.get() == this, alarm()).1;
    let tip = t!(
        "tabs.shortcut",
        name = this.title(),
        hint = shortcut_hint(),
        n = this.index() + 1
    )
    .to_string();
    // The tooltip on a view around the tab: `nsh_base`'s tab element
    // takes none of its own.
    view()
        .flex_none()
        .tooltip(widgets::wrap_tooltip(tip))
        .child(
            tab()
                .flex_none()
                .accessibility_label(Some(SharedString::from(this.title())))
                .rounded(px(7.))
                .selected(move || active.get() == this)
                .active(|s| s.opacity(0.78))
                .focus_visible(widgets::focus_ring)
                .on_press(move || store::update(|state, fx| state.set_tab(this, fx)))
                .child(
                    when(
                        move || active.get() == this,
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
                                        .child(widgets::icon(tab_icon(this), TAB_ICON_ON, ink))
                                        .child(
                                            tab_name(this).text_color(move || theme::color(ink)),
                                        ),
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
                            .transition_colors()
                            .duration(120.)
                            .child(widgets::icon(tab_icon(this), 16., ink))
                    }),
                ),
        )
        .into_element()
}

/// One of the footer's icon buttons: a 32×30 box, the icon at 16, lit
/// with the active theme item fill while `lit` says so.
fn footer_button(
    icon: impl IntoElement,
    lit: impl Fn() -> bool + Copy + 'static,
    label: impl Fn() -> String + 'static,
    tip: TooltipElement,
    on_press: impl Fn() + 'static,
) -> AnyElement {
    view()
        .flex_none()
        .tooltip(tip)
        .child(
            button()
                .flex_none()
                .selected(lit)
                .accessibility_label(move || Some(SharedString::from(label())))
                .w(px(32.))
                .h(px(CELL))
                .items_center()
                .justify_center()
                .rounded(px(7.))
                .bg(move || {
                    theme::color(move || {
                        if lit() {
                            palette::active_item().0
                        } else {
                            CLEAR
                        }
                    })
                })
                .hover(move |s| {
                    s.bg(move || {
                        theme::color(move || {
                            if lit() {
                                palette::active_item_hover()
                            } else {
                                palette::surface_raised()
                            }
                        })
                    })
                })
                .transition_colors()
                .duration(120.)
                .active(|s| s.opacity(0.76))
                .focus_visible(widgets::focus_ring)
                .on_press(on_press)
                .child(icon),
        )
        .into_element()
}

/// The ink of a footer glyph, lit or not.
fn footer_ink(lit: bool) -> Rgba {
    if lit {
        palette::active_item().1
    } else {
        palette::text_dim()
    }
}

/// Footer desktop lock button: opens the temporary-password setup dialog
/// (available on both macOS and Linux).
fn lock_button(live: Live) -> AnyElement {
    let locked = live.watch(crate::screenlock::is_active);
    footer_button(
        widgets::icon(CustomIconName::Lock, 16., move || footer_ink(locked.get())),
        move || locked.get(),
        || i18n::tr("common.lock_screen"),
        widgets::wrap_tooltip(i18n::tr("common.lock_screen_tip")),
        || root::ask(HostAction::RequestScreenLock),
    )
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
        move || {
            i18n::tr(if pinned.get() {
                "common.unpin"
            } else {
                "common.pin"
            })
        },
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
        move || {
            i18n::tr(if detail.get() {
                "overview.collapse_tip"
            } else {
                "overview.expand_tip"
            })
        },
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
        || i18n::tr("tabs.settings"),
        widgets::live_tooltip(move || match (nudge.get(), template_nudge.get()) {
            (Some(v), true) => t!("config.both_nudge_tip", v = v).to_string(),
            (Some(v), false) => t!("config.update_nudge_tip", v = v).to_string(),
            (None, true) => i18n::tr("config.template_nudge_tip"),
            (None, false) => i18n::tr("tabs.settings"),
        }),
        || root::ask(HostAction::Open(AuxWindow::Settings)),
    );
    let left = view()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .child(pin)
        .child(form)
        .child(settings)
        .child(keep_awake(live))
        .child(lock_button(live));
    view()
        .flex_row()
        .flex_none()
        .h(px(FOOTER_HEIGHT))
        .items_center()
        .justify_between()
        .gap(px(8.))
        .px(px(8.))
        .border_t(px(1.))
        .border_color(theme::color(palette::border_subtle))
        .child(left)
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
                        .transition_colors()
                        .duration(120.)
                        .active(|s| s.opacity(0.76))
                        .focus_visible(widgets::focus_ring)
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

/// Keep the machine awake for a while (`awake.rs`, plus `clamshell.rs` on
/// macOS): one coffee-cup control that prevents both display sleep/auto-lock
/// and lid-closed sleep. Lit while either hold runs, with a countdown when
/// timed.
#[cfg(target_os = "macos")]
fn keep_awake(live: Live) -> AnyElement {
    let hold = live.watch(awake::held);
    let mode = live.watch(clamshell::mode);
    let busy = live.watch(clamshell::busy);
    let held = move || hold.get().is_some() || mode.get().is_on();
    let timed_until = move || {
        hold.get()
            .and_then(|h| h.until())
            .or_else(|| match mode.get() {
                clamshell::Mode::Timed { until, .. } => Some(until),
                _ => None,
            })
    };
    let timed = move || timed_until().is_some();
    let tip = move || match (hold.get(), mode.get()) {
        (None, clamshell::Mode::Off) => i18n::tr("common.keep_awake"),
        (Some(h), m) => {
            if let Some(until) = h.until() {
                t!(
                    if m.is_on() {
                        "common.keep_awake_on"
                    } else {
                        "common.keep_awake_on_display_only"
                    },
                    time = format::clock(until)
                )
                .to_string()
            } else if m.is_on() {
                i18n::tr("common.keep_awake_on_indefinite")
            } else {
                i18n::tr("common.keep_awake_on_indefinite_display_only")
            }
        }
        (None, clamshell::Mode::Timed { until, .. }) => {
            t!("common.clamshell_on_until", time = format::clock(until)).to_string()
        }
        (None, clamshell::Mode::Indefinite) => i18n::tr("common.clamshell_on_indefinite"),
        (None, clamshell::Mode::External) => i18n::tr("common.clamshell_on_external"),
    };
    // Worked out again on every change of the store — a tick, while the
    // panel is on screen — and heard of only when the figure moves. Off
    // `hold` it never moved: a hold is the same value from the pick to its
    // end, so "30m" stood for the whole half hour.
    let left = live.watch(|| {
        let now = SystemTime::now();
        awake::held()
            .and_then(|hold| hold.minutes_left(now))
            .or_else(|| clamshell::mode().minutes_left(now))
            .map_or_else(String::new, format::countdown)
    });
    let item = move |label: String, req: clamshell::Request| {
        let on = move || match req {
            clamshell::Request::Off => {
                hold.get().is_none() && matches!(mode.get(), clamshell::Mode::Off)
            }
            clamshell::Request::Indefinite => {
                matches!(hold.get(), Some(awake::Hold::Indefinite))
                    || (hold.get().is_none() && matches!(mode.get(), clamshell::Mode::Indefinite))
            }
            clamshell::Request::Timed(minutes) => match hold.get() {
                Some(h) => h.minutes() == Some(minutes),
                None => {
                    matches!(mode.get(), clamshell::Mode::Timed { minutes: m, .. } if m == minutes)
                }
            },
        };
        widgets::check_item(
            label,
            on,
            move || busy.get(),
            move || match req {
                clamshell::Request::Timed(minutes) => {
                    awake::hold_for(minutes);
                    clamshell::request(req, store::changed);
                }
                clamshell::Request::Indefinite => {
                    awake::hold_indefinite();
                    clamshell::request(req, store::changed);
                }
                clamshell::Request::Off => {
                    awake::hold_for(0);
                    if clamshell::mode().is_on() {
                        clamshell::request(req, store::changed);
                    } else {
                        store::changed();
                    }
                }
            },
        )
    };
    // No tooltip under the open menu: it came up over the items it
    // describes.
    let open = signal(false);
    root::close_on_hide(open);
    let mut popup = menu()
        .offset(px(4.))
        .open(move || Some(open.get()))
        .on_open_change(move |event: &MenuEvent| {
            if let MenuEvent::OpenChange(next, _) = event {
                open.set(*next);
            }
        })
        .popup_style(widgets::popup_surface(180.))
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
                .bg(move || {
                    theme::color(move || {
                        if held() {
                            palette::active_item().0
                        } else if open.get() {
                            palette::surface_raised()
                        } else {
                            CLEAR
                        }
                    })
                })
                .hover(move |s| {
                    s.bg(move || {
                        theme::color(move || {
                            if held() {
                                palette::active_item_hover()
                            } else {
                                palette::surface_raised()
                            }
                        })
                    })
                })
                .transition_colors()
                .duration(120.)
                .active(|s| s.opacity(0.78))
                .tooltip(widgets::live_tooltip(tip).disabled(move || open.get()))
                .child(widgets::icon(CustomIconName::Coffee, 16., move || {
                    if held() {
                        palette::active_item().1
                    } else if open.get() {
                        palette::text()
                    } else {
                        palette::text_dim()
                    }
                }))
                .child(when(timed, move || {
                    text(move || left.get())
                        .pr(px(2.))
                        .tabular_nums()
                        .text_size(px(10.))
                        .line_height(px(14.))
                        .text_color(move || theme::color(move || palette::active_item().1))
                })),
        )
        // The menu's first line: until when, or what it is for.
        .child(
            text(move || {
                if let Some(until) = timed_until() {
                    t!("common.keep_awake_until", time = format::clock(until)).to_string()
                } else if matches!(hold.get(), Some(awake::Hold::Indefinite))
                    || matches!(mode.get(), clamshell::Mode::Indefinite)
                {
                    i18n::tr("common.keep_awake_active")
                } else if matches!(mode.get(), clamshell::Mode::External) {
                    i18n::tr("common.clamshell_external")
                } else {
                    i18n::tr("common.keep_awake_for")
                }
            })
            .px(px(8.))
            .py(px(4.))
            .text_size(px(12.))
            .text_color(theme::color(palette::text_dim)),
        );
    for minutes in awake::HOLD_MINUTES {
        popup = popup.child(item(
            hold_label(minutes),
            clamshell::Request::Timed(minutes),
        ));
    }
    popup
        .child(item(
            i18n::tr("common.clamshell_indefinite"),
            clamshell::Request::Indefinite,
        ))
        .child(
            menu_separator()
                .h(px(1.))
                .my(px(4.))
                .bg(theme::color(palette::border_subtle)),
        )
        .child(item(
            i18n::tr("common.keep_awake_off"),
            clamshell::Request::Off,
        ))
        .into_element()
}

/// Keep the machine awake for a while (`awake.rs`): the cup, lit while a
/// hold runs with the time left beside it, opens a menu of lengths with
/// the running one checked, and Off (`views::keep_awake_menu`).
#[cfg(not(target_os = "macos"))]
fn keep_awake(live: Live) -> AnyElement {
    let hold = live.watch(awake::held);
    let held = move || hold.get().is_some();
    let tip = move || match hold.get().and_then(|h| h.until()) {
        Some(until) => t!("common.keep_awake_on_linux", time = format::clock(until)).to_string(),
        None => i18n::tr("common.keep_awake_linux"),
    };
    let left = live.watch(|| {
        awake::held()
            .and_then(|hold| hold.minutes_left(SystemTime::now()))
            .map_or_else(String::new, format::countdown)
    });
    let item = move |label: String, minutes: u32| {
        let on = move || hold.get().and_then(|hold| hold.minutes()).unwrap_or(0) == minutes;
        widgets::check_item(
            label,
            on,
            || false,
            move || {
                awake::hold_for(minutes);
                store::changed();
            },
        )
    };
    // No tooltip under the open menu: it came up over the items it
    // describes.
    let open = signal(false);
    root::close_on_hide(open);
    let mut popup = menu()
        .offset(px(4.))
        .open(move || Some(open.get()))
        .on_open_change(move |event: &MenuEvent| {
            if let MenuEvent::OpenChange(next, _) = event {
                open.set(*next);
            }
        })
        .popup_style(widgets::popup_surface(170.))
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
                .bg(move || {
                    theme::color(move || {
                        if held() {
                            palette::active_item().0
                        } else if open.get() {
                            palette::surface_raised()
                        } else {
                            CLEAR
                        }
                    })
                })
                .hover(move |s| {
                    s.bg(move || {
                        theme::color(move || {
                            if held() {
                                palette::active_item_hover()
                            } else {
                                palette::surface_raised()
                            }
                        })
                    })
                })
                .transition_colors()
                .duration(120.)
                .active(|s| s.opacity(0.78))
                .tooltip(widgets::live_tooltip(tip).disabled(move || open.get()))
                .child(widgets::icon(CustomIconName::Coffee, 16., move || {
                    if held() {
                        palette::active_item().1
                    } else if open.get() {
                        palette::text()
                    } else {
                        palette::text_dim()
                    }
                }))
                .child(when(held, move || {
                    text(move || left.get())
                        .pr(px(2.))
                        .tabular_nums()
                        .text_size(px(10.))
                        .line_height(px(14.))
                        .text_color(move || theme::color(move || palette::active_item().1))
                })),
        )
        // The menu's first line: until when, or what it is for.
        .child(
            text(move || match hold.get().and_then(|h| h.until()) {
                Some(until) => {
                    t!("common.keep_awake_until", time = format::clock(until)).to_string()
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
