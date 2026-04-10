//! File browser state and directory scanning

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::convert::formats::AudioFormat;
use crate::tui::probe::{SourceInfo, SourceMetadata};
use crate::tui::text_input::TextInputState;

/// Cached info for an audio file: probe data + metadata tags
#[derive(Debug, Clone)]
pub struct CachedInfo {
    pub source: SourceInfo,
    pub metadata: SourceMetadata,
}

/// Cached statistics for a directory
#[derive(Debug, Clone, Default)]
pub struct DirStats {
    pub file_count: usize,
    pub audio_count: usize,
    pub total_size: u64,
}

/// What to do when the user selects a file in the browse screen
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BrowseReturnTarget {
    /// Return selected file to the source pane on the convert screen
    ConvertSource,
    /// Add selected files to the conversion queue
    ConvertQueue,
    /// Navigate only (no target)
    None,
}

impl Default for BrowseReturnTarget {
    fn default() -> Self {
        Self::None
    }
}

/// Kind of entry in a directory listing
#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    /// `..` entry (parent directory)
    ParentDir,
    /// A subdirectory
    Directory,
    /// An audio file (format detected from extension)
    AudioFile(AudioFormat),
    /// A 7z archive (or similar)
    Archive,
    /// Any other file
    OtherFile,
}

/// A single entry in the browse listing
#[derive(Debug, Clone)]
pub struct BrowseEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

impl BrowseEntry {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Directory | EntryKind::ParentDir)
    }

    pub fn is_audio(&self) -> bool {
        matches!(self.kind, EntryKind::AudioFile(_))
    }

    pub fn is_archive(&self) -> bool {
        matches!(self.kind, EntryKind::Archive)
    }
}

/// State for the browse screen
#[derive(Debug, Clone)]
pub struct BrowseState {
    pub current_dir: PathBuf,
    pub entries: Vec<BrowseEntry>,
    pub selected_index: usize,
    pub scroll_offset: usize,
    pub visible_height: usize,

    /// Multi-selected file paths
    pub multi_selected: Vec<PathBuf>,

    /// Filter input (when /-mode is active)
    pub filter_input: Option<TextInputState>,
    /// Committed filter text (empty = no filter)
    pub filter_text: String,
    pub show_hidden: bool,

    /// Probe cache: path → Some(info) if probed, None if probe failed
    pub probe_cache: HashMap<PathBuf, Option<Arc<CachedInfo>>>,

    /// Directory stats cache: path → (file_count, audio_count, total_size)
    pub dir_stats_cache: HashMap<PathBuf, Arc<DirStats>>,

    /// Where to send selected files
    pub return_target: BrowseReturnTarget,

    /// Error message from last directory read, if any
    pub error: Option<String>,
}

impl BrowseState {
    pub fn new() -> Self {
        let start_dir = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/"));

        let mut state = Self {
            current_dir: start_dir,
            entries: Vec::new(),
            selected_index: 0,
            scroll_offset: 0,
            visible_height: 0,
            multi_selected: Vec::new(),
            filter_input: None,
            filter_text: String::new(),
            show_hidden: false,
            probe_cache: HashMap::new(),
            dir_stats_cache: HashMap::new(),
            return_target: BrowseReturnTarget::None,
            error: None,
        };
        state.refresh();
        state
    }

