//! The settings window on NSH: the gpui front end's `SettingsWindow`
//! (`gpui_app.rs`) and `views/config.rs`, whose comments say why every
//! page and row is the way it is — this file carries the same choices and
//! does not repeat their reasons. The window's rules are shared
//! (`settingspage.rs`); the pages arrived in phase 4j of
//! docs/nsh-migration.md.
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
use crate::settingspage::{UpdateAction, UpdateView};
use crate::state::{CachesSync, HintsSync, TemplateSync, WebhookTest, ZStatsAppState};
use crate::{about, alerttpl, appicon, autostart, cachepreset, cleanhints, color, i18n, opener};
use crate::{palette, proxy, updater, watch, webhook};
use nsh_base::{ButtonElement, TextViewLinkPressEvent, TextViewPart, TextViewParts};
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

/// A new settings window opens on the first page, as the gpui one does;
/// only a restyle keeps the page.
pub fn opening() {
    SECTION.store(0, Ordering::Relaxed);
}

/// The window came to the front: what may have moved in System Settings
/// while it was behind — launch at login, and Full Disk Access while its
/// page is the one on screen.
pub fn became_key() {
    autostart::refresh();
    #[cfg(target_os = "macos")]
    if section() == SettingsSection::Permissions {
        settingspage::refresh_full_disk_access();
    }
    // The pages read them, and nothing else would say so.
    store::changed();
}

