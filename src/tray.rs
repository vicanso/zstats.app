//! System tray icon and menu.
//!
//! Two backends, one shape: an `NSStatusItem` on macOS, a
//! StatusNotifierItem over D-Bus on Linux (`tray-icon`'s `ksni` feature —
//! its default `libappindicator` backend is the GTK main loop this module
//! used to be excluded over, and that one really cannot coexist with
//! gpui's event loop). What the host hands back differs in three ways,
//! and all three are load-bearing below:
//!
//! - **No icon rect.** `TrayIcon::rect` is hard-coded `None` on SNI and
//!   click events carry `Rect::default()`, so nothing can hang the panel
//!   off the item — the compositor anchors it instead (`panel_kind` in
//!   gpui_app.rs). Every anchor on this path is `None`, which is the same
//!   fallback macOS already takes before AppKit has laid the item out.
//! - **No right-click event.** SNI maps `secondary_activate` to
//!   `MouseButton::Middle` and never sends a right click: the context
//!   menu is opened by the host out of what `set_menu` gave it, and does
//!   not come back through us.
//! - **No template images.** macOS recolours an alpha-only bitmap to the
//!   menu bar's own ink and follows a wallpaper change without being
//!   told; SNI paints exactly the pixels it is given. The glyph therefore
//!   has to choose a colour, and nothing in the protocol says what is
//!   behind it — see [`ink`].
//!
//! Both front ends share it since phase 1d of docs/nsh-migration.md. The
//! tray is kept on the thread that built it, the main thread, rather than
//! in a gpui `Global`; [`sync`] reads the store alone; and what a click or
//! the menu asks for goes, as a [`TrayAction`], to the handler [`start`]
//! was given — what showing the panel means is the front end's.

use crate::APP_NAME;
use crate::assets::CustomIconName;
use crate::format;
use crate::glyph;
use crate::i18n;
/// Only the ink reads it, and only where there is no template image.
#[cfg(not(target_os = "macos"))]
use crate::palette;
use crate::prefs::{self, TrayPref};
use crate::state::{TrayAnchor, ZStatsAppState};
use rust_i18n::t;
use std::cell::{Cell, OnceCell, RefCell};
use std::sync::Arc;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const MENU_ID_SHOW: &str = "show";
const MENU_ID_QUIT: &str = "quit";
const MENU_ID_STORAGE: &str = "storage";

/// Which figure the menu bar item wears. The glyph and the percent beside
/// it always agree — a bare `91%` means nothing until the icon says what
/// it is a percent *of*, which is the whole reason the icon moves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayFace {
    Cpu,
    Memory,
    Disk,
}

/// One tray item and what it currently wears. Dropping it removes the item
/// from the bar — which is how Both mode's second item leaves when the
/// preference turns it off.
struct Item {
    icon: TrayIcon,
    /// `"primary"` / `"second"`, for the diary line a face change writes.
    label: &'static str,
    /// Face actually on the item, so an unchanged face is not re-applied —
    /// `set_icon` rebuilds an `NSImage` and re-lays out the menu bar on
    /// macOS, and pushes a fresh pixmap over D-Bus on SNI.
    face: Cell<Option<TrayFace>>,
    /// Last title actually pushed to the host, same reason.
    title: RefCell<String>,
    /// Last applied unread-alert corner spec. Independent of `face` so
    /// a live report restamps the same die with a dot, without a face
    /// change.
    hot: Cell<bool>,
}

/// The faces, rasterised once at build so a swap hands the platform a
/// cached bitmap instead of parsing an SVG on the collector's hand-off.
/// `None` where the SVG failed to render — that face is then never
/// applied, and an item keeps whatever it had.
///
/// `*_hot` is the same die with a spec in the corner, in the glyph's own
/// ink rather than a colour of its own — see [`tray_icon_hot`]. Every
/// Auto face carries one: Auto can be wearing the memory stick or the
/// disk when an alert lands, and a spec that only exists on CPU would
/// vanish the moment the face moved.
struct Faces {
    cpu: Option<Icon>,
    cpu_hot: Option<Icon>,
    memory: Option<Icon>,
    memory_hot: Option<Icon>,
    disk: Option<Icon>,
    disk_hot: Option<Icon>,
}

