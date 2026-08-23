//! System tray icon and menu.
//!
//! Excluded on Linux (see `docs/design.md`): `tray-icon` drives its menu from a GTK
//! main loop there, which can't coexist with gpui's own event loop.

use crate::assets::{self, CustomIconName};
use crate::format;
use crate::i18n;
use crate::prefs::{self, TrayPref};
use crate::state::{TrayAnchor, ZStatsAppState};
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
}

/// One `NSStatusItem` and what it currently wears. Dropping it removes the
/// item from the menu bar — which is how Both mode's second item leaves
/// when the preference turns it off.
struct Item {
    icon: TrayIcon,
    /// `"primary"` / `"second"`, for the diary line a face change writes.
    label: &'static str,
    /// Face actually on the item, so an unchanged face is not re-applied —
    /// `set_icon` rebuilds an `NSImage` and re-lays out the menu bar.
    face: Cell<Option<TrayFace>>,
    /// Last title actually pushed to AppKit, same reason.
    title: RefCell<String>,
}

/// The two faces, rasterised once at build so a swap hands AppKit a
/// cached bitmap instead of parsing an SVG on the collector's hand-off.
/// `None` where the SVG failed to render — that face is then never
/// applied, and an item keeps whatever it had.
struct Faces {
    cpu: Option<Icon>,
    memory: Option<Icon>,
}

impl Faces {
    fn icon(&self, face: TrayFace) -> Option<&Icon> {
        match face {
            TrayFace::Cpu => self.cpu.as_ref(),
            TrayFace::Memory => self.memory.as_ref(),
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
    /// reads left-to-right the way the picker names the mode.
    second: RefCell<Option<Item>>,
    faces: Faces,
}

impl Global for TrayHandle {}

impl TrayHandle {
    /// Grow or shed the second item. A no-op when it already matches.
    fn set_both(&self, on: bool) {
        let mut second = self.second.borrow_mut();
        match (on, second.is_some()) {
            (true, false) => *second = build_item("second", TrayFace::Cpu, &self.faces),
            (false, true) => *second = None,
            _ => {}
        }
    }
}

impl Item {
    /// Put a face and, once there is a sample, its figure on the item.
    fn wear(&self, face: TrayFace, figure: Option<Figure>, faces: &Faces) {
        self.set_face(face, faces);
        if let Some(figure) = figure {
            self.set_title(figure);
        }
    }

    fn set_face(&self, face: TrayFace, faces: &Faces) {
        if self.face.get() == Some(face) {
            return;
        }
        let Some(icon) = faces.icon(face) else {
            return;
        };
        // Not `set_icon`: on macOS the crate hard-codes template *off* in
        // that path, and our bitmaps are flattened to black with only alpha
        // meaning anything — swapped in untemplated, the glyph is a black
        // block on a dark menu bar. This is the call that carries the flag.
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
                "could not swap the tray icon: {e}"
            );
            return;
        }
        // A face change is a state change the user sees — the diary
        // should say when and to what, like every alert verdict does.
        tracing::info!(item = self.label, ?face, "tray face changed");
        self.face.set(Some(face));
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
    let icon = faces.icon(face).cloned();
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
        }),
        Err(e) => {
            // Not fatal: the window still works, the app just has no tray.
            tracing::error!(item = label, "failed to create tray icon: {e}");
            None
        }
    }
}

/// The face for a preference and the store's answer to "is memory what
/// needs attention right now" (`ZStatsAppState::memory_needs_attention`).
///
/// Auto is the only mode with two answers, and it has exactly one
/// trigger. CPU is the resting face — it is what a menu bar monitor is
/// expected to show, and what the design's tray strip shows — so a CPU
/// episode changes nothing: the figure it concerns is already there.
/// Memory is the face that has to be *earned*, and when both are in
/// trouble it wins, because memory is the one macOS escalates
/// (compressor, swap, jetsam) while a busy CPU just stays busy.
///
/// This is the *primary* item's face. In Both that is memory: the
/// second item lands to its left (see `TrayHandle::second`) and wears
/// CPU, so the pair reads CPU · memory.
pub fn face_for(pref: TrayPref, memory_needs_attention: bool) -> TrayFace {
    match pref {
        TrayPref::Cpu => TrayFace::Cpu,
        TrayPref::Memory | TrayPref::Both => TrayFace::Memory,
        TrayPref::Auto if memory_needs_attention => TrayFace::Memory,
        TrayPref::Auto => TrayFace::Cpu,
    }
}

