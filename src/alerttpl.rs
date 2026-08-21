//! The alert template — zstats' per-process threshold table, and the
//! pull-update path zstats deliberately left to a client.
//!
//! The table is what stops a machine alerting constantly: it raises (or
//! zeroes) the bar for processes that sustain high CPU or hold a large
//! share of RAM *by design*, so `kernel_task` at 300% is not news. zstats
//! compiles one in per platform and reads `~/.zstats/template.toml`
//! instead when that file exists — wholesale, never layered. Its own
//! comment says why the fetching is not in there:
//!
//! > Keeping it a plain file is what makes "refresh the table on a
//! > schedule" a one-line cron job (`curl -o`) instead of an HTTP client
//! > inside a local metrics collector
//!
//! This app *is* an HTTP client, so it can be the other half. **It is
//! still zstats that owns the alerts** (CLAUDE.md): writing this file is
//! the same act as the Alerts tab writing `[alerts]` overrides through
//! `apply_add` — bytes into zstats' own config, with every threshold
//! still evaluated by zstats' rule engine. Nothing here decides when
//! anything fires.
//!
//! Three properties make that safe, and the first two are zstats':
//!
//! - **A user override outranks the template.** Precedence is user
//!   `-add` entry → template → base rule, so an update can never
//!   overwrite a threshold set from the Alerts tab.
//! - **A bad file is refused, not half-applied.** `Template::parse`
//!   checks the format version, rejects unknown tables, and rejects any
//!   key its matcher cannot honour. Validated here *before* a byte
//!   lands, so this app never writes a file that would make
//!   `reload_settings` start failing.
//! - **An update that changes nothing writes nothing.** The published
//!   table tracks `main`, so between releases it is normally identical
//!   to the compiled-in one; writing it anyway would leave an override
//!   that outranks every *future* built-in, quietly pinning the machine
//!   to today's thresholds. See [`write_template`].
//!
//! Strictly user-triggered, one fetch at a time, through
//! [`proxy::app_proxy`] like every other request this app makes.

use crate::about;
use crate::metrics;
use crate::proxy;
use std::fs;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use zstats::alerts::{TEMPLATE_VERSION, Template};

/// This platform's published table in the zstats repository.
///
/// One file per OS there, and for a harder reason than the clean hints:
/// process names are not portable at all. macOS's `Google Chrome Helper
/// (Renderer)` is `chrome.exe` on Windows and `Isolated Web Co` on Linux
/// (the kernel truncates `comm` to 15 bytes), so a shared table would
/// leave two platforms matching nothing, every process falling through
/// to the base bar, and the machine alerting constantly.
#[cfg(target_os = "macos")]
pub const FILE: &str = "alerts-macos.toml";
#[cfg(target_os = "windows")]
pub const FILE: &str = "alerts-windows.toml";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const FILE: &str = "alerts-linux.toml";

/// The zstats repository's `templates/`, not this app's `assets/` — the
/// table is zstats' data, published where zstats publishes it.
/// raw.githubusercontent.com, not /blob/ — the latter is the HTML page.
///
/// `main`, deliberately, not the tag matching the pinned crate. The
/// format version moves rarely and the *contents* move often (apps and
/// toolchains come and go), so a tag would be permanently identical to
/// what is already compiled in — an update button that can never find an
/// update. A format bump is then possible, and is exactly what
/// [`RemoteUpdate::VersionMismatch`] exists to say out loud.
const REMOTE_DIR: &str = "https://raw.githubusercontent.com/vicanso/zstats/main/templates/";

/// Generous for a ~14 KB file: the point is not hanging a thread when a
/// proxy blackholes the connection.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Which table zstats is actually running with.
#[derive(Clone)]
pub enum Source {
    /// No override present — the table compiled into the zstats crate.
    Builtin(usize),
    /// `~/.zstats/template.toml`, parsed, with its entry count.
    User(usize),
    /// The override is there and zstats refuses it. Said out loud rather
    /// than reported as the built-in, because that is not what happens:
    /// `load_template` returns an error, `reload_settings` fails whole,
    /// and the collector keeps whatever thresholds it already had. A
    /// panel claiming "built-in, 214 entries" would be describing a
    /// state the engine is not in.
    Broken(String),
}