/// Rasterise all six, in whatever ink is current. Called at startup and
/// again whenever [`TrayHandle::ensure_ink`] finds the theme has moved.
fn build_faces() -> Faces {
    Faces {
        cpu: tray_icon(CustomIconName::Cpu),
        cpu_hot: tray_icon_hot(CustomIconName::Cpu),
        memory: tray_icon(CustomIconName::MemoryStick),
        memory_hot: tray_icon_hot(CustomIconName::MemoryStick),
        disk: tray_icon(CustomIconName::HardDrive),
        disk_hot: tray_icon_hot(CustomIconName::HardDrive),
    }
}

impl Faces {
    fn icon(&self, face: TrayFace, hot: bool) -> Option<&Icon> {
        match (face, hot) {
            (TrayFace::Cpu, true) => self.cpu_hot.as_ref().or(self.cpu.as_ref()),
            (TrayFace::Cpu, false) => self.cpu.as_ref(),
            (TrayFace::Memory, true) => self.memory_hot.as_ref().or(self.memory.as_ref()),
            (TrayFace::Memory, false) => self.memory.as_ref(),
            (TrayFace::Disk, true) => self.disk_hot.as_ref().or(self.disk.as_ref()),
            (TrayFace::Disk, false) => self.disk.as_ref(),
        }
    }
}

/// Keeps the tray alive for the process lifetime.
struct TrayHandle {
    /// The item that always exists.
    primary: Item,
    /// Both mode's second item: built when the preference turns Both on,
    /// dropped — and so removed from the menu bar — when it turns off.
    /// AppKit inserts a new status item to the *left* of the ones already
    /// there, so this one sits left of `primary` and wears CPU, which
    /// reads left-to-right the way the picker names the mode. SNI makes
    /// no such promise — ordering within a host's tray is the host's, and
    /// the pair may read either way round there.
    second: RefCell<Option<Item>>,
    /// Rebuilt in place when the ink changes, which is why it is a cell
    /// rather than a plain field.
    faces: RefCell<Faces>,
    /// The ink `faces` was rasterised in. macOS never moves it (a
    /// template inks itself), so this only ever changes on the SNI path.
    ink: Cell<[u8; 3]>,
}

thread_local! {
    /// The tray, on the thread that built it — the main thread, where
    /// macOS makes and changes an `NSStatusItem`. Set once: by [`start`],
    /// or on Linux by the first build after a tray host appeared.
    static TRAY: OnceCell<TrayHandle> = const { OnceCell::new() };
}

/// `f` on the tray; `None` when it was never built, or when called from a
/// thread other than the one that keeps it.
fn with_tray<R>(f: impl FnOnce(&TrayHandle) -> R) -> Option<R> {
    TRAY.with(|tray| tray.get().map(f))
}

impl TrayHandle {
    /// Grow or shed the second item. A no-op when it already matches.
    fn set_both(&self, on: bool) {
        let mut second = self.second.borrow_mut();
        match (on, second.is_some()) {
            (true, false) => *second = build_item("second", TrayFace::Cpu, &self.faces.borrow()),
            (false, true) => *second = None,
            _ => {}
        }
    }

    /// Re-rasterise when the theme moved under us.
    ///
    /// The bitmaps are cached precisely so a face swap costs nothing, so
    /// the SVG parse belongs here — once per theme change — rather than
    /// per swap. Both items then have to forget what they are wearing, or
    /// [`Item::set_face`] would skip the re-apply as a no-op and the old
    /// ink would stay on the bar until the face happened to move.
    ///
    /// Free on macOS: [`ink`] is a constant there, so this never fires.
    fn ensure_ink(&self) {
        let ink = ink();
        if self.ink.get() == ink {
            return;
        }
        *self.faces.borrow_mut() = build_faces();
        self.ink.set(ink);
        self.primary.forget_face();
        if let Some(second) = self.second.borrow().as_ref() {
            second.forget_face();
        }
        // Same reason a face change is logged: the user sees this, and an
        // icon that went missing needs a line saying what it turned into.
        tracing::info!(?ink, "tray ink re-rasterised for the theme");
    }
}

