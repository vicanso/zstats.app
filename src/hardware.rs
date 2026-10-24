//! The Hardware tab's rules, on either front end: what each card calls a
//! volume, a drive, a GPU, a sensor and the battery, which of them it
//! shows and in what order, and the one act it takes — ejecting a
//! removable volume. The colour lines are `hot.rs`'s. Every figure is
//! zstats' own; this formats and picks, it derives nothing. Shared since
//! the NSH Hardware page (phase 4f of docs/nsh-migration.md); the gpui
//! cards (`views/disk.rs`, `drives.rs`, `gpu.rs`, `sensors.rs`) read it
//! too, and say why each card looks the way it does.

use crate::state::ZStatsAppState;
use crate::{format, hot, i18n};
use rust_i18n::t;
use std::process;
use zstats::snapshot::{
    BatterySnapshot, DiskSnapshot, DriveSnapshot, GpuSnapshot, TemperatureSnapshot,
};

/// The volume's name as the Finder shows it — "Macintosh HD", not "/".
///
/// zstats reports it (`DiskSnapshot::name`), so this is a display choice
/// and not a second source: the panel used to paint the mount point,
/// which names the volume correctly and identifies it to nobody outside
/// a terminal. The mount point is still on the card, in the footer where
/// the file system already is. Falls back to it when the OS gave no name
/// — an unnamed volume must not render a blank title.
pub fn volume_title(disk: &DiskSnapshot) -> String {
    if disk.name.trim().is_empty() {
        disk.mount_point.clone()
    } else {
        disk.name.clone()
    }
}

/// The footer's right slot: "/ · apfs · SSD". The technical identity, in
/// the smallest text on the card, so moving the name up top loses
/// nothing. The drive kind joined it from the header, where its outlined
/// pill sat beside the Analyze button and read as a second button.
pub fn volume_footer_id(disk: &DiskSnapshot) -> String {
    let mut parts = Vec::new();
    if !disk.name.trim().is_empty() {
        parts.push(disk.mount_point.as_str());
    }
    parts.push(disk.file_system.as_str());
    let kind = disk.kind.trim();
    if !kind.is_empty() && !kind.eq_ignore_ascii_case("unknown") {
        parts.push(kind);
    }
    parts.join(" · ")
}

