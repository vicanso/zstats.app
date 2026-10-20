//! Notifications to a URL.
//!
//! Everything the app delivers as a banner is also POSTed, as one JSON
//! object, to an address the user configured (`app.toml` `webhook`, the
//! Interface page's row) — a phone through ntfy, a chat room, a script.
//! A second transport for the same deliveries, not a second opinion
//! about what deserves one: the call sits beside every banner
//! (`notify::post*`), so the Notifications switch, a snooze and the
//! auto-quiet hold a POST back exactly as they hold the banner back,
//! and the log's verdict for one is the verdict for both. What it does
//! not share is the banner's dependence on macOS: a bare `cargo run`
//! has no banners and still posts, and so does an app whose banners
//! were turned off in System Settings — which is how someone gets the
//! phone without the desktop.
//!
//! **The body** is flat where a reader wants to grep and nested where
//! the shape depends on the kind:
//!
//! ```json
//! {
//!   "hostname": "…",
//!   "category": "cpu",
//!   "level": "warning",
//!   "title": "Google Chrome Helper",
//!   "subtitle": "Warning · CPU",
//!   "message": "…the banner's own sentence…",
//!   "summary": "…zstats' one-line English rendering…",
//!   "subject": { "type": "process", "name": "…", "display_name": null, "pid": 830 },
//!   "detail": { "avg_percent": 215.0, "threshold_percent": 200.0, "window_secs": 60, "runaway": false },
//!   "repeat_after_secs": null,
//!   "silent": false,
//!   "test": false,
//!   "time": "2026-10-12T15:04:05+08:00",
//!   "timestamp": 1791788645,
//!   "app": "zstats", "version": "0.4.7",
//!   "os": "Darwin", "os_version": "27.0.1", "arch": "aarch64",
//!   "schema": 1
//! }
//! ```
//!
//! (On the wire every key, nested ones included, has a line of its
//! own.)
//!
//! `category` is the kind — `cpu`, `app_cpu`, `memory`, `app_memory`,
//! `disk`, `pressure` for zstats' alerts at `warning` or `critical`,
//! and `sustained_cpu`, `memory_growth`, `disk_growth` at `info` for
//! the panel's own observers, whose banners are the silent ones
//! (`silent` says so, for a receiver that wants to keep them quiet).
//! `title` / `subtitle` / `message` are the banner's three lines in
//! the interface language; every number under `detail` is the event's
//! own field, never recomputed here.
//!
//! **Written for someone reading it raw.** A receiver that shows the
//! body as text — ntfy does, for a plain POST to a topic — shows it
//! exactly as sent, and the first build sent one unbroken line: every
//! field there and none findable on a phone. So three things are for
//! that reader and cost a parser nothing. The keys keep the order
//! above — who, what and how bad first, the build's identity last —
//! rather than a map's alphabet, which opens on `"app": "zstats",
//! "arch"`. The object is pretty-printed, a key to a line. And
//! fractions are cut to two decimals: they are all percentages, and
//! the collector's `7.55712855303729` is not a figure anyone reads.
//!
//! **Delivery** is one worker thread behind a bounded queue: a POST
//! must never run on the main thread, and a dead endpoint must not
//! grow a backlog — a full queue drops the notification and says so.
//! One attempt, ten seconds, through the app's proxy setting like every
//! other request it makes. No retry: a notification that arrives ten
//! minutes late describes a machine that has moved on, and the engine
//! repeats what still holds. The URL is a secret more often than not
//! (an ntfy topic is its own password), so the log names the host and
//! never the path.

use crate::about;
use crate::prefs;
use crate::proxy;
use serde_json::{Value, json};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{OnceLock, RwLock};
use std::thread;
use std::time::Duration;
use zstats::snapshot::HostInfo;
use zstats::{AlertDetail, AlertEvent, AlertKind, AlertSubject, Severity};

