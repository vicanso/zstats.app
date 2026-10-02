//! The disk-space window: the directory analyser, the Spotlight
//! large-file query and the duplicate search, one tab each, in a
//! standard window of their own.
//!
//! Both were sections of the Hardware tab's boot-volume card until the
//! panel ran out of width for them. Three ranked tables at 320px meant
//! every path was an ellipsis and every table a fold; the window opens at
//! 507 and has room to be read. It also settles a smaller
//! contradiction: these are one-shot *queries* — the walk takes minutes
//! and survives hide by design — while the popover auto-hides on any
//! focus loss. A surface you cannot look away from was the wrong home
//! for them.
//!
//! What that costs, stated plainly: the results no longer sit inside the
//! card of the volume they were asked about, which is what
//! docs/disk-analysis.md used to require. The Hardware tab keeps one
//! button, and the window carries the answer.
//!
//! Tabs rather than one stack of cards: the three answer different
//! questions (which folder is big, which file is big, what is stored
//! twice), each result is a long table, and stacked they made the third
//! one a long scroll away. The tab strip stays put above the scrolling
//! body, and each tab keeps its own scroll position (`StorageWindow`).
//!
//! Nothing here owns state. Every feature lives in `ZStatsAppState`,
//! the selected tab included; this window observes the same store the
//! panel does, and every action (start, cancel, open a row, trash) goes
//! through the store's methods — so a scan or a search started here
//! keeps running with the window closed, exactly as it did with the
//! panel hidden.

use super::widgets;
use crate::active;
use crate::assets::CustomIconName;
use crate::bigfiles;
use crate::cleanhints::{self, CleanHint};
use crate::confirm;
use crate::diskscan::{self, DiffBaseline, DirHit, FileHit, HitKind, ScanResult, ScanScope};
use crate::diskwatch;
use crate::dupes::{DupeFile, DupeGroup, DupeScope};
use crate::font;
use crate::format;
use crate::i18n;
use crate::prefs;
use crate::state::{
    BigFiles, DiskAnalysis, DupeProgress, DupeSearch, Expansion, StorageTab, ZStatsAppState,
    ZStatsGlobalStore,
};
use crate::theme;
use gpui::Entity;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px, relative,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The selected tab's body, under the band every tab shares: the disk
/// the window is named for, and what is waiting in the Trash.
pub fn render(state: &ZStatsAppState, exclude: &Entity<InputState>) -> Vec<AnyElement> {
    let mut cards = Vec::new();
    match summary_band(state) {
        Some(band) => cards.push(band),
        // No sample yet: the trashed line still has to be said.
        None => cards.extend(trashed_note(state.trashed_this_session())),
    }
    cards.push(match state.storage_tab() {
        StorageTab::Analysis => analysis_card(state, exclude),
        StorageTab::LargeFiles => big_files_card(state),
        StorageTab::Duplicates => dupes_card(state),
    });
    cards
}

/// The three tabs, underlined: labels in a row over a hairline, the
/// selected one standing on a 2px bar (the theme's mark where it has one,
/// otherwise the text ink — accent stays for thresholds). Hovering an
/// unselected tab brightens its label and previews the bar in the border
/// ink, which is its affordance. Every label keeps one weight: these tabs
/// are as wide as their words, and a bolder selected label would nudge
/// the ones after it on every switch.
///
/// A tab whose work is running while another is selected wears a dot:
/// walks and searches outlive the tab as they outlive the window, and the
/// dot is how one left running stays findable.
pub fn tab_strip(state: &ZStatsAppState) -> AnyElement {
    let active = state.storage_tab();
    let busy = |tab: StorageTab| match tab {
        StorageTab::Analysis => matches!(state.disk_analysis(), DiskAnalysis::Running { .. }),
        StorageTab::LargeFiles => matches!(state.big_files(), BigFiles::Running),
        StorageTab::Duplicates => matches!(state.dupe_search(), DupeSearch::Running { .. }),
    };
    let bar = theme::mark().map_or(theme::text(), |(fill, _)| fill);
    let cell = |tab: StorageTab| {
        let on = tab == active;
        let group = SharedString::from(format!("storage-tab-{}", tab.index()));
        let title = t!(
            "tabs.shortcut",
            name = i18n::tr(tab.label_key()),
            hint = super::shortcut_hint(),
            n = tab.index() + 1
        )
        .to_string();
        v_flex()
            .id(("storage-tab", tab.index()))
            .group(group.clone())
            .tooltip(widgets::wrap_tooltip(title))
            .flex_none()
            .child(
                h_flex()
                    .items_center()
                    .gap(px(5.))
                    .px(px(2.))
                    .pt(px(2.))
                    .pb(px(7.))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(if on { theme::text() } else { theme::text_dim() })
                    .when(!on, |d| {
                        d.group_hover(group.clone(), |s| s.text_color(theme::text()))
                    })
                    .child(i18n::tr(tab.label_key()))
                    .when(busy(tab) && !on, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .size(px(5.))
                                .rounded_full()
                                .bg(theme::text_dim()),
                        )
                    }),
            )
            .child(
                div()
                    .h(px(2.))
                    .rounded_full()
                    .when(on, |d| d.bg(bar))
                    .when(!on, |d| d.group_hover(group, |s| s.bg(theme::border()))),
            )
            .on_click(move |_, _window, cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.set_storage_tab(tab, cx));
            })
    };
    // The hairline is painted first and the bars over it, so the selected
    // bar sits on the line rather than floating above it.
    div()
        .relative()
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(1.))
                .bg(theme::border_subtle()),
        )
        .child(
            h_flex()
                .gap(px(18.))
                .children(StorageTab::ALL.into_iter().map(cell)),
        )
        .into_any_element()
}

// ---- the window's shared pieces ----------------------------------------

/// The window's type scale: three sizes. The panel's kit, carried over
/// whole, had seven inside three points (9 to 12) — legible at 320px and
/// fussy at 507, where every step read as a near-miss rather than a
/// level. Meta is captions, counts and units; rows are what is ranked;
/// headings are the few lines that say what a block is.
const META_PT: f32 = 10.;
const ROW_PT: f32 = 12.;
const HEAD_PT: f32 = 13.;

/// One trailing button's slot. Every row reserves its two, filled or
/// not, so sizes share one right edge — rows with a trash control and
/// rows without one used to end the size column in three places.
const ACTION_SLOT: f32 = 22.;

/// The chevron's slot, held on every analysis row — mid-walk rows have
/// no chevron yet, and labels used to shift right when the walk ended.
const CHEVRON_SLOT: f32 = 12.;

/// Gap between a row's parts.
const ROW_GAP: f32 = 6.;

/// A size as its number in bold and its unit in the dim ink — a bold
/// "GB GB GB" column was half the weight of every table, spent on the
/// part that tells the rows apart least.
fn size_text(bytes: u64) -> AnyElement {
    let text = format::memory(bytes);
    let (value, unit) = text.split_once(' ').unwrap_or((text.as_str(), ""));
    h_flex()
        .flex_none()
        .items_baseline()
        .gap(px(2.))
        .font_family(font::MONO)
        .child(
            div()
                .text_size(px(ROW_PT))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::text())
                .child(value.to_string()),
        )
        .child(
            div()
                .text_size(px(META_PT))
                .text_color(theme::text_dim())
                .child(unit.to_string()),
        )
        .into_any_element()
}

/// The widest a file or folder name may grow before it, too, truncates.
const NAME_MAX: f32 = 240.;

/// A path as two runs: the folders it sits in, dim and the first to give
/// way, then its own name, bright and kept. Cut at the end, five rows
/// under one QQMusic container read identically, and the large-file
/// list was four rows of "snapshot". `text` is what the row shows (a
/// relative or `~` path); the tooltip carries the whole location.
fn path_label(id: SharedString, text: &str, tip: String, size: f32) -> AnyElement {
    let (prefix, name) = match text.trim_end_matches('/').rfind('/') {
        Some(at) => text.split_at(at + 1),
        None => ("", text),
    };
    h_flex()
        .id(id)
        .flex_1()
        .min_w_0()
        .items_baseline()
        .text_size(px(size))
        .tooltip(widgets::wrap_tooltip(tip))
        .when(!prefix.is_empty(), |d| {
            d.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::text_dim())
                    .child(prefix.to_string()),
            )
        })
        .child(
            div()
                .flex_none()
                .max_w(px(NAME_MAX))
                .truncate()
                .text_color(theme::text())
                .child(name.to_string()),
        )
        .into_any_element()
}

/// A row's two trailing slots, each [`ACTION_SLOT`] wide whether or not
/// it holds a button.
fn action_slots(first: Option<AnyElement>, second: Option<AnyElement>) -> AnyElement {
    let slot = |button: Option<AnyElement>| {
        h_flex()
            .flex_none()
            .w(px(ACTION_SLOT))
            .justify_center()
            .children(button)
    };
    h_flex()
        .flex_none()
        .child(slot(first))
        .child(slot(second))
        .into_any_element()
}

/// [`theme::ink`] at the share Overview's history bars keep. A ranking's
/// bars are context for the figures beside them; in full ink, twenty of
/// them were the loudest thing in the window — solid black in light mode.
fn meter_ink() -> Hsla {
    let ink = theme::ink();
    Hsla::from(gpui::Rgba {
        a: ink.a * 0.45,
        ..ink
    })
}

/// A row's share bar: 2px, the dimmed ink, starting under the label and
/// ending under the size.
fn row_meter(fraction: f32, depth: usize) -> AnyElement {
    div()
        .mt(px(3.))
        .ml(px(CHEVRON_SLOT + ROW_GAP + indent(depth)))
        .mr(px(ACTION_SLOT * 2. + ROW_GAP))
        .child(widgets::meter(fraction, meter_ink(), 2.))
        .into_any_element()
}

/// A block's heading inside a card: its name at heading size, a dim
/// clause of what it holds, and a control on the right.
fn section_heading(title: String, meta: Option<String>, right: Option<AnyElement>) -> AnyElement {
    h_flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .pt(px(10.))
        .pb(px(5.))
        .child(
            h_flex()
                .min_w_0()
                .items_baseline()
                .gap(px(8.))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(HEAD_PT))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(title),
                )
                .children(meta.map(|meta| {
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(META_PT))
                        .text_color(theme::text_dim())
                        .child(meta)
                })),
        )
        .children(right)
        .into_any_element()
}

/// The one filled button a tab shows before it has run — the theme's own
/// primary (Golden's champagne, Omarchy's accent, neutral otherwise),
/// never the alarm red. An outlined 10pt pill in a corner was the whole
/// call to action, under a paragraph that read like a man page.
fn primary_button(
    id: &'static str,
    label: String,
    on_click: impl Fn(&mut gpui::App) + 'static,
) -> AnyElement {
    Button::new(id)
        .primary()
        .small()
        .label(label)
        .on_click(move |_, _window, cx| on_click(cx))
        .into_any_element()
}

/// A tab before its first run: what it finds, what it costs, one button.
/// The method — how it decides — is the ⓘ's, not the body's.
fn empty_state(lead: String, cost: String, tip: String, button: AnyElement) -> AnyElement {
    v_flex()
        .px(px(13.))
        .pt(px(4.))
        .pb(px(16.))
        .gap(px(6.))
        .child(
            h_flex()
                .items_center()
                .gap(px(5.))
                .child(
                    div()
                        .text_size(px(HEAD_PT))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::text())
                        .child(lead),
                )
                .child(widgets::info_icon(SharedString::from("empty-tip"), tip)),
        )
        .child(
            div()
                .text_size(px(META_PT + 1.))
                .line_height(relative(1.4))
                .text_color(theme::text_muted())
                .child(cost),
        )
        .child(h_flex().pt(px(6.)).child(button))
        .into_any_element()
}

/// Above every tab: the boot volume as zstats measures it — the same
/// figures, words and colour rule as its Hardware card, nothing derived
/// here — and what is waiting in the Trash. The window was titled "Disk
/// Space" and never showed the disk; after a cleanup the one number that
/// proves it worked (free space, once the Trash is emptied) was in
/// another window.
fn summary_band(state: &ZStatsAppState) -> Option<AnyElement> {
    let disks = state.latest()?.snapshot.disks.as_deref()?;
    let boot = disks.iter().find(|d| d.mount_point == "/")?;
    let used = boot.used_percent;
    let hot = used > super::disk::FULL_PERCENT;
    let name = if boot.name.trim().is_empty() {
        boot.mount_point.clone()
    } else {
        boot.name.clone()
    };
    let trash = trash_line(state);
    Some(
        widgets::card()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(
                                        Icon::new(IconName::HardDrive)
                                            .with_size(Size::Size(px(13.)))
                                            .text_color(Hsla::from(theme::text_dim())),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(HEAD_PT))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(theme::text())
                                            .child(name),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .items_baseline()
                                    .gap(px(5.))
                                    .child(size_text(boot.available_bytes))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(META_PT))
                                            .text_color(theme::text_dim())
                                            .child(
                                                t!(
                                                    "disk.free_of",
                                                    total = format::capacity(boot.total_bytes),
                                                    used = format!("{used:.0}")
                                                )
                                                .to_string(),
                                            ),
                                    ),
                            ),
                    )
                    .children(trash),
            )
            .child(div().mt(px(9.)).child(widgets::meter(
                used / 100.0,
                Hsla::from(theme::fill_for(hot)),
                4.,
            )))
            .into_any_element(),
    )
}

