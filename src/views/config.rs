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

use super::widgets;
use crate::about;
use crate::alerttpl;
use crate::assets;
use crate::autostart;
use crate::bigfiles;
use crate::cachepreset;
use crate::cleanhints;
use crate::confirm;
use crate::font;
use crate::format;
use crate::i18n;
use crate::opener;
use crate::prefs::{self, LanguagePref, ThemePref, TrayPref};
use crate::state::{
    CachesSync, HintsSync, TemplateSync, UpdateStatus, ZStatsAppState, ZStatsGlobalStore,
};
use crate::theme;
use crate::updater;
use gpui::Entity;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Div, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    Stateful, StatefulInteractiveElement, Styled, div, img, px, relative,
};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Icon, IconName, Sizable, h_flex, v_flex};
use rust_i18n::t;
use std::collections::BTreeMap;
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
            // gpui-kit ships no shield; ours rides CustomIconName.
            SettingsSection::Permissions => assets::CustomIconName::Shield.into(),
            SettingsSection::About => Icon::new(IconName::Info),
        }
    }
}

/// `body_height` is what the window can show without scrolling. Only
/// About reads it — every other section is short enough that the body's
/// own scroll is the right answer.
pub fn render(
    state: &ZStatsAppState,
    section: SettingsSection,
    body_height: f32,
    proxy_input: &Entity<InputState>,
    proxy_valid: bool,
) -> Vec<AnyElement> {
    match section {
        SettingsSection::Interface => vec![interface_card(state, proxy_input, proxy_valid)],
        SettingsSection::Config => render_config(state),
        SettingsSection::Permissions => vec![permissions_card()],
        SettingsSection::About => vec![about_card(state, body_height)],
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
            titled(
                "perm-fda-info",
                i18n::tr("config.perm_fda"),
                i18n::tr("config.perm_fda_note"),
            ),
            Some(status),
        ))
        .when(!granted, |d| {
            d.child(
                h_flex().px(px(13.)).pb(px(11.)).child(
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
        })
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
            // Right under the counts it explains: the thresholds card
            // says twelve programs are tuned, this one says which.
            cards.extend(overrides_card(state));
            // Directly under the thresholds it sits between: the table
            // is the layer below a hand-written override and above the
            // base rule.
            cards.push(template_card(state));
        }
    }
    cards.push(hints_card(state));
    cards.push(caches_card(state));
    cards.push(reset_card());
    cards
}

/// Shared chrome for the About update actions. Full-width or half-row is
/// the caller's job — the cramped chip-next-to-caption layout is what
/// overflowed once a version was found. The label is the caller's too:
/// the download button paints a progress fill *behind* its text, and a
/// child added by this function could only ever land in front of it.
fn update_btn(id: &'static str, accent: bool) -> Stateful<Div> {
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
}

/// How full the download button's bar is. `None` when the server never
/// sent a length — the label then reads "3.2 MB" with no total, and a
/// bar with no denominator would have to invent one.
fn download_fraction(received: u64, total: u64) -> Option<f32> {
    (total > 0).then(|| received as f32 / total as f32)
}

/// The download button, whose fill *is* the progress bar: the same
/// chrome as every other action, with an accent wash growing left to
/// right behind the "3.2 MB / 6.6 MB" it already carried. A separate
/// bar under the button would have said the same thing twice and cost a
/// row; a button that is 40% filled says it once.
///
/// `overflow_hidden` matters — without it the fill's square corner
/// escapes the button's 7px radius at 100%.
///
/// No `progress` means the server never sent a length. The button then
/// looks like any other disabled one and the label reads "3.2 MB"
/// without a total, which is the honest pair: a bar with no denominator
/// would have to invent one.
fn update_progress_btn(label: String, progress: Option<f32>) -> AnyElement {
    update_btn("about-update-progress", false)
        .w_full()
        .overflow_hidden()
        .children(progress.map(|fraction| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .bottom_0()
                .w(relative(fraction.clamp(0.0, 1.0)))
                // Neutral, not accent: `theme::accent` is reserved for
                // over-threshold states and the primary action, so a
                // red bar would read as "something is wrong" when the
                // only thing happening is a file arriving. This is the
                // same ink every meter fills with below its line,
                // dropped to a wash because it sits behind text rather
                // than standing alone as a 6px bar.
                .bg(Hsla::from(theme::ink()).opacity(0.16))
        }))
        .child(div().text_color(theme::text_dim()).child(label))
        .into_any_element()
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
    let btn = update_btn("about-update-check", false)
        .child(i18n::tr(if enabled {
            "config.update_check"
        } else {
            "config.update_checking"
        }))
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
fn update_install_btns(version: String) -> AnyElement {
    let skip_version = version.clone();
    let download = update_btn("about-update-download", true)
        .child(i18n::tr("config.update_download"))
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
            // Mutes the gear's dot for this tag only — manual checks
            // stay truthful, the next release re-arms the dot.
            update_btn("about-update-skip", false)
                .child(i18n::tr("config.update_skip"))
                .flex_1()
                .min_w_0()
                .tooltip(super::widgets::wrap_tooltip(i18n::tr(
                    "config.update_skip_tip",
                )))
                .hover(|d| d.bg(theme::surface_raised()))
                .on_click(move |_, _window, cx| {
                    let version = skip_version.clone();
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.ignore_update(&version, cx));
                }),
        )
        .into_any_element()
}

