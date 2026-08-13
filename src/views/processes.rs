//! Processes: the top-N table, ranked by the 60s rolling average.

use super::widgets;
use crate::font;
use crate::format;
use crate::i18n;
use crate::procscan::ProcState;
use crate::state::{ProcSort, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, div, px,
};
use gpui_component::{Icon, IconName, Sizable, Size, h_flex, v_flex};
use rust_i18n::t;
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
    let Some(mut rows) = ranked(tick) else {
        return vec![widgets::empty_card(
            i18n::tr("processes.off"),
            i18n::tr("processes.off_body"),
        )];
    };
    // `ranked` hands back CPU order, which Overview also depends on; the
    // user's choice is applied here so it only affects this tab.
    match state.proc_sort() {
        ProcSort::Cpu => {}
        ProcSort::Memory => rows.sort_by_key(|(p, _)| std::cmp::Reverse(p.memory_bytes)),
        // Case-insensitive, or every capitalised app name would sort ahead of
        // every lowercase daemon — not what "by name" means to someone
        // scanning for one.
        ProcSort::Name => rows.sort_by_key(|(p, _)| p.name.to_lowercase()),
    }

    let total = tick
        .snapshot
        .total_processes
        .map(|t| t.to_string())
        .unwrap_or_else(|| format::PLACEHOLDER.to_string());
    let selected = state.selected_pid();
    let only_abnormal = state.only_abnormal();

    let list = widgets::list_shell()
        .child(widgets::list_header(
            t!(
                "processes.count_of",
                shown = if only_abnormal {
                    state.abnormal().len()
                } else {
                    rows.len() + state.abnormal().len()
                },
                total = total
            )
            .to_string(),
            Some(
                h_flex()
                    .items_center()
                    .gap(px(5.))
                    .child(abnormal_filter(state))
                    .when(!state.abnormal().is_empty(), |d| {
                        d.child(sort_control(state))
                    })
                    .into_any_element(),
            ),
        ))
        .children(abnormal_rows(state, !only_abnormal && !rows.is_empty()))
        .children({
            let shown = if only_abnormal { 0 } else { rows.len() };
            rows.into_iter()
                .take(shown)
                .enumerate()
                .map(move |(i, (p, avg))| {
                    let is_last = i + 1 == shown;
                    let hot = avg > HOT_PERCENT;
                    let expanded = selected == Some(p.pid);
                    let pid = p.pid;

                    let mut row = v_flex()
                        .id(("proc", pid as usize))
                        .cursor_pointer()
                        .px(px(13.))
                        .py(px(9.))
                        .when(!is_last, |d| {
                            d.border_b(px(1.)).border_color(theme::border_subtle())
                        })
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
                                    h_flex()
                                        .items_center()
                                        .gap(px(5.))
                                        .min_w_0()
                                        .child(
                                            // Almost every process is asleep at any
                                            // instant, so the state is only worth the
                                            // space when it is something that should
                                            // not persist — a zombie, a stopped
                                            // process. Those get the accent too.
                                            div()
                                                .text_color(if format::notable_status(&p.status) {
                                                    theme::accent_light()
                                                } else {
                                                    theme::text_dim()
                                                })
                                                .child(if format::notable_status(&p.status) {
                                                    t!(
                                                        "processes.pid_status",
                                                        pid = p.pid,
                                                        status = p.status.clone()
                                                    )
                                                    .to_string()
                                                } else {
                                                    t!("processes.pid_only", pid = p.pid)
                                                        .to_string()
                                                }),
                                        )
                                        .children(state.sustained_load(p.pid).map(|d| {
                                            // This never trips the alert — that asks
                                            // whether it is over the line right now,
                                            // and it never is. So the badge is the
                                            // only place it can announce itself.
                                            div()
                                                .flex_none()
                                                .rounded(px(3.))
                                                .px(px(4.))
                                                .bg(theme::accent_wash(14))
                                                .text_size(px(9.))
                                                .text_color(theme::accent_light())
                                                .child(
                                                    t!(
                                                        "processes.sustained",
                                                        duration = format::uptime(d.as_secs())
                                                    )
                                                    .to_string(),
                                                )
                                        })),
                                )
                                .child(
                                    div()
                                        .font_family(font::MONO)
                                        .text_color(theme::text_muted())
                                        .child(format::memory(p.memory_bytes)),
                                ),
                        )
                        .child(div().mt(px(6.)).child(widgets::meter(
                            (avg as f32) / BAR_FULL_PERCENT,
                            Hsla::from(theme::fill_for(hot)),
                            4.,
                        )));

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
                        let name = p.name.clone();
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
                                .child(widgets::kv_columns(detail))
                                .when(safe_to_kill(pid), |col| col.child(kill_button(pid, name))),
                        );
                    }

                    row.on_click(move |_, _window, cx| {
                        cx.global::<ZStatsGlobalStore>()
                            .clone()
                            .update(cx, |state, cx| state.toggle_pid(pid, cx));
                    })
                })
        });

    vec![list.into_any_element()]
}

