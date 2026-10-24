//! Overview on NSH: the gpui front end's `views/overview.rs`, whose comments
//! say why every row, figure and colour is the way it is — this file
//! carries the same choices and does not repeat their reasons.
//!
//! The short form (phase 4b of docs/nsh-migration.md): the notice row while
//! there is something to announce, the vitals card (the processor's figure
//! and its half hour as a line, then four facts), and the apps card. The
//! long form arrives in phase 4c.
//!
//! What the page reads is gathered into a few plain values (`Vitals`,
//! `TopApps`, the notice) as memos of the store, and the elements bind to
//! them: a tick re-reads the store, and only what came out different is
//! drawn again. A row of the apps card is kept by its pid (`each`), so a
//! program that stays in the list keeps its row — and its icon — while its
//! figure moves.

use crate::chart::{ChartUnit, CurveStroke};
use crate::notice::{self, Notice};
use crate::nsh::chart::{self as canvas_chart, Chart};
use crate::nsh::root::{self, AuxWindow, CARD_GAP, HostAction, Live};
use crate::nsh::widgets;
use crate::nsh::{store, theme};
use crate::state::{Tab, ZStatsAppState};
use crate::{chart, format, hot, i18n, palette, placement, prefs, series, trend};
use nsh_core::ui::prelude::*;
use rust_i18n::t;
use std::collections::HashSet;
use std::time::Instant;
use zstats::snapshot::{ProcessGroupSnapshot, SystemSnapshot};

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

/// `overview::PRESSURE_CRITICAL`: the kernel's pressure level at
/// critical, the only tier that earns a word beside memory's.
const PRESSURE_CRITICAL: u32 = 4;

/// Overview's page: the short form, or (until phase 4c) a card where the
/// long one will be.
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
            return widgets::empty_card(
                "Overview".to_owned(),
                "The detailed form arrives in phase 4c of docs/nsh-migration.md.".to_owned(),
            );
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
        .child(top_apps(live))
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
        .is_some_and(|level| level >= PRESSURE_CRITICAL);
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
fn rows_wanted(state: &ZStatsAppState, base: usize) -> usize {
    let Some(bounds) = state.window_bounds() else {
        return base;
    };
    let notice = notice::has_notice(state);
    let (_, own) = placement::window_size(false, notice, placement::Dragged::default());
    let spare = bounds.size.height - own;
    if spare < APP_ROW {
        return base;
    }
    base + (spare / APP_ROW).floor() as usize
}

fn top_apps_of(state: &ZStatsAppState) -> Option<TopApps> {
    let tick = state.latest()?;
    let groups = tick.snapshot.process_groups.as_deref()?;
    let processes = tick
        .snapshot
        .processes
        .as_deref()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let topology = state.member_processes().unwrap_or(processes);
    let n = rows_wanted(state, TOP_N_SHORT);
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
/// hour when there are any, the current processor ranking otherwise.
fn top_apps(live: Live) -> AnyElement {
    let top = live.memo(top_apps_of);
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
