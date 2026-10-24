//! What the Apps and Processes pages share — on the gpui front end, what
//! `views/processes.rs` exported for `views/apps.rs`: the measures of a list
//! under a pinned header, the name filter (one filter, both lists; the
//! listening card has one of the same shape), the All chip's shape, the disk
//! check's badge, and an open row's pieces — its figures, its alert bars, the
//! chips that edit them, and the outlined button that asks a program to quit.

use crate::alertbars::{self, BarDisplay, Resolved};
use crate::assets::CustomIconName;
use crate::diskwatch::Hold;
use crate::nsh::root::{CARD_GAP, CARD_PAD, Live};
use crate::nsh::widgets::{self, CLEAR};
use crate::nsh::{store, theme};
use crate::state::{Effects, ZStatsAppState};
use crate::{format, i18n, palette};
use nsh_base::TooltipElement;
use nsh_core::ui::prelude::*;
use rust_i18n::t;

/// What stands above and below a collapsed row's lines, before the fit
/// gives the row its share of the leftover (`processes::ROW_PAD`).
pub const ROW_PAD: f32 = 7.;

/// The pinned header: 11 above the 12px title's 19, 9 under it
/// (`processes::LIST_HEADER`; NSH's lines are whole points,
/// `widgets::line`).
pub const LIST_HEADER: f32 = 11. + 19. + 9.;

/// The open filter row: the field's 28 and the 6 under it
/// (`processes::FILTER_ROW_HEIGHT`).
pub const FILTER_ROW: f32 = 34.;
const FIELD_H: f32 = 28.;

/// Never shorter than this, however small the window gets
/// (`processes::FULL_LIST_MIN`).
pub const LIST_MIN: f32 = 120.;

/// The rows region under the pinned header, from the page's height on
/// screen: less the gap under the card, the header, and the filter row
/// while it is open (`processes::rows_height`).
pub fn rows_budget(state: &ZStatsAppState, body: f32) -> f32 {
    let chrome = LIST_HEADER
        + if state.proc_filter_open() {
            FILTER_ROW
        } else {
            0.
        };
    (body - CARD_GAP - chrome).max(LIST_MIN)
}

/// The header's left side: the count, and the ⓘ beside it.
pub fn count_title(count: impl Fn() -> String + 'static, tip: TooltipElement) -> AnyElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .min_w(px(0.))
        .child(widgets::list_title(count).flex_shrink(1.))
        .child(widgets::info_icon(tip))
        .into_element()
}

/// The note a filter that matched nothing leaves in the region.
pub fn no_match(key: &'static str) -> View {
    view()
        .px(px(CARD_PAD))
        .py(px(10.))
        .child(widgets::note(i18n::tr(key)))
}

/// A search's end in the store, and its words: the name filter Apps and
/// Processes share, or the listening card's.
#[derive(Clone, Copy)]
pub struct Filter {
    open: fn(&ZStatsAppState) -> bool,
    text: fn(&ZStatsAppState) -> &str,
    show: fn(&mut ZStatsAppState, &mut dyn Effects),
    hide: fn(&mut ZStatsAppState, &mut dyn Effects),
    set: fn(&mut ZStatsAppState, &str, &mut dyn Effects),
    tip: &'static str,
    placeholder: &'static str,
}

/// The name filter over Apps and Processes.
pub const NAME_FILTER: Filter = Filter {
    open: ZStatsAppState::proc_filter_open,
    text: ZStatsAppState::proc_filter_text,
    show: ZStatsAppState::open_proc_filter,
    hide: ZStatsAppState::close_proc_filter,
    set: ZStatsAppState::set_proc_filter_text,
    tip: "processes.filter_tip",
    placeholder: "processes.filter_placeholder",
};

