//! The settings window on NSH: the gpui front end's `SettingsWindow`
//! (`gpui_app.rs`) and `views/config.rs`, whose comments say why every
//! page and row is the way it is — this file carries the same choices and
//! does not repeat their reasons. The window's rules are shared
//! (`settingspage.rs`). The pages arrive in phase 4j of
//! docs/nsh-migration.md: Interface first; the others are a card that
//! says so until theirs is ported.
//!
//! A nav down the left, the page on the right in a scroll view. The page
//! on screen outlives a restyle — a change of theme or language loads
//! this root again (`HostAction::Restyle`), and a click on a language
//! chip must not land the reader back on the first page. The root follows
//! the store for as long as the window is open; the preferences it shows
//! are not in the store, so a setter here says the store changed.
//!
//! Its keys are the gpui settings window's dismissals: Escape, and `mod+w`
//! through the host's Window menu.

use crate::assets::CustomIconName;
use crate::nsh::root::{self, AuxWindow, HostAction, Live};
use crate::nsh::widgets::{self, CLEAR};
use crate::nsh::{store, theme};
use crate::prefs::{self, LanguagePref, ThemePref, TrayPref};
use crate::settingspage::{self, Autostart, Choices, SettingsSection, ThresholdRow};
use crate::state::ZStatsAppState;
use crate::{autostart, i18n, palette};
use nsh_core::ui::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The nav's width (`gpui_app::settings_nav`).
const NAV_W: f32 = 132.;

/// Above and below the page (`gpui_app::AUX_BODY_PAD`).
const BODY_PAD: f32 = 14.;

/// A row's inset inside its card (`config.rs`'s rows: 13, a point more
/// than a list row's).
const ROW_PAD_X: f32 = 13.;

/// The page on screen, as its place in [`SettingsSection::ALL`]: kept
/// across a restyle, which makes this root again.
static SECTION: AtomicUsize = AtomicUsize::new(0);

fn section() -> SettingsSection {
    SettingsSection::ALL
        .get(SECTION.load(Ordering::Relaxed))
        .copied()
        .unwrap_or_default()
}

fn set_section(section: SettingsSection) {
    let at = SettingsSection::ALL
        .iter()
        .position(|s| *s == section)
        .unwrap_or(0);
    SECTION.store(at, Ordering::Relaxed);
}

#[component]
pub fn Settings() -> impl IntoElement {
    on_key("escape", |_| {
        root::ask(HostAction::Close(AuxWindow::Settings))
    });
    let live = Live::follow(|_| true);
    let current = signal(section());
    view()
        .size_full()
        .flex_row()
        .bg(theme::color(palette::ground))
        .child(nav(live, current))
        .child(
            scroll().flex_1().min_w(px(0.)).h_full().child(
                view()
                    .px(px(16.))
                    .py(px(BODY_PAD))
                    .gap(px(8.))
                    .child(dynamic(move || page(live, current.get()))),
            ),
        )
}

/// The pages down the left, the one on screen in the card's fill; About
/// wears the gear's dot while an update waits and About is not open.
fn nav(live: Live, current: Signal<SettingsSection>) -> AnyElement {
    let nudge = live.memo(|state: &ZStatsAppState| state.update_nudge().is_some());
    // Made again as the page changes: the label's weight is the one part
    // a binding cannot carry.
    dynamic(move || nav_items(current, nudge)).into_element()
}

