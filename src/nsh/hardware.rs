//! Hardware: a port of the gpui front end's `views/disk.rs`, `drives.rs`,
//! `gpu.rs` and `sensors.rs`, whose comments say why every card, figure
//! and control is the way it is — this file carries the same choices and
//! does not repeat their reasons. The tab's rules are shared
//! (`hardware.rs`), its colour lines too (`hot.rs`).
//!
//! The machine's physical substrate, one card stack: the volumes (is it
//! full), the physical drives under them (is it slow), the GPU, then the
//! temperatures and the battery. Each card binds to a plain value the page
//! gathers from the store as a memo; the lists are kept by their keys
//! (`each`: a volume by its mount point, a drive by its name, a row of
//! sensors by its place), so a tick changes figures, not elements.

use crate::assets::CustomIconName;
use crate::nsh::lists;
use crate::nsh::root::{self, AuxWindow, CARD_GAP, CARD_PAD, HostAction, Live};
use crate::nsh::widgets;
use crate::nsh::{store, theme};
use crate::state::ZStatsAppState;
use crate::{format, hardware, hot, i18n, palette};
use nsh_core::ui::prelude::*;
use rust_i18n::t;
use std::thread;

/// The Hardware page.
pub fn page(live: Live) -> AnyElement {
    let sampled = live.memo(|state: &ZStatsAppState| state.latest().is_some());
    dynamic(move || {
        if !sampled.get() {
            // The volumes' and the sensors' words, as the gpui page had
            // them: the drives and the GPU say nothing until a sample.
            return view()
                .gap(px(CARD_GAP))
                .child(widgets::empty_card(
                    i18n::tr("disk.title"),
                    i18n::tr("common.waiting_sample"),
                ))
                .child(widgets::empty_card(
                    i18n::tr("sensors.title"),
                    i18n::tr("common.waiting_sample"),
                ))
                .into_element();
        }
        view()
            .gap(px(CARD_GAP))
            .child(volumes(live))
            .child(drives(live))
            .child(gpus(live))
            .child(sensors(live))
            .child(battery(live))
            .into_element()
    })
}

/// What the volume cards show.
#[derive(Clone, PartialEq)]
enum Volumes {
    /// Collection is off, or there is no volume to list: a card that says
    /// which.
    Empty {
        title: String,
        body: String,
    },
    Cards(Vec<Volume>),
}

/// One volume's card.
#[derive(Clone, PartialEq)]
struct Volume {
    mount: String,
    title: String,
    free: String,
    used_of: String,
    fraction: f32,
    full: bool,
    rw: String,
    footer: String,
    removable: bool,
    boot: bool,
}

fn volumes_of(state: &ZStatsAppState) -> Volumes {
    let empty = |title: &str, body: &str| Volumes::Empty {
        title: i18n::tr(title),
        body: i18n::tr(body),
    };
    let Some(disks) = state.latest().and_then(|t| t.snapshot.disks.as_deref()) else {
        return empty("disk.off", "disk.off_body");
    };
    // A volume just ejected stays hidden until a snapshot agrees
    // (`ZStatsAppState::mark_ejected`).
    let cards: Vec<Volume> = disks
        .iter()
        .filter(|d| !state.is_ejected(&d.mount_point))
        .map(|d| Volume {
            mount: d.mount_point.clone(),
            title: hardware::volume_title(d),
            free: format::capacity(d.available_bytes),
            used_of: t!(
                "disk.used_of",
                total = format::capacity(d.total_bytes),
                used = format!("{:.0}", d.used_percent)
            )
            .to_string(),
            fraction: d.used_percent / 100.0,
            full: hot::volume_full(d),
            rw: t!(
                "disk.rw",
                read = format::rate(d.read_bytes_per_sec),
                write = format::rate(d.write_bytes_per_sec)
            )
            .to_string(),
            footer: hardware::volume_footer_id(d),
            removable: d.is_removable,
            boot: d.mount_point == "/",
        })
        .collect();
    if cards.is_empty() {
        return empty("disk.no_volumes", "disk.no_volumes_body");
    }
    Volumes::Cards(cards)
}

