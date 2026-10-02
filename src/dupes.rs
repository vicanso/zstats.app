//! Exact duplicate files — same bytes, any type — for the disk-space
//! window's Duplicates tab.
//!
//! Three passes, each only over what the one before could not rule out:
//! group by length (two files of different lengths are never equal),
//! then a BLAKE3 hash of the first [`HEAD_BYTES`] of every file that
//! shares a length, then the whole file for those whose heads still
//! match. Almost everything is settled by the length alone, and the
//! full read — the one real cost — is paid only by files that are
//! almost certainly copies. That is why a re-downloaded installer or a
//! video imported twice is found in seconds.
//!
//! **Not the analyser's walk.** That walk keeps one counter per
//! directory and no file list, by design (a million-inode `~/Library`
//! in a menu-bar app), and the scope differs anyway: duplicates are
//! looked for among the user's own files. The default scope is the home
//! folder, files of [`HOME_MIN_BYTES`] and up; a folder the user picks is
//! searched as soon as it is picked, every non-empty file in it. Either
//! way `~/Library` is left out unless it is what was picked (or the
//! picked folder is inside it — iCloud Drive lives there): what is in it
//! is apps' own. And some places are never descended ([`skips_dir`]),
//! because what is inside belongs to a tool or an app and trashing one
//! "copy" breaks the whole: hidden directories (a `.git`, a toolchain),
//! `CACHEDIR.TAG` trees, every directory the clean-up hints name as a
//! tool's own (`~/go/pkg/mod`, `node_modules`, `target` — measured on a
//! developer's home, Go's module cache alone was a third of the groups,
//! and it is read-only, so the move would fail anyway), a `vendor`
//! directory beside the manifest that fills it, and packages (`.app`, a
//! Photos or Final Cut library). The analyser's TCC list is pruned too,
//! and an iCloud placeholder is never read (that would download it).
//!
//! **What "can be freed" means.** A hard link is one file under two
//! names — listed once, never a duplicate of itself, and a file that
//! still has another name frees nothing when this one goes
//! ([`DupeFile::linked`]). Every file is listed once per `(device,
//! inode)`, not only linked ones: a bind mount shows one file at two
//! paths with a link count of 1, and offering to trash "the other copy"
//! there would delete the only one. An APFS clone (what Finder's
//! Duplicate makes) shares its blocks with the original until one is
//! edited, so trashing it frees nothing: clones are grouped by
//! `ATTR_CMNEXT_CLONEID` ([`clone_key`]), shown as sharing storage, and
//! each set counts once toward [`DupeGroup::reclaimable`]. Nothing here
//! deletes; the view moves single copies to the Trash, never the last.

use crate::cleanhints;
use crate::diskscan;
use jwalk::{Parallelism, WalkDirGeneric};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// The default scope's floor. Below a megabyte a duplicate frees too
/// little to be worth a row, and small files are most of a home folder.
pub const HOME_MIN_BYTES: u64 = 1024 * 1024;

/// How much of each same-length file the second pass hashes. Enough to
/// tell apart nearly every pair of different files of equal length
/// (headers, first frames, first pages differ) for one small read.
const HEAD_BYTES: usize = 64 * 1024;

/// Read size for the full hash.
const READ_CHUNK: usize = 1024 * 1024;

/// How often progress lands on the channel.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// Directory names never descended, wherever they are. The clean-up
/// hints name these too; listed here as well because a user's hints file
/// replaces the built-in one wholesale, and dropping a line from it must
/// not make a dependency tree look like the user's duplicates.
const SKIP_NAMES: &[&str] = &["node_modules", "Pods"];

/// Manifests whose tool fills a `vendor` directory beside them (Go,
/// Composer, Bundler, `cargo vendor`). Only a `vendor` next to one is
/// skipped: the name alone is also an ordinary folder name.
const VENDOR_MANIFESTS: &[&str] = &["go.mod", "composer.json", "Gemfile", "Cargo.toml"];

/// Package extensions: a directory that is one document to the user
/// and to its app. Its insides are the app's to manage — trashing a
/// "duplicate" inside a Photos library corrupts the library.
const PACKAGES: &[&str] = &[
    "app",
    "appex",
    "bundle",
    "framework",
    "plugin",
    "kext",
    "photoslibrary",
    "photolibrary",
    "migratedphotolibrary",
    "aplibrary",
    "musiclibrary",
    "tvlibrary",
    "imovielibrary",
    "fcpbundle",
    "logicx",
    "band",
    "lrdata",
    "xcarchive",
    "sparsebundle",
    "pages",
    "numbers",
    "key",
];

