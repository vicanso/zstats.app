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
// Only the Permissions nav icon rides `CustomIconName`, and that page is
// macOS.
#[cfg(target_os = "macos")]
use crate::assets;
use crate::autostart;
use crate::cachepreset;
use crate::cleanhints;
use crate::confirm;
use crate::font;
use crate::format;
use crate::i18n;
use crate::prefs;
pub use crate::settingspage::SettingsSection;
use crate::settingspage::{self, Autostart};
use crate::state::{
    CachesSync, HintsSync, TemplateSync, UpdateStatus, WebhookTest, ZStatsAppState,
    ZStatsGlobalStore,
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
// The next four serve the Full Disk Access probe alone, which is a
// TCC question and does not exist off macOS.
#[cfg(target_os = "macos")]
use std::env;
#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use zstats::CollectorConfig;
use zstats::alerts::ActiveThresholds;
use zstats::settings::PressureAlert;

/// The nav row's icon. Settings2 on Advanced is where config.toml's own
/// knobs are; the footer gear that opens this window is the same glyph.
/// Alerts wears the panel's Alerts tab's bell.
pub fn section_icon(section: SettingsSection) -> Icon {
    match section {
        SettingsSection::Interface => Icon::new(IconName::Palette),
        SettingsSection::Alerts => Icon::new(IconName::Bell),
        SettingsSection::Advanced => Icon::new(IconName::Settings2),
        // gpui-kit ships no shield; ours rides CustomIconName.
        #[cfg(target_os = "macos")]
        SettingsSection::Permissions => assets::CustomIconName::Shield.into(),
        SettingsSection::About => Icon::new(IconName::Info),
    }
}

/// The settings window's text fields, with whether each currently
/// parses. They live with the window (`main.rs`), not in the store —
/// a half-typed URL is nobody's state but the field's.
pub struct Fields<'a> {
    pub proxy: &'a Entity<InputState>,
    pub proxy_valid: bool,
    pub webhook: &'a Entity<InputState>,
    pub webhook_valid: bool,
}

/// `body_height` is what the window can show without scrolling. Only
/// About reads it — every other section is short enough that the body's
/// own scroll is the right answer.
pub fn render(
    state: &ZStatsAppState,
    section: SettingsSection,
    body_height: f32,
    fields: &Fields,
) -> Vec<AnyElement> {
    match section {
        SettingsSection::Interface => vec![interface_card()],
        SettingsSection::Alerts => render_alerts(state),
        SettingsSection::Advanced => render_advanced(state, fields),
        #[cfg(target_os = "macos")]
        SettingsSection::Permissions => vec![permissions_card()],
        SettingsSection::About => vec![about_card(state, body_height)],
    }
}

/// Full Disk Access, the one switch that covers every prompt the disk
/// analysis can trigger. Shows live status and deep-links to the pane;
/// the app never touches the permission itself.
#[cfg(target_os = "macos")]
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

/// Last Full Disk Access probe. Distinct from the real 0/1 so the first
/// Permissions paint before [`refresh_full_disk_access`] still asks once
/// rather than drawing "not granted" as a guess.
#[cfg(target_os = "macos")]
const FDA_UNREAD: u8 = u8::MAX;
#[cfg(target_os = "macos")]
static FDA: AtomicU8 = AtomicU8::new(FDA_UNREAD);

