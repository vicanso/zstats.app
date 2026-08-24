// Release builds detach from the console on Windows — otherwise launching the
// app pops an empty terminal behind the window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// macOS only, and it fails here rather than six files later. `procscan`,
// `terminate` and `window_ext` are declared `#[cfg(target_os = "macos")]`
// below while six modules import them unconditionally, so another target
// produces a scatter of unresolved-import errors that say nothing about
// why. This says it once.
//
// The gates are deliberately NOT spread across those six files: a build
// that compiles with no tray, no window show/hide, no abnormal-process
// scan and a permanently failing Trash button would be a worse answer
// than a build that stops. Adding them belongs to a port, where each
// gated feature gets a real implementation or an honest empty state —
// not to scaffolding kept warm for a port nobody has committed to. See
// "非 macOS 平台编译不过" in docs/design.md for the full list.
#[cfg(not(target_os = "macos"))]
compile_error!(
    "zstats-app builds on macOS only. See the platform section in docs/design.md \
     — porting means implementing the tray, window show/hide, positioning and \
     process scanning, not just satisfying the compiler."
);

mod about;
mod alertlog;
mod alerttpl;
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
mod logger;
mod metrics;
mod notify;
mod opener;
mod placement;
mod prefs;
#[cfg(target_os = "macos")]
mod procscan;
mod proxy;
mod spaceinfo;
mod state;
#[cfg(target_os = "macos")]
mod terminate;
mod theme;
#[cfg(not(target_os = "linux"))]
mod tray;
mod trend;
mod updater;
mod views;
mod watch;
#[cfg(target_os = "macos")]
mod window_ext;

use crate::assets::Assets;
use crate::placement::{DEFAULT_WINDOW_SIZE, MIN_WINDOW_SIZE, bounds_below_tray};
use crate::state::{TrayAnchor, ZStatsAppState, ZStatsGlobalStore};
use std::time::Duration;

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
/// Dark: one thin wash over the whole window — a tint in our token's
/// hue, not protection. The protection is the popover material
/// (`use_popover_material`): its luminosity clamp keeps the ground dark
/// over any wallpaper — but only down to mid-grey against pure white,
/// so the wash still shares the load: 0.35 keeps the dim captions
/// legible there while a coloured desktop still shines through
/// clearly. 0.55 (the raw-blur era's value) stacked on the material
/// read as fully opaque; 0.20 washed the captions out on white.
const BACKGROUND_OPACITY_DARK: f32 = if cfg!(target_os = "macos") { 0.35 } else { 1.0 };
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

/// Vertical padding inside an auxiliary window's scrolling body (both
/// settings and disk-space use it). Named because `about_card`'s height
/// budget has to subtract exactly it.
const AUX_BODY_PAD: f32 = 14.;

/// Both auxiliary windows open at the same size, deliberately: settings
/// and disk space are two halves of "the app's own windows", and two
/// nearly-equal sizes read as an accident rather than a decision. Still
/// resizable — this is where they open, not where they must stay.
const AUX_WINDOW_SIZE: (f32, f32) = (507., 620.);
/// Floor for both. Has to stay under [`AUX_WINDOW_SIZE`]: a minimum
/// wider than the opening width would silently widen the window.
const AUX_MIN_WINDOW_SIZE: (f32, f32) = (460., 420.);

