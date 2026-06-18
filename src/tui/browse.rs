//! File browser state and directory scanning

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Type-ahead buffer resets after this duration of inactivity.
const TYPE_AHEAD_TIMEOUT: Duration = Duration::from_millis(1500);

use crate::convert::formats::AudioFormat;
use crate::tui::probe::{SourceInfo, SourceMetadata};
use crate::tui::disc_browser::{DiscProbeCacheEntry, DiscProbeFollowup};
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
    /// SACD ISO image (Super Audio CD). Detected via ScarletBook
    /// magic-byte probe at LSN 510/520/530, not by extension alone
    /// (some `.iso` files are DVD-V or generic ISO9660). Population
    /// happens in a post-scan upgrade pass keyed by (path, mtime)
    /// against `BrowseState.sacd_classify_cache`.
    SacdIso,
    /// DVD-Audio ISO image. Lightweight classification happens after scanning
    /// and is cached by path + mtime + len.
    DvdAudioIso,
    /// Filesystem DVD-Audio directory (contains AUDIO_TS/AUDIO_TS.IFO).
    DvdAudioDir,
    /// DVD-Video ISO image. Hybrid DVD-Audio/DVD-Video ISOs remain DVD-Audio.
    DvdVideoIso,
    /// Filesystem DVD-Video directory (contains VIDEO_TS/VIDEO_TS.IFO and no
    /// non-empty AUDIO_TS DVD-Audio root).
    DvdVideoDir,
    /// Any other file
    OtherFile,
}

/// Metadata fingerprint for bounded DVD-Audio classification caches.
/// Compared by len + mtime so unchanged ISOs are not re-probed.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassificationFingerprint {
    pub len: u64,
    pub modified: Option<std::time::SystemTime>,
}

impl ClassificationFingerprint {
    pub fn from_entry(entry: &BrowseEntry) -> Self {
        Self {
            len: entry.size,
            modified: entry.modified,
        }
    }
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
            Self::Only(AudioFormat::Aiff) => Self::Only(AudioFormat::Dsf),
            Self::Only(AudioFormat::Dsf) => Self::Only(AudioFormat::Dff),
            Self::Only(AudioFormat::Dff) => Self::Only(AudioFormat::Dts),
            Self::Only(AudioFormat::Dts) => Self::Only(AudioFormat::Ac3),
            Self::Only(AudioFormat::Ac3) => Self::Only(AudioFormat::Ape),
            Self::Only(AudioFormat::Ape) => Self::Only(AudioFormat::Lpcm),
            Self::Only(AudioFormat::Lpcm) => Self::Off,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Off => "off".to_string(),
            Self::AudioOnly => "audio".to_string(),
            Self::Only(fmt) => fmt.name().to_string(),
        }
    }

    /// Whether a given entry kind passes the filter when only the kind is known.
    /// Prefer `allows_entry` when the path is available so convertible control
    /// files such as `.cue` can participate in the audio filter without being
    /// misclassified as audio bytes.
    pub fn allows(&self, kind: &EntryKind) -> bool {
        match self {
            Self::Off => true,
            Self::AudioOnly => matches!(kind, EntryKind::AudioFile(_) | EntryKind::SacdIso | EntryKind::DvdAudioIso | EntryKind::DvdAudioDir | EntryKind::DvdVideoIso | EntryKind::DvdVideoDir),
            Self::Only(fmt) => matches!(kind, EntryKind::AudioFile(f) if f == fmt),
        }
    }

    /// Whether a concrete browse entry passes the filter.
    ///
    /// `.cue` files are not audio, but they are valid conversion sources: the
    /// pipeline materializes the referenced image(s) from the CUE sheet. Keep
    /// them visible under the AudioOnly filter so the right-click Convert action
    /// that exists for `OtherFile` entries is actually reachable.
    pub fn allows_entry(&self, entry: &BrowseEntry) -> bool {
        match self {
            Self::Off => true,
            Self::AudioOnly => {
                matches!(entry.kind, EntryKind::AudioFile(_) | EntryKind::SacdIso | EntryKind::DvdAudioIso | EntryKind::DvdAudioDir | EntryKind::DvdVideoIso | EntryKind::DvdVideoDir)
                    || is_cue_sheet_path(&entry.path)
            }
            Self::Only(fmt) => matches!(entry.kind, EntryKind::AudioFile(f) if f == *fmt),
        }
    }
}

/// Search mode: what to match the query against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchMode {
    Filename,
    Tags,
    Both,
}

impl SearchMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Filename => "filename",
            Self::Tags => "tags",
            Self::Both => "both",
        }
    }
    pub fn cycle(&self) -> Self {
        match self {
            Self::Filename => Self::Tags,
            Self::Tags => Self::Both,
            Self::Both => Self::Filename,
        }
    }
}

/// Sort field for search results.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchSort {
    Score,
    Name,
    Date,
    Size,
    Extension,
    Artist,
    Album,
    Year,
    Title,
}

impl SearchSort {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Score => "relevance",
            Self::Name => "name",
            Self::Date => "date",
            Self::Size => "size",
            Self::Extension => "ext",
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Year => "year",
            Self::Title => "title",
        }
    }

    /// Cycle to next sort. `tag_mode` controls whether tag-based sorts
    /// are included (only when search mode is Tags or Both).
    pub fn cycle_with_mode(&self, tag_mode: bool) -> Self {
        match self {
            Self::Score => Self::Name,
            Self::Name => Self::Date,
            Self::Date => Self::Size,
            Self::Size => Self::Extension,
            Self::Extension => {
                if tag_mode {
                    Self::Artist
                } else {
                    Self::Score
                }
            }
            Self::Artist => Self::Album,
            Self::Album => Self::Year,
            Self::Year => Self::Title,
            Self::Title => Self::Score,
        }
    }

    /// True if this sort requires tag data.
    pub fn is_tag_sort(&self) -> bool {
        matches!(self, Self::Artist | Self::Album | Self::Year | Self::Title)
    }
}

/// Which element of the search panel has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchFocus {
    Input,
    Recursive,
    Mode,
    Sort,
    AudioOnly,
    /// Focus is on the results list — normal browse keys work.
    Results,
}

/// State for the inline search panel in the browse screen.
#[derive(Debug, Clone)]
pub struct SearchState {
    /// Text input for the search query.
    pub input: super::text_input::TextInputState,
    /// Search subdirectories recursively.
    pub recursive: bool,
    /// Only show audio files (default true).
    pub audio_only: bool,
    /// What to match against.
    pub mode: SearchMode,
    /// Sort field for results.
    pub sort: SearchSort,
    /// Sort direction for results.
    pub sort_dir: SortDir,
    /// Whether the search panel is open.
    pub active: bool,
    /// Which element has keyboard focus.
    pub focus: SearchFocus,
    /// Debounce: instant of last keystroke (search fires after 200ms idle).
    pub last_keystroke: Option<std::time::Instant>,
    /// True while an async search is in flight.
    pub searching: bool,
    /// In-memory tag cache for non-recursive tag search (avoids repeated
    /// lofty reads on each debounce). Cleared on search close / dir change.
    pub tag_cache: std::collections::HashMap<PathBuf, String>,
    /// Cancel flag for async recursive search tasks.
    pub cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            input: super::text_input::TextInputState::empty(),
            recursive: false,
            audio_only: true,
            mode: SearchMode::Filename,
            sort: SearchSort::Score,
            sort_dir: SortDir::Desc,
            active: false,
            focus: SearchFocus::Input,
            last_keystroke: None,
            searching: false,
            tag_cache: std::collections::HashMap::new(),
            cancel: None,
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

    pub fn is_sacd_iso(&self) -> bool {
        matches!(self.kind, EntryKind::SacdIso)
    }

    pub fn is_dvda_iso(&self) -> bool {
        matches!(self.kind, EntryKind::DvdAudioIso)
    }

    pub fn is_dvda_dir(&self) -> bool {
        matches!(self.kind, EntryKind::DvdAudioDir)
    }

    pub fn is_dvdv_iso(&self) -> bool {
        matches!(self.kind, EntryKind::DvdVideoIso)
    }

    pub fn is_dvdv_dir(&self) -> bool {
        matches!(self.kind, EntryKind::DvdVideoDir)
    }

    pub fn is_disc_source(&self) -> bool {
        matches!(self.kind, EntryKind::SacdIso | EntryKind::DvdAudioIso | EntryKind::DvdAudioDir | EntryKind::DvdVideoIso | EntryKind::DvdVideoDir)
    }

    /// Probe-pipeline gate: entries this returns `true` for produce
    /// a `SourceInfo` + `SourceMetadata` pair via `probe_audio` +
    /// `read_metadata`, which routes SACDs to their ScarletBook/
    /// sidecar-aware variants. Distinct from `is_audio()`, which
    /// gates per-file lofty tag operations (`:edit-tags`, lofty
    /// save) — SACDs use a different write path. Use this for
    /// probe-cache and InfoPane decisions; use `is_audio()` for
    /// lofty-specific feature gates.
    pub fn is_probeable(&self) -> bool {
        self.is_audio() || self.is_disc_source()
    }

