//! Disk: one card per volume — capacity, then IO rates.

use super::widgets::{self, card};
use crate::bigfiles;
use crate::cleanhints;
use crate::confirm;
use crate::diskscan::{self, DiffBaseline, DirHit, FileHit, HitKind, ScanResult};
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{BigFiles, DiskAnalysis, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px, relative,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName, Sizable, Size, h_flex};
use rust_i18n::t;
use zstats::snapshot::DiskSnapshot;

/// The design's default disk alert bar.
const FULL_PERCENT: f32 = 90.0;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(tick) = state.latest() else {
        return vec![widgets::empty_card(
            i18n::tr("disk.title"),
            i18n::tr("common.waiting_sample"),
        )];
    };
    let Some(disks) = tick.snapshot.disks.as_deref() else {
        return vec![widgets::empty_card(
            i18n::tr("disk.off"),
            i18n::tr("disk.off_body"),
        )];
    };
    if disks.is_empty() {
        return vec![widgets::empty_card(
            i18n::tr("disk.no_volumes"),
            i18n::tr("disk.no_volumes_body"),
        )];
    }

    let mut cards: Vec<AnyElement> = disks
        .iter()
        .enumerate()
        .map(|(i, d)| {
            // The library's own figure, not ours: it is what the CLI paints
            // and what the disk alert fires on, so recomputing it here could
            // only ever produce a number that disagrees with both.
            let used_pct = d.used_percent;
            // A full removable drive is normal; a full boot volume is not.
            let hot = used_pct > FULL_PERCENT && !d.is_removable;

            card()
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .truncate()
                                .child(d.mount_point.clone()),
                        )
                        // The large-file entry lives on the volume that
                        // holds ~ — a standing card of its own would be a
                        // permanent block of chrome for a one-shot query.
                        .when(d.mount_point == "/", |row| {
                            row.child(big_files_chip(state))
                                .child(analysis_chip(state))
                                .children(analysis_preset_chips(state))
                                .child(analysis_pick_chip())
                        })
                        .child(volume_badge(i, d)),
                )
                .child(
                    h_flex()
                        .items_baseline()
                        .justify_between()
                        .gap(px(6.))
                        .mt(px(6.))
                        .child(
                            div()
                                .font_family(font::MONO)
                                .text_size(px(20.))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(theme::text())
                                .child(format::capacity(d.available_bytes)),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme::text_muted())
                                .child(
                                    t!(
                                        "disk.free_of",
                                        total = format::capacity(d.total_bytes),
                                        used = format!("{used_pct:.0}")
                                    )
                                    .to_string(),
                                ),
                        ),
                )
                .child(div().mt(px(7.)).child(widgets::meter(
                    used_pct / 100.0,
                    Hsla::from(theme::fill_for(hot)),
                    6.,
                )))
                .child(
                    h_flex()
                        .justify_between()
                        .mt(px(7.))
                        .text_size(px(10.))
                        .text_color(theme::text_dim())
                        .child(
                            div().font_family(font::MONO).child(
                                t!(
                                    "disk.rw",
                                    read = format::rate(d.read_bytes_per_sec),
                                    write = format::rate(d.write_bytes_per_sec)
                                )
                                .to_string(),
                            ),
                        )
                        .child(div().child(d.file_system.clone())),
                )
                // The line the capacity meter cannot say: purgeable
                // space and local snapshots — where a full disk often
                // actually went, and why per-directory sums do not add
                // up to "used". Boot volume only; Apple's own figures.
                .when(d.mount_point == "/", |c| c.children(space_line(state)))
                // The query's results live inside the card of the volume
                // they were asked about, as a section under a hairline —
                // not a sibling card, which would separate the chip from
                // its answer. Off renders nothing at all.
                .when(
                    d.mount_point == "/" && !matches!(state.big_files(), BigFiles::Off),
                    |c| c.child(big_files_section(state)),
                )
                .into_any_element()
        })
        .collect();
    // The analyser gets a card of its own (three tables do not fit a
    // section), right under the volume whose chip launches it.
    if !matches!(state.disk_analysis(), DiskAnalysis::Off) {
        let at = disks
            .iter()
            .position(|d| d.mount_point == "/")
            .map_or(cards.len(), |i| i + 1);
        cards.insert(at, analysis_card(state));
    }
    cards
}