actions!(zstats, [Quit, CloseWindow]);

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

        let tint = prefs::opacity().unwrap_or(if theme::is_dark() {
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

/// The Interface page's tray picker: persist, then re-face the tray from
/// the store as it stands — the next tick would do it too, but at the idle
/// cadence that is five seconds of a chip that looks ignored.
pub fn set_tray_pref(pref: prefs::TrayPref, cx: &mut App) {
    prefs::set_tray(pref);
    #[cfg(not(target_os = "linux"))]
    {
        let store = cx.global::<ZStatsGlobalStore>().clone();
        store.update(cx, |state, cx| tray::sync(cx, state));
    }
    repaint(cx);
}

/// The Interface page's sustained-duration picker. Persist and repaint;
/// the store builds the rule from prefs on every question, so the next
/// tick already runs with it — no collector rebuild, this watcher is the
/// panel's own. The default chip stores `None`, so the key leaves the
/// file rather than pinning today's default into it.
pub fn set_sustained_after_pref(minutes: u16, cx: &mut App) {
    let default = (watch::DEFAULT_SUSTAINED_AFTER.as_secs() / 60) as u16;
    prefs::set_sustained_after((minutes != default).then_some(minutes));
    repaint(cx);
}

/// The Interface page's sustained-divisor picker — same shape.
pub fn set_sustained_divisor_pref(divisor: u8, cx: &mut App) {
    prefs::set_sustained_divisor((divisor != prefs::DEFAULT_SUSTAINED_DIVISOR).then_some(divisor));
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

/// The Interface page's opacity picker: persist and repaint — the wash
/// reads the pref per frame, so the change is visible immediately.
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
    use objc2_app_kit::{
        NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    };
    use objc2_foundation::NSRect;
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
        // Setting the material on gpui's own blur view is not enough — it
        // is a subclass that pins `Selection` and strips the layer
        // background on every `updateLayer` ("colorless" by design), so
        // any material's adaptive backdrop is removed the moment AppKit
        // repaints. What is left is pure blur: a white wallpaper arrives
        // at full brightness and the dark theme sits on light glass. So a
        // *stock* `NSVisualEffectView` goes in above gpui's (below its
        // Metal content layer): the popover material's luminosity clamp —
        // the reason every system menu stays dark over any wallpaper — is
        // exactly the ground the panel needs, and nothing strips it.
        let mut gpui_blur: *mut AnyObject = std::ptr::null_mut();
        for i in 0..count {
            let view: *mut AnyObject = objc2::msg_send![subviews, objectAtIndex: i];
            let is_ours: bool =
                objc2::msg_send![view, isMemberOfClass: NSVisualEffectView::class()];
            if is_ours {
                // Already installed (the panel window is built once, but
                // being re-entrant here costs nothing).
                return;
            }
            let is_effect_view: bool =
                objc2::msg_send![view, isKindOfClass: NSVisualEffectView::class()];
            if is_effect_view {
                gpui_blur = view;
            }
        }
        let bounds: NSRect = objc2::msg_send![content, bounds];
        let effect: *mut AnyObject = objc2::msg_send![NSVisualEffectView::class(), alloc];
        let effect: *mut AnyObject = objc2::msg_send![effect, initWithFrame: bounds];
        if effect.is_null() {
            return;
        }
        // Popover: the strongest practical clamp of the stock materials
        // on current macOS (HUDWindow was tried and reads *lighter*
        // over a white desktop). The clamp alone leaves pure white at
        // mid-grey, so the wash above still carries part of the load —
        // hence its 0.35, between "tint" and "rescue".
        let _: () = objc2::msg_send![effect, setMaterial: NSVisualEffectMaterial::Popover];
        let _: () =
            objc2::msg_send![effect, setBlendingMode: NSVisualEffectBlendingMode::BehindWindow];
        // Active always: the popover auto-hides on focus loss anyway, so
        // there is no "inactive but visible" state worth a paler ground.
        let _: () = objc2::msg_send![effect, setState: NSVisualEffectState::Active];
        // NSViewWidthSizable | NSViewHeightSizable — track the window.
        let _: () = objc2::msg_send![effect, setAutoresizingMask: 18usize];
        // NSWindowAbove = 1: directly above gpui's blur view, below the
        // Metal layer that draws every element.
        let _: () = objc2::msg_send![
            content,
            addSubview: effect,
            positioned: 1isize,
            relativeTo: gpui_blur
        ];
        let _: *mut AnyObject = objc2::msg_send![effect, autorelease];
    }
}

/// The settings window: a left nav (Config / About) and a scrolling
/// body. A separate window rather than a tab so a settings session is
/// not cut short by the popover auto-hiding on focus loss — and it is a
/// *standard* window (title bar, traffic lights, opaque background), not
/// a second popover. Repaints are driven by observing the store, the
/// same signal the panel's per-tick repaint rides on.
struct SettingsWindow {
    /// Keyboard anchor: gpui dispatches keystrokes along the focus path,
    /// so without a focused node the Escape / cmd-w bindings would never
    /// reach the root's `key_context` at all. Focused at creation; a
    /// click into the proxy field moves focus there and back-arrives
    /// here on the next window activation.
    focus_handle: gpui::FocusHandle,
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
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            focus_handle,
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
        // How tall a section may be before the window itself has to
        // scroll. Only About uses it, to cap its card and let the
        // release notes absorb the difference — see `about_card`.
        // `viewport_size`, not `bounds`: the latter is the window frame
        // in screen space and would hand out the title bar's height as
        // if it were usable, putting the card that much past the bottom.
        let body_height = f32::from(window.viewport_size().height) - AUX_BODY_PAD * 2.;
        let body = gpui_component::v_flex()
            .gap(px(8.))
            .children(views::config::render(
                state,
                section,
                body_height,
                &self.proxy_input,
                self.proxy_valid,
            ));
        div()
            .relative()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("SettingsWindow")
            // Same effect as the title bar's close button; the stored
            // handle fails its next update and a fresh window is built,
            // so no state cleanup belongs here.
            .on_action(cx.listener(|_, _: &CloseWindow, window, _cx| {
                window.remove_window();
            }))
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
                            .py(px(AUX_BODY_PAD))
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
    // The gear's dot led the user into this window; without a second one
    // here the trail stops at the door and they have to guess which
    // section holds the news. Read once, before `cx.listener` borrows.
    let update_pending = cx
        .global::<ZStatsGlobalStore>()
        .read(cx)
        .update_nudge()
        .is_some();
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
                    // Not while About is the open section: the update row
                    // on that page is already saying it, in words.
                    let dot = update_pending && item == SettingsSection::About && !on;
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
                                // Entering About with a silent finding on
                                // file: run a real check — the silent one
                                // does not retain release notes, and the
                                // user is now looking at the one surface
                                // that shows them.
                                if item == views::config::SettingsSection::About {
                                    cx.global::<ZStatsGlobalStore>()
                                        .clone()
                                        .update(cx, |state, cx| state.refresh_update_for_about(cx));
                                }
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
                                        // Takes the slack so the dot sits
                                        // at the row's end; the label is
                                        // still left-aligned inside it, so
                                        // nothing moves when the dot comes
                                        // and goes.
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(12.))
                                        .font_weight(if on {
                                            gpui::FontWeight::MEDIUM
                                        } else {
                                            gpui::FontWeight::NORMAL
                                        })
                                        .text_color(if on { theme::ink() } else { theme::text() })
                                        .child(i18n::tr(item.label_key())),
                                )
                                .when(dot, |row| {
                                    row.child(
                                        div()
                                            .flex_none()
                                            .w(px(6.))
                                            .h(px(6.))
                                            .rounded_full()
                                            .bg(gpui::Hsla::from(theme::accent())),
                                    )
                                }),
                        )
                }),
        )
        .into_any_element()
}