    /// Re-scan the current directory
    pub fn refresh(&mut self) {
        self.entries.clear();
        self.error = None;

        // Add parent entry if not at root
        if self.current_dir.parent().is_some() {
            self.entries.push(BrowseEntry {
                path: self.current_dir.parent().unwrap().to_path_buf(),
                name: "..".to_string(),
                kind: EntryKind::ParentDir,
                size: 0,
                modified: None,
            });
        }

        // Read directory entries
        match fs::read_dir(&self.current_dir) {
            Ok(read) => {
                let mut dirs: Vec<BrowseEntry> = Vec::new();
                let mut files: Vec<BrowseEntry> = Vec::new();

                for entry in read.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();

                    // Skip hidden files unless show_hidden is true
                    if !self.show_hidden && name.starts_with('.') {
                        continue;
                    }

                    let metadata = match entry.metadata() {
                        Ok(m) => m,
                        Err(_) => continue,
                    };

                    let size = metadata.len();
                    let modified = metadata.modified().ok();

                    let kind = if metadata.is_dir() {
                        EntryKind::Directory
                    } else {
                        classify_file(&path)
                    };

                    let browse_entry = BrowseEntry {
                        path,
                        name,
                        kind: kind.clone(),
                        size,
                        modified,
                    };

                    if matches!(kind, EntryKind::Directory) {
                        dirs.push(browse_entry);
                    } else {
                        files.push(browse_entry);
                    }
                }

                dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

                self.entries.extend(dirs);
                self.entries.extend(files);
            }
            Err(e) => {
                self.error = Some(format!("Cannot read directory: {}", e));
            }
        }

        // Clamp selection
        if self.selected_index >= self.entries.len() {
            self.selected_index = self.entries.len().saturating_sub(1);
        }
        self.scroll_offset = 0;
    }

    /// Enter a directory (or the parent if index points to `..`)
    pub fn enter_selected(&mut self) -> bool {
        if let Some(entry) = self.entries.get(self.selected_index) {
            if entry.is_dir() {
                self.current_dir = entry.path.clone();
                self.selected_index = 0;
                self.refresh();
                return true;
            }
        }
        false
    }

    /// Navigate to the parent directory
    pub fn go_parent(&mut self) -> bool {
        if let Some(parent) = self.current_dir.parent() {
            let prev_name = self
                .current_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string());
            self.current_dir = parent.to_path_buf();
            self.refresh();

            // Try to position cursor on the directory we came from
            if let Some(name) = prev_name {
                if let Some(idx) = self.entries.iter().position(|e| e.name == name) {
                    self.selected_index = idx;
                    self.ensure_visible();
                }
            }
            return true;
        }
        false
    }

    /// Navigate directly to a given path
    pub fn navigate_to(&mut self, path: PathBuf) {
        if path.is_dir() {
            self.current_dir = path;
            self.selected_index = 0;
            self.refresh();
        }
    }

    pub fn selected_entry(&self) -> Option<&BrowseEntry> {
        self.entries.get(self.selected_index)
    }

    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
            self.ensure_visible();
        }
    }

    pub fn move_down(&mut self) {
        if self.selected_index + 1 < self.entries.len() {
            self.selected_index += 1;
            self.ensure_visible();
        }
    }

    pub fn move_top(&mut self) {
        self.selected_index = 0;
        self.scroll_offset = 0;
    }

    pub fn move_bottom(&mut self) {
        if !self.entries.is_empty() {
            self.selected_index = self.entries.len() - 1;
            self.ensure_visible();
        }
    }

    pub fn page_up(&mut self) {
        let jump = self.visible_height.max(1);
        self.selected_index = self.selected_index.saturating_sub(jump);
        self.ensure_visible();
    }

    pub fn page_down(&mut self) {
        let jump = self.visible_height.max(1);
        self.selected_index = (self.selected_index + jump).min(self.entries.len().saturating_sub(1));
        self.ensure_visible();
    }

    /// Scroll to keep the selected index visible
    pub fn ensure_visible(&mut self) {
        if self.visible_height == 0 {
            return;
        }
        if self.selected_index < self.scroll_offset {
            self.scroll_offset = self.selected_index;
        } else if self.selected_index >= self.scroll_offset + self.visible_height {
            self.scroll_offset = self.selected_index - self.visible_height + 1;
        }
    }

    /// Toggle multi-select on the current entry
    pub fn toggle_selection(&mut self) {
        if let Some(entry) = self.entries.get(self.selected_index) {
            if !entry.is_dir() {
                let path = entry.path.clone();
                if let Some(pos) = self.multi_selected.iter().position(|p| p == &path) {
                    self.multi_selected.remove(pos);
                } else {
                    self.multi_selected.push(path);
                }
            }
        }
    }

    pub fn is_multi_selected(&self, path: &Path) -> bool {
        self.multi_selected.iter().any(|p| p.as_path() == path)
    }

    pub fn clear_multi_selection(&mut self) {
        self.multi_selected.clear();
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.refresh();
    }

    /// Probe the currently selected entry (audio files only).
    /// Caches the result; cached entries are not re-probed.
    pub fn probe_current(&mut self) {
        let path = match self.entries.get(self.selected_index) {
            Some(entry) if entry.is_audio() => entry.path.clone(),
            Some(entry) if entry.is_dir() && !matches!(entry.kind, EntryKind::ParentDir) => {
                // Compute directory stats lazily
                if !self.dir_stats_cache.contains_key(&entry.path) {
                    let stats = compute_dir_stats(&entry.path);
                    self.dir_stats_cache.insert(entry.path.clone(), Arc::new(stats));
                }
                return;
            }
            _ => return,
        };

        if self.probe_cache.contains_key(&path) {
            return; // already probed (successfully or not)
        }

        match crate::tui::probe::probe_audio(&path) {
            Ok(source) => {
                let metadata = crate::tui::probe::read_metadata(&path).unwrap_or_default();
                self.probe_cache.insert(
                    path,
                    Some(Arc::new(CachedInfo { source, metadata })),
                );
            }
            Err(_) => {
                self.probe_cache.insert(path, None);
            }
        }
    }

    /// Get cached info for the currently selected audio file, if probed
    pub fn current_cached_info(&self) -> Option<&Arc<CachedInfo>> {
        let entry = self.entries.get(self.selected_index)?;
        if !entry.is_audio() {
            return None;
        }
        self.probe_cache.get(&entry.path)?.as_ref()
    }

    /// Get cached directory stats for the current selection (if it's a directory)
    pub fn current_dir_stats(&self) -> Option<&Arc<DirStats>> {
        let entry = self.entries.get(self.selected_index)?;
        if !matches!(entry.kind, EntryKind::Directory) {
            return None;
        }
        self.dir_stats_cache.get(&entry.path)
    }
}