/// Bring the menu bar in line with the store: how many items the
/// preference wants, the face from the preference and the store's memory
/// signal, the figure from the latest sample — `cpu.usage_percent` or
/// `memory.available_bytes`, both zstats' own fields. Called after every
/// ingest and when the picker changes; before the first sample there is
/// a face but no figure. A no-op if the tray failed to build.
pub fn sync(cx: &App, state: &ZStatsAppState) {
    let Some(handle) = cx.try_global::<TrayHandle>() else {
        return;
    };
    let pref = prefs::tray();
    handle.set_both(pref == TrayPref::Both);
    let figure = |face| {
        state.latest().map(|tick| match face {
            TrayFace::Cpu => Figure::cpu(tick.snapshot.cpu.usage_percent),
            TrayFace::Memory => {
                let mem = &tick.snapshot.memory;
                Figure::memory(mem.available_bytes, mem.total_bytes)
            }
        })
    };
    let face = face_for(pref, state.memory_needs_attention());
    handle.primary.wear(face, figure(face), &handle.faces);
    if let Some(second) = handle.second.borrow().as_ref() {
        second.wear(TrayFace::Cpu, figure(TrayFace::Cpu), &handle.faces);
    }
}

enum TrayAction {
    /// Left-clicking the icon; carries the icon's screen rect to hang the
    /// window off. Toggles.
    Toggle(TrayAnchor),
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
    // usvg cannot resolve on its own. The colour is irrelevant anyway: as a
    // template image only the alpha channel survives.
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
    // Flatten to black and keep only alpha. tiny-skia hands back premultiplied
    // colour, which `Icon::from_rgba` would read as straight — moot here,
    // since a template image is recoloured by the system from alpha alone.
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel[0] = 0;
        pixel[1] = 0;
        pixel[2] = 0;
    }
    Some(rgba)
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
pub fn init_tray(cx: &mut App) {
    // Rasterised up front so a later swap is cheap.
    let faces = Faces {
        cpu: tray_icon(CustomIconName::Cpu),
        memory: tray_icon(CustomIconName::MemoryStick),
    };
    // The store is empty here, so the face is the preference's resting
    // one (`memory_needs_attention` = false): a pinned mode launches
    // already wearing its face instead of flipping on the first sample.
    let pref = prefs::tray();
    let Some(primary) = build_item("primary", face_for(pref, false), &faces) else {
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
        faces,
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
            // means a press-and-hold doesn't fire until the button is released.
            // `rect` is the icon's screen rect, which is what the window gets
            // anchored to.
            let anchor = match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    rect,
                    ..
                } => TrayAnchor {
                    x: rect.position.x,
                    y: rect.position.y,
                    width: f64::from(rect.size.width),
                    height: f64::from(rect.size.height),
                },
                _ => continue,
            };
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
        for glyph in [CustomIconName::Cpu, CustomIconName::MemoryStick] {
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

    /// Auto's one trigger is memory; the pinned modes ignore it, and
    /// Both's primary is the memory one (CPU goes on the item to its
    /// left).
    #[test]
    fn auto_rests_on_cpu_and_turns_only_for_memory() {
        assert_eq!(face_for(TrayPref::Auto, false), TrayFace::Cpu);
        assert_eq!(face_for(TrayPref::Auto, true), TrayFace::Memory);
        assert_eq!(face_for(TrayPref::Cpu, true), TrayFace::Cpu);
        assert_eq!(face_for(TrayPref::Memory, false), TrayFace::Memory);
        assert_eq!(face_for(TrayPref::Both, false), TrayFace::Memory);
        assert_eq!(face_for(TrayPref::Both, true), TrayFace::Memory);
    }
}
