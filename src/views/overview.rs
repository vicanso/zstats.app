//! Overview: processor, top application trees, memory.

use super::alerts;
use super::processes;
use super::widgets::{self, card};
use crate::font;
use crate::format;
use crate::i18n;
use crate::placement;
use crate::prefs;
use crate::series::{self, Point};
use crate::state::{MemoryCreep, Tab, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use crate::trend;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Bounds, Hsla, InteractiveElement, IntoElement, ParentElement, PathBuilder,
    Pixels, Rgba, StatefulInteractiveElement, Styled, Window, div, linear_color_stop,
    linear_gradient, point, px, size,
};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use zstats::snapshot::{
    Capabilities, CpuSnapshot, DiskSnapshot, IoTotalsSnapshot, LoadSnapshot, MemorySnapshot,
    ProcessGroupSnapshot, SystemSnapshot,
};

/// How many trees the first panel names. Enough to answer "who's
/// hot" without turning Overview into a second Apps tab. Three since
/// the charts arrived: the first one or two rows are the answer, the
/// rest were the same few residents every time, and each row is ~30pt
/// of a panel the charts had already made taller. All opens the rest.
const TOP_N: usize = 3;
/// The short form's apps card. It has the room the long form gives its
/// charts, and "which app is it" is the question the three bars above
/// it leave open — five answers instead of three.
const TOP_N_SHORT: usize = 5;

/// One row of the apps card. Also the unit a taller panel buys rows in
/// ([`apps_rows`]).
const APP_ROW: f32 = 34.;

/// Per-core bar turns accent past this.
const CORE_HOT: f32 = 85.0;
/// Swap past this share of **physical memory** is worth colour — display
/// only. The kernel can still report pressure Normal while a lot of the
/// working set has been pushed to disk; that is the signal the badge
/// will not give, and it is what this is for.
///
/// Measured against RAM, and not against swap's own allocation, because
/// macOS sizes swap on demand: `/System/Volumes/VM/` holds uniform 1 GB
/// files and the kernel adds one whenever free swap drops to roughly a
/// file's worth. Steady state is therefore `used/total ≈ (N-1)/N` for N
/// files, which *rises toward 100% as the machine swaps more* — 80% at
/// five files, 86% at seven, 93% at fourteen. A share-of-allocation bar
/// is consequently permanently crossed on any Mac that ever grew past
/// about five swapfiles, and was observed painting red on a machine
/// sitting at 55% memory free. Against RAM the same reading is 24%, and
/// the number means the same thing on a 16 GB laptop as on a 128 GB
/// desktop.
///
/// 50 is a judgement, not a derivation: half the physical memory's worth
/// of pages living on disk. Apple Silicon swaps readily well below that
/// without anything being wrong.
const SWAP_HOT: f32 = 50.0;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(snapshot) = state.latest().map(|t| &t.snapshot) else {
        return vec![widgets::empty_card(
            i18n::tr("common.waiting_sample"),
            i18n::tr("common.waiting_sample_body"),
        )];
    };
    let watts = snapshot
        .battery
        .as_ref()
        .and_then(|b| b.power_watts)
        .and_then(format::whole_watts);
    // One `now` for every line, so a point sits on the same x in each.
    let now = Instant::now();
    let recent = Recent {
        cpu: state.cpu_series(),
        memory: state.memory_series(),
        net_down: state.net_down_series(),
        net_up: state.net_up_series(),
        now,
    };
    // Two forms, each designed as itself rather than one cut down from
    // the other. The short one is the default and follows the
    // 2026-10-12 design: the three things a Mac runs out of in one
    // grammar — name, percent, bar — then who is using it, and above
    // them a notice only while there is one. The long one is the page
    // as it always was; the way back from it is the footer's form
    // toggle (`views::footer`). Each has a fixed height
    // (`placement::window_size`) and nothing in either comes and goes
    // from one sample to the next; the notice is the one row that does
    // come and go, minutes or hours apart, and the panel's height goes
    // with it.
    let mut cards = Vec::new();
    if prefs::overview_detail() {
        cards.push(processor(
            &snapshot.cpu,
            &snapshot.load,
            snapshot.host.uptime_secs,
            watts,
            recent,
        ));
        cards.push(top_apps(state, apps_rows(state, true, TOP_N)));
        cards.push(memory(
            &snapshot.memory,
            snapshot.capabilities,
            state.memory_climbers(),
            state.memory_needs_attention(),
            recent,
        ));
        cards.extend(io_card(&snapshot.io_totals, recent));
    } else {
        cards.extend(notice_row(state));
        cards.push(vitals_card(state, snapshot, recent));
        cards.push(top_apps(state, apps_rows(state, false, TOP_N_SHORT)));
    }
    cards
}

/// The kernel's memory-pressure level at critical (1 is normal, 2
/// warning). The only tier that earns a word of its own, beside the
/// memory row's name and in the long form's badge.
const PRESSURE_CRITICAL: u32 = 4;

/// What the notice row says, most pressing first.
#[derive(Debug, PartialEq, Eq)]
enum Notice {
    /// The menu bar is on memory ([`ZStatsAppState::memory_needs_attention`]).
    Memory,
    /// The menu bar is on a disk ([`ZStatsAppState::disk_needs_attention`]).
    Disk,
    /// Live alert cards that do not turn the menu bar: `count` of them,
    /// the newest named.
    Alerts { count: usize, latest: String },
    /// The sustained-load watcher is holding a card.
    Busy(String),
    /// The memory-climb watcher is holding a card.
    Growing(String),
}

/// Everything the notice row reads, already reduced to names.
#[derive(Default)]
struct Signs {
    memory: bool,
    disk: bool,
    /// Subjects of the live alert cards, newest first.
    alerts: Vec<String>,
    busy: Vec<String>,
    growing: Vec<String>,
}

/// One sentence for the machine, when there is one to say; `None` is
/// the ordinary case and draws nothing.
///
/// It is display of verdicts already made — zstats' alert episodes, in
/// the form the menu bar and the Alerts tab already show them, then the
/// panel's two slow-burn watchers — and compares nothing itself: no
/// figure is read here, so there is no line of its own to cross. The
/// order is the menu bar's (memory before disk), then alerts that are
/// cards only, then the watchers, which are observations rather than
/// crossed thresholds.
///
/// **Every one of these is held by something with a memory** — an
/// episode, a watcher's card — so the answer changes minutes or hours
/// apart, never from one sample to the next. That is a requirement and
/// not a habit: the row's coming and going resizes the panel
/// ([`has_notice`]). The kernel's pressure level was read here too, at
/// critical, for the minutes the engine waits before it reports one;
/// it is a raw level that flaps between samples, and it is now only
/// the word beside the memory row's name.
fn notice(signs: Signs) -> Option<Notice> {
    if signs.memory {
        return Some(Notice::Memory);
    }
    if signs.disk {
        return Some(Notice::Disk);
    }
    if let Some(latest) = signs.alerts.first() {
        return Some(Notice::Alerts {
            count: signs.alerts.len(),
            latest: latest.clone(),
        });
    }
    if let Some(name) = signs.busy.into_iter().next() {
        return Some(Notice::Busy(name));
    }
    signs.growing.into_iter().next().map(Notice::Growing)
}

/// What there is to announce: each source, less what the Alerts tab has
/// already shown (`ZStatsAppState::see_alerts`). The row is an
/// announcement and not a state. It used to stand for as long as its
/// episode did — a disk at 91% said so in red from the morning's banner
/// to midnight, 46 of the panel's height, to someone who had opened the
/// card hours before. Having been followed, or the bell having been, it
/// is done; the red figure on the card below and the red bell go on
/// saying that the condition holds.
fn signs(state: &ZStatsAppState) -> Signs {
    Signs {
        memory: state.memory_is_news(),
        disk: state.disk_is_news(),
        alerts: state
            .alerts()
            .iter()
            .filter(|seen| seen.is_news())
            .map(|seen| alerts::subject_name(&seen.event.subject))
            .collect(),
        busy: state.sustained_news(),
        growing: state.creeps_news(),
    }
}

/// Whether the short form is showing its notice row. The panel is
/// [`placement::NOTICE_HEIGHT`] taller while it is
/// (`crate::sync_panel_notice`), so this and the row must be one
/// answer: both go through [`notice`].
pub fn has_notice(state: &ZStatsAppState) -> bool {
    notice(signs(state)).is_some()
}

/// The short form's first row, there only while there is something to
/// say: the sentence, tinted by what it says — red when an alert is
/// behind it, the card's own fill for the watchers, which are news and
/// not alarms — and the whole row the way into Alerts, where a card
/// says it in full.
///
/// **It was always there until 2026-10-12**, green and reading "Nothing
/// needs attention" on an ordinary day. That sentence went the way
/// "Memory is tight" had gone at the warning tier, for the reason
/// written then: a line that says the same thing from morning to
/// evening is read once. Three bars that are not red and a grey bell
/// already say nothing is wrong, and a row that is absent on an
/// ordinary day is seen on the day it appears. The link to the long
/// form left with it — the footer's toggle is that control, and beside
/// a sentence that opens Alerts it was a second destination on one
/// row.
///
/// The long form never has it: someone who asked for every figure reads
/// the Memory card's badge and a red bell without being told, and the
/// row was 46 of a height that has to fit a 13-inch screen.
fn notice_row(state: &ZStatsAppState) -> Option<AnyElement> {
    let notice = notice(signs(state))?;
    // (the dot, its halo, the row's fill)
    let (dot, halo, fill) = match notice {
        Notice::Memory | Notice::Disk | Notice::Alerts { .. } => (
            theme::accent_light(),
            theme::accent_wash(18),
            theme::accent_wash(10),
        ),
        Notice::Busy(_) | Notice::Growing(_) => (theme::gauge(), theme::chip(), theme::surface()),
    };
    let sentence = match notice {
        Notice::Memory => i18n::tr("overview.status_memory"),
        Notice::Disk => i18n::tr("overview.status_disk"),
        Notice::Alerts { count: 1, latest } => {
            t!("overview.status_alert", name = latest).to_string()
        }
        Notice::Alerts { count, latest } => {
            t!("overview.status_alerts", count = count, name = latest).to_string()
        }
        Notice::Busy(name) => t!("overview.status_busy", name = name).to_string(),
        Notice::Growing(name) => t!("overview.status_growing", name = name).to_string(),
    };
    Some(
        h_flex()
            .id("overview-notice")
            .h(px(NOTICE_ROW))
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .rounded(px(10.))
            .bg(Hsla::from(fill))
            .hover(|d| d.bg(Hsla::from(halo)))
            .tooltip(widgets::wrap_tooltip(i18n::tr("overview.status_tip")))
            .on_click(|_, _window, cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.set_tab(Tab::Alerts, cx));
            })
            .child(
                // The dot in its halo: the same colour, thinned, 3px out.
                div()
                    .flex()
                    .flex_none()
                    .size(px(13.))
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(Hsla::from(halo))
                    .child(div().size(px(7.)).rounded_full().bg(Hsla::from(dot))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::text())
                    .child(sentence),
            )
            // The row opens Alerts, and says so the way every row that
            // opens does (`widgets::expand_mark`'s reason).
            .child(
                Icon::new(IconName::ChevronRight)
                    .with_size(Size::Size(px(12.)))
                    .text_color(Hsla::from(theme::text_muted())),
            )
            .into_any_element(),
    )
}

/// Height of the notice row. With the gap under it, what the panel
/// grows by while the row is shown: [`placement::NOTICE_HEIGHT`], which
/// a `const` assertion below holds to this.
const NOTICE_ROW: f32 = 36.;
const _: () = assert!(NOTICE_ROW + super::CARD_GAP == placement::NOTICE_HEIGHT);

