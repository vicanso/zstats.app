//! Overview: a port of the gpui front end's `views/overview.rs`, whose comments
//! say why every row, figure and colour is the way it is — this file
//! carries the same choices and does not repeat their reasons.
//!
//! The short form (phase 4b of docs/nsh-migration.md): the notice row while
//! there is something to announce, the vitals card (the processor's figure
//! and its half hour as a line, then four facts), and the apps card. The
//! long form (phase 4c): the processor card, the apps card, the memory
//! card and the disk-and-network card, each chart with its caption line.
//!
//! What the page reads is gathered into a few plain values (`Vitals`,
//! `Processor`, `Memory`, `TopApps`, the notice, each chart's caption) as
//! memos of the store, and the elements bind to them: a tick re-reads the
//! store, and only what came out different is drawn again. A row of the apps card is kept by its pid (`each`), so a
//! program that stays in the list keeps its row — and its icon — while its
//! figure moves.

use crate::assets::CustomIconName;
use crate::chart::{
    CHART_CAPTION, CHART_CAPTION_GAP, CURVE_H, CURVE_TOP, ChartUnit, CurveStroke, MEM_AXIS_TOP,
};
use crate::notice::{self, Notice};
use crate::nsh::chart::{self as canvas_chart, Chart};
use crate::nsh::root::{self, AuxWindow, CARD_GAP, HostAction, Live};
use crate::nsh::widgets::{self, LegendMark, Segment};
use crate::nsh::{store, theme};
use crate::series::{Point, Series};
use crate::state::{MemoryCreep, Tab, ZStatsAppState};
use crate::{chart, format, hot, i18n, palette, placement, prefs, pressure, series, trend};
use nsh_base::TooltipElement;
use nsh_core::prelude::*;
use rust_i18n::t;
use std::collections::HashSet;
use std::time::Instant;
use zstats::snapshot::{ProcessGroupSnapshot, SystemSnapshot};

/// `overview::TOP_N`: the long form's apps.
const TOP_N: usize = 3;

/// `overview::TOP_N_SHORT`: the short form's apps.
const TOP_N_SHORT: usize = 5;

/// `overview::APP_ROW`: an apps row, and what a dragged panel adds a row
/// for.
const APP_ROW: f32 = 34.;

/// `overview::PULSE_H`: the short form's processor chart.
const PULSE_H: f32 = 36.;

/// `overview::NOTICE_ROW`.
const NOTICE_ROW: f32 = 36.;

/// `overview::RISE_FLOOR`: a tree climbing less than this is not called
/// climbing. Display only.
const RISE_FLOOR: f32 = 15.0;

/// `overview::MEM_RISE_N`: how many climbers the memory card's tag lists
/// on hover.
const MEM_RISE_N: usize = 3;

/// Overview's page, in the form the preference picks.
pub fn page(live: Live) -> AnyElement {
    let sampled = live.memo(|state: &ZStatsAppState| state.latest().is_some());
    let detail = live.watch(prefs::overview_detail);
    dynamic(move || {
        if !sampled.get() {
            return widgets::empty_card(
                i18n::tr("common.waiting_sample"),
                i18n::tr("common.waiting_sample_body"),
            );
        }
        if detail.get() {
            return long_form(live);
        }
        short_form(live)
    })
}

fn short_form(live: Live) -> AnyElement {
    let shown = live.memo(notice::current);
    view()
        .gap(px(CARD_GAP))
        .child(when(
            move || shown.with(Option::is_some),
            move || notice_row(shown),
        ))
        .child(vitals_card(live))
        .child(top_apps(live, false))
        .into_element()
}

fn long_form(live: Live) -> AnyElement {
    view()
        .gap(px(CARD_GAP))
        .child(processor_card(live))
        .child(top_apps(live, true))
        .child(memory_card(live))
        .child(io_card(live))
        .into_element()
}

/// Whether the notice is an alarm — an alert behind the sentence, red —
/// or a watcher's news, in the card's own fill.
fn alarming(notice: &Option<Notice>) -> bool {
    !matches!(notice, Some(Notice::Busy(_) | Notice::Growing(_)))
}

fn notice_sentence(notice: &Notice) -> String {
    match notice {
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
    }
}

/// One sentence for the machine, the whole row the way into Alerts.
fn notice_row(shown: Memo<Option<Notice>>) -> View {
    let alarm = move || shown.with(alarming);
    let fill = move || {
        if alarm() {
            palette::accent_wash(10)
        } else {
            palette::surface()
        }
    };
    let halo = move || {
        if alarm() {
            palette::accent_wash(18)
        } else {
            palette::chip()
        }
    };
    let dot = move || {
        if alarm() {
            palette::accent_light()
        } else {
            palette::gauge()
        }
    };
    view()
        .tooltip(widgets::wrap_tooltip(i18n::tr("overview.status_tip")))
        .child(
            nsh_base::button()
                .flex_row()
                .h(px(NOTICE_ROW))
                .items_center()
                .gap(px(8.))
                .px(px(12.))
                .rounded(px(10.))
                .bg(move || theme::color(fill))
                .hover(move |s| s.bg(theme::color(halo)))
                .on_press(|| store::update(|state, fx| state.set_tab(Tab::Alerts, fx)))
                .child(
                    // The dot in its halo: the same colour, thinned, 3px out.
                    view()
                        .flex_none()
                        .w(px(13.))
                        .h(px(13.))
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(move || theme::color(halo))
                        .child(
                            view()
                                .w(px(7.))
                                .h(px(7.))
                                .rounded_full()
                                .bg(move || theme::color(dot)),
                        ),
                )
                .child(
                    text(move || {
                        shown.with(|n| n.as_ref().map(notice_sentence).unwrap_or_default())
                    })
                    .flex_1()
                    .number_of_lines(1u32)
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::color(palette::text)),
                )
                .child(widgets::icon(
                    crate::assets::CustomIconName::ChevronRight,
                    12.,
                    palette::text_muted,
                )),
        )
}

/// A share fact's value: the percent, whether an alert is behind it, and
/// the words that say what it leaves.
#[derive(Clone, PartialEq, Default)]
struct Share {
    percent: String,
    hot: bool,
    words: String,
}