fn update_notes_box(notes: &str) -> AnyElement {
    v_flex()
        .w_full()
        .min_h_0()
        .gap(px(4.))
        .child(
            div()
                .flex_none()
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::ink())
                .child(i18n::tr("config.update_notes")),
        )
        .child(
            div()
                .id("about-update-notes")
                .w_full()
                // The page's one flexible part. It keeps its natural
                // height while everything fits, and gives up exactly the
                // overflow once the card hits its cap — so the window
                // shows one screen and the scrollbar that appears is
                // this pane's, not the whole page's. `min_h_0` is what
                // permits the shrink; the fixed siblings above are all
                // `flex_none`, so the height comes from here or nowhere.
                // (It used to be a hard 160px with its own scroll, which
                // put a cramped window onto the notes while the settings
                // window had room to spare below it.)
                .min_h_0()
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
                // markdown crate is a hard dep of gpui-kit, so
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
        Check {
            enabled: bool,
        },
        Install {
            version: String,
        },
        Unskip,
        Busy {
            /// `None` while the server has not said how big the file is —
            /// no bar rather than a made-up one.
            progress: Option<f32>,
        },
        /// The in-place install landed: quit and reopen the bundle.
        Restart,
        /// The fallback landed a drag window instead: quitting is the
        /// most this app can contribute to the manual install.
        Quit,
    }

    let (caption, action, notes): (Option<String>, Action, Option<&str>) =
        match state.update_status() {
            // No check ran this session, but the silent one has a
            // finding on file: surface it ready to act on — the gear's
            // dot led here, so the door must not play dumb and demand a
            // re-check. Notes are not retained by the silent check;
            // entering About triggers a real one that fills them in.
            None => match (state.update_nudge(), state.update_ignored()) {
                (Some(v), _) => (
                    Some(t!("config.update_newer", v = v).to_string()),
                    Action::Install {
                        version: v.to_string(),
                    },
                    None,
                ),
                // Skipped, and it still applies: say so and offer the
                // way back, rather than going blank and leaving the
                // click that got here looking inert.
                (None, Some(v)) => (
                    Some(t!("config.update_ignored", v = v).to_string()),
                    Action::Unskip,
                    None,
                ),
                (None, None) => (None, Action::Check { enabled: true }, None),
            },
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
            Some(UpdateStatus::Done(updater::UpdateCheck::Newer { version, notes })) => (
                Some(t!("config.update_newer", v = version.as_str()).to_string()),
                Action::Install {
                    version: version.clone(),
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
                Action::Busy {
                    progress: download_fraction(*received, *total),
                },
                nonempty_notes(notes),
            ),
            Some(UpdateStatus::Installing { notes }) => (
                Some(i18n::tr("config.update_installing")),
                Action::Busy { progress: None },
                nonempty_notes(notes),
            ),
            Some(UpdateStatus::Installed { manual: false }) => {
                (Some(i18n::tr("config.update_ready")), Action::Restart, None)
            }
            Some(UpdateStatus::Installed { manual: true }) => (
                Some(i18n::tr("config.update_installed")),
                Action::Quit,
                None,
            ),
            Some(UpdateStatus::DownloadFailed {
                version,
                error,
                notes,
            }) => (
                Some(t!("config.update_dl_failed", e = error.as_str()).to_string()),
                Action::Install {
                    version: version.clone(),
                },
                nonempty_notes(notes),
            ),
        };

    let actions = match action {
        Action::Check { enabled } => update_check_btn(enabled),
        Action::Install { version } => update_install_btns(version),
        Action::Unskip => update_btn("about-update-unskip", true)
            .child(i18n::tr("config.update_unskip"))
            .w_full()
            .hover(|d| d.bg(theme::surface_raised()))
            .on_click(|_, _window, cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.unignore_update(cx));
            })
            .into_any_element(),
        Action::Busy { progress } => {
            update_progress_btn(caption.clone().unwrap_or_default(), progress)
        }
        Action::Restart => update_btn("about-update-restart", true)
            .child(i18n::tr("config.update_restart"))
            .w_full()
            .hover(|d| d.bg(theme::surface_raised()))
            .on_click(|_, _window, cx| {
                updater::relaunch();
                cx.quit();
            })
            .into_any_element(),
        Action::Quit => update_btn("about-update-quit", true)
            .child(i18n::tr("config.update_quit"))
            .w_full()
            .hover(|d| d.bg(theme::surface_raised()))
            .on_click(|_, _window, cx| cx.quit())
            .into_any_element(),
    };

    // During download and install the caption is the button label
    // itself — don't repeat it above.
    let show_caption = !matches!(
        state.update_status(),
        Some(UpdateStatus::Downloading { .. }) | Some(UpdateStatus::Installing { .. })
    );

    v_flex()
        .w_full()
        // Shrinkable, and `min_h_0` so it may actually go below its
        // content size — without it the notes box inside can never give
        // up the height the card's cap is asking for.
        .min_h_0()
        .px(px(13.))
        .pt(px(10.))
        .pb(px(12.))
        .gap(px(8.))
        .border_t(px(1.))
        .border_color(theme::border_subtle())
        .when(show_caption, |col| {
            col.children(caption.map(|text| div().flex_none().child(update_caption(text))))
        })
        .child(div().flex_none().child(actions))
        .children(notes.map(update_notes_box))
        .into_any_element()
}

