//! Check GitHub Releases for a newer version, download, verify — and
//! finish the install.
//!
//! Same shape as zedis up to the download: one user-triggered check,
//! then the DMG for this architecture, verified against the release's
//! SHA256SUMS. The install used to stop at `open`-ing the image for
//! the manual drag; that asked the user to finish the update by hand
//! and left the mounted volume behind every single time (the
//! notification-killing debris [`sweep_installer_mounts`] exists for).
//! [`install`] now completes it in place when it can: mount silently,
//! copy the bundle over the running one, detach. Two boundaries keep
//! the old fault worry answered. The running bundle is *renamed
//! aside* into the temp directory, never deleted while the process
//! running from it is alive — every file it might still fault in
//! stays reachable, and the OS prunes temp on its own schedule. And
//! anything that blocks the in-place path — bare `cargo run` with no
//! bundle, an unwritable /Applications, a foreign bundle on the
//! image — degrades to the old drag flow instead of failing; that
//! flow is exactly what shipped before, and it works from anywhere.
//! The checksum is the integrity story: ureq writes no quarantine
//! xattr, so Gatekeeper never re-inspects the copy — what the
//! SHA256SUMS line vouched for is what runs.
//!
//! The flow detaches its own image now; [`sweep_installer_mounts`] at
//! the next launch remains the backstop for a detach that reported
//! busy, an install abandoned half-way, and the volumes older builds
//! left mounted — a lingering installer volume contends for the
//! notification identity and the banners silently stop.
//!
//! `releases/latest` excludes drafts and prereleases by definition, so
//! the rolling `nightly` build never counts as an update.
//!
//! Every request has a second address: the release workflow mirrors each
//! tagged release to Gitee asset for asset, and both the check and the
//! download fall back to it when GitHub cannot be reached — see
//! [`GITEE_API`] for why a mirror does not weaken the checksum story.

use crate::about;
#[cfg(target_os = "macos")]
use crate::opener;
use crate::proxy;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::{self, Command};
#[cfg(target_os = "macos")]
use std::thread;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const LATEST_URL: &str = "https://api.github.com/repos/vicanso/zstats.app/releases/latest";
/// The Gitee mirror the release workflow copies every tagged release to,
/// asset for asset. It exists for one audience: networks that cannot
/// reach GitHub at all, where the check below would otherwise only ever
/// fail and the user would never learn a release happened.
///
/// A mirror is only safe because it is not a second build. The workflow
/// uploads the *same files* the GitHub release carries, `SHA256SUMS`
/// included, so the digest that vouches for a download is unchanged no
/// matter which host served the bytes — a mirror serving something else
/// fails [`file_sha256`] exactly like a corrupted transfer would.
/// Anonymous reads; the token in CI is for writing.
const GITEE_API: &str = "https://gitee.com/api/v5/repos/vicanso/zstats.app";
/// The mirror's web root. Downloads do not live under `/api`, and unlike
/// the API they need no token — see [`gitee_download_url`].
const GITEE_REPO_URL: &str = "https://gitee.com/vicanso/zstats.app";
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// The DMG for this build's architecture — half the bytes of the
/// universal image (6.6 vs 13.3 MB measured on v0.1.1). `ARCH` is a
/// compile-time constant: a universal install runs its native slice,
/// so this picks the machine's real architecture. Unknown arch falls
/// back to the universal image, which fits everything.
#[cfg(target_os = "macos")]
fn asset_name() -> Option<&'static str> {
    Some(match env::consts::ARCH {
        "aarch64" => "zstats-aarch64.dmg",
        "x86_64" => "zstats-x86_64.dmg",
        _ => "zstats.dmg",
    })
}

/// The tarball publish.yml stages for this architecture. No universal
/// fallback exists here, so an architecture without a build gets `None`
/// and an honest refusal rather than a 404 dressed up as a download.
#[cfg(target_os = "linux")]
fn asset_name() -> Option<&'static str> {
    match env::consts::ARCH {
        "x86_64" => Some("zstats-linux-x86_64.tar.gz"),
        "aarch64" => Some("zstats-linux-aarch64.tar.gz"),
        _ => None,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn asset_name() -> Option<&'static str> {
    None
}
/// sha256sum-format digests the release workflow uploads beside it.
const CHECKSUMS_NAME: &str = "SHA256SUMS";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// How long to spend *reaching* a host, as opposed to how long a
/// transfer may take.
///
/// [`DOWNLOAD_TIMEOUT`] is five minutes because a 17 MB image over a
/// slow link legitimately needs them — but that same budget also
/// covered the GitHub attempt that must fail before the mirror is
/// tried. So on exactly the networks the mirror exists for, a blocked
/// GitHub could sit for five minutes per asset (ten across the DMG and
/// its SHA256SUMS) behind a progress bar that never moved. Splitting
/// the two means an unreachable host is abandoned in seconds while a
/// slow one still gets the whole transfer window. Matches
/// [`FETCH_TIMEOUT`], which is the same judgement about the same hosts.
const REACH_TIMEOUT: Duration = Duration::from_secs(10);
/// Guards against a runaway body; the DMG is ~15 MB.
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