/// A link in the page's own voice: a word and a chevron, in `ink`.
/// The form switch and the way into the disk-space window were two
/// filled pills, the heaviest marks on a page whose news is the
/// sentence beside them.
fn quiet_link(
    id: &'static str,
    label: String,
    tip: String,
    ink: Rgba,
    go: impl Fn(&mut App) + 'static,
) -> AnyElement {
    h_flex()
        .id(id)
        .flex_none()
        .items_center()
        .gap(px(4.))
        .rounded(px(7.))
        .px(px(8.))
        .py(px(4.))
        .tooltip(widgets::wrap_tooltip(tip))
        .hover(|d| d.bg(theme::chip()))
        .on_click(move |_, _window, cx| go(cx))
        .child(
            div()
                .text_size(px(12.))
                .line_height(px(16.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(ink)
                .child(label),
        )
        .child(
            Icon::new(IconName::ChevronRight)
                .with_size(Size::Size(px(12.)))
                .text_color(Hsla::from(ink)),
        )
        .into_any_element()
}

/// The boot volume, when the snapshot lists it.
fn boot_disk(snapshot: &SystemSnapshot) -> Option<&DiskSnapshot> {
    snapshot
        .disks
        .as_deref()
        .and_then(|disks| disks.iter().find(|disk| disk.mount_point == "/"))
}

/// The hottest sensor in degrees — the figure the Hardware tab's
/// sensor card leads with, read the same way.
fn hottest(snapshot: &SystemSnapshot) -> Option<f32> {
    snapshot
        .temperatures
        .as_deref()
        .filter(|temps| !temps.is_empty())
        .map(|temps| temps.iter().map(|t| t.celsius).fold(f32::MIN, f32::max))
}

/// The short form's card: the processor — its figure and the line of
/// its last half hour — and under a hairline four facts in one form,
/// memory, the disk, the network and the hottest sensor.
///
/// **One picture and four facts.** The card began as three rows of one
/// grammar — a name, a percent at 20px, a bar — for processor, memory
/// and disk, with the network and the sensor as two small facts under
/// them, and came apart one question at a time on 2026-10-12. The
/// processor's bar became a line ([`cpu_pulse`] says why: what flows
/// is a shape). Memory's and the disk's bars went next: each said the
/// figure beside it a second time, and they were the two brightest
/// marks on the page for the two things on it that do not change in a
/// day. That left two rows that were a name, a few words and a large
/// percent — a third form on a card that already had two, for figures
/// that are read the way the network's are: looked up, not watched.
/// They are facts now, in the facts' form.
///
/// Each of the two keeps the words that say what its percent means:
/// how much memory is still available (`available_bytes`, never total
/// minus used — a Mac at 77% used is an ordinary Mac, and a bare
/// "77%" is the most misread figure on one), how much of the disk is
/// free, which is the figure people act on and here the way into the
/// disk-space window.
///
/// **A fact is red only when an alert is behind it** — the verdicts
/// that turn the menu bar (`memory_needs_attention`,
/// `disk_needs_attention`), the same ones that put a red notice above
/// this card. Not the kernel's pressure level — at critical that is a
/// word ahead of memory's and no more, and at warning it is nothing
/// here at all, because that tier is where a memory-heavy Mac sits all
/// day — and not a figure past some line of this page's own: a red
/// figure under a grey bell and no red sentence told three stories at
/// once.
fn vitals_card(
    state: &ZStatsAppState,
    snapshot: &SystemSnapshot,
    recent: Recent<'_>,
) -> AnyElement {
    card()
        .child(processor_row(&snapshot.cpu, recent))
        .child(facts(state, snapshot))
        .into_any_element()
}

/// The card's head: the name, how far back the line goes, the figure,
/// and the line.
fn processor_row(cpu: &CpuSnapshot, recent: Recent<'_>) -> AnyElement {
    let span = recent.cpu.span(recent.now).map(|span| {
        font::tabular(div())
            .min_w_0()
            .truncate()
            .text_size(px(12.))
            .line_height(px(16.))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme::text_muted())
            .child(t!("overview.pulse_span", span = series::span_label(span)).to_string())
    });
    v_flex()
        .gap(px(8.))
        .child(
            h_flex()
                .items_baseline()
                .justify_between()
                .gap(px(8.))
                .child(
                    h_flex()
                        .min_w_0()
                        .items_baseline()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(13.))
                                .line_height(px(16.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .child(i18n::tr("overview.processor")),
                        )
                        .children(span),
                )
                .child(
                    // The figure large, its unit a size and an ink down.
                    h_flex()
                        .flex_none()
                        .items_baseline()
                        .gap(px(1.))
                        .child(
                            // The line box is set by hand: gpui's
                            // default is 1.618 × the size, 32px for
                            // this figure.
                            font::tabular(div())
                                .text_size(px(20.))
                                .line_height(px(24.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .child(format::whole_pct(cpu.usage_percent)),
                        )
                        .child(
                            div()
                                .text_size(px(13.))
                                .line_height(px(16.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::text_muted())
                                .child("%"),
                        ),
                ),
        )
        .child(cpu_pulse(recent))
        .into_any_element()
}

/// Height of the short form's processor chart: the long form's well
/// less a little, since it carries no caption under it. With
/// [`CHART_PAD`] inside, 28 of swing.
const PULSE_H: f32 = 36.;

/// The processor row's picture in the short form, and the card's only
/// one: the last half hour as a line.
///
/// **What flows gets a line; what fills is its figure.** All three rows
/// had a bar, for the sake of one grammar down the card. The
/// processor's was the one that said nothing — at 7% it is an empty
/// track, and the figure is a reading that will be different in two
/// seconds; what a glance wants from it is whether the Mac has been
/// busy, which is a shape and not a level. Memory's and the disk's said
/// the figure beside them a second time, and were the two brightest
/// marks on the page for the two things on it that do not change in a
/// day: once the processor had a picture that adds something, they
/// were plainly the ones that did not. A half hour of memory as bars
/// was asked for with the line and not built, for the same reason — it
/// is sixty bars of one height, which the long form dims.
///
/// The long form's chart without its caption line: the same well, the
/// same averaged points and axis rule ([`cpu_axis_top`]), the top of
/// the axis in the corner once the stroke is a line
/// ([`stroke_is_a_line`]). The span is the word beside the row's name
/// and the peak is the line's own highest point; a "Max" and a "Last"
/// under it were another line of the card's height for figures the
/// long form is there to give.
fn cpu_pulse(recent: Recent<'_>) -> AnyElement {
    let points = chart_buckets(recent.cpu.points(), recent.now);
    let scale = cpu_axis_top(&points);
    let span = recent.cpu.span(recent.now);
    let axis = stroke_is_a_line(&points);
    div()
        .id("cpu-pulse")
        .relative()
        .w_full()
        .h(px(PULSE_H))
        .rounded(px(CHART_WELL_RADIUS))
        .bg(theme::inset())
        .overflow_hidden()
        .tooltip(widgets::wrap_tooltip(format!(
            "{} {}",
            i18n::tr("overview.cpu_curve_tip"),
            i18n::tr("overview.chart_gap_tip")
        )))
        .child(sparkline(
            vec![points],
            span,
            recent.now,
            scale,
            CurveStroke::Smooth,
        ))
        .when(axis, |well| {
            well.child(corner_label(scale_label(ChartUnit::Percent, scale)))
        })
        .into_any_element()
}

/// The card's facts, under a hairline: memory and the disk, then the
/// network's two rates and the hottest sensor. Two to a row, each a
/// small label over its value.
fn facts(state: &ZStatsAppState, snapshot: &SystemSnapshot) -> AnyElement {
    let mem = &snapshot.memory;
    let memory_alarm = state.memory_needs_attention();
    // What is left, in the kernel's own count. The pressure level
    // joins it at critical.
    let available = t!(
        "overview.mem_available",
        n = format::gb(mem.available_bytes)
    )
    .to_string();
    let critical = mem
        .pressure_level
        .is_some_and(|level| level >= PRESSURE_CRITICAL);
    let memory_words = if critical {
        format!("{} · {available}", i18n::tr("overview.pressure_critical"))
    } else {
        available
    };
    let memory = fact(
        i18n::tr("overview.fact_memory"),
        share(
            mem.used_percent,
            memory_alarm,
            fact_text(memory_words, fact_ink(memory_alarm)),
        ),
    );
    let disk = boot_disk(snapshot).map(|disk| {
        let hot = state.disk_needs_attention();
        fact(
            i18n::tr("overview.fact_disk"),
            share(
                disk.used_percent,
                hot,
                storage_fact(
                    t!(
                        "overview.disk_free",
                        n = format::capacity(disk.available_bytes)
                    )
                    .to_string(),
                    hot,
                ),
            ),
        )
    });

    let io = &snapshot.io_totals;
    let (down, up) = (
        io.network_received_bytes_per_sec,
        io.network_transmitted_bytes_per_sec,
    );
    // The one value on the page in the mono face (`font`'s module doc):
    // a rate rewrites its digits and its unit every couple of seconds.
    // One space between the directions. Two put the pair a couple of
    // pixels over this fact's half of the card — JetBrains Mono at 12px,
    // "↓ 27 kB/s  ↑ 154 kB/s" is 151 against a 149 column — and
    // `truncate` then ate the unit, so the line read "154 kB…".
    let network = (down.is_some() || up.is_some()).then(|| {
        fact(
            i18n::tr("overview.fact_network"),
            div()
                .font_family(font::MONO)
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(theme::text())
                .truncate()
                .child(format!("↓ {} ↑ {}", format::rate(down), format::rate(up)))
                .into_any_element(),
        )
    });
    let heat = hottest(snapshot).map(|hottest| {
        fact(
            i18n::tr("overview.fact_hottest"),
            fact_text(format!("{hottest:.0} °C"), theme::text()),
        )
    });
    let second = (network.is_some() || heat.is_some())
        .then(|| h_flex().gap(px(12.)).children(network).children(heat));
    v_flex()
        .mt(px(16.))
        .gap(px(12.))
        .child(div().h(px(1.)).bg(theme::border_subtle()))
        .child(h_flex().gap(px(12.)).child(memory).children(disk))
        .children(second)
        .into_any_element()
}

/// One fact: a small label over its value, half the card wide.
fn fact(label: String, value: AnyElement) -> AnyElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap(px(4.))
        .child(
            div()
                .text_size(px(11.))
                .line_height(px(14.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::text_dim())
                .child(label),
        )
        .child(value)
        .into_any_element()
}

/// A fact's value in words or figures, in the page's face.
fn fact_text(text: String, ink: Rgba) -> AnyElement {
    font::tabular(div())
        .min_w_0()
        .truncate()
        .text_size(px(12.))
        .line_height(px(16.))
        .text_color(ink)
        .child(text)
        .into_any_element()
}

/// Ink of the words after a share: the alarm's when an alert is behind
/// the fact, the second ink when not.
fn fact_ink(hot: bool) -> Rgba {
    if hot {
        theme::accent_light()
    } else {
        theme::text_muted()
    }
}

/// The value of a fact that is a share of something: the percent in the
/// first ink — the alarm's when `hot` — then what the percent leaves.
fn share(percent: f32, hot: bool, rest: AnyElement) -> AnyElement {
    h_flex()
        .min_w_0()
        .items_baseline()
        .gap(px(8.))
        .child(
            font::tabular(div())
                .flex_none()
                .text_size(px(12.))
                .line_height(px(16.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::text_for(hot))
                .child(format!("{}%", format::whole_pct(percent))),
        )
        .child(rest)
        .into_any_element()
}

/// How much of the disk is free, as the way into the disk-space
/// window: a [`quiet_link`] in the ink of the words after memory's
/// percent, so the two facts read as one shape and the chevron is what
/// says this one opens. It was the words "Free up space" in the
/// accent, at the end of a line of small print the card no longer has.
fn storage_fact(free: String, hot: bool) -> AnyElement {
    // The link's hover box, pulled back so its words start where
    // memory's do and the value keeps a line's height.
    div()
        .ml(px(-8.))
        .my(px(-4.))
        .child(quiet_link(
            "overview-storage",
            free,
            i18n::tr("disk.storage_hint"),
            fact_ink(hot),
            crate::open_storage_window,
        ))
        .into_any_element()
}

/// The half-hour series behind the three charts, stamped once per paint.
#[derive(Clone, Copy)]
struct Recent<'a> {
    cpu: &'a series::Series,
    memory: &'a series::Series,
    net_down: &'a series::Series,
    net_up: &'a series::Series,
    now: Instant,
}

/// A tree's recent minutes must sit this many percent-of-one-core
/// points above its earlier-hour average before the card calls it
/// climbing. Below ~an eighth of a core the delta is scheduler mood and
/// short blips — ranking by it would be ranking noise. Display only,
/// like every threshold in `views/`: a climb fires nothing.
const RISE_FLOOR: f32 = 15.0;

/// Compact "who changed" list — whole trees, ranked by their hour-window
/// climb (`trend.rs`) when anything is climbing, by current CPU when
/// nothing is. The panel usually gets opened because the machine *got*
/// loud, and the instantaneous top cannot answer that: the resident
/// that is always first is normal, the tree that climbed out of nowhere
/// is the reason — and a steady 30% outranks a 2%→21% climber in any
/// snapshot ranking. Always [`TOP_N`] rows: climbers take the top, the
/// rest of the slots keep the current CPU ranking so a quiet climb
/// (one tree) does not leave the card rows short of the window.
/// All still opens the full Apps list.
/// How many rows the apps card shows: `base` at the form's own height,
/// and one more for every row's worth of panel the reader dragged on
/// top of it. Each form's page was measured to end on the gap above the
/// footer at its own height, so what a taller panel adds is that much
/// empty panel under the last card — at 780 the short form stood over a
/// band of 241 with nothing in it. The ranking is what grows into it:
/// the card that can, and the one whose next rows are the next answers
/// to the question it asks. A panel cut shorter by a screen keeps
/// `base` and scrolls, as it did.
fn apps_rows(state: &ZStatsAppState, detail: bool, base: usize) -> usize {
    let Some(bounds) = state.window_bounds() else {
        return base;
    };
    let notice = !detail && has_notice(state);
    let (_, own) = placement::window_size(detail, notice, placement::Dragged::default());
    let spare = f32::from(bounds.size.height) - own;
    if spare < APP_ROW {
        return base;
    }
    base + (spare / APP_ROW).floor() as usize
}

fn top_apps(state: &ZStatsAppState, rows: usize) -> AnyElement {
    let Some(tick) = state.latest() else {
        return widgets::empty_card(
            i18n::tr("overview.top_cpu"),
            i18n::tr("common.waiting_sample"),
        );
    };
    let Some(groups) = tick.snapshot.process_groups.as_deref() else {
        return widgets::empty_card(
            i18n::tr("overview.top_cpu_off"),
            i18n::tr("overview.top_cpu_off_body"),
        );
    };
    let processes = tick
        .snapshot
        .processes
        .as_deref()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let topology = state.member_processes().unwrap_or(processes);
    let mut risers: Vec<_> = groups
        .iter()
        .filter_map(|g| {
            state
                .app_rise(trend::tree_key(g))
                .filter(|delta| *delta >= RISE_FLOOR)
                .map(|delta| (g, delta))
        })
        .collect();
    // Two modes, one card: risers when there are any, the current top
    // otherwise — a quiet hour ranked by ±noise deltas would be worse
    // than the ranking it replaced. The header says which question is
    // being answered.
    let (title_key, tip_key, rows): (_, _, Vec<_>) = if risers.is_empty() {
        (
            "overview.top_cpu",
            "overview.top_apps_tip",
            pad_rising(Vec::new(), groups, rows),
        )
    } else {
        risers.sort_by(|a, b| b.1.total_cmp(&a.1));
        (
            "overview.rising_title",
            "overview.rising_tip",
            pad_rising(risers, groups, rows),
        )
    };
    // In the climbing mode, the rows after the climbers are the current
    // CPU ranking filling the card — not climbers. Without a mark the
    // title called all three "climbing". A firmer hairline above the
    // first of them and a word in its rise slot set them apart, and
    // neither costs a pixel of height (this card is in the panel's fixed
    // budget).
    let first_fill = rows
        .first()
        .is_some_and(|(_, delta)| delta.is_some())
        .then(|| rows.iter().position(|(_, delta)| delta.is_none()))
        .flatten();

    let top = title_key == "overview.top_cpu";
    // No hairlines between the rows: each is its own rounded hover box,
    // and five lines under five names were five more things to read.
    widgets::list_shell()
        .p(px(4.))
        .child(
            h_flex()
                .h(px(36.))
                .px(px(8.))
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    h_flex()
                        .min_w_0()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .child(i18n::tr(title_key)),
                        )
                        // What the list is ranked by. The climbing
                        // mode's title already says.
                        .when(top, |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .text_size(px(12.))
                                    .text_color(theme::text_dim())
                                    .child(i18n::tr("overview.top_by_cpu")),
                            )
                        })
                        .child(widgets::info_icon("overview-top-apps", i18n::tr(tip_key))),
                )
                .child(top_apps_all()),
        )
        .children(rows.into_iter().enumerate().map(|(i, (g, delta))| {
            // Burst only: a tree has no sustained flag of its own, and
            // walking members here would make Overview a second Apps tab.
            let hot = processes::is_hot(f64::from(g.cpu_usage_percent), false)
                && !processes::own_check_cpu(g.root_pid, state);
            let root_pid = g.root_pid;
            // Face, not [`trend::tree_key`]: a login compile shows as
            // cargo, a compile in Zed's terminal as `Zed · cargo`. The
            // rise is still looked up on the launchd child so the hour
            // does not split.
            let face = trend::tree_face(g, topology, processes, state.member_pgids());
            let pct = format::pct(g.cpu_usage_percent);
            h_flex()
                .id(("top-app-row", root_pid as usize))
                .h(px(APP_ROW))
                .px(px(8.))
                .items_center()
                .gap(px(10.))
                .rounded(px(7.))
                // The row answers "who" — the Apps tab answers the next
                // question, so a click lands there with this tree open
                // and scrolled into view. Hover fill is the affordance,
                // same as the Apps rows the click leads to.
                .hover(|d| d.bg(theme::surface_raised()))
                .on_click(move |_, _window, cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.reveal_app(root_pid, cx));
                })
                .child(widgets::app_mark(root_pid, &g.name, &face.title))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(widgets::truncating_name_tailed(
                            ("top-app-name", g.root_pid as usize),
                            face.title,
                            face.job.map(gpui::SharedString::from),
                            13.,
                            gpui::FontWeight::NORMAL,
                            Hsla::from(theme::text()),
                        )),
                )
                // Climb + current as one right-hand cluster. Muted on
                // purpose: a rise is news, not a threshold, and the
                // alarm's ink stays reserved for over-the-line.
                .child(
                    h_flex()
                        .flex_none()
                        .items_baseline()
                        .gap(px(8.))
                        // In the climbing mode, the rows after the
                        // climbers are the current ranking filling the
                        // card — not climbers. A word in the first one's
                        // rise slot says where they start.
                        .when(Some(i) == first_fill, |cluster| {
                            cluster.child(
                                div()
                                    .id(("top-app-fill", g.root_pid as usize))
                                    .flex_none()
                                    .text_size(px(11.))
                                    .text_color(theme::text_dim())
                                    .tooltip(widgets::wrap_tooltip(i18n::tr(
                                        "overview.busy_now_tip",
                                    )))
                                    .child(i18n::tr("overview.busy_now")),
                            )
                        })
                        .children(delta.map(|delta| {
                            font::tabular(div())
                                .id(("top-app-rise", g.root_pid as usize))
                                .flex_none()
                                .text_size(px(11.))
                                .text_color(theme::text_muted())
                                .tooltip(widgets::wrap_tooltip(
                                    t!("overview.rise_row_tip", delta = format::pct(delta))
                                        .to_string(),
                                ))
                                .child(format!("↑{}", format::pct(delta)))
                        }))
                        .child(
                            font::tabular(div())
                                .id(("top-app-pct", g.root_pid as usize))
                                .flex()
                                .flex_none()
                                .min_w(px(44.))
                                .justify_end()
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(app_pct_ink(g.cpu_usage_percent, hot))
                                .tooltip(widgets::wrap_tooltip(i18n::tr(
                                    "overview.top_app_pct_tip",
                                )))
                                .child(pct),
                        ),
                )
        }))
        .into_any_element()
}