/// Floor for the About card's height budget. Below this the window is
/// smaller than the fixed content, and the body's own scroll is the
/// honest answer — squeezing the notes to nothing would not help.
const MIN_ABOUT_HEIGHT: f32 = 360.;

/// The About page, laid out to fit one screen: everything above the
/// release notes is fixed, and the notes take whatever height is left
/// and scroll inside it. `max_h` rather than `h` on purpose — short
/// notes keep their natural size instead of stretching an empty inset
/// box to the window's bottom edge.
fn about_card(state: &ZStatsAppState, body_height: f32) -> AnyElement {
    let rows = [
        (
            i18n::tr("config.about_version"),
            about::version().to_string(),
        ),
        (i18n::tr("config.about_commit"), about::commit().to_string()),
        (i18n::tr("config.about_arch"), about::architecture()),
    ];
    widgets::list_shell()
        .max_h(px(body_height.max(MIN_ABOUT_HEIGHT)))
        // The identity block a macOS About view leads with: the app icon
        // over the app name. The icon is the real bundle artwork (a
        // 256px cut of icons/zstats-1024.png, embedded), so this view
        // can never drift from what the Dock and Finder show.
        .child(
            v_flex()
                // Fixed: only the notes may give up height.
                .flex_none()
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
                        .flex_none()
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
                tracing::error!("apply {key}={value}: {e}");
            }
        });
}

fn reset_card() -> AnyElement {
    widgets::list_shell()
        .child(widgets::list_header(
            titled(
                "cfg-reset-info",
                i18n::tr("config.reset"),
                i18n::tr("config.reset_note"),
            ),
            Some(
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
                                cx.global::<ZStatsGlobalStore>()
                                    .clone()
                                    .update(cx, |state, cx| {
                                        if let Err(e) = state.reset_settings(cx) {
                                            tracing::error!("reset settings: {e}");
                                        }
                                    });
                            },
                        );
                    })
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::accent_light())
                            .child(i18n::tr("config.reset_ok")),
                    )
                    .into_any_element(),
            ),
        ))
        .into_any_element()
}

/// Card title plus the ⓘ that used to be a standing note underneath.
/// Surface is the name; the sentence is the hover.
fn titled(
    id: &'static str,
    title: impl Into<SharedString>,
    tip: impl Into<SharedString> + 'static,
) -> AnyElement {
    h_flex()
        .min_w_0()
        .items_center()
        .gap(px(4.))
        .child(div().min_w_0().truncate().child(title.into()))
        .child(widgets::info_icon(id, tip))
        .into_any_element()
}

/// A card header's file-name note, clickable: the name reveals the file
/// in Finder. These names answer "where does this setting actually
/// live", and the natural follow-up — opening that place — used to
/// mean typing the path yourself. Hover lift is the affordance (the
/// arrow stays, per the rule in views/mod.rs); the tooltip says where
/// it goes. A file that does not exist yet (config.toml on a machine
/// still on builtin defaults) reveals nothing, so the folder opens
/// instead — the answer to "where" is still given.
fn file_note(id: &'static str, name: &'static str) -> AnyElement {
    div()
        .id(id)
        .rounded(px(4.))
        .px(px(4.))
        .mx(px(-4.))
        .text_size(px(10.))
        .text_color(theme::text_dim())
        .hover(|d| d.bg(theme::surface_raised()).text_color(theme::text()))
        .tooltip(widgets::wrap_tooltip(i18n::tr("config.file_reveal_tip")))
        .on_click(move |_, _window, cx| {
            cx.stop_propagation();
            let path = zstats::settings::default_dir().join(name);
            if path.is_file() {
                bigfiles::reveal(&path);
            } else if let Err(e) = opener::open([path.parent().unwrap_or(&path).as_os_str()]) {
                tracing::warn!("open config dir: {e}");
            }
        })
        .child(name)
        .into_any_element()
}

