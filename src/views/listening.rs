//! Listening: which process is waiting for connections on which address
//! and port. Rendered on the Network tab under the interfaces.
//!
//! The list is `zstats::listeners()` — a one-shot function outside the
//! snapshot, read by the store when this tab comes on screen and every
//! 15s while it stays (`state::ensure_listeners`). Nothing here asks for
//! it; views only format.
//!
//! **Exposure is the headline, not the port.** `*:6379` and
//! `localhost:6379` are the same program on the same port, but only the
//! first can be reached from another machine. So a row is a process, its
//! endpoints read `*:port` (every interface), `localhost:port` (this Mac)
//! or the specific address, and the rows that reach beyond this Mac come
//! first and in the brighter ink. Not accent: accent means a threshold was
//! crossed, and listening on every interface is a fact, not a verdict.
//!
//! A row is a process *name*, not a pid: a development machine runs
//! twenty `redis-server`s and five container runtimes, and one row per
//! pid (the first version) filled the panel with the same word. The row
//! says how many processes it stands for; its tooltip says which pid holds
//! which endpoint.
//!
//! A search (the header's magnifier) matches a process name, a pid, a
//! port or an address — `redis`, `17302`, `6379`, `0.0.0.0`, `localhost`,
//! `udp` — over the whole answer, UDP included and uncapped: a query is
//! the reader naming the rows they want, so no preview stands in front of
//! them. A name match keeps the row whole; otherwise only the endpoints
//! that matched stay, so `6379` shows the one container that holds it
//! rather than all five with their other ports.
//!
//! The preview is TCP servers only. Unconnected UDP is mostly noise for
//! this question — measured here, 20 of 66 such sockets were mDNS on 5353,
//! one per app — so it arrives with "show more", merged into the same
//! process rows.

use super::widgets;
use crate::font;
use crate::i18n;
use crate::state::{ListenerView, ZStatsAppState, ZStatsGlobalStore};
use crate::theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px, relative,
};
use gpui_kit::component::input::Input;
use gpui_kit::component::{Icon, IconName, Sizable, Size, h_flex};
use rust_i18n::t;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;
use zstats::listeners::Protocol;
use zstats::snapshot::Capabilities;
use zstats::{Listeners, OwnerCoverage};

/// Rows shown before "show more" — a hard cap. Exposed rows sort first, so
/// they are what the preview shows, but unlike the sensor preview's hot
/// rows they are not exempt from the cut: listening on every interface is
/// ordinary on a development machine (every row but three was, measured
/// here), and an exemption turned the preview into the whole list.
const PREVIEW_ROWS: usize = 6;

pub fn render(state: &ZStatsAppState) -> Vec<AnyElement> {
    // No such view on this platform: no card, rather than an apology on
    // every visit.
    if !Capabilities::current().listeners {
        return Vec::new();
    }
    let body = match state.listeners() {
        None => vec![note_line(i18n::tr("listen.reading"))],
        Some(ListenerView::Restricted) => vec![note_line(i18n::tr("listen.restricted"))],
        Some(ListenerView::Failed(error)) => {
            vec![note_line(
                t!("listen.failed", error = error.clone()).to_string(),
            )]
        }
        Some(ListenerView::Ready(listeners)) => return vec![ready_card(state, listeners)],
    };
    vec![
        widgets::list_shell()
            .child(header(None, None))
            .children(body)
            .into_any_element(),
    ]
}

