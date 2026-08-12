//! System tray icon and menu.
//!
//! Excluded on Linux (see `README.md`): `tray-icon` drives its menu from a GTK
//! main loop there, which can't coexist with gpui's own event loop.

use crate::format;
use crate::i18n;
use crate::state::TrayAnchor;
use crate::{APP_NAME, show_main_window, toggle_main_window};
use gpui::{App, Global};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const MENU_ID_SHOW: &str = "show";
const MENU_ID_QUIT: &str = "quit";

/// Keeps the tray alive for the process lifetime — dropping a `TrayIcon`
/// removes it from the menu bar.
struct TrayHandle(TrayIcon);

impl Global for TrayHandle {}

/// Mirror the current CPU load next to the menu bar icon, which is what the
/// design shows in its tray strip. A no-op if the tray failed to build.
pub fn set_cpu_title(cx: &App, cpu_percent: f32) {
    if let Some(handle) = cx.try_global::<TrayHandle>() {
        handle.0.set_title(Some(format::pct(cpu_percent)));
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

/// Placeholder icon: a 22×22 bar chart drawn in code so there's no binary
/// asset to carry yet. It's registered as a macOS template image, so only the
/// alpha channel matters — the system recolours it for light / dark menu bars.
/// Replace with `include_bytes!("../assets/icon.png")` + `image::load_from_memory`
/// once there's real artwork.
fn placeholder_icon() -> Option<Icon> {
    const SIZE: u32 = 22;
    const BASELINE: u32 = 18;
    /// (left edge, width, top edge) — three bars of rising height.
    const BARS: [(u32, u32, u32); 3] = [(4, 4, 12), (9, 4, 8), (14, 4, 4)];

    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    for (x0, width, y0) in BARS {
        for y in y0..BASELINE {
            for x in x0..x0 + width {
                let i = ((y * SIZE + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).ok()
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
    if let Some(icon) = placeholder_icon() {
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
            eprintln!("failed to create tray icon: {e}");
            return;
        }
    };
    cx.set_global(TrayHandle(tray));

    // Both receivers only block, so park a dedicated thread on each (zero CPU
    // while idle) and funnel their events onto the main-thread executor.
    // Two threads rather than a `select!` keeps crossbeam out of our deps.
    let (action_tx, action_rx) = smol::channel::unbounded::<TrayAction>();

    let menu_tx = action_tx.clone();
    std::thread::spawn(move || {
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

    std::thread::spawn(move || {
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