/// The band's right half: what this session moved to the Trash, or —
/// before anything has moved — how much the last home analysis found in
/// it. Either way the space comes back only when the Trash is emptied,
/// and the button opens it; emptying stays the reader's own act.
fn trash_line(state: &ZStatsAppState) -> Option<AnyElement> {
    let moved = state.trashed_this_session();
    let (figure, caption, tip) = if moved > 0 {
        (
            moved,
            i18n::tr("disk.band_moved"),
            t!("disk.trashed_note", bytes = format::memory(moved)).to_string(),
        )
    } else {
        let DiskAnalysis::Ready(result) = state.disk_analysis() else {
            return None;
        };
        let home = env::var_os("HOME")?;
        let trash = Path::new(&home).join(".Trash");
        let hit = result.dirs.iter().find(|d| d.path == trash)?;
        (
            hit.bytes,
            i18n::tr("disk.band_in_trash"),
            t!(
                "disk.band_in_trash_tip",
                ago = format::ago(result.scanned_at.elapsed().unwrap_or_default())
            )
            .to_string(),
        )
    };
    Some(
        h_flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .child(
                v_flex()
                    .id("band-trash")
                    .items_end()
                    .gap(px(2.))
                    .tooltip(widgets::wrap_tooltip(tip))
                    .child(size_text(figure))
                    .child(
                        div()
                            .text_size(px(META_PT))
                            .text_color(theme::text_dim())
                            .child(caption),
                    ),
            )
            .child(
                Button::new("band-open-trash")
                    .xsmall()
                    .label(i18n::tr("disk.open_trash"))
                    .on_click(|_, _, _| bigfiles::open_trash()),
            )
            .into_any_element(),
    )
}

/// The analyser card: what the shown result is and the control that
/// re-runs it, one toolbar line of scope and settings, then the result.
///
/// The result starts as high as it can. Scope, exclusions and the daily
/// check used to be three labelled rows above it — at the window's 620pt
/// the first result row sat at 38% and the big-directory table at 87%,
/// below rows used once a month. They are one line now; the exclusions
/// open as a drawer from it.
fn analysis_card(state: &ZStatsAppState, exclude: &Entity<InputState>) -> AnyElement {
    let running = matches!(state.disk_analysis(), DiskAnalysis::Running { .. });
    let mismatch = analysis_mismatch(state);
    let body =
        match state.disk_analysis() {
            DiskAnalysis::Off => analysis_empty(state),
            // Partial tables, same renderer as the final result: figures are
            // lower bounds that only grow. No delete controls mid-walk (the
            // walker may still be inside any of these trees), no deltas (a
            // lower bound against a finished run reads as shrinkage).
            DiskAnalysis::Running { partial, .. } => div()
                .children(partial.as_ref().map(|r| {
                    analysis_tables(state, r, false, state.analysis_show_all_dirs(), None)
                }))
                .into_any_element(),
            DiskAnalysis::Failed(_) => div()
                .px(px(13.))
                .pb(px(12.))
                .child(widgets::note(i18n::tr("disk.ana_failed_body")))
                .into_any_element(),
            // Dimmed — paint only — when it is not the scope now selected,
            // so the table cannot pass for the answer to the chip that is lit.
            DiskAnalysis::Ready(result) => div()
                .when(mismatch.is_some(), |d| d.opacity(0.5))
                .child(analysis_tables(
                    state,
                    result,
                    true,
                    state.analysis_show_all_dirs(),
                    state.analysis_diff_for(result),
                ))
                .into_any_element(),
        };
    widgets::list_shell()
        .pt(px(4.))
        .children(analysis_header(state))
        .child(analysis_toolbar(state, running))
        .children(
            (state.analysis_exclude_open() && !running).then(|| analysis_exclude_drawer(exclude)),
        )
        .children(fda_hint(state).map(|hint| div().px(px(13.)).pb(px(8.)).child(hint)))
        .children(mismatch.map(|(shown, ago, selected)| {
            div().px(px(13.)).pb(px(8.)).child(widgets::note(
                t!(
                    "disk.ana_showing",
                    shown = shown,
                    ago = ago,
                    selected = selected
                )
                .to_string(),
            ))
        }))
        .child(body)
        .into_any_element()
}

/// The scope a person would call it: "Home", "Caches", "Whole disk", or
/// the folder's `~` path. Every caption leads with it — a home result
/// used to omit its root ("~ goes without saying"), so nothing on screen
/// said which scope a table answered once another chip was lit.
fn scope_word(roots: &[PathBuf], base: &Path) -> String {
    let home = diskscan::default_root();
    if home.as_ref().is_some_and(|home| roots == [home.clone()]) {
        return i18n::tr("disk.ana_preset_home");
    }
    if ScanScope::cache_set().is_some_and(|caches| caches.roots == roots) {
        return i18n::tr("disk.ana_preset_caches");
    }
    if ScanScope::whole_disk().roots == roots {
        return i18n::tr("disk.ana_preset_disk");
    }
    let home_s = env::var("HOME").unwrap_or_default();
    scope_display(roots, base, &home_s)
}

/// The scope the next walk covers: the picked one, or home.
fn selected_scope(state: &ZStatsAppState) -> Option<ScanScope> {
    state
        .disk_analysis_scope()
        .or_else(|| diskscan::default_root().map(ScanScope::single))
}

/// `(shown, age, selected)` when the result on screen answers a scope
/// other than the selected one. Picking a chip selects, Analyze walks —
/// that keeps a minutes-long walk from starting on a stray click — but
/// it also let a lit "Caches" sit over a table that was still Home's.
fn analysis_mismatch(state: &ZStatsAppState) -> Option<(String, String, String)> {
    let DiskAnalysis::Ready(result) = state.disk_analysis() else {
        return None;
    };
    let selected = selected_scope(state)?;
    (selected.roots != result.roots).then(|| {
        (
            scope_word(&result.roots, &result.root),
            format::ago(result.scanned_at.elapsed().unwrap_or_default()),
            scope_word(&selected.roots, &selected.base),
        )
    })
}

/// Before the first walk: what it finds, what it costs, and the one
/// button — named for the scope it will walk.
fn analysis_empty(state: &ZStatsAppState) -> AnyElement {
    let word = selected_scope(state)
        .map(|s| scope_word(&s.roots, &s.base))
        .unwrap_or_else(|| i18n::tr("disk.ana_preset_home"));
    empty_state(
        i18n::tr("disk.ana_empty_lead"),
        i18n::tr("disk.ana_empty_cost"),
        i18n::tr("disk.ana_hint"),
        primary_button(
            "ana-start",
            t!("disk.ana_scan_scope", scope = word).to_string(),
            |cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.start_disk_analysis(cx));
            },
        ),
    )
}

/// The caption and the run control. Nothing before the first walk —
/// the empty state carries that button.
fn analysis_header(state: &ZStatsAppState) -> Option<AnyElement> {
    let (scope, rest) = analysis_caption(state)?;
    let ready = matches!(state.disk_analysis(), DiskAnalysis::Ready(_));
    let progress = match state.disk_analysis() {
        DiskAnalysis::Running {
            dirs_done,
            expected_dirs: Some(expected),
            ..
        } if *expected > 0 => Some(*dirs_done as f32 / *expected as f32),
        _ => None,
    };
    let skips = match state.disk_analysis() {
        DiskAnalysis::Ready(result) => analysis_skips(result),
        _ => None,
    };
    let controls = h_flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .child(analysis_chip(state))
        .when(ready, |row| row.child(clear_button()))
        .when(
            matches!(state.disk_analysis(), DiskAnalysis::Failed(_)),
            |row| row.child(dismiss_failed_button()),
        )
        .into_any_element();
    let caption = h_flex()
        .min_w_0()
        .flex_wrap()
        .items_center()
        .gap_x(px(5.))
        .child(
            div()
                .min_w_0()
                .whitespace_normal()
                .text_color(theme::text_dim())
                .child(rest),
        )
        .children(skips.map(|(label, lines)| {
            h_flex()
                .id("ana-skips")
                .flex_none()
                .items_center()
                .gap(px(3.))
                .text_color(theme::text_dim())
                .tooltip(widgets::wrap_tooltip_lines(lines))
                .child(format!("· {label}"))
                .child(
                    Icon::new(IconName::Info)
                        .with_size(Size::Size(px(10.)))
                        .text_color(Hsla::from(theme::text_dim())),
                )
        }))
        .into_any_element();
    Some(
        div()
            .child(header_rows(scope, caption, controls))
            // Measured against the last walk of this scope: an estimate,
            // and drawn as one — dim, and never quite full until the
            // walk is.
            .children(progress.map(|fraction| {
                div().px(px(13.)).pb(px(8.)).child(widgets::meter(
                    fraction.min(0.97),
                    meter_ink(),
                    2.,
                ))
            }))
            .into_any_element(),
    )
}

/// A tab card's head: the scope it answered as the card's title with the
/// run controls beside it, and the caption on a line of its own under
/// them — beside the buttons it wrapped, and a lone "· 70 skipped"
/// started the second line.
fn header_rows(scope: String, caption: AnyElement, controls: AnyElement) -> AnyElement {
    v_flex()
        .px(px(13.))
        .pt(px(8.))
        .pb(px(8.))
        .gap(px(2.))
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap(px(10.))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(HEAD_PT))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(scope),
                )
                .child(controls),
        )
        .child(
            div()
                .text_size(px(META_PT))
                .line_height(relative(1.4))
                .child(caption),
        )
        .into_any_element()
}

/// Drops the finished result — and the saved copy and the ± baseline
/// with it — after a confirm: it took minutes to produce, and it sat one
/// button away from Analyze again with nothing in between.
fn clear_button() -> AnyElement {
    widgets::with_wrap_tooltip(
        "ana-dismiss-tip",
        i18n::tr("disk.ana_dismiss_hint"),
        Button::new("ana-dismiss")
            .icon(IconName::Close)
            .ghost()
            .xsmall()
            .on_click(|_, window, cx| {
                confirm::ask(
                    window,
                    cx,
                    i18n::tr("disk.ana_clear_title"),
                    i18n::tr("disk.ana_clear_body"),
                    i18n::tr("disk.ana_clear_ok"),
                    |cx| {
                        cx.global::<ZStatsGlobalStore>()
                            .clone()
                            .update(cx, |state, cx| state.clear_disk_analysis(cx));
                    },
                );
            }),
    )
}

/// A failed walk has nothing to lose: no confirm.
fn dismiss_failed_button() -> AnyElement {
    Button::new("ana-dismiss-failed")
        .icon(IconName::Close)
        .ghost()
        .xsmall()
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.clear_disk_analysis(cx));
        })
        .into_any_element()
}

/// Run / cancel. Running stays clickable — it is the cancel, the only
/// way a walk stops early (closing the window deliberately does not).
/// The label names the scope it will walk whenever that is not the
/// result on screen.
fn analysis_chip(state: &ZStatsAppState) -> AnyElement {
    let running = matches!(state.disk_analysis(), DiskAnalysis::Running { .. });
    // Older than [`STALE_AFTER`]: the chip asks, with a brighter outline
    // and a sentence on hover. Neutral on purpose — an old result is not
    // a crossed line, so no accent.
    let stale = match state.disk_analysis() {
        DiskAnalysis::Ready(result) => {
            result.scanned_at.elapsed().unwrap_or_default() > STALE_AFTER
        }
        _ => false,
    };
    let label = if running {
        i18n::tr("disk.ana_cancel")
    } else if let Some((_, _, selected)) = analysis_mismatch(state) {
        t!("disk.ana_scan_scope", scope = selected).to_string()
    } else if matches!(state.disk_analysis(), DiskAnalysis::Ready(_)) {
        i18n::tr("disk.ana_rescan")
    } else {
        let word = selected_scope(state)
            .map(|s| scope_word(&s.roots, &s.base))
            .unwrap_or_default();
        t!("disk.ana_scan_scope", scope = word).to_string()
    };
    div()
        .id("diskscan-chip")
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(if stale {
            theme::text_muted()
        } else {
            theme::border()
        })
        .bg(if stale { theme::chip() } else { theme::inset() })
        .px(px(9.))
        .py(px(2.))
        .tooltip(widgets::wrap_tooltip(if stale {
            i18n::tr("disk.ana_stale")
        } else {
            i18n::tr("disk.ana_hint")
        }))
        .text_size(px(META_PT + 1.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text())
        .hover(|d| d.bg(theme::surface_raised()))
        .on_click(move |_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| {
                    if matches!(state.disk_analysis(), DiskAnalysis::Running { .. }) {
                        state.cancel_disk_analysis(cx);
                    } else {
                        state.start_disk_analysis(cx);
                    }
                });
        })
        .child(label)
        .into_any_element()
}

/// Scope, exclusions and the daily check, as one line of chips. While a
/// walk runs the line stays where it is, dimmed and inert — changing
/// what a running walk covers goes through Cancel — instead of vanishing
/// and pulling the tables up by 110pt, then pushing them back down when
/// the walk ends.
fn analysis_toolbar(state: &ZStatsAppState, running: bool) -> AnyElement {
    h_flex()
        .flex_wrap()
        .items_center()
        .gap(px(4.))
        .px(px(13.))
        .pb(px(8.))
        .when(running, |d| d.opacity(0.45))
        .children(analysis_preset_chips(state, running))
        .child(analysis_pick_chip(running))
        .child(
            div()
                .flex_none()
                .w(px(1.))
                .h(px(12.))
                .mx(px(4.))
                .bg(theme::border()),
        )
        .child(exclude_toggle(state, running))
        .child(daily_chip(state, running))
        .into_any_element()
}

