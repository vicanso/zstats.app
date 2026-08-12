//! Disk: one card per volume — capacity, then IO rates.

use super::widgets::{self, card};
use crate::font;
use crate::format;
use crate::i18n;
use rust_i18n::t;
use crate::state::ZStatsAppState;
use crate::theme;
use gpui::{AnyElement, Hsla, IntoElement, ParentElement, Styled, div, px};
use gpui_component::h_flex;

/// The design's default disk alert bar.
const FULL_PERCENT: f32 = 90.0;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(tick) = state.latest() else {
        return vec![widgets::empty_card(i18n::tr("disk.title"), i18n::tr("common.waiting_sample"))];
    };
    let Some(disks) = tick.snapshot.disks.as_deref() else {
        return vec![widgets::empty_card(i18n::tr("disk.off"), i18n::tr("disk.off_body"))];
    };
    if disks.is_empty() {
        return vec![widgets::empty_card(i18n::tr("disk.no_volumes"), i18n::tr("disk.no_volumes_body"))];
    }

    disks
        .iter()
        .map(|d| {
            let total = d.total_bytes.max(1);
            let used = total.saturating_sub(d.available_bytes);
            let used_pct = used as f32 / total as f32 * 100.0;
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
                        .child(widgets::outline_pill(if d.is_removable {
                            i18n::tr("disk.removable")
                        } else {
                            d.kind.clone()
                        })),
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
                .child(
                    div()
                        .mt(px(7.))
                        .child(widgets::meter(used_pct / 100.0, Hsla::from(theme::fill_for(hot)), 6.)),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .mt(px(7.))
                        .text_size(px(10.))
                        .text_color(theme::text_dim())
                        .child(div().font_family(font::MONO).child(
                            t!(
                                "disk.rw",
                                read = format::rate(d.read_bytes_per_sec),
                                write = format::rate(d.write_bytes_per_sec)
                            )
                            .to_string(),
                        ))
                        .child(div().child(d.file_system.clone())),
                )
                .into_any_element()
        })
        .collect()
}
