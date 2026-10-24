//! What every NSH root shares — there is one per window (the panel,
//! settings, disk space), each on an engine thread of its own: the way to
//! the host, the store as an engine reads it, and how a root is made with
//! the theme on screen ([`make`]).
//!
//! A root never touches AppKit itself: what it needs of a window or the
//! process goes to the host as a [`HostAction`], which `host.rs` carries
//! to the main queue.
//!
//! **How a root follows the store** is the shape the pages will take. The
//! store is not a signal; a task on the engine thread waits on
//! `store::subscribe` and, while its window is on screen, bumps one
//! revision signal that every closure reading the store also reads
//! ([`Live`]). A change re-runs those closures and NSH redraws what came
//! out different — the gpui front end's "repaint the window on every
//! tick", narrowed to what moved. A hidden panel lays nothing out, as the
//! gpui one does not repaint (`CollectorPace::is_visible`); showing it
//! sends one more change (`host::mark_shown`).

use crate::nsh::{store, theme, widgets};
use crate::palette;
use crate::state::ZStatsAppState;
use nsh_base::button;
use nsh_core::ui::prelude::*;
use std::sync::OnceLock;

/// What a root asks of the AppKit side, which owns the windows and the
/// process.
#[derive(Clone, Copy, Debug)]
pub enum HostAction {
    Quit,
    Open(AuxWindow),
    Close(AuxWindow),
    /// The appearance NSH resolved for the panel, dark or not — at its
    /// first frame and whenever it turns ([`theme::follow`]).
    Appearance(bool),
}

/// The two ordinary windows beside the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuxWindow {
    Settings,
    Storage,
}

static HOST: OnceLock<Box<dyn Fn(HostAction) + Send + Sync>> = OnceLock::new();

/// Wires the roots to the host, before the first one mounts.
pub fn connect(host: impl Fn(HostAction) + Send + Sync + 'static) {
    let _ = HOST.set(Box::new(host));
}

/// Hands `action` to the host.
pub fn ask(action: HostAction) {
    if let Some(host) = HOST.get() {
        host(action);
    }
}

/// The store, as the closures of one engine read it: reading through it
/// subscribes the closure to the store's changes.
#[derive(Clone, Copy)]
pub struct Live(Signal<u64>);

impl Live {
    /// Starts following the store for the life of the current owner, while
    /// `on_screen` says the window is there to be drawn.
    pub fn follow(on_screen: fn(&ZStatsAppState) -> bool) -> Live {
        let revision = signal(0_u64);
        let changes = store::subscribe();
        spawn(async move {
            while changes.recv().await.is_ok() {
                if store::read(on_screen) {
                    revision.update(|n| *n += 1);
                }
            }
        });
        Live(revision)
    }

    /// A closure of `f` over the store, for a text or a style to bind.
    pub fn read<T: 'static>(self, f: fn(&ZStatsAppState) -> T) -> impl Fn() -> T + Clone + 'static {
        move || {
            self.0.get();
            store::read(f)
        }
    }

    /// [`Live::read`] for a text.
    pub fn text(self, f: fn(&ZStatsAppState) -> String) -> impl Fn() -> String + Clone + 'static {
        self.read(f)
    }
}

/// `views::GUTTER` and `widgets::CARD_PAD`.
pub const GUTTER: f32 = 12.;
pub const CARD_PAD: f32 = 12.;

/// Root `C`, in the appearance the theme preference asks for
/// ([`theme::appearance`]) and with the bundled mono face. Every window's
/// root is made here, so loading them again is how a change of theme
/// reaches them.
pub fn make<C: Component>() -> nsh_core::ui::Root
where
    C::Props: Default,
{
    widgets::fonts()
        .into_iter()
        .fold(nsh_core::ui::Root::of::<C>(), |root, font| root.font(font))
        .appearance(theme::appearance())
}

/// A text button in the second ink with the chip's fill under the
/// pointer — the placeholders' one kind of control.
pub fn quiet_button(label: String, on_press: impl Fn() + 'static) -> impl IntoElement {
    button()
        .h(px(24.))
        .px(px(10.))
        .rounded(px(6.))
        .hover(|s| s.bg(theme::color(palette::chip)))
        .on_press(on_press)
        .child(
            text(label)
                .text_size(px(12.))
                .text_color(theme::color(palette::text_muted)),
        )
}