/// Where to look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DupeScope {
    /// `$HOME`, files of [`HOME_MIN_BYTES`] and up.
    Home,
    /// A folder the user picked: every non-empty file in it.
    Folder(PathBuf),
}

impl DupeScope {
    fn root(&self) -> Option<PathBuf> {
        match self {
            DupeScope::Home => env::var_os("HOME").map(PathBuf::from),
            DupeScope::Folder(path) => Some(path.clone()),
        }
    }

    /// Smallest file compared. A picked folder has no floor beyond
    /// "not empty": every empty file equals every other and frees
    /// nothing.
    pub fn min_bytes(&self) -> u64 {
        match self {
            DupeScope::Home => HOME_MIN_BYTES,
            DupeScope::Folder(_) => 1,
        }
    }
}

/// One copy.
#[derive(Clone, Debug, PartialEq)]
pub struct DupeFile {
    pub path: PathBuf,
    /// Physical bytes (`st_blocks`), what trashing it could free.
    pub bytes: u64,
    pub modified: Option<SystemTime>,
    /// Shares its blocks with another copy in this group (APFS clone):
    /// trashing it frees nothing while the other stays.
    pub shares_storage: bool,
    /// Has other names (hard links), inside the scope or not: moving
    /// this name to the Trash frees nothing while they stay.
    pub linked: bool,
    storage: (u64, u64),
}

impl DupeFile {
    /// Whether moving this copy to the Trash (and emptying it) gives
    /// its bytes back while the rest of its group stays.
    pub fn frees(&self) -> bool {
        !self.shares_storage && !self.linked
    }
}

/// Files with identical content.
#[derive(Clone, Debug, PartialEq)]
pub struct DupeGroup {
    /// Length of each copy.
    pub len: u64,
    pub files: Vec<DupeFile>,
    /// What trashing every copy but one would free: one storage set
    /// kept, every other set counted once (clones share a set). A set
    /// holding a linked file cannot be freed from here, and it is the
    /// one kept when there is one.
    pub reclaimable: u64,
}

impl DupeGroup {
    /// Re-derive the clone sets and [`DupeGroup::reclaimable`] after a
    /// copy left the group.
    pub fn settle(&mut self) {
        #[derive(Default)]
        struct Set {
            bytes: u64,
            copies: usize,
            pinned: bool,
        }
        let mut sets: HashMap<(u64, u64), Set> = HashMap::new();
        for file in &self.files {
            let set = sets.entry(file.storage).or_default();
            set.bytes = set.bytes.max(file.bytes);
            set.copies += 1;
            set.pinned |= file.linked;
        }
        for file in &mut self.files {
            file.shares_storage = sets.get(&file.storage).is_some_and(|set| set.copies > 1);
        }
        let freeable: u64 = sets.values().filter(|s| !s.pinned).map(|s| s.bytes).sum();
        let kept = if sets.values().any(|s| s.pinned) {
            0
        } else {
            sets.values().map(|s| s.bytes).max().unwrap_or(0)
        };
        self.reclaimable = freeable - kept;
    }

    /// The copy changed longest ago, when exactly one is — usually the
    /// original, and the fact a careful reader looks for before choosing
    /// which to keep. `None` when a copy has no date or the earliest is
    /// shared (a clone or `cp -p` keeps the original's time): then the
    /// dates cannot tell the copies apart, and a mark would be a guess.
    pub fn oldest(&self) -> Option<&Path> {
        let dated: Vec<(&Path, SystemTime)> = self
            .files
            .iter()
            .map(|f| f.modified.map(|at| (f.path.as_path(), at)))
            .collect::<Option<_>>()?;
        let earliest = dated.iter().map(|(_, at)| *at).min()?;
        let mut at_earliest = dated.iter().filter(|(_, at)| *at == earliest);
        let first = at_earliest.next()?;
        at_earliest.next().is_none().then_some(first.0)
    }

