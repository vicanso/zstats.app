//! History: a port of the gpui front end's `views/history.rs`, whose comments
//! say why every row, figure and control is the way it is — this file
//! carries the same choices and does not repeat their reasons. The page's
//! rules are shared (`historylist.rs`), the reading is `history.rs`'s.
//!
//! One card: the lens as its title and the menu that switches it, the
//! range and a refresh beside it, Today's clock under it, then the rows in
//! a region that ends on a whole row at the gap above the footer — kept
//! by their `(pid, name)` (a row's identity in the records), so a refresh
//! or another lens changes figures, not elements.

use crate::assets::CustomIconName;
use crate::color::Rgba;
use crate::historylist;
use crate::nsh::lists;
use crate::nsh::root::{CARD_PAD, Live};
use crate::nsh::widgets::{self, CLEAR, Choice};
use crate::nsh::{store, theme};
use crate::rows;
use crate::state::{HistoryRange, HistorySort, ZStatsAppState};
use crate::{i18n, palette};
use nsh_core::ui::prelude::*;
use rust_i18n::t;

/// Today's clock row, reserved out of the rows' budget
/// (`history::AXIS_ROW`).
const AXIS_ROW: f32 = 16.;

/// What stands above and below a row's lines, before the fit gives the
/// row its share of the leftover (`history::ROW_PAD`).
const ROW_PAD: f32 = 9.;

/// A row with Today's band, hairline included: the padding twice, the
/// 12px name's 19, 2, the 10px caption's 16, 6, the 8-tall band and the
/// hairline (`history::BAND_ROW`, whose lines are φ's fractions where
/// NSH's are whole points).
const BAND_ROW: f32 = ROW_PAD + ROW_PAD + 19. + 2. + 16. + 6. + 8. + 1.;

/// The same row under the wider ranges, a 4-tall share meter in the
/// band's place.
const METER_ROW: f32 = BAND_ROW - 8. + 4.;

/// Never shorter than this (`history::rows_height`).
const ROWS_MIN: f32 = 160.;

/// The History page; `body` is the height it has on screen.
pub fn page(live: Live, body: ReadSignal<f32>) -> AnyElement {
    let page = live.watch(move || {
        let body = body.get();
        store::read(|state| page_of(state, body))
    });
    let kind = computed(move || page.with(Page::kind));
    dynamic(move || match kind.get() {
        Kind::Loading => {
            let title = page.with_untracked(|p| p.title.clone());
            widgets::empty_card(title, i18n::tr("history.loading"))
        }
        Kind::Empty => widgets::list_shell()
            .child(widgets::list_header(
                widgets::list_title(i18n::tr("history.empty_title")).into_element(),
                Some(controls(live)),
            ))
            .child(
                view()
                    .px(px(CARD_PAD))
                    .pb(px(11.))
                    .child(widgets::note(i18n::tr("history.empty_body"))),
            )
            .into_element(),
        Kind::List => list(live, page),
    })
    .into_element()
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// The read has not landed (or a switched range is re-reading)
    Loading,
    /// Nothing recorded in the range: the range chips stay reachable
    Empty,
    List,
}

/// What the page shows.
#[derive(Clone, PartialEq)]
struct Page {
    loaded: bool,
    /// The lens's name, the card's title
    title: String,
    /// Today: the rows draw bands on a clock, and the clock is drawn
    today: bool,
    /// Today's six-hour marks, each at its fraction of the axis
    marks: Vec<(String, f32)>,
    rows: Vec<Row>,
    viewport: f32,
    /// The chip after the rows, while there are more than the list names
    more: Option<String>,
}

impl Page {
    fn kind(&self) -> Kind {
        if !self.loaded {
            Kind::Loading
        } else if self.rows.is_empty() {
            Kind::Empty
        } else {
            Kind::List
        }
    }
}

/// One spender's row.
#[derive(Clone, PartialEq)]
struct Row {
    key: (u32, String),
    name: String,
    /// The pid, where another row carries the same name
    pid: Option<String>,
    headline: String,
    tag: String,
    shape: Option<(String, String)>,
    caption: String,
    peak: String,
    /// Today's cells, `None` for the wider ranges' meter
    cells: Option<Vec<Option<f32>>>,
    band_tip: Vec<String>,
    meter: f32,
    first: bool,
    pad: f32,
}