/// Re-read Full Disk Access. Called when Permissions is selected and
/// when the settings window becomes key while that page is showing —
/// coming back from the system pane. Not per frame: `open(TCC.db)` is
/// a permission check, and the settings window must not follow the
/// collector tick.
#[cfg(target_os = "macos")]
pub fn refresh_full_disk_access() {
    let granted = probe_full_disk_access();
    FDA.store(u8::from(granted), Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
fn full_disk_access_granted() -> bool {
    match FDA.load(Ordering::Relaxed) {
        FDA_UNREAD => {
            refresh_full_disk_access();
            FDA.load(Ordering::Relaxed) == 1
        }
        n => n == 1,
    }
}

/// Opening the TCC database itself requires Full Disk Access and —
/// unlike probing user data — never shows a prompt. The failed attempt
/// has a side effect we want: macOS registers this app in the Full Disk
/// Access list, so the Settings pane offers a ready-made toggle instead
/// of demanding a manual "+".
#[cfg(target_os = "macos")]
fn probe_full_disk_access() -> bool {
    let Ok(home) = env::var("HOME") else {
        return false;
    };
    fs::File::open(Path::new(&home).join("Library/Application Support/com.apple.TCC/TCC.db"))
        .is_ok()
}

/// The thresholds, and the per-subject rules written from alert cards.
fn render_alerts(state: &ZStatsAppState) -> Vec<AnyElement> {
    let mut cards = Vec::new();
    match state.settings() {
        None => cards.push(widgets::empty_card(
            i18n::tr("config.unavailable"),
            i18n::tr("config.unavailable_body"),
        )),
        Some(file) => {
            cards.push(thresholds_card(file));
            // Right under the counts it explains: the thresholds card
            // says twelve programs are tuned, this one says which.
            cards.extend(overrides_card(state));
        }
    }
    cards
}

/// Everything that is not a threshold: the watcher's two knobs, the
/// webhook and the proxy, the collector's cadences, the three tables
/// that update from GitHub, and the reset.
fn render_advanced(state: &ZStatsAppState, fields: &Fields) -> Vec<AnyElement> {
    let mut cards = vec![advanced_card(state, fields)];
    if let Some(file) = state.settings() {
        let collector = file.collector.clone().unwrap_or_default();
        cards.push(collection_card(&collector));
        // The layer below a hand-written override and above the base
        // rule; the Alerts page's thresholds note names it.
        cards.push(template_card(state));
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
        // The project page. It was a button in the panel's footer, one
        // of five glyphs in a row of things the panel does; a link out
        // of the app belongs with what the app is. Still the one
        // control that wears the hand cursor, for the reason it always
        // was: it is the only one that opens something outside.
        .child(
            h_flex()
                .id("about-github")
                .cursor_pointer()
                .flex_none()
                .items_center()
                .justify_between()
                .px(px(13.))
                .py(px(8.))
                .border_t(px(1.))
                .border_color(theme::border_subtle())
                .hover(|d| d.bg(theme::surface_raised()))
                .on_click(|_, _window, cx| cx.open_url(super::REPO_URL))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::ink())
                        .child(i18n::tr("common.github")),
                )
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            div()
                                .font_family(font::MONO)
                                .text_size(px(11.))
                                .text_color(theme::text_muted())
                                .child(super::REPO_URL.trim_start_matches("https://")),
                        )
                        .child(
                            Icon::new(IconName::ExternalLink)
                                .with_size(gpui_kit::component::Size::Size(px(11.)))
                                .text_color(Hsla::from(theme::text_dim())),
                        ),
                ),
        )
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
        .tooltip(widgets::wrap_tooltip(settingspage::file_reveal_tip()))
        .on_click(move |_, _window, cx| {
            cx.stop_propagation();
            settingspage::reveal_file(name);
        })
        .child(name)
        .into_any_element()
}

/// Language and theme. Selection reuses the accent chips of the Alerts
/// threshold editor — in this app a picked value is accent, like a crossed
/// threshold, and everything else stays neutral.
fn interface_card() -> AnyElement {
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
            settingspage::language_choices(),
            prefs::language(),
            crate::set_language_pref,
            None,
        ))
        .child({
            let (themes, tip) = settingspage::theme_choices();
            pref_row(
                "pref-theme",
                i18n::tr("config.theme"),
                themes,
                prefs::theme_in_force(),
                crate::set_theme_pref,
                Some(tip),
            )
        })
        .child(pref_row(
            "pref-tray",
            i18n::tr("config.tray"),
            settingspage::tray_choices(),
            prefs::tray(),
            crate::set_tray_pref,
            Some(i18n::tr("config.tray_tip")),
        ))
        .child(notifications_row())
        .child(autostart_row())
        .child(disk_watch_row())
        .child(overview_detail_row())
        .when(cfg!(target_os = "macos"), |card| {
            card.child(panel_size_row())
        })
        .child(opacity_row())
        .into_any_element()
}

