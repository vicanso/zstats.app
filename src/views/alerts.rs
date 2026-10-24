//! Alerts: what to look at now, then today's record.
//!
//! `Tick::alerts` reports the *moment* a threshold is crossed, not a standing
//! list, so what is shown here is accumulated by the store (see
//! `state::SeenAlert`) along with when this process saw it.
//!
//! Recency beats type. Live episodes (reported this session) sit first,
//! newest report first. The watcher cards — sustained load, and the
//! memory climbs with a banner out — join that group by when they
//! started being noticed: neither is an alert, but both are happening
//! now, and burying them under restored cards made the tab look stale.
//! Episodes restored from earlier today follow, under their own heading.
//!
//! Clicking a card expands a per-subject threshold editor. Writes go
//! through `zstats::settings::apply_add` so they match the CLI, then the
//! collector reloads `[alerts]` in place.

use super::widgets::{self, card};
use crate::alertbars;
use crate::alertlist::{self, Goto, OverrideTarget, QuitSubject};
use crate::alertlog::{self, DayLog};
use crate::assets;
use crate::confirm;
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{
    GrowthNotice, MemoryCreep, SeenAlert, SustainedNotice, ZStatsAppState, ZStatsGlobalStore,
};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Icon, IconName, Sizable};
use gpui_kit::component::{h_flex, v_flex};
use rust_i18n::t;
use std::time::Duration;
use zstats::settings::FileConfig;
use zstats::{AlertDetail, AlertEvent, AlertSubject, Severity};

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let live: Vec<&SeenAlert> = state.alerts().iter().filter(|s| s.live).collect();
    let earlier: Vec<&SeenAlert> = state.alerts().iter().filter(|s| !s.live).collect();
    let holdings = state.sustained_active();
    let creeps = state.creeps_active();
    let growth = state.disk_growth_announced();

    if live.is_empty()
        && earlier.is_empty()
        && holdings.is_empty()
        && creeps.is_empty()
        && growth.is_empty()
    {
        // An empty list is indistinguishable from a broken watcher unless
        // it says what it is armed with — so quote the thresholds in force.
        // The week's record still follows: a quiet today is not a quiet
        // week, and "how often does this fire" is asked on quiet days.
        let mut cards = vec![empty_card(state)];
        cards.extend(past_days_block(state.alert_history()));
        return cards;
    }

    let mut now: Vec<AnyElement> = live.iter().copied().map(|s| alert_card(s, state)).collect();
    let after = state.sustained_rule().after;
    let mut watchers: Vec<(Duration, AnyElement)> = Vec::new();
    if let (Some(ago), Some(card)) = (
        alertlist::newest_noticed_ago(&holdings, after),
        sustained_from(&holdings),
    ) {
        watchers.push((ago, card));
    }
    if let Some(card) = creep_from(&creeps) {
        let ago = creeps.iter().map(|(_, ago)| *ago).min().unwrap_or_default();
        watchers.push((ago, card));
    }
    if let Some(card) = growth_from(&growth) {
        let ago = growth.iter().map(|(_, ago)| *ago).min().unwrap_or_default();
        watchers.push((ago, card));
    }
    watchers.sort_by_key(|(ago, _)| *ago);
    let ages: Vec<Duration> = live.iter().map(|s| s.age()).collect();
    let agos: Vec<Duration> = watchers.iter().map(|(ago, _)| *ago).collect();
    for (at, (_, card)) in alertlist::watch_cards_at(&ages, &agos)
        .into_iter()
        .zip(watchers)
    {
        now.splice(at..at, std::iter::once(card));
    }

    let mut cards = now;
    if !earlier.is_empty() {
        cards.push(earlier_heading());
        cards.extend(earlier.iter().copied().map(|s| alert_card(s, state)));
    }
    // Watching belongs here too, not only on the empty list: otherwise
    // one episode leaves half the panel blank and hides what is armed.
    // In a card of its own: as loose text between cards it read as a
    // card that failed to draw.
    cards.extend(armed_block(state).map(|block| card().child(block).into_any_element()));
    cards.extend(past_days_block(state.alert_history()));
    cards
}

