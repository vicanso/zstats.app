//! Alert episodes, banner delivery filters, and the tray-face questions.
//!
//! zstats owns the events; this module only merges them into episodes,
//! persists the day's list, and answers whether the menu bar needs to
//! change face. Kept out of the store file so later alert-list work
//! has one place to land.

use crate::alertlog;
#[cfg(not(target_os = "linux"))]
use crate::tray;
use crate::volflag;
use gpui::Context;
use std::collections::{HashMap, VecDeque};
#[cfg(test)]
use std::time::UNIX_EPOCH;
use std::time::{Duration, Instant, SystemTime};
use zstats::snapshot::SystemSnapshot;
use zstats::{AlertDetail, AlertEvent, AlertKind, AlertSubject, Severity};

use super::{Tab, ZStatsAppState};

/// How many past alerts the Alerts tab can show.
const MAX_ALERTS: usize = 20;

/// Days the Alerts tab's read-only record reaches back — a week, the
/// span "how often did this fire" is usually asked over. The files
/// keep a month (`alertlog::RETENTION_DAYS`); the tab shows the part
/// that fits a glance.
const ALERT_HISTORY_DAYS: u16 = 7;

/// Identity of an alerting episode: who, plus what about them.
///
/// Both halves are needed — one process can be over on CPU and on memory at
/// the same time, and those are two separate stories.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Episode {
    /// By pid, not name: two processes can share a name.
    Process(u32, AlertKind),
    App(u32, AlertKind),
    Volume(String, AlertKind),
    System(AlertKind),
}

/// How far back the auto-quiet rule looks, and how many banners it lets
/// through in that span before it stops interrupting.
///
/// Aimed at a condition that keeps crossing, clearing and crossing again:
/// zstats already spaces the reminders *inside* one episode (the pressure
/// rule backs off 30m/1h/2h/4h), but a flapping subject opens a fresh
/// episode each time and each one is news. Two banners is enough to have
/// said it; a third within the hour is the same sentence again.
///
/// Deliberately per episode, not global: a second, different subject
/// crossing its line is new information and must still arrive.
const NOISY_WINDOW: Duration = Duration::from_secs(3600);
const NOISY_AFTER: usize = 2;

/// How long a memory-class episode must look recovered before Auto
/// puts the tray back on CPU. Same five minutes zstats waits to end a
/// pressure episode (`PRESSURE_REARM` = `SLOW_WINDOW`): a one-sample
/// dip must not flip the icon, and the face turns back when the engine
/// would have cleared that episode, not five minutes after. Process
/// and app memory get the same hold so a leak that just went under
/// its own bar does not flicker the menu bar. The card stays.
const TRAY_RECOVER: Duration = Duration::from_secs(5 * 60);

/// One episode's quiet hours: banners are skipped until the deadline.
struct Snooze {
    until: Instant,
    /// Wall-clock form of `until` ("14:32"), fixed at snooze time — the
    /// deadline does not move, so neither should its label.
    until_label: String,
}

impl Episode {
    fn of(event: &AlertEvent) -> Self {
        let kind = event.kind();
        match &event.subject {
            AlertSubject::Process { pid, .. } => Episode::Process(*pid, kind),
            AlertSubject::App { root_pid, .. } => Episode::App(*root_pid, kind),
            AlertSubject::Volume { mount_point } => Episode::Volume(mount_point.clone(), kind),
            AlertSubject::System => Episode::System(kind),
        }
    }
}

/// A disk alert the user cannot act on is not news: a read-only extra
/// volume (an installer DMG under `/Volumes`) is full by construction.
/// Other kinds, and the boot disk, pass. `statfs` failing is fail-open
/// — see [`volflag`].
pub(super) fn keep_alert(event: &AlertEvent) -> bool {
    let AlertSubject::Volume { mount_point } = &event.subject else {
        return true;
    };
    if event.kind() != AlertKind::Disk {
        return true;
    }
    if !volflag::skips_disk_alert(mount_point) {
        return true;
    }
    tracing::info!(
        kind = ?event.kind(),
        subject = ?event.subject,
        banner = "skipped",
        "disk alert skipped: volume is read-only"
    );
    false
}

/// One alerting episode, with the freshest numbers it has reported.
///
/// [`AlertEvent`] carries no timestamp of its own, and `Tick::alerts` reports
/// the *moment* a threshold is crossed rather than a standing list — so the
/// "currently interesting" list the design shows has to be accumulated here.
///
/// Accumulated by episode, not by event. zstats alerts once on the crossing,
/// once more after 30 minutes if it still holds, then stays quiet until the
/// value falls back and re-arms. Appending a card per event would give the
/// same condition two entries, and a value hovering at the threshold could
/// push everything else out of a 20-slot list on its own.
pub struct SeenAlert {
    /// Stable id for the UI. The deque reorders as episodes resurface, so an
    /// index would silently reassign element state — hover, tooltips, the
    /// expanded editor — to a different card.
    pub seq: u64,
    /// When this episode first crossed. Wall clock, not `Instant`:
    /// these outlive the process now ([`crate::alertlog`]), and a
    /// monotonic clock restarts with the machine.
    pub first_at: SystemTime,
    /// Most recent report within the episode.
    pub at: SystemTime,
    /// How many times zstats has reported it — 1 on the crossing, 2 once the
    /// 30-minute follow-up lands.
    pub reports: u32,
    /// Whether this episode has been reported *in this session*.
    ///
    /// The gate on every acting control the card carries. A card
    /// restored from yesterday names a pid, and after a reboot macOS
    /// hands low pids straight back out — so "quit Google Chrome ·
    /// 923" could deliver SIGTERM to whatever holds 923 now. Nothing in
    /// `terminate::can_quit` catches that: `kill(pid, 0)` answers "may
    /// I signal this pid", never "is this still that program". So the
    /// buttons appear only once a live report has confirmed the pid
    /// during this run — restored cards are records to read, and the
    /// Processes tab still offers a quit for anything actually running.
    pub live: bool,
    /// When this memory-class episode last started looking recovered,
    /// for the tray's Auto face. `None` while it still holds. The card
    /// stays on the Alerts tab; the menu bar goes back to CPU after
    /// [`TRAY_RECOVER`]. Display-layer only — the engine is not asked.
    recovered_since: Option<SystemTime>,
    pub event: AlertEvent,
}

impl SeenAlert {
    /// Time since the most recent report. A clock stepped backwards
    /// (NTP, a manual change) reads as "just now" rather than a
    /// negative age.
    pub fn age(&self) -> Duration {
        self.at.elapsed().unwrap_or_default()
    }

    /// First report to last report, once that differs from [`age`] by
    /// enough to be worth a second timestamp. Not "still happening":
    /// zstats goes quiet after the follow-up, so this span can end
    /// hours before the card is read.
    pub fn span(&self) -> Option<Duration> {
        let span = self.at.duration_since(self.first_at).unwrap_or_default();
        (span >= Duration::from_secs(60)).then_some(span)
    }