fn page_of(state: &ZStatsAppState, body: f32) -> Page {
    let sort = state.history_sort();
    let title = i18n::tr(sort.title_key());
    let Some(rows) = state.history() else {
        return Page {
            loaded: false,
            title,
            today: false,
            marks: Vec::new(),
            rows: Vec::new(),
            viewport: 0.,
            more: None,
        };
    };
    let show_all = state.show_all_history();
    let ranked = historylist::ranked(rows, sort, show_all);
    let today = rows.iter().any(|s| s.band.is_some());
    let now = jiff::Zoned::now();
    let lived = historylist::lived(&now);
    let now_clock = historylist::now_clock(&now);
    let budget =
        (lists::rows_budget(state, body) - if today { AXIS_ROW } else { 0. }).max(ROWS_MIN);
    let fit = rows::fit_rows(budget, if today { BAND_ROW } else { METER_ROW }, ROW_PAD);
    let shown: Vec<Row> = ranked
        .shown
        .iter()
        .enumerate()
        .map(|(i, s)| Row {
            key: (s.pid, s.name.clone()),
            name: historylist::shown_name(s).to_owned(),
            pid: ranked
                .says_pid(s)
                .then(|| t!("processes.pid_only", pid = s.pid).to_string()),
            headline: historylist::headline(s, sort),
            tag: historylist::headline_tag(sort),
            shape: historylist::shape(s),
            caption: historylist::caption(s, sort),
            peak: historylist::peak_line(s),
            cells: s
                .band
                .as_ref()
                .map(|band| historylist::band_cells(band, lived)),
            band_tip: s
                .band
                .as_ref()
                .map(|band| historylist::band_tip(band, lived, &now_clock))
                .unwrap_or_default(),
            meter: historylist::meter_fraction(s, sort, ranked.top),
            first: i == 0,
            pad: fit.pad(i),
        })
        .collect();
    Page {
        loaded: true,
        title,
        today,
        marks: if today {
            historylist::axis_marks(lived)
        } else {
            Vec::new()
        },
        rows: shown,
        viewport: fit.viewport,
        more: (ranked.hidden > 0).then(|| {
            if show_all {
                i18n::tr("history.less")
            } else {
                t!("history.more", count = ranked.hidden).to_string()
            }
        }),
    }
}

fn list(live: Live, page: Memo<Page>) -> AnyElement {
    let title = computed(move || page.with(|p| p.title.clone()));
    let today = computed(move || page.with(|p| p.today));
    let more = computed(move || page.with(|p| p.more.clone()));
    widgets::list_shell()
        .child(widgets::list_header(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .min_w(px(0.))
                .child(lens_title(live, title))
                .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
                    "history.about_body",
                ))))
                .into_element(),
            Some(controls(live)),
        ))
        .child(when(
            move || today.get(),
            move || axis(computed(move || page.with(|p| p.marks.clone()))),
        ))
        .child(
            scroll()
                .max_h(move || px(page.with(|p| p.viewport)))
                .child(each(
                    move || page.with(|p| p.rows.clone()),
                    |row: &Row| row.key.clone(),
                    |row, _| history_row(computed(move || row.get())),
                ))
                .child(when(
                    move || more.with(Option::is_some),
                    move || {
                        view()
                            .flex_row()
                            .px(px(CARD_PAD))
                            .py(px(8.))
                            .child(more_chip(more))
                    },
                )),
        )
        .into_element()
}

/// The lens as the card's title and the menu that switches it.
fn lens_title(live: Live, title: Memo<String>) -> AnyElement {
    let sort = live.memo(ZStatsAppState::history_sort);
    let choices = HistorySort::ALL
        .into_iter()
        .map(|lens| Choice {
            label: i18n::tr(lens.title_key()),
            on: Box::new(move || sort.get() == lens),
            choose: Box::new(move || store::update(|state, fx| state.set_history_sort(lens, fx))),
        })
        .collect();
    widgets::title_menu(
        move || title.get(),
        move || i18n::tr(sort.get().tip_key()),
        choices,
    )
}

