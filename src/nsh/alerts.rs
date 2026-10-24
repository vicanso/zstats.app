//! Alerts on NSH: the gpui front end's `views/alerts.rs`, whose comments say
//! why every card, line and control is the way it is — this file carries
//! the same choices and does not repeat their reasons. The tab's rules are
//! shared (`alertlist.rs`), its thresholds' resolution too (`alertbars.rs`).
//!
//! The page is a list of blocks — an episode's card, a watcher's card, a
//! heading, the armed list, a past day — gathered from the store as one
//! memo and kept by their keys (an episode by its sequence number, a day by
//! its date); a block is made again only when what it shows changes, which
//! is minutes or hours apart, never per sample.

use crate::alertbars;
use crate::alertlist::{self, Goto, OverrideTarget, PastRow, QuitSubject};
use crate::assets::CustomIconName;
use crate::color::{Rgba, rgba};
use crate::nsh::root::{self, AuxWindow, CARD_GAP, CARD_PAD, HostAction, Live};
use crate::nsh::widgets;
use crate::nsh::{store, theme};
use crate::state::{SeenAlert, Tab, ZStatsAppState};
use crate::{format, i18n, palette};
use nsh_core::ui::prelude::*;
use rust_i18n::t;
use std::time::Duration;
use zstats::{AlertDetail, Severity};

/// The critical pill's words, on the alarm's fill.
const WHITE: Rgba = rgba(0xffffffff);

/// The Alerts page.
pub fn page(live: Live) -> AnyElement {
    let blocks = live.memo(blocks_of);
    view()
        .gap(px(CARD_GAP))
        .child(each(
            move || blocks.get(),
            Block::key,
            // A row's root is a view: NSH keeps a row by its node, and a
            // bare `dynamic` is a region with none ("a kept row is gone").
            |b, _| {
                let b = computed(move || b.get());
                view().child(dynamic(move || block(b.get())))
            },
        ))
        .into_element()
}

/// One block of the page.
#[derive(Clone, PartialEq)]
enum Block {
    /// Nothing today: what the engine is armed with
    Empty(Option<Armed>),
    Alert(Box<Card>),
    Sustained(Vec<Holding>),
    /// A climbing tree: its name and figures
    Creep(Vec<(String, String)>),
    /// A grown directory: its path and figures
    Growth(Vec<(String, String)>),
    /// The heading over the episodes restored from earlier today
    Earlier,
    Armed(Armed),
    PastHeading,
    PastDay(Day),
    PastEmpty,
}

impl Block {
    fn key(&self) -> String {
        match self {
            Block::Empty(_) => "empty".into(),
            Block::Alert(card) => format!("alert-{}", card.seq),
            Block::Sustained(_) => "sustained".into(),
            Block::Creep(_) => "creep".into(),
            Block::Growth(_) => "growth".into(),
            Block::Earlier => "earlier".into(),
            Block::Armed(_) => "armed".into(),
            Block::PastHeading => "past".into(),
            Block::PastDay(day) => format!("day-{}", day.date),
            Block::PastEmpty => "past-empty".into(),
        }
    }
}

/// An episode's card.
#[derive(Clone, PartialEq)]
struct Card {
    seq: u64,
    critical: bool,
    live: bool,
    title: String,
    when: String,
    sentence: String,
    /// What to do about it — a live card's only
    advice: Option<String>,
    consumers: Vec<Consumer>,
    follow_up: Option<String>,
    snoozed: bool,
    auto_quiet: bool,
    quit: Option<QuitSubject>,
    goto: Option<Goto>,
    target: Option<OverrideTarget>,
    /// The threshold editor and quiet hours, while open
    editor: Option<Editor>,
}

/// One of a pressure alert's top consumers.
#[derive(Clone, PartialEq)]
struct Consumer {
    name: String,
    bytes: String,
    quit: Option<QuitSubject>,
}