/// Climbers first (with their delta), then current CPU to fill `n`.
/// A two-tree climb used to leave the card three rows short of the
/// window it was sized for.
fn pad_rising<'a>(
    risers: Vec<(&'a ProcessGroupSnapshot, f32)>,
    groups: &'a [ProcessGroupSnapshot],
    n: usize,
) -> Vec<(&'a ProcessGroupSnapshot, Option<f32>)> {
    let mut rows: Vec<_> = risers
        .into_iter()
        .take(n)
        .map(|(g, d)| (g, Some(d)))
        .collect();
    if rows.len() >= n {
        return rows;
    }
    let taken: HashSet<&str> = rows.iter().map(|(g, _)| trend::tree_key(g)).collect();
    let mut rest: Vec<_> = groups
        .iter()
        .filter(|g| !taken.contains(trend::tree_key(g)))
        .collect();
    rest.sort_by(|a, b| b.cpu_usage_percent.total_cmp(&a.cpu_usage_percent));
    rows.extend(rest.into_iter().take(n - rows.len()).map(|g| (g, None)));
    rows
}

fn top_apps_all() -> AnyElement {
    // The chevron says this opens. Accent stays on the selected tab and
    // on the processor's line; a champagne "All" outranked every row.
    quiet_link(
        "top-apps-all",
        i18n::tr("overview.top_cpu_all"),
        Tab::Apps.title(),
        theme::text_muted(),
        |cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.set_tab(Tab::Apps, cx));
        },
    )
}

fn processor(
    cpu: &CpuSnapshot,
    load: &LoadSnapshot,
    uptime_secs: u64,
    watts: Option<u32>,
    recent: Recent<'_>,
) -> AnyElement {
    let header_right = processor_caption(cpu, watts);
    let mut body = card()
        .child(widgets::card_header(
            i18n::tr("overview.processor"),
            Some(header_right),
        ))
        .child(
            // The figure stands at the card's edge and the two
            // footnotes hug the right, on its baseline. Load used to
            // follow the figure, which made the figure's width part of
            // the layout: it was padded to two digits so 9 → 10 did not
            // push the caption, and a lone "7" stood a digit in from
            // the title above it. Anchored right, nothing follows the
            // figure and it needs no pad.
            h_flex()
                .w_full()
                .items_baseline()
                .justify_between()
                .gap(px(10.))
                .mt(px(4.))
                .child(
                    div()
                        .id("cpu-usage")
                        .flex_none()
                        .tooltip(widgets::wrap_tooltip(i18n::tr("overview.usage_tip")))
                        .child(widgets::big_number(
                            format::whole_pct(cpu.usage_percent),
                            "%",
                        )),
                )
                .child(
                    h_flex()
                        .items_baseline()
                        .justify_end()
                        .gap(px(12.))
                        .min_w_0()
                        .child(load_caption(load, cpu.logical_cores))
                        .child(uptime_caption(uptime_secs)),
                ),
        )
        .children(recent_line(
            Chart {
                id: "cpu-curve",
                lines: vec![ChartLine {
                    series: recent.cpu,
                    arrow: None,
                }],
                scale: cpu_axis_top(&chart_buckets(recent.cpu.points(), recent.now)),
                tip: format!(
                    "{} {}",
                    i18n::tr("overview.cpu_curve_tip"),
                    i18n::tr("overview.chart_gap_tip")
                ),
                // Each point is a 20s average. The cubic follows those and
                // does not rise above one that was the peak.
                stroke: CurveStroke::Smooth,
                unit: ChartUnit::Percent,
                icon: None,
                corner_label: true,
            },
            recent.now,
        ));

    // Apple Silicon and friends: usage split by performance cluster.
    if let Some(levels) = cpu.perf_levels.as_ref().filter(|l| l.len() > 1) {
        body = body.child(
            v_flex()
                .pt(px(8.))
                .gap(px(6.))
                .children(levels.iter().map(|level| {
                    let over = level.usage_percent > CORE_HOT;
                    h_flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .w(px(96.))
                                .flex_none()
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::text_muted())
                                .truncate()
                                .child(cluster_label(&level.name, level.logical_cores)),
                        )
                        .child(div().flex_1().child(widgets::meter(
                            level.usage_percent / 100.0,
                            Hsla::from(theme::gauge_for(over)),
                            5.,
                        )))
                        .child(
                            div()
                                .w(px(44.))
                                .flex_none()
                                .font_features(font::tnum())
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight::NORMAL)
                                .text_color(theme::text())
                                .child(format::pct_col(level.usage_percent)),
                        )
                })),
        );
    }

    body.into_any_element()
}

/// Brand plus the reported clock, e.g. "Apple M4 Pro (4.5 GHz)", and
/// the live draw when the battery reports one.
///
/// Apple Silicon does not expose live per-cluster MHz through sysinfo;
/// this is the rated clock and usually never moves. zstats still only
/// *asks* for it every 30s (cheaper than every usage sample) — that is
/// a collect cadence, not a claim that the number changes.
///
/// Watts sit here rather than beside CPU%: identity (which chip, what
/// it is drawing) vs activity (how busy, how long). The brand is the
/// one that ellipsises — watts is `flex_none`, and the text has to live
/// on the truncating div itself (`note()` wrapping it once shoved the
/// figure off the card).
fn processor_caption(cpu: &CpuSnapshot, watts: Option<u32>) -> AnyElement {
    let freq = cpu
        .frequency_mhz
        .map(|mhz| format!("{:.1} GHz", mhz as f64 / 1000.0));
    let text = match (cpu.brand.as_deref(), freq.as_deref()) {
        (Some(brand), Some(freq)) => {
            t!("overview.brand_freq", brand = brand, freq = freq).to_string()
        }
        (Some(brand), None) => brand.to_string(),
        (None, Some(freq)) => freq.to_string(),
        (None, None) => i18n::tr("overview.freq_unknown"),
    };
    let tip = i18n::tr("overview.freq_tip");
    h_flex()
        .min_w_0()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .id("cpu-brand")
                .min_w_0()
                .truncate()
                .text_size(px(10.))
                .text_color(theme::text_dim())
                .tooltip(widgets::wrap_tooltip(tip))
                .child(text),
        )
        .children(watts.map(watts_caption))
        .into_any_element()
}

/// The 1 / 5 / 15-minute load averages beside the usage figure. Usage
/// says how busy the cores are; load says how much work is *waiting*
/// for one — a machine pinned at 100% with load 3 and one at 100% with
/// load 30 are two different conditions, and only the count tells them
/// apart. Footnote-sized on purpose: it qualifies the headline, it does
/// not compete with it. The tooltip carries the core count so the
/// reader has the line to compare against. Display only — zstats has
/// no load rule, and this adds none.
fn load_caption(load: &LoadSnapshot, cores: u32) -> AnyElement {
    let text = t!(
        "overview.load",
        one = format::load(load.load1),
        five = format::load(load.load5),
        fifteen = format::load(load.load15)
    )
    .to_string();
    let tip = t!("overview.load_tip", cores = cores).to_string();
    div()
        .id("cpu-load")
        .font_features(font::tnum())
        .text_size(px(10.))
        .text_color(theme::text_dim())
        .tooltip(widgets::wrap_tooltip(tip))
        .child(text)
        .into_any_element()
}

/// Live draw, beside the chip name. Whole watts from [`format::whole_watts`];
/// the caller already dropped zero. Absent on desktops and VMs.
fn watts_caption(watts: u32) -> AnyElement {
    div()
        .id("cpu-watts")
        .flex_none()
        .font_features(font::tnum())
        .text_size(px(10.))
        .text_color(theme::text_dim())
        .tooltip(widgets::wrap_tooltip(i18n::tr("overview.watts_tip")))
        .child(format!("{watts} W"))
        .into_any_element()
}

/// Time since boot. Always present — even a just-booted machine has a
/// number — and it is the sentence Overview was missing: how long this
/// load has had to build.
fn uptime_caption(secs: u64) -> AnyElement {
    div()
        .id("cpu-uptime")
        .flex_none()
        .font_features(font::tnum())
        .text_size(px(10.))
        .text_color(theme::text_dim())
        .tooltip(widgets::wrap_tooltip(i18n::tr("overview.uptime_tip")))
        .child(t!("overview.uptime", time = format::uptime_short(secs)).to_string())
        .into_any_element()
}

/// Activity Monitor / Stats wording: "P-cores · 8", not "P·8".
fn cluster_label(name: &str, cores: u32) -> String {
    let pretty = match name {
        "P" | "p" | "Performance" | "performance" => i18n::tr("overview.p_cores"),
        "E" | "e" | "Efficiency" | "efficiency" => i18n::tr("overview.e_cores"),
        other => other.to_string(),
    };
    format!("{pretty} · {cores}")
}

/// How many climbers the tag's tooltip lists. The tag itself names the
/// biggest; three is as far as "who" stays a glance.
const MEM_RISE_N: usize = 3;

/// The climbers worth naming: past the floor, in the store's order
/// (largest climb first).
fn mem_climbers(climbers: &[MemoryCreep], total_bytes: u64) -> Vec<&MemoryCreep> {
    let floor = trend::mem_rise_floor(total_bytes);
    climbers.iter().filter(|c| c.climb_bytes >= floor).collect()
}

/// Who has been climbing this hour and is still up, at the right of
/// the headline row — `Climbing  Zed +1.1 GB`, with `×3` on the lead
/// when there are more and the rest in the tooltip. The hour's answer
/// to the question the History tab asks of the day, and the one the
/// memory rules cannot ask at all (a climb crosses no line until it is
/// too late).
///
/// On the headline's line, not under the card's last one, where it used
/// to be a strip of its own: a climb comes and goes, the strip came and
/// went with it, and every card below moved by a line each time — in a
/// panel whose height is fitted to this page. The row it sits on is
/// always there and taller than the tag, so the card is the same height
/// with it or without. The name gives way before the figure does: the
/// delta is the news.
fn mem_climb_tag(climbers: &[MemoryCreep], total_bytes: u64) -> Option<AnyElement> {
    let named = mem_climbers(climbers, total_bytes);
    let top = named.first()?;
    let lead = if named.len() > 1 {
        format!("{} ×{}", i18n::tr("overview.mem_rise_short"), named.len())
    } else {
        i18n::tr("overview.mem_rise_short")
    };
    let listed = named
        .iter()
        .take(MEM_RISE_N)
        .map(|c| {
            t!(
                "overview.mem_rise_item",
                name = c.name.clone(),
                delta = format::memory(c.climb_bytes)
            )
            .to_string()
        })
        .collect::<Vec<_>>()
        .join(" · ");
    Some(
        h_flex()
            .id("mem-climbers")
            .items_baseline()
            .justify_end()
            .gap(px(6.))
            .min_w_0()
            .text_size(px(10.))
            .tooltip(widgets::wrap_tooltip_lines(vec![
                format!("{}  {listed}", i18n::tr("overview.mem_rise_lead")).into(),
                i18n::tr("overview.mem_rise_tip").into(),
            ]))
            .child(div().flex_none().text_color(theme::text_dim()).child(lead))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_features(font::tnum())
                    .text_color(theme::text_muted())
                    .child(top.name.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .font_features(font::tnum())
                    .text_color(theme::text_muted())
                    .child(format!("+{}", format::memory(top.climb_bytes))),
            )
            .into_any_element(),
    )
}

/// The kernel's available percent for the pressure badge — only beside a
/// verdict. Without a level the badge reads `—` or "not measured", and a
/// percentage hung on either would be a figure with nothing to explain.
fn badge_available(level: Option<u32>, kernel_available: Option<u32>) -> Option<u32> {
    level.and(kernel_available)
}

/// Compressor segments moved per second, as (out, in) — `None` while both
/// are still, not yet measured (the first sample), or not measurable (off
/// macOS). A settled machine gets no clause rather than a pair of zeros:
/// zstats' own pressure line makes the same call, and it is what lets the
/// memory card and the pressure alert's sentence grow only when there is
/// something to read.
pub(crate) fn swap_moving(ins: Option<u64>, outs: Option<u64>) -> Option<(u64, u64)> {
    match (outs, ins) {
        (Some(outs), Some(ins)) if outs > 0 || ins > 0 => Some((outs, ins)),
        _ => None,
    }
}

