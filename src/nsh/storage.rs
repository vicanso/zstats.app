//! The disk-space window on NSH: `views/storage.rs` and its
//! `StorageWindow` (`gpui_app.rs`), whose comments say why the window and
//! each part of it is the way it is — this file carries the same choices
//! and does not repeat their reasons. The window's rules are shared
//! (`storagepage.rs`, the directory map's layout in `dirmap.rs`); the
//! state is the store's, so a walk or a search outlives the window. The
//! tabs arrived in phase 4k of docs/nsh-migration.md: the frame and
//! Duplicates first, then the analysis's lists, then the directory map —
//! its tiles views placed in points, not a canvas: a new canvas layer
//! draws once asynchronously before the host can stop it (the plan's
//! problem 1), and the map is made again on every click.
//!
//! The tab strip stays put, and so does each card's head: only the
//! result scrolls, inside the card's border. A folder is picked in the
//! system's panel, which the host opens (`HostAction::PickFolder`).
//!
//! Its keys are the gpui window's: `mod+1` and `mod+2` pick the tab,
//! `mod+r` runs the tab's walk again, Escape closes (and `mod+w`, through
//! the host's Window menu).

use crate::assets::CustomIconName;
use crate::cleanhints;
use crate::dirmap::{self, Crumb, Kids, NARROW_MAP_H, Paint, Tile};
use crate::diskscan::{self, DirHit};
use crate::dupes::DupeScope;
use crate::nsh::root::{self, AuxWindow, FolderPick, HostAction, Live};
use crate::nsh::widgets::{self, CLEAR};
use crate::nsh::{chrome, store, theme};
use crate::state::{DiskAnalysis, DupeSearch, Expansion, StorageTab, ZStatsAppState};
use crate::storagepage::{
    self, DUPE_FILES_SHOWN, HEAD_PT, META_PT, Preset, ROW_PT, SIDE_BY_SIDE_MIN_W, SIDE_W,
};
use crate::{active, color, finder, format, i18n, opener, palette, prefs};
use nsh_base::TooltipElement;
use nsh_core::ui::prelude::*;
use rust_i18n::t;
use std::env;
use std::path::{Path, PathBuf};

/// Above the tab strip and around the tab's card (`gpui_app`'s
/// `AUX_BODY_PAD`).
const BODY_PAD: f32 = 14.;

/// The window's side margin.
const SIDE: f32 = 16.;

/// A card's rows and heads sit this far in (`views/storage.rs`'s 13).
const INSET: f32 = 13.;

/// One trailing button's slot; every row reserves two.
const ACTION_SLOT: f32 = 22.;

/// How far a row's buttons recede until the row is hovered.
const ACTION_REST: f32 = 0.4;

/// Gap between a row's parts.
const ROW_GAP: f32 = 6.;

/// One indent step.
const INDENT_STEP: f32 = 13.;

/// The widest a file or folder name may grow before it, too, truncates.
const NAME_MAX: f32 = 240.;

/// The words the system's folder panel confirms with, for `pick`.
pub fn pick_prompt(pick: FolderPick) -> String {
    i18n::tr(match pick {
        FolderPick::Analysis => "disk.ana_pick_go",
        FolderPick::Exclude => "disk.ana_exclude_pick_go",
        FolderPick::Dupes => "disk.dup_pick_go",
    })
}

/// What the host's folder panel answered, for `pick`. Any thread.
pub fn picked(pick: FolderPick, paths: Vec<PathBuf>) {
    match pick {
        FolderPick::Analysis => {
            if let Some(root) = paths.into_iter().next() {
                store::update(|state, fx| state.set_disk_analysis_at(root, fx));
            }
        }
        FolderPick::Exclude => {
            if storagepage::add_exclude(paths.iter().map(|p| p.display().to_string())) {
                store::changed();
            }
        }
        FolderPick::Dupes => {
            if let Some(folder) = paths.into_iter().next() {
                store::update(|state, fx| state.search_dupes_in(folder, fx));
            }
        }
    }
}

#[component]
pub fn Storage() -> impl IntoElement {
    // The window is there to be drawn for as long as it exists.
    let live = Live::follow(|_| true);
    for (key, tab) in [
        ("mod+1", StorageTab::Analysis),
        ("mod+2", StorageTab::Duplicates),
    ] {
        on_key(key, move |_| {
            store::update(|state, fx| state.set_storage_tab(tab, fx));
        });
    }
    on_key("mod+r", |_| {
        store::update(|state, fx| state.rerun_storage_tab(fx))
    });
    on_key("escape", |_| {
        root::ask(HostAction::Close(AuxWindow::Storage))
    });
    let tab = live.memo(ZStatsAppState::storage_tab);
    let trashed =
        live.memo(|state: &ZStatsAppState| storagepage::trashed_note(state.trashed_this_session()));
    // Room for the map beside the lists rather than above them.
    let width = signal(0.0_f32);
    let wide = computed(move || width.get() >= SIDE_BY_SIDE_MIN_W);
    // The bulk sheet, open while it holds what it would move.
    let sheet: Signal<Option<Sheet>> = signal(None);
    // The directory map's box as last laid out.
    let map_size = signal((0.0_f32, 0.0_f32));
    view()
        .size_full()
        .bg(theme::color(palette::ground))
        .on_layout(move |b: &Bounds<Points>| width.set(b.size.width.0))
        .child(
            view()
                .flex_none()
                .px(px(SIDE))
                .pt(px(BODY_PAD))
                .child(tab_strip(live, tab)),
        )
        .child(
            view()
                .flex_1()
                .min_h(px(0.))
                .w_full()
                .px(px(SIDE))
                .py(px(BODY_PAD))
                .gap(px(8.))
                .child(when(
                    move || trashed.with(Option::is_some),
                    move || trashed_note(trashed),
                ))
                .child(dynamic(move || match tab.get() {
                    StorageTab::Analysis => analysis_card(live, wide, sheet, map_size),
                    StorageTab::Duplicates => dupes_card(live),
                })),
        )
        .child(when(
            move || sheet.with(Option::is_some),
            move || pick_sheet(sheet),
        ))
}

/// The two tabs, underlined (`storage::tab_strip`): the one on screen on
/// a 2px bar, a tab whose work runs while another is shown wearing a dot.
/// Made again as the tab changes: an unselected tab's hover previews the
/// bar, and a hover style is fixed when it is made.
fn tab_strip(live: Live, tab: Memo<StorageTab>) -> AnyElement {
    let busy = live.memo(|state: &ZStatsAppState| {
        (
            matches!(state.disk_analysis(), DiskAnalysis::Running { .. }),
            matches!(state.dupe_search(), DupeSearch::Running { .. }),
        )
    });
    dynamic(move || strip(tab.get(), busy)).into_element()
}

fn strip(shown: StorageTab, busy: Memo<(bool, bool)>) -> AnyElement {
    let cell = move |this: StorageTab| {
        let on = shown == this;
        let busy = move || {
            let (analysis, dupes) = busy.get();
            !on && match this {
                StorageTab::Analysis => analysis,
                StorageTab::Duplicates => dupes,
            }
        };
        let tip = t!(
            "tabs.shortcut",
            name = i18n::tr(this.label_key()),
            hint = chrome::shortcut_hint(),
            n = this.index() + 1
        )
        .to_string();
        let mut label = text(i18n::tr(this.label_key()))
            .text_size(px(12.))
            .line_height(widgets::line(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::color(move || {
                if on {
                    palette::text()
                } else {
                    palette::text_dim()
                }
            }));
        let mut bar = view().h(px(2.)).rounded_full();
        if on {
            bar = bar.bg(theme::color(|| {
                palette::mark().map_or(palette::text(), |(fill, _)| fill)
            }));
        } else {
            label = label.group_hover(|s| s.text_color(theme::color(palette::text)));
            bar = bar.group_hover(|s| s.bg(theme::color(palette::border)));
        }
        view()
            .flex_none()
            .group()
            .tooltip(widgets::wrap_tooltip(tip))
            .child(
                nsh_base::button()
                    .on_press(move || store::update(|state, fx| state.set_storage_tab(this, fx)))
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(5.))
                            .px(px(2.))
                            .pt(px(2.))
                            .pb(px(7.))
                            .child(label)
                            .child(when(busy, || {
                                view()
                                    .flex_none()
                                    .w(px(5.))
                                    .h(px(5.))
                                    .rounded_full()
                                    .bg(theme::color(palette::text_dim))
                            })),
                    )
                    .child(bar),
            )
            .into_element()
    };
    // The hairline first, the bars over it.
    view()
        .relative()
        .child(
            view()
                .absolute()
                .left(px(0.))
                .right(px(0.))
                .bottom(px(0.))
                .h(px(1.))
                .bg(theme::color(palette::border_subtle)),
        )
        .child(
            view()
                .flex_row()
                .gap(px(18.))
                .children(StorageTab::ALL.into_iter().map(cell).collect::<Vec<_>>()),
        )
        .into_element()
}

/// Above every tab once anything went to the Trash this session: the
/// space comes back only when the Trash is emptied, which stays the
/// reader's own act — the button opens it, never empties it.
fn trashed_note(line: Memo<Option<String>>) -> AnyElement {
    widgets::card()
        .flex_none()
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(10.))
                .px(px(INSET))
                .py(px(9.))
                .child(
                    text(move || line.get().unwrap_or_default())
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(px(11.))
                        .line_height(px(15.))
                        .text_color(theme::color(palette::text_muted)),
                )
                .child(small_button(
                    i18n::tr("disk.open_trash"),
                    None,
                    finder::open_trash,
                )),
        )
        .into_element()
}

// ---- the window's shared pieces ----------------------------------------

/// gpui-kit's default button at its smallest (`Button::xsmall()`), with
/// an icon before its words when given one (`Button::ghost()` then): 20
/// tall, 4 in, the kit's 6 corner, its words at 12.
fn small_button(
    label: String,
    icon: Option<CustomIconName>,
    on_press: impl Fn() + 'static,
) -> AnyElement {
    let ghost = icon.is_some();
    let mut content = view().flex_row().items_center().gap(px(4.));
    if let Some(icon) = icon {
        content = content.child(widgets::icon(icon, 12., palette::text));
    }
    nsh_base::button()
        .flex_none()
        .h(px(20.))
        .px(px(4.))
        .rounded(px(6.))
        .border(px(if ghost { 0. } else { 1. }))
        .border_color(theme::color(palette::border))
        .bg(theme::color(
            move || if ghost { CLEAR } else { palette::inset() },
        ))
        .items_center()
        .justify_center()
        .hover(|s| s.bg(theme::color(palette::surface_raised)))
        .on_press(on_press)
        .child(
            content.child(
                text(label)
                    .text_size(px(12.))
                    .line_height(px(15.))
                    .text_color(theme::color(palette::text)),
            ),
        )
        .into_element()
}