#[derive(Clone, PartialEq)]
struct Editor {
    caption: String,
    /// The preset in force, lit
    current: Option<String>,
    presets: Vec<(String, &'static str)>,
    snooze: String,
    snoozed: bool,
}

/// A sustained hold's row.
#[derive(Clone, PartialEq)]
struct Holding {
    name: String,
    pid: String,
    line: String,
}

/// The armed list: its ⓘ and its lines (`alertlist::armed_lines`).
#[derive(Clone, PartialEq)]
struct Armed {
    tip: String,
    lines: Vec<(Vec<(String, String)>, bool)>,
}

/// A past day's card.
#[derive(Clone, PartialEq)]
struct Day {
    date: String,
    heading: String,
    count: String,
    rows: Vec<PastRow>,
    more: Option<String>,
}

fn blocks_of(state: &ZStatsAppState) -> Vec<Block> {
    let live: Vec<&SeenAlert> = state.alerts().iter().filter(|s| s.live).collect();
    let earlier: Vec<&SeenAlert> = state.alerts().iter().filter(|s| !s.live).collect();
    let holdings = state.sustained_active();
    let creeps = state.creeps_active();
    let growth = state.disk_growth_announced();
    let armed = armed_of(state);
    if live.is_empty()
        && earlier.is_empty()
        && holdings.is_empty()
        && creeps.is_empty()
        && growth.is_empty()
    {
        let mut blocks = vec![Block::Empty(armed)];
        blocks.extend(past_blocks(state));
        return blocks;
    }

    let mut blocks: Vec<Block> = live
        .iter()
        .map(|s| Block::Alert(Box::new(card_of(s, state))))
        .collect();
    let mut watchers: Vec<(Duration, Block)> = Vec::new();
    if let Some(ago) = alertlist::newest_noticed_ago(&holdings, state.sustained_rule().after) {
        let rows = holdings
            .iter()
            .map(|n| Holding {
                name: n.name.clone(),
                pid: t!("processes.pid_only", pid = n.pid).to_string(),
                line: alertlist::sustained_line(n),
            })
            .collect();
        watchers.push((ago, Block::Sustained(rows)));
    }
    if !creeps.is_empty() {
        let ago = creeps.iter().map(|(_, ago)| *ago).min().unwrap_or_default();
        let rows = creeps
            .iter()
            .map(|(c, _)| (c.name.clone(), alertlist::creep_line(c)))
            .collect();
        watchers.push((ago, Block::Creep(rows)));
    }
    if !growth.is_empty() {
        let ago = growth.iter().map(|(_, ago)| *ago).min().unwrap_or_default();
        let rows = growth
            .iter()
            .map(|(n, _)| (format::tilde(&n.growth.path), alertlist::growth_line(n)))
            .collect();
        watchers.push((ago, Block::Growth(rows)));
    }
    watchers.sort_by_key(|(ago, _)| *ago);
    let ages: Vec<Duration> = live.iter().map(|s| s.age()).collect();
    let agos: Vec<Duration> = watchers.iter().map(|(ago, _)| *ago).collect();
    for (at, (_, watcher)) in alertlist::watch_cards_at(&ages, &agos)
        .into_iter()
        .zip(watchers)
    {
        blocks.insert(at, watcher);
    }
    if !earlier.is_empty() {
        blocks.push(Block::Earlier);
        blocks.extend(
            earlier
                .iter()
                .map(|s| Block::Alert(Box::new(card_of(s, state)))),
        );
    }
    blocks.extend(armed.map(Block::Armed));
    blocks.extend(past_blocks(state));
    blocks
}

fn card_of(seen: &SeenAlert, state: &ZStatsAppState) -> Card {
    let event = &seen.event;
    let target = alertlist::override_target(event);
    let snoozed = state.snoozed_until(event);
    let consumers = match &event.detail {
        AlertDetail::Pressure { top_consumers, .. } => top_consumers
            .iter()
            .map(|c| Consumer {
                name: alertlist::consumer_name(c),
                bytes: format::memory(c.bytes),
                quit: alertlist::consumer_quit(c, seen.live),
            })
            .collect(),
        _ => Vec::new(),
    };
    let editor = target
        .as_ref()
        .filter(|t| alertlist::editing(state, t))
        .map(|t| {
            let (caption, current) = alertlist::threshold_caption(state.settings(), t);
            Editor {
                caption,
                current,
                presets: alertbars::presets(t.key),
                snooze: alertlist::snooze_caption(snoozed),
                snoozed: snoozed.is_some(),
            }
        });
    Card {
        seq: seen.seq,
        critical: event.severity() == Severity::Critical,
        live: seen.live,
        title: alertlist::subject_label(&event.subject),
        when: alertlist::when_text(seen),
        sentence: alertlist::alert_sentence(event),
        advice: seen.live.then(|| i18n::tr(alertlist::advice_key(event))),
        consumers,
        follow_up: alertlist::follow_up(event),
        snoozed: snoozed.is_some(),
        auto_quiet: snoozed.is_none() && state.banner_auto_quiet(event),
        quit: alertlist::quit_subject(seen),
        goto: alertlist::goto_target(seen),
        target,
        editor,
    }
}

fn armed_of(state: &ZStatsAppState) -> Option<Armed> {
    Some(Armed {
        tip: alertlist::watching_tip(),
        lines: alertlist::armed_lines(alertlist::armed_rows(state)?),
    })
}

fn past_blocks(state: &ZStatsAppState) -> Vec<Block> {
    let days = state.alert_history();
    if days.is_empty() {
        return vec![Block::PastEmpty];
    }
    let today = jiff::Zoned::now().date();
    let mut blocks = vec![Block::PastHeading];
    blocks.extend(days.iter().map(|day| {
        let groups = alertlist::past_groups(&day.episodes);
        let shown = groups.len().min(alertlist::PAST_ROWS_PER_DAY);
        let hidden = groups.len() - shown;
        Block::PastDay(Day {
            date: day.date.clone(),
            heading: format::day_heading(&day.date, today),
            count: alertlist::past_count(day.episodes.len()),
            rows: groups
                .iter()
                .take(shown)
                .map(|g| alertlist::past_row(g))
                .collect(),
            more: (hidden > 0).then(|| t!("alerts.past_more", count = hidden).to_string()),
        })
    }));
    blocks
}

fn block(b: Block) -> AnyElement {
    match b {
        Block::Empty(armed) => empty_card(armed),
        Block::Alert(card) => alert_card(*card),
        Block::Sustained(rows) => sustained_card(rows),
        Block::Creep(rows) => creep_card(rows),
        Block::Growth(rows) => growth_card(rows),
        Block::Earlier => view()
            .px(px(CARD_PAD))
            .child(small_heading(i18n::tr("alerts.earlier")))
            .into_element(),
        Block::Armed(armed) => widgets::card().child(armed_block(armed)).into_element(),
        Block::PastHeading => view().px(px(CARD_PAD)).child(past_heading()).into_element(),
        Block::PastDay(day) => past_day_card(day),
        Block::PastEmpty => widgets::card()
            .child(past_heading())
            .child(
                view()
                    .mt(px(6.))
                    .child(widgets::note(i18n::tr("alerts.past_none"))),
            )
            .into_element(),
    }
}

/// A text at `size` in `ink`, at gpui's line height for it.
fn words(words: impl IntoReactive<SharedString>, size: f32, ink: fn() -> Rgba) -> Text {
    text(words)
        .text_size(px(size))
        .line_height(widgets::line(size))
        .text_color(theme::color(ink))
}

/// The dim 10px heading between cards and over the armed list.
fn small_heading(label: String) -> Text {
    words(label, 10., palette::text_dim).font_weight(FontWeight::MEDIUM)
}

/// A card's title: 12px, the first ink.
fn card_title(label: String) -> Text {
    words(label, 12., palette::text).font_weight(FontWeight::SEMIBOLD)
}

fn empty_card(armed: Option<Armed>) -> AnyElement {
    let mut card = widgets::card()
        .pt(px(16.))
        .pb(px(16.))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .child(
                    words(i18n::tr("alerts.empty_title"), 13., palette::text)
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
                    "alerts.empty_tip",
                )))),
        )
        .child(words(i18n::tr("alerts.empty_body"), 11., palette::text_muted).mt(px(6.)));
    if let Some(armed) = armed {
        card = card.child(view().mt(px(8.)).child(armed_block(armed)));
    }
    card.into_element()
}

