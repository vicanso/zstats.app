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
use crate::hardware;
use crate::hot;
use crate::i18n;
use crate::state::{ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, div, px,
};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, IconName, Sizable, Size, h_flex};
use rust_i18n::t;
use zstats::snapshot::DiskSnapshot;

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
            let hot = hot::volume_full(d);

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
                                .child(hardware::volume_title(d)),
                        )
                        // One door to the space analysis, on the volume
                        // that holds ~. Both queries behind it are
                        // one-shot and long — they live in a window now
                        // (views/storage.rs), not in this card.
                        .when(d.mount_point == "/", |row| row.child(storage_chip()))
                        .children(eject_badge(i, d)),
                )
                .child(
                    h_flex()
                        .items_baseline()
                        .justify_between()
                        .gap(px(8.))
                        .mt(px(6.))
                        // Free is the headline — the figure people act on,
                        // and the one Overview and the menu bar's disk face
                        // quote — with the word that says so beside it. The
                        // word stood at the far end of the row, past the
                        // total, over a bar filled to the *used* share, so
                        // "121 GB" read as what the bar measured.
                        .child(widgets::big_figure_with(
                            &format::capacity(d.available_bytes),
                            &i18n::tr("disk.free"),
                        ))
                        .child(
                            font::tabular(div())
                                .text_size(px(12.))
                                .text_color(theme::text_muted())
                                .child(
                                    t!(
                                        "disk.used_of",
                                        total = format::capacity(d.total_bytes),
                                        used = format!("{used_pct:.0}")
                                    )
                                    .to_string(),
                                ),
                        ),
                )
                .child(div().mt(px(7.)).child(widgets::meter(
                    used_pct / 100.0,
                    Hsla::from(theme::gauge_for(hot)),
                    6.,
                )))
                .child(
                    h_flex()
                        .justify_between()
                        .mt(px(7.))
                        .text_size(px(10.))
                        .text_color(theme::text_dim())
                        .child(
                            // A sentence with numbers in it, and the zh
                            // twin spends Han characters on the labels —
                            // which the bundled mono face cannot draw at
                            // all. Mono keeps the two rates from
                            // jittering where it has the glyphs, and
                            // gets out of the way where it does not.
                            font::mono_unless_cjk(div()).child(
                                t!(
                                    "disk.rw",
                                    read = format::rate(d.read_bytes_per_sec),
                                    write = format::rate(d.write_bytes_per_sec)
                                )
                                .to_string(),
                            ),
                        )
                        .child(div().child(hardware::volume_footer_id(d))),
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
pub(super) fn storage_chip() -> AnyElement {
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
    let line = hardware::space_extra(state)?;
    Some(
        div()
            .id("disk-space-extra")
            .mt(px(6.))
            .text_size(px(10.))
            .text_color(theme::text_dim())
            .tooltip(widgets::wrap_tooltip(i18n::tr("disk.space_extra_tip")))
            .child(line)
            .into_any_element(),
    )
}

/// Eject, on a removable volume — the header's one control there. A
/// fixed volume has none: its kind is a fact, and it moved to the footer
/// (`volume_footer_id`).
fn eject_badge(index: usize, disk: &DiskSnapshot) -> Option<AnyElement> {
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
            .into()
    } else {
        None
    }
}

/// Unmount / eject a removable volume, off the main thread
/// (`hardware::eject`, which refuses the boot volume a second time).
///
/// On success the collector is woken rather than the card being hidden
/// here: the volume list the panel paints is zstats', and a row that
/// vanished because the *view* decided so would be the one place the
/// panel asserted a machine state on its own. Waking makes the next
/// sample land in milliseconds instead of at the end of the cadence, so
/// the row goes away because the OS no longer lists the volume.
fn eject(mount: &str, cx: &mut gpui::App) {
    let mount = mount.to_string();
    let mount_for_hide = mount.clone();
    cx.spawn(async move |cx| {
        let ejected = cx
            .background_executor()
            .spawn(async move { hardware::eject(&mount) })
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
