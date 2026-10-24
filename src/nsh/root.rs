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
    /// Overview's form changed (the footer's toggle): the panel takes that
    /// form's height.
    FitForm,
    /// Sample now rather than at the end of the cadence: a volume was just
    /// ejected, and the next snapshot should stop listing it
    /// (`CollectorPace::wake`).
    Wake,
    /// A preference every root was made with changed — the theme, the
    /// language, the panel's opacity: every window's root is made again
    /// ([`make`]), and the menus and the tray follow.
    Restyle,
    /// The system's folder panel, for what [`FolderPick`] says; NSH has
    /// none of its own.
    PickFolder(FolderPick),
}

/// What a folder picked in the system's panel is for (the disk-space
/// window).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderPick {
    /// The analysis's scope: picking selects, the run chip walks.
    Analysis,
    /// Folders the analysis leaves out — several at once.
    Exclude,
    /// Where to look for duplicates: picking is the ask, the search runs.
    Dupes,
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

/// Quit (`mod+q`) and, in an ordinary window, close (`mod+w`) — the
/// gpui front end's `Quit` and `CloseWindow` bindings. On macOS the host's
/// own menu carries both, so a root binds them only elsewhere, where a
/// window has no menu bar.
pub fn bind_window_keys(window: Option<AuxWindow>) {
    if cfg!(target_os = "macos") {
        return;
    }
    on_key("mod+q", |_| ask(HostAction::Quit));
    if let Some(aux) = window {
        on_key("mod+w", move |_| ask(HostAction::Close(aux)));
    }
}

/// Hands `action` to the host.
pub fn ask(action: HostAction) {
    if let Some(host) = HOST.get() {
        host(action);
    }
}

/// The panel's showings as they go (`ZStatsAppState::panel_visit`, bumped
/// as it hides), heard whether or not it is on screen — `Live` hears
/// nothing while it is hidden. What must not outlive a showing reads it.
#[derive(Clone, Copy)]
struct Visits(ReadSignal<u32>);

/// Follows the panel's showings for the life of the current owner — the
/// panel's root, for what is made under it ([`close_on_hide`]).
pub fn follow_visits() {
    let visit = signal(store::read(ZStatsAppState::panel_visit));
    let changes = store::subscribe();
    spawn(async move {
        while changes.recv().await.is_ok() {
            let now = store::read(ZStatsAppState::panel_visit);
            if visit.try_get() != Some(now) {
                let _ = visit.try_set(now);
            }
        }
    });
    provide_context(Visits(visit.read_only()));
}

/// Closes `open` as the panel hides: the panel is hidden, not destroyed,
/// and a menu open as it went away was still open the next time it came
/// up (gpui's dropdowns did the same; their ids carried the visit). Does
/// nothing outside the panel.
pub fn close_on_hide(open: Signal<bool>) {
    if let Some(Visits(visit)) = use_context::<Visits>() {
        effect(move || {
            visit.get();
            open.set(false);
        });
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

    /// Subscribes the closure it is read in to the store's changes, for
    /// one that reads the store itself (a list made again as it moves).
    pub fn track(self) {
        self.0.get();
    }

    /// `f` of the store as a memo: read again on every change, but what
    /// reads the memo hears only of a different answer — a region keyed
    /// on the tab is made again when the tab changes, not on every tick.
    pub fn memo<T: PartialEq + 'static>(self, f: fn(&ZStatsAppState) -> T) -> Memo<T> {
        computed(move || {
            self.track();
            store::read(f)
        })
    }

    /// [`Live::memo`] for something outside the store that changes when it
    /// does: a preference, the keep-awake hold.
    pub fn watch<T: PartialEq + 'static>(self, f: impl Fn() -> T + 'static) -> Memo<T> {
        computed(move || {
            self.track();
            f()
        })
    }

    /// [`Live::read`] for a text.
    pub fn text(self, f: fn(&ZStatsAppState) -> String) -> impl Fn() -> String + Clone + 'static {
        self.read(f)
    }
}

/// `views::GUTTER`, `widgets::CARD_PAD` and `views::CARD_GAP`.
pub const GUTTER: f32 = 12.;
pub const CARD_PAD: f32 = 12.;
pub const CARD_GAP: f32 = 10.;

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