impl Source {
    /// Whether an override file exists at all, i.e. whether reverting to
    /// the built-in table is something the user can still do. A refused
    /// file counts — that is precisely when the way back matters.
    pub fn has_override(&self) -> bool {
        !matches!(self, Source::Builtin(_))
    }
}

/// The cached verdict *and* the table it was reached about: the Config
/// card reads `source`, and the process/app expansions resolve their
/// per-name alert bars against `template` — sharing the one parse
/// instead of re-reading a 14 KB file per repaint.
pub struct Loaded {
    pub source: Source,
    /// The table the engine is running with: the parsed override when
    /// there is a good one, the compiled-in table otherwise. For a
    /// *refused* override this is an approximation — the engine kept
    /// whatever it last applied — but the card is announcing Broken
    /// right beside any number a view shows against it.
    pub template: Template,
}

/// Cached so the windows, which repaint on every tick, do not re-read
/// and re-parse the file at the collector's cadence. Dropped by
/// [`reload`], which every path that touches the file calls.
static CACHE: RwLock<Option<Arc<Loaded>>> = RwLock::new(None);

/// The Config page's source line, and the live table for anything that
/// resolves per-name thresholds for display.
pub fn info() -> Arc<Loaded> {
    if let Some(cached) = CACHE.read().unwrap().as_ref() {
        return cached.clone();
    }
    let loaded = Arc::new(read_source());
    *CACHE.write().unwrap() = Some(loaded.clone());
    loaded
}

/// Drop the cached verdict; the next [`info`] re-reads the file. Called
/// by every path here that writes or deletes it, and wired to the Config
/// page's reload control for a file some other tool dropped in.
pub fn reload() {
    *CACHE.write().unwrap() = None;
}

fn read_source() -> Loaded {
    let dir = zstats::settings::default_dir();
    let path = zstats::settings::template_path(&dir);
    let Ok(text) = fs::read_to_string(&path) else {
        // Unreadable is reported as built-in on purpose: that is also
        // what zstats does for a missing file, and a permissions error
        // on a file that is not there is not worth a scary line.
        return Loaded {
            source: Source::Builtin(entries(Template::builtin())),
            template: Template::builtin().clone(),
        };
    };
    match Template::parse(&text) {
        Ok(template) => Loaded {
            source: Source::User(entries(&template)),
            template,
        },
        Err(e) => Loaded {
            source: Source::Broken(e),
            template: Template::builtin().clone(),
        },
    }
}

/// How many thresholds a table carries, across all four of its groups.
/// One number rather than four: the card says how big the table is, and
/// the split between per-process and per-app is zstats' business.
fn entries(template: &Template) -> usize {
    template.cpu.len() + template.mem.len() + template.app_cpu.len() + template.app_mem.len()
}

/// What the update button's press came to.
pub enum RemoteUpdate {
    /// The published table differed and now lives in the user file;
    /// carries the entry count. The collector has been asked to reload.
    Updated(usize),
    /// Same thresholds as the table already live — the override if
    /// there is one, the compiled-in table otherwise. Nothing written.
    AlreadyCurrent,
    /// Parsed as TOML, but claims a format version this build's zstats
    /// does not read. Its own concern, not the network's, and the two
    /// must not share a message: "download failed" would send someone to
    /// check their connection when what they need is a newer app.
    VersionMismatch {
        found: Option<u32>,
        expected: u32,
    },
    /// Downloaded and refused for any other reason — malformed TOML, an
    /// unknown table, a key the matcher cannot honour, or a table with
    /// nothing in it. Never written; the working local table stays.
    Invalid(String),
    Failed(String),
}