fn ready_card(state: &ZStatsAppState, listeners: &Listeners) -> AnyElement {
    let query = state.listen_filter_text();
    let (shown, chip) = if query.is_empty() {
        let all = rows(listeners, true);
        let mut preview = rows(listeners, false);
        preview.truncate(PREVIEW_ROWS);
        // Counted against the collapsed view whichever is showing, so the
        // chip stays a toggle once expanded. A process that is in both but
        // gains UDP endpoints is not a hidden row; "show UDP" covers that.
        let hidden = all.len().saturating_sub(preview.len());
        let has_udp = listeners
            .sockets
            .iter()
            .any(|s| s.protocol == Protocol::Udp);
        let show_all = state.show_all_listeners();
        let chip = (hidden > 0 || has_udp).then(|| more_chip(hidden, show_all));
        (if show_all { all } else { preview }, chip)
    } else {
        // Every match, UDP included: the preview's cap would hide exactly
        // what was asked for. No "show more" beside a search — there is
        // nothing more to show.
        (matching(rows(listeners, true), query), None)
    };

    let controls = h_flex()
        .items_center()
        .gap(px(5.))
        .child(search_chip(state))
        .children(chip)
        .into_any_element();
    let shell = widgets::list_shell()
        .child(header(Some(controls), Some(listeners.coverage)))
        .children(search_row(state));
    if shown.is_empty() {
        let text = if query.is_empty() {
            i18n::tr("listen.none")
        } else {
            t!("listen.no_match", query = query).to_string()
        };
        return shell.child(note_line(text)).into_any_element();
    }
    let last = shown.len() - 1;
    shell
        .children(
            shown
                .iter()
                .enumerate()
                .map(|(i, row)| row_element(i, row, i == last)),
        )
        .into_any_element()
}

/// The header control that opens the search — the process filter's
/// magnifier, so the two read as the same idiom.
fn search_chip(state: &ZStatsAppState) -> AnyElement {
    let on = state.listen_filter_open();
    div()
        .id("listen-filter")
        .flex_none()
        .rounded(px(4.))
        .p(px(2.))
        .when(on, |d| d.bg(theme::chip()))
        .when(!on, |d| d.hover(|d| d.bg(theme::surface_raised())))
        .tooltip(widgets::wrap_tooltip(i18n::tr("listen.filter_tip")))
        .child(
            Icon::new(IconName::Search)
                .with_size(Size::Size(px(11.)))
                .text_color(Hsla::from(if on {
                    theme::text()
                } else {
                    theme::text_muted()
                })),
        )
        .on_click(|_, window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.toggle_listen_filter(window, cx));
        })
        .into_any_element()
}

/// The search input under the header, while open.
fn search_row(state: &ZStatsAppState) -> Option<AnyElement> {
    if !state.listen_filter_open() {
        return None;
    }
    let input = state.listen_filter_input()?;
    Some(
        div()
            .px(px(13.))
            .pb(px(6.))
            .child(Input::new(input).xsmall().cleanable(true))
            .into_any_element(),
    )
}

fn header(chip: Option<AnyElement>, coverage: Option<OwnerCoverage>) -> AnyElement {
    let mut tip = i18n::tr("listen.tip");
    if coverage == Some(OwnerCoverage::OwnProcessesOnly) {
        tip = format!("{tip} {}", i18n::tr("listen.tip_partial"));
    }
    widgets::list_header(
        h_flex()
            .items_center()
            .gap(px(4.))
            .child(i18n::tr("listen.title"))
            .child(widgets::info_icon("listen-tip", tip)),
        chip,
    )
}

fn note_line(text: String) -> AnyElement {
    div()
        .px(px(13.))
        .pb(px(11.))
        .text_size(px(11.))
        .text_color(theme::text_dim())
        .child(text)
        .into_any_element()
}

fn row_element(index: usize, row: &Row, last: bool) -> AnyElement {
    let endpoints: Vec<String> = row.endpoints.iter().map(|(e, _)| e.label()).collect();
    // Several processes under one name: the tooltip is where each endpoint
    // gets its pid back, which is what a kill or a `lsof -p` needs.
    let several = row.pids.len() > 1;
    let tip: Vec<SharedString> = row
        .endpoints
        .iter()
        .map(|(endpoint, pids)| {
            if several {
                let pids: Vec<String> = pids.iter().map(u32::to_string).collect();
                format!("{}  ·  {}", endpoint.label(), pids.join(", ")).into()
            } else {
                endpoint.label().into()
            }
        })
        .collect();
    h_flex()
        .items_baseline()
        .justify_between()
        .gap(px(8.))
        .px(px(13.))
        .py(px(6.))
        .when(!last, |d| {
            d.border_b(px(1.)).border_color(theme::border_subtle())
        })
        .child(
            h_flex()
                .items_baseline()
                .gap(px(6.))
                .flex_1()
                .min_w_0()
                .child(widgets::truncating_name(
                    ("listen-owner", index),
                    row.owner_label(),
                    11.,
                    gpui::FontWeight::NORMAL,
                    theme::text().into(),
                ))
                .children(row.pid_label().map(|label| {
                    div()
                        .flex_none()
                        .text_size(px(10.))
                        .text_color(theme::text_dim())
                        .child(label)
                })),
        )
        .child(
            font::mono_unless_cjk(div())
                .id(("listen-endpoints", index))
                .flex_none()
                .max_w(relative(0.62))
                .min_w_0()
                .truncate()
                .text_size(px(10.))
                .text_color(if row.exposed() {
                    theme::text()
                } else {
                    theme::text_dim()
                })
                .tooltip(widgets::wrap_tooltip_lines(tip))
                .child(endpoints.join("  ")),
        )
        .into_any_element()
}

