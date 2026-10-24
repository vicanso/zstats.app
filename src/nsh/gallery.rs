//! The building blocks on live figures (`widgets.rs`): a debug build shows
//! it in Overview's place while `ZSTATS_GALLERY` is set, to look at a
//! block before the page that uses it is ported. Not translated — a
//! development build's page; it goes when the last page arrives
//! (docs/nsh-migration.md, phase 4).

use crate::nsh::root::{self, AuxWindow, CARD_GAP, CARD_PAD, HostAction, Live};
use crate::nsh::widgets::{self, Choice};
use crate::nsh::{store, theme};
use crate::state::ZStatsAppState;
use crate::{APP_NAME, appicon, format, i18n, palette, prefs};
use nsh_core::ui::prelude::*;
use std::cmp::Reverse;

/// The processor's figure, for the headline.
fn cpu_value(state: &ZStatsAppState) -> String {
    state.latest().map_or_else(
        || format::PLACEHOLDER.to_owned(),
        |tick| format!("{:.0}", tick.snapshot.cpu.usage_percent),
    )
}

fn cpu_fraction(state: &ZStatsAppState) -> f32 {
    state
        .latest()
        .map_or(0., |tick| tick.snapshot.cpu.usage_percent / 100.)
}

/// How long the collector has been sampling, as a pill.
fn span(state: &ZStatsAppState) -> String {
    format!("{} alerts", state.alerts().len())
}

/// Five processes by the memory they hold, with their marks: the first
/// three that run from an app with an icon, then two that do not — one
/// of each kind of mark, whatever is running.
fn busiest(state: &ZStatsAppState) -> Vec<(u32, String, String, String)> {
    let Some(processes) = state
        .latest()
        .and_then(|tick| tick.snapshot.processes.as_deref())
    else {
        return Vec::new();
    };
    let mut by_memory: Vec<_> = processes.iter().collect();
    by_memory.sort_by_key(|p| Reverse(p.memory_bytes));
    let (apps, others): (Vec<_>, Vec<_>) = by_memory
        .into_iter()
        .partition(|p| appicon::icon_for_pid(p.pid, &p.name).is_some());
    apps.into_iter()
        .take(3)
        .chain(others.into_iter().take(2))
        .map(|p| {
            let title = p.display_name.clone().unwrap_or_else(|| p.name.clone());
            (p.pid, p.name.clone(), title, format::gb(p.memory_bytes))
        })
        .collect()
}

fn total(state: &ZStatsAppState) -> String {
    state.latest().map_or_else(
        || format::PLACEHOLDER.to_owned(),
        |tick| format::gb(tick.snapshot.memory.total_bytes),
    )
}

fn available(state: &ZStatsAppState) -> String {
    state.latest().map_or_else(
        || format::PLACEHOLDER.to_owned(),
        |tick| format::gb(tick.snapshot.memory.available_bytes),
    )
}

fn memory_line(state: &ZStatsAppState) -> String {
    match state.latest() {
        Some(tick) => format!(
            "Memory {} available of {}",
            format::gb(tick.snapshot.memory.available_bytes),
            format::gb(tick.snapshot.memory.total_bytes)
        ),
        None => format!("Memory {}", format::PLACEHOLDER),
    }
}

fn store_line(state: &ZStatsAppState) -> String {
    format!(
        "{} alert cards · tab {:?} · visit {}{}",
        state.alerts().len(),
        state.tab(),
        state.panel_visit(),
        if prefs::pinned() { " · pinned" } else { "" }
    )
}

