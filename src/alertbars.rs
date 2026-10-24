//! One program's alert bars as zstats resolved them, and how the panel
//! says them: the figure an expansion's bar row prints, which layer
//! decided it, and the preset chips that edit it. Read back from zstats'
//! own `ActiveThresholds` — nothing here evaluates a condition or matches
//! a name itself. Shared since the NSH Apps page (phase 4d of
//! docs/nsh-migration.md); the gpui Alerts, Processes and Apps pages read
//! it too.

use crate::alerttpl;
use crate::format;
use crate::i18n;
use zstats::alerts::{ActiveThresholds, Template};
use zstats::settings::{AlertsConfig, FileConfig};

/// Preset chips. Labels are already localised / unit-suffixed; values are
/// what `apply_add` accepts (`0` disables a per-name rule).
pub fn presets(key: &str) -> Vec<(String, &'static str)> {
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
            (i18n::tr("alerts.warning"), "warning"),
            (i18n::tr("alerts.critical"), "critical"),
            (i18n::tr("alerts.off"), "off"),
        ],
        _ => Vec::new(),
    }
}

/// Which layer zstats took one subject's alert bar from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarSource {
    /// An entry of the user's own under `[alerts.*_overrides]`.
    Yours,
    /// A row of the alert table (compiled in, or `template.toml`).
    Table,
    /// No per-name entry: the global value.
    Base,
}

/// One subject's bar the way zstats resolved it: the preset chip that
/// stands for it, when one does, and the layer that decided it.
#[derive(Debug, PartialEq, Eq)]
pub struct InForce {
    pub lit: Option<String>,
    pub source: BarSource,
}

/// zstats' resolution of the per-name bars, held for one paint.
///
/// The chips under a bar used to light the file's own value — the
/// user's entry, else the global value, else the default — which leaves
/// the alert table out, while the bar above them is `ActiveThresholds`
/// with the table in. For every program the table names the two
/// disagreed: "CPU alert off" over a lit 200%. And a lit global memory
/// share was not the bar either — that is the lower of the share and
/// the ceiling — so clicking the lit 25% moved the bar from 4 GB to
/// 6 GB.
///
/// Both read this now. zstats is asked twice: as the engine armed
/// itself, and against an empty table, which leaves only the user's
/// entries. The layer is then zstats' own answer — a name the second
/// matches was decided by the user (its doc: "a user override always
/// beats a matching template entry"), one only the first matches by the
/// table — and nothing here matches a name itself. A user key may be a
/// pattern (`name*`), which a `contains_key` on the file would miss.
pub struct Resolved {
    eff: ActiveThresholds,
    own: ActiveThresholds,
}

impl Resolved {
    pub fn of(settings: &FileConfig) -> Self {
        Self::against(&settings.alerts, &alerttpl::info().template)
    }

    fn against(alerts: &AlertsConfig, table: &Template) -> Self {
        Self {
            eff: ActiveThresholds::from_config_with_template(alerts, table),
            own: ActiveThresholds::from_config_with_template(alerts, &Template::default()),
        }
    }

    /// The bars the engine is running with.
    pub fn eff(&self) -> &ActiveThresholds {
        &self.eff
    }

    /// `None` for a key with no per-name table layer (disk, pressure).
    pub fn in_force(&self, key: &str, name: &str) -> Option<InForce> {
        // The bar as a percent, whether an entry of the user's set it,
        // and the global value where a chip can stand for it.
        let (matched, yours, base) = match key {
            "alert-cpu" => (
                self.eff.cpu.override_for(name).map(|v| v.map(f64::from)),
                self.own.cpu.override_for(name).is_some(),
                Some(self.eff.cpu.base().map(f64::from)),
            ),
            "alert-app-cpu" => (
                self.eff
                    .app_cpu
                    .override_for(name)
                    .map(|v| v.map(f64::from)),
                self.own.app_cpu.override_for(name).is_some(),
                Some(self.eff.app_cpu.base().map(f64::from)),
            ),
            // A global memory bar is the lower of the share and the
            // ceiling: no chip is that figure, unless the rule is off.
            "alert-mem" => (
                self.eff
                    .memory
                    .override_for(name)
                    .map(|v| v.map(|share| share * 100.0)),
                self.own.memory.override_for(name).is_some(),
                self.eff.memory.base().is_none().then_some(None),
            ),
            "alert-app-mem" => (
                self.eff
                    .app_memory
                    .override_for(name)
                    .map(|v| v.map(|share| share * 100.0)),
                self.own.app_memory.override_for(name).is_some(),
                self.eff.app_memory.base().is_none().then_some(None),
            ),
            _ => return None,
        };
        let source = match (yours, matched.is_some()) {
            (true, _) => BarSource::Yours,
            (false, true) => BarSource::Table,
            (false, false) => BarSource::Base,
        };
        Some(InForce {
            lit: lit_value(matched, base),
            source,
        })
    }
}

