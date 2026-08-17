//! Config: app preferences (`app.toml`) and live edits of `config.toml`.
//!
//! Collection cadences write through `apply_add` then rebuild the
//! `Monitor` — those fields are baked into `LocalCollector` at
//! construction, so the first sample after a flip legitimately reads `—`.
//! CPU%, memory, per-core bars, temperatures, battery and application
//! trees are always collected and have no switch. Process-disk-io rides
//! the process-table pass (no independent cadence).
//! Alert base values reload in place, same path as the Alerts tab chips.
//!
//! Language and theme stay in `app.toml` (`prefs`): `zstats::settings::save`
//! would drop any extra key in config.toml. Reset writes a default
//! `config.toml` (confirm first) and rebuilds; it does not touch `app.toml`.
//!
//! The settings window has four left-nav pages: Interface (`app.toml`),
//! Config (`config.toml`), Permissions (Full Disk Access status + deep
//! link), and About (version, commit, arch from `crate::about`).

use super::widgets::{self, card};
use crate::about;
use crate::assets;
use crate::autostart;
use crate::cleanhints;
use crate::confirm;
use crate::font;
use crate::format;
use crate::i18n;
use crate::prefs::{self, LanguagePref, ThemePref};
use crate::state::{HintsSync, UpdateStatus, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use crate::updater;
use gpui::Entity;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Div, Hsla, InteractiveElement, IntoElement, ParentElement, Stateful,
    StatefulInteractiveElement, Styled, div, img, px, relative,
};
use gpui_component::input::{Input, InputState};
use gpui_component::switch::Switch;
use gpui_component::text::TextView;
use gpui_component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;
use std::env;
use std::fs;
use std::path::Path;
use std::time::Duration;
use zstats::CollectorConfig;
use zstats::alerts::ActiveThresholds;
use zstats::settings::PressureAlert;

/// Left-nav sections of the settings window.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsSection {
    /// Language and theme — `app.toml`, not shared with the CLI.
    #[default]
    Interface,
    Config,
    /// Full Disk Access: live status plus a deep link into System
    /// Settings. Status only — granting stays a user act there.
    Permissions,
    About,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 4] = [
        SettingsSection::Interface,
        SettingsSection::Config,
        SettingsSection::Permissions,
        SettingsSection::About,
    ];

    pub fn label_key(self) -> &'static str {
        match self {
            SettingsSection::Interface => "config.nav_interface",
            SettingsSection::Config => "config.nav_config",
            SettingsSection::Permissions => "config.nav_permissions",
            SettingsSection::About => "config.nav_about",
        }
    }

    /// The nav row's icon. Settings2 deliberately matches the footer
    /// gear that opens this window — same symbol, same meaning.
    pub fn icon(self) -> Icon {
        match self {
            SettingsSection::Interface => Icon::new(IconName::Palette),
            SettingsSection::Config => Icon::new(IconName::Settings2),
            // gpui-component ships no shield; ours rides CustomIconName.
            SettingsSection::Permissions => assets::CustomIconName::Shield.into(),
            SettingsSection::About => Icon::new(IconName::Info),
        }
    }
}

pub fn render(
    state: &ZStatsAppState,
    section: SettingsSection,
    proxy_input: &Entity<InputState>,
    proxy_valid: bool,
) -> Vec<AnyElement> {
    match section {
        SettingsSection::Interface => vec![interface_card(proxy_input, proxy_valid)],
        SettingsSection::Config => render_config(state),
        SettingsSection::Permissions => vec![permissions_card()],
        SettingsSection::About => vec![about_card(state)],
    }
}

/// Full Disk Access, the one switch that covers every prompt the disk
/// analysis can trigger. Shows live status and deep-links to the pane;
/// the app never touches the permission itself.
fn permissions_card() -> AnyElement {
    let granted = full_disk_access_granted();
    let status = div()
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(if granted {
            theme::text()
        } else {
            Hsla::from(theme::ink()).into()
        })
        .child(i18n::tr(if granted {
            "config.perm_granted"
        } else {
            "config.perm_missing"
        }))
        .into_any_element();
    widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("config.perm_fda"),
            Some(status),
        ))
        .child(
            div()
                .px(px(13.))
                .pb(px(11.))
                .child(widgets::note(i18n::tr("config.perm_fda_note")))
                .when(!granted, |d| {
                    d.child(
                        h_flex().mt(px(8.)).child(
                            div()
                                .id("perm-fda-open")
                                .flex_none()
                                .rounded_full()
                                .border_1()
                                .border_color(theme::border())
                                .bg(theme::inset())
                                .px(px(10.))
                                .py(px(3.))
                                .text_size(px(11.))
                                .text_color(theme::text())
                                .hover(|d| d.bg(theme::surface_raised()))
                                .on_click(|_, _window, _cx| super::disk::open_full_disk_access())
                                .child(i18n::tr("config.perm_open")),
                        ),
                    )
                }),
        )
        .into_any_element()
}