/// A toolbar chip's look, shared so the scope presets, the exclusions
/// toggle and the daily check read as one row.
fn toolbar_chip(id: impl Into<gpui::ElementId>, on: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .rounded(px(5.))
        .px(px(6.))
        .py(px(2.))
        .text_size(px(META_PT + 1.))
        .font_weight(gpui::FontWeight::MEDIUM)
        // `surface_raised`, not `chip`: light mode's chip is white, the
        // card's own colour, and a selected scope vanished into it.
        .when(on, |d| {
            d.bg(theme::surface_raised()).text_color(theme::text())
        })
        .when(!on, |d| {
            d.text_color(theme::text_muted())
                .hover(|d| d.bg(theme::surface_raised()).text_color(theme::text()))
        })
}

/// Opens the exclusions drawer. Says how many there are, because a walk
/// that leaves folders out must not let that be forgotten.
fn exclude_toggle(state: &ZStatsAppState, running: bool) -> AnyElement {
    let n = prefs::analysis_exclude_raw().len();
    let open = state.analysis_exclude_open();
    toolbar_chip("ana-exclude-toggle", open)
        .tooltip(widgets::wrap_tooltip(i18n::tr("disk.ana_exclude_note")))
        .when(!running, |d| {
            d.on_click(|_, _window, cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.toggle_analysis_exclude(cx));
            })
        })
        .child(if n == 0 {
            i18n::tr("disk.ana_exclude_add")
        } else {
            t!("disk.ana_excluded_n", n = n).to_string()
        })
        .into_any_element()
}

/// The daily background check (`diskwatch`), on or off, with its state
/// on hover. A chip in the toolbar's neutral ink rather than a switch:
/// the switch wore the theme's mark and was the most saturated thing in
/// the window, for a preference set once.
fn daily_chip(state: &ZStatsAppState, running: bool) -> AnyElement {
    let on = prefs::disk_watch();
    let status = if !on {
        i18n::tr("disk.watch_off")
    } else if state.disk_check_running() {
        i18n::tr("disk.watch_running")
    } else {
        match state.disk_check_last() {
            Some(at) => {
                let since = at.elapsed().unwrap_or_default();
                let last = t!("disk.watch_last", ago = format::ago(since)).to_string();
                if since > diskwatch::CHECK_EVERY {
                    format!("{last} · {}", i18n::tr(overdue_reason(state)))
                } else {
                    last
                }
            }
            None => i18n::tr("disk.watch_first"),
        }
    };
    toolbar_chip("ana-watch", false)
        .tooltip(widgets::wrap_tooltip_lines(vec![
            status.into(),
            i18n::tr("disk.watch_tip").into(),
        ]))
        .when(!running, |d| {
            d.on_click(move |_, _window, cx| {
                prefs::set_disk_watch(!on);
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |_, cx| cx.notify());
            })
        })
        .child(
            h_flex()
                .items_center()
                .gap(px(5.))
                .child(
                    div()
                        .flex_none()
                        .size(px(6.))
                        .rounded_full()
                        .when(on, |d| d.bg(theme::text_muted()))
                        .when(!on, |d| d.border_1().border_color(theme::text_dim())),
                )
                .child(i18n::tr("disk.watch")),
        )
        .into_any_element()
}

/// Directories this walk leaves alone: the excluded chips, the field to
/// add one, and the folder picker — a drawer under the toolbar, open
/// when asked for. It stays in this window rather than Settings because
/// the reason to exclude `~/github` occurs to you while looking at
/// `~/github` at the top of the table.
fn analysis_exclude_drawer(exclude: &Entity<InputState>) -> AnyElement {
    let home = env::var("HOME").unwrap_or_default();
    let entries = prefs::analysis_exclude_raw();
    div()
        .px(px(13.))
        .pb(px(8.))
        .child(
            h_flex()
                .items_center()
                .flex_wrap()
                .gap(px(4.))
                .p(px(6.))
                .rounded(px(7.))
                .bg(theme::inset())
                .children(entries.iter().enumerate().map(|(i, raw)| {
                    let expanded = match raw.strip_prefix("~/") {
                        Some(rest) if !home.is_empty() => Path::new(&home).join(rest),
                        _ => PathBuf::from(raw),
                    };
                    // A path that does not resolve today is dimmed, not
                    // dropped: a directory can come back, and quietly
                    // discarding what someone typed is the worse failure.
                    let present = expanded.is_dir();
                    let drop_me = raw.clone();
                    h_flex()
                        .id(("ana-exclude-chip", i))
                        .items_center()
                        .gap(px(3.))
                        .flex_none()
                        .rounded(px(4.))
                        .bg(theme::chip())
                        .px(px(5.))
                        .py(px(1.))
                        .text_size(px(META_PT + 1.))
                        .text_color(if present {
                            theme::text()
                        } else {
                            theme::text_dim()
                        })
                        .when(!present, |d| {
                            d.tooltip(widgets::wrap_tooltip(i18n::tr("disk.ana_exclude_missing")))
                        })
                        .child(raw.clone())
                        .child(
                            div()
                                .id(("ana-exclude-drop", i))
                                .flex_none()
                                .hover(|d| d.text_color(theme::text()))
                                .tooltip(widgets::wrap_tooltip(i18n::tr("disk.ana_exclude_drop")))
                                .child(
                                    Icon::new(IconName::Close)
                                        .with_size(Size::Size(px(9.)))
                                        .text_color(Hsla::from(theme::text_dim())),
                                )
                                .on_click(move |_, _window, cx| {
                                    let kept: Vec<String> = prefs::analysis_exclude_raw()
                                        .into_iter()
                                        .filter(|p| *p != drop_me)
                                        .collect();
                                    prefs::set_analysis_exclude(&kept);
                                    cx.global::<ZStatsGlobalStore>()
                                        .clone()
                                        .update(cx, |_, cx| cx.notify());
                                }),
                        )
                }))
                .child(
                    // Fixed and small: this is a field used once a
                    // month, and it must not out-shout the table.
                    div()
                        .flex_none()
                        .w(px(180.))
                        .child(Input::new(exclude).xsmall()),
                )
                .child(
                    Button::new("ana-exclude-pick")
                        .icon(IconName::FolderOpen)
                        .ghost()
                        .xsmall()
                        .tooltip(i18n::tr("disk.ana_exclude_pick"))
                        .on_click(|_, _window, cx| {
                            let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
                                files: false,
                                directories: true,
                                multiple: true,
                                prompt: Some(i18n::tr("disk.ana_exclude_pick_go").into()),
                            });
                            cx.spawn(async move |cx| {
                                if let Ok(Ok(Some(paths))) = rx.await {
                                    let mut list = prefs::analysis_exclude_raw();
                                    list.extend(paths.iter().map(|p| p.display().to_string()));
                                    prefs::set_analysis_exclude(&list);
                                    cx.update(|cx| {
                                        cx.global::<ZStatsGlobalStore>()
                                            .clone()
                                            .update(cx, |_, cx| cx.notify());
                                    });
                                }
                            })
                            .detach();
                        }),
                ),
        )
        .into_any_element()
}

/// Why a check that is due has not run, in the order the gates fall
/// (`diskwatch::due`). "Last 1d ago" beside a check that is on read as
/// a broken schedule, when it was only waiting — and one of the things
/// it waits for is this very window closing, so that is what it says
/// once nothing else is in the way. Reads the same fields the gate
/// does, for display only.
fn overdue_reason(state: &ZStatsAppState) -> &'static str {
    let tick = state.latest();
    let on_battery = tick
        .and_then(|t| t.snapshot.battery.as_ref())
        .is_some_and(|b| matches!(b.state.as_str(), "Discharging" | "Empty"));
    let busy = tick.is_some_and(|t| t.snapshot.cpu.usage_percent > diskwatch::BUSY_CPU);
    if on_battery {
        "disk.watch_wait_power"
    } else if busy {
        "disk.watch_wait_quiet"
    } else {
        "disk.watch_wait_closed"
    }
}

/// What grew in the home tree since about a week ago (`diskwatch`), as
/// the first table of a home result — the question "what is big" has a
/// sibling, "what is getting big", and only this table answers it.
/// Read-only rows: growth is a reason to look, not a cleanup verdict;
/// the reveal button leads to the folder, and a cache that grew is
/// already in the suggestions below with its own controls.
fn growth_section(report: &diskwatch::Report) -> Option<AnyElement> {
    if report.rows.is_empty() {
        return None;
    }
    let days = (report.over.as_secs_f64() / 86_400.0).round().max(1.0) as u64;
    let max = report.rows.iter().map(|g| g.grew).max().unwrap_or(1).max(1);
    let home = env::var("HOME").unwrap_or_default();
    Some(
        div()
            .px(px(13.))
            .pb(px(6.))
            .child(
                div()
                    .id("ana-growth-title")
                    .tooltip(widgets::wrap_tooltip(i18n::tr("disk.growth_tip")))
                    .child(section_heading(
                        t!("disk.growth_title", days = days).to_string(),
                        None,
                        None,
                    )),
            )
            .children(report.rows.iter().map(|row| {
                let reveal = row.path.clone();
                let key = format!("ana-growth:{}", row.path.display());
                let shown = format::tilde_path(&row.path.display().to_string(), &home);
                div()
                    .py(px(5.))
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(ROW_GAP))
                            .child(div().flex_none().w(px(CHEVRON_SLOT)))
                            .child(path_label(
                                SharedString::from(format!("{key}-name")),
                                &shown,
                                shown.clone(),
                                ROW_PT,
                            ))
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(font::MONO)
                                    .text_size(px(META_PT))
                                    .text_color(theme::text_muted())
                                    .child(format!("+{}", format::memory(row.grew))),
                            )
                            .child(size_text(row.now))
                            .child(action_slots(
                                Some(
                                    Button::new(SharedString::from(format!("{key}-reveal")))
                                        .icon(IconName::Folder)
                                        .ghost()
                                        .xsmall()
                                        .tooltip(reveal_tip())
                                        .on_click(move |_, _, _| bigfiles::reveal(&reveal))
                                        .into_any_element(),
                                ),
                                None,
                            )),
                    )
                    .child(row_meter(row.grew as f32 / max as f32, 0))
            }))
            .into_any_element(),
    )
}

/// Pick a folder to analyze instead of the default home tree — the
/// native directory panel. Choosing only selects: the run control is what
/// starts the walk. The panel takes key focus while it is up; this
/// window survives that (it has a title bar, not the popover's
/// hide-on-blur).
fn analysis_pick_chip(running: bool) -> AnyElement {
    let button = Button::new("ana-pick")
        .icon(IconName::FolderOpen)
        .ghost()
        .xsmall()
        .when(!running, |b| {
            b.on_click(|_, _window, cx| {
                let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some(i18n::tr("disk.ana_pick_go").into()),
                });
                cx.spawn(async move |cx| {
                    if let Ok(Ok(Some(paths))) = rx.await
                        && let Some(root) = paths.into_iter().next()
                    {
                        cx.update(|cx| {
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| state.set_disk_analysis_at(root, cx));
                        });
                    }
                })
                .detach();
            })
        });
    // A sentence, not a label — it needs the wrapping tooltip a Button
    // cannot build for itself.
    widgets::with_wrap_tooltip("ana-pick-tip", i18n::tr("disk.ana_pick_hint"), button)
}

/// Preset scopes (docs/disk-analysis.md's scope table): the home tree,
/// `~/Library` — the blind-spot close-up — the merged cache roots, and
/// the writable volume. Clicking selects; the run control walks. No
/// label in front: each chip's tooltip says what it covers, and "Whole
/// disk" says why `/` is not a choice.
///
/// `~` is a chip even though it is also the default, because a scope
/// sticks once picked: after a `~/Library` run, "re-analyze" means
/// `~/Library` this launch and the next. Without this chip the only way
/// back was the ✕, which also deletes the cached result and the Δ
/// baseline — a heavy price for changing your mind about scope.
fn analysis_preset_chips(state: &ZStatsAppState, running: bool) -> Vec<AnyElement> {
    let selected = state.disk_analysis_scope();
    let is_sel = |scope: Option<ScanScope>| -> bool {
        match (selected.as_ref(), scope.as_ref()) {
            (Some(a), Some(b)) => a.roots == b.roots,
            _ => false,
        }
    };
    let home = diskscan::default_root();
    let home_scope = home.clone().map(ScanScope::single);
    let library_scope = home.clone().map(|h| ScanScope::single(h.join("Library")));
    let caches_scope = ScanScope::cache_set();
    let disk_scope = Some(ScanScope::whole_disk());
    // No pick yet is the default home walk: its chip is the lit one.
    let home_on = selected.is_none() || is_sel(home_scope.clone());
    let chip = |id: &'static str,
                label: String,
                tip: String,
                on: bool,
                go: fn(&mut ZStatsAppState, &mut gpui::Context<ZStatsAppState>)|
     -> AnyElement {
        toolbar_chip(id, on)
            .tooltip(widgets::wrap_tooltip(tip))
            .when(!running, |d| {
                d.on_click(move |_, _window, cx| {
                    cx.global::<ZStatsGlobalStore>().clone().update(cx, go);
                })
            })
            .child(label)
            .into_any_element()
    };
    let mut chips = vec![
        chip(
            "ana-preset-home",
            // A word, not the path: `~` alone is a one-character chip
            // that means nothing outside a terminal. The exact path rides
            // the tooltip, where `~/Library` beside it keeps its own —
            // that one is a specific subfolder, and naming it "Library"
            // would collide with /Library and /System/Library.
            i18n::tr("disk.ana_preset_home"),
            i18n::tr("disk.ana_preset_home_tip"),
            home_on,
            |state, cx| {
                if let Some(home) = diskscan::default_root() {
                    state.set_disk_analysis_at(home, cx);
                }
            },
        ),
        chip(
            "ana-preset-library",
            "~/Library".into(),
            i18n::tr("disk.ana_preset_library_tip"),
            is_sel(library_scope),
            |state, cx| {
                if let Some(home) = diskscan::default_root() {
                    state.set_disk_analysis_at(home.join("Library"), cx);
                }
            },
        ),
        chip(
            "ana-preset-caches",
            i18n::tr("disk.ana_preset_caches"),
            i18n::tr("disk.ana_preset_caches_tip"),
            is_sel(caches_scope),
            |state, cx| state.set_disk_analysis_caches(cx),
        ),
        // Last among presets, and deliberately so: it is the slowest
        // by a wide margin and the only one that needs Full Disk
        // Access to be complete.
        chip(
            "ana-preset-disk",
            i18n::tr("disk.ana_preset_disk"),
            format!(
                "{} {}",
                i18n::tr("disk.ana_preset_disk_tip"),
                i18n::tr("disk.ana_scope_tip")
            ),
            is_sel(disk_scope),
            |state, cx| state.set_disk_analysis_whole_disk(cx),
        ),
    ];
    // A picked folder is not a preset — without a chip here the
    // selection would only live in the next walk, and the row would look
    // like nothing was chosen.
    if let Some(scope) = selected.as_ref() {
        let preset = [
            home.clone().map(ScanScope::single),
            home.map(|h| ScanScope::single(h.join("Library"))),
            ScanScope::cache_set(),
            Some(ScanScope::whole_disk()),
        ]
        .into_iter()
        .flatten()
        .any(|p| p.roots == scope.roots);
        if !preset {
            let home_s = env::var("HOME").unwrap_or_default();
            let label = scope_display(&scope.roots, &scope.base, &home_s);
            chips.push(
                toolbar_chip("ana-preset-custom", true)
                    .child(div().max_w(px(140.)).min_w_0().truncate().child(label))
                    .into_any_element(),
            );
        }
    }
    chips
}