/// The preset value (`apply_add`'s spelling, `0` for off) a bar reads
/// as. `matched` is zstats' per-name answer; `base` is the global value
/// when a chip may stand for it, absent when none can.
fn lit_value(matched: Option<Option<f64>>, base: Option<Option<f64>>) -> Option<String> {
    let spell = |bar: Option<f64>| bar.map_or_else(|| "0".to_string(), |v| format!("{v:.0}"));
    match matched {
        Some(bar) => Some(spell(bar)),
        None => base.map(spell),
    }
}

/// The figure an alert-bar row paints, and whether a per-name entry
/// decided it. Surface is the number (or "off"); `overridden` is the
/// word and ⓘ beside the label — putting "override" in the figure
/// itself is what used to truncate "CPU alert" to "CPU…" at 320px.
/// `yours` picks the word: the user's own entry, or the alert table's.
#[derive(Clone, Debug, PartialEq)]
pub struct BarDisplay {
    pub text: String,
    pub overridden: bool,
    pub yours: bool,
}

impl BarDisplay {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            overridden: false,
            yours: false,
        }
    }

    fn of(text: impl Into<String>, overridden: bool) -> Self {
        Self {
            text: text.into(),
            overridden,
            yours: false,
        }
    }

    /// Name the layer that decided the bar ([`Resolved::in_force`]).
    pub fn decided_by(mut self, held: Option<InForce>) -> Self {
        self.yours = held.is_some_and(|h| h.source == BarSource::Yours);
        self
    }
}

/// A percent-style alert bar (CPU on the Processes expansion, app CPU on
/// the Apps one), resolved from what zstats' own `Thresholds` said about
/// this name. `overridden` means a per-name entry matched — the user's
/// own or the template table's. Which of the two is
/// [`BarDisplay::decided_by`]'s to say, from zstats' own second answer:
/// re-matching the name here would be a second implementation of its
/// precedence.
pub fn pct_bar_text(matched: Option<Option<f32>>, base: Option<f32>) -> BarDisplay {
    match matched {
        Some(Some(v)) => BarDisplay::of(format!("{v:.0}%"), true),
        Some(None) => BarDisplay::of(i18n::tr("processes.bar_rule_off"), true),
        None => match base {
            Some(v) => BarDisplay::plain(format!("{v:.0}%")),
            None => BarDisplay::plain(i18n::tr("processes.bar_rule_off")),
        },
    }
}