/// Deterministic per-tag asset URL — no JSON walking, and no race with
/// a release published mid-flow (unlike `latest/download/…`).
fn release_download_url(tag: &str, name: &str) -> String {
    format!("https://github.com/vicanso/zstats.app/releases/download/{tag}/{name}")
}

/// GET `url` as text. The two hosts differ only in whether the GitHub
/// media type is worth asking for.
fn fetch_text(agent: &ureq::Agent, url: &str, accept: Option<&str>) -> Result<String, String> {
    let mut request = agent
        .get(url)
        .header("User-Agent", format!("zstats/{}", about::version()));
    if let Some(accept) = accept {
        request = request.header("Accept", accept);
    }
    request
        .call()
        .map_err(|e| e.to_string())?
        .into_body()
        .read_to_string()
        .map_err(|e| e.to_string())
}

/// One release asset, GitHub first and the mirror second.
///
/// The order is deliberate: GitHub is the origin, the URL there needs no
/// lookup, and a reachable network pays nothing for the mirror existing.
/// A blocked one pays [`REACH_TIMEOUT`] once per asset before falling
/// back — seconds, not the transfer budget — which is cheaper than
/// guessing which host to prefer and being wrong.
fn fetch_asset(
    agent: &ureq::Agent,
    tag: &str,
    name: &str,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    let direct = match agent
        .get(&release_download_url(tag, name))
        .header("User-Agent", format!("zstats/{}", about::version()))
        .call()
    {
        Ok(response) => return Ok(response),
        Err(e) => e.to_string(),
    };
    agent
        .get(&gitee_download_url(tag, name))
        .header("User-Agent", format!("zstats/{}", about::version()))
        .call()
        .map_err(|e| format!("{direct}; mirror: {e}"))
}

/// The mirror's URL for one attachment of `tag` — spelled, not looked up.
///
/// This used to cost two API calls and a walk over the attachment array,
/// on the premise that "Gitee's attachment URLs carry the attachment's
/// own id, so they cannot be spelled from the tag and the file name".
/// **That premise was wrong.** The id appears only in the *intermediate*
/// redirect (`/attach_files/<id>/download/<name>`); the URL Gitee
/// advertises as `browser_download_url`, and the one that works, is
/// `/releases/download/<tag>/<name>` — the same shape GitHub uses, with
/// no id in it (measured 2026-09-19 against the live mirror).
///
/// The walk was also broken, and silently so: it split the array on `{`
/// believing there was one object per attachment, but every entry embeds
/// an `uploader` object, so the fragment holding `"name"` and the
/// fragment holding `"browser_download_url"` were never the same one and
/// the lookup returned `None` every time. Its test passed throughout
/// because the fixture had no nested object. Spelling the URL deletes
/// the hand-rolled JSON walk that made that possible, and turns three
/// requests into one.
fn gitee_download_url(tag: &str, name: &str) -> String {
    format!("{GITEE_REPO_URL}/releases/download/{tag}/{name}")
}

