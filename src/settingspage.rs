//! The settings window's rules, on either front end: its pages and their
//! order, and what each picker on them offers — the options, the one in
//! force, and the words that go with them. Shared since the NSH settings
//! window (phase 4j of docs/nsh-migration.md); `views/config.rs` reads it
//! too, and says why each row is the way it is.

use crate::prefs::{self, LanguagePref, ThemePref, TrayPref};
use crate::{autostart, finder, format, i18n, omarchy, opener};
use rust_i18n::t;
use std::collections::BTreeMap;
use zstats::alerts::ActiveThresholds;
use zstats::settings::{AlertsConfig, FileConfig, PressureAlert};

/// Left-nav sections of the settings window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SettingsSection {
    /// Language and theme — `app.toml`, not shared with the CLI.
    #[default]
    Interface,
    /// When zstats alerts: the thresholds and the per-program rules.
    /// Half of what was the Config page until 2026-10-12 — the half
    /// someone opens Settings *for* ("stop telling me about Chrome"),
    /// which sat under collector cadences and above three "Update from
    /// GitHub" cards.
    Alerts,
    /// The other half, plus the four Interface rows that asked a
    /// question nobody arrives with: the sustained-load pair, the
    /// webhook and the proxy. Still everything `config.toml` holds that
    /// is not a threshold.
    Advanced,
    /// Full Disk Access: live status plus a deep link into System
    /// Settings. Status only — granting stays a user act there. macOS
    /// only, and the variant itself is gated rather than merely left
    /// out of [`Self::ALL`]: a page nothing can navigate to should not
    /// exist as a value either. See `ALL` for why there is no Linux
    /// counterpart.
    #[cfg(target_os = "macos")]
    Permissions,
    About,
}

impl SettingsSection {
    /// The pages, in nav order.
    ///
    /// Permissions is macOS only. Full Disk Access is a TCC concept: one
    /// switch that pre-empts every per-app data prompt the disk analysis
    /// could trigger. A Linux session has no such switch to offer — a
    /// directory the user cannot read is simply unreadable, and the
    /// analysis already counts those as skipped — so a page whose only
    /// control opens `x-apple.systempreferences:` would be a page about
    /// another operating system.
    #[cfg(target_os = "macos")]
    pub const ALL: [SettingsSection; 5] = [
        SettingsSection::Interface,
        SettingsSection::Alerts,
        SettingsSection::Advanced,
        SettingsSection::Permissions,
        SettingsSection::About,
    ];
    #[cfg(not(target_os = "macos"))]
    pub const ALL: [SettingsSection; 4] = [
        SettingsSection::Interface,
        SettingsSection::Alerts,
        SettingsSection::Advanced,
        SettingsSection::About,
    ];

    pub fn label_key(self) -> &'static str {
        match self {
            SettingsSection::Interface => "config.nav_interface",
            SettingsSection::Alerts => "config.nav_alerts",
            SettingsSection::Advanced => "config.nav_advanced",
            #[cfg(target_os = "macos")]
            SettingsSection::Permissions => "config.nav_permissions",
            SettingsSection::About => "config.nav_about",
        }
    }
}

/// A picker's options, each a label and the value it sets.
pub type Choices<T> = Vec<(String, T)>;

/// The language picker. Each language in its own name, so it stays
/// findable from inside the other locale — deliberately not translated.
pub fn language_choices() -> Choices<LanguagePref> {
    vec![
        (i18n::tr("config.follow_system"), LanguagePref::System),
        ("English".into(), LanguagePref::English),
        ("中文".into(), LanguagePref::Chinese),
    ]
}