/// "purgeable ~X · N local snapshots" under the boot volume's footer.
/// Display and honest pointing only — the tooltip says whose figures
/// these are and that reclaiming is the system's (or tmutil's) job.
fn space_line(state: &ZStatsAppState) -> Option<AnyElement> {
    let info = state.space_info()?;
    let mut parts = Vec::new();
    if let Some(purgeable) = info.purgeable_bytes.filter(|p| *p > 0) {
        parts.push(t!("disk.purgeable", v = format::memory(purgeable)).to_string());
    }
    if info.snapshots > 0 {
        parts.push(t!("disk.snapshots", n = info.snapshots).to_string());
    }
    if parts.is_empty() {
        return None;
    }
    Some(
        div()
            .id("disk-space-extra")
            .mt(px(6.))
            .text_size(px(10.))
            .text_color(theme::text_dim())
            .tooltip(widgets::wrap_tooltip(i18n::tr("disk.space_extra_tip")))
            .child(parts.join(" · "))
            .into_any_element(),
    )
}

/// Trigger / cancel for the directory analyser. Unlike the large-file
/// chip, Running stays clickable — it IS the explicit cancel, the only
/// way a walk stops early (hide deliberately does not, see state.rs).
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
/// The panel takes key focus, so the popover may auto-hide behind it;
/// the walk survives hide by design, and the result is waiting on the
/// next open.
fn analysis_pick_chip() -> AnyElement {
    Button::new("ana-pick")
        .icon(IconName::FolderOpen)
        .ghost()
        .xsmall()
        .tooltip(i18n::tr("disk.ana_pick_hint"))
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
        })
        .into_any_element()
}