/// The sample, as the page Overview's tab shows for now.
pub fn page(live: Live) -> AnyElement {
    let line = move |f: fn(&ZStatsAppState) -> String| {
        text(live.text(f))
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(theme::color(palette::text_muted))
            .tabular_nums()
    };
    // The page's own, not the store's: a sample of each control.
    let open = signal(false);
    let form = signal(false);
    let sort = signal("CPU");
    let sort_choice = move |label: &'static str| Choice {
        label: label.to_owned(),
        on: Box::new(move || sort.get() == label),
        choose: Box::new(move || sort.set(label)),
    };
    view()
        .gap(px(CARD_GAP))
        .child(
            widgets::card()
                .gap(px(8.))
                .child(widgets::card_header(
                    "Processor",
                    Some(
                        widgets::outline_pill(live.text(span))
                            .tooltip("Alert cards today")
                            .into_element(),
                    ),
                ))
                .child(widgets::big_number(live.text(cpu_value), "%"))
                .child(widgets::meter(
                    live.read(cpu_fraction),
                    palette::gauge,
                    6.,
                ))
                .child(
                    view()
                        .flex_row()
                        .justify_between()
                        .child(widgets::big_figure_with(
                            live.text(available),
                            "available".into(),
                        ))
                        .child(widgets::big_figure(live.text(total))),
                ),
        )
        .child(
            widgets::list_shell()
                .gap(px(8.))
                .pb(px(CARD_PAD))
                .child(
                    view().px(px(CARD_PAD)).pt(px(CARD_PAD)).child(widgets::card_header(
                        APP_NAME,
                        Some(widgets::menu_trigger(
                            move || format!("↓ {}", sort.get()),
                            || "Orders the rows.".to_owned(),
                            vec![sort_choice("CPU"), sort_choice("Memory"), sort_choice("Name")],
                        )),
                    )),
                )
                .child(
                    nsh_base::button()
                        .flex_row()
                        .items_center()
                        .gap(px(4.))
                        .px(px(CARD_PAD))
                        .on_press(move || open.update(|o| *o = !*o))
                        .child(widgets::expand_mark(move || open.get()))
                        .child(
                            text("NSH front end — phase 3 of docs/nsh-migration.md")
                                .text_size(px(12.))
                                .text_color(theme::color(palette::text)),
                        ),
                )
                .child(when(
                    move || open.get(),
                    move || {
                        view()
                            .gap(px(2.))
                            .px(px(CARD_PAD))
                            .child(line(memory_line))
                            .child(line(store_line))
                    },
                ))
                .child(
                    view()
                        .flex_row()
                        .gap(px(6.))
                        .px(px(CARD_PAD))
                        .child(widgets::option_chip("Short", move || !form.get(), move || form.set(false)))
                        .child(widgets::option_chip("Long", move || form.get(), move || form.set(true))),
                )
                .child(
                    view()
                        .flex_row()
                        .px(px(CARD_PAD))
                        .child(
                            text(env!("CARGO_PKG_VERSION"))
                                .font_family(widgets::MONO)
                                .text_size(px(11.))
                                .text_color(theme::color(palette::text_dim)),
                        )
                        .tooltip(widgets::wrap_tooltip(
                            "The version this build was made from.\n\nRead from Cargo.toml at compile time; a development build carries the next release's number until it is cut.",
                        )),
                ),
        )
        .child(
            widgets::list_shell()
                .py(px(6.))
                .child(dynamic(move || {
                    live.track();
                    store::read(busiest)
                        .into_iter()
                        .map(|(pid, name, title, memory)| {
                            view()
                                .flex_row()
                                .items_center()
                                .gap(px(8.))
                                .px(px(CARD_PAD))
                                .py(px(4.))
                                .child(widgets::app_mark(pid, &name, &title))
                                .child(
                                    text(title)
                                        .flex_1()
                                        .number_of_lines(1u32)
                                        .text_size(px(12.))
                                        .text_color(theme::color(palette::text)),
                                )
                                .child(
                                    text(memory)
                                        .tabular_nums()
                                        .text_size(px(12.))
                                        .text_color(theme::color(palette::text_muted)),
                                )
                        })
                        .collect::<Vec<_>>()
                })),
        )
        .child(
            view()
                .flex_row()
                .gap(px(4.))
                .child(root::quiet_button(i18n::tr("disk.storage_title"), || {
                    root::ask(HostAction::Open(AuxWindow::Storage));
                }))
                .child(root::quiet_button("Confirm…".to_owned(), move || {
                    widgets::confirm(
                        "Clear the sample?".to_owned(),
                        "The sample's chips go back to Short. Nothing on the machine changes."
                            .to_owned(),
                        "Clear".to_owned(),
                        move || form.set(false),
                    );
                })),
        )
        .into_element()
}