/// The one filled button a tab shows before its first run — the theme's
/// primary (`Button::primary().small()`): 24 tall, 8 in, words at 14.
fn primary_button(label: String, on_press: impl Fn() + 'static) -> AnyElement {
    nsh_base::button()
        .flex_none()
        .h(px(24.))
        .px(px(8.))
        .rounded(px(6.))
        .bg(theme::color(palette::primary))
        .items_center()
        .justify_center()
        .hover(|s| {
            s.bg(theme::color(|| color::Rgba {
                a: 0.9,
                ..palette::primary()
            }))
        })
        .on_press(on_press)
        .child(
            text(label)
                .text_size(px(14.))
                .line_height(px(17.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(palette::primary_ink)),
        )
        .into_element()
}

/// A tab before its first run: what it finds, what it costs, one button.
fn empty_state(lead: String, cost: String, tip: String, button: AnyElement) -> AnyElement {
    view()
        .px(px(INSET))
        .pt(px(4.))
        .pb(px(16.))
        .gap(px(6.))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(5.))
                .child(
                    text(lead)
                        .text_size(px(HEAD_PT))
                        .line_height(widgets::line(HEAD_PT))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::color(palette::text)),
                )
                .child(widgets::info_icon(widgets::wrap_tooltip(tip))),
        )
        .child(
            text(cost)
                .text_size(px(META_PT + 1.))
                .line_height(px(15.))
                .text_color(theme::color(palette::text_muted)),
        )
        .child(view().flex_row().pt(px(6.)).child(button))
        .into_element()
}

/// A tab's card: `head` stays put, `body` scrolls inside the card's
/// border, and the card keeps its natural height until it meets the
/// window's bottom margin.
fn tab_card(head: Vec<AnyElement>, body: AnyElement) -> AnyElement {
    widgets::list_shell()
        .flex_shrink(1.)
        .min_h(px(0.))
        .pt(px(4.))
        .children(
            head.into_iter()
                .map(|part| view().flex_none().child(part).into_element())
                .collect::<Vec<_>>(),
        )
        .child(scroll().flex_shrink(1.).min_h(px(0.)).child(body))
        .into_element()
}

/// A tab card's head: the scope it answered as the title with the run
/// controls beside it, and the caption on a line of its own under them.
fn header_rows(scope: String, caption: AnyElement, controls: AnyElement) -> AnyElement {
    view()
        .px(px(INSET))
        .pt(px(8.))
        .pb(px(8.))
        .gap(px(2.))
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(10.))
                .child(
                    text(scope)
                        .flex_shrink(1.)
                        .min_w(px(0.))
                        .number_of_lines(1u32)
                        .text_size(px(HEAD_PT))
                        .line_height(widgets::line(HEAD_PT))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::color(palette::text)),
                )
                .child(controls),
        )
        .child(caption)
        .into_element()
}

/// The caption under a head: what the run found, then the method behind
/// an ⓘ.
fn caption_line(rest: String, tip: Option<String>) -> AnyElement {
    caption_with(rest, tip, None)
}

/// [`caption_line`], with what a walk left out after its words.
fn caption_with(rest: String, tip: Option<String>, extra: Option<AnyElement>) -> AnyElement {
    let mut row = view()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(px(5.))
        .child(
            text(rest)
                .flex_shrink(1.)
                .min_w(px(0.))
                .text_size(px(META_PT))
                .line_height(px(14.))
                .text_color(theme::color(palette::text_dim)),
        );
    if let Some(extra) = extra {
        row = row.child(extra);
    }
    if let Some(tip) = tip {
        row = row.child(widgets::info_icon(widgets::wrap_tooltip(tip)));
    }
    row.into_element()
}

/// A tab's run / re-run / cancel chip.
fn run_chip(label: String, on_press: impl Fn() + 'static) -> AnyElement {
    run_chip_with(label, false, None, on_press)
}

/// [`run_chip`], brighter and with its sentence on hover when the result
/// it would replace is old.
fn run_chip_with(
    label: String,
    stale: bool,
    tip: Option<String>,
    on_press: impl Fn() + 'static,
) -> AnyElement {
    let chip = nsh_base::button()
        .flex_none()
        .rounded_full()
        .border(px(1.))
        .border_color(theme::color(move || {
            if stale {
                palette::text_muted()
            } else {
                palette::border()
            }
        }))
        .bg(theme::color(move || {
            if stale {
                palette::chip()
            } else {
                palette::inset()
            }
        }))
        .px(px(9.))
        .py(px(2.))
        .hover(|s| s.bg(theme::color(palette::surface_raised)))
        .on_press(on_press)
        .child(
            text(label)
                .text_size(px(META_PT + 1.))
                .line_height(widgets::line(META_PT + 1.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(palette::text)),
        );
    match tip {
        Some(tip) => view()
            .flex_none()
            .tooltip(widgets::wrap_tooltip(tip))
            .child(chip)
            .into_element(),
        None => chip.into_element(),
    }
}

/// A toolbar chip (`storage::toolbar_chip`): lit in the raised fill, the
/// others muted until hovered. `on_press` is `None` while it is inert.
fn toolbar_chip(
    content: AnyElement,
    on: bool,
    tip: Option<TooltipElement>,
    on_press: Option<Box<dyn Fn()>>,
) -> AnyElement {
    let mut chip = nsh_base::button()
        .flex_none()
        .rounded(px(5.))
        .px(px(6.))
        .py(px(2.))
        .bg(theme::color(move || {
            if on { palette::surface_raised() } else { CLEAR }
        }))
        .hover(move |s| {
            if on {
                s
            } else {
                s.bg(theme::color(palette::surface_raised))
            }
        })
        .child(content);
    if let Some(press) = on_press {
        chip = chip.on_press(press);
    }
    match tip {
        Some(tip) => view().flex_none().tooltip(tip).child(chip).into_element(),
        None => chip.into_element(),
    }
}

/// A toolbar chip's words: the first ink while lit or hovered.
fn chip_words(words: String, on: bool) -> Text {
    text(words)
        .number_of_lines(1u32)
        .text_size(px(META_PT + 1.))
        .line_height(widgets::line(META_PT + 1.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::color(move || {
            if on {
                palette::text()
            } else {
                palette::text_muted()
            }
        }))
}

/// A block's heading inside a card: its name at heading size, a dim
/// clause of what it holds, and a control on the right.
fn section_heading(title: String, meta: Option<String>, right: Option<AnyElement>) -> AnyElement {
    let mut left = view()
        .flex_row()
        .flex_shrink(1.)
        .min_w(px(0.))
        .items_baseline()
        .gap(px(8.))
        .child(
            text(title)
                .flex_none()
                .text_size(px(HEAD_PT))
                .line_height(widgets::line(HEAD_PT))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::color(palette::text)),
        );
    if let Some(meta) = meta {
        left = left.child(
            text(meta)
                .flex_shrink(1.)
                .min_w(px(0.))
                .number_of_lines(1u32)
                .text_size(px(META_PT))
                .line_height(widgets::line(META_PT))
                .text_color(theme::color(palette::text_dim)),
        );
    }
    let mut row = view()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .pt(px(10.))
        .pb(px(5.))
        .child(left);
    if let Some(right) = right {
        row = row.child(right);
    }
    row.into_element()
}

/// A size as its number in bold and its unit in the dim ink.
fn size_text(bytes: u64) -> AnyElement {
    let text_ = format::memory(bytes);
    let (value, unit) = text_.split_once(' ').unwrap_or((text_.as_str(), ""));
    view()
        .flex_row()
        .flex_none()
        .items_baseline()
        .gap(px(2.))
        .child(
            text(value.to_owned())
                .font_family(widgets::MONO)
                .text_size(px(ROW_PT))
                .line_height(widgets::line(ROW_PT))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::color(palette::text)),
        )
        .child(
            text(unit.to_owned())
                .font_family(widgets::MONO)
                .text_size(px(META_PT))
                .line_height(widgets::line(META_PT))
                .text_color(theme::color(palette::text_dim)),
        )
        .into_element()
}

/// A path as two runs: the folders it sits in, dim and the first to give
/// way, then its own name, bright and kept; the whole location on hover.
fn path_label(shown: &str, tip: String, size: f32) -> AnyElement {
    let (prefix, name) = match shown.trim_end_matches('/').rfind('/') {
        Some(at) => shown.split_at(at + 1),
        None => ("", shown),
    };
    let mut row = view()
        .flex_row()
        .flex_1()
        .min_w(px(0.))
        .items_baseline()
        .tooltip(widgets::wrap_tooltip(tip));
    if !prefix.is_empty() {
        row = row.child(
            text(prefix.to_owned())
                .flex_shrink(1.)
                .min_w(px(0.))
                .number_of_lines(1u32)
                .text_size(px(size))
                .line_height(widgets::line(size))
                .text_color(theme::color(palette::text_dim)),
        );
    }
    row.child(
        text(name.to_owned())
            .flex_shrink(0.)
            .max_w(px(NAME_MAX))
            .number_of_lines(1u32)
            .text_size(px(size))
            .line_height(widgets::line(size))
            .text_color(theme::color(palette::text)),
    )
    .into_element()
}

/// A row's two trailing slots, each [`ACTION_SLOT`] wide whether or not
/// it holds a button, receded until the row (a `group`) is hovered.
fn action_slots(first: Option<AnyElement>, second: Option<AnyElement>) -> AnyElement {
    let slot = |button: Option<AnyElement>| {
        let slot = view()
            .flex_row()
            .flex_none()
            .w(px(ACTION_SLOT))
            .justify_center();
        match button {
            Some(button) => slot.child(button),
            None => slot,
        }
    };
    view()
        .flex_row()
        .flex_none()
        .opacity(ACTION_REST)
        .group_hover(|s| s.opacity(1.))
        .child(slot(first))
        .child(slot(second))
        .into_element()
}