/// Fetch this platform's published table and, when it differs from what
/// is live locally, replace `~/.zstats/template.toml` with it and ask the
/// collector to reload.
///
/// Validated before a byte lands. Blocking — call on the background
/// executor.
pub fn update_from_remote() -> RemoteUpdate {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(FETCH_TIMEOUT))
        .proxy(proxy::app_proxy())
        .build()
        .new_agent();
    let text = match agent
        .get(format!("{REMOTE_DIR}{FILE}"))
        .header("User-Agent", format!("zstats/{}", about::version()))
        .call()
    {
        Ok(response) => match response.into_body().read_to_string() {
            Ok(text) => text,
            Err(e) => return RemoteUpdate::Failed(e.to_string()),
        },
        Err(e) => return RemoteUpdate::Failed(e.to_string()),
    };
    match validate(&text) {
        Ok(template) => write_template(&text, &template),
        Err(refusal) => refusal,
    }
}

/// Parse and refuse, in the order that produces the most useful message.
///
/// The version is read out of the raw document first so a format bump can
/// be named precisely. `Template::parse` reports it too, but only as
/// prose inside its error string, and matching on that string would break
/// the moment zstats rewords it.
fn validate(text: &str) -> Result<Template, RemoteUpdate> {
    if let Ok(doc) = toml::from_str::<toml::Value>(text) {
        let found = doc
            .get("version")
            .and_then(toml::Value::as_integer)
            .and_then(|v| u32::try_from(v).ok());
        if found != Some(TEMPLATE_VERSION) {
            return Err(RemoteUpdate::VersionMismatch {
                found,
                expected: TEMPLATE_VERSION,
            });
        }
    }
    let template = Template::parse(text).map_err(RemoteUpdate::Invalid)?;
    if entries(&template) == 0 {
        // An empty table parses fine and would silently drop every
        // exemption — the one property the template exists for. That is
        // a worse outcome than not updating, so it is refused like a
        // malformed file.
        return Err(RemoteUpdate::Invalid("no entries".into()));
    }
    Ok(template)
}

/// Do two tables say the same thing about thresholds?
///
/// The comparison the update makes, rather than a byte compare: a
/// reworded comment upstream changes nothing zstats acts on, and
/// rewriting a file in a shared config directory for it is churn.
fn same_thresholds(a: &Template, b: &Template) -> bool {
    a.cpu == b.cpu && a.mem == b.mem && a.app_cpu == b.app_cpu && a.app_mem == b.app_mem
}

fn write_template(text: &str, template: &Template) -> RemoteUpdate {
    let dir = zstats::settings::default_dir();
    let path = zstats::settings::template_path(&dir);
    let live = match fs::read_to_string(&path) {
        // A broken override parses to `None` and is therefore never
        // "already current" — replacing it is the fix.
        Ok(existing) => Template::parse(&existing).ok(),
        // No override, so the live table is the one compiled into
        // zstats. Compared rather than skipped, because writing a file
        // identical to the built-in is *worse* than doing nothing: it
        // pins the machine to today's table, and the next crate upgrade
        // shipping a newer built-in would silently lose to it. The
        // published table tracks `main`, so this is the normal state
        // between releases, not an edge case.
        Err(_) => Some(Template::builtin().clone()),
    };
    if live
        .as_ref()
        .is_some_and(|live| same_thresholds(live, template))
    {
        return RemoteUpdate::AlreadyCurrent;
    }
    if let Err(e) = fs::create_dir_all(&dir).and_then(|()| fs::write(&path, text)) {
        return RemoteUpdate::Failed(e.to_string());
    }
    reload();
    metrics::request_reload();
    RemoteUpdate::Updated(entries(template))
}

