//! Shared UI roots for the desktop lock feature:
//! - [`LockSetup`]: the compact password setup / permission prompt window.
//! - [`LockOverlay`]: the primary full-screen unlock card shown over the
//!   frosted desktop backdrop while locked.

use crate::assets::CustomIconName;
use crate::color::Rgba;
use crate::nsh::root::{self, AuxWindow, HostAction, Live};
use crate::nsh::theme;
use crate::nsh::widgets;
use crate::screenlock::{self, SetupError};
use crate::{format, i18n, palette};
use nsh_base::input;
use nsh_core::prelude::*;
use std::time::{Duration, SystemTime};

/// Dialog root for setting the temporary lock password (or guiding the user
/// to grant macOS Accessibility permission before engaging the lock).
#[component]
pub fn LockSetup() -> impl IntoElement {
    on_key("escape", |_| {
        root::ask(HostAction::Close(AuxWindow::LockSetup))
    });
    root::bind_window_keys(Some(AuxWindow::LockSetup));

    let live = Live::follow(|_| true);
    let granted = live.watch(screenlock::accessibility_granted);
    let start_err = live.watch(screenlock::start_error);

    let password = signal(String::new());
    let confirm = signal(String::new());
    let err = signal(None::<&'static str>);
    let confirm_ref = element_ref();

    #[cfg(target_os = "macos")]
    if !screenlock::accessibility_granted() {
        spawn(async move {
            loop {
                smol::Timer::after(Duration::from_secs(1)).await;
                if password.try_get().is_none() {
                    break;
                }
                if screenlock::refresh_accessibility() {
                    crate::nsh::store::changed();
                    break;
                }
            }
        });
    }

    let close = move || {
        password.set(String::new());
        confirm.set(String::new());
        err.set(None);
        screenlock::clear_start_error();
        root::ask(HostAction::Close(AuxWindow::LockSetup));
    };

    let submit = move || {
        let p = password.get_untracked();
        let c = confirm.get_untracked();
        match screenlock::validate_setup(&p, &c) {
            Ok(()) => {
                err.set(None);
                screenlock::clear_start_error();
                if screenlock::arm(&p) {
                    password.set(String::new());
                    confirm.set(String::new());
                    root::ask(HostAction::EngageScreenLock);
                }
            }
            Err(SetupError::Empty) => err.set(Some("lock.err_empty")),
            Err(SetupError::Mismatch) => err.set(Some("lock.err_mismatch")),
        }
    };

    view()
        .size_full()
        .bg(theme::color(palette::ground))
        .p(px(18.))
        .child(dynamic(move || {
            if !granted.get() {
                permission_view(close).into_element()
            } else {
                setup_view(
                    password,
                    confirm,
                    err,
                    start_err,
                    confirm_ref,
                    close,
                    submit,
                )
                .into_element()
            }
        }))
}

fn permission_view(close: impl Fn() + Copy + 'static) -> View {
    view()
        .size_full()
        .justify_between()
        .child(
            view()
                .gap(px(8.))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .child(widgets::icon(CustomIconName::Lock, 16., palette::ink))
                        .child(
                            text(i18n::tr("lock.perm_needed_title"))
                                .text_size(px(13.))
                                .line_height(widgets::line(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::color(palette::text)),
                        ),
                )
                .child(
                    text(i18n::tr("lock.perm_needed_body"))
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .text_color(theme::color(palette::text_muted)),
                ),
        )
        .child(
            view()
                .flex_row()
                .justify_end()
                .gap(px(8.))
                .child(secondary_button(i18n::tr("common.cancel"), close))
                .child(primary_button(i18n::tr("lock.perm_open"), || {
                    screenlock::request_accessibility();
                })),
        )
}

fn setup_view(
    password: Signal<String>,
    confirm: Signal<String>,
    err: Signal<Option<&'static str>>,
    start_err: Memo<bool>,
    confirm_ref: ElementRef,
    close: impl Fn() + Copy + 'static,
    submit: impl Fn() + Copy + 'static,
) -> View {
    let error_text = move || {
        if let Some(key) = err.get() {
            i18n::tr(key)
        } else if start_err.get() {
            i18n::tr("lock.err_start_failed")
        } else {
            String::new()
        }
    };
    let has_error = move || err.get().is_some() || start_err.get();

    view()
        .size_full()
        .justify_between()
        .child(
            view()
                .gap(px(10.))
                .child(
                    view()
                        .gap(px(4.))
                        .child(
                            view()
                                .flex_row()
                                .items_center()
                                .gap(px(8.))
                                .child(widgets::icon(CustomIconName::Lock, 15., palette::ink))
                                .child(
                                    text(i18n::tr("lock.title"))
                                        .text_size(px(13.))
                                        .line_height(widgets::line(13.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme::color(palette::text)),
                                ),
                        )
                        .child(
                            text(i18n::tr("lock.subtitle"))
                                .text_size(px(11.))
                                .line_height(px(15.))
                                .text_color(theme::color(palette::text_muted)),
                        ),
                )
                .child(
                    view()
                        .gap(px(8.))
                        .child(
                            password_field(
                                i18n::tr("lock.password_placeholder"),
                                password,
                                true,
                                None,
                            )
                            .return_key_type(ReturnKeyType::Next)
                            .submit_behavior(SubmitBehavior::Submit)
                            .on_change(move |t: &str| {
                                password.set(t.to_owned());
                                err.set(None);
                                screenlock::clear_start_error();
                            })
                            .on_submit(move |_: &str| confirm_ref.focus()),
                        )
                        .child(
                            password_field(
                                i18n::tr("lock.confirm_placeholder"),
                                confirm,
                                false,
                                Some(&confirm_ref),
                            )
                            .return_key_type(ReturnKeyType::Go)
                            .on_change(move |t: &str| {
                                confirm.set(t.to_owned());
                                err.set(None);
                                screenlock::clear_start_error();
                            })
                            .on_submit(move |_: &str| submit()),
                        ),
                )
                .child(
                    view()
                        .h(px(16.))
                        .justify_center()
                        .child(when(has_error, move || {
                            text(error_text)
                                .text_size(px(11.))
                                .line_height(widgets::line(11.))
                                .text_color(theme::color(palette::accent_light))
                        })),
                ),
        )
        .child(
            view()
                .flex_row()
                .justify_end()
                .gap(px(8.))
                .child(secondary_button(i18n::tr("common.cancel"), close))
                .child(primary_button(i18n::tr("lock.lock_btn"), submit)),
        )
}

fn password_field(
    placeholder: String,
    value: Signal<String>,
    auto_focus: bool,
    el_ref: Option<&ElementRef>,
) -> Input {
    let mut field = widgets::styled_input(input())
        .h(px(28.))
        .px(px(9.))
        .rounded(px(7.))
        .bg(theme::color(palette::inset))
        .border(px(1.))
        .border_color(theme::color(palette::border))
        .focus(|s| s.border_color(theme::color(palette::focus_ring)))
        .masked(true)
        .auto_focus(auto_focus)
        .value(move || value.get())
        .placeholder(placeholder);
    if let Some(el_ref) = el_ref {
        field = field.element_ref(el_ref);
    }
    field
}

fn secondary_button(label: String, on_press: impl Fn() + 'static) -> AnyElement {
    nsh_base::button()
        .flex_none()
        .h(px(26.))
        .px(px(12.))
        .rounded(px(7.))
        .border(px(1.))
        .border_color(theme::color(palette::border))
        .bg(theme::color(palette::inset))
        .items_center()
        .justify_center()
        .hover(|s| s.bg(theme::color(palette::surface_raised)))
        .transition_colors()
        .duration(120.)
        .active(|s| s.opacity(0.76))
        .focus_visible(widgets::focus_ring)
        .on_press(on_press)
        .child(
            text(label)
                .text_size(px(12.))
                .line_height(px(15.))
                .text_color(theme::color(palette::text)),
        )
        .into_element()
}

fn primary_button(label: String, on_press: impl Fn() + 'static) -> AnyElement {
    nsh_base::button()
        .flex_none()
        .h(px(26.))
        .px(px(12.))
        .rounded(px(7.))
        .bg(theme::color(palette::primary))
        .items_center()
        .justify_center()
        .hover(|s| s.opacity(0.9))
        .transition_colors()
        .duration(120.)
        .active(|s| s.opacity(0.76))
        .focus_visible(widgets::focus_ring)
        .on_press(on_press)
        .child(
            text(label)
                .text_size(px(12.))
                .line_height(px(15.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::color(palette::primary_ink)),
        )
        .into_element()
}

/// Primary screen full-screen lock overlay root.
#[component]
pub fn LockOverlay() -> impl IntoElement {
    let live = Live::follow(|_| true);
    // Clear NSH's opaque backing layer after first frame and on appearance
    // switches so the frosted `NSVisualEffectView` shines through.
    effect(|| {
        let _ = dark();
        root::ask(HostAction::LockSurfaceReady);
    });

    let snap = live.watch(screenlock::snapshot);
    let clock = signal(format::clock(SystemTime::now()));
    spawn(async move {
        loop {
            smol::Timer::after(Duration::from_secs(1)).await;
            let now = format::clock(SystemTime::now());
            if !clock.try_set(now) {
                break;
            }
        }
    });

    let backdrop = if cfg!(target_os = "macos") {
        Rgba {
            r: 0.,
            g: 0.,
            b: 0.,
            a: 0.52,
        }
    } else {
        Rgba {
            r: 10. / 255.,
            g: 12. / 255.,
            b: 16. / 255.,
            a: 0.92,
        }
    };

    view()
        .size_full()
        .items_center()
        .justify_center()
        .bg(theme::color(move || backdrop))
        .child(
            view()
                .w(px(320.))
                .rounded(px(16.))
                .p(px(24.))
                .gap(px(12.))
                .items_center()
                .bg(theme::color(palette::popover))
                .border(px(1.))
                .border_color(theme::color(palette::border))
                .shadow_lg()
                .child(
                    view()
                        .w(px(44.))
                        .h(px(44.))
                        .rounded_full()
                        .bg(theme::color(palette::inset))
                        .items_center()
                        .justify_center()
                        .child(widgets::icon(CustomIconName::Lock, 20., palette::text)),
                )
                .child(
                    text(move || clock.get())
                        .tabular_nums()
                        .text_size(px(36.))
                        .line_height(px(42.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::color(palette::text)),
                )
                .child(
                    text(i18n::tr("lock.overlay_title"))
                        .text_size(px(12.))
                        .line_height(widgets::line(12.))
                        .text_color(theme::color(palette::text_muted)),
                )
                .child(
                    view()
                        .w_full()
                        .h(px(38.))
                        .px(px(12.))
                        .rounded(px(8.))
                        .bg(theme::color(palette::inset))
                        .border(px(1.))
                        .border_color(move || {
                            theme::color(move || {
                                if snap.get().error {
                                    palette::accent()
                                } else {
                                    palette::focus_ring()
                                }
                            })
                        })
                        .items_center()
                        .justify_center()
                        .child(
                            text(move || {
                                let len = snap.get().entered_len;
                                if len == 0 {
                                    i18n::tr("lock.overlay_placeholder")
                                } else {
                                    "•".repeat(len.min(32))
                                }
                            })
                            .number_of_lines(1u32)
                            .text_size(px(11.))
                            .style_when(
                                move || snap.get().entered_len > 0,
                                |s| s.text_size(px(16.)).font_weight(FontWeight::BOLD),
                            )
                            .text_color(move || {
                                theme::color(move || {
                                    if snap.get().entered_len == 0 {
                                        palette::text_dim()
                                    } else {
                                        palette::text()
                                    }
                                })
                            }),
                        ),
                )
                .child(when(
                    move || snap.get().error,
                    || {
                        text(i18n::tr("lock.overlay_wrong"))
                            .text_size(px(11.))
                            .line_height(widgets::line(11.))
                            .text_color(theme::color(palette::accent_light))
                    },
                ))
                .child(
                    text(i18n::tr("lock.overlay_hint"))
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .text_color(theme::color(palette::text_dim)),
                ),
        )
}