fn volumes(live: Live) -> AnyElement {
    let v = live.memo(volumes_of);
    let space = live.memo(hardware::space_extra);
    let empty = computed(move || {
        v.with(|v| match v {
            Volumes::Empty { title, body } => Some((title.clone(), body.clone())),
            Volumes::Cards(_) => None,
        })
    });
    dynamic(move || match empty.get() {
        Some((title, body)) => widgets::empty_card(title, body),
        None => view()
            .gap(px(CARD_GAP))
            .child(each(
                move || {
                    v.with(|v| match v {
                        Volumes::Cards(cards) => cards.clone(),
                        Volumes::Empty { .. } => Vec::new(),
                    })
                },
                |volume: &Volume| volume.mount.clone(),
                move |volume, _| volume_card(volume, space),
            ))
            .into_element(),
    })
}

/// One volume: its name, the door to the disk-space window on the boot
/// volume or the eject control on a removable one; free space as the
/// headline beside the used share; the bar; the rates and the volume's
/// technical identity; the boot volume's purgeable space and snapshots.
fn volume_card(volume: ReadSignal<Volume>, space: Memo<Option<String>>) -> AnyElement {
    let boot = move || volume.with(|v| v.boot);
    widgets::card()
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    text(move || volume.with(|v| v.title.clone()))
                        .flex_1()
                        .min_w(px(0.))
                        .number_of_lines(1u32)
                        .text_size(px(12.))
                        .line_height(widgets::line(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::color(palette::text)),
                )
                .child(when(boot, storage_chip))
                .child(when(
                    move || volume.with(|v| v.removable),
                    move || eject_badge(volume.with_untracked(|v| v.mount.clone())),
                )),
        )
        .child(
            view()
                .flex_row()
                .items_baseline()
                .justify_between()
                .gap(px(8.))
                .mt(px(6.))
                .child(widgets::big_figure_with(
                    move || volume.with(|v| v.free.clone()),
                    i18n::tr("disk.free"),
                ))
                .child(
                    text(move || volume.with(|v| v.used_of.clone()))
                        .tabular_nums()
                        .text_size(px(12.))
                        .line_height(widgets::line(12.))
                        .text_color(theme::color(palette::text_muted)),
                ),
        )
        .child(view().mt(px(7.)).child(widgets::meter(
            move || volume.with(|v| v.fraction),
            move || palette::gauge_for(volume.with(|v| v.full)),
            6.,
        )))
        .child(
            view()
                .flex_row()
                .justify_between()
                .mt(px(7.))
                .child(widgets::mono_unless_cjk(
                    text(move || volume.with(|v| v.rw.clone()))
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .text_color(theme::color(palette::text_dim)),
                ))
                .child(
                    text(move || volume.with(|v| v.footer.clone()))
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .text_color(theme::color(palette::text_dim)),
                ),
        )
        .child(when(
            move || boot() && space.with(Option::is_some),
            move || {
                view()
                    .mt(px(6.))
                    .tooltip(widgets::wrap_tooltip(i18n::tr("disk.space_extra_tip")))
                    .child(widgets::note(move || {
                        space.with(|s| s.clone().unwrap_or_default())
                    }))
            },
        ))
        .into_element()
}

/// The door to the disk-space window, on the boot volume's card
/// (`disk::storage_chip`): no trailing ellipsis, the tooltip says where
/// it leads.
fn storage_chip() -> AnyElement {
    view()
        .flex_none()
        .tooltip(widgets::wrap_tooltip(i18n::tr("disk.storage_hint")))
        .child(
            nsh_base::button()
                .rounded_full()
                .border(px(1.))
                .border_color(theme::color(palette::border))
                .bg(theme::color(palette::inset))
                .hover(|s| s.bg(theme::color(palette::surface_raised)))
                .px(px(8.))
                .py(px(2.))
                .on_press(|| root::ask(HostAction::Open(AuxWindow::Storage)))
                .child(
                    text(i18n::tr("disk.storage_open"))
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::color(palette::text)),
                ),
        )
        .into_element()
}

