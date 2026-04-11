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

/// Sort field for browse listings
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortBy {
    Name,
    Date,
    Type,
    Size,
}

impl SortBy {
    pub fn next(&self) -> Self {
        match self {
            Self::Name => Self::Date,
            Self::Date => Self::Type,
            Self::Type => Self::Size,
            Self::Size => Self::Name,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Date => "date",
            Self::Type => "type",
            Self::Size => "size",
        }
    }
}

/// Sort direction
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    pub fn toggle(&self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

/// Format filter: None = all audio formats, Some(fmt) = only that format,
/// or use the special sentinel via `AudioOnly` to hide non-audio files.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FormatFilter {
    Off,
    AudioOnly,
    Only(AudioFormat),
}

impl FormatFilter {
    /// Cycle to the next filter: Off → AudioOnly → each audio format → Off
    pub fn next(&self) -> Self {
        match self {
            Self::Off => Self::AudioOnly,
            Self::AudioOnly => Self::Only(AudioFormat::Flac),
            Self::Only(AudioFormat::Flac) => Self::Only(AudioFormat::Opus),
            Self::Only(AudioFormat::Opus) => Self::Only(AudioFormat::Aac),
            Self::Only(AudioFormat::Aac) => Self::Only(AudioFormat::Mp3),
            Self::Only(AudioFormat::Mp3) => Self::Only(AudioFormat::Alac),
            Self::Only(AudioFormat::Alac) => Self::Only(AudioFormat::Wav),
            Self::Only(AudioFormat::Wav) => Self::Only(AudioFormat::WavPack),
            Self::Only(AudioFormat::WavPack) => Self::Only(AudioFormat::Aiff),
            Self::Only(AudioFormat::Aiff) => Self::Off,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Off => "off".to_string(),
            Self::AudioOnly => "audio".to_string(),
            Self::Only(fmt) => fmt.name().to_string(),
        }
    }

    /// Whether a given entry passes the filter
    pub fn allows(&self, kind: &EntryKind) -> bool {
        match self {
            Self::Off => true,
            Self::AudioOnly => matches!(kind, EntryKind::AudioFile(_)),
            Self::Only(fmt) => matches!(kind, EntryKind::AudioFile(f) if f == fmt),
        }
    }
}

/// A single entry in the browse listing
#[derive(Debug, Clone)]
pub struct BrowseEntry {
    pub path: PathBuf,
    pub name: String,
    /// Lowercased copy of `name` cached for fast filter matching.
    pub name_lower: String,
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// True if this entry is a symlink (determined via `symlink_metadata`).
    pub is_symlink: bool,
    /// True if the entry is a broken symlink (target doesn't exist). Only
    /// meaningful when `is_symlink` is true.
    pub is_broken_symlink: bool,
}

impl BrowseEntry {
    /// Construct a new entry, computing the lowercased name for filter matching.
    pub fn new(
        path: PathBuf,
        name: String,
        kind: EntryKind,
        size: u64,
        modified: Option<SystemTime>,
    ) -> Self {
        let name_lower = name.to_lowercase();
        Self {
            path,
            name,
            name_lower,
            kind,
            size,
            modified,
            is_symlink: false,
            is_broken_symlink: false,
        }
    }

    /// Construct a new entry with explicit symlink flags.
    pub fn new_with_symlink(
        path: PathBuf,
        name: String,
        kind: EntryKind,
        size: u64,
        modified: Option<SystemTime>,
        is_symlink: bool,
        is_broken_symlink: bool,
    ) -> Self {
        let name_lower = name.to_lowercase();
        Self {
            path,
            name,
            name_lower,
            kind,
            size,
            modified,
            is_symlink,
            is_broken_symlink,
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Directory | EntryKind::ParentDir)
    }

    pub fn is_audio(&self) -> bool {
        matches!(self.kind, EntryKind::AudioFile(_))
    }

    pub fn is_archive(&self) -> bool {
        matches!(self.kind, EntryKind::Archive)
    }