/// The listening card's search.
pub const LISTEN_FILTER: Filter = Filter {
    open: ZStatsAppState::listen_filter_open,
    text: ZStatsAppState::listen_filter_text,
    show: ZStatsAppState::open_listen_filter,
    hide: ZStatsAppState::close_listen_filter,
    set: ZStatsAppState::set_listen_filter_text,
    tip: "listen.filter_tip",
    placeholder: "listen.filter_placeholder",
};

/// The header control that opens a search.
pub fn filter_chip(live: Live, filter: Filter) -> AnyElement {
    let on = live.memo(filter.open);
    view()
        .flex_none()
        .tooltip(widgets::wrap_tooltip(i18n::tr(filter.tip)))
        .child(
            nsh_base::button()
                .rounded(px(4.))
                .p(px(2.))
                .bg(move || theme::color(move || if on.get() { palette::chip() } else { CLEAR }))
                .hover(|s| s.bg(theme::color(palette::surface_raised)))
                .on_press(move || {
                    let open = on.get_untracked();
                    store::update(|state, fx| {
                        if open {
                            (filter.hide)(state, fx);
                        } else {
                            (filter.show)(state, fx);
                        }
                    });
                })
                .child(widgets::icon(CustomIconName::Search, 11., move || {
                    if on.get() {
                        palette::text()
                    } else {
                        palette::text_muted()
                    }
                })),
        )
        .into_element()
}