/// One line under Swap / Compressed while the compressor is swapping:
/// `Swapping  out 120/s · in 40/s`. The badge says memory is tight; this
/// says how hard macOS is working to stay there — a warning with nothing
/// moving is a machine that has settled, one at hundreds a second is
/// still fighting. Absent otherwise — and at the end of the Swap row
/// rather than on a line under it, for the climb tag's reason: a line
/// that comes and goes moves everything below it. The row's own label
/// is the lead, so the tag is the figures alone — or, when the kernel
/// says so, its "Thrashing" verdict alone.
///
/// Segments are the kernel's unit — bundles of compressed pages, no fixed
/// size — so they are shown as activity and never converted to bytes.
/// `swap_thrashing` is the kernel's own detector (segments written out
/// only to be read straight back): a verdict, so it recolours the lead
/// in accent and is never compared to anything here.
fn swap_activity_tag(mem: &MemorySnapshot) -> Option<AnyElement> {
    let thrashing = mem.swap_thrashing == Some(true);
    let moving = swap_moving(mem.swap_ins_per_sec, mem.swap_outs_per_sec);
    if !thrashing && moving.is_none() {
        return None;
    }
    let (outs, ins) = moving.unwrap_or((0, 0));
    let figures = t!(
        "overview.swap_activity",
        outs = format::thousands(outs as usize),
        ins = format::thousands(ins as usize)
    )
    .to_string();
    let tip = i18n::tr("overview.swap_activity_tip");
    let tag = h_flex()
        .id("mem-swapping")
        .items_baseline()
        .justify_end()
        .min_w_0()
        .text_size(px(10.));
    Some(if thrashing {
        // The verdict alone, and the figures on hover: both do not fit
        // beside the Swap pair, and what a row cut short loses is the
        // end of a number — "in 4…" for 400. The word is the news.
        tag.tooltip(widgets::wrap_tooltip_lines(vec![
            figures.into(),
            tip.into(),
        ]))
        .child(
            div()
                .flex_none()
                .text_color(theme::accent())
                .child(i18n::tr("overview.swap_thrashing")),
        )
        .into_any_element()
    } else {
        tag.tooltip(widgets::wrap_tooltip(tip))
            .child(
                font::mono_unless_cjk(div())
                    .min_w_0()
                    .truncate()
                    .text_color(theme::text_muted())
                    .child(figures),
            )
            .into_any_element()
    })
}

/// `caps` decides how an absent figure reads: this build cannot measure
/// it, or it can and has not yet. On macOS every capability is true, so
/// every branch here resolves exactly as it did before 0.5.2.
fn memory(
    mem: &MemorySnapshot,
    caps: Capabilities,
    climbers: Vec<MemoryCreep>,
    alarm: bool,
    recent: Recent<'_>,
) -> AnyElement {
    // The kernel's own verdict, not a number we derive: 1 normal, 2 warning,
    // 4 critical. Absent has two readings and they are not the same
    // sentence: this build cannot measure pressure at all, or it can and
    // has nothing yet. zstats 0.5.2 answers the first through
    // `capabilities`, so the panel stops inferring it from a `None` —
    // on macOS the capability is true and every arm below reads exactly
    // as it did.
    let supported = caps.memory_pressure;
    let (label, tip) = match mem.pressure_level {
        Some(l) if l >= PRESSURE_CRITICAL => (
            i18n::tr("overview.pressure_critical"),
            i18n::tr("overview.pressure_tip_critical"),
        ),
        Some(l) if l >= 2 => (
            i18n::tr("overview.pressure_warning"),
            i18n::tr("overview.pressure_tip_warning"),
        ),
        Some(_) => (
            i18n::tr("overview.pressure_normal"),
            i18n::tr("overview.pressure_tip_normal"),
        ),
        None if !supported => (
            i18n::tr("overview.pressure_none"),
            i18n::tr("overview.pressure_tip_none"),
        ),
        // Measurable here, just not measured yet — the same `—` every
        // other waiting figure shows, not a claim about the platform.
        None => (
            format::PLACEHOLDER.to_string(),
            i18n::tr("overview.pressure_tip_waiting"),
        ),
    };
    // The badge is red when an alert is behind memory — the verdict that
    // turns the menu bar and the short form's memory row — and never for
    // the kernel's level alone. It was red at "Warning", which is where
    // a memory-heavy Mac sits all day: a red pill under a grey bell, on
    // the page whose short form had nothing to report at that moment.
    // The word still says the level; a level that lasts becomes an
    // alert, and that is what the red is for.
    let (fg, bg, line) = if alarm {
        (
            Hsla::from(theme::accent_light()),
            Hsla::from(theme::accent_wash(14)),
            Hsla::from(theme::accent_wash(40)),
        )
    } else {
        (
            Hsla::from(theme::text_muted()),
            Hsla::from(theme::track()),
            Hsla::from(theme::card_line()),
        )
    };
    // The figure the verdict is derived from (`kern.memorystatus_level`),
    // printed in the badge beside the verdict — "Normal · 62% available" —
    // not as a row next to Used, where a lower "available" would read as a
    // contradiction rather than as the kernel's narrower definition. The
    // word stays in the badge: a bare "62%" beside a memory card reads as
    // used. The tooltip says why it differs from free GB. macOS only.
    let (label, tip) = match badge_available(mem.pressure_level, mem.kernel_available_percent) {
        Some(pct) => (
            t!("overview.pressure_badge", level = label, pct = pct).to_string(),
            format!("{tip} {}", t!("overview.pressure_tip_kernel", pct = pct)),
        ),
        None => (label, tip),
    };

    let total = mem.total_bytes.max(1) as f32;
    let compressed = mem.compressed_bytes.unwrap_or(0).min(mem.used_bytes);
    let resident = mem.used_bytes.saturating_sub(compressed);
    let resident_w = resident as f32 / total;
    let comp_w = compressed as f32 / total;
    // Used and compressed are painted slices. Free is the unfilled
    // trough — leftover, not a third colour that has to fight the badge.
    // One paint in two strengths: compressed is part of used, and a grey
    // slice beside the bar's own colour read as a second quantity.
    let used_fill = Hsla::from(theme::gauge());
    let compressed_fill = Hsla::from(dim_ink());

    // Total is already in the hero caption ("used of 24 GB"). The bar
    // shows compressed as a slice, and its legend names how many GB that
    // is — that number is the early pressure signal. It used to be a row
    // of its own as well, under a legend that said "Compressed" with no
    // figure: the same fact twice. Swap is the other fact the hero and
    // the bar both omit. Used and Free stay words: the slices they mark
    // are used − compressed and total − used, figures zstats does not
    // report and this app does not derive.
    // Deliberately not `mem.swap_used_percent`: that field *is* swap
    // against its own allocation, the ratio [`SWAP_HOT`] explains is
    // unusable here. Dividing those two bytes locally would also have
    // been a second copy of a figure zstats already reports, which
    // CLAUDE.md forbids. This asks something zstats does not answer, and
    // only ever to pick a colour.
    //
    // The `> 0` is not division safety — `total` is already floored at 1
    // — but a guard against a collector reporting no memory at all, where
    // that floor would turn any swap into a huge percentage and paint red.
    let swap_hot = mem.total_bytes > 0 && (mem.swap_used_bytes as f32 / total) * 100.0 >= SWAP_HOT;
    let rows = vec![(
        i18n::tr("overview.swap"),
        format!(
            "{} / {}",
            format::gb(mem.swap_used_bytes),
            format::gb(mem.swap_total_bytes)
        ),
        swap_hot,
    )];

    let mut legend = vec![(
        widgets::LegendMark::Fill(used_fill),
        i18n::tr("overview.used").into(),
        i18n::tr("overview.used_tip").into(),
    )];
    if compressed > 0 {
        legend.push((
            widgets::LegendMark::Fill(compressed_fill),
            format!(
                "{} {}",
                i18n::tr("overview.compressed"),
                format::gb(compressed)
            )
            .into(),
            i18n::tr("overview.compressed_tip").into(),
        ));
    }
    legend.push((
        widgets::LegendMark::Hollow,
        i18n::tr("overview.free").into(),
        i18n::tr("overview.free_tip").into(),
    ));

    card()
        .child(widgets::card_header(
            i18n::tr("overview.memory"),
            Some(
                div()
                    .id("mem-pressure")
                    .flex_none()
                    .rounded_full()
                    .border_1()
                    .border_color(line)
                    .bg(bg)
                    .px(px(8.))
                    .py(px(2.))
                    .text_size(px(10.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .tooltip(widgets::wrap_tooltip(tip))
                    .child(label)
                    .into_any_element(),
            ),
        ))
        .child(
            // The processor card's headline shape: the figure and its
            // caption a left-hand pair, a small tag hugging the right.
            h_flex()
                .w_full()
                .items_baseline()
                .justify_between()
                .gap(px(10.))
                .mt(px(8.))
                .child(
                    h_flex()
                        .flex_none()
                        .items_baseline()
                        .gap(px(4.))
                        .child(widgets::big_figure(&format::gb(mem.used_bytes)))
                        .child(
                            font::tabular(div())
                                .text_size(px(12.))
                                .text_color(theme::text_muted())
                                .child(
                                    t!("overview.used_of", total = format::gb(mem.total_bytes))
                                        .to_string(),
                                ),
                        ),
                )
                .children(mem_climb_tag(&climbers, mem.total_bytes)),
        )
        .children(recent_line(
            Chart {
                id: "mem-curve",
                lines: vec![ChartLine {
                    series: recent.memory,
                    arrow: None,
                }],
                scale: MEM_AXIS_TOP,
                tip: format!(
                    "{} {}",
                    i18n::tr("overview.mem_curve_tip"),
                    i18n::tr("overview.chart_gap_tip")
                ),
                stroke: CurveStroke::Bars,
                unit: ChartUnit::Percent,
                icon: None,
                corner_label: false,
            },
            recent.now,
        ))
        .child(div().mt(px(10.)).child(widgets::stacked_meter(
            vec![(resident_w, used_fill), (comp_w, compressed_fill)],
            6.,
        )))
        .child(div().mt(px(8.)).child(widgets::legend(legend)))
        // The card's last line, whatever the machine is doing: nothing
        // that comes and goes gets a line of its own here (the two tags'
        // docs say what that cost).
        .child(widgets::kv_packed(rows, swap_activity_tag(mem)))
        .into_any_element()
}

/// Disk and network throughput: the rates row and the network chart. A
/// card of its own — they used to close the Memory card, so the network
/// chart sat under a "Memory" title. It had no title of its own until
/// 2026-10-12 — the row's two glyphs are the tab strip's Hardware and
/// Network icons, and a header cost 20px for a word they already say.
/// That held for a reader who knew the tab strip; to anyone else it was
/// the one card on the page with no name, four unlabelled rates under
/// two icons. The long form pays the 20px; the short form says the
/// network's rates in its last line ([`side_facts`]).
fn io_card(io: &IoTotalsSnapshot, recent: Recent<'_>) -> Option<AnyElement> {
    let rates = io_strip(io);
    let chart = recent_line(
        Chart {
            id: "net-curve",
            // Download leads: it is the line most people read. Upload
            // stays on the chart, dimmer, because a backup or a sync
            // pushing out is just as often why the network is busy.
            lines: vec![
                ChartLine {
                    series: recent.net_down,
                    arrow: Some("↓"),
                },
                ChartLine {
                    series: recent.net_up,
                    arrow: Some("↑"),
                },
            ],
            // One ceiling for both, so the two heights compare.
            scale: net_scale(
                &[
                    chart_buckets(recent.net_down.points(), recent.now),
                    chart_buckets(recent.net_up.points(), recent.now),
                ]
                .concat(),
            ),
            tip: format!(
                "{} {}",
                i18n::tr("overview.net_curve_tip"),
                i18n::tr("overview.chart_gap_tip")
            ),
            // Same cubic as CPU. A rate that holds between the 15s
            // refreshes has a flat tangent, so the hold stays a hold;
            // the bend is only where two readings differ.
            stroke: CurveStroke::Smooth,
            unit: ChartUnit::Rate,
            // The row above carries disk rates too; the glyph says
            // which pair this chart is.
            icon: Some(IconName::Network),
            corner_label: true,
        },
        recent.now,
    );
    // Nothing measured and nothing drawn yet: no card, rather than an
    // empty one.
    if rates.is_none() && chart.is_none() {
        return None;
    }
    Some(
        card()
            .child(widgets::card_header(i18n::tr("overview.io_title"), None))
            .child(div().mt(px(8.)).children(rates))
            .children(chart)
            .into_any_element(),
    )
}

/// Height of an Overview chart. CPU and network are curves; memory is
/// bars. Three of them, each with [`CURVE_TOP`] above and a
/// [`CHART_CAPTION`] under it, are what `placement::DEFAULT_WINDOW_SIZE`
/// grew by so this tab still ends a few pixels above the footer. The
/// well stays this tall in both themes: a ceiling change moves the
/// mark inside the slot and does not resize the card.
const CURVE_H: f32 = 40.;

/// Air above a curve. Part of the window-height budget, with [`CURVE_H`].
const CURVE_TOP: f32 = 4.;

/// Air between a chart and the caption under it. Same budget.
const CHART_CAPTION_GAP: f32 = 4.;

/// The caption itself: peak, latest sample, and collected span, in one
/// line. 10px type in 14px, so the line does not clip. Three charts
/// add this plus [`CHART_CAPTION_GAP`] to the panel height.
const CHART_CAPTION: f32 = 14.;

/// Stroke. [`CHART_PAD`] is wider than half of this, so a peak on the
/// top of the drawing is not cut by the well.
const CURVE_STROKE: f32 = 1.5;

/// Share of the lead line's alpha the wash under it starts with, at the
/// line's highest point; it fades to nothing at the axis. A line alone
/// in a 40-tall well read thin beside the memory card's bars, and the
/// wash gives a busy stretch the weight of an area. Faint on purpose:
/// sixty full-ink bars were once the brightest block in the panel for
/// the least news, and the line's shape is still the news here.
const AREA_ALPHA: f32 = 0.24;

/// Radius of the chart well. Nested inside the card's 12px corner, and
/// the same in both themes: only the fill changes with the theme.
const CHART_WELL_RADIUS: f32 = 8.;

/// How far the marks sit inside the well. The row stays [`CURVE_H`];
/// this only keeps a bar or a peak off the rounded edge.
const CHART_PAD: f32 = 4.;

/// How one run of samples is drawn.
#[derive(Clone, Copy)]
enum CurveStroke {
    /// Monotone cubic through the samples ([`smooth_cubics`]). CPU and
    /// the network rates are a new point every tick, and a polyline of
    /// those is a run of corners. The cubic bends between readings that
    /// differ and does not rise above a sample that was the peak. A
    /// network total that holds still has a flat tangent, so the hold
    /// stays a hold.
    Smooth,
    /// One bar per reading, from the axis up to the sample. Memory's
    /// percent moves slowly; a line through it reads as flat, and the
    /// bars keep each reading's share of the 40px slot.
    Bars,
}

/// CPU axis while every reading in the window stays at or under this.
/// A machine at 8% drawn on 0–100 is a stripe on the floor; 0–30 lets
/// that range use the slot. 12% then 14% stays on this top, so the
/// line does not rescale while the machine is in the quiet band.
const CPU_AXIS_LOW: f64 = 30.0;

/// Past [`CPU_AXIS_LOW`] the top is the next multiple of this above the
/// highest point: 33% draws on 0–40. It used to be the highest point
/// itself, which the corner label would have printed as "33.2%" and
/// moved on every decimal of a new high; a round step reads as an axis
/// and holds still until the peak crosses it.
const CPU_AXIS_STEP: f64 = 10.0;

/// Memory axis. The series is `used_percent`, so the top is the whole
/// machine. It does not zoom to the recent band — a line that grew
/// because the percent wobbled would read as the machine filling up.
const MEM_AXIS_TOP: f64 = 100.0;

/// Network ceiling while every point stays at or under this — the
/// [`CPU_AXIS_LOW`] of the network line. It was 64 KiB/s (the Network
/// tab's per-program floor), and everyday traffic swinging between
/// 100 and 300 kB/s in the first minutes doubled it 128 → 256 → 512 KiB
/// with each new high, so the line rescaled inside the well while the
/// window was still filling. 1 MB/s holds still across ordinary use;
/// past it the doubling resumes (2, 4, 8 MB/s), and a few kB/s of
/// background traffic stays on the floor. Not shared with the program
/// curves on purpose: those draw each program's own shape, this is the
/// whole machine.
const NET_SCALE_FLOOR: f64 = 1024.0 * 1024.0;

/// How many points a curve draws. The ring keeps every tick; this is
/// only the display. 30 minutes / 90 is 20s.
const CHART_BUCKETS: usize = 90;

/// How many memory bars the chart draws. Same 30-minute axis as the
/// curves, coarser so a bar stays a bar: 30 minutes / 60 is 30s.
const BAR_BUCKETS: usize = 60;

/// Share of each of the [`BAR_BUCKETS`] slots left as the gap between
/// bars. The rest is the bar. A quarter keeps a gap on the minimum
/// window and stays a hairline on the default panel.
const BAR_GAP_SHARE: f32 = 0.25;

// One slice is wider than the raw missed-sample gap, so an empty slice
// is a break and not a tick that merely arrived late. Both grids.
const _: () = assert!(series::WINDOW.as_secs() / CHART_BUCKETS as u64 > series::GAP.as_secs());
const _: () = assert!(series::WINDOW.as_secs() / BAR_BUCKETS as u64 > series::GAP.as_secs());

/// One display slice. [`series::WINDOW`] divides evenly by both grids.
fn chart_slice(buckets: usize) -> Duration {
    series::WINDOW / buckets as u32
}

/// At most [`CHART_BUCKETS`] points, oldest first. CPU and network.
fn chart_buckets(points: &[Point], now: Instant) -> Vec<Point> {
    bucketed(points, now, CHART_BUCKETS)
}

/// At most [`BAR_BUCKETS`] bars, oldest first. Memory.
fn bar_buckets(points: &[Point], now: Instant) -> Vec<Point> {
    bucketed(points, now, BAR_BUCKETS)
}

/// Averages `points` into `buckets` equal slices of [`series::WINDOW`].
///
/// A slice before the first sample is left out, so the left of the
/// 30-minute axis stays empty and the slice does not dilute the first
/// average. A slice with no reading between two that have one becomes
/// a `None`, so the line breaks there and a bar slot stays empty. A
/// `None` beside a reading in the same slice is skipped; it is not
/// counted as zero.
///
/// Averaged from the first sample, not only once the ring outgrows the
/// grid. At the 2s open cadence 90 raw samples cover three minutes, the
/// newest tenth of the axis — about 30px, a third of a pixel apiece, so
/// raw points there are noise rather than detail — and switching to
/// averages at the 91st would drop the peaks in one frame and rescale
/// the CPU top and the network ceiling with them.
fn bucketed(points: &[Point], now: Instant, buckets: usize) -> Vec<Point> {
    if buckets == 0 {
        return Vec::new();
    }
    let bucket = chart_slice(buckets);
    let secs = bucket.as_secs();
    if secs == 0 {
        return Vec::new();
    }
    struct Acc {
        sum: f64,
        n: u32,
        saw_none: bool,
    }
    let mut acc = Vec::with_capacity(buckets);
    for _ in 0..buckets {
        acc.push(Acc {
            sum: 0.0,
            n: 0,
            saw_none: false,
        });
    }
    for point in points {
        let age = now.saturating_duration_since(point.at);
        if age >= series::WINDOW {
            continue;
        }
        let mut index = (age.as_secs() / secs) as usize;
        if index >= buckets {
            index = buckets - 1;
        }
        match point.value {
            Some(value) => {
                acc[index].sum += value;
                acc[index].n += 1;
            }
            None => acc[index].saw_none = true,
        }
    }
    let Some(oldest) = acc.iter().rposition(|slot| slot.n > 0 || slot.saw_none) else {
        return Vec::new();
    };
    let Some(newest) = acc.iter().position(|slot| slot.n > 0 || slot.saw_none) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for index in (newest..=oldest).rev() {
        let age = bucket * index as u32 + bucket / 2;
        let at = now.checked_sub(age).unwrap_or(now);
        let value = if acc[index].n > 0 {
            Some(acc[index].sum / f64::from(acc[index].n))
        } else {
            None
        };
        out.push(Point { at, value });
    }
    out
}

/// Runs the chart stroke can draw. A `None` bucket is the only break:
/// neighbouring buckets are one slice apart (20s on a curve, 30s on
/// the bars), and that step is the grid, not a missed sample. One
/// occupied bucket is still a reading.
fn chart_runs(points: &[Point]) -> Vec<Vec<(Instant, f64)>> {
    let mut out = Vec::new();
    let mut run = Vec::new();
    for point in points {
        match point.value {
            Some(value) => run.push((point.at, value)),
            None => {
                if !run.is_empty() {
                    out.push(run);
                    run = Vec::new();
                }
            }
        }
    }
    if !run.is_empty() {
        out.push(run);
    }
    out
}

/// 30 while every point stays at or under 30. Once one goes over, the
/// next [`CPU_AXIS_STEP`] above the window's highest point, at most
/// 100 — a 33% peak draws on 0–40 instead of sitting low on 0–100.
/// Callers pass [`chart_buckets`], so the top follows the points on
/// screen; the corner label ([`scale_label`]) prints it.
fn cpu_axis_top(points: &[Point]) -> f64 {
    let peak = points
        .iter()
        .filter_map(|point| point.value)
        .fold(0.0, f64::max);
    if peak <= CPU_AXIS_LOW {
        return CPU_AXIS_LOW;
    }
    ((peak / CPU_AXIS_STEP).ceil() * CPU_AXIS_STEP).min(100.0)
}

/// Where `age` sits on the 30-minute axis. `1` is now, the right edge.
/// `0` is [`series::WINDOW`] ago, the left edge. A short history stays
/// on the right. The time before the first sample is an empty stretch
/// of this axis, not a zero line.
fn axis_t(age: Duration) -> f64 {
    let window = series::WINDOW.as_secs_f64();
    if window <= 0.0 {
        return 1.0;
    }
    (1.0 - age.as_secs_f64() / window).clamp(0.0, 1.0)
}

/// The row's own peak, but never under the floor, and only on a
/// doubling. The exact peak would move the ceiling on every new high
/// and the whole line would jump inside the slot.
fn net_scale(points: &[Point]) -> f64 {
    let peak = points
        .iter()
        .filter_map(|point| point.value)
        .fold(NET_SCALE_FLOOR, f64::max);
    let mut step = NET_SCALE_FLOOR;
    // 64 KiB/s doubled forty times is 64 TiB/s. Past that, keep the
    // last step rather than loop.
    for _ in 0..40 {
        if step >= peak {
            return step;
        }
        step *= 2.0;
    }
    step
}

/// What a chart's samples are, so the readout formats them the way the
/// rest of the card does.
#[derive(Clone, Copy)]
enum ChartUnit {
    /// CPU and memory, `0–100`.
    Percent,
    /// Network download or upload, bytes per second.
    Rate,
}

/// Highest point the chart draws — the bucketed average the line or bar
/// is made of, not the raw tick. It was the raw reading: a two-second
/// spike set "Max 33%" while the 20s average under it barely bumped the
/// line, so the caption named a height nothing on the chart reached and
/// read as some older, all-time figure. There is no "now" beside it:
/// the latest reading is the card's own headline (CPU %, memory, the
/// rates row), and repeating it under the chart was the same number
/// twice.
fn chart_peak(points: &[Point]) -> Option<f64> {
    points
        .iter()
        .filter_map(|point| point.value)
        .reduce(f64::max)
}

fn chart_value(unit: ChartUnit, value: Option<f64>) -> String {
    let Some(value) = value else {
        return format::PLACEHOLDER.to_string();
    };
    match unit {
        ChartUnit::Percent => format::pct(value as f32),
        ChartUnit::Rate => format::rate(Some(value.max(0.0).round() as u64)),
    }
}

/// One Overview chart: what it draws and how it reads.
struct Chart<'a> {
    id: &'static str,
    /// The lead series first, in the full [`theme::ink`]. A second one
    /// (network upload) is drawn behind it in [`dim_ink`], on the same
    /// ceiling.
    lines: Vec<ChartLine<'a>>,
    scale: f64,
    tip: String,
    stroke: CurveStroke,
    unit: ChartUnit,
    /// Glyph at the head of the caption, for a chart whose card does
    /// not already say what it is.
    icon: Option<IconName>,
    /// Print the top of the axis in the well's corner. On for the two
    /// charts whose top moves (CPU, network); memory's is always 100%,
    /// which the bars' own share of the slot already says.
    corner_label: bool,
}

/// One series on a [`Chart`].
#[derive(Clone, Copy)]
struct ChartLine<'a> {
    series: &'a series::Series,
    /// Arrow before this line's peak in the caption, when a chart has
    /// two lines and the caption has to say which is which.
    arrow: Option<&'static str>,
}

