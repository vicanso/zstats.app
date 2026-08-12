//! Processes: the top-N table, ranked by the 60s rolling average.

use super::widgets;
use crate::font;
use crate::format;
use crate::i18n;
use rust_i18n::t;
use crate::state::{ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, div, px,
};
use gpui_component::{h_flex, v_flex};
use zstats::Tick;
use zstats::snapshot::ProcessSnapshot;

/// One whole core. Past this a process is worth flagging visually — it is
/// also the scale the bar is drawn against (8 cores' worth = full).
const BAR_FULL_PERCENT: f32 = 800.0;
pub(super) const HOT_PERCENT: f64 = 200.0;

/// Ranked by the 60s rolling average so the order is stable enough to read.
pub(super) fn ranked(tick: &Tick) -> Option<Vec<(&ProcessSnapshot, f64)>> {
    ranked_by(tick, true)
}

/// Same table, instantaneous sample — Overview uses this so the list
/// agrees with the Processor % above it.
pub(super) fn ranked_live(tick: &Tick) -> Option<Vec<(&ProcessSnapshot, f64)>> {
    ranked_by(tick, false)
}

fn ranked_by(tick: &Tick, rolling: bool) -> Option<Vec<(&ProcessSnapshot, f64)>> {
    let processes = tick.snapshot.processes.as_deref()?;
    let mut rows: Vec<_> = processes
        .iter()
        .map(|p| {
            let cpu = if rolling {
                tick.process_stats
                    .get(&p.pid)
                    .map(|s| s.cpu_avg)
                    .unwrap_or(p.cpu_usage_percent as f64)
            } else {
                p.cpu_usage_percent as f64
            };
            (p, cpu)
        })
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    Some(rows)
}

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(tick) = state.latest() else {
        return vec![widgets::empty_card(
            i18n::tr("processes.title"),
            i18n::tr("common.waiting_sample"),
        )];
    };
    let Some(rows) = ranked(tick) else {
        return vec![widgets::empty_card(i18n::tr("processes.off"), i18n::tr("processes.off_body"))];
    };

    let total = tick
        .snapshot
        .total_processes
        .map(|t| t.to_string())
        .unwrap_or_else(|| format::PLACEHOLDER.to_string());
    let selected = state.selected_pid();

    let list = widgets::list_shell()
        .child(widgets::list_header(
            t!("processes.count_of", shown = rows.len(), total = total).to_string(),
            Some(widgets::note(i18n::tr("processes.avg_60s"))),
        ))
        .children(rows.into_iter().map(|(p, avg)| {
            let hot = avg > HOT_PERCENT;
            let expanded = selected == Some(p.pid);
            let pid = p.pid;

            let mut row = v_flex()
                .id(("proc", pid as usize))
                .cursor_pointer()
                .px(px(13.))
                .py(px(9.))
                .border_b(px(1.))
                .border_color(theme::border_subtle())
                .when(expanded, |d| d.bg(theme::surface_raised()))
                .hover(|d| d.bg(theme::surface_raised()))
                .child(
                    h_flex()
                        .items_baseline()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::text())
                                .truncate()
                                .child(p.name.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family(font::MONO)
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(theme::text_for(hot))
                                .child(format::pct_col(avg as f32)),
                        ),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .gap(px(8.))
                        .mt(px(2.))
                        .text_size(px(10.))
                        .child(
                            div()
                                .text_color(theme::text_dim())
                                .child(
                                    t!("processes.pid_status", pid = p.pid, status = p.status.clone())
                                        .to_string(),
                                ),
                        )
                        .child(
                            div()
                                .font_family(font::MONO)
                                .text_color(theme::text_muted())
                                .child(format::gb(p.memory_bytes)),
                        ),
                )
                .child(
                    div().mt(px(6.)).child(widgets::meter(
                        (avg as f32) / BAR_FULL_PERCENT,
                        Hsla::from(theme::fill_for(hot)),
                        4.,
                    )),
                );

            if expanded {
                let detail = vec![
                    (
                        i18n::tr("processes.user"),
                        p.user_id.clone().unwrap_or_else(|| "—".into()),
                    ),
                    (i18n::tr("processes.run"), format::uptime(p.run_time_secs)),
                    (
                        i18n::tr("processes.parent"),
                        p.parent_pid.map_or("—".to_string(), |v| v.to_string()),
                    ),
                    (
                        i18n::tr("processes.disk_io"),
                        match (p.read_bytes_per_sec, p.write_bytes_per_sec) {
                            (None, None) => format::PLACEHOLDER.to_string(),
                            (r, w) => format!("{} / {}", format::rate(r), format::rate(w)),
                        },
                    ),
                ];
                row = row.child(
                    v_flex()
                        .mt(px(8.))
                        .p(px(10.))
                        .rounded(px(8.))
                        .bg(theme::inset())
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(theme::text_muted())
                                .child(p.cmd.clone()),
                        )
                        .child(widgets::kv_columns(detail)),
                );
            }

            row.on_click(move |_, _window, cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.toggle_pid(pid, cx));
            })
        }));

    vec![list.into_any_element()]
}
