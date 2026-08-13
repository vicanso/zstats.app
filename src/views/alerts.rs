//! Alerts: conditions that crossed a threshold, newest first.
//!
//! `Tick::alerts` reports the *moment* a threshold is crossed, not a standing
//! list, so what is shown here is accumulated by the store (see
//! `state::SeenAlert`) along with when this process saw it.
//!
//! Clicking a card expands a per-subject threshold editor. Writes go
//! through `zstats::settings::apply_add` so they match the CLI, then the
//! collector reloads `[alerts]` in place.

use super::widgets::{self, card};
use crate::format;
use crate::i18n;
use crate::state::{ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::h_flex;
use gpui_component::{IconName, Sizable};
use rust_i18n::t;
use zstats::settings::{FileConfig, PressureAlert};
use zstats::{AlertEvent, AlertKind, AlertSubject, Severity};

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    if state.alerts().is_empty() {
        return vec![widgets::empty_card(
            i18n::tr("alerts.empty_title"),
            i18n::tr("alerts.empty_body"),
        )];
    }

    let selected = state.selected_alert().cloned();
    let settings = state.settings();

    let mut cards: Vec<AnyElement> = state
        .alerts()
        .iter()
        .enumerate()
        .map(|(i, seen)| {
            let critical = seen.event.severity() == Severity::Critical;
            let line = if critical {
                Hsla::from(theme::accent_wash(45))
            } else {
                Hsla::from(theme::border())
            };
            let target = override_target(&seen.event);
            let expanded = target
                .as_ref()
                .is_some_and(|t| selected.as_ref() == Some(&(t.key.to_string(), t.name.clone())));
            let card_id = SharedString::from(format!("alert-{i}"));

            card()
                .id(card_id)
                .border_color(line)
                .when(critical, |d| d.bg(theme::accent_wash(7)))
                .child(alert_head(i, target.clone(), critical, line, seen.age()))
                .child(alert_title(&seen.event.subject))
                .child(
                    div()
                        .mt(px(3.))
                        .text_size(px(9.5))
                        .text_color(theme::text_muted())
                        // zstats renders the sentence from the same fields it
                        // evaluated, so no numbers are re-derived here.
                        .child(seen.event.summary()),
                )
                .when_some(seen.event.repeat_after, |d, after| {
                    d.child(div().mt(px(8.)).child(widgets::note(
                        t!("alerts.follow_up", when = format::uptime(after.as_secs())).to_string(),
                    )))
                })
                .when(target.is_some() && !expanded, |d| {
                    d.child(
                        div()
                            .mt(px(8.))
                            .child(widgets::note(i18n::tr("alerts.set_hint"))),
                    )
                })
                .when_some(target.filter(|_| expanded), |d, tgt| {
                    d.child(threshold_editor(i, &tgt, settings))
                })
                .into_any_element()
        })
        .collect();

    cards.push(widgets::note(i18n::tr("alerts.footer_note")));
    cards
}

fn alert_head(
    index: usize,
    target: Option<OverrideTarget>,
    critical: bool,
    line: Hsla,
    age: std::time::Duration,
) -> AnyElement {
    let row = h_flex().items_center().justify_between().gap(px(8.));
    row.child(
        div()
            .flex_none()
            .rounded_full()
            .border_1()
            .border_color(line)
            .bg(if critical {
                Hsla::from(theme::accent())
            } else {
                Hsla::from(theme::surface())
            })
            .px(px(8.))
            .py(px(1.5))
            .text_size(px(9.5))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(if critical {
                gpui::white()
            } else {
                Hsla::from(theme::text_muted())
            })
            .child(if critical {
                i18n::tr("alerts.critical")
            } else {
                i18n::tr("alerts.warning")
            }),
    )
    .child(
        h_flex()
            .items_center()
            .gap(px(6.))
            .child(widgets::note(format::ago(age)))
            // An explicit control rather than a clickable card: macOS does
            // not change the pointer over clickable things, so "the whole row
            // does something" has no way to announce itself.
            .children(target.map(|tgt| {
                Button::new(("edit-threshold", index))
                    .icon(IconName::Settings2)
                    .ghost()
                    .xsmall()
                    .tooltip(i18n::tr("alerts.edit_threshold"))
                    .on_click(move |_, _window, cx| {
                        cx.global::<ZStatsGlobalStore>()
                            .clone()
                            .update(cx, |state, cx| state.toggle_alert(tgt.key, &tgt.name, cx));
                    })
            })),
    )
    .into_any_element()
}