impl Item {
    /// Put a face and, once there is a sample, its figure on the item.
    fn wear(&self, face: TrayFace, figure: Option<Figure>, hot: bool, faces: &Faces) {
        self.set_face(face, hot, faces);
        if let Some(figure) = figure {
            self.set_title(figure);
        }
    }

    fn set_face(&self, face: TrayFace, hot: bool, faces: &Faces) {
        if self.face.get() == Some(face) && self.hot.get() == hot {
            return;
        }
        let Some(icon) = faces.icon(face, hot) else {
            return;
        };
        // Not `set_icon` on macOS: the crate hard-codes template *off* in
        // that path, and a template bitmap is alpha-only — swapped in
        // untemplated it is a black block on a dark menu bar. This is the
        // call that carries the flag, and the unread-alert spec is extra
        // alpha on the same template, so it takes the menu bar's ink
        // (white on dark) along with the die.
        //
        // Everywhere else `set_icon` is the whole story: there is no
        // template to ask for, the pixels are already inked (see `ink`),
        // and the spec is inked with them.
        #[cfg(target_os = "macos")]
        let applied = self
            .icon
            .set_icon_with_as_template(Some(icon.clone()), true);
        #[cfg(not(target_os = "macos"))]
        let applied = self.icon.set_icon(Some(icon.clone()));
        if let Err(e) = applied {
            tracing::warn!(
                item = self.label,
                ?face,
                hot,
                "could not swap the tray icon: {e}"
            );
            return;
        }
        if self.face.get() != Some(face) {
            // A face change is a state change the user sees — the diary
            // should say when and to what, like every alert verdict does.
            tracing::info!(item = self.label, ?face, "tray face changed");
        }
        self.face.set(Some(face));
        self.hot.set(hot);
    }

    /// Drop the "already wearing this" memo so the next [`Self::set_face`]
    /// really re-applies. Only [`TrayHandle::ensure_ink`] needs it: the
    /// face has not changed, the bitmap behind it has.
    fn forget_face(&self) {
        self.face.set(None);
    }

    fn set_title(&self, figure: Figure) {
        let mut last = self.title.borrow_mut();
        if *last == figure.title {
            // Setting a title re-lays out the menu bar, so skip the no-ops.
            return;
        }
        self.icon.set_title(Some(&figure.title));
        // The tooltip is what makes a bare `8.1G` readable — it spells
        // out which quantity the glyph stands for. Rides the title's
        // change gate: same cadence, and it cannot change alone.
        let _ = self.icon.set_tooltip(Some(&figure.tip));
        *last = figure.title;
    }
}

/// What an item shows for a face: the title beside the glyph, and the
/// tooltip that says what it is.
struct Figure {
    title: String,
    tip: String,
}

impl Figure {
    /// Whole percent: the menu bar is cramped, and a decimal would make
    /// the title twitch on every sample even when load is flat.
    fn cpu(percent: f32) -> Self {
        let pct = format!("{percent:.0}%");
        Figure {
            tip: format!("{APP_NAME} · {}", t!("tray.cpu_tip", pct = pct)),
            title: pct,
        }
    }

    /// Available memory, not used percent. macOS runs high-used by
    /// design — the cache fills whatever is free — so used% sits in the
    /// sixties on a healthy machine and says nothing when the face
    /// turns. Available is the figure that actually falls as the
    /// machine tightens, and it is the one the Overview hero pairs with
    /// the total (`format::gb`, same rounding).
    fn memory(available: u64, total: u64) -> Self {
        Figure {
            title: format::gb_short(available),
            tip: format!(
                "{APP_NAME} · {}",
                t!(
                    "tray.memory_tip",
                    avail = format::gb(available),
                    total = format::gb(total)
                )
            ),
        }
    }

    /// Free space on the volume that earned the face, not used percent.
    /// Used% is what the alert fired on, but the menu bar's job is the
    /// same as memory's: how much is *left*. Same `gb_short` rounding.
    fn disk(available: u64, total: u64, name: &str) -> Self {
        Figure {
            title: format::gb_short(available),
            tip: format!(
                "{APP_NAME} · {}",
                t!(
                    "tray.disk_tip",
                    avail = format::gb(available),
                    total = format::gb(total),
                    name = name
                )
            ),
        }
    }
}

