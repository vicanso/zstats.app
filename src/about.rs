//! Build identity shown on the settings window's About page.
//!
//! Version comes from Cargo.toml. The commit is stamped by `build.rs`.
//! Architecture is the target this binary was compiled for — not the
//! machine it happens to be running on, which would disagree after
//! Rosetta.

/// `Cargo.toml` `[package].version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Short git SHA from `build.rs`, with `-dirty` if the tree was dirty.
/// `unknown` when the build had no git metadata.
pub fn commit() -> &'static str {
    env!("GIT_COMMIT_HASH")
}

/// rustc target, e.g. `aarch64-apple-macos` / `x86_64-unknown-linux`.
pub fn architecture() -> String {
    let arch = env!("ZSTATS_TARGET_ARCH");
    let os = env!("ZSTATS_TARGET_OS");
    match env!("ZSTATS_TARGET_VENDOR") {
        "" | "unknown" => format!("{arch}-{os}"),
        vendor => format!("{arch}-{vendor}-{os}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_the_manifest() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
        assert!(!version().is_empty());
    }

    #[test]
    fn architecture_names_this_binary() {
        let label = architecture();
        assert!(
            label.contains(env!("ZSTATS_TARGET_ARCH")),
            "{label} should include the target arch"
        );
        assert!(
            label.contains(env!("ZSTATS_TARGET_OS")),
            "{label} should include the target os"
        );
    }

    #[test]
    fn commit_is_stamped_or_unknown() {
        let hash = commit().trim_end_matches("-dirty");
        assert!(
            hash == "unknown" || hash.chars().all(|c| c.is_ascii_hexdigit()),
            "commit {hash:?} is neither a SHA nor unknown"
        );
    }
}