/// The probe: opening the TCC database itself requires Full Disk Access
/// and — unlike probing user data — never shows a prompt. The failed
/// attempt has a side effect we want: macOS registers this app in the
/// Full Disk Access list, so the Settings pane offers a ready-made
/// toggle instead of demanding a manual "+". Re-checked per render
/// while the window is open (one failed open() per tick), so flipping
/// the switch shows up live.
fn full_disk_access_granted() -> bool {
    let Ok(home) = env::var("HOME") else {
        return false;
    };
    fs::File::open(Path::new(&home).join("Library/Application Support/com.apple.TCC/TCC.db"))
        .is_ok()
}

fn render_config(state: &ZStatsAppState) -> Vec<AnyElement> {
    let mut cards = Vec::new();
    match state.settings() {
        None => cards.push(widgets::empty_card(
            i18n::tr("config.unavailable"),
            i18n::tr("config.unavailable_body"),
        )),
        Some(file) => {
            let collector = file.collector.clone().unwrap_or_default();
            cards.push(collection_card(&collector));
            cards.push(thresholds_card(file));
        }
    }
    cards.push(hints_card(state));
    cards.push(reset_card());
    cards
}

/// Shared chrome for the About update actions. Full-width or half-row
/// is the caller's job — the cramped chip-next-to-caption layout is
/// what overflowed once a version was found.
fn update_btn(id: &'static str, label: impl Into<String>, accent: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .border_1()
        .border_color(if accent {
            theme::accent_wash(45)
        } else {
            theme::border()
        })
        .bg(if accent {
            theme::accent_wash(10)
        } else {
            theme::inset()
        })
        .px(px(10.))
        .text_size(px(11.))
        .text_color(theme::text())
        .child(label.into())
}

fn update_caption(text: impl Into<String>) -> AnyElement {
    div()
        .w_full()
        .text_size(px(10.))
        .line_height(relative(1.35))
        .text_color(theme::text_dim())
        .whitespace_normal()
        .child(text.into())
        .into_any_element()
}

fn update_check_btn(enabled: bool) -> AnyElement {
    let btn = update_btn(
        "about-update-check",
        i18n::tr(if enabled {
            "config.update_check"
        } else {
            "config.update_checking"
        }),
        false,
    )
    .w_full()
    .text_color(if enabled {
        theme::text()
    } else {
        theme::text_dim()
    });
    if enabled {
        btn.hover(|d| d.bg(theme::surface_raised()))
            .on_click(|_, _window, cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.check_update(cx));
            })
            .into_any_element()
    } else {
        btn.into_any_element()
    }
}

/// Download (accent) + release page — replace the check button once a
/// newer tag is known, so the two actions share the row instead of
/// squeezing in beside the "new version" caption.
fn update_install_btns(version: String, url: String) -> AnyElement {
    let download = update_btn(
        "about-update-download",
        i18n::tr("config.update_download"),
        true,
    )
    .flex_1()
    .min_w_0()
    .hover(|d| d.bg(theme::surface_raised()))
    .on_click(move |_, _window, cx| {
        let version = version.clone();
        cx.global::<ZStatsGlobalStore>()
            .clone()
            .update(cx, |state, cx| state.download_update(version, cx));
    });
    h_flex()
        .w_full()
        .gap(px(8.))
        .child(download)
        .child(
            update_btn("about-update-go", i18n::tr("config.update_go"), false)
                .flex_1()
                .min_w_0()
                .hover(|d| d.bg(theme::surface_raised()))
                .on_click(move |_, _window, cx| cx.open_url(&url)),
        )
        .into_any_element()
}

fn update_notes_box(notes: &str) -> AnyElement {
    v_flex()
        .w_full()
        .gap(px(4.))
        .child(
            div()
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::ink())
                .child(i18n::tr("config.update_notes")),
        )
        .child(
            div()
                .id("about-update-notes")
                .w_full()
                .max_h(px(160.))
                .overflow_y_scroll()
                .rounded(px(7.))
                .bg(theme::inset())
                .px(px(10.))
                .py(px(8.))
                .text_size(px(11.))
                .line_height(relative(1.4))
                .text_color(theme::text_muted())
                // git-cliff notes are markdown — headings, bullets,
                // commit links. TextView renders them natively (the
                // markdown crate is a hard dep of gpui-component, so
                // this capability was already paid for), and its links
                // open in the browser on click.
                .child(TextView::markdown(
                    "about-update-notes-md",
                    notes.to_string(),
                )),
        )
        .into_any_element()
}