/// One attempt's whole budget. The same judgement about a slow or
/// absent host the update check makes.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Notifications waiting for the worker. Alerts arrive minutes apart;
/// more than this queued means the endpoint is not answering, and the
/// thirty-third would be read long after it stopped being true.
const QUEUE: usize = 32;

/// Who is reporting. Taken from zstats' own host record on each tick
/// ([`note_host`]) rather than asked of the OS a second time.
#[derive(Clone, Debug, Default, PartialEq)]
struct Host {
    hostname: String,
    os: String,
    os_version: String,
    arch: String,
}

static HOST: RwLock<Option<Host>> = RwLock::new(None);

/// Remember the machine's identity from the latest snapshot. Called on
/// every ingest; writes only when something changed, which a hostname
/// does when the network renames the Mac.
pub fn note_host(info: &HostInfo) {
    let host = Host {
        hostname: info.hostname.clone(),
        os: info.os_name.clone(),
        os_version: info.os_version.clone(),
        arch: info.arch.clone(),
    };
    let unchanged = HOST
        .read()
        .is_ok_and(|current| current.as_ref() == Some(&host));
    if !unchanged && let Ok(mut current) = HOST.write() {
        *current = Some(host);
    }
}

/// One notification, as the receiver will read it.
pub struct Notice {
    category: &'static str,
    level: &'static str,
    title: String,
    subtitle: String,
    message: String,
    summary: Option<String>,
    subject: Value,
    detail: Value,
    repeat_after: Option<Duration>,
    silent: bool,
    test: bool,
}

impl Notice {
    /// A zstats alert, with the three lines its banner shows.
    pub fn alert(event: &AlertEvent, title: &str, subtitle: &str, message: &str) -> Self {
        Notice {
            category: match event.kind() {
                AlertKind::Cpu => "cpu",
                AlertKind::AppCpu => "app_cpu",
                AlertKind::Memory => "memory",
                AlertKind::AppMemory => "app_memory",
                AlertKind::Disk => "disk",
                AlertKind::Pressure => "pressure",
            },
            level: match event.severity() {
                Severity::Critical => "critical",
                Severity::Warning => "warning",
            },
            title: title.to_string(),
            subtitle: subtitle.to_string(),
            message: message.to_string(),
            summary: Some(event.summary()),
            subject: subject_of(&event.subject),
            detail: detail_of(&event.detail),
            repeat_after: event.repeat_after,
            silent: false,
            test: false,
        }
    }

    /// One of the panel's own observations — sustained load, a memory
    /// climb, a directory that grew. Not an alert, and labelled so:
    /// `info`, and `silent` like its banner.
    pub fn finding(
        category: &'static str,
        lines: [&str; 3],
        subject: Value,
        detail: Value,
    ) -> Self {
        let [title, subtitle, message] = lines;
        Notice {
            category,
            level: "info",
            title: title.to_string(),
            subtitle: subtitle.to_string(),
            message: message.to_string(),
            summary: None,
            subject,
            detail,
            repeat_after: None,
            silent: true,
            test: false,
        }
    }

    /// What the settings row's button sends: the real envelope around
    /// nothing, marked so a receiver can tell.
    fn test(message: &str) -> Self {
        Notice {
            category: "test",
            level: "info",
            title: crate::APP_NAME.to_string(),
            subtitle: String::new(),
            message: message.to_string(),
            summary: None,
            subject: json!({ "type": "system" }),
            detail: json!({}),
            repeat_after: None,
            silent: false,
            test: true,
        }
    }
}

fn subject_of(subject: &AlertSubject) -> Value {
    match subject {
        AlertSubject::Process {
            pid,
            name,
            display_name,
        } => json!({
            "type": "process",
            "name": name,
            "display_name": display_name,
            "pid": pid,
        }),
        AlertSubject::App {
            root_pid,
            name,
            display_name,
            process_count,
        } => json!({
            "type": "app",
            "name": name,
            "display_name": display_name,
            "pid": root_pid,
            "process_count": process_count,
        }),
        AlertSubject::Volume { mount_point } => json!({
            "type": "volume",
            "mount_point": mount_point,
        }),
        AlertSubject::System => json!({ "type": "system" }),
    }
}