/// "purgeable ~X · N local snapshots" under the boot volume's footer: the
/// line the capacity meter cannot say — where a full disk often actually
/// went, and why per-directory sums do not add up to "used". Apple's own
/// figures; `None` while there is nothing to say.
pub fn space_extra(state: &ZStatsAppState) -> Option<String> {
    let info = state.space_info()?;
    let mut parts = Vec::new();
    if let Some(purgeable) = info.purgeable_bytes.filter(|p| *p > 0) {
        parts.push(t!("disk.purgeable", v = format::memory(purgeable)).to_string());
    }
    if info.snapshots > 0 {
        parts.push(t!("disk.snapshots", n = info.snapshots).to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Whether `mount` may ever be ejected: never the boot volume or the
/// system's own — the eject control is only drawn for a removable volume,
/// and this is a second gate.
pub fn safe_to_eject(mount: &str) -> bool {
    let mount = mount.trim_end_matches('/');
    !matches!(
        mount,
        "" | "/" | "/System" | "/System/Volumes/Data" | "/home" | "/Users"
    )
}

/// Unmount and eject a removable volume, blocking: run it off the main
/// thread. `true` only when the tool reported success — a failed eject
/// must not shorten the wait for a sample that would show the volume
/// still mounted. On success the front end marks the volume ejected and
/// wakes the collector (`ZStatsAppState::mark_ejected` says why).
pub fn eject(mount: &str) -> bool {
    if !safe_to_eject(mount) {
        tracing::warn!("refusing to eject {mount}");
        return false;
    }
    match eject_command(mount) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("eject {mount}: {e}");
            false
        }
    }
}

/// `diskutil eject`: unmount and power the drive down in one step.
#[cfg(target_os = "macos")]
fn eject_command(mount: &str) -> Result<(), String> {
    run_tool("diskutil", &["eject", mount])
}

/// udisks, the desktop's own path — it lets the user who plugged the
/// drive in unmount it without root, which plain `umount` does not (that
/// wants `CAP_SYS_ADMIN` unless fstab says `user`, and a removable drive
/// a desktop auto-mounted under `/run/media` is never in fstab). So the
/// mount point is resolved to its device with `findmnt`, unmounted
/// through `udisksctl`, and the drive then powered off the way "safely
/// remove" does — that last step best-effort, because a drive with a
/// second partition still mounted refuses it and that is not a failed
/// eject of *this* volume. Without udisks (a headless box) it falls back
/// to `umount` and whatever permissions the user has.
#[cfg(not(target_os = "macos"))]
fn eject_command(mount: &str) -> Result<(), String> {
    if !tool_exists("udisksctl") {
        return run_tool("umount", &[mount]);
    }
    let out = process::Command::new("findmnt")
        .args(["-rn", "-o", "SOURCE", "--mountpoint", mount])
        .output()
        .map_err(|e| format!("findmnt: {e}"))?;
    let device = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || device.is_empty() {
        return Err(format!("findmnt: no device is mounted at {mount}"));
    }
    run_tool("udisksctl", &["unmount", "-b", &device])?;
    if let Err(e) = run_tool("udisksctl", &["power-off", "-b", &device]) {
        tracing::debug!("power-off after unmount: {e}");
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn tool_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
}

/// Run a tool and turn a non-zero exit into its stderr.
fn run_tool(tool: &str, args: &[&str]) -> Result<(), String> {
    let out = process::Command::new(tool)
        .args(args)
        .output()
        .map_err(|e| format!("{tool}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{tool} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Whether the drives card is shown at all: a platform without the
/// concept (Linux, Windows today) and a file that turned the channel off
/// both get no card — neither is a state the tab can do anything about.
pub fn drives_shown(state: &ZStatsAppState) -> bool {
    let file_allows = state
        .settings()
        .and_then(|s| s.collector.as_ref())
        .is_none_or(|c| c.collect_drives);
    state
        .latest()
        .is_some_and(|t| t.snapshot.capabilities.drive_io)
        && file_allows
}

/// The same two silences for the GPU cards.
pub fn gpus_shown(state: &ZStatsAppState) -> bool {
    let file_allows = state
        .settings()
        .and_then(|s| s.collector.as_ref())
        .is_none_or(|c| c.collect_gpu);
    state.latest().is_some_and(|t| t.snapshot.capabilities.gpu) && file_allows
}

/// A drive's model and size, beside its name.
pub fn drive_identity(drive: &DriveSnapshot) -> String {
    match (&drive.model, drive.size_bytes) {
        (Some(model), Some(size)) => format!("{model} · {}", format::capacity(size)),
        (Some(model), None) => model.clone(),
        (None, Some(size)) => format::capacity(size),
        (None, None) => String::new(),
    }
}

/// No baseline yet — the first read after arriving on the tab (the
/// switch restarts drive rates from nothing). Two rows of `R — · —` and a
/// `Q —` read as a broken card; one quiet word says the numbers are on
/// their way, and the queue waits with them.
pub fn drive_measuring(drive: &DriveSnapshot) -> bool {
    drive.read_ops_per_sec.is_none()
        && drive.write_ops_per_sec.is_none()
        && drive.queue_depth.is_none()
}

/// A GPU's name, with its core count where the driver gives one.
pub fn gpu_name(gpu: &GpuSnapshot) -> String {
    match gpu.cores {
        Some(cores) => t!("gpu.name_cores", name = gpu.name.clone(), cores = cores).to_string(),
        None => gpu.name.clone(),
    }
}

/// The figures under a GPU's bar, each only where the driver publishes it
/// — an Intel Mac's discrete GPU reports a different set than Apple
/// Silicon, and a row of `—` for a field this driver never has would
/// read as "not yet".
pub fn gpu_detail_rows(gpu: &GpuSnapshot) -> Vec<(String, String)> {
    let pct = |label: &str, v: Option<f32>| v.map(|v| (i18n::tr(label), format::pct(v)));
    let bytes = |label: &str, v: Option<u64>| v.map(|b| (i18n::tr(label), format::gb(b)));
    [
        pct("gpu.renderer", gpu.renderer_utilization_percent),
        pct("gpu.tiler", gpu.tiler_utilization_percent),
        bytes("gpu.mem_in_use", gpu.memory_in_use_bytes),
        bytes("gpu.mem_allocated", gpu.memory_allocated_bytes),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Sensor rows shown before "show more". Apple Silicon firmware exposes
/// dozens of channels and most sit within a degree of each other; the few
/// nearest their own limits carry the story, and the full list would bury
/// the cards that share this tab. Hot sensors are never hidden — the cap
/// only ever swallows quiet ones, and since the sort key and the colour
/// key are the same fraction (`hot::crit_fraction`), the hot rows are
/// always a prefix the truncation cannot reach into.
///
/// Four: two rows of the grid — a floor, with "show more" for the rest.
pub const SENSOR_PREVIEW: usize = 4;

/// The temperatures card's list: its rows, closest to their own limits
/// first, the hottest reading in degrees for the header, and how many
/// rows the collapse would hide whether or not they are on screen (so
/// the control stays a toggle once expanded).
pub struct Sensors<'a> {
    pub rows: Vec<&'a TemperatureSnapshot>,
    pub hottest: f32,
    pub hideable: usize,
}

/// [`Sensors`] for `temps`, all of them while `show_all`.
pub fn sensors(temps: &[TemperatureSnapshot], show_all: bool) -> Sensors<'_> {
    let hottest = temps.iter().map(|t| t.celsius).fold(f32::MIN, f32::max);
    let mut rows: Vec<_> = temps.iter().collect();
    rows.sort_by(|a, b| hot::crit_fraction(b).total_cmp(&hot::crit_fraction(a)));
    let hot_rows = rows
        .iter()
        .filter(|t| hot::sensor_hot(t.celsius, t.critical_celsius))
        .count();
    let keep = SENSOR_PREVIEW.max(hot_rows);
    let hideable = rows.len().saturating_sub(keep);
    if !show_all {
        rows.truncate(keep);
    }
    Sensors {
        rows,
        hottest,
        hideable,
    }
}

/// What the firmware reports beside a reading.
///
/// zstats' `max_celsius` is the **highest reading observed**, not a
/// thermal limit — painting "max 52" next to 51.8 °C read as "at the
/// ceiling" when the bar (drawn against 110 °C / crit) was only half
/// full. The label says *peak*. Colour and order use `hot::ceiling`.
pub fn limits_label(t: &TemperatureSnapshot) -> String {
    match (t.max_celsius, t.critical_celsius) {
        (Some(m), Some(c)) => t!(
            "sensors.max_crit",
            max = format!("{m:.0}"),
            crit = format!("{c:.0}")
        )
        .to_string(),
        (Some(m), None) => t!("sensors.max_only", max = format!("{m:.0}")).to_string(),
        (None, Some(c)) => t!("sensors.crit_only", crit = format!("{c:.0}")).to_string(),
        (None, None) => i18n::tr("sensors.no_limits"),
    }
}

/// What the battery is doing, for the card's header: where the power
/// comes from and, while there is one, how long until full or empty —
/// "Charging · 1h 20m to full", "On battery · 3h 40m left", "Plugged in
/// · full". zstats' doc for `state` says frontends derive "on AC" from
/// it, and these are its words for the states that say so (a battery
/// that is not discharging is on an external source); "unknown", or a
/// word it adds later, says nothing rather than a guess.
///
/// The time was a cell of the grid, and the header carried zstats' raw
/// word ("full", untranslated). A cell that comes and goes meant the
/// grid was padded to six to hold its height, and on a full Mac that
/// reports no cell temperature two of the six were blank under a
/// hairline — read as a broken card. Here the time costs no cell.
pub fn battery_status(b: &BatterySnapshot) -> Option<String> {
    let with_time = |bare: &str, timed: &str, secs: Option<u64>| match secs {
        Some(secs) => t!(timed, time = format::uptime(secs)).to_string(),
        None => i18n::tr(bare),
    };
    Some(match b.state.to_ascii_lowercase().as_str() {
        "charging" => with_time(
            "sensors.status_charging",
            "sensors.status_charging_for",
            b.time_to_full_secs,
        ),
        "discharging" => with_time(
            "sensors.status_on_battery",
            "sensors.status_on_battery_for",
            b.time_to_empty_secs,
        ),
        "full" => i18n::tr("sensors.status_full"),
        "paused" => i18n::tr("sensors.status_paused"),
        "empty" => i18n::tr("sensors.status_empty"),
        _ => return None,
    })
}

/// The battery's facts two-up, label and value, the left column first:
/// only the ones this machine reports, so the grid is the same size from
/// one sample to the next without blank cells to hold it — the left
/// column the battery now (charge, power, the cell temperature where
/// there is one), the right its wear. An odd count leaves the
/// bottom-right cell empty, so the two columns' hairlines still meet.
pub fn battery_cells(b: &BatterySnapshot) -> Vec<(String, String)> {
    let charge = (i18n::tr("sensors.charge"), format::pct(b.charge_percent));
    // zstats' `power_watts` is the present flow either way, and `state`
    // says which: "Draw" beside "charging" read as the machine's own use.
    let draw = (
        i18n::tr(power_label_key(&b.state)),
        b.power_watts
            .map_or(format::PLACEHOLDER.to_string(), |w| format!("{w:.1} W")),
    );
    let health = (
        i18n::tr("sensors.health"),
        b.health_percent
            .map_or(format::PLACEHOLDER.into(), format::pct),
    );
    let cycles = (
        i18n::tr("sensors.cycles"),
        b.cycle_count
            .map_or(format::PLACEHOLDER.to_string(), |c| c.to_string()),
    );
    // A cell temperature this machine does not report is not a row: a
    // dash under "Cell temp" said nothing a missing row does not.
    let temp = b
        .temperature_celsius
        .map(|c| (i18n::tr("sensors.cell_temp"), format!("{c:.1} °C")));
    let mut cells = vec![charge, draw];
    cells.extend(temp);
    cells.extend([health, cycles]);
    // The left column fills first, so a blank pushed last lands bottom
    // right.
    if cells.len() % 2 == 1 {
        cells.push((String::new(), "\u{00a0}".into()));
    }
    cells
}

/// The power cell's label for this battery state: the same zstats field
/// is charging power or draw.
fn power_label_key(state: &str) -> &'static str {
    match state.to_ascii_lowercase().as_str() {
        "charging" => "sensors.charging_power",
        "discharging" | "empty" => "sensors.draw",
        _ => "sensors.power",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(name: &str, mount: &str) -> DiskSnapshot {
        DiskSnapshot {
            name: name.into(),
            mount_point: mount.into(),
            file_system: "apfs".into(),
            kind: "SSD".into(),
            is_removable: false,
            total_bytes: 0,
            available_bytes: 0,
            used_percent: 0.0,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
        }
    }

    /// The title is the volume's name, the footer keeps the mount point —
    /// and a nameless volume must still title itself with something.
    #[test]
    fn volume_title_prefers_the_name_and_keeps_the_mount_point_below() {
        let boot = snapshot("Macintosh HD", "/");
        assert_eq!(volume_title(&boot), "Macintosh HD");
        assert_eq!(volume_footer_id(&boot), "/ · apfs · SSD");

        let unnamed = snapshot("  ", "/Volumes/disk3s2");
        assert_eq!(volume_title(&unnamed), "/Volumes/disk3s2");
        // The title already is the mount point; repeating it below would
        // be the same string twice on one card.
        assert_eq!(volume_footer_id(&unnamed), "apfs · SSD");
        let mut unknown = snapshot("Data", "/Volumes/Data");
        unknown.kind = "Unknown".into();
        assert_eq!(volume_footer_id(&unknown), "/Volumes/Data · apfs");
    }

    #[test]
    fn will_not_eject_the_boot_volume() {
        assert!(!safe_to_eject("/"));
        assert!(!safe_to_eject("/System/Volumes/Data"));
        assert!(safe_to_eject("/Volumes/Zedis Installer"));
    }

    fn gpu(renderer: Option<f32>, in_use: Option<u64>) -> GpuSnapshot {
        GpuSnapshot {
            name: "Apple M4 Pro".into(),
            cores: Some(20),
            utilization_percent: 12.0,
            renderer_utilization_percent: renderer,
            tiler_utilization_percent: None,
            memory_in_use_bytes: in_use,
            memory_allocated_bytes: None,
        }
    }

    #[test]
    fn only_the_figures_the_driver_publishes_get_a_row() {
        assert_eq!(gpu_detail_rows(&gpu(None, None)).len(), 0);
        assert_eq!(gpu_detail_rows(&gpu(Some(10.0), None)).len(), 1);
        assert_eq!(gpu_detail_rows(&gpu(Some(10.0), Some(1 << 30))).len(), 2);
    }

    fn temp(label: &str, celsius: f32, crit: Option<f32>) -> TemperatureSnapshot {
        TemperatureSnapshot {
            label: label.into(),
            celsius,
            max_celsius: None,
            critical_celsius: crit,
        }
    }

    /// The preview keeps the few nearest their limits and never hides a
    /// hot one; the count it would hide stays the same once expanded.
    #[test]
    fn the_sensor_preview_never_hides_a_hot_reading() {
        let mut temps: Vec<_> = (0..8).map(|i| temp("c", 40.0 + i as f32, None)).collect();
        let quiet = sensors(&temps, false);
        assert_eq!(quiet.rows.len(), SENSOR_PREVIEW);
        assert_eq!(quiet.hideable, 4);
        assert_eq!(quiet.hottest, 47.0);
        assert_eq!(quiet.rows[0].celsius, 47.0, "nearest its limit first");
        assert_eq!(sensors(&temps, true).rows.len(), 8);
        assert_eq!(sensors(&temps, true).hideable, 4);
        // Six readings near their own ceilings: all six stay.
        temps.extend((0..6).map(|_| temp("b", 58.0, Some(60.0))));
        let hot = sensors(&temps, false);
        assert_eq!(hot.rows.len(), 6);
        assert!(hot.rows.iter().all(|t| t.label == "b"));
    }

    fn battery(state: &str, to_empty: Option<u64>, to_full: Option<u64>) -> BatterySnapshot {
        BatterySnapshot {
            state: state.into(),
            charge_percent: 100.0,
            health_percent: None,
            cycle_count: None,
            temperature_celsius: None,
            power_watts: None,
            time_to_full_secs: to_full,
            time_to_empty_secs: to_empty,
        }
    }

    #[test]
    fn the_power_cell_is_named_for_the_direction_of_flow() {
        assert_eq!(power_label_key("Charging"), "sensors.charging_power");
        assert_eq!(power_label_key("Discharging"), "sensors.draw");
        assert_eq!(power_label_key("Full"), "sensors.power");
    }

    /// The header says where the power comes from, and the one time
    /// that applies while there is one. zstats' words arrive lowercase
    /// from the battery crate, though its doc capitalises them.
    #[test]
    fn the_header_says_where_the_power_comes_from_and_for_how_long() {
        let status = |state, to_empty, to_full| battery_status(&battery(state, to_empty, to_full));
        assert_eq!(
            status("discharging", Some(3600), None),
            Some(t!("sensors.status_on_battery_for", time = format::uptime(3600)).to_string())
        );
        assert_eq!(
            status("Charging", None, Some(900)),
            Some(t!("sensors.status_charging_for", time = format::uptime(900)).to_string())
        );
        assert_eq!(
            status("discharging", None, None),
            Some(i18n::tr("sensors.status_on_battery")),
            "no estimate yet is the state alone"
        );
        assert_eq!(
            status("full", None, None),
            Some(i18n::tr("sensors.status_full"))
        );
        assert_eq!(
            status("paused", None, None),
            Some(i18n::tr("sensors.status_paused"))
        );
        assert_eq!(status("unknown", None, Some(900)), None, "no guess");
    }

    /// No blank cell holds the grid's height any more, so a Mac that
    /// reports no cell temperature is a full two-by-two; one that does
    /// leaves only the bottom-right cell empty.
    #[test]
    fn the_battery_grid_has_no_blank_but_the_one_an_odd_count_needs() {
        let blank = |cells: &[(String, String)]| cells.iter().filter(|(k, _)| k.is_empty()).count();
        let cells = battery_cells(&battery("full", None, None));
        assert_eq!(cells.len(), 4);
        assert_eq!(blank(&cells), 0);
        let mut warm = battery("full", None, None);
        warm.temperature_celsius = Some(31.4);
        let cells = battery_cells(&warm);
        assert_eq!(cells.len(), 6);
        assert_eq!(blank(&cells), 1);
        assert!(cells[5].0.is_empty(), "the blank is bottom right");
        assert_eq!(
            cells[2].0,
            i18n::tr("sensors.cell_temp"),
            "temperature sits with the now"
        );
    }
}
