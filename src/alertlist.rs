//! The Alerts tab's rules: where the watchers' cards sit among the live
//! episodes, how a past day folds into rows, what each card says — its
//! subject, its sentence, the one thing to do, the rules armed — where a
//! card's jump goes and whom its Quit asks, and what the threshold editor
//! lights. Every figure is the event's own or zstats' resolution of the
//! file; this words and orders, it decides nothing (the rule engine does).
//! Moved out of the gpui page for the NSH one (phase 4h of
//! docs/nsh-migration.md); the gpui page, `views/alerts.rs`, says why each
//! card looks the way it does.

use crate::alertbars::{BarSource, InForce, Resolved};
use crate::alertlog;
use crate::state::{GrowthNotice, MemoryCreep, SeenAlert, SustainedNotice, ZStatsAppState};
use crate::terminate::{self, QuitMethod};
use crate::{diskwatch, format, i18n, notify, prefs, pressure, trend};
use rust_i18n::t;
use std::time::Duration;
use zstats::alerts::MemoryConsumer;
use zstats::settings::{FileConfig, PressureAlert};
use zstats::{AlertDetail, AlertEvent, AlertKind, AlertSubject, Severity};

// --- Order -----------------------------------------------------------------

/// How long ago the watcher started pointing at this stretch: the hold
/// itself is `after` old by then, but that is the bar, not the news.
pub fn newest_noticed_ago(holdings: &[SustainedNotice], after: Duration) -> Option<Duration> {
    holdings
        .iter()
        .map(|n| n.duration.saturating_sub(after))
        .min()
}

/// Index in the live list (newest first) a watcher card belongs at.
/// A watch noticed more recently than a live report sits above it.
fn watch_insert_at(live_ages: &[Duration], noticed_ago: Duration) -> usize {
    live_ages
        .iter()
        .position(|age| noticed_ago < *age)
        .unwrap_or(live_ages.len())
}

/// Final indices for the watcher cards among the live episodes, one per
/// noticed-ago, which the caller passes sorted ascending (newest
/// first). Each card lands `k` past its own slot because the `k` cards
/// before it are already in the list — and `watch_insert_at` is
/// monotonic in `ago`, so those cards always sit at or before it.
pub fn watch_cards_at(live_ages: &[Duration], noticed_agos: &[Duration]) -> Vec<usize> {
    noticed_agos
        .iter()
        .enumerate()
        .map(|(k, ago)| watch_insert_at(live_ages, *ago) + k)
        .collect()
}

// --- The past week -----------------------------------------------------------

/// Rows a day shows before folding the rest into "+N more": enough to
/// read a busy day, not enough for one day to push the week off the
/// panel.
pub const PAST_ROWS_PER_DAY: usize = 6;

/// A day's episodes with the same subject and kind, as one row: a
/// program that crossed the CPU line twice is one fact with a count, and
/// as two rows its pids (history, by the time anyone reads this) were
/// the only difference. Most recent first, like the episodes.
pub fn past_groups(episodes: &[alertlog::Restored]) -> Vec<Vec<&alertlog::Restored>> {
    let mut groups: Vec<Vec<&alertlog::Restored>> = Vec::new();
    for e in episodes {
        let key = (subject_name(&e.event.subject), e.event.kind());
        match groups
            .iter_mut()
            .find(|g| (subject_name(&g[0].event.subject), g[0].event.kind()) == key)
        {
            Some(group) => group.push(e),
            None => groups.push(vec![e]),
        }
    }
    groups
}

/// A subject without its pid — what makes two episodes the same story.
pub fn subject_name(subject: &AlertSubject) -> String {
    match subject {
        AlertSubject::Process {
            name, display_name, ..
        }
        | AlertSubject::App {
            name, display_name, ..
        } => display_name.as_deref().unwrap_or(name).to_string(),
        other => subject_label(other),
    }
}

/// One row of a past day.
#[derive(Clone, Debug, PartialEq)]
pub struct PastRow {
    /// When its most recent episode was reported
    pub at: String,
    /// Any of its episodes was critical: the dot is the alarm's colour
    pub critical: bool,
    /// The subject, with `×N` when the row stands for several episodes
    pub label: String,
    pub kind: String,
    /// Every one of its cards was waved away
    pub dismissed: bool,
    /// Each time (and pid, for a process) the count stands for; empty for
    /// a single episode, whose time is the row's
    pub times: Vec<String>,
}