/// Eject, on a removable volume, behind a confirm: the tool runs off the
/// main thread, and on success the card goes at once and the collector
/// samples early (`disk::eject`).
fn eject_badge(mount: String) -> AnyElement {
    view()
        .flex_none()
        .tooltip(widgets::wrap_tooltip(i18n::tr("disk.eject")))
        .child(
            nsh_base::button()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .rounded_full()
                .border(px(1.))
                .border_color(theme::color(palette::border))
                .hover(|s| {
                    s.bg(theme::color(palette::surface_raised))
                        .border_color(theme::color(|| palette::accent_wash(45)))
                })
                .pl(px(7.))
                .pr(px(4.))
                .py(px(1.))
                .on_press(move || {
                    let mount = mount.clone();
                    widgets::confirm(
                        i18n::tr("disk.eject_title"),
                        t!("disk.eject_body", mount = mount.clone()).to_string(),
                        i18n::tr("disk.eject_ok"),
                        move || {
                            thread::spawn(move || {
                                if hardware::eject(&mount) {
                                    store::update(|state, fx| state.mark_ejected(mount, fx));
                                    root::ask(HostAction::Wake);
                                }
                            });
                        },
                    );
                })
                .child(
                    text(i18n::tr("disk.removable"))
                        .text_size(px(10.))
                        .line_height(widgets::line(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::color(palette::text_muted)),
                )
                .child(widgets::icon(
                    CustomIconName::CircleX,
                    11.,
                    palette::text_dim,
                )),
        )
        .into_element()
}

/// What the drives card shows.
#[derive(Clone, PartialEq)]
enum Drives {
    /// No card: no such concept here, the channel is off, or the registry
    /// found no whole disk.
    Hidden,
    /// The first read since the tab opened has not landed.
    Reading,
    Rows(Vec<Drive>),
}

/// One physical drive's row.
#[derive(Clone, PartialEq)]
struct Drive {
    name: String,
    identity: String,
    measuring: bool,
    queue: String,
    read: String,
    write: String,
    errors: Option<String>,
    last: bool,
}

fn drives_of(state: &ZStatsAppState) -> Drives {
    if !hardware::drives_shown(state) {
        return Drives::Hidden;
    }
    match state.latest().and_then(|t| t.snapshot.drives.as_deref()) {
        Some([]) => Drives::Hidden,
        None => Drives::Reading,
        Some(drives) => {
            let last = drives.len().saturating_sub(1);
            Drives::Rows(
                drives
                    .iter()
                    .enumerate()
                    .map(|(i, d)| Drive {
                        name: d.name.clone(),
                        identity: hardware::drive_identity(d),
                        measuring: hardware::drive_measuring(d),
                        queue: t!(
                            "drives.queue",
                            depth = d
                                .queue_depth
                                .map_or(format::PLACEHOLDER.to_string(), |q| format!("{q:.2}"))
                        )
                        .to_string(),
                        read: t!(
                            "drives.read",
                            ops = format::ops(d.read_ops_per_sec),
                            lat = format::millis(d.read_latency_ms)
                        )
                        .to_string(),
                        write: t!(
                            "drives.write",
                            ops = format::ops(d.write_ops_per_sec),
                            lat = format::millis(d.write_latency_ms)
                        )
                        .to_string(),
                        errors: (d.read_errors + d.write_errors > 0).then(|| {
                            t!(
                                "drives.errors",
                                read = d.read_errors,
                                write = d.write_errors
                            )
                            .to_string()
                        }),
                        last: i == last,
                    })
                    .collect(),
            )
        }
    }
}

/// The physical drives, one row each (`drives::render`).
fn drives(live: Live) -> AnyElement {
    let d = live.memo(drives_of);
    let kind = computed(move || {
        d.with(|d| match d {
            Drives::Hidden => 0,
            Drives::Reading => 1,
            Drives::Rows(_) => 2,
        })
    });
    dynamic(move || {
        if kind.get() == 0 {
            return view().into_element();
        }
        let shell = widgets::list_shell().child(widgets::list_header(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .child(widgets::list_title(i18n::tr("drives.title")))
                .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
                    "drives.tip",
                ))))
                .into_element(),
            None,
        ));
        if kind.get() == 1 {
            return shell
                .child(
                    view()
                        .px(px(CARD_PAD))
                        .pb(px(11.))
                        .child(lists_note(i18n::tr("drives.reading"), 11.)),
                )
                .into_element();
        }
        shell
            .child(each(
                move || {
                    d.with(|d| match d {
                        Drives::Rows(rows) => rows.clone(),
                        _ => Vec::new(),
                    })
                },
                |drive: &Drive| drive.name.clone(),
                |drive, _| drive_row(drive),
            ))
            .into_element()
    })
}