/// Language and theme. Selection reuses the accent chips of the Alerts
/// threshold editor — in this app a picked value is accent, like a crossed
/// threshold, and everything else stays neutral.
fn interface_card(
    state: &ZStatsAppState,
    proxy_input: &Entity<InputState>,
    proxy_valid: bool,
) -> AnyElement {
    widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("config.interface"),
            // Mirrors "config.toml" on the collection card: says where
            // these two settings actually live.
            Some(file_note("file-note-app", "app.toml")),
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
            None,
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
            None,
        ))
        .child(pref_row(
            "pref-tray",
            i18n::tr("config.tray"),
            vec![
                (i18n::tr("config.tray_auto"), TrayPref::Auto),
                ("CPU".into(), TrayPref::Cpu),
                (i18n::tr("config.tray_memory"), TrayPref::Memory),
                (i18n::tr("config.tray_both"), TrayPref::Both),
            ],
            prefs::tray(),
            crate::set_tray_pref,
            Some(i18n::tr("config.tray_tip")),
        ))
        // The sustained-load watcher's two knobs. Minutes and a divisor
        // rather than a percent, so the bar keeps following `alert-cpu`
        // (see `prefs::SUSTAINED_DIVISOR`). The tip quotes the bar in
        // force so the divisor reads as a number, not a fraction.
        .child(pref_row(
            "pref-sustained-after",
            i18n::tr("config.sustained_after"),
            vec![
                ("1h".into(), 60u16),
                ("2h".into(), 120),
                ("4h".into(), 240),
                ("8h".into(), 480),
            ],
            (prefs::sustained_after().as_secs() / 60) as u16,
            crate::set_sustained_after_pref,
            Some(i18n::tr("config.sustained_after_tip")),
        ))
        .child(pref_row(
            "pref-sustained-divisor",
            i18n::tr("config.sustained_divisor"),
            vec![
                ("÷2".into(), 2u8),
                ("÷3".into(), 3),
                ("÷4".into(), 4),
                ("÷6".into(), 6),
            ],
            prefs::sustained_divisor(),
            crate::set_sustained_divisor_pref,
            Some(
                t!(
                    "config.sustained_divisor_tip",
                    divisor = prefs::sustained_divisor(),
                    bar = format!("{:.0}%", state.sustained_bar_percent())
                )
                .to_string(),
            ),
        ))
        .child(notifications_row())
        .child(autostart_row())
        .child(proxy_row(proxy_input, proxy_valid))
        .child(opacity_row())
        .into_any_element()
}

/// The notifications master switch — banners only. Everything else
/// keeps running: rules evaluate, the Alerts list and the daily record
/// fill, the log carries each banner's verdict ("muted"), and the
/// Alerts tab's Watching block says so — a banner that quietly never
/// arrives must never read as a rule that stopped firing.
fn notifications_row() -> AnyElement {
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
                        .child(i18n::tr("config.notifications")),
                )
                .child(widgets::info_icon(
                    "pref-notifications-info",
                    i18n::tr("config.notifications_tip"),
                )),
        )
        .child(
            Switch::new("pref-notifications")
                .small()
                .checked(prefs::notifications())
                .on_click(|checked, _window, cx| crate::set_notifications_pref(*checked, cx)),
        )
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
                .child(widgets::info_icon(
                    "pref-proxy-info",
                    i18n::tr("config.proxy_tip"),
                )),
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
                .child(widgets::info_icon(
                    "pref-autostart-info",
                    i18n::tr("config.autostart_tip"),
                )),
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
                .child(widgets::info_icon(
                    "pref-opacity-info",
                    i18n::tr("config.opacity_tip"),
                )),
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

/// One "label · option chips" row. `apply` is a plain fn pointer — the
/// handlers just forward to `main`, nothing to capture. `tip`, when
/// given, hangs the same ⓘ the switch rows carry off the label — for a
/// row whose options are not self-explanatory (what "Auto" means).
fn pref_row<T: Copy + PartialEq + 'static>(
    id: &'static str,
    label: String,
    options: Vec<(String, T)>,
    current: T,
    apply: fn(T, &mut App),
    tip: Option<String>,
) -> AnyElement {
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
                        .child(label),
                )
                .children(
                    tip.map(|tip| {
                        widgets::info_icon(SharedString::from(format!("{id}-info")), tip)
                    }),
                ),
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

