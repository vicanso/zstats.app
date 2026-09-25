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
use crate::format;
use crate::i18n;
use crate::state::ZStatsAppState;
use crate::theme;
use gpui::{AnyElement, Hsla, IntoElement, ParentElement, Styled, div, px};
use gpui_kit::component::h_flex;
use rust_i18n::t;
use zstats::snapshot::GpuSnapshot;

/// The busy bar turns accent here. The same line as a CPU core
/// (`overview::CORE_HOT`): a GPU pinned near full is the same kind of
/// fact as a core pinned near full. Display only, like every threshold in
/// `views/` — zstats has no GPU rule, and this fires nothing.
const GPU_HOT: f32 = 85.0;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(tick) = state.latest() else {
        return Vec::new();
    };
    let snapshot = &tick.snapshot;
    // Same two silences as the drives card: no such concept on this
    // platform, or the file turned the channel off.
    let file_allows = state
        .settings()
        .and_then(|s| s.collector.as_ref())
        .is_none_or(|c| c.collect_gpu);
    if !snapshot.capabilities.gpu || !file_allows {
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
    let hot = busy >= GPU_HOT;
    let name = match gpu.cores {
        Some(cores) => t!("gpu.name_cores", name = gpu.name.clone(), cores = cores).to_string(),
        None => gpu.name.clone(),
    };
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
                .child(widgets::big_number(format!("{busy:.0}"), "%", 20.)),
        )
        .child(div().mt(px(8.)).child(widgets::meter(
            busy / 100.0,
            Hsla::from(theme::fill_for(hot)),
            6.,
        )))
        .child(widgets::kv_pairs(detail_rows(gpu)))
        .into_any_element()
}

/// The figures under the bar, each only where the driver publishes it —
/// an Intel Mac's discrete GPU reports a different set than Apple
/// Silicon, and a row of `—` for a field this driver never has would
/// read as "not yet".
fn detail_rows(gpu: &GpuSnapshot) -> Vec<(String, String, bool)> {
    let pct = |label: &str, v: Option<f32>| v.map(|v| (i18n::tr(label), format::pct(v), false));
    let bytes = |label: &str, v: Option<u64>| v.map(|b| (i18n::tr(label), format::gb(b), false));
    [
        pct("gpu.renderer", gpu.renderer_utilization_percent),
        pct("gpu.tiler", gpu.tiler_utilization_percent),
        bytes("gpu.mem_in_use", gpu.memory_in_use_bytes),
        bytes("gpu.mem_allocated", gpu.memory_allocated_bytes),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(renderer: Option<f32>, in_use: Option<u64>) -> GpuSnapshot {
        GpuSnapshot {
            name: "Apple M4 Pro".into(),
            cores: Some(20),
            utilization_percent: 12.0,
            renderer_utilization_percent: renderer,
            tiler_utilization_percent: None,
            memory_in_use_bytes: in_use,
            memory_allocated_bytes: None,
        }
    }

    #[test]
    fn only_the_figures_the_driver_publishes_get_a_row() {
        assert_eq!(detail_rows(&gpu(None, None)).len(), 0);
        assert_eq!(detail_rows(&gpu(Some(10.0), None)).len(), 1);
        assert_eq!(detail_rows(&gpu(Some(10.0), Some(1 << 30))).len(), 2);
    }
}
