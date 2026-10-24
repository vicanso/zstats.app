//! The settings window's rules, on either front end: its pages and their
//! order, and what each picker on them offers — the options, the one in
//! force, and the words that go with them. Shared since the NSH settings
//! window (phase 4j of docs/nsh-migration.md); `views/config.rs` reads it
//! too, and says why each row is the way it is.

use crate::prefs::{self, LanguagePref, ThemePref, TrayPref};
use crate::{autostart, finder, i18n, omarchy, opener};
use rust_i18n::t;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_opacity_is_its_own_chip() {
        assert!(opacity_on(None, None));
        assert!(!opacity_on(None, Some(0.5)));
        assert!(opacity_on(Some(0.7), Some(0.70)));
        assert!(!opacity_on(Some(0.7), Some(0.8)));
    }
}
