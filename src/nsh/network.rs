//! Network on NSH: the gpui front end's `views/traffic.rs`,
//! `listening.rs` and `net.rs`, whose comments say why every card, figure
//! and control is the way it is — this file carries the same choices and
//! does not repeat their reasons. The cards' rules are shared
//! (`traffic.rs`, `listening.rs`, `net.rs`).
//!
//! Who is moving bytes, then who is listening, then the interfaces. Each
//! card binds to a plain value the page gathers from the store as a memo;
//! the lists are kept by their keys (`each`: a listener by its owner, an
//! interface by its name), so a tick changes figures, not elements. The
//! traffic rows are kept by their place instead, and a place the list no
//! longer reaches is folded rather than dropped: each row draws its line
//! on a canvas, a new canvas is drawn asynchronously until the next tick
//! turns that off (`host::draw_canvases_in_place`), and every such drawing
//! held about 100 MB for a second. Keyed by program, the rows were new
//! canvases whenever a program started or stopped moving bytes — every
//! few seconds with the list expanded.

use crate::color::Rgba;
use crate::listening::{self, Owner, Row};
use crate::net::{self, Ifaces};
use crate::nsh::chart;
use crate::nsh::lists::{self, LISTEN_FILTER};
use crate::nsh::root::{CARD_GAP, CARD_PAD, Live};
use crate::nsh::{store, theme, widgets};
use crate::state::{ListenerView, TrafficView, ZStatsAppState};
use crate::traffic::{self, CurvePoint, ProgramRate};
use crate::{format, i18n, palette, terminate};
use nsh_core::ui::prelude::*;
use rust_i18n::t;
use std::cell::Cell;
use std::time::{Duration, Instant};
use zstats::snapshot::Capabilities;

/// A traffic row's well, its inset and its corner (`traffic::CURVE_H`,
/// `WELL_PAD_X` / `WELL_PAD_Y`, `WELL_RADIUS`), and the line's stroke.
const CURVE_H: f32 = 22.;
const WELL_PAD_X: f32 = 4.;
const WELL_PAD_Y: f32 = 2.;
const WELL_RADIUS: f32 = 5.;
const CURVE_STROKE: f32 = 1.5;

/// More than a traffic row is ever tall: the open place's bound.
const FAR: f32 = 1000.;

/// A listener's Quit, and so every row's second line
/// (`listening::QUIT_HEIGHT`).
const QUIT_HEIGHT: f32 = 16.;

/// The Network page. A card this platform cannot fill is not there,
/// rather than an apology on every visit.
pub fn page(live: Live) -> AnyElement {
    let can = Capabilities::current();
    let mut cards = view().gap(px(CARD_GAP));
    if can.process_traffic {
        cards = cards.child(traffic_card(live));
    }
    if can.listeners {
        cards = cards.child(listening_card(live));
    }
    cards.child(interfaces_card(live)).into_element()
}

/// The line under a list's header that stands in for its rows.
fn note_line(words: String) -> AnyElement {
    view()
        .px(px(CARD_PAD))
        .pb(px(11.))
        .child(
            text(words)
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::text_dim)),
        )
        .into_element()
}

/// What the traffic card shows.
#[derive(Clone, PartialEq)]
struct TrafficCard {
    tip: String,
    /// How long the curves cover, beside the chip
    span: Option<String>,
    chip: Option<(String, bool)>,
    /// Instead of rows: reading, restricted, failed, or nobody talking
    note: Option<String>,
    rows: Vec<Program>,
}

/// One program's row.
#[derive(Clone, Default, PartialEq)]
struct Program {
    name: String,
    /// The first pid and the kernel's name, for the app's mark
    mark: (u32, String, String),
    pid: Option<String>,
    pids_tip: Option<String>,
    down: String,
    up: String,
    /// The line on a unit square (`traffic::trace`)
    runs: Vec<Vec<(f32, f32)>>,
    last: bool,
}