    fn recovered_for(&self, now: SystemTime) -> bool {
        let Some(at) = self.recovered_since else {
            return false;
        };
        if self.event.kind() == AlertKind::Disk {
            // Disk used-% is a slow state. Once this tick is under the
            // event's own bar, the face can drop — a volume that just
            // went under 90% does not flap the way kernel pressure does.
            // Memory still waits [`TRAY_RECOVER`].
            return true;
        }
        now.duration_since(at).unwrap_or_default() >= TRAY_RECOVER
    }

    #[cfg(test)]
    pub(crate) fn for_test(event: AlertEvent, live: bool) -> Self {
        Self {
            seq: 1,
            first_at: UNIX_EPOCH,
            at: UNIX_EPOCH,
            reports: 1,
            live,
            recovered_since: None,
            event,
        }
    }
}

fn is_memory_class(kind: AlertKind) -> bool {
    matches!(
        kind,
        AlertKind::Memory | AlertKind::AppMemory | AlertKind::Pressure
    )
}

/// Whether this episode is worth the menu bar changing face.
///
/// Every memory-class episode qualifies except **kernel pressure at
/// the warning tier**, and the exception is about what warning *means*
/// on this platform: a memory-heavy Mac sits at warning as its steady
/// state — zstats says so in the pressure rule's own comment, and
/// makes that tier wait five times as long before reporting for
/// exactly this reason. A face that spends half the day on memory has
/// stopped being a signal, so the tray waits for the kernel's
/// `critical` while the card and the banner still carry the warning.
///
/// The severity is `AlertEvent::severity()`, zstats' own field — the
/// panel is choosing *which verdict deserves the menu bar*, not
/// deciding when memory is a problem, and it reads no raw
/// `pressure_level` to do it. Process and application memory episodes
/// are Warning by construction in zstats (only pressure ≥ 4 and a
/// runaway CPU are Critical), so gating the whole class on severity
/// would have deleted the face's original job: naming the process or
/// tree that is eating the machine.
///
/// An episode that escalated from warning to critical turns the face
/// the moment the worsening is reported (`record_alert` keeps the
/// newest event), and keeps it through a fall back to warning — that
/// tail is still one unrecovered critical episode, and it ends the way
/// every other one does, on `TRAY_RECOVER` of the kernel calling the
/// machine normal again.
fn turns_the_face(event: &AlertEvent) -> bool {
    match event.kind() {
        AlertKind::Pressure => event.severity() == Severity::Critical,
        kind => is_memory_class(kind),
    }
}

/// Whether this memory-class event still holds in `snapshot`.
///
/// `None` if this sample cannot say (no pressure level, process
/// collection off). `false` if the subject is gone or its figure is
/// under the bar the event itself recorded — not a new threshold.
fn memory_event_holds(event: &AlertEvent, snapshot: &SystemSnapshot) -> Option<bool> {
    match &event.detail {
        AlertDetail::Pressure { .. } => {
            // zstats: `level <= 1` is normal. No level → cannot say.
            Some(snapshot.memory.pressure_level? > 1)
        }
        AlertDetail::Memory {
            threshold_bytes,
            threshold_percent,
            ..
        } => {
            let held = match &event.subject {
                AlertSubject::Process { pid, name, .. } => {
                    let processes = snapshot.processes.as_deref()?;
                    let Some(p) = processes.iter().find(|p| p.pid == *pid) else {
                        return Some(false);
                    };
                    if p.name != *name {
                        return Some(false);
                    }
                    p.phys_footprint_bytes.unwrap_or(p.memory_bytes)
                }
                AlertSubject::App { root_pid, name, .. } => {
                    let groups = snapshot.process_groups.as_deref()?;
                    let Some(g) = groups.iter().find(|g| g.root_pid == *root_pid) else {
                        return Some(false);
                    };
                    if g.name != *name {
                        return Some(false);
                    }
                    g.phys_footprint_bytes.unwrap_or(g.memory_bytes)
                }
                _ => return Some(false),
            };
            if *threshold_bytes > 0 {
                Some(held >= *threshold_bytes)
            } else if *threshold_percent > 0.0 && snapshot.memory.total_bytes > 0 {
                let share = held as f64 / snapshot.memory.total_bytes as f64 * 100.0;
                Some(share >= *threshold_percent)
            } else {
                None
            }
        }
        _ => Some(false),
    }
}

/// Whether this disk event still holds in `snapshot`.
///
/// The bar is the event's own `threshold_percent` against zstats'
/// `used_percent` — not a second threshold, same carve-out as
/// [`memory_event_holds`]. A volume that has left the listing, or that
/// this session just ejected, cannot hold: the condition is gone.
fn disk_event_holds(
    event: &AlertEvent,
    snapshot: &SystemSnapshot,
    ejected: &HashMap<String, Instant>,
) -> Option<bool> {
    let AlertDetail::Disk {
        threshold_percent, ..
    } = &event.detail
    else {
        return Some(false);
    };
    let AlertSubject::Volume { mount_point } = &event.subject else {
        return Some(false);
    };
    if ejected.contains_key(mount_point) {
        return Some(false);
    }
    let disks = snapshot.disks.as_deref()?;
    let Some(d) = disks.iter().find(|d| d.mount_point == *mount_point) else {
        return Some(false);
    };
    Some(f64::from(d.used_percent) >= *threshold_percent)
}

/// The store's alert cluster: today's episodes, the week's record, and
/// the delivery-layer filters (snooze / auto-quiet) plus the tray spec.
#[derive(Default)]
pub(crate) struct AlertBook {
    alerts: VecDeque<SeenAlert>,
    dismissed_today: Vec<alertlog::Restored>,
    alert_history: Vec<alertlog::DayLog>,
    tray_alert_unseen: bool,
    selected_alert: Option<(String, String)>,
    banner_sent: HashMap<Episode, Vec<Instant>>,
    snoozed: HashMap<Episode, Snooze>,
    next_seq: u64,
    alert_day_checked_at: Option<Instant>,
}

impl ZStatsAppState {
    /// Fold one alert into the list, merging into its episode if that episode
    /// is already there and moving it back to the front.
    pub(super) fn record_alert(&mut self, event: AlertEvent, now: SystemTime) {
        // A live report is news the tray spec can show. Ingest clears
        // it again if the Alerts tab is already on screen; a follow-up
        // of an episode they already saw re-lights once they look away,
        // the same way it would a banner.
        self.alert_book.tray_alert_unseen = true;
        let episode = Episode::of(&event);
        if let Some(i) = self
            .alert_book
            .alerts
            .iter()
            .position(|seen| Episode::of(&seen.event) == episode)
            && let Some(mut seen) = self.alert_book.alerts.remove(i)
        {
            seen.at = now;
            seen.reports += 1;
            // A live report just named this pid: the card may act again.
            seen.live = true;
            seen.recovered_since = None;
            // Keep the newest reading: the follow-up carries current numbers,
            // and a card showing the crossing value 30 minutes on is stale.
            seen.event = event;
            self.alert_book.alerts.push_front(seen);
            return;
        }

        self.alert_book.next_seq += 1;
        self.alert_book.alerts.push_front(SeenAlert {
            seq: self.alert_book.next_seq,
            first_at: now,
            at: now,
            reports: 1,
            live: true,
            recovered_since: None,
            event,
        });
        while self.alert_book.alerts.len() > MAX_ALERTS {
            self.alert_book.alerts.pop_back();
        }
    }