fn alert_title(subject: &AlertSubject) -> AnyElement {
    div()
        .mt(px(8.))
        .text_size(px(13.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::text())
        .truncate()
        .child(subject_label(subject))
        .into_any_element()
}

/// Who the alert is about, in the design's compact form.
fn subject_label(subject: &AlertSubject) -> String {
    match subject {
        AlertSubject::Process { pid, name } => {
            t!("alerts.process", name = name.clone(), pid = *pid).to_string()
        }
        AlertSubject::App {
            name,
            process_count,
            ..
        } => t!("alerts.app", name = name.clone(), count = *process_count).to_string(),
        AlertSubject::Volume { mount_point } => {
            t!("alerts.volume", mount = mount_point.clone()).to_string()
        }
        AlertSubject::System => i18n::tr("alerts.system"),
    }
}

/// The `[alerts]` key + override name this event writes when the user
/// picks a new threshold. `name` is empty for the global pressure rule.
#[derive(Clone)]
struct OverrideTarget {
    key: &'static str,
    name: String,
}

fn override_target(event: &AlertEvent) -> Option<OverrideTarget> {
    match (&event.subject, event.kind()) {
        (AlertSubject::Process { name, .. }, AlertKind::Cpu) => Some(OverrideTarget {
            key: "alert-cpu",
            name: name.clone(),
        }),
        (AlertSubject::Process { name, .. }, AlertKind::Memory) => Some(OverrideTarget {
            key: "alert-mem",
            name: name.clone(),
        }),
        (AlertSubject::App { name, .. }, AlertKind::AppCpu) => Some(OverrideTarget {
            key: "alert-app-cpu",
            name: name.clone(),
        }),
        (AlertSubject::App { name, .. }, AlertKind::AppMemory) => Some(OverrideTarget {
            key: "alert-app-mem",
            name: name.clone(),
        }),
        (AlertSubject::Volume { mount_point }, AlertKind::Disk) => Some(OverrideTarget {
            key: "alert-disk",
            name: mount_point.clone(),
        }),
        (AlertSubject::System, AlertKind::Pressure) => Some(OverrideTarget {
            key: "alert-pressure",
            name: String::new(),
        }),
        _ => None,
    }
}

fn threshold_editor(
    index: usize,
    target: &OverrideTarget,
    settings: Option<&FileConfig>,
) -> AnyElement {
    let current = configured_value(settings, target);
    let overridden = is_overridden(settings, target);
    let caption = match (&current, overridden) {
        (Some(v), true) => {
            t!("alerts.override_at", value = display_value(target.key, v)).to_string()
        }
        (Some(v), false) => {
            t!("alerts.default_at", value = display_value(target.key, v)).to_string()
        }
        (None, _) => i18n::tr("alerts.threshold_unknown"),
    };

    div()
        .mt(px(10.))
        .pt(px(8.))
        .border_t(px(1.))
        .border_color(theme::border_subtle())
        .child(widgets::note(caption))
        .child(
            h_flex()
                .mt(px(6.))
                .gap(px(4.))
                .children(presets(target.key).iter().map(|(label, value)| {
                    let on = current.as_deref() == Some(*value);
                    let key = target.key;
                    let name = target.name.clone();
                    let value = (*value).to_string();
                    div()
                        .id(SharedString::from(format!(
                            "th-{index}-{key}-{}-{value}",
                            target.name
                        )))
                        .flex_none()
                        .rounded_full()
                        .border_1()
                        .border_color(if on {
                            Hsla::from(theme::accent_wash(45))
                        } else {
                            Hsla::from(theme::border())
                        })
                        .bg(if on {
                            Hsla::from(theme::accent_wash(10))
                        } else {
                            Hsla::from(theme::inset())
                        })
                        .px(px(8.))
                        .py(px(2.))
                        .text_size(px(10.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(if on {
                            theme::accent_light()
                        } else {
                            theme::text()
                        })
                        .cursor_pointer()
                        .hover(|d| d.bg(theme::surface_raised()))
                        .on_click(move |_, _window, cx| {
                            let key = key;
                            let name = name.clone();
                            let value = value.clone();
                            cx.global::<ZStatsGlobalStore>()
                                .clone()
                                .update(cx, |state, cx| {
                                    if let Err(e) =
                                        state.apply_alert_override(key, &name, &value, cx)
                                    {
                                        eprintln!("alert override failed: {e}");
                                    }
                                });
                        })
                        .child((*label).to_string())
                })),
        )
        .into_any_element()
}

/// Preset chips. Labels are already localised / unit-suffixed; values are
/// what `apply_add` accepts (`0` disables a per-name rule).
fn presets(key: &str) -> Vec<(String, &'static str)> {
    match key {
        "alert-cpu" => vec![
            ("50%".into(), "50"),
            ("80%".into(), "80"),
            ("100%".into(), "100"),
            ("200%".into(), "200"),
            (i18n::tr("alerts.off"), "0"),
        ],
        "alert-mem" => vec![
            ("15%".into(), "15"),
            ("25%".into(), "25"),
            ("40%".into(), "40"),
            ("60%".into(), "60"),
            (i18n::tr("alerts.off"), "0"),
        ],
        "alert-app-cpu" => vec![
            ("200%".into(), "200"),
            ("400%".into(), "400"),
            ("800%".into(), "800"),
            (i18n::tr("alerts.off"), "0"),
        ],
        "alert-app-mem" => vec![
            ("30%".into(), "30"),
            ("40%".into(), "40"),
            ("60%".into(), "60"),
            (i18n::tr("alerts.off"), "0"),
        ],
        "alert-disk" => vec![
            ("80%".into(), "80"),
            ("90%".into(), "90"),
            ("95%".into(), "95"),
            (i18n::tr("alerts.off"), "0"),
        ],
        "alert-pressure" => vec![
            (i18n::tr("alerts.off"), "off"),
            (i18n::tr("alerts.warning"), "warning"),
            (i18n::tr("alerts.critical"), "critical"),
        ],
        _ => Vec::new(),
    }
}

fn configured_value(settings: Option<&FileConfig>, target: &OverrideTarget) -> Option<String> {
    let a = &settings?.alerts;
    match target.key {
        "alert-cpu" => a
            .cpu_overrides
            .get(&target.name)
            .copied()
            .or(a.cpu)
            .or(Some(30.0))
            .map(fmt_pct),
        "alert-mem" => a
            .mem_overrides
            .get(&target.name)
            .copied()
            .or(a.mem)
            .or(Some(25.0))
            .map(fmt_pct_f64),
        "alert-app-cpu" => a
            .app_cpu_overrides
            .get(&target.name)
            .copied()
            .or(a.app_cpu)
            .or(Some(200.0))
            .map(fmt_pct),
        "alert-app-mem" => a
            .app_mem_overrides
            .get(&target.name)
            .copied()
            .or(a.app_mem)
            .or(Some(40.0))
            .map(fmt_pct_f64),
        "alert-disk" => a
            .disk_overrides
            .get(&target.name)
            .copied()
            .or(a.disk)
            .or(Some(90.0))
            .map(fmt_pct),
        "alert-pressure" => Some(
            match a.pressure.unwrap_or(PressureAlert::Warning) {
                PressureAlert::Off => "off",
                PressureAlert::Warning => "warning",
                PressureAlert::Critical => "critical",
            }
            .into(),
        ),
        _ => None,
    }
}

fn is_overridden(settings: Option<&FileConfig>, target: &OverrideTarget) -> bool {
    let Some(a) = settings.map(|s| &s.alerts) else {
        return false;
    };
    match target.key {
        "alert-cpu" => a.cpu_overrides.contains_key(&target.name),
        "alert-mem" => a.mem_overrides.contains_key(&target.name),
        "alert-app-cpu" => a.app_cpu_overrides.contains_key(&target.name),
        "alert-app-mem" => a.app_mem_overrides.contains_key(&target.name),
        "alert-disk" => a.disk_overrides.contains_key(&target.name),
        "alert-pressure" => a.pressure.is_some(),
        _ => false,
    }
}

fn fmt_pct(v: f32) -> String {
    format!("{v:.0}")
}

fn fmt_pct_f64(v: f64) -> String {
    format!("{v:.0}")
}

fn display_value(key: &str, raw: &str) -> String {
    if raw == "0" || raw == "off" {
        return i18n::tr("alerts.off");
    }
    match (key, raw) {
        ("alert-pressure", "warning") => i18n::tr("alerts.warning"),
        ("alert-pressure", "critical") => i18n::tr("alerts.critical"),
        ("alert-pressure", _) => raw.to_string(),
        _ => format!("{raw}%"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use zstats::AlertDetail;

    fn cpu_process(name: &str) -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::Process {
                pid: 1,
                name: name.into(),
            },
            detail: AlertDetail::Cpu {
                avg_percent: 90.0,
                threshold_percent: 30.0,
                window: Duration::from_secs(60),
                runaway: false,
            },
            repeat_after: None,
        }
    }

    #[test]
    fn process_cpu_writes_alert_cpu_override() {
        let t = override_target(&cpu_process("ghostty")).expect("target");
        assert_eq!(t.key, "alert-cpu");
        assert_eq!(t.name, "ghostty");
    }

    #[test]
    fn system_pressure_has_no_per_name_override() {
        let event = AlertEvent {
            subject: AlertSubject::System,
            detail: AlertDetail::Pressure {
                level: 2,
                sustained: Duration::from_secs(60),
                swap_used_bytes: 0,
                swap_total_bytes: 0,
                compressed_bytes: None,
            },
            repeat_after: None,
        };
        let t = override_target(&event).expect("target");
        assert_eq!(t.key, "alert-pressure");
        assert!(t.name.is_empty());
    }
}