/// A search's field while it is open, focused as it appears; the clear
/// empties it, as Escape did on the gpui front end, and leaves the caret
/// in it as gpui-kit's clear button did.
pub fn filter_row(live: Live, filter: Filter) -> AnyElement {
    let on = live.memo(filter.open);
    let query = computed(move || {
        live.track();
        store::read(|state| (filter.text)(state).to_owned())
    });
    let typed = signal(String::new());
    when(
        move || on.get(),
        move || {
            typed.set(String::new());
            let field = element_ref();
            view()
                .h(px(FILTER_ROW))
                .px(px(CARD_PAD))
                .pb(px(FILTER_ROW - FIELD_H))
                .child(
                    view()
                        .flex_row()
                        .h(px(FIELD_H))
                        .items_center()
                        .gap(px(6.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .border(px(1.))
                        .border_color(theme::color(palette::border))
                        .child(widgets::icon(
                            CustomIconName::Search,
                            12.,
                            palette::text_dim,
                        ))
                        .child(
                            input()
                                .element_ref(&field)
                                .flex_1()
                                .value(move || typed.get())
                                .placeholder(i18n::tr(filter.placeholder))
                                .auto_focus(true)
                                .auto_correct(false)
                                .text_size(px(12.))
                                .text_color(theme::color(palette::text))
                                .on_change(move |text: &str| {
                                    typed.set(text.to_owned());
                                    store::update(|state, fx| (filter.set)(state, text, fx));
                                }),
                        )
                        .child(when(
                            move || !query.with(String::is_empty),
                            move || {
                                nsh_base::button()
                                    .flex_none()
                                    .rounded_full()
                                    .p(px(2.))
                                    .hover(|s| s.bg(theme::color(palette::surface_raised)))
                                    .on_press(move || {
                                        typed.set(String::new());
                                        store::update(|state, fx| (filter.set)(state, "", fx));
                                        field.focus();
                                    })
                                    .child(widgets::icon(
                                        CustomIconName::Close,
                                        11.,
                                        palette::text_dim,
                                    ))
                            },
                        )),
                )
        },
    )
    .into_element()
}

/// Where a list's one-off full scan stands, for its chip.
#[derive(Clone, Copy, PartialEq)]
pub enum ScanChip {
    Off,
    Running,
    Showing,
}

/// The words of a page's All chip: its three labels and two tooltips.
#[derive(Clone, Copy)]
pub struct ScanWords {
    pub chip: &'static str,
    pub scanning: &'static str,
    pub back: &'static str,
    pub chip_tip: &'static str,
    pub back_tip: &'static str,
}

/// The chip that asks for the whole table, and the one that gives the
/// top list back; no hover while a scan is in flight
/// (`processes::full_scan_chip`).
pub fn full_scan_chip(chip: Memo<ScanChip>, words: ScanWords, toggle: fn()) -> AnyElement {
    view()
        .flex_none()
        .tooltip(widgets::live_tooltip(move || {
            i18n::tr(if chip.get() == ScanChip::Showing {
                words.back_tip
            } else {
                words.chip_tip
            })
        }))
        .child(
            nsh_base::button()
                .rounded_full()
                .border(px(1.))
                .border_color(move || {
                    theme::color(move || {
                        if chip.get() == ScanChip::Showing {
                            palette::border()
                        } else {
                            palette::border_subtle()
                        }
                    })
                })
                .bg(move || {
                    theme::color(move || {
                        if chip.get() == ScanChip::Showing {
                            palette::chip()
                        } else {
                            CLEAR
                        }
                    })
                })
                .hover(move |s| {
                    if chip.get_untracked() == ScanChip::Off {
                        s.bg(theme::color(palette::surface_raised))
                            .border_color(theme::color(palette::border))
                    } else {
                        s
                    }
                })
                .px(px(7.))
                .py(px(1.))
                .on_press(toggle)
                .child(
                    text(move || {
                        i18n::tr(match chip.get() {
                            ScanChip::Running => words.scanning,
                            ScanChip::Showing => words.back,
                            ScanChip::Off => words.chip,
                        })
                    })
                    .text_size(px(9.))
                    .line_height(widgets::line(9.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(move || {
                        theme::color(move || match chip.get() {
                            ScanChip::Running => palette::text_dim(),
                            ScanChip::Showing => palette::text(),
                            ScanChip::Off => palette::text_muted(),
                        })
                    }),
                ),
        )
        .into_element()
}

/// What this app's own row says while the daily disk check is its work
/// (`processes::disk_check_badge`), `None` the rest of the time.
pub fn disk_check_label(state: &ZStatsAppState) -> Option<String> {
    state.disk_check_running().then(|| {
        i18n::tr(match state.disk_check_held() {
            None => "processes.disk_check",
            Some(Hold::OnBattery) => "processes.disk_check_power",
            Some(Hold::Busy) => "processes.disk_check_quiet",
        })
    })
}

/// The disk check's badge, with the tooltip that says what it is.
pub fn disk_check_badge(label: Memo<Option<String>>) -> View {
    view()
        .flex_none()
        .tooltip(widgets::wrap_tooltip(i18n::tr("processes.disk_check_tip")))
        .child(widgets::pill(
            move || label.with(|l| l.clone().unwrap_or_default()),
            false,
        ))
}

/// One figure of an open row: its label, which gives way, and the
/// figure; a hairline under it unless `last` (`apps::expand_row`,
/// `widgets::kv_row`).
pub fn expand_row(label: String, value: impl Fn() -> String + 'static, last: bool) -> View {
    let row = view()
        .flex_row()
        .justify_between()
        .gap(px(8.))
        .py(px(5.))
        .child(
            text(label)
                .flex_1()
                .min_w(px(0.))
                .number_of_lines(1u32)
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::text_muted)),
        )
        .child(
            text(value)
                .flex_none()
                .tabular_nums()
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::text)),
        );
    if last {
        row
    } else {
        row.border_b(px(1.))
            .border_color(theme::color(palette::border_subtle))
    }
}

/// `widgets::KV_TIP_FROM`: a value this long may be cut short, and earns
/// the whole of it on hover.
const KV_TIP_FROM: usize = 12;