/// The disk-space window's view. Owns nothing but focus and a scroll
/// offset — both features it shows live in the global store, which this
/// observes exactly as the panel does, so a walk's progress lands here
/// with no plumbing of its own.
///
/// Note the observer is unconditional, unlike the panel's: `CollectorPace`
/// gates the panel's repaint because that window is moved off screen
/// rather than destroyed. This one is either open and visible or gone.
struct StorageWindow {
    /// Keyboard anchor, same reason as [`SettingsWindow`]: gpui dispatches
    /// keystrokes along the focus path, so without a focused node the
    /// Escape / cmd-w bindings never reach the root's `key_context`.
    focus_handle: gpui::FocusHandle,
    scroll: ScrollHandle,
    /// Where a directory to leave out of the analysis is typed.
    /// Committed on Enter, never on a keystroke — half a path is a path
    /// that excludes the wrong thing.
    exclude_input: gpui::Entity<gpui_component::input::InputState>,
}

impl StorageWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = cx.global::<ZStatsGlobalStore>().clone();
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        let exclude_input = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(i18n::tr("disk.ana_exclude_placeholder"))
        });
        // `subscribe_in`, not `subscribe`: clearing the field afterwards
        // needs the window, and this is the only place that has one.
        cx.subscribe_in(
            &exclude_input,
            window,
            |this, input, event: &gpui_component::input::InputEvent, window, cx| {
                if !matches!(event, gpui_component::input::InputEvent::PressEnter { .. }) {
                    return;
                }
                let typed = input.read(cx).value().to_string();
                if typed.trim().is_empty() {
                    return;
                }
                let mut list = prefs::analysis_exclude_raw();
                list.push(typed);
                prefs::set_analysis_exclude(&list);
                // The field is a compose box, not a value: with the entry
                // now a chip beside it, text left behind only invites
                // adding the same path twice.
                this.exclude_input
                    .update(cx, |state, cx| state.set_value("", window, cx));
                cx.notify();
            },
        )
        .detach();
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            focus_handle,
            scroll: ScrollHandle::new(),
            exclude_input,
        }
    }
}

impl Render for StorageWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Every trash control in here raises `confirm::ask`, which needs
        // the dialog layer mounted on the window it is raised from.
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        let bg = cx.theme().background;
        let fg = cx.theme().foreground;
        let state = cx.global::<ZStatsGlobalStore>().read(cx);
        let body = gpui_component::v_flex()
            .gap(px(8.))
            .children(views::storage::render(state, &self.exclude_input));
        div()
            .relative()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("StorageWindow")
            // Same effect as the title bar's close button; the stored
            // handle fails its next update and a fresh window is built,
            // so no state cleanup belongs here — least of all cancelling
            // the walk, which outlives every surface by design.
            .on_action(cx.listener(|_, _: &CloseWindow, window, _cx| {
                window.remove_window();
            }))
            .bg(bg)
            .text_color(fg)
            .child(
                div()
                    .id("storage-body")
                    .track_scroll(&self.scroll)
                    .size_full()
                    .overflow_y_scroll()
                    .px(px(16.))
                    .py(px(AUX_BODY_PAD))
                    .child(body),
            )
            .children(dialog_layer)
            .children(notification_layer)
    }
}

