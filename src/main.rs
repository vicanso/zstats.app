// Release builds detach from the console on Windows — otherwise launching the
// app pops an empty terminal behind the window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod assets;
mod autostart;
mod bigfiles;
mod cleanhints;
mod confirm;
mod diskscan;
#[cfg(target_os = "macos")]
mod dock;
mod font;
mod format;
mod fullscan;
mod history;
mod i18n;
mod i18n_loader;
mod metrics;
mod notify;
mod placement;
mod prefs;
#[cfg(target_os = "macos")]
mod procscan;
mod proxy;
mod state;
#[cfg(target_os = "macos")]
mod terminate;
mod theme;
#[cfg(not(target_os = "linux"))]
mod tray;
mod views;
mod watch;
#[cfg(target_os = "macos")]
mod window_ext;

use crate::assets::Assets;
use crate::placement::{DEFAULT_WINDOW_SIZE, MIN_WINDOW_SIZE, bounds_below_tray};
use crate::state::{TrayAnchor, ZStatsAppState, ZStatsGlobalStore};

// Pointed at the empty `locales_stub/` so the macro embeds no translations.
// Real files live in `assets/locales/` and inflate via `i18n_loader`.
rust_i18n::i18n!(
    "locales_stub",
    fallback = "en",
    backend = crate::i18n_loader::runtime_backend()
);
use gpui::{
    App, Bounds, Context, KeyBinding, Menu, MenuItem, QuitMode, ScrollHandle, SharedString,
    Subscription, TitlebarOptions, Window, WindowAppearance, WindowBackgroundAppearance,
    WindowBounds, WindowOptions, actions, div, prelude::*, px, size,
};
use gpui_component::{ActiveTheme, Icon, Root, Sizable, Size, Theme, ThemeMode};

use std::time::Duration;

/// Shown in the app menu, the tray tooltip, the task switcher and the Linux
/// title bar.
pub const APP_NAME: &str = "zstats";
/// Wayland `app_id` / X11 WM_CLASS — task switchers group windows by it and
/// resolve the icon from the installed `.desktop` entry.
const LINUX_APP_ID: &str = "com.github.vicanso.zstats";

/// Native vibrancy, but only on macOS: there gpui backs `Blurred` with an
/// `NSVisualEffectView`. Elsewhere it's documented as "not always supported"
/// and degrades to plain transparency, which would show the raw desktop.
const WINDOW_BACKGROUND: WindowBackgroundAppearance = if cfg!(target_os = "macos") {
    WindowBackgroundAppearance::Blurred
} else {
    WindowBackgroundAppearance::Opaque
};
/// Dark: one wash over the whole window. 0.18 is fine on a dark desktop
/// but a light wallpaper shows straight through and the panel turns into
/// grey fog (light type on white glass). 0.55 is still one layer — tabs,
/// cards and empty space stay the same material, no extra body fill.
/// Light: a *thick* wash. Light tokens use dark type, and a dark wallpaper
/// shining through 0.2 opacity turns the panel into grey fog.
const BACKGROUND_OPACITY_DARK: f32 = if cfg!(target_os = "macos") { 0.55 } else { 1.0 };
/// Light mode is deliberately near-opaque: vibrancy there is pale-on-pale,
/// so the blur reads as barely anything while the desktop's detail still
/// bleeds through and fights the dark text. Legibility wins; the effect is
/// conceded. Dark mode keeps it, where the contrast actually carries.
const BACKGROUND_OPACITY_LIGHT: f32 = if cfg!(target_os = "macos") { 0.80 } else { 1.0 };
/// Clicking the tray icon first takes focus away from the window, which
/// auto-hides it, and only then delivers the click. A click landing inside
/// this window of an auto-hide is read as "the user wanted it gone" and does
/// not reopen — that's what makes the tray icon toggle.
const TOGGLE_GRACE: Duration = Duration::from_millis(300);

actions!(zstats, [Quit]);

