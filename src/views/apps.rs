//! Applications: whole process trees, summed.
//!
//! This is what makes a multi-process app legible — a browser's 37 helpers
//! never individually cross a threshold that the app as a whole blows past.
//! zstats computes the sums over the FULL process table, before the top-N
//! truncation that `processes` is subject to.

use super::widgets;
use crate::font;
use crate::format;
use crate::i18n;
use rust_i18n::t;
use crate::state::{ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px,
};
use gpui_component::{h_flex, v_flex};

const HOT_PERCENT: f32 = 200.0;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    let Some(tick) = state.latest() else {
        return vec![widgets::empty_card(i18n::tr("apps.title"), i18n::tr("common.waiting_sample"))];
    };
    let Some(groups) = tick.snapshot.process_groups.as_deref() else {
        return vec![widgets::empty_card(i18n::tr("apps.off"), i18n::tr("apps.off_body"))];
    };

    let mut rows: Vec<_> = groups.iter().collect();
    rows.sort_by(|a, b| b.cpu_usage_percent.total_cmp(&a.cpu_usage_percent));
    let selected = state.selected_app();

    let list = widgets::list_shell()
        .child(widgets::list_header(
            i18n::tr("apps.title"),
            Some(widgets::note(i18n::tr("apps.tree_totals"))),
        ))
        .children(rows.into_iter().map(|g| {
            let hot = g.cpu_usage_percent > HOT_PERCENT;
            let expanded = selected == Some(g.root_pid);
            let root_pid = g.root_pid;

            let mut row = v_flex()
                .id(("app", root_pid as usize))
                .cursor_pointer()
                .px(px(13.))
                .py(px(10.))
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
                                .child(g.name.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family(font::MONO)
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(theme::text_for(hot))
                                .child(format::pct_col(g.cpu_usage_percent)),
                        ),
                )
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .mt(px(5.))
                        .child(widgets::outline_pill(
                            t!("apps.n_processes", count = g.process_count).to_string(),
                        ))
                        .child(
                            div()
                                .font_family(font::MONO)
                                .text_size(px(9.5))
                                .text_color(theme::text_muted())
                                .child(format!(
                                    "{} · IO {}",
                                    format::gb(g.memory_bytes),
                                    format::rate(g.read_bytes_per_sec)
                                )),
                        ),
                );

            if expanded {
                // zstats aggregates the tree but does not hand back the
                // member list, so this shows the totals in full rather than
                // the per-member breakdown the design sketches.
                let detail = vec![
                    (i18n::tr("apps.root_pid"), g.root_pid.to_string()),
                    (i18n::tr("apps.processes"), g.process_count.to_string()),
                    (i18n::tr("apps.memory"), format::gb(g.memory_bytes)),
                    (i18n::tr("apps.read"), format::rate(g.read_bytes_per_sec)),
                    (i18n::tr("apps.write"), format::rate(g.write_bytes_per_sec)),
                    (i18n::tr("apps.cpu"), format::pct(g.cpu_usage_percent)),
                ];
                row = row.child(
                    v_flex()
                        .mt(px(8.))
                        .p(px(10.))
                        .rounded(px(8.))
                        .bg(theme::inset())
                        .child(widgets::kv_columns(detail))
                        .child(
                            div().mt(px(6.)).pt(px(6.)).border_t(px(1.)).border_color(theme::border()).child(
                                widgets::note(i18n::tr("apps.tree_note")),
                            ),
                        ),
                );
            }

            row.on_click(move |_, _window, cx| {
                cx.global::<ZStatsGlobalStore>()
                    .clone()
                    .update(cx, |state, cx| state.toggle_app(root_pid, cx));
            })
        }));

    vec![list.into_any_element()]
}