/// A dim line of `size`.
fn lists_note(words: String, size: f32) -> Text {
    text(words)
        .text_size(px(size))
        .line_height(widgets::line(size))
        .text_color(theme::color(palette::text_dim))
}

/// Name and model on top with the queue depth pinned right; the two
/// directions underneath; errors only when there are any.
fn drive_row(drive: ReadSignal<Drive>) -> AnyElement {
    let measuring = move || drive.with(|d| d.measuring);
    let small = |words: Box<dyn Fn() -> String>| {
        widgets::mono_unless_cjk(
            text(words)
                .text_size(px(10.))
                .line_height(widgets::line(10.))
                .text_color(theme::color(palette::text_muted)),
        )
    };
    view()
        .px(px(CARD_PAD))
        .py(px(8.))
        .border_b(move || px(if drive.with(|d| d.last) { 0. } else { 1. }))
        .border_color(theme::color(palette::border_subtle))
        .child(
            view()
                .flex_row()
                .items_baseline()
                .justify_between()
                .gap(px(8.))
                .child(
                    view()
                        .flex_row()
                        .flex_1()
                        .min_w(px(0.))
                        .items_baseline()
                        .gap(px(6.))
                        .child(
                            text(move || drive.with(|d| d.name.clone()))
                                .flex_none()
                                .text_size(px(11.))
                                .line_height(widgets::line(11.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::color(palette::text)),
                        )
                        .child(
                            text(move || drive.with(|d| d.identity.clone()))
                                .flex_shrink(1.)
                                .number_of_lines(1u32)
                                .text_size(px(10.))
                                .line_height(widgets::line(10.))
                                .text_color(theme::color(palette::text_dim)),
                        ),
                )
                .child(when(
                    move || !measuring(),
                    move || {
                        view()
                            .flex_none()
                            .tooltip(widgets::wrap_tooltip(i18n::tr("drives.queue_tip")))
                            .child(small(Box::new(move || drive.with(|d| d.queue.clone()))))
                    },
                )),
        )
        .child(when(measuring, || {
            view()
                .mt(px(4.))
                .child(lists_note(i18n::tr("drives.measuring"), 10.))
        }))
        .child(when(
            move || !measuring(),
            move || {
                view()
                    .flex_row()
                    .justify_between()
                    .gap(px(8.))
                    .mt(px(4.))
                    .child(small(Box::new(move || drive.with(|d| d.read.clone()))))
                    .child(small(Box::new(move || drive.with(|d| d.write.clone()))))
            },
        ))
        .child(when(
            move || drive.with(|d| d.errors.is_some()),
            move || {
                text(move || drive.with(|d| d.errors.clone().unwrap_or_default()))
                    .mt(px(4.))
                    .text_size(px(10.))
                    .line_height(widgets::line(10.))
                    .text_color(theme::color(|| palette::text_for(true)))
            },
        ))
        .into_element()
}

/// What the GPU cards show.
#[derive(Clone, PartialEq)]
enum Gpus {
    Hidden,
    Reading,
    Cards(Vec<Gpu>),
}

/// One accelerator's card.
#[derive(Clone, PartialEq)]
struct Gpu {
    name: String,
    busy: String,
    fraction: f32,
    hot: bool,
    labels: Vec<String>,
    values: Vec<String>,
}

fn gpus_of(state: &ZStatsAppState) -> Gpus {
    if !hardware::gpus_shown(state) {
        return Gpus::Hidden;
    }
    match state.latest().and_then(|t| t.snapshot.gpus.as_deref()) {
        Some([]) => Gpus::Hidden,
        None => Gpus::Reading,
        Some(gpus) => Gpus::Cards(
            gpus.iter()
                .map(|g| {
                    let (labels, values) = hardware::gpu_detail_rows(g).into_iter().unzip();
                    Gpu {
                        name: hardware::gpu_name(g),
                        busy: format!("{:.0}", g.utilization_percent),
                        fraction: g.utilization_percent / 100.0,
                        hot: g.utilization_percent >= hot::GPU_HOT,
                        labels,
                        values,
                    }
                })
                .collect(),
        ),
    }
}

/// How busy each accelerator is and how much memory it holds
/// (`gpu::render`).
fn gpus(live: Live) -> AnyElement {
    let g = live.memo(gpus_of);
    let kind = computed(move || {
        g.with(|g| match g {
            Gpus::Hidden => 0,
            Gpus::Reading => 1,
            Gpus::Cards(cards) => 2 + cards.len(),
        })
    });
    dynamic(move || match kind.get() {
        0 => view().into_element(),
        1 => widgets::empty_card(i18n::tr("gpu.title"), i18n::tr("gpu.reading")),
        n => {
            let cards: Vec<AnyElement> = (0..n - 2)
                .map(|i| {
                    let gpu = computed(move || {
                        g.with(|g| match g {
                            Gpus::Cards(cards) => cards.get(i).cloned(),
                            _ => None,
                        })
                    });
                    gpu_card(gpu)
                })
                .collect();
            view().gap(px(CARD_GAP)).children(cards).into_element()
        }
    })
}

fn gpu_card(gpu: Memo<Option<Gpu>>) -> AnyElement {
    let field = move |pick: fn(&Gpu) -> String| {
        move || gpu.with(|g| g.as_ref().map(pick).unwrap_or_default())
    };
    let labels = gpu.with_untracked(|g| g.as_ref().map(|g| g.labels.clone()).unwrap_or_default());
    widgets::card()
        .child(widgets::card_header(
            i18n::tr("gpu.title"),
            Some(
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .min_w(px(0.))
                    .child(
                        text(field(|g| g.name.clone()))
                            .flex_shrink(1.)
                            .number_of_lines(1u32)
                            .text_size(px(10.))
                            .line_height(widgets::line(10.))
                            .text_color(theme::color(palette::text_dim)),
                    )
                    .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
                        "gpu.tip",
                    ))))
                    .into_element(),
            ),
        ))
        .child(
            view()
                .mt(px(8.))
                .child(widgets::big_number(field(|g| g.busy.clone()), "%")),
        )
        .child(view().mt(px(8.)).child(widgets::meter(
            move || gpu.with(|g| g.as_ref().map_or(0., |g| g.fraction)),
            move || palette::gauge_for(gpu.with(|g| g.as_ref().is_some_and(|g| g.hot))),
            6.,
        )))
        .child(lists::kv_columns(labels, move |i| {
            gpu.with(|g| {
                g.as_ref()
                    .and_then(|g| g.values.get(i).cloned())
                    .unwrap_or_default()
            })
        }))
        .into_element()
}

