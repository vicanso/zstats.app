//! History: what actually burned the CPU today.
//!
//! The only view here ranked by an *amount* rather than a rate. Everything
//! else answers "how busy is it right now", which structurally cannot see the
//! process that never looks busy and yet costs the most over a day — see
//! [`crate::history`].

use super::processes;
use super::widgets;
use crate::assets::CustomIconName;
use crate::font;
use crate::gpui_ui;
use crate::history;
use crate::historylist;
use crate::i18n;
use crate::rows;
use crate::state::{HistoryRange, HistorySort, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, AnyElement, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, px, relative,
};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let range = state.history_range();
    let sort = state.history_sort();
    let title = i18n::tr(sort.title_key());
    let Some(rows) = state.history() else {
        // The read is on the background executor; this is the frame or two
        // before it lands (or a freshly switched range re-reading).
        return vec![widgets::empty_card(title, i18n::tr("history.loading"))];
    };
    if rows.is_empty() {
        // Keep the range chips reachable: an empty *day* must not strand
        // the user away from the week that does have data.
        return vec![
            widgets::list_shell()
                .child(widgets::list_header(
                    i18n::tr("history.empty_title"),
                    Some(header_controls(range)),
                ))
                .child(
                    div()
                        .px(px(widgets::CARD_PAD))
                        .pb(px(11.))
                        .child(widgets::note(i18n::tr("history.empty_body"))),
                )
                .into_any_element(),
        ];
    }

    let show_all = state.show_all_history();
    let ranked = historylist::ranked(rows, sort, show_all);
    let (hidden, top) = (ranked.hidden, ranked.top);
    // Today draws each row's band on a shared 00:00 → now axis; the
    // other ranges draw a share meter and have no clock to label.
    let today = rows.iter().any(|s| s.band.is_some());
    let fit = rows::fit_rows(
        rows_height(state, today),
        if today { BAND_ROW } else { METER_ROW },
        ROW_PAD,
    );

    let list = widgets::list_shell()
        .child(widgets::list_header(
            // The note this carries used to be a card of its own under
            // the list. It is a read-once fact about how the ranking
            // works, which is what an ⓘ beside the title is for (the
            // Processes header does the same with its CPU basis) — and
            // moving it out is what lets the list card be exactly one
            // screen tall, so the header can stay put for real.
            h_flex()
                .items_center()
                .gap(px(4.))
                .min_w_0()
                .child(lens_title(sort, title, state.panel_visit()))
                .child(widgets::info_icon(
                    "history-basis",
                    i18n::tr("history.about_body"),
                )),
            Some(header_controls(range)),
        ))
        .when(today, |list| list.child(band_axis()))
        .child(
            // The rows scroll under the header rather than taking the
            // whole card past the top of the panel — the same pinned
            // model as the Processes and Apps lists, and the reason is
            // the same: the range and sort controls live in that header,
            // and a control you have to scroll back up to reach is a
            // control you stop using.
            v_flex()
                .id("history-rows")
                .track_scroll(&gpui_ui::history_rows_scroll())
                .overflow_y_scroll()
                .max_h(px(fit.viewport))
                .children(ranked.shown.iter().copied().enumerate().map(|(i, s)| {
                    let pid = s.pid;
                    let name = s.name.clone();
                    v_flex()
                        .id(row_id(pid, &name))
                        .px(px(widgets::CARD_PAD))
                        // Its share of the region, and the hairline over
                        // the row: `processes::process_row` says why.
                        .py(px(fit.pad(i)))
                        .flex_none()
                        .h(px(fit.height(i)))
                        .when(i != 0, |d| {
                            d.border_t(px(1.)).border_color(theme::border_subtle())
                        })
                        // The row answers "who burned it" — Processes or
                        // Apps answers the next question. Hover fill is
                        // the affordance, same as Overview's top trees.
                        .hover(|d| d.bg(theme::surface_raised()))
                        .on_click(move |_, _window, cx| {
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| {
                                    state.reveal_history_subject(pid, &name, cx)
                                });
                        })
                        // The app's icon while the recorded pid still runs
                        // that program (`widgets::record_mark`), its letter
                        // otherwise, beside the two lines it names — three
                        // Chrome helpers in a day's list read as one app's.
                        .child(
                            h_flex()
                                .items_start()
                                .gap(px(10.))
                                .child(widgets::record_mark(
                                    s.pid,
                                    &s.name,
                                    historylist::shown_name(s),
                                ))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            h_flex()
                                                .items_baseline()
                                                .justify_between()
                                                .gap(px(8.))
                                                .child(
                                                    h_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .items_baseline()
                                                        .gap(px(5.))
                                                        .child(widgets::truncating_name(
                                                            ("hist-name", s.pid as usize),
                                                            historylist::shown_name(s).to_string(),
                                                            12.,
                                                            gpui::FontWeight::MEDIUM,
                                                            Hsla::from(theme::text()),
                                                        ))
                                                        // The pid beside the name, only
                                                        // where the name repeats (above).
                                                        // At the title, not in the caption,
                                                        // so the caption stays pure figures;
                                                        // the name truncates and the pid
                                                        // does not — a cut pid is a
                                                        // different number.
                                                        .when(ranked.says_pid(s), |title| {
                                                            title.child(
                                                                div()
                                                                    .flex_none()
                                                                    .text_size(px(9.5))
                                                                    .text_color(theme::text_faint())
                                                                    .child(
                                                                        t!(
                                                                            "processes.pid_only",
                                                                            pid = s.pid
                                                                        )
                                                                        .to_string(),
                                                                    ),
                                                            )
                                                        }),
                                                )
                                                // The headline figure is core-time, which shares a
                                                // unit *shape* with the wall-clock minutes below —
                                                // the CPU tag is what keeps "30m" from reading as a
                                                // duration of day.
                                                .child(
                                                    h_flex()
                                                        .flex_none()
                                                        .items_baseline()
                                                        .gap(px(3.))
                                                        .child(
                                                            div()
                                                                .font_features(font::tnum())
                                                                .text_size(px(12.))
                                                                .font_weight(
                                                                    gpui::FontWeight::SEMIBOLD,
                                                                )
                                                                .text_color(theme::text())
                                                                .child(historylist::headline(
                                                                    s, sort,
                                                                )),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(px(8.5))
                                                                .text_color(theme::tiny_label(
                                                                    theme::text_dim(),
                                                                ))
                                                                .child(historylist::headline_tag(
                                                                    sort,
                                                                )),
                                                        ),
                                                ),
                                        )
                                        .child(
                                            h_flex()
                                                .justify_between()
                                                .gap(px(8.))
                                                .mt(px(2.))
                                                .text_size(px(10.))
                                                .text_color(theme::text_dim())
                                                .child(
                                                    h_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .items_center()
                                                        .gap(px(6.))
                                                        // Steady is most rows on most days,
                                                        // and a word every row wears says
                                                        // nothing; only the exceptions do.
                                                        .children(historylist::shape(s).map(
                                                            |(label, tip)| {
                                                                shape_pill(
                                                                    ("hist-shape", s.pid as usize),
                                                                    label,
                                                                    tip,
                                                                )
                                                            },
                                                        ))
                                                        .child(
                                                            // Pure figure: the pid lives
                                                            // beside the name now, so the
                                                            // caption carries only the
                                                            // metric the sort is not
                                                            // already headlining.
                                                            div().min_w_0().truncate().child(
                                                                historylist::caption(s, sort),
                                                            ),
                                                        ),
                                                )
                                                // Peak beside total on purpose: a small peak next to a
                                                // large total is exactly the process this view exists
                                                // to find, and the pairing is what makes that legible.
                                                .child(
                                                    // A sentence with two figures in
                                                    // it, neither of which moves while
                                                    // it is read: the page's face.
                                                    font::tabular(div())
                                                        .flex_none()
                                                        .min_w_0()
                                                        .truncate()
                                                        .child(historylist::peak_line(s)),
                                                ),
                                        ),
                                ),
                        )
                        .child(div().mt(px(6.)).child(match &s.band {
                            // Today: WHEN it burned, on a shared
                            // 00:00 → now axis — the share meter it
                            // replaces repeated what the rank order
                            // and the headline number already said.
                            // Deliberately under both sorts: the axis
                            // is time, not a magnitude comparison, so
                            // reordering the rows does not change what
                            // a row's band means.
                            Some(cells) => band(("hist-band", s.pid as usize), cells),
                            // Wider windows: no single day to draw, the
                            // share meter stays.
                            None => widgets::meter(
                                historylist::meter_fraction(s, sort, top),
                                Hsla::from(theme::ink()),
                                4.,
                            ),
                        }))
                        .into_any_element()
                }))
                // Inside the card, at the end of the rows — the same chip
                // as every other "show more". It was a sentence under the
                // card that only said the rows existed.
                .when(hidden > 0, |rows| {
                    rows.child(
                        h_flex()
                            .flex_none()
                            .px(px(widgets::CARD_PAD))
                            .py(px(8.))
                            .child(more_chip(hidden, show_all)),
                    )
                }),
        );

    vec![list.into_any_element()]
}