/// A neutral pill on a row: what a copy or a folder is, its reason on
/// hover.
fn row_pill(label: String, tip: String, ink: fn() -> color::Rgba) -> AnyElement {
    view()
        .flex_none()
        .tooltip(widgets::wrap_tooltip(tip))
        .child(
            text(label)
                .rounded_full()
                .px(px(5.))
                .bg(theme::color(palette::inset))
                .text_size(px(9.))
                .line_height(widgets::line(9.))
                .text_color(theme::color(move || palette::tiny_label(ink()))),
        )
        .into_element()
}

/// When a file last changed, in the figures' face, the full sentence on
/// hover.
fn modified_label(day: String) -> AnyElement {
    view()
        .flex_none()
        .tooltip(widgets::wrap_tooltip(&t!(
            "disk.dup_modified",
            date = day.clone()
        )))
        .child(
            text(day)
                .font_family(widgets::MONO)
                .text_size(px(META_PT))
                .line_height(widgets::line(META_PT))
                .text_color(theme::color(palette::text_dim)),
        )
        .into_element()
}

/// Reveal in the file manager: a row's first slot.
fn reveal_button(path: PathBuf, tip: String) -> AnyElement {
    widgets::ghost_icon(
        CustomIconName::Folder,
        widgets::wrap_tooltip(tip),
        move || finder::reveal(&path),
    )
}

// ---- duplicates ---------------------------------------------------------

/// The duplicate search's head as it stands.
#[derive(Clone, PartialEq)]
struct DupesHead {
    scope: String,
    rest: String,
    run: String,
    running: bool,
    off: bool,
    /// The picked folder, `~`'d, when the scope is not home.
    folder: Option<String>,
}

fn dupes_head(state: &ZStatsAppState) -> DupesHead {
    let (scope, rest) = storagepage::dupes_caption(state);
    DupesHead {
        scope,
        rest,
        run: storagepage::dupes_run_label(state),
        running: matches!(state.dupe_search(), DupeSearch::Running { .. }),
        off: matches!(state.dupe_search(), DupeSearch::Off),
        folder: match state.dupe_scope() {
            DupeScope::Home => None,
            DupeScope::Folder(path) => Some(format::tilde(path)),
        },
    }
}

/// One copy of a duplicate, as its row shows it.
#[derive(Clone, PartialEq)]
struct DupeCopy {
    path: PathBuf,
    shown: String,
    oldest: bool,
    clone: bool,
    linked: bool,
    modified: Option<String>,
    /// The trash confirm's sentence.
    body: String,
}

/// One group of copies.
#[derive(Clone, PartialEq)]
struct Group {
    name: String,
    meta: String,
    frees: u64,
    frees_tip: String,
    copies: Vec<DupeCopy>,
    /// How many copies past [`DUPE_FILES_SHOWN`].
    hidden: usize,
}

/// The duplicate search's body as it stands.
#[derive(Clone, PartialEq)]
enum DupesBody {
    Off,
    Running,
    Failed,
    None,
    Ready {
        title: String,
        meta: String,
        frees: u64,
        stale: bool,
        groups: Vec<Group>,
        hidden: usize,
    },
}

fn dupes_body_of(state: &ZStatsAppState) -> DupesBody {
    let result = match state.dupe_search() {
        DupeSearch::Off => return DupesBody::Off,
        DupeSearch::Running { .. } => return DupesBody::Running,
        DupeSearch::Failed(_) => return DupesBody::Failed,
        DupeSearch::Ready(result) => result,
    };
    if result.groups.is_empty() {
        return DupesBody::None;
    }
    let home = env::var("HOME").unwrap_or_default();
    let shown = state.dupes_shown().min(result.groups.len());
    let (title, meta) = storagepage::dupes_heading(result.groups.len(), result.extra_copies());
    let groups = result.groups[..shown]
        .iter()
        .map(|group| {
            let others = group.files.len() - 1;
            let oldest = group.oldest().map(Path::to_path_buf);
            Group {
                name: storagepage::group_name(group),
                meta: storagepage::group_meta(group),
                frees: group.reclaimable,
                frees_tip: storagepage::frees_tip(group),
                hidden: group.files.len().saturating_sub(DUPE_FILES_SHOWN),
                copies: group
                    .files
                    .iter()
                    .take(DUPE_FILES_SHOWN)
                    .map(|file| {
                        let shown = format::tilde_path(&file.path.display().to_string(), &home);
                        DupeCopy {
                            body: storagepage::dupe_trash_body(&shown, others, file.frees()),
                            path: file.path.clone(),
                            oldest: oldest.as_deref() == Some(file.path.as_path()),
                            clone: file.shares_storage,
                            linked: file.linked,
                            modified: file.modified.map(format::date),
                            shown,
                        }
                    })
                    .collect(),
            }
        })
        .collect();
    DupesBody::Ready {
        title,
        meta,
        frees: result.reclaimable(),
        stale: state.dupes_stale(),
        groups,
        hidden: result.groups.len() - shown,
    }
}

/// The duplicate search's card: the head and the scope row, then either
/// the empty state or the groups, most to free first.
fn dupes_card(live: Live) -> AnyElement {
    let head = live.memo(dupes_head);
    let body = live.memo(dupes_body_of);
    // The card's frame is made again only as the search turns on or off:
    // its caption's age moves every minute, and a frame made again with
    // it would throw the list back to its top.
    let off = computed(move || head.with(|h| h.off));
    dynamic(move || {
        let off = off.get();
        let mut parts: Vec<AnyElement> = Vec::new();
        if !off {
            parts.push(dynamic(move || dupes_header(&head.get())).into_element());
        }
        parts.push(dynamic(move || dupes_scope_row(&head.get())).into_element());
        let content = if off {
            empty_state(
                i18n::tr("disk.dup_empty_lead"),
                i18n::tr("disk.dup_empty_cost"),
                i18n::tr("disk.dup_hint"),
                primary_button(i18n::tr("disk.dup_find"), || {
                    store::update(|state, fx| state.start_dupes(fx))
                }),
            )
        } else {
            view()
                .px(px(INSET))
                .pb(px(11.))
                .child(dynamic(move || dupes_body(body.get())))
                .into_element()
        };
        tab_card(parts, content)
    })
    .into_element()
}

fn dupes_header(h: &DupesHead) -> AnyElement {
    let mut controls = view()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .child(run_chip(h.run.clone(), || {
            store::update(|state, fx| storagepage::dupes_run_or_cancel(state, fx))
        }));
    if !h.running {
        controls = controls.child(widgets::ghost_icon(
            CustomIconName::Close,
            widgets::wrap_tooltip(i18n::tr("disk.dup_dismiss_hint")),
            || store::update(|state, fx| state.clear_dupes(fx)),
        ));
    }
    header_rows(
        h.scope.clone(),
        caption_line(h.rest.clone(), Some(i18n::tr("disk.dup_hint"))),
        controls.into_element(),
    )
}

/// Home, or a picked folder; picking one searches it at once. Dimmed and
/// inert while a search runs.
fn dupes_scope_row(h: &DupesHead) -> AnyElement {
    let running = h.running;
    let picked = h.folder.is_some();
    let mut row = view()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(px(4.))
        .px(px(INSET))
        .pb(px(8.))
        .opacity(if running { 0.45 } else { 1. })
        .child(toolbar_chip(
            chip_words(i18n::tr("disk.ana_preset_home"), !picked).into_element(),
            !picked,
            Some(widgets::wrap_tooltip(i18n::tr("disk.dup_scope_home_tip"))),
            (picked && !running).then(|| {
                Box::new(|| store::update(|state, fx| state.reset_dupe_scope(fx))) as Box<dyn Fn()>
            }),
        ));
    if let Some(folder) = h.folder.clone() {
        row = row.child(toolbar_chip(
            view()
                .max_w(px(220.))
                .child(chip_words(folder, true))
                .into_element(),
            true,
            Some(widgets::wrap_tooltip(i18n::tr("disk.dup_scope_folder_tip"))),
            None,
        ));
    }
    row.child(widgets::ghost_icon(
        CustomIconName::FolderOpen,
        widgets::wrap_tooltip(i18n::tr("disk.dup_pick_hint")),
        move || {
            if !running {
                root::ask(HostAction::PickFolder(FolderPick::Dupes));
            }
        },
    ))
    .into_element()
}

/// "Frees 203 MB", or that a group of clones frees nothing.
fn frees_label(bytes: u64) -> AnyElement {
    if bytes == 0 {
        return text(i18n::tr("disk.dup_frees_nothing"))
            .flex_none()
            .text_size(px(META_PT))
            .line_height(widgets::line(META_PT))
            .text_color(theme::color(palette::text_dim))
            .into_element();
    }
    view()
        .flex_row()
        .flex_none()
        .items_baseline()
        .gap(px(5.))
        .child(
            text(i18n::tr("disk.dup_frees"))
                .text_size(px(META_PT))
                .line_height(widgets::line(META_PT))
                .text_color(theme::color(palette::text_dim)),
        )
        .child(size_text(bytes))
        .into_element()
}

fn dupes_body(body: DupesBody) -> AnyElement {
    let (title, meta, frees, stale, groups, hidden) = match body {
        DupesBody::Off => return view().into_element(),
        DupesBody::Running => return widgets::note(i18n::tr("disk.dup_running")).into_element(),
        // The head's caption carries the error.
        DupesBody::Failed => return widgets::note(i18n::tr("disk.dup_failed_body")).into_element(),
        DupesBody::None => return widgets::note(i18n::tr("disk.dup_none")).into_element(),
        DupesBody::Ready {
            title,
            meta,
            frees,
            stale,
            groups,
            hidden,
        } => (title, meta, frees, stale, groups, hidden),
    };
    let last = groups.len().saturating_sub(1);
    let mut out = view().child(section_heading(title, Some(meta), Some(frees_label(frees))));
    if stale {
        out = out.child(
            view()
                .pb(px(4.))
                .child(widgets::note(i18n::tr("disk.dup_stale"))),
        );
    }
    out = out.children(
        groups
            .into_iter()
            .enumerate()
            .map(|(i, group)| dupe_group(group, i == last && hidden == 0))
            .collect::<Vec<_>>(),
    );
    if hidden > 0 {
        out = out.child(view().flex_row().pt(px(6.)).child(widgets::more_chip(
            t!("disk.ana_more", count = hidden).to_string(),
            false,
            || store::update(|state, fx| state.show_more_dupes(fx)),
        )));
    }
    out.into_element()
}