/// Download `tag`'s asset for this build and verify it. Returns the
/// downloaded path, ready for [`install`]. Blocking (up to minutes) —
/// background executor only. `on_progress(received, total)`; `total` is
/// 0 while unknown.
pub fn download(tag: &str, mut on_progress: impl FnMut(u64, u64)) -> Result<PathBuf, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        // Reaching a host and transferring from it are different
        // questions with very different right answers — see
        // [`REACH_TIMEOUT`].
        .timeout_resolve(Some(REACH_TIMEOUT))
        .timeout_connect(Some(REACH_TIMEOUT))
        .proxy(proxy::app_proxy())
        .build()
        .new_agent();
    let Some(asset) = asset_name() else {
        return Err(format!(
            "no release build for {} on this platform",
            env::consts::ARCH
        ));
    };
    let response = fetch_asset(&agent, tag, asset)?;
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
    match fetch_checksum(&agent, tag, asset) {
        Some(expected) => {
            let Some(got) = file_sha256(&path) else {
                let _ = fs::remove_file(&path);
                return Err("could not verify the download (could not hash it)".into());
            };
            if !got.eq_ignore_ascii_case(&expected) {
                let _ = fs::remove_file(&path);
                return Err(format!("checksum mismatch: expected {expected}, got {got}"));
            }
        }
        // Off macOS nothing else stands between these bytes and `exec`:
        // no signature, no Gatekeeper. The digest is the whole check, so
        // its absence is a refusal, not a shrug — the same rule
        // `install-linux.sh` applies to the same file.
        None if cfg!(not(target_os = "macos")) => {
            let _ = fs::remove_file(&path);
            return Err(
                "the release carries no SHA256SUMS; not installing unverified bytes".into(),
            );
        }
        None => {}
    }

    Ok(path)
}

