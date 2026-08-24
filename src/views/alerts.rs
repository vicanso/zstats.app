//! Alerts: what to look at now, then today's record.
//!
//! `Tick::alerts` reports the *moment* a threshold is crossed, not a standing
//! list, so what is shown here is accumulated by the store (see
//! `state::SeenAlert`) along with when this process saw it.
//!
//! Recency beats type. Live episodes (reported this session) sit first,
//! newest report first. The sustained-load card joins that group by when
//! it started being noticed — it is not an alert, but it is happening
//! now, and burying it under restored cards made the tab look stale.
//! Episodes restored from earlier today follow, under their own heading.
//!
//! Clicking a card expands a per-subject threshold editor. Writes go
//! through `zstats::settings::apply_add` so they match the CLI, then the
//! collector reloads `[alerts]` in place.

use super::widgets::{self, card};
use crate::alertlog::{self, DayLog};
use crate::assets;
use crate::confirm;
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{SeenAlert, SustainedNotice, ZStatsAppState, ZStatsGlobalStore};
use crate::terminate;
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{IconName, Sizable};
use gpui_component::{h_flex, v_flex};
use rust_i18n::t;
use std::time::Duration;
use zstats::settings::{FileConfig, PressureAlert};
use zstats::{AlertDetail, AlertEvent, AlertKind, AlertSubject, Severity};

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let live: Vec<&SeenAlert> = state.alerts().iter().filter(|s| s.live).collect();
    let earlier: Vec<&SeenAlert> = state.alerts().iter().filter(|s| !s.live).collect();
    let holdings = state.sustained_active();

    if live.is_empty() && earlier.is_empty() && holdings.is_empty() {
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
    if let (Some(ago), Some(card)) = (
        newest_noticed_ago(&holdings, after),
        sustained_from(&holdings),
    ) {
        let ages: Vec<Duration> = live.iter().map(|s| s.age()).collect();
        let at = sustained_insert_at(&ages, ago);
        now.splice(at..at, std::iter::once(card));
    }

    let mut cards = now;
    if !earlier.is_empty() {
        cards.push(earlier_heading());
        cards.extend(earlier.iter().copied().map(|s| alert_card(s, state)));
    }
    // Watching belongs here too, not only on the empty list: otherwise
    // one episode leaves half the panel blank and hides what is armed.
    cards.extend(armed_block(state).map(on_content_line));
    cards.extend(past_days_block(state.alert_history()));
    cards.push(on_content_line(widgets::note(i18n::tr(
        "alerts.footer_note",
    ))));
    cards
}

/// Bare text between cards sits on the card-content line — body gutter
/// plus a card's own 13 — the same line every heading and every card
/// interior uses. Without this the tab had two left edges: card content
/// and headings at one, loose text 13px to their left, and "Past 7
/// days" read as indented against the Watching rows above it.
fn on_content_line(el: AnyElement) -> AnyElement {
    div().px(px(13.)).child(el).into_any_element()
}