/// One status item wearing `face`, with the shared menu and tooltip.
/// `None` — logged — when AppKit refuses; the caller then carries on
/// with whatever items it has.
///
/// Must be called on the main thread (macOS creates an `NSStatusItem`):
/// true for [`start`] and for [`sync`], which run on the thread that
/// keeps the tray.
fn build_item(label: &'static str, face: TrayFace, faces: &Faces) -> Option<Item> {
    let mut builder = TrayIconBuilder::new()
        .with_menu(Box::new(build_menu()))
        .with_tooltip(APP_NAME)
        // Left click shows the window directly; the menu moves to right click.
        // The `TrayIconEvent::Click` is emitted either way — this only stops
        // the menu from popping up over it.
        .with_menu_on_left_click(false);
    let icon = faces.icon(face, false).cloned();
    if let Some(icon) = icon.clone() {
        builder = builder.with_icon(icon);
        #[cfg(target_os = "macos")]
        {
            builder = builder.with_icon_as_template(true);
        }
    }
    match builder.build() {
        Ok(tray) => Some(Item {
            icon: tray,
            label,
            face: Cell::new(icon.map(|_| face)),
            title: RefCell::new(String::new()),
            hot: Cell::new(false),
        }),
        Err(e) => {
            // Not fatal: the window still works, the app just has no tray.
            tracing::error!(item = label, "failed to create tray icon: {e}");
            None
        }
    }
}

/// The face for a preference and the store's answers to "what needs
/// attention right now".
///
/// Auto is the only mode that moves. CPU is the resting face — a CPU
/// episode changes nothing, the figure is already there. Memory and
/// disk both have to be *earned* from a live episode this session
/// (`memory_needs_attention` / `disk_needs_attention`). When both are
/// in trouble memory wins: macOS escalates memory (compressor, swap,
/// jetsam) while a full disk stays full. Pinned modes ignore both
/// signals. Auto switches memory back after five quiet minutes, disk
/// as soon as this tick is under the event's own used-% bar, or
/// immediately when the card is dismissed.
///
/// This is the *primary* item's face. In Both that is memory: the
/// second item lands to its left (see `TrayHandle::second`) and wears
/// CPU, so the pair reads CPU · memory. Disk is Auto-only — it does
/// not grow a third status item.
pub fn face_for(
    pref: TrayPref,
    memory_needs_attention: bool,
    disk_needs_attention: bool,
) -> TrayFace {
    match pref {
        TrayPref::Cpu => TrayFace::Cpu,
        TrayPref::Memory | TrayPref::Both => TrayFace::Memory,
        TrayPref::Auto if memory_needs_attention => TrayFace::Memory,
        TrayPref::Auto if disk_needs_attention => TrayFace::Disk,
        TrayPref::Auto => TrayFace::Cpu,
    }
}

/// Bring the menu bar in line with the store: how many items the
/// preference wants, the face from the preference and the store's memory
/// signal, the figure from the latest sample — `cpu.usage_percent` or
/// `memory.available_bytes`, both zstats' own fields — and the
/// unread-alert spec (`tray_alert_unseen`). Called after every ingest,
/// when the picker changes, and when Alerts is shown so the spec goes
/// out immediately rather than on the next tick. Before the first
/// sample there is a face but no figure. A no-op if the tray failed to
/// build.
pub fn sync(state: &ZStatsAppState) {
    with_tray(|handle| wear_state(handle, state));
}