/// One group: a name for it, how many and how big, what moving all but
/// one would free — then the copies themselves.
fn dupe_group(group: Group, last: bool) -> AnyElement {
    let Group {
        name,
        meta,
        frees,
        frees_tip,
        copies,
        hidden,
    } = group;
    let mut out = view()
        .py(px(8.))
        .border_b(px(if last { 0. } else { 1. }))
        .border_color(theme::color(palette::border_subtle))
        .child(
            view()
                .flex_row()
                .items_baseline()
                .gap(px(8.))
                .child(
                    text(name)
                        .flex_1()
                        .min_w(px(0.))
                        .number_of_lines(1u32)
                        .text_size(px(ROW_PT))
                        .line_height(widgets::line(ROW_PT))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::color(palette::text)),
                )
                .child(
                    text(meta)
                        .flex_none()
                        .text_size(px(META_PT))
                        .line_height(widgets::line(META_PT))
                        .text_color(theme::color(palette::text_dim)),
                )
                .child(
                    view()
                        .flex_none()
                        .tooltip(widgets::wrap_tooltip(frees_tip))
                        .child(frees_label(frees)),
                ),
        )
        .children(copies.into_iter().map(dupe_copy).collect::<Vec<_>>());
    if hidden > 0 {
        out = out.child(
            text(t!("disk.dup_more_copies", n = hidden).to_string())
                .pl(px(INDENT_STEP))
                .pt(px(3.))
                .text_size(px(META_PT))
                .line_height(widgets::line(META_PT))
                .text_color(theme::color(palette::text_dim)),
        );
    }
    out.into_element()
}

/// One copy: where it is, what it is, when it last changed, Reveal and
/// Trash — never the last copy, and the store checks that against the
/// disk again before it moves anything (`DupeGroup::spare`).
fn dupe_copy(copy: DupeCopy) -> AnyElement {
    let DupeCopy {
        path,
        shown,
        oldest,
        clone,
        linked,
        modified,
        body,
    } = copy;
    let mut row = view()
        .group()
        .flex_row()
        .items_center()
        .gap(px(ROW_GAP))
        .pl(px(INDENT_STEP))
        .pt(px(4.))
        .child(path_label(&shown, shown.clone(), ROW_PT));
    if oldest {
        row = row.child(row_pill(
            i18n::tr("disk.dup_oldest"),
            i18n::tr("disk.dup_oldest_tip"),
            palette::text_muted,
        ));
    }
    if clone {
        row = row.child(row_pill(
            i18n::tr("disk.dup_clone"),
            i18n::tr("disk.dup_clone_tip"),
            palette::text_muted,
        ));
    }
    if linked {
        row = row.child(row_pill(
            i18n::tr("disk.dup_linked"),
            i18n::tr("disk.dup_linked_tip"),
            palette::text_muted,
        ));
    }
    if let Some(day) = modified {
        row = row.child(modified_label(day));
    }
    let trash_path = path.clone();
    row.child(action_slots(
        Some(reveal_button(path, storagepage::reveal_tip())),
        Some(widgets::ghost_icon(
            CustomIconName::Trash,
            widgets::wrap_tooltip(i18n::tr("disk.dup_trash")),
            move || {
                let path = trash_path.clone();
                widgets::confirm(
                    i18n::tr("disk.dup_trash_title"),
                    body.clone(),
                    i18n::tr("disk.big_trash_ok"),
                    move || store::update(|state, fx| state.trash_dupe(&path, fx)),
                );
            },
        )),
    ))
    .into_element()
}

// ---- the analysis ---------------------------------------------------------

/// The chevron's slot, held on every analysis row.
const CHEVRON_SLOT: f32 = 12.;

/// The delta column's width: `-999.9 MB` at the meta size in mono.
const DELTA_W: f32 = 56.;

/// One indent step's width, for a row `depth` levels in.
fn indent(depth: usize) -> f32 {
    depth as f32 * INDENT_STEP
}

/// The ink at the share a ranking's bars keep: context for the figures,
/// not the loudest thing in the window.
fn meter_ink() -> color::Rgba {
    let ink = palette::ink();
    color::Rgba {
        a: ink.a * 0.45,
        ..ink
    }
}

/// A row's share bar: 2px, the dimmed ink, from under the label to under
/// the size.
fn row_meter(fraction: f32) -> AnyElement {
    view()
        .mt(px(3.))
        .ml(px(CHEVRON_SLOT + ROW_GAP))
        .mr(px(ACTION_SLOT * 2. + ROW_GAP))
        .child(widgets::meter(move || fraction, meter_ink, 2.))
        .into_element()
}

/// The analysis card's head and notes as they stand.
#[derive(Clone, PartialEq)]
struct AnaHead {
    caption: Option<(String, String)>,
    ready: bool,
    failed: bool,
    running: bool,
    progress: Option<f32>,
    skips: Option<(String, Vec<String>)>,
    run: String,
    stale: bool,
    presets: Vec<(Preset, bool)>,
    custom: Option<String>,
    exclude: String,
    exclude_open: bool,
    excludes: Vec<(String, bool)>,
    watch: bool,
    watch_status: String,
    fda: Option<String>,
    mismatch: Option<String>,
    phase: Phase,
}

/// What the card's body is.
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// Before the first walk: the empty state.
    Off,
    Failed,
    /// Walking, nothing to show yet.
    Waiting,
    /// A result: finished (rows open, actions) or a partial, dimmed when
    /// it is not the selected scope's.
    Result {
        finished: bool,
        dim: bool,
    },
}

fn ana_head(state: &ZStatsAppState) -> AnaHead {
    let mismatch = storagepage::mismatch_note(state);
    let phase = match state.disk_analysis() {
        DiskAnalysis::Off => Phase::Off,
        DiskAnalysis::Failed(_) => Phase::Failed,
        DiskAnalysis::Running { partial: None, .. } => Phase::Waiting,
        DiskAnalysis::Running { .. } => Phase::Result {
            finished: false,
            dim: false,
        },
        DiskAnalysis::Ready(_) => Phase::Result {
            finished: true,
            dim: mismatch.is_some(),
        },
    };
    AnaHead {
        caption: storagepage::caption(state),
        ready: matches!(state.disk_analysis(), DiskAnalysis::Ready(_)),
        failed: matches!(state.disk_analysis(), DiskAnalysis::Failed(_)),
        running: matches!(state.disk_analysis(), DiskAnalysis::Running { .. }),
        progress: storagepage::progress(state),
        skips: match state.disk_analysis() {
            DiskAnalysis::Ready(result) => storagepage::skips(result),
            _ => None,
        },
        run: storagepage::run_label(state),
        stale: storagepage::stale(state),
        presets: Preset::ALL.into_iter().map(|p| (p, p.on(state))).collect(),
        custom: storagepage::custom_scope(state),
        exclude: storagepage::exclude_label(),
        exclude_open: state.analysis_exclude_open(),
        excludes: storagepage::exclude_entries(),
        watch: prefs::disk_watch(),
        watch_status: storagepage::watch_status(state),
        fda: storagepage::fda_note(state),
        mismatch,
        phase,
    }
}

/// The analyser's card: what the shown result is and its run control, one
/// toolbar line of scope and settings (the exclusions as a drawer under
/// it), the notes about the result, then the result — beside the map
/// when the window is wide, under it otherwise. The frame is made again
/// only when what it holds changes shape (before the first walk, failed,
/// walking with nothing yet, a result; wide or not): each part binds its
/// own piece of the head, whose ages move every minute, and a frame made
/// again with them would throw the lists back to their top.
fn analysis_card(
    live: Live,
    wide: Memo<bool>,
    sheet: Signal<Option<Sheet>>,
    map_size: Signal<(f32, f32)>,
) -> AnyElement {
    let head = live.memo(ana_head);
    let lists = live.memo(lists_of);
    let shape = computed(move || {
        (
            head.with(|h| match h.phase {
                Phase::Result { .. } => Phase::Result {
                    finished: false,
                    dim: false,
                },
                phase => phase,
            }),
            wide.get(),
        )
    });
    let dim = computed(move || head.with(|h| matches!(h.phase, Phase::Result { dim: true, .. })));
    let drawer =
        computed(move || head.with(|h| (h.exclude_open && !h.running).then(|| h.excludes.clone())));
    let notes = computed(move || head.with(|h| (h.fda.clone(), h.mismatch.clone())));
    dynamic(move || {
        let (phase, wide) = shape.get();
        let parts: Vec<AnyElement> = vec![
            dynamic(move || {
                head.with(analysis_header)
                    .unwrap_or_else(|| view().into_element())
            })
            .into_element(),
            dynamic(move || head.with(analysis_toolbar)).into_element(),
            dynamic(move || match drawer.get() {
                Some(entries) => exclude_drawer(&entries),
                None => view().into_element(),
            })
            .into_element(),
        ];
        let notes = move || {
            dynamic(move || {
                let (fda, mismatch) = notes.get();
                view()
                    .children(analysis_notes(fda, mismatch))
                    .into_element()
            })
            .into_element()
        };
        let faded = move || if dim.get() { 0.5_f32 } else { 1. };
        match phase {
            Phase::Off => tab_card(
                parts,
                empty_state(
                    i18n::tr("disk.ana_empty_lead"),
                    i18n::tr("disk.ana_empty_cost"),
                    i18n::tr("disk.ana_hint"),
                    primary_button(
                        t!(
                            "disk.ana_scan_scope",
                            scope = store::read(storagepage::selected_word)
                        )
                        .to_string(),
                        || store::update(|state, fx| state.start_disk_analysis(fx)),
                    ),
                ),
            ),
            Phase::Failed => tab_card(
                parts,
                view()
                    .child(notes())
                    .child(
                        view()
                            .px(px(INSET))
                            .pb(px(12.))
                            .child(widgets::note(i18n::tr("disk.ana_failed_body"))),
                    )
                    .into_element(),
            ),
            Phase::Waiting => tab_card(parts, notes()),
            Phase::Result { .. } => {
                let lists = view()
                    .opacity(faded)
                    .child(dynamic(move || analysis_lists(lists.get(), sheet)))
                    .into_element();
                let map = view()
                    .opacity(faded)
                    .px(px(INSET))
                    .pb(px(INSET))
                    .child(dir_map(live, wide, map_size));
                if !wide {
                    return tab_card(
                        parts,
                        view()
                            .child(notes())
                            .child(map.into_element())
                            .child(lists)
                            .into_element(),
                    );
                }
                widgets::list_shell()
                    .flex_1()
                    .min_h(px(0.))
                    .pt(px(4.))
                    .children(
                        parts
                            .into_iter()
                            .map(|part| view().flex_none().child(part).into_element())
                            .collect::<Vec<_>>(),
                    )
                    .child(
                        view()
                            .flex_row()
                            .flex_1()
                            .min_h(px(0.))
                            .child(
                                view()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(map.flex_1().min_h(px(0.))),
                            )
                            .child(
                                scroll()
                                    .flex_none()
                                    .w(px(SIDE_W))
                                    .min_h(px(0.))
                                    .child(view().child(notes()).child(lists)),
                            ),
                    )
                    .into_element()
            }
        }
    })
    .into_element()
}

