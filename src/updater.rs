//! Check GitHub Releases for a newer version, and assist the install.
//!
//! Same shape as zedis: one user-triggered check, and — on request — a
//! download of the universal DMG, verified against the release's
//! SHA256SUMS, then handed to the OS (`open` mounts it, Finder is
//! brought forward for the drag-to-Applications window). The line that
//! is NOT crossed: no self-replacing. Copying over /Applications stays
//! a user act — replacing a running bundle underneath itself can fault
//! it, and Gatekeeper independently validates the notarized signature
//! at install time either way.
//!
//! `releases/latest` excludes drafts and prereleases by definition, so
//! the rolling `nightly` build never counts as an update.

use crate::about;
use crate::proxy;

const LATEST_URL: &str = "https://api.github.com/repos/vicanso/zstats.app/releases/latest";
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// The DMG for this build's architecture — half the bytes of the
/// universal image (6.6 vs 13.3 MB measured on v0.1.1). `ARCH` is a
/// compile-time constant: a universal install runs its native slice,
/// so this picks the machine's real architecture. Unknown arch falls
/// back to the universal image, which fits everything.
fn asset_name() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "zstats-aarch64.dmg",
        "x86_64" => "zstats-x86_64.dmg",
        _ => "zstats.dmg",
    }
}
/// sha256sum-format digests the release workflow uploads beside it.
const CHECKSUMS_NAME: &str = "SHA256SUMS";
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
/// Guards against a runaway body; the DMG is ~15 MB.
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

/// Deterministic per-tag asset URL — no JSON walking, and no race with
/// a release published mid-flow (unlike `latest/download/…`).
fn release_download_url(tag: &str, name: &str) -> String {
    format!("https://github.com/vicanso/zstats.app/releases/download/{tag}/{name}")
}

/// Download `tag`'s DMG, verify, and hand it to the OS. Returns the
/// downloaded path. Blocking (up to minutes) — background executor
/// only. `on_progress(received, total)`; `total` is 0 while unknown.
pub fn download_and_open(
    tag: &str,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<std::path::PathBuf, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .proxy(proxy::app_proxy())
        .build()
        .new_agent();
    let asset = asset_name();
    let response = agent
        .get(&release_download_url(tag, asset))
        .header("User-Agent", format!("zstats/{}", about::version()))
        .call()
        .map_err(|e| e.to_string())?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let mut reader = response.into_body().into_reader();
    let mut bytes: Vec<u8> = Vec::with_capacity(total.min(MAX_DOWNLOAD) as usize);
    let mut buf = [0u8; 64 * 1024];
    on_progress(0, total);
    loop {
        let n = std::io::Read::read(&mut reader, &mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        if bytes.len() as u64 > MAX_DOWNLOAD {
            return Err("download exceeded the size cap".into());
        }
        on_progress(bytes.len() as u64, total);
    }

    let path = std::env::temp_dir().join(format!("{tag}-{asset}"));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;

    // Transport-integrity check against the release's own digest list.
    // A missing SHA256SUMS degrades to unverified-but-proceed: the DMG
    // is signed and notarized, and Gatekeeper validates that signature
    // when the user installs — the checksum only fails *earlier*.
    if let Some(expected) = fetch_checksum(&agent, tag, asset)
        && let Some(got) = file_sha256(&path)
        && !got.eq_ignore_ascii_case(&expected)
    {
        let _ = std::fs::remove_file(&path);
        return Err(format!("checksum mismatch: expected {expected}, got {got}"));
    }

    // Mount the image and bring Finder forward — LaunchServices opens
    // the drag window *behind* whatever is focused otherwise.
    std::process::Command::new("open")
        .arg(&path)
        .spawn()
        .map_err(|e| e.to_string())?;
    let _ = std::process::Command::new("open")
        .args(["-a", "Finder"])
        .spawn();
    Ok(path)
}

/// The expected digest for `asset`, from the release's SHA256SUMS
/// (`<sha256>  <name>` lines).
fn fetch_checksum(agent: &ureq::Agent, tag: &str, asset: &str) -> Option<String> {
    let text = agent
        .get(&release_download_url(tag, CHECKSUMS_NAME))
        .header("User-Agent", format!("zstats/{}", about::version()))
        .call()
        .ok()?
        .into_body()
        .read_to_string()
        .ok()?;
    text.lines()
        .find(|line| line.trim_end().ends_with(&format!(" {asset}")))
        .and_then(|line| line.split_whitespace().next())
        .map(str::to_string)
}

/// `shasum -a 256` on the written file — ships with macOS, and this
/// whole install path is macOS-only anyway (it ends in a DMG).
fn file_sha256(path: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
}

pub enum UpdateCheck {
    UpToDate,
    Newer {
        /// The remote tag, "v0.1.2" — shown as-is.
        version: String,
        /// The release page; "go get it" opens this in the browser.
        url: String,
        /// Release body, unescaped. Empty when GitHub sent `null` or "".
        notes: String,
    },
    Failed(String),
}

/// Ask GitHub for the latest release and compare it to this build.
/// Blocking — call on the background executor.
pub fn check() -> UpdateCheck {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(FETCH_TIMEOUT))
        .proxy(proxy::app_proxy())
        .build()
        .new_agent();
    let body = match agent
        .get(LATEST_URL)
        .header("User-Agent", format!("zstats/{}", about::version()))
        .header("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(response) => match response.into_body().read_to_string() {
            Ok(body) => body,
            Err(e) => return UpdateCheck::Failed(e.to_string()),
        },
        Err(e) => return UpdateCheck::Failed(e.to_string()),
    };
    let Some(tag) = json_str_field(&body, "tag_name") else {
        return UpdateCheck::Failed("no tag_name in response".into());
    };
    let url = json_str_field(&body, "html_url")
        .unwrap_or_else(|| "https://github.com/vicanso/zstats.app/releases".into());
    let notes = json_str_field(&body, "body")
        .unwrap_or_default()
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    if is_newer(&tag, about::version()) {
        UpdateCheck::Newer {
            version: tag,
            url,
            notes,
        }
    } else {
        UpdateCheck::UpToDate
    }
}