/// The event's own numbers under plain names, durations in seconds.
/// Transcribed, never derived — the same rule the views follow.
fn detail_of(detail: &AlertDetail) -> Value {
    match detail {
        AlertDetail::Cpu {
            avg_percent,
            threshold_percent,
            window,
            runaway,
        } => json!({
            "avg_percent": avg_percent,
            "threshold_percent": threshold_percent,
            "window_secs": window.as_secs(),
            "runaway": runaway,
        }),
        AlertDetail::Memory {
            avg_bytes,
            share_percent,
            threshold_bytes,
            threshold_percent,
            window,
        } => json!({
            "avg_bytes": avg_bytes,
            "share_percent": share_percent,
            "threshold_bytes": threshold_bytes,
            "threshold_percent": threshold_percent,
            "window_secs": window.as_secs(),
        }),
        AlertDetail::Disk {
            used_percent,
            threshold_percent,
            available_bytes,
            total_bytes,
            critical_below_bytes,
        } => json!({
            "used_percent": used_percent,
            "threshold_percent": threshold_percent,
            "available_bytes": available_bytes,
            "total_bytes": total_bytes,
            // zstats 0.7.2's critical line, as free space; `level` is
            // `critical` once `available_bytes` is under it.
            "critical_below_bytes": critical_below_bytes,
        }),
        AlertDetail::Pressure {
            level,
            sustained,
            swap_used_bytes,
            swap_total_bytes,
            compressed_bytes,
            swap_ins_per_sec,
            swap_outs_per_sec,
            top_consumers,
        } => json!({
            "pressure_level": level,
            "sustained_secs": sustained.as_secs(),
            "swap_used_bytes": swap_used_bytes,
            "swap_total_bytes": swap_total_bytes,
            "compressed_bytes": compressed_bytes,
            "swap_ins_per_sec": swap_ins_per_sec,
            "swap_outs_per_sec": swap_outs_per_sec,
            "top_consumers": top_consumers
                .iter()
                .map(|consumer| json!({
                    "pid": consumer.pid,
                    "name": consumer.name,
                    "display_name": consumer.display_name,
                    "bytes": consumer.bytes,
                    "share_percent": consumer.share_percent,
                    "process_count": consumer.process_count,
                }))
                .collect::<Vec<_>>(),
        }),
    }
}

/// The JSON body: keys in reading order, pretty-printed, percentages
/// to two decimals — see the module doc for why each of the three is
/// for the person reading it raw. The order is `json!`'s, which holds
/// because Cargo.toml asks serde_json for `preserve_order`; a test
/// pins it.
fn body(notice: &Notice, host: &Host, at: &jiff::Zoned) -> String {
    let envelope = json!({
        "hostname": host.hostname,
        "category": notice.category,
        "level": notice.level,
        "title": notice.title,
        "subtitle": notice.subtitle,
        "message": notice.message,
        "summary": notice.summary,
        "subject": notice.subject,
        "detail": notice.detail,
        "repeat_after_secs": notice.repeat_after.map(|after| after.as_secs()),
        "silent": notice.silent,
        "test": notice.test,
        "time": at.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string(),
        "timestamp": at.timestamp().as_second(),
        "app": crate::APP_NAME,
        "version": about::version(),
        "os": host.os,
        "os_version": host.os_version,
        "arch": host.arch,
        // The envelope's version. Keys may be added under the same
        // number; renaming or removing one is what changes it.
        "schema": 1,
    });
    let envelope = rounded(envelope);
    // A `Value` of strings, numbers and maps cannot fail to serialise;
    // the compact form is the same object if it somehow did.
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| envelope.to_string())
}