/// Bare text between cards sits on the card-content line — body gutter
/// plus a card's own 13 — the same line every heading and every card
/// interior uses. Without this the tab had two left edges: card content
/// and headings at one, loose text 13px to their left, and "Past 7
/// days" read as indented against the Watching rows above it.
fn on_content_line(el: AnyElement) -> AnyElement {
    div().px(px(widgets::CARD_PAD)).child(el).into_any_element()
}

fn empty_card(state: &ZStatsAppState) -> AnyElement {
    card()
        .pt(px(16.))
        .pb(px(16.))
        .child(
            h_flex()
                .items_center()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(i18n::tr("alerts.empty_title")),
                )
                .child(widgets::info_icon(
                    "alerts-empty-info",
                    i18n::tr("alerts.empty_tip"),
                )),
        )
        .child(
            div()
                .mt(px(6.))
                .text_size(px(11.))
                .text_color(theme::text_muted())
                .child(i18n::tr("alerts.empty_body")),
        )
        .children(armed_block(state).map(|block| div().mt(px(8.)).child(block)))
        .into_any_element()
}

fn earlier_heading() -> AnyElement {
    div()
        .px(px(widgets::CARD_PAD))
        .text_size(px(10.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text_dim())
        .child(i18n::tr("alerts.earlier"))
        .into_any_element()
}

/// The past week, read-only: a heading, then one shell per day with a
/// line per episode — time, severity as the dot the cards use, subject,
/// kind, and "dismissed" where the card was acknowledged. No buttons,
/// no expansion: the pids are history and nothing here can be acted on.
/// Empty days are not listed; an empty week is one card so the heading
/// does not sit in the card-gap as a lone chip, which made it look
/// indented against Watching.
fn past_days_block(days: &[DayLog]) -> Vec<AnyElement> {
    if days.is_empty() {
        return vec![past_empty_card()];
    }
    let mut out = vec![on_content_line(past_heading())];
    out.extend(days.iter().map(past_day_card));
    out
}

fn past_empty_card() -> AnyElement {
    card()
        .child(past_heading())
        .child(
            div()
                .mt(px(6.))
                .child(widgets::note(i18n::tr("alerts.past_none"))),
        )
        .into_any_element()
}

fn past_heading() -> AnyElement {
    h_flex()
        .items_center()
        .gap(px(4.))
        .child(
            div()
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::text_dim())
                .child(i18n::tr("alerts.past_days")),
        )
        .child(widgets::info_icon(
            "alerts-past-info",
            i18n::tr("alerts.past_tip"),
        ))
        .into_any_element()
}

fn past_day_card(day: &DayLog) -> AnyElement {
    let count_text = alertlist::past_count(day.episodes.len());
    let groups = alertlist::past_groups(&day.episodes);
    let shown = groups.len().min(alertlist::PAST_ROWS_PER_DAY);
    let hidden = groups.len() - shown;
    let today = jiff::Zoned::now().date();
    widgets::list_shell()
        .child(widgets::list_header(
            format::day_heading(&day.date, today),
            Some(widgets::note(count_text)),
        ))
        .children(
            groups
                .iter()
                .take(shown)
                .enumerate()
                .map(|(i, group)| past_row(i, group, i + 1 == shown && hidden == 0)),
        )
        .when(hidden > 0, |d| {
            d.child(
                div()
                    .px(px(widgets::CARD_PAD))
                    .py(px(6.))
                    .child(widgets::note(
                        t!("alerts.past_more", count = hidden).to_string(),
                    )),
            )
        })
        .into_any_element()
}