/// The expected digest for `asset`, from the release's SHA256SUMS
/// (`<sha256>  <name>` lines).
fn fetch_checksum(agent: &ureq::Agent, tag: &str, asset: &str) -> Option<String> {
    let text = fetch_asset(agent, tag, CHECKSUMS_NAME)
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

// ---- in-place install --------------------------------------------------

/// What [`install`] did with the verified download.
pub enum Delivery {
    /// The fresh build was put in place of the running one; a relaunch
    /// completes the update.
    Replaced,
    /// The image was handed to the OS for the classic drag — no bundle
    /// to replace, or the in-place copy could not proceed. macOS only:
    /// Linux has no drag to fall back to, so there a blocked install is
    /// an error that names the file it left behind.
    #[cfg(target_os = "macos")]
    OpenedForDrag,
}

/// Install the verified image — in place when possible: mount without
/// a Finder window, copy the bundle over the running one, detach. The
/// drag, the "quit to install" and the stray mounted volume all
/// disappear. Anything that blocks the in-place path degrades to the
/// old manual flow (mount visibly, bring Finder forward) with a log
/// line saying why, rather than failing — an `Err` here means even
/// `open` refused. Blocking (hdiutil and ditto take seconds) —
/// background executor only.
#[cfg(target_os = "macos")]
pub fn install(dmg: &Path) -> Result<Delivery, String> {
    match running_bundle() {
        Some(target) => match install_over(&target, dmg) {
            Ok(()) => return Ok(Delivery::Replaced),
            Err(e) => {
                tracing::warn!(error = %e, "in-place install fell back to the drag window");
            }
        },
        None => tracing::info!("no running bundle (bare binary); opening the image to install"),
    }
    // Mount the image and bring Finder forward — LaunchServices opens
    // the drag window *behind* whatever is focused otherwise.
    opener::open([dmg.as_os_str()]).map_err(|e| e.to_string())?;
    let _ = opener::open(["-a", "Finder"]);
    Ok(Delivery::OpenedForDrag)
}

/// The bundle this process runs from — `…/zstats.app` for the
/// installed app, `None` under bare `cargo run`. `current_exe` reports
/// the path recorded at exec time, so after an in-place install it
/// names the *new* copy at the same location — exactly what a relaunch
/// wants, and why [`replace_bundle`] may move the file it points at.
#[cfg(target_os = "macos")]
fn running_bundle() -> Option<PathBuf> {
    bundle_root_of(&env::current_exe().ok()?)
}

/// `…/Foo.app/Contents/MacOS/foo` → `…/Foo.app`.
#[cfg(target_os = "macos")]
fn bundle_root_of(exe: &Path) -> Option<PathBuf> {
    let root = exe.parent()?.parent()?.parent()?;
    (root.extension().is_some_and(|ext| ext == "app")).then(|| root.to_path_buf())
}

/// Mount, replace `target`, detach. The volume is detached on every
/// exit — a failed copy must not leave the image mounted on top of the
/// failure it just reported.
#[cfg(target_os = "macos")]
fn install_over(target: &Path, dmg: &Path) -> Result<(), String> {
    let volume = attach(dmg)?;
    let result = replace_bundle(target, &volume);
    detach(&volume);
    result
}

/// Swap `target` for the bundle on the mounted volume. The old bundle
/// is renamed aside into the temp directory, never deleted: the
/// running process keeps every file it might still fault in, and the
/// OS prunes temp on its own schedule. The rename doubles as the
/// permission gate — an unwritable /Applications, or running straight
/// off a read-only image, fails it before anything has moved (a
/// cross-volume ceiling too: /Applications and $TMPDIR both live on
/// the data volume, so the rename only crosses filesystems in setups
/// unusual enough to deserve the manual flow). A failed copy renames
/// the old bundle straight back.
#[cfg(target_os = "macos")]
fn replace_bundle(target: &Path, volume: &Path) -> Result<(), String> {
    let fresh = volume.join(BUNDLE_NAME);
    if bundle_plist_value(&fresh, "CFBundleIdentifier").as_deref() != Some(BUNDLE_ID) {
        return Err("the mounted image does not carry our bundle".into());
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let aside = env::temp_dir().join(format!("zstats-previous-{}-{stamp}.app", process::id()));
    fs::rename(target, &aside).map_err(|e| format!("could not move the old bundle aside: {e}"))?;
    let copied = Command::new("/usr/bin/ditto")
        .arg(fresh.as_os_str())
        .arg(target.as_os_str())
        .output();
    let failure = match &copied {
        Ok(out) if out.status.success() => {
            tracing::info!(target = %target.display(), "update installed in place");
            return Ok(());
        }
        Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_string(),
        Err(e) => e.to_string(),
    };
    if let Err(e) = fs::rename(&aside, target) {
        tracing::error!(aside = %aside.display(), error = %e, "could not restore the old bundle");
    }
    Err(format!("ditto failed: {failure}"))
}

/// Mount the image without a Finder window and return its mount point,
/// parsed from hdiutil's own plist output. `-noverify` because the
/// image's bytes were already vouched for: [`download`] hashed the
/// whole file against the release's SHA256SUMS, and hdiutil's default
/// pass re-reads the entire image to answer the same question — 2.5s
/// vs 0.3s measured on a 16 MB DMG, most of what the user waits
/// through as "installing".
#[cfg(target_os = "macos")]
fn attach(dmg: &Path) -> Result<PathBuf, String> {
    let out = Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-noverify", "-plist"])
        .arg(dmg.as_os_str())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "hdiutil attach: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    mount_point_from_plist(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| "no mount point in hdiutil's output".into())
}

/// The `mount-point` string out of `hdiutil attach -plist` — the one
/// value needed, scanned in the same no-serde spirit as
/// [`json_str_field`]. The volume name is ours and ASCII ("zstats
/// Installer"), so XML entity escapes cannot occur in the value.
#[cfg(target_os = "macos")]
fn mount_point_from_plist(xml: &str) -> Option<PathBuf> {
    let after = xml.split("<key>mount-point</key>").nth(1)?;
    let start = after.find("<string>")? + "<string>".len();
    let end = start + after[start..].find("</string>")?;
    Some(PathBuf::from(&after[start..end]))
}

/// Detach the installer volume, with one retry after a beat — hdiutil
/// answers "resource busy" while Spotlight is still indexing the fresh
/// mount. A volume that stays stuck is left with a warning: the launch
/// sweep detaches it next start (by then the installed version equals
/// the running one, so the "not newer" gate passes).
#[cfg(target_os = "macos")]
fn detach(volume: &Path) {
    for attempt in 0..2 {
        if attempt > 0 {
            thread::sleep(Duration::from_secs(1));
        }
        let detached = Command::new("hdiutil")
            .arg("detach")
            .arg(volume.as_os_str())
            .output()
            .is_ok_and(|out| out.status.success());
        if detached {
            return;
        }
    }
    tracing::warn!(volume = %volume.display(), "installer image left mounted; the launch sweep will retry");
}

/// Quit-and-restart, for the button on the "installed" row. The
/// restart is handed to a detached `sh` that waits for this pid to
/// exit and then `open`s the bundle — the path rides in `$0`, so no
/// quoting happens inside the script. The caller quits right after;
/// the shell outlives us as launchd's orphan, so it is never a zombie
/// of ours.
#[cfg(target_os = "macos")]
pub fn relaunch() {
    let Some(bundle) = running_bundle() else {
        return;
    };
    let script = format!(
        "while /bin/kill -0 {pid} 2>/dev/null; do /bin/sleep 0.1; done; /usr/bin/open \"$0\"",
        pid = process::id()
    );
    match Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg(bundle.as_os_str())
        .spawn()
    {
        Ok(_) => {
            tracing::info!(bundle = %bundle.display(), "restart requested to finish the update")
        }
        Err(e) => tracing::warn!(error = %e, "could not spawn the relauncher"),
    }
}

// ---- Linux: tarball over the running binary ----------------------------

/// Where the Linux install put the new binary, for [`relaunch`].
///
/// Needed because Linux reports `/proc/self/exe` as `<path> (deleted)`
/// the moment the file the process started from is unlinked — which is
/// exactly what replacing it does (the rename retires the old name; the
/// process keeps its mapped inode). `current_exe()` after an install
/// therefore names a file that no longer exists under that name; macOS
/// has the opposite behaviour (`running_bundle` above). So the path is
/// recorded at install time, once, before anything moves.
#[cfg(target_os = "linux")]
static INSTALLED_AT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// The temp directory an install unpacks into. A prefix rather than a
/// fixed name so a crashed install's leftovers are recognisable to the
/// launch sweep.
#[cfg(target_os = "linux")]
const UNPACK_PREFIX: &str = "zstats-update-";

/// Install the verified tarball over this process's own binary. Blocking
/// (`tar` plus a copy) — background executor only. There is no
/// drag-window fallback here: a target that cannot be replaced — one a
/// package manager owns, typically — is an error that says so, and the
/// verified tarball stays in temp for whoever wants to finish by hand.
#[cfg(target_os = "linux")]
pub fn install(tarball: &Path) -> Result<Delivery, String> {
    let target = env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let target = without_deleted_suffix(&target);
    install_into(tarball, &target)?;
    *INSTALLED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(target);
    Ok(Delivery::Replaced)
}

/// The install proper, with the target named — so the whole path can be
/// exercised against a fixture tarball and a temp file, which is the
/// only way this ever gets tested on a machine that is not upgrading.
#[cfg(target_os = "linux")]
fn install_into(tarball: &Path, target: &Path) -> Result<(), String> {
    let unpack = env::temp_dir().join(format!("{UNPACK_PREFIX}{}", process::id()));
    let _ = fs::remove_dir_all(&unpack);
    fs::create_dir_all(&unpack)
        .map_err(|e| format!("could not create {}: {e}", unpack.display()))?;
    let result = unpack_and_replace(tarball, &unpack, target);
    let _ = fs::remove_dir_all(&unpack);
    result
}

#[cfg(target_os = "linux")]
fn unpack_and_replace(tarball: &Path, unpack: &Path, target: &Path) -> Result<(), String> {
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(tarball.as_os_str())
        .arg("-C")
        .arg(unpack.as_os_str())
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "tar -xzf: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let member = tarball_member_dir().ok_or("no release build for this architecture")?;
    let fresh = unpack.join(member).join(crate::APP_NAME);
    if !fresh.is_file() {
        return Err(format!(
            "the tarball has no {}/{} in it",
            member,
            crate::APP_NAME
        ));
    }
    replace_binary(target, &fresh)
}

/// The directory at the tarball's root: the asset's name without its
/// extension, which is how publish.yml stages it.
#[cfg(target_os = "linux")]
fn tarball_member_dir() -> Option<&'static str> {
    asset_name()?.strip_suffix(".tar.gz")
}

/// Copy beside, then rename over. The rename is atomic on one
/// filesystem, and Linux unlinks the *name*, so the running process
/// keeps the inode it was mapped from — the same "never pull the
/// ground out from under the running copy" the macOS path gets by
/// renaming the old bundle aside. Copying first is what makes an
/// unwritable directory fail before anything has moved.
#[cfg(target_os = "linux")]
fn replace_binary(target: &Path, fresh: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("the running binary has no file name")?;
    let staged = target.with_file_name(format!("{name}.new"));
    fs::copy(fresh, &staged).map_err(|e| {
        format!(
            "could not stage the new binary beside {}: {e} — installed by a package manager? \
             update it there",
            target.display()
        )
    })?;
    if let Err(e) = fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)) {
        let _ = fs::remove_file(&staged);
        return Err(format!("could not mark the new binary executable: {e}"));
    }
    if let Err(e) = fs::rename(&staged, target) {
        let _ = fs::remove_file(&staged);
        return Err(format!("could not replace {}: {e}", target.display()));
    }
    tracing::info!(target = %target.display(), "update installed in place");
    Ok(())
}