/// What the vitals card shows.
#[derive(Clone, PartialEq, Default)]
struct Vitals {
    cpu: String,
    span: Option<String>,
    memory: Share,
    disk: Option<Share>,
    network: Option<String>,
    heat: Option<String>,
}

fn vitals(state: &ZStatsAppState) -> Vitals {
    let Some(snapshot) = state.latest().map(|t| &t.snapshot) else {
        return Vitals::default();
    };
    let now = Instant::now();
    let mem = &snapshot.memory;
    let memory_alarm = state.memory_needs_attention();
    let available = t!(
        "overview.mem_available",
        n = format::gb(mem.available_bytes)
    )
    .to_string();
    let critical = mem
        .pressure_level
        .is_some_and(|level| level >= pressure::PRESSURE_CRITICAL);
    let memory = Share {
        percent: format!("{}%", format::whole_pct(mem.used_percent)),
        hot: memory_alarm,
        words: if critical {
            format!("{} · {available}", i18n::tr("overview.pressure_critical"))
        } else {
            available
        },
    };
    let disk = boot_disk(snapshot).map(|disk| Share {
        percent: format!("{}%", format::whole_pct(disk.used_percent)),
        hot: state.disk_needs_attention(),
        words: t!(
            "overview.disk_free",
            n = format::capacity(disk.available_bytes)
        )
        .to_string(),
    });
    let io = &snapshot.io_totals;
    let (down, up) = (
        io.network_received_bytes_per_sec,
        io.network_transmitted_bytes_per_sec,
    );
    Vitals {
        cpu: format::whole_pct(snapshot.cpu.usage_percent),
        span: state
            .cpu_series()
            .span(now)
            .map(|span| t!("overview.pulse_span", span = series::span_label(span)).to_string()),
        memory,
        disk,
        network: (down.is_some() || up.is_some())
            .then(|| format!("↓ {} ↑ {}", format::rate(down), format::rate(up))),
        heat: hottest(snapshot).map(|hottest| format!("{hottest:.0} °C")),
    }
}

fn boot_disk(snapshot: &SystemSnapshot) -> Option<&zstats::snapshot::DiskSnapshot> {
    snapshot
        .disks
        .as_deref()
        .and_then(|disks| disks.iter().find(|disk| disk.mount_point == "/"))
}

fn hottest(snapshot: &SystemSnapshot) -> Option<f32> {
    snapshot
        .temperatures
        .as_deref()
        .filter(|temps| !temps.is_empty())
        .map(|temps| temps.iter().map(|t| t.celsius).fold(f32::MIN, f32::max))
}

/// The processor's half hour, bucketed, as the short form draws it.
fn cpu_chart(state: &ZStatsAppState) -> Chart {
    let now = Instant::now();
    let series = state.cpu_series();
    let points = chart::buckets(series.points(), now);
    Chart {
        scale: chart::cpu_axis_top(&points),
        span: series.span(now),
        marks: vec![points],
        now,
        stroke: CurveStroke::Smooth,
    }
}

/// The axis top in the chart's corner, once the stroke is a line.
fn cpu_corner(state: &ZStatsAppState) -> Option<String> {
    let points = chart::buckets(state.cpu_series().points(), Instant::now());
    chart::stroke_is_a_line(&points)
        .then(|| chart::scale_label(ChartUnit::Percent, chart::cpu_axis_top(&points)))
}

