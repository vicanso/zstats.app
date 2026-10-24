//! The disk-space window's rules: what each tab's header says, which scope
//! a chip stands for, what a row's pills and tooltip say, what its trash
//! confirm says, how much of a list shows before "show more". Moved out of
//! the gpui window for the NSH one (phase 4k of docs/nsh-migration.md); the
//! gpui window, `views/storage.rs`, says why each part of it is the way it
//! is.

use crate::cleanhints::{self, CleanHint};
use crate::diskscan::{self, DiffBaseline, DirHit, HitKind, ScanResult, ScanScope};
use crate::dupes::{DupeGroup, DupeScope};
use crate::state::{DiskAnalysis, DupeProgress, DupeSearch, Effects, ZStatsAppState};
use crate::{diskwatch, format, i18n, prefs};
use rust_i18n::t;
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The window's type scale: three sizes (`views/storage.rs` says why).
/// Meta is captions, counts and units; rows are what is ranked; headings
/// are the few lines that say what a block is.
pub const META_PT: f32 = 10.;
pub const ROW_PT: f32 = 12.;
pub const HEAD_PT: f32 = 13.;

/// Width of the lists beside the directory map: the suggestions' rows —
/// path, pills, size, two buttons — at the width they had in the 507pt
/// window.
pub const SIDE_W: f32 = 440.;

/// Window width from which the map sits beside the lists. Under it the
/// map would be narrower than the lists beside it, so it goes above them.
pub const SIDE_BY_SIDE_MIN_W: f32 = 1000.;

/// Results older than this get a "consider re-analyzing" nudge on the
/// run chip — display only: a minutes-long walk never starts itself.
pub const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Suggestions listed before "show more".
pub const SUG_ROWS: usize = 5;

/// How many children an opened row lists before it says how many it is
/// holding back.
pub const EXPAND_ROWS: usize = 8;

/// Under a megabyte a child reads `0 MB`; those go to the summary line.
pub const EXPAND_FLOOR: u64 = 1024 * 1024;

/// Below this a row shows no delta: 2% of `TABLE_EXTEND_MIN`, the
/// smallest figure the extended table admits.
pub const DIFF_FLOOR: u64 = 10 * 1024 * 1024;

/// Copies listed under one duplicate group before the rest become a
/// count.
pub const DUPE_FILES_SHOWN: usize = 10;

/// The scope a person would call it: "Home", "Caches", "Whole disk", or
/// the folder's `~` path.
pub fn scope_word(roots: &[PathBuf], base: &Path) -> String {
    let home = diskscan::default_root();
    if home.as_ref().is_some_and(|home| roots == [home.clone()]) {
        return i18n::tr("disk.ana_preset_home");
    }
    if ScanScope::cache_set().is_some_and(|caches| caches.roots == roots) {
        return i18n::tr("disk.ana_preset_caches");
    }
    if ScanScope::whole_disk().roots == roots {
        return i18n::tr("disk.ana_preset_disk");
    }
    let home_s = env::var("HOME").unwrap_or_default();
    scope_display(roots, base, &home_s)
}

/// One string naming a scope: the single root, or a multi-root scope's
/// roots listed in full, every path tilde'd.
pub fn scope_display(roots: &[PathBuf], base: &Path, home: &str) -> String {
    if roots.len() > 1 {
        roots
            .iter()
            .map(|r| format::tilde_path(&r.display().to_string(), home))
            .collect::<Vec<_>>()
            .join(" + ")
    } else {
        format::tilde_path(&base.display().to_string(), home)
    }
}

/// The scope the next walk covers: the picked one, or home.
pub fn selected_scope(state: &ZStatsAppState) -> Option<ScanScope> {
    state
        .disk_analysis_scope()
        .or_else(|| diskscan::default_root().map(ScanScope::single))
}

/// `(shown, age, selected)` when the result on screen answers a scope
/// other than the selected one.
pub fn mismatch(state: &ZStatsAppState) -> Option<(String, String, String)> {
    let DiskAnalysis::Ready(result) = state.disk_analysis() else {
        return None;
    };
    let selected = selected_scope(state)?;
    (selected.roots != result.roots).then(|| {
        (
            scope_word(&result.roots, &result.root),
            format::ago(result.scanned_at.elapsed().unwrap_or_default()),
            scope_word(&selected.roots, &selected.base),
        )
    })
}

/// The line that says the result on screen is not the selected scope's.
pub fn mismatch_note(state: &ZStatsAppState) -> Option<String> {
    mismatch(state).map(|(shown, ago, selected)| {
        t!(
            "disk.ana_showing",
            shown = shown,
            ago = ago,
            selected = selected
        )
        .to_string()
    })
}