/// The notes about the result on screen: the Full Disk Access gap, and a
/// result that is not the selected scope's.
fn analysis_notes(fda: Option<String>, mismatch: Option<String>) -> Vec<AnyElement> {
    let mut notes = Vec::new();
    if let Some(fda) = fda {
        notes.push(
            view()
                .px(px(INSET))
                .pb(px(8.))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .mt(px(4.))
                        .child(view().flex_1().min_w(px(0.)).child(widgets::note(fda)))
                        .child(small_button(
                            i18n::tr("disk.ana_fda_open"),
                            Some(CustomIconName::Shield),
                            opener::full_disk_access_settings,
                        )),
                )
                .into_element(),
        );
    }
    if let Some(line) = mismatch {
        notes.push(
            view()
                .px(px(INSET))
                .pb(px(8.))
                .child(widgets::note(line))
                .into_element(),
        );
    }
    notes
}

/// The caption and the run control; nothing before the first walk.
fn analysis_header(h: &AnaHead) -> Option<AnyElement> {
    let (scope, rest) = h.caption.clone()?;
    let mut controls = view()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .child(run_chip_with(
            h.run.clone(),
            h.stale,
            Some(storagepage::run_tip(h.stale)),
            || store::update(|state, fx| storagepage::run_or_cancel(state, fx)),
        ));
    if h.ready {
        // Drops a result that took minutes, after a confirm.
        controls = controls.child(widgets::ghost_icon(
            CustomIconName::Close,
            widgets::wrap_tooltip(i18n::tr("disk.ana_dismiss_hint")),
            || {
                widgets::confirm(
                    i18n::tr("disk.ana_clear_title"),
                    i18n::tr("disk.ana_clear_body"),
                    i18n::tr("disk.ana_clear_ok"),
                    || store::update(|state, fx| state.clear_disk_analysis(fx)),
                )
            },
        ));
    }
    if h.failed {
        // A failed walk has nothing to lose: no confirm.
        controls = controls.child(widgets::ghost_icon(
            CustomIconName::Close,
            widgets::wrap_tooltip(i18n::tr("disk.ana_dismiss_hint")),
            || store::update(|state, fx| state.clear_disk_analysis(fx)),
        ));
    }
    let skips = h.skips.clone().map(|(label, lines)| {
        view()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(3.))
            .tooltip(widgets::tooltip_lines(move || lines.clone()))
            .child(
                text(format!("· {label}"))
                    .text_size(px(META_PT))
                    .line_height(px(14.))
                    .text_color(theme::color(palette::text_dim)),
            )
            .child(widgets::icon(CustomIconName::Info, 10., palette::text_dim))
            .into_element()
    });
    let mut out = view().child(header_rows(
        scope,
        caption_with(rest, None, skips),
        controls.into_element(),
    ));
    if let Some(fraction) = h.progress {
        // Against the last walk of this scope: an estimate, never quite
        // full until the walk is.
        out = out.child(view().px(px(INSET)).pb(px(8.)).child(widgets::meter(
            move || fraction.min(0.97),
            meter_ink,
            2.,
        )));
    }
    Some(out.into_element())
}

/// Scope, exclusions and the daily check, as one line of chips — dimmed
/// and inert while a walk runs.
fn analysis_toolbar(h: &AnaHead) -> AnyElement {
    let running = h.running;
    let press = |act: Box<dyn Fn()>| (!running).then_some(act);
    let mut row = view()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(px(4.))
        .px(px(INSET))
        .pb(px(8.))
        .opacity(if running { 0.45 } else { 1. });
    for (preset, on) in h.presets.iter().copied() {
        row = row.child(toolbar_chip(
            chip_words(preset.label(), on).into_element(),
            on,
            Some(widgets::wrap_tooltip(preset.tip())),
            press(Box::new(move || {
                store::update(|state, fx| preset.select(state, fx))
            })),
        ));
    }
    if let Some(custom) = h.custom.clone() {
        row = row.child(toolbar_chip(
            view()
                .max_w(px(140.))
                .child(chip_words(custom, true))
                .into_element(),
            true,
            None,
            None,
        ));
    }
    row = row.child(widgets::ghost_icon(
        CustomIconName::FolderOpen,
        widgets::wrap_tooltip(i18n::tr("disk.ana_pick_hint")),
        move || {
            if !running {
                root::ask(HostAction::PickFolder(FolderPick::Analysis));
            }
        },
    ));
    row = row.child(
        view()
            .flex_none()
            .w(px(1.))
            .h(px(12.))
            .mx(px(4.))
            .bg(theme::color(palette::border)),
    );
    row = row.child(toolbar_chip(
        chip_words(h.exclude.clone(), h.exclude_open).into_element(),
        h.exclude_open,
        Some(widgets::wrap_tooltip(i18n::tr("disk.ana_exclude_note"))),
        press(Box::new(|| {
            store::update(|state, fx| state.toggle_analysis_exclude(fx))
        })),
    ));
    // The daily check: a dot for on or off, its state on hover.
    let on = h.watch;
    let status = h.watch_status.clone();
    row.child(toolbar_chip(
        view()
            .flex_row()
            .items_center()
            .gap(px(5.))
            .child(
                view()
                    .flex_none()
                    .w(px(6.))
                    .h(px(6.))
                    .rounded_full()
                    .border(px(if on { 0. } else { 1. }))
                    .border_color(theme::color(palette::text_dim))
                    .bg(theme::color(
                        move || if on { palette::text_muted() } else { CLEAR },
                    )),
            )
            .child(chip_words(i18n::tr("disk.watch"), false))
            .into_element(),
        false,
        Some(widgets::tooltip_lines(move || {
            vec![status.clone(), i18n::tr("disk.watch_tip")]
        })),
        press(Box::new(move || {
            prefs::set_disk_watch(!on);
            store::changed();
        })),
    ))
    .into_element()
}

/// Directories the walk leaves alone: a chip each, the field to add one
/// (committed on Return, never on a keystroke), and the folder panel.
fn exclude_drawer(entries: &[(String, bool)]) -> AnyElement {
    let typed = signal(String::new());
    let chips: Vec<AnyElement> = entries
        .iter()
        .map(|(raw, present)| {
            let present = *present;
            let drop_me = raw.clone();
            let mut chip = view()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(3.))
                .rounded(px(4.))
                .bg(theme::color(palette::chip))
                .px(px(5.))
                .py(px(1.))
                .child(
                    text(raw.clone())
                        .text_size(px(META_PT + 1.))
                        .line_height(widgets::line(META_PT + 1.))
                        .text_color(theme::color(move || {
                            if present {
                                palette::text()
                            } else {
                                palette::text_dim()
                            }
                        })),
                )
                .child(
                    view()
                        .flex_none()
                        .tooltip(widgets::wrap_tooltip(i18n::tr("disk.ana_exclude_drop")))
                        .child(
                            nsh_base::button()
                                .on_press(move || {
                                    storagepage::drop_exclude(&drop_me);
                                    store::changed();
                                })
                                .child(widgets::icon(CustomIconName::Close, 9., palette::text_dim)),
                        ),
                );
            if !present {
                chip = chip.tooltip(widgets::wrap_tooltip(i18n::tr("disk.ana_exclude_missing")));
            }
            chip.into_element()
        })
        .collect();
    view()
        .px(px(INSET))
        .pb(px(8.))
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap(px(4.))
                .p(px(6.))
                .rounded(px(7.))
                .bg(theme::color(palette::inset))
                .children(chips)
                .child(
                    // Small and fixed: a field used once a month must not
                    // out-shout the table.
                    view().flex_none().w(px(180.)).child(
                        input()
                            .h(px(20.))
                            .px(px(4.))
                            .rounded(px(6.))
                            .border(px(1.))
                            .border_color(theme::color(palette::border))
                            .value(move || typed.get())
                            .placeholder(i18n::tr("disk.ana_exclude_placeholder"))
                            .auto_correct(false)
                            .text_size(px(12.))
                            .text_color(theme::color(palette::text))
                            .on_change(move |text: &str| typed.set(text.to_owned()))
                            .on_submit(move |text: &str| {
                                if storagepage::add_exclude([text.to_owned()]) {
                                    // A compose box, not a value: what was
                                    // added is a chip now.
                                    typed.set(String::new());
                                    store::changed();
                                }
                            }),
                    ),
                )
                .child(widgets::ghost_icon(
                    CustomIconName::FolderOpen,
                    widgets::wrap_tooltip(i18n::tr("disk.ana_exclude_pick")),
                    || root::ask(HostAction::PickFolder(FolderPick::Exclude)),
                )),
        )
        .into_element()
}

/// One line of an analysis table: a row, or the dim line an open row
/// shows in place of (or after) its children.
#[derive(Clone, PartialEq)]
enum Line {
    Row(Box<Row>),
    Note { depth: usize, text: String },
}