/// zstats' per-process threshold table: which one is live, and the two
/// buttons that change that.
///
/// Sits under the thresholds it modifies, because that is what it is —
/// the layer between a base rule and a hand-written override, raising
/// (or zeroing) the bar for programs that are busy or large by design.
/// Editing it is the same act as the Alerts tab's own writes: bytes into
/// zstats' config, with zstats still doing every evaluation.
///
/// The source line leads because the table is invisible otherwise. A
/// refused override especially: zstats then applies no `[alerts]` change
/// at all, and without a line saying so the Alerts tab would look like
/// it had simply stopped working.
fn template_card(state: &ZStatsAppState) -> AnyElement {
    let loaded = alerttpl::info();
    let line = match &loaded.source {
        alerttpl::Source::Builtin(n) => t!("config.template_builtin", n = n).to_string(),
        alerttpl::Source::User(n) => t!("config.template_user", n = n).to_string(),
        alerttpl::Source::Broken(_) => i18n::tr("config.template_broken"),
    };
    widgets::list_shell()
        .child(widgets::list_header(
            titled(
                "cfg-template-info",
                i18n::tr("config.template"),
                // Names the platform file it pulls, because the local
                // name cannot: zstats fixes the override at
                // `template.toml` with no platform in it, and ~/.zstats
                // may be synced between machines whose process names
                // have nothing in common.
                t!("config.template_note", file = alerttpl::FILE).to_string(),
            ),
            Some(widgets::note(line)),
        ))
        .child(
            h_flex()
                .items_center()
                .justify_end()
                .gap(px(8.))
                .px(px(13.))
                .py(px(8.))
                .child(template_update_chip(state))
                .children(loaded.source.has_override().then(template_builtin_chip)),
        )
        .children(match &loaded.source {
            alerttpl::Source::Broken(e) => Some(template_line(
                t!("config.template_broken_body", e = e.as_str()).to_string(),
            )),
            _ => None,
        })
        // The gear dot's landing spot: the probe found a clean,
        // different table. The line names the news; the Update chip
        // above is the action; "not this one" is display-layer only,
        // like the updater's skip.
        .children(state.template_nudge().then(|| {
            h_flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .px(px(13.))
                .pb(px(10.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(widgets::note(i18n::tr("config.template_offer"))),
                )
                .child(
                    template_chip("cfg-template-ignore")
                        .text_color(theme::text())
                        .hover(|d| d.bg(theme::surface_raised()))
                        .on_click(|_, _window, cx| {
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| state.ignore_template_offer(cx));
                        })
                        .child(i18n::tr("config.template_offer_ignore")),
                )
                .into_any_element()
        }))
        .children(template_sync_note(state))
        .into_any_element()
}

/// "Update" — fetches the published table, validates, writes, reloads.
/// Inert while a fetch runs, same as the clean-hints chip.
fn template_update_chip(state: &ZStatsAppState) -> AnyElement {
    let running = matches!(state.template_sync(), Some(TemplateSync::Running));
    let chip = template_chip("cfg-template-update")
        .text_color(if running {
            theme::text_dim()
        } else {
            theme::text()
        })
        .child(i18n::tr(if running {
            "config.template_updating"
        } else {
            "config.template_update"
        }));
    if running {
        return chip.into_any_element();
    }
    chip.hover(|d| d.bg(theme::surface_raised()))
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.update_alert_template(cx));
        })
        .into_any_element()
}

/// The way back. Rendered only when an override exists, so it is never
/// a button that can do nothing — and it matters more here than for the
/// clean hints: an override replaces the table wholesale, and a refused
/// one stops every `[alerts]` change from applying.
fn template_builtin_chip() -> AnyElement {
    template_chip("cfg-template-builtin")
        .text_color(theme::text())
        .hover(|d| d.bg(theme::surface_raised()))
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.use_builtin_alert_template(cx));
        })
        .child(i18n::tr("config.template_use_builtin"))
        .into_any_element()
}

/// Shared chrome for the two chips — same shape as the clean hints'.
fn template_chip(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(theme::border())
        .bg(theme::inset())
        .px(px(10.))
        .py(px(3.))
        .text_size(px(11.))
}