/// The root view. Owns the window-lifecycle subscriptions and hands the
/// panel itself to `views::root`; the gpui-component dialog / notification
/// layers are mounted over it.
struct ZStatsApp {
    /// Whether the window has ever held focus. A freshly created window also
    /// gets a deactivation callback before it is first activated; closing on
    /// that one would make it flash open and vanish.
    was_active: bool,
    _activation: Subscription,
    /// Follows System Settings → Appearance so our tokens stay in sync
    /// with the Popover material (they are not derived from Theme).
    _appearance: Subscription,
    /// Repaints the panel when a collection tick lands. Without it the store
    /// would update and nothing on screen would move.
    _metrics: Subscription,
}

impl ZStatsApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.was_active = true;
                return;
            }
            if !this.was_active {
                return;
            }
            // Debug keeps the panel up so you can inspect it from the IDE
            // or another window. Release still collapses to the tray.
            if cfg!(debug_assertions) {
                return;
            }
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| {
                    state.mark_auto_hidden();
                    state.reset_transient_views(window, cx);
                });
            // Order it off screen rather than destroy it — rebuilding the
            // window on every toggle is what leaked ~1 MB a cycle.
            #[cfg(target_os = "macos")]
            {
                window_ext::hide(window);
                cx.global::<metrics::CollectorPace>().hidden();
            }
            #[cfg(not(target_os = "macos"))]
            window.remove_window();
        });
        let appearance = cx.observe_window_appearance(window, |_this, window, cx| {
            apply_appearance(window.appearance(), cx);
            cx.notify();
        });
        let store = cx.global::<ZStatsGlobalStore>().clone();
        // Repaint only while the panel is actually on screen: the window is
        // hidden rather than destroyed, so without this check every tick would
        // render a full panel that nobody can see.
        let metrics = cx.observe(&store, |_this, _state, cx| {
            if cx.global::<metrics::CollectorPace>().is_visible() {
                cx.notify();
            }
        });

        Self {
            was_active: false,
            _activation: activation,
            _appearance: appearance,
            _metrics: metrics,
        }
    }
}

impl Render for ZStatsApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);

        // Mirror the window geometry into the global state on *every* frame,
        // including the first: reopening from the tray builds a brand-new
        // window, and this is the only place the old size and the display
        // scale factor survive. `set_window_metrics` does its own
        // change-detection, so this is cheap and doesn't loop.
        let bounds = window.bounds();
        let scale_factor = window.scale_factor();
        let store = cx.global::<ZStatsGlobalStore>().clone();
        store.update(cx, |state, cx| {
            state.set_window_metrics(bounds, scale_factor, cx)
        });

        let tint = prefs::applied_opacity().unwrap_or(if theme::is_dark() {
            BACKGROUND_OPACITY_DARK
        } else {
            BACKGROUND_OPACITY_LIGHT
        });
        div()
            .relative()
            .size_full()
            .bg(cx.theme().background.opacity(tint))
            .text_color(cx.theme().foreground)
            .child(views::root(cx))
            .children(dialog_layer)
            .children(notification_layer)
    }
}

/// Map the OS appearance to a theme mode when the user hasn't pinned one.
/// `VibrantLight` is macOS's translucent *light* appearance — group it with
/// `Light` so only genuinely dark appearances select the dark theme.
fn theme_mode_for_appearance(appearance: WindowAppearance) -> ThemeMode {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeMode::Light,
        _ => ThemeMode::Dark,
    }
}

/// gpui-component Theme plus our own tokens. `Theme::change` resets the
/// mono family, so [`font::apply`] has to run every time. `appearance` is
/// what the OS reports; a pinned theme preference wins over it.
fn apply_appearance(appearance: WindowAppearance, cx: &mut App) {
    let mode = match prefs::theme() {
        prefs::ThemePref::System => theme_mode_for_appearance(appearance),
        prefs::ThemePref::Light => ThemeMode::Light,
        prefs::ThemePref::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, None, cx);
    font::apply(cx);
    theme::set_dark(matches!(mode, ThemeMode::Dark));
}