/// One ranked row as it shows.
#[derive(Clone, PartialEq)]
struct Row {
    path: PathBuf,
    label: String,
    tip: String,
    bytes: u64,
    fraction: f32,
    /// The ± column, when the table has a baseline: what it says, if
    /// anything.
    delta: Option<Option<String>>,
    /// The basis pill (outside the suggestions), as `(word, reason)`.
    basis: Option<(String, String)>,
    /// The owner of a cache whose app is running.
    running: Option<String>,
    /// What the system declares, for a MobileAsset row.
    asset: Option<String>,
    modified: Option<String>,
    depth: usize,
    expandable: bool,
    open: bool,
    /// The trash confirm's sentence, where the row may go to the Trash.
    trash: Option<String>,
    /// A big file rather than a directory.
    file: bool,
    reveal_tip: String,
}

/// The growth since about a week ago.
#[derive(Clone, PartialEq)]
struct GrowthRow {
    path: PathBuf,
    shown: String,
    grew: u64,
    now: u64,
    fraction: f32,
}

/// The analysis's tables as they stand.
#[derive(Clone, PartialEq, Default)]
struct Lists {
    growth: Option<(String, Vec<GrowthRow>)>,
    sugs: Vec<Line>,
    sug_count: usize,
    sug_total: u64,
    /// The bulk sheet's rows and paths: the whole set.
    sug_picks: Vec<(String, u64, Option<String>)>,
    sug_paths: Vec<PathBuf>,
    sug_more: Option<String>,
    files: Vec<Line>,
    finished: bool,
}

fn lists_of(state: &ZStatsAppState) -> Lists {
    let (result, finished) = match state.disk_analysis() {
        DiskAnalysis::Ready(result) => (result, true),
        DiskAnalysis::Running {
            partial: Some(result),
            ..
        } => (result, false),
        _ => return Lists::default(),
    };
    let diff = finished.then(|| state.analysis_diff_for(result)).flatten();
    let running = active::running_bundle_ids();
    let home = env::var("HOME").unwrap_or_default();
    let growth = state
        .disk_growth()
        .filter(|_| finished && storagepage::home_result(result))
        .filter(|report| !report.rows.is_empty())
        .map(|report| {
            let max = report.rows.iter().map(|g| g.grew).max().unwrap_or(1).max(1);
            (
                storagepage::growth_title(report),
                report
                    .rows
                    .iter()
                    .map(|g| GrowthRow {
                        path: g.path.clone(),
                        shown: format::tilde_path(&g.path.display().to_string(), &home),
                        grew: g.grew,
                        now: g.now,
                        fraction: g.grew as f32 / max as f32,
                    })
                    .collect(),
            )
        });
    let sugs = storagepage::suggestions(result, state.analysis_show_all_sugs());
    let ctx = Ctx {
        state,
        diff,
        running: &running,
        suggested: true,
        deletable: finished,
        expandable: finished,
    };
    let sug_max = sugs.rows.iter().map(|h| h.bytes).max().unwrap_or(1).max(1);
    let mut sug_lines = Vec::new();
    for hit in &sugs.rows[..sugs.shown] {
        dir_lines(&ctx, hit, &result.root, sug_max, 0, &mut sug_lines);
    }
    let file_max = result
        .files
        .iter()
        .map(|h| h.bytes)
        .max()
        .unwrap_or(1)
        .max(1);
    let files = result
        .files
        .iter()
        .map(|hit| {
            let label = storagepage::row_label(&hit.path, &result.root);
            Line::Row(Box::new(Row {
                tip: storagepage::row_tip(&hit.path, None, false, None, None),
                trash: finished.then(|| storagepage::file_trash_body(&label)),
                label,
                path: hit.path.clone(),
                bytes: hit.bytes,
                fraction: hit.bytes as f32 / file_max as f32,
                delta: diff.map(|d| storagepage::delta_label(hit.bytes, d.bytes_for(&hit.path))),
                basis: None,
                running: None,
                asset: None,
                modified: hit.modified.map(format::date),
                depth: 0,
                expandable: false,
                open: false,
                file: true,
                reveal_tip: storagepage::reveal_row_tip(&hit.path),
            }))
        })
        .collect();
    Lists {
        growth,
        sug_more: (sugs.rows.len() > storagepage::SUG_ROWS).then(|| {
            storagepage::more_label(
                state.analysis_show_all_sugs(),
                sugs.rows.len() - storagepage::SUG_ROWS,
            )
        }),
        sug_count: sugs.rows.len(),
        sug_total: sugs.total,
        sug_picks: storagepage::pick_items(&sugs.rows, &running),
        sug_paths: sugs.rows.iter().map(|h| h.path.clone()).collect(),
        sugs: sug_lines,
        files,
        finished,
    }
}

/// What every row of one table shares.
struct Ctx<'a> {
    state: &'a ZStatsAppState,
    diff: Option<&'a diskscan::DiffBaseline>,
    running: &'a [String],
    suggested: bool,
    deletable: bool,
    expandable: bool,
}

/// One directory row and, when it is open, its children under it.
fn dir_lines(
    ctx: &Ctx,
    hit: &DirHit,
    parent: &Path,
    group_max: u64,
    depth: usize,
    out: &mut Vec<Line>,
) {
    let open = ctx.expandable && ctx.state.expansion(&hit.path).is_some();
    let hint = cleanhints::lookup(&hit.path);
    let owner_running = hint
        .as_ref()
        .filter(|hint| hint.running_in(ctx.running))
        .map(|hint| hint.owner.clone());
    let label = storagepage::row_label(&hit.path, parent);
    out.push(Line::Row(Box::new(Row {
        tip: storagepage::row_tip(
            &hit.path,
            Some(hit.kind),
            ctx.suggested,
            hint.as_ref(),
            hit.asset.as_ref(),
        ),
        basis: (!ctx.suggested)
            .then(|| storagepage::basis(Some(hit.kind), hint.as_ref()))
            .flatten(),
        trash: ctx
            .deletable
            .then(|| storagepage::dir_trash_body(&label, hit.bytes, owner_running.as_deref())),
        running: owner_running,
        asset: hit.asset.as_ref().map(storagepage::asset_clause),
        label,
        path: hit.path.clone(),
        bytes: hit.bytes,
        fraction: hit.bytes as f32 / group_max as f32,
        delta: ctx
            .diff
            .map(|d| storagepage::delta_label(hit.bytes, d.bytes_for(&hit.path))),
        modified: None,
        depth,
        expandable: ctx.expandable,
        open,
        file: false,
        reveal_tip: storagepage::reveal_row_tip(&hit.path),
    })));
    if !open {
        return;
    }
    let note = |text: String| Line::Note {
        depth: depth + 1,
        text,
    };
    match ctx.state.expansion(&hit.path) {
        // Children are measured against the row they open from.
        Some(Expansion::Ready(rows)) if !rows.is_empty() => {
            let max = hit.bytes.max(1);
            for child in rows.iter().take(storagepage::expand_shown(rows)) {
                dir_lines(ctx, child, &hit.path, max, depth + 1, out);
            }
            if let Some((hidden, bytes)) = storagepage::expand_hidden(rows) {
                out.push(note(storagepage::expand_rest(hidden, bytes)));
            }
        }
        Some(Expansion::Ready(_)) => out.push(note(i18n::tr("disk.ana_expand_empty"))),
        Some(Expansion::Walking) => out.push(note(i18n::tr("disk.ana_expanding"))),
        Some(Expansion::Failed) => out.push(note(i18n::tr("disk.ana_expand_failed"))),
        None => {}
    }
}

/// The growth, the suggestions and the big files, then the closing note.
fn analysis_lists(lists: Lists, sheet: Signal<Option<Sheet>>) -> AnyElement {
    let Lists {
        growth,
        sugs,
        sug_count,
        sug_total,
        sug_picks,
        sug_paths,
        sug_more,
        files,
        finished,
    } = lists;
    let mut out = view();
    if let Some((title, rows)) = growth {
        out = out.child(growth_section(title, rows));
    }
    if !sugs.is_empty() {
        let mut right = view()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .child(size_text(sug_total));
        if finished {
            right = right.child(small_button(
                i18n::tr("disk.sug_clear"),
                Some(CustomIconName::Trash),
                move || {
                    sheet.set(Some(Sheet {
                        title: i18n::tr("disk.sug_clear_title"),
                        body: i18n::tr("disk.sug_pick_body"),
                        ticks: sug_picks.iter().map(|(_, _, c)| c.is_none()).collect(),
                        items: sug_picks.clone(),
                        paths: sug_paths.clone(),
                    }))
                },
            ));
        }
        let mut section = view()
            .px(px(INSET))
            .pb(px(6.))
            .child(section_heading(
                i18n::tr("disk.sug_title"),
                Some(t!("disk.sug_meta", n = sug_count).to_string()),
                Some(right.into_element()),
            ))
            .children(sugs.into_iter().map(line).collect::<Vec<_>>());
        if let Some(more) = sug_more {
            let open = more == i18n::tr("disk.ana_less");
            section = section.child(view().flex_row().pt(px(6.)).child(widgets::more_chip(
                more,
                open,
                || store::update(|state, fx| state.toggle_analysis_show_all_sugs(fx)),
            )));
        }
        out = out.child(section);
    }
    if !files.is_empty() {
        out = out.child(
            view()
                .px(px(INSET))
                .pb(px(6.))
                .child(section_heading(
                    i18n::tr("disk.ana_files"),
                    Some(
                        t!(
                            "disk.ana_files_meta",
                            bar = format::memory(diskscan::BIG_FILE_IN_TREE)
                        )
                        .to_string(),
                    ),
                    None,
                ))
                .children(files.into_iter().map(line).collect::<Vec<_>>()),
        );
    }
    out.child(
        view()
            .px(px(INSET))
            .pb(px(10.))
            .child(widgets::note(i18n::tr("disk.ana_note"))),
    )
    .into_element()
}