/// One reading of the temperatures grid.
#[derive(Clone, PartialEq)]
struct Cell {
    label: String,
    value: String,
    hot: bool,
    limits: String,
}

/// A row of the grid: two readings, the second absent on an odd count.
#[derive(Clone, PartialEq)]
struct Pair {
    /// Its place in the ranking, which is what keys it: the readings move
    /// through the rows as they warm and cool.
    row: usize,
    left: Cell,
    right: Option<Cell>,
    last: bool,
}

/// What the temperatures card shows.
#[derive(Clone, PartialEq)]
enum Temps {
    /// Off, or ran and found nothing: the card that says which.
    Empty { title: String, body: String },
    List {
        hottest: String,
        hideable: usize,
        showing: bool,
        pairs: Vec<Pair>,
    },
}

fn temps_of(state: &ZStatsAppState) -> Temps {
    let empty = |title: &str, body: &str| Temps::Empty {
        title: i18n::tr(title),
        body: i18n::tr(body),
    };
    match state
        .latest()
        .and_then(|t| t.snapshot.temperatures.as_deref())
    {
        None => empty("sensors.off", "sensors.off_body"),
        Some([]) => empty("sensors.nothing", "sensors.nothing_body"),
        Some(temps) => {
            let showing = state.show_all_sensors();
            let list = hardware::sensors(temps, showing);
            let cells: Vec<Cell> = list
                .rows
                .iter()
                .map(|t| Cell {
                    label: t.label.clone(),
                    value: format!("{:.1} °C", t.celsius),
                    hot: hot::sensor_hot(t.celsius, t.critical_celsius),
                    limits: hardware::limits_label(t),
                })
                .collect();
            let rows = cells.len().div_ceil(2);
            let pairs = cells
                .chunks(2)
                .enumerate()
                .map(|(r, pair)| Pair {
                    row: r,
                    left: pair[0].clone(),
                    right: pair.get(1).cloned(),
                    last: r + 1 == rows,
                })
                .collect();
            Temps::List {
                hottest: t!("sensors.hottest", t = format!("{:.1} °C", list.hottest)).to_string(),
                hideable: list.hideable,
                showing,
                pairs,
            }
        }
    }
}