/// The last press's outcome, in one honest line under the row.
fn template_sync_note(state: &ZStatsAppState) -> Option<AnyElement> {
    use crate::alerttpl::RemoteUpdate;
    let text = match state.template_sync()? {
        TemplateSync::Running => return None,
        TemplateSync::Done(RemoteUpdate::Updated(n)) => {
            t!("config.template_updated", n = n).to_string()
        }
        TemplateSync::Done(RemoteUpdate::AlreadyCurrent) => i18n::tr("config.template_current"),
        // Named as its own outcome, never folded into a failure: a
        // format the local zstats cannot read is answered by updating
        // the app, and "download failed" would send someone to check
        // their network instead.
        TemplateSync::Done(RemoteUpdate::VersionMismatch { found, expected }) => t!(
            "config.template_version",
            found = found.map_or_else(|| "—".to_string(), |v| v.to_string()),
            expected = expected
        )
        .to_string(),
        TemplateSync::Done(RemoteUpdate::Invalid(e)) => {
            t!("config.template_invalid", e = e.as_str()).to_string()
        }
        TemplateSync::Done(RemoteUpdate::Failed(e)) => {
            t!("config.template_failed", e = e.as_str()).to_string()
        }
        TemplateSync::Reverted => i18n::tr("config.template_reverted"),
        TemplateSync::NothingToRevert => i18n::tr("config.template_nothing"),
        TemplateSync::RevertFailed(e) => {
            t!("config.template_revert_failed", e = e.as_str()).to_string()
        }
    };
    Some(template_line(text))
}

fn template_line(text: String) -> AnyElement {
    div()
        .px(px(13.))
        .pb(px(10.))
        .child(widgets::note(text))
        .into_any_element()
}

/// Cadences the user can pick. Off-switches for these channels are gone:
/// they stay collected, and a zero in the file means this app's 15s default.
/// The clean-hint rule set: which list is live (built-in vs the user
/// file) and a reload that takes effect immediately — the missing half
/// of the pull-update path: an external tool drops a new
/// ~/.zstats/&lt;cleanhints::FILE&gt;, one click here and the annotations and
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
            titled(
                "cfg-hints-info",
                i18n::tr("config.hints"),
                t!("config.hints_note", file = cleanhints::FILE).to_string(),
            ),
            Some(widgets::note(source.to_string())),
        ))
        .child(
            h_flex()
                .items_center()
                .justify_end()
                .gap(px(8.))
                .px(px(13.))
                .py(px(8.))
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

/// Walk roots for the Caches chip — same override/fetch shape as the
/// cleanhints card above it, a different file so a pulled annotation
/// cannot change what gets scanned.
fn caches_card(state: &ZStatsAppState) -> AnyElement {
    let (from_user, count) = cachepreset::info();
    let source = if from_user {
        t!("config.caches_user", n = count)
    } else {
        t!("config.caches_builtin", n = count)
    };
    widgets::list_shell()
        .child(widgets::list_header(
            titled(
                "cfg-caches-info",
                i18n::tr("config.caches_preset"),
                t!("config.caches_preset_note", file = cachepreset::FILE).to_string(),
            ),
            Some(widgets::note(source.to_string())),
        ))
        .child(
            h_flex()
                .items_center()
                .justify_end()
                .gap(px(8.))
                .px(px(13.))
                .py(px(8.))
                .child(caches_update_chip(state))
                .child(
                    div()
                        .id("cfg-caches-reload")
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
                            cachepreset::reload();
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |_, cx| cx.notify());
                        })
                        .child(i18n::tr("config.hints_reload")),
                ),
        )
        .children(caches_sync_note(state))
        .into_any_element()
}

fn caches_update_chip(state: &ZStatsAppState) -> AnyElement {
    let running = matches!(state.caches_sync(), Some(CachesSync::Running));
    let chip = div()
        .id("cfg-caches-update")
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
                .update(cx, |state, cx| state.update_cachepreset(cx));
        })
        .into_any_element()
}

fn caches_sync_note(state: &ZStatsAppState) -> Option<AnyElement> {
    let Some(CachesSync::Done(outcome)) = state.caches_sync() else {
        return None;
    };
    use crate::cachepreset::RemoteUpdate;
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
            titled(
                "cfg-collection-info",
                i18n::tr("config.collection"),
                i18n::tr("config.collection_note"),
            ),
            Some(file_note("file-note-collection", "config.toml")),
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
        // row's ⓘ instead.
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
                    h_flex()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme::ink())
                                .child(i18n::tr("config.max_processes")),
                        )
                        .child(widgets::info_icon(
                            "cfg-max-processes-info",
                            i18n::tr("config.max_processes_tip"),
                        )),
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
                    h_flex()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme::ink())
                                .child(label),
                        )
                        .child(widgets::info_icon(("cfg-interval-info", i), tip)),
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