/// The title names the lens and is the control that switches it: a
/// separate "CPU" chip beside Today / 7d / 30d read as a fourth range.
/// Its chevron opens the three lenses as a menu ([`super::MenuTrigger`]).
fn lens_title(sort: HistorySort, title: String, visit: u32) -> AnyElement {
    super::menu_trigger(
        ("history-sort", visit),
        i18n::tr(sort.tip_key()),
        h_flex()
            .items_center()
            .gap(px(3.))
            .min_w_0()
            .child(div().min_w_0().truncate().child(title))
            .child(
                Icon::new(IconName::ChevronDown)
                    .with_size(Size::Size(px(10.)))
                    .text_color(Hsla::from(theme::text_dim())),
            ),
    )
    .min_w_0()
    // Padded for the hover fill, pulled back so the title still
    // starts on the card's text line.
    .px(px(4.))
    .ml(px(-4.))
    .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
        HistorySort::ALL.iter().fold(menu, |menu, &lens| {
            menu.item(
                super::menu_text(i18n::tr(lens.title_key()))
                    .checked(lens == sort)
                    .on_click(move |_, _, cx| {
                        cx.global::<ZStatsGlobalStore>()
                            .clone()
                            .update(cx, |state, cx| state.set_history_sort(lens, cx));
                    }),
            )
        })
    })
    .into_any_element()
}