/// One-click preset scopes beside the folder picker (docs/
/// disk-analysis.md's scope table): `~/Library` — the blind-spot
/// close-up — and the merged cache roots. Hidden while a walk runs:
/// the header's job is then the cancel chip, and a preset click would
/// silently restart the walk. Clicking starts the walk immediately,
/// same contract as the picker.
fn analysis_preset_chips(state: &ZStatsAppState) -> Vec<AnyElement> {
    if matches!(state.disk_analysis(), DiskAnalysis::Running { .. }) {
        return Vec::new();
    }
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
            "ana-preset-library",
            // A path, not a phrase — the label needs no translation.
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

fn analysis_card(state: &ZStatsAppState) -> AnyElement {
    // Collapsed by default on every visit: the tables are long enough to
    // bury the volumes and sensors below. The summary line still carries
    // the vitals (age, cost, counters — or live progress), so collapsed
    // is informative, not blank.
    if !state.disk_analysis_expanded() {
        return widgets::list_shell()
            .child(analysis_header(state))
            .into_any_element();
    }
    let body = match state.disk_analysis() {
        DiskAnalysis::Off => div().into_any_element(),
        // The live progress line lives in the header caption (shared
        // with the collapsed summary); the body is the partial tables —
        // same renderer as the final result, figures are lower bounds
        // that only grow. No delete controls mid-scan: the walker may
        // still be inside any of these trees.
        // No deltas mid-scan either: partial figures are lower bounds,
        // and comparing them to a finished run would read as shrinkage.
        DiskAnalysis::Running { partial, .. } => div()
            .children(
                partial
                    .as_ref()
                    .map(|r| analysis_tables(r, false, state.analysis_show_all_dirs(), None)),
            )
            .into_any_element(),
        DiskAnalysis::Failed(e) => div()
            .children(back_chip(state))
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
            .children(back_chip(state))
            .child(analysis_tables(
                result,
                true,
                state.analysis_show_all_dirs(),
                state.analysis_diff_for(result),
            ))
            .into_any_element(),
    };
    widgets::list_shell()
        .child(analysis_header(state))
        .child(body)
        .into_any_element()
}

/// Title on its own row. The meta string is too long to sit in
/// [`widgets::list_header`]'s right slot — at 320px it overflowed
/// the panel ("6 protec…") instead of wrapping.
fn analysis_header(state: &ZStatsAppState) -> AnyElement {
    let caption = analysis_caption(state);
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
                        .child({
                            let expanded = state.disk_analysis_expanded();
                            div()
                                .id("ana-fold")
                                .flex_none()
                                .rounded(px(4.))
                                .px(px(6.))
                                .py(px(1.))
                                .text_size(px(10.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::text_muted())
                                .hover(|d| d.bg(theme::surface_raised()).text_color(theme::text()))
                                .child(i18n::tr(if expanded {
                                    "disk.ana_collapse"
                                } else {
                                    "disk.ana_expand"
                                }))
                                .on_click(move |_, _window, cx| {
                                    cx.global::<ZStatsGlobalStore>()
                                        .clone()
                                        .update(cx, |state, cx| {
                                            state.set_disk_analysis_expanded(!expanded, cx)
                                        });
                                })
                        })
                        .child(
                            // A view action: drops the whole result (and its
                            // drill index), touches nothing on disk. Cancels the
                            // walk too if one is still running. Icon-only — the
                            // tooltip carries the words, and Close (not Delete)
                            // keeps it visually apart from the file-trashing
                            // controls below.
                            Button::new("ana-dismiss")
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .tooltip(i18n::tr("disk.ana_dismiss_hint"))
                                .on_click(|_, _window, cx| {
                                    cx.global::<ZStatsGlobalStore>()
                                        .clone()
                                        .update(cx, |state, cx| state.clear_disk_analysis(cx));
                                }),
                        ),
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
                    .on_click(|_, _window, _cx| open_full_disk_access()),
            )
            .into_any_element(),
    )
}

/// Deep-link into System Settings → Privacy & Security → Full Disk
/// Access. Navigation only — granting stays a user act in the system UI.
pub(super) fn open_full_disk_access() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .spawn();
}

/// Age, how many directories were walked, and every honesty counter
/// that is non-zero. The scan root is omitted when it is just `~` —
/// that is the only root this card has, and it burned four characters
/// for nothing next to the title.
/// Results older than this get a "consider re-analyzing" nudge appended
/// to the caption. Display only, like every threshold in views/ —
/// nothing refreshes itself: a minutes-long walk must never
/// self-trigger, so a nudge is where staleness honesty ends. A day is
/// when "the numbers are from earlier" stops going without saying —
/// mostly reached through the persisted cache surviving a restart.
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