/// When permission gaps hid part of the tree, say so and offer the one
/// switch that covers them all. macOS 15+ gates every other app's
/// container behind its own per-app prompt; Full Disk Access supersedes
/// the whole category — the standard, proportionate ask for a disk
/// scanner. Tied to `skipped_denied` only: the TCC deny-list skips are
/// deliberate zero-touch and no permission would change them.
#[cfg(target_os = "macos")]
fn fda_hint(state: &ZStatsAppState) -> Option<AnyElement> {
    let DiskAnalysis::Ready(result) = state.disk_analysis() else {
        return None;
    };
    if result.skipped_denied == 0 {
        return None;
    }
    Some(
        h_flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .mt(px(4.))
            .child(div().flex_1().min_w_0().child(widgets::note(
                t!("disk.ana_fda_note", n = result.skipped_denied).to_string(),
            )))
            .child(
                Button::new("ana-fda")
                    .ghost()
                    .xsmall()
                    .label(i18n::tr("disk.ana_fda_open"))
                    .on_click(|_, _window, _cx| super::disk::open_full_disk_access()),
            )
            .into_any_element(),
    )
}

/// Off macOS a directory the user cannot read is simply unreadable —
/// there is no one switch that would open it, so there is nothing to
/// offer. The "N unreadable skipped" chip on the result row is the
/// whole of the truth here.
#[cfg(not(target_os = "macos"))]
fn fda_hint(_state: &ZStatsAppState) -> Option<AnyElement> {
    None
}

/// "Reveal in Finder" on macOS; the file manager has no one name on
/// Linux, and the sentence should not promise Finder.
fn reveal_tip() -> String {
    i18n::tr(if cfg!(target_os = "macos") {
        "disk.big_reveal"
    } else {
        "disk.big_reveal_linux"
    })
}

/// Results older than this get a "consider re-analyzing" nudge on the
/// Re-analyze chip. Display only, like every threshold in views/ —
/// nothing refreshes itself: a minutes-long walk must never
/// self-trigger, so a nudge is where staleness honesty ends. A day is
/// when "the numbers are from earlier" stops going without saying —
/// mostly reached through the persisted cache surviving a restart.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// The header's caption as `(scope, the rest)`: the scope in the
/// heading weight — always said, a home result included — then age,
/// cost and size of the walk, and what the ± compares against. While a
/// walk runs, the rest is its progress, against last time's count when
/// there was a last time. `None` before the first walk.
fn analysis_caption(state: &ZStatsAppState) -> Option<(String, String)> {
    match state.disk_analysis() {
        DiskAnalysis::Off => None,
        DiskAnalysis::Failed(e) => Some((
            i18n::tr("disk.ana_failed_title"),
            t!("disk.ana_failed", e = e.clone()).to_string(),
        )),
        DiskAnalysis::Running {
            dirs_done,
            scope,
            expected_dirs,
            ..
        } => {
            let done = format::thousands(*dirs_done);
            let rest = match expected_dirs {
                Some(last) => t!(
                    "disk.ana_running_of",
                    dirs = done,
                    last = format::thousands(*last)
                )
                .to_string(),
                None => t!("disk.ana_running", dirs = done).to_string(),
            };
            Some((scope_word(&scope.roots, &scope.base), rest))
        }
        DiskAnalysis::Ready(result) => {
            let mut parts = vec![
                format::ago(result.scanned_at.elapsed().unwrap_or_default()),
                t!("disk.ana_took", t = format::took(result.took)).to_string(),
                t!(
                    "disk.ana_dirs_seen",
                    n = format::thousands(result.dirs_seen)
                )
                .to_string(),
            ];
            // Names what the per-row ± compares against. Its absence when
            // no row moved is itself the answer: nothing big changed.
            if let Some(diff) = state.analysis_diff_for(result) {
                parts.push(
                    t!(
                        "disk.ana_diff_base",
                        ago = format::ago(diff.scanned_at().elapsed().unwrap_or_default())
                    )
                    .to_string(),
                );
            }
            Some((scope_word(&result.roots, &result.root), parts.join(" · ")))
        }
    }
}

/// Everything a finished walk left out, as one figure and its breakdown:
/// `("70 skipped", [each non-zero count])`. The figure counts places the
/// walk did not measure (protected, unreadable, cloud placeholders,
/// excluded); hard links counted once are not a skip and only join the
/// breakdown — alone, they are the figure. `None` when there is nothing
/// to say.
fn analysis_skips(result: &ScanResult) -> Option<(String, Vec<SharedString>)> {
    let skipped = [
        ("disk.ana_skip_protected", result.skipped_protected),
        ("disk.ana_skip_denied", result.skipped_denied),
        ("disk.ana_skip_dataless", result.skipped_dataless),
        // Said for the same reason as the others: a walk that left out
        // somebody's whole code tree must not let the totals below it
        // read as the whole scope.
        ("disk.ana_skip_excluded", result.skipped_excluded),
    ];
    let mut lines: Vec<SharedString> = skipped
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(key, n)| t!(*key, n = n).to_string().into())
        .collect();
    // Why the totals can read lower than Finder adding up the folders.
    let links = (result.shared_links > 0)
        .then(|| t!("disk.ana_shared_links", n = result.shared_links).to_string());
    let total: usize = skipped.iter().map(|(_, n)| *n).sum();
    if total == 0 {
        return links.map(|text| (text.clone(), vec![text.into()]));
    }
    lines.extend(links.map(SharedString::from));
    Some((t!("disk.ana_skipped_total", n = total).to_string(), lines))
}

/// One string naming a scope: the single root, or a multi-root scope's
/// roots listed in full — passing the base alone would read as a walk of
/// the whole home tree. Every path tilde'd; a plain "~" is the default
/// home walk, which callers omit (the only scope that goes without
/// saying).
fn scope_display(roots: &[PathBuf], base: &Path, home: &str) -> String {
    if roots.len() > 1 {
        roots
            .iter()
            .map(|r| format::tilde_path(&r.display().to_string(), home))
            .collect::<Vec<_>>()
            .join(" + ")
    } else {
        format::tilde_path(&base.display().to_string(), home)
    }
}

fn analysis_tables(
    state: &ZStatsAppState,
    result: &ScanResult,
    actions: bool,
    show_all_dirs: bool,
    diff: Option<&DiffBaseline>,
) -> AnyElement {
    let root = result.root.clone();
    // Asked once per paint of the tables, not per row.
    let running = active::running_bundle_ids();
    // One tooltip for every ± in these tables: which run the figure is
    // measured against, and why silence is not a claim of "new".
    let delta_tip = diff.map(|d| {
        t!(
            "disk.delta_tip",
            ago = format::ago(d.scanned_at().elapsed().unwrap_or_default())
        )
        .to_string()
    });
    let dir_rows = |hits: &[DirHit], id: &'static str, deletable: bool| -> Vec<AnyElement> {
        let max = hits.iter().map(|h| h.bytes).max().unwrap_or(1).max(1);
        let ctx = TreeCtx {
            state,
            diff,
            delta_tip: delta_tip.clone(),
            running: &running,
            id,
            deletable,
            // Only a finished result has the retained index behind it;
            // mid-walk tables are lower bounds with nothing to open.
            expandable: actions,
        };
        hits.iter()
            .flat_map(|h| dir_row_tree(&ctx, h, &root, max, 0))
            .collect()
    };
    let file_rows = |hits: &[FileHit]| -> Vec<AnyElement> {
        let max = hits.iter().map(|h| h.bytes).max().unwrap_or(1).max(1);
        hits.iter()
            .map(|h| {
                analysis_row(AnalysisRow {
                    key: row_key("ana-file", &h.path),
                    path: &h.path,
                    bytes: h.bytes,
                    prev_bytes: diff.and_then(|d| d.bytes_for(&h.path)),
                    delta_tip: delta_tip.clone(),
                    kind: None,
                    asset: None,
                    running: &running,
                    group_max: max,
                    root: &root,
                    deletable: false,
                    expandable: false,
                    open: false,
                    depth: 0,
                    // Files never sit in the suggestions.
                    suggested: false,
                })
            })
            .collect()
    };

    let section = |heading: AnyElement,
                   rows: Vec<AnyElement>,
                   footer: Option<AnyElement>|
     -> Option<AnyElement> {
        if rows.is_empty() {
            return None;
        }
        Some(
            div()
                .px(px(13.))
                .pb(px(6.))
                .child(heading)
                .children(rows)
                .children(footer.map(|chip| h_flex().pt(px(6.)).child(chip)))
                .into_any_element(),
        )
    };

    // The suggestion set, minus caches too small to name a size: a
    // "0 MB" row with a trash control frees nothing a reader could see.
    // The first few are listed; the heading carries the count and the
    // total, and the bulk button acts on the whole set — honest, because
    // unlike the capped tables the full list is retained.
    const MIB: u64 = 1024 * 1024;
    let sugs: Vec<DirHit> = result
        .suggestions
        .iter()
        .filter(|d| d.bytes >= MIB)
        .cloned()
        .collect();
    let sug_total: u64 = sugs.iter().map(|d| d.bytes).sum();
    let show_all_sugs = state.analysis_show_all_sugs();
    let sug_shown = if show_all_sugs {
        sugs.len()
    } else {
        sugs.len().min(SUG_ROWS)
    };
    // Growth is measured on the home tree only (`diskwatch`); a result
    // for any other scope says nothing about it.
    let home_result = diskscan::default_root().is_some_and(|home| result.roots == [home]);
    div()
        .children(
            state
                .disk_growth()
                .filter(|_| home_result && actions)
                .and_then(growth_section),
        )
        .children(section(
            // The payoff, at heading weight: the total a reader came for
            // used to be the dimmest text in the window.
            section_heading(
                i18n::tr("disk.sug_title"),
                Some(t!("disk.sug_meta", n = sugs.len()).to_string()),
                Some(
                    h_flex()
                        .items_center()
                        .gap(px(10.))
                        .child(size_text(sug_total))
                        .when(actions, |row| {
                            row.child(suggest_clear_button(&sugs, &running))
                        })
                        .into_any_element(),
                ),
            ),
            dir_rows(&sugs[..sug_shown], "ana-sug", actions),
            (sugs.len() > SUG_ROWS).then(|| {
                more_pill("ana-sug-more")
                    .child(if show_all_sugs {
                        i18n::tr("disk.ana_less")
                    } else {
                        t!("disk.ana_more", count = sugs.len() - SUG_ROWS).to_string()
                    })
                    .on_click(|_, _window, cx| {
                        cx.global::<ZStatsGlobalStore>()
                            .clone()
                            .update(cx, |state, cx| state.toggle_analysis_show_all_sugs(cx));
                    })
                    .into_any_element()
            }),
        ))
        .children({
            // Suggestions already name the trashable caches. Repeating
            // them under Big directories (same path, same bytes, no
            // trash control) reads as a double render — cargo-target
            // at 46.8 GB twice in the screenshot. Drop those paths
            // here; the ranking of everything else stays.
            let dirs: Vec<DirHit> = result
                .dirs
                .iter()
                .filter(|d| result.suggestions.iter().all(|s| s.path != d.path))
                .cloned()
                .collect();
            // Default 8–10 rows; "show more" reveals everything retained
            // (up to TABLE_KEEP). The chip states how many are hidden.
            let shown = if show_all_dirs {
                dirs.len()
            } else {
                diskscan::default_rows(&dirs, |d| d.bytes)
            };
            let hidden = dirs.len() - shown;
            section(
                section_heading(i18n::tr("disk.ana_dirs"), None, None),
                dir_rows(&dirs[..shown], "ana-dir", false),
                (hidden > 0 || show_all_dirs).then(|| more_chip(hidden, show_all_dirs)),
            )
        })
        .children(section(
            section_heading(i18n::tr("disk.ana_files"), None, None),
            file_rows(&result.files),
            None,
        ))
        .child(
            div()
                .px(px(13.))
                .pb(px(10.))
                .child(widgets::note(i18n::tr("disk.ana_note"))),
        )
        .into_any_element()
}