    /// Short type/format label for display in the type column.
    /// Audio files show their format (FLAC/MP3/etc), archives show "7z",
    /// directories show "dir", other files show their lowercase extension.
    /// Symlinks are prefixed with `↪`.
    pub fn type_label(&self) -> String {
        let base = match &self.kind {
            EntryKind::ParentDir => String::new(),
            EntryKind::Directory => "dir".to_string(),
            EntryKind::AudioFile(fmt) => fmt.name().to_string(),
            EntryKind::Archive => "7z".to_string(),
            EntryKind::OtherFile => self
                .path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default(),
        };
        if self.is_symlink {
            format!("↪{}", base)
        } else {
            base
        }
    }

    /// `YYYY-MM-DD` representation of the entry's modified time, or empty if unknown.
    pub fn date_label(&self) -> String {
        match self.modified {
            Some(t) => {
                let dt: chrono::DateTime<chrono::Local> = t.into();
                dt.format("%Y-%m-%d").to_string()
            }
            None => String::new(),
        }
    }
}

/// State for the browse screen
#[derive(Debug, Clone)]
pub struct BrowseState {
    pub current_dir: PathBuf,

    // ── Scan results (refreshed only by scan(), i.e. on cd) ─────────
    /// ParentDir entry, if `current_dir` has a parent. Always passed
    /// through view filtering unchanged.
    parent_entry: Option<BrowseEntry>,
    /// All directory entries from current_dir, unfiltered.
    all_dirs: Vec<BrowseEntry>,
    /// All file entries from current_dir, unfiltered (including hidden).
    all_files: Vec<BrowseEntry>,

    // ── View result (refilled by apply_view from scan results) ───────
    pub entries: Vec<BrowseEntry>,
    pub selected_index: usize,
    pub scroll_offset: usize,
    pub visible_height: usize,

    /// Multi-selected file paths
    pub multi_selected: Vec<PathBuf>,

    /// Anchor for range selection (Alt+click): the last plain-clicked entry.
    /// Path-based so it survives refresh/sort/filter. `None` when no anchor is set.
    pub multi_select_anchor: Option<PathBuf>,

    /// Filter input (when /-mode is active)
    pub filter_input: Option<TextInputState>,
    /// Committed filter text (empty = no filter)
    pub filter_text: String,
    /// Saved `filter_text` from before opening the input — used to restore on cancel.
    filter_text_prior: Option<String>,
    pub show_hidden: bool,

    /// Sort field and direction
    pub sort_by: SortBy,
    pub sort_dir: SortDir,

    /// Format filter (cycle with `f`)
    pub format_filter: FormatFilter,

    /// Probe cache: path → Some(info) if probed, None if probe failed
    pub probe_cache: HashMap<PathBuf, Option<Arc<CachedInfo>>>,