/// What grew in the home tree since about a week ago: read-only rows,
/// a reveal each.
fn growth_section(title: String, rows: Vec<GrowthRow>) -> AnyElement {
    view()
        .px(px(INSET))
        .pb(px(6.))
        .child(
            view()
                .tooltip(widgets::wrap_tooltip(i18n::tr("disk.growth_tip")))
                .child(section_heading(title, None, None)),
        )
        .children(
            rows.into_iter()
                .map(|row| {
                    view()
                        .group()
                        .py(px(5.))
                        .child(
                            view()
                                .flex_row()
                                .items_center()
                                .gap(px(ROW_GAP))
                                .child(view().flex_none().w(px(CHEVRON_SLOT)))
                                .child(path_label(&row.shown, row.shown.clone(), ROW_PT))
                                .child(
                                    text(format!("+{}", format::memory(row.grew)))
                                        .flex_none()
                                        .font_family(widgets::MONO)
                                        .text_size(px(META_PT))
                                        .line_height(widgets::line(META_PT))
                                        .text_color(theme::color(palette::text_muted)),
                                )
                                .child(size_text(row.now))
                                .child(action_slots(
                                    Some(reveal_button(row.path, storagepage::reveal_tip())),
                                    None,
                                )),
                        )
                        .child(row_meter(row.fraction))
                        .into_element()
                })
                .collect::<Vec<_>>(),
        )
        .into_element()
}

fn line(line: Line) -> AnyElement {
    match line {
        Line::Row(row) => analysis_row(*row),
        Line::Note { depth, text: words } => text(words)
            .pl(px(4. + indent(depth)))
            .py(px(3.))
            .text_size(px(10.))
            .line_height(widgets::line(10.))
            .text_color(theme::color(palette::text_dim))
            .into_element(),
    }
}

/// One ranked row: its path from the row above it, its pills, its size
/// and share bar, Reveal and — where it may go — Trash; a row that opens
/// does so in place, on a click anywhere on it.
fn analysis_row(row: Row) -> AnyElement {
    let Row {
        path,
        label,
        tip,
        bytes,
        fraction,
        delta,
        basis,
        running,
        asset,
        modified,
        depth,
        expandable,
        open,
        trash,
        file,
        reveal_tip,
    } = row;
    let mut parts = view()
        .flex_row()
        .items_center()
        .gap(px(ROW_GAP))
        .child({
            let slot = view().flex_row().flex_none().w(px(CHEVRON_SLOT));
            if expandable {
                slot.child(widgets::icon(
                    if open {
                        CustomIconName::ChevronDown
                    } else {
                        CustomIconName::ChevronRight
                    },
                    11.,
                    palette::text_dim,
                ))
            } else {
                slot
            }
        })
        .child(path_label(&label, tip, ROW_PT));
    if let Some((word, reason)) = basis {
        parts = parts.child(row_pill(word, reason, palette::text_muted));
    }
    if let Some(owner) = running {
        parts = parts.child(row_pill(
            i18n::tr("disk.running"),
            storagepage::running_tip(&owner),
            palette::text,
        ));
    }
    if let Some(clause) = asset {
        parts = parts.child(row_pill(
            i18n::tr("disk.asset_pill"),
            clause,
            palette::text_muted,
        ));
    }
    if let Some(day) = modified {
        parts = parts.child(modified_label(day));
    }
    if let Some(delta) = delta {
        let mut slot = view()
            .flex_row()
            .flex_none()
            .w(px(DELTA_W))
            .justify_end()
            .tooltip(widgets::live_tooltip(move || {
                store::read(|state| match state.disk_analysis() {
                    DiskAnalysis::Ready(result) => state
                        .analysis_diff_for(result)
                        .map(storagepage::delta_tip)
                        .unwrap_or_default(),
                    _ => String::new(),
                })
            }));
        if let Some(delta) = delta {
            slot = slot.child(
                text(delta)
                    .font_family(widgets::MONO)
                    .text_size(px(META_PT))
                    .line_height(widgets::line(META_PT))
                    .text_color(theme::color(palette::text_muted)),
            );
        }
        parts = parts.child(slot);
    }
    let is_trash = storagepage::is_trash_dir(&path);
    let reveal_path = path.clone();
    let reveal = widgets::ghost_icon(
        CustomIconName::Folder,
        widgets::wrap_tooltip(reveal_tip),
        move || {
            if is_trash {
                finder::open_trash();
            } else {
                finder::reveal(&reveal_path);
            }
        },
    );
    let trash = trash.map(|body| {
        let path = path.clone();
        widgets::ghost_icon(
            CustomIconName::Trash,
            widgets::wrap_tooltip(i18n::tr("disk.big_trash")),
            move || {
                let path = path.clone();
                widgets::confirm(
                    i18n::tr("disk.big_trash_title"),
                    body.clone(),
                    i18n::tr("disk.big_trash_ok"),
                    move || {
                        if file {
                            store::update(|state, fx| state.trash_analysis_file(&path, fx));
                        } else {
                            let paths = vec![path.clone()];
                            store::update(|state, fx| state.trash_regenerable(&paths, fx));
                        }
                    },
                );
            },
        )
    });
    parts = parts
        .child(size_text(bytes))
        .child(action_slots(Some(reveal), trash));
    let body = view().child(parts).child(row_meter(fraction));
    let row = nsh_base::button()
        .py(px(4.))
        .px(px(4.))
        .mx(px(-4.))
        .pl(px(4. + indent(depth)))
        .rounded(px(5.));
    let row = if expandable {
        row.hover(|s| s.bg(theme::color(palette::surface_raised)))
            .on_press(move || {
                let path = path.clone();
                store::update(|state, fx| state.toggle_expansion(path, fx));
            })
    } else {
        row
    };
    view().group().child(row.child(body)).into_element()
}

// ---- the bulk trash sheet ---------------------------------------------

/// The bulk sheet (`confirm::ask_pick`): every row it would move with its
/// size and a tick, a running app's cache unticked to start with.
#[derive(Clone, PartialEq)]
struct Sheet {
    title: String,
    body: String,
    items: Vec<(String, u64, Option<String>)>,
    ticks: Vec<bool>,
    paths: Vec<PathBuf>,
}

/// Rows this tall before the list scrolls.
const PICK_LIST_MAX_H: f32 = 240.;

/// The sheet over the window, on the ground NSH's own confirm dims it
/// with; Escape and Cancel close it, the action moves what is ticked.
fn pick_sheet(sheet: Signal<Option<Sheet>>) -> AnyElement {
    let Some(start) = sheet.get_untracked() else {
        return view().into_element();
    };
    let ticks = signal(start.ticks.clone());
    let items = start.items.clone();
    let paths = start.paths.clone();
    let chosen =
        move || ticks.with(|t: &Vec<bool>| (0..t.len()).filter(|&i| t[i]).collect::<Vec<usize>>());
    let total = {
        let items = items.clone();
        move || chosen().iter().map(|&i| items[i].1).sum::<u64>()
    };
    let rows: Vec<AnyElement> = items
        .iter()
        .enumerate()
        .map(|(i, (label, bytes, caution))| {
            let on = move || ticks.with(|t: &Vec<bool>| t[i]);
            let mut row = view().py(px(3.)).child(
                nsh_base::button()
                    .on_press(move || ticks.update(|t: &mut Vec<bool>| t[i] = !t[i]))
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .child(tick(on))
                            .child(
                                text(label.clone())
                                    .flex_1()
                                    .min_w(px(0.))
                                    .number_of_lines(1u32)
                                    .text_size(px(11.))
                                    .line_height(widgets::line(11.))
                                    .text_color(theme::color(palette::text)),
                            )
                            .child(
                                text(format::memory(*bytes))
                                    .flex_none()
                                    .font_family(widgets::MONO)
                                    .text_size(px(10.5))
                                    .line_height(widgets::line(10.5))
                                    .text_color(theme::color(palette::text_muted)),
                            ),
                    ),
            );
            if let Some(caution) = caution.clone() {
                row = row.child(
                    text(caution)
                        .pl(px(20.))
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .text_color(theme::color(|| palette::tiny_label(palette::text_muted()))),
                );
            }
            row.into_element()
        })
        .collect();
    let close = move || sheet.set(None);
    let ok_label = {
        let total = total.clone();
        move || storagepage::pick_ok(chosen().len(), total())
    };
    let none = move || chosen().is_empty();
    overlay()
        .absolute()
        .inset_0()
        .on_cancel(close)
        .child(
            view()
                .size_full()
                .items_center()
                .justify_center()
                .bg(theme::color(|| color::Rgba {
                    r: 0.,
                    g: 0.,
                    b: 0.,
                    a: 0.35,
                }))
                .child(
                    view()
                        .w(px(360.))
                        .rounded(px(12.))
                        .bg(theme::color(palette::popover))
                        .p(px(16.))
                        .gap(px(8.))
                        .child(
                            text(start.title)
                                .text_size(px(13.))
                                .line_height(widgets::line(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::color(palette::text)),
                        )
                        .child(
                            text(start.body)
                                .text_size(px(11.))
                                .line_height(px(15.))
                                .text_color(theme::color(palette::text_muted)),
                        )
                        .child(
                            scroll()
                                .max_h(px(PICK_LIST_MAX_H))
                                .child(view().children(rows)),
                        )
                        .child(
                            view()
                                .flex_row()
                                .justify_end()
                                .gap(px(6.))
                                .pt(px(4.))
                                .child(small_button(i18n::tr("common.cancel"), None, close))
                                .child(
                                    nsh_base::button()
                                        .flex_none()
                                        .h(px(20.))
                                        .px(px(6.))
                                        .rounded(px(6.))
                                        .bg(theme::color(|| palette::accent_wash(16)))
                                        .items_center()
                                        .justify_center()
                                        .hover(|s| s.bg(theme::color(|| palette::accent_wash(24))))
                                        .on_press(move || {
                                            let picked: Vec<PathBuf> = chosen()
                                                .iter()
                                                .map(|&i| paths[i].clone())
                                                .collect();
                                            if picked.is_empty() {
                                                return;
                                            }
                                            sheet.set(None);
                                            store::update(|state, fx| {
                                                state.trash_regenerable(&picked, fx)
                                            });
                                        })
                                        .child(
                                            text(ok_label)
                                                .text_size(px(12.))
                                                .line_height(px(15.))
                                                .text_color(move || {
                                                    theme::color(move || {
                                                        if none() {
                                                            palette::text_dim()
                                                        } else {
                                                            palette::accent_light()
                                                        }
                                                    })
                                                }),
                                        ),
                                ),
                        ),
                ),
        )
        .into_element()
}

