//! The gpui front end's own interface state: the handles gpui's elements
//! keep between frames — each tab's scroll offset, the rows regions'
//! offsets, the two search inputs, the full listings' virtual lists — and
//! the three windows.
//!
//! They lived in the store (`state/`) until phase 1c of
//! docs/nsh-migration.md made the store shared between front ends: these
//! are gpui's types, the NSH front end keeps its own, and a store that
//! moves between threads cannot hold them. What the store keeps is what
//! either front end needs to know — whether a search is open and what it
//! says, which rows a listing shows ([`FullScanData::rows`]).
//!
//! A thread-local rather than a gpui `Global`, because the views that read
//! these are functions of the store alone, with no `App` in hand; they all
//! run on the main thread, as gpui does. Nothing here outlives the run
//! loop that owns it.
//!
//! The reason each handle has to be held at all, rather than built per
//! frame, is the one the store's fields gave: gpui keys element state by
//! id and drops whatever it did not paint, and only one tab's body is ever
//! painted. A per-tab id alone resets every tab to the top on each switch;
//! a full listing built per frame would snap to the top every couple of
//! seconds, the panel repainting per tick.

use crate::i18n;
use crate::state::{
    Apply, Effects, FullAppScanData, FullScanData, Reply, Tab, ZStatsAppState, ZStatsGlobalStore,
};
use crate::tray;
use gpui::{
    AnyWindowHandle, App, AppContext, Context, Entity, Focusable, ListAlignment, ListState,
    ScrollHandle, Window, px,
};
use gpui_kit::component::input::{InputEvent, InputState};
use std::array;
use std::cell::RefCell;
use std::thread;

struct Ui {
    /// One scroll offset per tab, indexed by [`Tab::index`].
    scroll: [ScrollHandle; Tab::ALL.len()],
    /// The rows region *inside* the Processes top-N card — the rows
    /// scroll under a pinned header, so they need a handle of their own.
    proc_rows: ScrollHandle,
    /// Same, for History's rows.
    history_rows: ScrollHandle,
    /// Same, for the Applications card.
    app_rows: ScrollHandle,
    /// The process name filter, created on first open — an
    /// [`InputState`] needs a `Window`, which only the toggle click has.
    /// Kept once created, so reopening it does not rebuild cursor and
    /// undo state.
    proc_filter: Option<Entity<InputState>>,
    /// The listening card's search, created and kept the same way.
    listen_filter: Option<Entity<InputState>>,
    /// The Processes full listing's virtual list: measured row heights
    /// and the scroll offset, for the row set [`FullScanData::rows`]
    /// names. A new row set is a new list, back at the top — `gpui::list`
    /// is told its row count up front.
    process_list: Option<(u64, ListState)>,
    /// Same, for the Apps full listing.
    app_list: Option<(u64, ListState)>,
    /// The popover panel. Hidden rather than destroyed on macOS, so this
    /// outlives hide → show. `cx.windows().first()` is SlotMap insertion
    /// order, not "the panel", and would `orderOut` the wrong window once
    /// another exists.
    panel_window: Option<AnyWindowHandle>,
    /// The settings window, if one was ever opened. Kept so a second
    /// click focuses the existing window; a handle whose window the user
    /// closed fails its update and a fresh window is built instead.
    settings_window: Option<AnyWindowHandle>,
    /// The disk-space window, same reuse-or-rebuild contract.
    storage_window: Option<AnyWindowHandle>,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui {
        scroll: array::from_fn(|_| ScrollHandle::new()),
        proc_rows: ScrollHandle::new(),
        history_rows: ScrollHandle::new(),
        app_rows: ScrollHandle::new(),
        proc_filter: None,
        listen_filter: None,
        process_list: None,
        app_list: None,
        panel_window: None,
        settings_window: None,
        storage_window: None,
    });
}

/// This tab's scroll offset, held across frames so switching away and
/// back returns to where the list was left.
pub fn scroll_handle(tab: Tab) -> ScrollHandle {
    UI.with_borrow(|ui| ui.scroll[tab.index()].clone())
}

pub fn proc_rows_scroll() -> ScrollHandle {
    UI.with_borrow(|ui| ui.proc_rows.clone())
}

pub fn history_rows_scroll() -> ScrollHandle {
    UI.with_borrow(|ui| ui.history_rows.clone())
}

pub fn app_rows_scroll() -> ScrollHandle {
    UI.with_borrow(|ui| ui.app_rows.clone())
}

/// The virtual list for the Processes full listing on screen.
pub fn process_list(data: &FullScanData) -> ListState {
    UI.with_borrow_mut(|ui| list_for(&mut ui.process_list, data.rows, data.visible.len()))
}

/// The virtual list for the Apps full listing on screen.
pub fn app_list(data: &FullAppScanData) -> ListState {
    UI.with_borrow_mut(|ui| list_for(&mut ui.app_list, data.rows, data.visible.len()))
}

/// The list kept for `rows`, or a new one at the top when the row set
/// changed. Overdraw of roughly one panel: rows near the viewport are
/// pre-measured so scrolling does not pop as estimates get corrected.
fn list_for(slot: &mut Option<(u64, ListState)>, rows: u64, count: usize) -> ListState {
    match slot {
        Some((kept, list)) if *kept == rows => list.clone(),
        _ => {
            let list = ListState::new(count, ListAlignment::Top, px(400.));
            *slot = Some((rows, list.clone()));
            list
        }
    }
}

