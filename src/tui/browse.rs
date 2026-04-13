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
    /// Audio files show their format (FLAC/MP3/etc), archives show their
    /// format (7z/zip/rar/tar.gz/etc), directories show "dir", other
    /// files show their lowercase extension. Symlinks are prefixed with `↪`.
    pub fn type_label(&self) -> String {
        let base = match &self.kind {
            EntryKind::ParentDir => String::new(),
            EntryKind::Directory => "dir".to_string(),
            EntryKind::AudioFile(fmt) => fmt.name().to_string(),
            EntryKind::Archive => archive_label(&self.path),
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
    pub(super) parent_entry: Option<BrowseEntry>,
    /// All directory entries from current_dir, unfiltered.
    pub(super) all_dirs: Vec<BrowseEntry>,
    /// All file entries from current_dir, unfiltered (including hidden).
    pub(super) all_files: Vec<BrowseEntry>,

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

    /// Set of directory paths whose stats are currently being computed on
    /// a background task. Prevents duplicate spawns.
    pub dir_stats_pending: std::collections::HashSet<PathBuf>,

    /// Where to send selected files
    pub return_target: BrowseReturnTarget,

    /// Error message from last directory read, if any
    pub error: Option<String>,

    /// When set, we're browsing inside an archive rather than the filesystem.
    pub archive: Option<ArchiveBrowseState>,

    /// Handle to the in-flight async directory scan. `Some` while a background
    /// scan is running. Used for cancellation and loading indicator.
    pub scan_pending: Option<ScanHandle>,

    /// After `go_parent`, the name of the directory we came from — so the
    /// DirScanComplete handler can position the cursor on it.
    pub cursor_restore_target: Option<String>,

    /// Channel sender for async messages. Set after construction by the
    /// event loop. `None` during the initial synchronous scan.
    scan_tx: Option<tokio::sync::mpsc::Sender<super::message::AppMessage>>,
}

/// Handle to a cancellable background directory scan.
#[derive(Debug, Clone)]
pub struct ScanHandle {
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ScanHandle {
    pub fn new() -> (Self, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        (Self { cancel: flag.clone() }, flag)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// State for browsing inside an archive.
#[derive(Debug, Clone)]
pub struct ArchiveBrowseState {
    /// The parsed archive listing.
    pub listing: super::archive_listing::ArchiveListing,
    /// Current directory path inside the archive ("" = root).
    pub inner_path: String,
    /// Password used to open this archive (for re-listing / extraction).
    pub password: Option<String>,
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
            dir_stats_pending: std::collections::HashSet::new(),
            return_target: BrowseReturnTarget::None,
            error: None,
            archive: None,
            scan_pending: None,
            cursor_restore_target: None,
            scan_tx: None,
        };
        state.refresh(); // Initial scan is synchronous (no tx yet).
        state
    }

    /// Set the message channel sender (called once from the event loop).
    pub fn set_tx(&mut self, tx: tokio::sync::mpsc::Sender<super::message::AppMessage>) {
        self.scan_tx = Some(tx);
    }

    /// Whether async scanning is enabled (tx has been set).
    pub fn is_async_enabled(&self) -> bool {
        self.scan_tx.is_some()
    }

    /// Full refresh: re-scan disk, then re-apply the view filters/sort.
    /// Uses async scan if tx is available, otherwise falls back to synchronous.
    pub fn refresh(&mut self) {
        if self.archive.is_some() {
            self.refresh_archive_view();
            return;
        }
        if self.scan_tx.is_some() {
            self.begin_async_scan();
        } else {
            // Synchronous fallback (initial scan before tx is set).
            self.scan();
            self.apply_view();
        }
    }

    /// Start an async directory scan. Cancels any in-flight scan first.
    /// Clears entries immediately (renderer shows "Loading...").
    fn begin_async_scan(&mut self) {
        // Cancel previous scan if still running.
        if let Some(handle) = self.scan_pending.take() {
            handle.cancel();
        }

        // Clear display state.
        self.parent_entry = None;
        self.all_dirs.clear();
        self.all_files.clear();
        self.entries.clear();
        self.error = None;
        self.selected_index = 0;
        self.scroll_offset = 0;

        let tx = match &self.scan_tx {
            Some(tx) => tx.clone(),
            None => return, // No channel — shouldn't happen after set_tx.
        };

        let (handle, cancel_flag) = ScanHandle::new();
        self.scan_pending = Some(handle);

        spawn_dir_scan(self.current_dir.clone(), cancel_flag, tx);
    }

    /// Whether we're currently browsing inside an archive.
    pub fn is_in_archive(&self) -> bool {
        self.archive.is_some()
    }

    /// Enter an archive: set archive state and populate entries from listing.
    pub fn enter_archive(
        &mut self,
        listing: super::archive_listing::ArchiveListing,
        password: Option<String>,
    ) {
        self.archive = Some(ArchiveBrowseState {
            listing,
            inner_path: String::new(),
            password,
        });
        self.multi_selected.clear();
        self.refresh_archive_view();
    }

    /// Exit the archive and return to filesystem browsing.
    pub fn exit_archive(&mut self) {
        self.archive = None;
        self.multi_selected.clear();
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.refresh();
    }

    /// Navigate into a subdirectory inside the archive.
    pub fn enter_archive_dir(&mut self, dir_path: &str) {
        if let Some(ref mut arc) = self.archive {
            arc.inner_path = dir_path.to_string();
        }
        self.multi_selected.clear();
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.refresh_archive_view();
    }

    /// Navigate up one level inside the archive. Returns false if already
    /// at archive root (caller should exit the archive entirely).
    pub fn go_up_in_archive(&mut self) -> bool {
        if let Some(ref mut arc) = self.archive {
            if arc.inner_path.is_empty() {
                return false; // At root — caller should exit archive.
            }
            // Go to parent directory inside archive.
            arc.inner_path = match arc.inner_path.rfind('/') {
                Some(pos) => arc.inner_path[..pos].to_string(),
                None => String::new(),
            };
            self.multi_selected.clear();
            self.selected_index = 0;
            self.scroll_offset = 0;
            self.refresh_archive_view();
            return true;
        }
        false
    }

    /// Repopulate `entries` from the archive listing at the current inner path.
    fn refresh_archive_view(&mut self) {
        self.entries.clear();
        self.parent_entry = None;

        let arc = match &self.archive {
            Some(a) => a,
            None => return,
        };

        // Add parent-dir entry.
        self.parent_entry = Some(BrowseEntry::new(
            PathBuf::from(".."),
            "..".to_string(),
            EntryKind::ParentDir,
            0,
            None,
        ));

        let items = arc.listing.entries_at(&arc.inner_path);

        // Convert ArchiveListItems to BrowseEntries.
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for item in &items {
            let kind = if item.is_dir {
                EntryKind::Directory
            } else {
                classify_file(Path::new(&item.name))
            };
            let entry = BrowseEntry::new(
                arc.listing.archive_path.join(&item.full_path),
                item.name.clone(),
                kind,
                item.size,
                None,
            );
            if item.is_dir {
                dirs.push(entry);
            } else {
                files.push(entry);
            }
        }

        // Build entries: parent + dirs + files (same order as filesystem browse).
        if let Some(ref parent) = self.parent_entry {
            self.entries.push(parent.clone());
        }
        self.entries.extend(dirs);
        self.entries.extend(files);

        // Clamp selection.
        if self.selected_index >= self.entries.len() && !self.entries.is_empty() {
            self.selected_index = self.entries.len() - 1;
        }
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
    pub(super) fn apply_view(&mut self) {
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
            // Remember the directory we came from for cursor restoration
            // after the async scan completes.
            self.cursor_restore_target = self
                .current_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string());
            self.current_dir = parent.to_path_buf();
            self.reset_nav_state();
            self.refresh();
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
        // Tilde expansion (no I/O — just env var lookup).
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

        // Relative path resolution (no I/O — just path joining).
        let candidate = PathBuf::from(&expanded);
        let resolved = if candidate.is_absolute() {
            candidate
        } else {
            self.current_dir.join(candidate)
        };

        // If we have a tx, do the blocking canonicalize + is_dir check
        // asynchronously. Otherwise fall back to synchronous.
        if let Some(tx) = &self.scan_tx {
            let tx = tx.clone();
            let input_str = input.to_string();
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    let final_path = resolved.canonicalize()
                        .unwrap_or_else(|_| resolved.clone());
                    if final_path.is_dir() {
                        Ok(final_path)
                    } else {
                        Err(format!("not a directory: {}", final_path.display()))
                    }
                })
                .await
                .unwrap_or_else(|e| Err(format!("path check failed: {}", e)));

                let _ = tx.send(super::message::AppMessage::PathValidationComplete {
                    input: input_str,
                    result,
                }).await;
            });
            Ok(()) // The actual navigation happens when the result arrives.
        } else {
            // Synchronous fallback.
            let final_path = resolved.canonicalize()
                .unwrap_or_else(|_| resolved.clone());
            if !final_path.is_dir() {
                return Err(format!("not a directory: {}", final_path.display()));
            }
            self.current_dir = final_path;
            self.selected_index = 0;
            self.reset_nav_state();
            self.refresh();
            Ok(())
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
            // Allow selecting audio files, archives, directories, and
            // other files. Only ParentDir (..) is excluded — it's a
            // navigation pseudo-entry, not a real target.
            if !matches!(entry.kind, EntryKind::ParentDir) {
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

    /// Collect paths for an enqueue operation (`:queue` / `:convert` etc).
    ///
    /// - If `multi_selected` is non-empty, expands any directories into
    ///   their audio file contents (recursively) and returns the result.
    /// - Otherwise, if the cursor is on an audio file, archive, or
    ///   directory, returns it (directories expanded).
    /// - Returns an empty vec if nothing valid is selected.
    ///
    /// The expansion helper (`expand_paths_to_audio`) is screen-agnostic
    /// so Library and future screens can reuse the same logic.
    pub fn collect_selection_for_queue(&self) -> Vec<PathBuf> {
        if !self.multi_selected.is_empty() {
            return expand_paths_to_audio(&self.multi_selected);
        }
        if let Some(entry) = self.selected_entry() {
            match &entry.kind {
                EntryKind::AudioFile(_) | EntryKind::Archive => {
                    return vec![entry.path.clone()];
                }
                EntryKind::Directory => {
                    return expand_paths_to_audio(&[entry.path.clone()]);
                }
                _ => {}
            }
        }
        Vec::new()
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

    /// Kick off background lookup for the currently-selected entry:
    /// - Audio files → `spawn_audio_probe` (lofty + ffmpeg metadata read)
    /// - Subdirectories (not ParentDir) → `spawn_dir_stats` (file count + total size)
    /// - Other kinds → no-op
    ///
    /// Results arrive via `AppMessage::AudioProbeComplete` or
    /// `AppMessage::DirStatsComplete` and the event loop populates the
    /// respective caches. Pending sets prevent duplicate spawns when the
    /// cursor moves rapidly back and forth.
    pub fn probe_current(&mut self, tx: &tokio::sync::mpsc::Sender<super::message::AppMessage>) {
        self.probe_current_with_db(tx, None);
    }

    /// Probe the current selection, checking the SQLite cache first.
    /// If the DB has a valid cached probe (matching mtime + size), populates
    /// the in-memory cache directly and skips the async probe.
    pub fn probe_current_with_db(
        &mut self,
        tx: &tokio::sync::mpsc::Sender<super::message::AppMessage>,
        db: Option<&crate::db::Database>,
    ) {
        let entry = match self.entries.get(self.selected_index) {
            Some(e) => e,
            None => return,
        };

        if entry.is_audio() {
            let path = entry.path.clone();
            if self.probe_cache.contains_key(&path) || self.probe_pending.contains(&path) {
                return;
            }

            // Check SQLite probe cache before spawning an async probe.
            if let Some(db) = db {
                if let Some(mtime) = entry.modified {
                    let mtime_unix = crate::db::systemtime_to_unix(mtime);
                    if let Some(row) = db.get_cached_probe(
                        &path.display().to_string(),
                        mtime_unix,
                        entry.size,
                    ) {
                        if let Some(info) = row.to_cached_info(entry.size) {
                            self.probe_cache
                                .insert(path, Some(std::sync::Arc::new(info)));
                            return;
                        }
                    }
                }
            }

            self.probe_pending.insert(path.clone());
            spawn_audio_probe(path, tx.clone());
        } else if entry.is_dir() && !matches!(entry.kind, EntryKind::ParentDir) {
            let path = entry.path.clone();
            if self.dir_stats_cache.contains_key(&path)
                || self.dir_stats_pending.contains(&path)
            {
                return;
            }
            self.dir_stats_pending.insert(path.clone());
            spawn_dir_stats(path, tx.clone());
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

/// Spawn a background tokio task that computes directory stats for `path`
/// (file count, audio file count, total size) and sends the result back via
/// `DirStatsComplete`. The blocking `fs::read_dir` + per-entry stat loop
/// runs on `spawn_blocking` so it doesn't tie up an async worker thread —
/// the original sync version was the source of the Phase 4d UI freeze on
/// large directories like ~/Downloads.
pub fn spawn_dir_stats(
    path: PathBuf,
    tx: tokio::sync::mpsc::Sender<super::message::AppMessage>,
) {
    tokio::spawn(async move {
        let path_for_task = path.clone();
        let stats = tokio::task::spawn_blocking(move || compute_dir_stats(&path_for_task))
            .await
            .unwrap_or_default();

        let _ = tx
            .send(super::message::AppMessage::DirStatsComplete { path, stats })
            .await;
    });
}

/// Spawn a background directory scan. The blocking I/O (readdir + lstat per
/// entry) runs on `spawn_blocking`. Respects the cancel flag — checks every
/// 50 entries and aborts early if set. Sends `DirScanComplete` when done.
/// Wrapped in a 30-second timeout.
pub fn spawn_dir_scan(
    path: PathBuf,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tx: tokio::sync::mpsc::Sender<super::message::AppMessage>,
) {
    tokio::spawn(async move {
        let scan_path = path.clone();
        let cancel_flag = cancel.clone();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::task::spawn_blocking(move || {
                scan_directory_blocking(&scan_path, &cancel_flag)
            }),
        )
        .await;

        let (parent_entry, dirs, files, error) = match result {
            Ok(Ok(Ok((parent, dirs, files)))) => (parent, dirs, files, None),
            Ok(Ok(Err(e))) => (None, Vec::new(), Vec::new(), Some(e)),
            Ok(Err(join_err)) => {
                (None, Vec::new(), Vec::new(), Some(format!("scan task panicked: {}", join_err)))
            }
            Err(_timeout) => {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                (None, Vec::new(), Vec::new(), Some("scan timed out (30s)".into()))
            }
        };

        let _ = tx
            .send(super::message::AppMessage::DirScanComplete {
                path,
                parent_entry,
                dirs,
                files,
                error,
            })
            .await;
    });
}

/// Blocking directory scan — runs on a `spawn_blocking` thread.
/// Returns (parent_entry, dirs, files) or an error string.
fn scan_directory_blocking(
    dir: &Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(Option<BrowseEntry>, Vec<BrowseEntry>, Vec<BrowseEntry>), String> {
    use std::sync::atomic::Ordering;

    let parent_entry = dir.parent().map(|parent| {
        BrowseEntry::new(
            parent.to_path_buf(),
            "..".to_string(),
            EntryKind::ParentDir,
            0,
            None,
        )
    });

    let read = fs::read_dir(dir)
        .map_err(|e| format!("Cannot read directory: {}", e))?;

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for (i, entry) in read.flatten().enumerate() {
        // Check cancellation every 50 entries.
        if i % 50 == 0 && cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }

        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        let symlink_meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_symlink = symlink_meta.file_type().is_symlink();

        let (metadata, is_broken_symlink) = if is_symlink {
            match fs::metadata(&path) {
                Ok(m) => (Some(m), false),
                Err(_) => (None, true),
            }
        } else {
            (Some(symlink_meta.clone()), false)
        };

        let effective = metadata.as_ref().unwrap_or(&symlink_meta);
        let size = effective.len();
        let modified = effective.modified().ok();

        let kind = if is_broken_symlink {
            EntryKind::OtherFile
        } else if effective.is_dir() {
            EntryKind::Directory
        } else {
            classify_file(&path)
        };

        let browse_entry = BrowseEntry::new_with_symlink(
            path, name, kind.clone(), size, modified, is_symlink, is_broken_symlink,
        );

        if matches!(kind, EntryKind::Directory) {
            dirs.push(browse_entry);
        } else {
            files.push(browse_entry);
        }
    }

    Ok((parent_entry, dirs, files))
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

/// Compute stats for a directory: total file count, audio count, total size.
/// Walks recursively into all subdirectories. Symlinks are skipped (avoids
/// loops). Bounded by `MAX_WALK_DEPTH` and `MAX_WALK_FILES` to prevent
/// runaway computation on huge trees. Always called from a background task.
fn compute_dir_stats(path: &Path) -> DirStats {
    const MAX_WALK_DEPTH: u32 = 20;
    const MAX_WALK_FILES: usize = 1_000_000;

    let mut stats = DirStats::default();
    walk_dir_for_stats(path, &mut stats, 0, MAX_WALK_DEPTH, MAX_WALK_FILES);
    stats
}

/// Recursive helper for `compute_dir_stats`. Stops descending when:
/// - depth reaches `max_depth`
/// - file_count reaches `max_files`
/// - the directory can't be read
/// Symlinks are detected via `entry.file_type()` (which doesn't follow them)
/// and skipped entirely to prevent infinite loops on cyclic symlinks.
fn walk_dir_for_stats(
    path: &Path,
    stats: &mut DirStats,
    depth: u32,
    max_depth: u32,
    max_files: usize,
) {
    if depth >= max_depth || stats.file_count >= max_files {
        return;
    }
    let read = match fs::read_dir(path) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in read.flatten() {
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue; // skip symlinks (could be loops)
        }
        if file_type.is_file() {
            if let Ok(meta) = entry.metadata() {
                stats.file_count += 1;
                stats.total_size += meta.len();
                if matches!(classify_file(&entry.path()), EntryKind::AudioFile(_)) {
                    stats.audio_count += 1;
                }
                if stats.file_count >= max_files {
                    return;
                }
            }
        } else if file_type.is_dir() {
            walk_dir_for_stats(
                &entry.path(),
                stats,
                depth + 1,
                max_depth,
                max_files,
            );
            if stats.file_count >= max_files {
                return;
            }
        }
    }
}

impl Default for BrowseState {
    fn default() -> Self {
        Self::new()
    }
}

/// Classify a file by its extension
/// Expand a list of paths into audio files suitable for queuing.
/// - Audio files and archives are kept as-is.
/// - Directories are walked recursively; audio files within are collected.
/// - Non-audio files and unreadable entries are silently skipped.
///
/// Public and screen-agnostic — usable by Browse, Library, or any
/// future screen that needs to queue directories or mixed selections.
pub fn expand_paths_to_audio(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for path in paths {
        if path.is_dir() {
            collect_audio_recursive(path, &mut result);
        } else {
            let kind = classify_file(path);
            if matches!(kind, EntryKind::AudioFile(_) | EntryKind::Archive) {
                result.push(path.clone());
            }
        }
    }
    result
}

/// Recursively walk a directory, pushing audio files and archives into
/// `out`. Follows the same extension classification as `classify_file`
/// to stay consistent with the browse listing. Symlinks are skipped to
/// avoid loops (same policy as `walk_dir_for_stats`).
fn collect_audio_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in read.flatten() {
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_audio_recursive(&path, out);
        } else {
            let kind = classify_file(&path);
            if matches!(kind, EntryKind::AudioFile(_) | EntryKind::Archive) {
                out.push(path);
            }
        }
    }
}

fn classify_file(path: &Path) -> EntryKind {
    // Check for double-extension archives first (e.g., .tar.gz).
    if is_tar_compound(path) {
        return EntryKind::Archive;
    }

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
        Some("7z") | Some("zip") | Some("rar") | Some("tar") | Some("iso")
        | Some("cab") | Some("dmg") | Some("tgz") | Some("tbz2") | Some("txz") => {
            EntryKind::Archive
        }
        _ => EntryKind::OtherFile,
    }
}

/// Derive a short display label for an archive from its extension.
fn archive_label(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_lowercase())
        .unwrap_or_default();
    // Check compound extensions first.
    if name.ends_with(".tar.gz") { return "tar.gz".into(); }
    if name.ends_with(".tar.bz2") { return "tar.bz2".into(); }
    if name.ends_with(".tar.xz") { return "tar.xz".into(); }
    if name.ends_with(".tar.zst") { return "tar.zst".into(); }
    if name.ends_with(".tar.lz") { return "tar.lz".into(); }
    if name.ends_with(".tar.lzma") { return "tar.lzma".into(); }
    // Single extension.
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_else(|| "archive".into())
}

/// Public accessor for compound tar check (used by keybindings file-routing).
pub fn is_tar_compound_pub(path: &Path) -> bool {
    is_tar_compound(path)
}

/// Check for compound tar extensions (.tar.gz, .tar.bz2, .tar.xz, .tar.zst).
/// `Path::extension()` only returns the last component, so "file.tar.gz"
/// gives "gz" which would be classified as OtherFile without this check.
fn is_tar_compound(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_lowercase())
        .unwrap_or_default();
    name.ends_with(".tar.gz")
        || name.ends_with(".tar.bz2")
        || name.ends_with(".tar.xz")
        || name.ends_with(".tar.zst")
        || name.ends_with(".tar.lz")
        || name.ends_with(".tar.lzma")
}