fn kill_button(pid: u32, name: String) -> AnyElement {
    let label = i18n::tr("processes.kill");
    div()
        .id(("kill", pid as usize))
        .mt(px(10.))
        .h(px(26.))
        .rounded(px(7.))
        .border_1()
        .border_color(theme::accent_wash(45))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|d| d.bg(theme::accent_wash(10)))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let name = name.clone();
            crate::confirm::ask(
                window,
                cx,
                i18n::tr("processes.kill_title"),
                t!("processes.kill_body", name = name, pid = pid).to_string(),
                i18n::tr("processes.kill_ok"),
                move || kill(pid),
            );
        })
        .child(
            div()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::accent_light())
                .child(label),
        )
        .into_any_element()
}

fn abnormal_badge(pid: u32, state: ProcState) -> AnyElement {
    let tip = i18n::tr(match state {
        ProcState::Zombie => "processes.zombie_hint",
        ProcState::Stopped => "processes.stopped_hint",
    });
    h_flex()
        .items_center()
        .gap(px(4.))
        .flex_none()
        .child(
            div()
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent_light())
                .child(state.label()),
        )
        .child(
            div()
                .id(("abnormal-info", pid as usize))
                .flex_none()
                .p(px(1.))
                .tooltip(widgets::wrap_tooltip(tip))
                .child(
                    Icon::new(IconName::Info)
                        .with_size(Size::Size(px(12.)))
                        .text_color(Hsla::from(theme::text_dim())),
                ),
        )
        .into_any_element()
}

/// SIGTERM. Activity Monitor's "Quit", not Force Quit.
fn kill(pid: u32) {
    if !safe_to_kill(pid) {
        eprintln!("refusing to kill pid {pid}");
        return;
    }
    std::thread::spawn(move || {
        match std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output()
        {
            Ok(out) if out.status.success() => {}
            Ok(out) => eprintln!(
                "kill {pid} failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => eprintln!("kill {pid}: {e}"),
        }
    });
}

fn safe_to_kill(pid: u32) -> bool {
    pid > 1 && pid != std::process::id()
}

/// The filter chip in the list header.
///
/// Only appears when there is something abnormal — on a healthy machine the
/// header keeps its plain "60s avg" caption and no control is offered for a
/// filter that would empty the list.
fn abnormal_filter(state: &ZStatsAppState) -> AnyElement {
    let count = state.abnormal().len();
    if count == 0 {
        return sort_control(state);
    }
    let active = state.only_abnormal();
    div()
        .id("filter-abnormal")
        .cursor_pointer()
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(theme::accent_wash(45))
        .when(active, |d| d.bg(theme::accent()))
        .px(px(7.))
        .py(px(1.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(if active {
            gpui::white()
        } else {
            Hsla::from(theme::accent_light())
        })
        .child(t!("processes.abnormal_filter", count = count).to_string())
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.toggle_only_abnormal(cx));
        })
        .into_any_element()
}

/// Rows for processes stuck in a state that should not persist.
///
/// Collected separately from the metrics snapshot and shown at the top of the
/// same list: zstats keeps the top N by CPU then memory, and a zombie scores
/// near zero on both, so these would never surface in the table below however
/// far it is scrolled.
fn abnormal_rows(state: &ZStatsAppState, more_follows: bool) -> Vec<AnyElement> {
    let found = state.abnormal();
    let total = found.len();
    found
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let is_last = !more_follows && i + 1 == total;
            let observed = state.abnormal_observed(p.pid);
            v_flex()
                .px(px(12.))
                .py(px(8.))
                .when(!is_last, |d| {
                    d.border_b(px(1.)).border_color(theme::border_subtle())
                })
                .bg(theme::accent_wash(7))
                .child(
                    h_flex()
                        .items_baseline()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::text())
                                .truncate()
                                .child(p.name.clone()),
                        )
                        .child(abnormal_badge(p.pid, p.state)),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .gap(px(8.))
                        .mt(px(2.))
                        .text_size(px(9.))
                        .text_color(theme::text_dim())
                        .child(
                            div().child(
                                t!(
                                    "processes.abnormal_meta",
                                    ppid = p.parent_pid,
                                    age = format::uptime(p.age.as_secs())
                                )
                                .to_string(),
                            ),
                        )
                        // A lower bound only: the kernel does not record when
                        // a process became a zombie, so this counts from when
                        // we first saw it.
                        .children(observed.map(|d| {
                            div().child(
                                t!(
                                    "processes.abnormal_observed",
                                    observed = format!("≥{}", format::uptime(d.as_secs()))
                                )
                                .to_string(),
                            )
                        })),
                )
                .into_any_element()
        })
        .collect()
}

/// Cycles the list ordering. A single button rather than a menu — there are
/// three orderings and 285px of width.
fn sort_control(state: &ZStatsAppState) -> AnyElement {
    let sort = state.proc_sort();
    div()
        .id("sort-processes")
        .cursor_pointer()
        .flex_none()
        .rounded(px(4.))
        .px(px(5.))
        .py(px(1.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .hover(|d| d.bg(theme::surface_raised()))
        .child(format!("↓ {}", i18n::tr(sort.label_key())))
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.cycle_proc_sort(cx));
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn will_not_kill_init_or_self() {
        assert!(!safe_to_kill(0));
        assert!(!safe_to_kill(1));
        assert!(!safe_to_kill(std::process::id()));
        assert!(safe_to_kill(std::process::id().saturating_add(1000).max(2)));
    }
}