/// `/proc/self/exe` with the ` (deleted)` the kernel appends once the
/// file has been unlinked — see [`INSTALLED_AT`].
#[cfg(target_os = "linux")]
fn without_deleted_suffix(exe: &Path) -> PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(clean) => PathBuf::from(clean),
        None => exe.to_path_buf(),
    }
}

/// Quit-and-restart, the Linux shape of the macOS one above: a detached
/// `sh` waits for this pid to exit and then `exec`s the binary — the
/// path rides in `$0`, so nothing is quoted inside the script. Our
/// environment (the Wayland display, `XDG_RUNTIME_DIR`) is inherited,
/// and the single-instance socket the old process leaves behind is
/// exactly the stale one `ipc::claim` clears.
#[cfg(target_os = "linux")]
pub fn relaunch() {
    let recorded = INSTALLED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let Some(exe) =
        recorded.or_else(|| env::current_exe().ok().map(|e| without_deleted_suffix(&e)))
    else {
        return;
    };
    let script = format!(
        "while kill -0 {pid} 2>/dev/null; do sleep 0.1; done; exec \"$0\"",
        pid = process::id()
    );
    match Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg(exe.as_os_str())
        .spawn()
    {
        Ok(_) => tracing::info!(exe = %exe.display(), "restart requested to finish the update"),
        Err(e) => tracing::warn!(error = %e, "could not spawn the relauncher"),
    }
}

