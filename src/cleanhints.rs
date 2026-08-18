//! Clean-up hints for the analyser's directory rows — annotation, not
//! action.
//!
//! A hint states two facts about a well-known cache location: which tool
//! owns it and, when documented, that tool's own cleanup command. Rows
//! that match get the facts appended to their tooltip; the app never
//! runs a command or deletes anything on the strength of a hint — the
//! only acting paths remain the two confirm-gated ones (CLAUDE.md).
//!
//! The embedded defaults ([`FILE`]) come only from each tool's own
//! documentation. Third-party cleanup rule sets must not be collected or
//! copied into that file. A user copy at `~/.zstats/<FILE>` replaces the
//! embedded list wholesale when it parses to at least one entry — "read =
//! whole file, write = whole file" like the other side-files — so an
//! external puller can drop updates there. Cached on first use; the
//! Config page's reload control ([`reload`]) drops the cache, so a pulled
//! update takes effect without a restart.

use crate::about;
use crate::assets;
use crate::proxy;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::str;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// This platform's hints file, the one name behind all three copies: the
/// embedded default, the user override in `~/.zstats`, and the published
/// list the update button fetches.
///
/// The list is split per OS rather than kept as one file with a platform
/// column, because a cache location does not translate: `~/Library/Caches`
/// has no counterpart to rewrite a prefix into (Windows has
/// `%LOCALAPPDATA%`, Linux `$XDG_CACHE_HOME`), the owning tools differ, and
/// so do their documented cleanup commands — one shared file would be a
/// list where most rows can never match. The platform is in the user file's
/// name too: `~/.zstats` is shared with the zstats CLI and may be synced
/// between machines, so an unqualified name would be wrong on one of them.
///
/// Only the macOS list is written today; the other arms exist so the name
/// is decided in one place, and they resolve to nothing until someone
/// writes those files. That is the graceful end — no annotations, no
/// suggestions, rows still render — and it is unreachable anyway while the
/// crate builds on macOS alone (`main.rs`).
#[cfg(target_os = "macos")]
pub const FILE: &str = "cleanhints-macos.toml";
#[cfg(target_os = "windows")]
pub const FILE: &str = "cleanhints-windows.toml";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const FILE: &str = "cleanhints-linux.toml";

#[derive(Clone)]
pub struct CleanHint {
    pub owner: String,
    /// The owner tool's documented cleanup command, verbatim — display
    /// only. `None` labels the row regenerable without prescribing how.
    pub command: Option<String>,
    /// Whole directory is derived content, safe to move to the Trash —
    /// these join the analyser's cleanup suggestions. Defaults to false:
    /// an entry from a pulled update that forgets the flag can only ever
    /// under-suggest, never volunteer user data for deletion.
    pub trashable: bool,
    rule: Rule,
}

#[derive(Clone)]
enum Rule {
    /// `~/…` in the file — one specific directory.
    Exact(PathBuf),
    /// A bare name in the file — any directory with that last component
    /// (`node_modules` anywhere), same shape as the walk's suffix folds.
    Component(OsString),
}

impl CleanHint {
    fn matches(&self, path: &Path) -> bool {
        match &self.rule {
            Rule::Exact(p) => path == p,
            Rule::Component(name) => path.file_name() == Some(name),
        }
    }
}

/// The rules, cached after first load. Not a `OnceLock`: [`reload`]
/// has to be able to drop this so a pulled update to the user file
/// lands without a restart.
static CACHE: RwLock<Option<Arc<Loaded>>> = RwLock::new(None);

struct Loaded {
    hints: Vec<CleanHint>,
    /// Whether the user file at `~/.zstats/<FILE>` won — the Config
    /// page's source line says which list is live.
    from_user_file: bool,
}

fn current() -> Arc<Loaded> {
    if let Some(loaded) = CACHE.read().unwrap().as_ref() {
        return loaded.clone();
    }
    let home = PathBuf::from(env::var("HOME").unwrap_or_default());
    let loaded = Arc::new(load(&zstats::settings::default_dir(), &home));
    *CACHE.write().unwrap() = Some(loaded.clone());
    loaded
}