fn traffic_of(state: &ZStatsAppState) -> TrafficCard {
    let note = |words: String, coverage| TrafficCard {
        tip: traffic::tip(coverage),
        span: None,
        chip: None,
        note: Some(words),
        rows: Vec::new(),
    };
    let ready = match state.traffic() {
        None | Some(TrafficView::Reading) => return note(i18n::tr("traffic.reading"), None),
        Some(TrafficView::Restricted) => return note(i18n::tr("traffic.restricted"), None),
        Some(TrafficView::Failed(error)) => {
            return note(
                t!("traffic.failed", error = error.clone()).to_string(),
                None,
            );
        }
        Some(TrafficView::Ready(ready)) => ready,
    };
    let rows = &ready.rows;
    if rows.is_empty() {
        let words = if ready.process_count == 0 {
            "traffic.none"
        } else {
            "traffic.quiet"
        };
        return note(i18n::tr(words), Some(ready.coverage));
    }
    // One `now` for every row, so a point sits on the same x in each.
    let now = Instant::now();
    let curves: Vec<&[CurvePoint]> = rows.iter().map(|row| state.traffic_curve(row)).collect();
    let preview = traffic::preview(&traffic::averages(rows, &curves, now));
    let hidden = rows.len() - preview.len();
    let show_all = state.show_all_traffic();
    let shown: Vec<usize> = if show_all {
        (0..rows.len()).collect()
    } else {
        preview
    };
    let chip = (hidden > 0 || show_all).then(|| {
        let label = if show_all {
            i18n::tr("traffic.hide")
        } else {
            t!("traffic.show_more", count = hidden).to_string()
        };
        (label, show_all)
    });
    let span = state.traffic_span(now);
    let last = shown.len() - 1;
    TrafficCard {
        tip: traffic::tip(Some(ready.coverage)),
        span: span.map(traffic::span_label),
        chip,
        note: None,
        rows: shown
            .iter()
            .enumerate()
            .map(|(i, &index)| program(&rows[index], curves[index], span, now, i == last))
            .collect(),
    }
}

fn program(
    row: &ProgramRate,
    points: &[CurvePoint],
    span: Option<Duration>,
    now: Instant,
    last: bool,
) -> Program {
    let name = row
        .name
        .clone()
        .unwrap_or_else(|| i18n::tr("traffic.unknown"));
    Program {
        mark: (
            row.pids.first().copied().unwrap_or(0),
            row.name.clone().unwrap_or_default(),
            name.clone(),
        ),
        name,
        pid: traffic::pid_label(row),
        pids_tip: traffic::pids_tip(row),
        down: format!("↓ {}", format::rate(row.received_per_sec)),
        up: format!("↑ {}", format::rate(row.transmitted_per_sec)),
        runs: span
            .map(|span| traffic::trace(points, span, now))
            .unwrap_or_default(),
        last,
    }
}

fn traffic_card(live: Live) -> AnyElement {
    let card = live.memo(traffic_of);
    let tip = computed(move || card.with(|c| c.tip.clone()));
    let trailing = computed(move || card.with(|c| (c.span.clone(), c.chip.clone())));
    let note = computed(move || card.with(|c| c.note.clone()));
    widgets::list_shell()
        .child(widgets::list_header(
            lists::count_title(
                || i18n::tr("traffic.title"),
                widgets::live_tooltip(move || tip.get()),
            ),
            Some(
                dynamic(move || {
                    let (span, chip) = trailing.get();
                    traffic_trailing(span, chip)
                })
                .into_element(),
            ),
        ))
        .child(dynamic(move || match note.get() {
            Some(words) => note_line(words),
            None => {
                // As many places as the card has shown rows since it was
                // made; the ones past today's list are folded.
                let most = Cell::new(0);
                let places = computed(move || {
                    let n = card.with(|c| c.rows.len()).max(most.get());
                    most.set(n);
                    n
                });
                each(
                    move || (0..places.get()).collect::<Vec<_>>(),
                    |place: &usize| *place,
                    move |place, _| program_row(card, place.get_untracked()),
                )
                .into_element()
            }
        }))
        .into_element()
}

/// The real span, dim, beside the show-more chip.
fn traffic_trailing(span: Option<String>, chip: Option<(String, bool)>) -> AnyElement {
    let mut row = view().flex_row().items_center().gap(px(8.));
    if let Some(span) = span {
        row = row.child(
            text(span)
                .flex_none()
                .text_size(px(10.))
                .line_height(widgets::line(10.))
                .text_color(theme::color(palette::text_dim)),
        );
    }
    if let Some((label, showing)) = chip {
        row = row.child(widgets::more_chip(label, showing, || {
            store::update(|state, fx| state.toggle_all_traffic(fx))
        }));
    }
    row.into_element()
}