/// The row a group of [`past_groups`] reads as.
pub fn past_row(group: &[&alertlog::Restored]) -> PastRow {
    let e = group[0];
    let several = group.len() > 1;
    PastRow {
        at: format::clock(e.at),
        critical: group
            .iter()
            .any(|e| e.event.severity() == Severity::Critical),
        label: if several {
            format!("{} ×{}", subject_name(&e.event.subject), group.len())
        } else {
            subject_label(&e.event.subject)
        },
        kind: i18n::tr(past_kind_key(e.event.kind())),
        dismissed: group.iter().all(|e| e.dismissed),
        times: if several {
            group
                .iter()
                .map(|e| match &e.event.subject {
                    AlertSubject::Process { pid, .. } => format!(
                        "{} · {}",
                        format::clock(e.at),
                        t!("processes.pid_only", pid = pid)
                    ),
                    _ => format::clock(e.at),
                })
                .collect()
        } else {
            Vec::new()
        },
    }
}

/// "1 alert" / "3 alerts" beside a day's heading.
pub fn past_count(count: usize) -> String {
    if count == 1 {
        i18n::tr("alerts.past_count_one")
    } else {
        t!("alerts.past_count", count = count).to_string()
    }
}

/// The severity dot's word, on hover: a dot's colour was the only key to
/// it.
pub fn severity_word(critical: bool) -> String {
    i18n::tr(if critical {
        "alerts.severity_critical"
    } else {
        "alerts.severity_warning"
    })
}

fn past_kind_key(kind: AlertKind) -> &'static str {
    match kind {
        AlertKind::Cpu => "alerts.kind_cpu",
        AlertKind::Memory => "alerts.kind_mem",
        AlertKind::AppCpu => "alerts.kind_app_cpu",
        AlertKind::AppMemory => "alerts.kind_app_mem",
        AlertKind::Disk => "alerts.kind_disk",
        AlertKind::Pressure => "alerts.kind_pressure",
    }
}

// --- A card's words ------------------------------------------------------------

/// The one thing a reader can do about each kind of alert. Keyed on
/// the kind and severity zstats reported and nothing else: this is
/// copy, not a second opinion about the condition. A disk at its
/// critical line gets its own: "have a look when you can" is the wrong
/// sentence under "almost out of space".
pub fn advice_key(event: &AlertEvent) -> &'static str {
    match event.kind() {
        AlertKind::Cpu | AlertKind::AppCpu => "alerts.do_cpu",
        AlertKind::Memory | AlertKind::AppMemory => "alerts.do_mem",
        AlertKind::Disk if event.severity() == Severity::Critical => "alerts.do_disk_critical",
        AlertKind::Disk => "alerts.do_disk",
        AlertKind::Pressure => "alerts.do_pressure",
    }
}

/// When it was reported, and how long it has lasted when it is more than
/// one report.
pub fn when_text(seen: &SeenAlert) -> String {
    match seen.span() {
        Some(span) => t!(
            "alerts.episode_span",
            ago = format::ago(seen.age()),
            span = format::span(span)
        )
        .to_string(),
        None => format::ago(seen.age()),
    }
}

/// When zstats will report a condition that still holds again.
pub fn follow_up(event: &AlertEvent) -> Option<String> {
    let after = event.repeat_after?;
    Some(t!("alerts.follow_up", when = format::span(after)).to_string())
}

