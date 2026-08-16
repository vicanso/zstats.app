//! Clean-up hints for the analyser's directory rows — annotation, not
//! action.
//!
//! A hint states two facts about a well-known cache location: which tool
//! owns it and, when documented, that tool's own cleanup command. Rows
//! that match get the facts appended to their tooltip; the app never
//! runs a command or deletes anything on the strength of a hint — the
//! only acting paths remain the two confirm-gated ones (CLAUDE.md).
//!
//! The embedded defaults (`assets/cleanhints.toml`) come only from each
//! tool's own documentation. Third-party cleanup rule sets must not be
//! collected or copied into this file. A user copy at `~/.zstats/cleanhints.toml`
//! replaces the embedded list wholesale when it parses to at least one
//! entry — "read = whole file, write = whole file" like the other
//! side-files — so an external puller can drop updates there. Read once
//! per launch; a mid-session update lands on the next start.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

enum Rule {
    /// `~/…` in the file — one specific directory.
    Exact(PathBuf),
    /// A bare name in the file — any directory with that last component
    /// (`node_modules` anywhere), same shape as the walk's suffix folds.
    Component(std::ffi::OsString),
}

impl CleanHint {
    fn matches(&self, path: &Path) -> bool {
        match &self.rule {
            Rule::Exact(p) => path == p,
            Rule::Component(name) => path.file_name() == Some(name),
        }
    }
}

/// The hint for a path, if any. Exact entries win over component ones
/// (the load sorts them first); within a group, file order decides.
pub fn lookup(path: &Path) -> Option<&'static CleanHint> {
    static HINTS: OnceLock<Vec<CleanHint>> = OnceLock::new();
    let hints = HINTS.get_or_init(|| {
        let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
        load(&zstats::settings::default_dir(), &home)
    });
    hints.iter().find(|h| h.matches(path))
}

fn load(dir: &Path, home: &Path) -> Vec<CleanHint> {
    let user = dir.join("cleanhints.toml");
    if let Ok(content) = std::fs::read_to_string(&user) {
        let hints = parse(&content, home);
        if !hints.is_empty() {
            return hints;
        }
        // A present-but-broken file falls back rather than silently
        // stripping every row of its hint.
        eprintln!(
            "cleanhints: no valid entries in {}, using built-ins",
            user.display()
        );
    }
    let embedded = crate::assets::get("cleanhints.toml")
        .and_then(|bytes| String::from_utf8(bytes.into_owned()).ok())
        .unwrap_or_default();
    parse(&embedded, home)
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
        let dir = std::env::temp_dir().join(format!("zstats-cleanhints-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // No user file → embedded defaults.
        let built_in = load(&dir, &home);
        assert!(built_in.iter().any(|h| h.matches(&p("/Users/x/.npm"))));

        // A valid user file wins outright — the built-in npm entry is gone.
        std::fs::write(
            dir.join("cleanhints.toml"),
            "[[hint]]\nmatch = \"~/custom\"\nowner = \"me\"\n",
        )
        .unwrap();
        let user = load(&dir, &home);
        assert_eq!(user.len(), 1);
        assert!(user[0].matches(&p("/Users/x/custom")));

        // A broken user file falls back instead of emptying the list.
        std::fs::write(dir.join("cleanhints.toml"), "not toml [[").unwrap();
        let fallback = load(&dir, &home);
        assert!(fallback.iter().any(|h| h.matches(&p("/Users/x/.npm"))));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every embedded entry must parse into one of the two rule forms —
    /// a typo in the shipped file should fail here, not silently drop
    /// the entry at runtime.
    #[test]
    fn embedded_defaults_all_parse() {
        let raw = crate::assets::get("cleanhints.toml").expect("embedded");
        let content = std::str::from_utf8(&raw).unwrap();
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
        let raw = crate::assets::get("cleanhints.toml").expect("embedded");
        let hints = parse(std::str::from_utf8(&raw).unwrap(), &home);
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