/// Compute stats for a directory: file count, audio count, total size.
/// Reads the directory once. Does not recurse.
fn compute_dir_stats(path: &Path) -> DirStats {
    let mut stats = DirStats::default();
    if let Ok(read) = fs::read_dir(path) {
        for entry in read.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    stats.file_count += 1;
                    stats.total_size += meta.len();
                    if matches!(classify_file(&entry.path()), EntryKind::AudioFile(_)) {
                        stats.audio_count += 1;
                    }
                }
            }
        }
    }
    stats
}

impl Default for BrowseState {
    fn default() -> Self {
        Self::new()
    }
}

/// Classify a file by its extension
fn classify_file(path: &Path) -> EntryKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    match ext.as_deref() {
        Some("flac") => EntryKind::AudioFile(AudioFormat::Flac),
        Some("wav") | Some("wave") => EntryKind::AudioFile(AudioFormat::Wav),
        Some("aiff") | Some("aif") | Some("aifc") => EntryKind::AudioFile(AudioFormat::Aiff),
        Some("wv") => EntryKind::AudioFile(AudioFormat::WavPack),
        Some("mp3") => EntryKind::AudioFile(AudioFormat::Mp3),
        Some("m4a") | Some("mp4") | Some("aac") => EntryKind::AudioFile(AudioFormat::Aac),
        Some("opus") => EntryKind::AudioFile(AudioFormat::Opus),
        Some("7z") => EntryKind::Archive,
        _ => EntryKind::OtherFile,
    }
}