/// Delete the override so the compiled-in table takes over again.
///
/// The way back matters more here than it does for the clean hints: an
/// override replaces the table wholesale, and a refused one leaves the
/// collector unable to apply any `[alerts]` change at all. `Ok(false)`
/// means there was nothing to remove.
pub fn use_builtin() -> Result<bool, String> {
    let path = zstats::settings::template_path(&zstats::settings::default_dir());
    match fs::remove_file(&path) {
        Ok(()) => {
            reload();
            metrics::request_reload();
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{RemoteUpdate, entries, same_thresholds, validate};
    use zstats::alerts::{TEMPLATE_VERSION, Template};

    #[test]
    fn the_compiled_in_table_is_what_we_report_a_count_for() {
        // Guards the count path against a zstats bump that renames or
        // splits the four groups: the built-in table is never empty, so
        // a zero here means `entries` stopped seeing them.
        assert!(
            entries(Template::builtin()) > 0,
            "the built-in template should carry thresholds"
        );
    }

    #[test]
    fn a_format_bump_is_named_rather_than_blamed_on_the_network() {
        let text = format!("version = {}\n[cpu]\ngopls = 42.0\n", TEMPLATE_VERSION + 1);
        match validate(&text) {
            Err(RemoteUpdate::VersionMismatch { found, expected }) => {
                assert_eq!(found, Some(TEMPLATE_VERSION + 1));
                assert_eq!(expected, TEMPLATE_VERSION);
            }
            _ => panic!("a newer format must be reported as a version mismatch"),
        }
    }

    #[test]
    fn a_missing_version_is_a_mismatch_too() {
        // zstats treats an absent version as "the author forgot", not as
        // "assume current" — a table that quietly did not apply is
        // indistinguishable from a quiet machine.
        match validate("[cpu]\ngopls = 42.0\n") {
            Err(RemoteUpdate::VersionMismatch { found: None, .. }) => {}
            _ => panic!("an unversioned table must not be written"),
        }
    }

    #[test]
    fn an_empty_table_is_refused_like_a_malformed_one() {
        // It parses, and applying it would drop every exemption at once:
        // the machine would then alert on every process that is busy by
        // design. Not updating is the better failure.
        match validate(&format!("version = {TEMPLATE_VERSION}\n")) {
            Err(RemoteUpdate::Invalid(_)) => {}
            _ => panic!("an empty table must never overwrite a working one"),
        }
    }

    #[test]
    fn a_table_the_matcher_cannot_honour_is_refused() {
        // zstats validates every key as a Matcher pattern — a `*` in the
        // middle is not one — so a typo in a published row cannot land
        // as a threshold nobody asked for.
        let text = format!("version = {TEMPLATE_VERSION}\n[cpu]\n\"Chrome*Helper\" = 42.0\n");
        assert!(
            matches!(validate(&text), Err(RemoteUpdate::Invalid(_))),
            "an unmatchable key must be refused"
        );
    }

    #[test]
    fn a_good_table_comes_back_parsed() {
        let text =
            format!("version = {TEMPLATE_VERSION}\n[cpu]\ngopls = 42.0\n[app_mem]\nXcode = 30.0\n");
        let template = validate(&text).unwrap_or_else(|_| panic!("a valid table must be accepted"));
        assert_eq!(entries(&template), 2, "both groups should count");
    }

    #[test]
    fn a_table_matching_the_built_in_is_not_worth_writing() {
        // The published table tracks `main`, so between releases it is
        // normally identical to what the crate compiled in. Writing it
        // anyway would create an override that outranks every *future*
        // built-in — the update button would quietly freeze the machine
        // on today's thresholds.
        let builtin = Template::builtin();
        assert!(same_thresholds(builtin, &builtin.clone()));

        let mut changed = builtin.clone();
        changed.cpu.insert("something-new".into(), 42.0);
        assert!(
            !same_thresholds(builtin, &changed),
            "one added threshold has to count as different"
        );
    }

    #[test]
    fn the_comparison_is_thresholds_not_bytes() {
        // Two documents differing only in a comment and in key order say
        // the same thing, and rewriting a file in the shared config dir
        // for that is churn with no effect on any alert.
        let a = format!("version = {TEMPLATE_VERSION}\n[cpu]\ngopls = 42.0\nrustc = 90.0\n");
        let b = format!(
            "# reworded upstream\nversion = {TEMPLATE_VERSION}\n[cpu]\nrustc = 90.0\ngopls = 42.0\n"
        );
        assert_ne!(a, b, "the two documents must really differ as text");
        let a = validate(&a).unwrap_or_else(|_| panic!("a parses"));
        let b = validate(&b).unwrap_or_else(|_| panic!("b parses"));
        assert!(same_thresholds(&a, &b));
    }
}