fn past_row(index: usize, group: &[&alertlog::Restored], last: bool) -> AnyElement {
    let row = alertlist::past_row(group);
    let times: Vec<SharedString> = row.times.iter().cloned().map(SharedString::from).collect();
    h_flex()
        .items_center()
        .gap(px(8.))
        .px(px(widgets::CARD_PAD))
        .py(px(5.))
        .when(!last, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .child(
            div()
                .flex_none()
                .font_features(font::tnum())
                .text_size(px(10.))
                .text_color(theme::text_dim())
                .child(row.at),
        )
        // Severity as the cards paint it: accent past the line, ink
        // otherwise — the one colour rule every threshold shares. Its
        // word on hover: a dot's colour was the only key to it.
        .child(
            div()
                .id(("past-severity", index))
                .flex_none()
                .w(px(6.))
                .h(px(6.))
                .rounded_full()
                .bg(theme::fill_for(row.critical))
                .tooltip(widgets::wrap_tooltip(alertlist::severity_word(
                    row.critical,
                ))),
        )
        .child(
            div()
                .id(("past-subject", index))
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.))
                .text_color(theme::text())
                .when(!times.is_empty(), |d| {
                    d.tooltip(widgets::wrap_tooltip_lines(times.clone()))
                })
                .child(row.label),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(9.5))
                .text_color(theme::tiny_label(theme::text_muted()))
                .child(row.kind),
        )
        .children(
            row.dismissed
                .then(|| widgets::outline_pill(i18n::tr("alerts.past_dismissed"))),
        )
        .into_any_element()
}

/// One episode card. Keyed by the episode's own id, never by position:
/// the deque reorders whenever an episode resurfaces, and an index would
/// hand this card's hover / expansion state to whichever alert took the
/// slot.
fn alert_card(seen: &SeenAlert, state: &ZStatsAppState) -> AnyElement {
    let settings = state.settings();
    let i = seen.seq as usize;
    let critical = seen.event.severity() == Severity::Critical;
    let line = if critical {
        Hsla::from(theme::accent_wash(45))
    } else {
        Hsla::from(theme::border())
    };
    let target = alertlist::override_target(&seen.event);
    let expanded = target
        .as_ref()
        .is_some_and(|t| alertlist::editing(state, t));
    let card_id = SharedString::from(format!("alert-{i}"));

    card()
        .id(card_id)
        .border_color(line)
        .when(critical, |d| d.bg(theme::accent_wash(7)))
        .child(alert_head(i, target.clone(), critical, line, seen, state))
        .child(alert_title(&seen.event.subject))
        .child(div().mt(px(2.)).child(alert_when(seen)))
        .child(
            div()
                .mt(px(3.))
                .text_size(px(9.5))
                .text_color(theme::tiny_label(theme::text_muted()))
                .child(alertlist::alert_sentence(&seen.event)),
        )
        // What to do about it, in a sentence — the card said what
        // crossed and by how much, and left the reader there. Live
        // cards only: a restored one is a record, and its subject may
        // be gone.
        .when(seen.live, |d| {
            d.child(
                div()
                    .mt(px(4.))
                    .child(widgets::note(i18n::tr(alertlist::advice_key(&seen.event)))),
            )
        })
        .children(consumer_rows(i, &seen.event, seen.live))
        .when_some(alertlist::follow_up(&seen.event), |d, words| {
            d.child(div().mt(px(8.)).child(widgets::note(words)))
        })
        .when_some(target.filter(|_| expanded), |d, tgt| {
            d.child(threshold_editor(i, &tgt, settings))
                .child(snooze_row(i, &seen.event, state))
        })
        .into_any_element()
}

fn alert_when(seen: &SeenAlert) -> AnyElement {
    widgets::note(alertlist::when_text(seen))
}

/// The sustained-load watcher's holdings, as a read-only card. This is
/// the landing spot for the silent banner — its click opens this tab,
/// and an empty room here read as a broken feature. Display only: the
/// judgment lives in watch.rs, never in the rule engine, and the rows
/// carry no actions on purpose — a steady 12% is information, not an
/// offence.
fn sustained_from(active: &[SustainedNotice]) -> Option<AnyElement> {
    if active.is_empty() {
        return None;
    }
    let last = active.len() - 1;
    // What this card is rides the ⓘ; it was a paragraph above the rows,
    // the longest text on the tab for the one card that never changes.
    // Each row is two lines so the name is not cut to make room for the
    // figures beside it.
    Some(
        card()
            .child(
                h_flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(i18n::tr("alerts.sustained_title")),
                    )
                    .child(widgets::info_icon(
                        "alerts-sustained-info",
                        i18n::tr("alerts.sustained_note"),
                    )),
            )
            .children(active.iter().enumerate().map(|(i, notice)| {
                v_flex()
                    .py(px(7.))
                    .when(i != last, |d| {
                        d.border_b(px(1.)).border_color(theme::border_subtle())
                    })
                    .child(
                        h_flex()
                            .items_baseline()
                            .gap(px(6.))
                            .min_w_0()
                            .child(widgets::truncating_name(
                                ("sustained-name", i),
                                notice.name.clone(),
                                11.5,
                                gpui::FontWeight::MEDIUM,
                                theme::text().into(),
                            ))
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(10.))
                                    .text_color(theme::text_dim())
                                    .child(t!("processes.pid_only", pid = notice.pid).to_string()),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(2.))
                            .font_features(font::tnum())
                            .text_size(px(10.))
                            .text_color(theme::text_muted())
                            .child(alertlist::sustained_line(notice)),
                    )
                    .into_any_element()
            }))
            .into_any_element(),
    )
}