    /// Fill the list from today's saved episodes. Called once at
    /// startup rather than from `Default` so the startup order stays
    /// visible in `main` — and so tests construct an empty state
    /// instead of inheriting the developer's own alerts.
    pub fn restore_alerts(&mut self) {
        self.adopt_alerts(alertlog::load());
        self.refresh_alert_history();
    }

    /// The restore proper, minus the file read: episodes join the list
    /// with fresh ids from the same counter live ones use, so a later
    /// crossing of the same condition merges into the restored episode
    /// instead of opening a duplicate beside it.
    pub(super) fn adopt_alerts(&mut self, saved: Vec<alertlog::Restored>) {
        for saved in saved {
            self.alert_book.next_seq += 1;
            self.alert_book.alerts.push_back(SeenAlert {
                seq: self.alert_book.next_seq,
                first_at: saved.first_at,
                at: saved.at,
                reports: saved.reports,
                // Read-only until a live report confirms the subject —
                // see [`SeenAlert::live`].
                live: false,
                recovered_since: None,
                event: saved.event,
            });
        }
    }

    /// Drop one episode from the list and the file. Display-layer only,
    /// like the banner snooze: the engine keeps evaluating, and a
    /// condition that still holds re-opens the episode on its next
    /// report. Without this the list has no acknowledgement path at
    /// all — it outlives restarts now, so the tab's alert tint would
    /// otherwise stay lit for the rest of the day.
    pub fn dismiss_alert(&mut self, seq: u64, cx: &mut Context<Self>) {
        if self.drop_alert(seq) {
            self.persist_alerts();
            cx.notify();
        }
    }

    /// The removal proper, minus the file write — `true` when the list
    /// actually changed.
    pub(super) fn drop_alert(&mut self, seq: u64) -> bool {
        let Some(index) = self
            .alert_book
            .alerts
            .iter()
            .position(|seen| seen.seq == seq)
        else {
            return false;
        };
        let Some(seen) = self.alert_book.alerts.remove(index) else {
            return false;
        };
        // Out of the list, into the record: the day's file keeps it
        // with the acknowledgement, so the week still says it fired.
        self.alert_book.dismissed_today.push(alertlog::Restored {
            event: seen.event,
            first_at: seen.first_at,
            at: seen.at,
            reports: seen.reports,
            dismissed: true,
        });
        if self.alert_book.dismissed_today.len() > MAX_ALERTS {
            self.alert_book.dismissed_today.remove(0);
        }
        true
    }

    /// Retire episodes that are no longer today's. The file already
    /// draws this boundary when it loads; a session that runs past
    /// midnight has to draw it too, or "today's alerts" would quietly
    /// mean "since this app started". Throttled — the check is a
    /// calendar conversion, not something to do 30 times a minute.
    pub(super) fn prune_stale_alerts(&mut self) {
        const CHECK_EVERY: Duration = Duration::from_secs(60);
        if self
            .alert_book
            .alert_day_checked_at
            .is_some_and(|at| at.elapsed() < CHECK_EVERY)
        {
            return;
        }
        self.alert_book.alert_day_checked_at = Some(Instant::now());
        if self.retain_today(SystemTime::now()) {
            self.persist_alerts();
        }
    }

    /// Keep only `now`'s episodes — `true` when something was retired.
    /// A clock with no readable calendar (before the epoch) prunes
    /// nothing: dropping the list on a broken clock is worse than
    /// keeping it.
    pub(super) fn retain_today(&mut self, now: SystemTime) -> bool {
        let Some(today) = alertlog::local_date(now) else {
            return false;
        };
        let before = self.alert_book.alerts.len() + self.alert_book.dismissed_today.len();
        self.alert_book
            .alerts
            .retain(|seen| alertlog::local_date(seen.at).as_deref() == Some(today.as_str()));
        self.alert_book
            .dismissed_today
            .retain(|e| alertlog::local_date(e.at).as_deref() == Some(today.as_str()));
        let retired =
            self.alert_book.alerts.len() + self.alert_book.dismissed_today.len() != before;
        if retired {
            // Yesterday is now a past day: its file was written as it
            // happened, so the record only needs re-reading.
            self.refresh_alert_history();
        }
        retired
    }

    pub(super) fn persist_alerts(&self) {
        let episodes: Vec<alertlog::Restored> = self
            .alert_book
            .alerts
            .iter()
            .map(|seen| alertlog::Restored {
                event: seen.event.clone(),
                first_at: seen.first_at,
                at: seen.at,
                reports: seen.reports,
                dismissed: false,
            })
            .chain(
                self.alert_book
                    .dismissed_today
                    .iter()
                    .map(|e| alertlog::Restored {
                        event: e.event.clone(),
                        first_at: e.first_at,
                        at: e.at,
                        reports: e.reports,
                        dismissed: true,
                    }),
            )
            .collect();
        alertlog::save(&episodes);
    }

    /// The past week's record, newest day first, today excluded.
    pub fn alert_history(&self) -> &[alertlog::DayLog] {
        &self.alert_book.alert_history
    }

    /// Re-read the past days' files. A handful of small files, read on
    /// the events that can change what they say — launch, entering the
    /// tab, the day turning — never per frame.
    pub fn refresh_alert_history(&mut self) {
        self.alert_book.alert_history = alertlog::recent(ALERT_HISTORY_DAYS);
    }

    pub fn alerts(&self) -> &VecDeque<SeenAlert> {
        &self.alert_book.alerts
    }

    /// The one question the tray's auto mode asks the store: is memory
    /// what needs attention right now? A memory-class episode (process,
    /// application, or kernel pressure) reported *this session*, not
    /// yet dismissed, and not recovered for [`TRAY_RECOVER`]. Restored
    /// episodes do not count: they are yesterday-shaped records, and
    /// the tray is about now. Dismissing the card still switches back
    /// immediately; a condition that still holds re-opens it on the
    /// next report.
    ///
    /// Recovery is the event's own bar against this tick's numbers —
    /// `threshold_bytes` on the card, `pressure_level > 1` for the
    /// kernel verdict — not a second threshold. Turning *on* still
    /// waits for zstats to report: the raw level flaps, and reading it
    /// to face memory would put that flap on the menu bar. Turning
    /// *off* after five minutes of the same "normal" zstats uses to
    /// end a pressure episode is the clear side of that rule, which
    /// the list never heard.
    pub fn memory_needs_attention(&self) -> bool {
        self.memory_needs_attention_at(SystemTime::now())
    }

