//! The disk-space window: the Spotlight large-file query and the
//! directory analyser, in a standard window of their own.
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
//! Nothing here owns state. Both features live in `ZStatsAppState`, this
//! window observes the same store the panel does, and every action
//! (start, cancel, open a row, trash) goes through the methods the
//! panel's chips called — so a scan started here keeps running with
//! the window closed, exactly as it did with the panel hidden.

use super::widgets;
use crate::bigfiles;
use crate::cleanhints;
use crate::confirm;
use crate::diskscan::{self, DiffBaseline, DirHit, FileHit, HitKind, ScanResult};
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{BigFiles, DiskAnalysis, Expansion, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px, relative,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Icon, IconName, Sizable, Size, h_flex};
use rust_i18n::t;
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The window body, in the order the two answers should be reached for:
/// the index query first (seconds, no permission prompts, and often
/// answer enough), the walk second (minutes, and where you go when the
/// index saw nothing).
pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    vec![big_files_card(state), analysis_card(state)]
}

/// The analyser card: header, scope row, then whatever the current run
/// has produced.
///
/// No fold, unlike the panel card this replaces. That control existed
/// because three tables buried the volumes and sensors below them; in a
/// window of its own there is nothing underneath to bury, and a fold
/// would only hide the reason the window was opened.
fn analysis_card(state: &ZStatsAppState) -> AnyElement {
    let body =
        match state.disk_analysis() {
            // Nothing asked yet. The sentence explaining what a walk costs
            // and what it may prompt for was a tooltip on the start chip —
            // findable only by hovering the one control that commits you to
            // it. Here it is simply the card's body.
            DiskAnalysis::Off => div()
                .px(px(13.))
                .pb(px(11.))
                .child(widgets::note(i18n::tr("disk.ana_hint")))
                .into_any_element(),
            // The live progress line lives in the header caption; the body is
            // the partial tables — same renderer as the final result, figures
            // are lower bounds that only grow. No delete controls mid-scan:
            // the walker may still be inside any of these trees. No deltas
            // either — comparing a lower bound against a finished run would
            // read as shrinkage.
            DiskAnalysis::Running { partial, .. } => div()
                .children(partial.as_ref().map(|r| {
                    analysis_tables(state, r, false, state.analysis_show_all_dirs(), None)
                }))
                .into_any_element(),
            DiskAnalysis::Failed(e) => div()
                .child(
                    div()
                        .px(px(13.))
                        .pt(px(2.))
                        .pb(px(11.))
                        .child(widgets::note(
                            t!("disk.ana_failed", e = e.clone()).to_string(),
                        )),
                )
                .into_any_element(),
            DiskAnalysis::Ready(result) => div()
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
        .child(analysis_header(state))
        .children(analysis_scope_row(state))
        .child(body)
        .into_any_element()
}

/// Title, the start/cancel control, and the caption that says what the
/// current result is. The title sits on its own row: the caption is a
/// sentence with counters in it and has never fitted a header's right
/// slot.
fn analysis_header(state: &ZStatsAppState) -> AnyElement {
    let caption = analysis_caption(state);
    let asked = !matches!(state.disk_analysis(), DiskAnalysis::Off);
    div()
        .px(px(13.))
        .pt(px(11.))
        .pb(px(9.))
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(i18n::tr("disk.ana_title")),
                )
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(4.))
                        .child(analysis_chip(state))
                        // A view action: drops the whole result (and the
                        // retained index every opened row is served
                        // from), touches nothing on disk. Cancels
                        // the walk too if one is still running. Icon-only
                        // — the tooltip carries the words, and Close (not
                        // Delete) keeps it visually apart from the
                        // file-trashing controls below. Nothing to drop
                        // before the first run, so nothing to show.
                        .when(asked, |row| {
                            row.child(widgets::with_wrap_tooltip(
                                "ana-dismiss-tip",
                                i18n::tr("disk.ana_dismiss_hint"),
                                Button::new("ana-dismiss")
                                    .icon(IconName::Close)
                                    .ghost()
                                    .xsmall()
                                    .on_click(|_, _window, cx| {
                                        cx.global::<ZStatsGlobalStore>()
                                            .clone()
                                            .update(cx, |state, cx| state.clear_disk_analysis(cx));
                                    }),
                            ))
                        }),
                ),
        )
        .when(!caption.is_empty(), |d| {
            d.child(
                div()
                    .mt(px(3.))
                    .min_w_0()
                    .text_size(px(10.))
                    .line_height(relative(1.35))
                    .text_color(theme::text_dim())
                    .whitespace_normal()
                    .child(caption),
            )
        })
        .children(fda_hint(state))
        .into_any_element()
}