/// The human name for a threshold key. The key itself stays on the row
/// beneath it: these are the exact strings `config.toml` and the CLI's
/// `-add` take, and a card that only showed a translated name would
/// leave a reader who wants to change one with nothing to type.
fn threshold_label(key: &str) -> String {
    i18n::tr(match key {
        "alert-cpu" => "config.th_cpu",
        "alert-mem" => "config.th_mem",
        "alert-mem-bytes" => "config.th_mem_bytes",
        "alert-app-cpu" => "config.th_app_cpu",
        "alert-app-mem" => "config.th_app_mem",
        "alert-disk" => "config.th_disk",
        "alert-pressure" => "config.th_pressure",
        "alert-cooldown" => "config.th_cooldown",
        // Unreachable for the eight rows above; a future key shows its
        // own name rather than a blank until it is given one.
        other => return other.to_string(),
    })
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
            titled(
                "cfg-thresholds-info",
                i18n::tr("config.thresholds"),
                i18n::tr("config.thresholds_note"),
            ),
            Some(file_note("file-note-thresholds", "config.toml")),
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
                                .child(
                                    v_flex()
                                        .child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(theme::ink())
                                                .child(threshold_label(k)),
                                        )
                                        // The key stays, one size down:
                                        // it is what you type into
                                        // config.toml or `zstats -add`,
                                        // and this card is where you
                                        // would go looking for it.
                                        .child(
                                            div()
                                                .font_family(font::MONO)
                                                .text_size(px(9.))
                                                .text_color(theme::text_dim())
                                                .child(k),
                                        ),
                                )
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
        .into_any_element()
}

/// Every per-subject override, in one place, each with a way out.
///
/// The thresholds card counts them ("alert-cpu · 12 overrides") and the
/// Alerts tab writes them one subject at a time, which between them
/// left the set unreadable: after a few months nobody remembers which
/// twelve programs they tuned, what to, or how to undo one. The only
/// answer was to open config.toml — the file this app exists to keep
/// people out of. Personalised monitoring that cannot be reviewed
/// stops being personalisation and becomes a liability.
///
/// Grouped by rule rather than by subject: the rule is what the value
/// means (200% of one core is unremarkable for an app tree and absurd
/// for a process), and a subject usually appears under one rule only.
/// Sorted by name inside each group, which is `BTreeMap`'s own order —
/// stable across renders, so a row does not move under the pointer
/// between the read and the click.
/// Every override in the file, flattened to (rule key, subject, value)
/// and ordered by rule then name. Pure so the flattening — five maps,
/// two numeric widths, one row shape — can be tested without a config
/// file or a frame.
fn override_rows(a: &zstats::settings::AlertsConfig) -> Vec<(&'static str, String, f64)> {
    // Percent for every one of them — zstats parses `name=30` into
    // these maps as 30, and `0` is that subject's "never alert". Two
    // widths upstream (f32 for the CPU-ish rules, f64 for the memory
    // ones); both widen to f64 here rather than the row carrying which.
    let wide = |m: &BTreeMap<String, f32>| -> Vec<(String, f64)> {
        m.iter().map(|(n, v)| (n.clone(), f64::from(*v))).collect()
    };
    let exact = |m: &BTreeMap<String, f64>| -> Vec<(String, f64)> {
        m.iter().map(|(n, v)| (n.clone(), *v)).collect()
    };
    [
        ("alert-cpu", wide(&a.cpu_overrides)),
        ("alert-mem", exact(&a.mem_overrides)),
        ("alert-app-cpu", wide(&a.app_cpu_overrides)),
        ("alert-app-mem", exact(&a.app_mem_overrides)),
        ("alert-disk", wide(&a.disk_overrides)),
    ]
    .into_iter()
    .flat_map(|(key, entries)| entries.into_iter().map(move |(name, v)| (key, name, v)))
    .collect()
}

fn overrides_card(state: &ZStatsAppState) -> Option<AnyElement> {
    let file = state.settings()?;
    let rows = override_rows(&file.alerts);
    // Nothing set is not an empty state worth a card: the thresholds
    // above already say every rule is on its base value.
    if rows.is_empty() {
        return None;
    }
    let total = rows.len();
    Some(
        widgets::list_shell()
            .child(widgets::list_header(
                titled(
                    "cfg-overrides-info",
                    i18n::tr("config.overrides"),
                    i18n::tr("config.overrides_note"),
                ),
                Some(widgets::note(
                    t!("config.override", count = total).to_string(),
                )),
            ))
            .children(
                rows.into_iter()
                    .enumerate()
                    .map(|(i, (key, name, value))| override_row(i, total, key, name, value)),
            )
            .into_any_element(),
    )
}