/// The selected scope's word, for the run button before the first walk.
pub fn selected_word(state: &ZStatsAppState) -> String {
    selected_scope(state)
        .map(|s| scope_word(&s.roots, &s.base))
        .unwrap_or_else(|| i18n::tr("disk.ana_preset_home"))
}

/// The empty state's button: a walk of the selected scope.
pub fn start_label(state: &ZStatsAppState) -> String {
    t!("disk.ana_scan_scope", scope = selected_word(state)).to_string()
}

/// A finished result older than [`STALE_AFTER`].
pub fn stale(state: &ZStatsAppState) -> bool {
    match state.disk_analysis() {
        DiskAnalysis::Ready(result) => {
            result.scanned_at.elapsed().unwrap_or_default() > STALE_AFTER
        }
        _ => false,
    }
}

/// The run chip's words: cancel while walking, the selected scope when
/// that is not the result on screen, "re-analyze" over a finished one.
pub fn run_label(state: &ZStatsAppState) -> String {
    let running = matches!(state.disk_analysis(), DiskAnalysis::Running { .. });
    if running {
        i18n::tr("disk.ana_cancel")
    } else if let Some((_, _, selected)) = mismatch(state) {
        t!("disk.ana_scan_scope", scope = selected).to_string()
    } else if matches!(state.disk_analysis(), DiskAnalysis::Ready(_)) {
        i18n::tr("disk.ana_rescan")
    } else {
        let word = selected_scope(state)
            .map(|s| scope_word(&s.roots, &s.base))
            .unwrap_or_default();
        t!("disk.ana_scan_scope", scope = word).to_string()
    }
}

/// The run chip's tooltip: the method, or that the result is old.
pub fn run_tip(stale: bool) -> String {
    i18n::tr(if stale {
        "disk.ana_stale"
    } else {
        "disk.ana_hint"
    })
}

/// Starts the selected walk, or cancels the one running: the run chip.
pub fn run_or_cancel(state: &mut ZStatsAppState, fx: &mut dyn Effects) {
    if matches!(state.disk_analysis(), DiskAnalysis::Running { .. }) {
        state.cancel_disk_analysis(fx);
    } else {
        state.start_disk_analysis(fx);
    }
}

/// How far a running walk is against the last walk of its scope, when
/// there was one.
pub fn progress(state: &ZStatsAppState) -> Option<f32> {
    match state.disk_analysis() {
        DiskAnalysis::Running {
            dirs_done,
            expected_dirs: Some(expected),
            ..
        } if *expected > 0 => Some(*dirs_done as f32 / *expected as f32),
        _ => None,
    }
}

/// The header's caption as `(scope, the rest)`. `None` before the first
/// walk.
pub fn caption(state: &ZStatsAppState) -> Option<(String, String)> {
    match state.disk_analysis() {
        DiskAnalysis::Off => None,
        DiskAnalysis::Failed(e) => Some((
            i18n::tr("disk.ana_failed_title"),
            t!("disk.ana_failed", e = e.clone()).to_string(),
        )),
        DiskAnalysis::Running {
            dirs_done,
            scope,
            expected_dirs,
            ..
        } => {
            let done = format::thousands(*dirs_done);
            let rest = match expected_dirs {
                Some(last) => t!(
                    "disk.ana_running_of",
                    dirs = done,
                    last = format::thousands(*last)
                )
                .to_string(),
                None => t!("disk.ana_running", dirs = done).to_string(),
            };
            Some((scope_word(&scope.roots, &scope.base), rest))
        }
        DiskAnalysis::Ready(result) => {
            let mut parts = vec![
                format::ago(result.scanned_at.elapsed().unwrap_or_default()),
                t!("disk.ana_took", t = format::took(result.took)).to_string(),
                t!(
                    "disk.ana_dirs_seen",
                    n = format::thousands(result.dirs_seen)
                )
                .to_string(),
            ];
            // Names what the per-row ± compares against. Its absence when
            // no row moved is itself the answer: nothing big changed.
            if let Some(diff) = state.analysis_diff_for(result) {
                parts.push(
                    t!(
                        "disk.ana_diff_base",
                        ago = format::ago(diff.scanned_at().elapsed().unwrap_or_default())
                    )
                    .to_string(),
                );
            }
            Some((scope_word(&result.roots, &result.root), parts.join(" · ")))
        }
    }
}