#[component]
pub fn Settings() -> impl IntoElement {
    on_key("escape", |_| {
        root::ask(HostAction::Close(AuxWindow::Settings))
    });
    let live = Live::follow(|_| true);
    let current = signal(section());
    // The page's height on screen, for About's card, which fits it.
    let body = signal(0.0_f32);
    view()
        .size_full()
        .flex_row()
        .bg(theme::color(palette::ground))
        .child(nav(live, current))
        .child(
            scroll()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .on_layout(move |b: &Bounds<Points>| body.set(b.size.height.0))
                .child(
                    view()
                        .px(px(16.))
                        .py(px(BODY_PAD))
                        .gap(px(8.))
                        .child(dynamic(move || page(live, current.get(), body.read_only()))),
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
                                entered(item);
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

/// What a page asks for as the reader opens it from the nav: Permissions
/// probes Full Disk Access again, and About runs a real check where only
/// the silent one has — that one keeps no release notes, and this is the
/// one surface that shows them.
fn entered(section: SettingsSection) {
    match section {
        #[cfg(target_os = "macos")]
        SettingsSection::Permissions => {
            settingspage::refresh_full_disk_access();
            store::changed();
        }
        SettingsSection::About => store::update(|state, fx| state.refresh_update_for_about(fx)),
        _ => {}
    }
}

/// `body` is the height the page has on screen.
fn page(live: Live, section: SettingsSection, body: ReadSignal<f32>) -> AnyElement {
    match section {
        SettingsSection::Interface => interface_card(live),
        SettingsSection::Alerts => alerts_page(live),
        SettingsSection::Advanced => advanced_page(live),
        #[cfg(target_os = "macos")]
        SettingsSection::Permissions => permissions_card(live),
        SettingsSection::About => about_card(live, body),
    }
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

/// Everything that is not a threshold: the watcher's two knobs, the
/// webhook and the proxy, the collector's cadences, the three tables that
/// update from GitHub, and the reset. The two fields are made once — a
/// card made again on a change would take the caret out of them.
fn advanced_page(live: Live) -> AnyElement {
    let collector = live.memo(|state: &ZStatsAppState| {
        state.settings().map(|file| {
            let c = file.collector.clone().unwrap_or_default();
            (settingspage::interval_rows(&c), c.max_processes.to_string())
        })
    });
    view()
        .gap(px(8.))
        .child(advanced_card(live))
        .child(dynamic(move || match collector.get() {
            Some((rows, max)) => view()
                .gap(px(8.))
                .child(collection_card(rows, max))
                .child(template_card(live))
                .into_element(),
            None => view().into_element(),
        }))
        .child(table_card(live, Table::Hints))
        .child(table_card(live, Table::Caches))
        .child(reset_card())
        .into_element()
}

/// The `app.toml` settings that answer a question few people arrive with.
fn advanced_card(live: Live) -> AnyElement {
    let bar = live.memo(|state: &ZStatsAppState| format!("{:.0}", state.sustained_bar_percent()));
    widgets::list_shell()
        .child(widgets::list_header(
            widgets::list_title(i18n::tr("config.advanced")).into_element(),
            Some(file_note("app.toml")),
        ))
        .child(pref_row(
            live,
            i18n::tr("config.sustained_after"),
            Some(i18n::tr("config.sustained_after_tip")),
            settingspage::sustained_after_choices(),
            || (prefs::sustained_after().as_secs() / 60) as u16,
            |minutes: u16| {
                // The default chip stores nothing, so the key leaves the
                // file rather than pinning today's default into it.
                let default = (watch::DEFAULT_SUSTAINED_AFTER.as_secs() / 60) as u16;
                prefs::set_sustained_after((minutes != default).then_some(minutes));
                changed();
            },
        ))
        .child(dynamic(move || {
            let _ = bar.get();
            pref_row(
                live,
                i18n::tr("config.sustained_divisor"),
                Some(store::read(|state| {
                    settingspage::sustained_divisor_tip(state.sustained_bar_percent())
                })),
                settingspage::sustained_divisor_choices(),
                prefs::sustained_divisor,
                |divisor: u8| {
                    prefs::set_sustained_divisor(
                        (divisor != prefs::DEFAULT_SUSTAINED_DIVISOR).then_some(divisor),
                    );
                    changed();
                },
            )
        }))
        .child(webhook_row(live))
        .child(proxy_row())
        .into_element()
}

/// A free-text setting's field (`Input::xsmall`): 20 tall, its words at
/// 12, the outline the page's chips wear.
fn field(placeholder: String, initial: String, change: impl Fn(&str) + 'static) -> AnyElement {
    input()
        .h(px(20.))
        .px(px(4.))
        .rounded(px(6.))
        .border(px(1.))
        .border_color(theme::color(palette::border))
        .value(initial)
        .placeholder(placeholder)
        .auto_correct(false)
        .text_size(px(12.))
        .text_color(theme::color(palette::text))
        .on_change(move |text: &str| change(text))
        .into_element()
}

/// A line under a field: why it is refused, or what its test answered.
fn field_note(
    words: impl Fn() -> String + 'static,
    alarm: impl Fn() -> bool + Copy + 'static,
) -> Text {
    text(words)
        .mt(px(4.))
        .text_size(px(10.))
        .line_height(widgets::line(10.))
        .text_color(move || {
            theme::color(move || {
                if alarm() {
                    palette::accent_light()
                } else {
                    palette::text_dim()
                }
            })
        })
}

/// Where notifications are also POSTed, with a button that sends one
/// marked test and says what the endpoint answered.
fn webhook_row(live: Live) -> AnyElement {
    let valid = signal(webhook::valid(&prefs::webhook()));
    let test = live.memo(|state: &ZStatsAppState| {
        (
            settingspage::webhook_verdict(state.webhook_test()),
            matches!(state.webhook_test(), Some(WebhookTest::Sending)),
        )
    });
    let can_test = move || {
        let (_, sending) = test.get();
        valid.get() && !sending && !prefs::webhook().is_empty()
    };
    view()
        .px(px(ROW_PAD_X))
        .py(px(8.))
        .border_b(px(1.))
        .border_color(theme::color(palette::border_subtle))
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            text(i18n::tr("config.webhook"))
                                .text_size(px(11.))
                                .line_height(widgets::line(11.))
                                .text_color(theme::color(palette::ink)),
                        )
                        .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
                            "config.webhook_tip",
                        )))),
                )
                .child(
                    nsh_base::button()
                        .flex_none()
                        .rounded_full()
                        .border(px(1.))
                        .border_color(theme::color(palette::border))
                        .bg(theme::color(palette::inset))
                        .px(px(10.))
                        .py(px(3.))
                        .hover(|s| s.bg(theme::color(palette::surface_raised)))
                        .on_press(move || {
                            if can_test() {
                                store::update(|state, fx| state.test_webhook(fx));
                            }
                        })
                        .child(
                            text(i18n::tr("config.webhook_test"))
                                .text_size(px(11.))
                                .line_height(widgets::line(11.))
                                .text_color(move || {
                                    theme::color(move || {
                                        if can_test() {
                                            palette::text()
                                        } else {
                                            palette::text_faint()
                                        }
                                    })
                                }),
                        ),
                ),
        )
        .child(view().mt(px(6.)).child(field(
            i18n::tr("config.webhook_placeholder"),
            prefs::webhook(),
            move |text| {
                let ok = webhook::valid(text);
                valid.set(ok);
                if ok {
                    // Read at each send: the next notification goes to the
                    // new address, no restart.
                    prefs::set_webhook(text);
                }
                store::update(|state, fx| state.forget_webhook_test(fx));
            },
        )))
        .child(when(
            move || !valid.get(),
            || field_note(|| i18n::tr("config.webhook_invalid"), || true),
        ))
        .child(when(
            move || valid.get() && test.with(|(verdict, _)| verdict.is_some()),
            move || {
                field_note(
                    move || test.with(|(v, _)| v.as_ref().map(|v| v.0.clone()).unwrap_or_default()),
                    move || test.with(|(v, _)| v.as_ref().is_some_and(|v| v.1)),
                )
            },
        ))
        .into_element()
}