/// The alert's sentence, in the reader's language.
///
/// zstats ships `AlertEvent::summary()`, and its own documentation says
/// a frontend with its own layout or language should build from the
/// fields instead — which is exactly what this does. **Nothing is
/// recomputed**: every figure here is read off the event that fired, and
/// deciding that it should fire stayed in the rule engine. Only the
/// wording is ours.
///
/// Two things the English summary carries are deliberately dropped,
/// because this card already shows them better:
///
/// - the subject's name, which is the card's title one line above;
/// - the pressure alert's `top:` list, which the card renders underneath
///   as real rows with a quit button on each.
///
/// The notification banner has its own, terser set of these
/// (`notify_body`, `alerts.notify_*`): a macOS banner gets two lines, a
/// card gets as many as it needs. The wording differs on purpose; the
/// figures cannot drift apart, because both read the same fields off the
/// same event.
pub fn alert_sentence(event: &AlertEvent) -> String {
    let pct = |v: f64| format!("{v:.0}");
    match &event.detail {
        AlertDetail::Cpu {
            avg_percent,
            threshold_percent,
            window,
            runaway,
        } => t!(
            if *runaway {
                "alerts.msg_cpu_runaway"
            } else {
                "alerts.msg_cpu"
            },
            window = format::span(*window),
            avg = pct(*avg_percent),
            threshold = pct(*threshold_percent)
        )
        .to_string(),
        AlertDetail::Memory {
            avg_bytes,
            share_percent,
            threshold_bytes,
            threshold_percent,
            window,
        } => t!(
            "alerts.msg_memory",
            window = format::span(*window),
            used = format::memory(*avg_bytes),
            share = pct(*share_percent),
            // Bytes are the rule's native unit, but the field carries a
            // serde default of zero, so a card restored from a file an
            // older build wrote has none. The percent it was always
            // expressed against is the honest stand-in — "0 B" would be
            // a threshold nothing could cross.
            threshold = if *threshold_bytes > 0 {
                format::memory(*threshold_bytes)
            } else {
                format!("{}%", pct(*threshold_percent))
            }
        )
        .to_string(),
        // zstats 0.7.2 gave the disk rule a second line: a volume that
        // keeps filling is reported again, Critical, once its free space
        // is under `critical_below_bytes`. The critical sentence leads
        // with what is left, as zstats' own summary does — the figure the
        // reader acts on. A warning says where the critical line is, so
        // the second report does not arrive from nowhere. A card restored
        // from a file written before the tier carries 0: no line to name.
        AlertDetail::Disk {
            used_percent,
            available_bytes,
            total_bytes,
            critical_below_bytes,
            ..
        } if available_bytes < critical_below_bytes => t!(
            "alerts.msg_disk_critical",
            used = pct(*used_percent),
            available = format::capacity(*available_bytes),
            total = format::capacity(*total_bytes),
            critical = format::capacity(*critical_below_bytes)
        )
        .to_string(),
        AlertDetail::Disk {
            used_percent,
            threshold_percent,
            available_bytes,
            total_bytes,
            critical_below_bytes,
        } => {
            let mut text = t!(
                "alerts.msg_disk",
                used = pct(*used_percent),
                threshold = pct(*threshold_percent),
                available = format::capacity(*available_bytes),
                total = format::capacity(*total_bytes)
            )
            .to_string();
            if *critical_below_bytes > 0 {
                text.push_str(" · ");
                text.push_str(
                    t!(
                        "alerts.msg_disk_next",
                        critical = format::capacity(*critical_below_bytes)
                    )
                    .as_ref(),
                );
            }
            text
        }
        AlertDetail::Pressure {
            level: _,
            sustained,
            swap_used_bytes,
            swap_total_bytes,
            compressed_bytes,
            swap_ins_per_sec,
            swap_outs_per_sec,
            ..
        } => {
            // The kernel's own word for the level, the same one the
            // Overview card shows — not a number, because 2 and 4 are
            // labels rather than a scale (zstats says so on the field).
            // Level is the WARNING / CRITICAL pill already. Repeating
            // "Warning" mid-sentence wrapped as a second heading.
            let mut text = t!(
                "alerts.msg_pressure",
                span = format::span(*sustained),
                swap_used = format::memory(*swap_used_bytes),
                swap_total = format::memory(*swap_total_bytes)
            )
            .to_string();
            if let Some(compressed) = compressed_bytes {
                text.push_str(" · ");
                text.push_str(
                    t!("alerts.msg_compressed", size = format::memory(*compressed)).as_ref(),
                );
            }
            // zstats' own summary gained this clause in 0.5.7; this card
            // builds from the fields, so it arrives only if we add it.
            // What it tells the reader: the level says tight, the rate
            // says whether the machine is still fighting when this fired.
            if let Some((outs, ins)) = pressure::swap_moving(*swap_ins_per_sec, *swap_outs_per_sec)
            {
                text.push_str(" · ");
                text.push_str(
                    t!(
                        "alerts.msg_swapping",
                        outs = format::thousands(outs as usize),
                        ins = format::thousands(ins as usize)
                    )
                    .as_ref(),
                );
            }
            text
        }
    }
}

/// Who the alert is about, in the design's compact form.
pub fn subject_label(subject: &AlertSubject) -> String {
    match subject {
        // Display name first (zstats 0.5.3): the card is read by a
        // person, and "Electron" names every stock-packaged Electron
        // app at once. The matchable identity is still `name` — the
        // threshold editor keys on it, deliberately.
        AlertSubject::Process {
            pid,
            name,
            display_name,
        } => t!(
            "alerts.process",
            name = display_name.as_deref().unwrap_or(name),
            pid = *pid
        )
        .to_string(),
        AlertSubject::App {
            name,
            display_name,
            process_count,
            ..
        } => t!(
            "alerts.app",
            name = display_name.as_deref().unwrap_or(name),
            count = *process_count
        )
        .to_string(),
        AlertSubject::Volume { mount_point } => notify::volume_title(mount_point),
        AlertSubject::System => i18n::tr("alerts.system"),
    }
}