/// One "label ……… value" cell of a two-up grid (`widgets::kv_row`): the
/// label gives way, the value keeps at most a little over half the cell
/// and is cut short past that, whole on hover; a hairline under it unless
/// `last`.
fn kv_cell(label: String, value: impl Fn() -> String + Clone + 'static, last: bool) -> View {
    let long = {
        let value = value.clone();
        move || value().chars().count() >= KV_TIP_FROM
    };
    let tip = value.clone();
    let row = view()
        .flex_row()
        .justify_between()
        .gap(px(8.))
        .py(px(5.))
        .child(
            text(label)
                .flex_1()
                .min_w(px(0.))
                .number_of_lines(1u32)
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::text_muted)),
        )
        .child(
            view()
                .flex_none()
                .max_w(relative(0.55))
                .min_w(px(0.))
                .tooltip(widgets::live_tooltip(tip).disabled(move || !long()))
                .child(
                    text(value)
                        .number_of_lines(1u32)
                        .tabular_nums()
                        .text_size(px(11.))
                        .line_height(widgets::line(11.))
                        .text_color(theme::color(palette::text)),
                ),
        );
    if last {
        row
    } else {
        row.border_b(px(1.))
            .border_color(theme::color(palette::border_subtle))
    }
}

/// Key/value pairs two-up, the first half down the left column
/// (`widgets::kv_pairs`); `value(i)` reads the i-th pair's figure.
pub fn kv_columns(labels: Vec<String>, value: impl Fn(usize) -> String + Copy + 'static) -> View {
    let mid = labels.len().div_ceil(2);
    let column = |range: std::ops::Range<usize>, labels: &[String]| {
        let end = range.end;
        let cells: Vec<View> = range
            .map(|i| kv_cell(labels[i].clone(), move || value(i), i + 1 == end))
            .collect();
        view().flex_1().min_w(px(0.)).children(cells)
    };
    view()
        .flex_row()
        .gap(px(14.))
        .mt(px(10.))
        .child(column(0..mid, &labels))
        .child(column(mid..labels.len(), &labels))
}

/// One alert bar: its label, and the layer in a word with its ⓘ while a
/// per-name entry decided it; the figure at the right
/// (`processes::bar_row`).
pub fn bar_row(label: String, bar: impl Fn() -> BarDisplay + Copy + 'static, last: bool) -> View {
    let row = view()
        .flex_row()
        .justify_between()
        .items_center()
        .gap(px(8.))
        .py(px(5.))
        .child(
            view()
                .flex_row()
                .flex_1()
                .min_w(px(0.))
                .items_center()
                .gap(px(4.))
                .child(
                    text(label)
                        .flex_shrink(1.)
                        .number_of_lines(1u32)
                        .text_size(px(11.))
                        .line_height(widgets::line(11.))
                        .text_color(theme::color(palette::text_muted)),
                )
                .child(when(
                    move || bar().overridden,
                    move || {
                        let words = move || {
                            if bar().yours {
                                ("processes.bar_yours", "processes.bar_yours_tip")
                            } else {
                                ("processes.bar_table", "processes.bar_table_tip")
                            }
                        };
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(4.))
                            .child(
                                text(move || i18n::tr(words().0))
                                    .flex_none()
                                    .text_size(px(9.5))
                                    .line_height(widgets::line(9.5))
                                    .text_color(theme::color(|| {
                                        palette::tiny_label(palette::text_dim())
                                    })),
                            )
                            .child(widgets::info_icon(widgets::live_tooltip(move || {
                                i18n::tr(words().1)
                            })))
                    },
                )),
        )
        .child(
            text(move || bar().text)
                .flex_none()
                .tabular_nums()
                .text_size(px(11.))
                .line_height(widgets::line(11.))
                .text_color(theme::color(palette::text)),
        );
    if last {
        row
    } else {
        row.border_b(px(1.))
            .border_color(theme::color(palette::border_subtle))
    }
}

/// What a row's threshold editor shows: the name its two rules key on,
/// what the rows of bare percentages are percentages of, and which chip
/// each row lights.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Editor {
    pub name: String,
    pub caption: String,
    pub lit: [Option<String>; 2],
}