    /// Short type/format label for display in the type column.
    /// Audio files show their format (FLAC/MP3/etc), archives show their
    /// format (7z/zip/rar/tar.gz/etc), directories show "dir", other
    /// files show their lowercase extension. Symlinks are prefixed with `↪`.
    pub fn type_label(&self) -> String {
        let base = match &self.kind {
            EntryKind::ParentDir => String::new(),
            EntryKind::Directory => "dir".to_string(),
            EntryKind::DvdAudioDir => "dvda-dir".to_string(),
            EntryKind::DvdVideoDir => "dvdv-dir".to_string(),
            EntryKind::AudioFile(fmt) => fmt.name().to_string(),
            EntryKind::Archive => archive_label(&self.path),
            EntryKind::SacdIso => "sacd".to_string(),
            EntryKind::DvdAudioIso => "dvda".to_string(),
            EntryKind::DvdVideoIso => "dvdv".to_string(),
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

    /// Anchor for range selection: the last plain-clicked entry or V-mode start.
    /// Path-based so it survives refresh/sort/filter. `None` when no anchor is set.
    pub multi_select_anchor: Option<PathBuf>,

    /// Visual (V) selection mode: when true, moving the cursor extends the
    /// selection range from the anchor to the current cursor position.
    pub visual_mode: bool,

    /// Inline search panel state.
    pub search: SearchState,

    /// Path bar input (when the breadcrumb is in edit mode)
    pub path_input: Option<TextInputState>,

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

    /// Cache of SACD-ISO classifications keyed by path. The value
    /// pairs the file's mtime at probe time with the verdict
    /// (`true` = ScarletBook magic found). Re-probing skips when
    /// the cached mtime still matches; if the file has been
    /// touched the entry is re-evaluated (the underlying ISO
    /// could have been re-burned). Populated by `upgrade_iso_kinds`
    /// after every directory scan.
    pub sacd_classify_cache: HashMap<PathBuf, (std::time::SystemTime, bool)>,

    /// Cache of DVD-Audio ISO classifications keyed by path + len + mtime.
    /// This is used only by scan/upgrade code, never by render code.
    pub dvda_iso_classify_cache: HashMap<PathBuf, (ClassificationFingerprint, bool)>,

    /// Cache of DVD-Audio directory classifications keyed by path + IFO len + mtime.
    pub dvda_dir_classify_cache: HashMap<PathBuf, (ClassificationFingerprint, bool)>,

    /// Cache of DVD-Video ISO classifications keyed by path + len + mtime.
    pub dvdv_iso_classify_cache: HashMap<PathBuf, (ClassificationFingerprint, bool)>,

    /// Cache of DVD-Video directory classifications keyed by path + IFO len + mtime.
    pub dvdv_dir_classify_cache: HashMap<PathBuf, (ClassificationFingerprint, bool)>,

    /// Async unified disc parse cache with fingerprinted success/error entries.
    pub disc_probe_cache: HashMap<PathBuf, DiscProbeCacheEntry>,

    /// Disc probe tasks currently in flight.
    pub disc_probe_pending: std::collections::HashSet<PathBuf>,

    /// One-shot actions to run after a cold async disc probe completes.
    pub disc_probe_followup: HashMap<PathBuf, DiscProbeFollowup>,

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

    /// Type-ahead navigation buffer: accumulated keystrokes for prefix jump.
    pub type_ahead_buffer: String,
    /// Instant of the last type-ahead keystroke, for timeout reset.
    pub type_ahead_last_keystroke: Option<Instant>,

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
        (
            Self {
                cancel: flag.clone(),
            },
            flag,
        )
    }

    pub fn cancel(&self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
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
            visual_mode: false,
            search: SearchState::new(),
            path_input: None,
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
            sacd_classify_cache: HashMap::new(),
            dvda_iso_classify_cache: HashMap::new(),
            dvda_dir_classify_cache: HashMap::new(),
            dvdv_iso_classify_cache: HashMap::new(),
            dvdv_dir_classify_cache: HashMap::new(),
            disc_probe_cache: HashMap::new(),
            disc_probe_pending: std::collections::HashSet::new(),
            disc_probe_followup: HashMap::new(),
            return_target: BrowseReturnTarget::None,
            error: None,
            archive: None,
            scan_pending: None,
            cursor_restore_target: None,
            type_ahead_buffer: String::new(),
            type_ahead_last_keystroke: None,
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
            self.classify_dvda_directory_entries();
            self.upgrade_iso_kinds();
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

    /// Walk `all_files` and upgrade any `EntryKind::Archive` entry
    /// whose extension is `.iso` to `EntryKind::SacdIso` if a
    /// ScarletBook magic-byte probe succeeds.
    ///
    /// Uses `sacd_classify_cache` to skip the disk probe when the
    /// (path, mtime) pair has been seen before. The cache stays
    /// keyed by absolute path so two browse sessions visiting the
    /// same library share results across cd's. Cache entries
    /// auto-invalidate when the file's mtime changes (re-burn or
    /// re-rip).
    ///
    /// Cost per uncached ISO: 3 short reads (24 bytes) — negligible
    /// even on spinning disks. Cost per cached ISO: one HashMap
    /// lookup. Designed to run on the main thread immediately
    /// after a directory scan completes; if the directory contains
    /// dozens of ISOs, total wall-time is sub-50ms cold.
    pub(super) fn upgrade_iso_kinds(&mut self) {
        for entry in self.all_files.iter_mut() {
            if !matches!(entry.kind, EntryKind::Archive) {
                continue;
            }
            let is_iso = entry
                .path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("iso"))
                .unwrap_or(false);
            if !is_iso {
                continue;
            }

            // SACD first: the probe is three tiny magic-byte reads.
            let mtime = entry.modified;
            let cache_hit = mtime.and_then(|m| {
                self.sacd_classify_cache
                    .get(&entry.path)
                    .filter(|(cached_m, _)| *cached_m == m)
                    .map(|(_, verdict)| *verdict)
            });
            let is_sacd = if let Some(v) = cache_hit {
                v
            } else {
                let v = super::sacd::is_sacd_iso(&entry.path);
                if let Some(m) = mtime {
                    self.sacd_classify_cache.insert(entry.path.clone(), (m, v));
                }
                v
            };
            if is_sacd {
                entry.kind = EntryKind::SacdIso;
                continue;
            }

            // DVD-Audio second: bounded volume-open + AUDIO_TS.IFO check,
            // cached by (path, len, modified). Full disc parse/AOB probe stays
            // async in the DiscContents probe path.
            let fingerprint = ClassificationFingerprint::from_entry(entry);
            let is_dvda = self
                .dvda_iso_classify_cache
                .get(&entry.path)
                .filter(|(cached, _)| *cached == fingerprint)
                .map(|(_, verdict)| *verdict)
                .unwrap_or_else(|| {
                    let verdict = crate::disc::dvda_utils::is_dvda_iso(&entry.path);
                    self.dvda_iso_classify_cache
                        .insert(entry.path.clone(), (fingerprint.clone(), verdict));
                    verdict
                });
            if is_dvda {
                entry.kind = EntryKind::DvdAudioIso;
                continue;
            }

            // DVD-Video last: hybrids are intentionally excluded by dvdv_utils,
            // so DVD-Audio wins when both AUDIO_TS and VIDEO_TS exist.
            let is_dvdv = self
                .dvdv_iso_classify_cache
                .get(&entry.path)
                .filter(|(cached, _)| *cached == fingerprint)
                .map(|(_, verdict)| *verdict)
                .unwrap_or_else(|| {
                    let verdict = crate::disc::dvdv_utils::is_dvdv_iso(&entry.path);
                    self.dvdv_iso_classify_cache
                        .insert(entry.path.clone(), (fingerprint.clone(), verdict));
                    verdict
                });
            if is_dvdv {
                entry.kind = EntryKind::DvdVideoIso;
            }
        }
    }

    /// Classify scanned directory entries that are DVD-Audio roots.
    pub(super) fn classify_dvda_directory_entries(&mut self) {
        for entry in self.all_dirs.iter_mut() {
            classify_dvda_directory_entry(entry, &mut self.dvda_dir_classify_cache);
            classify_dvdv_directory_entry(entry, &mut self.dvdv_dir_classify_cache);
        }
    }

    pub(super) fn classify_scanned_directory_entries(&mut self, dirs: &mut [BrowseEntry]) {
        for entry in dirs.iter_mut() {
            classify_dvda_directory_entry(entry, &mut self.dvda_dir_classify_cache);
            classify_dvdv_directory_entry(entry, &mut self.dvdv_dir_classify_cache);
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
        let prev_path = self
            .entries
            .get(self.selected_index)
            .map(|e| e.path.clone());
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
                    let final_path = resolved.canonicalize().unwrap_or_else(|_| resolved.clone());
                    if final_path.is_dir() {
                        Ok(final_path)
                    } else {
                        Err(format!("not a directory: {}", final_path.display()))
                    }
                })
                .await
                .unwrap_or_else(|e| Err(format!("path check failed: {}", e)));

                let _ = tx
                    .send(super::message::AppMessage::PathValidationComplete {
                        input: input_str,
                        result,
                    })
                    .await;
            });
            Ok(()) // The actual navigation happens when the result arrives.
        } else {
            // Synchronous fallback.
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

    // ── Type-ahead navigation ──────────────────────────────────────────

    /// Reset the type-ahead buffer and timestamp.
    pub fn clear_type_ahead(&mut self) {
        self.type_ahead_buffer.clear();
        self.type_ahead_last_keystroke = None;
    }

    /// Append a character to the type-ahead buffer and jump to the first
    /// matching entry. Resets the buffer first if the timeout has elapsed.
    pub fn type_ahead_push(&mut self, c: char) {
        if let Some(last) = self.type_ahead_last_keystroke {
            if last.elapsed() >= TYPE_AHEAD_TIMEOUT {
                self.type_ahead_buffer.clear();
            }
        }

        self.type_ahead_buffer.push(c);
        self.type_ahead_last_keystroke = Some(Instant::now());

        let query = self.type_ahead_buffer.to_lowercase();
        // Priority 1: prefix match.
        // Priority 2: substring/contains match.
        let idx = self
            .entries
            .iter()
            .position(|e| e.name_lower.starts_with(&query))
            .or_else(|| {
                self.entries
                    .iter()
                    .position(|e| e.name_lower.contains(&query))
            });
        if let Some(idx) = idx {
            self.selected_index = idx;
            self.ensure_visible();
        }
    }

    /// Remove the last character from the type-ahead buffer and re-search.
    /// If the buffer becomes empty, clears the type-ahead state entirely.
    pub fn type_ahead_pop(&mut self) {
        self.type_ahead_buffer.pop();
        if self.type_ahead_buffer.is_empty() {
            self.type_ahead_last_keystroke = None;
        } else {
            self.type_ahead_last_keystroke = Some(Instant::now());
            let query = self.type_ahead_buffer.to_lowercase();
            let idx = self
                .entries
                .iter()
                .position(|e| e.name_lower.starts_with(&query))
                .or_else(|| {
                    self.entries
                        .iter()
                        .position(|e| e.name_lower.contains(&query))
                });
            if let Some(idx) = idx {
                self.selected_index = idx;
                self.ensure_visible();
            }
        }
    }

    /// Whether the type-ahead buffer is currently active (non-empty and
    /// not timed out).
    pub fn type_ahead_active(&self) -> bool {
        if self.type_ahead_buffer.is_empty() {
            return false;
        }
        match self.type_ahead_last_keystroke {
            Some(last) => last.elapsed() < TYPE_AHEAD_TIMEOUT,
            None => false,
        }
    }

    pub fn page_up(&mut self) {
        let jump = self.visible_height.max(1);
        self.selected_index = self.selected_index.saturating_sub(jump);
        self.ensure_visible();
    }

    pub fn page_down(&mut self) {
        let jump = self.visible_height.max(1);
        self.selected_index =
            (self.selected_index + jump).min(self.entries.len().saturating_sub(1));
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
        self.visual_mode = false;
    }

    /// Update the visual selection range from anchor to current cursor.
    /// Called after every cursor move while visual_mode is active.
    pub fn update_visual_selection(&mut self) {
        let anchor_idx = self
            .multi_select_anchor
            .as_ref()
            .and_then(|p| self.entries.iter().position(|e| e.path == *p))
            .unwrap_or(self.selected_index);
        let lo = anchor_idx.min(self.selected_index);
        let hi = anchor_idx.max(self.selected_index);

        // Replace multi_selected with the contiguous range (non-ParentDir entries).
        self.multi_selected.clear();
        for i in lo..=hi {
            if let Some(entry) = self.entries.get(i) {
                if !matches!(entry.kind, EntryKind::ParentDir) {
                    self.multi_selected.push(entry.path.clone());
                }
            }
        }
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
    pub fn collect_selection_for_queue(&self) -> QueueExpansionResult {
        if !self.multi_selected.is_empty() {
            return expand_paths_to_audio_with_metadata(&self.multi_selected);
        }
        if let Some(entry) = self.selected_entry() {
            match &entry.kind {
                EntryKind::AudioFile(_) | EntryKind::Archive => {
                    return QueueExpansionResult { paths: vec![entry.path.clone()], cue_artifact_audio: HashSet::new() };
                }
                EntryKind::OtherFile if is_cue_sheet_path(&entry.path) => {
                    return QueueExpansionResult { paths: vec![entry.path.clone()], cue_artifact_audio: HashSet::new() };
                }
                EntryKind::Directory => {
                    return expand_paths_to_audio_with_metadata(&[entry.path.clone()]);
                }
                _ => {}
            }
        }
        QueueExpansionResult::default()
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

    /// Open the path bar input, seeded with the current directory.
    pub fn open_path_input(&mut self) {
        let display = {
            let path_str = self.current_dir.display().to_string();
            let home = std::env::var("HOME").unwrap_or_default();
            if !home.is_empty() && path_str.starts_with(&home) {
                format!("~{}", &path_str[home.len()..])
            } else {
                path_str
            }
        };
        self.path_input = Some(TextInputState::new_selected(display));
    }

    /// Close the path bar input. If `commit`, navigate to the entered path.
    pub fn close_path_input(&mut self, commit: bool) {
        if commit {
            if let Some(input) = self.path_input.take() {
                let text = input.text.trim().to_string();
                if !text.is_empty() {
                    if let Err(err) = self.navigate_to_str(&text) {
                        log::warn!("path bar navigation failed: {err}");
                    }
                }
            }
        } else {
            self.path_input = None;
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

    /// Open the search panel. Clears any active old-style filter.
    pub fn open_search(&mut self) {
        self.search.active = true;
        self.search.focus = SearchFocus::Input;
        self.search.input = TextInputState::new(String::new());
        self.search.last_keystroke = None;
        self.search.searching = false;
        // Clear old filter if active.
        self.reset_filter_state();
    }

    /// Close the search panel and restore the normal directory listing.
    pub fn close_search(&mut self) {
        // Cancel any in-flight async search.
        if let Some(ref flag) = self.search.cancel {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.search.active = false;
        self.search.input = TextInputState::new(String::new());
        self.search.last_keystroke = None;
        self.search.searching = false;
        self.search.tag_cache.clear();
        self.search.cancel = None;
        // Restore normal listing.
        self.apply_view_preserving_cursor();
    }

    /// Execute a search. Non-recursive runs synchronously.
    /// Recursive spawns an async task and sends results via tx.
    pub fn execute_search(
        &mut self,
        tx: Option<&tokio::sync::mpsc::Sender<super::message::AppMessage>>,
    ) {
        let query = self.search.input.text.trim().to_ascii_lowercase();
        if query.is_empty() {
            self.apply_view_preserving_cursor();
            return;
        }

        let show_hidden = self.show_hidden;
        let audio_only = self.search.audio_only;
        let mode = self.search.mode;

        if self.search.recursive {
            if let Some(tx) = tx {
                self.spawn_search_async(&query, show_hidden, audio_only, mode, tx.clone());
            }
        } else {
            self.execute_search_local(&query, show_hidden, audio_only, mode);
        }
    }

    /// Non-recursive search: filter current directory's entries.
    fn execute_search_local(
        &mut self,
        query: &str,
        show_hidden: bool,
        audio_only: bool,
        mode: SearchMode,
    ) {
        use fuzzy_matcher::skim::SkimMatcherV2;
        use fuzzy_matcher::FuzzyMatcher;

        let matcher = SkimMatcherV2::default();
        let mut scored: Vec<(BrowseEntry, i64)> = Vec::new();
        let parent = self.parent_entry.clone();
        let search_tags = matches!(mode, SearchMode::Tags | SearchMode::Both);
        let search_filename = matches!(mode, SearchMode::Filename | SearchMode::Both);

        for entries_list in [&self.all_dirs, &self.all_files] {
            for e in entries_list {
                if !show_hidden && e.name.starts_with('.') {
                    continue;
                }
                if audio_only && !is_audio_filter_visible_entry(e) {
                    continue;
                }

                let mut best_score: Option<i64> = None;

                // Directories always match on filename (for navigation),
                // even in tags-only mode.
                if search_filename || matches!(e.kind, EntryKind::Directory) {
                    if let Some(s) = matcher.fuzzy_match(&e.name_lower, query) {
                        best_score = Some(best_score.map_or(s, |prev: i64| prev.max(s)));
                    }
                }

                if search_tags && matches!(e.kind, EntryKind::AudioFile(_)) {
                    let tag_str =
                        build_tag_search_string_cached(&e.path, &mut self.search.tag_cache);
                    if !tag_str.is_empty() {
                        if let Some(s) = matcher.fuzzy_match(&tag_str, query) {
                            best_score = Some(best_score.map_or(s, |prev: i64| prev.max(s)));
                        }
                    }
                }

                if let Some(score) = best_score {
                    scored.push((e.clone(), score));
                }
            }
        }

        sort_search_results(&mut scored, self.search.sort, self.search.sort_dir);

        let mut results: Vec<BrowseEntry> = Vec::new();
        if let Some(p) = parent {
            results.push(p);
        }
        results.extend(scored.into_iter().map(|(e, _)| e));

        self.entries = results;
        self.selected_index = 0;
        self.scroll_offset = 0;
    }

    /// Spawn an async recursive search task. Results arrive via SearchComplete.
    fn spawn_search_async(
        &mut self,
        query: &str,
        show_hidden: bool,
        audio_only: bool,
        mode: SearchMode,
        tx: tokio::sync::mpsc::Sender<super::message::AppMessage>,
    ) {
        // Cancel any previous search task.
        if let Some(ref flag) = self.search.cancel {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }

        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.search.cancel = Some(cancel.clone());
        self.search.searching = true;

        let root = self.current_dir.clone();
        let query = query.to_string();
        let search_tags = matches!(mode, SearchMode::Tags | SearchMode::Both);
        let search_filename = matches!(mode, SearchMode::Filename | SearchMode::Both);

        tokio::spawn(async move {
            let results =
                tokio::task::spawn_blocking(move || -> Option<Vec<(BrowseEntry, i64)>> {
                    use fuzzy_matcher::skim::SkimMatcherV2;
                    use fuzzy_matcher::FuzzyMatcher;
                    use walkdir::WalkDir;

                    let matcher = SkimMatcherV2::default();
                    let mut scored: Vec<(BrowseEntry, i64)> = Vec::new();

                    // Open own DB connection for tag cache.
                    let db = crate::db::Database::open().ok();

                    for entry in WalkDir::new(&root)
                        .min_depth(1)
                        .follow_links(false)
                        .into_iter()
                        .filter_entry(|e| {
                            if !show_hidden {
                                if let Some(name) = e.file_name().to_str() {
                                    if name.starts_with('.') {
                                        return false;
                                    }
                                }
                            }
                            true
                        })
                    {
                        // Check cancel flag periodically.
                        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            return None; // Cancelled — don't send results.
                        }

                        let entry = match entry {
                            Ok(e) => e,
                            Err(_) => continue,
                        };

                        if entry.file_type().is_dir() {
                            continue;
                        }

                        if !show_hidden {
                            if let Some(name) = entry.file_name().to_str() {
                                if name.starts_with('.') {
                                    continue;
                                }
                            }
                        }

                        let path = entry.path().to_path_buf();
                        let kind = classify_file(&path);

                        if audio_only
                            && !matches!(kind, EntryKind::AudioFile(_))
                            && !is_cue_sheet_path(&path)
                        {
                            continue;
                        }

                        let rel = path
                            .strip_prefix(&root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .to_string();
                        let rel_lower = rel.to_lowercase();

                        let mut best_score: Option<i64> = None;

                        if search_filename {
                            if let Some(s) = matcher.fuzzy_match(&rel_lower, &query) {
                                best_score = Some(s);
                            }
                        }

                        if search_tags && matches!(kind, EntryKind::AudioFile(_)) {
                            // Try DB cache first, then lofty.
                            let tag_str = if let Some(ref db) = db {
                                let path_str = path.display().to_string();
                                let meta = std::fs::metadata(&path).ok();
                                let mtime = meta
                                    .as_ref()
                                    .and_then(|m| m.modified().ok())
                                    .map(crate::db::systemtime_to_unix)
                                    .unwrap_or(0);
                                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);

                                if let Some((cached, ..)) =
                                    db.get_cached_tags(&path_str, mtime, size)
                                {
                                    cached
                                } else {
                                    let r = read_tags_from_file(&path);
                                    let _ = db.store_cached_tags(
                                        &path_str,
                                        mtime,
                                        size,
                                        r.title.as_deref(),
                                        r.artist.as_deref(),
                                        r.album.as_deref(),
                                        r.genre.as_deref(),
                                        r.year.as_deref(),
                                        &r.tag_string,
                                    );
                                    r.tag_string
                                }
                            } else {
                                read_tags_from_file(&path).tag_string
                            };

                            if !tag_str.is_empty() {
                                if let Some(s) = matcher.fuzzy_match(&tag_str, &query) {
                                    best_score =
                                        Some(best_score.map_or(s, |prev: i64| prev.max(s)));
                                }
                            }
                        }

                        if let Some(score) = best_score {
                            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                            let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
                            scored.push((BrowseEntry::new(path, rel, kind, size, modified), score));

                            if scored.len() >= 500 {
                                break;
                            }
                        }
                    }

                    Some(scored)
                })
                .await
                .unwrap_or(None);

            // Only send results if not cancelled.
            if let Some(results) = results {
                let _ = tx
                    .send(super::message::AppMessage::SearchComplete { results })
                    .await;
            }
        });
    }

    /// Reset filter state AND clear the multi-select anchor, used by navigation
    /// methods. The anchor is for range-select (Alt+click) and is a
    /// per-directory context.
    fn reset_nav_state(&mut self) {
        self.reset_filter_state();
        self.multi_select_anchor = None;
        self.clear_type_ahead();
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

        if entry.is_probeable() {
            let path = entry.path.clone();
            if self.probe_cache.contains_key(&path) || self.probe_pending.contains(&path) {
                return;
            }

            // Check SQLite probe cache before spawning an async probe.
            if let Some(db) = db {
                if let Some(mtime) = entry.modified {
                    let mtime_unix = crate::db::systemtime_to_unix(mtime);
                    if let Some(row) =
                        db.get_cached_probe(&path.display().to_string(), mtime_unix, entry.size)
                    {
                        if let Some(mut info) = row.to_cached_info(entry.size) {
                            // PE metadata check not stored in DB — run it now.
                            info.metadata.preemphasis_metadata =
                                super::probe::preemphasis_metadata_check_pub(&path);
                            // HDCD info from analysis cache (if previously analyzed).
                            if let Some(analysis) = db.get_cached_analysis(
                                &path.display().to_string(),
                                mtime_unix,
                                entry.size,
                            ) {
                                if analysis.hdcd_detected == Some(true) {
                                    info.metadata.hdcd_detail = analysis.hdcd_detail;
                                }
                            }
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
            if self.dir_stats_cache.contains_key(&path) || self.dir_stats_pending.contains(&path) {
                return;
            }
            self.dir_stats_pending.insert(path.clone());
            spawn_dir_stats(path, tx.clone());
        }
    }

    /// Get cached info for the currently selected audio file, if probed
    pub fn current_cached_info(&self) -> Option<&Arc<CachedInfo>> {
        let entry = self.entries.get(self.selected_index)?;
        if !entry.is_probeable() {
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
pub fn spawn_audio_probe(path: PathBuf, tx: tokio::sync::mpsc::Sender<super::message::AppMessage>) {
    if is_cue_sheet_path(&path) {
        // Defense in depth: callers should route CUE preview through
        // `spawn_cue_proxy_audio_probe`, but never allow a `.cue` text file to
        // reach ffmpeg probing through this generic audio helper.
        spawn_cue_proxy_audio_probe(path, tx);
        return;
    }

    tokio::spawn(async move {
        let path_for_task = path.clone();
        let result: Result<CachedInfo, String> = tokio::task::spawn_blocking(move || {
            let source =
                crate::tui::probe::probe_audio(&path_for_task).map_err(|e| format!("{}", e))?;
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

/// Spawn a background tokio task that probes the audio image(s) referenced by
/// a CUE sheet for Convert-screen batch cursor preview. This deliberately sends
/// the CUE path as the logical source key while resolving and probing the
/// referenced image files inside `probe_cue_proxy_source`; the `.cue` text file
/// itself is never routed through `probe_audio`.
pub fn spawn_cue_proxy_audio_probe(
    path: PathBuf,
    tx: tokio::sync::mpsc::Sender<super::message::AppMessage>,
) {
    tokio::spawn(async move {
        let path_for_task = path.clone();
        let result: Result<CachedInfo, String> = tokio::task::spawn_blocking(move || {
            let result = crate::tui::app::probe_cue_proxy_source(&path_for_task)
                .map_err(|err| format!("CUE proxy probe failed: {}; set format manually", err))?;

            match result.info {
                Some(source) => Ok(CachedInfo {
                    source,
                    metadata: result.metadata,
                }),
                None => Err(result.probe_notice.unwrap_or_else(|| {
                    "CUE proxy probe returned no source info; set format manually".to_string()
                })),
            }
        })
        .await
        .unwrap_or_else(|join_err| Err(format!("CUE proxy probe task panicked: {}", join_err)));

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
pub fn spawn_dir_stats(path: PathBuf, tx: tokio::sync::mpsc::Sender<super::message::AppMessage>) {
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
            tokio::task::spawn_blocking(move || scan_directory_blocking(&scan_path, &cancel_flag)),
        )
        .await;

        let (parent_entry, dirs, files, error) = match result {
            Ok(Ok(Ok((parent, dirs, files)))) => (parent, dirs, files, None),
            Ok(Ok(Err(e))) => (None, Vec::new(), Vec::new(), Some(e)),
            Ok(Err(join_err)) => (
                None,
                Vec::new(),
                Vec::new(),
                Some(format!("scan task panicked: {}", join_err)),
            ),
            Err(_timeout) => {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                (
                    None,
                    Vec::new(),
                    Vec::new(),
                    Some("scan timed out (30s)".into()),
                )
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

    let read = fs::read_dir(dir).map_err(|e| format!("Cannot read directory: {}", e))?;

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
            if crate::disc::dvda_utils::is_dvda_directory(&path) {
                EntryKind::DvdAudioDir
            } else if crate::disc::dvdv_utils::is_dvdv_directory(&path) {
                EntryKind::DvdVideoDir
            } else {
                EntryKind::Directory
            }
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
    // Format filter (only applies to non-directory entries). Use the
    // path-aware check so `.cue` stays visible as a convertible source under
    // AudioOnly without widening the filter to all `OtherFile` entries.
    if !matches!(entry.kind, EntryKind::Directory | EntryKind::DvdAudioDir | EntryKind::DvdVideoDir) && !format_filter.allows_entry(entry) {
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
        EntryKind::AudioFile(AudioFormat::Dsf) => 18,
        EntryKind::AudioFile(AudioFormat::Dff) => 19,
        EntryKind::AudioFile(AudioFormat::Dts) => 20,
        EntryKind::AudioFile(AudioFormat::Ac3) => 21,
        EntryKind::AudioFile(AudioFormat::Ape) => 22,
        EntryKind::AudioFile(AudioFormat::Lpcm) => 23,
        EntryKind::SacdIso | EntryKind::DvdAudioIso | EntryKind::DvdAudioDir | EntryKind::DvdVideoIso | EntryKind::DvdVideoDir => 25,
        EntryKind::Archive => 25,
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
            walk_dir_for_stats(&entry.path(), stats, depth + 1, max_depth, max_files);
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
/// Build a lowercase searchable string from an audio file's metadata tags.
/// Uses the in-memory `tag_cache` if available, otherwise reads via lofty
/// and populates the cache.
fn build_tag_search_string_cached(
    path: &Path,
    tag_cache: &mut std::collections::HashMap<PathBuf, String>,
) -> String {
    if let Some(cached) = tag_cache.get(path) {
        return cached.clone();
    }
    let result = read_tags_from_file(path);
    tag_cache.insert(path.to_path_buf(), result.tag_string.clone());
    result.tag_string
}

/// Tag data read from a file for search and cache storage.
struct TagReadResult {
    tag_string: String,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    genre: Option<String>,
    year: Option<String>,
}

/// Read tag fields from a file via lofty. Returns concatenated search
/// string plus individual fields for caching and sorting.
fn read_tags_from_file(path: &Path) -> TagReadResult {
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;

    let tagged = match lofty::read_from_path(path) {
        Ok(t) => t,
        Err(_) => {
            return TagReadResult {
                tag_string: String::new(),
                title: None,
                artist: None,
                album: None,
                genre: None,
                year: None,
            }
        }
    };
    let tag = match tagged.primary_tag().or_else(|| tagged.first_tag()) {
        Some(t) => t,
        None => {
            return TagReadResult {
                tag_string: String::new(),
                title: None,
                artist: None,
                album: None,
                genre: None,
                year: None,
            }
        }
    };

    let title = tag.title().map(|s| s.to_string());
    let artist = tag.artist().map(|s| s.to_string());
    let album = tag.album().map(|s| s.to_string());
    let genre = tag.genre().map(|s| s.to_string());
    let year = tag.year().map(|y| y.to_string());

    let mut parts: Vec<&str> = Vec::new();
    if let Some(ref v) = title {
        parts.push(v);
    }
    if let Some(ref v) = artist {
        parts.push(v);
    }
    if let Some(ref v) = album {
        parts.push(v);
    }
    if let Some(ref v) = genre {
        parts.push(v);
    }
    if let Some(ref v) = year {
        parts.push(v);
    }

    TagReadResult {
        tag_string: parts.join(" ").to_lowercase(),
        title,
        artist,
        album,
        genre,
        year,
    }
}

/// Sort scored search results by the given field and direction.
pub(super) fn sort_search_results(
    scored: &mut Vec<(BrowseEntry, i64)>,
    sort: SearchSort,
    dir: SortDir,
) {
    // For tag-based sorts, pre-extract the sort key to avoid repeated lofty reads.
    if sort.is_tag_sort() {
        let mut keyed: Vec<(String, usize)> = scored
            .iter()
            .enumerate()
            .map(|(i, (entry, _))| {
                let key = extract_tag_sort_key(&entry.path, sort);
                (key, i)
            })
            .collect();

        keyed.sort_by(|a, b| {
            let ord = a.0.cmp(&b.0);
            match dir {
                SortDir::Asc => ord,
                SortDir::Desc => ord.reverse(),
            }
        });

        let sorted: Vec<_> = keyed.iter().map(|(_, i)| scored[*i].clone()).collect();
        *scored = sorted;
        return;
    }

    scored.sort_by(|a, b| {
        let ord = match sort {
            SearchSort::Score => a.1.cmp(&b.1),
            SearchSort::Name => a.0.name_lower.cmp(&b.0.name_lower),
            SearchSort::Date => {
                let a_time = a.0.modified.unwrap_or(std::time::UNIX_EPOCH);
                let b_time = b.0.modified.unwrap_or(std::time::UNIX_EPOCH);
                a_time.cmp(&b_time)
            }
            SearchSort::Size => a.0.size.cmp(&b.0.size),
            SearchSort::Extension => {
                let a_ext = a.0.path.extension().and_then(|e| e.to_str()).unwrap_or("");
                let b_ext = b.0.path.extension().and_then(|e| e.to_str()).unwrap_or("");
                a_ext.to_ascii_lowercase().cmp(&b_ext.to_ascii_lowercase())
            }
            _ => std::cmp::Ordering::Equal, // tag sorts handled above
        };
        match dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
        }
    });
}

/// Extract a single tag field value for sorting, lowercased.
fn extract_tag_sort_key(path: &Path, sort: SearchSort) -> String {
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;

    let tagged = match lofty::read_from_path(path) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    let tag = match tagged.primary_tag().or_else(|| tagged.first_tag()) {
        Some(t) => t,
        None => return String::new(),
    };

    let val = match sort {
        SearchSort::Artist => tag.artist().map(|s| s.to_string()),
        SearchSort::Album => tag.album().map(|s| s.to_string()),
        SearchSort::Year => tag.year().map(|y| format!("{:04}", y)),
        SearchSort::Title => tag.title().map(|s| s.to_string()),
        _ => None,
    };

    val.unwrap_or_default().to_lowercase()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueExpansionResult {
    /// Paths to queue for conversion, in deterministic browse order.
    pub paths: Vec<PathBuf>,
    /// Audio paths whose sibling sidecar CUE was already classified as a
    /// metadata artifact during queue expansion. Downstream conversion must
    /// skip sidecar CUE discovery for these paths while still honoring
    /// embedded CUESHEET tags.
    pub cue_artifact_audio: HashSet<PathBuf>,
}

impl QueueExpansionResult {
    #[must_use]
    pub fn into_paths(self) -> Vec<PathBuf> {
        self.paths
    }
}

impl std::ops::Deref for QueueExpansionResult {
    type Target = [PathBuf];

    fn deref(&self) -> &Self::Target {
        &self.paths
    }
}

impl IntoIterator for QueueExpansionResult {
    type Item = PathBuf;
    type IntoIter = std::vec::IntoIter<PathBuf>;

    fn into_iter(self) -> Self::IntoIter {
        self.paths.into_iter()
    }
}

/// Expands files/directories to queueable audio paths using the historical
/// `Vec<PathBuf>` API. Keep this wrapper for non-queue call sites (metadata
/// editor, AccurateRip, context menu actions, and other tree-local utilities)
/// that only need paths and should not own conversion policy metadata.
pub fn expand_paths_to_audio(paths: &[PathBuf]) -> Vec<PathBuf> {
    expand_paths_to_audio_with_metadata(paths).into_paths()
}

/// Expands files/directories for conversion queue construction and carries
/// sidecar-CUE suppression metadata alongside the path list. Queue-building
/// callers must use this result; non-queue callers should use
/// `expand_paths_to_audio()` above to preserve the old API contract.
pub fn expand_paths_to_audio_with_metadata(paths: &[PathBuf]) -> QueueExpansionResult {
    let mut plan = QueueExpansionPlan::default();
    for path in paths {
        collect_queue_candidates(path, &mut plan);
    }
    plan.into_queue_paths()
}

/// Directory/file expansion plan for conversion queue inputs.
///
/// Build the whole candidate set before deciding what to queue. A split-source
/// CUE discovered late in a directory walk can suppress audio discovered earlier,
/// so queue decisions must happen after collection to stay idempotent.
#[derive(Default)]
struct QueueExpansionPlan {
    cue_sheets: Vec<CueQueueCandidate>,
    queueable_non_cue: Vec<PathBuf>,
    queueable_non_cue_keys: HashSet<PathBuf>,
}

#[derive(Debug, Clone)]
struct CueQueueCandidate {
    path: PathBuf,
    path_key: PathBuf,
    explicit: bool,
}

impl QueueExpansionPlan {
    fn add_explicit_file(&mut self, path: PathBuf) {
        self.add_file(path, true);
    }

    fn add_discovered_file(&mut self, path: PathBuf) {
        self.add_file(path, false);
    }

    fn add_file(&mut self, path: PathBuf, explicit: bool) {
        if is_cue_sheet_path(&path) {
            self.add_cue_sheet(path, explicit);
        } else if is_queueable_file(&path) {
            push_unique_path_with_keys(
                &mut self.queueable_non_cue,
                &mut self.queueable_non_cue_keys,
                path,
            );
        }
    }

    fn add_cue_sheet(&mut self, path: PathBuf, explicit: bool) {
        let path_key = queue_path_key(&path);
        if let Some(existing) = self
            .cue_sheets
            .iter_mut()
            .find(|existing| existing.path_key == path_key)
        {
            existing.explicit |= explicit;
            return;
        }

        self.cue_sheets.push(CueQueueCandidate {
            path,
            path_key,
            explicit,
        });
    }

    fn into_queue_paths(self) -> QueueExpansionResult {
        let QueueExpansionPlan {
            cue_sheets,
            queueable_non_cue,
            queueable_non_cue_keys: _,
        } = self;

        let mut result = Vec::new();
        let mut result_keys = HashSet::new();
        let mut suppressed_audio_keys = HashSet::new();
        let mut cue_artifact_audio_keys = HashSet::new();

        for cue in cue_sheets {
            match cue_queue_decision_for_path(&cue.path) {
                Ok(CueQueueDecision::SplitSource { referenced_audio }) => {
                    push_unique_path_with_keys(&mut result, &mut result_keys, cue.path);
                    for path in referenced_audio {
                        suppressed_audio_keys.insert(queue_path_key(&path));
                    }
                }
                Ok(CueQueueDecision::MetadataArtifact { referenced_audio }) => {
                    if cue.explicit {
                        push_unique_path_with_keys(&mut result, &mut result_keys, cue.path);
                    } else {
                        for path in referenced_audio {
                            cue_artifact_audio_keys.insert(queue_path_key(&path));
                        }
                    }
                }
                Err(err) => {
                    log::warn!(
                        "CUE {} is not safe to queue from folder expansion; suppressing it and marking sibling audio to skip sidecar CUE detection: {}",
                        cue.path.display(),
                        err
                    );
                    if cue.explicit {
                        push_unique_path_with_keys(&mut result, &mut result_keys, cue.path);
                    } else {
                        mark_sibling_audio_as_cue_artifacts(
                            &cue.path,
                            &queueable_non_cue,
                            &mut cue_artifact_audio_keys,
                        );
                    }
                }
            }
        }

        let mut cue_artifact_audio = HashSet::new();
        for path in queueable_non_cue {
            // Suppression applies to explicitly selected audio too when the
            // same expansion also contains an explicit split-source CUE that
            // references it. The explicit CUE selection is honored, and the
            // referenced audio is omitted by design to avoid converting the
            // same source twice through both the CUE materializer and the
            // raw audio-file path.
            let path_key = queue_path_key(&path);
            if is_audio_file_path(&path) && suppressed_audio_keys.contains(&path_key) {
                continue;
            }
            if is_audio_file_path(&path) && cue_artifact_audio_keys.contains(&path_key) {
                cue_artifact_audio.insert(path.clone());
            }
            push_unique_path_with_keys(&mut result, &mut result_keys, path);
        }

        QueueExpansionResult {
            paths: result,
            cue_artifact_audio,
        }
    }
}

fn mark_sibling_audio_as_cue_artifacts(
    cue_path: &Path,
    queueable_non_cue: &[PathBuf],
    cue_artifact_audio_keys: &mut HashSet<PathBuf>,
) {
    let Some(cue_parent) = cue_path.parent().map(queue_path_key) else {
        return;
    };

    for path in queueable_non_cue {
        if !is_audio_file_path(path) {
            continue;
        }
        let Some(audio_parent) = path.parent().map(queue_path_key) else {
            continue;
        };
        if audio_parent == cue_parent {
            cue_artifact_audio_keys.insert(queue_path_key(path));
        }
    }
}

fn collect_queue_candidates(path: &Path, plan: &mut QueueExpansionPlan) {
    if path.is_dir() {
        collect_queue_candidates_recursive(path, plan);
    } else {
        plan.add_explicit_file(path.to_path_buf());
    }
}

/// Recursively collect candidate queue inputs without deciding suppression.
/// Symlinks are skipped to avoid loops, matching the browse stats walk policy.
fn collect_queue_candidates_recursive(dir: &Path, plan: &mut QueueExpansionPlan) {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };

    let mut dirs = Vec::new();
    let mut files = Vec::new();
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
            dirs.push(path);
        } else {
            files.push(path);
        }
    }

    dirs.sort();
    files.sort();

    for file in files {
        plan.add_discovered_file(file);
    }
    for child in dirs {
        collect_queue_candidates_recursive(&child, plan);
    }
}

#[derive(Debug)]
enum CueQueueDecision {
    /// The CUE provides track boundaries that are not represented by the
    /// referenced audio file set alone. Queue the CUE and suppress every audio
    /// file it references so the materializer is the single source of tracks.
    SplitSource { referenced_audio: Vec<PathBuf> },
    /// The CUE points one-to-one at already-split tracks. For folder expansion,
    /// queue the audio files and suppress the CUE as a metadata artifact. This
    /// also covers a one-track image CUE: with no split points to materialize,
    /// the image file itself is the queueable source and the CUE is metadata.
    MetadataArtifact { referenced_audio: Vec<PathBuf> },
}

#[derive(Debug)]
struct CueQueueAnalysis {
    referenced_audio: Vec<PathBuf>,
    track_count_by_audio_key: BTreeMap<PathBuf, usize>,
}

fn cue_queue_decision_for_path(cue_path: &Path) -> Result<CueQueueDecision, String> {
    let analysis = analyze_cue_for_queue(cue_path)?;

    // A CUE is a split source as soon as it provides split points for at least
    // one referenced audio file. Mixed layouts can also reference one-track
    // bonus files; once the CUE is a split source, suppress every referenced
    // audio file so the materializer owns the complete track index and the
    // queue never double-converts the one-track references.
    let has_split_source = analysis
        .track_count_by_audio_key
        .values()
        .any(|track_count| *track_count > 1);

    if has_split_source {
        Ok(CueQueueDecision::SplitSource {
            referenced_audio: analysis.referenced_audio,
        })
    } else {
        Ok(CueQueueDecision::MetadataArtifact {
            referenced_audio: analysis.referenced_audio,
        })
    }
}

/// Return audio paths that queue expansion should suppress for a CUE.
///
/// This is deliberately a suppression helper, not a generic "materializable"
/// query: metadata-artifact CUEs return an empty list, while split-source CUEs
/// return every referenced audio path. In mixed layouts that includes one-track
/// referenced files, because once the CUE provides split points for any audio
/// file, the materializer owns the complete CUE track index and raw audio paths
/// must not be queued separately.
#[cfg(test)]
pub(crate) fn cue_referenced_audio_paths_to_suppress_for_queue(
    cue_path: &Path,
) -> Result<Vec<PathBuf>, String> {
    match cue_queue_decision_for_path(cue_path)? {
        CueQueueDecision::SplitSource { referenced_audio } => Ok(referenced_audio),
        CueQueueDecision::MetadataArtifact { .. } => Ok(Vec::new()),
    }
}

fn analyze_cue_for_queue(cue_path: &Path) -> Result<CueQueueAnalysis, String> {
    let sheet = crate::tui::cue_parser::parse_cue_file(cue_path)
        .map_err(|err| format!("failed to parse CUE: {err}"))?;
    let parent = cue_path
        .parent()
        .ok_or_else(|| "CUE path has no parent directory".to_string())?;
    let parent_key = queue_path_key(parent);

    if sheet.tracks.is_empty() {
        return Err("CUE sheet has no tracks".to_string());
    }

    let mut referenced_audio = Vec::new();
    let mut referenced_audio_keys = HashSet::new();
    let mut resolved_tracks = Vec::with_capacity(sheet.tracks.len());
    let mut track_count_by_audio_key = BTreeMap::new();
    for track in &sheet.tracks {
        let index01 = track
            .index01_frames
            .ok_or_else(|| format!("track {} has no INDEX 01", track.number))?;
        let file_ref = track
            .file
            .as_deref()
            .ok_or_else(|| format!("track {} has no FILE reference", track.number))?;

        let resolved = match resolve_cue_file_reference_for_queue(parent, file_ref) {
            CueReferenceResolution::Resolved(path) => path,
            CueReferenceResolution::Missing => {
                return Err(format!(
                    "track {} FILE reference {:?} was not found",
                    track.number, file_ref
                ));
            }
            CueReferenceResolution::Ambiguous(candidates) => {
                return Err(format!(
                    "track {} FILE reference {:?} was ambiguous: {}",
                    track.number,
                    file_ref,
                    format_candidate_paths_for_log(&candidates)
                ));
            }
        };

        if !is_audio_file_path(&resolved) {
            return Err(format!(
                "track {} FILE reference {:?} did not resolve to a supported audio file: {}",
                track.number,
                file_ref,
                resolved.display()
            ));
        }

        // Folder expansion intentionally accepts only CUE references to audio
        // in the exact same directory as the CUE. Some valid CUE layouts keep
        // the image under a child directory, but the queue heuristic chooses a
        // conservative boundary here: cross-directory references are treated as
        // unsafe metadata artifacts so a folder conversion does not unexpectedly
        // materialize audio outside the CUE's sibling file set. Explicit CUE
        // selection is still honored by `into_queue_paths()`.
        if !is_same_directory_key_for_queue(&parent_key, &resolved) {
            return Err(format!(
                "track {} FILE reference {:?} resolved outside the CUE directory: {}",
                track.number,
                file_ref,
                resolved.display()
            ));
        }

        let resolved_key = queue_path_key(&resolved);
        if referenced_audio_keys.insert(resolved_key.clone()) {
            referenced_audio.push(resolved.clone());
        }
        *track_count_by_audio_key.entry(resolved_key).or_insert(0) += 1;
        resolved_tracks.push((track.number, resolved, index01));
    }

    validate_queue_cue_index_order(&resolved_tracks)?;

    Ok(CueQueueAnalysis {
        referenced_audio,
        track_count_by_audio_key,
    })
}

fn validate_queue_cue_index_order(resolved_tracks: &[(u32, PathBuf, u32)]) -> Result<(), String> {
    let mut previous_by_file: BTreeMap<PathBuf, (u32, u32)> = BTreeMap::new();
    for (track_number, path, index01) in resolved_tracks {
        let key = queue_path_key(path);
        if let Some((previous_track, previous_index)) = previous_by_file.get(&key) {
            if index01 <= previous_index {
                return Err(format!(
                    "non-increasing INDEX 01 for track {} in {}; previous track {} was at frame {}",
                    track_number,
                    path.display(),
                    previous_track,
                    previous_index
                ));
            }
        }
        previous_by_file.insert(key, (*track_number, *index01));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum CueReferenceResolution {
    Resolved(PathBuf),
    Missing,
    Ambiguous(Vec<PathBuf>),
}

pub(crate) fn resolve_cue_file_reference_for_queue(parent: &Path, file_ref: &str) -> CueReferenceResolution {
    let normalized_ref = file_ref.replace('\\', &std::path::MAIN_SEPARATOR.to_string());
    let raw_path = PathBuf::from(&normalized_ref);

    if raw_path.is_absolute() && raw_path.is_file() {
        return CueReferenceResolution::Resolved(raw_path);
    }

    let direct = parent.join(&raw_path);
    if direct.is_file() {
        return CueReferenceResolution::Resolved(direct);
    }

    let wanted_name = raw_path.file_name().and_then(|value| value.to_str());
    let wanted_stem = raw_path.file_stem().and_then(|value| value.to_str());
    let fallback_search_dir = cue_reference_fallback_search_dir(parent, &raw_path);

    if let Some(wanted) = wanted_name {
        let name_matches = collect_audio_reference_candidates(&fallback_search_dir, |path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(wanted))
                .unwrap_or(false)
        });
        match unique_queue_reference_candidate(name_matches) {
            CueReferenceResolution::Missing => {}
            other => return other,
        }
    }

    if let Some(wanted) = wanted_stem {
        let stem_matches = collect_audio_reference_candidates(&fallback_search_dir, |path| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(wanted))
                .unwrap_or(false)
        });
        return unique_queue_reference_candidate(stem_matches);
    }

    CueReferenceResolution::Missing
}


fn cue_reference_fallback_search_dir(parent: &Path, raw_path: &Path) -> PathBuf {
    raw_path
        .parent()
        .filter(|component| !component.as_os_str().is_empty())
        .map(|component| parent.join(component))
        .unwrap_or_else(|| parent.to_path_buf())
}

fn collect_audio_reference_candidates(
    parent: &Path,
    matches_reference: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };

    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_audio_file_path(path) && matches_reference(path))
        .collect();
    candidates.sort_by_key(|path| deterministic_path_sort_key(path));
    let mut seen = HashSet::new();
    candidates.retain(|path| seen.insert(queue_path_key(path)));
    candidates
}

fn unique_queue_reference_candidate(candidates: Vec<PathBuf>) -> CueReferenceResolution {
    match candidates.len() {
        0 => CueReferenceResolution::Missing,
        1 => CueReferenceResolution::Resolved(candidates.into_iter().next().unwrap()),
        _ => CueReferenceResolution::Ambiguous(candidates),
    }
}

fn deterministic_path_sort_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

/// Stable key for queue path comparisons.
///
/// Existing files are canonicalized once before set/map operations so queue
/// expansion avoids repeated filesystem lookups in inner loops. The fallback
/// preserves the old behavior for paths that cannot be canonicalized.
fn queue_path_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn format_candidate_paths_for_log(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn push_unique_path_with_keys(
    paths: &mut Vec<PathBuf>,
    keys: &mut HashSet<PathBuf>,
    candidate: PathBuf,
) {
    if keys.insert(queue_path_key(&candidate)) {
        paths.push(candidate);
    }
}

#[cfg(test)]
fn path_list_contains(paths: &[PathBuf], candidate: &Path) -> bool {
    paths
        .iter()
        .any(|existing| same_path_for_queue(existing, candidate))
}

#[cfg(test)]
fn same_path_for_queue(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn is_same_directory_key_for_queue(left_key: &Path, right_file: &Path) -> bool {
    let Some(right) = right_file.parent() else {
        return false;
    };

    queue_path_key(right) == left_key
}


fn classify_dvda_directory_entry(
    entry: &mut BrowseEntry,
    cache: &mut HashMap<PathBuf, (ClassificationFingerprint, bool)>,
) {
    if !matches!(entry.kind, EntryKind::Directory | EntryKind::DvdAudioDir) {
        return;
    }
    let marker = entry.path.join("AUDIO_TS").join("AUDIO_TS.IFO");
    let fingerprint = std::fs::metadata(&marker)
        .ok()
        .map(|m| ClassificationFingerprint { len: m.len(), modified: m.modified().ok() })
        .unwrap_or_else(|| ClassificationFingerprint::from_entry(entry));

    let is_dvda = cache
        .get(&entry.path)
        .filter(|(cached, _)| *cached == fingerprint)
        .map(|(_, verdict)| *verdict)
        .unwrap_or_else(|| {
            let verdict = crate::disc::dvda_utils::is_dvda_directory(&entry.path);
            cache.insert(entry.path.clone(), (fingerprint.clone(), verdict));
            verdict
        });
    if is_dvda {
        entry.kind = EntryKind::DvdAudioDir;
    } else if matches!(entry.kind, EntryKind::DvdAudioDir) {
        entry.kind = EntryKind::Directory;
    }
}

fn classify_dvdv_directory_entry(
    entry: &mut BrowseEntry,
    cache: &mut HashMap<PathBuf, (ClassificationFingerprint, bool)>,
) {
    if !matches!(entry.kind, EntryKind::Directory | EntryKind::DvdVideoDir) {
        return;
    }
    let marker = crate::disc::dvdv_utils::directory_video_ts_file_path(&entry.path, "VIDEO_TS.IFO");
    let fingerprint = marker
        .as_ref()
        .and_then(|marker| std::fs::metadata(marker).ok())
        .map(|m| ClassificationFingerprint { len: m.len(), modified: m.modified().ok() })
        .unwrap_or_else(|| ClassificationFingerprint::from_entry(entry));

    let is_dvdv = cache
        .get(&entry.path)
        .filter(|(cached, _)| *cached == fingerprint)
        .map(|(_, verdict)| *verdict)
        .unwrap_or_else(|| {
            let verdict = crate::disc::dvdv_utils::is_dvdv_directory(&entry.path);
            cache.insert(entry.path.clone(), (fingerprint.clone(), verdict));
            verdict
        });
    if is_dvdv {
        entry.kind = EntryKind::DvdVideoDir;
    } else if matches!(entry.kind, EntryKind::DvdVideoDir) {
        entry.kind = EntryKind::Directory;
    }
}

fn is_audio_filter_visible_entry(entry: &BrowseEntry) -> bool {
    matches!(entry.kind, EntryKind::AudioFile(_) | EntryKind::SacdIso | EntryKind::DvdAudioIso | EntryKind::DvdAudioDir | EntryKind::DvdVideoIso | EntryKind::DvdVideoDir | EntryKind::Directory)
        || is_cue_sheet_path(&entry.path)
}

pub(super) fn is_cue_sheet_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("cue"))
        .unwrap_or(false)
}

fn is_audio_file_path(path: &Path) -> bool {
    matches!(classify_file(path), EntryKind::AudioFile(_))
}

/// A file is queueable for conversion if it's an audio file, a CUE sheet,
/// a supported archive (7z), or a valid SACD ISO. Generic ISOs, zips, rars,
/// etc. that the pipeline can't handle are excluded to avoid noisy queue errors.
fn is_queueable_file(path: &Path) -> bool {
    if is_cue_sheet_path(path) {
        return true;
    }

    let kind = classify_file(path);
    match kind {
        EntryKind::AudioFile(_) => true,
        EntryKind::Archive => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase());
            match ext.as_deref() {
                // 7z archives are always queueable (pipeline supports them).
                Some("7z") => true,
                // ISOs are only queueable if they're SACD ISOs.
                Some("iso") => crate::tui::sacd::is_sacd_iso(path)
                    || crate::disc::dvda_utils::is_dvda_iso(path)
                    || crate::disc::dvdv_utils::is_dvdv_iso(path),
                // Other archive formats (zip, rar, tar, etc.) are not
                // supported by the conversion pipeline.
                _ => false,
            }
        }
        _ => false,
    }
}

pub(super) fn classify_file(path: &Path) -> EntryKind {
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
        Some("7z") | Some("zip") | Some("rar") | Some("tar") | Some("iso") | Some("cab")
        | Some("dmg") | Some("tgz") | Some("tbz2") | Some("txz") => EntryKind::Archive,
        _ => EntryKind::OtherFile,
    }
}

/// Text file extensions that can be viewed in the built-in viewer.
const VIEWABLE_TEXT_EXTENSIONS: &[&str] = &[
    "cue", "log", "nfo", "txt", "json", "html", "htm", "xml", "md", "yaml", "yml", "toml", "ini",
    "cfg", "conf", "m3u", "m3u8",
];

/// Check if a file is a viewable text file (by extension).
pub fn is_viewable_text_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIEWABLE_TEXT_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Check if a file is an editable text file. Same as viewable but
/// excludes `.log` files (rip integrity records should not be modified).
pub fn is_editable_text_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_lowercase();
            lower != "log" && VIEWABLE_TEXT_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

/// Derive a short display label for an archive from its extension.
fn archive_label(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_lowercase())
        .unwrap_or_default();
    // Check compound extensions first.
    if name.ends_with(".tar.gz") {
        return "tar.gz".into();
    }
    if name.ends_with(".tar.bz2") {
        return "tar.bz2".into();
    }
    if name.ends_with(".tar.xz") {
        return "tar.xz".into();
    }
    if name.ends_with(".tar.zst") {
        return "tar.zst".into();
    }
    if name.ends_with(".tar.lz") {
        return "tar.lz".into();
    }
    if name.ends_with(".tar.lzma") {
        return "tar.lzma".into();
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    /// Build an `all_files` list with one Archive entry pointing at
    /// `path`, mtime taken from the file. Other BrowseState fields
    /// stay at their defaults via `BrowseState::new()` then we
    /// overwrite the relevant bits.
    fn make_browse_with_iso(path: &std::path::Path) -> BrowseState {
        let mut state = BrowseState::new();
        state.all_files.clear();
        state.sacd_classify_cache.clear();
        let meta = std::fs::metadata(path).expect("metadata");
        let modified = meta.modified().ok();
        state.all_files.push(BrowseEntry::new(
            path.to_path_buf(),
            path.file_name().unwrap().to_string_lossy().into_owned(),
            EntryKind::Archive,
            meta.len(),
            modified,
        ));
        state
    }

    #[test]
    fn upgrade_iso_kinds_marks_sacd_iso() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("disc.iso");

        // Build a file with the ScarletBook magic at LSN 510.
        let total = (crate::tui::sacd::MASTER_TOC_LSNS[0] + 1) * crate::tui::sacd::SECTOR_SIZE;
        let mut f = std::fs::File::create(&path).unwrap();
        f.set_len(total).unwrap();
        f.seek(SeekFrom::Start(
            crate::tui::sacd::MASTER_TOC_LSNS[0] * crate::tui::sacd::SECTOR_SIZE,
        ))
        .unwrap();
        f.write_all(crate::tui::sacd::MASTER_TOC_MAGIC).unwrap();
        drop(f);

        let mut state = make_browse_with_iso(&path);
        assert!(matches!(state.all_files[0].kind, EntryKind::Archive));

        state.upgrade_iso_kinds();
        assert!(matches!(state.all_files[0].kind, EntryKind::SacdIso));
        assert!(state.sacd_classify_cache.contains_key(&path));
    }

    #[test]
    fn upgrade_iso_kinds_leaves_non_sacd_iso_alone() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("data.iso");
        // Plain large ISO with no ScarletBook magic.
        let total = (crate::tui::sacd::MASTER_TOC_LSNS[0] + 1) * crate::tui::sacd::SECTOR_SIZE;
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(total).unwrap();
        drop(f);

        let mut state = make_browse_with_iso(&path);
        state.upgrade_iso_kinds();
        assert!(matches!(state.all_files[0].kind, EntryKind::Archive));
        // Cached negative result so we don't re-probe next refresh.
        assert_eq!(
            state.sacd_classify_cache.get(&path).map(|(_, v)| *v),
            Some(false)
        );
    }

    #[test]
    fn upgrade_iso_kinds_skips_non_iso_archives() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("bundle.7z");
        std::fs::write(&path, b"not really 7z").unwrap();

        let mut state = make_browse_with_iso(&path);
        state.upgrade_iso_kinds();
        // .7z is Archive, never upgraded; cache is untouched.
        assert!(matches!(state.all_files[0].kind, EntryKind::Archive));
        assert!(state.sacd_classify_cache.is_empty());
    }

    #[test]
    fn upgrade_iso_kinds_uses_cache_on_second_call() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("disc.iso");
        let total = (crate::tui::sacd::MASTER_TOC_LSNS[0] + 1) * crate::tui::sacd::SECTOR_SIZE;
        let mut f = std::fs::File::create(&path).unwrap();
        f.set_len(total).unwrap();
        f.seek(SeekFrom::Start(
            crate::tui::sacd::MASTER_TOC_LSNS[0] * crate::tui::sacd::SECTOR_SIZE,
        ))
        .unwrap();
        f.write_all(crate::tui::sacd::MASTER_TOC_MAGIC).unwrap();
        drop(f);

        let mut state = make_browse_with_iso(&path);
        state.upgrade_iso_kinds();
        // Reset kind and re-run; cache should still classify it.
        state.all_files[0].kind = EntryKind::Archive;
        state.upgrade_iso_kinds();
        assert!(matches!(state.all_files[0].kind, EntryKind::SacdIso));
    }

    #[test]
    fn is_probeable_covers_audio_files_and_sacd_isos() {
        let entry_with_kind = |kind: EntryKind| {
            BrowseEntry::new(
                std::path::PathBuf::from("/tmp/x"),
                "x".to_string(),
                kind,
                0,
                None,
            )
        };

        // Probeable: audio files (any format) and SACD ISOs.
        assert!(entry_with_kind(EntryKind::AudioFile(AudioFormat::Flac)).is_probeable());
        assert!(entry_with_kind(EntryKind::AudioFile(AudioFormat::Wav)).is_probeable());
        assert!(entry_with_kind(EntryKind::AudioFile(AudioFormat::Mp3)).is_probeable());
        assert!(entry_with_kind(EntryKind::SacdIso).is_probeable());

        // Not probeable: directories, archives (data ISOs included here),
        // other files. The probe pipeline produces no useful output for
        // these and the InfoPane has no SourceMetadata to render.
        assert!(!entry_with_kind(EntryKind::Directory).is_probeable());
        assert!(!entry_with_kind(EntryKind::Archive).is_probeable());
        assert!(!entry_with_kind(EntryKind::OtherFile).is_probeable());
        assert!(!entry_with_kind(EntryKind::ParentDir).is_probeable());
    }

    #[test]
    fn expand_paths_to_audio_suppresses_child_directory_split_source_cue_by_design() {
        let td = tempfile::tempdir().expect("tempdir");
        let subdir = td.path().join("disc");
        std::fs::create_dir(&subdir).unwrap();
        let image = subdir.join("image.flac");
        let loose = td.path().join("loose.flac");
        let cue = td.path().join("album.cue");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(&loose, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "disc/image.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        // Same-directory references are required by design. This suppresses
        // some materializable layouts, such as `album.cue` + `disc/image.flac`,
        // in favor of queueing discovered audio files without crossing the
        // CUE directory boundary.
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &loose));
        assert!(path_list_contains(&expanded, &image));
    }

    #[test]
    fn expand_paths_to_audio_suppresses_cue_that_references_external_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue_dir = td.path().join("cue_dir");
        std::fs::create_dir(&cue_dir).unwrap();
        let external = td.path().join("external.flac");
        let cue = cue_dir.join("album.cue");
        std::fs::write(&external, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "../external.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[cue_dir, external.clone()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &external));
    }

    #[test]
    fn expand_paths_to_audio_suppresses_ambiguous_cue_and_keeps_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let flac = td.path().join("album.flac");
        let wav = td.path().join("album.wav");
        std::fs::write(&flac, b"not real flac").unwrap();
        std::fs::write(&wav, b"not real wav").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album.ape" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &flac));
        assert!(path_list_contains(&expanded, &wav));
    }

    #[test]
    fn expand_paths_to_audio_suppresses_cue_with_subdirectory_reference() {
        let td = tempfile::tempdir().expect("tempdir");
        let disc = td.path().join("disc");
        std::fs::create_dir(&disc).unwrap();
        let cue = td.path().join("album.cue");
        let image = disc.join("image.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "disc/image.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &image));
    }

    #[test]
    fn expand_paths_to_audio_suppresses_cue_missing_index01_and_keeps_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "No INDEX 01"
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &image));
    }

    #[test]
    fn expand_paths_to_audio_suppresses_non_increasing_cue_and_keeps_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:10:00
  TRACK 02 AUDIO
    INDEX 01 00:05:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &image));
    }


    #[test]
    fn materializable_cue_suppresses_per_track_cue_and_keeps_stem_matched_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let track1 = td.path().join("01.flac");
        let track2 = td.path().join("02.opus");
        std::fs::write(&track1, b"not real flac").unwrap();
        std::fs::write(&track2, b"not real opus").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "01.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