    /// Whether `path` may go to the Trash now: it and at least one other
    /// copy are still on disk as the search left them — same length,
    /// same modification time. Asked right before the move, because the
    /// result is a photograph: an edited copy is no longer a duplicate
    /// (trashing it would lose the edit), and another copy deleted or
    /// changed since must not be what makes this one look spare. Not a
    /// re-hash — the length and the clock are what any edit moves.
    pub fn spare(&self, path: &Path) -> bool {
        let unchanged = |f: &DupeFile| {
            fs::symlink_metadata(&f.path).is_ok_and(|m| {
                m.is_file() && m.len() == self.len && m.modified().ok() == f.modified
            })
        };
        self.files
            .iter()
            .find(|f| f.path == path)
            .is_some_and(unchanged)
            && self.files.iter().filter(|f| f.path != path).any(unchanged)
    }
}

#[derive(Clone, Debug)]
pub struct DupeResult {
    pub scope: DupeScope,
    pub scanned_at: SystemTime,
    pub took: Duration,
    /// Files at or over the scope's floor that were compared by length.
    pub files_seen: usize,
    /// Files that could not be read for hashing (permission, vanished).
    pub unreadable: usize,
    /// Groups with something to free first, most first.
    pub groups: Vec<DupeGroup>,
}

impl DupeResult {
    pub fn reclaimable(&self) -> u64 {
        self.groups.iter().map(|g| g.reclaimable).sum()
    }

    /// Copies beyond the first in every group.
    pub fn extra_copies(&self) -> usize {
        self.groups.iter().map(|g| g.files.len() - 1).sum()
    }

    pub fn group_of(&self, path: &Path) -> Option<&DupeGroup> {
        self.groups
            .iter()
            .find(|g| g.files.iter().any(|f| f.path == path))
    }

    /// Drop one copy (it went to the Trash): out of its group, and the
    /// group out of the result once a single copy is left.
    pub fn remove(&mut self, path: &Path) {
        for group in &mut self.groups {
            group.files.retain(|f| f.path != path);
            group.settle();
        }
        self.groups.retain(|g| g.files.len() > 1);
    }
}

pub enum DupeEvent {
    /// Files found so far while walking.
    Walking {
        files: usize,
    },
    /// Bytes read so far by the hashing passes, against the total those
    /// passes will read.
    Hashing {
        read: u64,
        total: u64,
    },
    Done(Box<DupeResult>),
    Failed(String),
}

/// Search `scope` on its own thread; events arrive on `tx`. A cancelled
/// search says nothing.
pub fn spawn(scope: DupeScope, cancel: Arc<AtomicBool>, tx: smol::channel::Sender<DupeEvent>) {
    thread::spawn(move || match run(&scope, &cancel, &tx) {
        Ok(Some(result)) => {
            let _ = tx.send_blocking(DupeEvent::Done(Box::new(result)));
        }
        Ok(None) => {}
        Err(e) => {
            let _ = tx.send_blocking(DupeEvent::Failed(e));
        }
    });
}

struct Candidate {
    path: PathBuf,
    len: u64,
    bytes: u64,
    modified: Option<SystemTime>,
    linked: bool,
}