/// The theme picker, and its ⓘ. Omarchy only where there is an Omarchy
/// palette to follow, plus every development build so it can be tried
/// from a Mac (where it paints Tokyo Night, Omarchy's default) — in the
/// desktop's own name for itself, in both locales.
pub fn theme_choices() -> (Choices<ThemePref>, String) {
    let mut themes = vec![
        (i18n::tr("config.follow_system"), ThemePref::System),
        (i18n::tr("config.theme_light"), ThemePref::Light),
        (i18n::tr("config.theme_dark"), ThemePref::Dark),
        (i18n::tr("config.theme_golden"), ThemePref::Golden),
    ];
    let omarchy = omarchy::selectable();
    if omarchy {
        themes.push(("Omarchy".into(), ThemePref::Omarchy));
    }
    let tip = i18n::tr(if omarchy {
        "config.theme_tip_omarchy"
    } else {
        "config.theme_tip"
    });
    (themes, tip)
}

/// What the menu bar shows.
pub fn tray_choices() -> Choices<TrayPref> {
    vec![
        (i18n::tr("config.tray_auto"), TrayPref::Auto),
        ("CPU".into(), TrayPref::Cpu),
        (i18n::tr("config.tray_memory"), TrayPref::Memory),
        (i18n::tr("config.tray_both"), TrayPref::Both),
    ]
}

/// The panel's opacity: the scheme's own default, or a fixed wash.
pub fn opacity_choices() -> Choices<Option<f32>> {
    vec![
        (i18n::tr("config.opacity_default"), None),
        ("50%".into(), Some(0.50)),
        ("60%".into(), Some(0.60)),
        ("70%".into(), Some(0.70)),
        ("80%".into(), Some(0.80)),
        ("90%".into(), Some(0.90)),
        ("100%".into(), Some(1.00)),
    ]
}

/// Whether the opacity chip for `value` is the one in force.
pub fn opacity_on(current: Option<f32>, value: Option<f32>) -> bool {
    match (current, value) {
        (None, None) => true,
        (Some(a), Some(b)) => (a - b).abs() < 0.001,
        _ => false,
    }
}

/// How long low-grade CPU has to hold before the watcher speaks, in
/// minutes.
pub fn sustained_after_choices() -> Choices<u16> {
    vec![
        ("1h".into(), 60),
        ("2h".into(), 120),
        ("4h".into(), 240),
        ("8h".into(), 480),
    ]
}

/// The sustained bar as a divisor of `alert-cpu`.
pub fn sustained_divisor_choices() -> Choices<u8> {
    vec![
        ("÷2".into(), 2),
        ("÷3".into(), 3),
        ("÷4".into(), 4),
        ("÷6".into(), 6),
    ]
}

/// The sustained divisor's ⓘ, quoting the bar in force so the divisor
/// reads as a number, not a fraction.
pub fn sustained_divisor_tip(bar_percent: f64) -> String {
    t!(
        "config.sustained_divisor_tip",
        divisor = prefs::sustained_divisor(),
        bar = format!("{bar_percent:.0}%")
    )
    .to_string()
}

/// Launch at login's ⓘ. Two mechanisms, two descriptions: the switch
/// does the same job on both, but "Login Items" and "~/.config/autostart"
/// are not the same place to go and revoke it.
pub fn autostart_tip() -> String {
    i18n::tr(if cfg!(target_os = "macos") {
        "config.autostart_tip"
    } else {
        "config.autostart_tip_linux"
    })
}

/// What launch at login offers: a switch while the OS will honour
/// `register` / `unregister`, otherwise a sentence — one that opens Login
/// Items when approval is what is missing.
#[derive(Clone, Debug, PartialEq)]
pub enum Autostart {
    Switch(bool),
    Approve(String),
    Absent(String),
}

/// Launch at login as the row shows it (`autostart::status`).
pub fn autostart() -> Autostart {
    match autostart::status() {
        autostart::Status::Enabled | autostart::Status::NotRegistered => {
            Autostart::Switch(autostart::is_enabled())
        }
        autostart::Status::RequiresApproval => {
            Autostart::Approve(i18n::tr("config.autostart_needs_approval"))
        }
        // `NotFound` is "the mechanism is absent" on both platforms, but
        // the absence has a different name: no installed .app for
        // launchd, or no session component reading the directory.
        autostart::Status::NotFound => Autostart::Absent(i18n::tr(if cfg!(target_os = "macos") {
            "config.autostart_not_found"
        } else {
            "config.autostart_no_reader"
        })),
    }
}

