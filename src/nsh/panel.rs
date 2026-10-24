//! The panel's content as an NSH Rust root: a placeholder until Overview
//! takes its place in phase 4 of docs/nsh-migration.md. It shows what this
//! build is, the building blocks the pages will be made of
//! (`widgets.rs`) on live figures from the shared store, the ways to the
//! other two windows, and Quit — not translated, a development build's
//! page, gone with it.
//!
//! Its keys are the gpui panel's: `mod+1` … `mod+7` pick a tab and
//! `mod+p` pins the panel — `mod` is ⌘ on macOS and Ctrl elsewhere, the
//! gpui front end's `secondary-` for the same reason (Super is the
//! compositor's on a Hyprland desktop).

use crate::nsh::root::{self, AuxWindow, CARD_PAD, GUTTER, HostAction, Live};

use crate::nsh::widgets::{self, Choice};
use crate::nsh::{store, theme};
use crate::state::{Tab, ZStatsAppState};
use crate::{APP_NAME, format, i18n, palette, prefs};
use nsh_core::ui::prelude::*;

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

#[component]
pub fn Panel() -> impl IntoElement {
    let live = Live::follow(ZStatsAppState::panel_visible);
    // The appearance this engine resolved, for the palette outside it: at
    // the first frame, and again whenever the system's turns.
    effect(|| root::ask(HostAction::Appearance(dark())));
    for (i, tab) in Tab::ALL.into_iter().enumerate() {
        on_key(&format!("mod+{}", i + 1), move |_| {
            store::update(|state, fx| state.set_tab(tab, fx));
        });
    }
    on_key("mod+p", |_| {
        prefs::set_pinned(!prefs::pinned());
        store::changed();
    });
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
        .size_full()
        .bg(theme::color(palette::ground))
        .p(px(GUTTER))
        .gap(px(10.))
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
        .child(view().flex_1())
        .child(
            view()
                .flex_row()
                .gap(px(4.))
                .child(root::quiet_button(i18n::tr("tabs.settings"), || {
                    root::ask(HostAction::Open(AuxWindow::Settings));
                }))
                .child(root::quiet_button(i18n::tr("disk.storage_title"), || {
                    root::ask(HostAction::Open(AuxWindow::Storage));
                }))
                .child(view().flex_1())
                .child(root::quiet_button(i18n::tr("common.quit"), || {
                    root::ask(HostAction::Quit);
                })),
        )
}