fn wear_state(handle: &TrayHandle, state: &ZStatsAppState) {
    // Before anything is worn: a theme switch has to reach the bar on
    // this pass, not on the one after it.
    handle.ensure_ink();
    let pref = prefs::tray();
    handle.set_both(pref == TrayPref::Both);
    let figure = |face| {
        state.latest().map(|tick| match face {
            TrayFace::Cpu => Figure::cpu(tick.snapshot.cpu.usage_percent),
            TrayFace::Memory => {
                let mem = &tick.snapshot.memory;
                Figure::memory(mem.available_bytes, mem.total_bytes)
            }
            TrayFace::Disk => match state.disk_face_volume() {
                Some((avail, total, name)) => Figure::disk(avail, total, name),
                None => Figure {
                    title: format::PLACEHOLDER.to_string(),
                    tip: APP_NAME.to_string(),
                },
            },
        })
    };
    let face = face_for(
        pref,
        state.memory_needs_attention(),
        state.disk_needs_attention(),
    );
    // One spec, on the item that always exists. Both's second item is
    // the same news twice — two adjacent dots would look like two
    // alerts.
    let hot = state.tray_alert_unseen();
    let faces = handle.faces.borrow();
    handle.primary.wear(face, figure(face), hot, &faces);
    if let Some(second) = handle.second.borrow().as_ref() {
        second.wear(TrayFace::Cpu, figure(TrayFace::Cpu), false, &faces);
    }
}

/// What the tray asks of the front end, through the handler [`start`] was
/// given.
pub enum TrayAction {
    /// Left-clicking the icon; carries the icon's screen rect to hang the
    /// window off, or `None` where the platform reports none. Toggles.
    Toggle(Option<TrayAnchor>),
    /// The menu item, which has no position and always shows.
    Show,
    /// The menu's way into the disk-space window.
    Storage,
    Quit,
    /// A tray host appeared on the session bus after the first build
    /// failed: call [`host_appeared`] on the thread that keeps the tray.
    #[cfg(target_os = "linux")]
    HostAppeared,
}

/// The handler [`start`] was given. Called on whichever thread the
/// platform reports an event on.
type Deliver = Arc<dyn Fn(TrayAction) + Send + Sync>;

/// A menu bar face, rasterised from the bundled SVG (`glyph.rs`).
///
/// A CPU die and a memory stick rather than a trend arrow: an arrow makes
/// a claim about the data ("the numbers are going up"), while the subject
/// itself does not. The face that is showing *is* a claim — but one
/// zstats or the kernel already made (`face_for`), never a reading of
/// the numbers here.
fn tray_icon(face: CustomIconName) -> Option<Icon> {
    Icon::from_rgba(
        glyph::rasterise(face, ink())?,
        glyph::ICON_SIZE,
        glyph::ICON_SIZE,
    )
    .ok()
}

/// The colour the glyph's pixels carry.
///
/// **macOS never asks.** The status item is a *template* image there, so
/// only alpha survives: the menu bar inks the glyph itself, white on dark
/// and black on light, and it follows a wallpaper change without being
/// told. The value returned here is discarded.
///
/// **SNI has no such concept.** `set_icon` paints exactly the pixels it
/// is given, and no part of the protocol reports what colour the bar
/// behind the icon is. So the ink follows the panel's own resolved theme:
/// a dark theme means a dark desktop means light ink. That is a
/// correlation, not a reading, and it is wrong in exactly one case — a
/// light panel theme in front of a dark bar, where the glyph goes
/// invisible. The way out is the control that caused it (Interface →
/// theme, which moves both), which is why this is deliberately *not* a
/// preference of its own: a second switch for the same decision is one
/// nobody would think to go looking for when an icon disappears.
fn ink() -> [u8; 3] {
    #[cfg(target_os = "macos")]
    {
        // Arbitrary — the template keeps alpha and throws these away.
        [0, 0, 0]
    }
    #[cfg(not(target_os = "macos"))]
    {
        ink_for(palette::is_dark())
    }
}

/// The ink for a resolved theme, split from [`ink`] so the contrast rule
/// can be tested without touching the theme global — a test that moved it
/// would pull the ground out from under every other test in the process.
#[cfg(not(target_os = "macos"))]
fn ink_for(dark: bool) -> [u8; 3] {
    // Not pure white or pure black: a bar's own label rarely is either,
    // and a glyph brighter than the text beside it stops reading as part
    // of the same row.
    if dark { [0xe6; 3] } else { [0x1a; 3] }
}