/// The memory-climb watcher's holdings, as a read-only card — the
/// landing spot for the creep banner, exactly as [`sustained_from`] is
/// for the sustained one: the banner's click opens this tab, and until
/// this card existed it opened onto a page that never mentioned the
/// climber. Display only: the judgment lives in trend.rs, the rows
/// carry no actions, and nothing here is an alert — a climb crosses no
/// line until it is too late, which is the whole reason the watcher
/// exists. Only climbs with a banner out appear (the sub-gigabyte ones
/// stay on Overview's strip): this card answers "what was that
/// notification", not "what moved at all".
fn creep_from(creeps: &[(MemoryCreep, Duration)]) -> Option<AnyElement> {
    if creeps.is_empty() {
        return None;
    }
    let last = creeps.len() - 1;
    Some(
        card()
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(i18n::tr("alerts.creep_title")),
            )
            .child(
                div()
                    .mt(px(3.))
                    .text_size(px(10.))
                    .text_color(theme::text_dim())
                    .child(i18n::tr("alerts.creep_note")),
            )
            .children(creeps.iter().enumerate().map(|(i, (creep, _))| {
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .py(px(7.))
                    .when(i != last, |d| {
                        d.border_b(px(1.)).border_color(theme::border_subtle())
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.5))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .truncate()
                            .child(creep.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_features(font::tnum())
                            .text_size(px(10.))
                            .text_color(theme::text_muted())
                            .child(alertlist::creep_line(creep)),
                    )
                    .into_any_element()
            }))
            .into_any_element(),
    )
}

/// What the daily disk check announced, as a read-only card — the
/// landing spot for its banner, the way [`creep_from`] is for the memory
/// one. Display only (an observer, not a rule); the one control opens
/// the disk-space window, where the full growth list and the tools to
/// act on it are.
fn growth_from(growth: &[(GrowthNotice, Duration)]) -> Option<AnyElement> {
    if growth.is_empty() {
        return None;
    }
    let last = growth.len() - 1;
    Some(
        card()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(i18n::tr("alerts.growth_title")),
                    )
                    .child(
                        Button::new("growth-open-disk")
                            .xsmall()
                            .label(i18n::tr("alerts.growth_open"))
                            .on_click(|_, _, cx| crate::open_storage_window(cx)),
                    ),
            )
            .child(
                div()
                    .mt(px(3.))
                    .text_size(px(10.))
                    .text_color(theme::text_dim())
                    .child(i18n::tr("alerts.growth_note")),
            )
            .children(growth.iter().enumerate().map(|(i, (notice, _))| {
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .py(px(7.))
                    .when(i != last, |d| {
                        d.border_b(px(1.)).border_color(theme::border_subtle())
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.5))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .truncate()
                            .child(format::tilde(&notice.growth.path)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_features(font::tnum())
                            .text_size(px(10.))
                            .text_color(theme::text_muted())
                            .child(alertlist::growth_line(notice)),
                    )
                    .into_any_element()
            }))
            .into_any_element(),
    )
}

