//! Disk: one card per volume — capacity, then IO rates.
//!
//! The two one-shot queries this card used to host (the Spotlight
//! large-file listing and the directory analyser) moved to their own
//! window; what stays here is what a metrics panel is for — the live
//! figures, plus the door to that window and the eject control.

use super::widgets::{self, card};
use crate::confirm;
use crate::font;
use crate::format;
use crate::i18n;
use crate::opener;
use crate::state::{ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, div, px,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName, Sizable, Size, h_flex};
use rust_i18n::t;
use std::process;
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

    // A volume we have just ejected is gone as far as the OS is
    // concerned, but zstats serves the disk list from cache between
    // refreshes — without this the card outlives the drive by up to a
    // full cadence (`state::mark_ejected` carries the reasoning).
    let disks: Vec<&DiskSnapshot> = disks
        .iter()
        .filter(|d| !state.is_ejected(&d.mount_point))
        .collect();
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
                                .child(volume_title(d)),
                        )
                        // One door to the space analysis, on the volume
                        // that holds ~. Both queries behind it are
                        // one-shot and long — they live in a window now
                        // (views/storage.rs), not in this card.
                        .when(d.mount_point == "/", |row| row.child(storage_chip()))
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
                        .child(div().child(volume_footer_id(d))),
                )
                // The line the capacity meter cannot say: purgeable
                // space and local snapshots — where a full disk often
                // actually went, and why per-directory sums do not add
                // up to "used". Boot volume only; Apple's own figures.
                .when(d.mount_point == "/", |c| c.children(space_line(state)))
                .into_any_element()
        })
        .collect();
    cards
}

/// The volume's name as the Finder shows it — "Macintosh HD", not "/".
///
/// zstats reports it (`DiskSnapshot::name`), so this is a display choice
/// and not a second source: the panel used to paint the mount point,
/// which names the volume correctly and identifies it to nobody outside
/// a terminal. The mount point is still on the card, in the footer where
/// the file system already is. Falls back to it when the OS gave no name
/// — an unnamed volume must not render a blank title.
fn volume_title(disk: &DiskSnapshot) -> String {
    if disk.name.trim().is_empty() {
        disk.mount_point.clone()
    } else {
        disk.name.clone()
    }
}

/// The footer's right slot: "/ · apfs". The technical identity, in the
/// smallest text on the card, so moving the name up top loses nothing.
fn volume_footer_id(disk: &DiskSnapshot) -> String {
    if disk.name.trim().is_empty() {
        disk.file_system.clone()
    } else {
        format!("{} · {}", disk.mount_point, disk.file_system)
    }
}

/// Opens the disk-space window (large files + the directory analyser).
/// A button, not the two chips it replaces: both queries are one-shot,
/// long, and produce more rows than a 320px card can show — the panel
/// keeps the door, the window holds the room (views/storage.rs).
///
/// The label carries **no trailing ellipsis**, against the macOS
/// convention for a command that opens something. That convention is
/// menu-bar shorthand, and it needs room to read as punctuation: on a
/// 10px pill "Analyze…" is indistinguishable from "Analyze" with its
/// tail cut off, and it was read exactly that way. The tooltip says
/// where the click leads.
fn storage_chip() -> AnyElement {
    div()
        .id("storage-open")
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(theme::border())
        .bg(theme::inset())
        .px(px(8.))
        .py(px(2.))
        .tooltip(widgets::wrap_tooltip(i18n::tr("disk.storage_hint")))
        .text_size(px(10.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text())
        .hover(|d| d.bg(theme::surface_raised()))
        .on_click(|_, _window, cx| crate::open_storage_window(cx))
        .child(i18n::tr("disk.storage_open"))
        .into_any_element()
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

/// Deep-link into System Settings → Privacy & Security → Full Disk
/// Access. Navigation only — granting stays a user act in the system UI.
pub(super) fn open_full_disk_access() {
    let _ =
        opener::open(["x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"]);
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
                    move |cx| eject(&mount, cx),
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
///
/// On success the collector is woken rather than the card being hidden
/// here: the volume list the panel paints is zstats', and a row that
/// vanished because the *view* decided so would be the one place the
/// panel asserted a machine state on its own. Waking makes the next
/// sample land in milliseconds instead of at the end of the cadence, so
/// the row goes away because the OS no longer lists the volume.
fn eject(mount: &str, cx: &mut gpui::App) {
    if !safe_to_eject(mount) {
        eprintln!("refusing to eject {mount}");
        return;
    }
    let mount = mount.to_string();
    let mount_for_hide = mount.clone();
    cx.spawn(async move |cx| {
        let ejected = cx
            .background_executor()
            .spawn(async move { run_eject(&mount) })
            .await;
        if ejected {
            cx.update(|cx| {
                // Hide the card now, and ask for a sample so the
                // snapshot catches up as soon as its own disk cadence
                // allows — the hide is what makes it feel immediate,
                // the wake is what ends it early where it can.
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.mark_ejected(mount_for_hide, cx));
                if let Some(pace) = cx.try_global::<crate::metrics::CollectorPace>() {
                    pace.wake();
                }
            });
        }
    })
    .detach();
}

/// The subprocess half, off the main thread. `true` only when the tool
/// reported success — a failed eject must not shorten the wait for a
/// sample that would show the volume still mounted.
fn run_eject(mount: &str) -> bool {
    #[cfg(target_os = "macos")]
    let result = process::Command::new("diskutil")
        .args(["eject", mount])
        .output();
    #[cfg(not(target_os = "macos"))]
    let result = process::Command::new("umount").arg(mount).output();

    match result {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            eprintln!(
                "eject {mount} failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            false
        }
        Err(e) => {
            eprintln!("eject {mount}: {e}");
            false
        }
    }
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

    fn snapshot(name: &str, mount: &str) -> DiskSnapshot {
        DiskSnapshot {
            name: name.into(),
            mount_point: mount.into(),
            file_system: "apfs".into(),
            kind: "SSD".into(),
            is_removable: false,
            total_bytes: 0,
            available_bytes: 0,
            used_percent: 0.0,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    /// The title is the volume's name, the footer keeps the mount point —
    /// and a nameless volume must still title itself with something.
    #[test]
    fn volume_title_prefers_the_name_and_keeps_the_mount_point_below() {
        let boot = snapshot("Macintosh HD", "/");
        assert_eq!(volume_title(&boot), "Macintosh HD");
        assert_eq!(volume_footer_id(&boot), "/ · apfs");

        let unnamed = snapshot("  ", "/Volumes/disk3s2");
        assert_eq!(volume_title(&unnamed), "/Volumes/disk3s2");
        // The title already is the mount point; repeating it below would
        // be the same string twice on one card.
        assert_eq!(volume_footer_id(&unnamed), "apfs");
    }

    #[test]
    fn will_not_eject_the_boot_volume() {
        assert!(!safe_to_eject("/"));
        assert!(!safe_to_eject("/System/Volumes/Data"));
        assert!(safe_to_eject("/Volumes/Zedis Installer"));
    }
}
