//! What macOS itself says about a MobileAsset directory.
//!
//! The whole-disk scope walks `/System/Library/AssetsV2`, where the
//! biggest rows on a developer's machine live — measured 6.8 GB here,
//! 5.3 GB of developer documentation alone on a user's. Ranked by size
//! those rows are useless: `com_apple_MobileAsset_UAF_Siri_Understanding`
//! is 2.5 GB of *something*, and a reader cannot act on a number whose
//! subject they cannot read.
//!
//! The obvious fix — a hand-written table saying what each asset type
//! is and whether it is safe to delete — is the one thing `cleanhints`
//! forbids: its entries come only from each tool's own documentation,
//! and Apple documents none of these names. A table written from
//! experience would be us guessing about deletion, on system files, in
//! a tooltip. So nothing here is authored: every asset carries an
//! `Info.plist`, and this module reports what that file already says.
//!
//! Same shape as the `CACHEDIR.TAG` rule the analyser already trusts —
//! *the owner declared it* — and the same restraint: a declaration
//! earns a sentence, never a delete button. MobileAsset content is
//! `mobileassetd`'s to reclaim (System Settings → General → Storage);
//! removing one of these directories by hand can have the system
//! download it again, or worse.

use std::path::Path;
use std::process::Command;

/// The directory-name prefix every asset type wears, which is its
/// `CFBundleIdentifier` with the dots turned into underscores
/// (`com.apple.MobileAsset.Font8` → `com_apple_MobileAsset_Font8`).
const TYPE_PREFIX: &str = "com_apple_MobileAsset_";

/// One `.asset` bundle's suffix.
const ASSET_SUFFIX: &str = ".asset";

/// Plists read for one row before the flags are dropped. A type
/// directory holds anywhere from zero to 57 assets (measured), and
/// beyond a handful the honest answer is the type name alone: a verdict
/// from a sample would be a claim about assets nobody looked at.
const MAX_PLISTS: usize = 8;

/// How deep under a row an asset bundle is looked for. Assets sit
/// directly under their type directory or one level down
/// (`purpose_auto/…`); deeper than that is the asset's own payload.
const MAX_DEPTH: usize = 2;

/// What the system declares about the asset(s) a row covers. Every
/// field is absent unless the plists said so — the whole point is that
/// nothing here is inferred.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AssetNote {
    /// The asset type, read off the directory name: the part after
    /// `com_apple_MobileAsset_`. Apple's own naming, not a rewrite.
    pub kind: String,
    /// `MobileAssetProperties.__RequiredByOS`. `Some(true)` is the
    /// system saying it needs this.
    pub required_by_os: Option<bool>,
    /// `__AssetDefaultGarbageCollectionBehavior` said `NeverCollected`
    /// or `Precious` — the system declaring it will not reclaim this on
    /// its own.
    pub never_collected: Option<bool>,
    /// `AssetLocale`, where one is declared: a 900 MB row is a
    /// different decision when it is one language's voice data.
    pub locale: Option<String>,
}

/// The note for a path under `AssetsV2`, or `None` for anything else.
///
/// Blocking (it shells out to `plutil`, the way the rest of the app
/// reads plists) — call it while assembling a finished scan, never per
/// frame.
pub fn note_for(path: &Path) -> Option<AssetNote> {
    let kind = kind_of(path)?;
    let mut note = AssetNote {
        kind,
        ..AssetNote::default()
    };
    let plists = plists_under(path);
    // More than the cap, or none at all: the name is all that can be
    // said without guessing.
    if plists.is_empty() || plists.len() > MAX_PLISTS {
        return Some(note);
    }
    let mut required: Vec<bool> = Vec::new();
    let mut never: Vec<bool> = Vec::new();
    let mut locales: Vec<String> = Vec::new();
    for plist in &plists {
        if let Some(v) = read_bool(plist, "MobileAssetProperties.__RequiredByOS") {
            required.push(v);
        }
        if let Some(v) = read_raw(
            plist,
            "MobileAssetProperties.__AssetDefaultGarbageCollectionBehavior",
        ) {
            never.push(matches!(v.as_str(), "NeverCollected" | "Precious"));
        }
        if let Some(v) = read_raw(plist, "MobileAssetProperties.AssetLocale") {
            locales.push(v);
        }
    }
    // Unanimous or nothing: assets under one type can disagree, and a
    // majority verdict about system files is not a fact.
    note.required_by_os = unanimous(&required);
    note.never_collected = unanimous(&never);
    note.locale = unanimous(&locales);
    Some(note)
}

