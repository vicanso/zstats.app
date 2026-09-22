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
//!   main.rs). Every anchor on this path is `None`, which is the same
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

use crate::assets::{self, CustomIconName};
use crate::format;
use crate::i18n;
use crate::prefs::{self, TrayPref};
use crate::state::{TrayAnchor, ZStatsAppState};
/// Only the ink reads it, and only where there is no template image.
#[cfg(not(target_os = "macos"))]
use crate::theme;
use crate::{APP_NAME, show_main_window, toggle_main_window};
use gpui::{App, Global};
use resvg::{tiny_skia, usvg};
use rust_i18n::t;
use std::cell::{Cell, RefCell};
use std::str;
use std::thread;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const MENU_ID_SHOW: &str = "show";
const MENU_ID_QUIT: &str = "quit";

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

impl Global for TrayHandle {}

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
/// true for `init_tray` and for `sync`, which only ever runs inside
/// `cx.update`.
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
pub fn sync(cx: &App, state: &ZStatsAppState) {
    let Some(handle) = cx.try_global::<TrayHandle>() else {
        return;
    };
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

enum TrayAction {
    /// Left-clicking the icon; carries the icon's screen rect to hang the
    /// window off, or `None` where the platform reports none. Toggles.
    Toggle(Option<TrayAnchor>),
    /// The menu item, which has no position and always shows.
    Show,
    Quit,
}

/// A menu bar face, rasterised from the bundled SVG.
///
/// A CPU die and a memory stick rather than a trend arrow: an arrow makes
/// a claim about the data ("the numbers are going up"), while the subject
/// itself does not. The face that is showing *is* a claim — but one
/// zstats or the kernel already made (`face_for`), never a reading of
/// the numbers here.
///
/// `tray-icon` takes raw RGBA only — it has no SVG support and just re-encodes
/// whatever it is given as a PNG for `NSImage`. macOS then scales that to 18pt
/// tall (`platform_impl/macos/mod.rs`), so the bitmap is produced at 2x to stay
/// sharp on Retina.
fn tray_icon(glyph: CustomIconName) -> Option<Icon> {
    Icon::from_rgba(rasterise_icon(glyph, ICON_SIZE)?, ICON_SIZE, ICON_SIZE).ok()
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
        ink_for(theme::is_dark())
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

/// Same die, plus a filled spec in the top-left, in the glyph's own ink
/// rather than a colour of its own. On macOS that means extra alpha on
/// the same template, so the spec takes the menu bar's ink along with the
/// die (white on dark, black on light) — a painted colour there would
/// have to drop template mode and become a black block. Off macOS it is
/// [`ink`]'s colour, the same one the strokes carry.
fn tray_icon_hot(glyph: CustomIconName) -> Option<Icon> {
    let mut rgba = rasterise_icon(glyph, ICON_SIZE)?;
    stamp_hot_dot(&mut rgba, ICON_SIZE);
    Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE).ok()
}

/// 4 pt at 2×, top-left — away from the percent title on the right.
const DOT_DIAMETER: u32 = 8;
const DOT_INSET: u32 = 1;

fn stamp_hot_dot(rgba: &mut [u8], size: u32) {
    let ink = ink();
    let radius = DOT_DIAMETER as f32 / 2.0;
    let cx = DOT_INSET as f32 + radius;
    let cy = DOT_INSET as f32 + radius;
    let r2 = radius * radius;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= r2 {
                let i = ((y * size + x) * 4) as usize;
                rgba[i..i + 3].copy_from_slice(&ink);
                rgba[i + 3] = 255;
            }
        }
    }
}

/// Rendered at 2x the 18pt macOS uses, so no upscaling happens.
const ICON_SIZE: u32 = 36;

/// Rasterise the bundled SVG to straight RGBA. Split out from [`tray_icon`]
/// so the result can be inspected in a test — a silently empty bitmap would
/// otherwise just look like a missing icon at runtime.
fn rasterise_icon(glyph: CustomIconName, size: u32) -> Option<Vec<u8>> {
    rasterise_icon_scaled(glyph, size, GLYPH_SCALE)
}

/// Share of the canvas the glyph occupies; the rest is transparent margin.
///
/// lucide draws to the edges of its 24x24 viewBox, and macOS scales the whole
/// bitmap to 18pt — so at 1.0 the glyph is a full 18pt tall and outweighs the
/// ~12pt title beside it. System menu bar icons inset their artwork instead.
const GLYPH_SCALE: f32 = 0.78;

