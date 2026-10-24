//! The disk-space window on NSH: `views/storage.rs` and its
//! `StorageWindow` (`gpui_app.rs`), whose comments say why the window and
//! each part of it is the way it is — this file carries the same choices
//! and does not repeat their reasons. The window's rules are shared
//! (`storagepage.rs`, the directory map's layout in `dirmap.rs`); the
//! state is the store's, so a walk or a search outlives the window. The
//! tabs arrive in phase 4k of docs/nsh-migration.md: the frame and
//! Duplicates first; the analysis is a card that says so until it is
//! ported.
//!
//! The tab strip stays put, and so does each card's head: only the
//! result scrolls, inside the card's border. A folder is picked in the
//! system's panel, which the host opens (`HostAction::PickFolder`).
//!
//! Its keys are the gpui window's: `mod+1` and `mod+2` pick the tab,
//! `mod+r` runs the tab's walk again, Escape closes (and `mod+w`, through
//! the host's Window menu).

use crate::assets::CustomIconName;
use crate::dupes::DupeScope;
use crate::nsh::root::{self, AuxWindow, FolderPick, HostAction, Live};
use crate::nsh::widgets::{self, CLEAR};
use crate::nsh::{chrome, store, theme};
use crate::state::{DiskAnalysis, DupeSearch, StorageTab, ZStatsAppState};
use crate::storagepage::{self, DUPE_FILES_SHOWN, HEAD_PT, META_PT, ROW_PT};
use crate::{color, finder, format, i18n, palette};
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
        FolderPick::Dupes => "disk.dup_pick_go",
    })
}

/// What the host's folder panel answered, for `pick`. Any thread.
pub fn picked(pick: FolderPick, paths: Vec<PathBuf>) {
    match pick {
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
    view()
        .size_full()
        .bg(theme::color(palette::ground))
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
                    StorageTab::Analysis => not_yet(),
                    StorageTab::Duplicates => dupes_card(live),
                })),
        )
}

/// The analysis until it is ported. Not translated — a development
/// build's card, gone with the tab's port.
fn not_yet() -> AnyElement {
    widgets::empty_card(
        i18n::tr("disk.tab_analysis"),
        "Not on the NSH front end yet — phase 4k of docs/nsh-migration.md.".into(),
    )
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
    tip: Option<String>,
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
        Some(tip) => view()
            .flex_none()
            .tooltip(widgets::wrap_tooltip(tip))
            .child(chip)
            .into_element(),
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
    dynamic(move || {
        let h = head.get();
        let mut parts: Vec<AnyElement> = Vec::new();
        if !h.off {
            parts.push(dupes_header(&h));
        }
        parts.push(dupes_scope_row(&h));
        let content = if h.off {
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
            Some(i18n::tr("disk.dup_scope_home_tip")),
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
            Some(i18n::tr("disk.dup_scope_folder_tip")),
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