/// Trigger / cancel for the directory analyser. Unlike the large-file
/// chip, Running stays clickable — it IS the explicit cancel, the only
/// way a walk stops early (closing this window deliberately does not,
/// see state.rs).
fn analysis_chip(state: &ZStatsAppState) -> AnyElement {
    let running = matches!(state.disk_analysis(), DiskAnalysis::Running { .. });
    let label = if running {
        i18n::tr("disk.ana_cancel")
    } else if matches!(state.disk_analysis(), DiskAnalysis::Ready(_)) {
        i18n::tr("disk.ana_rescan")
    } else {
        i18n::tr("disk.ana_scan")
    };
    div()
        .id("diskscan-chip")
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(theme::border())
        .bg(theme::inset())
        .px(px(8.))
        .py(px(2.))
        .tooltip(widgets::wrap_tooltip(i18n::tr("disk.ana_hint")))
        .text_size(px(10.))
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

/// Pick a folder to analyze instead of the default home tree — the
/// native directory panel, and choosing starts the walk immediately.
/// The panel takes key focus while it is up; this window survives that
/// (it has a title bar, not the popover's hide-on-blur), and the walk
/// would survive it either way.
fn analysis_pick_chip() -> AnyElement {
    let button = Button::new("ana-pick")
        .icon(IconName::FolderOpen)
        .ghost()
        .xsmall()
        .on_click(|_, _window, cx| {
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
                            .update(cx, |state, cx| state.start_disk_analysis_at(root, cx));
                    });
                }
            })
            .detach();
        });
    // A sentence, not a label — it needs the wrapping tooltip a Button
    // cannot build for itself.
    widgets::with_wrap_tooltip("ana-pick-tip", i18n::tr("disk.ana_pick_hint"), button)
}

/// The "analysis scope" row: a dim label, the preset chips, and the
/// folder picker. `None` while a walk runs — the header's cancel chip is
/// the only control then.
///
/// The label carries the tooltip that says what a scope may be, and that
/// the whole volume is deliberately not one of the options: the reason
/// (firmlinks double-count, /System, TCC) belongs where someone looks
/// for the missing choice, not only in the error they get after picking
/// `/` in the folder panel.
fn analysis_scope_row(state: &ZStatsAppState) -> Option<AnyElement> {
    if matches!(state.disk_analysis(), DiskAnalysis::Running { .. }) {
        return None;
    }
    Some(
        h_flex()
            .items_center()
            .gap(px(4.))
            .px(px(13.))
            .pb(px(8.))
            .child(
                div()
                    .id("ana-scope-label")
                    .flex_none()
                    .text_size(px(10.))
                    .text_color(theme::text_dim())
                    .tooltip(widgets::wrap_tooltip(i18n::tr("disk.ana_scope_tip")))
                    .child(i18n::tr("disk.ana_scope_label")),
            )
            .children(analysis_preset_chips())
            .child(analysis_pick_chip())
            .into_any_element(),
    )
}