/// The panel's size is the reader's to drag (`window_ext::let_size_be_dragged`).
/// An edge that can be dragged says nothing about itself, so this row is
/// where that is written down, and the way back: the width the layout
/// was designed at, and both forms at the heights their pages were
/// measured for. Only on macOS — a layer surface's edges cannot be
/// dragged.
fn panel_size_row() -> AnyElement {
    let custom = prefs::panel_size_custom();
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
                        .child(i18n::tr("config.panel_size")),
                )
                .child(widgets::info_icon(
                    "pref-panel-size-info",
                    i18n::tr("config.panel_size_tip"),
                )),
        )
        .child(if custom {
            div()
                .id("pref-panel-size-reset")
                .h(px(20.))
                .px(px(8.))
                .rounded(px(5.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::inset())
                .flex()
                .items_center()
                .hover(|d| d.bg(theme::chip()))
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::text())
                .child(i18n::tr("config.panel_size_reset"))
                .on_click(|_, _window, cx| crate::reset_panel_size_pref(cx))
                .into_any_element()
        } else {
            div()
                .text_size(px(11.))
                .text_color(theme::text_muted())
                .child(i18n::tr("config.panel_size_default"))
                .into_any_element()
        })
        .into_any_element()
}

/// The `app.toml` settings that answer a question few people arrive
/// with. They sat in the Interface card between "Menu bar shows" and
/// "Launch at Login", where "Sustained load bar ÷3" was the fifth thing
/// someone changing the language read.
fn advanced_card(state: &ZStatsAppState, fields: &Fields) -> AnyElement {
    widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("config.advanced"),
            Some(file_note("file-note-app-advanced", "app.toml")),
        ))
        // The sustained-load watcher's two knobs. Minutes and a divisor
        // rather than a percent, so the bar keeps following `alert-cpu`
        // (see `prefs::SUSTAINED_DIVISOR`). The tip quotes the bar in
        // force so the divisor reads as a number, not a fraction.
        .child(pref_row(
            "pref-sustained-after",
            i18n::tr("config.sustained_after"),
            settingspage::sustained_after_choices(),
            (prefs::sustained_after().as_secs() / 60) as u16,
            crate::set_sustained_after_pref,
            Some(i18n::tr("config.sustained_after_tip")),
        ))
        .child(pref_row(
            "pref-sustained-divisor",
            i18n::tr("config.sustained_divisor"),
            settingspage::sustained_divisor_choices(),
            prefs::sustained_divisor(),
            crate::set_sustained_divisor_pref,
            Some(settingspage::sustained_divisor_tip(
                state.sustained_bar_percent(),
            )),
        ))
        .child(webhook_row(state, fields.webhook, fields.webhook_valid))
        .child(proxy_row(fields.proxy, fields.proxy_valid))
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

/// The daily disk check (`diskwatch`), on or off. It is also a chip on
/// the disk-space window's analysis toolbar, which is where it started
/// and where its status lives — but the thing that makes someone look
/// for this switch is the app working in the background, and the place
/// they look is here, among the other things it does unasked.
fn disk_watch_row() -> AnyElement {
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
                        .child(i18n::tr("config.disk_watch")),
                )
                .child(widgets::info_icon(
                    "pref-disk-watch-info",
                    i18n::tr("config.disk_watch_tip"),
                )),
        )
        .child(
            Switch::new("pref-disk-watch")
                .small()
                .checked(prefs::disk_watch())
                .on_click(|checked, _window, cx| crate::set_disk_watch_pref(*checked, cx)),
        )
        .into_any_element()
}

/// Overview's form. The panel footer's toggle is the same switch and
/// the one most people will meet; this is where someone looking for the
/// setting looks.
fn overview_detail_row() -> AnyElement {
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
                        .child(i18n::tr("config.overview_detail")),
                )
                .child(widgets::info_icon(
                    "pref-overview-detail-info",
                    i18n::tr("config.overview_detail_tip"),
                )),
        )
        .child(
            Switch::new("pref-overview-detail")
                .small()
                .checked(prefs::overview_detail())
                .on_click(|checked, _window, cx| crate::set_overview_detail_pref(*checked, cx)),
        )
        .into_any_element()
}