/// The outbound proxy for the app's fetches; only a value the resolver can
/// act on is kept. The card's last row: no divider under it.
fn proxy_row() -> AnyElement {
    let valid = signal(proxy::is_valid_proxy_setting(&prefs::proxy()));
    view()
        .px(px(ROW_PAD_X))
        .py(px(8.))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .child(
                    text(i18n::tr("config.proxy"))
                        .text_size(px(11.))
                        .line_height(widgets::line(11.))
                        .text_color(theme::color(palette::ink)),
                )
                .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
                    "config.proxy_tip",
                )))),
        )
        .child(view().mt(px(6.)).child(field(
            i18n::tr("config.proxy_placeholder"),
            prefs::proxy(),
            move |text| {
                let ok = proxy::is_valid_proxy_setting(text);
                valid.set(ok);
                if ok {
                    // Persists and mirrors into the proxy resolver — the
                    // next fetch picks it up, no restart.
                    prefs::set_proxy(text);
                }
            },
        )))
        .child(when(
            move || !valid.get(),
            || field_note(|| i18n::tr("config.proxy_invalid"), || true),
        ))
        .into_element()
}

/// The collector's cadences and how many processes it keeps — keys
/// `config.toml` bakes into the collector.
fn collection_card(rows: Vec<settingspage::IntervalRow>, max: String) -> AnyElement {
    let rows: Vec<AnyElement> = rows
        .into_iter()
        .map(|row| {
            let chips: Vec<AnyElement> = row
                .choices
                .iter()
                .map(|(label, value)| {
                    let (key, value) = (row.key, *value);
                    setting_chip(label.clone(), *label == row.shown, move || {
                        apply(key, value)
                    })
                })
                .collect();
            view()
                .px(px(ROW_PAD_X))
                .py(px(7.))
                .border_b(px(1.))
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
                                    text(row.label.clone())
                                        .text_size(px(11.))
                                        .line_height(widgets::line(11.))
                                        .text_color(theme::color(palette::ink)),
                                )
                                .child(widgets::info_icon(widgets::wrap_tooltip(row.tip.clone()))),
                        )
                        .child(
                            text(row.shown.clone())
                                .font_family(widgets::MONO)
                                .text_size(px(10.))
                                .line_height(widgets::line(10.))
                                .text_color(theme::color(palette::text)),
                        ),
                )
                .child(view().flex_row().mt(px(6.)).gap(px(4.)).children(chips))
                .into_element()
        })
        .collect();
    let max_chips: Vec<AnyElement> = settingspage::max_processes_choices()
        .into_iter()
        .map(|(label, value)| {
            let on = label == max;
            setting_chip(label, on, move || apply("max-processes", value))
        })
        .collect();
    widgets::list_shell()
        .child(widgets::list_header(
            titled(
                i18n::tr("config.collection"),
                i18n::tr("config.collection_note"),
            ),
            Some(file_note("config.toml")),
        ))
        .children(rows)
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .px(px(ROW_PAD_X))
                .py(px(8.))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            text(i18n::tr("config.max_processes"))
                                .text_size(px(11.))
                                .line_height(widgets::line(11.))
                                .text_color(theme::color(palette::ink)),
                        )
                        .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
                            "config.max_processes_tip",
                        )))),
                )
                .child(view().flex_row().gap(px(4.)).children(max_chips)),
        )
        .into_element()
}