fn nav_items(current: Signal<SettingsSection>, nudge: Memo<bool>) -> AnyElement {
    let shown = current.get();
    view()
        .flex_none()
        .w(px(NAV_W))
        .h_full()
        .px(px(10.))
        .py(px(12.))
        .gap(px(2.))
        .border_r(px(1.))
        .border_color(theme::color(palette::border_subtle))
        .bg(theme::color(palette::trough))
        .children(
            SettingsSection::ALL
                .into_iter()
                .map(|item| {
                    let on = move || shown == item;
                    nsh_base::button()
                        .w_full()
                        .rounded(px(6.))
                        .px(px(10.))
                        .py(px(6.))
                        .bg(move || {
                            theme::color(move || if on() { palette::surface() } else { CLEAR })
                        })
                        .hover(move |s| {
                            if on() {
                                s
                            } else {
                                s.bg(theme::color(palette::surface_raised))
                            }
                        })
                        .on_press(move || {
                            if current.get_untracked() != item {
                                set_section(item);
                                current.set(item);
                            }
                        })
                        .child(
                            view()
                                .flex_row()
                                .items_center()
                                .gap(px(6.))
                                .child(widgets::icon(section_icon(item), 13., move || {
                                    if on() {
                                        palette::ink()
                                    } else {
                                        palette::text_dim()
                                    }
                                }))
                                .child(
                                    text(i18n::tr(item.label_key()))
                                        .flex_1()
                                        .min_w(px(0.))
                                        .text_size(px(12.))
                                        .line_height(widgets::line(12.))
                                        .font_weight(if on() {
                                            FontWeight::MEDIUM
                                        } else {
                                            FontWeight::NORMAL
                                        })
                                        .text_color(move || {
                                            theme::color(move || {
                                                if on() {
                                                    palette::ink()
                                                } else {
                                                    palette::text()
                                                }
                                            })
                                        }),
                                )
                                .child(when(
                                    move || item == SettingsSection::About && nudge.get() && !on(),
                                    || {
                                        view()
                                            .flex_none()
                                            .w(px(6.))
                                            .h(px(6.))
                                            .rounded_full()
                                            .bg(theme::color(palette::accent))
                                    },
                                )),
                        )
                        .into_element()
                })
                .collect::<Vec<_>>(),
        )
        .into_element()
}

/// The nav row's glyph (`config::section_icon`).
fn section_icon(section: SettingsSection) -> CustomIconName {
    match section {
        SettingsSection::Interface => CustomIconName::Palette,
        SettingsSection::Alerts => CustomIconName::Bell,
        SettingsSection::Advanced => CustomIconName::Settings,
        #[cfg(target_os = "macos")]
        SettingsSection::Permissions => CustomIconName::Shield,
        SettingsSection::About => CustomIconName::Info,
    }
}

fn page(live: Live, section: SettingsSection) -> AnyElement {
    match section {
        SettingsSection::Interface => interface_card(live),
        SettingsSection::Alerts => alerts_page(live),
        other => not_yet(other),
    }
}

/// A page that has not reached this front end yet. Not translated — a
/// development build's card, gone with the last page.
fn not_yet(section: SettingsSection) -> AnyElement {
    widgets::card()
        .gap(px(6.))
        .child(widgets::card_header(i18n::tr(section.label_key()), None))
        .child(
            text("Not on the NSH front end yet — phase 4j of docs/nsh-migration.md.")
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(theme::color(palette::text_muted)),
        )
        .into_element()
}

/// The preferences a setter just wrote are not the store's, but every
/// page reads them: the store says it changed, and this root reads again.
fn changed() {
    store::changed();
}