/// The ⓘ on a config file's name: what clicking it does here.
pub fn file_reveal_tip() -> String {
    i18n::tr(if cfg!(target_os = "macos") {
        "config.file_reveal_tip"
    } else {
        "config.file_reveal_tip_linux"
    })
}

/// Shows where a config file lives: the file in the file manager, or its
/// folder when it does not exist yet (config.toml on a machine still on
/// builtin defaults) — the answer to "where" is still given.
pub fn reveal_file(name: &str) {
    let path = zstats::settings::default_dir().join(name);
    if path.is_file() {
        finder::reveal(&path);
    } else if let Err(e) = opener::open([path.parent().unwrap_or(&path).as_os_str()]) {
        tracing::warn!("open config dir: {e}");
    }
}

// --- The Alerts page -----------------------------------------------------------

/// One rule's value two ways: as its row prints it — the file's own, or
/// the default labelled as such — and as the chip label that value
/// reads as. The chips matched the printed form, so a rule nobody had
/// touched read "default (30%)" over five unlit chips: the one card
/// where every rule is at its default was the one that never said which
/// chip that is.
#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub shown: String,
    pub lit: String,
}

impl Setting {
    fn set(value: String) -> Self {
        Self {
            shown: value.clone(),
            lit: value,
        }
    }

    fn unset(effective: String) -> Self {
        Self {
            shown: t!("config.default_at", value = effective.clone()).to_string(),
            lit: effective,
        }
    }
}

/// A row of the thresholds card: the rule's key, its value, and how many
/// subjects override it.
#[derive(Clone, Debug, PartialEq)]
pub struct ThresholdRow {
    pub key: &'static str,
    pub setting: Setting,
    pub overrides: usize,
}

/// The ten rules the thresholds card lists, each with what runs: zstats'
/// own resolution of an unset key — "unset" alone would hide the threshold
/// actually in force. Base values are independent of the template layer
/// (templates only refine per-name overrides), so the builtin-template
/// variant is exact and costs no IO.
pub fn threshold_rows(file: &FileConfig) -> Vec<ThresholdRow> {
    let a = &file.alerts;
    let eff = ActiveThresholds::from_config(a);
    let row = |key, setting, overrides| ThresholdRow {
        key,
        setting,
        overrides,
    };
    vec![
        row(
            "alert-cpu",
            fmt_pct_setting(a.cpu, eff.cpu.base().map(f64::from)),
            a.cpu_overrides.len(),
        ),
        // Effective memory shares are fractions; the file speaks percent.
        row(
            "alert-mem",
            fmt_pct_setting(a.mem, eff.memory.base().map(|f| f * 100.0)),
            a.mem_overrides.len(),
        ),
        // The absolute half of the memory bar (zstats 0.5): the rule
        // fires on the LOWER of the share above and this ceiling.
        row(
            "alert-mem-bytes",
            fmt_bytes_setting(a.mem_bytes, eff.memory_bytes),
            0,
        ),
        row(
            "alert-app-cpu",
            fmt_pct_setting(a.app_cpu, eff.app_cpu.base().map(f64::from)),
            a.app_cpu_overrides.len(),
        ),
        row(
            "alert-app-mem",
            fmt_pct_setting(a.app_mem, eff.app_memory.base().map(|f| f * 100.0)),
            a.app_mem_overrides.len(),
        ),
        // The same ceiling one level up. It was missing here, so this card
        // said "App memory default (40%)" while the Alerts tab's armed list
        // said 8.0 GB — the ceiling biting, unnamed.
        row(
            "alert-app-mem-bytes",
            fmt_bytes_setting(a.app_mem_bytes, eff.app_memory_bytes),
            0,
        ),
        row(
            "alert-disk",
            fmt_pct_setting(a.disk, eff.disk.base().map(|f| f64::from(f) * 100.0)),
            a.disk_overrides.len(),
        ),
        // The disk rule's second line (zstats 0.7.2): a volume that keeps
        // filling is reported again, critical, once its free space is under
        // the lower of this and half the room the capacity line leaves.
        // Under capacity because it refines it and dies with it —
        // `alert-disk` 0 turns both off.
        row(
            "alert-disk-free",
            disk_free_setting(a.disk_free, eff.disk_free_bytes, eff.disk.base().is_some()),
            0,
        ),
        row(
            "alert-pressure",
            pressure_setting(a.pressure, eff.pressure),
            0,
        ),
        row(
            "alert-cooldown",
            match a.cooldown {
                Some(d) => Setting::set(format::humanize(d)),
                None => Setting::unset(format::humanize(eff.cooldown)),
            },
            0,
        ),
    ]
}