/// Pin (or release) AppKit's own appearance to match the theme preference.
///
/// Without this a forced theme would only recolour our tokens: the vibrancy
/// material underneath follows the *window's* appearance, and dark type over
/// a light Popover blur is exactly the grey fog the background opacities were
/// tuned to avoid. Pinning `NSApp.appearance` makes every window — and any
/// appearance AppKit resolves for it — agree with the forced theme; `None`
/// hands control back to System Settings.
#[cfg(target_os = "macos")]
fn apply_ns_appearance() {
    use objc2_app_kit::{
        NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
    };
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let appearance = match prefs::theme() {
        prefs::ThemePref::System => None,
        // SAFETY: reading AppKit's exported appearance-name constants.
        prefs::ThemePref::Light => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }),
        prefs::ThemePref::Dark => {
            NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua })
        }
    };
    app.setAppearance(appearance.as_deref());
}

/// The Interface page's launch-at-login switch. The OS records the
/// state (System Settings → Login Items), we only relay — the chip
/// re-reads the system's answer on repaint, so a failed register (a
/// bundle-less debug run) shows as the switch simply staying off.
pub fn set_autostart_pref(on: bool, cx: &mut App) {
    autostart::set_enabled(on);
    repaint(cx);
}

/// The Config tab's theme picker: persist, re-pin AppKit, restyle, repaint.
pub fn set_theme_pref(pref: prefs::ThemePref, cx: &mut App) {
    prefs::set_theme(pref);
    #[cfg(target_os = "macos")]
    apply_ns_appearance();
    apply_appearance(cx.window_appearance(), cx);
    repaint(cx);
}

/// The Config tab's language picker: persist, re-pin the locale, and rebuild
/// the chrome that snapshotted translated strings when it was built — the
/// tray menu and the app menu. Everything else re-translates on repaint.
pub fn set_language_pref(pref: prefs::LanguagePref, cx: &mut App) {
    prefs::set_language(pref);
    i18n::init();
    install_menus(cx);
    #[cfg(not(target_os = "linux"))]
    tray::rebuild_menu(cx);
    repaint(cx);
}

/// The Interface page's opacity picker: persist only. The wash is
/// sampled at launch, so a change sits in `app.toml` until the next start.
pub fn set_opacity_pref(value: Option<f32>, cx: &mut App) {
    prefs::set_opacity(value);
    repaint(cx);
}

/// Nudge the store so the visible panel repaints with the new preference.
fn repaint(cx: &mut App) {
    cx.global::<ZStatsGlobalStore>()
        .clone()
        .update(cx, |_, cx| cx.notify());
}

fn install_menus(cx: &mut App) {
    cx.set_menus(vec![Menu {
        name: APP_NAME.into(),
        items: vec![MenuItem::action(i18n::tr("common.quit"), Quit)],
        disabled: false,
    }]);
}

/// Fill in the app identity fields on a [`WindowOptions`] without clobbering
/// caller-supplied values. Call it for every window: Linux compositors
/// (especially KDE + Wayland) otherwise show a generic icon and an empty
/// title, and macOS / Windows still use the title in the task switcher even
/// when the title bar is custom-drawn.
fn with_app_identity(mut options: WindowOptions) -> WindowOptions {
    if options.app_id.is_none() {
        options.app_id = Some(LINUX_APP_ID.to_string());
    }
    match &mut options.titlebar {
        Some(titlebar) if titlebar.title.is_none() => {
            titlebar.title = Some(SharedString::from(APP_NAME));
        }
        // Only where a title bar is actually wanted. On macOS `None` is the
        // whole point — conjuring one here would put the traffic lights back.
        #[cfg(not(target_os = "macos"))]
        None => {
            options.titlebar = Some(TitlebarOptions {
                title: Some(SharedString::from(APP_NAME)),
                ..Default::default()
            });
        }
        _ => {}
    }
    options
}