/// Language, theme, the menu bar, banners, login, the daily check,
/// Overview's form, the panel's size and its opacity — `app.toml`.
fn interface_card(live: Live) -> AnyElement {
    live.track();
    let (themes, theme_tip) = settingspage::theme_choices();
    let mut card = widgets::list_shell()
        .child(widgets::list_header(
            widgets::list_title(i18n::tr("config.interface")).into_element(),
            Some(file_note("app.toml")),
        ))
        .child(pref_row(
            live,
            i18n::tr("config.language"),
            None,
            settingspage::language_choices(),
            prefs::language,
            |pref: LanguagePref| {
                prefs::set_language(pref);
                root::ask(HostAction::Restyle);
            },
        ))
        .child(pref_row(
            live,
            i18n::tr("config.theme"),
            Some(theme_tip),
            themes,
            prefs::theme_in_force,
            |pref: ThemePref| {
                prefs::set_theme(pref);
                root::ask(HostAction::Restyle);
            },
        ))
        .child(pref_row(
            live,
            i18n::tr("config.tray"),
            Some(i18n::tr("config.tray_tip")),
            settingspage::tray_choices(),
            prefs::tray,
            |pref: TrayPref| {
                prefs::set_tray(pref);
                // The tray re-faces from the store as it stands.
                store::update(|state, fx| fx.sync_tray(state));
            },
        ))
        .child(switch_row(
            live,
            i18n::tr("config.notifications"),
            i18n::tr("config.notifications_tip"),
            prefs::notifications,
            |on| {
                prefs::set_notifications(on);
                changed();
            },
        ))
        .child(autostart_row(live))
        .child(switch_row(
            live,
            i18n::tr("config.disk_watch"),
            i18n::tr("config.disk_watch_tip"),
            prefs::disk_watch,
            |on| {
                prefs::set_disk_watch(on);
                changed();
            },
        ))
        .child(switch_row(
            live,
            i18n::tr("config.overview_detail"),
            i18n::tr("config.overview_detail_tip"),
            prefs::overview_detail,
            |on| {
                prefs::set_overview_detail(on);
                changed();
                root::ask(HostAction::FitForm);
            },
        ));
    if cfg!(target_os = "macos") {
        card = card.child(panel_size_row(live));
    }
    card.child(opacity_row(live)).into_element()
}

/// A card header's file name, which shows the file: where these settings
/// actually live.
fn file_note(name: &'static str) -> AnyElement {
    view()
        .flex_none()
        .tooltip(widgets::wrap_tooltip(settingspage::file_reveal_tip()))
        .child(
            nsh_base::button()
                .rounded(px(4.))
                .px(px(4.))
                .ml(px(-4.))
                .mr(px(-4.))
                .hover(|s| s.bg(theme::color(palette::surface_raised)))
                .on_press(move || settingspage::reveal_file(name))
                .child(
                    text(name)
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .text_color(theme::color(palette::text_dim)),
                ),
        )
        .into_element()
}

/// A row of the card: its label (and ⓘ) on the left, its control on the
/// right, a hairline under it unless it is the card's last.
fn row(label: String, tip: Option<String>, control: AnyElement, last: bool) -> View {
    let mut name = view().flex_row().items_center().gap(px(4.)).child(
        text(label)
            .text_size(px(11.))
            .line_height(widgets::line(11.))
            .text_color(theme::color(palette::ink)),
    );
    if let Some(tip) = tip {
        name = name.child(widgets::info_icon(widgets::wrap_tooltip(tip)));
    }
    view()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .px(px(ROW_PAD_X))
        .py(px(8.))
        .border_b(px(if last { 0. } else { 1. }))
        .border_color(theme::color(palette::border_subtle))
        .child(name.flex_1().min_w(px(0.)))
        .child(control)
}

/// "Label · option chips" (`config::pref_row`).
fn pref_row<T: Copy + PartialEq + 'static>(
    live: Live,
    label: String,
    tip: Option<String>,
    choices: Choices<T>,
    current: fn() -> T,
    choose: impl Fn(T) + Copy + 'static,
) -> AnyElement {
    let chips: Vec<AnyElement> = choices
        .into_iter()
        .map(|(words, value)| {
            widgets::option_chip(
                words,
                move || {
                    live.track();
                    current() == value
                },
                move || choose(value),
            )
        })
        .collect();
    row(
        label,
        tip,
        view()
            .flex_row()
            .flex_none()
            .gap(px(4.))
            .children(chips)
            .into_element(),
        false,
    )
    .into_element()
}

/// "Label · switch" (`config::notifications_row` and its siblings).
fn switch_row(
    live: Live,
    label: String,
    tip: String,
    on: fn() -> bool,
    set: impl Fn(bool) + 'static,
) -> AnyElement {
    row(
        label,
        Some(tip),
        switch(
            move || {
                live.track();
                on()
            },
            set,
        ),
        false,
    )
    .into_element()
}