/// "Trash all" for the suggestion set — acts on the FULL set (TAG trees
/// plus hint-trashable caches), not just the rendered head. The confirm
/// lists every row with its size and a tick, so what moves is what the
/// reader saw, and a row can be kept back; a cache whose app is running
/// starts unticked, because moving it frees nothing until the app quits.
/// Suggestions listed before "show more". Enough to see the shape of
/// it — the largest usually dwarf the rest — without pushing the big
/// directories, the real answer to "where did it go", below the fold.
const SUG_ROWS: usize = 5;

fn suggest_clear_button(hits: &[DirHit], running: &[String]) -> AnyElement {
    let home = env::var("HOME").unwrap_or_default();
    let paths: Vec<PathBuf> = hits.iter().map(|h| h.path.clone()).collect();
    let items: Vec<(String, u64, Option<String>)> = hits
        .iter()
        .map(|hit| {
            let caution = cleanhints::lookup(&hit.path)
                .filter(|hint| hint.running_in(running))
                .map(|hint| t!("disk.pick_running", owner = &hint.owner).to_string());
            (
                format::tilde_path(&hit.path.display().to_string(), &home),
                hit.bytes,
                caution,
            )
        })
        .collect();
    Button::new("ana-sug-clear")
        .icon(CustomIconName::Trash)
        .ghost()
        .xsmall()
        .label(i18n::tr("disk.sug_clear"))
        .on_click(move |_, window, cx| {
            let paths = paths.clone();
            let sheet = confirm::PickSheet {
                title: i18n::tr("disk.sug_clear_title"),
                body: i18n::tr("disk.sug_pick_body"),
                items: items
                    .iter()
                    .map(|(label, bytes, caution)| confirm::PickItem {
                        label: label.clone(),
                        bytes: *bytes,
                        caution: caution.clone(),
                    })
                    .collect(),
                ok: |n, bytes| {
                    t!("disk.sug_pick_ok", n = n, bytes = format::memory(bytes)).to_string()
                },
            };
            confirm::ask_pick(window, cx, sheet, move |chosen, cx| {
                let picked: Vec<PathBuf> = chosen.iter().map(|&i| paths[i].clone()).collect();
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.trash_regenerable(&picked, cx));
            });
        })
        .into_any_element()
}

/// Above every tab's card once anything went to the Trash this session: a move
/// frees nothing until the Trash is emptied, and without saying so a
/// "Trash all · 26.8 GB" read as 26.8 GB back while the volume did not
/// move. The button only opens the Trash — emptying it is the reader's
/// own act, the one step that cannot be undone.
fn trashed_note(bytes: u64) -> Option<AnyElement> {
    if bytes == 0 {
        return None;
    }
    Some(
        widgets::card()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.))
                    .px(px(13.))
                    .py(px(9.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.))
                            .line_height(relative(1.35))
                            .text_color(theme::text_muted())
                            .child(
                                t!("disk.trashed_note", bytes = format::memory(bytes)).to_string(),
                            ),
                    )
                    .child(
                        Button::new("disk-open-trash")
                            .xsmall()
                            .label(i18n::tr("disk.open_trash"))
                            .on_click(|_, _, _| bigfiles::open_trash()),
                    ),
            )
            .into_any_element(),
    )
}

/// How many children an opened row lists before it stops and says how
/// many it is holding back. Eight keeps one open row from pushing the
/// tables under it off screen, and the summary line is what keeps the
/// cut visible instead of silent — the same bargain the dirs table's
/// "show more" makes.
const EXPAND_ROWS: usize = 8;

/// One indent step, in px. Enough to read as a level at 11px type
/// without spending the width the paths need.
const INDENT_STEP: f32 = 13.;

fn indent(depth: usize) -> f32 {
    depth as f32 * INDENT_STEP
}

/// Element ids come from the path, not a row index: an opened row shifts
/// every index below it, and gpui would then hand a row's hover and
/// tooltip state to whatever moved into its old slot.
fn row_key(table: &str, path: &Path) -> SharedString {
    SharedString::from(format!("{table}:{}", path.display()))
}

/// What every row in one table shares. A struct rather than passing six
/// more arguments down a recursion.
struct TreeCtx<'a> {
    state: &'a ZStatsAppState,
    diff: Option<&'a DiffBaseline>,
    delta_tip: Option<String>,
    /// Bundle ids of the running apps, for the "running" pill.
    running: &'a [String],
    /// Id prefix, one per table, so the same path listed in two tables
    /// (a suggestion can also rank as a big directory) stays two rows.
    id: &'static str,
    /// Whether these rows may carry the trash control (suggestions).
    deletable: bool,
    expandable: bool,
}

/// One directory row, plus — when it is open — its children under it,
/// recursively.
///
/// This is the whole of the tree: the state holds which paths are open
/// and what is under each ([`ZStatsAppState::toggle_expansion`]), and
/// the rows below an open one are rendered from that. Nothing above the
/// clicked row moves, which is the entire point of the change from the
/// old drill-down.
fn dir_row_tree(
    ctx: &TreeCtx,
    hit: &DirHit,
    parent: &Path,
    group_max: u64,
    depth: usize,
) -> Vec<AnyElement> {
    let key = row_key(ctx.id, &hit.path);
    let open = ctx.expandable && ctx.state.expansion(&hit.path).is_some();
    let mut out = vec![analysis_row(AnalysisRow {
        key: key.clone(),
        path: &hit.path,
        bytes: hit.bytes,
        prev_bytes: ctx.diff.and_then(|d| d.bytes_for(&hit.path)),
        delta_tip: ctx.delta_tip.clone(),
        kind: Some(hit.kind),
        asset: hit.asset.as_ref(),
        running: ctx.running,
        group_max,
        root: parent,
        deletable: ctx.deletable,
        expandable: ctx.expandable,
        open,
        depth,
        suggested: ctx.id == "ana-sug",
    })];
    if !open {
        return out;
    }
    match ctx.state.expansion(&hit.path) {
        // Children are ranked against each other, not against the
        // table's largest: a meter that reads 2% on every child says
        // nothing about which of them is the heavy one.
        Some(Expansion::Ready(rows)) if !rows.is_empty() => {
            let max = rows.iter().map(|r| r.bytes).max().unwrap_or(1).max(1);
            for child in rows.iter().take(expand_shown(rows)) {
                out.extend(dir_row_tree(ctx, child, &hit.path, max, depth + 1));
            }
            if let Some((hidden, bytes)) = expand_hidden(rows) {
                out.push(expand_note(
                    &key,
                    depth + 1,
                    t!(
                        "disk.ana_rest",
                        count = hidden,
                        bytes = format::memory(bytes)
                    )
                    .to_string(),
                ));
            }
        }
        // Empty is an answer, not a failure: nothing inside cleared the
        // bar these tables rank by.
        Some(Expansion::Ready(_)) => out.push(expand_note(
            &key,
            depth + 1,
            i18n::tr("disk.ana_expand_empty"),
        )),
        Some(Expansion::Walking) => {
            out.push(expand_note(&key, depth + 1, i18n::tr("disk.ana_expanding")))
        }
        Some(Expansion::Failed) => out.push(expand_note(
            &key,
            depth + 1,
            i18n::tr("disk.ana_expand_failed"),
        )),
        None => {}
    }
    out
}

/// Under a megabyte a row reads `0 MB` — `format::memory` has nothing
/// finer to say at that scale. A line that reads zero spends a row
/// saying nothing, so those go to the summary instead, which was
/// already there and can carry them as a total.
const EXPAND_FLOOR: u64 = 1024 * 1024;

/// How many children an open row lists: the ranked ones, down to where
/// they stop being distinguishable, and never more than [`EXPAND_ROWS`].
/// The list is sorted, so this is a prefix.
fn expand_shown(rows: &[DirHit]) -> usize {
    rows.iter()
        .take(EXPAND_ROWS)
        .take_while(|r| r.bytes >= EXPAND_FLOOR)
        .count()
}

/// What an open row is not listing: how many children it held back and
/// how much they add up to, or `None` when everything is shown. Counts
/// what the tables retained, exactly like the dirs table's own "show
/// more" — neither claims to have seen every directory on disk.
fn expand_hidden(rows: &[DirHit]) -> Option<(usize, u64)> {
    let shown = expand_shown(rows);
    let hidden = rows.len().checked_sub(shown).filter(|n| *n > 0)?;
    Some((hidden, rows.iter().skip(shown).map(|r| r.bytes).sum()))
}

/// The dim line an open row shows in place of children — scanning, empty,
/// failed, or the count it is not listing.
fn expand_note(key: &SharedString, depth: usize, text: String) -> AnyElement {
    div()
        .id(SharedString::from(format!("{key}-note-{depth}")))
        .pl(px(4. + indent(depth)))
        .py(px(3.))
        .text_size(px(10.))
        .text_color(theme::text_dim())
        .child(text)
        .into_any_element()
}

/// One ranked row: path relative to the scan root, physical size, a meter
/// against the group's largest, and Finder Reveal. `deletable` adds the
/// confirm-gated move-to-Trash — passed only for the regenerable table,
/// whose rows are all signature-checked `CACHEDIR.TAG` trees; heuristic
/// and plain rows never get the control. `expandable` makes the row
/// itself clickable — it opens in place, see [`dir_row_tree`].
/// Only the owner-declared tier gets a pill: "cache" maps straight to
/// an action semantic — this row is (or can be) a cleanup suggestion.
/// A heuristic fold earns no pill; its explanatory value did not pay
/// for the attention it took, so the how-it-was-classified note rides
/// the row's name tooltip instead. Plain directories say nothing.
/// The same pill for a MobileAsset row, carrying what the system
/// declares (`asset_clause`). It earns its attention where a heuristic
/// fold did not: without it a row reads as an unexplained
/// `com_apple_MobileAsset_UAF_Siri_Understanding` and nobody would
/// know a tooltip is there — and unlike "cache", this pill exists to
/// say the row is *not* yours to delete.
fn asset_pill(key: &SharedString, note: &diskscan::AssetNote) -> AnyElement {
    div()
        .id(SharedString::from(format!("{key}-asset")))
        .flex_none()
        .rounded_full()
        .px(px(5.))
        .text_size(px(9.))
        .bg(theme::inset())
        .text_color(theme::tiny_label(theme::text_muted()))
        .tooltip(widgets::wrap_tooltip(asset_clause(note)))
        .child(i18n::tr("disk.asset_pill"))
        .into_any_element()
}

/// What a row's cleanup standing rests on, as a word on the row rather
/// than only in the name's tooltip — the cleanup list used to label just
/// its CACHEDIR.TAG trees, so 31 documented tool caches beside one tagged
/// tree read as unexplained. "rebuilds": the owner declared the tree
/// regenerable (TAG), or its tool's documentation lists it as a cache it
/// re-creates (`trashable` hint). "manual": a known owner whose content
/// it does not simply rebuild — data, downloads, a working tree — which
/// is why it never joins the suggestions. Plain rows carry nothing.
fn basis_pill(
    key: &SharedString,
    kind: Option<HitKind>,
    hint: Option<&CleanHint>,
) -> Option<AnyElement> {
    let (label, tip) = if kind == Some(HitKind::Tag) {
        (i18n::tr("disk.kind_tag"), i18n::tr("disk.kind_tag_tip"))
    } else {
        let hint = hint?;
        let base = if hint.trashable {
            t!("disk.basis_rebuild_tip", owner = &hint.owner).to_string()
        } else {
            t!("disk.basis_manual_tip", owner = &hint.owner).to_string()
        };
        let tip = match &hint.command {
            Some(cmd) => format!(
                "{base} {}",
                t!("disk.basis_cmd", owner = &hint.owner, cmd = cmd)
            ),
            None => base,
        };
        let label = if hint.trashable {
            i18n::tr("disk.kind_tag")
        } else {
            i18n::tr("disk.basis_manual")
        };
        (label, tip)
    };
    Some(row_pill(
        SharedString::from(format!("{key}-kind")),
        label,
        tip,
        theme::text_muted(),
    ))
}

/// The cache's app is running. Moving a cache out from under a live app
/// frees nothing — it keeps writing into the moved folder until it
/// restarts — so the row says so before the trash control is reached.
fn running_pill(key: &SharedString, owner: &str) -> AnyElement {
    row_pill(
        SharedString::from(format!("{key}-running")),
        i18n::tr("disk.running"),
        t!("disk.running_tip", owner = owner).to_string(),
        theme::text(),
    )
}

fn row_pill(id: SharedString, label: String, tip: String, ink: gpui::Rgba) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .rounded_full()
        .px(px(5.))
        .text_size(px(9.))
        .bg(theme::inset())
        .text_color(theme::tiny_label(ink))
        .tooltip(widgets::wrap_tooltip(tip))
        .child(label)
        .into_any_element()
}

/// The outlined pill every "show more" in the app wears (Sensors,
/// Traffic, Listening, History) — this window's two used to be bare text
/// links, a fourth look for the same control.
fn more_pill(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
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
}

