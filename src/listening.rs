//! The listening card's rules, on either front end: a row per process
//! name, how far each endpoint reaches and how it is written, what a
//! search keeps, which rows the preview shows, and the one process a
//! row's Quit may act on. The list is `zstats::listeners()`, read by the
//! store (`state::ensure_listeners`); this groups and words it and
//! derives no figure. Shared since the NSH Network page (phase 4g of
//! docs/nsh-migration.md); `views/listening.rs` reads it too, and says why
//! the card looks the way it does.

use crate::terminate;
use crate::{format, i18n};
use rust_i18n::t;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;
use zstats::listeners::Protocol;
use zstats::{Listeners, OwnerCoverage};

/// Rows shown before "show more" — a hard cap. Exposed rows sort first, so
/// they are what the preview shows, but unlike the sensor preview's hot
/// rows they are not exempt from the cut: listening on every interface is
/// ordinary on a development machine (every row but three was, measured
/// here), and an exemption turned the preview into the whole list.
const PREVIEW_ROWS: usize = 6;

/// What the card lists, and its "show more" chip.
pub struct Card {
    pub rows: Vec<Row>,
    /// `None` beside a search — there is nothing more to show — and when
    /// the preview is already the whole answer.
    pub chip: Option<Chip>,
}

/// The chip that brings in what the preview leaves out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chip {
    /// Rows the preview leaves out, counted against the collapsed view
    /// whichever is showing, so the chip stays a toggle once expanded
    pub hidden: usize,
    pub showing: bool,
}

impl Chip {
    /// "Show 4 more", or "Show UDP" when the rows are all there and only
    /// their UDP endpoints are not, and "Show less" while expanded.
    pub fn label(self) -> String {
        if self.showing {
            i18n::tr("listen.hide")
        } else if self.hidden > 0 {
            t!("listen.show_more", count = self.hidden).to_string()
        } else {
            i18n::tr("listen.show_udp")
        }
    }
}

/// The card for a lowercased search (`state::listen_filter_text`, empty
/// while closed) and the "show more" toggle.
pub fn card(listeners: &Listeners, query: &str, show_all: bool) -> Card {
    if !query.is_empty() {
        // Every match, UDP included: the preview's cap would hide exactly
        // what was asked for.
        return Card {
            rows: matching(rows(listeners, true), query),
            chip: None,
        };
    }
    let all = rows(listeners, true);
    let mut preview = rows(listeners, false);
    preview.truncate(PREVIEW_ROWS);
    // A process that is in both but gains UDP endpoints is not a hidden
    // row; "show UDP" covers that.
    let hidden = all.len().saturating_sub(preview.len());
    let has_udp = listeners
        .sockets
        .iter()
        .any(|s| s.protocol == Protocol::Udp);
    Card {
        rows: if show_all { all } else { preview },
        chip: (hidden > 0 || has_udp).then_some(Chip {
            hidden,
            showing: show_all,
        }),
    }
}

/// The ⓘ beside the title, and the sentence a partial answer adds.
pub fn tip(coverage: Option<OwnerCoverage>) -> String {
    let tip = i18n::tr("listen.tip");
    if coverage == Some(OwnerCoverage::OwnProcessesOnly) {
        return format!("{tip} {}", i18n::tr("listen.tip_partial"));
    }
    tip
}

/// The one process a row's Quit would act on, if it may be offered at all.
///
/// Only a row that stands for exactly one named process. `×17` is
/// seventeen processes, and one click ending all of them would act on more
/// than a row names — the rule the process page keeps for its own Quit.
/// A search narrows `×N` to the holders of what matched, which is how one
/// of them gets its button. A pid with no name is never offered: the name
/// is what the delivering end checks the pid against. Then the two gates
/// the process page uses, plus the kernel's own permission check, so a
/// root-owned listener (`mDNSResponder`) shows no control that could
/// only fail.
pub fn quit_target(row: &Row) -> Option<(u32, String)> {
    let Owner::Name(name) = &row.owner else {
        return None;
    };
    if row.pids.len() != 1 {
        return None;
    }
    let pid = *row.pids.first()?;
    (terminate::can_term(pid) && terminate::can_quit(pid)).then(|| (pid, name.clone()))
}

/// How far a socket reaches. Declaration order is the sort order: the
/// rows and endpoints that reach beyond this Mac come first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Scope {
    /// `0.0.0.0` / `::` — every interface, other machines included
    Every,
    /// One non-loopback address — reachable on that network
    Specific(IpAddr),
    /// Loopback — this Mac only
    Local,
}