/// gpui-kit's small switch: a 28 × 16 track, a 12 thumb inset by 2, in
/// the theme's switch colours (`palette::switch_on` and its two siblings).
fn switch(on: impl Fn() -> bool + Copy + 'static, set: impl Fn(bool) + 'static) -> AnyElement {
    nsh_base::button()
        .flex_none()
        .w(px(28.))
        .h(px(16.))
        .p(px(2.))
        .rounded_full()
        .bg(move || {
            theme::color(move || {
                if on() {
                    palette::switch_on()
                } else {
                    palette::switch_off()
                }
            })
        })
        .on_press(move || set(!on()))
        .child(
            view()
                .w(px(12.))
                .h(px(12.))
                .rounded_full()
                .ml(move || px(if on() { 12. } else { 0. }))
                .bg(theme::color(palette::switch_thumb)),
        )
        .into_element()
}

/// Launch at login: a switch, or the sentence that stands for it.
fn autostart_row(live: Live) -> AnyElement {
    let state = computed(move || {
        live.track();
        settingspage::autostart()
    });
    row(
        i18n::tr("config.autostart"),
        Some(settingspage::autostart_tip()),
        dynamic(move || match state.get() {
            Autostart::Switch(_) => switch(
                move || matches!(state.get(), Autostart::Switch(true)),
                |on| {
                    autostart::set_enabled(on);
                    changed();
                },
            ),
            Autostart::Approve(words) => nsh_base::button()
                .flex_none()
                .rounded_full()
                .border(px(1.))
                .border_color(theme::color(palette::border))
                .bg(theme::color(palette::inset))
                .px(px(10.))
                .py(px(3.))
                .hover(|s| s.bg(theme::color(palette::surface_raised)))
                .on_press(autostart::open_login_items)
                .child(
                    text(words)
                        .text_size(px(11.))
                        .line_height(widgets::line(11.))
                        .text_color(theme::color(palette::text)),
                )
                .into_element(),
            Autostart::Absent(words) => text(words)
                .flex_none()
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::text_dim))
                .into_element(),
        })
        .into_element(),
        false,
    )
    .into_element()
}

/// The panel's dragged size, and the way back to the designed one.
fn panel_size_row(live: Live) -> AnyElement {
    let custom = computed(move || {
        live.track();
        prefs::panel_size_custom()
    });
    row(
        i18n::tr("config.panel_size"),
        Some(i18n::tr("config.panel_size_tip")),
        dynamic(move || {
            if custom.get() {
                nsh_base::button()
                    .flex_none()
                    .h(px(20.))
                    .px(px(8.))
                    .rounded(px(5.))
                    .border(px(1.))
                    .border_color(theme::color(palette::border))
                    .bg(theme::color(palette::inset))
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(theme::color(palette::chip)))
                    .on_press(|| {
                        prefs::reset_panel_size();
                        changed();
                        root::ask(HostAction::FitForm);
                    })
                    .child(
                        text(i18n::tr("config.panel_size_reset"))
                            .text_size(px(10.))
                            .line_height(widgets::line(10.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::color(palette::text)),
                    )
                    .into_element()
            } else {
                text(i18n::tr("config.panel_size_default"))
                    .text_size(px(11.))
                    .line_height(widgets::line(11.))
                    .text_color(theme::color(palette::text_muted))
                    .into_element()
            }
        })
        .into_element(),
        false,
    )
    .into_element()
}

/// The panel's opacity: the label, then its chips under it — the card's
/// last row.
fn opacity_row(live: Live) -> AnyElement {
    let chips: Vec<AnyElement> = settingspage::opacity_choices()
        .into_iter()
        .map(|(words, value)| {
            widgets::option_chip(
                words,
                move || {
                    live.track();
                    settingspage::opacity_on(prefs::opacity(), value)
                },
                move || {
                    prefs::set_opacity(value);
                    // The panel's ground is read as its root is made.
                    root::ask(HostAction::Restyle);
                },
            )
        })
        .collect();
    view()
        .px(px(ROW_PAD_X))
        .py(px(8.))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .child(
                    text(i18n::tr("config.opacity"))
                        .text_size(px(11.))
                        .line_height(widgets::line(11.))
                        .text_color(theme::color(palette::ink)),
                )
                .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
                    "config.opacity_tip",
                )))),
        )
        .child(view().flex_row().mt(px(6.)).gap(px(4.)).children(chips))
        .into_element()
}

