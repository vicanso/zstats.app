//! GPU: how busy each accelerator is right now, and how much system
//! memory it holds. Rendered on the Hardware tab after the drives.
//!
//! Every figure is zstats' own (`SystemSnapshot::gpus`), read from the
//! driver's `PerformanceStatistics` through an `ioreg` child process that
//! the collector runs only while this tab is on screen
//! (`metrics::registry_channels`). The busy figure is an instantaneous
//! gauge, not an average, so the first read after the tab opens is
//! already a true one. There is no per-app split — the registry does not
//! attribute GPU time to clients — which is why zstats has no GPU alert
//! and this card names no culprit.

use super::widgets::{self, card};
use crate::hardware;
use crate::hot;
use crate::i18n;
use crate::state::ZStatsAppState;
use crate::theme;
use gpui::{AnyElement, Hsla, IntoElement, ParentElement, Styled, div, px};
use gpui_kit::component::h_flex;
use zstats::snapshot::GpuSnapshot;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(tick) = state.latest() else {
        return Vec::new();
    };
    let snapshot = &tick.snapshot;
    // Same two silences as the drives card: no such concept on this
    // platform, or the file turned the channel off.
    if !hardware::gpus_shown(state) {
        return Vec::new();
    }
    match snapshot.gpus.as_deref() {
        // Read and found no accelerator.
        Some([]) => Vec::new(),
        // The first read since the tab opened has not landed yet, or the
        // registry did not answer this round.
        None => vec![widgets::empty_card(
            i18n::tr("gpu.title"),
            i18n::tr("gpu.reading"),
        )],
        Some(gpus) => gpus.iter().map(gpu_card).collect(),
    }
}

fn gpu_card(gpu: &GpuSnapshot) -> AnyElement {
    let busy = gpu.utilization_percent;
    let hot = busy >= hot::GPU_HOT;
    let name = hardware::gpu_name(gpu);
    card()
        .child(widgets::card_header(
            i18n::tr("gpu.title"),
            Some(
                h_flex()
                    .items_center()
                    .gap(px(4.))
                    .min_w_0()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(10.))
                            .text_color(theme::text_dim())
                            .child(name),
                    )
                    .child(widgets::info_icon("gpu-tip", i18n::tr("gpu.tip")))
                    .into_any_element(),
            ),
        ))
        .child(
            div()
                .mt(px(8.))
                .child(widgets::big_number(format!("{busy:.0}"), "%")),
        )
        .child(div().mt(px(8.)).child(widgets::meter(
            busy / 100.0,
            Hsla::from(theme::gauge_for(hot)),
            6.,
        )))
        .child(widgets::kv_pairs(
            hardware::gpu_detail_rows(gpu)
                .into_iter()
                .map(|(k, v)| (k, v, false))
                .collect(),
        ))
        .into_any_element()
}