/// Same die, plus the alert spec in the top-left (`glyph::stamp_hot_dot`),
/// in [`ink`]'s colour like the strokes.
fn tray_icon_hot(face: CustomIconName) -> Option<Icon> {
    let mut rgba = glyph::rasterise(face, ink())?;
    glyph::stamp_hot_dot(&mut rgba, ink());
    Icon::from_rgba(rgba, glyph::ICON_SIZE, glyph::ICON_SIZE).ok()
}

/// Where the primary status item sits right now, in the physical pixels
/// [`TrayAnchor`] carries — the same rect a click event hands over.
///
/// A click brings its own rect, so until the tray menu's "Show Window"
/// (and the banner click behind it) wanted one, nothing ever asked the
/// item where it was; those paths reopened the panel wherever it had
/// last been left, which reads as the panel drifting away from its
/// icon. In Both mode this is the right-hand item, the one that always
/// exists.
///
/// `None` on the SNI path — the protocol carries no icon rect at all and
/// `TrayIcon::rect` is hard-coded `None` there — and equally without a
/// tray, or when AppKit has not laid the item out yet. Every caller then
/// falls back to the last position, which is what they all did before.
pub fn anchor() -> Option<TrayAnchor> {
    let rect = with_tray(|handle| handle.primary.icon.rect())??;
    Some(TrayAnchor {
        x: rect.position.x,
        y: rect.position.y,
        width: f64::from(rect.size.width),
        height: f64::from(rect.size.height),
    })
}

/// Swap in a menu rebuilt in the active locale. The menu snapshots its item
/// titles when built, so a language switch has to hand the tray a fresh one —
/// the icon, tooltip and event threads stay put. A no-op without a tray.
pub fn rebuild_menu() {
    with_tray(|handle| {
        handle.primary.icon.set_menu(Some(Box::new(build_menu())));
        if let Some(second) = handle.second.borrow().as_ref() {
            second.icon.set_menu(Some(Box::new(build_menu())));
        }
    });
}

fn build_menu() -> Menu {
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::with_id(
        MENU_ID_SHOW,
        i18n::tr("common.show_window"),
        true,
        None,
    ));
    // The disk-space window was reachable only from a chip on the
    // Hardware tab and a card on Alerts. Someone whose disk is full
    // looks in the menu first.
    let _ = menu.append(&MenuItem::with_id(
        MENU_ID_STORAGE,
        i18n::tr("disk.storage_title"),
        true,
        None,
    ));
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id(
        MENU_ID_QUIT,
        i18n::tr("common.quit"),
        true,
        None,
    ));
    menu
}

