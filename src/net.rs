//! The interface card's rules: which interfaces get a row, in what order
//! and against what scale, and the words for their errors, their packets
//! and the idle ones. Every figure is zstats' own (`NetworkSnapshot`); this
//! picks and words, it derives nothing. Moved out of the gpui card for the
//! NSH Network page (phase 4g of docs/nsh-migration.md); the gpui card,
//! `views/net.rs`, says why it looks the way it does.

use crate::i18n;
use crate::state::ZStatsAppState;
use rust_i18n::t;
use zstats::snapshot::NetworkSnapshot;

/// Floor for the bar scale, below which the busiest row does not get to
/// define "full".
///
/// The track used to be a fixed 10 MB/s, which is a rate a laptop link
/// almost never sustains: at a normal 11 kB/s the fill was 0.1% — every
/// bar on the page painted empty, so fourteen tracks carried no
/// information at all. The scale is the page's own maximum now, the same
/// shape as the process list's, and this floor is what stops a machine
/// doing 3 kB/s of housekeeping from painting a full bar and reading as
/// saturated. 64 KiB/s is roughly where traffic starts being worth
/// seeing.
pub const SCALE_FLOOR_BYTES: f32 = 64.0 * 1024.0;

/// How many idle names the compact line keeps before an ellipsis.
/// Enough to show the usual suspects (awdl, bridges, a couple of
/// vmenet) without becoming a second table.
const IDLE_NAMES: usize = 8;

/// The card's rows.
pub struct Ifaces<'a> {
    /// A full row each, ranked ([`rank`])
    pub full: Vec<&'a NetworkSnapshot>,
    /// While expanded, the ones with nothing to show, as one line of
    /// names ([`idle_line`])
    pub compact: Vec<&'a NetworkSnapshot>,
    /// How many rows the filter *would* hide, whether or not they are on
    /// screen — so the chip stays a toggle in both states
    pub hideable: usize,
    /// What a full bar is ([`scale_for`])
    pub scale: f32,
}

/// Which interfaces the card lists. Only those that have moved bytes
/// recently (`state::net_is_recent`) until the chip asks for all: a
/// machine lists a pile of permanently silent ones — unused Ethernet,
/// tunnels, bridges — and they bury the two that matter. One rule, one
/// count: two filters used to run in sequence and the chip only knew
/// about the second, so it could report "nothing hidden" while the first
/// had quietly dropped 27 of 32 interfaces.
///
/// Expanding used to dump every silent bridge and VM adapter as a full
/// 0 B/s row, burying the two or three that actually move. Full rows
/// stay on interfaces that are live *or* recently so; the rest collapse
/// to a name line.
pub fn ifaces<'a>(state: &ZStatsAppState, nets: &'a [NetworkSnapshot]) -> Ifaces<'a> {
    let show_all = state.show_unused_nets();
    let hideable = nets
        .iter()
        .filter(|n| !state.net_is_recent(&n.interface))
        .count();
    let mut rows: Vec<_> = nets
        .iter()
        .filter(|n| show_all || state.net_is_recent(&n.interface))
        .collect();
    rank(&mut rows);
    let scale = scale_for(&rows);
    let (full, compact) = if show_all {
        rows.into_iter()
            .partition(|n| active(n) || state.net_is_recent(&n.interface))
    } else {
        (rows, Vec::new())
    };
    Ifaces {
        full,
        compact,
        hideable,
        scale,
    }
}

/// Moving bytes on this sample, either way.
pub fn active(n: &NetworkSnapshot) -> bool {
    n.received_bytes_per_sec + n.transmitted_bytes_per_sec > 0
}

/// Busiest first among the interfaces that reach a wire, then loopback.
pub fn rank(rows: &mut [&NetworkSnapshot]) {
    let total = |n: &NetworkSnapshot| {
        n.received_bytes_per_sec
            .saturating_add(n.transmitted_bytes_per_sec)
    };
    rows.sort_by(|a, b| {
        a.is_loopback
            .cmp(&b.is_loopback)
            .then_with(|| total(b).cmp(&total(a)))
            .then_with(|| a.interface.cmp(&b.interface))
    });
}

/// Both directions share one scale, so ↓ and ↑ can be read against each
/// other as well as against the other rows.
pub fn scale_for(rows: &[&NetworkSnapshot]) -> f32 {
    rows.iter()
        // Loopback does not get to define "full": at memory-copy speed
        // it would empty every bar that measures a real link.
        .filter(|n| !n.is_loopback)
        .flat_map(|n| [n.received_bytes_per_sec, n.transmitted_bytes_per_sec])
        .max()
        .map_or(SCALE_FLOOR_BYTES, |peak| {
            (peak as f32).max(SCALE_FLOOR_BYTES)
        })
}

/// How much of a bar `bytes_per_sec` fills against `scale`.
pub fn fraction(bytes_per_sec: u64, scale: f32) -> f32 {
    (bytes_per_sec as f32 / scale).clamp(0.0, 1.0)
}

/// Error rates only when something is actually erroring. A healthy
/// interface is not a row of zeros. Display only — zstats has no
/// error-rate alert.
pub fn errors_label(n: &NetworkSnapshot) -> Option<String> {
    match (n.received_errors_per_sec, n.transmitted_errors_per_sec) {
        (None, None) => None,
        (rx, tx) => {
            let rx = rx.unwrap_or(0);
            let tx = tx.unwrap_or(0);
            (rx + tx > 0).then(|| t!("net.errors", rx = rx, tx = tx).to_string())
        }
    }
}