fn alert_card(card: Card) -> AnyElement {
    let critical = card.critical;
    let mut el = widgets::card()
        .child(alert_head(&card))
        .child(
            words(card.title.clone(), 13., palette::text)
                .mt(px(8.))
                .number_of_lines(1u32)
                .font_weight(FontWeight::SEMIBOLD),
        )
        .child(view().mt(px(2.)).child(widgets::note(card.when.clone())))
        .child(
            text(card.sentence.clone())
                .mt(px(3.))
                .text_size(px(9.5))
                .line_height(widgets::line(9.5))
                .text_color(theme::color(|| palette::tiny_label(palette::text_muted()))),
        );
    if critical {
        el = el.bg(theme::color(|| palette::accent_wash(7)));
    }
    if let Some(advice) = card.advice.clone() {
        el = el.child(view().mt(px(4.)).child(widgets::note(advice)));
    }
    if !card.consumers.is_empty() {
        el = el.child(consumers(card.consumers.clone()));
    }
    if let Some(words) = card.follow_up.clone() {
        el = el.child(view().mt(px(8.)).child(widgets::note(words)));
    }
    if let (Some(target), Some(editor)) = (card.target.clone(), card.editor.clone()) {
        el = el
            .child(threshold_editor(&target, &editor))
            .child(snooze_row(card.seq, &editor));
    }
    el.into_element()
}