fn analysis_caption(state: &ZStatsAppState) -> String {
    let result = match state.disk_analysis() {
        DiskAnalysis::Ready(result) => result,
        // Collapsed while a walk runs: the summary line is the progress.
        DiskAnalysis::Running { dirs_done, .. } => {
            return t!("disk.ana_running", dirs = format::thousands(*dirs_done)).to_string();
        }
        DiskAnalysis::Failed(e) => {
            return t!("disk.ana_failed", e = e.clone()).to_string();
        }
        DiskAnalysis::Off => return String::new(),
    };
    let home = std::env::var("HOME").unwrap_or_default();
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
    // A multi-root scope lists its roots — passing the base alone would
    // read as a walk of the whole home tree. Pre-tilded, so the parts
    // helper's own tilde pass leaves the joined string untouched.
    let root_display = if result.roots.len() > 1 {
        result
            .roots
            .iter()
            .map(|r| tilde_path(&r.display().to_string(), &home))
            .collect::<Vec<_>>()
            .join(" + ")
    } else {
        result.root.display().to_string()
    };
    analysis_caption_parts(
        &root_display,
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
    result: &ScanResult,
    actions: bool,
    show_all_dirs: bool,
    diff: Option<&DiffBaseline>,
) -> AnyElement {
    let root = result.root.clone();
    let dir_rows = |hits: &[DirHit], id: &'static str, deletable: bool| -> Vec<AnyElement> {
        let max = hits.iter().map(|h| h.bytes).max().unwrap_or(1).max(1);
        hits.iter()
            .enumerate()
            // Directory rows drill on click (actions = a finished result;
            // mid-scan tables are inert), file rows never do.
            .map(|(i, h)| {
                analysis_row(AnalysisRow {
                    id,
                    index: i,
                    path: &h.path,
                    bytes: h.bytes,
                    prev_bytes: diff.and_then(|d| d.bytes_for(&h.path)),
                    kind: Some(h.kind),
                    group_max: max,
                    root: &root,
                    deletable,
                    drillable: actions,
                })
            })
            .collect()
    };
    let file_rows = |hits: &[FileHit]| -> Vec<AnyElement> {
        let max = hits.iter().map(|h| h.bytes).max().unwrap_or(1).max(1);
        hits.iter()
            .enumerate()
            .map(|(i, h)| {
                analysis_row(AnalysisRow {
                    id: "ana-file",
                    index: i,
                    path: &h.path,
                    bytes: h.bytes,
                    prev_bytes: diff.and_then(|d| d.bytes_for(&h.path)),
                    kind: None,
                    group_max: max,
                    root: &root,
                    deletable: false,
                    drillable: false,
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
    let paths: Vec<std::path::PathBuf> = hits.iter().map(|h| h.path.clone()).collect();
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

/// "← back" — exists only while a drill-down is showing; restores the
/// parked outer result instantly instead of re-walking it.
fn back_chip(state: &ZStatsAppState) -> Option<AnyElement> {
    state.disk_analysis_can_back().then(|| {
        div()
            .px(px(13.))
            .pt(px(2.))
            .pb(px(2.))
            .child(
                Button::new("ana-back")
                    .ghost()
                    .xsmall()
                    .label(i18n::tr("disk.ana_back"))
                    .on_click(|_, _window, cx| {
                        cx.global::<ZStatsGlobalStore>()
                            .clone()
                            .update(cx, |state, cx| state.pop_disk_analysis(cx));
                    }),
            )
            .into_any_element()
    })
}

/// One ranked row: path relative to the scan root, physical size, a meter
/// against the group's largest, and Finder Reveal. `deletable` adds the
/// confirm-gated move-to-Trash — passed only for the regenerable table,
/// whose rows are all signature-checked `CACHEDIR.TAG` trees; heuristic
/// and plain rows never get the control. `drillable` makes the row itself
/// clickable: no tree is retained, so "expand" honestly means re-walking
/// that path as the new root (seconds for a subtree), with "back" holding
/// the parked outer result.
/// Only the owner-declared tier gets a pill: "cache" maps straight to
/// an action semantic — this row is (or can be) a cleanup suggestion.
/// A heuristic fold earns no pill; its explanatory value did not pay
/// for the attention it took, so the how-it-was-classified note rides
/// the row's name tooltip instead. Plain directories say nothing.
fn kind_pill(id: &'static str, index: usize, kind: HitKind) -> Option<AnyElement> {
    if kind != HitKind::Tag {
        return None;
    }
    Some(
        div()
            .id(SharedString::from(format!("{id}-kind-{index}")))
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

/// One ranked row's inputs, named — a struct rather than nine
/// positional arguments (clippy's lint was right about the call sites).
struct AnalysisRow<'a> {
    id: &'static str,
    index: usize,
    path: &'a std::path::Path,
    bytes: u64,
    /// This path's figure in the previous run, when it ranked there —
    /// `None` renders no delta (absence proves nothing, see
    /// [`DiffBaseline`]).
    prev_bytes: Option<u64>,
    kind: Option<HitKind>,
    /// The group's largest row, the meter's 100%.
    group_max: u64,
    root: &'a std::path::Path,
    deletable: bool,
    drillable: bool,
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
        id,
        index,
        path,
        bytes,
        prev_bytes,
        kind,
        group_max,
        root,
        deletable,
        drillable,
    } = row;
    let label = path
        .strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string());
    let mut full = tilde_path(
        &path.display().to_string(),
        &std::env::var("HOME").unwrap_or_default(),
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
    let drill_path = path.to_path_buf();
    let confirm_label = label.clone();

    div()
        .id(SharedString::from(format!("{id}-row-{index}")))
        .py(px(4.))
        .px(px(4.))
        .mx(px(-4.))
        .rounded(px(5.))
        .when(drillable, |row| {
            // The hover fill is the affordance (see views/mod.rs — no
            // hand cursor on in-app controls).
            row.hover(|s| s.bg(theme::surface_raised()))
                .on_click(move |_, _window, cx| {
                    cx.global::<ZStatsGlobalStore>()
                        .clone()
                        .update(cx, |state, cx| {
                            state.drill_disk_analysis(drill_path.clone(), cx)
                        });
                })
        })
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-{index}")))
                        .flex_1()
                        .min_w_0()
                        .text_size(px(11.))
                        .text_color(theme::text())
                        .truncate()
                        .tooltip(widgets::wrap_tooltip(full))
                        .child(label),
                )
                .children(kind.and_then(|kind| kind_pill(id, index, kind)))
                .children(delta_label(bytes, prev_bytes).map(|delta| {
                    // Quiet on purpose: the sign carries the meaning, and
                    // accent is reserved for over-threshold (views/mod.rs).
                    div()
                        .flex_none()
                        .font_family(font::MONO)
                        .text_size(px(9.5))
                        .text_color(theme::text_muted())
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
                    Button::new(SharedString::from(format!("{id}-reveal-{index}")))
                        .icon(IconName::Folder)
                        .ghost()
                        .xsmall()
                        .tooltip(i18n::tr("disk.big_reveal"))
                        .on_click(move |_, _window, cx| {
                            // The row itself drills; the button must not.
                            cx.stop_propagation();
                            bigfiles::reveal(&reveal_path);
                        }),
                )
                .when(deletable, |row| {
                    row.child(
                        Button::new(SharedString::from(format!("{id}-trash-{index}")))
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

/// The one-shot Spotlight large-file query — plan step one of the disk
/// cleanup story — rendered as a section of the boot volume's card.
/// Everything here is a metadata lookup; the walk-based directory
/// analyser is a separate, later feature.
fn big_files_section(state: &ZStatsAppState) -> AnyElement {
    div()
        .mt(px(10.))
        .pt(px(9.))
        .border_t(px(1.))
        .border_color(theme::border_subtle())
        .child(big_files_body(state))
        .into_any_element()
}

fn big_files_chip(state: &ZStatsAppState) -> AnyElement {
    let label = match state.big_files() {
        BigFiles::Running => i18n::tr("disk.big_scanning"),
        BigFiles::Ready(_) => i18n::tr("disk.big_rescan"),
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
    // Inside the volume card, which already carries the padding.
    let padded_note = |text: String| div().child(widgets::note(text)).into_any_element();
    match state.big_files() {
        BigFiles::Off => padded_note(i18n::tr("disk.big_hint")),
        BigFiles::Running => padded_note(i18n::tr("disk.big_running")),
        BigFiles::Failed { indexing_off: true } => padded_note(i18n::tr("disk.big_index_off")),
        BigFiles::Failed {
            indexing_off: false,
        } => padded_note(i18n::tr("disk.big_failed")),
        BigFiles::Ready(scan) if scan.files.is_empty() => padded_note(i18n::tr("disk.big_none")),
        BigFiles::Ready(scan) => {
            let caption = {
                // The bar describes what the rows actually show — the
                // smallest displayed PHYSICAL size, floored to a clean
                // step. Quoting the query threshold here was a lie in
                // both directions: sparse files enter on logical size and
                // display far below it.
                let bar = display_bar(scan.files.iter().map(|f| f.size).min().unwrap_or(0));
                let mut text = if bar == 0 {
                    t!("disk.big_count_plain", count = scan.total).to_string()
                } else {
                    t!(
                        "disk.big_count",
                        thr = format::memory(bar),
                        count = scan.total
                    )
                    .to_string()
                };
                if scan.threshold == bigfiles::FALLBACK_THRESHOLD {
                    text.push_str(" · ");
                    text.push_str(&i18n::tr("disk.big_fallback_note"));
                }
                if scan.total > scan.files.len() {
                    text.push_str(" · ");
                    text.push_str(t!("disk.big_shown", shown = scan.files.len()).as_ref());
                }
                text
            };
            let total = scan.files.len();
            div()
                .child(div().pb(px(4.)).child(widgets::note(caption)))
                .children(
                    scan.files
                        .iter()
                        .enumerate()
                        .map(|(i, f)| big_file_row(i, f, i + 1 == total)),
                )
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

fn big_file_row(index: usize, file: &bigfiles::BigFile, last: bool) -> AnyElement {
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
        &std::env::var("HOME").unwrap_or_default(),
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

fn volume_badge(index: usize, disk: &DiskSnapshot) -> AnyElement {
    if disk.is_removable {
        let mount = disk.mount_point.clone();
        let tip = i18n::tr("disk.eject");
        h_flex()
            .id(("eject", index))
            .items_center()
            .gap(px(4.))
            .flex_none()
            .rounded_full()
            .border_1()
            .border_color(theme::border())
            .pl(px(7.))
            .pr(px(4.))
            .py(px(1.))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .hover(|d| {
                d.bg(theme::surface_raised())
                    .border_color(theme::accent_wash(45))
            })
            .on_click(move |_, window, cx| {
                let mount = mount.clone();
                confirm::ask(
                    window,
                    cx,
                    i18n::tr("disk.eject_title"),
                    t!("disk.eject_body", mount = mount.clone()).to_string(),
                    i18n::tr("disk.eject_ok"),
                    move |_| eject(&mount),
                );
            })
            .child(
                div()
                    .text_size(px(10.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::text_muted())
                    .child(i18n::tr("disk.removable")),
            )
            .child(
                Icon::new(IconName::CircleX)
                    .with_size(Size::Size(px(11.)))
                    .text_color(Hsla::from(theme::text_dim())),
            )
            .into_any_element()
    } else {
        widgets::outline_pill(disk.kind.clone())
    }
}

/// Unmount / eject a removable volume. Never the boot volume — the
/// badge is only rendered for `is_removable`, and this is a second gate.
fn eject(mount: &str) {
    if !safe_to_eject(mount) {
        eprintln!("refusing to eject {mount}");
        return;
    }
    let mount = mount.to_string();
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        let result = std::process::Command::new("diskutil")
            .args(["eject", &mount])
            .output();
        #[cfg(not(target_os = "macos"))]
        let result = std::process::Command::new("umount").arg(&mount).output();

        match result {
            Ok(out) if out.status.success() => {}
            Ok(out) => eprintln!(
                "eject {mount} failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => eprintln!("eject {mount}: {e}"),
        }
    });
}

fn safe_to_eject(mount: &str) -> bool {
    let mount = mount.trim_end_matches('/');
    !matches!(
        mount,
        "" | "/" | "/System" | "/System/Volumes/Data" | "/home" | "/Users"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn will_not_eject_the_boot_volume() {
        assert!(!safe_to_eject("/"));
        assert!(!safe_to_eject("/System/Volumes/Data"));
        assert!(safe_to_eject("/Volumes/Zedis Installer"));
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