/// The rates' tooltip: packets a second, when zstats reported them.
pub fn packets_tip(n: &NetworkSnapshot) -> Option<String> {
    match (n.received_packets_per_sec, n.transmitted_packets_per_sec) {
        (None, None) => None,
        (rx, tx) => Some(
            t!(
                "net.packets_tip",
                rx = rx.unwrap_or(0),
                tx = tx.unwrap_or(0)
            )
            .to_string(),
        ),
    }
}

/// The idle interfaces' one line, the first [`IDLE_NAMES`] of them.
pub fn idle_line(idle: &[&NetworkSnapshot]) -> Option<String> {
    if idle.is_empty() {
        return None;
    }
    let mut names: Vec<&str> = idle
        .iter()
        .take(IDLE_NAMES)
        .map(|n| n.interface.as_str())
        .collect();
    if idle.len() > IDLE_NAMES {
        names.push("…");
    }
    Some(t!("net.idle_compact", names = names.join(" · ")).to_string())
}

/// The chip's words, or `None` when nothing is hidden — then the card says
/// so in a note where the chip would be.
pub fn more_label(hideable: usize, showing: bool) -> Option<String> {
    if hideable == 0 {
        return None;
    }
    Some(if showing {
        i18n::tr("net.hide_more")
    } else {
        t!("net.show_more", count = hideable).to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(rx: u64, tx: u64) -> NetworkSnapshot {
        NetworkSnapshot {
            interface: "en0".into(),
            is_loopback: false,
            received_bytes_per_sec: rx,
            transmitted_bytes_per_sec: tx,
            received_packets_per_sec: None,
            transmitted_packets_per_sec: None,
            received_errors_per_sec: None,
            transmitted_errors_per_sec: None,
        }
    }

    /// The track means "against the busiest thing on this page", but not
    /// below a rate worth drawing: a quiet machine must not paint a full
    /// bar for a trickle, and a real download must not peg every row.
    #[test]
    fn the_track_follows_the_page_but_never_below_the_floor() {
        let quiet = [net(11_000, 9_000), net(3_000, 7_000)];
        let scale = scale_for(&quiet.iter().collect::<Vec<_>>());
        assert_eq!(scale, SCALE_FLOOR_BYTES, "11 kB/s does not get to be full");
        assert!(11_000.0 / scale > 0.1, "and is still visible");

        let busy = [net(5 * 1024 * 1024, 0), net(1024 * 1024, 0)];
        let scale = scale_for(&busy.iter().collect::<Vec<_>>());
        assert_eq!(scale, 5.0 * 1024.0 * 1024.0, "the busiest row defines full");

        // An empty page still divides by something.
        assert_eq!(scale_for(&[]), SCALE_FLOOR_BYTES);
    }

    /// A build cache talking to itself over 127.0.0.1 is the busiest
    /// thing on the card and no part of the network: it goes last, and
    /// the real link beside it keeps a bar it can be read from.
    #[test]
    fn loopback_goes_last_and_does_not_set_the_scale() {
        let mut local = net(5_000_000_000, 5_000_000_000);
        local.interface = "lo0".into();
        local.is_loopback = true;
        let mut wifi = net(2 * 1024 * 1024, 100_000);
        wifi.interface = "en0".into();
        let mut tunnel = net(300_000, 50_000);
        tunnel.interface = "utun10".into();

        let mut rows = vec![&local, &tunnel, &wifi];
        rank(&mut rows);
        let names: Vec<&str> = rows.iter().map(|n| n.interface.as_str()).collect();
        assert_eq!(names, ["en0", "utun10", "lo0"]);
        assert_eq!(scale_for(&rows), 2.0 * 1024.0 * 1024.0, "en0 is full");
        assert_eq!(scale_for(&[&local]), SCALE_FLOOR_BYTES);
    }

    #[test]
    fn error_rates_stay_off_a_healthy_row() {
        let quiet = net(1000, 1000);
        assert!(errors_label(&quiet).is_none());
        assert!(packets_tip(&quiet).is_none());
        let mut noisy = net(1000, 1000);
        noisy.received_errors_per_sec = Some(12);
        noisy.transmitted_errors_per_sec = Some(0);
        assert!(errors_label(&noisy).unwrap().contains("12"));
        noisy.received_packets_per_sec = Some(800);
        noisy.transmitted_packets_per_sec = Some(90);
        assert!(packets_tip(&noisy).is_some());
    }

    #[test]
    fn the_idle_line_names_eight_and_then_stops() {
        assert_eq!(idle_line(&[]), None);
        let idle: Vec<NetworkSnapshot> = (0..10)
            .map(|i| NetworkSnapshot {
                interface: format!("vmenet{i}"),
                ..net(0, 0)
            })
            .collect();
        let line = idle_line(&idle.iter().collect::<Vec<_>>()).unwrap();
        assert!(
            line.contains("vmenet7") && !line.contains("vmenet8"),
            "{line}"
        );
        assert!(line.contains('…'), "{line}");
    }
}