    pub(super) fn memory_needs_attention_at(&self, now: SystemTime) -> bool {
        self.alert_book
            .alerts
            .iter()
            .any(|seen| seen.live && turns_the_face(&seen.event) && !seen.recovered_for(now))
    }

    /// Auto's second trigger: a live disk episode from this session,
    /// not yet dismissed, still at or over the event's own used-% bar,
    /// whose volume is still there. Unlike memory, a tick already under
    /// the bar drops the face immediately — disk used-% does not flap.
    /// Restored cards do not count. Memory still wins when both are on
    /// (`tray::face_for`).
    ///
    /// The presence check is what keeps the face honest after an eject
    /// or an unplug: the clock still starts (a replug onto the same path
    /// resets it), but [`Self::disk_face_volume`] has no volume to name,
    /// so without the check the menu bar wore a disk glyph over `—` for
    /// the whole recovery window.
    pub fn disk_needs_attention(&self) -> bool {
        self.disk_needs_attention_at(SystemTime::now())
    }

    pub(super) fn disk_needs_attention_at(&self, now: SystemTime) -> bool {
        self.alert_book.alerts.iter().any(|seen| {
            seen.live
                && seen.event.kind() == AlertKind::Disk
                && !seen.recovered_for(now)
                && match &seen.event.subject {
                    AlertSubject::Volume { mount_point } => self.disk_volume_present(mount_point),
                    _ => false,
                }
        })
    }

    /// Whether a disk episode's volume can still wear the face: not
    /// ejected by us, and listed in this tick's disks. A tick with no
    /// disk list at all is an unknown sample and keeps the episode, the
    /// same rule the recovery clock follows.
    fn disk_volume_present(&self, mount_point: &str) -> bool {
        if self.ejected.contains_key(mount_point) {
            return false;
        }
        match self
            .latest
            .as_ref()
            .and_then(|tick| tick.snapshot.disks.as_deref())
        {
            Some(disks) => disks.iter().any(|d| d.mount_point == mount_point),
            None => true,
        }
    }

    /// The volume Auto's disk face should name: the newest live
    /// unrecovered disk episode that is still in the snapshot and not
    /// ejected. `(available, total, name)` — zstats' own fields, the
    /// same available-not-used% choice the memory face makes.
    pub fn disk_face_volume(&self) -> Option<(u64, u64, &str)> {
        let now = SystemTime::now();
        let disks = self.latest.as_ref()?.snapshot.disks.as_deref()?;
        self.alert_book.alerts.iter().find_map(|seen| {
            if !seen.live || seen.event.kind() != AlertKind::Disk || seen.recovered_for(now) {
                return None;
            }
            let AlertSubject::Volume { mount_point } = &seen.event.subject else {
                return None;
            };
            if self.ejected.contains_key(mount_point) {
                return None;
            }
            let d = disks.iter().find(|d| d.mount_point == *mount_point)?;
            let name = if d.name.trim().is_empty() {
                d.mount_point.as_str()
            } else {
                d.name.as_str()
            };
            Some((d.available_bytes, d.total_bytes, name))
        })
    }

    /// Start or reset each live memory or disk episode's recovery clock
    /// from this tick. Unknown samples (no process table, no pressure
    /// level, no disk list) leave the clock where it was.
    ///
    /// Both transitions are logged, and at INFO rather than DEBUG on
    /// purpose: the question they answer — "the episode looks over,
    /// why is the menu bar still on memory?" — is asked about the
    /// *installed* build, where DEBUG is not being captured. It was
    /// asked once with no record to answer it from, and the honest
    /// reply was a guess about the level flapping. A reset line with
    /// how long the clock had run says which of the two it was.
    /// Transitions only: the arm re-holds every tick the condition
    /// holds, and those would be a line every few seconds saying
    /// nothing changed.
    pub(super) fn note_memory_recovery(&mut self, now: SystemTime) {
        let Some(tick) = self.latest.as_ref() else {
            return;
        };
        let snapshot = &tick.snapshot;
        for seen in &mut self.alert_book.alerts {
            if !seen.live {
                continue;
            }
            let holds = if is_memory_class(seen.event.kind()) {
                memory_event_holds(&seen.event, snapshot)
            } else if seen.event.kind() == AlertKind::Disk {
                disk_event_holds(&seen.event, snapshot, &self.ejected)
            } else {
                continue;
            };
            let disk = seen.event.kind() == AlertKind::Disk;
            match holds {
                Some(true) => {
                    // Only a clock that was actually running is a reset;
                    // the arm holds on every tick the condition holds,
                    // and logging those would be a line every few
                    // seconds saying nothing changed.
                    if let Some(started) = seen.recovered_since.take() {
                        let ran_for = now.duration_since(started).unwrap_or_default();
                        if disk {
                            tracing::info!(
                                kind = ?seen.event.kind(),
                                subject = ?seen.event.subject,
                                ?ran_for,
                                "disk recovery clock reset"
                            );
                        } else {
                            tracing::info!(
                                kind = ?seen.event.kind(),
                                subject = ?seen.event.subject,
                                ?ran_for,
                                "memory recovery clock reset"
                            );
                        }
                    }
                }
                Some(false) if seen.recovered_since.is_none() => {
                    seen.recovered_since = Some(now);
                    if disk {
                        tracing::info!(
                            kind = ?seen.event.kind(),
                            subject = ?seen.event.subject,
                            "disk face recovered: used percent under the bar"
                        );
                    } else {
                        tracing::info!(
                            kind = ?seen.event.kind(),
                            subject = ?seen.event.subject,
                            after = ?TRAY_RECOVER,
                            "memory recovery clock started"
                        );
                    }
                }
                Some(false) | None => {}
            }
        }
    }

    // ---- banner snooze -------------------------------------------------

    /// Quiet this episode's banners for `hours`. Suppression is delivery-
    /// layer only: the engine keeps evaluating and the Alerts list keeps
    /// recording — the interruption is what stops.
    pub fn snooze_banners(&mut self, event: &AlertEvent, hours: u64, cx: &mut Context<Self>) {
        let until_label = jiff::Zoned::now()
            .checked_add(jiff::Span::new().hours(hours as i64))
            .map(|z| z.strftime("%H:%M").to_string())
            .unwrap_or_default();
        self.alert_book.snoozed.insert(
            Episode::of(event),
            Snooze {
                until: Instant::now() + Duration::from_secs(hours * 3600),
                until_label,
            },
        );
        cx.notify();
    }

    pub fn unsnooze_banners(&mut self, event: &AlertEvent, cx: &mut Context<Self>) {
        self.resume_banners(event);
        cx.notify();
    }