/// One chart, once a ring holds a point. The slot is reserved from
/// that first point, so the card does not jump when the second sample
/// turns it into a mark. Under a second the time reads — and nothing
/// is stroked. The tooltip carries the scale and the tray cadence; the
/// canvas itself is not the hover target.
fn recent_line(chart: Chart, now: Instant) -> Option<AnyElement> {
    if chart
        .lines
        .iter()
        .all(|line| line.series.points().is_empty())
    {
        return None;
    }
    let span = chart
        .lines
        .iter()
        .filter_map(|line| line.series.span(now))
        .max();
    let marks: Vec<Vec<Point>> = chart
        .lines
        .iter()
        .map(|line| match chart.stroke {
            CurveStroke::Bars => bar_buckets(line.series.points(), now),
            CurveStroke::Smooth => chart_buckets(line.series.points(), now),
        })
        .collect();
    // The highest point drawn, so the caption names a height the reader
    // can find on the line.
    let peaks: Vec<(Option<&'static str>, Option<f64>)> = chart
        .lines
        .iter()
        .zip(&marks)
        .map(|(line, points)| (line.arrow, chart_peak(points)))
        .collect();
    let axis = chart.corner_label && corner_label_earned(&marks);
    Some(
        // The well stays [`CURVE_H`]. A ceiling change (CPU crossing
        // 30, a network step) moves the mark inside it. The caption
        // under the well is a fixed line, so the card does not jump.
        div()
            .id(chart.id)
            .mt(px(CURVE_TOP))
            .flex_none()
            .tooltip(widgets::wrap_tooltip(chart.tip))
            .child(
                // Recessed track, the same fill as a meter. It marks
                // the chart off the card. The marks are inset in
                // [`paint_curve`].
                div()
                    .relative()
                    .w_full()
                    .h(px(CURVE_H))
                    .rounded(px(CHART_WELL_RADIUS))
                    .bg(theme::inset())
                    .overflow_hidden()
                    .child(sparkline(marks, span, now, chart.scale, chart.stroke))
                    .when(axis, |well| {
                        well.child(corner_label(scale_label(chart.unit, chart.scale)))
                    }),
            )
            .child(chart_readout(&peaks, span, chart.unit, chart.icon))
            .into_any_element(),
    )
}

/// The top of the axis, printed as an axis: a whole percent, or whole
/// MB/s for the network's doubling ceilings.
fn scale_label(unit: ChartUnit, scale: f64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    match unit {
        ChartUnit::Percent => format!("{scale:.0}%"),
        ChartUnit::Rate if scale >= MIB => format!("{:.0} MB/s", scale / MIB),
        ChartUnit::Rate => format::rate(Some(scale.max(0.0).round() as u64)),
    }
}

/// The axis top in the well's top-left corner, where a reader looks for
/// it: without it the top of the chart was a rule in a tooltip — 30%
/// under a "Max 15.8%" caption, or an unnamed doubling under "Max 6.6
/// MB/s". The left is the oldest half hour, so the newest data on the
/// right is never under it.
///
/// The digits used to sit on a second coat of the well's own fill, so a
/// line passing beneath would dim. That fill is translucent, and the
/// second coat composites darker than the well — `#151516` on a
/// `#1c1c1d` well, measured on the short form — so "30%" became a chip,
/// the brightest mark in a quiet half hour and a second percent beside
/// the headline. 9px in the dim ink is the axis. A line at the ceiling
/// may cross it; that end of the chart is the oldest.
fn corner_label(text: String) -> AnyElement {
    div()
        .absolute()
        .top(px(2.))
        .left(px(CHART_PAD))
        .font_features(font::tnum())
        .text_size(px(9.))
        .line_height(px(11.))
        .text_color(theme::text_dim())
        .child(text)
        .into_any_element()
}

/// The highest reading (one per line, arrowed when there are two) and
/// how long the window has been collecting, one line under the chart.
/// The time stays — until the series covers a second, which is when
/// [`series::span`] starts answering.
fn chart_readout(
    peaks: &[(Option<&'static str>, Option<f64>)],
    span: Option<Duration>,
    unit: ChartUnit,
    icon: Option<IconName>,
) -> AnyElement {
    let span = span
        .map(series::span_label)
        .unwrap_or_else(|| format::PLACEHOLDER.to_string());
    let peaks = peaks.iter().enumerate().map(|(index, (arrow, value))| {
        let value = chart_value(unit, *value);
        let text = match arrow {
            Some(arrow) => format!("{arrow} {value}"),
            None => value,
        };
        // The dimmed line's figure is dimmed too: the caption doubles
        // as the legend for which stroke is which.
        let ink = if index == 0 {
            theme::text()
        } else {
            theme::text_muted()
        };
        div()
            .flex_none()
            .font_features(font::tnum())
            .text_size(px(10.))
            .line_height(px(CHART_CAPTION))
            .text_color(ink)
            .child(text)
    });
    h_flex()
        .mt(px(CHART_CAPTION_GAP))
        .h(px(CHART_CAPTION))
        .w_full()
        .items_center()
        .justify_between()
        .child(
            h_flex()
                .items_center()
                .gap(px(4.))
                .when_some(icon, |row, icon| {
                    row.child(
                        Icon::new(icon)
                            .with_size(Size::Size(px(10.)))
                            .text_color(Hsla::from(theme::text_dim())),
                    )
                })
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.))
                        .line_height(px(CHART_CAPTION))
                        .text_color(theme::text_dim())
                        .child(i18n::tr("overview.chart_peak")),
                )
                .child(h_flex().items_center().gap(px(8.)).children(peaks)),
        )
        .child(chart_stat(i18n::tr("overview.chart_span"), span))
        .into_any_element()
}

