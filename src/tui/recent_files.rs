//! Recent files history — persistent list of files recently loaded as source.
//!
//! Stored at `~/.cache/tonepoet/recent.json`. Capped at MAX_ENTRIES (most recent
//! first). Duplicates are deduplicated by path on insert, with the new timestamp
//! floating the entry to the top.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Maximum number of recent entries to retain.
pub const MAX_ENTRIES: usize = 50;

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
    /// True when the overlay is displayed.
    pub overlay_open: bool,
}

impl RecentFilesState {
    /// Load the recent files list from disk, or return an empty state if the
    /// file doesn't exist or can't be parsed.
    pub fn load() -> Self {
        let path = Self::storage_path();
        let mut state = Self::default();
        if let Ok(text) = fs::read_to_string(&path) {
            if let Ok(entries) = serde_json::from_str::<Vec<RecentEntry>>(&text) {
                state.entries = entries;
            }
        }
        state
    }

    /// Record a file as recently used: prepend (deduping), trim to MAX_ENTRIES,
    /// persist to disk. Safe to call frequently.
    pub fn record_use(&mut self, path: &Path) {
        // Remove any existing entry for the same path.
        self.entries.retain(|e| e.path != path);
        // Prepend the new entry.
        self.entries.insert(0, RecentEntry::new(path.to_path_buf()));
        // Cap at MAX_ENTRIES.
        self.entries.truncate(MAX_ENTRIES);
        // Clamp selection for consistency.
        if self.overlay_selected >= self.entries.len() {
            self.overlay_selected = self.entries.len().saturating_sub(1);
        }
        // Persist.
        let _ = self.save();
    }

    /// Remove the entry at `index`. Persists to disk.
    pub fn remove(&mut self, index: usize) {
        if index < self.entries.len() {
            self.entries.remove(index);
            if self.overlay_selected >= self.entries.len() && self.overlay_selected > 0 {
                self.overlay_selected = self.entries.len().saturating_sub(1);
            }
            let _ = self.save();
        }
    }

    /// Persist current entries to disk. Best-effort: returns Err on I/O failure
    /// but callers can ignore it (non-critical feature).
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::storage_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&self.entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        fs::write(&path, text)
    }

    /// Path to the recent files JSON file.
    fn storage_path() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tonepoet")
            .join("recent.json")
    }

    /// Navigate the overlay selection up.
    pub fn overlay_move_up(&mut self) {
        if self.overlay_selected > 0 {
            self.overlay_selected -= 1;
        }
    }

    /// Navigate the overlay selection down.
    pub fn overlay_move_down(&mut self) {
        if self.overlay_selected + 1 < self.entries.len() {
            self.overlay_selected += 1;
        }
    }

    /// Get the currently-selected entry, if any.
    pub fn selected(&self) -> Option<&RecentEntry> {
        self.entries.get(self.overlay_selected)
    }

    /// Open the overlay and reset selection to the top.
    pub fn open_overlay(&mut self) {
        self.overlay_open = true;
        self.overlay_selected = 0;
    }

    /// Close the overlay.
    pub fn close_overlay(&mut self) {
        self.overlay_open = false;
    }
}