/// Everything a finished walk left out, as one figure and its breakdown:
/// `("70 skipped", [each non-zero count])`, hard links counted once only
/// joining the breakdown — alone, they are the figure.
pub fn skips(result: &ScanResult) -> Option<(String, Vec<String>)> {
    let skipped = [
        ("disk.ana_skip_protected", result.skipped_protected),
        ("disk.ana_skip_denied", result.skipped_denied),
        ("disk.ana_skip_dataless", result.skipped_dataless),
        ("disk.ana_skip_excluded", result.skipped_excluded),
    ];
    let mut lines: Vec<String> = skipped
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(key, n)| t!(*key, n = format::thousands(*n)).to_string())
        .collect();
    let links = (result.shared_links > 0).then(|| {
        t!(
            "disk.ana_shared_links",
            n = format::thousands(result.shared_links)
        )
        .to_string()
    });
    let total: usize = skipped.iter().map(|(_, n)| *n).sum();
    if total == 0 {
        return links.map(|text| (text.clone(), vec![text]));
    }
    lines.extend(links);
    Some((
        t!("disk.ana_skipped_total", n = format::thousands(total)).to_string(),
        lines,
    ))
}

/// The Full Disk Access note, when permission gaps hid part of the tree
/// (macOS; a directory the user cannot read elsewhere has no one switch).
pub fn fda_note(state: &ZStatsAppState) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let DiskAnalysis::Ready(result) = state.disk_analysis() else {
        return None;
    };
    (result.skipped_denied > 0)
        .then(|| t!("disk.ana_fda_note", n = result.skipped_denied).to_string())
}

/// Why a check that is due has not run, in the order the gates fall
/// (`diskwatch::due`) — display only.
pub fn overdue_reason(state: &ZStatsAppState) -> &'static str {
    let tick = state.latest();
    let on_battery = diskwatch::on_battery(tick.and_then(|t| t.snapshot.battery.as_ref()));
    let busy = tick.is_some_and(|t| t.snapshot.cpu.usage_percent > diskwatch::BUSY_CPU);
    if on_battery {
        "disk.watch_wait_power"
    } else if busy {
        "disk.watch_wait_quiet"
    } else {
        "disk.watch_wait_closed"
    }
}

/// The daily check's state, for its chip's tooltip: off, running, when it
/// last ran and what it waits for.
pub fn watch_status(state: &ZStatsAppState) -> String {
    if !prefs::disk_watch() {
        i18n::tr("disk.watch_off")
    } else if state.disk_check_running() {
        i18n::tr("disk.watch_running")
    } else {
        match state.disk_check_last() {
            Some(at) => {
                let since = at.elapsed().unwrap_or_default();
                let last = t!("disk.watch_last", ago = format::ago(since)).to_string();
                if since > diskwatch::CHECK_EVERY {
                    format!("{last} · {}", i18n::tr(overdue_reason(state)))
                } else {
                    last
                }
            }
            None => i18n::tr("disk.watch_first"),
        }
    }
}

/// The growth section's title: what grew over how many days.
pub fn growth_title(report: &diskwatch::Report) -> String {
    let days = (report.over.as_secs_f64() / 86_400.0).round().max(1.0) as u64;
    t!("disk.growth_title", days = days).to_string()
}

/// Growth is measured on the home tree only (`diskwatch`); a result for
/// any other scope says nothing about it.
pub fn home_result(result: &ScanResult) -> bool {
    diskscan::default_root().is_some_and(|home| result.roots == [home])
}

/// The preset scopes the toolbar offers (docs/disk-analysis.md's scope
/// table), in the toolbar's order — the whole disk last, the slowest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Home,
    Library,
    Caches,
    Disk,
}

impl Preset {
    pub const ALL: [Preset; 4] = [Preset::Home, Preset::Library, Preset::Caches, Preset::Disk];

    /// The chip's words: a word for home, not `~`; the exact path rides
    /// the tooltip.
    pub fn label(self) -> String {
        match self {
            Preset::Home => i18n::tr("disk.ana_preset_home"),
            Preset::Library => "~/Library".into(),
            Preset::Caches => i18n::tr("disk.ana_preset_caches"),
            Preset::Disk => i18n::tr("disk.ana_preset_disk"),
        }
    }

    pub fn tip(self) -> String {
        match self {
            Preset::Home => i18n::tr("disk.ana_preset_home_tip"),
            Preset::Library => i18n::tr("disk.ana_preset_library_tip"),
            Preset::Caches => i18n::tr("disk.ana_preset_caches_tip"),
            Preset::Disk => format!(
                "{} {}",
                i18n::tr("disk.ana_preset_disk_tip"),
                i18n::tr("disk.ana_scope_tip")
            ),
        }
    }