/// The severity pill on the left; on the right what the card says about
/// its banners and where it came from, then what can be done with it,
/// dismissal last.
fn alert_head(card: &Card) -> AnyElement {
    let critical = card.critical;
    let mut right = view().flex_row().items_center().gap(px(6.));
    if card.snoozed {
        right = right.child(widgets::outline_pill(i18n::tr("alerts.snoozed_pill")));
    }
    if card.auto_quiet {
        right = right.child(
            view()
                .flex_none()
                .tooltip(widgets::wrap_tooltip(i18n::tr("alerts.auto_quiet_tip")))
                .child(widgets::outline_pill(i18n::tr("alerts.auto_quiet_pill"))),
        );
    }
    if !card.live {
        right = right.child(widgets::outline_pill(i18n::tr("alerts.earlier_session")));
    }
    if let Some(subject) = card.quit.clone() {
        right = right.child(quit_button(subject));
    }
    if let Some(goto) = card.goto {
        let icon = match goto {
            Goto::Hardware => CustomIconName::HardDrive,
            Goto::Process(_) => CustomIconName::Cpu,
            Goto::App(_) => CustomIconName::AppWindow,
        };
        right = right.child(widgets::ghost_icon(
            icon,
            widgets::wrap_tooltip(goto.tip()),
            move || {
                store::update(|state, fx| match goto {
                    Goto::Hardware => state.set_tab(Tab::Hardware, fx),
                    Goto::Process(pid) => state.reveal_pid(pid, fx),
                    Goto::App(root) => state.reveal_app(root, fx),
                })
            },
        ));
    }
    if let Some(target) = card.target.clone() {
        right = right.child(adjust_chip(target));
    }
    let seq = card.seq;
    right = right.child(widgets::ghost_icon(
        CustomIconName::Close,
        widgets::wrap_tooltip(i18n::tr("alerts.dismiss_hint")),
        move || store::update(|state, fx| state.dismiss_alert(seq, fx)),
    ));
    view()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .child(
            text(i18n::tr(if critical {
                "alerts.critical"
            } else {
                "alerts.warning"
            }))
            .flex_none()
            .rounded_full()
            .border(px(1.))
            .border_color(theme::color(move || {
                if critical {
                    palette::accent_wash(45)
                } else {
                    palette::border()
                }
            }))
            .bg(theme::color(move || {
                if critical {
                    palette::accent()
                } else {
                    palette::surface()
                }
            }))
            .px(px(8.))
            .py(px(1.5))
            .text_size(px(9.5))
            .line_height(widgets::line(9.5))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::color(move || {
                if critical {
                    WHITE
                } else {
                    palette::tiny_label(palette::text_muted())
                }
            })),
        )
        .child(right)
        .into_element()
}

