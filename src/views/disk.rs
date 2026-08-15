//! Disk: one card per volume — capacity, then IO rates.

use super::widgets::{self, card};
use crate::font;
use crate::format;
use crate::i18n;
use crate::state::{BigFiles, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, div, px,
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

    let cards: Vec<AnyElement> = disks
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
                        .when(d.mount_point == "/", |row| row.child(big_files_chip(state)))
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
    cards
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
}