/// The row at `place`: who and how fast on one line, beside the app's
/// mark, the curve under — folded to nothing while the list is shorter.
fn program_row(card: Memo<TrafficCard>, place: usize) -> AnyElement {
    let slot = computed(move || card.with(|c| c.rows.get(place).cloned()));
    let shown = computed(move || slot.with(Option::is_some));
    let p = computed(move || slot.get().unwrap_or_default());
    let mark = computed(move || p.with(|p| p.mark.clone()));
    let pid = computed(move || p.with(|p| p.pid.clone()));
    let pids_tip = computed(move || p.with(|p| p.pids_tip.clone()));
    view()
        .px(px(CARD_PAD))
        .py(move || px(if shown.get() { 8. } else { 0. }))
        .max_h(move || px(if shown.get() { FAR } else { 0. }))
        .overflow_hidden()
        .border_b(move || {
            px(if !shown.get() || p.with(|p| p.last) {
                0.
            } else {
                1.
            })
        })
        .border_color(theme::color(palette::border_subtle))
        .child(
            view()
                .flex_row()
                .items_start()
                .gap(px(10.))
                .child(dynamic(move || {
                    let (pid, name, title) = mark.get();
                    widgets::app_mark(pid, &name, &title)
                }))
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.))
                        .child(
                            view()
                                .flex_row()
                                .items_baseline()
                                .justify_between()
                                .gap(px(8.))
                                .child(
                                    view()
                                        .flex_row()
                                        .items_baseline()
                                        .gap(px(6.))
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(widgets::name(
                                            move || p.with(|p| p.name.clone()),
                                            11.,
                                            FontWeight::MEDIUM,
                                        ))
                                        .child(when(
                                            move || pid.with(Option::is_some),
                                            move || {
                                                view()
                                                    .flex_none()
                                                    .tooltip(
                                                        widgets::live_tooltip(move || {
                                                            pids_tip.get().unwrap_or_default()
                                                        })
                                                        .disabled(move || {
                                                            pids_tip.with(Option::is_none)
                                                        }),
                                                    )
                                                    .child(
                                                        text(move || pid.get().unwrap_or_default())
                                                            .text_size(px(10.))
                                                            .line_height(widgets::line(10.))
                                                            .text_color(theme::color(
                                                                palette::text_dim,
                                                            )),
                                                    )
                                            },
                                        )),
                                )
                                .child(
                                    view()
                                        .flex_row()
                                        .flex_none()
                                        .gap(px(8.))
                                        .child(rate(move || p.with(|p| p.down.clone())))
                                        .child(rate(move || p.with(|p| p.up.clone()))),
                                ),
                        )
                        .child(
                            view()
                                .mt(px(4.))
                                .h(px(CURVE_H))
                                .px(px(WELL_PAD_X))
                                .py(px(WELL_PAD_Y))
                                .rounded(px(WELL_RADIUS))
                                .bg(theme::color(palette::inset))
                                .child(sparkline(move || p.with(|p| p.runs.clone()))),
                        ),
                ),
        )
        .into_element()
}

/// A rate in the mono face, in the first ink.
fn rate(words: impl Fn() -> String + 'static) -> Text {
    text(words)
        .font_family(widgets::MONO)
        .text_size(px(11.))
        .line_height(widgets::line(11.))
        .text_color(theme::color(palette::text))
}

/// One ↓+↑ line across the well, from `traffic::trace`'s runs.
fn sparkline(runs: impl Fn() -> Vec<Vec<(f32, f32)>> + 'static) -> AnyElement {
    let size = signal((0.0_f32, 0.0_f32));
    canvas(move || {
        let (w, h) = size.get();
        let dark = dark();
        let runs = runs();
        palette::in_mode(dark, || paint_trace(&runs, w, h))
    })
    .size_full()
    .on_layout(move |b: &Bounds<Points>| size.set((b.size.width.0, b.size.height.0)))
    .into_element()
}