fn empty_card(state: &ZStatsAppState) -> AnyElement {
    card()
        .pt(px(16.))
        .pb(px(16.))
        .child(
            div()
                .text_size(px(13.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(i18n::tr("alerts.empty_title")),
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

/// How long ago the watcher started pointing at this stretch: the hold
/// itself is `after` old by then, but that is the bar, not the news.
fn newest_noticed_ago(holdings: &[SustainedNotice], after: Duration) -> Option<Duration> {
    holdings
        .iter()
        .map(|n| n.duration.saturating_sub(after))
        .min()
}

/// Index in the live list (newest first) the sustained card belongs at.
/// A hold noticed more recently than a live report sits above it.
fn sustained_insert_at(live_ages: &[Duration], noticed_ago: Duration) -> usize {
    live_ages
        .iter()
        .position(|age| noticed_ago < *age)
        .unwrap_or(live_ages.len())
}

fn earlier_heading() -> AnyElement {
    div()
        .px(px(13.))
        .text_size(px(10.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text_dim())
        .child(i18n::tr("alerts.earlier"))
        .into_any_element()
}

/// Rows a day shows before folding the rest into "+N more": enough to
/// read a busy day, not enough for one day to push the week off the
/// panel.
const PAST_ROWS_PER_DAY: usize = 6;

/// The past week, read-only: a heading, then one shell per day with a
/// line per episode — time, severity as the dot the cards use, subject,
/// kind, and "dismissed" where the card was acknowledged. No buttons,
/// no expansion: the pids are history and nothing here can be acted on.
/// Empty days are not listed; an empty week says so, and says that it
/// only knows what this app saw.
fn past_days_block(days: &[DayLog]) -> Vec<AnyElement> {
    let mut out = vec![past_heading()];
    if days.is_empty() {
        out.push(on_content_line(widgets::note(i18n::tr("alerts.past_none"))));
        return out;
    }
    out.extend(days.iter().map(past_day_card));
    out
}

fn past_heading() -> AnyElement {
    h_flex()
        .items_center()
        .gap(px(4.))
        .px(px(13.))
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
    let count = day.episodes.len();
    let count_text = if count == 1 {
        i18n::tr("alerts.past_count_one")
    } else {
        t!("alerts.past_count", count = count).to_string()
    };
    let shown = count.min(PAST_ROWS_PER_DAY);
    let hidden = count - shown;
    widgets::list_shell()
        .child(widgets::list_header(
            day.date.clone(),
            Some(widgets::note(count_text)),
        ))
        .children(
            day.episodes
                .iter()
                .take(shown)
                .enumerate()
                .map(|(i, e)| past_row(e, i + 1 == shown && hidden == 0)),
        )
        .when(hidden > 0, |d| {
            d.child(div().px(px(13.)).py(px(6.)).child(widgets::note(
                t!("alerts.past_more", count = hidden).to_string(),
            )))
        })
        .into_any_element()
}

fn past_row(e: &alertlog::Restored, last: bool) -> AnyElement {
    let critical = e.event.severity() == Severity::Critical;
    h_flex()
        .items_center()
        .gap(px(8.))
        .px(px(13.))
        .py(px(5.))
        .when(!last, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .child(
            div()
                .flex_none()
                .font_family(font::MONO)
                .text_size(px(10.))
                .text_color(theme::text_dim())
                .child(format::clock(e.at)),
        )
        // Severity as the cards paint it: accent past the line, ink
        // otherwise — the one colour rule every threshold shares.
        .child(
            div()
                .flex_none()
                .w(px(6.))
                .h(px(6.))
                .rounded_full()
                .bg(theme::fill_for(critical)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.))
                .text_color(theme::text())
                .child(subject_label(&e.event.subject)),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(9.5))
                .text_color(theme::tiny_label(theme::text_muted()))
                .child(i18n::tr(past_kind_key(e.event.kind()))),
        )
        .children(
            e.dismissed
                .then(|| widgets::outline_pill(i18n::tr("alerts.past_dismissed"))),
        )
        .into_any_element()
}

fn past_kind_key(kind: AlertKind) -> &'static str {
    match kind {
        AlertKind::Cpu => "alerts.kind_cpu",
        AlertKind::Memory => "alerts.kind_mem",
        AlertKind::AppCpu => "alerts.kind_app_cpu",
        AlertKind::AppMemory => "alerts.kind_app_mem",
        AlertKind::Disk => "alerts.kind_disk",
        AlertKind::Pressure => "alerts.kind_pressure",
    }
}

/// One episode card. Keyed by the episode's own id, never by position:
/// the deque reorders whenever an episode resurfaces, and an index would
/// hand this card's hover / expansion state to whichever alert took the
/// slot.
fn alert_card(seen: &SeenAlert, state: &ZStatsAppState) -> AnyElement {
    let selected = state.selected_alert();
    let settings = state.settings();
    let i = seen.seq as usize;
    let critical = seen.event.severity() == Severity::Critical;
    let line = if critical {
        Hsla::from(theme::accent_wash(45))
    } else {
        Hsla::from(theme::border())
    };
    let target = override_target(&seen.event);
    let expanded = target
        .as_ref()
        .is_some_and(|t| selected.is_some_and(|(k, n)| k.as_str() == t.key && n == &t.name));
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
                .child(alert_sentence(&seen.event)),
        )
        .children(consumer_rows(i, &seen.event, seen.live))
        .when_some(seen.event.repeat_after, |d, after| {
            d.child(div().mt(px(8.)).child(widgets::note(
                t!("alerts.follow_up", when = format::span(after)).to_string(),
            )))
        })
        .when_some(target.filter(|_| expanded), |d, tgt| {
            d.child(threshold_editor(i, &tgt, settings))
                .child(snooze_row(i, &seen.event, state))
        })
        .into_any_element()
}

fn alert_when(seen: &SeenAlert) -> AnyElement {
    widgets::note(match seen.span() {
        Some(span) => t!(
            "alerts.episode_span",
            ago = format::ago(seen.age()),
            span = format::span(span)
        )
        .to_string(),
        None => format::ago(seen.age()),
    })
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
    Some(
        card()
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(i18n::tr("alerts.sustained_title")),
            )
            .child(
                div()
                    .mt(px(3.))
                    .text_size(px(10.))
                    .text_color(theme::text_dim())
                    .child(i18n::tr("alerts.sustained_note")),
            )
            .children(active.iter().enumerate().map(|(i, notice)| {
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
                            .child(format!("{} — pid {}", notice.name, notice.pid)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(font::MONO)
                            .text_size(px(10.))
                            .text_color(theme::text_muted())
                            .child(
                                t!(
                                    "alerts.sustained_subtitle",
                                    duration = format::took(notice.duration),
                                    cpu = format::pct(notice.cpu_avg as f32)
                                )
                                .to_string(),
                            ),
                    )
                    .into_any_element()
            }))
            .into_any_element(),
    )
}

/// The rules the engine is armed with right now, resolved through
/// zstats' own [`ActiveThresholds`] (same source as the Config tab, so
/// the two can never disagree). One pair per rule — a single joined
/// line wrapped mid-token at 320px (`CPU 30 Memory:`). `None` with no
/// readable config: the empty body already covers the waiting state.
fn armed_rows(state: &ZStatsAppState) -> Option<Vec<(String, String)>> {
    let file = state.settings()?;
    let eff = zstats::alerts::ActiveThresholds::from_config(&file.alerts);
    let mut rows = Vec::new();
    if let Some(v) = eff.cpu.base() {
        rows.push((i18n::tr("alerts.kind_cpu"), format!("{v:.0}%")));
    }
    // Memory bars are the LOWER of a share and an absolute ceiling, so
    // the percentage alone overstates them: 25% reads as 16 GB on a
    // 64 GiB machine while the rule actually trips at 4 GB. zstats
    // resolves the effective figure itself — ask it rather than paint
    // half the rule. The empty name matches no override, which is what
    // "the base bar" means here.
    let total = state.latest().map(|tick| tick.snapshot.memory.total_bytes);
    match total.and_then(|t| eff.memory_bar_bytes("", t)) {
        Some(bytes) => rows.push((i18n::tr("alerts.kind_mem"), format::memory(bytes))),
        // Before the first sample there is no total to resolve against;
        // the share is all that can be said honestly.
        None => {
            if let Some(f) = eff.memory.base() {
                rows.push((i18n::tr("alerts.kind_mem"), format!("{:.0}%", f * 100.0)));
            }
        }
    }
    // The whole-app rules, which zstats 0.5.1 made reachable (the app
    // memory bar gained an absolute ceiling; a share of RAM alone was
    // unreachable on the machines browsers actually run on). They are
    // half of what "per-program thresholds" means, so an armed list
    // that omits them understates the watch.
    if let Some(v) = eff.app_cpu.base() {
        rows.push((i18n::tr("alerts.kind_app_cpu"), format!("{v:.0}%")));
    }
    if let Some(bytes) = total.and_then(|t| eff.app_memory_bar_bytes("", t)) {
        rows.push((i18n::tr("alerts.kind_app_mem"), format::memory(bytes)));
    }
    if let Some(f) = eff.disk.base() {
        rows.push((
            i18n::tr("alerts.kind_disk"),
            format!("{:.0}%", f64::from(f) * 100.0),
        ));
    }
    rows.push((
        i18n::tr("alerts.kind_sustained"),
        t!(
            "alerts.watch_sustained",
            cpu = format!("{:.0}%", state.sustained_bar_percent()),
            after = format::span(state.sustained_rule().after)
        )
        .to_string(),
    ));
    rows.push((
        i18n::tr("alerts.empty_cooldown"),
        super::config::humanize(eff.cooldown),
    ));
    Some(rows)
}

fn armed_block(state: &ZStatsAppState) -> Option<AnyElement> {
    let rows = armed_rows(state)?;
    let last = rows.len().saturating_sub(1);
    Some(
        v_flex()
            .w_full()
            .child(
                div()
                    .text_size(px(10.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::text_dim())
                    .child(i18n::tr("alerts.empty_watching")),
            )
            .children(rows.into_iter().enumerate().map(|(i, (k, v))| {
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap(px(8.))
                    .pt(px(4.))
                    .when(i != last, |d| d.pb(px(4.)))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(10.))
                            .text_color(theme::text_dim())
                            .child(k),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(font::MONO)
                            .text_size(px(10.))
                            .text_color(theme::text_muted())
                            .child(v),
                    )
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
            .children(hardware_button(index, &seen.event))
            .children(target.map(|tgt| {
                // The note this tooltip carries used to be a line of its
                // own at the foot of every card — one line per card, on
                // every card, explaining a button that is right here.
                widgets::with_wrap_tooltip(
                    ("edit-threshold-tip", index),
                    i18n::tr("alerts.set_hint"),
                    Button::new(("edit-threshold", index))
                        .icon(IconName::Settings2)
                        .ghost()
                        .xsmall()
                        .on_click(move |_, _window, cx| {
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| state.toggle_alert(tgt.key, &tgt.name, cx));
                        }),
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

/// Disk alerts jump to the Hardware tab: the volume cards and the
/// space tooling (large files, the analyser) live there, and without
/// this the two halves of the story were disconnected — an alert said
/// "full" while the remedy sat two tabs away.
fn hardware_button(index: usize, event: &AlertEvent) -> Option<Button> {
    matches!(event.kind(), AlertKind::Disk).then(|| {
        Button::new(("goto-hardware", index))
            .icon(IconName::HardDrive)
            .ghost()
            .xsmall()
            .tooltip(i18n::tr("alerts.goto_hardware"))
            .on_click(|_, _window, cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| {
                        state.set_tab(crate::state::Tab::Hardware, cx)
                    });
            })
    })
}

fn alert_title(subject: &AlertSubject) -> AnyElement {
    div()
        .mt(px(8.))
        .text_size(px(13.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::text())
        .truncate()
        .child(subject_label(subject))
        .into_any_element()
}

/// The alert's sentence, in the reader's language.
///
/// zstats ships `AlertEvent::summary()`, and its own documentation says
/// a frontend with its own layout or language should build from the
/// fields instead — which is exactly what this does. **Nothing is
/// recomputed**: every figure here is read off the event that fired, and
/// deciding that it should fire stayed in the rule engine. Only the
/// wording is ours.
///
/// Two things the English summary carries are deliberately dropped,
/// because this card already shows them better:
///
/// - the subject's name, which is the card's title one line above;
/// - the pressure alert's `top:` list, which the card renders underneath
///   as real rows with a quit button on each.
///
/// The notification banner has its own, terser set of these
/// (`notify_body`, `alerts.notify_*`): a macOS banner gets two lines, a
/// card gets as many as it needs. The wording differs on purpose; the
/// figures cannot drift apart, because both read the same fields off the
/// same event.
fn alert_sentence(event: &AlertEvent) -> String {
    let pct = |v: f64| format!("{v:.0}");
    match &event.detail {
        AlertDetail::Cpu {
            avg_percent,
            threshold_percent,
            window,
            runaway,
        } => t!(
            if *runaway {
                "alerts.msg_cpu_runaway"
            } else {
                "alerts.msg_cpu"
            },
            window = format::span(*window),
            avg = pct(*avg_percent),
            threshold = pct(*threshold_percent)
        )
        .to_string(),
        AlertDetail::Memory {
            avg_bytes,
            share_percent,
            threshold_bytes,
            threshold_percent,
            window,
        } => t!(
            "alerts.msg_memory",
            window = format::span(*window),
            used = format::memory(*avg_bytes),
            share = pct(*share_percent),
            // Bytes are the rule's native unit, but the field carries a
            // serde default of zero, so a card restored from a file an
            // older build wrote has none. The percent it was always
            // expressed against is the honest stand-in — "0 B" would be
            // a threshold nothing could cross.
            threshold = if *threshold_bytes > 0 {
                format::memory(*threshold_bytes)
            } else {
                format!("{}%", pct(*threshold_percent))
            }
        )
        .to_string(),
        AlertDetail::Disk {
            used_percent,
            threshold_percent,
            available_bytes,
            total_bytes,
        } => t!(
            "alerts.msg_disk",
            used = pct(*used_percent),
            threshold = pct(*threshold_percent),
            available = format::capacity(*available_bytes),
            total = format::capacity(*total_bytes)
        )
        .to_string(),
        AlertDetail::Pressure {
            level: _,
            sustained,
            swap_used_bytes,
            swap_total_bytes,
            compressed_bytes,
            ..
        } => {
            // The kernel's own word for the level, the same one the
            // Overview card shows — not a number, because 2 and 4 are
            // labels rather than a scale (zstats says so on the field).
            // Level is the WARNING / CRITICAL pill already. Repeating
            // "Warning" mid-sentence wrapped as a second heading.
            let mut text = t!(
                "alerts.msg_pressure",
                span = format::span(*sustained),
                swap_used = format::memory(*swap_used_bytes),
                swap_total = format::memory(*swap_total_bytes)
            )
            .to_string();
            if let Some(compressed) = compressed_bytes {
                text.push_str(" · ");
                text.push_str(
                    t!("alerts.msg_compressed", size = format::memory(*compressed)).as_ref(),
                );
            }
            text
        }
    }
}

/// Who the alert is about, in the design's compact form.
fn subject_label(subject: &AlertSubject) -> String {
    match subject {
        // Display name first (zstats 0.5.3): the card is read by a
        // person, and "Electron" names every stock-packaged Electron
        // app at once. The matchable identity is still `name` — the
        // gear's threshold editor below keys on it, deliberately.
        AlertSubject::Process {
            pid,
            name,
            display_name,
        } => t!(
            "alerts.process",
            name = display_name.as_deref().unwrap_or(name),
            pid = *pid
        )
        .to_string(),
        AlertSubject::App {
            name,
            display_name,
            process_count,
            ..
        } => t!(
            "alerts.app",
            name = display_name.as_deref().unwrap_or(name),
            count = *process_count
        )
        .to_string(),
        AlertSubject::Volume { mount_point } => {
            t!("alerts.volume", mount = mount_point.clone()).to_string()
        }
        AlertSubject::System => i18n::tr("alerts.system"),
    }
}

/// The quit button, on memory alerts only. Freeing memory means evicting
/// the holder — unlike CPU spikes, which pass on their own often enough
/// that eviction from an alert card would be premature. The decision that
/// something is over the line stays zstats' (this consumes its event);
/// the click, the confirm sheet and the delivery are `terminate`'s.
#[cfg(target_os = "macos")]
fn quit_button(index: usize, seen: &SeenAlert) -> Option<Button> {
    use crate::terminate;

    // A restored card's pid may belong to something else entirely by
    // now — see [`SeenAlert::live`]. No button rather than a button
    // that could hit the wrong target.
    if !seen.live {
        return None;
    }
    let event = &seen.event;
    // The confirm sheet speaks the name a person recognises; delivery
    // is by pid either way, so the display name risks nothing.
    let (pid, name) = match (&event.subject, event.kind()) {
        (
            AlertSubject::Process {
                pid,
                name,
                display_name,
            },
            AlertKind::Memory,
        ) => (*pid, display_name.clone().unwrap_or_else(|| name.clone())),
        (
            AlertSubject::App {
                root_pid,
                name,
                display_name,
                ..
            },
            AlertKind::AppMemory,
        ) => (
            *root_pid,
            display_name.clone().unwrap_or_else(|| name.clone()),
        ),
        _ => return None,
    };
    // No control that could only fail: a subject this user cannot signal
    // (root-owned, or already gone) simply gets no button.
    if !terminate::can_quit(pid) {
        return None;
    }
    Some(quit_request_button(
        ("quit-subject", index).into(),
        pid,
        name,
    ))
}

/// The refusable-quit control itself, shared between the memory alert's
/// head and the pressure card's consumer rows. Callers gate on
/// `terminate::can_quit` first.
#[cfg(target_os = "macos")]
fn quit_request_button(id: gpui::ElementId, pid: u32, name: String) -> Button {
    use crate::terminate::{self, QuitMethod};

    Button::new(id)
        // Log-out, not the footer's power glyph: that one quits *this*
        // app, and seeing both on the Alerts tab made the two requests
        // look like the same act.
        .icon(gpui_component::Icon::from(assets::CustomIconName::LogOut))
        .ghost()
        .xsmall()
        .tooltip(t!("alerts.quit_tip", name = name.clone()).to_string())
        .on_click(move |_, window, cx| {
            // Resolved at click time, not render time: whether the pid
            // still counts as an application can change in between, and
            // the sheet must describe what will actually be sent.
            let body = match terminate::method_for(pid) {
                QuitMethod::App => t!("alerts.quit_body_app", name = name.clone()),
                QuitMethod::Term => t!("alerts.quit_body_term", name = name.clone()),
            }
            .to_string();
            let title = t!("alerts.quit_title", name = name.clone()).to_string();
            confirm::ask(
                window,
                cx,
                title,
                body,
                i18n::tr("alerts.quit_ok"),
                move |_| {
                    if !terminate::request_quit(pid) {
                        tracing::warn!("quit request for pid {pid} was not delivered");
                    }
                },
            );
        })
}

/// Never-run stub — see "Platform reality" in CLAUDE.md.
#[cfg(not(target_os = "macos"))]
fn quit_button(_index: usize, _seen: &SeenAlert) -> Option<Button> {
    None
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
                            .child(c.display_name.clone().unwrap_or_else(|| c.name.clone())),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(4.))
                            .child(
                                div()
                                    .font_family(font::MONO)
                                    .text_size(px(10.))
                                    .text_color(theme::text_muted())
                                    .child(format::memory(c.bytes)),
                            )
                            .children(live.then(|| consumer_quit(index, i, c)).flatten()),
                    )
                    .into_any_element()
            }))
            .into_any_element(),
    )
}

#[cfg(target_os = "macos")]
fn consumer_quit(index: usize, row: usize, c: &zstats::alerts::MemoryConsumer) -> Option<Button> {
    if !terminate::can_quit(c.pid) {
        return None;
    }
    Some(quit_request_button(
        SharedString::from(format!("quit-consumer-{index}-{row}")).into(),
        c.pid,
        c.name.clone(),
    ))
}

/// Never-run stub — see "Platform reality" in CLAUDE.md.
#[cfg(not(target_os = "macos"))]
fn consumer_quit(
    _index: usize,
    _row: usize,
    _c: &zstats::alerts::MemoryConsumer,
) -> Option<Button> {
    None
}

/// The `[alerts]` key + override name this event writes when the user
/// picks a new threshold. `name` is empty for the global pressure rule.
#[derive(Clone)]
struct OverrideTarget {
    key: &'static str,
    name: String,
}

fn override_target(event: &AlertEvent) -> Option<OverrideTarget> {
    match (&event.subject, event.kind()) {
        (AlertSubject::Process { name, .. }, AlertKind::Cpu) => Some(OverrideTarget {
            key: "alert-cpu",
            name: name.clone(),
        }),
        (AlertSubject::Process { name, .. }, AlertKind::Memory) => Some(OverrideTarget {
            key: "alert-mem",
            name: name.clone(),
        }),
        (AlertSubject::App { name, .. }, AlertKind::AppCpu) => Some(OverrideTarget {
            key: "alert-app-cpu",
            name: name.clone(),
        }),
        (AlertSubject::App { name, .. }, AlertKind::AppMemory) => Some(OverrideTarget {
            key: "alert-app-mem",
            name: name.clone(),
        }),
        (AlertSubject::Volume { mount_point }, AlertKind::Disk) => Some(OverrideTarget {
            key: "alert-disk",
            name: mount_point.clone(),
        }),
        (AlertSubject::System, AlertKind::Pressure) => Some(OverrideTarget {
            key: "alert-pressure",
            name: String::new(),
        }),
        _ => None,
    }
}

/// Quiet hours for this episode's banners, below the threshold chips.
/// Deliberately a delivery-layer control, not a rule change: the engine
/// keeps evaluating, the list above keeps recording, config.toml is
/// untouched — only the interruption stops, and only until the deadline.
fn snooze_row(index: usize, event: &AlertEvent, state: &ZStatsAppState) -> AnyElement {
    let active = state.snoozed_until(event);
    let caption = match active {
        Some(time) => t!("alerts.snoozed_until", time = time).to_string(),
        None => i18n::tr("alerts.snooze_hint"),
    };

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
        for (slug, label_key, hours) in
            [("1h", "alerts.snooze_1h", 1), ("3h", "alerts.snooze_3h", 3)]
        {
            let event = event.clone();
            row = row.child(
                chip(slug, i18n::tr(label_key)).on_click(move |_, _window, cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.snooze_banners(&event, hours, cx));
                }),
            );
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
    let current = configured_value(settings, target);
    let overridden = is_overridden(settings, target);
    let caption = match (&current, overridden) {
        (Some(v), true) => {
            t!("alerts.override_at", value = display_value(target.key, v)).to_string()
        }
        (Some(v), false) => {
            t!("alerts.default_at", value = display_value(target.key, v)).to_string()
        }
        (None, _) => i18n::tr("alerts.threshold_unknown"),
    };

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
                .children(presets(target.key).iter().map(|(label, value)| {
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
                        .border_color(if on {
                            Hsla::from(theme::accent_wash(45))
                        } else {
                            Hsla::from(theme::border())
                        })
                        .bg(if on {
                            Hsla::from(theme::accent_wash(10))
                        } else {
                            Hsla::from(theme::inset())
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

/// Preset chips. Labels are already localised / unit-suffixed; values are
/// what `apply_add` accepts (`0` disables a per-name rule).
fn presets(key: &str) -> Vec<(String, &'static str)> {
    match key {
        "alert-cpu" => vec![
            ("50%".into(), "50"),
            ("80%".into(), "80"),
            ("100%".into(), "100"),
            ("200%".into(), "200"),
            (i18n::tr("alerts.off"), "0"),
        ],
        "alert-mem" => vec![
            ("15%".into(), "15"),
            ("25%".into(), "25"),
            ("40%".into(), "40"),
            ("60%".into(), "60"),
            (i18n::tr("alerts.off"), "0"),
        ],
        "alert-app-cpu" => vec![
            ("200%".into(), "200"),
            ("400%".into(), "400"),
            ("800%".into(), "800"),
            (i18n::tr("alerts.off"), "0"),
        ],
        "alert-app-mem" => vec![
            ("30%".into(), "30"),
            ("40%".into(), "40"),
            ("60%".into(), "60"),
            (i18n::tr("alerts.off"), "0"),
        ],
        "alert-disk" => vec![
            ("80%".into(), "80"),
            ("90%".into(), "90"),
            ("95%".into(), "95"),
            (i18n::tr("alerts.off"), "0"),
        ],
        "alert-pressure" => vec![
            (i18n::tr("alerts.warning"), "warning"),
            (i18n::tr("alerts.critical"), "critical"),
            (i18n::tr("alerts.off"), "off"),
        ],
        _ => Vec::new(),
    }
}

fn configured_value(settings: Option<&FileConfig>, target: &OverrideTarget) -> Option<String> {
    let a = &settings?.alerts;
    match target.key {
        "alert-cpu" => a
            .cpu_overrides
            .get(&target.name)
            .copied()
            .or(a.cpu)
            .or(Some(30.0))
            .map(fmt_pct),
        "alert-mem" => a
            .mem_overrides
            .get(&target.name)
            .copied()
            .or(a.mem)
            .or(Some(25.0))
            .map(fmt_pct_f64),
        "alert-app-cpu" => a
            .app_cpu_overrides
            .get(&target.name)
            .copied()
            .or(a.app_cpu)
            .or(Some(200.0))
            .map(fmt_pct),
        "alert-app-mem" => a
            .app_mem_overrides
            .get(&target.name)
            .copied()
            .or(a.app_mem)
            .or(Some(40.0))
            .map(fmt_pct_f64),
        "alert-disk" => a
            .disk_overrides
            .get(&target.name)
            .copied()
            .or(a.disk)
            .or(Some(90.0))
            .map(fmt_pct),
        "alert-pressure" => Some(
            match a.pressure.unwrap_or(PressureAlert::Warning) {
                PressureAlert::Off => "off",
                PressureAlert::Warning => "warning",
                PressureAlert::Critical => "critical",
            }
            .into(),
        ),
        _ => None,
    }
}

fn is_overridden(settings: Option<&FileConfig>, target: &OverrideTarget) -> bool {
    let Some(a) = settings.map(|s| &s.alerts) else {
        return false;
    };
    match target.key {
        "alert-cpu" => a.cpu_overrides.contains_key(&target.name),
        "alert-mem" => a.mem_overrides.contains_key(&target.name),
        "alert-app-cpu" => a.app_cpu_overrides.contains_key(&target.name),
        "alert-app-mem" => a.app_mem_overrides.contains_key(&target.name),
        "alert-disk" => a.disk_overrides.contains_key(&target.name),
        "alert-pressure" => a.pressure.is_some(),
        _ => false,
    }
}

fn fmt_pct(v: f32) -> String {
    format!("{v:.0}")
}

fn fmt_pct_f64(v: f64) -> String {
    format!("{v:.0}")
}

fn display_value(key: &str, raw: &str) -> String {
    if raw == "0" || raw == "off" {
        return i18n::tr("alerts.off");
    }
    match (key, raw) {
        ("alert-pressure", "warning") => i18n::tr("alerts.warning"),
        ("alert-pressure", "critical") => i18n::tr("alerts.critical"),
        ("alert-pressure", _) => raw.to_string(),
        _ => format!("{raw}%"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use zstats::AlertDetail;

    fn cpu_process(name: &str) -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::Process {
                pid: 1,
                name: name.into(),
                display_name: None,
            },
            detail: AlertDetail::Cpu {
                avg_percent: 90.0,
                threshold_percent: 30.0,
                window: Duration::from_secs(60),
                runaway: false,
            },
            repeat_after: None,
        }
    }

    #[test]
    fn process_cpu_writes_alert_cpu_override() {
        let t = override_target(&cpu_process("ghostty")).expect("target");
        assert_eq!(t.key, "alert-cpu");
        assert_eq!(t.name, "ghostty");
    }

    /// A hold that just crossed the two-hour bar is news; a live alert
    /// last reported hours ago is not, even though it is a "real" alert.
    #[test]
    fn a_just_noticed_hold_sits_above_stale_live_alerts() {
        let live = [Duration::from_secs(4 * 3600), Duration::from_secs(9 * 3600)];
        assert_eq!(sustained_insert_at(&live, Duration::from_secs(2 * 60)), 0);
    }

    #[test]
    fn a_fresh_live_alert_keeps_the_top() {
        let live = [Duration::from_secs(30), Duration::from_secs(4 * 3600)];
        assert_eq!(sustained_insert_at(&live, Duration::from_secs(2 * 60)), 1);
    }

    #[test]
    fn with_no_live_alerts_sustained_is_first() {
        assert_eq!(sustained_insert_at(&[], Duration::from_secs(60)), 0);
    }

    #[test]
    fn an_old_hold_falls_through_to_the_bottom_of_live() {
        let live = [Duration::from_secs(60)];
        assert_eq!(sustained_insert_at(&live, Duration::from_secs(3 * 3600)), 1);
    }

    #[test]
    fn noticed_ago_is_the_hold_minus_the_bar() {
        let n = SustainedNotice {
            pid: 1,
            name: "ghostty".into(),
            cpu_avg: 23.7,
            duration: crate::watch::DEFAULT_SUSTAINED_AFTER + Duration::from_secs(120),
        };
        let after = crate::watch::DEFAULT_SUSTAINED_AFTER;
        assert_eq!(
            newest_noticed_ago(&[n], after),
            Some(Duration::from_secs(120))
        );
        assert_eq!(newest_noticed_ago(&[], after), None);
    }

    #[test]
    fn system_pressure_has_no_per_name_override() {
        let event = AlertEvent {
            subject: AlertSubject::System,
            detail: AlertDetail::Pressure {
                level: 2,
                sustained: Duration::from_secs(60),
                swap_used_bytes: 0,
                swap_total_bytes: 0,
                compressed_bytes: None,
                top_consumers: Vec::new(),
            },
            repeat_after: None,
        };
        let t = override_target(&event).expect("target");
        assert_eq!(t.key, "alert-pressure");
        assert!(t.name.is_empty());
    }
}