/// The dirs section's fold: "show more · N" ↔ "show less".
fn more_chip(hidden: usize, show_all: bool) -> AnyElement {
    more_pill("ana-dirs-more")
        .child(if show_all {
            i18n::tr("disk.ana_less")
        } else {
            t!("disk.ana_more", count = hidden).to_string()
        })
        .on_click(move |_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| {
                    state.set_analysis_show_all_dirs(!show_all, cx)
                });
        })
        .into_any_element()
}

/// One ranked row's inputs, named — a struct rather than a dozen
/// positional arguments (clippy's lint was right about the call sites).
struct AnalysisRow<'a> {
    /// Element-id prefix, derived from the path rather than a row index
    /// ([`row_key`]): nested rows have no flat index, and a stable id
    /// keeps gpui's per-element state (hover, tooltips) attached to the
    /// same directory when a sibling above it opens.
    key: SharedString,
    path: &'a Path,
    bytes: u64,
    /// This path's figure in the previous run, when it ranked there —
    /// `None` renders no delta (absence proves nothing, see
    /// [`DiffBaseline`]).
    prev_bytes: Option<u64>,
    /// The ± explainer, shared by the whole table (names the baseline
    /// run). Only read when a delta actually renders.
    delta_tip: Option<String>,
    kind: Option<HitKind>,
    /// What macOS declares about a MobileAsset row (`assetinfo`).
    /// `None` for every other path, and for the file table — files
    /// carry no declaration of their own.
    asset: Option<&'a diskscan::AssetNote>,
    /// Bundle ids of the running apps: a row whose cache belongs to one
    /// wears "running", and its trash confirm says what that costs.
    running: &'a [String],
    /// The group's largest row, the meter's 100%.
    group_max: u64,
    /// The row's label is this path made relative — the scan root at
    /// top level, the parent row's path inside an opened one.
    root: &'a Path,
    deletable: bool,
    /// Whether the row opens on click; also whether it wears a chevron.
    expandable: bool,
    open: bool,
    /// Nesting level, purely visual: one indent step per level.
    depth: usize,
    /// A row of the cleanup suggestions. Every one of those rebuilds
    /// itself — the section title says so once — so its basis pill would
    /// be the same word on every row; the basis moves to the name's
    /// tooltip and only the exceptions ("running") stay as pills.
    suggested: bool,
}

/// The delta column's width: `-999.9 MB` at 9.5px mono, the widest a
/// delta prints.
const DELTA_W: f32 = 56.;

/// Below this a row shows no delta: the tables rank hundreds of MB and
/// up, so a ±few-MB drift on every row would be noise dressed as
/// signal. 2% of `TABLE_EXTEND_MIN`, the smallest figure the extended
/// table admits.
const DIFF_FLOOR: u64 = 10 * 1024 * 1024;

/// `+1.2 GB` / `-340.0 MB` against the previous run, or `None` when
/// there is nothing honest to say (no baseline row, or under the floor).
fn delta_label(bytes: u64, prev_bytes: Option<u64>) -> Option<String> {
    let prev = prev_bytes?;
    let (sign, diff) = if bytes >= prev {
        ("+", bytes - prev)
    } else {
        ("-", prev - bytes)
    };
    (diff >= DIFF_FLOOR).then(|| format!("{sign}{}", format::memory(diff)))
}

/// The system's own words about a MobileAsset row, as one sentence.
///
/// Every clause is a field that was actually present in the asset's
/// `Info.plist`; absent fields say nothing, because the alternative is
/// the app guessing about system files. The closing line is the part
/// the reader most needs and the only part not read off disk: this
/// content belongs to `mobileassetd`, and the way to reclaim it is the
/// system's own storage management, not a delete button here.
fn asset_clause(note: &diskscan::AssetNote) -> String {
    let mut out = t!("disk.asset_kind", kind = &note.kind).to_string();
    if let Some(locale) = &note.locale {
        out.push_str(&format!(" · {}", t!("disk.asset_locale", lang = locale)));
    }
    if note.required_by_os == Some(true) {
        out.push_str(&format!(" · {}", i18n::tr("disk.asset_required")));
    }
    if note.never_collected == Some(true) {
        out.push_str(&format!(" · {}", i18n::tr("disk.asset_never_collected")));
    }
    out.push_str(&format!(" — {}", i18n::tr("disk.asset_reclaim")));
    out
}

fn analysis_row(row: AnalysisRow) -> AnyElement {
    let AnalysisRow {
        key,
        path,
        bytes,
        prev_bytes,
        delta_tip,
        kind,
        asset,
        running,
        group_max,
        root,
        deletable,
        expandable,
        open,
        depth,
        suggested,
    } = row;
    let label = path
        .strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string());
    let mut full = format::tilde_path(
        &path.display().to_string(),
        &env::var("HOME").unwrap_or_default(),
    );
    // A heuristic fold explains itself here rather than with a pill.
    if kind == Some(HitKind::Heuristic) {
        full.push_str(" — ");
        full.push_str(&i18n::tr("disk.kind_guess_tip"));
    }
    // In the suggestions the pill is gone (`AnalysisRow::suggested`), so
    // a TAG tree's declaration rides the name like a hint's owner does.
    if suggested && kind == Some(HitKind::Tag) {
        full.push_str(" — ");
        full.push_str(&i18n::tr("disk.kind_tag_tip"));
    }
    // Annotation, not action: a matching clean-hint rides the tooltip —
    // owner tool plus its own cleanup command, never run by us.
    let hint = cleanhints::lookup(path);
    let app_running = hint.as_ref().is_some_and(|hint| hint.running_in(running));
    if let Some(hint) = &hint {
        full.push_str(" — ");
        full.push_str(&match &hint.command {
            Some(cmd) => t!("disk.hint_cmd", owner = &hint.owner, cmd = cmd).to_string(),
            None => t!("disk.hint_owner", owner = &hint.owner).to_string(),
        });
    }
    // The same posture one step further: what the *system* declares
    // about a MobileAsset row, quoted rather than interpreted. No
    // delete button follows from it — `mobileassetd` owns this content.
    if let Some(note) = asset {
        full.push_str(" — ");
        full.push_str(&asset_clause(note));
    }
    // The Trash is the one directory whose space comes back without
    // judging anything in it: its row's reveal opens the Trash itself —
    // never empties it, that step stays the reader's. (The band at the
    // top says how much is waiting there.)
    let is_trash = env::var_os("HOME").is_some_and(|home| path == Path::new(&home).join(".Trash"));
    let reveal_path = path.to_path_buf();
    let trash_path = path.to_path_buf();
    let open_path = path.to_path_buf();
    let confirm_label = label.clone();

    div()
        .id(SharedString::from(format!("{key}-row")))
        .py(px(4.))
        .px(px(4.))
        .mx(px(-4.))
        // One indent step per level, applied to the row rather than to a
        // wrapper: the hover fill then starts where the row starts, so a
        // nested row still reads as one target.
        .pl(px(4. + indent(depth)))
        .rounded(px(5.))
        .when(expandable, |row| {
            // The hover fill is the affordance (see views/mod.rs — no
            // hand cursor on in-app controls); the chevron says which way
            // the click goes.
            row.hover(|s| s.bg(theme::surface_raised()))
                .on_click(move |_, _window, cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| {
                            state.toggle_expansion(open_path.clone(), cx)
                        });
                })
        })
        .child(
            h_flex()
                .items_center()
                .gap(px(ROW_GAP))
                // Held even when empty (mid-walk rows, file rows), so a
                // label does not move when a finished walk adds chevrons.
                .child(
                    h_flex()
                        .flex_none()
                        .w(px(CHEVRON_SLOT))
                        .children(expandable.then(|| {
                            Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .with_size(Size::Size(px(11.)))
                            .text_color(Hsla::from(theme::text_dim()))
                        })),
                )
                .child(path_label(
                    SharedString::from(format!("{key}-name")),
                    &label,
                    full,
                    ROW_PT,
                ))
                .children(
                    (!suggested)
                        .then(|| basis_pill(&key, kind, hint.as_ref()))
                        .flatten(),
                )
                .children(
                    hint.as_ref()
                        .filter(|_| app_running)
                        .map(|hint| running_pill(&key, &hint.owner)),
                )
                .children(asset.map(|note| asset_pill(&key, note)))
                // A fixed slot whenever the table has a baseline, filled or
                // not: sized to the content, the column wandered with the
                // pills beside it and no two deltas lined up.
                .when_some(delta_tip, |row, tip| {
                    row.child(
                        h_flex()
                            .id(SharedString::from(format!("{key}-delta")))
                            .flex_none()
                            .w(px(DELTA_W))
                            .justify_end()
                            // Quiet on purpose: the sign carries the
                            // meaning, and accent is reserved for
                            // over-threshold (views/mod.rs).
                            .font_family(font::MONO)
                            .text_size(px(META_PT))
                            .text_color(theme::text_muted())
                            .tooltip(widgets::wrap_tooltip(tip))
                            .children(delta_label(bytes, prev_bytes)),
                    )
                })
                .child(size_text(bytes))
                .child(action_slots(
                    Some(
                        // Look-first before remove, same order as every
                        // other row in this window.
                        Button::new(SharedString::from(format!("{key}-reveal")))
                            .icon(IconName::Folder)
                            .ghost()
                            .xsmall()
                            .tooltip(if is_trash {
                                i18n::tr("disk.open_trash")
                            } else {
                                reveal_tip()
                            })
                            .on_click(move |_, _window, cx| {
                                // The row itself opens; the button must not.
                                cx.stop_propagation();
                                if is_trash {
                                    bigfiles::open_trash();
                                } else {
                                    bigfiles::reveal(&reveal_path);
                                }
                            })
                            .into_any_element(),
                    ),
                    deletable.then(|| {
                        Button::new(SharedString::from(format!("{key}-trash")))
                            .icon(CustomIconName::Trash)
                            .ghost()
                            .xsmall()
                            .tooltip(i18n::tr("disk.big_trash"))
                            .on_click({
                                let bytes_str = format::memory(bytes);
                                // A running owner is the one thing the
                                // sheet must add: the move frees nothing
                                // until that app lets go.
                                let caution = hint
                                    .as_ref()
                                    .filter(|_| app_running)
                                    .map(|hint| {
                                        t!("disk.trash_running", owner = &hint.owner).to_string()
                                    })
                                    .unwrap_or_default();
                                move |_, window, cx| {
                                    cx.stop_propagation();
                                    let path = trash_path.clone();
                                    confirm::ask(
                                        window,
                                        cx,
                                        i18n::tr("disk.big_trash_title"),
                                        format!(
                                            "{}{}",
                                            t!(
                                                "disk.ana_trash_body",
                                                name = confirm_label.clone(),
                                                bytes = bytes_str.clone()
                                            ),
                                            caution
                                        ),
                                        i18n::tr("disk.big_trash_ok"),
                                        move |cx| {
                                            let paths = vec![path.clone()];
                                            cx.global::<ZStatsGlobalStore>()
                                                .clone()
                                                .update(cx, |state, cx| {
                                                    state.trash_regenerable(&paths, cx)
                                                });
                                        },
                                    );
                                }
                            })
                            .into_any_element()
                    }),
                )),
        )
        .child(row_meter(bytes as f32 / group_max as f32, 0))
        .into_any_element()
}

// ---- duplicates ---------------------------------------------------------

/// Copies listed under one group before the rest become a count. A
/// picked folder has no size floor, and the same small file can sit in a
/// hundred places; ten rows say "this is everywhere" as well as a
/// hundred would.
const DUPE_FILES_SHOWN: usize = 10;

/// A card header for a tab that has run: the scope it answered in the
/// heading weight, the caption beside it, the method behind an ⓘ, and
/// the run control on the right. The card's own title is gone — it only
/// repeated the tab's name.
fn tab_header(
    scope: String,
    rest: String,
    tip: (&'static str, String),
    controls: AnyElement,
) -> AnyElement {
    header_rows(
        scope,
        h_flex()
            .min_w_0()
            .flex_wrap()
            .items_center()
            .gap_x(px(5.))
            .child(
                div()
                    .min_w_0()
                    .whitespace_normal()
                    .text_color(theme::text_dim())
                    .child(rest),
            )
            .child(widgets::info_icon(tip.0, tip.1))
            .into_any_element(),
        controls,
    )
}

/// A tab's run / re-run / cancel chip, in the same shape as the
/// analyser's.
fn run_chip(id: &'static str, label: String, enabled: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(theme::border())
        .bg(theme::inset())
        .px(px(9.))
        .py(px(2.))
        .text_size(px(META_PT + 1.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(if enabled {
            theme::text()
        } else {
            theme::text_dim()
        })
        .when(enabled, |d| d.hover(|d| d.bg(theme::surface_raised())))
        .child(label)
}

/// The duplicate search's card: the scope row, then either the empty
/// state or the header and the groups, most to free first.
fn dupes_card(state: &ZStatsAppState) -> AnyElement {
    let off = matches!(state.dupe_search(), DupeSearch::Off);
    widgets::list_shell()
        .pt(px(4.))
        .children((!off).then(|| dupes_header(state)))
        .children(dupes_scope_row(state))
        .child(if off {
            empty_state(
                i18n::tr("disk.dup_empty_lead"),
                i18n::tr("disk.dup_empty_cost"),
                i18n::tr("disk.dup_hint"),
                primary_button("dupes-start", i18n::tr("disk.dup_find"), |cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.start_dupes(cx));
                }),
            )
        } else {
            div()
                .px(px(13.))
                .pb(px(11.))
                .child(dupes_body(state))
                .into_any_element()
        })
        .into_any_element()
}

fn dupes_header(state: &ZStatsAppState) -> AnyElement {
    let (scope, rest) = dupes_caption(state);
    let running = matches!(state.dupe_search(), DupeSearch::Running { .. });
    let label = match state.dupe_search() {
        DupeSearch::Running { .. } => i18n::tr("disk.ana_cancel"),
        DupeSearch::Ready(_) => i18n::tr("disk.dup_again"),
        DupeSearch::Off | DupeSearch::Failed(_) => i18n::tr("disk.dup_find"),
    };
    tab_header(
        scope,
        rest,
        ("dupes-basis", i18n::tr("disk.dup_hint")),
        h_flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            // Running stays clickable — it is the cancel, the only way a
            // search stops early, as with the analyser's chip.
            .child(
                run_chip("dupes-chip", label, true).on_click(|_, _window, cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| {
                            if matches!(state.dupe_search(), DupeSearch::Running { .. }) {
                                state.clear_dupes(cx);
                            } else {
                                state.start_dupes(cx);
                            }
                        });
                }),
            )
            // A view action, like the analyser's ✕: drops the list,
            // touches nothing on disk. A search is seconds to repeat, so
            // no confirm.
            .when(!running, |row| {
                row.child(widgets::with_wrap_tooltip(
                    "dupes-dismiss-tip",
                    i18n::tr("disk.dup_dismiss_hint"),
                    Button::new("dupes-dismiss")
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .on_click(|_, _window, cx| {
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| state.clear_dupes(cx));
                        }),
                ))
            })
            .into_any_element(),
    )
}