/// Range and refresh, side by side in the header.
fn controls(live: Live) -> AnyElement {
    let range = live.memo(ZStatsAppState::history_range);
    view()
        .flex_row()
        .items_center()
        .gap(px(2.))
        .children(
            HistoryRange::ALL
                .into_iter()
                .map(|choice| {
                    let on = move || range.get() == choice;
                    nsh_base::button()
                        .flex_none()
                        .rounded(px(4.))
                        .px(px(5.))
                        .py(px(1.))
                        .bg(move || {
                            theme::color(move || if on() { palette::chip() } else { CLEAR })
                        })
                        .hover(move |s| {
                            if on() {
                                s
                            } else {
                                s.bg(theme::color(palette::surface_raised))
                            }
                        })
                        .on_press(move || {
                            store::update(|state, fx| state.set_history_range(choice, fx))
                        })
                        .child(
                            text(i18n::tr(choice.label_key()))
                                .text_size(px(9.))
                                .line_height(widgets::line(9.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(move || {
                                    theme::color(move || {
                                        if on() {
                                            palette::text()
                                        } else {
                                            palette::text_muted()
                                        }
                                    })
                                }),
                        )
                        .into_element()
                })
                .collect::<Vec<_>>(),
        )
        .child(
            view()
                .flex_none()
                .tooltip(widgets::wrap_tooltip(i18n::tr("history.refresh")))
                .child(
                    nsh_base::button()
                        .rounded(px(4.))
                        .px(px(4.))
                        .py(px(1.))
                        .hover(|s| s.bg(theme::color(palette::surface_raised)))
                        .on_press(|| store::update(|state, fx| state.load_history(fx)))
                        .child(widgets::icon(
                            CustomIconName::RefreshCw,
                            10.,
                            palette::text_dim,
                        )),
                ),
        )
        .into_element()
}

/// The clock under Today's bands: 0:00 at the left, now at the right, the
/// six-hour marks the lived day has reached between them.
fn axis(marks: Memo<Vec<(String, f32)>>) -> AnyElement {
    let label = |words: String| {
        text(words)
            .tabular_nums()
            .text_size(px(9.))
            .line_height(widgets::line(9.))
            .text_color(theme::color(palette::text_dim))
            .absolute()
            .top(px(0.))
    };
    view()
        .mx(px(13.))
        .h(px(AXIS_ROW))
        .relative()
        .child(label(i18n::tr("history.axis_start")).left(px(0.)))
        .child(dynamic(move || {
            marks
                .get()
                .into_iter()
                .map(|(mark, at)| label(mark).left(relative(at)).into_element())
                .collect::<Vec<_>>()
        }))
        .child(label(i18n::tr("history.axis_now")).right(px(0.)))
        .into_element()
}

/// The chip after the rows: show the rest, or fewer again.
fn more_chip(label: Memo<Option<String>>) -> AnyElement {
    nsh_base::button()
        .flex_none()
        .rounded_full()
        .border(px(1.))
        .border_color(theme::color(palette::border_subtle))
        .hover(|s| {
            s.bg(theme::color(palette::surface_raised))
                .border_color(theme::color(palette::border))
        })
        .px(px(7.))
        .py(px(1.))
        .on_press(|| store::update(|state, fx| state.toggle_all_history(fx)))
        .child(
            text(move || label.get().unwrap_or_default())
                .text_size(px(9.))
                .line_height(widgets::line(9.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(palette::text_dim)),
        )
        .into_element()
}

/// One spender: the app's mark beside its name and figure, its shape and
/// the other figures, then Today's band or the range's share meter. A
/// click opens the program on Processes or Apps
/// (`state::reveal_history_subject`).
fn history_row(row: Memo<Row>) -> AnyElement {
    let key = computed(move || row.with(|r| r.key.clone()));
    let name = computed(move || row.with(|r| r.name.clone()));
    let pid = computed(move || row.with(|r| r.pid.clone()));
    let shape = computed(move || row.with(|r| r.shape.clone()));
    let cells = computed(move || row.with(|r| r.cells.clone()));
    let pad = move || px(row.with(|r| r.pad));
    view()
        .child(view().h(px(1.)).bg(move || {
            theme::color(move || {
                if row.with(|r| r.first) {
                    CLEAR
                } else {
                    palette::border_subtle()
                }
            })
        }))
        .child(
            nsh_base::button()
                .px(px(CARD_PAD))
                .pt(pad)
                .pb(pad)
                .hover(|s| s.bg(theme::color(palette::surface_raised)))
                .on_press(move || {
                    let (pid, name) = key.get_untracked();
                    store::update(|state, fx| state.reveal_history_subject(pid, &name, fx));
                })
                .child(
                    view()
                        .flex_row()
                        .items_start()
                        .gap(px(10.))
                        .child(dynamic(move || {
                            let (pid, recorded) = key.get();
                            widgets::record_mark(pid, &recorded, &name.get())
                        }))
                        .child(
                            view()
                                .flex_1()
                                .min_w(px(0.))
                                .child(first_line(row, name, pid))
                                .child(second_line(row, shape)),
                        ),
                )
                // Under the mark and the lines, the row's whole width.
                .child(view().mt(px(6.)).child(dynamic(move || {
                    match cells.get() {
                        Some(cells) => band(cells, row),
                        None => widgets::meter(move || row.with(|r| r.meter), palette::ink, 4.)
                            .into_element(),
                    }
                }))),
        )
        .into_element()
}

/// Who, the pid where the name repeats, and the lens's figure.
fn first_line(row: Memo<Row>, name: Memo<String>, pid: Memo<Option<String>>) -> View {
    view()
        .flex_row()
        .items_baseline()
        .justify_between()
        .gap(px(8.))
        .child(
            view()
                .flex_row()
                .flex_1()
                .min_w(px(0.))
                .items_baseline()
                .gap(px(5.))
                .child(widgets::name(move || name.get(), 12., FontWeight::MEDIUM))
                .child(when(
                    move || pid.with(Option::is_some),
                    move || {
                        text(move || pid.get().unwrap_or_default())
                            .flex_none()
                            .text_size(px(9.5))
                            .line_height(widgets::line(9.5))
                            .text_color(theme::color(palette::text_faint))
                    },
                )),
        )
        .child(
            view()
                .flex_row()
                .flex_none()
                .items_baseline()
                .gap(px(3.))
                .child(
                    text(move || row.with(|r| r.headline.clone()))
                        .tabular_nums()
                        .text_size(px(12.))
                        .line_height(widgets::line(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::color(palette::text)),
                )
                .child(
                    text(move || row.with(|r| r.tag.clone()))
                        .text_size(px(8.5))
                        .line_height(widgets::line(8.5))
                        .text_color(theme::color(|| palette::tiny_label(palette::text_dim()))),
                ),
        )
}

/// The shape where it is not steady, the other figure, and the peak.
fn second_line(row: Memo<Row>, shape: Memo<Option<(String, String)>>) -> View {
    let small = |words: Box<dyn Fn() -> String>| {
        text(words)
            .text_size(px(10.))
            .line_height(widgets::line(10.))
            .text_color(theme::color(palette::text_dim))
    };
    view()
        .flex_row()
        .justify_between()
        .gap(px(8.))
        .mt(px(2.))
        .child(
            view()
                .flex_row()
                .flex_1()
                .min_w(px(0.))
                .items_center()
                .gap(px(6.))
                // `when`, not an empty view: the row's gap would indent
                // the figure after it.
                .child(when(
                    move || shape.with(Option::is_some),
                    move || {
                        dynamic(move || {
                            let (label, tip) = shape.get().unwrap_or_default();
                            view()
                                .flex_none()
                                .tooltip(widgets::wrap_tooltip(tip))
                                .child(
                                    text(label)
                                        .rounded(px(3.))
                                        .px(px(4.))
                                        .bg(theme::color(palette::inset))
                                        .text_size(px(9.))
                                        .line_height(widgets::line(9.))
                                        .text_color(theme::color(|| {
                                            palette::tiny_label(palette::text_muted())
                                        })),
                                )
                                .into_element()
                        })
                    },
                ))
                .child(
                    small(Box::new(move || row.with(|r| r.caption.clone())))
                        .min_w(px(0.))
                        .number_of_lines(1u32),
                ),
        )
        .child(
            small(Box::new(move || row.with(|r| r.peak.clone())))
                .tabular_nums()
                .flex_none()
                .number_of_lines(1u32),
        )
}

/// One day of the program's recorded half-hours on the lived day's axis:
/// each cell the ink at its step's opacity, the trough where no line was
/// recorded; the stretches in clock time on hover.
fn band(cells: Vec<Option<f32>>, row: Memo<Row>) -> AnyElement {
    let lit = cells.iter().any(Option::is_some);
    let mut strip = view()
        .flex_row()
        .h(px(8.))
        .rounded(px(2.))
        .overflow_hidden()
        .bg(theme::color(palette::trough))
        .children(
            cells
                .into_iter()
                .map(|alpha| {
                    let cell = view().flex_1().h_full();
                    match alpha {
                        Some(alpha) => cell.bg(theme::color(move || {
                            let ink = palette::ink();
                            Rgba {
                                a: ink.a * alpha,
                                ..ink
                            }
                        })),
                        None => cell,
                    }
                    .into_element()
                })
                .collect::<Vec<_>>(),
        );
    if lit {
        strip = strip.tooltip(widgets::tooltip_lines(move || {
            row.with(|r| r.band_tip.clone())
        }));
    }
    strip.into_element()
}