/// A card's title and the ⓘ that carries its standing note
/// (`config::titled`).
fn titled(title: String, tip: String) -> AnyElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .min_w(px(0.))
        .child(widgets::list_title(title).flex_shrink(1.))
        .child(widgets::info_icon(widgets::wrap_tooltip(tip)))
        .into_element()
}

/// Writes one `config.toml` key through the store (`apply_setting`, the
/// CLI's `-add`).
fn apply(key: &'static str, value: &'static str) {
    store::update(|state, fx| {
        if let Err(e) = state.apply_setting(key, value, fx) {
            tracing::error!("apply {key}={value}: {e}");
        }
    });
}

/// What the Alerts page shows: the rules and the per-subject overrides,
/// or nothing to show while there is no readable config.
#[derive(Clone, PartialEq)]
struct Rules {
    thresholds: Vec<ThresholdRow>,
    overrides: Vec<(&'static str, String, f64)>,
}

fn rules_of(state: &ZStatsAppState) -> Option<Rules> {
    let file = state.settings()?;
    Some(Rules {
        thresholds: settingspage::threshold_rows(file),
        overrides: settingspage::override_rows(&file.alerts),
    })
}

/// The thresholds, and the per-subject rules written from alert cards.
fn alerts_page(live: Live) -> AnyElement {
    let rules = live.memo(rules_of);
    dynamic(move || match rules.get() {
        None => widgets::empty_card(
            i18n::tr("config.unavailable"),
            i18n::tr("config.unavailable_body"),
        ),
        Some(rules) => {
            let page = view().gap(px(8.)).child(thresholds_card(rules.thresholds));
            if rules.overrides.is_empty() {
                page.into_element()
            } else {
                page.child(overrides_card(rules.overrides)).into_element()
            }
        }
    })
    .into_element()
}

fn thresholds_card(rows: Vec<ThresholdRow>) -> AnyElement {
    let last = rows.len().saturating_sub(1);
    widgets::list_shell()
        .child(widgets::list_header(
            titled(
                i18n::tr("config.thresholds"),
                i18n::tr("config.thresholds_note"),
            ),
            Some(file_note("config.toml")),
        ))
        .children(
            rows.into_iter()
                .enumerate()
                .map(|(i, row)| threshold_row(row, i == last))
                .collect::<Vec<_>>(),
        )
        .into_element()
}

/// A rule: its name and key on the left, how many subjects override it
/// and its value on the right, its presets under them.
fn threshold_row(row: ThresholdRow, last: bool) -> AnyElement {
    let ThresholdRow {
        key,
        setting,
        overrides,
    } = row;
    let mut figures = view().flex_row().items_center().gap(px(6.));
    if overrides > 0 {
        figures = figures.child(
            text(settingspage::override_count(overrides))
                .rounded_full()
                .border(px(1.))
                .border_color(theme::color(|| palette::accent_wash(45)))
                .px(px(6.))
                .text_size(px(8.5))
                .line_height(widgets::line(8.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(palette::accent_light)),
        );
    }
    figures = figures.child(
        text(setting.shown.clone())
            .font_family(widgets::MONO)
            .text_size(px(10.))
            .line_height(widgets::line(10.))
            .text_color(theme::color(palette::text)),
    );
    let chips: Vec<AnyElement> = settingspage::threshold_choices(key)
        .into_iter()
        .map(|(label, value)| {
            let on = label == setting.lit;
            setting_chip(label, on, move || apply(key, value))
        })
        .collect();
    view()
        .px(px(ROW_PAD_X))
        .py(px(7.))
        .border_b(px(if last { 0. } else { 1. }))
        .border_color(theme::color(palette::border_subtle))
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            text(settingspage::threshold_label(key))
                                .text_size(px(11.))
                                .line_height(widgets::line(11.))
                                .text_color(theme::color(palette::ink)),
                        )
                        .child(widgets::info_icon(widgets::wrap_tooltip(
                            settingspage::threshold_key_tip(key),
                        ))),
                )
                .child(figures),
        )
        .child(view().flex_row().mt(px(6.)).gap(px(4.)).children(chips))
        .into_element()
}