/// The cards' button: the page's chip chrome; inert and dim while what it
/// starts is running.
fn card_chip(label: String, busy: bool, act: impl Fn() + 'static) -> AnyElement {
    nsh_base::button()
        .flex_none()
        .rounded_full()
        .border(px(1.))
        .border_color(theme::color(palette::border))
        .bg(theme::color(palette::inset))
        .px(px(10.))
        .py(px(3.))
        .hover(move |s| {
            if busy {
                s
            } else {
                s.bg(theme::color(palette::surface_raised))
            }
        })
        .on_press(move || {
            if !busy {
                act();
            }
        })
        .child(
            text(label)
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(move || {
                    if busy {
                        palette::text_dim()
                    } else {
                        palette::text()
                    }
                })),
        )
        .into_element()
}

/// A line under a card's buttons.
fn card_line(words: String, pb: f32) -> AnyElement {
    view()
        .px(px(ROW_PAD_X))
        .pb(px(pb))
        .child(widgets::note(words))
        .into_element()
}

/// What the alert table's card shows.
#[derive(Clone, PartialEq)]
struct TemplateCard {
    source: String,
    running: bool,
    has_override: bool,
    broken: Option<String>,
    offer: bool,
    outcome: Option<String>,
}

fn template_of(state: &ZStatsAppState) -> TemplateCard {
    let loaded = alerttpl::info();
    TemplateCard {
        source: settingspage::template_source(&loaded),
        running: matches!(state.template_sync(), Some(TemplateSync::Running)),
        has_override: loaded.source.has_override(),
        broken: settingspage::template_broken(&loaded),
        offer: state.template_nudge(),
        outcome: settingspage::template_outcome(state.template_sync()),
    }
}

/// zstats' per-process threshold table: which one is live, and the two
/// buttons that change that.
fn template_card(live: Live) -> AnyElement {
    let card = live.memo(template_of);
    dynamic(move || {
        let card = card.get();
        let mut buttons = view()
            .flex_row()
            .items_center()
            .justify_end()
            .gap(px(8.))
            .px(px(ROW_PAD_X))
            .py(px(8.))
            .child(card_chip(
                i18n::tr(if card.running {
                    "config.template_updating"
                } else {
                    "config.template_update"
                }),
                card.running,
                || store::update(|state, fx| state.update_alert_template(fx)),
            ));
        if card.has_override {
            buttons = buttons.child(card_chip(
                i18n::tr("config.template_use_builtin"),
                false,
                || store::update(|state, fx| state.use_builtin_alert_template(fx)),
            ));
        }
        let mut shell = widgets::list_shell()
            .child(widgets::list_header(
                titled(i18n::tr("config.template"), settingspage::template_tip()),
                Some(widgets::note(card.source.clone()).into_element()),
            ))
            .child(buttons);
        if let Some(broken) = card.broken.clone() {
            shell = shell.child(card_line(broken, 10.));
        }
        if card.offer {
            shell = shell.child(
                view()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .px(px(ROW_PAD_X))
                    .pb(px(10.))
                    .child(
                        view()
                            .flex_1()
                            .min_w(px(0.))
                            .child(widgets::note(i18n::tr("config.template_offer"))),
                    )
                    .child(card_chip(
                        i18n::tr("config.template_offer_ignore"),
                        false,
                        || store::update(|state, fx| state.ignore_template_offer(fx)),
                    )),
            );
        }
        if let Some(outcome) = card.outcome.clone() {
            shell = shell.child(card_line(outcome, 10.));
        }
        shell.into_element()
    })
    .into_element()
}