    fn scope(self) -> Option<ScanScope> {
        let home = diskscan::default_root();
        match self {
            Preset::Home => home.map(ScanScope::single),
            Preset::Library => home.map(|h| ScanScope::single(h.join("Library"))),
            Preset::Caches => ScanScope::cache_set(),
            Preset::Disk => Some(ScanScope::whole_disk()),
        }
    }

    /// Whether this chip is the lit one: no pick yet is the default home
    /// walk.
    pub fn on(self, state: &ZStatsAppState) -> bool {
        match state.disk_analysis_scope() {
            None => self == Preset::Home,
            Some(selected) => self.scope().is_some_and(|s| s.roots == selected.roots),
        }
    }

    /// Selects this scope; the run chip walks it.
    pub fn select(self, state: &mut ZStatsAppState, fx: &mut dyn Effects) {
        match self {
            Preset::Home => {
                if let Some(home) = diskscan::default_root() {
                    state.set_disk_analysis_at(home, fx);
                }
            }
            Preset::Library => {
                if let Some(home) = diskscan::default_root() {
                    state.set_disk_analysis_at(home.join("Library"), fx);
                }
            }
            Preset::Caches => state.set_disk_analysis_caches(fx),
            Preset::Disk => state.set_disk_analysis_whole_disk(fx),
        }
    }
}

/// A picked folder that is not a preset: its chip's words. Without a
/// chip the selection would only live in the next walk.
pub fn custom_scope(state: &ZStatsAppState) -> Option<String> {
    let scope = state.disk_analysis_scope()?;
    let preset = Preset::ALL
        .into_iter()
        .filter_map(Preset::scope)
        .any(|p| p.roots == scope.roots);
    (!preset).then(|| {
        let home_s = env::var("HOME").unwrap_or_default();
        scope_display(&scope.roots, &scope.base, &home_s)
    })
}

/// The exclusions chip's words: how many there are, because a walk that
/// leaves folders out must not let that be forgotten.
pub fn exclude_label() -> String {
    match prefs::analysis_exclude_raw().len() {
        0 => i18n::tr("disk.ana_exclude_add"),
        n => t!("disk.ana_excluded_n", n = n).to_string(),
    }
}

/// Each exclusion as written, and whether it resolves to a directory
/// today — one that does not is dimmed, never dropped.
pub fn exclude_entries() -> Vec<(String, bool)> {
    let home = env::var("HOME").unwrap_or_default();
    prefs::analysis_exclude_raw()
        .into_iter()
        .map(|raw| {
            let expanded = match raw.strip_prefix("~/") {
                Some(rest) if !home.is_empty() => Path::new(&home).join(rest),
                _ => PathBuf::from(&raw),
            };
            let present = expanded.is_dir();
            (raw, present)
        })
        .collect()
}

/// Leaves `raw` out of the exclusions.
pub fn drop_exclude(raw: &str) {
    let kept: Vec<String> = prefs::analysis_exclude_raw()
        .into_iter()
        .filter(|p| p != raw)
        .collect();
    prefs::set_analysis_exclude(&kept);
}

/// Adds what was typed or picked; blank input adds nothing. `true` when
/// something was added.
pub fn add_exclude(paths: impl IntoIterator<Item = String>) -> bool {
    let mut list = prefs::analysis_exclude_raw();
    let before = list.len();
    list.extend(paths.into_iter().filter(|p| !p.trim().is_empty()));
    if list.len() == before {
        return false;
    }
    prefs::set_analysis_exclude(&list);
    true
}

/// "Reveal in Finder" on macOS; the file manager has no one name on
/// Linux, and the sentence should not promise Finder.
pub fn reveal_tip() -> String {
    i18n::tr(if cfg!(target_os = "macos") {
        "disk.big_reveal"
    } else {
        "disk.big_reveal_linux"
    })
}

/// `+1.2 GB` / `-340.0 MB` against the previous run, or `None` when
/// there is nothing honest to say (no baseline row, or under the floor).
pub fn delta_label(bytes: u64, prev_bytes: Option<u64>) -> Option<String> {
    let prev = prev_bytes?;
    let (sign, diff) = if bytes >= prev {
        ("+", bytes - prev)
    } else {
        ("-", prev - bytes)
    };
    (diff >= DIFF_FLOOR).then(|| format!("{sign}{}", format::memory(diff)))
}

