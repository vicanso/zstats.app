//! Disk: one card per volume — capacity, then IO rates.

use super::widgets::{self, card};
use crate::diskscan::{DirHit, FileHit, ScanResult};
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
                            row.child(big_files_chip(state)).child(analysis_chip(state))
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

fn analysis_card(state: &ZStatsAppState) -> AnyElement {
    let body = match state.disk_analysis() {
        DiskAnalysis::Off => div().into_any_element(),
        DiskAnalysis::Running {
            dirs_done, partial, ..
        } => div()
            .child(div().px(px(13.)).pt(px(2.)).pb(px(8.)).child(widgets::note(
                t!("disk.ana_running", dirs = dirs_done).to_string(),
            )))
            // Whatever has been aggregated so far, rendered with the same
            // tables as the final result — figures are lower bounds and
            // the ranking reshuffles as data lands, which the running
            // banner above frames. No delete controls mid-scan: the
            // walker may still be inside any of these trees.
            .children(partial.as_ref().map(|r| analysis_tables(r, false)))
            .into_any_element(),
        DiskAnalysis::Failed(e) => div()
            .px(px(13.))
            .pt(px(2.))
            .pb(px(11.))
            .child(widgets::note(
                t!("disk.ana_failed", e = e.clone()).to_string(),
            ))
            .into_any_element(),
        DiskAnalysis::Ready(result) => analysis_tables(result, true),
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
            div()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(i18n::tr("disk.ana_title")),
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
        .into_any_element()
}

/// Age, how many directories were walked, and every honesty counter
/// that is non-zero. The scan root is omitted when it is just `~` —
/// that is the only root this card has, and it burned four characters
/// for nothing next to the title.
fn analysis_caption(state: &ZStatsAppState) -> String {
    let DiskAnalysis::Ready(result) = state.disk_analysis() else {
        return String::new();
    };
    let home = std::env::var("HOME").unwrap_or_default();
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
    analysis_caption_parts(
        &result.root.display().to_string(),
        &home,
        format::ago(result.scanned_at.elapsed()),
        t!("disk.ana_took", t = format::took(result.took)).to_string(),
        t!("disk.ana_dirs_seen", n = result.dirs_seen).to_string(),
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

fn analysis_tables(result: &ScanResult, actions: bool) -> AnyElement {
    let root = result.root.clone();
    let dir_rows = |hits: &[DirHit], id: &'static str, deletable: bool| -> Vec<AnyElement> {
        let max = hits.iter().map(|h| h.bytes).max().unwrap_or(1).max(1);
        hits.iter()
            .enumerate()
            .map(|(i, h)| analysis_row(id, i, &h.path, h.bytes, max, &root, deletable))
            .collect()
    };
    let file_rows = |hits: &[FileHit]| -> Vec<AnyElement> {
        let max = hits.iter().map(|h| h.bytes).max().unwrap_or(1).max(1);
        hits.iter()
            .enumerate()
            .map(|(i, h)| analysis_row("ana-file", i, &h.path, h.bytes, max, &root, false))
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

    div()
        .children(section(
            i18n::tr("disk.ana_regen"),
            dir_rows(&result.regenerable, "ana-regen", actions),
            actions.then(|| clear_listed_button(&result.regenerable)),
        ))
        .children(section(
            i18n::tr("disk.ana_dirs"),
            dir_rows(&result.dirs, "ana-dir", false),
            None,
        ))
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

/// "Trash the N listed" — the regenerable table's bulk action. "Listed"
/// is the honest word: the ranking is capped at `TABLE_CAP`, so this can
/// only ever clear the rows on screen, not every tagged tree on disk.
fn clear_listed_button(hits: &[DirHit]) -> AnyElement {
    let n = hits.len();
    let total: u64 = hits.iter().map(|h| h.bytes).sum();
    let paths: Vec<std::path::PathBuf> = hits.iter().map(|h| h.path.clone()).collect();
    Button::new("ana-regen-clear")
        .icon(IconName::Delete)
        .ghost()
        .xsmall()
        .label(t!("disk.ana_clear_all", n = n).to_string())
        .on_click(move |_, window, cx| {
            let paths = paths.clone();
            crate::confirm::ask(
                window,
                cx,
                i18n::tr("disk.ana_clear_title"),
                t!("disk.ana_clear_body", n = n, bytes = format::memory(total)).to_string(),
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

/// One ranked row: path relative to the scan root, physical size, a meter
/// against the group's largest, and Finder Reveal. `deletable` adds the
/// confirm-gated move-to-Trash — passed only for the regenerable table,
/// whose rows are all signature-checked `CACHEDIR.TAG` trees; heuristic
/// and plain rows never get the control.
#[allow(clippy::too_many_arguments)]
fn analysis_row(
    id: &'static str,
    index: usize,
    path: &std::path::Path,
    bytes: u64,
    group_max: u64,
    root: &std::path::Path,
    deletable: bool,
) -> AnyElement {
    let label = path
        .strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string());
    let full = tilde_path(
        &path.display().to_string(),
        &std::env::var("HOME").unwrap_or_default(),
    );
    let reveal_path = path.to_path_buf();
    let trash_path = path.to_path_buf();
    let confirm_label = label.clone();

    div()
        .py(px(4.))
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
                        .on_click(move |_, _window, _cx| {
                            crate::bigfiles::reveal(&reveal_path);
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
                                    let path = trash_path.clone();
                                    crate::confirm::ask(
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
                if scan.threshold == crate::bigfiles::FALLBACK_THRESHOLD {
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

fn big_file_row(index: usize, file: &crate::bigfiles::BigFile, last: bool) -> AnyElement {
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
                    move |_, _window, _cx| crate::bigfiles::reveal(&path)
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
                    crate::confirm::ask(
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
                crate::confirm::ask(
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
}