fn more_chip(hidden: usize, showing: bool) -> AnyElement {
    div()
        .id("history-more")
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(theme::border_subtle())
        .hover(|d| d.bg(theme::surface_raised()).border_color(theme::border()))
        .px(px(7.))
        .py(px(1.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text_dim())
        .child(if showing {
            i18n::tr("history.less")
        } else {
            t!("history.more", count = hidden).to_string()
        })
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.toggle_all_history(cx));
        })
        .into_any_element()
}

/// Height of [`band_axis`], reserved out of the rows' budget.
const AXIS_ROW: f32 = 16.;

/// What stands above and below a row's lines, before
/// [`rows::fit_rows`] gives the row its share of the leftover.
const ROW_PAD: f32 = 9.;

/// A row with Today's band, hairline included: the padding twice, the
/// 12px name (19.5 at 2×), 2, the 10px caption (16), 6, the 8-tall band
/// and the hairline. Measured at 70.5 on screen. As with
/// [`processes::COLLAPSED_ROW`] it decides how many rows the region
/// holds and no more; this page's last row used to be cut through its
/// name by the card's bottom edge.
const BAND_ROW: f32 = ROW_PAD + ROW_PAD + 19.5 + 2. + 16. + 6. + 8. + 1.;

/// The same row under the wider ranges, where a 4-tall share meter
/// stands in the band's place.
const METER_ROW: f32 = BAND_ROW - 8. + 4.;

/// The clock under Today's bands: 0:00 at the left, now at the right,
/// and the six-hour marks the lived day has reached between them. The
/// bands had no axis at all — a lit cell could be placed only by
/// hovering it. Same width and same lived-day scale as [`band`].
fn band_axis() -> AnyElement {
    let lived = historylist::lived(&jiff::Zoned::now());
    let label = |text: String| {
        div()
            .font_features(font::tnum())
            .text_size(px(9.))
            .text_color(theme::text_dim())
            .child(text)
    };
    div()
        .mx(px(13.))
        .h(px(AXIS_ROW))
        .relative()
        .child(
            label(i18n::tr("history.axis_start"))
                .absolute()
                .left_0()
                .top_0(),
        )
        .children(
            historylist::axis_marks(lived)
                .into_iter()
                .map(|(mark, at)| label(mark).absolute().left(relative(at)).top_0()),
        )
        .child(
            label(i18n::tr("history.axis_now"))
                .absolute()
                .right_0()
                .top_0(),
        )
        .into_any_element()
}

/// One day of a process's recorded half-hours: 00:00 at the left edge,
/// now at the right. Every row shares the axis, so two rows dark in the
/// same column were burning at the same time — the cross-process
/// correlation the ranked totals could never show.
///
/// Cells are quantized ink opacity, not a red ramp: accent is reserved
/// for over-threshold, and what a cell encodes is magnitude — the same
/// job a meter's length does, done in the same ink. The track showing
/// through means "no line in the file for this half hour", which is not
/// zero (records are conditional — see `history::Band`) and therefore
/// must not be painted as a value.
fn band(id: impl Into<ElementId>, cells: &history::Band) -> AnyElement {
    let now = jiff::Zoned::now();
    let lived = historylist::lived(&now);
    let strip = h_flex()
        .h(px(8.))
        .rounded(px(2.))
        .overflow_hidden()
        // `trough`, not `inset`: in dark mode inset is a heavy black,
        // and black segments alternating with lit cells read as a
        // barcode — figure and ground become guessable, and "black =
        // strong" is a print instinct that guesses wrong. The trough is
        // a near-card lift, so the picture is unambiguous: lit cells
        // float on an almost-invisible track, nothing reads as a black
        // value. Light mode is untouched in practice (the two tokens
        // nearly coincide there), where the white ground was never
        // ambiguous to begin with.
        .bg(theme::trough())
        .children(
            historylist::band_cells(cells, lived)
                .into_iter()
                .map(|alpha| {
                    div().flex_1().h_full().when_some(alpha, |d, alpha| {
                        d.bg(Hsla::from(theme::ink()).opacity(alpha))
                    })
                }),
        );
    // The same clock `lived` was cut at, so the readout and the strip
    // agree on where "so far" ends within this one frame.
    let tip = historylist::band_tip(cells, lived, &historylist::now_clock(&now));
    if tip.is_empty() {
        return strip.into_any_element();
    }
    strip
        .id(id)
        .tooltip(widgets::wrap_tooltip_lines(
            tip.into_iter().map(SharedString::from).collect(),
        ))
        .into_any_element()
}