/// Build the tray and hand what it asks for to `deliver`.
///
/// Must be called on the main thread (macOS creates an `NSStatusItem`),
/// and [`sync`], [`anchor`] and [`rebuild_menu`] then run on that same
/// thread, where the tray is kept. `deliver` is called on whichever
/// thread the platform reports an event on — the main thread on macOS,
/// inside the status item's own action; ksni's own on Linux — and takes
/// the action to wherever the front end handles it.
///
/// Without a tray host — no SNI host on the session bus — `build_item`
/// logs and returns `None`. That is not an error path: the panel still
/// works, it just has no icon, and on Linux the way back to it is the
/// keybinding (`docs/omarchy-port.md` 阶段 2). On Linux it is also often
/// not the final answer: a login launch can beat the bar that hosts the
/// tray, so the tray is built again when a host appears ([`sni`]).
pub fn start(deliver: impl Fn(TrayAction) + Send + Sync + 'static) {
    let deliver: Deliver = Arc::new(deliver);
    route_events(deliver.clone());
    let built = try_build();
    // Elsewhere a failed build is the answer: an `NSStatusItem` has no
    // host to wait for.
    #[cfg(target_os = "linux")]
    if !built {
        sni::build_when_a_host_appears(deliver);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (built, deliver);
}

/// [`TrayAction::HostAppeared`]: build the tray now, unless an earlier
/// appearance already did.
#[cfg(target_os = "linux")]
pub fn host_appeared() {
    if with_tray(|_| ()).is_some() {
        return;
    }
    if try_build() {
        tracing::info!("tray built after a tray host appeared");
    } else {
        tracing::warn!("a tray host is on the bus but the tray still failed");
    }
}

/// Build the tray; `false` when the primary item could not be created,
/// having changed nothing — so it can simply be called again.
fn try_build() -> bool {
    // Rasterised up front so a later swap is cheap.
    let faces = build_faces();
    // The store is empty here, so the face is the preference's resting
    // one (no live episode): a pinned mode launches already wearing
    // its face instead of flipping on the first sample.
    let pref = prefs::tray();
    let Some(primary) = build_item("primary", face_for(pref, false, false), &faces) else {
        return false;
    };
    // Both's second item is built here too rather than left to the first
    // `sync`: a launch in that mode should not show one item and grow
    // another a few seconds later.
    let second = (pref == TrayPref::Both)
        .then(|| build_item("second", TrayFace::Cpu, &faces))
        .flatten();
    let handle = TrayHandle {
        primary,
        second: RefCell::new(second),
        faces: RefCell::new(faces),
        ink: Cell::new(ink()),
    };
    TRAY.with(|tray| {
        let _ = tray.set(handle);
    });
    #[cfg(target_os = "linux")]
    sni::BUILT.store(true, std::sync::atomic::Ordering::Release);
    true
}

/// tray-icon's two handlers, set once whether or not an item gets built —
/// without one they never fire — and before the first build: each is a
/// set-once cell that the first event fixes for good, to the receiver if
/// nothing is set by then. A handler rather than the crate's receivers,
/// which only block: the gpui front end used to park a thread on each to
/// forward its events, and the NSH one has no need to.
fn route_events(deliver: Deliver) {
    let menu = deliver.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = match event.id().0.as_str() {
            MENU_ID_SHOW => TrayAction::Show,
            MENU_ID_STORAGE => TrayAction::Storage,
            MENU_ID_QUIT => TrayAction::Quit,
            _ => return,
        };
        menu(action);
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        // macOS emits Click on both mouseDown and mouseUp — keying off Up
        // means a press-and-hold doesn't fire until the button is
        // released. SNI's `activate` is a single event that already
        // arrives as Up, so the same filter passes it through.
        //
        // Left only, and on SNI that is the only button there is: the
        // host opens the context menu itself, and `secondary_activate`
        // (a *middle* click, not a right one) is not a control this
        // app offers anywhere else either.
        let rect = match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } => rect,
            _ => return,
        };
        // The icon's screen rect, which is what the window gets anchored
        // to. SNI reports none (`Rect::default()` — every field zero),
        // and a fabricated origin would put the panel in the corner of
        // the screen rather than leave the compositor to place it, so
        // the honest answer there is no answer.
        #[cfg(not(target_os = "linux"))]
        let anchor = Some(TrayAnchor {
            x: rect.position.x,
            y: rect.position.y,
            width: f64::from(rect.size.width),
            height: f64::from(rect.size.height),
        });
        #[cfg(target_os = "linux")]
        let anchor = {
            let _ = rect;
            None
        };
        // Every click the host delivers, before anything is done with
        // it. A tray host is free to turn one activation into two
        // events, and from the panel's side that is indistinguishable
        // from an auto-hide: it opens and is gone. Two of these lines
        // milliseconds apart is the whole diagnosis.
        tracing::info!("tray click received");
        deliver(TrayAction::Toggle(anchor));
    }));
}

/// Building the tray once a StatusNotifier host is on the session bus.
///
/// A login launch races the bar. On Omarchy (2026-09-28) the XDG autostart
/// started zstats at 08:30:16 and the tray failed at once — `failed to
/// register to the StatusNotifierWatcher: ServiceUnknown: The name is not
/// activatable` — because quickshell, which owns
/// `org.kde.StatusNotifierWatcher`, was not up yet. Nothing tried again,
/// so the session ran without an icon; a manual launch after the desktop
/// was up had never met the race, which is why it only appeared once
/// launch-at-login existed.
///
/// Only the *first* registration is the problem. Once spawned, ksni
/// follows the watcher's name itself and re-registers when a new owner
/// appears (its `service.rs`), so a bar restart is survived — the same
/// session's quickshell restarted at 08:50. But tray-icon spawns without
/// ksni's `assume_sni_available`, so a missing watcher at spawn time is a
/// hard error, and this is where it is caught.
///
/// Event-driven, like `active.rs`: a thread parked on the bus's
/// `NameOwnerChanged` for the watcher's name costs nothing while it waits,
/// and a desktop that never grows a tray host just keeps one idle thread.
#[cfg(target_os = "linux")]
mod sni {
    use super::{Deliver, TrayAction};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    const WATCHER: &str = "org.kde.StatusNotifierWatcher";

