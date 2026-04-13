//! Browse-screen bookmarks: persistent list of user-curated directory shortcuts.
//!
//! Stored at `~/.config/tonepoet/bookmarks.toml`. Unlike recent files (which
//! are automatic and capped), bookmarks are explicitly managed by the user
//! and have no cap.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use super::text_input::TextInputState;

/// A single bookmark: user-friendly name and target path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub path: PathBuf,
}

/// The on-disk wrapper: a TOML file with a top-level `[[entries]]` array.
#[derive(Debug, Default, Serialize, Deserialize)]
struct BookmarksFile {
    #[serde(default)]
    entries: Vec<Bookmark>,
}

/// Naming mode for the bookmarks overlay: either adding a new bookmark for
/// a captured path, or renaming an existing entry by index.
#[derive(Debug, Clone)]
pub enum BookmarkNaming {
    Add { input: TextInputState, path: PathBuf },
    Rename { input: TextInputState, idx: usize },
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
}

impl BookmarksState {
    /// Load bookmarks from disk, or return empty state on missing file / parse error.
    pub fn load() -> Self {
        let path = Self::storage_path();
        let mut state = Self::default();
        if let Ok(text) = fs::read_to_string(&path) {
            if let Ok(file) = toml::from_str::<BookmarksFile>(&text) {
                state.entries = file.entries;
            }
        }
        state
    }

    /// Load from SQLite DB (preferred) with TOML import fallback.
    pub fn load_from_db(db: &crate::db::Database) -> Self {
        let mut state = Self::default();

        // Try loading from DB first.
        if let Ok(rows) = db.list_bookmarks() {
            if !rows.is_empty() {
                state.entries = rows
                    .into_iter()
                    .map(|(_id, name, path)| Bookmark {
                        name,
                        path: PathBuf::from(path),
                    })
                    .collect();
                return state;
            }
        }

        // DB empty — try importing from TOML.
        let toml_path = Self::storage_path();
        if let Ok(text) = fs::read_to_string(&toml_path) {
            if let Ok(file) = toml::from_str::<BookmarksFile>(&text) {
                for bm in &file.entries {
                    let _ = db.add_bookmark(&bm.name, &bm.path.display().to_string());
                }
                state.entries = file.entries;
            }
        }

        state
    }

    /// Persist entries to disk. Best-effort: returns Err on IO failure but
    /// callers can ignore it (non-critical feature).
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::storage_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = BookmarksFile { entries: self.entries.clone() };
        let text = toml::to_string_pretty(&file)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        fs::write(&path, text)
    }

    /// Path to the bookmarks TOML file.
    fn storage_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tonepoet")
            .join("bookmarks.toml")
    }

    // ── CRUD helpers (in-memory only; wrappers call save) ───────────

    /// Add a bookmark and persist.
    pub fn add(&mut self, name: String, path: PathBuf) {
        self.add_in_memory(name, path);
        let _ = self.save();
    }

    /// Add a bookmark, persisting to both TOML and SQLite.
    pub fn add_with_db(&mut self, name: String, path: PathBuf, db: &crate::db::Database) {
        self.add_in_memory(name, path);
        let _ = self.save();
        self.sync_to_db(db);
    }

    /// Remove a bookmark, persisting to both TOML and SQLite.
    pub fn remove_with_db(&mut self, index: usize, db: &crate::db::Database) -> bool {
        if self.remove_in_memory(index) {
            let _ = self.save();
            self.sync_to_db(db);
            true
        } else {
            false
        }
    }

    /// Sync the entire in-memory bookmark list to the DB (clear + rebuild).
    /// Simple and correct for small lists (typically 5-20 bookmarks).
    fn sync_to_db(&self, db: &crate::db::Database) {
        // Clear existing DB bookmarks.
        let _ = db.clear_bookmarks();
        // Re-insert all.
        for bm in &self.entries {
            let _ = db.add_bookmark(&bm.name, &bm.path.display().to_string());
        }
    }

    fn add_in_memory(&mut self, name: String, path: PathBuf) {
        self.entries.push(Bookmark { name, path });
    }

    /// Remove the entry at `index` and persist. Returns true if the index was valid.
    pub fn remove(&mut self, index: usize) -> bool {
        if self.remove_in_memory(index) {
            let _ = self.save();
            true
        } else {
            false
        }
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

    /// Rename the entry at `index` and persist. Returns true on success.
    pub fn rename_at(&mut self, index: usize, new_name: String) -> bool {
        if self.rename_at_in_memory(index, new_name) {
            let _ = self.save();
            true
        } else {
            false
        }
    }

    fn rename_at_in_memory(&mut self, index: usize, new_name: String) -> bool {
        if let Some(entry) = self.entries.get_mut(index) {
            entry.name = new_name;
            true
        } else {
            false
        }
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
        self.overlay_selected = (self.overlay_selected + step)
            .min(self.entries.len().saturating_sub(1));
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
        let naming = match self.naming.take() {
            Some(n) => n,
            None => return false,
        };
        let committed = match naming {
            BookmarkNaming::Add { input, path } => {
                let name = input.text.trim().to_string();
                if name.is_empty() {
                    false
                } else {
                    self.add(name, path);
                    true
                }
            }
            BookmarkNaming::Rename { input, idx } => {
                let name = input.text.trim().to_string();
                if name.is_empty() {
                    false
                } else {
                    self.rename_at(idx, name)
                }
            }
        };
        committed
    }

    /// Commit the naming operation, syncing to DB.
    pub fn commit_naming_with_db(&mut self, db: &crate::db::Database) -> bool {
        let naming = match self.naming.take() {
            Some(n) => n,
            None => return false,
        };
        let committed = match naming {
            BookmarkNaming::Add { input, path } => {
                let name = input.text.trim().to_string();
                if name.is_empty() {
                    false
                } else {
                    self.add_in_memory(name, path);
                    let _ = self.save();
                    true
                }
            }
            BookmarkNaming::Rename { input, idx } => {
                let name = input.text.trim().to_string();
                if name.is_empty() {
                    false
                } else {
                    self.rename_at_in_memory(idx, name);
                    let _ = self.save();
                    true
                }
            }
        };
        if committed {
            self.sync_to_db(db);
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
        assert!(s.naming.is_none());
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