fn nonempty_notes(notes: &str) -> Option<&str> {
    let trimmed = notes.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// "Check for updates" — one query of the latest (non-prerelease)
/// release, a version compare, and a link. The check button is full
/// width; finding a newer tag *replaces* it with download / release-page
/// so the actions never share a row with the caption. Installing stays
/// a user act on the signed DMG.
fn update_row(state: &ZStatsAppState) -> AnyElement {
    enum Action {
        Check { enabled: bool },
        Install { version: String, url: String },
        Busy,
        Quit,
    }

    let (caption, action, notes): (Option<String>, Action, Option<&str>) =
        match state.update_status() {
            None => (None, Action::Check { enabled: true }, None),
            Some(UpdateStatus::Checking) => (None, Action::Check { enabled: false }, None),
            Some(UpdateStatus::Done(updater::UpdateCheck::UpToDate)) => (
                Some(i18n::tr("config.update_latest")),
                Action::Check { enabled: true },
                None,
            ),
            Some(UpdateStatus::Done(updater::UpdateCheck::Failed(e))) => (
                Some(t!("config.update_failed", e = e.as_str()).to_string()),
                Action::Check { enabled: true },
                None,
            ),
            Some(UpdateStatus::Done(updater::UpdateCheck::Newer {
                version,
                url,
                notes,
            })) => (
                Some(t!("config.update_newer", v = version.as_str()).to_string()),
                Action::Install {
                    version: version.clone(),
                    url: url.clone(),
                },
                nonempty_notes(notes),
            ),
            Some(UpdateStatus::Downloading {
                received,
                total,
                notes,
                ..
            }) => (
                Some(if *total > 0 {
                    t!(
                        "config.update_downloading",
                        got = format::memory(*received),
                        total = format::memory(*total)
                    )
                    .to_string()
                } else {
                    t!(
                        "config.update_downloading_plain",
                        got = format::memory(*received)
                    )
                    .to_string()
                }),
                Action::Busy,
                nonempty_notes(notes),
            ),
            Some(UpdateStatus::Installed) => (
                Some(i18n::tr("config.update_installed")),
                Action::Quit,
                None,
            ),
            Some(UpdateStatus::DownloadFailed {
                version,
                error,
                url,
                notes,
            }) => (
                Some(t!("config.update_dl_failed", e = error.as_str()).to_string()),
                Action::Install {
                    version: version.clone(),
                    url: url.clone(),
                },
                nonempty_notes(notes),
            ),
        };

    let actions = match action {
        Action::Check { enabled } => update_check_btn(enabled),
        Action::Install { version, url } => update_install_btns(version, url),
        Action::Busy => update_btn(
            "about-update-progress",
            caption.clone().unwrap_or_default(),
            false,
        )
        .w_full()
        .text_color(theme::text_dim())
        .into_any_element(),
        Action::Quit => update_btn("about-update-quit", i18n::tr("config.update_quit"), true)
            .w_full()
            .hover(|d| d.bg(theme::surface_raised()))
            .on_click(|_, _window, cx| cx.quit())
            .into_any_element(),
    };

    // During download the caption is the button label itself — don't
    // repeat it above.
    let show_caption = !matches!(
        state.update_status(),
        Some(UpdateStatus::Downloading { .. })
    );

    v_flex()
        .w_full()
        .px(px(13.))
        .pt(px(10.))
        .pb(px(12.))
        .gap(px(8.))
        .border_t(px(1.))
        .border_color(theme::border_subtle())
        .when(show_caption, |col| {
            col.children(caption.map(update_caption))
        })
        .child(actions)
        .children(notes.map(update_notes_box))
        .into_any_element()
}

fn about_card(state: &ZStatsAppState) -> AnyElement {
    let rows = [
        (
            i18n::tr("config.about_version"),
            about::version().to_string(),
        ),
        (i18n::tr("config.about_commit"), about::commit().to_string()),
        (i18n::tr("config.about_arch"), about::architecture()),
    ];
    widgets::list_shell()
        // The identity block a macOS About view leads with: the app icon
        // over the app name. The icon is the real bundle artwork (a
        // 256px cut of icons/zstats-1024.png, embedded), so this view
        // can never drift from what the Dock and Finder show.
        .child(
            v_flex()
                .items_center()
                .gap(px(8.))
                .pt(px(18.))
                .pb(px(12.))
                .border_b(px(1.))
                .border_color(theme::border_subtle())
                .child(img("zstats-icon.png").w(px(64.)).h(px(64.)))
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(crate::APP_NAME),
                ),
        )
        .children({
            let total = rows.len();
            rows.into_iter()
                .enumerate()
                .map(move |(i, (label, value))| {
                    h_flex()
                        .items_center()
                        .justify_between()
                        .px(px(13.))
                        .py(px(8.))
                        .when(i + 1 != total, |d| {
                            d.border_b(px(1.)).border_color(theme::border_subtle())
                        })
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme::ink())
                                .child(label),
                        )
                        .child(
                            div()
                                .font_family(font::MONO)
                                .text_size(px(11.))
                                .text_color(theme::text())
                                .child(value),
                        )
                })
        })
        .child(update_row(state))
        .into_any_element()
}

/// Persist one `zstats -add` key. Errors stay in the log — there is no
/// toast surface, and a failed write must not look like it took.
fn apply(key: &'static str, value: impl Into<String>, cx: &mut App) {
    let value = value.into();
    cx.global::<ZStatsGlobalStore>()
        .clone()
        .update(cx, |state, cx| {
            if let Err(e) = state.apply_setting(key, &value, cx) {
                eprintln!("apply {key}={value}: {e}");
            }
        });
}

