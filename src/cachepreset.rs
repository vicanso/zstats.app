//! Walk roots for the Caches analysis preset.
//!
//! Same shape as [`crate::cleanhints`]: an embedded default, a user file
//! at `~/.zstats/<FILE>` that replaces it wholesale, and a Config-page
//! fetch of the published copy. Different *job*: this list is the
//! scope the Caches chip walks, not annotations on rows. Mixing the
//! two would let a pulled hint change what gets scanned.
//!
//! Changing the list does not start a walk and does not rewrite a
//! result already on screen — Analyze reads the live list the next
//! time it is pressed. The result cache is keyed by the root set, so
//! an edit is a new scope, not a silent mutation of last time's file.

use crate::about;
use crate::assets;
use crate::proxy;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// This platform's roots file — embedded default, user override, and
/// the published list the update button fetches. Same naming reasons
/// as [`crate::cleanhints::FILE`].
#[cfg(target_os = "macos")]
pub const FILE: &str = "caches-macos.toml";
#[cfg(target_os = "windows")]
pub const FILE: &str = "caches-windows.toml";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const FILE: &str = "caches-linux.toml";

static CACHE: RwLock<Option<Arc<Loaded>>> = RwLock::new(None);

struct Loaded {
    roots: Vec<PathBuf>,
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

const REMOTE_DIR: &str = "https://raw.githubusercontent.com/vicanso/zstats.app/main/assets/";
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// What the update button's press came to. Same outcomes as
/// [`crate::cleanhints::RemoteUpdate`] — a pulled list that does not
/// parse to at least one root never overwrites a working file.
pub enum RemoteUpdate {
    Updated(usize),
    AlreadyCurrent,
    Invalid,
    Failed(String),
}

/// Fetch the published list, validate, write `~/.zstats/<FILE>`, reload.
/// User-triggered; blocking — call on the background executor.
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

pub fn reload() {
    *CACHE.write().unwrap() = None;
}

/// `(from user file, entry count)` — the Config page's source line.
pub fn info() -> (bool, usize) {
    let loaded = current();
    (loaded.from_user_file, loaded.roots.len())
}

/// Live walk roots, already joined to home. Empty only when even the
/// embedded list failed to parse — `ScanScope::cache_set` then refuses
/// rather than walking nothing under a Caches label.
pub fn roots() -> Vec<PathBuf> {
    current().roots.clone()
}

fn load(dir: &Path, home: &Path) -> Loaded {
    let user = dir.join(FILE);
    if let Ok(content) = fs::read_to_string(&user) {
        let roots = parse(&content, home);
        if !roots.is_empty() {
            return Loaded {
                roots,
                from_user_file: true,
            };
        }
        tracing::warn!(
            "cachepreset: no valid roots in {}, using built-ins",
            user.display()
        );
    }
    let embedded = assets::get(FILE)
        .and_then(|bytes| String::from_utf8(bytes.into_owned()).ok())
        .unwrap_or_default();
    Loaded {
        roots: parse(&embedded, home),
        from_user_file: false,
    }
}

/// Parse a roots file. Bad entries are skipped so one typo in a pulled
/// update cannot take the whole list down; an empty result is what
/// [`load`] treats as "fall back".
fn parse(content: &str, home: &Path) -> Vec<PathBuf> {
    let Ok(value) = toml::from_str::<toml::Value>(content) else {
        return Vec::new();
    };
    let Some(entries) = value.get("roots").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    entries
        .iter()
        .filter_map(|entry| expand(entry.as_str()?, home))
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

fn expand(raw: &str, home: &Path) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "~" {
        return None;
    }
    let path = if let Some(rest) = raw.strip_prefix("~/") {
        if rest.is_empty() {
            return None;
        }
        home.join(rest)
    } else if raw.starts_with('/') {
        PathBuf::from(raw)
    } else {
        home.join(raw)
    };
    if path == Path::new("/") {
        return None;
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn parse_expands_relative_and_tilde_and_skips_junk() {
        let home = p("/Users/x");
        let roots = parse(
            r#"
roots = [
  "Library/Caches",
  "~/.npm",
  "/opt/cache",
  "/",
  "~",
  "",
  "../outside",
  "Library/Caches",
]
"#,
            &home,
        );
        assert_eq!(
            roots,
            vec![
                p("/Users/x/Library/Caches"),
                p("/Users/x/.npm"),
                p("/opt/cache"),
            ]
        );
    }

    #[test]
    fn user_file_replaces_the_embedded_list_wholesale() {
        let home = p("/Users/x");
        let dir = env::temp_dir().join(format!("zstats-cachepreset-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let built_in = load(&dir, &home);
        assert!(!built_in.from_user_file);
        assert!(
            built_in
                .roots
                .iter()
                .any(|r| r == &p("/Users/x/Library/Caches"))
        );

        fs::write(dir.join(FILE), "roots = [\"~/custom\"]\n").unwrap();
        let user = load(&dir, &home);
        assert!(user.from_user_file);
        assert_eq!(user.roots, vec![p("/Users/x/custom")]);

        fs::write(dir.join(FILE), "not toml [[").unwrap();
        let fallback = load(&dir, &home);
        assert!(!fallback.from_user_file, "broken file falls back");
        assert!(
            fallback
                .roots
                .iter()
                .any(|r| r == &p("/Users/x/Library/Caches"))
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_embedded_file_parses() {
        let bytes = assets::get(FILE).expect("caches list is embedded");
        let text = std::str::from_utf8(&bytes).expect("utf-8");
        let roots = parse(text, &p("/Users/x"));
        assert!(
            roots.len() >= 3,
            "built-in list should name the platform caches"
        );
        assert!(roots.contains(&p("/Users/x/Library/Caches")));
        assert!(roots.contains(&p("/Users/x/.cargo/registry")));
    }
}