/// A sustained hold's second line: how long, at what average.
pub fn sustained_line(notice: &SustainedNotice) -> String {
    t!(
        "alerts.sustained_subtitle",
        duration = format::took(notice.duration),
        cpu = format::pct(notice.cpu_avg as f32)
    )
    .to_string()
}

/// A climbing tree's figures: by how much, to what.
pub fn creep_line(creep: &MemoryCreep) -> String {
    t!(
        "alerts.creep_row",
        delta = format::memory(creep.climb_bytes),
        now = format::memory(creep.now_bytes)
    )
    .to_string()
}

/// A grown directory's figures: by how much, over how many days, to what.
pub fn growth_line(notice: &GrowthNotice) -> String {
    let days = (notice.over.as_secs_f64() / 86_400.0).round().max(1.0) as u64;
    t!(
        "alerts.growth_row",
        grew = format::memory(notice.growth.grew),
        days = days,
        now = format::memory(notice.growth.now)
    )
    .to_string()
}

// --- Watching -------------------------------------------------------------------

/// The rules the engine is armed with right now, resolved through
/// zstats' own [`ActiveThresholds`] (same source as the Config tab, so
/// the two can never disagree). One pair per rule — a single joined
/// line wrapped mid-token at 320px (`CPU 30 Memory:`). `None` with no
/// readable config: the empty body already covers the waiting state.
///
/// The third field marks a wide row — a watcher's sentence ("in an hour
/// · ≥2.0 GB") that does not fit half the card; the thresholds are a few
/// characters and go two to a row ([`armed_lines`]). The figure is the
/// last token, so it ends on the same edge as the right-hand column.
///
/// [`ActiveThresholds`]: zstats::alerts::ActiveThresholds
pub fn armed_rows(state: &ZStatsAppState) -> Option<Vec<(String, String, bool)>> {
    let file = state.settings()?;
    let eff = zstats::alerts::ActiveThresholds::from_config(&file.alerts);
    let mut rows = Vec::new();
    if let Some(v) = eff.cpu.base() {
        rows.push((i18n::tr("alerts.kind_cpu"), format!("{v:.0}%"), false));
    }
    // Memory bars are the LOWER of a share and an absolute ceiling, so
    // the percentage alone overstates them: 25% reads as 16 GB on a
    // 64 GiB machine while the rule actually trips at 4 GB. zstats
    // resolves the effective figure itself — ask it rather than paint
    // half the rule. The empty name matches no override, which is what
    // "the base bar" means here.
    let total = state.latest().map(|tick| tick.snapshot.memory.total_bytes);
    match total.and_then(|t| eff.memory_bar_bytes("", t)) {
        Some(bytes) => rows.push((i18n::tr("alerts.kind_mem"), format::memory(bytes), false)),
        // Before the first sample there is no total to resolve against;
        // the share is all that can be said honestly.
        None => {
            if let Some(f) = eff.memory.base() {
                rows.push((
                    i18n::tr("alerts.kind_mem"),
                    format!("{:.0}%", f * 100.0),
                    false,
                ));
            }
        }
    }
    // The whole-app rules, which zstats 0.5.1 made reachable (the app
    // memory bar gained an absolute ceiling; a share of RAM alone was
    // unreachable on the machines browsers actually run on). They are
    // half of what "per-program thresholds" means, so an armed list
    // that omits them understates the watch.
    if let Some(v) = eff.app_cpu.base() {
        rows.push((i18n::tr("alerts.kind_app_cpu"), format!("{v:.0}%"), false));
    }
    if let Some(bytes) = total.and_then(|t| eff.app_memory_bar_bytes("", t)) {
        rows.push((
            i18n::tr("alerts.kind_app_mem"),
            format::memory(bytes),
            false,
        ));
    }
    if let Some(f) = eff.disk.base() {
        rows.push((
            i18n::tr("alerts.kind_disk"),
            format!("{:.0}%", f64::from(f) * 100.0),
            false,
        ));
        // The second line (zstats 0.7.2), for the startup disk: free
        // space under which it is reported again, critical. zstats
        // resolves it against the volume's size — half of what 90%
        // leaves is 13 GB on a 256 GB disk and 100 GB on a 2 TB one,
        // which the absolute floor then caps — so it is asked rather
        // than worked out here, and there is nothing to say before the
        // first sample lists the volume.
        let startup = state
            .latest()
            .and_then(|tick| tick.snapshot.disks.as_deref())
            .and_then(|disks| disks.iter().find(|d| d.mount_point == "/"));
        if let Some(below) = startup
            .map(|d| eff.disk_critical_below_bytes(f, d.total_bytes))
            .filter(|below| *below > 0)
        {
            rows.push((
                i18n::tr("alerts.kind_disk_critical"),
                format!("< {}", format::capacity(below)),
                false,
            ));
        }
    }
    rows.push((
        i18n::tr("alerts.kind_sustained"),
        t!(
            "alerts.watch_sustained",
            cpu = format!("{:.0}%", state.sustained_bar_percent()),
            after = format::span(state.sustained_rule().after)
        )
        .to_string(),
        true,
    ));
    // The other watcher: a list that names one and not the other would
    // make the creep card (and its banner) look like they came from
    // nowhere.
    rows.push((
        i18n::tr("alerts.kind_creep"),
        t!(
            "alerts.watch_creep",
            delta = format::memory(trend::creep_notify_bytes(total.unwrap_or(0)))
        )
        .to_string(),
        true,
    ));
    // The daily disk check is the other watcher with a banner; listed
    // while it is on, for the creep row's reason.
    if prefs::disk_watch() {
        rows.push((
            i18n::tr("alerts.kind_growth"),
            t!(
                "alerts.watch_growth",
                delta = format::memory(diskwatch::NOTIFY_BYTES)
            )
            .to_string(),
            true,
        ));
    }
    rows.push((
        i18n::tr("alerts.empty_cooldown"),
        format::humanize(eff.cooldown),
        false,
    ));
    // The master switch, said where the watching is listed: a banner
    // that quietly never arrives reads as a rule that stopped firing —
    // the same reason every filtered banner logs its verdict.
    if !prefs::notifications() {
        rows.push((
            i18n::tr("alerts.armed_notifications"),
            i18n::tr("alerts.armed_muted"),
            true,
        ));
    }
    Some(rows)
}