/// The two tables the disk-space window reads.
#[derive(Clone, Copy, PartialEq)]
enum Table {
    /// What the analysis says about a directory (`cleanhints`)
    Hints,
    /// Where the Caches chip looks (`cachepreset`)
    Caches,
}

/// A table's card as it stands: where it came from, whether a fetch is
/// running, and what the last one did.
#[derive(Clone, PartialEq)]
struct TableCard {
    source: String,
    running: bool,
    outcome: Option<String>,
}

/// The clean hints' or the cache roots' card: which list is live, a fetch
/// from GitHub, and a reload of the user's file.
fn table_card(live: Live, table: Table) -> AnyElement {
    let card = live.memo(match table {
        Table::Hints => |state: &ZStatsAppState| TableCard {
            source: settingspage::hints_source(),
            running: matches!(state.hints_sync(), Some(HintsSync::Running)),
            outcome: settingspage::hints_outcome(state.hints_sync()),
        },
        Table::Caches => |state: &ZStatsAppState| TableCard {
            source: settingspage::caches_source(),
            running: matches!(state.caches_sync(), Some(CachesSync::Running)),
            outcome: settingspage::caches_outcome(state.caches_sync()),
        },
    });
    let (title, tip) = match table {
        Table::Hints => (i18n::tr("config.hints"), settingspage::hints_tip()),
        Table::Caches => (i18n::tr("config.caches_preset"), settingspage::caches_tip()),
    };
    dynamic(move || {
        let card = card.get();
        let mut shell = widgets::list_shell()
            .child(widgets::list_header(
                titled(title.clone(), tip.clone()),
                Some(widgets::note(card.source.clone()).into_element()),
            ))
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .px(px(ROW_PAD_X))
                    .py(px(8.))
                    .child(card_chip(
                        i18n::tr(if card.running {
                            "config.hints_updating"
                        } else {
                            "config.hints_update"
                        }),
                        card.running,
                        move || match table {
                            Table::Hints => store::update(|state, fx| state.update_cleanhints(fx)),
                            Table::Caches => {
                                store::update(|state, fx| state.update_cachepreset(fx))
                            }
                        },
                    ))
                    .child(card_chip(
                        i18n::tr("config.hints_reload"),
                        false,
                        move || {
                            match table {
                                Table::Hints => cleanhints::reload(),
                                Table::Caches => cachepreset::reload(),
                            }
                            // The counts and tooltips read the table again.
                            changed();
                        },
                    )),
            );
        if let Some(outcome) = card.outcome.clone() {
            shell = shell.child(card_line(outcome, 9.));
        }
        shell.into_element()
    })
    .into_element()
}

/// Writes a default `config.toml` after asking; `app.toml` is untouched.
fn reset_card() -> AnyElement {
    widgets::list_shell()
        .child(widgets::list_header(
            titled(i18n::tr("config.reset"), i18n::tr("config.reset_note")),
            Some(
                nsh_base::button()
                    .flex_none()
                    .h(px(20.))
                    .px(px(8.))
                    .rounded(px(5.))
                    .border(px(1.))
                    .border_color(theme::color(|| palette::accent_wash(45)))
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(theme::color(|| palette::accent_wash(10))))
                    .on_press(|| {
                        widgets::confirm(
                            i18n::tr("config.reset_title"),
                            i18n::tr("config.reset_body"),
                            i18n::tr("config.reset_ok"),
                            || {
                                store::update(|state, fx| {
                                    if let Err(e) = state.reset_settings(fx) {
                                        tracing::error!("reset settings: {e}");
                                    }
                                })
                            },
                        )
                    })
                    .child(
                        text(i18n::tr("config.reset_ok"))
                            .text_size(px(10.))
                            .line_height(widgets::line(10.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::color(palette::accent_light)),
                    )
                    .into_element(),
            ),
        ))
        .into_element()
}