fn run(
    scope: &DupeScope,
    cancel: &AtomicBool,
    tx: &smol::channel::Sender<DupeEvent>,
) -> Result<Option<DupeResult>, String> {
    let started = Instant::now();
    let root = scope.root().ok_or_else(|| "HOME is not set".to_string())?;
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }
    let Some(candidates) = walk(scope, &root, cancel, tx) else {
        return Ok(None);
    };
    let files_seen = candidates.len();
    let mut unreadable = 0usize;

    // Pass 1: length. Only lengths shared by two or more files go on.
    let mut by_len: HashMap<u64, Vec<Candidate>> = HashMap::new();
    for c in candidates {
        by_len.entry(c.len).or_default().push(c);
    }
    let same_len: Vec<Vec<Candidate>> = by_len.into_values().filter(|g| g.len() > 1).collect();

    // What the two hashing passes will read, for the progress line: every
    // head, and (an upper bound) every whole file past its head.
    let total: u64 = same_len
        .iter()
        .flatten()
        .map(|c| c.len.min(HEAD_BYTES as u64) + c.len.saturating_sub(HEAD_BYTES as u64))
        .sum();
    let mut progress = Progress::new(total, tx);

    // Pass 2: the head.
    let mut by_head: Vec<Vec<Candidate>> = Vec::new();
    for group in same_len {
        let mut split: HashMap<[u8; 32], Vec<Candidate>> = HashMap::new();
        for c in group {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            match hash_file(&c.path, Some(HEAD_BYTES), cancel, &mut progress) {
                Ok(Some(hash)) => split.entry(hash).or_default().push(c),
                Ok(None) => return Ok(None),
                Err(_) => unreadable += 1,
            }
        }
        by_head.extend(split.into_values().filter(|g| g.len() > 1));
    }

    // Pass 3: the whole file, only where the head did not cover it.
    let mut groups: Vec<DupeGroup> = Vec::new();
    for group in by_head {
        let len = group[0].len;
        let finished: Vec<Vec<Candidate>> = if len <= HEAD_BYTES as u64 {
            vec![group]
        } else {
            let mut split: HashMap<[u8; 32], Vec<Candidate>> = HashMap::new();
            for c in group {
                match hash_file(&c.path, None, cancel, &mut progress) {
                    Ok(Some(hash)) => split.entry(hash).or_default().push(c),
                    Ok(None) => return Ok(None),
                    Err(_) => unreadable += 1,
                }
            }
            split.into_values().filter(|g| g.len() > 1).collect()
        };
        for same in finished {
            let mut group = DupeGroup {
                len,
                files: same
                    .into_iter()
                    .map(|c| DupeFile {
                        storage: clone_key(&c.path),
                        path: c.path,
                        bytes: c.bytes,
                        modified: c.modified,
                        shares_storage: false,
                        linked: c.linked,
                    })
                    .collect(),
                reclaimable: 0,
            };
            group.files.sort_by(|a, b| a.path.cmp(&b.path));
            group.settle();
            groups.push(group);
        }
    }
    groups.sort_by_key(|g| (Reverse(g.reclaimable), Reverse(g.len)));
    Ok(Some(DupeResult {
        scope: scope.clone(),
        scanned_at: SystemTime::now(),
        took: started.elapsed(),
        files_seen,
        unreadable,
        groups,
    }))
}

/// Every regular, local, non-empty file of at least the scope's floor,
/// one path per `(device, inode)` (module doc). `None` when cancelled.
fn walk(
    scope: &DupeScope,
    root: &Path,
    cancel: &AtomicBool,
    tx: &smol::channel::Sender<DupeEvent>,
) -> Option<Vec<Candidate>> {
    let min = scope.min_bytes();
    let mut deny = diskscan::tcc_deny();
    // Pruned only where the walk meets it from above: a root that is
    // `~/Library` or inside it is searched as picked.
    if let Some(home) = env::var_os("HOME") {
        deny.push(Path::new(&home).join("Library"));
    }
    // The analyser's exclusions are not applied: that list lives on the
    // Analysis tab, and here it would be a filter nobody can see. Picking
    // a folder is how this search is narrowed.
    let root_owned = root.to_path_buf();
    let walk = WalkDirGeneric::<((), Option<fs::Metadata>)>::new(root)
        .follow_links(false)
        .skip_hidden(false)
        .parallelism(Parallelism::RayonNewPool(diskscan::walk_threads()))
        .process_read_dir(move |_depth, _path, _state, children| {
            for child in children.iter_mut().flatten() {
                if child.file_type.is_dir() {
                    let path = child.path();
                    if path != root_owned && (deny.contains(&path) || skips_dir(&path)) {
                        child.read_children = None;
                    }
                } else if child.file_type.is_file() {
                    child.client_state = fs::symlink_metadata(child.path()).ok();
                }
            }
        });
    let mut out = Vec::new();
    let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();
    let mut last = Instant::now();
    for entry in walk {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let Ok(mut entry) = entry else {
            continue;
        };
        if !entry.file_type.is_file() {
            continue;
        }
        let Some(meta) = entry.client_state.take() else {
            continue;
        };
        let len = meta.len();
        if len == 0 || len < min || diskscan::is_dataless(&meta) {
            continue;
        }
        let (identity, linked) = identity(&meta);
        if let Some(identity) = identity
            && !seen_inodes.insert(identity)
        {
            continue; // the same file under another path, already listed
        }
        out.push(Candidate {
            path: entry.path(),
            len,
            bytes: diskscan::physical_size(&meta),
            modified: meta.modified().ok(),
            linked,
        });
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            let _ = tx.try_send(DupeEvent::Walking { files: out.len() });
        }
    }
    Some(out)
}