/// One picture and four facts (`overview::vitals_card`).
fn vitals_card(live: Live) -> View {
    let v = live.memo(vitals);
    let tip = format!(
        "{} {}",
        i18n::tr("overview.cpu_curve_tip"),
        i18n::tr("overview.chart_gap_tip")
    );
    widgets::card()
        .child(
            view()
                .gap(px(8.))
                .child(
                    view()
                        .flex_row()
                        .items_baseline()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            view()
                                .flex_row()
                                .min_w(px(0.))
                                .items_baseline()
                                .gap(px(8.))
                                .child(
                                    text(i18n::tr("overview.processor"))
                                        .flex_none()
                                        .text_size(px(13.))
                                        .line_height(px(16.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme::color(palette::text)),
                                )
                                .child(
                                    text(move || v.with(|v| v.span.clone().unwrap_or_default()))
                                        .number_of_lines(1u32)
                                        .tabular_nums()
                                        .text_size(px(12.))
                                        .line_height(px(16.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme::color(palette::text_muted)),
                                ),
                        )
                        .child(
                            view()
                                .flex_row()
                                .flex_none()
                                .items_baseline()
                                .gap(px(1.))
                                .child(
                                    text(move || v.with(|v| v.cpu.clone()))
                                        .tabular_nums()
                                        .text_size(px(20.))
                                        .line_height(px(24.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme::color(palette::text)),
                                )
                                .child(
                                    text("%")
                                        .text_size(px(13.))
                                        .line_height(px(16.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme::color(palette::text_muted)),
                                ),
                        ),
                )
                .child(
                    view()
                        .tooltip(widgets::wrap_tooltip(tip))
                        .child(canvas_chart::well(
                            move || {
                                live.track();
                                store::read(cpu_chart)
                            },
                            PULSE_H,
                            move || {
                                live.track();
                                store::read(cpu_corner)
                            },
                        )),
                ),
        )
        .child(facts(v))
}

/// Ink of the words after a share: the alarm's when an alert is behind
/// the fact, the second ink when not.
fn fact_ink(hot: bool) -> crate::color::Rgba {
    if hot {
        palette::accent_light()
    } else {
        palette::text_muted()
    }
}

/// The card's facts under a hairline: memory and the disk, then the
/// network's two rates and the hottest sensor, two to a row.
fn facts(v: Memo<Vitals>) -> View {
    let memory = fact(
        i18n::tr("overview.fact_memory"),
        share(
            move || v.with(|v| v.memory.clone()),
            move |s: Share| {
                text(s.words)
                    .number_of_lines(1u32)
                    .tabular_nums()
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(theme::color(move || fact_ink(s.hot)))
                    .into_element()
            },
        ),
    );
    let disk = when(
        move || v.with(|v| v.disk.is_some()),
        move || {
            fact(
                i18n::tr("overview.fact_disk"),
                share(
                    move || v.with(|v| v.disk.clone().unwrap_or_default()),
                    move |s: Share| {
                        // The link's hover box pulled back, so its words
                        // start where memory's do.
                        view()
                            .ml(px(-8.))
                            .my(px(-4.))
                            .child(widgets::quiet_link(
                                s.words,
                                widgets::wrap_tooltip(i18n::tr("disk.storage_hint")),
                                move || fact_ink(s.hot),
                                || root::ask(HostAction::Open(AuxWindow::Storage)),
                            ))
                            .into_element()
                    },
                ),
            )
        },
    );
    let network = when(
        move || v.with(|v| v.network.is_some()),
        move || {
            fact(
                i18n::tr("overview.fact_network"),
                text(move || v.with(|v| v.network.clone().unwrap_or_default()))
                    .font_family(widgets::MONO)
                    .number_of_lines(1u32)
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(theme::color(palette::text))
                    .into_element(),
            )
        },
    );
    let heat = when(
        move || v.with(|v| v.heat.is_some()),
        move || {
            fact(
                i18n::tr("overview.fact_hottest"),
                text(move || v.with(|v| v.heat.clone().unwrap_or_default()))
                    .tabular_nums()
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(theme::color(palette::text))
                    .into_element(),
            )
        },
    );
    view()
        .mt(px(16.))
        .gap(px(12.))
        .child(view().h(px(1.)).bg(theme::color(palette::border_subtle)))
        .child(view().flex_row().gap(px(12.)).child(memory).child(disk))
        .child(view().flex_row().gap(px(12.)).child(network).child(heat))
}

/// One fact: a small label over its value, half the card wide.
fn fact(label: String, value: AnyElement) -> View {
    view()
        .flex_1()
        .min_w(px(0.))
        .gap(px(4.))
        .child(
            text(label)
                .text_size(px(11.))
                .line_height(px(14.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(palette::text_dim)),
        )
        .child(value)
}

/// A share fact's value: the percent in the first ink — the alarm's when
/// an alert is behind it — then what `rest` makes of the words. The words
/// part is made again when the share changes.
fn share(
    read: impl Fn() -> Share + Copy + 'static,
    rest: impl Fn(Share) -> AnyElement + 'static,
) -> AnyElement {
    view()
        .flex_row()
        .min_w(px(0.))
        .items_baseline()
        .gap(px(8.))
        .child(
            text(move || read().percent)
                .flex_none()
                .tabular_nums()
                .text_size(px(12.))
                .line_height(px(16.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || theme::color(move || palette::text_for(read().hot))),
        )
        .child(dynamic(move || rest(read())))
        .into_element()
}

/// How a top app's percent is inked (`overview::app_pct_ink`).
#[derive(Clone, Copy, PartialEq)]
enum PctInk {
    Idle,
    Plain,
    Hot,
}

/// One row of the apps card.
#[derive(Clone, PartialEq)]
struct AppRow {
    pid: u32,
    name: String,
    title: String,
    job: Option<String>,
    pct: String,
    ink: PctInk,
    rise: Option<String>,
    /// The first row after the climbers, which says the rest are the
    /// current ranking (`overview.busy_now`).
    fill_word: bool,
}

/// What the apps card shows: which question it answers, and its rows.
#[derive(Clone, PartialEq)]
struct TopApps {
    by_cpu: bool,
    rows: Vec<AppRow>,
}

/// How many rows the card shows (`overview::apps_rows`): the form's
/// count, and one more for each row's worth of panel dragged on top of it.
fn rows_wanted(state: &ZStatsAppState, detail: bool) -> usize {
    let base = if detail { TOP_N } else { TOP_N_SHORT };
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

fn top_apps_of(state: &ZStatsAppState, detail: bool) -> Option<TopApps> {
    let tick = state.latest()?;
    let groups = tick.snapshot.process_groups.as_deref()?;
    let processes = tick
        .snapshot
        .processes
        .as_deref()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let topology = state.member_processes().unwrap_or(processes);
    let n = rows_wanted(state, detail);
    let mut risers: Vec<_> = groups
        .iter()
        .filter_map(|g| {
            state
                .app_rise(trend::tree_key(g))
                .filter(|delta| *delta >= RISE_FLOOR)
                .map(|delta| (g, delta))
        })
        .collect();
    let by_cpu = risers.is_empty();
    risers.sort_by(|a, b| b.1.total_cmp(&a.1));
    let ranked = pad_rising(risers, groups, n);
    let first_fill = ranked
        .first()
        .is_some_and(|(_, delta)| delta.is_some())
        .then(|| ranked.iter().position(|(_, delta)| delta.is_none()))
        .flatten();
    let rows = ranked
        .into_iter()
        .enumerate()
        .map(|(i, (g, delta))| {
            let hot = hot::is_hot(f64::from(g.cpu_usage_percent), false)
                && !hot::own_check_cpu(g.root_pid, state);
            let face = trend::tree_face(g, topology, processes, state.member_pgids());
            let pct = format::pct(g.cpu_usage_percent);
            AppRow {
                pid: g.root_pid,
                name: g.name.clone(),
                title: face.title,
                job: face.job,
                ink: if hot {
                    PctInk::Hot
                } else if pct == "0.0%" {
                    PctInk::Idle
                } else {
                    PctInk::Plain
                },
                pct,
                rise: delta.map(|delta| format!("↑{}", format::pct(delta))),
                fill_word: Some(i) == first_fill,
            }
        })
        .collect();
    Some(TopApps { by_cpu, rows })
}

/// Climbers first (with their delta), then current CPU to fill `n`
/// (`overview::pad_rising`).
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

/// Who is using the machine, ranked (`overview::top_apps`): climbers this
/// hour when there are any, the current processor ranking otherwise. The
/// two forms differ only in how many rows.
fn top_apps(live: Live, detail: bool) -> AnyElement {
    let top = live.watch(move || store::read(|state| top_apps_of(state, detail)));
    let off = move || top.with(Option::is_none);
    dynamic(move || {
        if off() {
            return widgets::empty_card(
                i18n::tr("overview.top_cpu_off"),
                i18n::tr("overview.top_cpu_off_body"),
            );
        }
        let by_cpu = move || top.with(|t| t.as_ref().is_some_and(|t| t.by_cpu));
        let rows = move || top.with(|t| t.as_ref().map(|t| t.rows.clone()).unwrap_or_default());
        widgets::list_shell()
            .p(px(4.))
            .child(
                view()
                    .flex_row()
                    .h(px(36.))
                    .px(px(8.))
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        view()
                            .flex_row()
                            .min_w(px(0.))
                            .items_center()
                            .gap(px(8.))
                            .child(
                                text(move || {
                                    i18n::tr(if by_cpu() {
                                        "overview.top_cpu"
                                    } else {
                                        "overview.rising_title"
                                    })
                                })
                                .number_of_lines(1u32)
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::color(palette::text)),
                            )
                            .child(when(by_cpu, || {
                                text(i18n::tr("overview.top_by_cpu"))
                                    .flex_none()
                                    .text_size(px(12.))
                                    .text_color(theme::color(palette::text_dim))
                            }))
                            .child(widgets::info_icon(widgets::live_tooltip(move || {
                                i18n::tr(if by_cpu() {
                                    "overview.top_apps_tip"
                                } else {
                                    "overview.rising_tip"
                                })
                            }))),
                    )
                    .child(widgets::quiet_link(
                        i18n::tr("overview.top_cpu_all"),
                        widgets::wrap_tooltip(Tab::Apps.title()),
                        palette::text_muted,
                        || store::update(|state, fx| state.set_tab(Tab::Apps, fx)),
                    )),
            )
            .child(each(rows, |row: &AppRow| row.pid, |row, _| app_row(row)))
            .into_element()
    })
}

fn app_row(row: ReadSignal<AppRow>) -> AnyElement {
    // The mark is made again only when the program on the row changes.
    let ident = computed(move || row.with(|r| (r.pid, r.name.clone(), r.title.clone())));
    let tail = move || row.with(|r| r.job.clone());
    let pct_ink = move || match row.with(|r| r.ink) {
        PctInk::Idle => palette::text_dim(),
        PctInk::Plain => palette::text(),
        PctInk::Hot => palette::accent_light(),
    };
    nsh_base::button()
        .flex_row()
        .h(px(APP_ROW))
        .px(px(8.))
        .items_center()
        .gap(px(10.))
        .rounded(px(7.))
        .hover(|s| s.bg(theme::color(palette::surface_raised)))
        .on_press(move || {
            let pid = row.with(|r| r.pid);
            store::update(|state, fx| state.reveal_app(pid, fx));
        })
        .child(dynamic(move || {
            let (pid, name, title) = ident.get();
            widgets::app_mark(pid, &name, &title)
        }))
        .child(
            view()
                .flex_row()
                .flex_1()
                .min_w(px(0.))
                .items_baseline()
                .gap(px(4.))
                .child(
                    text(move || row.with(|r| format::display_name(&r.title).to_owned()))
                        .flex_shrink(1.)
                        .number_of_lines(1u32)
                        .text_size(px(13.))
                        .text_color(theme::color(palette::text)),
                )
                .child(when(
                    move || tail().is_some(),
                    move || {
                        text(move || format!("· {}", tail().unwrap_or_default()))
                            .flex_none()
                            .text_size(px(13.))
                            .text_color(theme::color(palette::text_muted))
                    },
                )),
        )
        .child(
            view()
                .flex_row()
                .flex_none()
                .items_baseline()
                .gap(px(8.))
                .child(when(
                    move || row.with(|r| r.fill_word),
                    || {
                        view()
                            .tooltip(widgets::wrap_tooltip(i18n::tr("overview.busy_now_tip")))
                            .child(
                                text(i18n::tr("overview.busy_now"))
                                    .text_size(px(11.))
                                    .text_color(theme::color(palette::text_dim)),
                            )
                    },
                ))
                .child(when(
                    move || row.with(|r| r.rise.is_some()),
                    move || {
                        text(move || row.with(|r| r.rise.clone().unwrap_or_default()))
                            .tabular_nums()
                            .text_size(px(11.))
                            .text_color(theme::color(palette::text_muted))
                    },
                ))
                .child(
                    view()
                        .tooltip(widgets::wrap_tooltip(i18n::tr("overview.top_app_pct_tip")))
                        .child(
                            text(move || row.with(|r| r.pct.clone()))
                                .min_w(px(44.))
                                .text_right()
                                .tabular_nums()
                                .text_size(px(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(move || theme::color(pct_ink)),
                        ),
                ),
        )
        .into_element()
}

/// One of the long form's charts (`overview::Chart`): which rings it
/// draws, the lead first, and how it reads.
#[derive(Clone, Copy)]
struct Recent {
    rings: fn(&ZStatsAppState) -> Vec<&Series>,
    /// The arrow before each line's peak in the caption, when a chart has
    /// two lines and the caption has to say which is which.
    arrows: &'static [Option<&'static str>],
    /// The axis top, from the bucketed lines.
    scale: fn(&[Vec<Point>]) -> f64,
    stroke: CurveStroke,
    unit: ChartUnit,
    /// The glyph at the head of the caption, for a chart whose card does
    /// not already say what it is.
    icon: Option<CustomIconName>,
    /// Whether the axis top is printed in the well's corner: the two
    /// charts whose top moves.
    corner_label: bool,
    tip: &'static str,
}

fn cpu_rings(state: &ZStatsAppState) -> Vec<&Series> {
    vec![state.cpu_series()]
}

fn memory_rings(state: &ZStatsAppState) -> Vec<&Series> {
    vec![state.memory_series()]
}

/// Download leads; upload stays on the chart, dimmer.
fn net_rings(state: &ZStatsAppState) -> Vec<&Series> {
    vec![state.net_down_series(), state.net_up_series()]
}

fn cpu_scale(marks: &[Vec<Point>]) -> f64 {
    marks
        .first()
        .map_or(chart::CPU_AXIS_LOW, |points| chart::cpu_axis_top(points))
}

fn memory_scale(_: &[Vec<Point>]) -> f64 {
    MEM_AXIS_TOP
}

/// One ceiling for both lines, so the two heights compare.
fn net_scale(marks: &[Vec<Point>]) -> f64 {
    chart::net_scale(&marks.concat())
}

const CPU_RECENT: Recent = Recent {
    rings: cpu_rings,
    arrows: &[None],
    scale: cpu_scale,
    stroke: CurveStroke::Smooth,
    unit: ChartUnit::Percent,
    icon: None,
    corner_label: true,
    tip: "overview.cpu_curve_tip",
};

const MEMORY_RECENT: Recent = Recent {
    rings: memory_rings,
    arrows: &[None],
    scale: memory_scale,
    stroke: CurveStroke::Bars,
    unit: ChartUnit::Percent,
    icon: None,
    corner_label: false,
    tip: "overview.mem_curve_tip",
};

const NET_RECENT: Recent = Recent {
    rings: net_rings,
    arrows: &[Some("↓"), Some("↑")],
    scale: net_scale,
    stroke: CurveStroke::Smooth,
    unit: ChartUnit::Rate,
    icon: Some(CustomIconName::Network),
    corner_label: true,
    tip: "overview.net_curve_tip",
};

fn chart_of(recent: Recent, state: &ZStatsAppState) -> Chart {
    let now = Instant::now();
    let rings = (recent.rings)(state);
    let marks: Vec<Vec<Point>> = rings
        .iter()
        .map(|ring| match recent.stroke {
            CurveStroke::Bars => chart::bar_buckets(ring.points(), now),
            CurveStroke::Smooth => chart::buckets(ring.points(), now),
        })
        .collect();
    Chart {
        scale: (recent.scale)(&marks),
        span: rings.iter().filter_map(|ring| ring.span(now)).max(),
        marks,
        now,
        stroke: recent.stroke,
    }
}

/// What a chart's caption line and corner say.
#[derive(Clone, PartialEq, Default)]
struct Readout {
    /// Whether any ring holds a point: the chart's slot is there from the
    /// first one, so the card does not jump when the second draws a mark.
    held: bool,
    /// The highest point drawn on each line, arrowed when there are two.
    peaks: Vec<String>,
    span: String,
    corner: Option<String>,
}

fn readout_of(recent: Recent, state: &ZStatsAppState) -> Readout {
    let held = (recent.rings)(state)
        .iter()
        .any(|ring| !ring.points().is_empty());
    let chart = chart_of(recent, state);
    let peaks = recent
        .arrows
        .iter()
        .zip(&chart.marks)
        .map(|(arrow, points)| {
            let value = chart::reading(recent.unit, chart::peak(points));
            match arrow {
                Some(arrow) => format!("{arrow} {value}"),
                None => value,
            }
        })
        .collect();
    Readout {
        held,
        peaks,
        span: chart
            .span
            .map(series::span_label)
            .unwrap_or_else(|| format::PLACEHOLDER.to_string()),
        corner: (recent.corner_label && chart::corner_label_earned(&chart.marks))
            .then(|| chart::scale_label(recent.unit, chart.scale)),
    }
}

/// One chart, once a ring holds a point (`overview::recent_line`): the
/// well, and under it the highest point drawn and how long the window has
/// been collecting.
fn recent_chart(live: Live, recent: Recent) -> AnyElement {
    let read = live.watch(move || store::read(|state| readout_of(recent, state)));
    let tip = format!(
        "{} {}",
        i18n::tr(recent.tip),
        i18n::tr("overview.chart_gap_tip")
    );
    when(
        move || read.with(|r| r.held),
        move || {
            view()
                .mt(px(CURVE_TOP))
                .flex_none()
                .tooltip(widgets::wrap_tooltip(&tip))
                .child(canvas_chart::well(
                    move || {
                        live.track();
                        store::read(|state| chart_of(recent, state))
                    },
                    CURVE_H,
                    move || read.with(|r| r.corner.clone()),
                ))
                .child(chart_caption(read, recent))
        },
    )
    .into_element()
}

/// `overview::chart_readout`: the peaks after "Max", the lead's in the
/// first ink and the dimmed line's dimmed — the caption doubles as the
/// key to which stroke is which — and the span at the right.
fn chart_caption(read: Memo<Readout>, recent: Recent) -> View {
    let caption = |words: SharedString| {
        text(words)
            .flex_none()
            .text_size(px(10.))
            .line_height(px(CHART_CAPTION))
            .text_color(theme::color(palette::text_dim))
    };
    let figure = |i: usize, ink: fn() -> crate::color::Rgba| {
        text(move || read.with(|r| r.peaks.get(i).cloned().unwrap_or_default()))
            .flex_none()
            .tabular_nums()
            .text_size(px(10.))
            .line_height(px(CHART_CAPTION))
            .text_color(theme::color(ink))
    };
    let mut peaks = view().flex_row().items_center().gap(px(8.));
    for i in 0..recent.arrows.len() {
        peaks = peaks.child(figure(
            i,
            if i == 0 {
                palette::text
            } else {
                palette::text_muted
            },
        ));
    }
    let mut lead = view().flex_row().items_center().gap(px(4.));
    if let Some(icon) = recent.icon {
        lead = lead.child(widgets::icon(icon, 10., palette::text_dim));
    }
    view()
        .flex_row()
        .mt(px(CHART_CAPTION_GAP))
        .h(px(CHART_CAPTION))
        .w_full()
        .items_center()
        .justify_between()
        .child(
            lead.child(caption(i18n::tr("overview.chart_peak").into()))
                .child(peaks),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .child(caption(i18n::tr("overview.chart_span").into()))
                .child(
                    text(move || read.with(|r| r.span.clone()))
                        .flex_none()
                        .tabular_nums()
                        .text_size(px(10.))
                        .line_height(px(CHART_CAPTION))
                        .text_color(theme::color(palette::text)),
                ),
        )
}

/// A footnote beside a figure: 10px, the third ink, its tooltip.
fn footnote(words: impl Fn() -> String + 'static, tip: TooltipElement) -> View {
    view().flex_none().tooltip(tip).child(
        text(words)
            .tabular_nums()
            .text_size(px(10.))
            .line_height(widgets::line(10.))
            .text_color(theme::color(palette::text_dim)),
    )
}

/// One performance cluster's row.
#[derive(Clone, PartialEq)]
struct Cluster {
    label: String,
    fraction: f32,
    pct: String,
    over: bool,
}

/// What the processor card shows.
#[derive(Clone, PartialEq, Default)]
struct Processor {
    /// Brand plus the rated clock, "Apple M4 Pro (4.5 GHz)".
    caption: String,
    watts: Option<String>,
    usage: String,
    load: String,
    cores: u32,
    uptime: String,
    clusters: Vec<Cluster>,
}

fn processor(state: &ZStatsAppState) -> Processor {
    let Some(snapshot) = state.latest().map(|t| &t.snapshot) else {
        return Processor::default();
    };
    let cpu = &snapshot.cpu;
    let freq = cpu
        .frequency_mhz
        .map(|mhz| format!("{:.1} GHz", mhz as f64 / 1000.0));
    let caption = match (cpu.brand.as_deref(), freq.as_deref()) {
        (Some(brand), Some(freq)) => {
            t!("overview.brand_freq", brand = brand, freq = freq).to_string()
        }
        (Some(brand), None) => brand.to_string(),
        (None, Some(freq)) => freq.to_string(),
        (None, None) => i18n::tr("overview.freq_unknown"),
    };
    let load = &snapshot.load;
    Processor {
        caption,
        watts: snapshot
            .battery
            .as_ref()
            .and_then(|b| b.power_watts)
            .and_then(format::whole_watts)
            .map(|watts| format!("{watts} W")),
        usage: format::whole_pct(cpu.usage_percent),
        load: t!(
            "overview.load",
            one = format::load(load.load1),
            five = format::load(load.load5),
            fifteen = format::load(load.load15)
        )
        .to_string(),
        cores: cpu.logical_cores,
        uptime: t!(
            "overview.uptime",
            time = format::uptime_short(snapshot.host.uptime_secs)
        )
        .to_string(),
        clusters: cpu
            .perf_levels
            .as_ref()
            .filter(|levels| levels.len() > 1)
            .map(|levels| {
                levels
                    .iter()
                    .map(|level| Cluster {
                        label: cluster_label(&level.name, level.logical_cores),
                        fraction: level.usage_percent / 100.0,
                        pct: format::pct_col(level.usage_percent),
                        over: level.usage_percent > hot::CORE_HOT,
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
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

/// `overview::processor`: the chip and its draw in the header, the figure
/// at the card's edge with load and uptime hugging the right, the half
/// hour, then the performance clusters where there are several.
fn processor_card(live: Live) -> View {
    let p = live.memo(processor);
    let clusters = move || p.with(|p| p.clusters.clone());
    widgets::card()
        .child(widgets::card_header(
            i18n::tr("overview.processor"),
            Some(processor_caption(p)),
        ))
        .child(
            view()
                .flex_row()
                .w_full()
                .items_baseline()
                .justify_between()
                .gap(px(10.))
                .mt(px(4.))
                .child(
                    view()
                        .flex_none()
                        .tooltip(widgets::wrap_tooltip(i18n::tr("overview.usage_tip")))
                        .child(widgets::big_number(
                            move || p.with(|p| p.usage.clone()),
                            "%",
                        )),
                )
                .child(
                    view()
                        .flex_row()
                        .min_w(px(0.))
                        .items_baseline()
                        .justify_end()
                        .gap(px(12.))
                        .child(footnote(
                            move || p.with(|p| p.load.clone()),
                            widgets::live_tooltip(move || {
                                t!("overview.load_tip", cores = p.with(|p| p.cores)).to_string()
                            }),
                        ))
                        .child(footnote(
                            move || p.with(|p| p.uptime.clone()),
                            widgets::wrap_tooltip(i18n::tr("overview.uptime_tip")),
                        )),
                ),
        )
        .child(recent_chart(live, CPU_RECENT))
        .child(when(
            move || p.with(|p| !p.clusters.is_empty()),
            move || {
                view().pt(px(8.)).gap(px(6.)).child(each(
                    clusters,
                    |c: &Cluster| c.label.clone(),
                    |c, _| cluster_row(c),
                ))
            },
        ))
}

/// The chip's name — what gives way when the header is tight — and the
/// live draw when the battery reports one.
fn processor_caption(p: Memo<Processor>) -> AnyElement {
    view()
        .flex_row()
        .min_w(px(0.))
        .items_center()
        .gap(px(8.))
        .child(
            view()
                .min_w(px(0.))
                .tooltip(widgets::wrap_tooltip(i18n::tr("overview.freq_tip")))
                .child(
                    text(move || p.with(|p| p.caption.clone()))
                        .number_of_lines(1u32)
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .text_color(theme::color(palette::text_dim)),
                ),
        )
        .child(when(
            move || p.with(|p| p.watts.is_some()),
            move || {
                footnote(
                    move || p.with(|p| p.watts.clone().unwrap_or_default()),
                    widgets::wrap_tooltip(i18n::tr("overview.watts_tip")),
                )
            },
        ))
        .into_element()
}

fn cluster_row(c: ReadSignal<Cluster>) -> AnyElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .child(
            text(move || c.with(|c| c.label.clone()))
                .w(px(96.))
                .flex_none()
                .number_of_lines(1u32)
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(palette::text_muted)),
        )
        .child(view().flex_1().child(widgets::meter(
            move || c.with(|c| c.fraction),
            move || palette::gauge_for(c.with(|c| c.over)),
            5.,
        )))
        .child(
            text(move || c.with(|c| c.pct.clone()))
                .w(px(44.))
                .flex_none()
                .tabular_nums()
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::text)),
        )
        .into_element()
}

/// The biggest memory climber this hour, for the tag at the right of the
/// memory card's headline row (`overview::mem_climb_tag`).
#[derive(Clone, PartialEq, Default)]
struct Climb {
    lead: String,
    name: String,
    delta: String,
    /// The tooltip's first line: the climbers, up to [`MEM_RISE_N`].
    listed: String,
}

fn climb_of(climbers: &[MemoryCreep], total_bytes: u64) -> Option<Climb> {
    let floor = trend::mem_rise_floor(total_bytes);
    let named: Vec<&MemoryCreep> = climbers.iter().filter(|c| c.climb_bytes >= floor).collect();
    let top = named.first()?;
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
    Some(Climb {
        lead: if named.len() > 1 {
            format!("{} ×{}", i18n::tr("overview.mem_rise_short"), named.len())
        } else {
            i18n::tr("overview.mem_rise_short")
        },
        name: top.name.clone(),
        delta: format!("+{}", format::memory(top.climb_bytes)),
        listed: format!("{}  {listed}", i18n::tr("overview.mem_rise_lead")),
    })
}

/// The compressor's activity at the end of the Swap row
/// (`overview::swap_activity_tag`): the kernel's thrashing verdict alone,
/// or the segments moved per second.
#[derive(Clone, PartialEq)]
struct SwapTag {
    thrashing: bool,
    figures: String,
}

fn swap_tag_of(state: &ZStatsAppState) -> Option<SwapTag> {
    let mem = &state.latest()?.snapshot.memory;
    let thrashing = mem.swap_thrashing == Some(true);
    let moving = pressure::swap_moving(mem.swap_ins_per_sec, mem.swap_outs_per_sec);
    if !thrashing && moving.is_none() {
        return None;
    }
    let (outs, ins) = moving.unwrap_or((0, 0));
    Some(SwapTag {
        thrashing,
        figures: t!(
            "overview.swap_activity",
            outs = format::thousands(outs as usize),
            ins = format::thousands(ins as usize)
        )
        .to_string(),
    })
}

/// What the memory card shows.
#[derive(Clone, PartialEq, Default)]
struct Memory {
    badge: String,
    badge_tip: String,
    /// An alert is behind memory: the badge's red.
    alarm: bool,
    used: String,
    used_of: String,
    climb: Option<Climb>,
    /// The bar's two slices, as shares of the whole memory.
    resident: f32,
    compressed: f32,
    /// The legend's "Compressed 3.2 GB", while there is any.
    compressed_label: Option<String>,
    swap: String,
    swap_hot: bool,
}

fn memory(state: &ZStatsAppState) -> Memory {
    let Some(snapshot) = state.latest().map(|t| &t.snapshot) else {
        return Memory::default();
    };
    let mem = &snapshot.memory;
    let (badge, badge_tip) = pressure::badge(mem, snapshot.capabilities.memory_pressure);
    let total = mem.total_bytes.max(1) as f32;
    let compressed = mem.compressed_bytes.unwrap_or(0).min(mem.used_bytes);
    let resident = mem.used_bytes.saturating_sub(compressed);
    Memory {
        badge,
        badge_tip,
        alarm: state.memory_needs_attention(),
        used: format::gb(mem.used_bytes),
        used_of: t!("overview.used_of", total = format::gb(mem.total_bytes)).to_string(),
        climb: climb_of(&state.memory_climbers(), mem.total_bytes),
        resident: resident as f32 / total,
        compressed: compressed as f32 / total,
        compressed_label: (compressed > 0).then(|| {
            format!(
                "{} {}",
                i18n::tr("overview.compressed"),
                format::gb(compressed)
            )
        }),
        swap: format!(
            "{} / {}",
            format::gb(mem.swap_used_bytes),
            format::gb(mem.swap_total_bytes)
        ),
        swap_hot: hot::swap_hot(mem),
    }
}

/// `overview::memory`: the pressure badge, used of total with the climb
/// tag at the right, the half hour as bars, the bar of used and
/// compressed with its key, and the Swap row closing the card.
fn memory_card(live: Live) -> View {
    let m = live.memo(memory);
    let tag = live.memo(swap_tag_of);
    widgets::card()
        .child(widgets::card_header(
            i18n::tr("overview.memory"),
            Some(pressure_badge(m)),
        ))
        .child(
            view()
                .flex_row()
                .w_full()
                .items_baseline()
                .justify_between()
                .gap(px(10.))
                .mt(px(8.))
                .child(
                    view()
                        .flex_row()
                        .flex_none()
                        .items_baseline()
                        .gap(px(4.))
                        .child(widgets::big_figure(move || m.with(|m| m.used.clone())))
                        .child(
                            text(move || m.with(|m| m.used_of.clone()))
                                .tabular_nums()
                                .text_size(px(12.))
                                .line_height(widgets::line(12.))
                                .text_color(theme::color(palette::text_muted)),
                        ),
                )
                .child(when(
                    move || m.with(|m| m.climb.is_some()),
                    move || climb_tag(m),
                )),
        )
        .child(recent_chart(live, MEMORY_RECENT))
        .child(view().mt(px(10.)).child(widgets::stacked_meter(
            vec![
                Segment {
                    fraction: Box::new(move || m.with(|m| m.resident)),
                    fill: palette::gauge,
                },
                Segment {
                    fraction: Box::new(move || m.with(|m| m.compressed)),
                    fill: canvas_chart::dim_ink,
                },
            ],
            6.,
        )))
        .child(
            view()
                .flex_row()
                .mt(px(8.))
                .gap(px(12.))
                .child(widgets::legend_item(
                    LegendMark::Fill(palette::gauge),
                    i18n::tr("overview.used"),
                    widgets::wrap_tooltip(i18n::tr("overview.used_tip")),
                ))
                .child(when(
                    move || m.with(|m| m.compressed_label.is_some()),
                    move || {
                        widgets::legend_item(
                            LegendMark::Fill(canvas_chart::dim_ink),
                            move || m.with(|m| m.compressed_label.clone().unwrap_or_default()),
                            widgets::wrap_tooltip(i18n::tr("overview.compressed_tip")),
                        )
                    },
                ))
                .child(widgets::legend_item(
                    LegendMark::Hollow,
                    i18n::tr("overview.free"),
                    widgets::wrap_tooltip(i18n::tr("overview.free_tip")),
                )),
        )
        .child(
            view()
                .flex_row()
                .w_full()
                .mt(px(10.))
                .items_baseline()
                .justify_between()
                .gap(px(12.))
                .child(widgets::kv_pair(
                    i18n::tr("overview.swap"),
                    move || m.with(|m| m.swap.clone()),
                    move || m.with(|m| m.swap_hot),
                ))
                .child(when(
                    move || tag.with(Option::is_some),
                    move || swap_tag(tag),
                )),
        )
}

/// The kernel's verdict as a pill — red only when an alert is behind
/// memory, never for the kernel's level alone.
fn pressure_badge(m: Memo<Memory>) -> AnyElement {
    let alarm = move || m.with(|m| m.alarm);
    view()
        .flex_none()
        .tooltip(widgets::live_tooltip(move || {
            m.with(|m| m.badge_tip.clone())
        }))
        .child(
            text(move || m.with(|m| m.badge.clone()))
                .rounded_full()
                .border(px(1.))
                .border_color(move || {
                    theme::color(move || {
                        if alarm() {
                            palette::accent_wash(40)
                        } else {
                            palette::card_line()
                        }
                    })
                })
                .bg(move || {
                    theme::color(move || {
                        if alarm() {
                            palette::accent_wash(14)
                        } else {
                            palette::track()
                        }
                    })
                })
                .px(px(8.))
                .py(px(2.))
                .text_size(px(10.))
                .line_height(widgets::line(10.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(move || {
                    theme::color(move || {
                        if alarm() {
                            palette::accent_light()
                        } else {
                            palette::text_muted()
                        }
                    })
                }),
        )
        .into_element()
}

/// `Climbing  Zed +1.1 GB`, `×3` on the lead when there are more and the
/// rest on hover; the name gives way before the figure does.
fn climb_tag(m: Memo<Memory>) -> View {
    let climb = move || m.with(|m| m.climb.clone().unwrap_or_default());
    view()
        .flex_row()
        .min_w(px(0.))
        .items_baseline()
        .justify_end()
        .gap(px(6.))
        .tooltip(widgets::tooltip_lines(move || {
            vec![climb().listed, i18n::tr("overview.mem_rise_tip")]
        }))
        .child(
            text(move || climb().lead)
                .flex_none()
                .text_size(px(10.))
                .line_height(widgets::line(10.))
                .text_color(theme::color(palette::text_dim)),
        )
        .child(
            text(move || climb().name)
                .flex_shrink(1.)
                .number_of_lines(1u32)
                .tabular_nums()
                .text_size(px(10.))
                .line_height(widgets::line(10.))
                .text_color(theme::color(palette::text_muted)),
        )
        .child(
            text(move || climb().delta)
                .flex_none()
                .tabular_nums()
                .text_size(px(10.))
                .line_height(widgets::line(10.))
                .text_color(theme::color(palette::text_muted)),
        )
}

/// The thrashing verdict in the accent with the figures on hover, or the
/// figures — mono outside a CJK locale, whose words would not be.
fn swap_tag(tag: Memo<Option<SwapTag>>) -> AnyElement {
    let thrashing = computed(move || tag.with(|t| t.as_ref().is_some_and(|t| t.thrashing)));
    let figures = move || tag.with(|t| t.as_ref().map(|t| t.figures.clone()).unwrap_or_default());
    dynamic(move || {
        let row = view()
            .flex_row()
            .min_w(px(0.))
            .items_baseline()
            .justify_end();
        if thrashing.get() {
            return row
                .tooltip(widgets::tooltip_lines(move || {
                    vec![figures(), i18n::tr("overview.swap_activity_tip")]
                }))
                .child(
                    text(i18n::tr("overview.swap_thrashing"))
                        .flex_none()
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .text_color(theme::color(palette::accent)),
                )
                .into_element();
        }
        let words = text(figures)
            .number_of_lines(1u32)
            .text_size(px(10.))
            .line_height(widgets::line(10.))
            .text_color(theme::color(palette::text_muted));
        let words = if i18n::is_cjk() {
            words
        } else {
            words.font_family(widgets::MONO)
        };
        row.tooltip(widgets::wrap_tooltip(i18n::tr(
            "overview.swap_activity_tip",
        )))
        .child(words)
        .into_element()
    })
}

/// The disk's and the network's rates, R and W against ↓ and ↑.
#[derive(Clone, PartialEq, Default)]
struct Rates {
    disk: (String, String),
    net: (String, String),
}

fn rates_of(state: &ZStatsAppState) -> Option<Rates> {
    let io = &state.latest()?.snapshot.io_totals;
    let sides = [
        io.disk_read_bytes_per_sec,
        io.disk_write_bytes_per_sec,
        io.network_received_bytes_per_sec,
        io.network_transmitted_bytes_per_sec,
    ];
    if sides.iter().all(Option::is_none) {
        return None;
    }
    let side = |arrow: &str, rate: Option<u64>| format!("{arrow} {}", format::rate(rate));
    Some(Rates {
        disk: (side("R", sides[0]), side("W", sides[1])),
        net: (side("↓", sides[2]), side("↑", sides[3])),
    })
}

/// `overview::io_card`: the rates row and the network chart under a
/// title; no card while neither has anything.
fn io_card(live: Live) -> AnyElement {
    let rates = live.memo(rates_of);
    let held = live.memo(|state: &ZStatsAppState| {
        net_rings(state)
            .iter()
            .any(|ring| !ring.points().is_empty())
    });
    let pair = move |pick: fn(&Rates) -> &(String, String)| {
        move || rates.with(|r| r.as_ref().map(|r| pick(r).clone()).unwrap_or_default())
    };
    when(
        move || rates.with(Option::is_some) || held.get(),
        move || {
            widgets::card()
                .child(widgets::card_header(i18n::tr("overview.io_title"), None))
                .child(view().mt(px(8.)).child(when(
                    move || rates.with(Option::is_some),
                    move || {
                        view()
                            .flex_row()
                            .gap(px(16.))
                            .child(io_cell(
                                CustomIconName::HardDrive,
                                i18n::tr("overview.io_disk"),
                                pair(|r| &r.disk),
                            ))
                            .child(io_cell(
                                CustomIconName::Network,
                                i18n::tr("overview.io_net"),
                                pair(|r| &r.net),
                            ))
                    },
                )))
                .child(recent_chart(live, NET_RECENT))
        },
    )
    .into_element()
}

/// One half of the rates row: the glyph, named on hover, and its two
/// rates in the mono face.
fn io_cell(
    icon: CustomIconName,
    label: String,
    pair: impl Fn() -> (String, String) + Copy + 'static,
) -> View {
    let rate = |words: Box<dyn Fn() -> String>| {
        text(words)
            .font_family(widgets::MONO)
            .text_size(px(10.))
            .line_height(widgets::line(10.))
            .text_color(theme::color(palette::text_muted))
    };
    view()
        .flex_row()
        .flex_1()
        .min_w(px(0.))
        .items_center()
        .gap(px(6.))
        .child(
            view()
                .flex_none()
                .tooltip(widgets::wrap_tooltip(label))
                .child(widgets::icon(icon, 12., palette::text_dim)),
        )
        .child(
            view()
                .flex_row()
                .gap(px(6.))
                .child(rate(Box::new(move || pair().0)))
                .child(rate(Box::new(move || pair().1))),
        )
}