fn chart_stat(label: String, value: String) -> AnyElement {
    h_flex()
        .items_center()
        .gap(px(4.))
        .child(
            div()
                .flex_none()
                .text_size(px(10.))
                .line_height(px(CHART_CAPTION))
                .text_color(theme::text_dim())
                .child(label),
        )
        .child(
            div()
                .flex_none()
                .font_features(font::tnum())
                .text_size(px(10.))
                .line_height(px(CHART_CAPTION))
                .text_color(theme::text())
                .child(value),
        )
        .into_any_element()
}

/// The slot is always the same height. Nothing is stroked until the
/// series covers a second. The well behind it is already up, and the
/// axis under the stroke is the full 30 minutes, marked off by
/// [`paint_guides`] once the span reaches each mark. `marks` is one
/// bucketed series per line, the lead first.
fn sparkline(
    marks: Vec<Vec<Point>>,
    span: Option<Duration>,
    now: Instant,
    scale: f64,
    stroke: CurveStroke,
) -> AnyElement {
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            paint_guides(bounds, span, window);
            if span.is_none() {
                return;
            }
            // Behind first, so the lead line is painted on top. Only the
            // lead has a wash under it: the network's upload line is the
            // one behind, and a second wash would muddy the first.
            for (index, points) in marks.iter().enumerate().rev() {
                let lead = index == 0;
                let look = LineInk {
                    ink: if lead { theme::gauge() } else { dim_ink() },
                    wash: lead,
                };
                paint_curve(bounds, points, now, scale, stroke, look, window);
            }
            // The lead line's holes only: two dotted rows under the
            // network's pair would be one fact drawn twice.
            if let Some(lead) = marks.first() {
                paint_gaps(bounds, lead, now, stroke, window);
            }
        },
    )
    .size_full()
    .into_any_element()
}

/// How far apart the time guides are. Two of them, at 10 and 20
/// minutes ago, split the 30-minute axis into thirds.
const GUIDE_EVERY: Duration = Duration::from_secs(10 * 60);

/// Hairlines across the well at every [`GUIDE_EVERY`] the span has
/// reached. A mark at 10 minutes on a 13-second line cuts an empty well
/// into thirds, and that grid is what a reader sees instead of the dash;
/// the span beside the chart already says the window is short. Once the
/// oldest sample is at least this far back, the mark sits on collected
/// time and the empty left of it is the rest of the half hour. Painted
/// under the stroke, in the row-separator ink, full well height. On the
/// bar chart a guide falls on a slot boundary, in the gap between two bars.
fn paint_guides(bounds: Bounds<Pixels>, span: Option<Duration>, window: &mut Window) {
    let pad = px(CHART_PAD);
    let left = bounds.left() + pad;
    let width = bounds.size.width - pad * 2.;
    if width <= px(0.) {
        return;
    }
    let mut age = GUIDE_EVERY;
    while age < series::WINDOW {
        if guide_due(age, span) {
            // Whole pixels: a 1px hairline straddling two columns paints
            // as two faint ones.
            let x = (f32::from(left) + f32::from(width) * axis_t(age) as f32).round();
            window.paint_quad(gpui::fill(
                Bounds::new(point(px(x), bounds.top()), size(px(1.), bounds.size.height)),
                theme::border_subtle(),
            ));
        }
        age += GUIDE_EVERY;
    }
}

/// Whether the guide `age` back from now sits on time the series has
/// actually collected.
fn guide_due(age: Duration, span: Option<Duration>) -> bool {
    span.is_some_and(|span| span >= age)
}

/// The corner figure names the top of the axis, and it waits until the
/// stroke is a line. One occupied bucket is the dash [`lone_reading`]
/// draws — a couple of pixels on the right of an empty well — and beside
/// it "30%" is a second percentage with nothing under it to scale. Two
/// buckets are a segment, and the top is then what that segment is drawn
/// against.
fn stroke_is_a_line(points: &[Point]) -> bool {
    chart_runs(points).iter().any(|run| run.len() >= 2)
}

/// Either series earning a line is enough: CPU has one, the network
/// chart has two, and one ceiling covers both.
fn corner_label_earned(marks: &[Vec<Point>]) -> bool {
    marks.iter().any(|points| stroke_is_a_line(points))
}

/// Ink of a top-apps percent. `0.0%` is what the column prints for every
/// idle tree, and five of them in the name's ink were the loudest marks
/// on a quiet page. A reading the decimal keeps, and anything over the
/// line, stays as it was.
fn app_pct_ink(percent: f32, hot: bool) -> Rgba {
    if !hot && format::pct(percent) == "0.0%" {
        theme::text_dim()
    } else {
        theme::text_for(hot)
    }
}

/// One dot of a gap's baseline, and the pitch between dots.
const GAP_DOT: f32 = 2.;
const GAP_DOT_STEP: f32 = 5.;

/// A dotted baseline across every hole *between* readings — not before
/// the first, which is the window still filling. The span beside the
/// chart says how far that goes, and [`paint_guides`] marks 10 and 20
/// minutes once the span reaches them. The rings are fed every tick,
/// panel hidden or not, so a hole in the middle is time with nothing
/// recorded: the Mac asleep, or this app not running. Bare, that read
/// as a chart that broke; dotted, it reads as a stretch where nothing
/// was measured.
fn paint_gaps(
    bounds: Bounds<Pixels>,
    points: &[Point],
    now: Instant,
    stroke: CurveStroke,
    window: &mut Window,
) {
    let pad = px(CHART_PAD);
    let left = f32::from(bounds.left() + pad);
    let width = f32::from(bounds.size.width - pad * 2.);
    let y = f32::from(bounds.bottom() - pad) - 1.0;
    if width <= 0.0 {
        return;
    }
    let x_of = |at: Instant| left + width * axis_t(now.saturating_duration_since(at)) as f32;
    match stroke {
        CurveStroke::Smooth => {
            // A one-bucket run is drawn as a dash a slice wide
            // (`lone_reading`), so its end is half a slice out.
            let half = width / CHART_BUCKETS as f32 / 2.0;
            let runs = chart_runs(points);
            for pair in runs.windows(2) {
                let (prev, next) = (&pair[0], &pair[1]);
                let (Some(last), Some(first)) = (prev.last(), next.first()) else {
                    continue;
                };
                let from = x_of(last.0) + if prev.len() == 1 { half } else { 0.0 };
                let to = x_of(first.0) - if next.len() == 1 { half } else { 0.0 };
                paint_dotted(from, to, y, window);
            }
        }
        CurveStroke::Bars => {
            let mut slots: Vec<usize> = points
                .iter()
                .filter(|sample| sample.value.is_some())
                .filter_map(|sample| bucket_index(sample.at, now))
                .collect();
            // Oldest (highest index) first, left to right.
            slots.sort_unstable_by(|a, b| b.cmp(a));
            slots.dedup();
            for pair in slots.windows(2) {
                if pair[0] - pair[1] < 2 {
                    continue;
                }
                let (_, from) = bar_edges(pair[0], left, width);
                let (to, _) = bar_edges(pair[1], left, width);
                paint_dotted(from, to, y, window);
            }
        }
    }
}

/// Dots from `from` to `to` on the row at `y`, in the history's dim ink.
/// A hole narrower than two dots says nothing a dot could.
fn paint_dotted(from: f32, to: f32, y: f32, window: &mut Window) {
    if to - from < GAP_DOT_STEP * 2.0 {
        return;
    }
    let mut x = from + (GAP_DOT_STEP - GAP_DOT) / 2.0;
    while x + GAP_DOT <= to {
        window.paint_quad(gpui::fill(
            Bounds::new(point(px(x.round()), px(y)), size(px(GAP_DOT), px(1.))),
            dim_ink(),
        ));
        x += GAP_DOT_STEP;
    }
}

/// Share of [`theme::gauge`]'s alpha kept by the marks behind the lead:
/// network upload under download, every memory bar but the newest, and
/// the compressed slice of the memory meter.
const CHART_DIM: f32 = 0.45;

/// [`theme::gauge`] at [`CHART_DIM`]. Paint only, so a theme switch moves
/// no geometry.
fn dim_ink() -> Rgba {
    let ink = theme::gauge();
    Rgba {
        a: ink.a * CHART_DIM,
        ..ink
    }
}

/// How one line of a chart is painted: its ink, and whether a wash of
/// it fills the well under it ([`AREA_ALPHA`] — the lead line only).
#[derive(Clone, Copy)]
struct LineInk {
    ink: Rgba,
    wash: bool,
}

fn paint_curve(
    bounds: Bounds<Pixels>,
    points: &[Point],
    now: Instant,
    scale: f64,
    stroke: CurveStroke,
    look: LineInk,
    window: &mut Window,
) {
    let LineInk { ink, wash: area } = look;
    if scale <= 0.0 {
        return;
    }
    let pad = px(CHART_PAD);
    let top = bounds.top() + pad;
    let bottom = bounds.bottom() - pad;
    let left = bounds.left() + pad;
    let width = bounds.size.width - pad * 2.;
    if bottom <= top || width <= px(0.) {
        return;
    }
    let height = bottom - top;
    let x_of = |at: Instant| {
        let t = axis_t(now.saturating_duration_since(at));
        left + width * (t as f32)
    };
    let y_of = |value: f64| {
        let h = (value / scale).clamp(0.0, 1.0);
        bottom - height * (h as f32)
    };
    if let CurveStroke::Bars = stroke {
        paint_bars(points, left, width, now, bottom, &y_of, window);
        return;
    }
    let mut builder = PathBuilder::stroke(px(CURVE_STROKE));
    // The wash under the line: each run closed down to the axis, so a
    // break stays a break in the fill too.
    let mut wash = PathBuilder::fill();
    let mut washed = false;
    let mut drew = false;
    for segment in chart_runs(points) {
        let plotted: Vec<(f32, f32)> = segment
            .iter()
            .map(|(at, value)| (f32::from(x_of(*at)), f32::from(y_of(*value))))
            .collect();
        if plotted.is_empty() {
            continue;
        }
        // One bucket has nothing to bend through: the first 20s, or a
        // run cut off on both sides by breaks. See [`lone_reading`].
        if plotted.len() == 1 {
            let (x, y) = plotted[0];
            let slice = f32::from(width) / CHART_BUCKETS as f32;
            let (from, to) = lone_reading(x, slice, f32::from(left), f32::from(left + width));
            builder.move_to(point(px(from), px(y)));
            builder.line_to(point(px(to), px(y)));
            drew = true;
            continue;
        }
        let cubics = smooth_cubics(&plotted);
        builder.move_to(point(px(plotted[0].0), px(plotted[0].1)));
        for cubic in &cubics {
            builder.cubic_bezier_to(
                point(px(cubic.to.0), px(cubic.to.1)),
                point(px(cubic.ctrl_a.0), px(cubic.ctrl_a.1)),
                point(px(cubic.ctrl_b.0), px(cubic.ctrl_b.1)),
            );
        }
        if area {
            let (first, last) = (plotted[0], plotted[plotted.len() - 1]);
            wash.move_to(point(px(first.0), bottom));
            wash.line_to(point(px(first.0), px(first.1)));
            for cubic in &cubics {
                wash.cubic_bezier_to(
                    point(px(cubic.to.0), px(cubic.to.1)),
                    point(px(cubic.ctrl_a.0), px(cubic.ctrl_a.1)),
                    point(px(cubic.ctrl_b.0), px(cubic.ctrl_b.1)),
                );
            }
            wash.line_to(point(px(last.0), bottom));
            wash.close();
            washed = true;
        }
        drew = true;
    }
    if !drew {
        return;
    }
    // Under the stroke, from the line's highest point down to nothing:
    // the gradient spans the fill's own box, so a quiet line gets a
    // shallow wash and a busy one a tall one.
    if washed && let Ok(path) = wash.build() {
        let top = Rgba {
            a: ink.a * AREA_ALPHA,
            ..ink
        };
        let clear = Rgba { a: 0., ..ink };
        window.paint_path(
            path,
            linear_gradient(
                180.,
                linear_color_stop(top, 0.),
                linear_color_stop(clear, 1.),
            ),
        );
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, ink);
    }
}

/// Horizontal extent of a run that holds one bucket: a flat dash at the
/// reading's height, one slice wide, centred on the bucket and kept
/// inside the drawing. It used to be a vertical from the axis up to
/// the reading, which is how a bar is read — "from 0 to here", beside
/// the memory card's real bars — when it is one level, the first piece
/// of the line the next bucket extends.
fn lone_reading(x: f32, slice: f32, left: f32, right: f32) -> (f32, f32) {
    let half = slice / 2.0;
    ((x - half).max(left), (x + half).min(right))
}

/// Bars for [`CurveStroke::Bars`]. One rectangle per reading that has a
/// value, from the axis up to the sample. An empty slot stays empty,
/// on the left before the first sample and in a hole alike, so a bar
/// does not grow across it.
///
/// Only the newest bar is in the full ink; the history behind it is
/// [`dim_ink`]. Memory in use barely moves in half an hour, so sixty
/// full-ink bars were a solid block at ~70% height — the brightest
/// thing in the panel, for the least news on it.
fn paint_bars(
    points: &[Point],
    left: Pixels,
    width: Pixels,
    now: Instant,
    bottom: Pixels,
    y_of: &impl Fn(f64) -> Pixels,
    window: &mut Window,
) {
    let left = f32::from(left);
    let width = f32::from(width);
    if width <= 0.0 {
        return;
    }
    let floor = f32::from(bottom);
    // Oldest first, so the newest reading is the last one with a value.
    let newest = points.iter().rposition(|sample| sample.value.is_some());
    let mut history = PathBuilder::fill();
    let mut latest = PathBuilder::fill();
    let (mut drew_history, mut drew_latest) = (false, false);
    for (position, sample) in points.iter().enumerate() {
        let Some(value) = sample.value else {
            continue;
        };
        let Some(index) = bucket_index(sample.at, now) else {
            continue;
        };
        let top = f32::from(y_of(value));
        if top >= floor {
            continue;
        }
        let (bar_left, bar_right) = bar_edges(index, left, width);
        if bar_right <= bar_left {
            continue;
        }
        let builder = if Some(position) == newest {
            drew_latest = true;
            &mut latest
        } else {
            drew_history = true;
            &mut history
        };
        builder.move_to(point(px(bar_left), px(floor)));
        builder.line_to(point(px(bar_left), px(top)));
        builder.line_to(point(px(bar_right), px(top)));
        builder.line_to(point(px(bar_right), px(floor)));
        builder.close();
    }
    if drew_history && let Ok(path) = history.build() {
        window.paint_path(path, dim_ink());
    }
    if drew_latest && let Ok(path) = latest.build() {
        window.paint_path(path, theme::gauge());
    }
}