/// A rule's name on the thresholds card.
pub fn threshold_label(key: &str) -> String {
    i18n::tr(match key {
        "alert-cpu" => "config.th_cpu",
        "alert-mem" => "config.th_mem",
        "alert-mem-bytes" => "config.th_mem_bytes",
        "alert-app-cpu" => "config.th_app_cpu",
        "alert-app-mem" => "config.th_app_mem",
        "alert-app-mem-bytes" => "config.th_app_mem_bytes",
        "alert-disk" => "config.th_disk",
        "alert-disk-free" => "config.th_disk_free",
        "alert-pressure" => "config.th_pressure",
        "alert-cooldown" => "config.th_cooldown",
        // Unreachable for the ten rows above; a future key shows its own
        // name rather than a blank until it is given one.
        other => return other.to_string(),
    })
}

/// A rule's ⓘ: the key, for whoever types it into config.toml or
/// `zstats -add` — this card is where they would go looking for it.
pub fn threshold_key_tip(key: &str) -> String {
    t!("config.th_key_tip", key = key).to_string()
}

/// "3 overrides".
pub fn override_count(count: usize) -> String {
    t!("config.override", count = count).to_string()
}

/// The presets a rule's chips offer, each a label and the value written
/// with `apply_setting`. A chip is lit when its label is the row's
/// [`Setting::lit`].
pub fn threshold_choices(key: &str) -> Vec<(String, &'static str)> {
    let off = i18n::tr("alerts.off");
    match key {
        "alert-cpu" => vec![
            ("20%".into(), "20"),
            ("30%".into(), "30"),
            ("50%".into(), "50"),
            ("80%".into(), "80"),
            (off, "0"),
        ],
        "alert-mem" => vec![
            ("15%".into(), "15"),
            ("25%".into(), "25"),
            ("40%".into(), "40"),
            ("60%".into(), "60"),
            (off, "0"),
        ],
        // `g` = GiB in zstats' size parser; the decimal `gb` would store
        // values the "4 GB" display form (GiB-multiples) can't round-trip.
        "alert-mem-bytes" => vec![
            ("2 GB".into(), "2g"),
            ("4 GB".into(), "4g"),
            ("8 GB".into(), "8g"),
            (off, "0"),
        ],
        "alert-app-cpu" => vec![
            ("200%".into(), "200"),
            ("400%".into(), "400"),
            ("800%".into(), "800"),
            (off, "0"),
        ],
        "alert-app-mem" => vec![
            ("30%".into(), "30"),
            ("40%".into(), "40"),
            ("60%".into(), "60"),
            (off, "0"),
        ],
        "alert-app-mem-bytes" => vec![
            ("4 GB".into(), "4g"),
            ("8 GB".into(), "8g"),
            ("16 GB".into(), "16g"),
            (off, "0"),
        ],
        "alert-disk" => vec![
            ("80%".into(), "80"),
            ("90%".into(), "90"),
            ("95%".into(), "95"),
            (off, "0"),
        ],
        // 0 is "no floor", not "off": the half of what the capacity line
        // leaves still makes a critical line.
        "alert-disk-free" => vec![
            ("5 GB".into(), "5g"),
            ("10 GB".into(), "10g"),
            ("20 GB".into(), "20g"),
            (i18n::tr("config.th_disk_free_none"), "0"),
        ],
        "alert-pressure" => vec![
            (i18n::tr("alerts.warning"), "warning"),
            (i18n::tr("alerts.critical"), "critical"),
            (off, "off"),
        ],
        "alert-cooldown" => vec![
            ("5m".into(), "5m"),
            ("10m".into(), "10m"),
            ("30m".into(), "30m"),
        ],
        _ => Vec::new(),
    }
}

