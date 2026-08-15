//! Stamp the binary with the git commit it was built from.
//!
//! About needs a real id, not "whatever Cargo.toml says". Missing `.git`
//! (a source tarball, some CI checkouts) falls back to `unknown` rather
//! than failing the build.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Ok(head) = std::fs::read_to_string(".git/HEAD")
        && let Some(rest) = head.strip_prefix("ref: ")
    {
        println!("cargo:rerun-if-changed=.git/{}", rest.trim());
    }

    let mut hash = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    if hash != "unknown" && git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty()) {
        hash.push_str("-dirty");
    }
    println!("cargo:rustc-env=GIT_COMMIT_HASH={hash}");

    // CARGO_CFG_* is set for the build script, not for the crate.
    fn stamp(from: &str, into: &str) {
        let value = std::env::var(from).unwrap_or_default();
        println!("cargo:rustc-env={into}={value}");
    }
    stamp("CARGO_CFG_TARGET_ARCH", "ZSTATS_TARGET_ARCH");
    stamp("CARGO_CFG_TARGET_OS", "ZSTATS_TARGET_OS");
    stamp("CARGO_CFG_TARGET_VENDOR", "ZSTATS_TARGET_VENDOR");
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}