/// Retarget gpui's vibrancy view at the `Popover` material.
///
/// gpui hardcodes `NSVisualEffectMaterial::Selection` (`gpui_macos/src/window.rs`),
/// which is the selection-highlight material, not the one AppKit uses for
/// menu bar panels. The view itself is fine — it's the bottom-most subview of
/// the window's content view — so this just walks the subviews and re-materials
/// the `NSVisualEffectView` it finds. A no-op if gpui ever stops adding one.
#[cfg(target_os = "macos")]
fn use_popover_material(window: &Window) {
    use objc2::ClassType;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSVisualEffectMaterial, NSVisualEffectView};
    use raw_window_handle::RawWindowHandle;

    // Fully qualified: gpui's inherent `Window::window_handle()` (returning
    // `AnyWindowHandle`) shadows the `HasWindowHandle` trait method.
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return;
    };
    unsafe {
        let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
        let ns_window: *mut AnyObject = objc2::msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        let content: *mut AnyObject = objc2::msg_send![ns_window, contentView];
        let subviews: *mut AnyObject = objc2::msg_send![content, subviews];
        let count: usize = objc2::msg_send![subviews, count];
        for i in 0..count {
            let view: *mut AnyObject = objc2::msg_send![subviews, objectAtIndex: i];
            let is_effect_view: bool =
                objc2::msg_send![view, isKindOfClass: NSVisualEffectView::class()];
            if is_effect_view {
                let _: () = objc2::msg_send![view, setMaterial: NSVisualEffectMaterial::Popover];
            }
        }
    }
}

/// The settings window: a left nav (Config / About) and a scrolling
/// body. A separate window rather than a tab so a settings session is
/// not cut short by the popover auto-hiding on focus loss — and it is a
/// *standard* window (title bar, traffic lights, opaque background), not
/// a second popover. Repaints are driven by observing the store, the
/// same signal the panel's per-tick repaint rides on.
struct SettingsWindow {
    section: views::config::SettingsSection,
    scroll: ScrollHandle,
    /// The proxy setting's text field. Lives with this window, like the
    /// panel's filter input lives with the panel — not in global state.
    proxy_input: gpui::Entity<gpui_component::input::InputState>,
    /// Whether the field currently parses; drives the inline warning.
    /// Only valid values are persisted, so junk never reaches app.toml.
    proxy_valid: bool,
}

impl SettingsWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = cx.global::<ZStatsGlobalStore>().clone();
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        let proxy_input = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(i18n::tr("config.proxy_placeholder"))
                .default_value(prefs::proxy())
        });
        cx.subscribe(
            &proxy_input,
            |this, input, event: &gpui_component::input::InputEvent, cx| {
                if matches!(event, gpui_component::input::InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    this.proxy_valid = proxy::is_valid_proxy_setting(&value);
                    if this.proxy_valid {
                        // Persists and mirrors into the proxy resolver —
                        // the next fetch picks it up, no restart.
                        prefs::set_proxy(&value);
                    }
                    cx.notify();
                }
            },
        )
        .detach();
        Self {
            section: views::config::SettingsSection::Interface,
            scroll: ScrollHandle::new(),
            proxy_input,
            proxy_valid: true,
        }
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The confirm sheet (config reset) needs the dialog layer, same
        // as the main window's root.
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        let bg = cx.theme().background;
        let fg = cx.theme().foreground;
        let state = cx.global::<ZStatsGlobalStore>().read(cx);
        let section = self.section;
        let body = gpui_component::v_flex()
            .gap(px(8.))
            .children(views::config::render(
                state,
                section,
                &self.proxy_input,
                self.proxy_valid,
            ));
        div()
            .relative()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .child(
                gpui_component::h_flex()
                    .size_full()
                    .child(settings_nav(section, cx))
                    .child(
                        div()
                            .id("settings-body")
                            .track_scroll(&self.scroll)
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .px(px(16.))
                            .py(px(14.))
                            .child(body),
                    ),
            )
            .children(dialog_layer)
            .children(notification_layer)
    }
}