fn reset_card() -> AnyElement {
    card()
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(i18n::tr("config.reset")),
                )
                .child(
                    div()
                        .id("cfg-reset")
                        .h(px(20.))
                        .px(px(8.))
                        .rounded(px(5.))
                        .border_1()
                        .border_color(theme::accent_wash(45))
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|d| d.bg(theme::accent_wash(10)))
                        .on_click(move |_, window, cx| {
                            confirm::ask(
                                window,
                                cx,
                                i18n::tr("config.reset_title"),
                                i18n::tr("config.reset_body"),
                                i18n::tr("config.reset_ok"),
                                |cx| {
                                    cx.global::<ZStatsGlobalStore>().clone().update(
                                        cx,
                                        |state, cx| {
                                            if let Err(e) = state.reset_settings(cx) {
                                                eprintln!("reset settings: {e}");
                                            }
                                        },
                                    );
                                },
                            );
                        })
                        .child(
                            div()
                                .text_size(px(10.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::accent_light())
                                .child(i18n::tr("config.reset_ok")),
                        ),
                ),
        )
        .child(
            div()
                .mt(px(6.))
                .child(widgets::note(i18n::tr("config.reset_note"))),
        )
        .into_any_element()
}

/// Language and theme. Selection reuses the accent chips of the Alerts
/// threshold editor — in this app a picked value is accent, like a crossed
/// threshold, and everything else stays neutral.
fn interface_card(proxy_input: &Entity<InputState>, proxy_valid: bool) -> AnyElement {
    widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("config.interface"),
            // Mirrors "config.toml" on the collection card: says where
            // these two settings actually live.
            Some(widgets::note("app.toml")),
        ))
        .child(pref_row(
            "pref-language",
            i18n::tr("config.language"),
            vec![
                (i18n::tr("config.follow_system"), LanguagePref::System),
                // Each language in its own name, so it stays findable from
                // inside the other locale. Deliberately not translated.
                ("English".into(), LanguagePref::English),
                ("中文".into(), LanguagePref::Chinese),
            ],
            prefs::language(),
            crate::set_language_pref,
            false,
        ))
        .child(pref_row(
            "pref-theme",
            i18n::tr("config.theme"),
            vec![
                (i18n::tr("config.follow_system"), ThemePref::System),
                (i18n::tr("config.theme_light"), ThemePref::Light),
                (i18n::tr("config.theme_dark"), ThemePref::Dark),
            ],
            prefs::theme(),
            crate::set_theme_pref,
            false,
        ))
        .child(autostart_row())
        .child(proxy_row(proxy_input, proxy_valid))
        .child(opacity_row())
        .into_any_element()
}

/// Outbound proxy for the clean-hints fetch — the app's only network
/// call. Free text because proxies are arbitrary URIs; only values the
/// resolver can act on are persisted, and the inline warning says when
/// the field holds something else. "" follows the environment / system
/// proxy, "none" forces direct.
fn proxy_row(input: &Entity<InputState>, valid: bool) -> AnyElement {
    v_flex()
        .px(px(13.))
        .py(px(8.))
        .border_b(px(1.))
        .border_color(theme::border_subtle())
        .child(
            h_flex()
                .items_center()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::ink())
                        .child(i18n::tr("config.proxy")),
                )
                .child(
                    div()
                        .id("pref-proxy-info")
                        .flex_none()
                        .p(px(1.))
                        .tooltip(widgets::wrap_tooltip(i18n::tr("config.proxy_tip")))
                        .child(
                            Icon::new(IconName::Info)
                                .with_size(Size::Size(px(12.)))
                                .text_color(Hsla::from(theme::text_dim())),
                        ),
                ),
        )
        .child(div().mt(px(6.)).child(Input::new(input).xsmall()))
        .when(!valid, |d| {
            d.child(
                div()
                    .mt(px(4.))
                    .text_size(px(10.))
                    .text_color(Hsla::from(theme::accent_light()))
                    .child(i18n::tr("config.proxy_invalid")),
            )
        })
        .into_any_element()
}

/// Launch at login. A Switch rather than chips: the control follows the
/// data — enums pick from chips, a boolean flips a switch, which is
/// also the System Settings idiom. State is asked live from the OS
/// record each repaint, so a change made in System Settings shows up
/// here by itself.
fn autostart_row() -> AnyElement {
    h_flex()
        .items_center()
        .justify_between()
        .px(px(13.))
        .py(px(8.))
        .border_b(px(1.))
        .border_color(theme::border_subtle())
        .child(
            h_flex()
                .items_center()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::ink())
                        .child(i18n::tr("config.autostart")),
                )
                .child(
                    div()
                        .id("pref-autostart-info")
                        .flex_none()
                        .p(px(1.))
                        .tooltip(widgets::wrap_tooltip(i18n::tr("config.autostart_tip")))
                        .child(
                            Icon::new(IconName::Info)
                                .with_size(Size::Size(px(12.)))
                                .text_color(Hsla::from(theme::text_dim())),
                        ),
                ),
        )
        .child(
            Switch::new("pref-autostart")
                .small()
                .checked(autostart::is_enabled())
                .on_click(|checked, _window, cx| crate::set_autostart_pref(*checked, cx)),
        )
        .into_any_element()
}