fn paint_trace(runs: &[Vec<(f32, f32)>], w: f32, h: f32) -> Painter {
    let mut p = Painter::new();
    // Half the stroke, rounded up, so a peak on the top edge is not cut.
    let (top, bottom) = (1., h - 1.);
    if w <= 0. || bottom <= top {
        return p;
    }
    let height = bottom - top;
    for run in runs {
        let mut steps = run.iter().map(|&(x, y)| (w * x, bottom - height * y));
        let Some((x0, y0)) = steps.next() else {
            continue;
        };
        let mut path = p.begin_path().move_to(x0, y0);
        for (x, y) in steps {
            path = path.line_to(x, y);
        }
        p = path
            .stroke_color(chart::ink(palette::ink()))
            .line_width(CURVE_STROKE)
            .stroke();
    }
    p
}

/// What the listening card shows.
#[derive(Clone, PartialEq)]
struct ListenCard {
    tip: String,
    /// A list is in hand: the header has its search and its chip
    ready: bool,
    chip: Option<(String, bool)>,
    /// Instead of rows: reading, restricted, failed, nothing, no match
    note: Option<String>,
    rows: Vec<Listener>,
}

/// One process name's row.
#[derive(Clone, PartialEq)]
struct Listener {
    key: Owner,
    owner: String,
    pid: Option<String>,
    /// The age and what it means
    age: Option<(String, String)>,
    endpoints: String,
    exposed: bool,
    lines: Vec<String>,
    /// The pid, the name it was shown under, and the confirm's words
    quit: Option<(u32, String, String)>,
    last: bool,
}

fn listening_of(state: &ZStatsAppState) -> ListenCard {
    let note = |words: String| ListenCard {
        tip: listening::tip(None),
        ready: false,
        chip: None,
        note: Some(words),
        rows: Vec::new(),
    };
    let listeners = match state.listeners() {
        None => return note(i18n::tr("listen.reading")),
        Some(ListenerView::Restricted) => return note(i18n::tr("listen.restricted")),
        Some(ListenerView::Failed(error)) => {
            return note(t!("listen.failed", error = error.clone()).to_string());
        }
        Some(ListenerView::Ready(listeners)) => listeners,
    };
    let query = state.listen_filter_text();
    let card = listening::card(listeners, query, state.show_all_listeners());
    let note = card.rows.is_empty().then(|| {
        if query.is_empty() {
            i18n::tr("listen.none")
        } else {
            t!("listen.no_match", query = query).to_string()
        }
    });
    let last = card.rows.len().saturating_sub(1);
    ListenCard {
        tip: listening::tip(Some(listeners.coverage)),
        ready: true,
        chip: card.chip.map(|chip| (chip.label(), chip.showing)),
        note,
        rows: card
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| listener(row, i == last))
            .collect(),
    }
}

fn listener(row: &Row, last: bool) -> Listener {
    Listener {
        key: row.owner.clone(),
        owner: row.owner_label(),
        pid: row.pid_label(),
        age: row.age_label().map(|label| (label, row.age_tip())),
        endpoints: row.endpoints_line(),
        exposed: row.exposed(),
        lines: row.endpoint_lines(),
        quit: listening::quit_target(row).map(|(pid, name)| {
            let body = row.quit_body(pid, &name);
            (pid, name, body)
        }),
        last,
    }
}

fn listening_card(live: Live) -> AnyElement {
    let card = live.memo(listening_of);
    let tip = computed(move || card.with(|c| c.tip.clone()));
    let ready = computed(move || card.with(|c| c.ready));
    let chip = computed(move || card.with(|c| c.chip.clone()));
    let note = computed(move || card.with(|c| c.note.clone()));
    widgets::list_shell()
        .child(widgets::list_header(
            lists::count_title(
                || i18n::tr("listen.title"),
                widgets::live_tooltip(move || tip.get()),
            ),
            Some(
                dynamic(move || {
                    if !ready.get() {
                        return view().into_element();
                    }
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(5.))
                        .child(lists::filter_chip(live, LISTEN_FILTER))
                        // `when`, not an empty view: the row's gap would
                        // stand after the magnifier for nothing.
                        .child(when(
                            move || chip.with(Option::is_some),
                            move || {
                                dynamic(move || {
                                    let (label, showing) = chip.get().unwrap_or_default();
                                    widgets::more_chip(label, showing, || {
                                        store::update(|state, fx| state.toggle_all_listeners(fx))
                                    })
                                })
                            },
                        ))
                        .into_element()
                })
                .into_element(),
            ),
        ))
        .child(when(
            move || ready.get(),
            move || lists::filter_row(live, LISTEN_FILTER),
        ))
        .child(dynamic(move || match note.get() {
            Some(words) => note_line(words),
            None => each(
                move || card.with(|c| c.rows.clone()),
                |l: &Listener| l.key.clone(),
                |l, _| listener_row(l),
            )
            .into_element(),
        }))
        .into_element()
}