impl Scope {
    fn of(address: IpAddr) -> Self {
        // `::ffff:127.0.0.1` is loopback too; judge the address the
        // mapping stands for.
        let address = address.to_canonical();
        if address.is_unspecified() {
            Scope::Every
        } else if address.is_loopback() {
            Scope::Local
        } else {
            Scope::Specific(address)
        }
    }
}

/// One address and port a process listens on, with its IPv4/IPv6 twins
/// already folded together by [`Scope`]: `*:7777` over both families is
/// one endpoint here, as it is one service to the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Endpoint {
    scope: Scope,
    port: u16,
    udp: bool,
}

impl Endpoint {
    /// What a search looks through: the label as shown, the addresses
    /// `*` and `localhost` stand for (so `0.0.0.0` and `127.0.0.1` find
    /// what they mean), and the protocol by name (TCP has no suffix on
    /// screen, but `tcp` should still narrow to it).
    fn haystack(&self) -> String {
        let aliases = match self.scope {
            Scope::Every => "0.0.0.0 ::",
            Scope::Local => "127.0.0.1 ::1",
            Scope::Specific(_) => "",
        };
        let proto = if self.udp { "udp" } else { "tcp" };
        format!("{} {aliases} {proto}", self.label()).to_lowercase()
    }

    pub fn label(&self) -> String {
        let host = match self.scope {
            Scope::Every => "*".to_string(),
            Scope::Local => "localhost".to_string(),
            Scope::Specific(IpAddr::V4(a)) => a.to_string(),
            Scope::Specific(IpAddr::V6(a)) => format!("[{a}]"),
        };
        let proto = if self.udp { "/udp" } else { "" };
        format!("{host}:{}{proto}", self.port)
    }
}

/// What a row groups by. A name when the platform gave one — so twenty
/// `redis-server`s are one row — a pid when the process exited before its
/// name was read, and the user when there is no pid at all (on Linux,
/// another user's process the kernel would not name).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Owner {
    Name(String),
    Pid(u32),
    User(Option<u32>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub owner: Owner,
    /// Every process the row stands for
    pub pids: BTreeSet<u32>,
    /// Sorted: exposed first, then port, TCP before UDP — each with the
    /// pids holding it
    pub endpoints: Vec<(Endpoint, BTreeSet<u32>)>,
    /// How long each of `pids` had been running when the list was read —
    /// zstats' figure, from the entry the name came from
    pub ages: BTreeMap<u32, u64>,
}

impl Row {
    pub fn exposed(&self) -> bool {
        self.endpoints.iter().any(|(e, _)| e.scope != Scope::Local)
    }

    /// `pid N` for a one-process row, `×N` for a name that stands for
    /// several, nothing when no pid is known. Spelled out like every other
    /// pid in the panel: a bare number beside a name read as a count.
    pub fn pid_label(&self) -> Option<String> {
        match self.pids.len() {
            0 => None,
            1 => self
                .pids
                .first()
                .map(|pid| t!("processes.pid_only", pid = pid).to_string()),
            n => Some(format!("×{n}")),
        }
    }

    /// `up 3d` for one process — the largest unit only, as Overview's
    /// uptime reads: a row is glanced at, and "11d" answers "how long has
    /// this been up" as well as "11d 10h" does in half the width. For a
    /// name that stands for several, the newest and the oldest —
    /// `up 5m–12d`: twenty servers started at
    /// twenty different times have no single age, and the youngest is the
    /// one that just restarted. `None` when zstats knew no age (no pid, or
    /// the process was gone before its entry was read).
    pub fn age_label(&self) -> Option<String> {
        let newest = format::uptime_short(*self.ages.values().min()?);
        let oldest = format::uptime_short(*self.ages.values().max()?);
        Some(if newest == oldest {
            t!("listen.up", time = newest).to_string()
        } else {
            t!("listen.up_range", newest = newest, oldest = oldest).to_string()
        })
    }

    pub fn owner_label(&self) -> String {
        match &self.owner {
            Owner::Name(name) => name.clone(),
            Owner::Pid(_) | Owner::User(None) => i18n::tr("listen.unknown_owner"),
            Owner::User(Some(uid)) => t!("listen.other_user", uid = uid).to_string(),
        }
    }

    /// The second line: every endpoint, two spaces apart.
    pub fn endpoints_line(&self) -> String {
        self.labels().join("  ")
    }

    /// The endpoints' tooltip, a line each. Several processes under one
    /// name: this is where each endpoint gets its pid back, which is what
    /// a kill or a `lsof -p` needs.
    pub fn endpoint_lines(&self) -> Vec<String> {
        let several = self.pids.len() > 1;
        self.endpoints
            .iter()
            .map(|(endpoint, pids)| {
                if several {
                    let pids: Vec<String> = pids.iter().map(u32::to_string).collect();
                    format!("{}  ·  {}", endpoint.label(), pids.join(", "))
                } else {
                    endpoint.label()
                }
            })
            .collect()
    }