/// Pull one string field out of the release JSON. No serde_json: the
/// payload is machine-generated, and we only need a handful of top-level
/// keys. `"key"` must be followed by `:` so a mention inside another
/// string (the notes body quoting `"tag_name"`) cannot win. `null`
/// becomes an empty string so a missing body is just "no notes".
fn json_str_field(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut search = 0;
    loop {
        let rel = json[search..].find(&needle)?;
        let after = search + rel + needle.len();
        let rest = json[after..].trim_start();
        if let Some(rest) = rest.strip_prefix(':') {
            let rest = rest.trim_start();
            if rest.starts_with("null") {
                return Some(String::new());
            }
            if let Some(rest) = rest.strip_prefix('"') {
                return unescape_json_string(rest);
            }
        }
        search = after;
    }
}

/// Walk a JSON string literal (the opening quote already consumed) and
/// unescape it. The notes body is full of `\"` and `\n`; a naïve
/// `find('"')` would cut it short.
fn unescape_json_string(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{0008}'),
                'f' => out.push('\u{000c}'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() != 4 {
                        return None;
                    }
                    let cp = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(cp)?);
                }
                _ => return None,
            },
            other => out.push(other),
        }
    }
    None
}

/// `v0.1.2` vs `0.1.1` — numeric segment compare, missing segments are
/// zero. Anything unparsable compares as not-newer: a malformed remote
/// tag must not nag about an "update".
fn is_newer(remote_tag: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split('.')
            .map(|seg| {
                seg.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    };
    let (remote, local) = (parse(remote_tag), parse(current));
    for i in 0..remote.len().max(local.len()) {
        let r = remote.get(i).copied().unwrap_or(0);
        let l = local.get(i).copied().unwrap_or(0);
        if r != l {
            return r > l;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_is_numeric_per_segment() {
        assert!(is_newer("v0.1.2", "0.1.1"));
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("v0.1.1", "0.1.1"));
        assert!(!is_newer("v0.1.0", "0.1.1"));
        // A fourth segment counts; a missing one reads as zero.
        assert!(is_newer("v0.1.1.1", "0.1.1"));
        assert!(!is_newer("v0.1.1", "0.1.1.0"));
        // Garbage never claims to be an update.
        assert!(!is_newer("nightly", "0.1.1"));
    }

    #[test]
    fn json_fields_come_from_the_top_level_not_the_notes() {
        let body = r#"{"url":"x","html_url":"https://github.com/vicanso/zstats.app/releases/tag/v0.1.2","id":1,"tag_name":"v0.1.2","body":"notes mentioning \"tag_name\": \"v9.9.9\" in text"}"#;
        assert_eq!(json_str_field(body, "tag_name").as_deref(), Some("v0.1.2"));
        assert_eq!(
            json_str_field(body, "html_url").as_deref(),
            Some("https://github.com/vicanso/zstats.app/releases/tag/v0.1.2")
        );
        assert_eq!(
            json_str_field(body, "body").as_deref(),
            Some("notes mentioning \"tag_name\": \"v9.9.9\" in text")
        );
        assert_eq!(json_str_field(body, "missing"), None);
    }

    #[test]
    fn json_string_unescapes_newlines_and_null_body() {
        // Regular string (not raw): `##` is reserved in edition 2024, and
        // a raw `r#"..."#` would also terminate at the `"#` inside `"# Notes`.
        let with_breaks = "{\"body\":\"Notes\\r\\n- fix \\\"foo\\\"\\n- bar\"}";
        assert_eq!(
            json_str_field(with_breaks, "body").as_deref(),
            Some("Notes\r\n- fix \"foo\"\n- bar")
        );
        assert_eq!(
            json_str_field("{\"body\":null}", "body").as_deref(),
            Some("")
        );
    }
}