/// Who (name, pid or `×N`, how long it has been up), then where (the
/// endpoints, with Quit at the end where it can act). The row is the
/// hover group its Quit turns red in.
fn listener_row(l: ReadSignal<Listener>) -> AnyElement {
    let pid = computed(move || l.with(|l| l.pid.clone()));
    let age = computed(move || l.with(|l| l.age.clone()));
    let quit = computed(move || l.with(|l| l.quit.clone()));
    view()
        .group()
        .px(px(CARD_PAD))
        .py(px(7.))
        .border_b(move || px(if l.with(|l| l.last) { 0. } else { 1. }))
        .border_color(theme::color(palette::border_subtle))
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    view()
                        .flex_row()
                        .items_baseline()
                        .gap(px(6.))
                        .flex_1()
                        .min_w(px(0.))
                        .child(widgets::name(
                            move || l.with(|l| l.owner.clone()),
                            11.,
                            FontWeight::NORMAL,
                        ))
                        .child(when(
                            move || pid.with(Option::is_some),
                            move || {
                                text(move || pid.get().unwrap_or_default())
                                    .flex_none()
                                    .text_size(px(10.))
                                    .line_height(widgets::line(10.))
                                    .text_color(theme::color(palette::text_dim))
                            },
                        )),
                )
                .child(when(
                    move || age.with(Option::is_some),
                    move || {
                        let part = move |second: bool| {
                            move || {
                                age.with(|age| {
                                    age.as_ref()
                                        .map(|(label, tip)| if second { tip } else { label })
                                        .cloned()
                                        .unwrap_or_default()
                                })
                            }
                        };
                        view()
                            .flex_none()
                            .tooltip(widgets::live_tooltip(part(true)))
                            .child(
                                text(part(false))
                                    .text_size(px(10.))
                                    .line_height(widgets::line(10.))
                                    .text_color(theme::color(palette::text_dim)),
                            )
                    },
                )),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .mt(px(3.))
                .min_h(px(QUIT_HEIGHT))
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.))
                        .tooltip(widgets::tooltip_lines(move || l.with(|l| l.lines.clone())))
                        .child(
                            widgets::mono_unless_cjk(text(move || l.with(|l| l.endpoints.clone())))
                                .number_of_lines(1u32)
                                .text_size(px(10.))
                                .line_height(widgets::line(10.))
                                .text_color(move || {
                                    theme::color(move || {
                                        if l.with(|l| l.exposed) {
                                            palette::text()
                                        } else {
                                            palette::text_dim()
                                        }
                                    })
                                }),
                        ),
                )
                .child(when(
                    move || quit.with(Option::is_some),
                    move || {
                        dynamic(move || match quit.get() {
                            Some((pid, name, body)) => quit_button(pid, name, body),
                            None => view().into_element(),
                        })
                    },
                )),
        )
        .into_element()
}