/// "Adjust": the word, not the sliders glyph (that one is Settings), at the
/// pills' size; it opens the card's threshold editor.
fn adjust_chip(target: OverrideTarget) -> AnyElement {
    view()
        .flex_none()
        .tooltip(widgets::wrap_tooltip(i18n::tr("alerts.set_hint")))
        .child(
            nsh_base::button()
                .rounded_full()
                .border(px(1.))
                .border_color(theme::color(palette::border))
                .bg(theme::color(palette::inset))
                .hover(|s| s.bg(theme::color(palette::surface_raised)))
                .px(px(8.))
                .py(px(2.))
                .on_press(move || {
                    store::update(|state, fx| state.toggle_alert(target.key, &target.name, fx))
                })
                .child(
                    words(i18n::tr("alerts.adjust"), 10., palette::text)
                        .font_weight(FontWeight::MEDIUM),
                ),
        )
        .into_element()
}

/// The refusable quit, behind its sheet — the log-out glyph, not the
/// footer's power one, which quits this app.
fn quit_button(subject: QuitSubject) -> AnyElement {
    let tip = subject.tip();
    widgets::ghost_icon(
        CustomIconName::LogOut,
        widgets::wrap_tooltip(tip),
        move || {
            let (title, body) = subject.sheet();
            let subject = subject.clone();
            widgets::confirm(title, body, i18n::tr("alerts.quit_ok"), move || {
                subject.request()
            });
        },
    )
}

/// Who held the memory when pressure fired, each with its quit.
fn consumers(rows: Vec<Consumer>) -> AnyElement {
    view()
        .mt(px(8.))
        .gap(px(2.))
        .child(widgets::note(i18n::tr("alerts.top_consumers")))
        .children(
            rows.into_iter()
                .map(|c| {
                    let mut figures = view()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .child(words(c.bytes, 10., palette::text_muted).tabular_nums());
                    if let Some(subject) = c.quit {
                        figures = figures.child(quit_button(subject));
                    }
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            words(c.name, 11., palette::text)
                                .flex_1()
                                .min_w(px(0.))
                                .number_of_lines(1u32),
                        )
                        .child(figures)
                        .into_element()
                })
                .collect::<Vec<_>>(),
        )
        .into_element()
}

fn threshold_editor(target: &OverrideTarget, editor: &Editor) -> AnyElement {
    let chips: Vec<AnyElement> = editor
        .presets
        .iter()
        .map(|(label, value)| {
            let on = editor.current.as_deref() == Some(*value);
            let key = target.key;
            let name = target.name.clone();
            let value = *value;
            widgets::option_chip(
                label.clone(),
                move || on,
                move || {
                    let name = name.clone();
                    store::update(|state, fx| {
                        if let Err(e) = state.apply_alert_override(key, &name, value, fx) {
                            tracing::error!("alert override failed: {e}");
                        }
                    });
                },
            )
        })
        .collect();
    view()
        .mt(px(10.))
        .pt(px(8.))
        .border_t(px(1.))
        .border_color(theme::color(palette::border_subtle))
        .child(widgets::note(editor.caption.clone()))
        .child(view().flex_row().mt(px(6.)).gap(px(4.)).children(chips))
        .into_element()
}

/// Quiet hours for this episode's banners, under its thresholds.
fn snooze_row(seq: u64, editor: &Editor) -> AnyElement {
    let mut row = view()
        .flex_row()
        .items_center()
        .mt(px(8.))
        .gap(px(4.))
        .child(
            view()
                .mr(px(2.))
                .child(widgets::note(editor.snooze.clone())),
        );
    if editor.snoozed {
        row = row.child(snooze_chip(i18n::tr("alerts.snooze_off"), move || {
            store::update(|state, fx| {
                if let Some(event) = event_of(state, seq) {
                    state.unsnooze_banners(&event, fx);
                }
            })
        }));
    } else {
        for (label, hours) in alertlist::SNOOZE_CHOICES {
            row = row.child(snooze_chip(i18n::tr(label), move || {
                store::update(|state, fx| {
                    if let Some(event) = event_of(state, seq) {
                        state.snooze_banners(&event, hours, fx);
                    }
                })
            }));
        }
    }
    row.into_element()
}