    /// Set of paths whose probe is currently in flight on a background task.
    /// Prevents duplicate spawns when the cursor moves rapidly.
    pub probe_pending: std::collections::HashSet<PathBuf>,

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
            parent_entry: None,
            all_dirs: Vec::new(),
            all_files: Vec::new(),
            entries: Vec::new(),
            selected_index: 0,
            scroll_offset: 0,
            visible_height: 0,
            multi_selected: Vec::new(),
            multi_select_anchor: None,
            filter_input: None,
            filter_text: String::new(),
            filter_text_prior: None,
            show_hidden: false,
            sort_by: SortBy::Name,
            sort_dir: SortDir::Asc,
            format_filter: FormatFilter::Off,
            probe_cache: HashMap::new(),
            probe_pending: std::collections::HashSet::new(),
            dir_stats_cache: HashMap::new(),
            return_target: BrowseReturnTarget::None,
            error: None,
        };
        state.refresh();
        state
    }

    /// Full refresh: re-scan disk, then re-apply the view filters/sort.
    pub fn refresh(&mut self) {
        self.scan();
        self.apply_view();
    }

    /// Read the directory from disk into `parent_entry` / `all_dirs` / `all_files`.
    /// Stores ALL entries (including hidden) — view-layer filters apply later.
    /// Slow; only call on cd or explicit refresh.
    fn scan(&mut self) {
        self.parent_entry = None;
        self.all_dirs.clear();
        self.all_files.clear();
        self.error = None;

        // Capture parent entry if not at root.
        if let Some(parent) = self.current_dir.parent() {
            self.parent_entry = Some(BrowseEntry::new(
                parent.to_path_buf(),
                "..".to_string(),
                EntryKind::ParentDir,
                0,
                None,
            ));
        }

        match fs::read_dir(&self.current_dir) {
            Ok(read) => {
                for entry in read.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();

                    // Use symlink_metadata to detect symlinks WITHOUT following them.
                    // For non-symlinks this returns the same data as metadata().
                    let symlink_meta = match fs::symlink_metadata(&path) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    let is_symlink = symlink_meta.file_type().is_symlink();

                    // For non-symlinks: use symlink_meta directly (it's the file).
                    // For symlinks: try to follow with metadata() to determine kind
                    // and broken-ness. If metadata() fails (broken link), the entry
                    // is rendered as a broken symlink.
                    let (metadata, is_broken_symlink) = if is_symlink {
                        match fs::metadata(&path) {
                            Ok(m) => (Some(m), false),
                            Err(_) => (None, true),
                        }
                    } else {
                        (Some(symlink_meta.clone()), false)
                    };

                    // Use the followed metadata for size/modified/kind when valid;
                    // otherwise fall back to the symlink's own data.
                    let effective = metadata.as_ref().unwrap_or(&symlink_meta);
                    let size = effective.len();
                    let modified = effective.modified().ok();

                    let kind = if is_broken_symlink {
                        EntryKind::OtherFile // broken symlink → treat as plain
                    } else if effective.is_dir() {
                        EntryKind::Directory
                    } else {
                        classify_file(&path)
                    };

                    let browse_entry = BrowseEntry::new_with_symlink(
                        path,
                        name,
                        kind.clone(),
                        size,
                        modified,
                        is_symlink,
                        is_broken_symlink,
                    );

                    if matches!(kind, EntryKind::Directory) {
                        self.all_dirs.push(browse_entry);
                    } else {
                        self.all_files.push(browse_entry);
                    }
                }
            }
            Err(e) => {
                self.error = Some(format!("Cannot read directory: {}", e));
            }
        }
    }

    /// Rebuild `entries` from the cached scan results, applying:
    /// - hidden filter (`show_hidden`)
    /// - format filter (`format_filter`)
    /// - text filter (`filter_text`, case-insensitive substring on `name_lower`)
    /// Then sorting dirs and files independently (dirs-first invariant).
    /// ParentDir is always first and never filtered.
    fn apply_view(&mut self) {
        self.entries.clear();

        // Lowercase the filter text once per view application.
        let filter_lower_owned = if self.filter_text.is_empty() {
            None
        } else {
            Some(self.filter_text.to_lowercase())
        };
        let filter_lower = filter_lower_owned.as_deref();

        // Parent entry always present (if scan found one), never filtered.
        if let Some(parent) = &self.parent_entry {
            self.entries.push(parent.clone());
        }

        let mut dirs: Vec<BrowseEntry> = self
            .all_dirs
            .iter()
            .filter(|e| entry_passes_view(e, self.show_hidden, &self.format_filter, filter_lower))
            .cloned()
            .collect();
        let mut files: Vec<BrowseEntry> = self
            .all_files
            .iter()
            .filter(|e| entry_passes_view(e, self.show_hidden, &self.format_filter, filter_lower))
            .cloned()
            .collect();

        sort_entries(&mut dirs, self.sort_by, self.sort_dir);
        sort_entries(&mut files, self.sort_by, self.sort_dir);

        self.entries.extend(dirs);
        self.entries.extend(files);

        // Clamp selection (cursor preservation is the caller's responsibility).
        if self.selected_index >= self.entries.len() {
            self.selected_index = self.entries.len().saturating_sub(1);
        }
        self.scroll_offset = 0;
    }

    /// Apply the view layer while keeping the cursor on the same entry path
    /// (or clamping if it's been filtered out).
    fn apply_view_preserving_cursor(&mut self) {
        let prev_path = self.entries.get(self.selected_index).map(|e| e.path.clone());
        self.apply_view();
        self.restore_cursor_on_path(prev_path);
    }

    /// Cycle to the next sort field and re-apply, preserving cursor on current entry
    pub fn cycle_sort_by(&mut self) {
        self.sort_by = self.sort_by.next();
        self.apply_view_preserving_cursor();
    }

    /// Toggle sort direction and re-apply, preserving cursor on current entry
    pub fn toggle_sort_dir(&mut self) {
        self.sort_dir = self.sort_dir.toggle();
        self.apply_view_preserving_cursor();
    }

    /// Set sort field and direction explicitly, preserving cursor
    pub fn set_sort(&mut self, by: SortBy, dir: SortDir) {
        self.sort_by = by;
        self.sort_dir = dir;
        self.apply_view_preserving_cursor();
    }

    /// Cycle to the next format filter and re-apply, preserving cursor if possible
    pub fn cycle_format_filter(&mut self) {
        self.format_filter = self.format_filter.next();
        self.apply_view_preserving_cursor();
    }

    /// Set format filter explicitly, preserving cursor
    pub fn set_format_filter(&mut self, filter: FormatFilter) {
        self.format_filter = filter;
        self.apply_view_preserving_cursor();
    }

    /// After a refresh, try to reposition the cursor on the entry with the given path.
    /// If the entry no longer exists (e.g., filtered out), leave cursor at current index.
    fn restore_cursor_on_path(&mut self, path: Option<PathBuf>) {
        if let Some(p) = path {
            if let Some(idx) = self.entries.iter().position(|e| e.path == p) {
                self.selected_index = idx;
                self.ensure_visible();
            }
        }
    }

    /// Enter a directory (or the parent if index points to `..`)
    pub fn enter_selected(&mut self) -> bool {
        if let Some(entry) = self.entries.get(self.selected_index) {
            if entry.is_dir() {
                self.current_dir = entry.path.clone();
                self.selected_index = 0;
                self.reset_nav_state();
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
            self.reset_nav_state();
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
            self.reset_nav_state();
            self.refresh();
        }
    }

    /// Navigate to a path expressed as a string. Resolves `~` and relative paths
    /// against `current_dir`. Returns Err with a user-friendly message on failure.
    ///
    /// Supported tilde forms: bare `~` and `~/foo`. The `~user` form (per-user
    /// home directory) is NOT supported and is rejected with a clear error
    /// rather than silently mangled into an invalid path.
    pub fn navigate_to_str(&mut self, input: &str) -> Result<(), String> {
        // Tilde expansion: only `~` and `~/...` forms.
        let expanded = if input == "~" {
            std::env::var("HOME").map_err(|_| "HOME not set".to_string())?
        } else if let Some(rest) = input.strip_prefix("~/") {
            let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
            format!("{}/{}", home, rest)
        } else if input.starts_with('~') {
            return Err("~user paths are not supported (use ~/...)".to_string());
        } else {
            input.to_string()
        };

        // Relative paths resolve against current_dir
        let candidate = PathBuf::from(&expanded);
        let resolved = if candidate.is_absolute() {
            candidate
        } else {
            self.current_dir.join(candidate)
        };

        // Canonicalize if possible; fall back to raw resolved on failure
        let final_path = resolved.canonicalize().unwrap_or_else(|_| resolved.clone());

        if !final_path.is_dir() {
            return Err(format!("not a directory: {}", final_path.display()));
        }

        self.current_dir = final_path;
        self.selected_index = 0;
        self.reset_nav_state();
        self.refresh();
        Ok(())
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

    /// Scroll the viewport by `delta` rows without moving the cursor.
    /// Positive delta scrolls down; negative scrolls up. Clamped to valid range.
    pub fn scroll_viewport(&mut self, delta: i32) {
        if self.visible_height == 0 || self.entries.is_empty() {
            return;
        }
        let max_offset = self.entries.len().saturating_sub(self.visible_height);
        let new_offset = (self.scroll_offset as i32 + delta)
            .max(0)
            .min(max_offset as i32) as usize;
        self.scroll_offset = new_offset;
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
        // Hidden files were captured by scan(); just re-apply the view layer.
        self.apply_view_preserving_cursor();
    }

    // ── Text filter (live, case-insensitive substring on entry name) ─

    /// Open the filter input, seeded with the current `filter_text` so it
    /// can be edited. Saves the prior `filter_text` for cancellation.
    pub fn open_filter_input(&mut self) {
        self.filter_text_prior = Some(self.filter_text.clone());
        self.filter_input = Some(TextInputState::new(self.filter_text.clone()));
    }

    /// Sync `filter_text` from the open input and re-apply the view.
    /// No-op if no input is active.
    pub fn update_filter_from_input(&mut self) {
        if let Some(input) = &self.filter_input {
            self.filter_text = input.text.clone();
            self.apply_view_preserving_cursor();
        }
    }

    /// Close the filter input. If `commit`, keep `filter_text` as-is and drop
    /// the saved prior value. If `!commit`, restore the prior `filter_text`.
    pub fn close_filter_input(&mut self, commit: bool) {
        self.filter_input = None;
        if commit {
            self.filter_text_prior = None;
        } else {
            let prior = self.filter_text_prior.take().unwrap_or_default();
            if prior != self.filter_text {
                self.filter_text = prior;
                self.apply_view_preserving_cursor();
            }
        }
    }

    /// Drop all filter state and re-apply the view.
    pub fn clear_filter(&mut self) {
        self.reset_filter_state();
        self.apply_view_preserving_cursor();
    }

    /// Reset filter state without re-applying the view (used by navigation
    /// methods that will refresh anyway).
    fn reset_filter_state(&mut self) {
        self.filter_text.clear();
        self.filter_input = None;
        self.filter_text_prior = None;
    }

    /// Reset filter state AND clear the multi-select anchor, used by navigation
    /// methods. The anchor is for range-select (Alt+click) and is a
    /// per-directory context.
    fn reset_nav_state(&mut self) {
        self.reset_filter_state();
        self.multi_select_anchor = None;
    }

    /// Resolve the range-select anchor to an index in the current `entries` vec.
    /// Returns the anchor's current index if its path is still present, otherwise
    /// falls back to the current cursor (`selected_index`). Useful when the
    /// anchor path has been filtered out or removed since it was set.
    pub fn resolve_anchor_index(&self) -> usize {
        if let Some(anchor_path) = &self.multi_select_anchor {
            if let Some(idx) = self.entries.iter().position(|e| e.path == *anchor_path) {
                return idx;
            }
        }
        self.selected_index
    }

    /// Probe the currently selected entry (audio files only) on a background
    /// tokio task. The result arrives via `AppMessage::AudioProbeComplete`,
    /// which the event loop handles by inserting into `probe_cache`.
    ///
    /// No-op if the entry is already in the cache OR if a probe for the same
    /// path is already in flight.
    ///
    /// Directory stats are NOT computed here — see `compute_dir_stats_current`
    /// for an explicit, deferred-execution entry point.
    pub fn probe_current(&mut self, tx: &tokio::sync::mpsc::Sender<super::message::AppMessage>) {
        let path = match self.entries.get(self.selected_index) {
            Some(entry) if entry.is_audio() => entry.path.clone(),
            _ => return,
        };

        if self.probe_cache.contains_key(&path) {
            return; // already probed (successfully or not)
        }
        if self.probe_pending.contains(&path) {
            return; // probe already in flight
        }

        self.probe_pending.insert(path.clone());
        spawn_audio_probe(path, tx.clone());
    }

    /// Compute directory stats for the currently-selected entry, if it's a
    /// directory. SLOW on large directories — never call from an interactive
    /// event handler; reserved for deferred/background execution.
    #[allow(dead_code)]
    pub fn compute_dir_stats_current(&mut self) {
        if let Some(entry) = self.entries.get(self.selected_index) {
            if entry.is_dir() && !matches!(entry.kind, EntryKind::ParentDir) {
                if !self.dir_stats_cache.contains_key(&entry.path) {
                    let path = entry.path.clone();
                    let stats = compute_dir_stats(&path);
                    self.dir_stats_cache.insert(path, Arc::new(stats));
                }
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

/// Spawn a background tokio task that probes the audio file at `path` and
/// sends the result back to the main loop via `AudioProbeComplete`. The
/// blocking probe (`probe_audio` + `read_metadata`) runs on `spawn_blocking`
/// so it doesn't tie up an async worker thread.
pub fn spawn_audio_probe(
    path: PathBuf,
    tx: tokio::sync::mpsc::Sender<super::message::AppMessage>,
) {
    tokio::spawn(async move {
        let path_for_task = path.clone();
        let result: Result<CachedInfo, String> = tokio::task::spawn_blocking(move || {
            let source = crate::tui::probe::probe_audio(&path_for_task)
                .map_err(|e| format!("{}", e))?;
            let metadata = crate::tui::probe::read_metadata(&path_for_task).unwrap_or_default();
            Ok(CachedInfo { source, metadata })
        })
        .await
        .unwrap_or_else(|join_err| Err(format!("probe task panicked: {}", join_err)));

        let _ = tx
            .send(super::message::AppMessage::AudioProbeComplete {
                path,
                result: Box::new(result),
            })
            .await;
    });
}

/// View-layer filter check: returns true if the entry passes the hidden,
/// format, and text filters. Pure function — no state captured, easy to test.
fn entry_passes_view(
    entry: &BrowseEntry,
    show_hidden: bool,
    format_filter: &FormatFilter,
    filter_lower: Option<&str>,
) -> bool {
    // Hidden filter
    if !show_hidden && entry.name.starts_with('.') {
        return false;
    }
    // Format filter (only applies to non-directory entries)
    if !matches!(entry.kind, EntryKind::Directory)
        && !format_filter.allows(&entry.kind)
    {
        return false;
    }
    // Text filter (case-insensitive substring)
    if let Some(needle) = filter_lower {
        if !entry.name_lower.contains(needle) {
            return false;
        }
    }
    true
}

/// Sort a vec of entries by the given field and direction
fn sort_entries(entries: &mut [BrowseEntry], by: SortBy, dir: SortDir) {
    use std::cmp::Ordering;

    entries.sort_by(|a, b| {
        let ord = match by {
            SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortBy::Date => match (a.modified, b.modified) {
                (Some(at), Some(bt)) => at.cmp(&bt),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
            SortBy::Size => a.size.cmp(&b.size),
            SortBy::Type => {
                // Sort by kind first (audio formats grouped), then by name within group
                let a_rank = entry_type_rank(&a.kind);
                let b_rank = entry_type_rank(&b.kind);
                a_rank
                    .cmp(&b_rank)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            }
        };
        match dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
        }
    });
}

/// Numeric rank for type sorting: audio files grouped by format, then archive, then other
fn entry_type_rank(kind: &EntryKind) -> u8 {
    match kind {
        EntryKind::ParentDir => 0,
        EntryKind::Directory => 1,
        EntryKind::AudioFile(AudioFormat::Flac) => 10,
        EntryKind::AudioFile(AudioFormat::Wav) => 11,
        EntryKind::AudioFile(AudioFormat::Aiff) => 12,
        EntryKind::AudioFile(AudioFormat::WavPack) => 13,
        EntryKind::AudioFile(AudioFormat::Alac) => 14,
        EntryKind::AudioFile(AudioFormat::Mp3) => 15,
        EntryKind::AudioFile(AudioFormat::Aac) => 16,
        EntryKind::AudioFile(AudioFormat::Opus) => 17,
        EntryKind::Archive => 20,
        EntryKind::OtherFile => 30,
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