/// Same vocabulary for the memory bar, whose effective figure is bytes
/// (zstats resolves share-vs-ceiling itself — `bytes` is its answer for
/// this name). `base_share` is the fallback wording for the frame or
/// two before a tick has reported a total to resolve against.
pub fn mem_bar_text(
    matched: Option<Option<f64>>,
    bytes: Option<u64>,
    base_share: Option<f64>,
) -> BarDisplay {
    match matched {
        Some(Some(_)) => match bytes {
            Some(b) => BarDisplay::of(format::memory(b), true),
            None => BarDisplay::plain(i18n::tr("processes.bar_rule_off")),
        },
        Some(None) => BarDisplay::of(i18n::tr("processes.bar_rule_off"), true),
        None => match bytes {
            Some(b) => BarDisplay::plain(format::memory(b)),
            None => match base_share {
                Some(f) => BarDisplay::plain(format!("{:.0}%", f * 100.0)),
                None => BarDisplay::plain(i18n::tr("processes.bar_rule_off")),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(lit: Option<&str>, source: BarSource) -> Option<InForce> {
        Some(InForce {
            lit: lit.map(str::to_string),
            source,
        })
    }

    /// The chips light what the bar reads, and the word beside the bar
    /// names the layer — both from the one resolution. The table's
    /// "off" over a lit 200% was the bug this replaced.
    #[test]
    fn the_chips_light_the_bar_zstats_resolved() {
        let mut table = Template::default();
        table.app_cpu.insert("claude".into(), 0.0);
        table.mem.insert("Xcode".into(), 60.0);
        let mut alerts = AlertsConfig {
            app_cpu: Some(200.0),
            ..Default::default()
        };

        let bars = Resolved::against(&alerts, &table);
        // The table switched it off: the Off chip, and not the user's.
        assert_eq!(
            bars.in_force("alert-app-cpu", "claude"),
            held(Some("0"), BarSource::Table)
        );
        assert_eq!(
            bars.in_force("alert-mem", "Xcode"),
            held(Some("60"), BarSource::Table)
        );
        // No entry: a CPU bar is the global value, which is a chip.
        assert_eq!(
            bars.in_force("alert-app-cpu", "Finder"),
            held(Some("200"), BarSource::Base)
        );
        // A global memory bar is the lower of the share and the ceiling,
        // so no chip is it — lighting the share made a click on the lit
        // chip move the bar.
        assert_eq!(
            bars.in_force("alert-mem", "Finder"),
            held(None, BarSource::Base)
        );
        assert_eq!(
            bars.in_force("alert-app-mem", "Finder"),
            held(None, BarSource::Base)
        );

        // The user's entry wins over the table's — a pattern included,
        // which is why the file's keys are not searched for the name.
        alerts.app_cpu_overrides.insert("clau*".into(), 800.0);
        alerts.mem_overrides.insert("Finder".into(), 40.0);
        let bars = Resolved::against(&alerts, &table);
        assert_eq!(
            bars.in_force("alert-app-cpu", "claude"),
            held(Some("800"), BarSource::Yours)
        );
        assert_eq!(
            bars.in_force("alert-mem", "Finder"),
            held(Some("40"), BarSource::Yours)
        );
        // Disk and pressure have no table layer to resolve.
        assert_eq!(bars.in_force("alert-disk", "/"), None);
    }

    /// With the global rule off the bar reads "off" for every name, and
    /// the chip that says so is the one lit.
    #[test]
    fn a_disabled_rule_lights_off() {
        let alerts = AlertsConfig {
            cpu: Some(0.0),
            mem: Some(0.0),
            ..Default::default()
        };
        let bars = Resolved::against(&alerts, &Template::default());
        assert_eq!(
            bars.in_force("alert-cpu", "Finder"),
            held(Some("0"), BarSource::Base)
        );
        assert_eq!(
            bars.in_force("alert-mem", "Finder"),
            held(Some("0"), BarSource::Base)
        );
    }

    /// The two pages edit two different pairs of rules, and mixing them
    /// up would write a process bar onto an app tree — 200% is
    /// unremarkable for a tree and absurd for one process. The editor
    /// takes the keys from its caller precisely so this stays a fact
    /// about the call sites rather than a guess inside the widget.
    #[test]
    fn each_page_edits_its_own_pair_of_rules() {
        for key in ["alert-cpu", "alert-mem", "alert-app-cpu", "alert-app-mem"] {
            let presets = presets(key);
            assert!(!presets.is_empty(), "{key} has no chips to offer");
            // Every rule can be turned off for one subject, and that is
            // what `0` means to `apply_add` — not "unset".
            assert!(
                presets.iter().any(|(_, v)| *v == "0"),
                "{key} must offer an off chip"
            );
        }
    }

    /// The expansion's alert-bar rows answer "why is this at 300% and
    /// quiet" — so a matched per-name entry (user's or the template's)
    /// must be distinguishable from the base rule, and a disabled rule
    /// from one that simply sits high. The figure is just the number;
    /// `overridden` is what lights the ⓘ.
    #[test]
    fn a_bar_says_whether_a_per_name_entry_decided_it() {
        let base = pct_bar_text(None, Some(80.0));
        assert_eq!(base.text, "80%");
        assert!(!base.overridden);
        // kernel_task: a matched entry, the row the question is about.
        let over = pct_bar_text(Some(Some(300.0)), Some(80.0));
        assert_eq!(over.text, "300%");
        assert!(over.overridden);
        let off = pct_bar_text(Some(None), Some(80.0));
        assert_eq!(off.text, "off");
        assert!(off.overridden);
        let rule_off = pct_bar_text(None, None);
        assert_eq!(rule_off.text, "off");
        assert!(!rule_off.overridden);
    }

    #[test]
    fn the_memory_bar_prefers_zstats_resolved_bytes() {
        // zstats resolves share-vs-ceiling itself; the row shows its
        // answer, never a locally derived one.
        let base = mem_bar_text(None, Some(4 * 1024 * 1024 * 1024), Some(0.25));
        assert_eq!(base.text, "4.0 GB");
        assert!(!base.overridden);
        let over = mem_bar_text(Some(Some(0.7)), Some(45 * 1024 * 1024 * 1024), Some(0.25));
        assert_eq!(over.text, "45.0 GB");
        assert!(over.overridden);
        let off = mem_bar_text(Some(None), None, Some(0.25));
        assert_eq!(off.text, "off");
        assert!(off.overridden);
        // Before the first tick there is no total to resolve against —
        // the share is all that can be said honestly.
        let share = mem_bar_text(None, None, Some(0.25));
        assert_eq!(share.text, "25%");
        assert!(!share.overridden);
        let rule_off = mem_bar_text(None, None, None);
        assert_eq!(rule_off.text, "off");
        assert!(!rule_off.overridden);
    }

    /// The word beside a matched bar is "yours" only for the user's own
    /// entry; the alert table's, and a key with no layers to tell apart,
    /// stay the other word.
    #[test]
    fn the_word_beside_a_bar_names_the_layer() {
        let bar = |source| {
            pct_bar_text(Some(Some(300.0)), Some(80.0))
                .decided_by(Some(InForce { lit: None, source }))
        };
        assert!(bar(BarSource::Yours).yours);
        assert!(!bar(BarSource::Table).yours);
        assert!(!pct_bar_text(Some(None), Some(80.0)).decided_by(None).yours);
    }
}