/// Full Disk Access: whether it is granted, and the way to the pane that
/// grants it while it is not. The app never touches the permission.
#[cfg(target_os = "macos")]
fn permissions_card(live: Live) -> AnyElement {
    let granted = computed(move || {
        live.track();
        settingspage::full_disk_access_granted()
    });
    widgets::list_shell()
        .child(widgets::list_header(
            titled(
                i18n::tr("config.perm_fda"),
                i18n::tr("config.perm_fda_note"),
            ),
            Some(
                text(move || {
                    i18n::tr(if granted.get() {
                        "config.perm_granted"
                    } else {
                        "config.perm_missing"
                    })
                })
                .flex_none()
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || {
                    theme::color(move || {
                        if granted.get() {
                            palette::text()
                        } else {
                            palette::ink()
                        }
                    })
                })
                .into_element(),
            ),
        ))
        .child(when(
            move || !granted.get(),
            || {
                view()
                    .flex_row()
                    .px(px(ROW_PAD_X))
                    .pb(px(11.))
                    .child(card_chip(
                        i18n::tr("config.perm_open"),
                        false,
                        opener::full_disk_access_settings,
                    ))
            },
        ))
        .into_element()
}

/// Floor for the About card's height budget (`config::MIN_ABOUT_HEIGHT`):
/// below it the page's own scroll is the honest answer.
const MIN_ABOUT_HEIGHT: f32 = 360.;

/// The About page, fitted to the page on screen: everything above the
/// release notes keeps its height, and the notes take what is left and
/// scroll inside it — a cap, so short notes keep their own height.
fn about_card(live: Live, body: ReadSignal<f32>) -> AnyElement {
    let rows = settingspage::about_rows();
    let total = rows.len();
    widgets::list_shell()
        .max_h(move || px((body.get() - BODY_PAD * 2.).max(MIN_ABOUT_HEIGHT)))
        .child(identity())
        .children(
            rows.into_iter()
                .enumerate()
                .map(|(i, (label, value))| about_row(label, value, i + 1 == total))
                .collect::<Vec<_>>(),
        )
        .child(github_row())
        .child(update_row(live))
        .into_element()
}

/// The app's icon over its name, as a macOS About view leads.
fn identity() -> AnyElement {
    view()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .pt(px(18.))
        .pb(px(12.))
        .border_b(px(1.))
        .border_color(theme::color(palette::border_subtle))
        .child(
            image(SharedString::from(appicon::own_data_uri()))
                .w(px(64.))
                .h(px(64.)),
        )
        .child(
            text(crate::APP_NAME)
                .text_size(px(14.))
                .line_height(widgets::line(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::color(palette::text)),
        )
        .into_element()
}

/// Version, commit, architecture.
fn about_row(label: String, value: String, last: bool) -> AnyElement {
    view()
        .flex_none()
        .flex_row()
        .items_center()
        .justify_between()
        .px(px(ROW_PAD_X))
        .py(px(8.))
        .border_b(px(if last { 0. } else { 1. }))
        .border_color(theme::color(palette::border_subtle))
        .child(
            text(label)
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::ink)),
        )
        .child(
            text(value)
                .font_family(widgets::MONO)
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::text)),
        )
        .into_element()
}