/// Where the published copies live; the Config page's update button
/// fetches this platform's ([`FILE`] is appended).
/// raw.githubusercontent.com, not /blob/ — the latter is the HTML page.
const REMOTE_DIR: &str = "https://raw.githubusercontent.com/vicanso/zstats.app/main/assets/";
/// Generous for a ~10 KB file: the point is not hanging a thread when a
/// proxy blackholes the connection.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// What the update button's press came to.
pub enum RemoteUpdate {
    /// The published list differed and now lives in the user file;
    /// carries the entry count. Already reloaded.
    Updated(usize),
    /// Byte-identical with what is live locally — nothing written.
    AlreadyCurrent,
    /// Downloaded fine but parsed to zero entries — never written, the
    /// working local list stays untouched.
    Invalid,
    Failed(String),
}

/// Fetch this platform's published list and, when it differs from what
/// is live locally (the user file if present, the embedded built-ins
/// otherwise), replace `~/.zstats/<FILE>` with it and reload. Validated
/// before a byte lands: content that does not parse to at least one
/// entry never overwrites a working file. Strictly user-triggered;
/// goes through [`proxy::app_proxy`] like every other fetch. Blocking
/// — call on the background executor.
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
    let home = PathBuf::from(env::var("HOME").unwrap_or_default());
    let parsed = parse(&text, &home);
    if parsed.is_empty() {
        return RemoteUpdate::Invalid;
    }
    let dir = zstats::settings::default_dir();
    let user = dir.join(FILE);
    let live = fs::read_to_string(&user)
        .ok()
        .or_else(|| assets::get(FILE).and_then(|bytes| String::from_utf8(bytes.into_owned()).ok()));
    if live.as_deref() == Some(text.as_str()) {
        return RemoteUpdate::AlreadyCurrent;
    }
    if let Err(e) = fs::create_dir_all(&dir).and_then(|()| fs::write(&user, &text)) {
        return RemoteUpdate::Failed(e.to_string());
    }
    reload();
    RemoteUpdate::Updated(parsed.len())
}

/// Drop the cached rules; the next lookup re-reads the user file or the
/// built-ins. Wired to the Config page's reload control.
pub fn reload() {
    *CACHE.write().unwrap() = None;
}

/// `(from user file, entry count)` — the Config page's source line.
pub fn info() -> (bool, usize) {
    let loaded = current();
    (loaded.from_user_file, loaded.hints.len())
}

/// The hint for a path, if any. Exact entries win over component ones
/// (the load sorts them first); within a group, file order decides.
/// Returns a clone — the backing list can be swapped out by [`reload`]
/// at any time, so no borrow may outlive it.
pub fn lookup(path: &Path) -> Option<CleanHint> {
    current().hints.iter().find(|h| h.matches(path)).cloned()
}

/// Name the list was overridden under before the per-OS split. Kept only
/// to adopt it once; nothing reads it as a fallback.
const LEGACY_USER_FILE: &str = "cleanhints.toml";

/// Take over a pre-split override. That file can only have been written
/// by a macOS build (the app has never built anywhere else), so on macOS
/// it *is* this platform's list — renaming it beats silently ignoring
/// someone's customisation, which is what the split would otherwise do.
/// Never overwrites an existing new-name file, and a failure is only a
/// log line: the built-ins still load.
#[cfg(target_os = "macos")]
fn adopt_legacy_user_file(dir: &Path) {
    let legacy = dir.join(LEGACY_USER_FILE);
    let current = dir.join(FILE);
    if !legacy.is_file() || current.exists() {
        return;
    }
    if let Err(e) = fs::rename(&legacy, &current) {
        eprintln!("cleanhints: could not adopt {}: {e}", legacy.display());
    }
}

#[cfg(not(target_os = "macos"))]
fn adopt_legacy_user_file(_dir: &Path) {}

fn load(dir: &Path, home: &Path) -> Loaded {
    adopt_legacy_user_file(dir);
    let user = dir.join(FILE);
    if let Ok(content) = fs::read_to_string(&user) {
        let hints = parse(&content, home);
        if !hints.is_empty() {
            return Loaded {
                hints,
                from_user_file: true,
            };
        }
        // A present-but-broken file falls back rather than silently
        // stripping every row of its hint.
        eprintln!(
            "cleanhints: no valid entries in {}, using built-ins",
            user.display()
        );
    }
    // No embedded list for this platform yet (only macOS has one) means
    // no annotations — rows still render, they just carry no owner.
    let embedded = assets::get(FILE)
        .and_then(|bytes| String::from_utf8(bytes.into_owned()).ok())
        .unwrap_or_default();
    Loaded {
        hints: parse(&embedded, home),
        from_user_file: false,
    }
}