/// Which of the [`BAR_BUCKETS`] slots `at` falls in. `0` is the newest,
/// on the right. A bar is drawn at its centre, and the centre of slot
/// `n` is `n * 30s + 15s`, which divides back to `n`.
fn bucket_index(at: Instant, now: Instant) -> Option<usize> {
    let age = now.saturating_duration_since(at);
    if age >= series::WINDOW {
        return None;
    }
    let secs = chart_slice(BAR_BUCKETS).as_secs();
    if secs == 0 {
        return None;
    }
    Some(((age.as_secs() / secs) as usize).min(BAR_BUCKETS - 1))
}

/// Edges of the bar in slot `index` (`0` is the rightmost). The chart
/// is split into [`BAR_BUCKETS`] equal slots, and [`BAR_GAP_SHARE`] of
/// each slot is the gap, half on either side of the bar. Sixty bars
/// and their gaps fill `width` exactly.
fn bar_edges(index: usize, left: f32, width: f32) -> (f32, f32) {
    let pitch = width / BAR_BUCKETS as f32;
    let inset = pitch * BAR_GAP_SHARE / 2.0;
    let slot_right = left + width - index as f32 * pitch;
    (slot_right - pitch + inset, slot_right - inset)
}

/// One cubic of [`smooth_cubics`]. `from` is where the previous cubic
/// ended; the stroke starts there and does not move again.
struct Cubic {
    ctrl_a: (f32, f32),
    ctrl_b: (f32, f32),
    to: (f32, f32),
}

/// Monotone cubic through `points`, as Bézier controls.
///
/// Fritsch–Carlson: the tangent at a sample is zero wherever the
/// slope changes sign, and each segment's tangents are shortened
/// until the curve stays between that segment's two samples. A plain
/// spline overshoots, and on a 0–100 axis an overshoot is a reading
/// the machine did not take. Two samples come back as the straight
/// chord. X is the pixel position, so a 5s tray step and a 2s open
/// step keep their own slopes.
fn smooth_cubics(points: &[(f32, f32)]) -> Vec<Cubic> {
    let n = points.len();
    if n < 2 {
        return Vec::new();
    }
    let slope = monotone_slopes(points);
    let mut out = Vec::with_capacity(n - 1);
    for i in 0..n - 1 {
        let (x0, y0) = points[i];
        let (x1, y1) = points[i + 1];
        let dx = (x1 - x0) / 3.0;
        out.push(Cubic {
            ctrl_a: (x0 + dx, y0 + slope[i] * dx),
            ctrl_b: (x1 - dx, y1 - slope[i + 1] * dx),
            to: (x1, y1),
        });
    }
    out
}

/// dy/dx at each sample. Interior points use the weighted harmonic
/// mean of the neighbouring secants; a sign change is a real peak
/// and gets a flat tangent. Endpoints use the three-point estimate,
/// then every segment is clamped so its Bézier cannot leave the
/// range of its two samples.
fn monotone_slopes(points: &[(f32, f32)]) -> Vec<f32> {
    let n = points.len();
    let mut h = vec![0.0; n - 1];
    let mut secant = vec![0.0; n - 1];
    for i in 0..n - 1 {
        h[i] = points[i + 1].0 - points[i].0;
        secant[i] = if h[i].abs() <= f32::EPSILON {
            0.0
        } else {
            (points[i + 1].1 - points[i].1) / h[i]
        };
    }
    let mut slope = vec![0.0; n];
    if n == 2 {
        slope[0] = secant[0];
        slope[1] = secant[0];
    } else {
        slope[0] = endpoint_slope(h[0], h[1], secant[0], secant[1]);
        let last = n - 2;
        slope[n - 1] = endpoint_slope(h[last], h[last - 1], secant[last], secant[last - 1]);
        for i in 1..n - 1 {
            if secant[i - 1] * secant[i] <= 0.0 {
                slope[i] = 0.0;
            } else {
                let w1 = 2.0 * h[i] + h[i - 1];
                let w2 = h[i] + 2.0 * h[i - 1];
                slope[i] = (w1 + w2) / (w1 / secant[i - 1] + w2 / secant[i]);
            }
        }
    }
    for i in 0..n - 1 {
        if secant[i].abs() <= f32::EPSILON {
            slope[i] = 0.0;
            slope[i + 1] = 0.0;
            continue;
        }
        let a = slope[i] / secant[i];
        let b = slope[i + 1] / secant[i];
        let sum = a * a + b * b;
        if sum > 9.0 {
            let tau = 3.0 / sum.sqrt();
            slope[i] *= tau;
            slope[i + 1] *= tau;
        }
    }
    slope
}

/// Slope at an end sample. A sign that disagrees with the first
/// secant would leave the data immediately, so it becomes flat; a
/// slope steeper than three times that secant is the same overshoot
/// the per-segment clamp removes, caught here before it bends the end.
fn endpoint_slope(h0: f32, h1: f32, d0: f32, d1: f32) -> f32 {
    let width = h0 + h1;
    if width.abs() <= f32::EPSILON {
        return 0.0;
    }
    let slope = ((2.0 * h0 + h1) * d0 - h0 * d1) / width;
    if slope * d0 <= 0.0 {
        0.0
    } else if d0 * d1 <= 0.0 && slope.abs() > 3.0 * d0.abs() {
        3.0 * d0
    } else {
        slope
    }
}