/// The Linux counterpart of the mount sweep: no image to detach, but an
/// install that died mid-way leaves its unpack directory in temp.
/// Nothing is installing at launch, so every one of them is stale.
#[cfg(target_os = "linux")]
pub fn sweep_installer_mounts() {
    let Ok(entries) = fs::read_dir(env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(UNPACK_PREFIX));
        if stale && fs::remove_dir_all(entry.path()).is_ok() {
            tracing::info!(dir = %entry.path().display(), "stale update unpack directory removed");
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn install(_asset: &Path) -> Result<Delivery, String> {
    Err("in-place install is not implemented on this platform".into())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn relaunch() {}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn sweep_installer_mounts() {}

// ---- installer-image sweep (once, at launch) ---------------------------

/// The identity notifications are granted to — must match
/// `[package.metadata.bundle] identifier` in Cargo.toml (a test guards
/// the pair, same as notify.rs guards the delivery side).
#[cfg(target_os = "macos")]
const BUNDLE_ID: &str = "com.github.vicanso.zstats";

/// The bundle's name, on the image and on disk.
#[cfg(target_os = "macos")]
const BUNDLE_NAME: &str = "zstats.app";

/// The volume name the release workflow gives the DMG. Finder mounts
/// repeats as "zstats Installer 1", "… 2" — hence a prefix match.
#[cfg(target_os = "macos")]
const INSTALLER_VOLUME_PREFIX: &str = "zstats Installer";

/// LaunchServices' registration tool; no public API does this.
#[cfg(target_os = "macos")]
const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

/// Detach the installer image an update left mounted.
///
/// The manual-drag era left one behind every time: nothing ejected the
/// volume, Spotlight registered the mounted copy with LaunchServices,
/// and two live registrations of one bundle id make notification
/// attribution sway — deliveries still log as accepted while no banner
/// renders (measured on v0.1.13 — System Settings showed zstats fully
/// authorized, `banner="delivered"` in the log, nothing on screen;
/// ejecting the image brought the banners straight back — see
/// docs/design.md 系统通知). [`install`] now detaches its own image,
/// so this is the backstop: a detach that stayed busy, an install the
/// fallback path handed to a drag window, volumes older builds left
/// mounted.
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
#[cfg(target_os = "macos")]
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
        let app = volume.join(BUNDLE_NAME);
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
#[cfg(target_os = "macos")]
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
    // Both hosts name the same two fields (`tag_name`, `body`), so the
    // parsing below does not care which answered. Gitee's `latest` is
    // also free of the nightly question: the workflow mirrors tagged
    // releases only.
    let body = match fetch_text(&agent, LATEST_URL, Some("application/vnd.github+json")) {
        Ok(body) => body,
        Err(origin) => match fetch_text(&agent, &format!("{GITEE_API}/releases/latest"), None) {
            Ok(body) => body,
            Err(mirror) => return UpdateCheck::Failed(format!("{origin}; mirror: {mirror}")),
        },
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
    #[cfg(target_os = "macos")]
    #[test]
    fn the_sweep_identifier_matches_the_manifest() {
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains(&format!("identifier = \"{BUNDLE_ID}\"")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mount_point_comes_out_of_hdiutil_plist_output() {
        // Trimmed real output: the disk entity has no mount-point key,
        // the filesystem entity carries it.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>system-entities</key><array>
  <dict>
    <key>content-hint</key><string>GUID_partition_scheme</string>
    <key>dev-entry</key><string>/dev/disk5</string>
  </dict>
  <dict>
    <key>content-hint</key><string>Apple_HFS</string>
    <key>dev-entry</key><string>/dev/disk5s1</string>
    <key>mount-point</key>
    <string>/Volumes/zstats Installer</string>
  </dict>
</array></dict></plist>"#;
        assert_eq!(
            mount_point_from_plist(xml),
            Some(PathBuf::from("/Volumes/zstats Installer"))
        );
        assert_eq!(mount_point_from_plist("<plist></plist>"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bundle_root_is_the_app_directory_or_nothing() {
        assert_eq!(
            bundle_root_of(Path::new("/Applications/zstats.app/Contents/MacOS/zstats")),
            Some(PathBuf::from("/Applications/zstats.app"))
        );
        // Bare `cargo run` has no bundle to replace.
        assert_eq!(
            bundle_root_of(Path::new("/Users/x/proj/target/debug/zstats")),
            None
        );
    }

    /// The Linux install end to end against a fixture: a tarball laid
    /// out the way publish.yml stages it, a target file to replace, and
    /// afterwards the target carries the new bytes, is executable, and
    /// no `.new` staging file is left beside it. Real `tar` — a second
    /// at most, and the one tool the path depends on.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_verified_tarball_replaces_the_binary_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let dir = env::temp_dir().join(format!("zstats-linux-install-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        let member = tarball_member_dir().expect("this architecture has a build");
        let payload = dir.join("payload").join(member);
        fs::create_dir_all(&payload).unwrap();
        fs::write(payload.join(crate::APP_NAME), b"new build").unwrap();
        let tarball = dir.join("release.tar.gz");
        let tar = Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(dir.join("payload"))
            .arg(member)
            .status()
            .unwrap();
        assert!(tar.success());
        let target = dir.join("bin").join("zstats");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"old build").unwrap();

        install_into(&tarball, &target).expect("install");

        assert_eq!(fs::read(&target).unwrap(), b"new build");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755,
            "the replacement is executable"
        );
        assert!(
            !target.with_file_name("zstats.new").exists(),
            "no staging file is left beside the binary"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A target nobody may write to — a package-managed /usr/bin — has
    /// to fail before anything moves, and say what the reader should do
    /// instead. The old binary must be untouched afterwards.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_unwritable_target_fails_before_anything_moves() {
        use std::os::unix::fs::PermissionsExt;
        let dir = env::temp_dir().join(format!("zstats-linux-ro-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let target = bin.join("zstats");
        fs::write(&target, b"old build").unwrap();
        let fresh = dir.join("fresh");
        fs::write(&fresh, b"new build").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o555)).unwrap();

        let err = replace_binary(&target, &fresh).expect_err("read-only directory");

        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(err.contains("package manager"), "{err}");
        assert_eq!(fs::read(&target).unwrap(), b"old build", "nothing moved");
        assert!(!bin.join("zstats.new").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// What `/proc/self/exe` reads as after the file was replaced.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_deleted_suffix_the_kernel_appends_is_stripped() {
        assert_eq!(
            without_deleted_suffix(Path::new("/opt/zstats/zstats (deleted)")),
            PathBuf::from("/opt/zstats/zstats")
        );
        assert_eq!(
            without_deleted_suffix(Path::new("/opt/zstats/zstats")),
            PathBuf::from("/opt/zstats/zstats")
        );
        assert_eq!(
            tarball_member_dir().map(|m| m.starts_with("zstats-linux-")),
            Some(true)
        );
    }

    /// Build a DMG whose payload is `zstats.app` carrying `id` as its
    /// bundle identifier, under `dir`. Real hdiutil, ~a second — which is
    /// why this and everything below it is macOS-only: the in-place
    /// install is a `.app` on a mounted image, and Linux has neither.
    #[cfg(target_os = "macos")]
    fn fixture_dmg(dir: &Path, id: &str, marker: &[u8], volname: &str) -> PathBuf {
        let contents = dir.join("payload").join(BUNDLE_NAME).join("Contents");
        fs::create_dir_all(contents.join("MacOS")).unwrap();
        fs::write(contents.join("MacOS/zstats"), marker).unwrap();
        fs::write(
            contents.join("Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>{id}</string>
</dict></plist>
"#
            ),
        )
        .unwrap();
        let dmg = dir.join("update.dmg");
        let created = Command::new("hdiutil")
            .args(["create", "-srcfolder"])
            .arg(dir.join("payload").as_os_str())
            .args(["-volname", volname, "-format", "UDZO", "-quiet"])
            .arg(dmg.as_os_str())
            .output()
            .unwrap();
        assert!(created.status.success(), "hdiutil create failed");
        dmg
    }

    /// An old bundle standing where the install will land.
    #[cfg(target_os = "macos")]
    fn fixture_target(dir: &Path) -> PathBuf {
        let target = dir.join("Applications").join(BUNDLE_NAME);
        fs::create_dir_all(target.join("Contents/MacOS")).unwrap();
        fs::write(target.join("Contents/MacOS/zstats"), b"old build").unwrap();
        target
    }

    /// The asides this test run parked in temp — found by prefix, so
    /// the test can both assert the old bundle survived and clean up.
    #[cfg(target_os = "macos")]
    fn asides() -> Vec<PathBuf> {
        let prefix = format!("zstats-previous-{}-", process::id());
        fs::read_dir(env::temp_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
            })
            .collect()
    }

    /// The whole in-place path against a real image: mount, verify the
    /// bundle id, rename the old bundle aside (kept, not deleted),
    /// copy the new one in, detach the volume.
    #[cfg(target_os = "macos")]
    #[test]
    fn in_place_install_swaps_the_bundle_and_detaches() {
        let dir = env::temp_dir().join(format!("zstats-inplace-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let volname = "zstats-inplace-test";
        let dmg = fixture_dmg(&dir, BUNDLE_ID, b"new build", volname);
        let target = fixture_target(&dir);

        install_over(&target, &dmg).expect("in-place install");

        assert_eq!(
            fs::read(target.join("Contents/MacOS/zstats")).unwrap(),
            b"new build"
        );
        assert!(
            !Path::new("/Volumes").join(volname).exists(),
            "the volume must be detached"
        );
        // The old bundle was parked, not destroyed — the running
        // process may still fault pages in from it.
        let parked = asides();
        assert!(
            parked
                .iter()
                .any(|p| fs::read(p.join("Contents/MacOS/zstats"))
                    .is_ok_and(|bytes| bytes == b"old build")),
            "the old bundle must survive in temp"
        );
        for p in parked {
            let _ = fs::remove_dir_all(p);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// One spelling serves both hosts: everything after `/releases/` is
    /// identical, which is the property that lets the mirror be a second
    /// *address* rather than a second lookup.
    ///
    /// What this replaces is worth remembering. The old code read the
    /// mirror's attachment array to find a URL, and its fixture here
    /// carried two beliefs that the live API does not share — that an
    /// attachment's download URL contains its id, and that an entry has
    /// no nested objects. Both were wrong, the array walk therefore
    /// never matched anything, and this test passed the whole time
    /// because it was asking the fixture rather than Gitee. A test whose
    /// sample is hand-written can only ever check the belief that wrote
    /// it.
    #[test]
    fn the_mirror_url_is_spelled_the_same_way_githubs_is() {
        let tag = "v0.3.2";
        let name = "zstats-aarch64.dmg";
        assert_eq!(
            gitee_download_url(tag, name),
            "https://gitee.com/vicanso/zstats.app/releases/download/v0.3.2/zstats-aarch64.dmg"
        );
        let tail = |url: String| url.split_once("/releases/").map(|(_, t)| t.to_string());
        assert_eq!(
            tail(release_download_url(tag, name)),
            tail(gitee_download_url(tag, name)),
            "the two hosts differ only in their root"
        );
    }

    /// The identity gate fires before anything moves — and the volume
    /// still gets detached on the failure exit.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_foreign_bundle_is_refused_before_anything_moves() {
        let dir = env::temp_dir().join(format!("zstats-foreign-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let volname = "zstats-foreign-test";
        let dmg = fixture_dmg(&dir, "com.example.stranger", b"impostor", volname);
        let target = fixture_target(&dir);

        let refused = install_over(&target, &dmg);

        assert!(refused.is_err(), "a foreign identifier must be refused");
        assert_eq!(
            fs::read(target.join("Contents/MacOS/zstats")).unwrap(),
            b"old build",
            "the standing install must be untouched"
        );
        assert!(
            !Path::new("/Volumes").join(volname).exists(),
            "the volume must be detached even on refusal"
        );
        let _ = fs::remove_dir_all(&dir);
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