/// Every override in the file, flattened to (rule key, subject, value)
/// and ordered by rule then name — the rule is what the value means (200%
/// of one core is unremarkable for an app tree and absurd for a process),
/// and the name order is `BTreeMap`'s own, stable across renders, so a row
/// does not move under the pointer between the read and the click.
pub fn override_rows(a: &AlertsConfig) -> Vec<(&'static str, String, f64)> {
    // Percent for every one of them — zstats parses `name=30` into these
    // maps as 30, and `0` is that subject's "never alert". Two widths
    // upstream (f32 for the CPU-ish rules, f64 for the memory ones); both
    // widen to f64 here rather than the row carrying which.
    let wide = |m: &BTreeMap<String, f32>| -> Vec<(String, f64)> {
        m.iter().map(|(n, v)| (n.clone(), f64::from(*v))).collect()
    };
    let exact = |m: &BTreeMap<String, f64>| -> Vec<(String, f64)> {
        m.iter().map(|(n, v)| (n.clone(), *v)).collect()
    };
    [
        ("alert-cpu", wide(&a.cpu_overrides)),
        ("alert-mem", exact(&a.mem_overrides)),
        ("alert-app-cpu", wide(&a.app_cpu_overrides)),
        ("alert-app-mem", exact(&a.app_mem_overrides)),
        ("alert-disk", wide(&a.disk_overrides)),
    ]
    .into_iter()
    .flat_map(|(key, entries)| entries.into_iter().map(move |(name, v)| (key, name, v)))
    .collect()
}

/// An override's value: `0` is not "no value" — it is this subject opting
/// out of the rule, and reads as such.
pub fn override_value(value: f64) -> String {
    if value == 0.0 {
        i18n::tr("alerts.off")
    } else {
        format!("{value:.0}%")
    }
}

/// The file's value when set; otherwise the effective default, labeled as
/// such, so an untouched key still tells the user what the engine runs.
fn fmt_pct_setting<T: Into<f64> + Copy>(file: Option<T>, effective_pct: Option<f64>) -> Setting {
    match file {
        None => Setting::unset(match effective_pct {
            Some(v) => format!("{v:.0}%"),
            None => i18n::tr("alerts.off"),
        }),
        Some(v) if v.into() == 0.0 => Setting::set(i18n::tr("alerts.off")),
        Some(v) => Setting::set(format!("{:.0}%", v.into())),
    }
}

/// The memory rule's absolute ceiling. Values are whole GiB in practice
/// (the chips only offer those), so the clean "4 GB" form applies; a
/// hand-edited odd value falls back to the byte formatter.
fn fmt_bytes_setting(file: Option<u64>, effective: Option<u64>) -> Setting {
    const GIB: u64 = 1 << 30;
    let clean = |v: u64| {
        if v.is_multiple_of(GIB) {
            format!("{} GB", v / GIB)
        } else {
            format::memory(v)
        }
    };
    match file {
        None => Setting::unset(match effective {
            Some(v) => clean(v),
            None => i18n::tr("alerts.off"),
        }),
        Some(0) => Setting::set(i18n::tr("alerts.off")),
        Some(v) => Setting::set(clean(v)),
    }
}