/// The rows region's height — the same budget the Processes list gets,
/// less Today's axis row when it is drawn. Reserving exactly that is what
/// makes the pin real: the card adds up to the body's height, the tab
/// has nothing left to scroll, and the header cannot be scrolled away.
fn rows_height(state: &ZStatsAppState, axis: bool) -> f32 {
    let axis = if axis { AXIS_ROW } else { 0. };
    (processes::rows_height(state) - axis).max(160.)
}

/// Range and refresh, side by side in the header. The lens is the title
/// ([`lens_title`]).
fn header_controls(current: HistoryRange) -> AnyElement {
    h_flex()
        .items_center()
        .gap(px(2.))
        .children(HistoryRange::ALL.into_iter().enumerate().map(|(i, range)| {
            let on = range == current;
            div()
                .id(("history-range", i))
                .flex_none()
                .rounded(px(4.))
                .px(px(5.))
                .py(px(1.))
                .text_size(px(9.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .when(on, |d| d.bg(theme::chip()).text_color(theme::text()))
                .when(!on, |d| {
                    d.text_color(theme::text_muted())
                        .hover(|d| d.bg(theme::surface_raised()).text_color(theme::text()))
                })
                .child(i18n::tr(range.label_key()))
                .on_click(move |_, _window, cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.set_history_range(range, cx));
                })
        }))
        .child(refresh_control())
        .into_any_element()
}

fn shape_pill(id: impl Into<gpui::ElementId>, label: String, tip: String) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .rounded(px(3.))
        .px(px(4.))
        .bg(theme::inset())
        .text_size(px(9.))
        .text_color(theme::tiny_label(theme::text_muted()))
        .tooltip(widgets::wrap_tooltip(tip))
        .child(label)
        .into_any_element()
}

fn refresh_control() -> AnyElement {
    // Icon only: the labelled chip was the first thing `list_header`
    // clipped at 320px ("Reloa"). The word lives on the tooltip.
    let tip = i18n::tr("history.refresh");
    div()
        .id("history-refresh")
        .flex_none()
        .rounded(px(4.))
        .px(px(4.))
        .py(px(1.))
        .hover(|d| d.bg(theme::surface_raised()))
        .tooltip(widgets::wrap_tooltip(tip))
        .child(
            Icon::from(CustomIconName::RefreshCw)
                .with_size(Size::Size(px(10.)))
                .text_color(Hsla::from(theme::text_dim())),
        )
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.load_history(cx));
        })
        .into_any_element()
}

/// A row's element id. Spenders are grouped on `(pid, name)`, so the pid
/// alone repeats: a week's range spans reboots that hand low pids back
/// out, and an `exec` renames a process under the same pid. Two siblings
/// with one id share gpui's pending-mouse-down cell, and the earlier row's
/// mouse-up listener clears it before the later row can fire its click.
fn row_id(pid: u32, name: &str) -> SharedString {
    SharedString::from(format!("hist-row-{pid}-{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wider ranges' rows are 66.5 and the short panel's region is
    /// 385.5: six of them at 64 and 65, where five would each stand
    /// eleven over its own height.
    #[test]
    fn the_short_panel_holds_six_rows_under_the_wider_ranges() {
        let fit = rows::fit_rows(385.5, METER_ROW, ROW_PAD);
        assert_eq!(fit.viewport, 385.5);
        let reach: f32 = (0..6).map(|i| fit.height(i)).sum();
        assert_eq!(reach, 386.);
        assert!(METER_ROW - fit.height(5) <= 2.5);
    }

    #[test]
    fn rows_sharing_a_pid_keep_distinct_ids() {
        assert_ne!(row_id(400, "WindowServer"), row_id(400, "mds"));
        assert_eq!(row_id(400, "mds"), row_id(400, "mds"));
    }
}