fn settings_nav(
    current: views::config::SettingsSection,
    cx: &mut Context<SettingsWindow>,
) -> gpui::AnyElement {
    use views::config::SettingsSection;
    gpui_component::v_flex()
        .id("settings-nav")
        .flex_none()
        .w(px(132.))
        .h_full()
        .px(px(10.))
        .py(px(12.))
        .gap(px(2.))
        .border_r(px(1.))
        .border_color(theme::border_subtle())
        .bg(theme::trough())
        .children(
            SettingsSection::ALL
                .into_iter()
                .enumerate()
                .map(|(i, item)| {
                    let on = item == current;
                    div()
                        .id(("settings-nav-item", i))
                        .w_full()
                        .rounded(px(6.))
                        .px(px(10.))
                        .py(px(6.))
                        .bg(if on {
                            theme::surface()
                        } else {
                            gpui::rgba(0x00000000)
                        })
                        .when(!on, |d| d.hover(|d| d.bg(theme::surface_raised())))
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            if this.section != item {
                                this.section = item;
                                this.scroll = ScrollHandle::new();
                                cx.notify();
                            }
                        }))
                        .child(
                            gpui_component::h_flex()
                                .items_center()
                                .gap(px(6.))
                                .child(
                                    Icon::new(item.icon())
                                        .with_size(Size::Size(px(13.)))
                                        .text_color(gpui::Hsla::from(if on {
                                            theme::ink()
                                        } else {
                                            theme::text_dim()
                                        })),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .font_weight(if on {
                                            gpui::FontWeight::MEDIUM
                                        } else {
                                            gpui::FontWeight::NORMAL
                                        })
                                        .text_color(if on { theme::ink() } else { theme::text() })
                                        .child(i18n::tr(item.label_key())),
                                ),
                        )
                }),
        )
        .into_any_element()
}

/// Open the settings window, or focus the one already open. Closing it
/// really closes (the main window's own stance); the stored handle then
/// fails its update and the next click builds a fresh window.
pub fn open_settings_window(cx: &mut App) {
    let existing = cx.global::<ZStatsGlobalStore>().read(cx).settings_window();
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let bounds = Bounds::centered(None, size(px(520.), px(620.)), cx);
    let opened = cx.open_window(
        with_app_identity(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(460.), px(420.))),
            // A real title bar on purpose — this window closes with its
            // own traffic lights, unlike the chromeless panel.
            titlebar: Some(TitlebarOptions {
                title: Some(SharedString::from(i18n::tr("tabs.config"))),
                ..Default::default()
            }),
            ..Default::default()
        }),
        |window, cx| {
            window.activate_window();
            let view = cx.new(|cx| SettingsWindow::new(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        },
    );
    if let Ok(handle) = opened {
        cx.global::<ZStatsGlobalStore>()
            .clone()
            .update(cx, |state, _| state.set_settings_window(handle.into()));
    }
}

