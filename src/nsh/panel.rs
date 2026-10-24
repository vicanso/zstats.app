//! The panel's content as an NSH Rust root: a placeholder on Golden Dark's
//! ground that says what this build is, a few live figures from the
//! shared store, the ways to the other two windows, and Quit. Overview
//! takes its place in phase 4 of docs/nsh-migration.md.
//!
//! Its keys are the gpui panel's: `mod+1` … `mod+7` pick a tab and
//! `mod+p` pins the panel — `mod` is ⌘ on macOS and Ctrl elsewhere, the
//! gpui front end's `secondary-` for the same reason (Super is the
//! compositor's on a Hyprland desktop).

use crate::nsh::root::{self, AuxWindow, CARD_PAD, GUTTER, HostAction, Live};
use crate::nsh::{store, theme};
use crate::state::{Tab, ZStatsAppState};
use crate::{APP_NAME, format, i18n, palette, prefs};
use nsh_core::ui::prelude::*;

/// The figures the placeholder shows, enough to see the store move.
/// Not translated: a development build's lines, gone with it.
fn cpu_line(state: &ZStatsAppState) -> String {
    let cpu = state.latest().map_or_else(
        || format::PLACEHOLDER.to_owned(),
        |tick| format::pct(tick.snapshot.cpu.usage_percent),
    );
    format!("Processor {cpu}")
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
    let figure = move |f: fn(&ZStatsAppState) -> String| {
        text(live.text(f))
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(theme::color(palette::text))
            .tabular_nums()
    };
    view()
        .size_full()
        .bg(theme::color(palette::ground))
        .p(px(GUTTER))
        .gap(px(10.))
        .child(
            view()
                .rounded(px(12.))
                .bg(theme::color(palette::surface))
                .p(px(CARD_PAD))
                .gap(px(6.))
                .child(
                    text(APP_NAME)
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::color(palette::text)),
                )
                // Not translated: a development build's placeholder, gone
                // when Overview takes its place in phase 4.
                .child(
                    text("NSH front end — phase 2 of docs/nsh-migration.md")
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(theme::color(palette::text_muted)),
                )
                .child(figure(cpu_line))
                .child(figure(memory_line))
                .child(figure(store_line))
                .child(
                    text(env!("CARGO_PKG_VERSION"))
                        .text_size(px(11.))
                        .text_color(theme::color(palette::text_dim))
                        .tabular_nums(),
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