/// One tooltip for every ± in the tables: which run the figure is
/// measured against.
pub fn delta_tip(diff: &DiffBaseline) -> String {
    t!(
        "disk.delta_tip",
        ago = format::ago(diff.scanned_at().elapsed().unwrap_or_default())
    )
    .to_string()
}

/// The system's own words about a MobileAsset row, as one sentence: every
/// clause a field the asset's `Info.plist` carried, then the closing line
/// that this content is `mobileassetd`'s.
pub fn asset_clause(note: &diskscan::AssetNote) -> String {
    let mut out = t!("disk.asset_kind", kind = &note.kind).to_string();
    if let Some(locale) = &note.locale {
        out.push_str(&format!(" · {}", t!("disk.asset_locale", lang = locale)));
    }
    if note.required_by_os == Some(true) {
        out.push_str(&format!(" · {}", i18n::tr("disk.asset_required")));
    }
    if note.never_collected == Some(true) {
        out.push_str(&format!(" · {}", i18n::tr("disk.asset_never_collected")));
    }
    out.push_str(&format!(" — {}", i18n::tr("disk.asset_reclaim")));
    out
}

/// What a row's cleanup standing rests on, as `(word, tooltip)`:
/// "rebuilds" for a TAG tree or a documented cache, "manual" for a known
/// owner whose content it does not simply rebuild. Plain rows: `None`.
pub fn basis(kind: Option<HitKind>, hint: Option<&CleanHint>) -> Option<(String, String)> {
    if kind == Some(HitKind::Tag) {
        return Some((i18n::tr("disk.kind_tag"), i18n::tr("disk.kind_tag_tip")));
    }
    let hint = hint?;
    let base = if hint.trashable {
        t!("disk.basis_rebuild_tip", owner = &hint.owner).to_string()
    } else {
        t!("disk.basis_manual_tip", owner = &hint.owner).to_string()
    };
    let tip = match &hint.command {
        Some(cmd) => format!(
            "{base} {}",
            t!("disk.basis_cmd", owner = &hint.owner, cmd = cmd)
        ),
        None => base,
    };
    let label = if hint.trashable {
        i18n::tr("disk.kind_tag")
    } else {
        i18n::tr("disk.basis_manual")
    };
    Some((label, tip))
}

/// The "running" pill's tooltip: moving a live app's cache frees nothing
/// until it restarts.
pub fn running_tip(owner: &str) -> String {
    t!("disk.running_tip", owner = owner).to_string()
}