/// Temperatures, nearest their own limits first, two to a row read left
/// to right (`sensors::render`).
fn sensors(live: Live) -> AnyElement {
    let temps = live.memo(temps_of);
    let empty = computed(move || {
        temps.with(|t| match t {
            Temps::Empty { title, body } => Some((title.clone(), body.clone())),
            Temps::List { .. } => None,
        })
    });
    let list = move |pick: fn(&Temps) -> String| move || temps.with(pick);
    let hideable = computed(move || {
        temps.with(|t| match t {
            Temps::List {
                hideable, showing, ..
            } => (*hideable, *showing),
            Temps::Empty { .. } => (0, false),
        })
    });
    dynamic(move || match empty.get() {
        Some((title, body)) => widgets::empty_card(title, body),
        None => widgets::card()
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .min_w(px(0.))
                            .child(
                                text(i18n::tr("sensors.title"))
                                    .flex_none()
                                    .text_size(px(12.))
                                    .line_height(widgets::line(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::color(palette::text)),
                            )
                            .child(
                                text(list(|t| match t {
                                    Temps::List { hottest, .. } => hottest.clone(),
                                    Temps::Empty { .. } => String::new(),
                                }))
                                .flex_shrink(1.)
                                .number_of_lines(1u32)
                                .text_size(px(10.))
                                .line_height(widgets::line(10.))
                                .text_color(theme::color(palette::text_muted)),
                            )
                            .child(widgets::info_icon(widgets::wrap_tooltip(i18n::tr(
                                "sensors.names_tip",
                            )))),
                    )
                    .child(dynamic(move || {
                        let (hideable, showing) = hideable.get();
                        more_chip(hideable, showing)
                    })),
            )
            .child(view().mt(px(10.)).child(each(
                move || {
                    temps.with(|t| match t {
                        Temps::List { pairs, .. } => pairs.clone(),
                        Temps::Empty { .. } => Vec::new(),
                    })
                },
                |pair: &Pair| pair.row,
                |pair, _| sensor_row(pair),
            )))
            .into_element(),
    })
}

/// One row of the grid.
fn sensor_row(pair: ReadSignal<Pair>) -> AnyElement {
    let last = move || pair.with(|p| p.last);
    view()
        .flex_row()
        .gap(px(14.))
        .child(sensor_cell(
            move || pair.with(|p| Some(p.left.clone())),
            last,
        ))
        .child(sensor_cell(move || pair.with(|p| p.right.clone()), last))
        .into_element()
}