/// A directory the search never descends (module doc).
fn skips_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name.starts_with('.') || SKIP_NAMES.contains(&name) {
        return true;
    }
    if name
        .rsplit_once('.')
        .is_some_and(|(_, ext)| PACKAGES.contains(&ext.to_ascii_lowercase().as_str()))
    {
        return true;
    }
    if name == "vendor"
        && let Some(parent) = path.parent()
        && VENDOR_MANIFESTS.iter().any(|m| parent.join(m).is_file())
    {
        return true;
    }
    cleanhints::lookup(path).is_some() || diskscan::has_cache_tag(path)
}

/// A file's `(device, inode)`, and whether it has more than one name.
fn identity(meta: &fs::Metadata) -> (Option<(u64, u64)>, bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (Some((meta.dev(), meta.ino())), meta.nlink() > 1)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        (None, false)
    }
}

struct Progress<'a> {
    read: u64,
    total: u64,
    last: Instant,
    tx: &'a smol::channel::Sender<DupeEvent>,
}

impl<'a> Progress<'a> {
    fn new(total: u64, tx: &'a smol::channel::Sender<DupeEvent>) -> Self {
        Progress {
            read: 0,
            total,
            last: Instant::now(),
            tx,
        }
    }

    fn add(&mut self, bytes: u64) {
        self.read += bytes;
        if self.last.elapsed() >= PROGRESS_EVERY {
            self.last = Instant::now();
            let _ = self.tx.try_send(DupeEvent::Hashing {
                read: self.read.min(self.total),
                total: self.total,
            });
        }
    }
}

/// BLAKE3 of the first `limit` bytes, or of the whole file. `Ok(None)`
/// when cancelled mid-read.
fn hash_file(
    path: &Path,
    limit: Option<usize>,
    cancel: &AtomicBool,
    progress: &mut Progress,
) -> io::Result<Option<[u8; 32]>> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; limit.unwrap_or(READ_CHUNK).min(READ_CHUNK)];
    let mut left = limit.unwrap_or(usize::MAX);
    while left > 0 {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let want = buf.len().min(left);
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        progress.add(n as u64);
        left -= n;
    }
    Ok(Some(*hasher.finalize().as_bytes()))
}

/// The storage a copy's bytes live in: `(device, clone id)` on APFS,
/// where pure clones share a clone id; the file's own `(device, inode)`
/// anywhere else, so every copy is its own storage.
fn clone_key(path: &Path) -> (u64, u64) {
    let fallback = || {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            fs::symlink_metadata(path)
                .map(|m| (m.dev(), m.ino()))
                .unwrap_or_default()
        }
        #[cfg(not(unix))]
        {
            (0, 0)
        }
    };
    #[cfg(target_os = "macos")]
    if let Some(key) = apfs::clone_key(path) {
        return key;
    }
    fallback()
}

#[cfg(target_os = "macos")]
mod apfs {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const ATTR_CMNEXT_CLONEID: u32 = 0x0000_0100;
    const FSOPT_ATTR_CMN_EXTENDED: u32 = 0x0000_0020;
    const ATTR_CMN_DEVID: u32 = 0x0000_0002;

    #[repr(C, packed(4))]
    struct Reply {
        len: u32,
        returned: libc::attribute_set_t,
        dev: libc::dev_t,
        clone_id: u64,
    }