/// Every fraction in the body to two decimals. They are all
/// percentages, and `7.55712855303729` in a notification is the
/// collector's arithmetic showing through, not precision anyone reads.
fn rounded(value: Value) -> Value {
    match value {
        Value::Number(number) if number.is_f64() => number
            .as_f64()
            .map(|fraction| (fraction * 100.0).round() / 100.0)
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Null, Value::Number),
        Value::Array(items) => Value::Array(items.into_iter().map(rounded).collect()),
        Value::Object(map) => {
            Value::Object(map.into_iter().map(|(k, v)| (k, rounded(v))).collect())
        }
        other => other,
    }
}

fn body_now(notice: &Notice) -> String {
    let host = HOST
        .read()
        .ok()
        .and_then(|host| host.clone())
        .unwrap_or_default();
    body(notice, &host, &jiff::Zoned::now())
}

/// `http://` or `https://` with a host, or nothing at all — empty is
/// the off switch.
pub fn valid(setting: &str) -> bool {
    let setting = setting.trim();
    setting.is_empty() || host_of(setting).is_some()
}

/// The endpoint's host — all of a URL the log is allowed to say.
fn host_of(url: &str) -> Option<String> {
    let uri: ureq::http::Uri = url.trim().parse().ok()?;
    let scheme = uri.scheme_str()?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    uri.host()
        .filter(|host| !host.is_empty())
        .map(str::to_string)
}

struct Job {
    url: String,
    body: String,
    category: &'static str,
}

/// Queue one notification for the configured URL; nothing happens
/// with none configured. Returns at once.
pub fn send(notice: Notice) {
    let url = prefs::webhook();
    if url.is_empty() {
        return;
    }
    let job = Job {
        url,
        body: body_now(&notice),
        category: notice.category,
    };
    match worker().try_send(job) {
        Ok(()) => {}
        Err(TrySendError::Full(job)) => tracing::warn!(
            category = job.category,
            "webhook queue full; notification dropped"
        ),
        Err(TrySendError::Disconnected(job)) => tracing::warn!(
            category = job.category,
            "webhook worker is gone; notification dropped"
        ),
    }
}

fn worker() -> &'static SyncSender<Job> {
    static WORKER: OnceLock<SyncSender<Job>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Job>(QUEUE);
        let spawned = thread::Builder::new()
            .name("zstats-webhook".into())
            .spawn(move || {
                for job in rx {
                    let host = host_of(&job.url).unwrap_or_default();
                    match post(&job.url, &job.body) {
                        Ok(status) => tracing::info!(
                            category = job.category,
                            host = %host,
                            status,
                            "webhook delivered"
                        ),
                        Err(error) => tracing::warn!(
                            category = job.category,
                            host = %host,
                            error = %error,
                            "webhook failed"
                        ),
                    }
                }
            });
        if let Err(e) = spawned {
            // The receiver went with the closure: every `send` from
            // here on reports a gone worker instead of queueing.
            tracing::error!(error = %e, "could not start the webhook worker");
        }
        tx
    })
}

/// One POST. `Ok` is the 2xx status; anything else is the reason, with
/// the URL taken back out of it — an error string is headed for the
/// log and for the settings row.
fn post(url: &str, body: &str) -> Result<u16, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .proxy(proxy::app_proxy())
        .build()
        .new_agent();
    agent
        .post(url)
        .header("Content-Type", "application/json")
        .header("User-Agent", format!("zstats/{}", about::version()))
        .send(body)
        .map(|response| response.status().as_u16())
        .map_err(|e| e.to_string().replace(url, "the webhook URL"))
}