    /// What the age beside the name is, for one process or for several.
    pub fn age_tip(&self) -> String {
        if self.pids.len() > 1 {
            t!("listen.up_range_tip", count = self.pids.len()).to_string()
        } else {
            i18n::tr("listen.up_tip")
        }
    }

    /// The confirm's body for this row's Quit (`quit_target`'s `pid` and
    /// `name`): it names the endpoints too, so it says what will stop
    /// answering, not just which process.
    pub fn quit_body(&self, pid: u32, name: &str) -> String {
        t!(
            "listen.quit_body",
            name = name,
            pid = pid,
            endpoints = self.labels().join(", ")
        )
        .to_string()
    }

    fn labels(&self) -> Vec<String> {
        self.endpoints.iter().map(|(e, _)| e.label()).collect()
    }

    fn lowest_port(&self) -> u16 {
        self.endpoints
            .iter()
            .map(|(e, _)| e.port)
            .min()
            .unwrap_or(0)
    }
}

/// One row per process name, exposed rows first, then by the lowest port,
/// then by name. `udp` off drops UDP sockets, and a name left with nothing
/// drops with them.
fn rows(listeners: &Listeners, udp: bool) -> Vec<Row> {
    type Holders = BTreeMap<Endpoint, BTreeSet<u32>>;
    type Ages = BTreeMap<u32, u64>;
    let mut grouped: HashMap<Owner, (BTreeSet<u32>, Holders, Ages)> = HashMap::new();
    for socket in &listeners.sockets {
        let is_udp = socket.protocol == Protocol::Udp;
        if is_udp && !udp {
            continue;
        }
        let owner = match (&socket.process, socket.pid) {
            (Some(name), _) => Owner::Name(name.clone()),
            (None, Some(pid)) => Owner::Pid(pid),
            (None, None) => Owner::User(socket.uid),
        };
        let (pids, holders, ages) = grouped.entry(owner).or_default();
        let endpoint = Endpoint {
            scope: Scope::of(socket.address),
            port: socket.port,
            udp: is_udp,
        };
        let held = holders.entry(endpoint).or_default();
        if let Some(pid) = socket.pid {
            pids.insert(pid);
            held.insert(pid);
            if let Some(secs) = socket.run_time_secs {
                ages.insert(pid, secs);
            }
        }
    }
    let mut rows: Vec<Row> = grouped
        .into_iter()
        .map(|(owner, (pids, holders, ages))| Row {
            owner,
            pids,
            endpoints: holders.into_iter().collect(),
            ages,
        })
        .collect();
    rows.sort_by(|a, b| {
        (!a.exposed(), a.lowest_port(), a.owner_label()).cmp(&(
            !b.exposed(),
            b.lowest_port(),
            b.owner_label(),
        ))
    });
    rows
}