/// [`armed_rows`] laid out: the short thresholds two to a row, then each
/// watcher's sentence on a row of its own — nine single rows were a third
/// of the tab for a reference list. Each line says whether it is a half
/// row (thresholds, whose single cell on an odd count keeps half the
/// width) or a wide one.
pub fn armed_lines(rows: Vec<(String, String, bool)>) -> Vec<(Vec<(String, String)>, bool)> {
    let (short, wide): (Vec<_>, Vec<_>) = rows.into_iter().partition(|(_, _, wide)| !wide);
    let mut lines: Vec<(Vec<(String, String)>, bool)> = short
        .chunks(2)
        .map(|pair| {
            (
                pair.iter()
                    .map(|(k, v, _)| (k.clone(), v.clone()))
                    .collect(),
                true,
            )
        })
        .collect();
    lines.extend(wide.into_iter().map(|(k, v, _)| (vec![(k, v)], false)));
    lines
}

/// The ⓘ beside "Watching": what this list is, and that banners are off
/// while the master switch is.
pub fn watching_tip() -> String {
    i18n::tr(if prefs::notifications() {
        "alerts.footer_note"
    } else {
        "alerts.footer_note_muted"
    })
}

// --- Acting on a card ------------------------------------------------------------

/// Where a card's subject lives on the panel. Disk stays ungated: a
/// volume is still that volume after a restart. Process and app jumps
/// need [`SeenAlert::live`] — a restored card's pid may now belong to
/// someone else, the same reason the quit button stays off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goto {
    Hardware,
    Process(u32),
    App(u32),
}

pub fn goto_target(seen: &SeenAlert) -> Option<Goto> {
    match &seen.event.subject {
        AlertSubject::Volume { .. } if matches!(seen.event.kind(), AlertKind::Disk) => {
            Some(Goto::Hardware)
        }
        AlertSubject::Process { pid, .. } if seen.live => Some(Goto::Process(*pid)),
        AlertSubject::App { root_pid, .. } if seen.live => Some(Goto::App(*root_pid)),
        _ => None,
    }
}

impl Goto {
    /// The jump's tooltip.
    pub fn tip(self) -> String {
        i18n::tr(match self {
            Goto::Hardware => "alerts.goto_hardware",
            Goto::Process(_) => "alerts.goto_process",
            Goto::App(_) => "alerts.goto_app",
        })
    }
}