/// One-click preset scopes (docs/disk-analysis.md's scope table): the
/// home tree, `~/Library` — the blind-spot close-up — and the merged
/// cache roots. Clicking starts the walk immediately, same contract as
/// the picker.
///
/// `~` is a chip even though it is also the default, because a scope
/// sticks once picked: after a `~/Library` run, "re-analyze" means
/// `~/Library` this launch and the next. Without this chip the only way
/// back was the ✕, which also deletes the cached result and the Δ
/// baseline — a heavy price for changing your mind about scope.
fn analysis_preset_chips() -> Vec<AnyElement> {
    let chip = |id: &'static str,
                label: String,
                tip: String,
                go: fn(&mut ZStatsAppState, &mut gpui::Context<ZStatsAppState>)|
     -> AnyElement {
        div()
            .id(id)
            .flex_none()
            .rounded(px(4.))
            .px(px(5.))
            .py(px(1.))
            .text_size(px(10.))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme::text_muted())
            .hover(|d| d.bg(theme::surface_raised()).text_color(theme::text()))
            .tooltip(widgets::wrap_tooltip(tip))
            .on_click(move |_, _window, cx| {
                cx.global::<ZStatsGlobalStore>().clone().update(cx, go);
            })
            .child(label)
            .into_any_element()
    };
    vec![
        chip(
            "ana-preset-home",
            // A word, not the path: `~` alone is a one-character chip
            // that means nothing outside a terminal, and it is the one
            // scope every reader needs to be able to find. The exact
            // path rides the tooltip, where `~/Library` beside it keeps
            // its own — that one is a specific subfolder, and naming it
            // "Library" would collide with /Library and /System/Library.
            i18n::tr("disk.ana_preset_home"),
            i18n::tr("disk.ana_preset_home_tip"),
            |state, cx| {
                if let Some(home) = diskscan::default_root() {
                    state.start_disk_analysis_at(home, cx);
                }
            },
        ),
        chip(
            "ana-preset-library",
            "~/Library".into(),
            i18n::tr("disk.ana_preset_library_tip"),
            |state, cx| {
                if let Some(home) = diskscan::default_root() {
                    state.start_disk_analysis_at(home.join("Library"), cx);
                }
            },
        ),
        chip(
            "ana-preset-caches",
            i18n::tr("disk.ana_preset_caches"),
            i18n::tr("disk.ana_preset_caches_tip"),
            |state, cx| state.start_disk_analysis_caches(cx),
        ),
    ]
}
/// When permission gaps hid part of the tree, say so and offer the one
/// switch that covers them all. macOS 15+ gates every other app's
/// container behind its own per-app prompt; Full Disk Access supersedes
/// the whole category — the standard, proportionate ask for a disk
/// scanner. Tied to `skipped_denied` only: the TCC deny-list skips are
/// deliberate zero-touch and no permission would change them.
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
/// Results older than this get a "consider re-analyzing" nudge appended
/// to the caption. Display only, like every threshold in views/ —
/// nothing refreshes itself: a minutes-long walk must never
/// self-trigger, so a nudge is where staleness honesty ends. A day is
/// when "the numbers are from earlier" stops going without saying —
/// mostly reached through the persisted cache surviving a restart.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Age, how many directories were walked, and every honesty counter
/// that is non-zero. The scan root is omitted when it is just `~` —
/// that is this card's default, and it burned four characters for
/// nothing next to the title.
fn analysis_caption(state: &ZStatsAppState) -> String {
    let result = match state.disk_analysis() {
        DiskAnalysis::Ready(result) => result,
        // While a walk runs the caption is the progress, led by the
        // scope — someone returning mid-run must not have to guess
        // whether the walker is in the whole home tree or one preset.
        // "~" goes without saying, like the finished caption.
        DiskAnalysis::Running {
            dirs_done, scope, ..
        } => {
            let progress = t!("disk.ana_running", dirs = format::thousands(*dirs_done)).to_string();
            let home = env::var("HOME").unwrap_or_default();
            let root = scope_display(&scope.roots, &scope.base, &home);
            return if root == "~" {
                progress
            } else {
                format!("{root} · {progress}")
            };
        }
        DiskAnalysis::Failed(e) => {
            return t!("disk.ana_failed", e = e.clone()).to_string();
        }
        DiskAnalysis::Off => return String::new(),
    };
    let home = env::var("HOME").unwrap_or_default();
    let age = result.scanned_at.elapsed().unwrap_or_default();
    let mut extras = Vec::new();
    if result.skipped_protected > 0 {
        extras.push(t!("disk.ana_skip_protected", n = result.skipped_protected).to_string());
    }
    if result.skipped_denied > 0 {
        extras.push(t!("disk.ana_skip_denied", n = result.skipped_denied).to_string());
    }
    if result.skipped_dataless > 0 {
        extras.push(t!("disk.ana_skip_dataless", n = result.skipped_dataless).to_string());
    }
    // Names what the per-row ± compares against. Its absence when no
    // row moved is itself the answer: nothing big changed.
    if let Some(diff) = state.analysis_diff_for(result) {
        extras.push(
            t!(
                "disk.ana_diff_base",
                ago = format::ago(diff.scanned_at().elapsed().unwrap_or_default())
            )
            .to_string(),
        );
    }
    if age > STALE_AFTER {
        extras.push(i18n::tr("disk.ana_stale"));
    }
    analysis_caption_parts(
        &scope_display(&result.roots, &result.root, &home),
        &home,
        format::ago(age),
        t!("disk.ana_took", t = format::took(result.took)).to_string(),
        t!(
            "disk.ana_dirs_seen",
            n = format::thousands(result.dirs_seen)
        )
        .to_string(),
        extras,
    )
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
            .map(|r| tilde_path(&r.display().to_string(), home))
            .collect::<Vec<_>>()
            .join(" + ")
    } else {
        tilde_path(&base.display().to_string(), home)
    }
}