fn armed_block(state: &ZStatsAppState) -> Option<AnyElement> {
    let lines = alertlist::armed_lines(alertlist::armed_rows(state)?);
    let last = lines.len().saturating_sub(1);
    // `wide`: one sentence across the card. The label yields, and the
    // sentence stays whole at the right edge — the same edge the column
    // above ends on. A truncated value used to start beside the label
    // and lose its tail, so the figure never reached that column.
    let cell = |(k, v): (String, String), wide: bool| {
        h_flex()
            .flex_1()
            .min_w_0()
            .justify_between()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(theme::text_dim())
                    .when(wide, |d| d.min_w_0().truncate().flex_1())
                    .when(!wide, |d| d.flex_none())
                    .child(k),
            )
            .child(
                div()
                    .font_features(font::tnum())
                    .text_size(px(10.))
                    .text_color(theme::text_muted())
                    .when(wide, |d| d.flex_none())
                    .when(!wide, |d| d.min_w_0().truncate())
                    .child(v),
            )
    };
    Some(
        v_flex()
            .w_full()
            .child(
                h_flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::text_dim())
                            .child(i18n::tr("alerts.empty_watching")),
                    )
                    .child(widgets::info_icon(
                        "alerts-watching-info",
                        alertlist::watching_tip(),
                    )),
            )
            .children(lines.into_iter().enumerate().map(|(i, (line, half))| {
                let lone = half && line.len() == 1;
                let wide = !half;
                h_flex()
                    .w_full()
                    .gap(px(16.))
                    .pt(px(4.))
                    .when(i != last, |d| d.pb(px(4.)))
                    .children(line.into_iter().map(|pair| cell(pair, wide)))
                    // A threshold left alone on the last short row keeps
                    // half the width, so its value lines up with the
                    // column above instead of the card's right edge.
                    .when(lone, |row| row.child(div().flex_1()))
                    .into_any_element()
            }))
            .into_any_element(),
    )
}

fn alert_head(
    index: usize,
    target: Option<OverrideTarget>,
    critical: bool,
    line: Hsla,
    seen: &SeenAlert,
    state: &ZStatsAppState,
) -> AnyElement {
    let row = h_flex().items_center().justify_between().gap(px(8.));
    row.child(
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
                // The label arm only: the tier is carried by the fill
                // and border (red vs quiet), not by this grey.
                Hsla::from(theme::tiny_label(theme::text_muted()))
            })
            .child(if critical {
                i18n::tr("alerts.critical")
            } else {
                i18n::tr("alerts.warning")
            }),
    )
    .child(
        h_flex()
            .items_center()
            .gap(px(6.))
            // Timestamps live under the title now — this slot used to
            // hold WARNING + "13m ago · lasted 30m" + pills + icons,
            // and at 320px the actions lost.
            // A muted episode says so without being expanded — otherwise
            // "why no banner?" has no visible answer on the card.
            .children(
                state
                    .snoozed_until(&seen.event)
                    .map(|_| widgets::outline_pill(i18n::tr("alerts.snoozed_pill"))),
            )
            // Auto-quiet says so on the card for the same reason the
            // snooze does: a banner that silently stopped arriving is
            // indistinguishable from a rule that stopped firing. Only
            // when the user has not already asked for quiet — one
            // reason for the silence is enough.
            .when(
                state.snoozed_until(&seen.event).is_none() && state.banner_auto_quiet(&seen.event),
                |d| {
                    d.child(
                        div()
                            .id(SharedString::from(format!("auto-quiet-{index}")))
                            .child(widgets::outline_pill(i18n::tr("alerts.auto_quiet_pill")))
                            .tooltip(widgets::wrap_tooltip(i18n::tr("alerts.auto_quiet_tip"))),
                    )
                },
            )
            // A card from before the last launch says so: it explains
            // both the old numbers and the missing action buttons
            // (see [`SeenAlert::live`]).
            .when(!seen.live, |d| {
                d.child(widgets::outline_pill(i18n::tr("alerts.earlier_session")))
            })
            // An explicit control rather than a clickable card: macOS does
            // not change the pointer over clickable things, so "the whole row
            // does something" has no way to announce itself.
            .children(quit_button(index, seen))
            .children(goto_button(index, seen))
            .children(target.map(|tgt| {
                // The note this tooltip carries used to be a line of its
                // own at the foot of every card — one line per card, on
                // every card, explaining a button that is right here.
                widgets::with_wrap_tooltip(
                    ("edit-threshold-tip", index),
                    i18n::tr("alerts.set_hint"),
                    // A word, not the sliders glyph: that glyph is the
                    // footer's Settings, two inches below, and here it
                    // opened something else.
                    // Built by hand at the pills' size: the kit's
                    // smallest button sets its label at 14px, which
                    // beside a 10px "WARNING" was the loudest word on
                    // the card.
                    div()
                        .id(("edit-threshold", index))
                        .flex_none()
                        .rounded_full()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::inset())
                        .px(px(8.))
                        .py(px(2.))
                        .text_size(px(10.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::text())
                        .hover(|d| d.bg(theme::surface_raised()))
                        .on_click(move |_, _window, cx| {
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| state.toggle_alert(tgt.key, &tgt.name, cx));
                        })
                        .child(i18n::tr("alerts.adjust")),
                )
            }))
            .child({
                // The list's only acknowledgement path — and the edge
                // action, so it goes last. Close, not Delete: this
                // removes a record, it touches nothing on the system.
                let seq = seen.seq;
                widgets::with_wrap_tooltip(
                    ("dismiss-alert-tip", index),
                    i18n::tr("alerts.dismiss_hint"),
                    Button::new(("dismiss-alert", index))
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .on_click(move |_, _window, cx| {
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| state.dismiss_alert(seq, cx));
                        }),
                )
            }),
    )
    .into_any_element()
}