/// Whom a Quit asks to leave: the pid, the name the sheet speaks, and the
/// name delivery checks the pid against (`terminate::request_quit`) — a
/// recycled pid that now holds a different program must not inherit the
/// button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuitSubject {
    pub pid: u32,
    pub display: String,
    pub identity: String,
}

/// The quit button, on memory alerts only. Freeing memory means evicting
/// the holder — unlike CPU spikes, which pass on their own often enough
/// that eviction from an alert card would be premature. The decision that
/// something is over the line stays zstats' (this consumes its event);
/// the click, the confirm sheet and the delivery are `terminate`'s.
///
/// A restored card's pid may belong to something else entirely by now —
/// see [`SeenAlert::live`]: no button rather than a button that could hit
/// the wrong target.
pub fn quit_subject(seen: &SeenAlert) -> Option<QuitSubject> {
    if !seen.live {
        return None;
    }
    let event = &seen.event;
    let (pid, display, identity) = match (&event.subject, event.kind()) {
        (
            AlertSubject::Process {
                pid,
                name,
                display_name,
            },
            AlertKind::Memory,
        ) => (
            *pid,
            display_name.clone().unwrap_or_else(|| name.clone()),
            name.clone(),
        ),
        (
            AlertSubject::App {
                root_pid,
                name,
                display_name,
                ..
            },
            AlertKind::AppMemory,
        ) => (
            *root_pid,
            display_name.clone().unwrap_or_else(|| name.clone()),
            name.clone(),
        ),
        _ => return None,
    };
    QuitSubject {
        pid,
        display,
        identity,
    }
    .offered()
}

/// One of a pressure alert's top consumers, with the same refusable quit
/// the memory cards carry — on a live card only.
pub fn consumer_quit(c: &MemoryConsumer, live: bool) -> Option<QuitSubject> {
    if !live {
        return None;
    }
    QuitSubject {
        pid: c.pid,
        display: consumer_name(c),
        identity: c.name.clone(),
    }
    .offered()
}

/// A consumer as a person knows it — "the half of a pressure alert a
/// person acts on", zstats' own reason for putting the display name on it.
pub fn consumer_name(c: &MemoryConsumer) -> String {
    c.display_name.clone().unwrap_or_else(|| c.name.clone())
}

impl QuitSubject {
    /// No control that could only fail: a subject this user cannot signal
    /// (root-owned, init, ourselves, or already gone) gets no button.
    fn offered(self) -> Option<Self> {
        (terminate::can_term(self.pid) && terminate::can_quit(self.pid)).then_some(self)
    }

    pub fn tip(&self) -> String {
        t!("alerts.quit_tip", name = self.display.clone()).to_string()
    }

    /// The sheet's title and body. Resolved at click time, not render
    /// time: whether the pid still counts as an application can change in
    /// between, and the sheet must describe what will actually be sent.
    /// On Linux there is only the signal tier, and the sheet says so — the
    /// same button, one sentence fewer.
    pub fn sheet(&self) -> (String, String) {
        let name = self.display.clone();
        let body = match terminate::method_for(self.pid) {
            #[cfg(target_os = "macos")]
            QuitMethod::App => t!("alerts.quit_body_app", name = name.clone()),
            QuitMethod::Term => t!("alerts.quit_body_term", name = name.clone()),
        };
        (
            t!("alerts.quit_title", name = name).to_string(),
            body.to_string(),
        )
    }

    /// Asks it to leave, once the sheet was confirmed.
    pub fn request(&self) {
        if !terminate::request_quit(self.pid, &self.identity) {
            tracing::warn!("quit request for pid {} was not delivered", self.pid);
        }
    }
}

// --- Thresholds and quiet hours ----------------------------------------------------

/// The `[alerts]` key + override name this event writes when the user
/// picks a new threshold. `name` is empty for the global pressure rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverrideTarget {
    pub key: &'static str,
    pub name: String,
}

pub fn override_target(event: &AlertEvent) -> Option<OverrideTarget> {
    let (key, name) = match (&event.subject, event.kind()) {
        (AlertSubject::Process { name, .. }, AlertKind::Cpu) => ("alert-cpu", name.clone()),
        (AlertSubject::Process { name, .. }, AlertKind::Memory) => ("alert-mem", name.clone()),
        (AlertSubject::App { name, .. }, AlertKind::AppCpu) => ("alert-app-cpu", name.clone()),
        (AlertSubject::App { name, .. }, AlertKind::AppMemory) => ("alert-app-mem", name.clone()),
        (AlertSubject::Volume { mount_point }, AlertKind::Disk) => {
            ("alert-disk", mount_point.clone())
        }
        (AlertSubject::System, AlertKind::Pressure) => ("alert-pressure", String::new()),
        _ => return None,
    };
    Some(OverrideTarget { key, name })
}