/// A preset under a rule (`config::setting_chips`): the option chip's
/// look, a little tighter.
fn setting_chip(label: String, on: bool, act: impl Fn() + 'static) -> AnyElement {
    nsh_base::button()
        .flex_none()
        .rounded_full()
        .border(px(1.))
        .border_color(theme::color(move || palette::choice_line(on)))
        .bg(theme::color(move || palette::choice_fill(on)))
        .hover(move |s| s.bg(theme::color(move || palette::choice_hover(on))))
        .px(px(7.))
        .py(px(1.))
        .on_press(act)
        .child(
            text(label)
                .text_size(px(10.))
                .line_height(widgets::line(10.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(move || palette::choice_ink(on))),
        )
        .into_element()
}

/// Every per-subject override, each with the way back to the rule.
fn overrides_card(rows: Vec<(&'static str, String, f64)>) -> AnyElement {
    let total = rows.len();
    widgets::list_shell()
        .child(widgets::list_header(
            titled(
                i18n::tr("config.overrides"),
                i18n::tr("config.overrides_note"),
            ),
            Some(widgets::note(settingspage::override_count(total)).into_element()),
        ))
        .children(
            rows.into_iter()
                .enumerate()
                .map(|(i, (key, name, value))| override_row(key, name, value, i + 1 == total))
                .collect::<Vec<_>>(),
        )
        .into_element()
}

/// One override: who it is for, which rule, what it was set to, and the
/// control that drops it.
fn override_row(key: &'static str, name: String, value: f64, last: bool) -> AnyElement {
    let removal = name.clone();
    view()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .px(px(ROW_PAD_X))
        .py(px(7.))
        .border_b(px(if last { 0. } else { 1. }))
        .border_color(theme::color(palette::border_subtle))
        .child(
            view()
                .flex_1()
                .min_w(px(0.))
                .child(
                    text(name)
                        .number_of_lines(1u32)
                        .text_size(px(11.))
                        .line_height(widgets::line(11.))
                        .text_color(theme::color(palette::ink)),
                )
                .child(
                    text(key)
                        .font_family(widgets::MONO)
                        .text_size(px(9.))
                        .line_height(widgets::line(9.))
                        .text_color(theme::color(palette::text_dim)),
                ),
        )
        .child(
            view()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .child(
                    text(settingspage::override_value(value))
                        .font_family(widgets::MONO)
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .text_color(theme::color(palette::text)),
                )
                .child(
                    view()
                        .flex_none()
                        .tooltip(widgets::wrap_tooltip(i18n::tr("config.override_reset_tip")))
                        .child(
                            nsh_base::button()
                                .rounded_full()
                                .border(px(1.))
                                .border_color(theme::color(palette::border))
                                .bg(theme::color(palette::inset))
                                .px(px(7.))
                                .py(px(1.))
                                .hover(|s| s.bg(theme::color(palette::surface_raised)))
                                .on_press(move || {
                                    let name = removal.clone();
                                    store::update(|state, fx| {
                                        if let Err(e) = state.remove_alert_override(key, &name, fx)
                                        {
                                            tracing::warn!("remove {key} override for {name}: {e}");
                                        }
                                    });
                                })
                                .child(
                                    text(i18n::tr("config.override_reset"))
                                        .text_size(px(10.))
                                        .line_height(widgets::line(10.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme::color(palette::text)),
                                ),
                        ),
                ),
        )
        .into_element()
}