/// Outbound proxy for the clean-hints fetch — the app's only network
/// call. Free text because proxies are arbitrary URIs; only values the
/// resolver can act on are persisted, and the inline warning says when
/// the field holds something else. "" follows the environment / system
/// proxy, "none" forces direct.
///
/// The placeholder is the three tokens and nothing more. The sentence
/// that says what each one does does not fit the field — it used to
/// stop at `socks5:` — and the ⓘ beside the label already carries it
/// (`config.proxy_tip`).
fn proxy_row(input: &Entity<InputState>, valid: bool) -> AnyElement {
    // The Advanced card's last row: no divider under it.
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

/// Where notifications are also POSTed (`webhook.rs`). Under the
/// Notifications switch because it is that switch's second road, and
/// held back by it the same way. The button sends one marked test and
/// says what the endpoint answered: a URL typed from memory is wrong
/// more often than not, and the first real alert is a poor moment to
/// find out. The test's verdict is dropped as soon as the URL changes.
fn webhook_row(state: &ZStatsAppState, input: &Entity<InputState>, valid: bool) -> AnyElement {
    let test = state.webhook_test();
    let sending = matches!(test, Some(WebhookTest::Sending));
    let can_test = valid && !sending && !prefs::webhook().is_empty();
    let verdict = match test {
        Some(WebhookTest::Sending) => Some((i18n::tr("config.webhook_test_sending"), false)),
        Some(WebhookTest::Delivered(status)) => Some((
            t!("config.webhook_test_ok", status = status).to_string(),
            false,
        )),
        Some(WebhookTest::Failed(error)) => Some((
            t!("config.webhook_test_failed", error = error.as_str()).to_string(),
            true,
        )),
        None => None,
    };
    v_flex()
        .px(px(13.))
        .py(px(8.))
        .border_b(px(1.))
        .border_color(theme::border_subtle())
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme::ink())
                                .child(i18n::tr("config.webhook")),
                        )
                        .child(widgets::info_icon(
                            "pref-webhook-info",
                            i18n::tr("config.webhook_tip"),
                        )),
                )
                // The page's chip chrome, as the Login Items prompt
                // wears it; dimmed with no hover while there is nothing
                // to send to.
                .child(
                    div()
                        .id("pref-webhook-test")
                        .flex_none()
                        .rounded_full()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::inset())
                        .px(px(10.))
                        .py(px(3.))
                        .text_size(px(11.))
                        .text_color(if can_test {
                            theme::text()
                        } else {
                            theme::text_faint()
                        })
                        .when(can_test, |d| {
                            d.hover(|d| d.bg(theme::surface_raised()))
                                .on_click(|_, _window, cx| {
                                    cx.global::<ZStatsGlobalStore>()
                                        .clone()
                                        .update(cx, |state, cx| state.test_webhook(cx));
                                })
                        })
                        .child(i18n::tr("config.webhook_test")),
                ),
        )
        .child(div().mt(px(6.)).child(Input::new(input).xsmall()))
        .when(!valid, |d| {
            d.child(
                div()
                    .mt(px(4.))
                    .text_size(px(10.))
                    .text_color(Hsla::from(theme::accent_light()))
                    .child(i18n::tr("config.webhook_invalid")),
            )
        })
        .children(verdict.filter(|_| valid).map(|(text, failed)| {
            div()
                .mt(px(4.))
                .text_size(px(10.))
                .text_color(if failed {
                    Hsla::from(theme::accent_light())
                } else {
                    Hsla::from(theme::text_dim())
                })
                .child(text)
        }))
        .into_any_element()
}

/// Launch at login. A Switch rather than chips when the OS will honour
/// `register` / `unregister` — enums pick from chips, a boolean flips a
/// switch, which is also the System Settings idiom. `RequiresApproval`
/// and `NotFound` are not that boolean: the first is a revoke the
/// switch cannot undo, the second is a run with no .app (or one the OS
/// just refused to register), so the row says so instead of painting a
/// toggle that snaps back. A fresh install is neither — the OS reports
/// it as "not found" too, and `autostart::status` sorts that out.
fn autostart_row() -> AnyElement {
    h_flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .px(px(13.))
        .py(px(8.))
        .border_b(px(1.))
        .border_color(theme::border_subtle())
        .child(
            h_flex()
                .items_center()
                .gap(px(4.))
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::ink())
                        .child(i18n::tr("config.autostart")),
                )
                .child(widgets::info_icon(
                    "pref-autostart-info",
                    settingspage::autostart_tip(),
                )),
        )
        .child(autostart_control())
        .into_any_element()
}

fn autostart_control() -> AnyElement {
    match settingspage::autostart() {
        Autostart::Switch(on) => Switch::new("pref-autostart")
            .small()
            .checked(on)
            .on_click(|checked, _window, cx| crate::set_autostart_pref(*checked, cx))
            .into_any_element(),
        // The sentence is the control: it opens Login Items, the only
        // place approval can actually happen. Same chip chrome as the
        // FDA "Open Settings" button — hover fill, not a fake switch.
        Autostart::Approve(words) => div()
            .id("pref-autostart-approve")
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
            .on_click(|_, _window, _cx| autostart::open_login_items())
            .child(words)
            .into_any_element(),
        Autostart::Absent(words) => div()
            .flex_none()
            .text_size(px(11.))
            .text_color(theme::text_dim())
            .child(words)
            .into_any_element(),
    }
}