/// The process page's Quit, sized for a row's line: in the second ink at
/// rest, red while its row is hovered; SIGTERM behind the confirm.
fn quit_button(pid: u32, name: String, body: String) -> AnyElement {
    nsh_base::button()
        .flex_none()
        .h(px(QUIT_HEIGHT))
        .px(px(7.))
        .rounded(px(5.))
        .border(px(1.))
        .border_color(theme::color(palette::border))
        .group_hover(|s| s.border_color(theme::color(|| palette::accent_wash(45))))
        .hover(|s| s.bg(theme::color(|| palette::accent_wash(10))))
        .items_center()
        .justify_center()
        .on_press(move || {
            let name = name.clone();
            widgets::confirm(
                i18n::tr("processes.kill_title"),
                body.clone(),
                i18n::tr("processes.kill_ok"),
                move || {
                    // Checked against the name it was shown under: the
                    // list can be 15s old. Re-read on the next tick, so
                    // the row goes once the process has.
                    if terminate::request_term_named(pid, &name) {
                        store::update(|state, _| state.expire_listeners());
                    }
                },
            );
        })
        .child(
            text(i18n::tr("processes.kill_ok"))
                .text_size(px(10.))
                .line_height(widgets::line(10.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(palette::text_muted))
                .group_hover(|s| s.text_color(theme::color(palette::accent_light))),
        )
        .into_element()
}

/// What the interface card shows.
#[derive(Clone, PartialEq)]
enum IfaceCard {
    /// No sample yet, or the channel is off: a card that says which.
    Empty { title: String, body: String },
    List {
        /// The chip's words and whether it is on; `None` when nothing is
        /// hidden, where the card says so instead
        chip: Option<(String, bool)>,
        /// Nothing recent: the header, its chip and one line
        quiet: bool,
        rows: Vec<Iface>,
        idle: Option<String>,
    },
}

/// One interface's row.
#[derive(Clone, PartialEq)]
struct Iface {
    name: String,
    loopback: bool,
    active: bool,
    down: String,
    up: String,
    down_fraction: f32,
    up_fraction: f32,
    errors: Option<String>,
    packets: Option<String>,
    rule: bool,
}

fn ifaces_of(state: &ZStatsAppState) -> IfaceCard {
    let empty = |title: &str, body: &str| IfaceCard::Empty {
        title: i18n::tr(title),
        body: i18n::tr(body),
    };
    let Some(tick) = state.latest() else {
        return empty("net.title", "common.waiting_sample");
    };
    let Some(nets) = tick.snapshot.networks.as_deref() else {
        return empty("net.off", "net.off_body");
    };
    let Ifaces {
        full,
        compact,
        hideable,
        scale,
    } = net::ifaces(state, nets);
    let showing = state.show_unused_nets();
    let last = full.len().saturating_sub(1);
    let has_compact = !compact.is_empty();
    IfaceCard::List {
        chip: net::more_label(hideable, showing).map(|label| (label, showing)),
        quiet: full.is_empty() && compact.is_empty(),
        rows: full
            .iter()
            .enumerate()
            .map(|(i, n)| Iface {
                name: n.interface.clone(),
                loopback: n.is_loopback,
                active: net::active(n),
                down: format!("↓ {}", format::rate(Some(n.received_bytes_per_sec))),
                up: format!("↑ {}", format::rate(Some(n.transmitted_bytes_per_sec))),
                down_fraction: net::fraction(n.received_bytes_per_sec, scale),
                up_fraction: net::fraction(n.transmitted_bytes_per_sec, scale),
                errors: net::errors_label(n),
                packets: net::packets_tip(n),
                rule: i != last || has_compact,
            })
            .collect(),
        idle: net::idle_line(&compact),
    }
}

fn interfaces_card(live: Live) -> AnyElement {
    let card = live.memo(ifaces_of);
    let empty = computed(move || {
        card.with(|c| match c {
            IfaceCard::Empty { title, body } => Some((title.clone(), body.clone())),
            IfaceCard::List { .. } => None,
        })
    });
    let chip = computed(move || {
        card.with(|c| match c {
            IfaceCard::List { chip, .. } => chip.clone(),
            IfaceCard::Empty { .. } => None,
        })
    });
    let quiet = computed(move || card.with(|c| matches!(c, IfaceCard::List { quiet: true, .. })));
    let idle = computed(move || {
        card.with(|c| match c {
            IfaceCard::List { idle, .. } => idle.clone(),
            IfaceCard::Empty { .. } => None,
        })
    });
    dynamic(move || match empty.get() {
        Some((title, body)) => widgets::empty_card(title, body),
        None => widgets::list_shell()
            .child(widgets::list_header(
                widgets::list_title(i18n::tr("net.title")).into_element(),
                Some(
                    dynamic(move || match chip.get() {
                        Some((label, showing)) => widgets::more_chip(label, showing, || {
                            store::update(|state, fx| state.toggle_unused_nets(fx))
                        }),
                        None => widgets::note(i18n::tr("net.all_shown")).into_element(),
                    })
                    .into_element(),
                ),
            ))
            .child(dynamic(move || {
                if quiet.get() {
                    return note_line(i18n::tr("net.idle_body"));
                }
                view()
                    .child(each(
                        move || {
                            card.with(|c| match c {
                                IfaceCard::List { rows, .. } => rows.clone(),
                                IfaceCard::Empty { .. } => Vec::new(),
                            })
                        },
                        |n: &Iface| n.name.clone(),
                        |n, _| iface_row(n),
                    ))
                    .child(when(
                        move || idle.with(Option::is_some),
                        move || {
                            view()
                                .px(px(CARD_PAD))
                                .py(px(8.))
                                .child(widgets::note(move || idle.get().unwrap_or_default()))
                        },
                    ))
                    .into_element()
            }))
            .into_element(),
    })
    .into_element()
}

/// The name (and "local" under loopback's), then the rates over their
/// two bars, and the errors when there are any.
fn iface_row(n: ReadSignal<Iface>) -> AnyElement {
    let loopback = move || n.with(|n| n.loopback);
    let fg = move || {
        theme::color(move || {
            n.with(|n| {
                if !n.active {
                    palette::text_faint()
                } else if n.loopback {
                    palette::text_muted()
                } else {
                    palette::text()
                }
            })
        })
    };
    let errors = computed(move || n.with(|n| n.errors.clone()));
    let packets = computed(move || n.with(|n| n.packets.clone()));
    view()
        .flex_row()
        .items_center()
        .gap(px(10.))
        .px(px(CARD_PAD))
        .py(px(10.))
        .border_b(move || px(if n.with(|n| n.rule) { 1. } else { 0. }))
        .border_color(theme::color(palette::border_subtle))
        .child(
            view()
                .w(px(64.))
                .flex_none()
                .tooltip(
                    widgets::wrap_tooltip(i18n::tr("net.local_tip")).disabled(move || !loopback()),
                )
                .child(
                    text(move || n.with(|n| n.name.clone()))
                        .number_of_lines(1u32)
                        .text_size(px(11.))
                        .line_height(widgets::line(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(fg),
                )
                .child(when(loopback, || {
                    text(i18n::tr("net.local"))
                        .text_size(px(9.))
                        .line_height(widgets::line(9.))
                        .text_color(theme::color(palette::text_dim))
                })),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(0.))
                .tooltip(
                    widgets::live_tooltip(move || packets.get().unwrap_or_default())
                        .disabled(move || packets.with(Option::is_none)),
                )
                .child(
                    view()
                        .flex_row()
                        .justify_between()
                        .child(rate(move || n.with(|n| n.down.clone())).text_color(fg))
                        .child(rate(move || n.with(|n| n.up.clone())).text_color(fg)),
                )
                .child(
                    view()
                        .flex_row()
                        .gap(px(3.))
                        .mt(px(5.))
                        .child(bar(
                            move || n.with(|n| n.down_fraction),
                            move || {
                                if loopback() {
                                    palette::border()
                                } else {
                                    palette::ink()
                                }
                            },
                        ))
                        .child(bar(
                            move || n.with(|n| n.up_fraction),
                            move || {
                                if loopback() {
                                    palette::border()
                                } else {
                                    palette::text_dim()
                                }
                            },
                        )),
                )
                .child(when(
                    move || errors.with(Option::is_some),
                    move || {
                        text(move || errors.get().unwrap_or_default())
                            .mt(px(4.))
                            .font_family(widgets::MONO)
                            .text_size(px(9.))
                            .line_height(widgets::line(9.))
                            .text_color(theme::color(palette::accent_light))
                    },
                )),
        )
        .into_element()
}

/// One direction's bar, on the recessed track (`views/net.rs`'s `bar`).
fn bar(fraction: impl Fn() -> f32 + 'static, fill: impl Fn() -> Rgba + Copy + 'static) -> View {
    view()
        .flex_1()
        .h(px(4.))
        .rounded_full()
        .bg(theme::color(palette::inset))
        .overflow_hidden()
        .child(
            view()
                .h_full()
                .w(move || relative(fraction()))
                .rounded_full()
                .bg(move || theme::color(fill)),
        )
}