/// The Network tab's chip idiom, for this card's own filter.
fn more_chip(hidden: usize, showing: bool) -> AnyElement {
    let label = if showing {
        i18n::tr("listen.hide")
    } else if hidden > 0 {
        t!("listen.show_more", count = hidden).to_string()
    } else {
        i18n::tr("listen.show_udp")
    };
    div()
        .id("listen-more")
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(if showing {
            theme::border()
        } else {
            theme::border_subtle()
        })
        .when(showing, |d| d.bg(theme::chip()))
        // Only while off: the "on" fill is the state, and a hover that
        // repainted it would read as the toggle having flipped.
        .when(!showing, |d| {
            d.hover(|d| d.bg(theme::surface_raised()).border_color(theme::border()))
        })
        .px(px(7.))
        .py(px(1.))
        .text_size(px(9.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(if showing {
            theme::text()
        } else {
            theme::text_dim()
        })
        .child(label)
        .on_click(|_, _window, cx| {
            cx.global::<ZStatsGlobalStore>()
                .clone()
                .update(cx, |state, cx| state.toggle_all_listeners(cx));
        })
        .into_any_element()
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
struct Endpoint {
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

    fn label(&self) -> String {
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
enum Owner {
    Name(String),
    Pid(u32),
    User(Option<u32>),
}

#[derive(Debug)]
struct Row {
    owner: Owner,
    /// Every process the row stands for
    pids: BTreeSet<u32>,
    /// Sorted: exposed first, then port, TCP before UDP — each with the
    /// pids holding it
    endpoints: Vec<(Endpoint, BTreeSet<u32>)>,
}

impl Row {
    fn exposed(&self) -> bool {
        self.endpoints.iter().any(|(e, _)| e.scope != Scope::Local)
    }

    /// The pid for a one-process row, `×N` for a name that stands for
    /// several, nothing when no pid is known.
    fn pid_label(&self) -> Option<String> {
        match self.pids.len() {
            0 => None,
            1 => self.pids.first().map(u32::to_string),
            n => Some(format!("×{n}")),
        }
    }

    fn owner_label(&self) -> String {
        match &self.owner {
            Owner::Name(name) => name.clone(),
            Owner::Pid(_) | Owner::User(None) => i18n::tr("listen.unknown_owner"),
            Owner::User(Some(uid)) => t!("listen.other_user", uid = uid).to_string(),
        }
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
    let mut grouped: HashMap<Owner, (BTreeSet<u32>, Holders)> = HashMap::new();
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
        let (pids, holders) = grouped.entry(owner).or_default();
        let endpoint = Endpoint {
            scope: Scope::of(socket.address),
            port: socket.port,
            udp: is_udp,
        };
        let held = holders.entry(endpoint).or_default();
        if let Some(pid) = socket.pid {
            pids.insert(pid);
            held.insert(pid);
        }
    }
    let mut rows: Vec<Row> = grouped
        .into_iter()
        .map(|(owner, (pids, holders))| Row {
            owner,
            pids,
            endpoints: holders.into_iter().collect(),
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
        assert_eq!(rows[0].pid_label().as_deref(), Some("20"));
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
        assert_eq!(found[0].pid_label().as_deref(), Some("98170"));
        assert_eq!(labels(&found[1]), ["*:36379"]);
        assert_eq!(
            found[1].pid_label().as_deref(),
            Some("17302"),
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