/// `…/com_apple_MobileAsset_Font8/…` → `Font8`. `None` when the path
/// has no asset-type component, which is every path outside AssetsV2.
fn kind_of(path: &Path) -> Option<String> {
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .find_map(|name| name.strip_prefix(TYPE_PREFIX))
        .filter(|kind| !kind.is_empty())
        .map(str::to_string)
}

/// The `Info.plist` of every asset bundle at or under `path`, bounded
/// by [`MAX_DEPTH`] and stopped once past [`MAX_PLISTS`] (one over, so
/// the caller can tell "too many" from "exactly the cap").
fn plists_under(path: &Path) -> Vec<std::path::PathBuf> {
    // The row may be inside one asset already — then that bundle is the
    // answer, and no directory needs reading.
    if let Some(bundle) = enclosing_bundle(path) {
        let plist = bundle.join("Info.plist");
        return if plist.is_file() {
            vec![plist]
        } else {
            Vec::new()
        };
    }
    let mut found = Vec::new();
    collect(path, MAX_DEPTH, &mut found);
    found
}

/// The `*.asset` directory this path is in or is, if any.
fn enclosing_bundle(path: &Path) -> Option<std::path::PathBuf> {
    let mut current = Some(path);
    while let Some(dir) = current {
        if dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(ASSET_SUFFIX))
        {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

fn collect(dir: &Path, depth: usize, found: &mut Vec<std::path::PathBuf>) {
    if depth == 0 || found.len() > MAX_PLISTS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if found.len() > MAX_PLISTS {
            return;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(ASSET_SUFFIX))
        {
            let plist = path.join("Info.plist");
            if plist.is_file() {
                found.push(plist);
            }
        } else {
            collect(&path, depth - 1, found);
        }
    }
}

/// `Some` only when every reading agrees; an empty list has no verdict.
fn unanimous<T: PartialEq + Clone>(values: &[T]) -> Option<T> {
    let first = values.first()?;
    values.iter().all(|v| v == first).then(|| first.clone())
}

/// One key out of a plist, via `plutil` — the same posture as the rest
/// of the app's plist reads (`defaults` in updater.rs): these files are
/// binary as often as XML, and the system's own tool decodes both.
fn read_raw(plist: &Path, key: &str) -> Option<String> {
    let out = Command::new("plutil")
        .arg("-extract")
        .arg(key)
        .arg("raw")
        .arg("-o")
        .arg("-")
        .arg(plist)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8(out.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// The flag is written as `0`/`1` on some assets and `true`/`false` on
/// others (measured: both spellings on this machine), so both are read.
fn read_bool(plist: &Path, key: &str) -> Option<bool> {
    match read_raw(plist, key)?.as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_type_comes_off_the_directory_name() {
        assert_eq!(
            kind_of(&PathBuf::from(
                "/System/Library/AssetsV2/com_apple_MobileAsset_Font8/x.asset"
            ))
            .as_deref(),
            Some("Font8")
        );
        // The identifier's own dots survive as underscores — Apple's
        // spelling, left alone rather than rewritten into a guess.
        assert_eq!(
            kind_of(&PathBuf::from(
                "/System/Library/AssetsV2/com_apple_MobileAsset_DictionaryServices_dictionary3macOS"
            ))
            .as_deref(),
            Some("DictionaryServices_dictionary3macOS")
        );
        assert_eq!(kind_of(&PathBuf::from("/Users/x/Downloads")), None);
        // The bare prefix names no type.
        assert_eq!(
            kind_of(&PathBuf::from(
                "/System/Library/AssetsV2/com_apple_MobileAsset_"
            )),
            None
        );
    }

    #[test]
    fn a_row_inside_a_bundle_resolves_to_that_bundle() {
        let deep = PathBuf::from("/x/com_apple_MobileAsset_Font8/a.asset/AssetData/Restore");
        assert_eq!(
            enclosing_bundle(&deep),
            Some(PathBuf::from("/x/com_apple_MobileAsset_Font8/a.asset"))
        );
        // A type directory is not inside a bundle: its assets get
        // enumerated instead.
        assert_eq!(
            enclosing_bundle(&PathBuf::from("/x/com_apple_MobileAsset_Font8")),
            None
        );
    }

    /// Assets under one type can disagree, and a majority verdict about
    /// system files is not a fact — so disagreement says nothing.
    #[test]
    fn only_a_unanimous_reading_becomes_a_claim() {
        assert_eq!(unanimous(&[true, true]), Some(true));
        assert_eq!(unanimous(&[true, false]), None);
        assert_eq!(unanimous::<bool>(&[]), None);
        assert_eq!(
            unanimous(&["zh_Hans".to_string(), "zh_Hans".to_string()]),
            Some("zh_Hans".to_string())
        );
    }
}