/// Panel wash. Chips write `app.toml` only — the painted value is
/// whatever `prefs::load` froze at launch.
fn opacity_row() -> AnyElement {
    let current = prefs::opacity();
    let chips: [(String, Option<f32>); 7] = [
        (i18n::tr("config.opacity_default"), None),
        ("50%".into(), Some(0.50)),
        ("60%".into(), Some(0.60)),
        ("70%".into(), Some(0.70)),
        ("80%".into(), Some(0.80)),
        ("90%".into(), Some(0.90)),
        ("100%".into(), Some(1.00)),
    ];
    v_flex()
        .px(px(13.))
        .py(px(8.))
        .child(
            h_flex()
                .items_center()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::ink())
                        .child(i18n::tr("config.opacity")),
                )
                .child(
                    div()
                        .id("pref-opacity-info")
                        .flex_none()
                        .p(px(1.))
                        .tooltip(widgets::wrap_tooltip(i18n::tr("config.opacity_tip")))
                        .child(
                            Icon::new(IconName::Info)
                                .with_size(Size::Size(px(12.)))
                                .text_color(Hsla::from(theme::text_dim())),
                        ),
                ),
        )
        .child(
            h_flex()
                .mt(px(6.))
                .gap(px(4.))
                .children(chips.into_iter().enumerate().map(|(i, (text, value))| {
                    let on = match (current, value) {
                        (None, None) => true,
                        (Some(a), Some(b)) => (a - b).abs() < 0.001,
                        _ => false,
                    };
                    div()
                        .id(("pref-opacity", i))
                        .flex_none()
                        .rounded_full()
                        .border_1()
                        .border_color(if on {
                            theme::accent_wash(45)
                        } else {
                            theme::border()
                        })
                        .bg(if on {
                            theme::accent_wash(10)
                        } else {
                            theme::inset()
                        })
                        .px(px(8.))
                        .py(px(2.))
                        .text_size(px(10.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(if on {
                            theme::accent_light()
                        } else {
                            theme::text()
                        })
                        .hover(|d| d.bg(theme::surface_raised()))
                        .on_click(move |_, _window, cx| crate::set_opacity_pref(value, cx))
                        .child(text)
                })),
        )
        .into_any_element()
}

/// One "label · option chips" row. `apply` is a plain fn pointer — both
/// handlers just forward to `main`, nothing to capture.
fn pref_row<T: Copy + PartialEq + 'static>(
    id: &'static str,
    label: String,
    options: Vec<(String, T)>,
    current: T,
    apply: fn(T, &mut App),
    last: bool,
) -> AnyElement {
    h_flex()
        .items_center()
        .justify_between()
        .px(px(13.))
        .py(px(8.))
        .when(!last, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme::ink())
                .child(label),
        )
        .child(
            h_flex()
                .gap(px(4.))
                .children(options.into_iter().enumerate().map(|(i, (text, value))| {
                    let on = value == current;
                    div()
                        .id((id, i))
                        .flex_none()
                        .rounded_full()
                        .border_1()
                        .border_color(if on {
                            theme::accent_wash(45)
                        } else {
                            theme::border()
                        })
                        .bg(if on {
                            theme::accent_wash(10)
                        } else {
                            theme::inset()
                        })
                        .px(px(8.))
                        .py(px(2.))
                        .text_size(px(10.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(if on {
                            theme::accent_light()
                        } else {
                            theme::text()
                        })
                        .hover(|d| d.bg(theme::surface_raised()))
                        .on_click(move |_, _window, cx| apply(value, cx))
                        .child(text)
                })),
        )
        .into_any_element()
}

/// Cadences the user can pick. Off-switches for these channels are gone:
/// they stay collected, and a zero in the file means this app's 15s default.
/// The clean-hint rule set: which list is live (built-in vs the user
/// file) and a reload that takes effect immediately — the missing half
/// of the pull-update path: an external tool drops a new
/// ~/.zstats/cleanhints.toml, one click here and the annotations and
/// suggestions follow, no restart.
fn hints_card(state: &ZStatsAppState) -> AnyElement {
    let (from_user, count) = cleanhints::info();
    let source = if from_user {
        t!("config.hints_user", n = count)
    } else {
        t!("config.hints_builtin", n = count)
    };
    widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("config.hints"),
            Some(widgets::note(source.to_string())),
        ))
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .px(px(13.))
                .py(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(widgets::note(i18n::tr("config.hints_note"))),
                )
                .child(hints_update_chip(state))
                .child(
                    div()
                        .id("cfg-hints-reload")
                        .flex_none()
                        .rounded_full()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::inset())
                        .px(px(10.))
                        .py(px(3.))
                        .text_size(px(11.))
                        .text_color(theme::text())
                        .hover(|d| d.bg(theme::surface_raised()))
                        .on_click(|_, _window, cx| {
                            cleanhints::reload();
                            // The counts and tooltips re-read on repaint.
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |_, cx| cx.notify());
                        })
                        .child(i18n::tr("config.hints_reload")),
                ),
        )
        .children(hints_sync_note(state))
        .into_any_element()
}

