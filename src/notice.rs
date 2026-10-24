//! Overview's notice row as a decision: whether the short form has
//! something to announce, and what. The row itself is drawn by the page
//! (`nsh/overview.rs`), but its coming and going resizes the panel, which
//! is the host's business — so the answer is here, and the page and the
//! window ask the same function.

use crate::notify;
use crate::prefs;
use crate::state::ZStatsAppState;

/// What the notice row says, most pressing first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// The menu bar is on memory ([`ZStatsAppState::memory_needs_attention`]).
    Memory,
    /// The menu bar is on a disk ([`ZStatsAppState::disk_needs_attention`]);
    /// `critical` once zstats has reported it under its critical line —
    /// "almost full" and "almost out of space" are different news
    /// (zstats 0.7.2 reports the second as its own, Critical, alert).
    Disk { critical: bool },
    /// Live alert cards that do not turn the menu bar: `count` of them,
    /// the newest named.
    Alerts { count: usize, latest: String },
    /// The sustained-load watcher is holding a card.
    Busy(String),
    /// The memory-climb watcher is holding a card.
    Growing(String),
}

/// Everything the notice row reads, already reduced to names.
#[derive(Default)]
struct Signs {
    memory: bool,
    disk: bool,
    /// The disk news is zstats' Critical report.
    disk_critical: bool,
    /// Subjects of the live alert cards, newest first.
    alerts: Vec<String>,
    busy: Vec<String>,
    growing: Vec<String>,
}

/// One sentence for the machine, when there is one to say; `None` is
/// the ordinary case and draws nothing.
///
/// It is display of verdicts already made — zstats' alert episodes, in
/// the form the menu bar and the Alerts tab already show them, then the
/// panel's two slow-burn watchers — and compares nothing itself: no
/// figure is read here, so there is no line of its own to cross. The
/// order is the menu bar's (memory before disk), then alerts that are
/// cards only, then the watchers, which are observations rather than
/// crossed thresholds.
///
/// **Every one of these is held by something with a memory** — an
/// episode, a watcher's card — so the answer changes minutes or hours
/// apart, never from one sample to the next. That is a requirement and
/// not a habit: the row's coming and going resizes the panel
/// ([`row_shown`]). The kernel's pressure level was read here too, at
/// critical, for the minutes the engine waits before it reports one;
/// it is a raw level that flaps between samples, and it is now only
/// the word beside the memory row's name.
fn decide(signs: Signs) -> Option<Notice> {
    if signs.memory {
        return Some(Notice::Memory);
    }
    if signs.disk {
        return Some(Notice::Disk {
            critical: signs.disk_critical,
        });
    }
    if let Some(latest) = signs.alerts.first() {
        return Some(Notice::Alerts {
            count: signs.alerts.len(),
            latest: latest.clone(),
        });
    }
    if let Some(name) = signs.busy.into_iter().next() {
        return Some(Notice::Busy(name));
    }
    signs.growing.into_iter().next().map(Notice::Growing)
}

/// What there is to announce: each source, less what the Alerts tab has
/// already shown (`ZStatsAppState::see_alerts`). The row is an
/// announcement and not a state. It used to stand for as long as its
/// episode did — a disk at 91% said so in red from the morning's banner
/// to midnight, 46 of the panel's height, to someone who had opened the
/// card hours before. Having been followed, or the bell having been, it
/// is done; the red figure on the card below and the red bell go on
/// saying that the condition holds.
fn signs(state: &ZStatsAppState) -> Signs {
    Signs {
        memory: state.memory_is_news(),
        disk: state.disk_is_news(),
        disk_critical: state.disk_news_is_critical(),
        alerts: state
            .alerts()
            .iter()
            .filter(|seen| seen.is_news())
            .map(|seen| notify::subject_title(&seen.event.subject))
            .collect(),
        busy: state.sustained_news(),
        growing: state.creeps_news(),
    }
}

/// What the notice row says now, if anything.
pub fn current(state: &ZStatsAppState) -> Option<Notice> {
    decide(signs(state))
}

/// Whether Overview has something to announce, in either form.
pub fn has_notice(state: &ZStatsAppState) -> bool {
    current(state).is_some()
}

/// Whether the short Overview is showing its notice row: the one fact
/// about a page the panel's window needs, since the panel is
/// [`placement::NOTICE_HEIGHT`] taller while it is. The row and the
/// height must be one answer, so both go through [`current`]. The long
/// form never has the row.
///
/// [`placement::NOTICE_HEIGHT`]: crate::placement::NOTICE_HEIGHT
pub fn row_shown(state: &ZStatsAppState) -> bool {
    !prefs::overview_detail() && has_notice(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    /// The notice names the most pressing thing, and there is none on
    /// an ordinary day.
    #[test]
    fn the_notice_names_the_most_pressing_thing() {
        let everything = || Signs {
            memory: true,
            disk: true,
            disk_critical: false,
            alerts: names(&["Google Chrome", "Zed"]),
            busy: names(&["mds"]),
            growing: names(&["Slack"]),
        };
        assert_eq!(decide(everything()), Some(Notice::Memory));
        assert_eq!(
            decide(Signs {
                memory: false,
                ..everything()
            }),
            Some(Notice::Disk { critical: false })
        );
        // The second report of a filling volume says it is running out.
        assert_eq!(
            decide(Signs {
                memory: false,
                disk_critical: true,
                ..everything()
            }),
            Some(Notice::Disk { critical: true })
        );
        // Newest first: the count is the list, the name its head.
        assert_eq!(
            decide(Signs {
                memory: false,
                disk: false,
                ..everything()
            }),
            Some(Notice::Alerts {
                count: 2,
                latest: "Google Chrome".into()
            })
        );
        assert_eq!(
            decide(Signs {
                busy: names(&["mds"]),
                growing: names(&["Slack"]),
                ..Signs::default()
            }),
            Some(Notice::Busy("mds".into()))
        );
        assert_eq!(
            decide(Signs {
                growing: names(&["Slack"]),
                ..Signs::default()
            }),
            Some(Notice::Growing("Slack".into()))
        );
        // Nothing held by an episode or a watcher: no row at all.
        assert_eq!(decide(Signs::default()), None);
    }
}
