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
//! The flow's one piece of debris is cleaned at the next launch:
//! [`sweep_installer_mounts`] detaches the installer image the finished
//! update left mounted, because its bundle contends for the notification
//! identity and the banners silently stop.
//!
//! `releases/latest` excludes drafts and prereleases by definition, so
//! the rolling `nightly` build never counts as an update.

use crate::about;
use crate::opener;
use crate::proxy;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const LATEST_URL: &str = "https://api.github.com/repos/vicanso/zstats.app/releases/latest";
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// The DMG for this build's architecture — half the bytes of the
/// universal image (6.6 vs 13.3 MB measured on v0.1.1). `ARCH` is a
/// compile-time constant: a universal install runs its native slice,
/// so this picks the machine's real architecture. Unknown arch falls
/// back to the universal image, which fits everything.
fn asset_name() -> &'static str {
    match env::consts::ARCH {
        "aarch64" => "zstats-aarch64.dmg",
        "x86_64" => "zstats-x86_64.dmg",
        _ => "zstats.dmg",
    }
}
/// sha256sum-format digests the release workflow uploads beside it.
const CHECKSUMS_NAME: &str = "SHA256SUMS";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
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
) -> Result<PathBuf, String> {
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
        let n = io::Read::read(&mut reader, &mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        if bytes.len() as u64 > MAX_DOWNLOAD {
            return Err("download exceeded the size cap".into());
        }
        on_progress(bytes.len() as u64, total);
    }

    let path = env::temp_dir().join(format!("{tag}-{asset}"));
    fs::write(&path, &bytes).map_err(|e| e.to_string())?;

    // Transport-integrity check against the release's own digest list.
    // A missing SHA256SUMS degrades to unverified-but-proceed: the DMG
    // is signed and notarized, and Gatekeeper validates that signature
    // when the user installs — the checksum only fails *earlier*.
    //
    // Having the digest but failing to compute ours is a different
    // story and must not take the same exit: "we meant to verify and
    // could not" is a failure, not a pass. Handing over bytes we
    // intended to check and didn't would make the check decorative.
    if let Some(expected) = fetch_checksum(&agent, tag, asset) {
        let Some(got) = file_sha256(&path) else {
            let _ = fs::remove_file(&path);
            return Err("could not verify the download (could not hash it)".into());
        };
        if !got.eq_ignore_ascii_case(&expected) {
            let _ = fs::remove_file(&path);
            return Err(format!("checksum mismatch: expected {expected}, got {got}"));
        }
    }

    // Mount the image and bring Finder forward — LaunchServices opens
    // the drag window *behind* whatever is focused otherwise.
    opener::open([path.as_os_str()]).map_err(|e| e.to_string())?;
    let _ = opener::open(["-a", "Finder"]);
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

/// SHA-256 of the written file, computed in-process.
fn file_sha256(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    // Streamed rather than read-to-end: the DMG is ~13 MB today and
    // nothing says it stays that size. Hashing the file on disk, not
    // the buffer in hand, so a truncated write is caught too.
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Some(
        hasher
            .finalize()
            .iter()
            .fold(String::with_capacity(64), |mut out, byte| {
                out.push_str(&format!("{byte:02x}"));
                out
            }),
    )
}

// ---- installer-image sweep (once, at launch) ---------------------------

/// The identity notifications are granted to — must match
/// `[package.metadata.bundle] identifier` in Cargo.toml (a test guards
/// the pair, same as notify.rs guards the delivery side).
const BUNDLE_ID: &str = "com.github.vicanso.zstats";

/// The volume name the release workflow gives the DMG. Finder mounts
/// repeats as "zstats Installer 1", "… 2" — hence a prefix match.
const INSTALLER_VOLUME_PREFIX: &str = "zstats Installer";

/// LaunchServices' registration tool; no public API does this.
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

/// Detach the installer image a finished update left mounted.
///
/// `download_and_open` hands the DMG to the OS and the user drags the
/// bundle out — but nothing ever ejects the volume, Spotlight registers
/// the mounted copy with LaunchServices, and two live registrations of
/// one bundle id make notification attribution sway: deliveries still
/// log as accepted while no banner renders (measured on v0.1.13 —
/// System Settings showed zstats fully authorized, `banner="delivered"`
/// in the log, nothing on screen; ejecting the image brought the
/// banners straight back — docs/design.md 系统通知). So the freshly
/// installed version cleans up after the update that produced it.
///
/// Three gates keep it honest: the volume's bundle must carry *our*
/// identifier (a stranger's volume that happens to be named "zstats
/// Installer" is not ours to eject); the running executable must not
/// live on that volume (never pull the ground out from a copy launched
/// off the image); and the bundle must not be *newer* than the running
/// build — newer means downloaded-but-not-yet-copied, an install in
/// progress whose drag window must stay. A busy detach is left alone
/// with a warning; the next launch retries. Blocking child processes
/// (hdiutil takes a second or two) — background executor only.
pub fn sweep_installer_mounts() {
    let running = env::current_exe().ok();
    let Ok(volumes) = fs::read_dir("/Volumes") else {
        return;
    };
    for entry in volumes.flatten() {
        let name = entry.file_name();
        if !name
            .to_str()
            .is_some_and(|name| name.starts_with(INSTALLER_VOLUME_PREFIX))
        {
            continue;
        }
        let volume = entry.path();
        let app = volume.join("zstats.app");
        if bundle_plist_value(&app, "CFBundleIdentifier").as_deref() != Some(BUNDLE_ID) {
            continue;
        }
        if running.as_ref().is_some_and(|exe| exe.starts_with(&volume)) {
            continue;
        }
        let version = bundle_plist_value(&app, "CFBundleShortVersionString").unwrap_or_default();
        if is_newer(&version, about::version()) {
            continue;
        }
        let detached = Command::new("hdiutil")
            .arg("detach")
            .arg(volume.as_os_str())
            .output()
            .is_ok_and(|out| out.status.success());
        if !detached {
            tracing::warn!(volume = %volume.display(), "installer image would not detach");
            continue;
        }
        // LaunchServices keeps the record after the unmount (measured),
        // so the stale claimant is dropped explicitly.
        let _ = Command::new(LSREGISTER)
            .arg("-u")
            .arg(app.as_os_str())
            .output();
        tracing::info!(volume = %volume.display(), %version, "stale installer image detached");
    }
}

/// One string key out of a bundle's Info.plist, via `defaults read`
/// (which handles both the XML cargo-bundle writes and a binary
/// conversion something else may have made). `None` for a missing
/// bundle, key, or a failed spawn — every caller treats those alike.
fn bundle_plist_value(app: &Path, key: &str) -> Option<String> {
    let out = Command::new("defaults")
        .arg("read")
        // Sans extension — `defaults` appends ".plist" itself.
        .arg(app.join("Contents/Info").as_os_str())
        .arg(key)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8(out.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

pub enum UpdateCheck {
    UpToDate,
    Newer {
        /// The remote tag, "v0.1.2" — shown as-is.
        version: String,
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
    let notes = json_str_field(&body, "body")
        .unwrap_or_default()
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    if is_newer(&tag, about::version()) {
        UpdateCheck::Newer {
            version: tag,
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

// ---- silent periodic check (the settings gear's dot) -------------------

/// How often the silent background check may run. Two days: releases
/// land at most a few times a week, one ~1 KB API request every other
/// day is invisible, and the dot appears within a day or two of a
/// release without the user ever thinking about updates. Seven days is
/// the other sanctioned cadence — this one constant.
const AUTO_CHECK_EVERY: Duration = Duration::from_secs(2 * 24 * 60 * 60);

fn auto_check_path(dir: &Path) -> PathBuf {
    dir.join("update-check.toml")
}

/// What the check file holds. `ignored` is the user's "skip this
/// version": it silences the gear's dot for that tag only — the About
/// page's manual check keeps telling the truth, and the next release
/// (a different tag) brings the dot back on its own.
#[derive(Default)]
struct CheckFile {
    checked_unix: u64,
    latest: Option<String>,
    ignored: Option<String>,
}

fn read_check_in(dir: &Path) -> CheckFile {
    let doc = fs::read_to_string(auto_check_path(dir))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok());
    let Some(doc) = doc else {
        return CheckFile::default();
    };
    let get = |k: &str| doc.get(k).and_then(toml::Value::as_str).map(str::to_string);
    CheckFile {
        checked_unix: doc
            .get("checked_unix")
            .and_then(toml::Value::as_integer)
            .unwrap_or(0)
            .max(0) as u64,
        latest: get("latest"),
        ignored: get("ignored"),
    }
}

fn write_check_in(dir: &Path, file: &CheckFile) {
    // Tags come from GitHub's own JSON; the escape is belt only.
    let clean = |v: &str| v.replace(['\\', '"'], "");
    let mut out = format!("checked_unix = {}\n", file.checked_unix);
    if let Some(v) = &file.latest {
        out.push_str(&format!("latest = \"{}\"\n", clean(v)));
    }
    if let Some(v) = &file.ignored {
        out.push_str(&format!("ignored = \"{}\"\n", clean(v)));
    }
    let _ = fs::create_dir_all(dir);
    let _ = fs::write(auto_check_path(dir), out);
}

/// Whether the silent check is due: no record yet, or the last attempt
/// is older than [`AUTO_CHECK_EVERY`]. Attempts are stamped regardless
/// of outcome — an offline machine gets one try per period, not one
/// per tick.
pub fn auto_check_due(now: SystemTime) -> bool {
    auto_check_due_in(&zstats::settings::default_dir(), now)
}

fn auto_check_due_in(dir: &Path, now: SystemTime) -> bool {
    let checked = UNIX_EPOCH + Duration::from_secs(read_check_in(dir).checked_unix);
    now.duration_since(checked)
        .map_or(true, |age| age >= AUTO_CHECK_EVERY)
}

/// Stamp a check's outcome. `Newer` stores the version, `UpToDate`
/// clears it, and `Failed` keeps whatever the last successful check
/// learned — a network error says nothing about versions, it only
/// spends this period's attempt. The ignore mark always survives.
/// Manual checks record too: they answer the same question, so they
/// also reset the silent clock.
pub fn record_outcome(now: SystemTime, outcome: &UpdateCheck) {
    record_outcome_in(&zstats::settings::default_dir(), now, outcome);
}

fn record_outcome_in(dir: &Path, now: SystemTime, outcome: &UpdateCheck) {
    let mut file = read_check_in(dir);
    file.checked_unix = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match outcome {
        UpdateCheck::Newer { version, .. } => file.latest = Some(version.clone()),
        UpdateCheck::UpToDate => file.latest = None,
        UpdateCheck::Failed(_) => {}
    }
    write_check_in(dir, &file);
}

/// Mute the dot for `version` alone. Display-layer only, like the
/// banner snooze: checks keep running and recording, the About page
/// keeps answering truthfully — the unsolicited reminder is what stops.
pub fn ignore(version: &str) {
    ignore_in(&zstats::settings::default_dir(), version);
}

fn ignore_in(dir: &Path, version: &str) {
    let mut file = read_check_in(dir);
    file.ignored = Some(version.to_string());
    write_check_in(dir, &file);
}

/// The version the user chose to skip, while this build still has not
/// caught up to it. `None` once installed — the mark is then history,
/// not a standing choice.
pub fn ignored() -> Option<String> {
    ignored_in(&zstats::settings::default_dir(), about::version())
}

fn ignored_in(dir: &Path, current: &str) -> Option<String> {
    let marked = read_check_in(dir).ignored?;
    is_newer(&marked, current).then_some(marked)
}

/// Drop the skip mark: the dot and the About row speak up again.
pub fn unignore() {
    unignore_in(&zstats::settings::default_dir());
}

fn unignore_in(dir: &Path) {
    let mut file = read_check_in(dir);
    file.ignored = None;
    write_check_in(dir, &file);
}

/// The version a past check found and this build has not caught up to —
/// the meaning of the settings gear's dot. File plus version compare,
/// no network: installing the update clears the dot by comparison, not
/// by bookkeeping.
pub fn nudge() -> Option<String> {
    nudge_in(&zstats::settings::default_dir(), about::version())
}

fn nudge_in(dir: &Path, current: &str) -> Option<String> {
    let file = read_check_in(dir);
    let latest = file.latest?;
    if file.ignored.as_deref() == Some(latest.as_str()) {
        return None;
    }
    is_newer(&latest, current).then_some(latest)
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
    use std::process;

    /// The sweep ejects volumes on the strength of this identifier —
    /// if the bundle id ever moves, the const must move with it or the
    /// sweep goes blind (it would never *mis*-eject: a mismatch only
    /// makes it skip).
    #[test]
    fn the_sweep_identifier_matches_the_manifest() {
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains(&format!("identifier = \"{BUNDLE_ID}\"")));
    }

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

    /// The digest has to match what `sha256sum`/`shasum -a 256` print,
    /// because that is what the release's SHA256SUMS was generated with —
    /// lowercase hex, no separators. Known vectors, so a future crate bump
    /// cannot silently change the encoding.
    #[test]
    fn file_digest_matches_the_reference_vectors() {
        let dir = env::temp_dir().join(format!("zstats-sha-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let empty = dir.join("empty");
        fs::write(&empty, b"").unwrap();
        assert_eq!(
            file_sha256(&empty).as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );

        let abc = dir.join("abc");
        fs::write(&abc, b"abc").unwrap();
        assert_eq!(
            file_sha256(&abc).as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );

        // Larger than the 64 KiB read buffer: the streaming loop must
        // fold every chunk, not just the first.
        let big = dir.join("big");
        fs::write(&big, vec![b'z'; 200_000]).unwrap();
        let digest = file_sha256(&big).expect("hashed");
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );

        // A file that is not there is not a zero digest.
        assert_eq!(file_sha256(&dir.join("nope")), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn silent_check_cadence_ignore_and_nudge_round_trip() {
        let dir = env::temp_dir().join(format!("zstats-autocheck-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let newer = |v: &str| UpdateCheck::Newer {
            version: v.into(),
            notes: String::new(),
        };
        assert!(auto_check_due_in(&dir, t0), "no record yet means due");
        record_outcome_in(&dir, t0, &newer("v9.9.8"));
        assert!(!auto_check_due_in(&dir, t0 + Duration::from_secs(3600)));
        assert!(auto_check_due_in(&dir, t0 + AUTO_CHECK_EVERY));
        assert_eq!(nudge_in(&dir, "0.1.2"), Some("v9.9.8".into()));
        assert_eq!(
            nudge_in(&dir, "9.9.9"),
            None,
            "installing past it clears the dot by comparison"
        );
        // Skip this version: the dot goes quiet for v9.9.8 alone…
        ignore_in(&dir, "v9.9.8");
        assert_eq!(nudge_in(&dir, "0.1.2"), None);
        // …a failure keeps both the finding and the ignore mark…
        record_outcome_in(&dir, t0, &UpdateCheck::Failed("offline".into()));
        assert_eq!(nudge_in(&dir, "0.1.2"), None);
        assert_eq!(
            ignored_in(&dir, "0.1.2"),
            Some("v9.9.8".into()),
            "the skip is a standing choice while it still applies"
        );
        assert_eq!(
            ignored_in(&dir, "9.9.9"),
            None,
            "installing past it makes the mark history"
        );
        // Undo puts it back on the board.
        unignore_in(&dir);
        assert_eq!(nudge_in(&dir, "0.1.2"), Some("v9.9.8".into()));
        assert_eq!(ignored_in(&dir, "0.1.2"), None);
        ignore_in(&dir, "v9.9.8");
        // …and the next release brings the dot back on its own.
        record_outcome_in(&dir, t0, &newer("v9.9.9"));
        assert_eq!(nudge_in(&dir, "0.1.2"), Some("v9.9.9".into()));
        record_outcome_in(&dir, t0, &UpdateCheck::UpToDate);
        assert_eq!(nudge_in(&dir, "0.1.2"), None, "up-to-date clears it");
        let _ = fs::remove_dir_all(&dir);
    }
}