/// Create the main window. With a tray `anchor` it opens under the tray icon;
/// without one (startup, or the tray menu's "Show Window") it restores the
/// last known position.
pub fn open_main_window(cx: &mut App, anchor: Option<TrayAnchor>) {
    let saved = cx.global::<ZStatsGlobalStore>().read(cx).window_bounds();
    let default_size = {
        let (w, h) = DEFAULT_WINDOW_SIZE;
        size(px(w), px(h))
    };
    let bounds = match anchor {
        // Keep whatever size the user last left the window at.
        Some(anchor) => {
            let window_size = saved.map_or(default_size, |b| b.size);
            bounds_below_tray(anchor, window_size, cx)
        }
        None => saved.unwrap_or_else(|| Bounds::centered(None, default_size, cx)),
    };
    let (min_w, min_h) = MIN_WINDOW_SIZE;

    let opened = cx.open_window(
        with_app_identity(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(min_w), px(min_h))),
            window_background: WINDOW_BACKGROUND,
            // macOS: `None`, and it has to be written explicitly —
            // `WindowOptions::default().titlebar` is `Some(..)`
            // (`gpui/src/platform.rs:1964`), so leaving the field out would
            // put a default (opaque, traffic-lit) title bar back.
            //
            // With `None`, gpui builds the window with a
            // `Titled | FullSizeContentView` style mask and *without*
            // `Closable`/`Miniaturizable`/`Resizable`, so there are no traffic
            // lights, while it still applies `titlebarAppearsTransparent` +
            // `titleHidden` (`gpui_macos/src/window.rs:815,977`). The result is
            // a clean panel that is nonetheless a normal titled window — it
            // keeps the system rounding and shadow, and can still take keyboard
            // focus, which `WindowKind::PopUp` (a nonactivating panel) could
            // not. Linux keeps server-side decorations with the title from
            // `with_app_identity`.
            #[cfg(target_os = "macos")]
            titlebar: None,
            #[cfg(target_os = "windows")]
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                ..Default::default()
            }),
            // macOS only: create the window hidden and reveal it after the
            // first themed frame (see `on_next_frame` below) so there's no
            // white flash. Windows drives frames from WM_PAINT, which hidden
            // windows never receive — the reveal would deadlock and the window
            // would never appear; Wayland can't reliably reveal a window that
            // was never mapped either.
            show: cfg!(not(target_os = "macos")),
            ..Default::default()
        }),
        |window, cx| {
            // No `on_window_should_close` override: closing really closes, on
            // every platform. `QuitMode::Explicit` keeps the process and the
            // tray alive, and the next tray click rebuilds the window.

            // Pairs with `show: false` above — macOS paints hidden windows, so
            // this fires; on Windows / Linux it never would.
            #[cfg(target_os = "macos")]
            window.on_next_frame(|window, _cx| {
                window.activate_window();
                use_popover_material(window);
                window_ext::join_all_spaces(window);
            });

            let view = cx.new(|cx| ZStatsApp::new(window, cx));
            cx.new(|cx| {
                let root = Root::new(view, window, cx);
                match WINDOW_BACKGROUND {
                    WindowBackgroundAppearance::Opaque => root,
                    // `Root::render` paints an opaque `theme.tokens.background`
                    // across the whole window, which would bury the vibrancy
                    // layer. Its `refine_style` runs after that `bg`, so this
                    // overrides it and lets our own translucent fill be the
                    // only thing between the content and the blur.
                    _ => root.bg(gpui::transparent_black()),
                }
            })
        },
    );
    match opened {
        Ok(_) => cx.global::<metrics::CollectorPace>().shown(),
        Err(e) => eprintln!("failed to open main window: {e}"),
    }
}