/// One of the quiet-hours chips.
fn snooze_chip(label: String, act: impl Fn() + 'static) -> AnyElement {
    nsh_base::button()
        .flex_none()
        .rounded_full()
        .border(px(1.))
        .border_color(theme::color(palette::border))
        .bg(theme::color(palette::inset))
        .hover(|s| s.bg(theme::color(palette::surface_raised)))
        .px(px(8.))
        .py(px(2.))
        .on_press(act)
        .child(words(label, 10., palette::text).font_weight(FontWeight::MEDIUM))
        .into_element()
}

/// The event behind card `seq`, for the store calls that take one.
fn event_of(state: &ZStatsAppState, seq: u64) -> Option<zstats::AlertEvent> {
    state
        .alerts()
        .iter()
        .find(|s| s.seq == seq)
        .map(|s| s.event.clone())
}

/// A watcher's rows under its title: the hairline over every row but the
/// first, 7 above and below.
fn watch_row(row: View, last: bool) -> View {
    view()
        .py(px(7.))
        .border_b(px(if last { 0. } else { 1. }))
        .border_color(theme::color(palette::border_subtle))
        .child(row)
}

fn sustained_card(rows: Vec<Holding>) -> AnyElement {
    let last = rows.len().saturating_sub(1);
    widgets::card()
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .child(card_title(i18n::tr("alerts.sustained_title")))
                .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
                    "alerts.sustained_note",
                )))),
        )
        .children(
            rows.into_iter()
                .enumerate()
                .map(|(i, h)| {
                    let name = h.name.clone();
                    watch_row(
                        view()
                            .child(
                                view()
                                    .flex_row()
                                    .items_baseline()
                                    .gap(px(6.))
                                    .min_w(px(0.))
                                    .child(widgets::name(
                                        move || name.clone(),
                                        11.5,
                                        FontWeight::MEDIUM,
                                    ))
                                    .child(words(h.pid, 10., palette::text_dim).flex_none()),
                            )
                            .child(
                                words(h.line, 10., palette::text_muted)
                                    .mt(px(2.))
                                    .tabular_nums(),
                            ),
                        i == last,
                    )
                    .into_element()
                })
                .collect::<Vec<_>>(),
        )
        .into_element()
}

/// A name that truncates beside its figures — the creep and growth rows.
fn figure_row(name: String, figures: String) -> View {
    view()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .child(
            words(name, 11.5, palette::text)
                .flex_1()
                .min_w(px(0.))
                .number_of_lines(1u32)
                .font_weight(FontWeight::MEDIUM),
        )
        .child(
            words(figures, 10., palette::text_muted)
                .flex_none()
                .tabular_nums(),
        )
}

fn creep_card(rows: Vec<(String, String)>) -> AnyElement {
    let last = rows.len().saturating_sub(1);
    widgets::card()
        .child(card_title(i18n::tr("alerts.creep_title")))
        .child(words(i18n::tr("alerts.creep_note"), 10., palette::text_dim).mt(px(3.)))
        .children(
            rows.into_iter()
                .enumerate()
                .map(|(i, (name, figures))| {
                    watch_row(figure_row(name, figures), i == last).into_element()
                })
                .collect::<Vec<_>>(),
        )
        .into_element()
}

fn growth_card(rows: Vec<(String, String)>) -> AnyElement {
    let last = rows.len().saturating_sub(1);
    widgets::card()
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(card_title(i18n::tr("alerts.growth_title")))
                .child(
                    // gpui-kit's xsmall button with a label: 20 tall, its
                    // 12px words, the input's outline.
                    nsh_base::button()
                        .flex_none()
                        .h(px(20.))
                        .px(px(4.))
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .border(px(1.))
                        .border_color(theme::color(palette::border))
                        .hover(|s| s.bg(theme::color(palette::surface_raised)))
                        .on_press(|| root::ask(HostAction::Open(AuxWindow::Storage)))
                        .child(words(i18n::tr("alerts.growth_open"), 12., palette::text)),
                ),
        )
        .child(words(i18n::tr("alerts.growth_note"), 10., palette::text_dim).mt(px(3.)))
        .children(
            rows.into_iter()
                .enumerate()
                .map(|(i, (path, figures))| {
                    watch_row(figure_row(path, figures), i == last).into_element()
                })
                .collect::<Vec<_>>(),
        )
        .into_element()
}