/// One reading: the firmware's raw label and the figure, the figure in
/// the alarm's ink near its own limit; the label, the limits and what
/// they mean on hover. An odd count leaves the right half empty.
fn sensor_cell(
    cell: impl Fn() -> Option<Cell> + Copy + 'static,
    last: impl Fn() -> bool + Copy + 'static,
) -> AnyElement {
    let field =
        move |pick: fn(&Cell) -> String| move || cell().as_ref().map(pick).unwrap_or_default();
    view()
        .flex_1()
        .min_w(px(0.))
        .child(when(
            move || cell().is_some(),
            move || {
                view()
                    .flex_row()
                    .justify_between()
                    .gap(px(8.))
                    .py(px(5.))
                    .border_b(move || px(if last() { 0. } else { 1. }))
                    .border_color(theme::color(palette::border_subtle))
                    .tooltip(widgets::tooltip_lines(move || {
                        cell().map_or_else(Vec::new, |c| {
                            vec![c.label, c.limits, i18n::tr("sensors.limit_tip")]
                        })
                    }))
                    .child(
                        text(field(|c| c.label.clone()))
                            .flex_1()
                            .min_w(px(0.))
                            .number_of_lines(1u32)
                            .text_size(px(11.))
                            .line_height(widgets::line(11.))
                            .text_color(theme::color(palette::text_muted)),
                    )
                    .child(
                        text(field(|c| c.value.clone()))
                            .flex_none()
                            .tabular_nums()
                            .text_size(px(11.))
                            .line_height(widgets::line(11.))
                            .text_color(move || {
                                theme::color(move || {
                                    palette::text_for(cell().is_some_and(|c| c.hot))
                                })
                            }),
                    )
            },
        ))
        .into_element()
}

/// "Show N more" / "Show fewer", or the note that the list is ranked when
/// nothing is hidden (`sensors::more_chip`).
fn more_chip(hideable: usize, showing: bool) -> AnyElement {
    if hideable == 0 {
        return widgets::note(i18n::tr("sensors.hottest_first")).into_element();
    }
    let label = if showing {
        i18n::tr("sensors.hide_more")
    } else {
        t!("sensors.show_more", count = hideable).to_string()
    };
    widgets::more_chip(label, showing, || {
        store::update(|state, fx| state.toggle_all_sensors(fx))
    })
}

/// What the battery card shows: `None` without a battery.
#[derive(Clone, PartialEq)]
struct Battery {
    status: Option<String>,
    labels: Vec<String>,
    values: Vec<String>,
}

fn battery_of(state: &ZStatsAppState) -> Option<Battery> {
    let b = state.latest()?.snapshot.battery.as_ref()?;
    let (labels, values) = hardware::battery_cells(b).into_iter().unzip();
    Some(Battery {
        status: hardware::battery_status(b),
        labels,
        values,
    })
}

/// The battery: what it is doing in the header, its facts two-up.
fn battery(live: Live) -> AnyElement {
    let b = live.memo(battery_of);
    // The grid is made again only when its labels change (the power
    // cell's, between charging and draw); the figures are bound.
    let labels = computed(move || b.with(|b| b.as_ref().map(|b| b.labels.clone())));
    dynamic(move || match labels.get() {
        None => widgets::empty_card(i18n::tr("sensors.battery"), i18n::tr("sensors.no_battery")),
        Some(labels) => widgets::card()
            .pb(px(8.))
            .child(widgets::card_header(
                i18n::tr("sensors.battery"),
                Some(
                    widgets::note(move || {
                        b.with(|b| {
                            b.as_ref()
                                .and_then(|b| b.status.clone())
                                .unwrap_or_default()
                        })
                    })
                    .into_element(),
                ),
            ))
            .child(lists::kv_columns(labels, move |i| {
                b.with(|b| {
                    b.as_ref()
                        .and_then(|b| b.values.get(i).cloned())
                        .unwrap_or_default()
                })
            }))
            .into_element(),
    })
}
