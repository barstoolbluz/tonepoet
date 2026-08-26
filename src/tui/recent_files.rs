//! Recent files history — persistent list of files recently loaded as source.
//!
//! SQLite is the durable authority. `~/.cache/tonepoet/recent.json` is read only
//! for the explicitly gated one-time legacy import. The list is capped at
//! MAX_ENTRIES (most recent first), with duplicates deduplicated by path.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Maximum number of recent entries to retain.
pub const MAX_ENTRIES: usize = crate::db::RECENT_FILES_RETENTION_LIMIT;

/// A single recent-files entry: path and last-used timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: PathBuf,
    /// Seconds since UNIX_EPOCH. Storing as u64 for simple serde interop.
    pub timestamp: u64,
}

impl RecentEntry {
    pub fn new(path: PathBuf) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        Self { path, timestamp }
    }

    /// Human-friendly "relative time" label like "2m ago", "3h ago", "5d ago".
    pub fn relative_time(&self) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        let delta = now.saturating_sub(self.timestamp);
        if delta < 60 {
            "just now".to_string()
        } else if delta < 3600 {
            format!("{}m ago", delta / 60)
        } else if delta < 86400 {
            format!("{}h ago", delta / 3600)
        } else if delta < 86400 * 30 {
            format!("{}d ago", delta / 86400)
        } else {
            format!("{}mo ago", delta / (86400 * 30))
        }
    }
}

/// In-memory recent files list plus overlay UI state.
#[derive(Debug, Clone, Default)]
pub struct RecentFilesState {
    pub entries: Vec<RecentEntry>,
    /// Currently selected index in the overlay (when it's open).
    pub overlay_selected: usize,
    /// Scroll offset (top visible index) when the overlay list exceeds
    /// the visible row budget.
    pub overlay_scroll: usize,
    /// Number of entry rows visible in the overlay. Set by the renderer
    /// so `ensure_visible` can adjust scroll correctly after navigation.
    pub overlay_visible_rows: usize,
    /// True when the overlay is displayed.
    pub overlay_open: bool,
}

impl RecentFilesState {
    /// Load the SQLite authority, performing the one-time JSON import only
    /// while the durable marker says it has not completed.
    pub fn load_from_db(db: &crate::db::Database) -> Result<Self, String> {
        if !db.recent_legacy_import_done()? {
            if db.has_recent_items()? {
                let _ = db.publish_legacy_recent_import(&[])?;
            } else {
                let entries = Self::load_legacy_json_for_import()?;
                let rows = entries
                    .iter()
                    .map(|entry| {
                        (
                            entry.path.display().to_string(),
                            entry.timestamp.min(i64::MAX as u64) as i64,
                        )
                    })
                    .collect::<Vec<_>>();
                if matches!(
                    db.publish_legacy_recent_import(&rows)?,
                    crate::db::LegacyImportPublication::Imported
                ) {
                    Self::remove_legacy_json_after_import();
                }
            }
        }

        let rows = db.list_recent(MAX_ENTRIES)?;
        Ok(Self {
            entries: rows
                .into_iter()
                .map(|(path, ts)| RecentEntry {
                    path: PathBuf::from(path),
                    timestamp: ts.max(0) as u64,
                })
                .collect(),
            ..Self::default()
        })
    }