    /// The un-mute proper, minus the repaint. "Resume" is unambiguous, so
    /// it clears the auto-quiet too — one left standing would keep the
    /// subject silent behind the user's back.
    pub(super) fn resume_banners(&mut self, event: &AlertEvent) {
        let key = Episode::of(event);
        self.alert_book.snoozed.remove(&key);
        self.alert_book.banner_sent.remove(&key);
    }

    /// Whether this event's banner is muted right now. Runs on every fresh
    /// event, which is also where expired entries get dropped — the map
    /// never outlives its deadlines by more than one alert.
    pub fn banner_snoozed(&mut self, event: &AlertEvent) -> bool {
        let now = Instant::now();
        self.alert_book.snoozed.retain(|_, s| s.until > now);
        self.alert_book.snoozed.contains_key(&Episode::of(event))
    }

    /// Whether this event's banner is being held back because the same
    /// episode has already interrupted [`NOISY_AFTER`] times inside
    /// [`NOISY_WINDOW`]. Delivery-layer only, exactly like the snooze:
    /// the engine keeps evaluating, the list keeps recording and the card
    /// keeps counting reports — what stops is the interruption.
    ///
    /// Records the delivery it permits, so the window slides and the
    /// subject gets its voice back once it quiets down.
    pub fn banner_damped(&mut self, event: &AlertEvent, now: Instant) -> bool {
        let sent = self
            .alert_book
            .banner_sent
            .entry(Episode::of(event))
            .or_default();
        sent.retain(|at| now.duration_since(*at) < NOISY_WINDOW);
        if sent.len() >= NOISY_AFTER {
            return true;
        }
        sent.push(now);
        false
    }

    /// Whether a card should say it has gone auto-quiet. Read-only — the
    /// count is only ever advanced by an actual delivery attempt.
    pub fn banner_auto_quiet(&self, event: &AlertEvent) -> bool {
        let now = Instant::now();
        self.alert_book
            .banner_sent
            .get(&Episode::of(event))
            .is_some_and(|sent| {
                sent.iter()
                    .filter(|at| now.duration_since(**at) < NOISY_WINDOW)
                    .count()
                    >= NOISY_AFTER
            })
    }

    /// The "muted until 14:32" label for a card, if its episode is muted.
    pub fn snoozed_until(&self, event: &AlertEvent) -> Option<&str> {
        let snooze = self.alert_book.snoozed.get(&Episode::of(event))?;
        (snooze.until > Instant::now()).then_some(snooze.until_label.as_str())
    }

    /// The tray corner spec is on: a live report has landed since the
    /// Alerts tab was last on screen.
    pub fn tray_alert_unseen(&self) -> bool {
        self.alert_book.tray_alert_unseen
    }

    /// The Alerts tab is (or is about to be) what the user is looking
    /// at: the spec has done its job. Does not touch the episode list.
    pub fn see_alerts(&mut self) {
        self.alert_book.tray_alert_unseen = false;
    }

    /// Reveal path: opening the panel onto Alerts is the same as
    /// switching to it. Other tabs leave the spec — that is the
    /// reminder to go look.
    pub fn see_alerts_if_showing(&mut self, cx: &mut Context<Self>) {
        if self.tab == Tab::Alerts {
            self.see_alerts();
            #[cfg(not(target_os = "linux"))]
            tray::sync(cx, self);
            // No tray on Linux yet (docs/omarchy-port.md, phase 3), so
            // there is nothing here for the context to do.
            #[cfg(target_os = "linux")]
            let _ = cx;
        }
    }

    pub(super) fn alerts_are_showing(&self, cx: &Context<Self>) -> bool {
        self.tab == Tab::Alerts && super::panel_visible(cx)
    }

    pub fn selected_alert(&self) -> Option<&(String, String)> {
        self.alert_book.selected_alert.as_ref()
    }

    /// Clicking the open card closes it, as with process rows.
    pub fn toggle_alert(&mut self, key: &str, name: &str, cx: &mut Context<Self>) {
        let id = (key.to_string(), name.to_string());
        self.alert_book.selected_alert = if self.alert_book.selected_alert.as_ref() == Some(&id) {
            None
        } else {
            Some(id)
        };
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alertlog;
    use crate::state::ZStatsAppState;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime};
    use zstats::AlertDetail;
    use zstats::snapshot::{ProcessSnapshot, SystemSnapshot};
    use zstats::{AlertEvent, AlertSubject, Severity, Tick};

    fn snap(pid: u32, name: &str) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            name: name.into(),
            display_name: None,
            cmd: String::new(),
            cpu_usage_percent: 0.0,
            cpu_time_ms: 0,
            memory_bytes: 0,
            phys_footprint_bytes: None,
            virtual_memory_bytes: 0,
            run_time_secs: 0,
            parent_pid: None,
            user_id: None,
            status: String::new(),
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    #[test]
    fn snooze_mutes_by_episode_and_expires() {
        let mut state = ZStatsAppState::new();
        let event = cpu_alert(7);

        // Active snooze mutes this episode, and only this episode: the
        // same pid's MEMORY alert is a different story and stays loud.
        state.alert_book.snoozed.insert(
            Episode::of(&event),
            Snooze {
                until: Instant::now() + Duration::from_secs(3600),
                until_label: "14:32".into(),
            },
        );
        assert!(state.banner_snoozed(&event));
        assert!(!state.banner_snoozed(&mem_alert(7)));
        assert_eq!(state.snoozed_until(&event), Some("14:32"));

        // Past the deadline the entry is pruned on the next check.
        state
            .alert_book
            .snoozed
            .get_mut(&Episode::of(&event))
            .unwrap()
            .until = Instant::now() - Duration::from_secs(1);
        assert!(!state.banner_snoozed(&event));
        assert!(
            state.alert_book.snoozed.is_empty(),
            "expired snooze should be pruned"
        );
    }

    #[test]
    fn a_writable_disk_still_alerts_and_statfs_failure_is_fail_open() {
        assert!(super::keep_alert(&cpu_alert(1)));
        assert!(
            super::keep_alert(&disk_alert("/")),
            "the boot volume must still alert"
        );
        assert!(
            super::keep_alert(&disk_alert("/Volumes/no-such-volume")),
            "a mount we cannot inspect is not silently exempted"
        );
    }