/// gpui-kit's smallest checkbox: a 14 box, filled with the theme's
/// primary and its check while ticked.
fn tick(on: impl Fn() -> bool + Copy + 'static) -> AnyElement {
    view()
        .flex_none()
        .w(px(14.))
        .h(px(14.))
        .rounded(px(4.))
        .border(px(1.))
        .border_color(move || {
            theme::color(move || {
                if on() {
                    palette::primary()
                } else {
                    palette::border()
                }
            })
        })
        .bg(move || theme::color(move || if on() { palette::primary() } else { CLEAR }))
        .items_center()
        .justify_center()
        .child(when(on, || {
            widgets::icon(CustomIconName::Check, 10., palette::primary_ink)
        }))
        .into_element()
}

// ---- the directory map ----------------------------------------------------

/// The map as it stands for a box of its last measured size.
#[derive(Clone, PartialEq)]
struct MapView {
    crumbs: Vec<Crumb>,
    total: Option<u64>,
    path: PathBuf,
    /// The sentence (and a walk's button) in place of tiles.
    note: Option<(String, Option<String>)>,
    tiles: Vec<Tile>,
    interactive: bool,
}

fn map_of(state: &ZStatsAppState, (width, height): (f32, f32)) -> Option<MapView> {
    let (result, interactive) = match state.disk_analysis() {
        DiskAnalysis::Ready(result) => (result, true),
        DiskAnalysis::Running {
            partial: Some(result),
            ..
        } => (result, false),
        _ => return None,
    };
    let (path, total, kids) = dirmap::level(state, result);
    let note = dirmap::kids_note(&kids, &path);
    let tiles = match kids {
        Kids::Ready(kids) if width > 0. && height > 0. => dirmap::tiles(
            state,
            result,
            &path,
            total,
            kids,
            (width, height),
            interactive,
        ),
        _ => Vec::new(),
    };
    // Before the first layout there is no width to fold the path against.
    let room = if width > 0. {
        width - dirmap::CRUMB_ROW_RESERVE
    } else {
        f32::INFINITY
    };
    Some(MapView {
        crumbs: dirmap::crumbs(state, result, room),
        total,
        path,
        note,
        tiles,
        interactive,
    })
}

/// The directory map (`storage/dirmap.rs`): the path back up over a box of
/// tiles, filling the height it is given beside the lists or
/// [`NARROW_MAP_H`] above them. Tiles are laid out in points against the
/// box's size as last measured, so a resize lays them out again; the size
/// is the window root's (`size`), because a box being taken down still
/// reports a layout after the part that made it has gone.
fn dir_map(live: Live, fill: bool, size: Signal<(f32, f32)>) -> AnyElement {
    let map = computed(move || {
        live.track();
        let size = size.get();
        store::read(|state| map_of(state, size))
    });
    let column = view().gap(px(6.));
    let column = if fill {
        column.flex_1().min_h(px(0.))
    } else {
        column
    };
    let area = view()
        .relative()
        .overflow_hidden()
        .rounded(px(6.))
        .bg(theme::color(palette::inset))
        .on_layout(move |b: &Bounds<Points>| {
            let next = (b.size.width.0, b.size.height.0);
            if size.try_get() != Some(next) {
                let _ = size.try_set(next);
            }
        })
        .child(dynamic(move || map_body(map.get())));
    let area = if fill {
        area.flex_1().min_h(px(0.))
    } else {
        area.flex_none().h(px(NARROW_MAP_H))
    };
    column
        .child(dynamic(move || map_crumbs(map.get())))
        .child(area)
        .into_element()
}

/// The way down as a path to click back along, its size and its reveal on
/// the right.
fn map_crumbs(map: Option<MapView>) -> AnyElement {
    let Some(map) = map else {
        return view().into_element();
    };
    let interactive = map.interactive;
    let parts: Vec<AnyElement> = map
        .crumbs
        .into_iter()
        .map(|crumb| match crumb {
            Crumb::Separator => text("›")
                .flex_none()
                .text_size(px(ROW_PT))
                .line_height(widgets::line(ROW_PT))
                .text_color(theme::color(palette::text_dim))
                .into_element(),
            Crumb::Level {
                depth,
                label,
                tip,
                here,
            } => {
                let words = text(label)
                    .number_of_lines(1u32)
                    .text_size(px(ROW_PT))
                    .line_height(widgets::line(ROW_PT))
                    .font_weight(if here {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(theme::color(move || {
                        if here {
                            palette::text()
                        } else {
                            palette::text_muted()
                        }
                    }));
                let cap = if here {
                    dirmap::CRUMB_HERE_MAX_W
                } else {
                    dirmap::CRUMB_MAX_W
                };
                let crumb = nsh_base::button()
                    .min_w(px(0.))
                    .max_w(px(cap))
                    .px(px(dirmap::CRUMB_PAD_W / 2.))
                    .py(px(1.))
                    .rounded(px(4.));
                let crumb = if !here && interactive {
                    crumb
                        .hover(|s| s.bg(theme::color(palette::surface_raised)))
                        .on_press(move || store::update(|state, fx| state.map_back_to(depth, fx)))
                } else {
                    crumb
                };
                view()
                    .flex_shrink(1.)
                    .min_w(px(0.))
                    .tooltip(widgets::wrap_tooltip(tip))
                    .child(crumb.child(words))
                    .into_element()
            }
        })
        .collect();
    let mut right = view().flex_row().flex_none().items_center().gap(px(8.));
    if let Some(total) = map.total {
        right = right.child(size_text(total));
    }
    right = right.child(reveal_button(map.path, storagepage::reveal_tip()));
    view()
        .flex_row()
        .flex_none()
        .items_center()
        .justify_between()
        .gap(px(10.))
        .child(
            view()
                .flex_row()
                .flex_1()
                .min_w(px(0.))
                .items_center()
                .gap(px(2.))
                .children(parts)
                .child(
                    view()
                        .pl(px(4.))
                        .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
                            "disk.map_hint",
                        )))),
                ),
        )
        .child(right)
        .into_element()
}

/// The tiles, or the sentence in their place.
fn map_body(map: Option<MapView>) -> AnyElement {
    let Some(map) = map else {
        return view().into_element();
    };
    if let Some((words, action)) = map.note {
        let mut note = view()
            .absolute()
            .left(px(0.))
            .top(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .items_center()
            .justify_center()
            .gap(px(8.))
            .child(
                text(words)
                    .text_size(px(ROW_PT))
                    .line_height(widgets::line(ROW_PT))
                    .text_color(theme::color(palette::text_muted)),
            );
        if let Some(label) = action {
            note = note.child(measure_button(label));
        }
        return note.into_element();
    }
    let interactive = map.interactive;
    view()
        .absolute()
        .left(px(0.))
        .top(px(0.))
        .right(px(0.))
        .bottom(px(0.))
        .children(
            map.tiles
                .into_iter()
                .map(|tile| map_tile(tile, interactive))
                .collect::<Vec<_>>(),
        )
        .into_element()
}

/// gpui-kit's default small button (`Button::small()`): the map's walk
/// of a level its index cannot answer.
fn measure_button(label: String) -> AnyElement {
    nsh_base::button()
        .flex_none()
        .h(px(24.))
        .px(px(8.))
        .rounded(px(6.))
        .border(px(1.))
        .border_color(theme::color(palette::border))
        .bg(theme::color(palette::inset))
        .items_center()
        .justify_center()
        .hover(|s| s.bg(theme::color(palette::surface_raised)))
        .on_press(|| store::update(|state, fx| state.measure_map_level(fx)))
        .child(
            text(label)
                .text_size(px(14.))
                .line_height(px(17.5))
                .text_color(theme::color(palette::text)),
        )
        .into_element()
}

/// One tile: its fill, its name (and size where there is room) in the ink
/// that stands out from it, the path and share on hover, and a click that
/// opens it on a finished map.
fn map_tile(tile: Tile, interactive: bool) -> AnyElement {
    let fill: Paint = tile.fill();
    let hover: Paint = tile.hover();
    let ink = move || dirmap::ink_on(fill.color());
    let faint = move || {
        let ink = ink();
        color::Rgba {
            a: ink.a * 0.75,
            ..ink
        }
    };
    let labelled = tile.labelled();
    let shows_size = tile.shows_size();
    let Tile {
        rect,
        name,
        bytes,
        tip,
        open,
        nests,
        name_pt,
        ..
    } = tile;
    let name_text = text(name)
        .number_of_lines(1u32)
        .text_size(px(name_pt))
        .line_height(widgets::line(name_pt))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::color(ink));
    let size_line = move || {
        text(format::memory(bytes))
            .number_of_lines(1u32)
            .font_family(widgets::MONO)
            .text_size(px(META_PT))
            .line_height(widgets::line(META_PT))
            .text_color(theme::color(faint))
    };
    let label: Option<AnyElement> = labelled.then(|| {
        if nests {
            // The strip: name left, size right, one line.
            view()
                .flex_row()
                .h(px(dirmap::HEADER_H - 4.))
                .items_center()
                .justify_between()
                .gap(px(6.))
                .child(name_text.flex_shrink(1.).min_w(px(0.)))
                .child(size_line().flex_none())
                .into_element()
        } else {
            let mut column = view().gap(px(1.)).child(name_text);
            if shows_size {
                column = column.child(size_line());
            }
            column.into_element()
        }
    });
    let body = view()
        .size_full()
        .overflow_hidden()
        .rounded(px(3.))
        .px(px(6.))
        .py(px(if nests { 2. } else { 4. }))
        .bg(theme::color(move || fill.color()));
    let body = match label {
        Some(label) => body.child(label),
        None => body,
    };
    let content: AnyElement = match open.filter(|_| interactive) {
        Some(chain) => nsh_base::button()
            .size_full()
            .rounded(px(3.))
            .hover(move |s| s.bg(theme::color(move || hover.color())))
            .on_press(move || {
                let chain = chain.clone();
                store::update(|state, fx| state.open_in_map(chain, fx));
            })
            .child(body.bg(theme::color(|| CLEAR)))
            .bg(theme::color(move || fill.color()))
            .into_element(),
        None => body.into_element(),
    };
    view()
        .absolute()
        .left(px(rect.x))
        .top(px(rect.y))
        .w(px(rect.w))
        .h(px(rect.h))
        .tooltip(widgets::tooltip_lines(move || tip.clone()))
        .child(content)
        .into_element()
}