/// The project page — the one control that opens something outside the
/// app, and the one that wears the hand.
fn github_row() -> AnyElement {
    nsh_base::button()
        .flex_none()
        .cursor_pointer()
        .flex_row()
        .items_center()
        .justify_between()
        .px(px(ROW_PAD_X))
        .py(px(8.))
        .border_t(px(1.))
        .border_color(theme::color(palette::border_subtle))
        .hover(|s| s.bg(theme::color(palette::surface_raised)))
        .on_press(|| {
            let _ = opener::open([about::REPO_URL]);
        })
        .child(
            text(i18n::tr("common.github"))
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::ink)),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .child(
                    text(about::REPO_URL.trim_start_matches("https://"))
                        .font_family(widgets::MONO)
                        .text_size(px(11.))
                        .line_height(widgets::line(11.))
                        .text_color(theme::color(palette::text_muted)),
                )
                .child(widgets::icon(
                    CustomIconName::ExternalLink,
                    11.,
                    palette::text_dim,
                )),
        )
        .into_element()
}

/// The update row: the sentence over the buttons, the buttons, and the
/// notes of what an update brings. Three bindings rather than one card
/// made again: a download moves its figures many times a second, and the
/// notes someone is reading must keep their place meanwhile.
fn update_row(live: Live) -> AnyElement {
    let update = live.memo(settingspage::update_view);
    let caption = computed(move || {
        update.with(|u: &UpdateView| u.caption.clone().filter(|_| u.show_caption))
    });
    let action =
        computed(move || update.with(|u: &UpdateView| (u.action.clone(), u.caption.clone())));
    let notes = computed(move || update.with(|u: &UpdateView| u.notes.clone()));
    view()
        .flex_shrink(1.)
        .min_h(px(0.))
        .px(px(ROW_PAD_X))
        .pt(px(10.))
        .pb(px(12.))
        .gap(px(8.))
        .border_t(px(1.))
        .border_color(theme::color(palette::border_subtle))
        .child(when(
            move || caption.with(Option::is_some),
            move || {
                text(move || caption.get().unwrap_or_default())
                    .flex_none()
                    .text_size(px(10.))
                    .line_height(px(13.5))
                    .text_color(theme::color(palette::text_dim))
            },
        ))
        .child(view().flex_none().child(dynamic(move || {
            let (action, caption) = action.get();
            update_actions(action, caption.unwrap_or_default())
        })))
        .child(when(
            move || notes.with(Option::is_some),
            move || notes_box(notes),
        ))
        .into_element()
}

/// The About page's buttons (`config::update_btn`): 28 tall, the accent's
/// wash on the one that moves the update on.
fn update_button(label: String, accent: bool, act: impl Fn() + 'static) -> ButtonElement {
    nsh_base::button()
        .h(px(28.))
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .border(px(1.))
        .border_color(theme::color(move || {
            if accent {
                palette::accent_wash(45)
            } else {
                palette::border()
            }
        }))
        .bg(theme::color(move || {
            if accent {
                palette::accent_wash(10)
            } else {
                palette::inset()
            }
        }))
        .px(px(10.))
        .hover(|s| s.bg(theme::color(palette::surface_raised)))
        .on_press(act)
        .child(
            text(label)
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::text)),
        )
}

/// What the update row offers as it stands; `caption` is the busy
/// button's own label.
fn update_actions(action: UpdateAction, caption: String) -> AnyElement {
    match action {
        UpdateAction::Check { enabled: true } => {
            update_button(i18n::tr("config.update_check"), false, || {
                store::update(|state, fx| state.check_update(fx))
            })
            .w_full()
            .into_element()
        }
        UpdateAction::Check { enabled: false } => {
            inert_button(i18n::tr("config.update_checking"), None)
        }
        UpdateAction::Install { version } => {
            let skip = version.clone();
            view()
                .flex_row()
                .w_full()
                .gap(px(8.))
                .child(
                    update_button(i18n::tr("config.update_download"), true, move || {
                        let version = version.clone();
                        store::update(|state, fx| state.download_update(version, fx));
                    })
                    .flex_1()
                    .min_w(px(0.)),
                )
                .child(
                    // Mutes the gear's dot for this version only.
                    view()
                        .flex_1()
                        .min_w(px(0.))
                        .tooltip(widgets::wrap_tooltip(i18n::tr("config.update_skip_tip")))
                        .child(
                            update_button(i18n::tr("config.update_skip"), false, move || {
                                let version = skip.clone();
                                store::update(|state, fx| state.ignore_update(&version, fx));
                            })
                            .w_full(),
                        ),
                )
                .into_element()
        }
        UpdateAction::Unskip => update_button(i18n::tr("config.update_unskip"), true, || {
            store::update(|state, fx| state.unignore_update(fx))
        })
        .w_full()
        .into_element(),
        UpdateAction::Busy { progress } => inert_button(caption, progress),
        UpdateAction::Restart => update_button(i18n::tr("config.update_restart"), true, || {
            updater::relaunch();
            root::ask(HostAction::Quit);
        })
        .w_full()
        .into_element(),
        UpdateAction::Quit => update_button(i18n::tr("config.update_quit"), true, || {
            root::ask(HostAction::Quit)
        })
        .w_full()
        .into_element(),
    }
}