/// What the engine is armed with: the short thresholds two to a row, a
/// watcher's sentence across the card.
fn armed_block(armed: Armed) -> AnyElement {
    let last = armed.lines.len().saturating_sub(1);
    let cell = |(key, value): (String, String), wide: bool| {
        let key = words(key, 10., palette::text_dim);
        let value = words(value, 10., palette::text_muted).tabular_nums();
        view()
            .flex_row()
            .flex_1()
            .min_w(px(0.))
            .justify_between()
            .gap(px(8.))
            .child(if wide {
                key.flex_1().min_w(px(0.)).number_of_lines(1u32)
            } else {
                key.flex_none()
            })
            .child(if wide {
                value.flex_none()
            } else {
                value.min_w(px(0.)).number_of_lines(1u32)
            })
    };
    view()
        .w_full()
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .child(small_heading(i18n::tr("alerts.empty_watching")))
                .child(widgets::info_icon(widgets::wrap_tooltip(armed.tip))),
        )
        .children(
            armed
                .lines
                .into_iter()
                .enumerate()
                .map(|(i, (line, half))| {
                    let lone = half && line.len() == 1;
                    let mut row = view()
                        .flex_row()
                        .w_full()
                        .gap(px(16.))
                        .pt(px(4.))
                        .pb(px(if i == last { 0. } else { 4. }))
                        .children(
                            line.into_iter()
                                .map(|pair| cell(pair, !half))
                                .collect::<Vec<_>>(),
                        );
                    // A threshold left alone on the last short row keeps
                    // half the width.
                    if lone {
                        row = row.child(view().flex_1());
                    }
                    row.into_element()
                })
                .collect::<Vec<_>>(),
        )
        .into_element()
}

fn past_heading() -> AnyElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .child(small_heading(i18n::tr("alerts.past_days")))
        .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
            "alerts.past_tip",
        ))))
        .into_element()
}

fn past_day_card(day: Day) -> AnyElement {
    let shown = day.rows.len();
    let folded = day.more.is_some();
    let mut card = widgets::list_shell()
        .child(widgets::list_header(
            widgets::list_title(day.heading).into_element(),
            Some(widgets::note(day.count).into_element()),
        ))
        .children(
            day.rows
                .into_iter()
                .enumerate()
                .map(|(i, row)| past_row(row, i + 1 == shown && !folded))
                .collect::<Vec<_>>(),
        );
    if let Some(more) = day.more {
        card = card.child(
            view()
                .px(px(CARD_PAD))
                .py(px(6.))
                .child(widgets::note(more)),
        );
    }
    card.into_element()
}

/// Time, severity as the cards' dot, subject, kind, and "dismissed" where
/// every card of the row was waved away. Read-only.
fn past_row(row: PastRow, last: bool) -> AnyElement {
    let critical = row.critical;
    let times = row.times.clone();
    let mut subject = view().flex_1().min_w(px(0.));
    if !times.is_empty() {
        subject = subject.tooltip(widgets::tooltip_lines(move || times.clone()));
    }
    let mut el = view()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .px(px(CARD_PAD))
        .py(px(5.))
        .border_b(px(if last { 0. } else { 1. }))
        .border_color(theme::color(palette::border_subtle))
        .child(
            words(row.at, 10., palette::text_dim)
                .flex_none()
                .tabular_nums(),
        )
        .child(
            view()
                .flex_none()
                .tooltip(widgets::wrap_tooltip(alertlist::severity_word(critical)))
                .child(
                    view()
                        .w(px(6.))
                        .h(px(6.))
                        .rounded_full()
                        .bg(theme::color(move || palette::fill_for(critical))),
                ),
        )
        .child(subject.child(words(row.label, 11., palette::text).number_of_lines(1u32)))
        .child(
            text(row.kind)
                .flex_none()
                .text_size(px(9.5))
                .line_height(widgets::line(9.5))
                .text_color(theme::color(|| palette::tiny_label(palette::text_muted()))),
        );
    if row.dismissed {
        el = el.child(widgets::outline_pill(i18n::tr("alerts.past_dismissed")));
    }
    el.into_element()
}