/// "Update from GitHub" — the app's only network action, user-triggered
/// and one-at-a-time. The chip goes inert while a fetch runs.
fn hints_update_chip(state: &ZStatsAppState) -> AnyElement {
    let running = matches!(state.hints_sync(), Some(HintsSync::Running));
    let chip = div()
        .id("cfg-hints-update")
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(theme::border())
        .bg(theme::inset())
        .px(px(10.))
        .py(px(3.))
        .text_size(px(11.))
        .text_color(if running {
            theme::text_dim()
        } else {
            theme::text()
        })
        .child(i18n::tr(if running {
            "config.hints_updating"
        } else {
            "config.hints_update"
        }));
    if running {
        return chip.into_any_element();
    }
    chip.hover(|d| d.bg(theme::surface_raised()))
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.update_cleanhints(cx));
        })
        .into_any_element()
}

/// The last fetch's outcome, in one honest line under the row.
fn hints_sync_note(state: &ZStatsAppState) -> Option<AnyElement> {
    let Some(HintsSync::Done(outcome)) = state.hints_sync() else {
        return None;
    };
    use crate::cleanhints::RemoteUpdate;
    let text = match outcome {
        RemoteUpdate::Updated(n) => t!("config.hints_updated", n = n).to_string(),
        RemoteUpdate::AlreadyCurrent => i18n::tr("config.hints_current"),
        RemoteUpdate::Invalid => i18n::tr("config.hints_remote_invalid"),
        RemoteUpdate::Failed(e) => t!("config.hints_update_failed", e = e.as_str()).to_string(),
    };
    Some(
        div()
            .px(px(13.))
            .pb(px(9.))
            .child(widgets::note(text))
            .into_any_element(),
    )
}

fn collection_card(c: &CollectorConfig) -> AnyElement {
    use crate::metrics::{
        PANEL_DISK_IO_INTERVAL, PANEL_NETWORK_INTERVAL, PANEL_PROCESS_INTERVAL, panel_interval,
    };

    let process = panel_interval(c.process_refresh_interval, PANEL_PROCESS_INTERVAL);
    let disk_io = panel_interval(c.disk_io_refresh_interval, PANEL_DISK_IO_INTERVAL);
    let network = panel_interval(c.network_refresh_interval, PANEL_NETWORK_INTERVAL);

    widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("config.collection"),
            Some(widgets::note(i18n::tr("config.file"))),
        ))
        .child(interval_row(
            0,
            "cfg-proc",
            "process-interval",
            i18n::tr("config.collect_processes"),
            i18n::tr("config.collect_processes_tip"),
            process,
            &[("5s", "5s"), ("10s", "10s"), ("15s", "15s")],
        ))
        // No row for process disk IO: it rides the process-table pass and
        // has no cadence of its own — a second control writing the same
        // key would just fight this one. The fact lives in the process
        // row's tooltip instead.
        .child(interval_row(
            1,
            "cfg-disk",
            "disk-interval",
            i18n::tr("config.collect_disks"),
            i18n::tr("config.collect_disks_tip"),
            disk_io,
            &[("10s", "10s"), ("15s", "15s"), ("30s", "30s")],
        ))
        .child(interval_row(
            2,
            "cfg-net",
            "network-interval",
            i18n::tr("config.collect_networks"),
            i18n::tr("config.collect_networks_tip"),
            network,
            &[("10s", "10s"), ("15s", "15s"), ("30s", "30s")],
        ))
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .px(px(13.))
                .py(px(8.))
                .child(
                    div()
                        .id("cfg-max-processes-label")
                        .text_size(px(11.))
                        .text_color(theme::ink())
                        .tooltip(widgets::wrap_tooltip(i18n::tr("config.max_processes_tip")))
                        .child(i18n::tr("config.max_processes")),
                )
                .child(setting_chips(
                    "max-processes",
                    "max-processes",
                    &c.max_processes.to_string(),
                    vec![
                        ("20".into(), "20"),
                        ("50".into(), "50"),
                        ("100".into(), "100"),
                        ("200".into(), "200"),
                    ],
                )),
        )
        .child(
            div()
                .px(px(13.))
                .pt(px(8.))
                .pb(px(10.))
                .child(widgets::note(i18n::tr("config.collection_note"))),
        )
        .into_any_element()
}