/// The editor for `name`'s two rules `keys`, once the settings are read:
/// each row lights the preset its bar reads as (`Resolved::in_force`).
pub fn editor_of(state: &ZStatsAppState, name: &str, keys: [&str; 2]) -> Option<Editor> {
    let bars = Resolved::of(state.settings()?);
    let total = state.latest()?.snapshot.memory.total_bytes;
    Some(Editor {
        name: name.to_owned(),
        caption: t!("processes.th_caption", total = format::memory(total)).to_string(),
        lit: keys.map(|key| bars.in_force(key, name).and_then(|held| held.lit)),
    })
}

/// The per-subject threshold editor (`processes::threshold_editor`): one
/// chip row per rule, writing exactly what the Alerts tab's card writes,
/// keyed on the executable's name as the bars above it are. The keys
/// are the caller's — a process's pair and a tree's are different rules.
pub fn threshold_editor(
    editor: impl Fn() -> Editor + Copy + 'static,
    keys: [&'static str; 2],
) -> View {
    let mut col = view()
        .mt(px(8.))
        .pt(px(8.))
        .gap(px(4.))
        .border_t(px(1.))
        .border_color(theme::color(palette::border_subtle))
        .child(widgets::note(move || editor().caption));
    let labels = [
        i18n::tr("processes.bar_cpu_short"),
        i18n::tr("processes.bar_mem_short"),
    ];
    for (i, (key, label)) in keys.into_iter().zip(labels).enumerate() {
        col = col.child(threshold_row(editor, i, key, label));
    }
    col
}

fn threshold_row(
    editor: impl Fn() -> Editor + Copy + 'static,
    i: usize,
    key: &'static str,
    label: String,
) -> View {
    let chips: Vec<AnyElement> = alertbars::presets(key)
        .into_iter()
        .map(|(chip_label, value)| {
            let on = move || editor().lit[i].as_deref() == Some(value);
            nsh_base::button()
                .flex_none()
                .rounded_full()
                .border(px(1.))
                .border_color(move || theme::color(move || palette::choice_line(on())))
                .bg(move || theme::color(move || palette::choice_fill(on())))
                .hover(move |s| s.bg(theme::color(move || palette::choice_hover(on()))))
                .px(px(6.))
                .on_press(move || {
                    let name = editor().name;
                    store::update(|state, fx| {
                        if let Err(e) = state.apply_alert_override(key, &name, value, fx) {
                            tracing::error!("alert override failed: {e}");
                        }
                    });
                })
                .child(
                    text(chip_label)
                        .text_size(px(9.5))
                        .line_height(widgets::line(9.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(move || theme::color(move || palette::choice_ink(on()))),
                )
                .into_element()
        })
        .collect();
    view()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .child(
            text(label)
                .flex_none()
                .w(px(54.))
                .number_of_lines(1u32)
                .text_size(px(9.5))
                .line_height(widgets::line(9.5))
                .text_color(theme::color(|| palette::tiny_label(palette::text_dim()))),
        )
        .child(
            view()
                .flex_row()
                .flex_1()
                .min_w(px(0.))
                .gap(px(3.))
                .children(chips),
        )
}

/// Quit, in the accent's outline, at the end of an open row; what it
/// asks first is the caller's (`apps::quit_button`,
/// `processes::kill_button`).
pub fn quit_button(label: String, on_press: impl Fn() + 'static) -> AnyElement {
    view()
        .flex_row()
        .justify_end()
        .child(
            nsh_base::button()
                .mt(px(8.))
                .h(px(20.))
                .px(px(8.))
                .rounded(px(5.))
                .border(px(1.))
                .border_color(theme::color(|| palette::accent_wash(45)))
                .items_center()
                .justify_center()
                .hover(|s| s.bg(theme::color(|| palette::accent_wash(10))))
                .on_press(on_press)
                .child(
                    text(label)
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::color(palette::accent_light)),
                ),
        )
        .into_element()
}