fn analysis_caption_parts(
    root_display: &str,
    home: &str,
    ago: String,
    took: String,
    dirs_seen: String,
    extras: Vec<String>,
) -> String {
    let root = tilde_path(root_display, home);
    let mut parts = Vec::new();
    if root != "~" {
        parts.push(root);
    }
    parts.push(ago);
    parts.push(took);
    parts.push(dirs_seen);
    parts.extend(extras);
    parts.join(" · ")
}

fn analysis_tables(
    state: &ZStatsAppState,
    result: &ScanResult,
    actions: bool,
    show_all_dirs: bool,
    diff: Option<&DiffBaseline>,
) -> AnyElement {
    let root = result.root.clone();
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
                    group_max: max,
                    root: &root,
                    deletable: false,
                    expandable: false,
                    open: false,
                    depth: 0,
                })
            })
            .collect()
    };

    let section =
        |title: String, rows: Vec<AnyElement>, control: Option<AnyElement>| -> Option<AnyElement> {
            if rows.is_empty() {
                return None;
            }
            Some(
                div()
                    .px(px(13.))
                    .pb(px(8.))
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .pb(px(4.))
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme::text_dim())
                                    .child(title),
                            )
                            .children(control),
                    )
                    .children(rows)
                    .into_any_element(),
            )
        };

    // The head of the suggestion set; the title carries the full count
    // and total, and the bulk button acts on the whole set — honest,
    // because unlike the capped tables the full list is retained.
    let sug_head = &result.suggestions[..result.suggestions.len().min(diskscan::TABLE_CAP)];
    let sug_total: u64 = result.suggestions.iter().map(|d| d.bytes).sum();
    div()
        .children(section(
            t!(
                "disk.sug_title",
                n = result.suggestions.len(),
                bytes = format::memory(sug_total)
            )
            .to_string(),
            dir_rows(sug_head, "ana-sug", actions),
            actions.then(|| suggest_clear_button(&result.suggestions, sug_total)),
        ))
        .children({
            // Default 8–10 rows; "show more" reveals everything retained
            // (up to TABLE_KEEP). The chip states how many are hidden.
            let shown = if show_all_dirs {
                result.dirs.len()
            } else {
                diskscan::default_rows(&result.dirs, |d| d.bytes)
            };
            let hidden = result.dirs.len() - shown;
            section(
                i18n::tr("disk.ana_dirs"),
                dir_rows(&result.dirs[..shown], "ana-dir", false),
                (hidden > 0 || show_all_dirs).then(|| more_chip(hidden, show_all_dirs)),
            )
        })
        .children(section(
            i18n::tr("disk.ana_files"),
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
/// plus hint-trashable caches), not just the rendered head; the confirm
/// restates the count and total so nothing moves that was not announced.
fn suggest_clear_button(hits: &[DirHit], total: u64) -> AnyElement {
    let n = hits.len();
    let paths: Vec<PathBuf> = hits.iter().map(|h| h.path.clone()).collect();
    Button::new("ana-sug-clear")
        .icon(IconName::Delete)
        .ghost()
        .xsmall()
        .label(i18n::tr("disk.sug_clear"))
        .on_click(move |_, window, cx| {
            let paths = paths.clone();
            confirm::ask(
                window,
                cx,
                i18n::tr("disk.sug_clear_title"),
                t!("disk.sug_clear_body", n = n, bytes = format::memory(total)).to_string(),
                i18n::tr("disk.big_trash_ok"),
                move |cx| {
                    let paths = paths.clone();
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.trash_regenerable(&paths, cx));
                },
            );
        })
        .into_any_element()
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
        group_max,
        root: parent,
        deletable: ctx.deletable,
        expandable: ctx.expandable,
        open,
        depth,
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
            for child in rows.iter().take(EXPAND_ROWS) {
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

/// What an open row is not listing: how many children past [`EXPAND_ROWS`]
/// and how much they add up to, or `None` when everything is shown.
/// Counts what the tables retained, exactly like the dirs table's own
/// "show more" — neither claims to have seen every directory on disk.
fn expand_hidden(rows: &[DirHit]) -> Option<(usize, u64)> {
    let hidden = rows.len().checked_sub(EXPAND_ROWS).filter(|n| *n > 0)?;
    Some((hidden, rows.iter().skip(EXPAND_ROWS).map(|r| r.bytes).sum()))
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
fn kind_pill(key: &SharedString, kind: HitKind) -> Option<AnyElement> {
    if kind != HitKind::Tag {
        return None;
    }
    Some(
        div()
            .id(SharedString::from(format!("{key}-kind")))
            .flex_none()
            .rounded_full()
            .px(px(5.))
            .text_size(px(9.))
            .bg(theme::inset())
            .text_color(theme::text_muted())
            .tooltip(widgets::wrap_tooltip(i18n::tr("disk.kind_tag_tip")))
            .child(i18n::tr("disk.kind_tag"))
            .into_any_element(),
    )
}

/// The dirs section's fold: "show more · N" ↔ "show less".
fn more_chip(hidden: usize, show_all: bool) -> AnyElement {
    div()
        .id("ana-dirs-more")
        .flex_none()
        .rounded(px(4.))
        .px(px(6.))
        .py(px(1.))
        .text_size(px(10.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .hover(|d| d.bg(theme::surface_raised()).text_color(theme::text()))
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
}

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

fn analysis_row(row: AnalysisRow) -> AnyElement {
    let AnalysisRow {
        key,
        path,
        bytes,
        prev_bytes,
        delta_tip,
        kind,
        group_max,
        root,
        deletable,
        expandable,
        open,
        depth,
    } = row;
    let label = path
        .strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string());
    let mut full = tilde_path(
        &path.display().to_string(),
        &env::var("HOME").unwrap_or_default(),
    );
    // A heuristic fold explains itself here rather than with a pill.
    if kind == Some(HitKind::Heuristic) {
        full.push_str(" — ");
        full.push_str(&i18n::tr("disk.kind_guess_tip"));
    }
    // Annotation, not action: a matching clean-hint rides the tooltip —
    // owner tool plus its own cleanup command, never run by us.
    if let Some(hint) = cleanhints::lookup(path) {
        full.push_str(" — ");
        full.push_str(&match &hint.command {
            Some(cmd) => t!("disk.hint_cmd", owner = &hint.owner, cmd = cmd).to_string(),
            None => t!("disk.hint_owner", owner = &hint.owner).to_string(),
        });
    }
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
                .justify_between()
                .gap(px(8.))
                .children(expandable.then(|| {
                    Icon::new(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .with_size(Size::Size(px(11.)))
                    .text_color(Hsla::from(theme::text_dim()))
                }))
                .child(
                    div()
                        .id(SharedString::from(format!("{key}-name")))
                        .flex_1()
                        .min_w_0()
                        .text_size(px(11.))
                        .text_color(theme::text())
                        .truncate()
                        .tooltip(widgets::wrap_tooltip(full))
                        .child(label),
                )
                .children(kind.and_then(|kind| kind_pill(&key, kind)))
                .children(delta_label(bytes, prev_bytes).map(|delta| {
                    // Quiet on purpose: the sign carries the meaning, and
                    // accent is reserved for over-threshold (views/mod.rs).
                    div()
                        .id(SharedString::from(format!("{key}-delta")))
                        .flex_none()
                        .font_family(font::MONO)
                        .text_size(px(9.5))
                        .text_color(theme::text_muted())
                        .when_some(delta_tip, |d, tip| d.tooltip(widgets::wrap_tooltip(tip)))
                        .child(delta)
                }))
                .child(
                    div()
                        .flex_none()
                        .font_family(font::MONO)
                        .text_size(px(10.5))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::text())
                        .child(format::memory(bytes)),
                )
                .child(
                    // Look-first before remove, same order as the
                    // large-file rows.
                    Button::new(SharedString::from(format!("{key}-reveal")))
                        .icon(IconName::Folder)
                        .ghost()
                        .xsmall()
                        .tooltip(i18n::tr("disk.big_reveal"))
                        .on_click(move |_, _window, cx| {
                            // The row itself opens; the button must not.
                            cx.stop_propagation();
                            bigfiles::reveal(&reveal_path);
                        }),
                )
                .when(deletable, |row| {
                    row.child(
                        Button::new(SharedString::from(format!("{key}-trash")))
                            .icon(IconName::Delete)
                            .ghost()
                            .xsmall()
                            .tooltip(i18n::tr("disk.big_trash"))
                            .on_click({
                                let bytes_str = format::memory(bytes);
                                move |_, window, cx| {
                                    cx.stop_propagation();
                                    let path = trash_path.clone();
                                    confirm::ask(
                                        window,
                                        cx,
                                        i18n::tr("disk.big_trash_title"),
                                        t!(
                                            "disk.ana_trash_body",
                                            name = confirm_label.clone(),
                                            bytes = bytes_str.clone()
                                        )
                                        .to_string(),
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
                            }),
                    )
                }),
        )
        .child(div().mt(px(3.)).child(widgets::meter(
            bytes as f32 / group_max as f32,
            Hsla::from(theme::ink()),
            3.,
        )))
        .into_any_element()
}

/// The index query's card. `Off` is not an empty state — it is "not
/// asked yet", and [`big_files_body`] says which of the two it is in a
/// sentence rather than leaving a blank panel.
fn big_files_card(state: &ZStatsAppState) -> AnyElement {
    // A finished listing is twenty rows deep and sits above the
    // analyser; the ✕ is how you put it away again. Only shown once
    // there is something to put away — and only for a *finished* one,
    // because `mdfind` is spawned without a cancel and a control that
    // cannot stop what it points at would be a lie.
    let answered = matches!(
        state.big_files(),
        BigFiles::Ready { .. } | BigFiles::Failed { .. }
    );
    let controls = h_flex()
        .items_center()
        .gap(px(4.))
        .child(big_files_chip(state))
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
        .child(widgets::list_header(
            i18n::tr("disk.big_title"),
            Some(controls),
        ))
        .child(div().px(px(13.)).pb(px(11.)).child(big_files_body(state)))
        .into_any_element()
}

fn big_files_chip(state: &ZStatsAppState) -> AnyElement {
    let label = match state.big_files() {
        BigFiles::Running => i18n::tr("disk.big_scanning"),
        BigFiles::Ready { .. } => i18n::tr("disk.big_rescan"),
        BigFiles::Off | BigFiles::Failed { .. } => i18n::tr("disk.big_scan"),
    };
    let running = matches!(state.big_files(), BigFiles::Running);
    div()
        .id("bigfiles-scan")
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(theme::border())
        .bg(theme::inset())
        .px(px(8.))
        .py(px(2.))
        .tooltip(widgets::wrap_tooltip(i18n::tr("disk.big_hint")))
        .text_size(px(10.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(if running {
            theme::text_dim()
        } else {
            theme::text()
        })
        .when(!running, |d| {
            d.hover(|d| d.bg(theme::surface_raised()))
                .on_click(|_, _window, cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| state.start_big_files(cx));
                })
        })
        .child(label)
        .into_any_element()
}

fn big_files_body(state: &ZStatsAppState) -> AnyElement {
    // The card body carries the padding; these are plain notes.
    let padded_note = |text: String| div().child(widgets::note(text)).into_any_element();
    match state.big_files() {
        BigFiles::Off => padded_note(i18n::tr("disk.big_hint")),
        BigFiles::Running => padded_note(i18n::tr("disk.big_running")),
        BigFiles::Failed { indexing_off: true } => padded_note(i18n::tr("disk.big_index_off")),
        BigFiles::Failed {
            indexing_off: false,
        } => padded_note(i18n::tr("disk.big_failed")),
        BigFiles::Ready { scan, .. } if scan.files.is_empty() => {
            padded_note(i18n::tr("disk.big_none"))
        }
        BigFiles::Ready { scan, added, since } => {
            let caption = {
                // The bar describes what the rows actually show — the
                // smallest displayed PHYSICAL size, floored to a clean
                // step. Quoting the query threshold here was a lie in
                // both directions: sparse files enter on logical size and
                // display far below it.
                let bar = display_bar(scan.files.iter().map(|f| f.size).min().unwrap_or(0));
                // "~" leads because the query is `mdfind -onlyin $HOME`
                // while the window it sits in is titled for the whole
                // volume. Same notation the analysis caption uses to
                // name a scope.
                let mut text = String::from("~ · ");
                text.push_str(&if bar == 0 {
                    t!("disk.big_count_plain", count = scan.total).to_string()
                } else {
                    t!(
                        "disk.big_count",
                        thr = format::memory(bar),
                        count = scan.total
                    )
                    .to_string()
                });
                if scan.threshold == bigfiles::FALLBACK_THRESHOLD {
                    text.push_str(" · ");
                    text.push_str(&i18n::tr("disk.big_fallback_note"));
                }
                if scan.total > scan.files.len() {
                    text.push_str(" · ");
                    text.push_str(t!("disk.big_shown", shown = scan.files.len()).as_ref());
                }
                // What "new" means, in the one place it can be checked:
                // the run it is measured against. Without this the marks
                // would be a claim about an unnamed past.
                if let Some(since) = since {
                    text.push_str(" · ");
                    text.push_str(
                        t!(
                            "disk.big_since",
                            ago = format::ago(since.elapsed().unwrap_or_default())
                        )
                        .as_ref(),
                    );
                    if !added.is_empty() {
                        text.push_str(" · ");
                        text.push_str(t!("disk.big_added", count = added.len()).as_ref());
                    }
                }
                text
            };
            let total = scan.files.len();
            let since_label = since.map(|t| format::ago(t.elapsed().unwrap_or_default()));
            div()
                .child(div().pb(px(4.)).child(widgets::note(caption)))
                .children(scan.files.iter().enumerate().map(|(i, f)| {
                    big_file_row(
                        i,
                        f,
                        i + 1 == total,
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

/// `/Users/you/…` collapses to `~/…` — the shared prefix every row would
/// otherwise spend its tooltip width repeating. Pure so it can be tested
/// without touching the real environment.
fn tilde_path(path: &str, home: &str) -> String {
    match path.strip_prefix(home) {
        // Component boundary required: /Users/xy must not collapse under
        // a /Users/x home.
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_string(),
    }
}

/// `new_since` carries the baseline's age when this row is one the
/// previous listing would have shown and did not — the pill says "new",
/// its tooltip says since when. `None` covers both "was there before"
/// and "cannot tell", which the caption's absence of a baseline line
/// already explains.
fn big_file_row(
    index: usize,
    file: &bigfiles::BigFile,
    last: bool,
    new_since: Option<String>,
) -> AnyElement {
    let name = file
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.path.display().to_string());
    // The full location rides the name's tooltip instead of a second line:
    // most rows never need it (the Finder button answers "where" better),
    // and a 320px column has no honest way to show a deep path anyway.
    let mut full = tilde_path(
        &file.path.display().to_string(),
        &env::var("HOME").unwrap_or_default(),
    );
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
    let confirm_name = name.clone();

    h_flex()
        .items_center()
        .justify_between()
        .gap(px(8.))
        .py(px(8.))
        .when(!last, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .child(
            div()
                .id(("bigfile-name", index))
                .flex_1()
                .min_w_0()
                .text_size(px(11.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::text())
                .truncate()
                .tooltip(widgets::wrap_tooltip(full))
                .child(name),
        )
        .children(new_since.map(|ago| {
            // Neutral, like the analyser's "cache" pill: accent is for
            // over-threshold, and a new file is news, not a problem.
            div()
                .id(("bigfile-new", index))
                .flex_none()
                .rounded_full()
                .px(px(5.))
                .text_size(px(9.))
                .bg(theme::inset())
                .text_color(theme::text_muted())
                .tooltip(widgets::wrap_tooltip(
                    t!("disk.big_new_tip", ago = ago).to_string(),
                ))
                .child(i18n::tr("disk.big_new"))
        }))
        .child(
            div()
                .flex_none()
                .font_family(font::MONO)
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::text())
                .child(format::memory(file.size)),
        )
        .child(
            // Navigation, not an action — no confirm, just Finder with the
            // file selected. Sits left of the destructive control so the
            // "look first" option comes before the "remove" one.
            Button::new(("bigfile-reveal", index))
                .icon(IconName::Folder)
                .ghost()
                .xsmall()
                .tooltip(i18n::tr("disk.big_reveal"))
                .on_click({
                    let path = file.path.clone();
                    move |_, _window, _cx| bigfiles::reveal(&path)
                }),
        )
        .child(
            // An explicit control with confirm, and the request is Finder's
            // own recoverable move-to-Trash — never a direct unlink.
            Button::new(("bigfile-trash", index))
                .icon(IconName::Delete)
                .ghost()
                .xsmall()
                .tooltip(i18n::tr("disk.big_trash"))
                .on_click(move |_, window, cx| {
                    let path = path.clone();
                    confirm::ask(
                        window,
                        cx,
                        i18n::tr("disk.big_trash_title"),
                        t!("disk.big_trash_body", name = confirm_name.clone()).to_string(),
                        i18n::tr("disk.big_trash_ok"),
                        move |cx| {
                            let path = path.clone();
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| state.trash_big_file(&path, cx));
                        },
                    );
                }),
        )
        .into_any_element()
}
#[cfg(test)]
mod tests {
    use super::*;

    /// An opened row may only stay silent about its children when there
    /// are none left to mention — the summary line is what keeps the cut
    /// at [`EXPAND_ROWS`] visible.
    #[test]
    fn an_opened_row_accounts_for_the_children_it_does_not_list() {
        let hit = |bytes| DirHit {
            path: PathBuf::from(format!("/r/{bytes}")),
            bytes,
            kind: HitKind::Plain,
        };
        let rows: Vec<DirHit> = (1..=EXPAND_ROWS as u64 + 3).map(|n| hit(n * 100)).collect();
        let (hidden, bytes) = expand_hidden(&rows).expect("three past the cut");
        assert_eq!(hidden, 3);
        // The three past the cut, and only those.
        assert_eq!(bytes, (900 + 1000 + 1100));
        // Exactly the cut, and under it, say nothing.
        assert_eq!(expand_hidden(&rows[..EXPAND_ROWS]), None);
        assert_eq!(expand_hidden(&[]), None);
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

    #[test]
    fn tilde_path_collapses_home_only() {
        assert_eq!(
            tilde_path("/Users/x/Movies/a.mkv", "/Users/x"),
            "~/Movies/a.mkv"
        );
        assert_eq!(tilde_path("/tmp/a", "/Users/x"), "/tmp/a");
        // An empty home must not turn every path into "~<path>".
        assert_eq!(tilde_path("/tmp/a", ""), "/tmp/a");
        // Prefix only counts on a component boundary.
        assert_eq!(tilde_path("/Users/xy/f", "/Users/x"), "/Users/xy/f");
    }

    #[test]
    fn analysis_caption_omits_a_bare_home_root() {
        let home = analysis_caption_parts(
            "/Users/x",
            "/Users/x",
            "3m ago".into(),
            "took 2m 34s".into(),
            "12 directories".into(),
            vec!["2 protected skipped".into()],
        );
        assert_eq!(
            home,
            "3m ago · took 2m 34s · 12 directories · 2 protected skipped"
        );
        let nested = analysis_caption_parts(
            "/Users/x/Library",
            "/Users/x",
            "just now".into(),
            "took 41s".into(),
            "3 directories".into(),
            vec![],
        );
        assert_eq!(nested, "~/Library · just now · took 41s · 3 directories");
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