fn interval_row(
    i: usize,
    id: &'static str,
    key: &'static str,
    label: String,
    tip: String,
    current: Duration,
    chips: &'static [(&'static str, &'static str)],
) -> AnyElement {
    let shown = humanize(current);
    v_flex()
        .px(px(13.))
        .py(px(7.))
        .border_b(px(1.))
        .border_color(theme::border_subtle())
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .id(("cfg-interval-label", i))
                        .text_size(px(11.))
                        .text_color(theme::ink())
                        .tooltip(widgets::wrap_tooltip(tip))
                        .child(label),
                )
                .child(
                    div()
                        .font_family(font::MONO)
                        .text_size(px(10.))
                        .text_color(theme::text())
                        .child(shown.clone()),
                ),
        )
        .child(
            div().mt(px(6.)).child(setting_chips(
                id,
                key,
                &shown,
                chips
                    .iter()
                    .map(|(label, value)| ((*label).to_string(), *value))
                    .collect(),
            )),
        )
        .into_any_element()
}

fn thresholds_card(file: &zstats::settings::FileConfig) -> AnyElement {
    let a = &file.alerts;
    // zstats' own resolution of what runs when a key is unset — "unset"
    // alone would hide the threshold actually in force. Base values are
    // independent of the template layer (templates only refine per-name
    // overrides), so the builtin-template variant is exact and costs no IO.
    let eff = ActiveThresholds::from_config(a);
    let rows: [(&'static str, String, usize); 8] = [
        (
            "alert-cpu",
            fmt_pct_setting(a.cpu, eff.cpu.base().map(f64::from)),
            a.cpu_overrides.len(),
        ),
        (
            "alert-mem",
            // Effective memory shares are fractions; the file speaks percent.
            fmt_pct_setting(a.mem, eff.memory.base().map(|f| f * 100.0)),
            a.mem_overrides.len(),
        ),
        (
            // The absolute half of the memory bar (zstats 0.5): the rule
            // fires on the LOWER of the share above and this ceiling.
            "alert-mem-bytes",
            fmt_bytes_setting(a.mem_bytes, eff.memory_bytes),
            0,
        ),
        (
            "alert-app-cpu",
            fmt_pct_setting(a.app_cpu, eff.app_cpu.base().map(f64::from)),
            a.app_cpu_overrides.len(),
        ),
        (
            "alert-app-mem",
            fmt_pct_setting(a.app_mem, eff.app_memory.base().map(|f| f * 100.0)),
            a.app_mem_overrides.len(),
        ),
        (
            "alert-disk",
            fmt_pct_setting(a.disk, eff.disk.base().map(|f| f64::from(f) * 100.0)),
            a.disk_overrides.len(),
        ),
        (
            "alert-pressure",
            pressure_setting(a.pressure, eff.pressure),
            0,
        ),
        (
            "alert-cooldown",
            match a.cooldown {
                Some(d) => humanize(d),
                None => default_at(humanize(eff.cooldown)),
            },
            0,
        ),
    ];

    widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("config.thresholds"),
            Some(widgets::note(i18n::tr("config.file"))),
        ))
        .children({
            let total = rows.len();
            rows.into_iter()
                .enumerate()
                .map(move |(i, (k, v, overrides))| {
                    v_flex()
                        .px(px(13.))
                        .py(px(7.))
                        .when(i + 1 != total, |d| {
                            d.border_b(px(1.)).border_color(theme::border_subtle())
                        })
                        .child(
                            h_flex()
                                .items_center()
                                .justify_between()
                                .child(div().text_size(px(11.)).text_color(theme::ink()).child(k))
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .when(overrides > 0, |d| {
                                            d.child(
                                                div()
                                                    .rounded_full()
                                                    .border_1()
                                                    .border_color(theme::accent_wash(45))
                                                    .px(px(6.))
                                                    .text_size(px(8.5))
                                                    .font_weight(gpui::FontWeight::MEDIUM)
                                                    .text_color(theme::accent_light())
                                                    .child(
                                                        t!("config.override", count = overrides)
                                                            .to_string(),
                                                    ),
                                            )
                                        })
                                        .child(
                                            div()
                                                .font_family(font::MONO)
                                                .text_size(px(10.))
                                                .text_color(theme::text())
                                                .child(v.clone()),
                                        ),
                                ),
                        )
                        .child(div().mt(px(6.)).child(threshold_chips(k, &v)))
                })
        })
        .child(
            div()
                .px(px(13.))
                .pt(px(9.))
                .pb(px(11.))
                .child(widgets::note(i18n::tr("config.thresholds_note"))),
        )
        .into_any_element()
}