/// Jump to the tab that actually shows this subject. An explicit
/// control rather than a clickable card: macOS does not change the
/// pointer over clickable things, so "the whole card does something"
/// has no way to announce itself.
fn goto_button(index: usize, seen: &SeenAlert) -> Option<Button> {
    let target = alertlist::goto_target(seen)?;
    let tip = target.tip();
    let (id, icon) = match target {
        Goto::Hardware => (("goto-hardware", index), Icon::new(IconName::HardDrive)),
        Goto::Process(_) => (("goto-process", index), Icon::new(IconName::Cpu)),
        Goto::App(_) => (
            ("goto-app", index),
            Icon::from(assets::CustomIconName::AppWindow),
        ),
    };
    Some(
        Button::new(id)
            .icon(icon)
            .ghost()
            .xsmall()
            .tooltip(tip)
            .on_click(move |_, _window, cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| match target {
                        Goto::Hardware => state.set_tab(crate::state::Tab::Hardware, cx),
                        Goto::Process(pid) => state.reveal_pid(pid, cx),
                        Goto::App(root) => state.reveal_app(root, cx),
                    });
            }),
    )
}

fn alert_title(subject: &AlertSubject) -> AnyElement {
    div()
        .mt(px(8.))
        .text_size(px(13.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::text())
        .truncate()
        .child(alertlist::subject_label(subject))
        .into_any_element()
}

/// The quit button, on memory alerts only (`alertlist::quit_subject` says
/// whom, and when there is none).
fn quit_button(index: usize, seen: &SeenAlert) -> Option<Button> {
    let subject = alertlist::quit_subject(seen)?;
    Some(quit_request_button(("quit-subject", index).into(), subject))
}

/// The refusable-quit control itself, shared between the memory alert's
/// head and the pressure card's consumer rows.
fn quit_request_button(id: gpui::ElementId, subject: QuitSubject) -> Button {
    Button::new(id)
        // Log-out, not the footer's power glyph: that one quits *this*
        // app, and seeing both on the Alerts tab made the two requests
        // look like the same act.
        .icon(gpui_kit::component::Icon::from(
            assets::CustomIconName::LogOut,
        ))
        .ghost()
        .xsmall()
        .tooltip(subject.tip())
        .on_click(move |_, window, cx| {
            let (title, body) = subject.sheet();
            let subject = subject.clone();
            confirm::ask(
                window,
                cx,
                title,
                body,
                i18n::tr("alerts.quit_ok"),
                move |_| subject.request(),
            );
        })
}

/// The engine's answer to "who is holding the RAM" when pressure fires,
/// as rows with the same refusable-quit control the memory cards carry.
/// Attribution is zstats' (`top_consumers`, snapshotted at the crossing);
/// this renders and offers the exit, nothing more — pressure goes from
/// "machine state, nothing to act on" to "these are holding it, you pick".
fn consumer_rows(index: usize, event: &AlertEvent, live: bool) -> Option<AnyElement> {
    let AlertDetail::Pressure { top_consumers, .. } = &event.detail else {
        return None;
    };
    if top_consumers.is_empty() {
        return None;
    }
    Some(
        v_flex()
            .mt(px(8.))
            .gap(px(2.))
            .child(widgets::note(i18n::tr("alerts.top_consumers")))
            .children(top_consumers.iter().enumerate().map(|(i, c)| {
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.))
                            .text_color(theme::text())
                            .truncate()
                            // "The half of a pressure alert a person
                            // acts on" — zstats' own reason for putting
                            // the display name on the consumer.
                            .child(alertlist::consumer_name(c)),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(4.))
                            .child(
                                div()
                                    .font_features(font::tnum())
                                    .text_size(px(10.))
                                    .text_color(theme::text_muted())
                                    .child(format::memory(c.bytes)),
                            )
                            .children(alertlist::consumer_quit(c, live).map(|subject| {
                                quit_request_button(
                                    SharedString::from(format!("quit-consumer-{index}-{i}")).into(),
                                    subject,
                                )
                            })),
                    )
                    .into_any_element()
            }))
            .into_any_element(),
    )
}