FILE "02.wav" WAVE
  TRACK 02 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("per-track CUE should be materializer-compatible");
        assert!(referenced.is_empty());

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &track1));
        assert!(path_list_contains(&expanded, &track2));
    }

    #[test]
    fn materializable_cue_suppresses_frostbite_style_per_track_cue() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("Frostbite.cue");
        let track1 = td.path().join("01 - If You Love Me Like You Say.flac");
        let track2 = td.path().join("02 - Blue Monday Hangover.flac");
        std::fs::write(&track1, b"not real flac").unwrap();
        std::fs::write(&track2, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "01 - If You Love Me Like You Say.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 00 04:06:70
FILE "02 - Blue Monday Hangover.wav" WAVE
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("Frostbite-style per-track CUE should be materializer-compatible");
        assert!(referenced.is_empty());

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &track1));
        assert!(path_list_contains(&expanded, &track2));
    }

    #[test]
    fn materializable_cue_returns_single_image_stem_matched_audio_for_suppression() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("single-image CUE should be materializer-compatible");
        assert_eq!(referenced.len(), 1);
        assert!(path_list_contains(&referenced, &image));

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &cue));
        assert!(!path_list_contains(&expanded, &image));
    }

    #[test]
    fn materializable_cue_returns_each_shared_multi_image_audio_for_suppression() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let side_a = td.path().join("side-a.flac");
        let side_b = td.path().join("side-b.wv");
        std::fs::write(&side_a, b"not real flac").unwrap();
        std::fs::write(&side_b, b"not real wavpack").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "side-a.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
  TRACK 03 AUDIO
    INDEX 01 07:24:00