fn threshold_chips(key: &'static str, current: &str) -> AnyElement {
    let off = i18n::tr("alerts.off");
    let chips = match key {
        "alert-cpu" => vec![
            ("20%".into(), "20"),
            ("30%".into(), "30"),
            ("50%".into(), "50"),
            ("80%".into(), "80"),
            (off, "0"),
        ],
        "alert-mem" => vec![
            ("15%".into(), "15"),
            ("25%".into(), "25"),
            ("40%".into(), "40"),
            ("60%".into(), "60"),
            (off, "0"),
        ],
        // `g` = GiB in zstats' size parser; the decimal `gb` would store
        // values the "4 GB" display form (GiB-multiples) can't round-trip.
        "alert-mem-bytes" => vec![
            ("2 GB".into(), "2g"),
            ("4 GB".into(), "4g"),
            ("8 GB".into(), "8g"),
            (off, "0"),
        ],
        "alert-app-cpu" => vec![
            ("200%".into(), "200"),
            ("400%".into(), "400"),
            ("800%".into(), "800"),
            (off, "0"),
        ],
        "alert-app-mem" => vec![
            ("30%".into(), "30"),
            ("40%".into(), "40"),
            ("60%".into(), "60"),
            (off, "0"),
        ],
        "alert-disk" => vec![
            ("80%".into(), "80"),
            ("90%".into(), "90"),
            ("95%".into(), "95"),
            (off, "0"),
        ],
        "alert-pressure" => vec![
            (i18n::tr("alerts.off"), "off"),
            (i18n::tr("alerts.warning"), "warning"),
            (i18n::tr("alerts.critical"), "critical"),
        ],
        "alert-cooldown" => vec![
            ("5m".into(), "5m"),
            ("10m".into(), "10m"),
            ("30m".into(), "30m"),
        ],
        _ => Vec::new(),
    };
    setting_chips(key, key, current, chips)
}

fn setting_chips(
    id: &'static str,
    key: &'static str,
    current: &str,
    chips: Vec<(String, &'static str)>,
) -> AnyElement {
    if chips.is_empty() {
        return div().into_any_element();
    }
    h_flex()
        .gap(px(4.))
        .children(chips.into_iter().enumerate().map(|(i, (label, value))| {
            let on = current == label;
            div()
                .id((id, i))
                .flex_none()
                .rounded_full()
                .border_1()
                .border_color(if on {
                    theme::accent_wash(45)
                } else {
                    theme::border()
                })
                .bg(if on {
                    theme::accent_wash(10)
                } else {
                    theme::inset()
                })
                .px(px(7.))
                .py(px(1.))
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(if on {
                    theme::accent_light()
                } else {
                    theme::text()
                })
                .hover(|d| d.bg(theme::surface_raised()))
                .on_click(move |_, _window, cx| apply(key, value, cx))
                .child(label)
        }))
        .into_any_element()
}

/// The file's value when set; otherwise the effective default, labeled as
/// such, so an untouched key still tells the user what the engine runs.
fn fmt_pct_setting<T: Into<f64> + Copy>(file: Option<T>, effective_pct: Option<f64>) -> String {
    match file {
        None => default_at(match effective_pct {
            Some(v) => format!("{v:.0}%"),
            None => i18n::tr("alerts.off"),
        }),
        Some(v) if v.into() == 0.0 => i18n::tr("alerts.off"),
        Some(v) => format!("{:.0}%", v.into()),
    }
}

/// The memory rule's absolute ceiling. Values are whole GiB in practice
/// (the chips only offer those), so the clean "4 GB" form applies; a
/// hand-edited odd value falls back to the byte formatter.
fn fmt_bytes_setting(file: Option<u64>, effective: Option<u64>) -> String {
    const GIB: u64 = 1 << 30;
    let clean = |v: u64| {
        if v.is_multiple_of(GIB) {
            format!("{} GB", v / GIB)
        } else {
            format::memory(v)
        }
    };
    match file {
        None => default_at(match effective {
            Some(v) => clean(v),
            None => i18n::tr("alerts.off"),
        }),
        Some(0) => i18n::tr("alerts.off"),
        Some(v) => clean(v),
    }
}

/// Same for the pressure tri-state. `effective` uses zstats' encoding:
/// 2 = warning, 4 = critical, `None` = rule off.
fn pressure_setting(file: Option<PressureAlert>, effective: Option<u32>) -> String {
    match file {
        Some(PressureAlert::Off) => i18n::tr("alerts.off"),
        Some(PressureAlert::Warning) => i18n::tr("alerts.warning"),
        Some(PressureAlert::Critical) => i18n::tr("alerts.critical"),
        None => default_at(match effective {
            Some(level) if level >= 4 => i18n::tr("alerts.critical"),
            Some(_) => i18n::tr("alerts.warning"),
            None => i18n::tr("alerts.off"),
        }),
    }
}

fn default_at(value: String) -> String {
    t!("config.default_at", value = value).to_string()
}

/// Durations the way the config file writes them. Shared with the Alerts
/// empty state, which quotes the effective cooldown.
pub(super) fn humanize(d: Duration) -> String {
    let ms = d.as_millis();
    if ms >= 60_000 && ms.is_multiple_of(60_000) {
        format!("{}m", ms / 60_000)
    } else if ms.is_multiple_of(1_000) {
        format!("{}s", ms / 1_000)
    } else {
        format!("{ms}ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_matches_file_form() {
        assert_eq!(humanize(Duration::ZERO), "0s");
        assert_eq!(humanize(Duration::from_secs(2)), "2s");
        assert_eq!(humanize(Duration::from_secs(60)), "1m");
        assert_eq!(humanize(Duration::from_millis(500)), "500ms");
    }
}