/// Parse a hints file. Entries missing `match` or `owner` are skipped —
/// one bad entry in a pulled update must not take the whole list down.
fn parse(content: &str, home: &Path) -> Vec<CleanHint> {
    let Ok(value) = toml::from_str::<toml::Value>(content) else {
        return Vec::new();
    };
    let Some(entries) = value.get("hint").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut hints: Vec<CleanHint> = entries
        .iter()
        .filter_map(|entry| {
            let matcher = entry.get("match")?.as_str()?;
            let owner = entry.get("owner")?.as_str()?.to_string();
            let command = entry
                .get("command")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let trashable = entry
                .get("trashable")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let rule = if let Some(rest) = matcher.strip_prefix("~/") {
                Rule::Exact(home.join(rest))
            } else if matcher.starts_with('/') {
                Rule::Exact(PathBuf::from(matcher))
            } else if !matcher.contains('/') && !matcher.is_empty() {
                Rule::Component(matcher.into())
            } else {
                return None; // relative multi-component patterns are not a thing
            };
            Some(CleanHint {
                owner,
                command,
                trashable,
                rule,
            })
        })
        .collect();
    // Exact before component, so the specific fact beats the generic one.
    hints.sort_by_key(|h| matches!(h.rule, Rule::Component(_)));
    hints
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn parse_expands_tilde_matches_components_and_skips_broken_entries() {
        let home = p("/Users/x");
        let hints = parse(
            r#"
[[hint]]
match = "~/.npm"
owner = "npm"
command = "npm cache clean --force"

[[hint]]
match = "node_modules"
owner = "npm / pnpm / yarn"

[[hint]]
owner = "no match line — skipped"

[[hint]]
match = "some/relative/path"
owner = "skipped too"
"#,
            &home,
        );
        assert_eq!(hints.len(), 2);
        let npm = hints
            .iter()
            .find(|h| h.matches(&p("/Users/x/.npm")))
            .expect("tilde expanded against home");
        assert_eq!(npm.owner, "npm");
        assert_eq!(npm.command.as_deref(), Some("npm cache clean --force"));
        assert!(!npm.matches(&p("/Users/y/.npm")), "someone else's home");
        let nm = hints
            .iter()
            .find(|h| h.matches(&p("/Users/x/dev/proj/node_modules")))
            .expect("component matches anywhere");
        assert!(nm.command.is_none());
        assert!(!nm.matches(&p("/Users/x/dev/node_modules_backup")));
    }

    #[test]
    fn user_file_replaces_the_embedded_list_wholesale() {
        let home = p("/Users/x");
        let dir = env::temp_dir().join(format!("zstats-cleanhints-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // No user file → embedded defaults.
        let built_in = load(&dir, &home);
        assert!(!built_in.from_user_file);
        assert!(
            built_in
                .hints
                .iter()
                .any(|h| h.matches(&p("/Users/x/.npm")))
        );

        // A valid user file wins outright — the built-in npm entry is gone.
        fs::write(
            dir.join(FILE),
            "[[hint]]\nmatch = \"~/custom\"\nowner = \"me\"\n",
        )
        .unwrap();
        let user = load(&dir, &home);
        assert!(user.from_user_file);
        assert_eq!(user.hints.len(), 1);
        assert!(user.hints[0].matches(&p("/Users/x/custom")));

        // A broken user file falls back instead of emptying the list.
        fs::write(dir.join(FILE), "not toml [[").unwrap();
        let fallback = load(&dir, &home);
        assert!(
            !fallback.from_user_file,
            "broken file falls back to built-ins"
        );
        assert!(
            fallback
                .hints
                .iter()
                .any(|h| h.matches(&p("/Users/x/.npm")))
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// A user file written before the per-OS split keeps working: it is
    /// adopted under the platform name once, and an already-migrated
    /// directory is left alone.
    #[test]
    fn a_pre_split_user_file_is_adopted_not_ignored() {
        let home = p("/Users/x");
        let dir = env::temp_dir().join(format!("zstats-cleanhints-legacy-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        fs::write(
            dir.join(LEGACY_USER_FILE),
            "[[hint]]\nmatch = \"~/legacy\"\nowner = \"me\"\n",
        )
        .unwrap();
        let adopted = load(&dir, &home);
        assert!(adopted.from_user_file, "the old override still wins");
        assert!(adopted.hints[0].matches(&p("/Users/x/legacy")));
        assert!(dir.join(FILE).is_file(), "renamed to the platform name");
        assert!(!dir.join(LEGACY_USER_FILE).exists(), "and only once");

        // A new-name file already there is never clobbered by a stray old one.
        fs::write(
            dir.join(LEGACY_USER_FILE),
            "[[hint]]\nmatch = \"~/stale\"\nowner = \"me\"\n",
        )
        .unwrap();
        let kept = load(&dir, &home);
        assert!(kept.hints[0].matches(&p("/Users/x/legacy")));

        let _ = fs::remove_dir_all(&dir);
    }

    /// Every embedded entry must parse into one of the two rule forms —
    /// a typo in the shipped file should fail here, not silently drop
    /// the entry at runtime.
    #[test]
    fn embedded_defaults_all_parse() {
        let raw = assets::get(FILE).expect("embedded");
        let content = str::from_utf8(&raw).unwrap();
        let entry_count = content.matches("[[hint]]").count();
        let hints = parse(content, &p("/Users/x"));
        assert_eq!(hints.len(), entry_count, "an embedded entry was dropped");
        assert!(entry_count >= 20, "the shipped list lost its substance");
    }

    /// Working trees, pulled models and uv's hard-linked cache must stay
    /// annotation-only — trashable here would put a delete button on
    /// user data (docs/disk-analysis.md).
    #[test]
    fn annotation_only_locations_are_not_trashable() {
        let home = p("/Users/x");
        let raw = assets::get(FILE).expect("embedded");
        let hints = parse(str::from_utf8(&raw).unwrap(), &home);
        let must_not_trash = [
            "/Users/x/.cache/huggingface",
            "/Users/x/.ollama/models",
            "/Users/x/.cache/uv",
            "/Users/x/dev/proj/node_modules",
            "/Users/x/dev/proj/target",
            "/Users/x/.rustup/toolchains",
            "/Users/x/Library/Developer/CoreSimulator",
            "/Users/x/Library/Developer/Xcode/Archives",
            "/Users/x/Library/Containers/com.docker.docker",
            "/Users/x/Library/Group Containers/HUAQ24HBR6.dev.orbstack",
            "/Users/x/Library/Containers/com.utmapp.UTM",
            "/Users/x/Library/Containers/com.tencent.xinWeChat",
            "/Users/x/Library/Containers/com.tencent.WeWorkMac",
            "/Users/x/Library/Containers/com.tencent.qq",
            "/Users/x/Library/Group Containers/6N38VWS5BX.ru.keepcoder.Telegram",
            "/Users/x/Documents/Zoom",
            "/Users/x/Library/Application Support/Spotify/PersistentCache",
            "/Users/x/Library/Mobile Documents",
            "/Users/x/Library/Application Support/Google/DriveFS",
            "/Users/x/Library/Application Support/Notion",
            "/Users/x/Library/Application Support/Figma",
            "/Users/x/Library/Application Support/Arc",
            "/Users/x/Library/Application Support/Steam",
            "/Users/x/Library/Containers/com.kingsoft.wpsoffice.mac",
            "/Users/x/.Trash",
        ];
        for path in must_not_trash {
            let hint = hints
                .iter()
                .find(|h| h.matches(&p(path)))
                .unwrap_or_else(|| panic!("no hint for {path}"));
            assert!(
                !hint.trashable,
                "{path} is user data / a working tree and must not be trashable"
            );
        }
        let must_trash = [
            "/Users/x/.npm",
            "/Users/x/Library/Caches/com.spotify.client",
            "/Users/x/Library/Caches/Google/Chrome",
            "/Users/x/Library/Caches/com.brave.Browser",
            "/Users/x/Library/Caches/us.zoom.xos",
            "/Users/x/Library/Application Support/Slack/Cache",
            "/Users/x/Library/Application Support/Adobe/Common/Media Cache Files",
            "/Users/x/Library/Application Support/Claude/Cache",
            "/Users/x/Library/Application Support/Notion/Cache",
            "/Users/x/Library/Developer/Xcode/watchOS DeviceSupport",
        ];
        for path in must_trash {
            let hint = hints
                .iter()
                .find(|h| h.matches(&p(path)))
                .unwrap_or_else(|| panic!("no hint for {path}"));
            assert!(
                hint.trashable,
                "{path} is a pure cache and should stay on the cleanup list"
            );
        }
    }
}