/// Home, or a picked folder, as the analyser's toolbar chips. Home is
/// run with the header's control, because it is the slow one; a picked
/// folder is searched the moment it is picked — picking is the ask.
/// Dimmed and inert while a search runs: changing what a running search
/// covers goes through cancel.
fn dupes_scope_row(state: &ZStatsAppState) -> Option<AnyElement> {
    let running = matches!(state.dupe_search(), DupeSearch::Running { .. });
    let folder = match state.dupe_scope() {
        DupeScope::Home => None,
        DupeScope::Folder(path) => Some(path.clone()),
    };
    let picked = folder.is_some();
    let pick = Button::new("dupes-pick")
        .icon(IconName::FolderOpen)
        .ghost()
        .xsmall()
        .when(!running, |b| {
            b.on_click(|_, _window, cx| {
                let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some(i18n::tr("disk.dup_pick_go").into()),
                });
                cx.spawn(async move |cx| {
                    if let Ok(Ok(Some(paths))) = rx.await
                        && let Some(folder) = paths.into_iter().next()
                    {
                        cx.update(|cx| {
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| state.search_dupes_in(folder, cx));
                        });
                    }
                })
                .detach();
            })
        });
    Some(
        h_flex()
            .items_center()
            .flex_wrap()
            .gap(px(4.))
            .px(px(13.))
            .pb(px(8.))
            .when(running, |d| d.opacity(0.45))
            .child(
                toolbar_chip("dupes-scope-home", !picked)
                    .tooltip(widgets::wrap_tooltip(i18n::tr("disk.dup_scope_home_tip")))
                    .when(picked && !running, |d| {
                        d.on_click(|_, _window, cx| {
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| state.reset_dupe_scope(cx));
                        })
                    })
                    .child(i18n::tr("disk.ana_preset_home")),
            )
            .children(folder.map(|path| {
                toolbar_chip("dupes-scope-folder", true)
                    .tooltip(widgets::wrap_tooltip(i18n::tr("disk.dup_scope_folder_tip")))
                    .child(
                        div()
                            .max_w(px(220.))
                            .min_w_0()
                            .truncate()
                            .child(format::tilde(&path)),
                    )
            }))
            .child(widgets::with_wrap_tooltip(
                "dupes-pick-tip",
                i18n::tr("disk.dup_pick_hint"),
                pick,
            ))
            .into_any_element(),
    )
}

/// `(scope, the rest)`: progress while running; afterwards the age, what
/// was compared and every honesty counter that is not zero.
fn dupes_caption(state: &ZStatsAppState) -> (String, String) {
    let word = |scope: &DupeScope| match scope {
        DupeScope::Home => i18n::tr("disk.ana_preset_home"),
        DupeScope::Folder(path) => format::tilde(path),
    };
    match state.dupe_search() {
        DupeSearch::Off => (word(state.dupe_scope()), String::new()),
        DupeSearch::Failed(e) => (
            i18n::tr("disk.dup_failed_title"),
            t!("disk.dup_failed", e = e.clone()).to_string(),
        ),
        DupeSearch::Running {
            scope, progress, ..
        } => {
            let what = match progress {
                DupeProgress::Walking { files } => {
                    t!("disk.dup_walking", n = format::thousands(*files)).to_string()
                }
                // "At most": the total assumes every same-size file is read
                // whole, and the 64 KB pass rules most of them out first.
                DupeProgress::Hashing { read, total } => t!(
                    "disk.dup_hashing",
                    read = format::memory(*read),
                    total = format::memory(*total)
                )
                .to_string(),
            };
            (word(scope), what)
        }
        DupeSearch::Ready(result) => {
            let mut parts: Vec<String> = vec![
                format::ago(result.scanned_at.elapsed().unwrap_or_default()),
                t!("disk.ana_took", t = format::took(result.took)).to_string(),
            ];
            let n = format::thousands(result.files_seen);
            parts.push(match result.scope {
                DupeScope::Home => t!("disk.dup_files_home", n = n).to_string(),
                DupeScope::Folder(_) => t!("disk.dup_files", n = n).to_string(),
            });
            if result.unreadable > 0 {
                parts.push(t!("disk.dup_unreadable", n = result.unreadable).to_string());
            }
            (word(&result.scope), parts.join(" · "))
        }
    }
}

/// "Frees 203 MB" — this tab's bold figure is what moving the extra
/// copies gives back, not a size, so it says so; a group of clones says
/// it frees nothing instead of a bold "0 MB".
fn frees_label(bytes: u64) -> AnyElement {
    if bytes == 0 {
        return div()
            .flex_none()
            .text_size(px(META_PT))
            .text_color(theme::text_dim())
            .child(i18n::tr("disk.dup_frees_nothing"))
            .into_any_element();
    }
    h_flex()
        .flex_none()
        .items_baseline()
        .gap(px(5.))
        .child(
            div()
                .text_size(px(META_PT))
                .text_color(theme::text_dim())
                .child(i18n::tr("disk.dup_frees")),
        )
        .child(size_text(bytes))
        .into_any_element()
}

fn dupes_body(state: &ZStatsAppState) -> AnyElement {
    let result = match state.dupe_search() {
        DupeSearch::Off => return div().into_any_element(),
        DupeSearch::Running { .. } => {
            return widgets::note(i18n::tr("disk.dup_running"));
        }
        // The header's caption carries the error.
        DupeSearch::Failed(_) => return widgets::note(i18n::tr("disk.dup_failed_body")),
        DupeSearch::Ready(result) => result,
    };
    if result.groups.is_empty() {
        return widgets::note(i18n::tr("disk.dup_none"));
    }
    let home = env::var("HOME").unwrap_or_default();
    let shown = state.dupes_shown().min(result.groups.len());
    let hidden = result.groups.len() - shown;
    div()
        .child(section_heading(
            t!("disk.dup_groups", n = result.groups.len()).to_string(),
            Some(t!("disk.dup_copies", n = result.extra_copies()).to_string()),
            Some(frees_label(result.reclaimable())),
        ))
        .when(state.dupes_stale(), |d| {
            d.child(
                div()
                    .pb(px(4.))
                    .child(widgets::note(i18n::tr("disk.dup_stale"))),
            )
        })
        .children(
            result.groups[..shown]
                .iter()
                .enumerate()
                .map(|(i, group)| dupe_group(group, &home, i + 1 == shown && hidden == 0)),
        )
        .when(hidden > 0, |d| {
            d.child(
                h_flex().pt(px(6.)).child(
                    more_pill("dupes-more")
                        .child(t!("disk.ana_more", count = hidden).to_string())
                        .on_click(|_, _window, cx| {
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| state.show_more_dupes(cx));
                        }),
                ),
            )
        })
        .into_any_element()
}

/// One group: a name for it, how many and how big, what moving all but
/// one would free — then the copies themselves.
fn dupe_group(group: &DupeGroup, home: &str, last: bool) -> AnyElement {
    let key = row_key("dup", &group.files[0].path);
    // The shortest name among the copies: "Installer.dmg" rather than
    // "Installer (1).dmg", "trip.mov" rather than "trip copy.mov" — the
    // copy's name is usually the original's plus what made it a copy.
    let name = group
        .files
        .iter()
        .filter_map(|f| f.path.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .min_by_key(|n| n.chars().count())
        .unwrap_or_default();
    let frees_tip = if group.reclaimable == 0 {
        i18n::tr("disk.dup_frees_none_tip")
    } else {
        t!(
            "disk.dup_frees_tip",
            bytes = format::memory(group.reclaimable)
        )
        .to_string()
    };
    let hidden = group.files.len().saturating_sub(DUPE_FILES_SHOWN);
    let others = group.files.len() - 1;
    v_flex()
        .py(px(8.))
        .when(!last, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .child(
            h_flex()
                .items_baseline()
                .gap(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(ROW_PT))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .truncate()
                        .child(name),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(META_PT))
                        .text_color(theme::text_dim())
                        .child(
                            t!(
                                "disk.dup_group_meta",
                                n = group.files.len(),
                                size = format::memory(group.len)
                            )
                            .to_string(),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("{key}-frees")))
                        .flex_none()
                        .tooltip(widgets::wrap_tooltip(frees_tip))
                        .child(frees_label(group.reclaimable)),
                ),
        )
        .children(
            group
                .files
                .iter()
                .take(DUPE_FILES_SHOWN)
                .map(|file| dupe_file_row(file, home, others)),
        )
        .when(hidden > 0, |d| {
            d.child(
                div()
                    .pl(px(INDENT_STEP))
                    .pt(px(3.))
                    .text_size(px(META_PT))
                    .text_color(theme::text_dim())
                    .child(t!("disk.dup_more_copies", n = hidden).to_string()),
            )
        })
        .into_any_element()
}

/// One copy: where it is, what it frees if it goes, when it was last
/// changed (the usual way to choose which to keep), Reveal and Trash.
/// Every listed group has two copies or more, so Trash is never the last
/// one — and the store re-checks that against the disk before it moves
/// anything (`DupeGroup::spare`).
fn dupe_file_row(file: &DupeFile, home: &str, others: usize) -> AnyElement {
    let key = row_key("dupf", &file.path);
    let full = format::tilde_path(&file.path.display().to_string(), home);
    let path = file.path.clone();
    let frees = file.frees();
    let confirm_path = full.clone();
    h_flex()
        .items_center()
        .gap(px(ROW_GAP))
        .pl(px(INDENT_STEP))
        .pt(px(4.))
        .child(path_label(
            SharedString::from(format!("{key}-path")),
            &full,
            full.clone(),
            ROW_PT,
        ))
        .when(file.shares_storage, |d| {
            d.child(row_pill(
                SharedString::from(format!("{key}-clone")),
                i18n::tr("disk.dup_clone"),
                i18n::tr("disk.dup_clone_tip"),
                theme::text_muted(),
            ))
        })
        .when(file.linked, |d| {
            d.child(row_pill(
                SharedString::from(format!("{key}-linked")),
                i18n::tr("disk.dup_linked"),
                i18n::tr("disk.dup_linked_tip"),
                theme::text_muted(),
            ))
        })
        .children(file.modified.map(|at| {
            let day = format::date(at);
            div()
                .id(SharedString::from(format!("{key}-modified")))
                .flex_none()
                .font_family(font::MONO)
                .text_size(px(META_PT))
                .text_color(theme::text_dim())
                .tooltip(widgets::wrap_tooltip(
                    t!("disk.dup_modified", date = day.clone()).to_string(),
                ))
                .child(day)
        }))
        .child(action_slots(
            Some(
                Button::new(SharedString::from(format!("{key}-reveal")))
                    .icon(IconName::Folder)
                    .ghost()
                    .xsmall()
                    .tooltip(reveal_tip())
                    .on_click({
                        let path = file.path.clone();
                        move |_, _window, _cx| bigfiles::reveal(&path)
                    })
                    .into_any_element(),
            ),
            Some(
                Button::new(SharedString::from(format!("{key}-trash")))
                    .icon(CustomIconName::Trash)
                    .ghost()
                    .xsmall()
                    .tooltip(i18n::tr("disk.dup_trash"))
                    .on_click(move |_, window, cx| {
                        let path = path.clone();
                        let mut body = t!(
                            "disk.dup_trash_body",
                            path = confirm_path.clone(),
                            n = others
                        )
                        .to_string();
                        if !frees {
                            body.push(' ');
                            body.push_str(&i18n::tr("disk.dup_trash_nothing"));
                        }
                        confirm::ask(
                            window,
                            cx,
                            i18n::tr("disk.dup_trash_title"),
                            body,
                            i18n::tr("disk.big_trash_ok"),
                            move |cx| {
                                let path = path.clone();
                                cx.global::<ZStatsGlobalStore>()
                                    .clone()
                                    .update(cx, |state, cx| state.trash_dupe(&path, cx));
                            },
                        );
                    })
                    .into_any_element(),
            ),
        ))
        .into_any_element()
}

