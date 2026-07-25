//! Browse-screen bookmarks: persistent list of user-curated directory shortcuts.
//!
//! Stored at `~/.config/tonepoet/bookmarks.toml`. Unlike recent files (which
//! are automatic and capped), bookmarks are explicitly managed by the user
//! and have no cap.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::text_input::TextInputState;

/// A single bookmark: user-friendly name and target path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub path: PathBuf,
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
    pub overlay_selected: usize,
    pub overlay_scroll: usize,
    pub overlay_visible_rows: usize,
    pub naming: Option<BookmarkNaming>,
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
                if let Err(error) = state.sync_to_db(db) {
                    log::warn!("bookmarks: SQLite mirror reconciliation failed: {error}");
                    state.last_warning = Some(error);
                }
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
                                if let Err(error) = state.sync_to_db(db) {
                                    log::warn!(
                                        "bookmarks: concurrent TOML winner could not be mirrored to SQLite: {error}"
                                    );
                                    state.last_warning = Some(match state.last_warning.take() {
                                        Some(existing) => format!("{existing}; {error}"),
                                        None => error,
                                    });
                                }
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
    }

    fn apply_mutation(&mut self, mutation: tui_file_picker::BookmarkMutation) -> bool {
        match tui_file_picker::mutate_bookmarks_atomic(mutation) {
            Ok(commit) => {
                self.apply_commit(commit);
                true
            }
            Err(error) => {
                log::warn!("bookmarks: authoritative mutation failed: {error}");
                self.last_warning = Some(error.to_string());
                false
            }
        }
    }

    /// Add a bookmark and persist it against the latest authoritative state.
    pub fn add(&mut self, name: String, path: PathBuf) -> bool {
        self.apply_mutation(tui_file_picker::BookmarkMutation::Add(
            tui_file_picker::BookmarkRecord { name, path },
        ))
    }

    /// Add a bookmark, then reconcile the SQLite compatibility mirror.
    pub fn add_with_db(
        &mut self,
        name: String,
        path: PathBuf,
        db: &crate::db::Database,
    ) -> bool {
        if !self.add(name, path) {
            return false;
        }
        self.reconcile_mirror(db);
        true
    }

    /// Remove a bookmark, then reconcile the SQLite compatibility mirror.
    pub fn remove_with_db(&mut self, index: usize, db: &crate::db::Database) -> bool {
        if !self.remove(index) {
            return false;
        }
        self.reconcile_mirror(db);
        true
    }

    fn reconcile_mirror(&mut self, db: &crate::db::Database) {
        if let Err(error) = self.sync_to_db(db) {
            log::warn!("bookmarks: SQLite mirror reconciliation failed: {error}");
            self.last_warning = Some(match self.last_warning.take() {
                Some(existing) => format!("{existing}; {error}"),
                None => error,
            });
        }
    }

    /// Rebuild the SQLite compatibility mirror with checked rollback.
    ///
    /// SQLite is not authoritative. If rebuilding fails, this function makes a
    /// best-effort restoration of the prior mirror and returns every failure to
    /// the caller; no database error is discarded.
    fn sync_to_db(&self, db: &crate::db::Database) -> Result<(), String> {
        let previous = db
            .list_bookmarks()
            .map_err(|error| format!("could not snapshot SQLite bookmarks: {error}"))?;

        let rebuild = || -> Result<(), String> {
            db.clear_bookmarks()
                .map_err(|error| format!("could not clear SQLite bookmarks: {error}"))?;
            for bookmark in &self.entries {
                db.add_bookmark(&bookmark.name, &bookmark.path.display().to_string())
                    .map_err(|error| {
                        format!(
                            "could not mirror bookmark '{}' ({}): {error}",
                            bookmark.name,
                            bookmark.path.display()
                        )
                    })?;
            }
            Ok(())
        };

        if let Err(rebuild_error) = rebuild() {
            let restore = (|| -> Result<(), String> {
                db.clear_bookmarks()
                    .map_err(|error| format!("could not clear partial SQLite mirror: {error}"))?;
                for (_id, name, path) in &previous {
                    db.add_bookmark(name, path).map_err(|error| {
                        format!("could not restore SQLite bookmark '{name}' ({path}): {error}")
                    })?;
                }
                Ok(())
            })();
            return match restore {
                Ok(()) => Err(format!(
                    "{rebuild_error}; restored the previous SQLite mirror"
                )),
                Err(restore_error) => Err(format!(
                    "{rebuild_error}; SQLite rollback also failed: {restore_error}"
                )),
            };
        }
        Ok(())
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
    }

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
        })
    }

    fn add_in_memory(&mut self, name: String, path: PathBuf) {
        self.entries.push(Bookmark { name, path });
    }

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

    // ── Overlay navigation ──────────────────────────────────────────

    pub fn overlay_move_up(&mut self) {
        if self.overlay_selected > 0 {
            self.overlay_selected -= 1;
            self.ensure_visible();
        }
    }

    pub fn overlay_move_down(&mut self) {
        if self.overlay_selected + 1 < self.entries.len() {
            self.overlay_selected += 1;
            self.ensure_visible();
        }
    }

    pub fn overlay_page_up(&mut self) {
        let step = self.overlay_visible_rows.max(1);
        self.overlay_selected = self.overlay_selected.saturating_sub(step);
        self.ensure_visible();
    }

    pub fn overlay_page_down(&mut self) {
        let step = self.overlay_visible_rows.max(1);
        self.overlay_selected =
            (self.overlay_selected + step).min(self.entries.len().saturating_sub(1));
        self.ensure_visible();
    }

    pub fn overlay_move_top(&mut self) {
        self.overlay_selected = 0;
        self.overlay_scroll = 0;
    }

    pub fn overlay_move_bottom(&mut self) {
        self.overlay_selected = self.entries.len().saturating_sub(1);
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        if self.overlay_visible_rows == 0 {
            return;
        }
        if self.overlay_selected < self.overlay_scroll {
            self.overlay_scroll = self.overlay_selected;
        } else if self.overlay_selected >= self.overlay_scroll + self.overlay_visible_rows {
            self.overlay_scroll = self.overlay_selected - self.overlay_visible_rows + 1;
        }
    }

    pub fn selected(&self) -> Option<&Bookmark> {
        self.entries.get(self.overlay_selected)
    }

    pub fn open_overlay(&mut self) {
        self.overlay_open = true;
        self.overlay_selected = 0;
        self.overlay_scroll = 0;
        self.naming = None;
    }

    pub fn close_overlay(&mut self) {
        self.overlay_open = false;
        self.naming = None;
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
        let default = Self::default_name_for_path(&path);
        self.naming = Some(BookmarkNaming::Add {
            input: TextInputState::new(default),
            path,
        });
    }

    /// Start renaming the entry at `index`, seeded with its current name.
    pub fn start_rename(&mut self, index: usize) {
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
    pub fn commit_naming_with_db(&mut self, db: &crate::db::Database) -> bool {
        let Some(naming) = self.naming.clone() else {
            return false;
        };
        let committed = match naming {
            BookmarkNaming::Add { input, path } => {
                let name = input.text.trim().to_string();
                !name.is_empty() && self.add_with_db(name, path, db)
            }
            BookmarkNaming::Rename { input, idx } => {
                let name = input.text.trim().to_string();
                if name.is_empty() || !self.rename_at(idx, name) {
                    false
                } else {
                    self.reconcile_mirror(db);
                    true
                }
            }
        };
        if committed {
            self.naming = None;
        }
        committed
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
}