/// Quiet hours for this episode's banners, below the threshold chips.
/// Deliberately a delivery-layer control, not a rule change: the engine
/// keeps evaluating, the list above keeps recording, config.toml is
/// untouched — only the interruption stops, and only until the deadline.
fn snooze_row(index: usize, event: &AlertEvent, state: &ZStatsAppState) -> AnyElement {
    let active = state.snoozed_until(event);
    let caption = alertlist::snooze_caption(active);

    let chip = |slug: &str, label: String| {
        div()
            .id(SharedString::from(format!("snooze-{index}-{slug}")))
            .flex_none()
            .rounded_full()
            .border_1()
            .border_color(theme::border())
            .bg(theme::inset())
            .px(px(8.))
            .py(px(2.))
            .text_size(px(10.))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme::text())
            .hover(|d| d.bg(theme::surface_raised()))
            .child(label)
    };

    let mut row = h_flex()
        .mt(px(8.))
        .gap(px(4.))
        .child(div().mr(px(2.)).child(widgets::note(caption)));
    if active.is_none() {
        for (label_key, hours) in alertlist::SNOOZE_CHOICES {
            let event = event.clone();
            row = row.child(chip(&format!("{hours}h"), i18n::tr(label_key)).on_click(
                move |_, _window, cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.snooze_banners(&event, hours, cx));
                },
            ));
        }
    } else {
        let event = event.clone();
        row = row.child(chip("off", i18n::tr("alerts.snooze_off")).on_click(
            move |_, _window, cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.unsnooze_banners(&event, cx));
            },
        ));
    }
    row.into_any_element()
}

fn threshold_editor(
    index: usize,
    target: &OverrideTarget,
    settings: Option<&FileConfig>,
) -> AnyElement {
    let (caption, current) = alertlist::threshold_caption(settings, target);

    div()
        .mt(px(10.))
        .pt(px(8.))
        .border_t(px(1.))
        .border_color(theme::border_subtle())
        .child(widgets::note(caption))
        .child(
            h_flex()
                .mt(px(6.))
                .gap(px(4.))
                .children(alertbars::presets(target.key).iter().map(|(label, value)| {
                    let on = current.as_deref() == Some(*value);
                    let key = target.key;
                    let name = target.name.clone();
                    let value = (*value).to_string();
                    div()
                        .id(SharedString::from(format!(
                            "th-{index}-{key}-{}-{value}",
                            target.name
                        )))
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
                        .on_click(move |_, _window, cx| {
                            let key = key;
                            let name = name.clone();
                            let value = value.clone();
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| {
                                    if let Err(e) =
                                        state.apply_alert_override(key, &name, &value, cx)
                                    {
                                        tracing::error!("alert override failed: {e}");
                                    }
                                });
                        })
                        .child((*label).to_string())
                })),
        )
        .into_any_element()
}