/// Left-clicking the tray icon: open the window under the icon, or close it if
/// it's already up.
///
/// Deliberately independent of the focus-loss timing. Two orders are possible
/// and both end up closed:
///   - the click doesn't deactivate the window → it's still open here, so
///     close it outright;
///   - the click deactivates it first → auto-hide already closed it, and
///     `TOGGLE_GRACE` stops this click from immediately reopening it.
pub fn toggle_main_window(cx: &mut App, anchor: TrayAnchor) {
    // First click of the session: nothing to reveal yet.
    let Some(handle) = cx.windows().first().copied() else {
        cx.activate(true);
        open_main_window(cx, Some(anchor));
        return;
    };

    #[cfg(target_os = "macos")]
    {
        let visible = handle
            .update(cx, |_, window, _| window_ext::is_visible(window))
            .unwrap_or(false);
        if visible {
            hide_main_window(cx);
            return;
        }
        // Clicking the icon steals focus first, which auto-hides the panel;
        // the click then arrives to a hidden window. Without this the toggle
        // would read as "it was hidden, so show it" and never close.
        let just_auto_hid = cx
            .global::<ZStatsGlobalStore>()
            .clone()
            .update(cx, |state, _| state.took_recent_auto_hide(TOGGLE_GRACE));
        if just_auto_hid {
            return;
        }
        reveal_main_window(cx, handle, Some(anchor));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

/// Take the panel off screen and drop the collector back to idle.
///
/// Hiding rather than closing: gpui leaks roughly 1 MB per create/destroy
/// cycle (see `window_ext`), and this window is toggled constantly.
pub fn hide_main_window(cx: &mut App) {
    #[cfg(target_os = "macos")]
    if let Some(handle) = cx.windows().first().copied() {
        let _ = handle.update(cx, |_, window, cx| {
            window_ext::hide(window);
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.reset_transient_views(window, cx));
        });
        cx.global::<metrics::CollectorPace>().hidden();
    }
    let _ = cx;
}

/// Position the existing window under the tray icon and bring it forward.
#[cfg(target_os = "macos")]
fn reveal_main_window(cx: &mut App, handle: gpui::AnyWindowHandle, anchor: Option<TrayAnchor>) {
    let current = handle.update(cx, |_, window, _| window.bounds()).ok();
    let origin = match (anchor, current) {
        (Some(anchor), Some(bounds)) => bounds_below_tray(anchor, bounds.size, cx).origin,
        // No anchor (the tray menu's "Show Window"): leave it where it was.
        _ => match current {
            Some(bounds) => bounds.origin,
            None => return,
        },
    };
    cx.activate(true);
    let _ = handle.update(cx, |_, window, _| window_ext::show_at(window, origin));
    cx.global::<metrics::CollectorPace>().shown();
}

/// Banner click, or anything else that wants the Alerts tab in front:
/// pin the tab first so a freshly built window paints it, then show.
pub fn show_alerts_window(cx: &mut App) {
    cx.global::<ZStatsGlobalStore>()
        .clone()
        .update(cx, |state, cx| state.set_tab(state::Tab::Alerts, cx));
    show_main_window(cx);
}

/// The tray menu's "Show Window": always shows, never toggles.
pub fn show_main_window(cx: &mut App) {
    // Drop any pending auto-hide mark so an explicit "show" is never swallowed
    // by the toggle grace period.
    cx.global::<ZStatsGlobalStore>()
        .clone()
        .update(cx, |state, _| state.took_recent_auto_hide(TOGGLE_GRACE));

    match cx.windows().first().copied() {
        #[cfg(target_os = "macos")]
        Some(handle) => reveal_main_window(cx, handle, None),
        #[cfg(not(target_os = "macos"))]
        Some(handle) => {
            cx.activate(true);
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        }
        None => {
            cx.activate(true);
            open_main_window(cx, None);
        }
    }
}

fn main() {
    // Before anything else, and specifically before gpui starts the run loop:
    // this neuters the `setActivationPolicy(Regular)` it would otherwise make
    // during `applicationDidFinishLaunching`.
    #[cfg(target_os = "macos")]
    dock::suppress_regular_policy();

    // `Assets` supplies the SVGs behind `IconName`; without it every icon
    // renders empty.
    let app = gpui_platform::application().with_assets(Assets);

    app.run(|cx| {
        // Still required: a `cargo run` binary has no `LSUIElement`, so
        // LaunchServices already made it `Regular` without going through the
        // swizzled setter.
        #[cfg(target_os = "macos")]
        dock::hide_dock_icon();

        // Must run before touching any gpui-component feature.
        gpui_component::init(cx);
        font::register(cx);
        // Feeds both the theme resolution and the locale pin below, so it
        // has to precede them.
        prefs::load();
        // Pin AppKit before the first frame, so a forced theme's vibrancy
        // material never briefly renders in the system appearance.
        #[cfg(target_os = "macos")]
        apply_ns_appearance();
        // Resolve light/dark against the OS appearance *before* the window
        // opens, so the first painted frame is already themed (otherwise the
        // stock theme shows for a frame and flashes).
        apply_appearance(cx.window_appearance(), cx);
        i18n::init();
        // The tray outlives the window. Without this, gpui's default quits the
        // process as soon as the last window closes on every platform except
        // macOS — taking the tray icon with it.
        cx.set_quit_mode(QuitMode::Explicit);

        let app_state = cx.new(|_| ZStatsAppState::new());
        cx.set_global(ZStatsGlobalStore::new(app_state));

        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.bind_keys([KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-q"
            } else {
                "ctrl-q"
            },
            Quit,
            None,
        )]);
        install_menus(cx);

        #[cfg(not(target_os = "linux"))]
        tray::init_tray(cx);
        notify::start(cx);
        metrics::start(cx);

        // Release builds start with no window at all — the app lives in the
        // tray until the icon is clicked, and deliberately does not
        // `cx.activate`, because launching shouldn't steal focus from
        // whatever the user is doing.
        //
        // Debug builds open the panel immediately instead, so `cargo run`
        // puts the thing being worked on straight on screen.
        #[cfg(debug_assertions)]
        {
            cx.activate(true);
            open_main_window(cx, None);
        }
    });
}