/// The index query's card: the empty state before the first query, the
/// header and the rows after it.
fn big_files_card(state: &ZStatsAppState) -> AnyElement {
    if matches!(state.big_files(), BigFiles::Off) {
        return widgets::list_shell()
            .pt(px(4.))
            .child(empty_state(
                i18n::tr("disk.big_empty_lead"),
                i18n::tr("disk.big_empty_cost"),
                i18n::tr("disk.big_hint"),
                primary_button("bigfiles-start", i18n::tr("disk.big_scan"), |cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.start_big_files(cx));
                }),
            ))
            .into_any_element();
    }
    // A finished listing is twenty rows deep; the ✕ is how you put it
    // away again — only for a *finished* one, because `mdfind` is spawned
    // without a cancel and a control that cannot stop what it points at
    // would be a lie.
    let answered = matches!(
        state.big_files(),
        BigFiles::Ready { .. } | BigFiles::Failed { .. }
    );
    let running = matches!(state.big_files(), BigFiles::Running);
    let label = match state.big_files() {
        BigFiles::Running => i18n::tr("disk.big_scanning"),
        BigFiles::Ready { .. } => i18n::tr("disk.big_rescan"),
        BigFiles::Off | BigFiles::Failed { .. } => i18n::tr("disk.big_scan"),
    };
    let controls = h_flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .child(
            run_chip("bigfiles-scan", label, !running).when(!running, |d| {
                d.on_click(|_, _window, cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.start_big_files(cx));
                })
            }),
        )
        .when(answered, |row| {
            row.child(widgets::with_wrap_tooltip(
                "bigfiles-dismiss-tip",
                i18n::tr("disk.big_dismiss_hint"),
                Button::new("bigfiles-dismiss")
                    .icon(IconName::Close)
                    .ghost()
                    .xsmall()
                    .on_click(|_, _window, cx| {
                        cx.global::<ZStatsGlobalStore>()
                            .clone()
                            .update(cx, |state, cx| state.clear_big_files(cx));
                    }),
            ))
        })
        .into_any_element();
    widgets::list_shell()
        .pt(px(4.))
        .child(tab_header(
            // `mdfind -onlyin $HOME`: the query's scope, said as the
            // analyser and the duplicate search say theirs.
            i18n::tr("disk.ana_preset_home"),
            big_files_caption(state),
            ("big-files-basis", i18n::tr("disk.big_hint")),
            controls,
        ))
        .child(div().px(px(13.)).pb(px(11.)).child(big_files_body(state)))
        .into_any_element()
}

/// The threshold the rows clear, how many there are, and what "new"
/// is measured against.
fn big_files_caption(state: &ZStatsAppState) -> String {
    let BigFiles::Ready { scan, added, since } = state.big_files() else {
        return match state.big_files() {
            BigFiles::Running => i18n::tr("disk.big_running"),
            _ => String::new(),
        };
    };
    // The bar describes what the rows actually show — the smallest
    // displayed PHYSICAL size, floored to a clean step. Quoting the query
    // threshold here was a lie in both directions: sparse files enter on
    // logical size and display far below it.
    let bar = display_bar(scan.files.iter().map(|f| f.size).min().unwrap_or(0));
    let mut parts = vec![if bar == 0 {
        t!("disk.big_count_plain", count = scan.total).to_string()
    } else {
        t!(
            "disk.big_count",
            thr = format::memory(bar),
            count = scan.total
        )
        .to_string()
    }];
    if scan.threshold == bigfiles::FALLBACK_THRESHOLD {
        parts.push(i18n::tr("disk.big_fallback_note"));
    }
    if scan.total > scan.files.len() {
        parts.push(t!("disk.big_shown", shown = scan.files.len()).to_string());
    }
    // What "new" means, in the one place it can be checked: the run it
    // is measured against. Without this the marks would be a claim about
    // an unnamed past.
    if let Some(since) = since {
        parts.push(
            t!(
                "disk.big_since",
                ago = format::ago(since.elapsed().unwrap_or_default())
            )
            .to_string(),
        );
        if !added.is_empty() {
            parts.push(t!("disk.big_added", count = added.len()).to_string());
        }
    }
    parts.join(" · ")
}

fn big_files_body(state: &ZStatsAppState) -> AnyElement {
    match state.big_files() {
        // The empty state replaces the card body before the first query.
        BigFiles::Off | BigFiles::Running => div().into_any_element(),
        BigFiles::Failed { indexing_off: true } => widgets::note(i18n::tr("disk.big_index_off")),
        BigFiles::Failed {
            indexing_off: false,
        } => widgets::note(i18n::tr("disk.big_failed")),
        BigFiles::Ready { scan, .. } if scan.files.is_empty() => {
            widgets::note(i18n::tr("disk.big_none"))
        }
        BigFiles::Ready { scan, added, since } => {
            let since_label = since.map(|t| format::ago(t.elapsed().unwrap_or_default()));
            let max = scan.files.iter().map(|f| f.size).max().unwrap_or(1).max(1);
            let home = env::var("HOME").unwrap_or_default();
            div()
                .children(scan.files.iter().map(|f| {
                    big_file_row(
                        f,
                        max,
                        &home,
                        added
                            .contains(&f.path)
                            .then(|| since_label.clone())
                            .flatten(),
                    )
                }))
                .into_any_element()
        }
    }
}

/// Floor a size to the step a caption can claim with a straight face:
/// 100 MB granularity above 100 MB (220 → 200), 10 MB in the tens,
/// 1 MB in the ones. Below 1 MB returns 0 — the caller drops the "≥"
/// clause entirely rather than round a sparse sliver up into a lie.
fn display_bar(bytes: u64) -> u64 {
    const MIB: u64 = 1024 * 1024;
    let step = if bytes >= 100 * MIB {
        100 * MIB
    } else if bytes >= 10 * MIB {
        10 * MIB
    } else {
        MIB
    };
    (bytes / step) * step
}

/// One large file: where it is (the two-part path every row in this
/// window uses — a bare name made four rows of "snapshot"), whether it
/// is new since the last listing, its size, Reveal and Trash.
///
/// `new_since` carries the baseline's age when this row is one the
/// previous listing would have shown and did not — the pill says "new",
/// its tooltip says since when. `None` covers both "was there before"
/// and "cannot tell", which the caption's absence of a baseline line
/// already explains.
fn big_file_row(
    file: &bigfiles::BigFile,
    max: u64,
    home: &str,
    new_since: Option<String>,
) -> AnyElement {
    let key = row_key("bigfile", &file.path);
    let shown = format::tilde_path(&file.path.display().to_string(), home);
    let mut full = shown.clone();
    // Sparse and compressed files qualify by logical size but display
    // physical — without both figures a "300 MB" row under a "≥ 500 MB"
    // caption reads as the list breaking its own bar.
    if format::memory(file.size) != format::memory(file.logical) {
        full.push_str(" — ");
        full.push_str(
            t!(
                "disk.big_sizes",
                phys = format::memory(file.size),
                logical = format::memory(file.logical)
            )
            .as_ref(),
        );
    }
    let path = file.path.clone();
    // The whole location, in the sentence the reader confirms: these are
    // their files, not caches that rebuild.
    let confirm_path = shown.clone();
    v_flex()
        .py(px(5.))
        .child(
            h_flex()
                .items_center()
                .gap(px(ROW_GAP))
                .child(path_label(
                    SharedString::from(format!("{key}-name")),
                    &shown,
                    full,
                    ROW_PT,
                ))
                .children(new_since.map(|ago| {
                    // Neutral, like the analyser's pills: accent is for
                    // over-threshold, and a new file is news, not a
                    // problem.
                    row_pill(
                        SharedString::from(format!("{key}-new")),
                        i18n::tr("disk.big_new"),
                        t!("disk.big_new_tip", ago = ago).to_string(),
                        theme::text_muted(),
                    )
                }))
                .child(size_text(file.size))
                .child(action_slots(
                    // Navigation, not an action — no confirm, just Finder
                    // with the file selected. Left of the destructive
                    // control, so "look first" comes before "remove".
                    Some(
                        Button::new(SharedString::from(format!("{key}-reveal")))
                            .icon(IconName::Folder)
                            .ghost()
                            .xsmall()
                            .tooltip(reveal_tip())
                            .on_click({
                                let path = file.path.clone();
                                move |_, _window, _cx| bigfiles::reveal(&path)
                            })
                            .into_any_element(),
                    ),
                    // An explicit control with confirm, and the request is
                    // Finder's own recoverable move-to-Trash — never a
                    // direct unlink.
                    Some(
                        Button::new(SharedString::from(format!("{key}-trash")))
                            .icon(CustomIconName::Trash)
                            .ghost()
                            .xsmall()
                            .tooltip(i18n::tr("disk.big_trash"))
                            .on_click(move |_, window, cx| {
                                let path = path.clone();
                                confirm::ask(
                                    window,
                                    cx,
                                    i18n::tr("disk.big_trash_title"),
                                    t!("disk.big_trash_body", name = confirm_path.clone())
                                        .to_string(),
                                    i18n::tr("disk.big_trash_ok"),
                                    move |cx| {
                                        let path = path.clone();
                                        cx.global::<ZStatsGlobalStore>()
                                            .clone()
                                            .update(cx, |state, cx| {
                                                state.trash_big_file(&path, cx)
                                            });
                                    },
                                );
                            })
                            .into_any_element(),
                    ),
                )),
        )
        .child(
            div()
                .mt(px(3.))
                .mr(px(ACTION_SLOT * 2. + ROW_GAP))
                .child(widgets::meter(
                    file.size as f32 / max as f32,
                    meter_ink(),
                    2.,
                )),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::slice;

    /// An opened row may only stay silent about its children when there
    /// are none left to mention — the summary line is what keeps the cut
    /// at [`EXPAND_ROWS`] visible.
    #[test]
    fn an_opened_row_accounts_for_the_children_it_does_not_list() {
        let hit = |bytes| DirHit {
            path: PathBuf::from(format!("/r/{bytes}")),
            bytes,
            kind: HitKind::Plain,
            asset: None,
        };
        let big = |n: u64| hit(n * EXPAND_FLOOR);
        let rows: Vec<DirHit> = (1..=EXPAND_ROWS as u64 + 3).rev().map(big).collect();
        let (hidden, bytes) = expand_hidden(&rows).expect("three past the cut");
        assert_eq!(hidden, 3);
        // The three past the cut, and only those.
        assert_eq!(bytes, (1 + 2 + 3) * EXPAND_FLOOR);
        // Exactly the cut, and under it, say nothing.
        assert_eq!(expand_hidden(&rows[..EXPAND_ROWS]), None);
        assert_eq!(expand_hidden(&[]), None);

        // A child too small to render as anything but "0 MB" is summed
        // into the line below instead of spending a row on a zero.
        let mixed = vec![big(4), big(2), hit(900_000), hit(400_000)];
        assert_eq!(expand_shown(&mixed), 2, "the two that can be read");
        let (hidden, bytes) = expand_hidden(&mixed).expect("two below the floor");
        assert_eq!(hidden, 2);
        assert_eq!(bytes, 1_300_000);
    }

    #[test]
    fn display_bar_floors_to_a_clean_step() {
        const MIB: u64 = 1024 * 1024;
        assert_eq!(display_bar(220 * MIB), 200 * MIB);
        assert_eq!(display_bar(1433 * MIB), 1400 * MIB);
        assert_eq!(display_bar(95 * MIB), 90 * MIB);
        assert_eq!(display_bar(5 * MIB + 1), 5 * MIB);
        // Below a megabyte the caption drops its claim instead of lying.
        assert_eq!(display_bar(500 * 1024), 0);
    }

    /// Every clause is a field the plist actually carried; the closing
    /// line is always there, because "do not delete this by hand" is
    /// the part a reader most needs and the one thing not read off
    /// disk.
    #[test]
    fn the_asset_clause_quotes_only_what_was_declared() {
        let bare = diskscan::AssetNote {
            kind: "Font8".into(),
            required_by_os: None,
            never_collected: None,
            locale: None,
        };
        let text = asset_clause(&bare);
        assert!(text.contains("Font8"));
        assert!(!text.contains("language"), "{text}");
        let full = diskscan::AssetNote {
            kind: "VoiceServices".into(),
            required_by_os: Some(true),
            never_collected: Some(true),
            locale: Some("zh_Hans".into()),
        };
        let text = asset_clause(&full);
        assert!(text.contains("zh_Hans"), "{text}");
        // A declared `false` is not a claim either way — only `true`
        // earns a clause.
        let denied = diskscan::AssetNote {
            kind: "X".into(),
            required_by_os: Some(false),
            never_collected: Some(false),
            locale: None,
        };
        let text = asset_clause(&denied);
        assert!(!text.contains("required"), "{text}");
    }

    #[test]
    fn a_scope_is_named_the_way_its_chip_is() {
        let home = diskscan::default_root().expect("HOME is set under cargo test");
        assert_eq!(scope_word(slice::from_ref(&home), &home), "Home");
        assert_eq!(
            scope_word(&ScanScope::whole_disk().roots, &diskscan::whole_disk_root()),
            "Whole disk"
        );
        let library = home.join("Library");
        assert_eq!(scope_word(slice::from_ref(&library), &library), "~/Library");
    }

    #[test]
    fn delta_speaks_only_when_it_clears_the_floor() {
        let gib = 1024 * 1024 * 1024;
        // Growth and shrinkage, signed.
        assert_eq!(delta_label(3 * gib, Some(2 * gib)), Some("+1.0 GB".into()));
        assert_eq!(delta_label(2 * gib, Some(3 * gib)), Some("-1.0 GB".into()));
        // No baseline row → silence, not "new".
        assert_eq!(delta_label(3 * gib, None), None);
        // Under the floor either way → unchanged for table purposes.
        assert_eq!(delta_label(gib + DIFF_FLOOR - 1, Some(gib)), None);
        assert_eq!(delta_label(gib, Some(gib)), None);
    }
}
