//! The panel's content as an NSH Rust root: a placeholder on Golden Dark's
//! ground that says what this build is, a few live figures from the
//! shared store, and Quit. Overview takes its place in phase 4 of
//! docs/nsh-migration.md.
//!
//! The root runs on its engine thread and never touches AppKit itself: what
//! it needs of the window or the process goes to the host as a
//! [`HostAction`], which `host.rs` carries to the main queue.
//!
//! **How it follows the store** is the shape the pages will take. The
//! store is not a signal; a task on the engine thread waits on
//! `store::subscribe` and, while the panel is on screen, bumps one
//! revision signal that every closure reading the store also reads
//! ([`Live`]). A change re-runs those closures and NSH redraws what came
//! out different — the gpui front end's "repaint the panel on every tick",
//! with the repaint narrowed to what moved. A hidden panel lays nothing
//! out, as the gpui one does not repaint (`CollectorPace::is_visible`);
//! showing it sends one more change (`host::mark_shown`).

use crate::color;
use crate::nsh::store;
use crate::state::ZStatsAppState;
use crate::{APP_NAME, format, i18n, palette};
use nsh_base::button;
use nsh_core::ui::prelude::*;
use std::sync::OnceLock;

/// What the panel asks of the AppKit side, which owns the window and the
/// process.
#[derive(Clone, Copy, Debug)]
pub enum HostAction {
    Quit,
}

static HOST: OnceLock<Box<dyn Fn(HostAction) + Send + Sync>> = OnceLock::new();

/// Wires the panel to the host, before the root mounts.
pub fn connect(host: impl Fn(HostAction) + Send + Sync + 'static) {
    let _ = HOST.set(Box::new(host));
}

fn host(action: HostAction) {
    if let Some(host) = HOST.get() {
        host(action);
    }
}

/// Golden Dark's ground, `#1c1c1e`. The gpui front end takes it from the
/// kit's theme file (`themes/golden.json`) rather than from the palette,
/// so there is no token for it yet; phase 3 of docs/nsh-migration.md gives
/// the panel its themes and this a place in `palette.rs`.
const GROUND: u32 = 0x1c1c1eff;

/// A palette token as NSH's colour.
fn paint(c: color::Rgba) -> Rgba {
    c.into()
}

/// `views::GUTTER` and `widgets::CARD_PAD`.
const GUTTER: f32 = 12.;
const CARD_PAD: f32 = 12.;

/// The store, as the closures of one engine read it: reading through it
/// subscribes the closure to the store's changes.
#[derive(Clone, Copy)]
struct Live(Signal<u64>);

impl Live {
    /// Starts following the store, for the life of the current owner.
    fn follow() -> Live {
        let revision = signal(0_u64);
        let changes = store::subscribe();
        spawn(async move {
            while changes.recv().await.is_ok() {
                if store::read(ZStatsAppState::panel_visible) {
                    revision.update(|n| *n += 1);
                }
            }
        });
        Live(revision)
    }

    /// A closure of `f` over the store, for a text to bind.
    fn text(self, f: fn(&ZStatsAppState) -> String) -> impl Fn() -> String + 'static {
        move || {
            self.0.get();
            store::read(f)
        }
    }
}

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
        "{} alert cards · tab {:?} · visit {}",
        state.alerts().len(),
        state.tab(),
        state.panel_visit()
    )
}

#[component]
pub fn Panel() -> impl IntoElement {
    let live = Live::follow();
    let figure = move |f: fn(&ZStatsAppState) -> String| {
        text(live.text(f))
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(paint(palette::text()))
            .tabular_nums()
    };
    view()
        .size_full()
        .bg(rgba(GROUND))
        .p(px(GUTTER))
        .gap(px(10.))
        .child(
            view()
                .rounded(px(12.))
                .bg(paint(palette::surface()))
                .p(px(CARD_PAD))
                .gap(px(6.))
                .child(
                    text(APP_NAME)
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(paint(palette::text())),
                )
                // Not translated: a development build's placeholder, gone
                // when Overview takes its place in phase 4.
                .child(
                    text("NSH front end — phase 2 of docs/nsh-migration.md")
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(paint(palette::text_muted())),
                )
                .child(figure(cpu_line))
                .child(figure(memory_line))
                .child(figure(store_line))
                .child(
                    text(env!("CARGO_PKG_VERSION"))
                        .text_size(px(11.))
                        .text_color(paint(palette::text_dim()))
                        .tabular_nums(),
                ),
        )
        .child(view().flex_1())
        .child(
            view().flex_row().justify_end().child(
                button()
                    .h(px(24.))
                    .px(px(10.))
                    .rounded(px(6.))
                    .hover(|s| s.bg(paint(palette::chip())))
                    .on_press(|| host(HostAction::Quit))
                    .child(
                        text(i18n::tr("common.quit"))
                            .text_size(px(12.))
                            .text_color(paint(palette::text_muted())),
                    ),
            ),
        )
}