fn aux_window_size() -> gpui::Size<gpui::Pixels> {
    let (w, h) = AUX_WINDOW_SIZE;
    size(px(w), px(h))
}

fn aux_min_window_size() -> gpui::Size<gpui::Pixels> {
    let (w, h) = AUX_MIN_WINDOW_SIZE;
    size(px(w), px(h))
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
    let bounds = Bounds::centered(None, aux_window_size(), cx);
    let opened = cx.open_window(
        with_app_identity(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(aux_min_window_size()),
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

/// Open the disk-space window (large files + the directory analyser), or
/// focus the one already open. Same reuse-or-rebuild contract as the
/// settings window, and for the same reason: a minutes-long walk must
/// not die to the popover's auto-hide, and neither must the reading of
/// its result.
///
/// Opens at [`AUX_WINDOW_SIZE`], the same as settings. Even at that width
/// a row has half again the panel's 320 to spend on a path, which is what
/// drove the tables out of the card; anyone reading deep paths all day can
/// drag it wider, and macOS remembers nothing here on purpose — every open
/// starts from the same known-good frame.
pub fn open_storage_window(cx: &mut App) {
    let existing = cx.global::<ZStatsGlobalStore>().read(cx).storage_window();
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    // A fresh window, not a raised one: yesterday's index query is not
    // what someone reopening this is asking about.
    cx.global::<ZStatsGlobalStore>()
        .clone()
        .update(cx, |state, cx| state.reset_storage_views(cx));
    let bounds = Bounds::centered(None, aux_window_size(), cx);
    let opened = cx.open_window(
        with_app_identity(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(aux_min_window_size()),
            titlebar: Some(TitlebarOptions {
                title: Some(SharedString::from(i18n::tr("disk.storage_title"))),
                ..Default::default()
            }),
            ..Default::default()
        }),
        |window, cx| {
            window.activate_window();
            let view = cx.new(|cx| StorageWindow::new(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        },
    );
    if let Ok(handle) = opened {
        cx.global::<ZStatsGlobalStore>()
            .clone()
            .update(cx, |state, _| state.set_storage_window(handle.into()));
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
        Err(e) => tracing::error!("failed to open main window: {e}"),
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
    // First, before the collector thread exists to race it: the log
    // subscriber. The guard flushes the rolling file's worker; dropping
    // it at the end of main is what makes the last lines land.
    let _log_guard = logger::init();

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

        let app_state = cx.new(|_| {
            let mut state = ZStatsAppState::new();
            // Today's alerts survive a restart (alertlog.rs): the panel
            // opens showing what already fired, not an empty list that
            // implies a quiet morning.
            state.restore_alerts();
            state
        });
        cx.set_global(ZStatsGlobalStore::new(app_state));

        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.bind_keys([
            KeyBinding::new(
                if cfg!(target_os = "macos") {
                    "cmd-q"
                } else {
                    "ctrl-q"
                },
                Quit,
                None,
            ),
            // The auxiliary windows are standard windows, so they answer
            // the standard dismissals — context-scoped, so neither key
            // leaks into the panel (whose Escape-free, auto-hide life is
            // deliberate).
            KeyBinding::new("escape", CloseWindow, Some("SettingsWindow")),
            KeyBinding::new("escape", CloseWindow, Some("StorageWindow")),
            KeyBinding::new(
                if cfg!(target_os = "macos") {
                    "cmd-w"
                } else {
                    "ctrl-w"
                },
                CloseWindow,
                Some("SettingsWindow"),
            ),
            KeyBinding::new(
                if cfg!(target_os = "macos") {
                    "cmd-w"
                } else {
                    "ctrl-w"
                },
                CloseWindow,
                Some("StorageWindow"),
            ),
        ]);
        install_menus(cx);

        #[cfg(not(target_os = "linux"))]
        tray::init_tray(cx);
        notify::start(cx);
        metrics::start(cx);

        // The previous update's installer image, if the user never
        // ejected it: a second registration of our bundle id costs the
        // banners (updater.rs explains). Child processes that block for
        // seconds — off the main thread.
        cx.background_executor()
            .spawn(async { updater::sweep_installer_mounts() })
            .detach();

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