FILE "side-b.wav" WAVE
  TRACK 04 AUDIO
    INDEX 01 00:00:00
  TRACK 05 AUDIO
    INDEX 01 04:00:00
  TRACK 06 AUDIO
    INDEX 01 08:00:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("multi-image CUE should be materializer-compatible");
        assert_eq!(referenced.len(), 2);
        assert!(path_list_contains(&referenced, &side_a));
        assert!(path_list_contains(&referenced, &side_b));

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &cue));
        assert!(!path_list_contains(&expanded, &side_a));
        assert!(!path_list_contains(&expanded, &side_b));
    }

    #[test]
    fn materializable_cue_with_any_shared_file_is_split_source_and_suppresses_all_references() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let main = td.path().join("album-main.flac");
        let bonus = td.path().join("09 - Bonus Track.flac");
        let live = td.path().join("10 - Live Version.aac");
        std::fs::write(&main, b"not real flac").unwrap();
        std::fs::write(&bonus, b"not real flac").unwrap();
        std::fs::write(&live, b"not real aac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album-main.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:00:00
  TRACK 03 AUDIO
    INDEX 01 06:00:00
  TRACK 04 AUDIO
    INDEX 01 09:00:00
  TRACK 05 AUDIO
    INDEX 01 12:00:00
  TRACK 06 AUDIO
    INDEX 01 15:00:00
  TRACK 07 AUDIO
    INDEX 01 18:00:00
  TRACK 08 AUDIO
    INDEX 01 21:00:00
FILE "09 - Bonus Track.wav" WAVE
  TRACK 09 AUDIO
    INDEX 01 00:00:00
FILE "10 - Live Version.wav" WAVE
  TRACK 10 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("mixed-layout CUE should be materializer-compatible");
        assert_eq!(referenced.len(), 3);
        assert!(path_list_contains(&referenced, &main));
        assert!(path_list_contains(&referenced, &bonus));
        assert!(path_list_contains(&referenced, &live));

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &cue));
        assert!(!path_list_contains(&expanded, &main));
        assert!(!path_list_contains(&expanded, &bonus));
        assert!(!path_list_contains(&expanded, &live));
        assert!(expanded.cue_artifact_audio.is_empty());
    }


    #[test]
    fn expand_paths_to_audio_queues_tracks_and_suppresses_twelve_track_cue_artifact() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let mut cue_text = String::new();
        let mut tracks = Vec::new();

        for number in 1..=12 {
            let stem = format!("{number:02}");
            let audio = td.path().join(format!("{stem}.flac"));
            std::fs::write(&audio, b"not real flac").unwrap();
            tracks.push(audio);
            cue_text.push_str(&format!(
                "FILE \"{stem}.wav\" WAVE\n  TRACK {number:02} AUDIO\n    INDEX 01 00:00:00\n"
            ));
        }

        std::fs::write(&cue, cue_text).unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("per-track CUE should be materializer-compatible");
        assert!(referenced.is_empty());

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert_eq!(expanded.len(), tracks.len());
        assert!(!path_list_contains(&expanded, &cue));
        for track in tracks {
            assert!(path_list_contains(&expanded, &track));
            assert!(
                expanded.cue_artifact_audio.contains(&track),
                "per-track audio must carry EmbeddedOnly override metadata"
            );
        }
    }

    #[test]
    fn expand_paths_to_audio_marks_sibling_audio_when_nonexplicit_cue_errors() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("broken.cue");
        let track1 = td.path().join("01.flac");
        let track2 = td.path().join("02.flac");
        let nested = td.path().join("nested");
        let nested_track = nested.join("03.flac");

        std::fs::create_dir(&nested).unwrap();
        std::fs::write(&track1, b"not real flac").unwrap();
        std::fs::write(&track2, b"not real flac").unwrap();
        std::fs::write(&nested_track, b"not real flac").unwrap();
        std::fs::write(&cue, "this is not a cue sheet").unwrap();

        let expanded = expand_paths_to_audio_with_metadata(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &track1));
        assert!(path_list_contains(&expanded, &track2));
        assert!(path_list_contains(&expanded, &nested_track));
        assert!(
            expanded.cue_artifact_audio.contains(&track1),
            "audio next to an error-classified non-explicit CUE must carry EmbeddedOnly override metadata"
        );
        assert!(
            expanded.cue_artifact_audio.contains(&track2),
            "audio next to an error-classified non-explicit CUE must carry EmbeddedOnly override metadata"
        );
        assert!(
            !expanded.cue_artifact_audio.contains(&nested_track),
            "CUE error fallback only applies to sibling audio that downstream sidecar discovery could associate with the CUE"
        );
    }

    #[test]
    fn expand_paths_to_audio_queues_side_cues_and_suppresses_side_images() {
        let td = tempfile::tempdir().expect("tempdir");
        let side_a_cue = td.path().join("side_a.cue");
        let side_b_cue = td.path().join("side_b.cue");
        let side_a = td.path().join("side_a.wav");
        let side_b = td.path().join("side_b.wav");
        std::fs::write(&side_a, b"not real wav").unwrap();
        std::fs::write(&side_b, b"not real wav").unwrap();
        std::fs::write(
            &side_a_cue,
            r#"FILE "side_a.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:00:00
"#,
        )
        .unwrap();
        std::fs::write(
            &side_b_cue,
            r#"FILE "side_b.wav" WAVE
  TRACK 03 AUDIO
    INDEX 01 00:00:00
  TRACK 04 AUDIO
    INDEX 01 04:00:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &side_a_cue));
        assert!(path_list_contains(&expanded, &side_b_cue));
        assert!(!path_list_contains(&expanded, &side_a));
        assert!(!path_list_contains(&expanded, &side_b));
    }

    #[test]
    fn expand_paths_to_audio_suppresses_unparseable_cue_and_keeps_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("broken.cue");
        let audio = td.path().join("track.flac");
        std::fs::write(&cue, b"this is not a cue sheet").unwrap();
        std::fs::write(&audio, b"not real flac").unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &audio));
    }

    #[test]
    fn expand_paths_to_audio_always_queues_explicit_cue_selection() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("broken.cue");
        std::fs::write(&cue, b"this is not a cue sheet").unwrap();

        let expanded = expand_paths_to_audio(&[cue.clone()]);
        assert_eq!(expanded.len(), 1);
        assert!(path_list_contains(&expanded, &cue));
    }


    #[test]
    fn split_source_cue_resolves_case_insensitive_stem_matched_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.FLAC");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "ALBUM.WAV" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("case-insensitive stem match should resolve the image");
        assert_eq!(referenced.len(), 1);
        assert!(path_list_contains(&referenced, &image));

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &cue));
        assert!(!path_list_contains(&expanded, &image));
    }

    #[test]
    fn two_split_source_cues_can_reference_same_image_and_suppress_it_once() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue_a = td.path().join("album-main.cue");
        let cue_b = td.path().join("album-alt.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue_a,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:00:00
"#,
        )
        .unwrap();
        std::fs::write(
            &cue_b,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 04:00:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &cue_a));
        assert!(path_list_contains(&expanded, &cue_b));
        assert!(!path_list_contains(&expanded, &image));
    }

    #[test]
    fn split_source_cue_suppresses_audio_shared_with_artifact_cue() {
        let td = tempfile::tempdir().expect("tempdir");
        let split_cue = td.path().join("album.cue");
        let artifact_cue = td.path().join("album-index.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &split_cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:00:00
"#,
        )
        .unwrap();
        std::fs::write(
            &artifact_cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert!(path_list_contains(&expanded, &split_cue));
        assert!(!path_list_contains(&expanded, &artifact_cue));
        assert!(!path_list_contains(&expanded, &image));
    }

    #[test]
    fn one_track_image_cue_is_metadata_artifact_by_design() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let referenced = cue_referenced_audio_paths_to_suppress_for_queue(&cue)
            .expect("one-track image CUE should parse but provide no split points");
        assert!(referenced.is_empty());

        let expanded = expand_paths_to_audio(&[td.path().to_path_buf()]);
        assert_eq!(expanded.len(), 1);
        assert!(!path_list_contains(&expanded, &cue));
        assert!(path_list_contains(&expanded, &image));
    }

    #[test]
    fn explicit_split_source_cue_suppresses_explicit_audio_by_design() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .unwrap();

        let expanded = expand_paths_to_audio(&[cue.clone(), image.clone()]);
        assert_eq!(expanded.len(), 1);
        assert!(path_list_contains(&expanded, &cue));
        assert!(!path_list_contains(&expanded, &image));
    }

}