    /// Asks for a build after each appearance of a host: at once, then
    /// twice more in case the host took its name a moment before it took
    /// calls. Past that the tray waits for the host's next appearance (a
    /// bar restart) rather than spinning against one that keeps refusing.
    const RETRY_AFTER: [Duration; 3] = [
        Duration::ZERO,
        Duration::from_secs(2),
        Duration::from_secs(10),
    ];

    /// Whether the tray is built, for the watcher thread: the tray itself
    /// is kept on a thread this one cannot see.
    pub(super) static BUILT: AtomicBool = AtomicBool::new(false);

    fn built() -> bool {
        BUILT.load(Ordering::Acquire)
    }

    pub(super) fn build_when_a_host_appears(deliver: Deliver) {
        thread::spawn(move || {
            if let Err(e) = watch(&deliver) {
                tracing::warn!(error = %e, "cannot watch the session bus for a tray host");
            }
        });
        tracing::info!("no tray host yet; the tray is built when one appears");
    }

    /// One round of [`RETRY_AFTER`] for one appearance of a host. True
    /// once the tray is built — each build happens on the thread that
    /// keeps the tray, so this only asks and then looks.
    fn ask(deliver: &Deliver) -> bool {
        for delay in RETRY_AFTER {
            thread::sleep(delay);
            if built() {
                return true;
            }
            deliver(TrayAction::HostAppeared);
        }
        built()
    }

    /// A round of asking per appearance of a watcher: now, if one is
    /// already there, then on every new owner, until the tray is built.
    /// Subscribes before it asks, or a host that took the name between the
    /// failed build and this call would never be heard of. Blocks for as
    /// long as the bus lives or the tray is missing.
    fn watch(deliver: &Deliver) -> zbus::Result<()> {
        let conn = zbus::blocking::Connection::session()?;
        let dbus = zbus::blocking::fdo::DBusProxy::new(&conn)?;
        let changes = dbus.receive_name_owner_changed_with_args(&[(0, WATCHER)])?;
        let name = zbus::names::BusName::try_from(WATCHER)?;
        if dbus.name_has_owner(name)? && ask(deliver) {
            return Ok(());
        }
        for change in changes {
            // The last round's last build may have held after it looked.
            if built() {
                return Ok(());
            }
            let args = change.args()?;
            if args.new_owner.is_some() && ask(deliver) {
                return Ok(());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole job of the ink is contrast with whatever is behind it,
    /// so a dark theme must not produce a dark glyph. A regression here
    /// is invisible in the literal sense: the icon is simply not there,
    /// with no error and nothing in the log.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_ink_is_light_on_a_dark_theme_and_dark_on_a_light_one() {
        assert!(ink_for(true).iter().all(|c| *c > 0x80), "dark theme");
        assert!(ink_for(false).iter().all(|c| *c < 0x80), "light theme");
    }

    /// Auto rests on CPU, turns for memory or disk, and memory wins
    /// when both are on. Pinned modes ignore both signals.
    #[test]
    fn auto_rests_on_cpu_and_turns_for_memory_or_disk() {
        assert_eq!(face_for(TrayPref::Auto, false, false), TrayFace::Cpu);
        assert_eq!(face_for(TrayPref::Auto, true, false), TrayFace::Memory);
        assert_eq!(face_for(TrayPref::Auto, false, true), TrayFace::Disk);
        assert_eq!(
            face_for(TrayPref::Auto, true, true),
            TrayFace::Memory,
            "memory still wins when the disk is full too"
        );
        assert_eq!(face_for(TrayPref::Cpu, true, true), TrayFace::Cpu);
        assert_eq!(face_for(TrayPref::Memory, false, true), TrayFace::Memory);
        assert_eq!(face_for(TrayPref::Both, false, true), TrayFace::Memory);
    }
}