/// [`fmt_bytes_setting`] for `alert-disk-free`, whose 0 means something
/// else: it removes the absolute floor and leaves the critical line at
/// half the room the capacity line leaves, where every other byte key's
/// 0 turns its rule off. Off only when the disk rule itself is, which
/// takes this line with it.
fn disk_free_setting(file: Option<u64>, effective: Option<u64>, rule_on: bool) -> Setting {
    match file {
        _ if !rule_on => Setting::set(i18n::tr("alerts.off")),
        Some(0) => Setting::set(i18n::tr("config.th_disk_free_none")),
        _ => fmt_bytes_setting(file, effective),
    }
}

/// Same for the pressure tri-state. `effective` uses zstats' encoding:
/// 2 = warning, 4 = critical, `None` = rule off.
fn pressure_setting(file: Option<PressureAlert>, effective: Option<u32>) -> Setting {
    match file {
        Some(PressureAlert::Off) => Setting::set(i18n::tr("alerts.off")),
        Some(PressureAlert::Warning) => Setting::set(i18n::tr("alerts.warning")),
        Some(PressureAlert::Critical) => Setting::set(i18n::tr("alerts.critical")),
        None => Setting::unset(match effective {
            Some(level) if level >= 4 => i18n::tr("alerts.critical"),
            Some(_) => i18n::tr("alerts.warning"),
            None => i18n::tr("alerts.off"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Five maps, two numeric widths, one row shape — and the order is
    /// what the reader scans: rule first, name inside it.
    #[test]
    fn override_rows_flatten_every_map_in_a_readable_order() {
        let mut a = AlertsConfig::default();
        a.cpu_overrides.insert("node".into(), 70.0);
        a.cpu_overrides.insert("Google Chrome".into(), 45.0);
        a.mem_overrides.insert("Xcode".into(), 25.0);
        a.app_cpu_overrides.insert("zed".into(), 600.0);
        a.app_mem_overrides.insert("Slack".into(), 12.0);
        a.disk_overrides.insert("/".into(), 95.0);
        // A base value is not an override and must not appear.
        a.cpu = Some(40.0);

        let rows = override_rows(&a);
        assert_eq!(
            rows.iter()
                .map(|(k, n, v)| (*k, n.as_str(), *v))
                .collect::<Vec<_>>(),
            vec![
                ("alert-cpu", "Google Chrome", 45.0),
                ("alert-cpu", "node", 70.0),
                ("alert-mem", "Xcode", 25.0),
                ("alert-app-cpu", "zed", 600.0),
                ("alert-app-mem", "Slack", 12.0),
                ("alert-disk", "/", 95.0),
            ]
        );
        // Nothing set is no card at all, not an empty one.
        assert!(override_rows(&AlertsConfig::default()).is_empty());
    }

    /// An untouched rule says it is the default and lights the chip the
    /// default is; a set one lights its own.
    #[test]
    fn a_default_lights_the_chip_it_is() {
        let file = FileConfig::default();
        let rows = threshold_rows(&file);
        assert_eq!(rows.len(), 10);
        let cpu = &rows[0];
        assert_eq!(cpu.key, "alert-cpu");
        assert_eq!(cpu.setting.lit, "30%");
        assert!(cpu.setting.shown.contains("30%") && cpu.setting.shown != "30%");
        assert!(
            threshold_choices("alert-cpu")
                .iter()
                .any(|(label, _)| *label == cpu.setting.lit)
        );
    }

    #[test]
    fn the_default_opacity_is_its_own_chip() {
        assert!(opacity_on(None, None));
        assert!(!opacity_on(None, Some(0.5)));
        assert!(opacity_on(Some(0.7), Some(0.70)));
        assert!(!opacity_on(Some(0.7), Some(0.8)));
    }
}