/// A row's label: its path made relative to the row above it (the scan
/// root at the top level).
pub fn row_label(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// A row's name tooltip: where it is, and what it is known as — a
/// heuristic fold's note, a TAG tree's declaration in the suggestions
/// (whose rows wear no basis pill), the cleanup hint's owner and command,
/// and what the system declares about a MobileAsset.
pub fn row_tip(
    path: &Path,
    kind: Option<HitKind>,
    suggested: bool,
    hint: Option<&CleanHint>,
    asset: Option<&diskscan::AssetNote>,
) -> String {
    let mut full = format::tilde_path(
        &path.display().to_string(),
        &env::var("HOME").unwrap_or_default(),
    );
    if kind == Some(HitKind::Heuristic) {
        full.push_str(" — ");
        full.push_str(&i18n::tr("disk.kind_guess_tip"));
    }
    if suggested && kind == Some(HitKind::Tag) {
        full.push_str(" — ");
        full.push_str(&i18n::tr("disk.kind_tag_tip"));
    }
    if let Some(hint) = hint {
        full.push_str(" — ");
        full.push_str(&match &hint.command {
            Some(cmd) => t!("disk.hint_cmd", owner = &hint.owner, cmd = cmd).to_string(),
            None => t!("disk.hint_owner", owner = &hint.owner).to_string(),
        });
    }
    if let Some(note) = asset {
        full.push_str(" — ");
        full.push_str(&asset_clause(note));
    }
    full
}

/// The Trash itself: its row's reveal opens the Trash, never empties it.
pub fn is_trash_dir(path: &Path) -> bool {
    env::var_os("HOME").is_some_and(|home| path == Path::new(&home).join(".Trash"))
}

/// The reveal button's tooltip on `path`'s row.
pub fn reveal_row_tip(path: &Path) -> String {
    if is_trash_dir(path) {
        i18n::tr("disk.open_trash")
    } else {
        reveal_tip()
    }
}

/// A directory row's trash confirm: what moves and how big, and when its
/// app is running, that the move frees nothing until that app lets go.
pub fn dir_trash_body(label: &str, bytes: u64, owner_running: Option<&str>) -> String {
    let caution = owner_running
        .map(|owner| t!("disk.trash_running", owner = owner).to_string())
        .unwrap_or_default();
    format!(
        "{}{}",
        t!(
            "disk.ana_trash_body",
            name = label,
            bytes = format::memory(bytes)
        ),
        caution
    )
}

/// A big file's trash confirm: the reader's own file, which does not come
/// back by itself as a cache would.
pub fn file_trash_body(label: &str) -> String {
    t!("disk.big_trash_body", name = label).to_string()
}

/// How many children an open row lists: the ranked ones, down to where
/// they stop being distinguishable, and never more than [`EXPAND_ROWS`].
pub fn expand_shown(rows: &[DirHit]) -> usize {
    rows.iter()
        .take(EXPAND_ROWS)
        .take_while(|r| r.bytes >= EXPAND_FLOOR)
        .count()
}

/// What an open row is not listing: how many children it held back and
/// how much they add up to, or `None` when everything is shown.
pub fn expand_hidden(rows: &[DirHit]) -> Option<(usize, u64)> {
    let shown = expand_shown(rows);
    let hidden = rows.len().checked_sub(shown).filter(|n| *n > 0)?;
    Some((hidden, rows.iter().skip(shown).map(|r| r.bytes).sum()))
}

/// The line under an open row's children for the ones it holds back.
pub fn expand_rest(hidden: usize, bytes: u64) -> String {
    t!(
        "disk.ana_rest",
        count = hidden,
        bytes = format::memory(bytes)
    )
    .to_string()
}

/// The cleanup suggestions as the card lists them: caches too small to
/// name a size left out, their total, and how many show before "show
/// more".
pub struct Suggestions {
    pub rows: Vec<DirHit>,
    pub total: u64,
    pub shown: usize,
}

pub fn suggestions(result: &ScanResult, show_all: bool) -> Suggestions {
    const MIB: u64 = 1024 * 1024;
    let rows: Vec<DirHit> = result
        .suggestions
        .iter()
        .filter(|d| d.bytes >= MIB)
        .cloned()
        .collect();
    let total = rows.iter().map(|d| d.bytes).sum();
    let shown = if show_all {
        rows.len()
    } else {
        rows.len().min(SUG_ROWS)
    };
    Suggestions { rows, total, shown }
}

/// The "show more" pill under the suggestions.
pub fn more_label(open: bool, hidden: usize) -> String {
    if open {
        i18n::tr("disk.ana_less")
    } else {
        t!("disk.ana_more", count = hidden).to_string()
    }
}

/// The bulk trash sheet's rows: each suggestion's `~` path, its size, and
/// the caution that starts a running app's cache unticked.
pub fn pick_items(hits: &[DirHit], running: &[String]) -> Vec<(String, u64, Option<String>)> {
    let home = env::var("HOME").unwrap_or_default();
    hits.iter()
        .map(|hit| {
            let caution = cleanhints::lookup(&hit.path)
                .filter(|hint| hint.running_in(running))
                .map(|hint| t!("disk.pick_running", owner = &hint.owner).to_string());
            (
                format::tilde_path(&hit.path.display().to_string(), &home),
                hit.bytes,
                caution,
            )
        })
        .collect()
}

/// The bulk sheet's action, as many as are ticked and what they add up
/// to.
pub fn pick_ok(n: usize, bytes: u64) -> String {
    t!("disk.sug_pick_ok", n = n, bytes = format::memory(bytes)).to_string()
}

/// The line above every tab once anything went to the Trash this
/// session.
pub fn trashed_note(bytes: u64) -> Option<String> {
    (bytes > 0).then(|| t!("disk.trashed_note", bytes = format::memory(bytes)).to_string())
}

/// The duplicate search's header, `(scope, the rest)`: progress while
/// running; afterwards the age, what was compared and every honesty
/// counter that is not zero.
pub fn dupes_caption(state: &ZStatsAppState) -> (String, String) {
    let word = |scope: &DupeScope| match scope {
        DupeScope::Home => i18n::tr("disk.ana_preset_home"),
        DupeScope::Folder(path) => format::tilde(path),
    };
    match state.dupe_search() {
        DupeSearch::Off => (word(state.dupe_scope()), String::new()),
        DupeSearch::Failed(e) => (
            i18n::tr("disk.dup_failed_title"),
            t!("disk.dup_failed", e = e.clone()).to_string(),
        ),
        DupeSearch::Running {
            scope, progress, ..
        } => {
            let what = match progress {
                DupeProgress::Walking { files } => {
                    t!("disk.dup_walking", n = format::thousands(*files)).to_string()
                }
                // "At most": the total assumes every same-size file is read
                // whole, and the 64 KB pass rules most of them out first.
                DupeProgress::Hashing { read, total } => t!(
                    "disk.dup_hashing",
                    read = format::memory(*read),
                    total = format::memory(*total)
                )
                .to_string(),
            };
            (word(scope), what)
        }
        DupeSearch::Ready(result) => {
            let mut parts: Vec<String> = vec![
                format::ago(result.scanned_at.elapsed().unwrap_or_default()),
                t!("disk.ana_took", t = format::took(result.took)).to_string(),
            ];
            let n = format::thousands(result.files_seen);
            parts.push(match result.scope {
                DupeScope::Home => t!("disk.dup_files_home", n = n).to_string(),
                DupeScope::Folder(_) => t!("disk.dup_files", n = n).to_string(),
            });
            if result.unreadable > 0 {
                parts.push(t!("disk.dup_unreadable", n = result.unreadable).to_string());
            }
            (word(&result.scope), parts.join(" · "))
        }
    }
}

/// The duplicate search's run chip: cancel while it runs.
pub fn dupes_run_label(state: &ZStatsAppState) -> String {
    match state.dupe_search() {
        DupeSearch::Running { .. } => i18n::tr("disk.ana_cancel"),
        DupeSearch::Ready(_) => i18n::tr("disk.dup_again"),
        DupeSearch::Off | DupeSearch::Failed(_) => i18n::tr("disk.dup_find"),
    }
}

/// Starts a search, or cancels the one running: the run chip.
pub fn dupes_run_or_cancel(state: &mut ZStatsAppState, fx: &mut dyn Effects) {
    if matches!(state.dupe_search(), DupeSearch::Running { .. }) {
        state.clear_dupes(fx);
    } else {
        state.start_dupes(fx);
    }
}

/// The groups' heading and the dim clause beside it.
pub fn dupes_heading(groups: usize, extra_copies: usize) -> (String, String) {
    (
        match groups {
            1 => i18n::tr("disk.dup_groups_one"),
            n => t!("disk.dup_groups", n = n).to_string(),
        },
        match extra_copies {
            1 => i18n::tr("disk.dup_copies_one"),
            n => t!("disk.dup_copies", n = n).to_string(),
        },
    )
}

/// A group's name: the shortest among the copies — "Installer.dmg"
/// rather than "Installer (1).dmg".
pub fn group_name(group: &DupeGroup) -> String {
    group
        .files
        .iter()
        .filter_map(|f| f.path.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .min_by_key(|n| n.chars().count())
        .unwrap_or_default()
}

/// How many copies and how big, beside a group's name.
pub fn group_meta(group: &DupeGroup) -> String {
    t!(
        "disk.dup_group_meta",
        n = group.files.len(),
        size = format::memory(group.len)
    )
    .to_string()
}

/// What moving all but one copy frees, on hover.
pub fn frees_tip(group: &DupeGroup) -> String {
    if group.reclaimable == 0 {
        i18n::tr("disk.dup_frees_none_tip")
    } else {
        t!(
            "disk.dup_frees_tip",
            bytes = format::memory(group.reclaimable)
        )
        .to_string()
    }
}

/// A copy's trash confirm: where it is, how many others stay, and when
/// moving it frees nothing (a clone, another hard-link name).
pub fn dupe_trash_body(shown: &str, others: usize, frees: bool) -> String {
    let mut body = t!("disk.dup_trash_body", path = shown, n = others).to_string();
    if !frees {
        body.push(' ');
        body.push_str(&i18n::tr("disk.dup_trash_nothing"));
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::slice;
    use std::time::UNIX_EPOCH;

    /// An opened row may only stay silent about its children when there
    /// are none left to mention — the summary line is what keeps the cut
    /// at [`EXPAND_ROWS`] visible.
    #[test]
    fn an_opened_row_accounts_for_the_children_it_does_not_list() {
        let hit = |bytes| DirHit {
            path: PathBuf::from(format!("/r/{bytes}")),
            bytes,
            kind: HitKind::Plain,
            asset: None,
        };
        let big = |n: u64| hit(n * EXPAND_FLOOR);
        let rows: Vec<DirHit> = (1..=EXPAND_ROWS as u64 + 3).rev().map(big).collect();
        let (hidden, bytes) = expand_hidden(&rows).expect("three past the cut");
        assert_eq!(hidden, 3);
        // The three past the cut, and only those.
        assert_eq!(bytes, (1 + 2 + 3) * EXPAND_FLOOR);
        // Exactly the cut, and under it, say nothing.
        assert_eq!(expand_hidden(&rows[..EXPAND_ROWS]), None);
        assert_eq!(expand_hidden(&[]), None);

        // A child too small to render as anything but "0 MB" is summed
        // into the line below instead of spending a row on a zero.
        let mixed = vec![big(4), big(2), hit(900_000), hit(400_000)];
        assert_eq!(expand_shown(&mixed), 2, "the two that can be read");
        let (hidden, bytes) = expand_hidden(&mixed).expect("two below the floor");
        assert_eq!(hidden, 2);
        assert_eq!(bytes, 1_300_000);
    }

    /// Every clause is a field the plist actually carried; the closing
    /// line is always there, because "do not delete this by hand" is
    /// the part a reader most needs and the one thing not read off
    /// disk.
    #[test]
    fn the_asset_clause_quotes_only_what_was_declared() {
        let bare = diskscan::AssetNote {
            kind: "Font8".into(),
            required_by_os: None,
            never_collected: None,
            locale: None,
        };
        let text = asset_clause(&bare);
        assert!(text.contains("Font8"));
        assert!(!text.contains("language"), "{text}");
        let full = diskscan::AssetNote {
            kind: "VoiceServices".into(),
            required_by_os: Some(true),
            never_collected: Some(true),
            locale: Some("zh_Hans".into()),
        };
        let text = asset_clause(&full);
        assert!(text.contains("zh_Hans"), "{text}");
        // A declared `false` is not a claim either way — only `true`
        // earns a clause.
        let denied = diskscan::AssetNote {
            kind: "X".into(),
            required_by_os: Some(false),
            never_collected: Some(false),
            locale: None,
        };
        let text = asset_clause(&denied);
        assert!(!text.contains("required"), "{text}");
    }

    #[test]
    fn a_scope_is_named_the_way_its_chip_is() {
        let home = diskscan::default_root().expect("HOME is set under cargo test");
        assert_eq!(scope_word(slice::from_ref(&home), &home), "Home");
        assert_eq!(
            scope_word(&ScanScope::whole_disk().roots, &diskscan::whole_disk_root()),
            "Whole disk"
        );
        let library = home.join("Library");
        assert_eq!(scope_word(slice::from_ref(&library), &library), "~/Library");
    }

    #[test]
    fn delta_speaks_only_when_it_clears_the_floor() {
        let gib = 1024 * 1024 * 1024;
        // Growth and shrinkage, signed.
        assert_eq!(delta_label(3 * gib, Some(2 * gib)), Some("+1.0 GB".into()));
        assert_eq!(delta_label(2 * gib, Some(3 * gib)), Some("-1.0 GB".into()));
        // No baseline row → silence, not "new".
        assert_eq!(delta_label(3 * gib, None), None);
        // Under the floor either way → unchanged for table purposes.
        assert_eq!(delta_label(gib + DIFF_FLOOR - 1, Some(gib)), None);
        assert_eq!(delta_label(gib, Some(gib)), None);
    }

    /// The suggestions drop what reads "0 MB", total the rest, and show
    /// five until asked for all.
    #[test]
    fn suggestions_list_five_until_asked() {
        let mib = 1024 * 1024;
        let hit = |n: u64, bytes: u64| DirHit {
            path: PathBuf::from(format!("/c/{n}")),
            bytes,
            kind: HitKind::Tag,
            asset: None,
        };
        let mut sugs: Vec<DirHit> = (1..=7).map(|n| hit(n, n * mib)).collect();
        sugs.push(hit(8, mib / 2));
        let result = ScanResult {
            root: PathBuf::from("/c"),
            roots: vec![PathBuf::from("/c")],
            scanned_at: UNIX_EPOCH,
            took: Duration::from_secs(1),
            dirs_seen: 1,
            skipped_denied: 0,
            skipped_protected: 0,
            skipped_dataless: 0,
            shared_links: 0,
            skipped_excluded: 0,
            regenerable: vec![],
            dirs: vec![],
            files: vec![],
            suggestions: sugs,
            index: None,
            total: None,
        };
        let s = suggestions(&result, false);
        assert_eq!(s.rows.len(), 7, "the half-megabyte one is left out");
        assert_eq!(s.total, 28 * mib);
        assert_eq!(s.shown, SUG_ROWS);
        assert_eq!(suggestions(&result, true).shown, 7);
    }
}