/// One override: who it is for, which rule, what it was set to, and the
/// control that drops it.
///
/// "Reset" rather than a trash icon: nothing is deleted in the sense
/// the Trash button on a file row means — the subject goes back to the
/// rule everyone else is on, and re-setting it is two clicks away in
/// the Alerts tab. And no confirmation sheet: the two gated actions in
/// this app move real things (a file to the Trash, a signal to a
/// process), while this one edits a line in a config file that the row
/// itself is showing you.
fn override_row(i: usize, total: usize, key: &'static str, name: String, value: f64) -> AnyElement {
    let removal_name = name.clone();
    let subject: SharedString = name.into();
    h_flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .px(px(13.))
        .py(px(7.))
        .when(i + 1 != total, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(theme::ink())
                        .child(subject),
                )
                // The rule's own key, in the spelling `zstats -add` and
                // config.toml use — same reasoning as the thresholds
                // card above, and what makes this row copyable into a
                // terminal.
                .child(
                    div()
                        .font_family(font::MONO)
                        .text_size(px(9.))
                        .text_color(theme::text_dim())
                        .child(key),
                ),
        )
        .child(
            h_flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .font_family(font::MONO)
                        .text_size(px(10.))
                        .text_color(theme::text())
                        // `0` is not "no value" — it is this subject
                        // opting out of the rule, and reads as such.
                        .child(if value == 0.0 {
                            i18n::tr("alerts.off")
                        } else {
                            format!("{value:.0}%")
                        }),
                )
                .child(
                    div()
                        .id(("override-reset", i))
                        .flex_none()
                        .rounded_full()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::inset())
                        .px(px(7.))
                        .py(px(1.))
                        .text_size(px(10.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::text())
                        .hover(|d| d.bg(theme::surface_raised()))
                        .tooltip(widgets::wrap_tooltip(i18n::tr("config.override_reset_tip")))
                        .on_click(move |_, _window, cx| {
                            let name = removal_name.clone();
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| {
                                    if let Err(e) = state.remove_alert_override(key, &name, cx) {
                                        tracing::warn!("remove {key} override for {name}: {e}");
                                    }
                                });
                        })
                        .child(i18n::tr("config.override_reset")),
                ),
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
            (i18n::tr("alerts.warning"), "warning"),
            (i18n::tr("alerts.critical"), "critical"),
            (i18n::tr("alerts.off"), "off"),
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

    /// The bar is the download's only quantitative claim, so it has to
    /// come from the same two numbers the label already shows — and a
    /// server that sent no length must produce no bar rather than a
    /// guess.
    #[test]
    fn download_fraction_tracks_the_bytes_and_declines_to_guess() {
        assert_eq!(download_fraction(0, 6_600_000), Some(0.0));
        assert_eq!(download_fraction(3_300_000, 6_600_000), Some(0.5));
        assert_eq!(download_fraction(6_600_000, 6_600_000), Some(1.0));
        assert_eq!(download_fraction(1_000, 0), None, "no length, no bar");
    }

    /// Five maps, two numeric widths, one row shape — and the order is
    /// what the reader scans: rule first (200% means one thing for an
    /// app tree and something absurd for a process), name inside it.
    #[test]
    fn override_rows_flatten_every_map_in_a_readable_order() {
        let mut a = zstats::settings::AlertsConfig::default();
        a.cpu_overrides.insert("node".into(), 70.0);
        a.cpu_overrides.insert("Google Chrome".into(), 45.0);
        a.mem_overrides.insert("Xcode".into(), 25.0);
        a.app_cpu_overrides.insert("zed".into(), 600.0);
        a.app_mem_overrides.insert("Slack".into(), 12.0);
        a.disk_overrides.insert("/".into(), 95.0);
        // A base value is not an override and must not appear.
        a.cpu = Some(40.0);

        let rows = override_rows(&a);
        assert_eq!(
            rows.iter()
                .map(|(k, n, v)| (*k, n.as_str(), *v))
                .collect::<Vec<_>>(),
            vec![
                // BTreeMap order inside each rule: "Google Chrome"
                // before "node", and stable across renders so a row
                // cannot move under the pointer between read and click.
                ("alert-cpu", "Google Chrome", 45.0),
                ("alert-cpu", "node", 70.0),
                ("alert-mem", "Xcode", 25.0),
                ("alert-app-cpu", "zed", 600.0),
                ("alert-app-mem", "Slack", 12.0),
                ("alert-disk", "/", 95.0),
            ]
        );
        // Nothing set is no card at all, not an empty one.
        assert!(override_rows(&zstats::settings::AlertsConfig::default()).is_empty());
    }

    #[test]
    fn humanize_matches_file_form() {
        assert_eq!(humanize(Duration::ZERO), "0s");
        assert_eq!(humanize(Duration::from_secs(2)), "2s");
        assert_eq!(humanize(Duration::from_secs(60)), "1m");
        assert_eq!(humanize(Duration::from_millis(500)), "500ms");
    }
}