/// Disk + net rates, summed by zstats after its own dedupe. A footnote
/// under Memory, not a fourth card and not a second section — one
/// muted line so the memory card stays about memory.
///
/// Icons, not words: "Disk" / "Network" next to the rates read as one
/// sentence at this size. The glyphs are the same ones the tab strip
/// already uses for Hardware and Network, so they carry that meaning
/// here; the translated name sits on the tooltip.
fn io_strip(io: &IoTotalsSnapshot) -> Option<AnyElement> {
    // The disk's pair is read and write, said as R and W — as the Apps
    // tab says them. It wore the network's arrows, and on a disk an
    // arrow does not say which way it points: down into memory, or down
    // onto the disk? Download and upload keep theirs, which every
    // reader already knows.
    let cells = [
        (
            "io-disk",
            IconName::HardDrive,
            i18n::tr("overview.io_disk"),
            ("R", "W"),
            io.disk_read_bytes_per_sec,
            io.disk_write_bytes_per_sec,
        ),
        (
            "io-net",
            IconName::Network,
            i18n::tr("overview.io_net"),
            ("↓", "↑"),
            io.network_received_bytes_per_sec,
            io.network_transmitted_bytes_per_sec,
        ),
    ];
    if cells
        .iter()
        .all(|(_, _, _, _, r, w)| r.is_none() && w.is_none())
    {
        return None;
    }

    // The first row of its own card now (`io_card`), so no hairline and
    // no air above it: the card's padding is the separation.
    let row = h_flex().gap(px(16.)).children(cells.into_iter().map(
        |(id, icon, label, sides, read, write)| {
            h_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .id(id)
                        .flex_none()
                        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                        .child(
                            Icon::new(icon)
                                .with_size(Size::Size(px(12.)))
                                .text_color(Hsla::from(theme::text_dim())),
                        ),
                )
                .child(
                    h_flex()
                        .gap(px(6.))
                        .font_family(font::MONO)
                        .text_size(px(10.))
                        .text_color(theme::text_muted())
                        .child(format!("{} {}", sides.0, format::rate(read)))
                        .child(format!("{} {}", sides.1, format::rate(write))),
                )
        },
    ));
    Some(row.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    /// The notice names the most pressing thing, and there is none on
    /// an ordinary day.
    #[test]
    fn the_notice_names_the_most_pressing_thing() {
        let everything = || Signs {
            memory: true,
            disk: true,
            alerts: names(&["Google Chrome", "Zed"]),
            busy: names(&["mds"]),
            growing: names(&["Slack"]),
        };
        assert_eq!(notice(everything()), Some(Notice::Memory));
        assert_eq!(
            notice(Signs {
                memory: false,
                ..everything()
            }),
            Some(Notice::Disk)
        );
        // Newest first: the count is the list, the name its head.
        assert_eq!(
            notice(Signs {
                memory: false,
                disk: false,
                ..everything()
            }),
            Some(Notice::Alerts {
                count: 2,
                latest: "Google Chrome".into()
            })
        );
        assert_eq!(
            notice(Signs {
                busy: names(&["mds"]),
                growing: names(&["Slack"]),
                ..Signs::default()
            }),
            Some(Notice::Busy("mds".into()))
        );
        assert_eq!(
            notice(Signs {
                growing: names(&["Slack"]),
                ..Signs::default()
            }),
            Some(Notice::Growing("Slack".into()))
        );
        // Nothing held by an episode or a watcher: no row at all.
        assert_eq!(notice(Signs::default()), None);
    }

    fn bezier_y(from_y: f32, cubic: &Cubic, t: f32) -> f32 {
        let u = 1.0 - t;
        u * u * u * from_y
            + 3.0 * u * u * t * cubic.ctrl_a.1
            + 3.0 * u * t * t * cubic.ctrl_b.1
            + t * t * t * cubic.to.1
    }

    #[test]
    fn the_cpu_curve_passes_through_the_samples_and_not_above_a_peak() {
        let peak = [(0.0, 10.0), (10.0, 80.0), (20.0, 12.0)];
        let cubics = smooth_cubics(&peak);
        assert_eq!(cubics.len(), 2);
        assert!((cubics[0].to.1 - 80.0).abs() < 1e-4);
        assert!((cubics[1].to.1 - 12.0).abs() < 1e-4);
        for (i, cubic) in cubics.iter().enumerate() {
            let low = peak[i].1.min(peak[i + 1].1);
            let high = peak[i].1.max(peak[i + 1].1);
            for step in 0..=8 {
                let y = bezier_y(peak[i].1, cubic, step as f32 / 8.0);
                assert!(
                    (low - 1e-3..=high + 1e-3).contains(&y),
                    "a sample of {high} must stay the high point, got {y}"
                );
            }
        }

        let chord = smooth_cubics(&[(0.0, 10.0), (9.0, 40.0)]);
        assert_eq!(chord.len(), 1);
        assert!(
            (chord[0].ctrl_a.1 - 20.0).abs() < 1e-3,
            "two samples stay a line"
        );
        assert!((chord[0].ctrl_b.1 - 30.0).abs() < 1e-3);

        let rising = [(0.0, 5.0), (4.0, 20.0), (14.0, 30.0)];
        let cubics = smooth_cubics(&rising);
        for (i, cubic) in cubics.iter().enumerate() {
            for step in 0..=8 {
                let y = bezier_y(rising[i].1, cubic, step as f32 / 8.0);
                assert!(
                    y + 1e-3 >= rising[i].1.min(rising[i + 1].1),
                    "a climb does not dip, got {y}"
                );
                assert!(
                    y <= rising[i].1.max(rising[i + 1].1) + 1e-3,
                    "a climb does not jump the next sample, got {y}"
                );
            }
        }
    }

    #[test]
    fn the_cpu_axis_stays_at_30_until_a_reading_goes_over_it() {
        let now = Instant::now();
        let point = |value: Option<f64>| Point { at: now, value };
        assert_eq!(cpu_axis_top(&[]), CPU_AXIS_LOW);
        assert_eq!(cpu_axis_top(&[point(None)]), CPU_AXIS_LOW);
        assert_eq!(cpu_axis_top(&[point(Some(12.0))]), CPU_AXIS_LOW);
        assert_eq!(cpu_axis_top(&[point(Some(29.9))]), CPU_AXIS_LOW);
        // Exactly 30 has not gone over the line, so the top stays 30.
        assert_eq!(cpu_axis_top(&[point(Some(30.0))]), CPU_AXIS_LOW);
        // Over it, the next 10% step above the peak, never past 100.
        assert_eq!(cpu_axis_top(&[point(Some(8.0)), point(Some(64.0))]), 70.0);
        assert_eq!(cpu_axis_top(&[point(Some(30.1))]), 40.0);
        assert_eq!(cpu_axis_top(&[point(Some(33.2))]), 40.0);
        assert_eq!(cpu_axis_top(&[point(Some(40.0))]), 40.0);
        assert_eq!(cpu_axis_top(&[point(Some(95.5))]), 100.0);
        assert_eq!(cpu_axis_top(&[point(Some(100.0))]), 100.0);
    }

    #[test]
    fn the_axis_label_waits_until_the_stroke_is_a_line() {
        let now = Instant::now();
        let point = |value: Option<f64>| Point { at: now, value };
        assert!(!corner_label_earned(&[]));
        assert!(!corner_label_earned(&[vec![point(None)]]));
        // One bucket is the dash on the right, label or not.
        assert!(!corner_label_earned(&[vec![point(Some(7.0))]]));
        assert!(!corner_label_earned(&[vec![
            point(None),
            point(Some(7.0)),
            point(None),
        ]]));
        assert!(corner_label_earned(&[vec![
            point(Some(7.0)),
            point(Some(8.0))
        ]]));
        // Either series earning the line is enough: one ceiling for both.
        assert!(corner_label_earned(&[
            vec![point(Some(1.0))],
            vec![point(Some(1.0)), point(Some(2.0))],
        ]));
    }

    #[test]
    fn a_guide_waits_until_the_span_reaches_it() {
        assert!(!guide_due(GUIDE_EVERY, None));
        assert!(!guide_due(GUIDE_EVERY, Some(Duration::from_secs(13))));
        assert!(guide_due(GUIDE_EVERY, Some(GUIDE_EVERY)));
        assert!(!guide_due(GUIDE_EVERY * 2, Some(GUIDE_EVERY)));
        assert!(guide_due(
            GUIDE_EVERY * 2,
            Some(GUIDE_EVERY * 2 + Duration::from_secs(1))
        ));
    }

    #[test]
    fn a_zero_percent_recedes_and_a_real_one_does_not() {
        assert_eq!(app_pct_ink(0.0, false), theme::text_dim());
        // What the column prints as 0.0% recedes with it.
        assert_eq!(format::pct(0.04), "0.0%");
        assert_eq!(app_pct_ink(0.04, false), theme::text_dim());
        assert_eq!(app_pct_ink(1.2, false), theme::text());
        assert_eq!(app_pct_ink(40.0, true), theme::text_for(true));
    }

    #[test]
    fn the_corner_label_reads_as_an_axis() {
        assert_eq!(scale_label(ChartUnit::Percent, 30.0), "30%");
        assert_eq!(scale_label(ChartUnit::Percent, 100.0), "100%");
        assert_eq!(scale_label(ChartUnit::Rate, 1024.0 * 1024.0), "1 MB/s");
        assert_eq!(
            scale_label(ChartUnit::Rate, 8.0 * 1024.0 * 1024.0),
            "8 MB/s"
        );
        // Under a MiB (not a ceiling the network line uses today) it
        // falls back to the rate formatter rather than printing "0 MB/s".
        assert_eq!(
            scale_label(ChartUnit::Rate, 512.0 * 1024.0),
            format::rate(Some(512 * 1024))
        );
    }

    #[test]
    fn a_short_history_stays_on_the_right_of_the_thirty_minute_axis() {
        assert!((axis_t(Duration::ZERO) - 1.0).abs() < 1e-9);
        assert!(axis_t(series::WINDOW).abs() < 1e-9);
        assert!((axis_t(series::WINDOW / 2) - 0.5).abs() < 1e-9);
        // 40s is the right edge of a 30-minute axis. The left stays empty.
        let t = axis_t(Duration::from_secs(40));
        assert!(t > 0.95, "40s sits on the right, got {t}");
    }

    #[test]
    fn sixty_bars_split_the_chart_and_a_new_bar_sits_on_the_right() {
        let left = 10.0;
        let width = 270.0;
        let pitch = width / BAR_BUCKETS as f32;
        let gap = pitch * BAR_GAP_SHARE;
        let (newest_left, newest_right) = bar_edges(0, left, width);
        let (oldest_left, oldest_right) = bar_edges(BAR_BUCKETS - 1, left, width);
        assert!((newest_right - (left + width - gap / 2.0)).abs() < 1e-3);
        assert!((oldest_left - (left + gap / 2.0)).abs() < 1e-3);
        assert!(((newest_right - newest_left) - (pitch - gap)).abs() < 1e-3);
        // The gap between the two newest bars is the share of one slot.
        let (_, next_right) = bar_edges(1, left, width);
        assert!((newest_left - next_right - gap).abs() < 1e-3);
        // Outer edges of the first and last slots meet the chart edges.
        let outer = (newest_right + gap / 2.0) - (oldest_left - gap / 2.0);
        assert!((outer - width).abs() < 1e-2);
        assert!(oldest_right < newest_left);
        let now = Instant::now();
        let at = now.checked_sub(chart_slice(BAR_BUCKETS) / 2).unwrap();
        assert_eq!(bucket_index(at, now), Some(0));
    }

    #[test]
    fn memory_draws_sixty_bars_of_thirty_seconds() {
        assert_eq!(chart_slice(BAR_BUCKETS), Duration::from_secs(30));
        assert_eq!(chart_slice(CHART_BUCKETS), Duration::from_secs(20));
        let now = Instant::now();
        // 5s and 20s share the newest 30s slice. 45s is the slice before.
        let bars = bar_buckets(
            &[
                Point {
                    at: ago(now, 45),
                    value: Some(6.0),
                },
                Point {
                    at: ago(now, 20),
                    value: Some(10.0),
                },
                Point {
                    at: ago(now, 5),
                    value: Some(30.0),
                },
            ],
            now,
        );
        assert_eq!(
            bars.iter().map(|point| point.value).collect::<Vec<_>>(),
            vec![Some(6.0), Some(20.0)]
        );
        let mut full = Vec::new();
        for step in 0..900 {
            full.push(Point {
                at: ago(now, step * 2),
                value: Some(1.0),
            });
        }
        assert_eq!(bar_buckets(&full, now).len(), BAR_BUCKETS);
        assert_eq!(chart_buckets(&full, now).len(), CHART_BUCKETS);
    }

    #[test]
    fn a_lone_reading_is_one_slice_wide_and_stays_in_the_drawing() {
        // Mid-axis: one slice, centred.
        assert_eq!(lone_reading(100.0, 4.0, 0.0, 300.0), (98.0, 102.0));
        // The newest bucket's centre is half a slice from the right
        // edge, so the dash ends on that edge rather than past it.
        assert_eq!(lone_reading(298.0, 4.0, 0.0, 300.0), (296.0, 300.0));
        assert_eq!(lone_reading(299.0, 4.0, 0.0, 300.0), (297.0, 300.0));
        assert_eq!(lone_reading(1.0, 4.0, 0.0, 300.0), (0.0, 3.0));
    }

    #[test]
    fn the_readout_is_the_highest_point_drawn_not_the_raw_spike() {
        // A 70% tick and a 10% tick in one 20s slice draw as 40%, and
        // the caption says 40%: the height the line actually reaches.
        let now = Instant::now();
        let drawn = chart_buckets(
            &[
                Point {
                    at: ago(now, 8),
                    value: Some(70.0),
                },
                Point {
                    at: ago(now, 2),
                    value: Some(10.0),
                },
            ],
            now,
        );
        assert_eq!(chart_peak(&drawn), Some(40.0));
    }

    #[test]
    fn the_peak_is_the_highest_value_of_what_it_is_given() {
        let now = Instant::now();
        let peak = chart_peak(&[
            Point {
                at: ago(now, 30),
                value: Some(70.0),
            },
            Point {
                at: ago(now, 8),
                value: Some(10.0),
            },
            Point {
                at: ago(now, 2),
                value: Some(4.0),
            },
        ]);
        assert_eq!(peak, Some(70.0));
        // A network break ends nothing: the earlier peak remains.
        let broken = chart_peak(&[
            Point {
                at: ago(now, 8),
                value: Some(10.0),
            },
            Point {
                at: ago(now, 2),
                value: None,
            },
        ]);
        assert_eq!(broken, Some(10.0));
        assert_eq!(chart_peak(&[]), None);
        assert_eq!(chart_value(ChartUnit::Percent, Some(12.0)), "12.0%");
        assert_eq!(
            chart_value(ChartUnit::Rate, Some(2048.0)),
            format::rate(Some(2048))
        );
        assert_eq!(chart_value(ChartUnit::Percent, None), format::PLACEHOLDER);
    }

    fn ago(now: Instant, secs: u64) -> Instant {
        now.checked_sub(Duration::from_secs(secs)).unwrap()
    }

    #[test]
    fn the_chart_averages_twenty_second_slices_and_stops_at_ninety() {
        let now = Instant::now();
        // 10 and 30 in the newest slice average to 20. A slice with
        // only a missing rate stays a break, and it does not become 0.
        let sparse = chart_buckets(
            &[
                Point {
                    at: ago(now, 50),
                    value: Some(6.0),
                },
                Point {
                    at: ago(now, 30),
                    value: None,
                },
                Point {
                    at: ago(now, 8),
                    value: Some(10.0),
                },
                Point {
                    at: ago(now, 2),
                    value: Some(30.0),
                },
            ],
            now,
        );
        assert_eq!(
            sparse.iter().map(|point| point.value).collect::<Vec<_>>(),
            vec![Some(6.0), None, Some(20.0)]
        );
        let runs = chart_runs(&sparse);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].len(), 1);
        assert_eq!(runs[1][0].1, 20.0);
        // A slice with no sample at all, between two readings, is the
        // same break. It is not filled with zero.
        let hole = chart_buckets(
            &[
                Point {
                    at: ago(now, 50),
                    value: Some(6.0),
                },
                Point {
                    at: ago(now, 5),
                    value: Some(4.0),
                },
            ],
            now,
        );
        assert_eq!(
            hole.iter().map(|point| point.value).collect::<Vec<_>>(),
            vec![Some(6.0), None, Some(4.0)]
        );

        // A missing rate beside a real reading is skipped, not averaged in.
        let mixed = chart_buckets(
            &[
                Point {
                    at: ago(now, 6),
                    value: None,
                },
                Point {
                    at: ago(now, 2),
                    value: Some(10.0),
                },
            ],
            now,
        );
        assert_eq!(mixed.len(), 1);
        assert_eq!(mixed[0].value, Some(10.0));

        // The empty slices before the first sample are not points.
        let young = chart_buckets(
            &[Point {
                at: ago(now, 5),
                value: Some(4.0),
            }],
            now,
        );
        assert_eq!(young.len(), 1);

        let mut full = Vec::new();
        for step in 0..900 {
            full.push(Point {
                at: ago(now, step * 2),
                value: Some(1.0),
            });
        }
        let chart = chart_buckets(&full, now);
        assert_eq!(chart.len(), CHART_BUCKETS);
        assert!(chart.iter().all(|point| point.value == Some(1.0)));

        // The top follows the averaged points. 10 and 70 in one slice
        // are 40, so the axis is 40; 10 and 20 stay under 30.
        assert_eq!(
            cpu_axis_top(&chart_buckets(
                &[
                    Point {
                        at: ago(now, 2),
                        value: Some(10.0),
                    },
                    Point {
                        at: ago(now, 8),
                        value: Some(70.0),
                    },
                ],
                now,
            )),
            40.0
        );
        assert_eq!(
            cpu_axis_top(&chart_buckets(
                &[
                    Point {
                        at: ago(now, 2),
                        value: Some(10.0),
                    },
                    Point {
                        at: ago(now, 8),
                        value: Some(20.0),
                    },
                ],
                now,
            )),
            CPU_AXIS_LOW
        );
    }

    #[test]
    fn the_network_line_steps_by_doubling_and_never_below_the_floor() {
        let now = Instant::now();
        let point = |value: Option<f64>| Point { at: now, value };
        assert_eq!(
            net_scale(&[point(Some(11_000.0))]),
            NET_SCALE_FLOOR,
            "11 kB/s does not get to be full"
        );
        // 5 MiB sits between 4 MiB and 8 MiB. The ceiling is the next
        // doubling, so a few more bytes do not move the line.
        assert_eq!(
            net_scale(&[point(Some(5.0 * 1024.0 * 1024.0))]),
            8.0 * 1024.0 * 1024.0
        );
        // Everyday traffic swinging under 1 MiB keeps one ceiling, so
        // the line does not rescale while the window fills.
        assert_eq!(net_scale(&[point(Some(70_000.0))]), NET_SCALE_FLOOR);
        assert_eq!(net_scale(&[point(Some(263_000.0))]), NET_SCALE_FLOOR);
        assert_eq!(net_scale(&[point(Some(1024.0 * 1024.0))]), NET_SCALE_FLOOR);
        // Past the floor, nearby peaks share a step: 1.2 and 1.9 MiB
        // are both under 2 MiB.
        let step = net_scale(&[point(Some(1.2 * 1024.0 * 1024.0))]);
        assert_eq!(step, 2.0 * 1024.0 * 1024.0);
        assert_eq!(net_scale(&[point(Some(1.9 * 1024.0 * 1024.0))]), step);

        assert_eq!(net_scale(&[]), NET_SCALE_FLOOR);
        assert_eq!(net_scale(&[point(None)]), NET_SCALE_FLOOR);
    }

    #[test]
    fn the_kernel_percent_rides_the_badge_only_beside_a_verdict() {
        assert_eq!(badge_available(Some(1), Some(62)), Some(62));
        assert_eq!(badge_available(Some(4), Some(8)), Some(8));
        // Waiting for a first sample, or a platform with neither figure
        assert_eq!(badge_available(None, Some(62)), None);
        assert_eq!(badge_available(Some(1), None), None);
    }

    /// The tag names what crossed the floor, in the store's order —
    /// and nothing at all when nothing did, which is the common hour.
    #[test]
    fn only_climbs_past_the_floor_are_named() {
        let total = 24u64 << 30;
        let floor = trend::mem_rise_floor(total);
        let creep = |name: &str, climb_bytes: u64| MemoryCreep {
            name: name.into(),
            root_pid: 1,
            climb_bytes,
            now_bytes: climb_bytes * 2,
        };
        let all = [
            creep("a", floor * 3),
            creep("b", floor),
            creep("c", floor - 1),
        ];
        let named: Vec<&str> = mem_climbers(&all, total)
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(named, ["a", "b"]);
        assert!(mem_climbers(&all[2..], total).is_empty());
    }

    #[test]
    fn swap_activity_is_news_only_while_something_moves() {
        assert_eq!(swap_moving(Some(40), Some(120)), Some((120, 40)));
        assert_eq!(swap_moving(Some(0), Some(3)), Some((3, 0)));
        // Settled, first sample, off macOS: no clause at all
        assert_eq!(swap_moving(Some(0), Some(0)), None);
        assert_eq!(swap_moving(None, None), None);
        assert_eq!(swap_moving(Some(5), None), None);
    }

    fn group(pid: u32, name: &str, cpu: f32) -> ProcessGroupSnapshot {
        ProcessGroupSnapshot {
            root_pid: pid,
            name: name.into(),
            display_name: None,
            process_count: 1,
            cpu_usage_percent: cpu,
            memory_bytes: 0,
            phys_footprint_bytes: None,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    /// Two climbers used to be a two-row card in a window sized for five.
    #[test]
    fn a_short_climb_keeps_five_rows() {
        let groups = vec![
            group(1, "Zed", 0.7),
            group(2, "Ghostty", 6.5),
            group(3, "Chrome", 40.0),
            group(4, "Finder", 8.0),
            group(5, "WindowServer", 5.0),
        ];
        let risers = vec![(&groups[0], 216.0), (&groups[1], 22.8)];
        let rows = pad_rising(risers, &groups, 5);
        assert_eq!(rows.len(), 5);
        assert_eq!(trend::tree_key(rows[0].0), "Zed");
        assert_eq!(rows[0].1, Some(216.0));
        assert_eq!(trend::tree_key(rows[1].0), "Ghostty");
        assert_eq!(rows[1].1, Some(22.8));
        // Leftover slots are current CPU, skip the climbers already named.
        assert_eq!(trend::tree_key(rows[2].0), "Chrome");
        assert!(rows[2].1.is_none());
        assert_eq!(trend::tree_key(rows[3].0), "Finder");
        assert_eq!(trend::tree_key(rows[4].0), "WindowServer");
    }

    #[test]
    fn five_climbers_are_not_padded() {
        let groups: Vec<_> = (0..5)
            .map(|i| group(i, &format!("a{i}"), 10.0 + i as f32))
            .collect();
        let risers: Vec<_> = groups.iter().map(|g| (g, 20.0)).collect();
        let rows = pad_rising(risers, &groups, 5);
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|(_, d)| d.is_some()));
    }
}
