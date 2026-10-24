//! Overview: processor, top application trees, memory.

use super::widgets::{self, card};
use crate::chart::{
    self, AREA_ALPHA, CHART_BUCKETS, CHART_CAPTION, CHART_CAPTION_GAP, CHART_DIM, CHART_PAD,
    CHART_WELL_RADIUS, CURVE_H, CURVE_STROKE, CURVE_TOP, ChartUnit, CurveStroke, GAP_DOT,
    GAP_DOT_STEP, GUIDE_EVERY, MEM_AXIS_TOP,
};
use crate::font;
use crate::format;
use crate::hot;
use crate::i18n;
use crate::notice::{self, Notice};
use crate::placement;
use crate::prefs;
use crate::pressure;
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
    let notice = notice::current(state)?;
    // (the dot, its halo, the row's fill)
    let (dot, halo, fill) = match notice {
        Notice::Memory | Notice::Disk { .. } | Notice::Alerts { .. } => (
            theme::accent_light(),
            theme::accent_wash(18),
            theme::accent_wash(10),
        ),
        Notice::Busy(_) | Notice::Growing(_) => (theme::gauge(), theme::chip(), theme::surface()),
    };
    let sentence = match notice {
        Notice::Memory => i18n::tr("overview.status_memory"),
        Notice::Disk { critical: false } => i18n::tr("overview.status_disk"),
        Notice::Disk { critical: true } => i18n::tr("overview.status_disk_critical"),
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
/// same averaged points and axis rule ([`chart::cpu_axis_top`]), the top of
/// the axis in the corner once the stroke is a line
/// ([`chart::stroke_is_a_line`]). The span is the word beside the row's name
/// and the peak is the line's own highest point; a "Max" and a "Last"
/// under it were another line of the card's height for figures the
/// long form is there to give.
fn cpu_pulse(recent: Recent<'_>) -> AnyElement {
    let points = chart::buckets(recent.cpu.points(), recent.now);
    let scale = chart::cpu_axis_top(&points);
    let span = recent.cpu.span(recent.now);
    let axis = chart::stroke_is_a_line(&points);
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
            well.child(corner_label(chart::scale_label(ChartUnit::Percent, scale)))
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
        .is_some_and(|level| level >= pressure::PRESSURE_CRITICAL);
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
    let notice = !detail && notice::has_notice(state);
    let (_, own) = placement::window_size(detail, notice, placement::Dragged::default());
    let spare = bounds.size.height - own;
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
            let hot = hot::is_hot(f64::from(g.cpu_usage_percent), false)
                && !hot::own_check_cpu(g.root_pid, state);
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
                scale: chart::cpu_axis_top(&chart::buckets(recent.cpu.points(), recent.now)),
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
                    let over = level.usage_percent > hot::CORE_HOT;
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
    let moving = pressure::swap_moving(mem.swap_ins_per_sec, mem.swap_outs_per_sec);
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
    let (label, tip) = pressure::badge(mem, caps.memory_pressure);
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
    let swap_hot = hot::swap_hot(mem);
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
            scale: chart::net_scale(
                &[
                    chart::buckets(recent.net_down.points(), recent.now),
                    chart::buckets(recent.net_up.points(), recent.now),
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
            CurveStroke::Bars => chart::bar_buckets(line.series.points(), now),
            CurveStroke::Smooth => chart::buckets(line.series.points(), now),
        })
        .collect();
    // The highest point drawn, so the caption names a height the reader
    // can find on the line.
    let peaks: Vec<(Option<&'static str>, Option<f64>)> = chart
        .lines
        .iter()
        .zip(&marks)
        .map(|(line, points)| (line.arrow, chart::peak(points)))
        .collect();
    let axis = chart.corner_label && chart::corner_label_earned(&marks);
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
                        well.child(corner_label(chart::scale_label(chart.unit, chart.scale)))
                    }),
            )
            .child(chart_readout(&peaks, span, chart.unit, chart.icon))
            .into_any_element(),
    )
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
        let value = chart::reading(unit, *value);
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
        if chart::guide_due(age, span) {
            // Whole pixels: a 1px hairline straddling two columns paints
            // as two faint ones.
            let x = (f32::from(left) + f32::from(width) * chart::axis_t(age) as f32).round();
            window.paint_quad(gpui::fill(
                Bounds::new(point(px(x), bounds.top()), size(px(1.), bounds.size.height)),
                theme::border_subtle(),
            ));
        }
        age += GUIDE_EVERY;
    }
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
    let x_of = |at: Instant| left + width * chart::axis_t(now.saturating_duration_since(at)) as f32;
    match stroke {
        CurveStroke::Smooth => {
            // A one-bucket run is drawn as a dash a slice wide
            // (`lone_reading`), so its end is half a slice out.
            let half = width / CHART_BUCKETS as f32 / 2.0;
            let runs = chart::runs(points);
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
                .filter_map(|sample| chart::bucket_index(sample.at, now))
                .collect();
            // Oldest (highest index) first, left to right.
            slots.sort_unstable_by(|a, b| b.cmp(a));
            slots.dedup();
            for pair in slots.windows(2) {
                if pair[0] - pair[1] < 2 {
                    continue;
                }
                let (_, from) = chart::bar_edges(pair[0], left, width);
                let (to, _) = chart::bar_edges(pair[1], left, width);
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
        let t = chart::axis_t(now.saturating_duration_since(at));
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
    for segment in chart::runs(points) {
        let plotted: Vec<(f32, f32)> = segment
            .iter()
            .map(|(at, value)| (f32::from(x_of(*at)), f32::from(y_of(*value))))
            .collect();
        if plotted.is_empty() {
            continue;
        }
        // One bucket has nothing to bend through: the first 20s, or a
        // run cut off on both sides by breaks. See [`chart::lone_reading`].
        if plotted.len() == 1 {
            let (x, y) = plotted[0];
            let slice = f32::from(width) / CHART_BUCKETS as f32;
            let (from, to) =
                chart::lone_reading(x, slice, f32::from(left), f32::from(left + width));
            builder.move_to(point(px(from), px(y)));
            builder.line_to(point(px(to), px(y)));
            drew = true;
            continue;
        }
        let cubics = chart::smooth_cubics(&plotted);
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
        let Some(index) = chart::bucket_index(sample.at, now) else {
            continue;
        };
        let top = f32::from(y_of(value));
        if top >= floor {
            continue;
        }
        let (bar_left, bar_right) = chart::bar_edges(index, left, width);
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

    #[test]
    fn a_zero_percent_recedes_and_a_real_one_does_not() {
        assert_eq!(app_pct_ink(0.0, false), theme::text_dim());
        // What the column prints as 0.0% recedes with it.
        assert_eq!(format::pct(0.04), "0.0%");
        assert_eq!(app_pct_ink(0.04, false), theme::text_dim());
        assert_eq!(app_pct_ink(1.2, false), theme::text());
        assert_eq!(app_pct_ink(40.0, true), theme::text_for(true));
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