    fn disk_alert(mount: &str) -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::Volume {
                mount_point: mount.into(),
            },
            detail: AlertDetail::Disk {
                used_percent: 99.0,
                threshold_percent: 90.0,
                available_bytes: 0,
                total_bytes: 1 << 30,
            },
            repeat_after: None,
        }
    }

    fn cpu_alert(pid: u32) -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::Process {
                pid,
                name: format!("p{pid}"),
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

    fn mem_alert(pid: u32) -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::Process {
                pid,
                name: format!("p{pid}"),
                display_name: None,
            },
            detail: AlertDetail::Memory {
                avg_bytes: 1 << 30,
                share_percent: 40.0,
                threshold_percent: 25.0,
                threshold_bytes: 4 << 30,
                window: Duration::from_secs(60),
            },
            repeat_after: None,
        }
    }

    fn pressure_alert(level: u32) -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::System,
            detail: AlertDetail::Pressure {
                level,
                sustained: Duration::from_secs(300),
                swap_used_bytes: 1 << 30,
                swap_total_bytes: 2 << 30,
                compressed_bytes: None,
                top_consumers: vec![],
            },
            repeat_after: None,
        }
    }

    fn empty_tick() -> Tick {
        use zstats::snapshot::{CpuSnapshot, HostInfo, LoadSnapshot, MemorySnapshot};
        Tick {
            snapshot: SystemSnapshot {
                timestamp: jiff::Timestamp::now(),
                host: HostInfo {
                    hostname: String::new(),
                    os_name: String::new(),
                    os_version: String::new(),
                    kernel_version: None,
                    arch: String::new(),
                    uptime_secs: 0,
                    labels: HashMap::new(),
                },
                cpu: CpuSnapshot {
                    usage_percent: 0.0,
                    per_core_usage: vec![],
                    logical_cores: 1,
                    physical_cores: None,
                    frequency_mhz: None,
                    per_core_frequency_mhz: vec![],
                    brand: None,
                    perf_levels: None,
                },
                memory: MemorySnapshot {
                    total_bytes: 16 << 30,
                    used_bytes: 0,
                    available_bytes: 16 << 30,
                    swap_total_bytes: 0,
                    swap_used_bytes: 0,
                    used_percent: 0.0,
                    swap_used_percent: 0.0,
                    compressed_bytes: None,
                    pressure_level: Some(1),
                },
                disks: None,
                networks: None,
                processes: None,
                process_groups: None,
                total_processes: None,
                battery: None,
                load: LoadSnapshot {
                    load1: 0.0,
                    load5: 0.0,
                    load15: 0.0,
                },
                temperatures: None,
                io_totals: Default::default(),
                capabilities: Default::default(),
                extras: HashMap::new(),
            },
            alerts: vec![],
            process_stats: HashMap::new(),
            records: vec![],
        }
    }

    /// zstats reports a crossing once and follows up once 30 minutes later.
    /// Both describe the same episode, and a list that appends a card per
    /// event turns one problem into two — then lets a flapping process crowd
    /// everything else out of the 20 slots.
    #[test]
    fn repeat_reports_merge_into_one_episode() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();

        state.record_alert(cpu_alert(7), t0);
        state.record_alert(cpu_alert(7), t0 + Duration::from_secs(1800));
        assert_eq!(state.alerts().len(), 1, "same process, same measure");
        assert_eq!(state.alerts()[0].reports, 2);
        assert_eq!(state.alerts()[0].span(), Some(Duration::from_secs(1800)));

        // Same process over on a *different* measure is a separate story.
        state.record_alert(mem_alert(7), t0);
        // A different process likewise.
        state.record_alert(cpu_alert(8), t0);
        assert_eq!(state.alerts().len(), 3);

        // Resurfacing moves an episode back to the front without duplicating.
        state.record_alert(cpu_alert(7), t0 + Duration::from_secs(3600));
        assert_eq!(state.alerts().len(), 3);
        assert_eq!(state.alerts()[0].reports, 3);
        assert_eq!(state.alerts()[0].seq, 1, "still the episode opened first");
    }

    /// A subject that keeps crossing, clearing and crossing again opens a
    /// fresh episode each time, and each one used to interrupt. Two is
    /// enough to have said it; the rest go to the list only.
    #[test]
    fn a_flapping_subject_stops_interrupting_after_two_banners() {
        let mut state = ZStatsAppState::new();
        let t0 = Instant::now();
        let event = cpu_alert(7);

        assert!(!state.banner_damped(&event, t0), "first one interrupts");
        assert!(
            !state.banner_damped(&event, t0 + Duration::from_secs(600)),
            "so does the second"
        );
        assert!(
            state.banner_damped(&event, t0 + Duration::from_secs(1200)),
            "the third within the hour does not"
        );
        assert!(state.banner_auto_quiet(&event), "and the card says so");

        // A different subject is different news — it must still arrive.
        assert!(!state.banner_damped(&mem_alert(9), t0 + Duration::from_secs(1200)));

        // Once the window has slid past both deliveries, it speaks again.
        let later = t0 + Duration::from_secs(3600 + 700);
        assert!(
            !state.banner_damped(&event, later),
            "quiet for an hour buys back a banner"
        );
    }

    /// "Resume" has to mean resume: an auto-quiet that outlived the
    /// explicit un-mute would keep the subject silent behind the user.
    #[test]
    fn resuming_a_snooze_also_clears_the_auto_quiet() {
        let mut state = ZStatsAppState::new();
        let t0 = Instant::now();
        let event = cpu_alert(7);
        assert!(!state.banner_damped(&event, t0));
        assert!(!state.banner_damped(&event, t0));
        assert!(state.banner_auto_quiet(&event));

        state.resume_banners(&event);
        assert!(!state.banner_auto_quiet(&event));
        assert!(
            !state.banner_damped(&event, t0),
            "and the next one interrupts again"
        );
    }

    /// A restart is not a new problem: an episode read back from the
    /// file is the same episode, so the next report merges into it and
    /// the count keeps climbing.
    #[test]
    fn a_restored_episode_is_continued_not_duplicated() {
        let mut state = ZStatsAppState::new();
        let morning = SystemTime::now() - Duration::from_secs(4 * 3600);
        state.adopt_alerts(vec![alertlog::Restored {
            event: cpu_alert(7),
            first_at: morning,
            at: morning,
            reports: 2,
            dismissed: false,
        }]);
        assert_eq!(state.alerts().len(), 1);

        state.record_alert(cpu_alert(7), SystemTime::now());
        assert_eq!(state.alerts().len(), 1, "same condition, same episode");
        assert_eq!(state.alerts()[0].reports, 3, "the count carries over");
        assert!(
            state.alerts()[0]
                .span()
                .is_some_and(|s| s >= Duration::from_secs(4 * 3600)),
            "the episode still knows it started this morning"
        );

        // A different condition opens its own card with its own id.
        state.record_alert(mem_alert(7), SystemTime::now());
        assert_eq!(state.alerts().len(), 2);
        assert_ne!(state.alerts()[0].seq, state.alerts()[1].seq);
    }

    /// The tray spec is "a live report you have not opened Alerts
    /// for", not a count of cards. Restored episodes are yesterday's
    /// news; a follow-up of one already seen re-lights once they look
    /// away.
    #[test]
    fn a_live_report_lights_the_tray_spec_until_alerts_are_shown() {
        let mut state = ZStatsAppState::new();
        assert!(!state.tray_alert_unseen());

        state.adopt_alerts(vec![alertlog::Restored {
            event: cpu_alert(7),
            first_at: SystemTime::now(),
            at: SystemTime::now(),
            reports: 1,
            dismissed: false,
        }]);
        assert!(
            !state.tray_alert_unseen(),
            "a restored card is not a new alert"
        );

        state.record_alert(cpu_alert(8), SystemTime::now());
        assert!(state.tray_alert_unseen(), "a live report lights the spec");

        state.see_alerts();
        assert!(!state.tray_alert_unseen());

        state.record_alert(cpu_alert(8), SystemTime::now());
        assert!(
            state.tray_alert_unseen(),
            "a follow-up while away re-lights"
        );
    }

    /// The acting controls on a card are gated on the pid having been
    /// confirmed *this session*: after a reboot the pid a restored card
    /// names may belong to something else entirely, and "quit Chrome"
    /// would deliver SIGTERM to whatever holds it now.
    #[test]
    fn a_restored_card_cannot_act_until_a_live_report_confirms_it() {
        let mut state = ZStatsAppState::new();
        state.adopt_alerts(vec![alertlog::Restored {
            event: mem_alert(923),
            first_at: SystemTime::now() - Duration::from_secs(7200),
            at: SystemTime::now() - Duration::from_secs(7200),
            reports: 1,
            dismissed: false,
        }]);
        assert!(!state.alerts()[0].live, "restored is read-only");

        // The same condition reported again names the pid live.
        state.record_alert(mem_alert(923), SystemTime::now());
        assert!(state.alerts()[0].live, "a live report re-arms the card");
        assert_eq!(state.alerts().len(), 1, "still one episode");
    }

    /// The list outlives restarts now, so it needs a way to be put down
    /// — otherwise the tab's alert tint stays lit for the rest of the
    /// day with nothing the user can do about it.
    #[test]
    fn dismiss_removes_one_episode_and_leaves_the_rest() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(cpu_alert(7), t0);
        state.record_alert(cpu_alert(8), t0);
        let doomed = state.alerts()[0].seq;

        assert!(state.drop_alert(doomed));
        assert_eq!(state.alerts().len(), 1);
        assert_ne!(state.alerts()[0].seq, doomed);
        assert!(
            !state.drop_alert(doomed),
            "dismissing twice changes nothing"
        );
    }

    /// Auto faces memory while a live memory episode still holds, and
    /// only after five minutes under the event's own bar — not on a
    /// one-sample dip, and not by evaluating a new threshold.
    #[test]
    fn auto_tray_returns_to_cpu_five_minutes_after_memory_recovers() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(mem_alert(7), t0);
        assert!(
            state.memory_needs_attention_at(t0),
            "a live memory episode faces memory"
        );

        let mut tick = empty_tick();
        let mut p = snap(7, "p7");
        p.phys_footprint_bytes = Some(100 << 20);
        tick.snapshot.processes = Some(Arc::new(vec![p]));
        state.latest = Some(tick);
        state.note_memory_recovery(t0);
        assert!(
            state.memory_needs_attention_at(t0),
            "just recovered is still memory"
        );
        assert!(
            state.memory_needs_attention_at(t0 + Duration::from_secs(4 * 60 + 59)),
            "four minutes under the bar is not five"
        );
        assert!(
            !state.memory_needs_attention_at(t0 + TRAY_RECOVER),
            "five minutes under the event's bar returns to CPU"
        );
    }

    #[test]
    fn auto_tray_stays_on_memory_while_the_process_is_still_over() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(mem_alert(7), t0);
        let mut tick = empty_tick();
        let mut p = snap(7, "p7");
        p.phys_footprint_bytes = Some(8 << 30);
        tick.snapshot.processes = Some(Arc::new(vec![p]));
        state.latest = Some(tick);
        state.note_memory_recovery(t0);
        assert!(state.memory_needs_attention_at(t0 + TRAY_RECOVER));
    }

    #[test]
    fn auto_tray_faces_memory_again_if_the_condition_returns() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(mem_alert(7), t0);
        let mut quiet = empty_tick();
        let mut p = snap(7, "p7");
        p.phys_footprint_bytes = Some(100 << 20);
        quiet.snapshot.processes = Some(Arc::new(vec![p.clone()]));
        state.latest = Some(quiet);
        state.note_memory_recovery(t0);

        p.phys_footprint_bytes = Some(8 << 30);
        let mut loud = empty_tick();
        loud.snapshot.processes = Some(Arc::new(vec![p]));
        state.latest = Some(loud);
        state.note_memory_recovery(t0 + Duration::from_secs(60));
        assert!(
            state.memory_needs_attention_at(t0 + TRAY_RECOVER + Duration::from_secs(60)),
            "crossing again resets the five minutes"
        );
    }

    #[test]
    fn auto_tray_pressure_returns_after_five_minutes_of_normal() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(pressure_alert(4), t0);
        let mut tick = empty_tick();
        tick.snapshot.memory.pressure_level = Some(4);
        state.latest = Some(tick);
        state.note_memory_recovery(t0);
        assert!(state.memory_needs_attention_at(t0));

        let mut normal = empty_tick();
        normal.snapshot.memory.pressure_level = Some(1);
        state.latest = Some(normal);
        state.note_memory_recovery(t0);
        assert!(state.memory_needs_attention_at(t0 + Duration::from_secs(60)));
        assert!(!state.memory_needs_attention_at(t0 + TRAY_RECOVER));
    }

    /// A memory-heavy Mac sits at the kernel's warning tier as its
    /// steady state, so that tier does not get the menu bar — the card
    /// and the banner still carry it. Critical does, and an episode
    /// that escalates turns the face on the report that says so.
    #[test]
    fn auto_tray_waits_for_critical_pressure_but_not_for_warning() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(pressure_alert(2), t0);
        let mut warned = empty_tick();
        warned.snapshot.memory.pressure_level = Some(2);
        state.latest = Some(warned);
        state.note_memory_recovery(t0);
        assert!(
            !state.memory_needs_attention_at(t0),
            "warning is this platform's normal, not news for the menu bar"
        );
        // The episode is still on the tab: only the face is withheld.
        assert_eq!(state.alerts().len(), 1);

        // Worsening is reported as a fresh event on the same episode.
        state.record_alert(pressure_alert(4), t0);
        let mut critical = empty_tick();
        critical.snapshot.memory.pressure_level = Some(4);
        state.latest = Some(critical);
        state.note_memory_recovery(t0);
        assert!(state.memory_needs_attention_at(t0));
    }

    /// The clock's two transitions are what the log reports, so the
    /// state they read from has to move exactly once per transition:
    /// the arm re-holds on every tick the condition holds, and a line
    /// per tick would drown the one that matters.
    #[test]
    fn the_recovery_clock_moves_only_on_a_transition() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(pressure_alert(4), t0);
        let normal = || {
            let mut tick = empty_tick();
            tick.snapshot.memory.pressure_level = Some(1);
            tick
        };
        state.latest = Some(normal());
        state.note_memory_recovery(t0);
        let started = state.alerts()[0].recovered_since.expect("clock started");
        // A second quiet tick must not restart it — that would push the
        // deadline out forever and log a line each time.
        state.latest = Some(normal());
        state.note_memory_recovery(t0 + Duration::from_secs(5));
        assert_eq!(state.alerts()[0].recovered_since, Some(started));

        // Back over the line: cleared, so the next quiet tick is a
        // genuine restart.
        let mut over = empty_tick();
        over.snapshot.memory.pressure_level = Some(4);
        state.latest = Some(over);
        state.note_memory_recovery(t0 + Duration::from_secs(60));
        assert!(state.alerts()[0].recovered_since.is_none());
        state.latest = Some(normal());
        state.note_memory_recovery(t0 + Duration::from_secs(65));
        assert_eq!(
            state.alerts()[0].recovered_since,
            Some(t0 + Duration::from_secs(65))
        );
    }

    /// A process over its memory bar is Warning in zstats — only
    /// pressure ≥ 4 and a runaway CPU are Critical — so gating the
    /// whole class on severity would have deleted the face's original
    /// job.
    #[test]
    fn auto_tray_still_turns_for_a_process_memory_episode() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(mem_alert(7), t0);
        assert_eq!(state.alerts()[0].event.severity(), Severity::Warning);
        assert!(state.memory_needs_attention_at(t0));
    }

    #[test]
    fn auto_tray_ignores_restored_memory_episodes() {
        let mut state = ZStatsAppState::new();
        state.adopt_alerts(vec![alertlog::Restored {
            event: mem_alert(7),
            first_at: SystemTime::now(),
            at: SystemTime::now(),
            reports: 1,
            dismissed: false,
        }]);
        assert!(!state.memory_needs_attention_at(SystemTime::now()));
    }

    fn disk_snap(mount: &str, used: f32, avail: u64, total: u64) -> zstats::snapshot::DiskSnapshot {
        zstats::snapshot::DiskSnapshot {
            name: "Macintosh HD".into(),
            mount_point: mount.into(),
            file_system: "apfs".into(),
            kind: "SSD".into(),
            is_removable: false,
            total_bytes: total,
            available_bytes: avail,
            used_percent: used,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    #[test]
    fn auto_tray_turns_for_a_live_disk_episode() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(disk_alert("/"), t0);
        assert!(state.disk_needs_attention_at(t0));
        assert!(
            !state.memory_needs_attention_at(t0),
            "a disk episode is not a memory one"
        );
    }

    #[test]
    fn auto_tray_returns_to_cpu_once_disk_is_under_the_bar() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(disk_alert("/"), t0);
        let mut tick = empty_tick();
        tick.snapshot.disks = Some(vec![disk_snap("/", 50.0, 100 << 30, 200 << 30)]);
        state.latest = Some(tick);
        state.note_memory_recovery(t0);
        assert!(
            !state.disk_needs_attention_at(t0),
            "this tick is already under the event's used-% bar"
        );
        assert!(
            state.alerts()[0].recovered_since.is_some(),
            "the card stays; only the face drops"
        );
    }

    #[test]
    fn auto_tray_stays_on_disk_while_the_volume_is_still_over() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(disk_alert("/"), t0);
        let mut tick = empty_tick();
        tick.snapshot.disks = Some(vec![disk_snap("/", 99.0, 1 << 30, 200 << 30)]);
        state.latest = Some(tick);
        state.note_memory_recovery(t0);
        assert!(state.disk_needs_attention_at(t0 + TRAY_RECOVER));
    }

    #[test]
    fn auto_tray_ignores_restored_disk_episodes() {
        let mut state = ZStatsAppState::new();
        state.adopt_alerts(vec![alertlog::Restored {
            event: disk_alert("/"),
            first_at: SystemTime::now(),
            at: SystemTime::now(),
            reports: 1,
            dismissed: false,
        }]);
        assert!(!state.disk_needs_attention_at(SystemTime::now()));
    }

    #[test]
    fn an_ejected_volume_gives_up_the_face_at_once() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(disk_alert("/Volumes/USB"), t0);
        state.ejected.insert("/Volumes/USB".into(), Instant::now());
        let mut tick = empty_tick();
        tick.snapshot.disks = Some(vec![disk_snap("/Volumes/USB", 99.0, 0, 1 << 30)]);
        state.latest = Some(tick);
        state.note_memory_recovery(t0);
        assert!(
            state.alerts()[0].recovered_since.is_some(),
            "ejected is gone, even if the snapshot still lists it"
        );
        assert!(
            !state.disk_needs_attention_at(t0),
            "no volume to name, so no disk face over a placeholder"
        );
    }

    #[test]
    fn an_unplugged_volume_gives_up_the_face_at_once() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        state.record_alert(disk_alert("/Volumes/USB"), t0);
        let mut tick = empty_tick();
        tick.snapshot.disks = Some(vec![disk_snap("/", 50.0, 100 << 30, 200 << 30)]);
        state.latest = Some(tick);
        assert!(!state.disk_needs_attention_at(t0));
        assert!(state.disk_face_volume().is_none());
    }

    /// "Today's alerts" has to keep meaning today on a machine that
    /// never restarts — the file draws that boundary when it loads, and
    /// a session running past midnight has to draw it too.
    #[test]
    fn the_day_boundary_retires_yesterdays_episodes() {
        let mut state = ZStatsAppState::new();
        let now = SystemTime::now();
        state.record_alert(cpu_alert(7), now - Duration::from_secs(3 * 86_400));
        state.record_alert(cpu_alert(8), now);
        assert_eq!(state.alerts().len(), 2);

        assert!(state.retain_today(now), "the stale one is retired");
        assert_eq!(state.alerts().len(), 1);
        assert!(matches!(
            state.alerts()[0].event.subject,
            AlertSubject::Process { pid: 8, .. }
        ));
        assert!(!state.retain_today(now), "nothing left to retire");
    }

    /// The id has to outlive reordering — it is what element state (hover,
    /// the expanded editor) is keyed on.
    #[test]
    fn episode_ids_are_unique_and_stable() {
        let mut state = ZStatsAppState::new();
        let t0 = SystemTime::now();
        for pid in 1..=3 {
            state.record_alert(cpu_alert(pid), t0);
        }
        let before: Vec<_> = state
            .alerts()
            .iter()
            .map(|a| (a.seq, a.event.kind()))
            .collect();
        // Push the oldest back to the front.
        state.record_alert(cpu_alert(1), t0 + Duration::from_secs(60));
        let after: Vec<_> = state.alerts().iter().map(|a| a.seq).collect();
        assert_eq!(after, vec![1, 3, 2], "order changes, ids do not");
        assert_eq!(before.len(), 3);
        let unique: HashSet<u64> = after.iter().copied().collect();
        assert_eq!(unique.len(), 3, "ids must not collide");
    }
}
