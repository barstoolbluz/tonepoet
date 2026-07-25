//! Browse-screen bookmarks: persistent list of user-curated directory shortcuts.
//!
//! Stored at `~/.config/tonepoet/bookmarks.toml`. Unlike recent files (which
//! are automatic and capped), bookmarks are explicitly managed by the user
//! and have no cap.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use super::text_input::TextInputState;

/// A single bookmark: user-friendly name and target path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkDetailEntry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkDetail {
    pub item_count: usize,
    pub entries: Vec<BookmarkDetailEntry>,
    pub omitted_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkDetailState {
    /// Accepted by the bounded detail queue but not yet started by a worker.
    Queued,
    /// A dedicated detail worker has begun the filesystem scan.
    Loading,
    Ready(BookmarkDetail),
    /// The bounded queue was full. This state is retryable whenever capacity
    /// is released by another detail completion.
    QueueUnavailable(String),
    /// No detail worker could be started after a controlled restart attempt.
    /// This is distinct from transient queue saturation and remains visible.
    WorkerUnavailable(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookmarkTargetStatus {
    Reachable,
    Missing,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookmarkDropdownChoice {
    Bookmark(usize),
    AddCurrent,
    Manage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookmarkActivationSurface {
    Dropdown,
    Manager,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBookmarkActivation {
    pub generation: u64,
    pub request_id: u64,
    pub path: PathBuf,
    pub surface: BookmarkActivationSurface,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookmarkNamingCommit {
    Changed,
    Unchanged,
    Failed,
}

/// Naming mode for the bookmarks overlay: either adding a new bookmark for
/// a captured path, or renaming an existing entry by index.
#[derive(Debug, Clone)]
pub enum BookmarkNaming {
    Add {
        input: TextInputState,
        path: PathBuf,
    },
    Rename {
        input: TextInputState,
        idx: usize,
    },
}

/// In-memory bookmarks list plus overlay UI state.
#[derive(Debug, Clone, Default)]
pub struct BookmarksState {
    pub entries: Vec<Bookmark>,
    pub overlay_open: bool,
    /// Absolute index into `entries`; filtering never changes bookmark identity.
    pub overlay_selected: usize,
    /// First row within the filtered result set.
    pub overlay_scroll: usize,
    pub overlay_visible_rows: usize,
    pub naming: Option<BookmarkNaming>,
    pub filter_input: Option<TextInputState>,
    pub feedback: Option<String>,
    /// Per-session reachability cache populated off the event thread.
    pub target_status: HashMap<PathBuf, BookmarkTargetStatus>,
    /// Paths already queued for the current generation. This suppresses
    /// duplicate probes when redraws or lifecycle messages request refreshes.
    pub target_probes_in_flight: HashSet<PathBuf>,
    /// Next bookmark index to consider when incrementally filling the bounded
    /// status queue. This avoids an O(n) rescan after every completion.
    pub target_probe_next_index: usize,
    /// Per-manager-open, non-recursive detail cache.
    pub detail_cache: HashMap<PathBuf, BookmarkDetailState>,
    pub detail_generation: u64,
    /// Shared cancellation generation observed by dedicated filesystem workers.
    pub worker_generation: Arc<AtomicU64>,
    pub next_activation_request_id: u64,
    pub pending_activation: Option<PendingBookmarkActivation>,
    pub dropdown_open: bool,
    pub dropdown_selected: usize,
    pub dropdown_scroll: usize,
    pub dropdown_visible_rows: usize,
    pub scrollbar_grab_offset: Option<usize>,
    /// Whether a prior SQLite mirror replacement failed and a later operation
    /// must retry reconciliation even when its authoritative mutation is a no-op.
    pub mirror_needs_repair: bool,
    /// Non-fatal warning from an authoritative commit or SQLite mirror repair.
    pub last_warning: Option<String>,
}

impl BookmarksState {
    /// Load bookmarks from disk. Missing storage is an empty list; corruption,
    /// permission failures, and other read errors remain visible to the user.
    pub fn load() -> Self {
        let mut state = Self::default();
        match tui_file_picker::load_bookmarks() {
            Ok(entries) => {
                state.entries = entries
                    .into_iter()
                    .map(|entry| Bookmark { name: entry.name, path: entry.path })
                    .collect();
            }
            Err(error) => {
                log::warn!("bookmarks: could not load shared store: {error}");
                state.last_warning = Some(format!("Could not load bookmarks: {error}"));
            }
        }
        state
    }

    /// Load the shared TOML store first, then mirror it into SQLite. A valid
    /// existing TOML file is authoritative even when empty. Only a genuinely
    /// absent TOML file may fall back to SQLite and be republished.
    pub fn load_from_db(db: &crate::db::Database) -> Self {
        let storage_path = tui_file_picker::bookmark_storage_path();
        let shared_store_probe = std::fs::symlink_metadata(&storage_path);
        let shared_store_absent = matches!(
            shared_store_probe.as_ref(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        );
        match tui_file_picker::load_bookmarks() {
            Ok(entries) if !shared_store_absent => {
                let mut state = Self {
                    entries: entries
                        .into_iter()
                        .map(|entry| Bookmark {
                            name: entry.name,
                            path: entry.path,
                        })
                        .collect(),
                    ..Self::default()
                };
                // A valid, explicitly empty TOML store is authoritative too:
                // mirror it by clearing stale SQLite rows rather than reviving them.
                state.reconcile_mirror(db);
                return state;
            }
            Ok(_) => {}
            Err(err) => {
                log::warn!(
                    "bookmarks: could not read shared store {}: {}",
                    storage_path.display(),
                    err
                );
                if !shared_store_absent {
                    // An existing shared store is authoritative even when it is
                    // temporarily unreadable or malformed. Likewise, a metadata
                    // probe error other than NotFound is not evidence of
                    // absence. Do not resurrect a stale SQLite mirror and
                    // diverge from the picker.
                    let mut state = Self::default();
                    state.last_warning = Some(err.to_string());
                    return state;
                }
            }
        }

        let mut state = Self::default();
        match db.list_bookmarks() {
            Ok(rows) => {
                state.entries = rows
                    .into_iter()
                    .map(|(_id, name, path)| Bookmark {
                        name,
                        path: PathBuf::from(path),
                    })
                    .collect();
                // Publish only when the TOML store is genuinely absent. The
                // initializer re-checks under the interprocess lock; if another
                // process won the race, adopt its authoritative entries instead
                // of overwriting them with this stale SQLite snapshot.
                if shared_store_absent && !state.entries.is_empty() {
                    let seed = state.as_records();
                    match tui_file_picker::initialize_bookmarks_if_absent(&seed) {
                        Ok(initialization) => {
                            state.entries = initialization
                                .entries
                                .into_iter()
                                .map(|entry| Bookmark {
                                    name: entry.name,
                                    path: entry.path,
                                })
                                .collect();
                            state.last_warning = initialization
                                .status
                                .as_ref()
                                .and_then(|status| status.warning())
                                .map(str::to_string);
                            if !initialization.initialized {
                                state.reconcile_mirror(db);
                            }
                        }
                        Err(error) => {
                            log::warn!("bookmarks: SQLite migration to TOML failed: {error}");
                            let authoritative_path_is_not_absent = match std::fs::symlink_metadata(
                                &storage_path,
                            ) {
                                Ok(_) => true,
                                Err(probe_error) => {
                                    probe_error.kind() != std::io::ErrorKind::NotFound
                                }
                            };
                            if authoritative_path_is_not_absent {
                                // The authoritative path appeared after the
                                // initial probe but could not be adopted. Do not
                                // keep presenting the stale SQLite snapshot as if
                                // it were authoritative.
                                state.entries.clear();
                            }
                            state.last_warning = Some(error.to_string());
                        }
                    }
                }
            }
            Err(error) => {
                log::warn!("bookmarks: SQLite fallback read failed: {error}");
                state.last_warning = Some(error.to_string());
            }
        }
        state
    }

    /// Persist entries to the authoritative shared TOML store.
    ///
    /// This whole-list API is retained for one-time migration. Interactive
    /// edits use mutation APIs so they cannot overwrite concurrent changes.
    pub fn save(&self) -> std::io::Result<tui_file_picker::BookmarkSaveStatus> {
        tui_file_picker::save_bookmarks_atomic(&self.as_records())
    }

    fn as_records(&self) -> Vec<tui_file_picker::BookmarkRecord> {
        self.entries
            .iter()
            .map(|entry| tui_file_picker::BookmarkRecord {
                name: entry.name.clone(),
                path: entry.path.clone(),
            })
            .collect()
    }

    fn apply_commit(&mut self, commit: tui_file_picker::BookmarkCommit) {
        self.entries = commit
            .entries
            .into_iter()
            .map(|entry| Bookmark {
                name: entry.name,
                path: entry.path,
            })
            .collect();
        self.overlay_selected = commit
            .affected_index
            .min(self.entries.len().saturating_sub(1));
        self.ensure_visible();
        self.last_warning = commit.status.warning().map(str::to_string);

        // Invalidate queued work against the pre-commit sequence, but retain
        // completed status/detail data for paths that still exist. The next
        // refill starts at index zero and skips those retained results.
        self.bump_detail_generation();
        self.target_probes_in_flight.clear();
        self.target_probe_next_index = 0;
        let live_paths = self
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<HashSet<_>>();
        self.target_status.retain(|path, _| live_paths.contains(path));
        self.detail_cache.retain(|path, state| {
            live_paths.contains(path)
                && !matches!(
                    state,
                    BookmarkDetailState::Queued
                        | BookmarkDetailState::Loading
                        | BookmarkDetailState::QueueUnavailable(_)
                        | BookmarkDetailState::WorkerUnavailable(_)
                )
        });
        self.pending_activation = None;
    }

    fn apply_mutation(
        &mut self,
        mutation: tui_file_picker::BookmarkMutation,
    ) -> Option<bool> {
        let baseline = self.as_records();
        match tui_file_picker::mutate_bookmarks_atomic(mutation) {
            Ok(commit) => {
                let changed = commit.changed;
                if changed || commit.entries.as_slice() != baseline.as_slice() {
                    self.apply_commit(commit);
                }
                Some(changed)
            }
            Err(error) => {
                log::warn!("bookmarks: authoritative mutation failed: {error}");
                self.last_warning = Some(error.to_string());
                None
            }
        }
    }

    fn apply_mutation_with_db(
        &mut self,
        mutation: tui_file_picker::BookmarkMutation,
        db: &crate::db::Database,
    ) -> Option<bool> {
        let baseline = self.as_records();
        let mirror_needs_repair = self.mirror_needs_repair;
        self.last_warning = None;
        match tui_file_picker::mutate_bookmarks_atomic_with_reconcile(
            mutation,
            |entries, changed| {
                if !changed && !mirror_needs_repair && entries == baseline.as_slice() {
                    return Ok(());
                }
                let mirror = entries
                    .iter()
                    .map(|entry| {
                        (entry.name.clone(), entry.path.display().to_string())
                    })
                    .collect::<Vec<_>>();
                db.replace_bookmarks_transactional(&mirror)
            },
        ) {
            Ok(result) => {
                let changed = result.commit.changed;
                if changed || result.commit.entries.as_slice() != baseline.as_slice() {
                    self.apply_commit(result.commit);
                }
                match result.reconcile_result {
                    Ok(()) => self.mirror_needs_repair = false,
                    Err(error) => self.record_mirror_warning(error),
                }
                Some(changed)
            }
            Err(error) => {
                log::warn!("bookmarks: authoritative mutation failed: {error}");
                self.last_warning = Some(error.to_string());
                None
            }
        }
    }

    /// Add a bookmark and persist it against the latest authoritative state.
    pub fn add(&mut self, name: String, path: PathBuf) -> bool {
        self.apply_mutation(tui_file_picker::BookmarkMutation::Add(
            tui_file_picker::BookmarkRecord { name, path },
        ))
        == Some(true)
    }

    /// Add a bookmark and transactionally reconcile the SQLite compatibility
    /// mirror before the authoritative bookmark lock is released.
    pub fn add_with_db(
        &mut self,
        name: String,
        path: PathBuf,
        db: &crate::db::Database,
    ) -> bool {
        self.apply_mutation_with_db(
            tui_file_picker::BookmarkMutation::Add(
                tui_file_picker::BookmarkRecord { name, path },
            ),
            db,
        ) == Some(true)
    }

    /// Remove a bookmark and transactionally reconcile the SQLite mirror under
    /// the same authoritative lock.
    pub fn remove_with_db(&mut self, index: usize, db: &crate::db::Database) -> bool {
        let Some(expected) = self.entries.get(index).map(|entry| {
            tui_file_picker::BookmarkRecord {
                name: entry.name.clone(),
                path: entry.path.clone(),
            }
        }) else {
            return false;
        };
        self.apply_mutation_with_db(
            tui_file_picker::BookmarkMutation::Remove {
                expected_index: index,
                expected,
            },
            db,
        ) == Some(true)
    }

    fn record_mirror_warning(&mut self, error: String) {
        self.mirror_needs_repair = true;
        log::warn!("bookmarks: SQLite mirror reconciliation failed: {error}");
        self.last_warning = Some(match self.last_warning.take() {
            Some(existing) => format!("{existing}; {error}"),
            None => error,
        });
    }

    fn adopt_records_preserving_selection(
        &mut self,
        records: Vec<tui_file_picker::BookmarkRecord>,
    ) {
        let selected = self.entries.get(self.overlay_selected).cloned();
        self.entries = records
            .into_iter()
            .map(|entry| Bookmark {
                name: entry.name,
                path: entry.path,
            })
            .collect();
        self.overlay_selected = selected
            .and_then(|selected| self.entries.iter().position(|entry| *entry == selected))
            .unwrap_or_else(|| self.overlay_selected.min(self.entries.len().saturating_sub(1)));
        self.ensure_visible();
    }

    /// Reload TOML and replace the SQLite mirror in one transaction while the
    /// authoritative bookmark lock remains held.
    fn reconcile_mirror(&mut self, db: &crate::db::Database) {
        match tui_file_picker::reconcile_bookmarks_locked(|entries| {
            let mirror = entries
                .iter()
                .map(|entry| (entry.name.clone(), entry.path.display().to_string()))
                .collect::<Vec<_>>();
            db.replace_bookmarks_transactional(&mirror)
        }) {
            Ok((entries, result)) => {
                self.adopt_records_preserving_selection(entries);
                match result {
                    Ok(()) => self.mirror_needs_repair = false,
                    Err(error) => self.record_mirror_warning(error),
                }
            }
            Err(error) => {
                log::warn!("bookmarks: authoritative reload for mirror failed: {error}");
                self.last_warning = Some(error.to_string());
            }
        }
    }

    #[cfg(test)]
    fn sync_to_db(&self, db: &crate::db::Database) -> Result<(), String> {
        let mirror = self
            .entries
            .iter()
            .map(|entry| (entry.name.clone(), entry.path.display().to_string()))
            .collect::<Vec<_>>();
        db.replace_bookmarks_transactional(&mirror)
    }

    /// Remove the entry at `index` from the authoritative store.
    pub fn remove(&mut self, index: usize) -> bool {
        let Some(expected) = self.entries.get(index).map(|entry| {
            tui_file_picker::BookmarkRecord {
                name: entry.name.clone(),
                path: entry.path.clone(),
            }
        }) else {
            return false;
        };
        self.apply_mutation(tui_file_picker::BookmarkMutation::Remove {
            expected_index: index,
            expected,
        })
        .is_some()
    }

    #[cfg(test)]
    fn remove_in_memory(&mut self, index: usize) -> bool {
        if index >= self.entries.len() {
            return false;
        }
        self.entries.remove(index);
        if self.overlay_selected >= self.entries.len() && self.overlay_selected > 0 {
            self.overlay_selected = self.entries.len().saturating_sub(1);
        }
        if self.overlay_scroll > 0 && self.overlay_scroll >= self.entries.len() {
            self.overlay_scroll = self.entries.len().saturating_sub(1);
        }
        self.ensure_visible();
        true
    }

    /// Rename the entry at `index` in the authoritative store.
    pub fn rename_at(&mut self, index: usize, new_name: String) -> bool {
        let Some(expected) = self.entries.get(index).map(|entry| {
            tui_file_picker::BookmarkRecord {
                name: entry.name.clone(),
                path: entry.path.clone(),
            }
        }) else {
            return false;
        };
        self.apply_mutation(tui_file_picker::BookmarkMutation::Rename {
            expected_index: index,
            expected,
            new_name,
        }) == Some(true)
    }

    /// Rename and reconcile the SQLite mirror under the authoritative lock.
    /// `Some(false)` is a valid, durable no-op.
    pub fn rename_at_with_db(
        &mut self,
        index: usize,
        new_name: String,
        db: &crate::db::Database,
    ) -> Option<bool> {
        let expected = self.entries.get(index).map(|entry| {
            tui_file_picker::BookmarkRecord {
                name: entry.name.clone(),
                path: entry.path.clone(),
            }
        })?;
        self.apply_mutation_with_db(
            tui_file_picker::BookmarkMutation::Rename {
                expected_index: index,
                expected,
                new_name,
            },
            db,
        )
    }

    /// Move an entry against the latest authoritative TOML order, then rebuild
    /// the SQLite mirror from that committed order.
    pub fn move_at_with_db(
        &mut self,
        index: usize,
        direction: tui_file_picker::BookmarkMoveDirection,
        db: &crate::db::Database,
    ) -> Option<bool> {
        let Some(expected) = self.entries.get(index).map(|entry| {
            tui_file_picker::BookmarkRecord {
                name: entry.name.clone(),
                path: entry.path.clone(),
            }
        }) else {
            return None;
        };
        self.apply_mutation_with_db(
            tui_file_picker::BookmarkMutation::Move {
                expected_index: index,
                expected,
                direction,
            },
            db,
        )
    }

    #[cfg(test)]
    fn add_in_memory(&mut self, name: String, path: PathBuf) {
        self.entries.push(Bookmark { name, path });
    }

    #[cfg(test)]
    fn rename_at_in_memory(&mut self, index: usize, new_name: String) -> bool {
        if let Some(entry) = self.entries.get_mut(index) {
            entry.name = new_name;
            true
        } else {
            false
        }
    }

    pub fn take_warning(&mut self) -> Option<String> {
        self.last_warning.take()
    }

    // ── Manager and dropdown navigation ─────────────────────────────

    pub fn filter_text(&self) -> &str {
        self.filter_input
            .as_ref()
            .map(|input| input.text.as_str())
            .unwrap_or("")
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        let needle = self.filter_text().trim().to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                (needle.is_empty() || entry.name.to_lowercase().contains(&needle))
                    .then_some(index)
            })
            .collect()
    }

    pub fn begin_filter(&mut self) {
        if self.filter_input.is_none() {
            self.filter_input = Some(TextInputState::empty());
        }
    }

    pub fn clear_filter(&mut self) -> bool {
        if self.filter_input.take().is_some() {
            self.overlay_scroll = 0;
            self.snap_selection_to_filter();
            true
        } else {
            false
        }
    }

    pub fn snap_selection_to_filter(&mut self) {
        let filtered = self.filtered_indices();
        if filtered.is_empty() {
            self.overlay_selected = 0;
            self.overlay_scroll = 0;
        } else if !filtered.contains(&self.overlay_selected) {
            self.overlay_selected = filtered[0];
            self.overlay_scroll = 0;
        }
        self.ensure_visible();
    }

    fn selected_filtered_position(&self, filtered: &[usize]) -> Option<usize> {
        filtered.iter().position(|index| *index == self.overlay_selected)
    }

    pub fn overlay_move_up(&mut self) {
        let filtered = self.filtered_indices();
        let Some(position) = self.selected_filtered_position(&filtered) else {
            self.snap_selection_to_filter();
            return;
        };
        if position > 0 {
            self.overlay_selected = filtered[position - 1];
            self.ensure_visible();
        }
    }

    pub fn overlay_move_down(&mut self) {
        let filtered = self.filtered_indices();
        let Some(position) = self.selected_filtered_position(&filtered) else {
            self.snap_selection_to_filter();
            return;
        };
        if position + 1 < filtered.len() {
            self.overlay_selected = filtered[position + 1];
            self.ensure_visible();
        }
    }

    pub fn overlay_page_up(&mut self) {
        let filtered = self.filtered_indices();
        let Some(position) = self.selected_filtered_position(&filtered) else {
            self.snap_selection_to_filter();
            return;
        };
        let destination = position.saturating_sub(self.overlay_visible_rows.max(1));
        self.overlay_selected = filtered[destination];
        self.ensure_visible();
    }

    pub fn overlay_page_down(&mut self) {
        let filtered = self.filtered_indices();
        let Some(position) = self.selected_filtered_position(&filtered) else {
            self.snap_selection_to_filter();
            return;
        };
        let destination = (position + self.overlay_visible_rows.max(1))
            .min(filtered.len().saturating_sub(1));
        self.overlay_selected = filtered[destination];
        self.ensure_visible();
    }

    pub fn overlay_move_top(&mut self) {
        if let Some(first) = self.filtered_indices().first().copied() {
            self.overlay_selected = first;
        }
        self.overlay_scroll = 0;
    }

    pub fn overlay_move_bottom(&mut self) {
        if let Some(last) = self.filtered_indices().last().copied() {
            self.overlay_selected = last;
            self.ensure_visible();
        }
    }

    fn ensure_visible(&mut self) {
        let filtered = self.filtered_indices();
        let Some(position) = self.selected_filtered_position(&filtered) else {
            self.overlay_scroll = 0;
            return;
        };
        if self.overlay_visible_rows == 0 {
            return;
        }
        let max_scroll = filtered.len().saturating_sub(self.overlay_visible_rows);
        if position < self.overlay_scroll {
            self.overlay_scroll = position;
        } else if position >= self.overlay_scroll + self.overlay_visible_rows {
            self.overlay_scroll = position + 1 - self.overlay_visible_rows;
        }
        self.overlay_scroll = self.overlay_scroll.min(max_scroll);
    }

    pub fn selected(&self) -> Option<&Bookmark> {
        self.entries.get(self.overlay_selected)
    }

    pub fn selected_filtered(&self) -> Option<&Bookmark> {
        let selected = self.selected()?;
        let needle = self.filter_text().trim().to_lowercase();
        (needle.is_empty() || selected.name.to_lowercase().contains(&needle)).then_some(selected)
    }

    pub fn target_status(&self, path: &Path) -> Option<BookmarkTargetStatus> {
        self.target_status.get(path).copied()
    }

    pub fn missing_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                self.target_status(&entry.path) == Some(BookmarkTargetStatus::Missing)
            })
            .count()
    }

    pub fn unavailable_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                self.target_status(&entry.path) == Some(BookmarkTargetStatus::Unavailable)
            })
            .count()
    }

    pub fn has_unknown_targets(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| !self.target_status.contains_key(&entry.path))
    }

    fn bump_detail_generation(&mut self) {
        self.detail_generation = self.detail_generation.wrapping_add(1);
        self.worker_generation
            .store(self.detail_generation, Ordering::Release);
        super::bookmark_workers::cancel_superseded_jobs();
    }

    pub fn worker_generation_guard(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.worker_generation)
    }

    pub fn open_overlay(&mut self) {
        self.dropdown_open = false;
        self.overlay_open = true;
        self.overlay_selected = 0;
        self.overlay_scroll = 0;
        self.naming = None;
        self.filter_input = None;
        self.feedback = None;
        self.bump_detail_generation();
        self.target_status.clear();
        self.target_probes_in_flight.clear();
        self.target_probe_next_index = 0;
        self.detail_cache.clear();
        self.pending_activation = None;
        self.scrollbar_grab_offset = None;
    }

    pub fn close_overlay(&mut self) {
        self.overlay_open = false;
        self.bump_detail_generation();
        self.naming = None;
        self.filter_input = None;
        self.target_status.clear();
        self.target_probes_in_flight.clear();
        self.target_probe_next_index = 0;
        self.detail_cache.clear();
        self.pending_activation = None;
        self.scrollbar_grab_offset = None;
    }

    pub fn toggle_dropdown(&mut self) {
        self.dropdown_open = !self.dropdown_open;
        if self.dropdown_open {
            self.overlay_open = false;
            self.dropdown_selected = 0;
            self.dropdown_scroll = 0;
            self.bump_detail_generation();
            self.target_status.clear();
            self.target_probes_in_flight.clear();
            self.target_probe_next_index = 0;
            self.detail_cache.clear();
            self.pending_activation = None;
        } else {
            self.bump_detail_generation();
            self.target_probes_in_flight.clear();
            self.target_probe_next_index = 0;
            self.pending_activation = None;
        }
    }

    pub fn close_dropdown(&mut self) {
        self.dropdown_open = false;
        self.bump_detail_generation();
        self.target_status.clear();
        self.target_probes_in_flight.clear();
        self.target_probe_next_index = 0;
        self.detail_cache.clear();
        self.pending_activation = None;
    }

    pub fn dropdown_choice_count(&self) -> usize {
        self.entries.len().saturating_add(2)
    }

    pub fn dropdown_choice(&self) -> BookmarkDropdownChoice {
        if self.dropdown_selected < self.entries.len() {
            BookmarkDropdownChoice::Bookmark(self.dropdown_selected)
        } else if self.dropdown_selected == self.entries.len() {
            BookmarkDropdownChoice::AddCurrent
        } else {
            BookmarkDropdownChoice::Manage
        }
    }

    pub fn dropdown_move(&mut self, delta: isize) {
        let count = self.dropdown_choice_count();
        if count == 0 {
            return;
        }
        self.dropdown_selected = if delta < 0 {
            if self.dropdown_selected == 0 { count - 1 } else { self.dropdown_selected - 1 }
        } else {
            (self.dropdown_selected + 1) % count
        };
        self.ensure_dropdown_visible();
    }

    pub fn ensure_dropdown_visible(&mut self) {
        if self.dropdown_selected >= self.entries.len() {
            return;
        }
        let visible = self.dropdown_visible_rows.max(1);
        if self.dropdown_selected < self.dropdown_scroll {
            self.dropdown_scroll = self.dropdown_selected;
        } else if self.dropdown_selected >= self.dropdown_scroll + visible {
            self.dropdown_scroll = self.dropdown_selected + 1 - visible;
        }
        self.dropdown_scroll = self
            .dropdown_scroll
            .min(self.entries.len().saturating_sub(visible));
    }

    pub fn set_overlay_visible_rows(&mut self, visible_rows: usize) {
        let visible_rows = visible_rows.max(1);
        let changed = self.overlay_visible_rows != visible_rows;
        self.overlay_visible_rows = visible_rows;
        if changed {
            self.ensure_visible();
        } else {
            let filtered_len = self.filtered_indices().len();
            self.overlay_scroll = self
                .overlay_scroll
                .min(filtered_len.saturating_sub(visible_rows));
        }
    }

    pub fn set_overlay_scroll(&mut self, offset: usize) {
        let filtered_len = self.filtered_indices().len();
        let max_scroll = filtered_len.saturating_sub(self.overlay_visible_rows.max(1));
        self.overlay_scroll = offset.min(max_scroll);
    }

    pub fn set_dropdown_visible_rows(&mut self, visible_rows: usize) {
        let visible_rows = visible_rows.max(1);
        let changed = self.dropdown_visible_rows != visible_rows;
        self.dropdown_visible_rows = visible_rows;
        if changed {
            self.ensure_dropdown_visible();
        } else {
            self.dropdown_scroll = self
                .dropdown_scroll
                .min(self.entries.len().saturating_sub(visible_rows));
        }
    }

    pub fn mark_target_probe_queued(&mut self, path: PathBuf) -> bool {
        !self.target_status.contains_key(&path)
            && self.target_probes_in_flight.insert(path)
    }

    pub fn cancel_target_probe(&mut self, path: &Path) {
        self.target_probes_in_flight.remove(path);
    }

    pub fn next_target_probe_candidate(&mut self) -> Option<PathBuf> {
        while self.target_probe_next_index < self.entries.len() {
            let path = self.entries[self.target_probe_next_index].path.clone();
            if self.target_status.contains_key(&path)
                || self.target_probes_in_flight.contains(&path)
            {
                self.target_probe_next_index = self.target_probe_next_index.saturating_add(1);
                continue;
            }
            return Some(path);
        }
        None
    }

    pub fn commit_target_probe_candidate(&mut self) {
        self.target_probe_next_index = self.target_probe_next_index.saturating_add(1);
    }

    pub fn begin_activation(
        &mut self,
        path: PathBuf,
        surface: BookmarkActivationSurface,
    ) -> PendingBookmarkActivation {
        self.next_activation_request_id = self.next_activation_request_id.wrapping_add(1);
        let pending = PendingBookmarkActivation {
            generation: self.detail_generation,
            request_id: self.next_activation_request_id,
            path,
            surface,
        };
        self.pending_activation = Some(pending.clone());
        pending
    }

    pub fn take_matching_activation(
        &mut self,
        generation: u64,
        request_id: u64,
        path: &Path,
    ) -> Option<PendingBookmarkActivation> {
        let matches = self.pending_activation.as_ref().is_some_and(|pending| {
            pending.generation == generation
                && pending.request_id == request_id
                && pending.path == path
        });
        matches.then(|| self.pending_activation.take()).flatten()
    }

    pub fn apply_target_statuses(
        &mut self,
        generation: u64,
        statuses: Vec<(PathBuf, BookmarkTargetStatus)>,
    ) {
        if generation != self.detail_generation {
            return;
        }
        for (path, status) in statuses {
            self.target_probes_in_flight.remove(&path);
            self.target_status.insert(path, status);
        }
    }

    pub fn detail_state(&self, path: &Path) -> Option<&BookmarkDetailState> {
        self.detail_cache.get(path)
    }

    pub fn mark_detail_queued(&mut self, path: PathBuf) -> bool {
        let can_queue = matches!(
            self.detail_cache.get(&path),
            None
                | Some(BookmarkDetailState::QueueUnavailable(_))
                | Some(BookmarkDetailState::WorkerUnavailable(_))
        );
        if !can_queue {
            return false;
        }
        self.detail_cache.insert(path, BookmarkDetailState::Queued);
        true
    }

    pub fn mark_detail_queue_unavailable(&mut self, path: PathBuf, message: String) {
        self.detail_cache
            .insert(path, BookmarkDetailState::QueueUnavailable(message));
    }

    pub fn mark_detail_worker_unavailable(&mut self, path: PathBuf, message: String) {
        self.detail_cache
            .insert(path, BookmarkDetailState::WorkerUnavailable(message));
    }

    pub fn clear_detail_request(&mut self, path: &Path) {
        if matches!(
            self.detail_cache.get(path),
            Some(BookmarkDetailState::Queued) | Some(BookmarkDetailState::Loading)
        ) {
            self.detail_cache.remove(path);
        }
    }

    pub fn apply_detail_started(&mut self, generation: u64, path: &Path) {
        if !self.overlay_open || generation != self.detail_generation {
            return;
        }
        let queued = matches!(
            self.detail_cache.get(path),
            Some(BookmarkDetailState::Queued)
        );
        if queued {
            self.detail_cache
                .insert(path.to_path_buf(), BookmarkDetailState::Loading);
        }
    }

    pub fn apply_detail_result(
        &mut self,
        generation: u64,
        path: PathBuf,
        result: Result<BookmarkDetail, String>,
    ) -> bool {
        if !self.overlay_open || generation != self.detail_generation {
            return false;
        }
        self.detail_cache.insert(
            path,
            match result {
                Ok(detail) => BookmarkDetailState::Ready(detail),
                Err(error) => BookmarkDetailState::Error(error),
            },
        );
        true
    }

    /// Compute a friendly default name for a new bookmark pointing at `path`.
    /// Uses the last path component, or "Root" for `/`.
    pub fn default_name_for_path(path: &Path) -> String {
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Root".to_string())
    }

    /// Start naming a new bookmark for `path` with `default_name` as the
    /// initial editable text.
    pub fn start_add(&mut self, path: PathBuf) {
        self.filter_input = None;
        let default = Self::default_name_for_path(&path);
        self.naming = Some(BookmarkNaming::Add {
            input: TextInputState::new(default),
            path,
        });
    }

    /// Start renaming the entry at `index`, seeded with its current name.
    pub fn start_rename(&mut self, index: usize) {
        self.filter_input = None;
        if let Some(entry) = self.entries.get(index) {
            self.naming = Some(BookmarkNaming::Rename {
                input: TextInputState::new(entry.name.clone()),
                idx: index,
            });
        }
    }

    /// Commit the current naming operation (Add or Rename) if the input is
    /// non-empty. Returns true if a change was committed.
    pub fn commit_naming(&mut self) -> bool {
        let Some(naming) = self.naming.clone() else {
            return false;
        };
        let committed = match naming {
            BookmarkNaming::Add { input, path } => {
                let name = input.text.trim().to_string();
                !name.is_empty() && self.add(name, path)
            }
            BookmarkNaming::Rename { input, idx } => {
                let name = input.text.trim().to_string();
                !name.is_empty() && self.rename_at(idx, name)
            }
        };
        if committed {
            self.naming = None;
        }
        committed
    }

    /// Commit the naming operation and reconcile the SQLite mirror.
    pub fn commit_naming_with_db(
        &mut self,
        db: &crate::db::Database,
    ) -> BookmarkNamingCommit {
        let Some(naming) = self.naming.clone() else {
            return BookmarkNamingCommit::Failed;
        };
        let outcome = match naming {
            BookmarkNaming::Add { input, path } => {
                let name = input.text.trim().to_string();
                if name.is_empty() {
                    BookmarkNamingCommit::Failed
                } else if self.add_with_db(name, path, db) {
                    BookmarkNamingCommit::Changed
                } else {
                    BookmarkNamingCommit::Failed
                }
            }
            BookmarkNaming::Rename { input, idx } => {
                let name = input.text.trim().to_string();
                if name.is_empty() {
                    BookmarkNamingCommit::Failed
                } else {
                    match self.rename_at_with_db(idx, name, db) {
                        Some(true) => BookmarkNamingCommit::Changed,
                        Some(false) => BookmarkNamingCommit::Unchanged,
                        None => BookmarkNamingCommit::Failed,
                    }
                }
            }
        };
        if !matches!(outcome, BookmarkNamingCommit::Failed) {
            self.naming = None;
        }
        outcome
    }

    /// Cancel the current naming operation (discard input).
    pub fn cancel_naming(&mut self) {
        self.naming = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state_with_entries(n: usize, visible: usize) -> BookmarksState {
        let mut s = BookmarksState::default();
        s.entries = (0..n)
            .map(|i| Bookmark {
                name: format!("bookmark-{}", i),
                path: PathBuf::from(format!("/tmp/{}", i)),
            })
            .collect();
        s.overlay_visible_rows = visible;
        s
    }

    // ── CRUD ─────────────────────────────────────────────────────────

    #[test]
    fn add_in_memory_appends() {
        let mut s = BookmarksState::default();
        s.add_in_memory("Music".to_string(), PathBuf::from("/home/u/Music"));
        s.add_in_memory("Downloads".to_string(), PathBuf::from("/home/u/Downloads"));
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.entries[0].name, "Music");
        assert_eq!(s.entries[1].name, "Downloads");
    }

    #[test]
    fn remove_in_memory_bounds_check() {
        let mut s = make_state_with_entries(3, 5);
        assert!(!s.remove_in_memory(99));
        assert_eq!(s.entries.len(), 3);
    }

    #[test]
    fn remove_in_memory_middle_keeps_index() {
        let mut s = make_state_with_entries(5, 5);
        s.overlay_selected = 2;
        assert!(s.remove_in_memory(2));
        assert_eq!(s.entries.len(), 4);
        // Selection stays at 2 (now pointing to the former index 3).
        assert_eq!(s.overlay_selected, 2);
    }

    #[test]
    fn remove_in_memory_last_clamps_selection() {
        let mut s = make_state_with_entries(3, 5);
        s.overlay_selected = 2;
        assert!(s.remove_in_memory(2));
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.overlay_selected, 1);
    }

    #[test]
    fn rename_at_in_memory_updates_name() {
        let mut s = make_state_with_entries(3, 5);
        assert!(s.rename_at_in_memory(1, "Renamed".to_string()));
        assert_eq!(s.entries[1].name, "Renamed");
    }

    #[test]
    fn rename_at_in_memory_out_of_bounds_returns_false() {
        let mut s = make_state_with_entries(3, 5);
        assert!(!s.rename_at_in_memory(99, "X".to_string()));
    }

    #[test]
    fn target_probe_cursor_is_incremental_and_deduplicates_paths() {
        let mut state = BookmarksState::default();
        state.entries = vec![
            Bookmark {
                name: "A".to_string(),
                path: PathBuf::from("/same"),
            },
            Bookmark {
                name: "A duplicate".to_string(),
                path: PathBuf::from("/same"),
            },
            Bookmark {
                name: "B".to_string(),
                path: PathBuf::from("/other"),
            },
        ];

        let first = state.next_target_probe_candidate().expect("first");
        assert_eq!(first, PathBuf::from("/same"));
        assert!(state.mark_target_probe_queued(first));
        state.commit_target_probe_candidate();

        let second = state.next_target_probe_candidate().expect("second");
        assert_eq!(second, PathBuf::from("/other"));
        assert_eq!(state.target_probe_next_index, 2);
    }

    #[test]
    fn stale_status_completion_cannot_clear_current_generation_dedup() {
        let mut state = BookmarksState::default();
        state.detail_generation = 2;
        state.worker_generation.store(2, Ordering::Release);
        let path = PathBuf::from("/network/music");
        assert!(state.mark_target_probe_queued(path.clone()));

        state.apply_target_statuses(
            1,
            vec![(path.clone(), BookmarkTargetStatus::Reachable)],
        );
        assert!(state.target_probes_in_flight.contains(&path));
        assert_eq!(state.target_status(&path), None);

        state.apply_target_statuses(
            2,
            vec![(path.clone(), BookmarkTargetStatus::Unavailable)],
        );
        assert!(!state.target_probes_in_flight.contains(&path));
        assert_eq!(
            state.target_status(&path),
            Some(BookmarkTargetStatus::Unavailable)
        );
    }

    #[test]
    fn activation_result_requires_exact_generation_request_and_path() {
        let mut state = BookmarksState::default();
        state.detail_generation = 4;
        state.worker_generation.store(4, Ordering::Release);
        let pending = state.begin_activation(
            PathBuf::from("/music"),
            BookmarkActivationSurface::Manager,
        );
        assert!(state
            .take_matching_activation(3, pending.request_id, &pending.path)
            .is_none());
        assert!(state
            .take_matching_activation(4, pending.request_id + 1, &pending.path)
            .is_none());
        assert_eq!(
            state
                .take_matching_activation(4, pending.request_id, &pending.path)
                .expect("matching result"),
            pending
        );
    }

    #[test]
    fn filtered_reorder_contract_uses_complete_saved_sequence() {
        let mut state = BookmarksState::default();
        state.entries = vec![
            Bookmark {
                name: "match-a".to_string(),
                path: PathBuf::from("/a"),
            },
            Bookmark {
                name: "hidden".to_string(),
                path: PathBuf::from("/hidden"),
            },
            Bookmark {
                name: "match-c".to_string(),
                path: PathBuf::from("/c"),
            },
        ];
        state.filter_input = Some(TextInputState::new("match".to_string()));
        assert_eq!(state.filtered_indices(), vec![0, 2]);

        // Shift+J/K moves one position in the authoritative complete array,
        // not directly to the next filtered row. The visible order can
        // therefore remain unchanged for one keypress, and UI feedback says so.
        state.entries.swap(0, 1);
        assert_eq!(state.filtered_indices(), vec![1, 2]);
        assert_eq!(
            state
                .filtered_indices()
                .into_iter()
                .map(|index| state.entries[index].name.as_str())
                .collect::<Vec<_>>(),
            vec!["match-a", "match-c"]
        );
    }

    // ── Navigation with scroll ──────────────────────────────────────

    #[test]
    fn move_down_scrolls_when_cursor_leaves_window() {
        let mut s = make_state_with_entries(10, 5);
        for _ in 0..5 {
            s.overlay_move_down();
        }
        assert_eq!(s.overlay_selected, 5);
        assert_eq!(s.overlay_scroll, 1);
    }

    #[test]
    fn move_up_scrolls_when_cursor_leaves_top() {
        let mut s = make_state_with_entries(10, 3);
        for _ in 0..5 {
            s.overlay_move_down();
        }
        assert_eq!(s.overlay_selected, 5);
        assert_eq!(s.overlay_scroll, 3);
        for _ in 0..3 {
            s.overlay_move_up();
        }
        assert_eq!(s.overlay_selected, 2);
        assert_eq!(s.overlay_scroll, 2);
    }

    #[test]
    fn page_down_clamps_to_last() {
        let mut s = make_state_with_entries(7, 5);
        s.overlay_page_down();
        s.overlay_page_down();
        assert_eq!(s.overlay_selected, 6);
    }

    #[test]
    fn move_top_and_bottom_jump() {
        let mut s = make_state_with_entries(30, 10);
        s.overlay_move_bottom();
        assert_eq!(s.overlay_selected, 29);
        assert_eq!(s.overlay_scroll, 20);
        s.overlay_move_top();
        assert_eq!(s.overlay_selected, 0);
        assert_eq!(s.overlay_scroll, 0);
    }

    // ── Default name helper ─────────────────────────────────────────

    #[test]
    fn default_name_uses_last_path_component() {
        assert_eq!(
            BookmarksState::default_name_for_path(&PathBuf::from("/home/user/Music")),
            "Music"
        );
    }

    #[test]
    fn default_name_falls_back_to_root() {
        assert_eq!(
            BookmarksState::default_name_for_path(&PathBuf::from("/")),
            "Root"
        );
    }

    // ── Naming state machine ────────────────────────────────────────

    #[test]
    fn start_add_seeds_default_name() {
        let mut s = BookmarksState::default();
        s.start_add(PathBuf::from("/home/u/Music"));
        match &s.naming {
            Some(BookmarkNaming::Add { input, path }) => {
                assert_eq!(input.text, "Music");
                assert_eq!(*path, PathBuf::from("/home/u/Music"));
            }
            _ => panic!("expected Add naming mode"),
        }
    }

    #[test]
    fn start_rename_seeds_existing_name() {
        let mut s = make_state_with_entries(3, 5);
        s.start_rename(1);
        match &s.naming {
            Some(BookmarkNaming::Rename { input, idx }) => {
                assert_eq!(input.text, "bookmark-1");
                assert_eq!(*idx, 1);
            }
            _ => panic!("expected Rename naming mode"),
        }
    }

    #[test]
    fn commit_naming_add_empty_name_rejected() {
        let mut s = BookmarksState::default();
        s.start_add(PathBuf::from("/x"));
        // Clear the input text.
        if let Some(BookmarkNaming::Add { input, .. }) = &mut s.naming {
            input.text.clear();
            input.cursor = 0;
        }
        let ok = s.commit_naming();
        assert!(!ok);
        assert!(s.naming.is_some());
        assert_eq!(s.entries.len(), 0);
    }

    #[test]
    fn cancel_naming_discards_input() {
        let mut s = BookmarksState::default();
        s.start_add(PathBuf::from("/x"));
        s.cancel_naming();
        assert!(s.naming.is_none());
        assert_eq!(s.entries.len(), 0);
    }

    #[test]
    fn filter_is_case_insensitive_and_preserves_absolute_identity() {
        let mut state = BookmarksState::default();
        state.entries = vec![
            Bookmark {
                name: "Music".to_string(),
                path: PathBuf::from("/music"),
            },
            Bookmark {
                name: "Downloads".to_string(),
                path: PathBuf::from("/downloads"),
            },
            Bookmark {
                name: "Live Music".to_string(),
                path: PathBuf::from("/live"),
            },
        ];
        state.overlay_selected = 2;
        state.filter_input = Some(TextInputState::new("MUSIC".to_string()));

        assert_eq!(state.filtered_indices(), vec![0, 2]);
        state.snap_selection_to_filter();
        assert_eq!(state.overlay_selected, 2);
    }

    #[test]
    fn dropdown_wraps_across_bookmarks_and_action_rows() {
        let mut state = make_state_with_entries(2, 5);
        state.dropdown_visible_rows = 1;

        state.dropdown_move(-1);
        assert_eq!(state.dropdown_choice(), BookmarkDropdownChoice::Manage);
        state.dropdown_move(1);
        assert_eq!(state.dropdown_choice(), BookmarkDropdownChoice::Bookmark(0));

        state.dropdown_selected = 1;
        state.ensure_dropdown_visible();
        assert_eq!(state.dropdown_scroll, 1);
        state.dropdown_move(1);
        assert_eq!(state.dropdown_choice(), BookmarkDropdownChoice::AddCurrent);
        assert_eq!(
            state.dropdown_scroll, 1,
            "action-row selection must not jump the bookmark viewport"
        );
    }

    #[test]
    fn stale_detail_worker_result_is_ignored_after_reopen() {
        let mut state = make_state_with_entries(1, 5);
        state.open_overlay();
        let stale_generation = state.detail_generation;
        let path = state.entries[0].path.clone();
        assert!(state.mark_detail_queued(path.clone()));

        state.close_overlay();
        state.open_overlay();
        state.apply_detail_result(
            stale_generation,
            path.clone(),
            Ok(BookmarkDetail {
                item_count: 0,
                entries: Vec::new(),
                omitted_count: 0,
            }),
        );

        assert!(state.detail_state(&path).is_none());
    }

    #[test]
    fn detail_backpressure_state_is_explicit_and_retryable() {
        let mut state = make_state_with_entries(1, 5);
        state.open_overlay();
        let generation = state.detail_generation;
        let path = state.entries[0].path.clone();

        assert!(state.mark_detail_queued(path.clone()));
        assert!(matches!(
            state.detail_state(&path),
            Some(BookmarkDetailState::Queued)
        ));

        state.apply_detail_started(generation, &path);
        assert!(matches!(
            state.detail_state(&path),
            Some(BookmarkDetailState::Loading)
        ));

        state.mark_detail_queue_unavailable(path.clone(), "queue busy".to_string());
        assert!(matches!(
            state.detail_state(&path),
            Some(BookmarkDetailState::QueueUnavailable(message)) if message == "queue busy"
        ));
        assert!(state.mark_detail_queued(path.clone()));
        assert!(matches!(
            state.detail_state(&path),
            Some(BookmarkDetailState::Queued)
        ));

        state.mark_detail_worker_unavailable(
            path.clone(),
            "detail workers unavailable".to_string(),
        );
        assert!(matches!(
            state.detail_state(&path),
            Some(BookmarkDetailState::WorkerUnavailable(message))
                if message == "detail workers unavailable"
        ));
        assert!(state.mark_detail_queued(path));
    }

    #[test]
    fn stale_detail_started_message_is_ignored() {
        let mut state = make_state_with_entries(1, 5);
        state.open_overlay();
        let stale_generation = state.detail_generation;
        let path = state.entries[0].path.clone();
        assert!(state.mark_detail_queued(path.clone()));

        state.close_overlay();
        state.open_overlay();
        state.apply_detail_started(stale_generation, &path);

        assert!(state.detail_state(&path).is_none());
    }

    #[test]
    fn sqlite_mirror_rebuild_preserves_committed_order() {
        let db = crate::db::Database::open_memory().expect("memory database");
        let mut state = BookmarksState::default();
        state.entries = vec![
            Bookmark {
                name: "Second".to_string(),
                path: PathBuf::from("/second"),
            },
            Bookmark {
                name: "First".to_string(),
                path: PathBuf::from("/first"),
            },
            Bookmark {
                name: "Third".to_string(),
                path: PathBuf::from("/third"),
            },
        ];

        state.sync_to_db(&db).expect("mirror rebuild");
        let names = db
            .list_bookmarks()
            .expect("list mirror")
            .into_iter()
            .map(|(_, name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["Second", "First", "Third"]);
    }

    #[test]
    fn move_refuses_invalid_index_without_touching_authoritative_storage() {
        let db = crate::db::Database::open_memory().expect("memory database");
        let mut state = make_state_with_entries(2, 5);
        assert_eq!(
            state.move_at_with_db(
                2,
                tui_file_picker::BookmarkMoveDirection::Down,
                &db,
            ),
            None
        );
    }

    #[test]
    fn target_status_counts_duplicate_paths_without_filesystem_access() {
        let mut state = BookmarksState::default();
        let shared = PathBuf::from("/not-inspected/shared");
        state.entries = vec![
            Bookmark {
                name: "One".to_string(),
                path: shared.clone(),
            },
            Bookmark {
                name: "Two".to_string(),
                path: shared.clone(),
            },
            Bookmark {
                name: "Three".to_string(),
                path: PathBuf::from("/not-inspected/other"),
            },
        ];
        state
            .target_status
            .insert(shared, BookmarkTargetStatus::Missing);
        state.target_status.insert(
            PathBuf::from("/not-inspected/other"),
            BookmarkTargetStatus::Unavailable,
        );

        assert_eq!(state.missing_count(), 2);
        assert_eq!(state.unavailable_count(), 1);
        assert!(!state.has_unknown_targets());
    }

    #[test]
    fn stale_target_status_worker_result_is_ignored_after_reopen() {
        let mut state = make_state_with_entries(1, 5);
        state.open_overlay();
        let stale_generation = state.detail_generation;
        let path = state.entries[0].path.clone();

        state.close_overlay();
        state.open_overlay();
        state.apply_target_statuses(
            stale_generation,
            vec![(path.clone(), BookmarkTargetStatus::Missing)],
        );

        assert_eq!(state.target_status(&path), None);
    }

    #[test]
    fn selected_filtered_is_none_when_filter_has_no_match() {
        let mut state = make_state_with_entries(1, 5);
        state.filter_input = Some(TextInputState::new("absent".to_string()));

        assert!(state.selected_filtered().is_none());
    }

    #[test]
    fn unchanged_manager_viewport_height_preserves_pointer_scroll() {
        let mut state = make_state_with_entries(20, 5);
        state.overlay_selected = 0;
        state.set_overlay_scroll(10);
        assert_eq!(state.overlay_scroll, 10);

        state.set_overlay_visible_rows(5);
        assert_eq!(
            state.overlay_scroll, 10,
            "a render pass must not recenter a viewport moved by wheel or scrollbar"
        );
    }

    #[test]
    fn unchanged_dropdown_viewport_height_preserves_pointer_scroll() {
        let mut state = make_state_with_entries(20, 5);
        state.dropdown_visible_rows = 5;
        state.dropdown_selected = 0;
        state.dropdown_scroll = 10;

        state.set_dropdown_visible_rows(5);
        assert_eq!(
            state.dropdown_scroll, 10,
            "a render pass must not recenter a dropdown viewport moved by wheel"
        );
    }

}