fn rasterise_icon_scaled(glyph: CustomIconName, size: u32, glyph_scale: f32) -> Option<Vec<u8>> {
    // From the shared embed rather than `include_str!`, which would compile a
    // second, uncompressed copy of the same file into the binary.
    let raw = assets::get(&glyph.path())?;
    // lucide ships `stroke="currentColor"`, which is a CSS-context keyword
    // usvg cannot resolve on its own. Which colour does not matter: the
    // channels are overwritten with `ink` below, and only alpha survives
    // this function.
    let svg = str::from_utf8(&raw)
        .ok()?
        .replace("currentColor", "#000000");

    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let source = tree.size();
    let longest = source.width().max(source.height());
    let scale = size as f32 * glyph_scale / longest;
    // Centre what is left over, so the margin is even on all four sides.
    let inset = (size as f32 - longest * scale) / 2.0;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_translate(inset, inset).pre_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    let mut rgba = pixmap.take();
    // Keep only alpha and repaint the colour channels. tiny-skia hands back
    // premultiplied colour, which `Icon::from_rgba` would read as straight,
    // so those channels have to be rewritten whatever happens — `ink` is
    // what decides to what. On macOS the template discards it and works off
    // alpha alone; off macOS it *is* the glyph.
    let ink = ink();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel[..3].copy_from_slice(&ink);
    }
    Some(rgba)
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
pub fn anchor(cx: &App) -> Option<TrayAnchor> {
    let handle = cx.try_global::<TrayHandle>()?;
    let rect = handle.primary.icon.rect()?;
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
pub fn rebuild_menu(cx: &App) {
    if let Some(handle) = cx.try_global::<TrayHandle>() {
        handle.primary.icon.set_menu(Some(Box::new(build_menu())));
        if let Some(second) = handle.second.borrow().as_ref() {
            second.icon.set_menu(Some(Box::new(build_menu())));
        }
    }
}

fn build_menu() -> Menu {
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::with_id(
        MENU_ID_SHOW,
        i18n::tr("common.show_window"),
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

/// Must be called on the main thread (macOS creates an `NSStatusItem`), which
/// is where `Application::run`'s callback already runs.
///
/// Without a tray host — no SNI host on the session bus, most of a
/// desktop that simply has no tray — `build_item` logs and returns `None`
/// and this returns early. That is not an error path: the panel still
/// works, it just has no icon, and on Linux the way back to it is the
/// keybinding (`docs/omarchy-port.md` 阶段 2).
pub fn init_tray(cx: &mut App) {
    // Rasterised up front so a later swap is cheap.
    let faces = build_faces();
    // The store is empty here, so the face is the preference's resting
    // one (no live episode): a pinned mode launches already wearing
    // its face instead of flipping on the first sample.
    let pref = prefs::tray();
    let Some(primary) = build_item("primary", face_for(pref, false, false), &faces) else {
        return;
    };
    // Both's second item is built here too rather than left to the first
    // `sync`: a launch in that mode should not show one item and grow
    // another a few seconds later.
    let second = (pref == TrayPref::Both)
        .then(|| build_item("second", TrayFace::Cpu, &faces))
        .flatten();
    cx.set_global(TrayHandle {
        primary,
        second: RefCell::new(second),
        faces: RefCell::new(faces),
        ink: Cell::new(ink()),
    });

    // Both receivers only block, so park a dedicated thread on each (zero CPU
    // while idle) and funnel their events onto the main-thread executor.
    // Two threads rather than a `select!` keeps crossbeam out of our deps.
    let (action_tx, action_rx) = smol::channel::unbounded::<TrayAction>();

    let menu_tx = action_tx.clone();
    thread::spawn(move || {
        let receiver = MenuEvent::receiver();
        while let Ok(event) = receiver.recv() {
            let action = match event.id().0.as_str() {
                MENU_ID_SHOW => TrayAction::Show,
                MENU_ID_QUIT => TrayAction::Quit,
                _ => continue,
            };
            if menu_tx.send_blocking(action).is_err() {
                return;
            }
        }
    });

    thread::spawn(move || {
        let receiver = TrayIconEvent::receiver();
        while let Ok(event) = receiver.recv() {
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
                _ => continue,
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
            if action_tx.send_blocking(TrayAction::Toggle(anchor)).is_err() {
                return;
            }
        }
    });

    cx.spawn(async move |cx| {
        // Ends on its own: once the app shuts down this task stops being
        // polled, and the sender thread exits when the channel drops.
        while let Ok(action) = action_rx.recv().await {
            cx.update(|cx| match action {
                TrayAction::Toggle(anchor) => toggle_main_window(cx, anchor),
                TrayAction::Show => show_main_window(cx),
                TrayAction::Quit => cx.quit(),
            });
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both faces, not just the one the item is built with: the memory
    /// glyph is applied only on a swap, so a broken SVG there would show
    /// up as "the icon vanished the first time memory got tight".
    #[test]
    fn every_face_rasterises_to_a_visible_glyph() {
        for glyph in [
            CustomIconName::Cpu,
            CustomIconName::MemoryStick,
            CustomIconName::HardDrive,
        ] {
            let rgba = rasterise_icon(glyph, ICON_SIZE).expect("icon should rasterise");
            assert_eq!(rgba.len() as u32, ICON_SIZE * ICON_SIZE * 4);

            let opaque = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|px| px[3] > 32)
                .count();
            let total = (ICON_SIZE * ICON_SIZE) as usize;
            let coverage = opaque as f32 / total as f32;

            // A failed parse (lucide's `currentColor`, a bad viewBox, a
            // scale of zero) yields a fully transparent bitmap that raises
            // no error and simply shows as a missing icon. A stroke-only
            // glyph covers a modest share of its box, so bracket it on
            // both sides.
            assert!(
                (0.02..0.50).contains(&coverage),
                "{glyph:?}: unexpected glyph coverage {coverage:.3} — SVG likely failed to render"
            );
        }
    }

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

    #[test]
    fn an_alert_spec_is_extra_alpha_in_the_glyphs_own_ink() {
        for glyph in [
            CustomIconName::Cpu,
            CustomIconName::MemoryStick,
            CustomIconName::HardDrive,
        ] {
            let mut rgba = rasterise_icon(glyph, ICON_SIZE).expect("glyph");
            let cx = (DOT_INSET + DOT_DIAMETER / 2) as usize;
            let i = (cx * ICON_SIZE as usize + cx) * 4;
            let before = rgba[i + 3];
            stamp_hot_dot(&mut rgba, ICON_SIZE);
            assert_eq!(rgba[i + 3], 255, "{glyph:?}: the spec must be opaque");
            assert!(rgba[i + 3] >= before);
            assert_eq!(
                &rgba[i..i + 3],
                &ink(),
                "{glyph:?}: the glyph's own ink, never a colour of its own"
            );
        }
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