/// The rows a lowercased search keeps. A row whose name contains the
/// query stays whole. Otherwise it keeps only the endpoints that matched —
/// by address, port or protocol, or by an exact pid among their holders
/// (a partial pid would match half the table) — and its pids narrow to the
/// ones still holding something, so `×19` does not survive a search that
/// found one of them. Order is the rows' own.
fn matching(rows: Vec<Row>, query: &str) -> Vec<Row> {
    if query.is_empty() {
        return rows;
    }
    rows.into_iter()
        .filter_map(|mut row| {
            if row.owner_label().to_lowercase().contains(query) {
                return Some(row);
            }
            row.endpoints.retain(|(endpoint, pids)| {
                endpoint.haystack().contains(query) || pids.iter().any(|p| p.to_string() == query)
            });
            if row.endpoints.is_empty() {
                return None;
            }
            row.pids = row
                .endpoints
                .iter()
                .flat_map(|(_, pids)| pids.iter().copied())
                .collect();
            let kept = &row.pids;
            row.ages.retain(|pid, _| kept.contains(pid));
            Some(row)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use zstats::ListenerSnapshot;

    fn socket(
        protocol: Protocol,
        address: IpAddr,
        port: u16,
        pid: Option<u32>,
        name: &str,
    ) -> ListenerSnapshot {
        ListenerSnapshot {
            protocol,
            address,
            port,
            pid,
            process: pid.map(|_| name.to_string()),
            // A day and a bit, distinct per pid: enough to tell a range
            // from a single age in the tests below
            run_time_secs: pid.map(|pid| 90_000 + u64::from(pid)),
            uid: Some(501),
        }
    }

    fn table(sockets: Vec<ListenerSnapshot>) -> Listeners {
        Listeners {
            sockets,
            coverage: OwnerCoverage::AllProcesses,
        }
    }

    const ANY4: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    const ANY6: IpAddr = IpAddr::V6(Ipv6Addr::UNSPECIFIED);
    const LO4: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
    const LO6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

    fn names(rows: &[Row]) -> Vec<String> {
        rows.iter().map(Row::owner_label).collect()
    }

    fn labels(row: &Row) -> Vec<String> {
        row.endpoints.iter().map(|(e, _)| e.label()).collect()
    }

    #[test]
    fn twins_fold_into_one_endpoint_and_exposed_rows_lead() {
        let listeners = table(vec![
            socket(Protocol::Tcp, LO4, 4226, Some(10), "sccache"),
            socket(Protocol::Tcp, ANY4, 7777, Some(20), "redis-server"),
            socket(Protocol::Tcp, ANY6, 7777, Some(20), "redis-server"),
            socket(Protocol::Tcp, LO6, 12334, Some(30), "tunnel"),
            socket(Protocol::Tcp, LO4, 12334, Some(30), "tunnel"),
        ]);
        let rows = rows(&listeners, false);
        // Exposed first even though its port is higher; local-only after,
        // by port
        assert_eq!(names(&rows), ["redis-server", "sccache", "tunnel"]);
        assert_eq!(labels(&rows[0]), ["*:7777"], "v4 and v6 * are one service");
        assert_eq!(labels(&rows[2]), ["localhost:12334"]);
        assert!(rows[0].exposed() && !rows[1].exposed());
        assert_eq!(rows[0].pid_label().as_deref(), Some("pid 20"));
    }

    #[test]
    fn one_name_is_one_row_however_many_processes_share_it() {
        let listeners = table(vec![
            socket(Protocol::Tcp, ANY4, 7777, Some(59358), "redis-server"),
            socket(Protocol::Tcp, ANY4, 16999, Some(57256), "redis-server"),
            socket(Protocol::Tcp, ANY4, 36379, Some(17302), "redis-server"),
        ]);
        let rows = rows(&listeners, false);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid_label().as_deref(), Some("×3"));
        assert_eq!(labels(&rows[0]), ["*:7777", "*:16999", "*:36379"]);
        // Each endpoint keeps the pid that holds it, for the tooltip
        let held: Vec<_> = rows[0]
            .endpoints
            .iter()
            .map(|(_, pids)| pids.iter().copied().collect::<Vec<_>>())
            .collect();
        assert_eq!(held, [vec![59358], vec![57256], vec![17302]]);
    }

    #[test]
    fn udp_waits_for_the_expanded_view() {
        let listeners = table(vec![
            socket(Protocol::Tcp, ANY4, 53, Some(1), "mDNSResponder"),
            socket(Protocol::Udp, ANY4, 5353, Some(1), "mDNSResponder"),
            socket(Protocol::Udp, ANY4, 5353, Some(2), "Chrome"),
        ]);
        let tcp = rows(&listeners, false);
        assert_eq!(names(&tcp), ["mDNSResponder"]);
        assert_eq!(labels(&tcp[0]), ["*:53"]);
        let all = rows(&listeners, true);
        assert_eq!(all.len(), 2, "a UDP-only process appears only expanded");
        assert_eq!(labels(&all[0]), ["*:53", "*:5353/udp"]);
    }

    #[test]
    fn a_socket_nobody_would_name_groups_by_its_user() {
        let mut other = socket(Protocol::Tcp, ANY4, 22, None, "");
        other.uid = Some(0);
        let listeners = table(vec![other]);
        let rows = rows(&listeners, false);
        assert_eq!(rows[0].owner, Owner::User(Some(0)));
        assert_eq!(rows[0].pid_label(), None);
    }

    fn fleet() -> Listeners {
        table(vec![
            socket(Protocol::Tcp, LO4, 4226, Some(15482), "sccache"),
            socket(Protocol::Tcp, ANY4, 2379, Some(72476), "container-runtime"),
            socket(Protocol::Tcp, ANY4, 6379, Some(98170), "container-runtime"),
            socket(Protocol::Tcp, ANY4, 7777, Some(59358), "redis-server"),
            socket(Protocol::Tcp, ANY4, 36379, Some(17302), "redis-server"),
            socket(Protocol::Udp, ANY4, 5353, Some(1), "mDNSResponder"),
        ])
    }

    #[test]
    fn a_name_match_keeps_the_row_whole() {
        let found = matching(rows(&fleet(), true), "redis");
        assert_eq!(names(&found), ["redis-server"]);
        assert_eq!(labels(&found[0]), ["*:7777", "*:36379"]);
        assert_eq!(found[0].pid_label().as_deref(), Some("×2"));
    }

    #[test]
    fn a_port_match_keeps_only_the_endpoint_that_matched() {
        let found = matching(rows(&fleet(), true), "6379");
        // Substring on the label: 6379 and 36379 both contain it
        assert_eq!(names(&found), ["container-runtime", "redis-server"]);
        assert_eq!(labels(&found[0]), ["*:6379"]);
        assert_eq!(found[0].pid_label().as_deref(), Some("pid 98170"));
        assert_eq!(labels(&found[1]), ["*:36379"]);
        assert_eq!(
            found[1].pid_label().as_deref(),
            Some("pid 17302"),
            "×2 narrowed to the holder"
        );
    }

    #[test]
    fn addresses_pids_and_protocols_find_what_they_mean() {
        let all = || rows(&fleet(), true);
        assert_eq!(names(&matching(all(), "127.0.0.1")), ["sccache"]);
        assert_eq!(names(&matching(all(), "localhost")), ["sccache"]);
        assert_eq!(matching(all(), "0.0.0.0").len(), 3, "every exposed row");
        assert_eq!(names(&matching(all(), "udp")), ["mDNSResponder"]);
        assert_eq!(names(&matching(all(), "17302")), ["redis-server"]);
        // A partial pid is not a pid match (and no label contains it)
        assert!(matching(all(), "1730").is_empty());
    }

    #[test]
    fn quit_is_offered_only_for_one_named_process_this_user_may_signal() {
        let row = |owner, pids: &[u32]| Row {
            owner,
            pids: pids.iter().copied().collect(),
            endpoints: Vec::new(),
            ages: BTreeMap::new(),
        };
        // The test runner: alive, ours to signal, and never actually
        // signalled here — this only asks whether the button would exist
        let runner = std::os::unix::process::parent_id();
        let named = |n: &str| Owner::Name(n.to_string());
        assert_eq!(
            quit_target(&row(named("runner"), &[runner])),
            Some((runner, "runner".to_string()))
        );
        assert_eq!(
            quit_target(&row(named("redis-server"), &[runner, runner + 1])),
            None,
            "×2 is two processes, not one row's worth"
        );
        assert_eq!(
            quit_target(&row(Owner::Pid(runner), &[runner])),
            None,
            "no name for the delivering end to check the pid against"
        );
        assert_eq!(
            quit_target(&row(named("self"), &[std::process::id()])),
            None
        );
        assert_eq!(quit_target(&row(named("launchd"), &[1])), None);
    }

    #[test]
    fn a_row_is_as_old_as_its_process_and_a_name_spans_its_processes() {
        let one = rows(
            &table(vec![socket(
                Protocol::Tcp,
                LO4,
                4226,
                Some(15482),
                "sccache",
            )]),
            false,
        );
        assert_eq!(one[0].ages.values().copied().collect::<Vec<_>>(), [105_482]);
        // 1d 5h 18m: the largest unit only
        let label = one[0].age_label().unwrap();
        assert!(label.contains("1d") && !label.contains("5h"), "{label}");

        let redis = |rows: &[Row]| {
            rows.iter()
                .find(|r| r.owner_label() == "redis-server")
                .map(|r| r.ages.keys().copied().collect::<Vec<_>>())
                .unwrap()
        };
        assert_eq!(
            redis(&rows(&fleet(), true)),
            [17302, 59358],
            "one age per process"
        );
        // A search that finds one of them narrows the ages with the pids
        assert_eq!(redis(&matching(rows(&fleet(), true), "36379")), [17302]);

        let mut ageless = socket(Protocol::Tcp, ANY4, 22, None, "");
        ageless.uid = Some(0);
        let rows = rows(&table(vec![ageless]), false);
        assert_eq!(rows[0].age_label(), None, "no pid, no age to claim");
    }

    #[test]
    fn a_specific_address_counts_as_reaching_beyond_this_mac() {
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5));
        let v6 = IpAddr::V6("fe80::1".parse().unwrap());
        let mapped_loopback = IpAddr::V6("::ffff:127.0.0.1".parse().unwrap());
        assert_eq!(Scope::of(lan), Scope::Specific(lan));
        assert_eq!(Scope::of(mapped_loopback), Scope::Local);
        let e = Endpoint {
            scope: Scope::of(v6),
            port: 8080,
            udp: true,
        };
        assert_eq!(e.label(), "[fe80::1]:8080/udp");
    }
}
