//! The disk-space window's duplicate search (`crate::dupes`) as store
//! state.
//!
//! Same lifecycle as the directory analyser, for the same reason: a
//! search reads every same-length file, which over a home folder of
//! videos and disk images is a minute or more, so it survives the
//! window's close and only the explicit cancel stops it. Unlike the
//! analyser nothing is cached to disk: the result is a list of paths
//! that goes stale the moment anything moves, and a search of a picked
//! folder — the common case once someone is tidying — is seconds.

use super::ZStatsAppState;
use crate::bigfiles;
use crate::dupes::{self, DupeEvent, DupeResult, DupeScope};
use gpui::Context;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Groups listed before "show more". The ranking puts what frees the
/// most first, so the twentieth group is already the long tail.
pub const DUPES_SHOWN: usize = 20;

#[derive(Clone, Copy, Debug)]
pub enum DupeProgress {
    Walking { files: usize },
    Hashing { read: u64, total: u64 },
}

#[derive(Default)]
pub enum DupeSearch {
    #[default]
    Off,
    Running {
        run_id: u64,
        scope: DupeScope,
        progress: DupeProgress,
        cancel: Arc<AtomicBool>,
    },
    Ready(DupeResult),
    Failed(String),
}

pub(crate) struct Dupes {
    search: DupeSearch,
    /// What the next search covers. The home folder until a folder is
    /// picked, and back to it from the picked folder's ✕.
    scope: DupeScope,
    runs: u64,
    shown: usize,
    /// A move was refused because the copy, or every other one, changed
    /// since the search (`DupeGroup::spare`). The card says to search
    /// again rather than leaving a button that does nothing.
    stale: bool,
}

impl Default for Dupes {
    fn default() -> Self {
        Self {
            search: DupeSearch::Off,
            scope: DupeScope::Home,
            runs: 0,
            shown: DUPES_SHOWN,
            stale: false,
        }
    }
}

impl ZStatsAppState {
    pub fn dupe_search(&self) -> &DupeSearch {
        &self.dupes.search
    }

    pub fn dupe_scope(&self) -> &DupeScope {
        &self.dupes.scope
    }

    pub fn dupes_shown(&self) -> usize {
        self.dupes.shown
    }

    pub fn dupes_stale(&self) -> bool {
        self.dupes.stale
    }

    pub(super) fn dupes_running(&self) -> bool {
        matches!(self.dupes.search, DupeSearch::Running { .. })
    }

    pub fn show_more_dupes(&mut self, cx: &mut Context<Self>) {
        self.dupes.shown += DUPES_SHOWN;
        cx.notify();
    }

    /// Search the current scope (the card's Find button).
    pub fn start_dupes(&mut self, cx: &mut Context<Self>) {
        let scope = self.dupes.scope.clone();
        self.launch_dupes(scope, cx);
    }

    /// A folder was picked: search it at once. Picking *is* the ask —
    /// there is no other scope to confirm, and a picked folder is
    /// usually seconds.
    pub fn search_dupes_in(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.dupes.scope = DupeScope::Folder(folder);
        let scope = self.dupes.scope.clone();
        self.launch_dupes(scope, cx);
    }

    /// Back to the home folder. A picked folder's result goes with it:
    /// under the home chip it would read as the home folder's answer.
    /// The home search is not started — it is the slow one, and Find is
    /// right there.
    pub fn reset_dupe_scope(&mut self, cx: &mut Context<Self>) {
        self.dupes.scope = DupeScope::Home;
        self.clear_dupes(cx);
    }

    /// Cancel a running search, or put a finished one away. Nothing on
    /// disk moves.
    pub fn clear_dupes(&mut self, cx: &mut Context<Self>) {
        if let DupeSearch::Running { cancel, .. } = &self.dupes.search {
            cancel.store(true, Ordering::Relaxed);
        }
        self.dupes.search = DupeSearch::Off;
        self.dupes.stale = false;
        self.dupes.shown = DUPES_SHOWN;
        cx.notify();
    }

    /// A fresh disk-space window folds the list back; the result itself
    /// stays, the way the analyser's does.
    pub(super) fn reset_dupe_views(&mut self) {
        self.dupes.shown = DUPES_SHOWN;
        self.dupes.stale = false;
    }

    fn launch_dupes(&mut self, scope: DupeScope, cx: &mut Context<Self>) {
        if let DupeSearch::Running { cancel, .. } = &self.dupes.search {
            cancel.store(true, Ordering::Relaxed);
        }
        // Same courtesy the analyser's walk gets: the user's search wins
        // over the daily check, which can run tomorrow.
        self.stop_disk_check();
        self.dupes.runs += 1;
        let run_id = self.dupes.runs;
        let cancel = Arc::new(AtomicBool::new(false));
        self.dupes.search = DupeSearch::Running {
            run_id,
            scope: scope.clone(),
            progress: DupeProgress::Walking { files: 0 },
            cancel: cancel.clone(),
        };
        self.dupes.stale = false;
        self.dupes.shown = DUPES_SHOWN;
        cx.notify();

        let (tx, rx) = smol::channel::unbounded::<DupeEvent>();
        dupes::spawn(scope, cancel, tx);
        cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                let done = matches!(event, DupeEvent::Done(_) | DupeEvent::Failed(_));
                let _ = this.update(cx, |state, cx| {
                    // Only the run that owns the Running state may write;
                    // a cancelled or superseded run stays silent.
                    let DupeSearch::Running {
                        run_id: id,
                        progress,
                        ..
                    } = &mut state.dupes.search
                    else {
                        return;
                    };
                    if *id != run_id {
                        return;
                    }
                    match event {
                        DupeEvent::Walking { files } => {
                            *progress = DupeProgress::Walking { files };
                        }
                        DupeEvent::Hashing { read, total } => {
                            *progress = DupeProgress::Hashing { read, total };
                        }
                        DupeEvent::Done(result) => {
                            tracing::info!(
                                groups = result.groups.len(),
                                reclaimable = result.reclaimable(),
                                files = result.files_seen,
                                took_ms = result.took.as_millis() as u64,
                                "duplicate search finished"
                            );
                            state.dupes.search = DupeSearch::Ready(*result);
                        }
                        DupeEvent::Failed(e) => {
                            tracing::warn!(error = %e, "duplicate search failed");
                            state.dupes.search = DupeSearch::Failed(e);
                        }
                    }
                    cx.notify();
                });
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    /// One copy's confirmed move to the Trash. Refused — the row stays,
    /// the card asks for a new search — unless this copy and another are
    /// still what the search compared (`DupeGroup::spare`): that is what
    /// keeps "never the last copy" true after the result was taken.
    pub fn trash_dupe(&mut self, path: &Path, cx: &mut Context<Self>) {
        let DupeSearch::Ready(result) = &mut self.dupes.search else {
            return;
        };
        let Some(group) = result.group_of(path) else {
            return;
        };
        if !group.spare(path) {
            tracing::warn!(
                path = %path.display(),
                "duplicate not moved: it or every other copy changed since the search"
            );
            self.dupes.stale = true;
            cx.notify();
            return;
        }
        // What the move gives back once the Trash is emptied: nothing for
        // a clone whose blocks another copy still holds, or a name whose
        // file has other names.
        let frees = group
            .files
            .iter()
            .find(|f| f.path == path)
            .filter(|f| f.frees())
            .map_or(0, |f| f.bytes);
        if let Err(e) = bigfiles::trash(path) {
            tracing::warn!("trash {}: {e}", path.display());
            return;
        }
        result.remove(path);
        self.note_trashed(frees);
        cx.notify();
    }
}