/// Send the test notification to the configured URL and wait for the
/// answer. Blocking — call on the background executor.
pub fn send_test(message: &str) -> Result<u16, String> {
    let url = prefs::webhook();
    post(&url, &body_now(&Notice::test(message)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host {
            hostname: "studio".into(),
            os: "macOS".into(),
            os_version: "27.0.1".into(),
            arch: "aarch64".into(),
        }
    }

    fn at() -> jiff::Zoned {
        jiff::civil::date(2026, 10, 12)
            .at(15, 4, 5, 0)
            .to_zoned(jiff::tz::TimeZone::fixed(jiff::tz::offset(8)))
            .unwrap()
    }

    fn cpu_event() -> AlertEvent {
        AlertEvent {
            subject: AlertSubject::Process {
                pid: 830,
                name: "Google Chrome Helper".into(),
                display_name: None,
            },
            detail: AlertDetail::Cpu {
                avg_percent: 215.0,
                threshold_percent: 200.0,
                window: Duration::from_secs(60),
                runaway: false,
            },
            repeat_after: None,
        }
    }

    /// The body is one JSON object a strict parser accepts, carrying
    /// the three fields the request named and the event's own numbers.
    #[test]
    fn an_alert_becomes_one_json_object_with_its_own_numbers() {
        let notice = Notice::alert(
            &cpu_event(),
            "Google Chrome Helper",
            "Warning · CPU",
            "busy",
        );
        let text = body(&notice, &host(), &at());
        let parsed: Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(parsed["hostname"], "studio");
        assert_eq!(parsed["category"], "cpu");
        assert_eq!(parsed["level"], "warning");
        assert_eq!(parsed["message"], "busy");
        assert_eq!(parsed["subject"]["type"], "process");
        assert_eq!(parsed["subject"]["pid"], 830);
        assert_eq!(parsed["detail"]["avg_percent"], 215.0);
        assert_eq!(parsed["detail"]["window_secs"], 60);
        assert_eq!(parsed["time"], "2026-10-12T15:04:05+08:00");
        assert_eq!(parsed["timestamp"], at().timestamp().as_second());
        assert_eq!(parsed["repeat_after_secs"], Value::Null);
        assert_eq!(parsed["silent"], false);
        assert_eq!(parsed["test"], false);
        assert_eq!(parsed["version"], about::version());
        assert!(
            parsed["summary"]
                .as_str()
                .is_some_and(|s| s.contains("215%"))
        );
    }

    /// A receiver that prints the body shows it as sent: who, what and
    /// how bad first, a key to a line, nested fields in the order they
    /// were written — whatever a JSON map would have sorted. Pins
    /// serde_json's `preserve_order`, which Cargo.toml asks for.
    #[test]
    fn the_body_reads_top_down_a_key_to_a_line() {
        let notice = Notice::alert(&cpu_event(), "t", "s", "m");
        let text = body(&notice, &host(), &at());
        assert!(
            text.starts_with(
                "{\n  \"hostname\": \"studio\",\n  \"category\": \"cpu\",\n  \"level\": \"warning\",\n  \"title\": \"t\",\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "  \"subject\": {\n    \"type\": \"process\",\n    \"name\": \"Google Chrome Helper\",\n    \"display_name\": null,\n    \"pid\": 830\n  },\n"
            ),
            "{text}"
        );
        assert!(text.ends_with("  \"schema\": 1\n}"), "{text}");
        // Still one JSON object to anything that parses it.
        assert!(serde_json::from_str::<Value>(&text).is_ok());
    }

    /// The collector's long fractions are cut to two decimals wherever
    /// they sit; whole numbers and everything else pass untouched.
    #[test]
    fn fractions_are_cut_to_two_decimals() {
        let mut event = cpu_event();
        event.detail = AlertDetail::Cpu {
            avg_percent: 7.557_128_553_037_29,
            threshold_percent: 3.0,
            window: Duration::from_secs(60),
            runaway: true,
        };
        let text = body(&Notice::alert(&event, "t", "s", "m"), &host(), &at());
        assert!(text.contains("\"avg_percent\": 7.56,"), "{text}");
        assert!(text.contains("\"threshold_percent\": 3.0,"), "{text}");
        assert!(text.contains("\"window_secs\": 60,"), "{text}");
        let nested =
            rounded(json!({ "list": [{ "share_percent": 12.3456 }], "n": 5, "s": "1.23456" }));
        assert_eq!(
            nested,
            json!({ "list": [{ "share_percent": 12.35 }], "n": 5, "s": "1.23456" })
        );
        assert_eq!(rounded(json!(f64::NAN)), Value::Null);
    }

    /// Names and paths are the user's: quotes, backslashes, newlines
    /// and non-ASCII all have to survive as JSON.
    #[test]
    fn awkward_text_is_escaped_not_trusted() {
        let title = "a \"quoted\" \\ name\nline — 进程";
        let notice = Notice::finding(
            "disk_growth",
            [title, "", "m"],
            json!({ "type": "directory", "path": "/Users/x/a \"b\"" }),
            json!({ "grew_bytes": 6u64 << 30 }),
        );
        let parsed: Value = serde_json::from_str(&body(&notice, &host(), &at())).unwrap();
        assert_eq!(parsed["title"], title);
        assert_eq!(parsed["subject"]["path"], "/Users/x/a \"b\"");
        assert_eq!(parsed["level"], "info");
        assert_eq!(parsed["silent"], true);
        assert_eq!(parsed["summary"], Value::Null);
    }

    /// Every alert kind names itself, and a follow-up says how long the
    /// episode had been going.
    #[test]
    fn kinds_levels_and_reminders_are_labelled() {
        let mut event = cpu_event();
        event.detail = AlertDetail::Cpu {
            avg_percent: 650.0,
            threshold_percent: 600.0,
            window: Duration::from_secs(60),
            runaway: true,
        };
        event.repeat_after = Some(Duration::from_secs(1800));
        let notice = Notice::alert(&event, "t", "s", "m");
        assert_eq!((notice.category, notice.level), ("cpu", "critical"));
        let parsed: Value = serde_json::from_str(&body(&notice, &host(), &at())).unwrap();
        assert_eq!(parsed["repeat_after_secs"], 1800);
        assert_eq!(parsed["detail"]["runaway"], true);

        let disk = AlertEvent {
            subject: AlertSubject::Volume {
                mount_point: "/".into(),
            },
            detail: AlertDetail::Disk {
                used_percent: 92.5,
                threshold_percent: 90.0,
                available_bytes: 30 << 30,
                total_bytes: 494 << 30,
                critical_below_bytes: 10 << 30,
            },
            repeat_after: None,
        };
        let notice = Notice::alert(&disk, "t", "s", "m");
        assert_eq!((notice.category, notice.level), ("disk", "warning"));
        assert_eq!(notice.subject["mount_point"], "/");
        assert_eq!(notice.detail["available_bytes"], 30u64 << 30);
        assert_eq!(notice.detail["critical_below_bytes"], 10u64 << 30);

        // Under its critical line the same volume is zstats' Critical.
        let mut almost_out = disk.clone();
        if let AlertDetail::Disk {
            available_bytes, ..
        } = &mut almost_out.detail
        {
            *available_bytes = 4 << 30;
        }
        let notice = Notice::alert(&almost_out, "t", "s", "m");
        assert_eq!((notice.category, notice.level), ("disk", "critical"));
    }

    #[test]
    fn only_an_http_address_or_nothing_is_a_setting() {
        assert!(valid(""));
        assert!(valid("   "));
        assert!(valid("https://ntfy.sh/some-topic"));
        assert!(valid(" http://192.168.1.10:8080/hook?x=1 "));
        assert!(!valid("ntfy.sh/some-topic"), "no scheme");
        assert!(!valid("ftp://example.com/x"));
        assert!(!valid("https://"));
        assert!(!valid("just words"));
        // The log may say this much of a URL and no more.
        assert_eq!(
            host_of("https://ntfy.sh/secret-topic").as_deref(),
            Some("ntfy.sh")
        );
    }
}