    /// `(device, clone id)` through `getattrlist`. `None` where the
    /// volume does not report a clone id (not APFS).
    pub(super) fn clone_key(path: &Path) -> Option<(u64, u64)> {
        let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: zeroed POD the kernel fills; sizes passed alongside.
        let mut list: libc::attrlist = unsafe { std::mem::zeroed() };
        list.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        list.commonattr = libc::ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_DEVID;
        list.forkattr = ATTR_CMNEXT_CLONEID;
        let mut reply: Reply = unsafe { std::mem::zeroed() };
        // SAFETY: valid C string, attrlist and reply buffer with its size.
        let rc = unsafe {
            libc::getattrlist(
                c_path.as_ptr(),
                &mut list as *mut _ as *mut libc::c_void,
                &mut reply as *mut _ as *mut libc::c_void,
                std::mem::size_of::<Reply>(),
                FSOPT_ATTR_CMN_EXTENDED | libc::FSOPT_NOFOLLOW,
            )
        };
        if rc != 0 {
            return None;
        }
        let returned = reply.returned;
        if returned.forkattr & ATTR_CMNEXT_CLONEID == 0 {
            return None;
        }
        let dev = reply.dev;
        let clone_id = reply.clone_id;
        Some((dev as u64, clone_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    fn scratch(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("zstats-dupes-{tag}-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn search(dir: &Path) -> DupeResult {
        let (tx, _rx) = smol::channel::unbounded();
        run(
            &DupeScope::Folder(dir.to_path_buf()),
            &AtomicBool::new(false),
            &tx,
        )
        .unwrap()
        .unwrap()
    }

    fn bytes(seed: u8, len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| seed.wrapping_add((i % 251) as u8))
            .collect()
    }

    #[test]
    fn copies_group_and_near_misses_do_not() {
        let dir = scratch("groups");
        let big = bytes(1, 300_000);
        fs::write(dir.join("a.dmg"), &big).unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/a copy.dmg"), &big).unwrap();
        // Same length and the same first 64 KB, different tail: the
        // full pass must split it off.
        let mut tail = big.clone();
        *tail.last_mut().unwrap() ^= 0xff;
        fs::write(dir.join("almost.dmg"), &tail).unwrap();
        // Same length, different head: split by the second pass.
        fs::write(dir.join("other.bin"), bytes(9, 300_000)).unwrap();
        // Small duplicates count in a picked folder; empty files never.
        fs::write(dir.join("n1.txt"), b"hello").unwrap();
        fs::write(dir.join("n2.txt"), b"hello").unwrap();
        fs::write(dir.join("e1"), b"").unwrap();
        fs::write(dir.join("e2"), b"").unwrap();

        let result = search(&dir);
        assert_eq!(result.groups.len(), 2, "{:?}", result.groups);
        let big_group = &result.groups[0];
        assert_eq!(big_group.len, 300_000);
        let names: Vec<_> = big_group
            .files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["a.dmg", "a copy.dmg"].map(std::ffi::OsString::from));
        assert!(big_group.reclaimable > 0);
        assert_eq!(result.groups[1].len, 5);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hard_link_is_not_a_duplicate() {
        let dir = scratch("links");
        fs::write(dir.join("a"), bytes(3, 200_000)).unwrap();
        fs::hard_link(dir.join("a"), dir.join("b")).unwrap();
        assert!(search(&dir).groups.is_empty());
        // A real copy beside the linked pair is a duplicate — and the
        // linked name is the one kept: trashing it would free nothing.
        fs::write(dir.join("c"), bytes(3, 200_000)).unwrap();
        let result = search(&dir);
        assert_eq!(result.groups.len(), 1);
        let group = &result.groups[0];
        assert_eq!(group.files.len(), 2, "one name for the linked pair");
        let linked = group.files.iter().find(|f| f.linked).expect("a or b");
        assert!(!linked.frees());
        let copy = group.files.iter().find(|f| !f.linked).unwrap();
        assert!(copy.frees());
        assert_eq!(group.reclaimable, copy.bytes);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn packages_hidden_dirs_and_node_modules_are_not_searched() {
        let dir = scratch("skips");
        let data = bytes(5, 100_000);
        for inner in [
            "Photos Library.photoslibrary/originals",
            ".git/objects",
            "node_modules/pkg",
            "Some.app/Contents",
        ] {
            fs::create_dir_all(dir.join(inner)).unwrap();
            fs::write(dir.join(inner).join("blob"), &data).unwrap();
        }
        // A Go project's vendored copy is the module's, not a duplicate;
        // a folder that is merely called vendor is searched.
        fs::create_dir_all(dir.join("proj/vendor/x")).unwrap();
        fs::write(dir.join("proj/go.mod"), b"module proj\n").unwrap();
        fs::write(dir.join("proj/vendor/x/blob"), &data).unwrap();
        fs::write(dir.join("loose"), &data).unwrap();
        // One copy outside the skipped places has nothing to pair with.
        assert!(search(&dir).groups.is_empty());
        fs::create_dir_all(dir.join("invoices/vendor")).unwrap();
        fs::write(dir.join("invoices/vendor/blob"), &data).unwrap();
        assert_eq!(search(&dir).groups.len(), 1, "a plain vendor folder");
        assert!(skips_dir(Path::new("/x/Library.photoslibrary")));
        assert!(skips_dir(Path::new("/x/Proj.FCPBUNDLE")));
        // Skipped because the built-in clean-up hints name it — macOS's
        // list; Linux ships none yet, so there the name alone decides
        // nothing (its build trees still carry a CACHEDIR.TAG).
        #[cfg(target_os = "macos")]
        assert!(skips_dir(Path::new("/x/target")), "a clean-up hint by name");
        assert!(!skips_dir(Path::new("/x/Movies")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_home_scope_keeps_its_floor() {
        assert_eq!(DupeScope::Home.min_bytes(), HOME_MIN_BYTES);
        assert_eq!(DupeScope::Folder(PathBuf::from("/x")).min_bytes(), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_apfs_clone_shares_storage_and_frees_nothing() {
        let dir = scratch("clones");
        fs::write(dir.join("orig.mov"), bytes(7, 500_000)).unwrap();
        let cloned = process::Command::new("cp")
            .arg("-c")
            .arg(dir.join("orig.mov"))
            .arg(dir.join("clone.mov"))
            .status()
            .is_ok_and(|s| s.success());
        if !cloned || apfs::clone_key(&dir.join("orig.mov")).is_none() {
            // Not APFS (or no clonefile): nothing to assert here.
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        let result = search(&dir);
        assert_eq!(result.groups.len(), 1);
        let group = &result.groups[0];
        assert!(group.files.iter().all(|f| f.shares_storage));
        assert_eq!(group.reclaimable, 0, "a clone pair frees nothing");
        // A real copy beside them is the one thing that would.
        fs::write(dir.join("copy.mov"), bytes(7, 500_000)).unwrap();
        let result = search(&dir);
        assert!(result.groups[0].reclaimable > 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_a_copy_resettles_and_a_lone_file_leaves() {
        let dir = scratch("remove");
        let data = bytes(2, 150_000);
        for name in ["x", "y", "z"] {
            fs::write(dir.join(name), &data).unwrap();
        }
        let mut result = search(&dir);
        assert_eq!(result.groups[0].files.len(), 3);
        let before = result.reclaimable();
        result.remove(&dir.join("x"));
        assert_eq!(result.groups[0].files.len(), 2);
        assert!(result.reclaimable() < before);
        result.remove(&dir.join("y"));
        assert!(result.groups.is_empty(), "one copy is not a duplicate");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_oldest_copy_is_named_only_when_the_dates_tell_them_apart() {
        let dir = scratch("oldest");
        let data = bytes(6, 90_000);
        for name in ["a", "b", "c"] {
            fs::write(dir.join(name), &data).unwrap();
        }
        let set = |name: &str, secs_ago: u64| {
            File::options()
                .write(true)
                .open(dir.join(name))
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(secs_ago))
                .unwrap();
        };
        set("a", 300);
        set("b", 9_000);
        set("c", 600);
        let result = search(&dir);
        assert_eq!(result.groups[0].oldest(), Some(dir.join("b").as_path()));
        // Two copies sharing the earliest time: the dates decide nothing.
        let shared = SystemTime::now() - Duration::from_secs(20_000);
        for name in ["a", "b"] {
            File::options()
                .write(true)
                .open(dir.join(name))
                .unwrap()
                .set_modified(shared)
                .unwrap();
        }
        let result = search(&dir);
        assert_eq!(result.groups[0].oldest(), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_copy_is_spare_only_while_it_and_another_are_unchanged() {
        let dir = scratch("spare");
        let data = bytes(4, 120_000);
        for name in ["p", "q"] {
            fs::write(dir.join(name), &data).unwrap();
        }
        let result = search(&dir);
        let group = &result.groups[0];
        assert!(group.spare(&dir.join("p")));
        assert!(!group.spare(&dir.join("elsewhere")), "not in the group");
        // The other copy was edited since the search: p is the last
        // copy of what was found, and stays.
        let earlier = SystemTime::now() - Duration::from_secs(3600);
        File::options()
            .write(true)
            .open(dir.join("q"))
            .unwrap()
            .set_modified(earlier)
            .unwrap();
        assert!(!group.spare(&dir.join("p")));
        // And q itself is no longer the file that was compared.
        assert!(!group.spare(&dir.join("q")));
        let _ = fs::remove_dir_all(&dir);
    }
}