    fn load_legacy_json_for_import() -> Result<Vec<RecentEntry>, String> {
        let path = Self::legacy_json_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(error) => {
                return Err(format!(
                    "read legacy recent-files JSON {:?}: {error}",
                    path
                ));
            }
        };
        serde_json::from_str::<Vec<RecentEntry>>(&text)
            .map_err(|e| format!("parse legacy recent-files JSON {:?}: {e}", path))
    }

    fn remove_legacy_json_after_import() {
        let path = Self::legacy_json_path();
        match std::fs::remove_file(&path) {
            Ok(()) => log::info!("Retired legacy recent-files JSON after SQLite import"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!(
                "SQLite recent-files import succeeded, but legacy JSON could not be removed: {}",
                error
            ),
        }
    }

    /// Record a file as recently used. SQLite is the only durable writer.
    pub fn record_use_with_db(&mut self, path: &Path, db: &crate::db::Database) {
        self.record_use_in_memory(path);
        if let Err(error) = db.record_recent(&path.display().to_string()) {
            log::error!("could not persist recent-file access: {error}");
        }
    }

    /// In-memory half of `record_use`: state mutation only, no disk IO.
    /// Extracted so unit tests can exercise the logic without touching the
    /// filesystem.
    fn record_use_in_memory(&mut self, path: &Path) {
        // Remove any existing entry for the same path.
        self.entries.retain(|e| e.path != path);
        // Prepend the new entry.
        self.entries.insert(0, RecentEntry::new(path.to_path_buf()));
        // Cap at MAX_ENTRIES.
        self.entries.truncate(MAX_ENTRIES);
        // Clamp selection + scroll for consistency.
        if self.overlay_selected >= self.entries.len() {
            self.overlay_selected = self.entries.len().saturating_sub(1);
        }
        if self.overlay_scroll >= self.entries.len() {
            self.overlay_scroll = 0;
        }
    }

    /// Remove an entry durably. The SQLite delete commits before the in-memory
    /// list changes, so a failed write cannot present a deletion that will
    /// resurrect on restart.
    pub fn remove_with_db(
        &mut self,
        index: usize,
        db: &crate::db::Database,
    ) -> Result<bool, String> {
        let Some(path) = self.entries.get(index).map(|entry| entry.path.clone()) else {
            return Ok(false);
        };
        db.remove_recent(&path.display().to_string())?;
        Ok(self.remove_in_memory(index))
    }

    /// In-memory half of `remove`: state mutation only, no disk IO.
    /// Returns true if the index was valid and an entry was removed.
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

    /// Path to the import-only legacy recent-files JSON file.
    fn legacy_json_path() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tonepoet")
            .join("recent.json")
    }

    /// Navigate the overlay selection up.
    pub fn overlay_move_up(&mut self) {
        if self.overlay_selected > 0 {
            self.overlay_selected -= 1;
            self.ensure_visible();
        }
    }

    /// Navigate the overlay selection down.
    pub fn overlay_move_down(&mut self) {
        if self.overlay_selected + 1 < self.entries.len() {
            self.overlay_selected += 1;
            self.ensure_visible();
        }
    }

    /// Page up — jump one visible page.
    pub fn overlay_page_up(&mut self) {
        let step = self.overlay_visible_rows.max(1);
        self.overlay_selected = self.overlay_selected.saturating_sub(step);
        self.ensure_visible();
    }

    /// Page down — jump one visible page.
    pub fn overlay_page_down(&mut self) {
        let step = self.overlay_visible_rows.max(1);
        self.overlay_selected =
            (self.overlay_selected + step).min(self.entries.len().saturating_sub(1));
        self.ensure_visible();
    }

    /// Jump to first entry.
    pub fn overlay_move_top(&mut self) {
        self.overlay_selected = 0;
        self.overlay_scroll = 0;
    }

    /// Jump to last entry.
    pub fn overlay_move_bottom(&mut self) {
        self.overlay_selected = self.entries.len().saturating_sub(1);
        self.ensure_visible();
    }

    /// Adjust `overlay_scroll` so the selected entry is visible. Called after
    /// navigation. Assumes `overlay_visible_rows` is set by the renderer.
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

    /// Get the currently-selected entry, if any.
    pub fn selected(&self) -> Option<&RecentEntry> {
        self.entries.get(self.overlay_selected)
    }

    /// Open the overlay and reset selection + scroll to the top.
    pub fn open_overlay(&mut self) {
        self.overlay_open = true;
        self.overlay_selected = 0;
        self.overlay_scroll = 0;
    }

    /// Close the overlay.
    pub fn close_overlay(&mut self) {
        self.overlay_open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a state with N dummy entries [/tmp/0, /tmp/1, ...] and a given
    /// visible_rows budget, without touching disk.
    fn make_state(n: usize, visible_rows: usize) -> RecentFilesState {
        let mut s = RecentFilesState::default();
        s.entries = (0..n)
            .map(|i| RecentEntry {
                path: PathBuf::from(format!("/tmp/{}", i)),
                timestamp: i as u64,
            })
            .collect();
        s.overlay_visible_rows = visible_rows;
        s
    }

    // ── Navigation: move_up / move_down ──────────────────────────────

    #[test]
    fn move_down_advances_within_visible_window() {
        let mut s = make_state(10, 5);
        assert_eq!(s.overlay_selected, 0);
        assert_eq!(s.overlay_scroll, 0);
        s.overlay_move_down();
        assert_eq!(s.overlay_selected, 1);
        assert_eq!(s.overlay_scroll, 0); // still visible
    }

    #[test]
    fn move_down_scrolls_when_cursor_leaves_window() {
        let mut s = make_state(10, 5);
        // Move down 5 times: cursor goes 0→1→2→3→4→5.
        // At 5, cursor is at (0 + 5) which equals scroll(0) + visible(5) → must scroll.
        for _ in 0..5 {
            s.overlay_move_down();
        }
        assert_eq!(s.overlay_selected, 5);
        assert_eq!(s.overlay_scroll, 1); // scrolled by one
    }

    #[test]
    fn move_down_stops_at_last_entry() {
        let mut s = make_state(3, 5);
        for _ in 0..10 {
            s.overlay_move_down();
        }
        assert_eq!(s.overlay_selected, 2); // last valid index
    }

    #[test]
    fn move_up_from_zero_is_noop() {
        let mut s = make_state(5, 5);
        s.overlay_move_up();
        assert_eq!(s.overlay_selected, 0);
    }

    #[test]
    fn move_up_scrolls_when_cursor_leaves_top() {
        let mut s = make_state(10, 3);
        // Scroll down to position 5 (scroll should be 3).
        for _ in 0..5 {
            s.overlay_move_down();
        }
        assert_eq!(s.overlay_selected, 5);
        assert_eq!(s.overlay_scroll, 3);
        // Now move up past the visible window (cursor goes to 2, scroll must follow).
        for _ in 0..3 {
            s.overlay_move_up();
        }
        assert_eq!(s.overlay_selected, 2);
        assert_eq!(s.overlay_scroll, 2);
    }

    // ── Navigation: page_up / page_down ──────────────────────────────

    #[test]
    fn page_down_jumps_visible_rows() {
        let mut s = make_state(20, 5);
        s.overlay_page_down();
        assert_eq!(s.overlay_selected, 5);
        assert_eq!(s.overlay_scroll, 1); // cursor just past visible (0..5) → scroll to 1
    }

    #[test]
    fn page_down_clamps_to_last() {
        let mut s = make_state(7, 5);
        s.overlay_page_down();
        s.overlay_page_down();
        assert_eq!(s.overlay_selected, 6); // clamped to len-1
    }

    #[test]
    fn page_up_at_top_is_noop_on_selection() {
        let mut s = make_state(20, 5);
        s.overlay_page_up();
        assert_eq!(s.overlay_selected, 0);
        assert_eq!(s.overlay_scroll, 0);
    }

    #[test]
    fn page_up_from_middle_scrolls() {
        let mut s = make_state(30, 5);
        s.overlay_selected = 15;
        s.overlay_scroll = 13;
        s.overlay_page_up();
        assert_eq!(s.overlay_selected, 10);
        // ensure_visible: 10 < scroll=13 → scroll = 10
        assert_eq!(s.overlay_scroll, 10);
    }

    // ── Navigation: move_top / move_bottom ───────────────────────────

    #[test]
    fn move_top_resets_selection_and_scroll() {
        let mut s = make_state(50, 10);
        s.overlay_selected = 30;
        s.overlay_scroll = 25;
        s.overlay_move_top();
        assert_eq!(s.overlay_selected, 0);
        assert_eq!(s.overlay_scroll, 0);
    }

    #[test]
    fn move_bottom_jumps_to_last_and_scrolls() {
        let mut s = make_state(30, 10);
        s.overlay_move_bottom();
        assert_eq!(s.overlay_selected, 29);
        assert_eq!(s.overlay_scroll, 20); // 29 - 10 + 1
    }

    #[test]
    fn move_bottom_on_empty_list_stays_at_zero() {
        let mut s = make_state(0, 10);
        s.overlay_move_bottom();
        assert_eq!(s.overlay_selected, 0);
    }

    // ── ensure_visible edge cases ───────────────────────────────────

    #[test]
    fn ensure_visible_bails_when_visible_rows_is_zero() {
        let mut s = make_state(10, 0);
        s.overlay_selected = 5;
        s.overlay_scroll = 0;
        // Call through a navigation method (they all invoke ensure_visible).
        s.overlay_move_down();
        // Scroll should not have moved because visible_rows = 0.
        assert_eq!(s.overlay_scroll, 0);
    }

    // ── record_use_in_memory: dedup and ordering ─────────────────────

    #[test]
    fn record_use_prepends_new_entry() {
        let mut s = RecentFilesState::default();
        s.record_use_in_memory(Path::new("/a.flac"));
        s.record_use_in_memory(Path::new("/b.flac"));
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.entries[0].path, PathBuf::from("/b.flac")); // most recent first
        assert_eq!(s.entries[1].path, PathBuf::from("/a.flac"));
    }

    #[test]
    fn record_use_dedups_and_floats_to_top() {
        let mut s = RecentFilesState::default();
        s.record_use_in_memory(Path::new("/a.flac"));
        s.record_use_in_memory(Path::new("/b.flac"));
        s.record_use_in_memory(Path::new("/a.flac")); // re-record first entry
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.entries[0].path, PathBuf::from("/a.flac"));
        assert_eq!(s.entries[1].path, PathBuf::from("/b.flac"));
    }

    #[test]
    fn record_use_caps_at_max_entries() {
        let mut s = RecentFilesState::default();
        for i in 0..(MAX_ENTRIES + 10) {
            s.record_use_in_memory(&PathBuf::from(format!("/f{}.flac", i)));
        }
        assert_eq!(s.entries.len(), MAX_ENTRIES);
        // Most recent is the last-inserted one.
        assert_eq!(
            s.entries[0].path,
            PathBuf::from(format!("/f{}.flac", MAX_ENTRIES + 9))
        );
    }

    // ── remove_in_memory: bounds and cursor clamping ─────────────────

    #[test]
    fn remove_out_of_bounds_returns_false() {
        let mut s = make_state(3, 5);
        assert!(!s.remove_in_memory(99));
        assert_eq!(s.entries.len(), 3); // unchanged
    }

    #[test]
    fn remove_last_entry_clamps_selection() {
        let mut s = make_state(3, 5);
        s.overlay_selected = 2;
        assert!(s.remove_in_memory(2));
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.overlay_selected, 1);
    }

    #[test]
    fn remove_middle_entry_keeps_index() {
        let mut s = make_state(5, 5);
        s.overlay_selected = 2;
        assert!(s.remove_in_memory(2));
        assert_eq!(s.entries.len(), 4);
        // Selection stays at 2 (which now points to what was at index 3).
        assert_eq!(s.overlay_selected, 2);
    }

    #[test]
    fn remove_only_entry_stays_at_zero() {
        let mut s = make_state(1, 5);
        s.overlay_selected = 0;
        assert!(s.remove_in_memory(0));
        assert_eq!(s.entries.len(), 0);
        assert_eq!(s.overlay_selected, 0);
    }

    #[test]
    fn remove_with_db_deletes_sqlite_before_memory() {
        let db = crate::db::Database::open_memory().expect("database");
        db.record_recent_at("/tmp/durable.flac", 123)
            .expect("seed durable recent");
        let mut state = RecentFilesState {
            entries: vec![RecentEntry {
                path: PathBuf::from("/tmp/durable.flac"),
                timestamp: 123,
            }],
            ..RecentFilesState::default()
        };

        assert!(state.remove_with_db(0, &db).expect("durable remove"));
        assert!(state.entries.is_empty());
        assert!(db.list_recent(MAX_ENTRIES).expect("list recents").is_empty());
    }

    #[test]
    fn durable_delete_stays_deleted_after_restart() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("tonepoet.db");
        {
            let db = crate::db::Database::open_path(&db_path).expect("database");
            db.record_recent_at("/tmp/restart-delete.flac", 123)
                .expect("seed recent");
            let mut state = RecentFilesState::load_from_db(&db).expect("load recent authority");
            assert_eq!(state.entries.len(), 1);
            assert!(state.remove_with_db(0, &db).expect("durable delete"));
        }

        let db = crate::db::Database::open_path(&db_path).expect("reopen database");
        let state = RecentFilesState::load_from_db(&db).expect("reload recent authority");
        assert!(state.entries.is_empty(), "deleted recent must not resurrect");
    }

    #[test]
    fn missing_file_auto_drop_path_stays_deleted_after_restart() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("tonepoet.db");
        let missing = temp.path().join("unmounted-source.flac");
        assert!(!missing.exists());
        {
            let db = crate::db::Database::open_path(&db_path).expect("database");
            db.record_recent_at(&missing.display().to_string(), 321)
                .expect("seed missing recent");
            let mut state = RecentFilesState::load_from_db(&db).expect("load recent authority");
            assert_eq!(state.entries.len(), 1);
            // This is the same durable operation used by the Enter-key
            // missing-file auto-drop path in keybindings.rs.
            assert!(state.remove_with_db(0, &db).expect("auto-drop missing recent"));
        }

        let db = crate::db::Database::open_path(&db_path).expect("reopen database");
        let state = RecentFilesState::load_from_db(&db).expect("reload recent authority");
        assert!(state.entries.is_empty(), "auto-dropped recent must not resurrect");
    }

    #[test]
    fn remove_with_db_failure_preserves_memory_for_restart_consistency() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("missing-recent-table.db");
        {
            let conn = rusqlite::Connection::open(&path).expect("raw database");
            conn.pragma_update(None, "user_version", 25)
                .expect("stamp current version without tables");
        }
        // A current-version database intentionally bypasses migration DDL, so
        // this fixture gives the durable delete path a deterministic missing
        // table failure without exposing Database internals across modules.
        let db = crate::db::Database::open_path(&path).expect("database without recent table");
        let mut state = RecentFilesState {
            entries: vec![RecentEntry {
                path: PathBuf::from("/tmp/durable.flac"),
                timestamp: 123,
            }],
            ..RecentFilesState::default()
        };

        assert!(state.remove_with_db(0, &db).is_err());
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].path, PathBuf::from("/tmp/durable.flac"));
    }

    // ── relative_time boundaries ─────────────────────────────────────

    #[test]
    fn relative_time_just_now_for_fresh_entry() {
        let e = RecentEntry::new(PathBuf::from("/x"));
        assert_eq!(e.relative_time(), "just now");
    }

    #[test]
    fn relative_time_future_timestamp_shows_just_now() {
        // Clock-skew protection: a timestamp in the future shouldn't panic.
        let e = RecentEntry {
            path: PathBuf::from("/x"),
            timestamp: u64::MAX,
        };
        // saturating_sub makes delta = 0 → "just now"
        assert_eq!(e.relative_time(), "just now");
    }
}