/// Panel wash. Chips write `app.toml` only — the painted value is
/// whatever `prefs::load` froze at launch.
fn opacity_row() -> AnyElement {
    let current = prefs::opacity();
    let chips = settingspage::opacity_choices();
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
                    let on = settingspage::opacity_on(current, value);
                    div()
                        .id(("pref-opacity", i))
                        .flex_none()
                        .rounded_full()
                        .border_1()
                        .border_color(theme::choice_line(on))
                        .bg(theme::choice_fill(on))
                        .px(px(8.))
                        .py(px(2.))
                        .text_size(px(10.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::choice_ink(on))
                        .hover(|d| d.bg(theme::choice_hover(on)))
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
                        .border_color(theme::choice_line(on))
                        .bg(theme::choice_fill(on))
                        .px(px(8.))
                        .py(px(2.))
                        .text_size(px(10.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::choice_ink(on))
                        .hover(|d| d.bg(theme::choice_hover(on)))
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
    let shown = format::humanize(current);
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

/// The human name for a threshold key. The key itself stays reachable,
/// behind the ⓘ beside the name: these are the exact strings
/// `config.toml` and the CLI's `-add` take, and a card that only had a
/// translated name would leave a reader who wants to change one with
/// nothing to type. It was a second line under every name until
/// 2026-10-12 — nine lines of `alert-app-mem-bytes` for the readers who
/// came to pick a number.
fn threshold_label(key: &str) -> String {
    i18n::tr(match key {
        "alert-cpu" => "config.th_cpu",
        "alert-mem" => "config.th_mem",
        "alert-mem-bytes" => "config.th_mem_bytes",
        "alert-app-cpu" => "config.th_app_cpu",
        "alert-app-mem" => "config.th_app_mem",
        "alert-app-mem-bytes" => "config.th_app_mem_bytes",
        "alert-disk" => "config.th_disk",
        "alert-disk-free" => "config.th_disk_free",
        "alert-pressure" => "config.th_pressure",
        "alert-cooldown" => "config.th_cooldown",
        // Unreachable for the ten rows above; a future key shows its
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
    let rows: [(&'static str, Setting, usize); 10] = [
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
            // The same ceiling one level up. It was missing here, so
            // this card said "App memory default (40%)" while the Alerts
            // tab's armed list said 8.0 GB — the ceiling biting, unnamed.
            "alert-app-mem-bytes",
            fmt_bytes_setting(a.app_mem_bytes, eff.app_memory_bytes),
            0,
        ),
        (
            "alert-disk",
            fmt_pct_setting(a.disk, eff.disk.base().map(|f| f64::from(f) * 100.0)),
            a.disk_overrides.len(),
        ),
        (
            // The disk rule's second line (zstats 0.7.2): a volume that
            // keeps filling is reported again, critical, once its free
            // space is under the lower of this and half the room the
            // capacity line leaves. Under capacity because it refines it
            // and dies with it — `alert-disk` 0 turns both off.
            "alert-disk-free",
            disk_free_setting(a.disk_free, eff.disk_free_bytes, eff.disk.base().is_some()),
            0,
        ),
        (
            "alert-pressure",
            pressure_setting(a.pressure, eff.pressure),
            0,
        ),
        (
            "alert-cooldown",
            match a.cooldown {
                Some(d) => Setting::set(format::humanize(d)),
                None => Setting::unset(format::humanize(eff.cooldown)),
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
                                    h_flex()
                                        .items_center()
                                        .gap(px(4.))
                                        .child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(theme::ink())
                                                .child(threshold_label(k)),
                                        )
                                        // The key, for whoever types it
                                        // into config.toml or `zstats
                                        // -add`: this card is where they
                                        // would go looking for it.
                                        .child(widgets::info_icon(
                                            ("cfg-th-key", i),
                                            t!("config.th_key_tip", key = k).to_string(),
                                        )),
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
                                                .child(v.shown),
                                        ),
                                ),
                        )
                        .child(div().mt(px(6.)).child(threshold_chips(k, &v.lit)))
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
        "alert-app-mem-bytes" => vec![
            ("4 GB".into(), "4g"),
            ("8 GB".into(), "8g"),
            ("16 GB".into(), "16g"),
            (off, "0"),
        ],
        "alert-disk" => vec![
            ("80%".into(), "80"),
            ("90%".into(), "90"),
            ("95%".into(), "95"),
            (off, "0"),
        ],
        // 0 is "no floor", not "off": the half of what the capacity line
        // leaves still makes a critical line.
        "alert-disk-free" => vec![
            ("5 GB".into(), "5g"),
            ("10 GB".into(), "10g"),
            ("20 GB".into(), "20g"),
            (i18n::tr("config.th_disk_free_none"), "0"),
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
                .border_color(theme::choice_line(on))
                .bg(theme::choice_fill(on))
                .px(px(7.))
                .py(px(1.))
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::choice_ink(on))
                .hover(|d| d.bg(theme::choice_hover(on)))
                .on_click(move |_, _window, cx| apply(key, value, cx))
                .child(label)
        }))
        .into_any_element()
}

/// One rule's value two ways: as its row prints it — the file's own, or
/// the default labelled as such — and as the chip label that value
/// reads as. The chips matched the printed form, so a rule nobody had
/// touched read "default (30%)" over five unlit chips: the one card
/// where every rule is at its default was the one that never said which
/// chip that is.
struct Setting {
    shown: String,
    lit: String,
}

impl Setting {
    fn set(value: String) -> Self {
        Self {
            shown: value.clone(),
            lit: value,
        }
    }

    fn unset(effective: String) -> Self {
        Self {
            shown: default_at(effective.clone()),
            lit: effective,
        }
    }
}

/// The file's value when set; otherwise the effective default, labeled as
/// such, so an untouched key still tells the user what the engine runs.
fn fmt_pct_setting<T: Into<f64> + Copy>(file: Option<T>, effective_pct: Option<f64>) -> Setting {
    match file {
        None => Setting::unset(match effective_pct {
            Some(v) => format!("{v:.0}%"),
            None => i18n::tr("alerts.off"),
        }),
        Some(v) if v.into() == 0.0 => Setting::set(i18n::tr("alerts.off")),
        Some(v) => Setting::set(format!("{:.0}%", v.into())),
    }
}

/// The memory rule's absolute ceiling. Values are whole GiB in practice
/// (the chips only offer those), so the clean "4 GB" form applies; a
/// hand-edited odd value falls back to the byte formatter.
fn fmt_bytes_setting(file: Option<u64>, effective: Option<u64>) -> Setting {
    const GIB: u64 = 1 << 30;
    let clean = |v: u64| {
        if v.is_multiple_of(GIB) {
            format!("{} GB", v / GIB)
        } else {
            format::memory(v)
        }
    };
    match file {
        None => Setting::unset(match effective {
            Some(v) => clean(v),
            None => i18n::tr("alerts.off"),
        }),
        Some(0) => Setting::set(i18n::tr("alerts.off")),
        Some(v) => Setting::set(clean(v)),
    }
}

/// [`fmt_bytes_setting`] for `alert-disk-free`, whose 0 means something
/// else: it removes the absolute floor and leaves the critical line at
/// half the room the capacity line leaves, where every other byte key's
/// 0 turns its rule off. Off only when the disk rule itself is, which
/// takes this line with it.
fn disk_free_setting(file: Option<u64>, effective: Option<u64>, rule_on: bool) -> Setting {
    match file {
        _ if !rule_on => Setting::set(i18n::tr("alerts.off")),
        Some(0) => Setting::set(i18n::tr("config.th_disk_free_none")),
        _ => fmt_bytes_setting(file, effective),
    }
}

/// Same for the pressure tri-state. `effective` uses zstats' encoding:
/// 2 = warning, 4 = critical, `None` = rule off.
fn pressure_setting(file: Option<PressureAlert>, effective: Option<u32>) -> Setting {
    match file {
        Some(PressureAlert::Off) => Setting::set(i18n::tr("alerts.off")),
        Some(PressureAlert::Warning) => Setting::set(i18n::tr("alerts.warning")),
        Some(PressureAlert::Critical) => Setting::set(i18n::tr("alerts.critical")),
        None => Setting::unset(match effective {
            Some(level) if level >= 4 => i18n::tr("alerts.critical"),
            Some(_) => i18n::tr("alerts.warning"),
            None => i18n::tr("alerts.off"),
        }),
    }
}

fn default_at(value: String) -> String {
    t!("config.default_at", value = value).to_string()
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
}