/// Whether the store has this card's editor open.
pub fn editing(state: &ZStatsAppState, target: &OverrideTarget) -> bool {
    state
        .selected_alert()
        .is_some_and(|(k, n)| k.as_str() == target.key && n == &target.name)
}

/// What one target's chips light and which layer its caption names.
pub fn in_force(settings: &FileConfig, target: &OverrideTarget) -> InForce {
    Resolved::of(settings)
        .in_force(target.key, &target.name)
        // Disk and pressure have no table layer and no ceiling: the
        // file is the whole answer.
        .unwrap_or_else(|| InForce {
            lit: configured_value(Some(settings), target),
            source: if is_overridden(Some(settings), target) {
                BarSource::Yours
            } else {
                BarSource::Base
            },
        })
}

/// The editor's caption, and the preset its chips light. The caption
/// names the layer and the chips light its value, both from the one
/// resolution ([`in_force`]) the bar itself comes from.
pub fn threshold_caption(
    settings: Option<&FileConfig>,
    target: &OverrideTarget,
) -> (String, Option<String>) {
    let held = settings.map(|s| in_force(s, target));
    let current = held.as_ref().and_then(|h| h.lit.clone());
    let caption = match &held {
        Some(InForce {
            lit: Some(v),
            source,
        }) => {
            let value = display_value(target.key, v);
            match source {
                BarSource::Yours => t!("alerts.override_at", value = value).to_string(),
                BarSource::Table => t!("alerts.table_at", value = value).to_string(),
                BarSource::Base => t!("alerts.default_at", value = value).to_string(),
            }
        }
        // A global memory bar is the lower of a share and a ceiling,
        // which no single preset stands for.
        Some(_) => i18n::tr("alerts.default_plain"),
        None => i18n::tr("alerts.threshold_unknown"),
    };
    (caption, current)
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

/// The quiet-hours chips: the label and how many hours of banners each
/// holds back.
pub const SNOOZE_CHOICES: [(&str, u64); 2] = [("alerts.snooze_1h", 1), ("alerts.snooze_3h", 3)];

/// The words before the quiet-hours chips: until when, or what they do.
pub fn snooze_caption(until: Option<&str>) -> String {
    match until {
        Some(time) => t!("alerts.snoozed_until", time = time).to_string(),
        None => i18n::tr("alerts.snooze_hint"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cpu_process(name: &str) -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::Process {
                pid: 1,
                name: name.into(),
                display_name: None,
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
    fn a_day_folds_repeats_of_one_subject_and_kind_into_a_row() {
        let at = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let episode = |pid: u32, name: &str| alertlog::Restored {
            event: AlertEvent {
                subject: AlertSubject::Process {
                    pid,
                    name: name.into(),
                    display_name: None,
                },
                ..cpu_process(name)
            },
            first_at: at,
            at,
            reports: 1,
            dismissed: false,
        };
        let day = vec![
            episode(98054, "Telegram"),
            episode(1, "zstats"),
            episode(12931, "Telegram"),
        ];
        let groups = past_groups(&day);
        assert_eq!(groups.len(), 2, "two Telegram episodes, one row");
        assert_eq!(groups[0].len(), 2);
        // Most recent first, as the day's file lists them.
        assert!(matches!(
            groups[0][0].event.subject,
            AlertSubject::Process { pid: 98054, .. }
        ));
        assert_eq!(groups[1].len(), 1);
        let row = past_row(&groups[0]);
        assert_eq!(row.label, "Telegram ×2");
        assert_eq!(row.times.len(), 2, "each time the count stands for");
        assert!(past_row(&groups[1]).times.is_empty());
    }

    fn seen(event: AlertEvent, live: bool) -> SeenAlert {
        SeenAlert::for_test(event, live)
    }

    #[test]
    fn a_live_process_card_jumps_to_that_pid() {
        assert_eq!(
            goto_target(&seen(cpu_process("ghostty"), true)),
            Some(Goto::Process(1))
        );
        assert_eq!(
            goto_target(&seen(cpu_process("ghostty"), false)),
            None,
            "a restored card's pid may now belong to someone else"
        );
    }

    #[test]
    fn a_disk_card_jumps_to_hardware_even_when_restored() {
        let event = AlertEvent {
            subject: AlertSubject::Volume {
                mount_point: "/".into(),
            },
            detail: AlertDetail::Disk {
                used_percent: 99.0,
                threshold_percent: 90.0,
                available_bytes: 0,
                total_bytes: 1 << 30,
                critical_below_bytes: 0,
            },
            repeat_after: None,
        };
        assert_eq!(goto_target(&seen(event, false)), Some(Goto::Hardware));
    }

    #[test]
    fn a_live_app_card_jumps_to_the_tree() {
        let event = AlertEvent {
            subject: AlertSubject::App {
                root_pid: 42,
                name: "Chrome".into(),
                display_name: None,
                process_count: 12,
            },
            detail: AlertDetail::Cpu {
                avg_percent: 200.0,
                threshold_percent: 80.0,
                window: Duration::from_secs(60),
                runaway: false,
            },
            repeat_after: None,
        };
        assert_eq!(goto_target(&seen(event, true)), Some(Goto::App(42)));
    }

    /// A hold that just crossed the two-hour bar is news; a live alert
    /// last reported hours ago is not, even though it is a "real" alert.
    #[test]
    fn a_just_noticed_hold_sits_above_stale_live_alerts() {
        let live = [Duration::from_secs(4 * 3600), Duration::from_secs(9 * 3600)];
        assert_eq!(watch_insert_at(&live, Duration::from_secs(2 * 60)), 0);
    }

    #[test]
    fn a_fresh_live_alert_keeps_the_top() {
        let live = [Duration::from_secs(30), Duration::from_secs(4 * 3600)];
        assert_eq!(watch_insert_at(&live, Duration::from_secs(2 * 60)), 1);
    }

    #[test]
    fn with_no_live_alerts_sustained_is_first() {
        assert_eq!(watch_insert_at(&[], Duration::from_secs(60)), 0);
    }

    #[test]
    fn an_old_hold_falls_through_to_the_bottom_of_live() {
        let live = [Duration::from_secs(60)];
        assert_eq!(watch_insert_at(&live, Duration::from_secs(3 * 3600)), 1);
    }

    /// Two watcher cards land among the live episodes without shoving
    /// each other: the second card's slot is computed as if the first
    /// were already in the list.
    #[test]
    fn two_watch_cards_offset_each_other() {
        let live = [Duration::from_secs(60), Duration::from_secs(10 * 60)];
        // Noticed 30s and 5m ago: one above the fresh alert, one
        // between the alerts — the 5m card shifts by the 30s one.
        let agos = [Duration::from_secs(30), Duration::from_secs(5 * 60)];
        assert_eq!(watch_cards_at(&live, &agos), vec![0, 2]);
        // Both newer than every live report: they take the top two
        // slots in their own order.
        let both_fresh = [Duration::from_secs(10), Duration::from_secs(30)];
        assert_eq!(watch_cards_at(&live, &both_fresh), vec![0, 1]);
        // No live episodes at all: the cards simply stack.
        assert_eq!(watch_cards_at(&[], &agos), vec![0, 1]);
    }

    #[test]
    fn noticed_ago_is_the_hold_minus_the_bar() {
        let n = SustainedNotice {
            pid: 1,
            name: "ghostty".into(),
            cpu_avg: 23.7,
            duration: crate::watch::DEFAULT_SUSTAINED_AFTER + Duration::from_secs(120),
        };
        let after = crate::watch::DEFAULT_SUSTAINED_AFTER;
        assert_eq!(
            newest_noticed_ago(&[n], after),
            Some(Duration::from_secs(120))
        );
        assert_eq!(newest_noticed_ago(&[], after), None);
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
                swap_ins_per_sec: None,
                swap_outs_per_sec: None,
                top_consumers: Vec::new(),
            },
            repeat_after: None,
        };
        let t = override_target(&event).expect("target");
        assert_eq!(t.key, "alert-pressure");
        assert!(t.name.is_empty());
    }

    #[test]
    fn the_thresholds_go_two_to_a_line_and_the_sentences_one() {
        let row = |k: &str, wide: bool| (k.to_string(), "v".to_string(), wide);
        let lines = armed_lines(vec![
            row("cpu", false),
            row("sustained", true),
            row("mem", false),
            row("disk", false),
        ]);
        let shape: Vec<(usize, bool)> = lines.iter().map(|(l, half)| (l.len(), *half)).collect();
        assert_eq!(shape, [(2, true), (1, true), (1, false)]);
        assert_eq!(lines[2].0[0].0, "sustained");
    }
}