pub fn proc_filter_input() -> Option<Entity<InputState>> {
    UI.with_borrow(|ui| ui.proc_filter.clone())
}

pub fn listen_filter_input() -> Option<Entity<InputState>> {
    UI.with_borrow(|ui| ui.listen_filter.clone())
}

pub fn panel_window() -> Option<AnyWindowHandle> {
    UI.with_borrow(|ui| ui.panel_window)
}

pub fn set_panel_window(handle: AnyWindowHandle) {
    UI.with_borrow_mut(|ui| ui.panel_window = Some(handle));
}

pub fn settings_window() -> Option<AnyWindowHandle> {
    UI.with_borrow(|ui| ui.settings_window)
}

pub fn set_settings_window(handle: AnyWindowHandle) {
    UI.with_borrow_mut(|ui| ui.settings_window = Some(handle));
}

pub fn storage_window() -> Option<AnyWindowHandle> {
    UI.with_borrow(|ui| ui.storage_window)
}

pub fn set_storage_window(handle: AnyWindowHandle) {
    UI.with_borrow_mut(|ui| ui.storage_window = Some(handle));
}

/// Whether the disk-space window is open now: its handle names a window
/// gpui still has.
pub fn storage_window_open(cx: &App) -> bool {
    storage_window().is_some_and(|handle| {
        cx.windows()
            .iter()
            .any(|w| w.window_id() == handle.window_id())
    })
}

/// Show or hide the process name filter, creating the input on first use.
pub fn toggle_proc_filter(window: &mut Window, cx: &mut App) {
    let store = cx.global::<ZStatsGlobalStore>().clone();
    if store.read(cx).proc_filter_open() {
        // `set_value` deliberately emits no Change event, so the store is
        // told by hand.
        if let Some(input) = proc_filter_input() {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        store.update(cx, |state, cx| state.close_proc_filter(cx));
        return;
    }
    let input = proc_filter_input().unwrap_or_else(|| {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n::tr("processes.filter_placeholder"))
                // Esc clears. Goes through `replace_text`, which — unlike
                // `set_value` — emits Change, so the store and the
                // full-scan rows follow without extra wiring.
                .clean_on_escape()
        });
        let follow = store.clone();
        cx.subscribe(&input, move |input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().to_string();
                follow.update(cx, |state, cx| state.set_proc_filter_text(&text, cx));
            }
        })
        .detach();
        UI.with_borrow_mut(|ui| ui.proc_filter = Some(input.clone()));
        input
    });
    store.update(cx, |state, cx| state.open_proc_filter(cx));
    // Focus so typing can start without a second click.
    input.read(cx).focus_handle(cx).focus(window, cx);
}

/// Show or hide the listening card's search, creating the input on first
/// use — the process filter's shape.
pub fn toggle_listen_filter(window: &mut Window, cx: &mut App) {
    let store = cx.global::<ZStatsGlobalStore>().clone();
    if store.read(cx).listen_filter_open() {
        if let Some(input) = listen_filter_input() {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        store.update(cx, |state, cx| state.close_listen_filter(cx));
        return;
    }
    let input = listen_filter_input().unwrap_or_else(|| {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n::tr("listen.filter_placeholder"))
                // Esc clears, through the path that emits Change.
                .clean_on_escape()
        });
        let follow = store.clone();
        cx.subscribe(&input, move |input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().to_string();
                follow.update(cx, |state, cx| state.set_listen_filter_text(&text, cx));
            }
        })
        .detach();
        UI.with_borrow_mut(|ui| ui.listen_filter = Some(input.clone()));
        input
    });
    store.update(cx, |state, cx| state.open_listen_filter(cx));
    input.read(cx).focus_handle(cx).focus(window, cx);
}

/// The store's `reset_transient_views`, plus the inputs' own text: a
/// search the store closes is emptied here, where the input lives.
pub fn reset_transient_views(window: &mut Window, cx: &mut App) {
    let store = cx.global::<ZStatsGlobalStore>().clone();
    let (proc_open, listen_open) = {
        let state = store.read(cx);
        (state.proc_filter_open(), state.listen_filter_open())
    };
    for (open, input) in [
        (proc_open, proc_filter_input()),
        (listen_open, listen_filter_input()),
    ] {
        if open && let Some(input) = input {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }
    store.update(cx, |state, cx| state.reset_transient_views(cx));
}

/// The store's side effects, on gpui: a change repaints through the
/// entity, a background job runs on a thread of its own and its changes
/// come back to the main thread through a task, in the order sent. The
/// store's call sites pass `cx` as they always did; it is this.
impl Effects for Context<'_, ZStatsAppState> {
    fn changed(&mut self) {
        self.notify();
    }

    fn background(&mut self, job: Box<dyn FnOnce(Reply) + Send>) {
        let (tx, rx) = smol::channel::unbounded::<Apply>();
        let reply = Reply::new(move |change| tx.send_blocking(change).is_ok());
        if let Err(e) = thread::Builder::new()
            .name("store-job".into())
            .spawn(move || job(reply))
        {
            tracing::error!("could not start a background job: {e}");
            return;
        }
        self.spawn(async move |this, cx| {
            while let Ok(change) = rx.recv().await {
                if this.update(cx, |state, cx| change(state, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn sync_tray(&mut self, state: &ZStatsAppState) {
        tray::sync(state);
    }

    fn fit_panel(&mut self) {
        self.defer(crate::sync_panel_notice);
    }

    fn storage_window_open(&self) -> bool {
        storage_window_open(self)
    }
}