/// A button that does nothing while what it started runs: the check under
/// way, or the download whose fill is its progress — a wash of the ink
/// growing behind the figures it already carries, none while the server
/// has not said how big the file is (`config::update_progress_btn`).
fn inert_button(label: String, progress: Option<f32>) -> AnyElement {
    let mut button = view()
        .w_full()
        .h(px(28.))
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .border(px(1.))
        .border_color(theme::color(palette::border))
        .bg(theme::color(palette::inset))
        .overflow_hidden()
        .px(px(10.));
    if let Some(fraction) = progress {
        button = button.child(
            view()
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .bottom(px(0.))
                .w(relative(fraction.clamp(0., 1.)))
                .bg(theme::color(|| color::Rgba {
                    a: 0.16,
                    ..palette::ink()
                })),
        );
    }
    button
        .child(
            text(label)
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::text_dim)),
        )
        .into_element()
}

/// The notes of what an update brings, as the release wrote them
/// (markdown), in an inset that scrolls once the card reaches its cap.
fn notes_box(notes: Memo<Option<String>>) -> AnyElement {
    // The notes in the page's sizes and inks rather than the text view's
    // reading typography. The column's own size goes on the element:
    // NSH 0.2.0 never applies a `TextViewParts` style to the root.
    provide_context(
        TextViewParts::new()
            .part(TextViewPart::Heading2, |s| {
                s.text_size(px(14.7)).line_height(px(20.))
            })
            .part(TextViewPart::Heading3, |s| {
                s.text_size(px(12.)).line_height(px(13.))
            })
            .part(TextViewPart::List, |s| s.gap(px(2.5)))
            .part(TextViewPart::ListItem, |s| s.ps(px(15.)))
            .part(TextViewPart::Code, |s| {
                s.font_family(widgets::MONO)
                    .text_size(px(10.))
                    .bg(theme::color(palette::chip))
                    .rounded(px(3.))
            })
            // Its underline 3 under the line, NSH's, falls outside a
            // 15-point line and is cut.
            .part(TextViewPart::Link, |s| {
                s.text_color(theme::color(palette::text))
                    .underline_offset(px(1.5))
            }),
    );
    view()
        .w_full()
        .flex_shrink(1.)
        .min_h(px(0.))
        .gap(px(4.))
        .child(
            text(i18n::tr("config.update_notes"))
                .flex_none()
                .text_size(px(10.))
                .line_height(widgets::line(10.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(palette::ink)),
        )
        .child(
            scroll()
                .w_full()
                .flex_shrink(1.)
                .min_h(px(0.))
                .rounded(px(7.))
                .bg(theme::color(palette::inset))
                .child(
                    view().px(px(10.)).py(px(8.)).child(
                        nsh_base::text_view()
                            .gap(px(8.))
                            .text_size(px(11.))
                            .line_height(px(15.))
                            .text_color(theme::color(palette::text_muted))
                            .markdown(move || notes.get().map(SharedString::from))
                            .on_link_press(|event: &TextViewLinkPressEvent| {
                                // A release's links go to the browser; an
                                // anchor in the notes has nowhere to go.
                                if event.url.starts_with("https://") {
                                    let _ = opener::open([event.url.as_ref()]);
                                }
                            }),
                    ),
                ),
        )
        .into_element()
}
