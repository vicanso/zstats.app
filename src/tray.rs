//! System tray icon and menu.
//!
//! Excluded on Linux (see `docs/design.md`): `tray-icon` drives its menu from a GTK
//! main loop there, which can't coexist with gpui's own event loop.

use crate::assets::{self, CustomIconName};
use crate::i18n;
use crate::state::TrayAnchor;
use crate::{APP_NAME, show_main_window, toggle_main_window};
use gpui::{App, Global};
use resvg::{tiny_skia, usvg};
use std::cell::RefCell;
use std::str;
use std::thread;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const MENU_ID_SHOW: &str = "show";
const MENU_ID_QUIT: &str = "quit";

/// Keeps the tray alive for the process lifetime — dropping a `TrayIcon`
/// removes it from the menu bar.
struct TrayHandle {
    icon: TrayIcon,
    /// Last title actually pushed to AppKit, so an unchanged reading is not
    /// re-applied — see `set_cpu_title`.
    last_title: RefCell<String>,
}

impl Global for TrayHandle {}

/// Mirror the current CPU load next to the menu bar icon, which is what the
/// design shows in its tray strip. A no-op if the tray failed to build.
pub fn set_cpu_title(cx: &App, cpu_percent: f32) {
    let Some(handle) = cx.try_global::<TrayHandle>() else {
        return;
    };
    // Whole percent: the menu bar is cramped, and a decimal would make the
    // title twitch on every sample even when load is flat.
    let title = format!("{cpu_percent:.0}%");
    let mut last = handle.last_title.borrow_mut();
    if *last == title {
        // Setting a title re-lays out the menu bar, so skip the no-ops.
        return;
    }
    handle.icon.set_title(Some(&title));
    *last = title;
}

enum TrayAction {
    /// Left-clicking the icon; carries the icon's screen rect to hang the
    /// window off. Toggles.
    Toggle(TrayAnchor),
    /// The menu item, which has no position and always shows.
    Show,
    Quit,
}

/// The menu bar icon, rasterised from the bundled SVG.
///
/// A CPU die rather than a trend arrow: an arrow makes a claim about the
/// data ("the numbers are going up"), while the subject itself does not.
///
/// `tray-icon` takes raw RGBA only — it has no SVG support and just re-encodes
/// whatever it is given as a PNG for `NSImage`. macOS then scales that to 18pt
/// tall (`platform_impl/macos/mod.rs`), so the bitmap is produced at 2x to stay
/// sharp on Retina.
fn tray_icon() -> Option<Icon> {
    Icon::from_rgba(rasterise_icon(ICON_SIZE)?, ICON_SIZE, ICON_SIZE).ok()
}

/// Rendered at 2x the 18pt macOS uses, so no upscaling happens.
const ICON_SIZE: u32 = 36;

/// Rasterise the bundled SVG to straight RGBA. Split out from [`tray_icon`]
/// so the result can be inspected in a test — a silently empty bitmap would
/// otherwise just look like a missing icon at runtime.
fn rasterise_icon(size: u32) -> Option<Vec<u8>> {
    rasterise_icon_scaled(size, GLYPH_SCALE)
}

/// Share of the canvas the glyph occupies; the rest is transparent margin.
///
/// lucide draws to the edges of its 24x24 viewBox, and macOS scales the whole
/// bitmap to 18pt — so at 1.0 the glyph is a full 18pt tall and outweighs the
/// ~12pt title beside it. System menu bar icons inset their artwork instead.
const GLYPH_SCALE: f32 = 0.78;

fn rasterise_icon_scaled(size: u32, glyph_scale: f32) -> Option<Vec<u8>> {
    // From the shared embed rather than `include_str!`, which would compile a
    // second, uncompressed copy of the same file into the binary.
    let raw = assets::get(&CustomIconName::Cpu.path())?;
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
    for pixel in rgba.chunks_exact_mut(4) {
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
        handle.icon.set_menu(Some(Box::new(build_menu())));
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
    let mut builder = TrayIconBuilder::new()
        .with_menu(Box::new(build_menu()))
        .with_tooltip(APP_NAME)
        // Left click shows the window directly; the menu moves to right click.
        // The `TrayIconEvent::Click` is emitted either way — this only stops
        // the menu from popping up over it.
        .with_menu_on_left_click(false);
    if let Some(icon) = tray_icon() {
        builder = builder.with_icon(icon);
        #[cfg(target_os = "macos")]
        {
            builder = builder.with_icon_as_template(true);
        }
    }

    let tray = match builder.build() {
        Ok(tray) => tray,
        Err(e) => {
            // Not fatal: the window still works, the app just has no tray.
            tracing::error!("failed to create tray icon: {e}");
            return;
        }
    };
    cx.set_global(TrayHandle {
        icon: tray,
        last_title: RefCell::new(String::new()),
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

    #[test]
    fn svg_rasterises_to_a_visible_glyph() {
        let rgba = rasterise_icon(ICON_SIZE).expect("icon should rasterise");
        assert_eq!(rgba.len() as u32, ICON_SIZE * ICON_SIZE * 4);

        let opaque = rgba.chunks_exact(4).filter(|px| px[3] > 32).count();
        let total = (ICON_SIZE * ICON_SIZE) as usize;
        let coverage = opaque as f32 / total as f32;

        // A failed parse (lucide's `currentColor`, a bad viewBox, a scale of
        // zero) yields a fully transparent bitmap that raises no error and
        // simply shows as a missing icon. A stroke-only glyph covers a modest
        // share of its box, so bracket it on both sides.
        assert!(
            (0.02..0.50).contains(&coverage),
            "unexpected glyph coverage {coverage:.3} — SVG likely failed to render"
        );
    }
}
