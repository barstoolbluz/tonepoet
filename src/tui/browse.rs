//! File browser state and directory scanning

use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
use std::time::{Duration, Instant, SystemTime};

use ratatui::layout::Rect;

/// Type-ahead buffer resets after this duration of inactivity.
const TYPE_AHEAD_TIMEOUT: Duration = Duration::from_millis(1500);

/// Cold filesystem audio probes are delayed briefly while the cursor is moving.
/// Cache hits still publish immediately, so steady-state browsing stays instant
/// while rapid scrolls avoid launching probes for entries the user only crossed.
const BROWSE_PROBE_DEBOUNCE: Duration = Duration::from_millis(175);

/// Keep scan-completion cache warming bounded. Cursor-focused lookups still
/// fall back to per-file SQLite/fresh probes for entries beyond the warm set.
const PROBE_CACHE_WARM_MAX_CANDIDATES: usize = 4096;
const PROBE_CACHE_WARM_MESSAGE_CHUNK: usize = 128;
const PROBE_CACHE_WARM_MERGE_MAX_PER_FRAME: usize = 128;
/// Cold filesystem probes are expensive and cannot be cancelled once the
/// blocking ffmpeg/lofty work has started. Keep only a tiny number in flight
/// and queue the cursor-focused request ahead of any stale pre-start work.
const BROWSE_COLD_PROBE_MAX_IN_FLIGHT: usize = 2;
const BROWSE_COLD_PROBE_QUEUE_MAX: usize = 16;
/// Recursive directory stats walk subtrees and can be expensive on large
/// libraries. Keep at most one active from Browse cursor movement and queue
/// only the current directory selection ahead of stale hover/cursor positions.
const BROWSE_DIR_STATS_MAX_IN_FLIGHT: usize = 1;
const BROWSE_DIR_STATS_QUEUE_MAX: usize = 8;
/// Short in-memory retry backoff for transient probe failures. This is not a
/// correctness cache: it only prevents hot retry loops for files that may
/// become readable or probeable without a content change.
const TRANSIENT_PROBE_FAILURE_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Skim fuzzy matching is intentionally permissive: any ordered subsequence can
/// produce a score. Browse search treats very low scores as false positives so
/// long filenames cannot match short queries solely because the query characters
/// appear far apart. The floor scales with the non-whitespace query length so
/// short queries still work while longer queries need proportionally stronger
/// evidence.
const SEARCH_FUZZY_MIN_SCORE_PER_QUERY_CHAR: i64 = 10;

fn search_fuzzy_min_score(query: &str) -> i64 {
    let significant_chars = query.chars().filter(|ch| !ch.is_whitespace()).count() as i64;
    significant_chars * SEARCH_FUZZY_MIN_SCORE_PER_QUERY_CHAR
}

fn search_fuzzy_score_passes_threshold(score: i64, min_score: i64) -> bool {
    score >= min_score
}

fn search_score_better(candidate: i64, incumbent: i64, dir: SortDir) -> bool {
    match dir {
        SortDir::Asc => candidate < incumbent,
        SortDir::Desc => candidate > incumbent,
    }
}

struct BoundedScoreSearchResults {
    heap: BinaryHeap<RetainedScoreResult>,
    cap: usize,
    dir: SortDir,
    next_seq: usize,
}

struct RetainedScoreResult {
    entry: BrowseEntry,
    score: i64,
    seq: usize,
    dir: SortDir,
}

impl PartialEq for RetainedScoreResult {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score && self.seq == other.seq && self.dir == other.dir
    }
}

impl Eq for RetainedScoreResult {}

impl PartialOrd for RetainedScoreResult {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RetainedScoreResult {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let score_order = match self.dir {
            // BinaryHeap exposes the greatest item. Make the root the worst
            // retained candidate so replacement is O(log cap) instead of a
            // linear scan across the retained set for every broad match.
            SortDir::Desc => other.score.cmp(&self.score),
            SortDir::Asc => self.score.cmp(&other.score),
        };
        // Equal scores keep the older match, matching the previous full-sort
        // behavior as closely as possible. Newer equal-score entries are worse
        // if the heap ever has to choose among them.
        score_order.then_with(|| self.seq.cmp(&other.seq))
    }
}

impl BoundedScoreSearchResults {
    fn new(cap: usize, dir: SortDir) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(cap),
            cap,
            dir,
            next_seq: 0,
        }
    }

    fn push(&mut self, entry: BrowseEntry, score: i64) {
        if self.cap == 0 {
            return;
        }
        let candidate = RetainedScoreResult {
            entry,
            score,
            seq: self.next_seq,
            dir: self.dir,
        };
        self.next_seq = self.next_seq.saturating_add(1);

        if self.heap.len() < self.cap {
            self.heap.push(candidate);
            return;
        }

        let Some(worst) = self.heap.peek() else {
            self.heap.push(candidate);
            return;
        };
        if search_score_better(candidate.score, worst.score, self.dir) {
            let mut slot = self.heap.peek_mut().expect("peek succeeded above");
            *slot = candidate;
        }
    }

    fn into_vec(self) -> Vec<(BrowseEntry, i64)> {
        let mut retained = self.heap.into_vec();
        retained.sort_by_key(|item| item.seq);
        retained
            .into_iter()
            .map(|item| (item.entry, item.score))
            .collect()
    }
}

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

/// File identity captured alongside in-memory probe-cache entries.
///
/// The persistent SQLite cache is validated by `(path, mtime, size)`, and the
/// in-memory layer must obey the same contract. Keeping this fingerprint with
/// each hit/failure prevents externally modified files from reusing stale
/// metadata after a refresh or a focused metadata check observes a new identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeCacheIdentity {
    pub modified: Option<SystemTime>,
    pub size: u64,
}

impl ProbeCacheIdentity {
    pub fn from_entry(entry: &BrowseEntry) -> Self {
        Self {
            modified: entry.modified,
            size: entry.size,
        }
    }

    pub fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            modified: metadata.modified().ok(),
            size: metadata.len(),
        }
    }
}

/// Identity-validated in-memory probe-cache value. `info = None` records only
/// deterministic negatives such as "not audio" for the same file identity.
/// Transient failures use a short retry backoff instead and are never persisted.
#[derive(Debug, Clone)]
pub struct ProbeCacheEntry {
    pub identity: ProbeCacheIdentity,
    pub info: Option<Arc<CachedInfo>>,
}

impl ProbeCacheEntry {
    pub fn hit(identity: ProbeCacheIdentity, info: Arc<CachedInfo>) -> Self {
        Self {
            identity,
            info: Some(info),
        }
    }

    pub fn miss(identity: ProbeCacheIdentity) -> Self {
        Self { identity, info: None }
    }

    pub fn is_valid_for(&self, identity: ProbeCacheIdentity) -> bool {
        self.identity == identity
    }
}

/// Row emitted by the asynchronous SQLite probe-cache warmer. Rows carry the
/// scan-time identity they were validated against; the reducer checks the
/// directory generation/path before merging them into `probe_cache`.
#[derive(Debug, Clone)]
pub struct ProbeCacheWarmRow {
    pub path: PathBuf,
    pub identity: ProbeCacheIdentity,
    pub info: CachedInfo,
}

#[derive(Debug, Clone)]
struct ProbeCacheWarmBatch {
    generation: u64,
    path: PathBuf,
    rows: VecDeque<ProbeCacheWarmRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowseColdProbeRequest {
    path: PathBuf,
    identity: ProbeCacheIdentity,
    /// Directory scan generation captured when the request was queued.
    /// Low-priority/background work is dropped before launch if navigation or
    /// refresh has moved Browse to a newer generation.
    scan_generation: u64,
    /// Cursor-focused requests are high priority. They are dropped before
    /// launch if the cursor has moved away, so old scroll positions cannot
    /// accumulate behind already-running probes.
    cursor_focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirStatsRequest {
    path: PathBuf,
    identity: ProbeCacheIdentity,
    scan_generation: u64,
    cursor_focused: bool,
}

#[derive(Debug, Clone)]
struct DirStatsActiveJob {
    identity: ProbeCacheIdentity,
    scan_generation: u64,
    cursor_focused: bool,
    cancel: Arc<AtomicBool>,
}

/// Coalesced Browse reducer work. Async completions set these flags while the
/// event loop drains messages, and the loop performs each expensive operation
/// at most once after the batch.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BrowseDeferredWorkFlags {
    pub probe_backed_resort_needed: bool,
    pub search_reapply_needed: bool,
    pub visible_entries_changed: bool,
    pub info_pane_changed: bool,
    pub classification_changed: bool,
}

impl BrowseDeferredWorkFlags {
    pub fn has_expensive_work(self) -> bool {
        self.probe_backed_resort_needed
            || self.search_reapply_needed
            || self.visible_entries_changed
            || self.info_pane_changed
            || self.classification_changed
    }
}

/// Reducer decision for filesystem-backed async completions. Every Browse
/// worker that can update state after navigation or external file mutation
/// must prove that the current filesystem identity still matches the identity
/// captured at dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemAsyncCompletion {
    /// The file or directory still has the launch identity; the reducer may
    /// apply the result.
    Accept,
    /// The path still exists but no longer matches the launch identity. The
    /// reducer must drop the stale result, invalidate derived state, and may
    /// re-evaluate the current selection once.
    Changed,
    /// The path disappeared or cannot be statted. The reducer must clear
    /// pending/cache state and must not schedule another probe from this stale
    /// completion.
    MissingOrUnstatable,
}

/// Cached statistics for a directory
#[derive(Debug, Clone, Default)]
pub struct DirStats {
    pub file_count: usize,
    pub audio_count: usize,
    pub total_size: u64,
}

/// Identity-validated recursive directory-stats cache value. Directory stats
/// are computed asynchronously and can arrive after external filesystem
/// mutation; keeping the launch identity with the result prevents stale subtree
/// totals from being shown for a changed directory path.
#[derive(Debug, Clone)]
pub struct DirStatsCacheEntry {
    pub identity: ProbeCacheIdentity,
    pub stats: Arc<DirStats>,
}

impl DirStatsCacheEntry {
    pub fn new(identity: ProbeCacheIdentity, stats: DirStats) -> Self {
        Self {
            identity,
            stats: Arc::new(stats),
        }
    }

    pub fn is_valid_for(&self, identity: ProbeCacheIdentity) -> bool {
        self.identity == identity
    }
}

/// Debounced cold-probe request for the current Browse cursor.
#[derive(Debug, Clone)]
pub struct BrowseProbeDebounce {
    pub path: PathBuf,
    pub deadline: Instant,
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
    /// Blu-ray ISO image.
    BlurayIso,
    /// Filesystem Blu-ray directory (contains BDMV/).
    BlurayDir,
    /// Any other file
    OtherFile,
}

/// Metadata fingerprint for bounded classification caches.
///
/// Simple file-like entries compare by length and mtime. Directory formats may
/// attach marker fingerprints so cached negative classifications become stale
/// when any detection-relevant child appears or changes.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassificationFingerprint {
    pub len: u64,
    pub modified: Option<std::time::SystemTime>,
    pub markers: Vec<ClassificationMarkerFingerprint>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassificationMarkerFingerprint {
    pub label: &'static str,
    pub path: Option<PathBuf>,
    pub len: Option<u64>,
    pub modified: Option<std::time::SystemTime>,
}

impl ClassificationFingerprint {
    pub fn from_entry(entry: &BrowseEntry) -> Self {
        Self {
            len: entry.size,
            modified: entry.modified,
            markers: Vec::new(),
        }
    }

    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            markers: Vec::new(),
        }
    }
}


/// Negative classification/probe results are cached only when the failure is
/// deterministic for the captured file or directory identity. Permission,
/// locking, disappearance, cancellation, and decoder/extraction failures are
/// intentionally treated as transient so they cannot poison long-lived caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegativeCacheDecision {
    CacheDeterministic,
    DoNotCacheTransient,
}

/// Classification cache updates produced by the background directory-scan
/// worker. The reducer applies them only after the scan generation/path has
/// validated, keeping cold ISO/DVD-A/DVD-Video/Blu-ray checks off the TUI path
/// while preserving the same pre-publication entry kinds.
#[derive(Debug, Clone, Default)]
pub struct BrowseClassificationCacheUpdates {
    pub sacd_iso: Vec<(PathBuf, ClassificationFingerprint, bool)>,
    pub dvda_iso: Vec<(PathBuf, ClassificationFingerprint, bool)>,
    pub dvdv_iso: Vec<(PathBuf, ClassificationFingerprint, bool)>,
    pub bluray_iso: Vec<(PathBuf, ClassificationFingerprint, bool)>,
    pub dvda_dir: Vec<(PathBuf, ClassificationFingerprint, bool)>,
    pub dvdv_dir: Vec<(PathBuf, ClassificationFingerprint, bool)>,
    pub bluray_dir: Vec<(PathBuf, ClassificationFingerprint, bool)>,
}

#[derive(Debug, Clone, Default)]
struct BrowseClassificationCacheSnapshot {
    sacd_iso: HashMap<PathBuf, (ClassificationFingerprint, bool)>,
    dvda_iso: HashMap<PathBuf, (ClassificationFingerprint, bool)>,
    dvdv_iso: HashMap<PathBuf, (ClassificationFingerprint, bool)>,
    bluray_iso: HashMap<PathBuf, (ClassificationFingerprint, bool)>,
    dvda_dir: HashMap<PathBuf, (ClassificationFingerprint, bool)>,
    dvdv_dir: HashMap<PathBuf, (ClassificationFingerprint, bool)>,
    bluray_dir: HashMap<PathBuf, (ClassificationFingerprint, bool)>,
}

impl BrowseClassificationCacheSnapshot {
    fn from_state(state: &BrowseState) -> Self {
        Self {
            sacd_iso: state.sacd_classify_cache.clone(),
            dvda_iso: state.dvda_iso_classify_cache.clone(),
            dvdv_iso: state.dvdv_iso_classify_cache.clone(),
            bluray_iso: state.bluray_iso_classify_cache.clone(),
            dvda_dir: state.dvda_dir_classify_cache.clone(),
            dvdv_dir: state.dvdv_dir_classify_cache.clone(),
            bluray_dir: state.bluray_dir_classify_cache.clone(),
        }
    }
}

/// Sort field for browse listings. Probe-backed fields are valid sort keys
/// too; unknown/unprobed values sort after known values in ascending order and
/// then fall back to entry name for deterministic results.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortBy {
    Name,
    Date,
    Type,
    Size,
    Format,
    Codec,
    SampleRate,
    Channels,
    Duration,
    Artist,
    Album,
}

impl SortBy {
    pub const ALL: [Self; 11] = [
        Self::Name,
        Self::Size,
        Self::Date,
        Self::Type,
        Self::Format,
        Self::Codec,
        Self::SampleRate,
        Self::Channels,
        Self::Duration,
        Self::Artist,
        Self::Album,
    ];

    pub fn next(&self) -> Self {
        let idx = Self::ALL.iter().position(|candidate| candidate == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Date => "date",
            Self::Type => "type",
            Self::Size => "size",
            Self::Format => "format",
            Self::Codec => "codec",
            Self::SampleRate => "sample_rate",
            Self::Channels => "channels",
            Self::Duration => "duration",
            Self::Artist => "artist",
            Self::Album => "album",
        }
    }

    pub fn display_label(&self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Date => "Date",
            Self::Type => "Type",
            Self::Size => "Size",
            Self::Format => "Format",
            Self::Codec => "Codec",
            Self::SampleRate => "Sample rate",
            Self::Channels => "Channels",
            Self::Duration => "Duration",
            Self::Artist => "Artist",
            Self::Album => "Album",
        }
    }

    pub fn from_label(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace(' ', "_").replace('-', "_").as_str() {
            "name" => Some(Self::Name),
            "date" | "modified" | "mtime" => Some(Self::Date),
            "type" | "extension" => Some(Self::Type),
            "size" => Some(Self::Size),
            "format" => Some(Self::Format),
            "codec" => Some(Self::Codec),
            "sample_rate" | "samplerate" => Some(Self::SampleRate),
            "channels" => Some(Self::Channels),
            "duration" => Some(Self::Duration),
            "artist" => Some(Self::Artist),
            "album" => Some(Self::Album),
            _ => None,
        }
    }

    pub fn uses_probe_cache(&self) -> bool {
        matches!(
            self,
            Self::Format
                | Self::Codec
                | Self::SampleRate
                | Self::Channels
                | Self::Duration
                | Self::Artist
                | Self::Album
        )
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

    pub fn from_label(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "asc" | "ascending" => Some(Self::Asc),
            "desc" | "descending" => Some(Self::Desc),
            _ => None,
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
    /// Full filter menu order. Keep this as the single source of truth for
    /// cycling, config restoration, and the Options → Filter submenu so the UI
    /// cannot silently omit formats supported elsewhere in Browse.
    pub fn menu_choices() -> Vec<Self> {
        vec![
            Self::Off,
            Self::AudioOnly,
            Self::Only(AudioFormat::Flac),
            Self::Only(AudioFormat::Opus),
            Self::Only(AudioFormat::Aac),
            Self::Only(AudioFormat::Mp3),
            Self::Only(AudioFormat::Alac),
            Self::Only(AudioFormat::Wav),
            Self::Only(AudioFormat::WavPack),
            Self::Only(AudioFormat::Aiff),
            Self::Only(AudioFormat::Dsf),
            Self::Only(AudioFormat::Dff),
            Self::Only(AudioFormat::Dts),
            Self::Only(AudioFormat::Ac3),
            Self::Only(AudioFormat::Ape),
            Self::Only(AudioFormat::Lpcm),
        ]
    }

    pub fn from_menu_index(index: usize) -> Option<Self> {
        Self::menu_choices().get(index).copied()
    }

    pub fn menu_label(&self) -> String {
        match self {
            Self::Off => "All files".to_string(),
            Self::AudioOnly => "Audio only".to_string(),
            Self::Only(fmt) => fmt.name().to_string(),
        }
    }

    /// Cycle to the next filter: Off → AudioOnly → each audio format → Off
    pub fn next(&self) -> Self {
        let choices = Self::menu_choices();
        let current = choices.iter().position(|choice| choice == self).unwrap_or(0);
        choices[(current + 1) % choices.len()]
    }

    pub fn label(&self) -> String {
        match self {
            Self::Off => "off".to_string(),
            Self::AudioOnly => "audio".to_string(),
            Self::Only(fmt) => fmt.name().to_string(),
        }
    }

    pub fn config_label(&self) -> String {
        match self {
            Self::Off => "all".to_string(),
            Self::AudioOnly => "audio".to_string(),
            Self::Only(fmt) => fmt.name().to_ascii_lowercase(),
        }
    }

    pub fn from_label(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace(' ', "_").replace('-', "_").as_str() {
            "all" | "off" => Some(Self::Off),
            "audio" | "audio_only" => Some(Self::AudioOnly),
            "flac" => Some(Self::Only(AudioFormat::Flac)),
            "opus" => Some(Self::Only(AudioFormat::Opus)),
            "aac" => Some(Self::Only(AudioFormat::Aac)),
            "mp3" => Some(Self::Only(AudioFormat::Mp3)),
            "alac" => Some(Self::Only(AudioFormat::Alac)),
            "wav" => Some(Self::Only(AudioFormat::Wav)),
            "wavpack" | "wv" => Some(Self::Only(AudioFormat::WavPack)),
            "aiff" => Some(Self::Only(AudioFormat::Aiff)),
            "dsf" => Some(Self::Only(AudioFormat::Dsf)),
            "dff" => Some(Self::Only(AudioFormat::Dff)),
            "dts" => Some(Self::Only(AudioFormat::Dts)),
            "ac3" => Some(Self::Only(AudioFormat::Ac3)),
            "ape" => Some(Self::Only(AudioFormat::Ape)),
            "lpcm" => Some(Self::Only(AudioFormat::Lpcm)),
            _ => None,
        }
    }

    /// Whether a given entry kind passes the filter when only the kind is known.
    /// Prefer `allows_entry` when the path is available so convertible control
    /// files such as `.cue` can participate in the audio filter without being
    /// misclassified as audio bytes.
    pub fn allows(&self, kind: &EntryKind) -> bool {
        match self {
            Self::Off => true,
            Self::AudioOnly => matches!(
                kind,
                EntryKind::AudioFile(_)
                    | EntryKind::SacdIso
                    | EntryKind::DvdAudioIso
                    | EntryKind::DvdAudioDir
                    | EntryKind::DvdVideoIso
                    | EntryKind::DvdVideoDir
                    | EntryKind::BlurayIso
                    | EntryKind::BlurayDir
            ),
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
                matches!(
                    entry.kind,
                    EntryKind::AudioFile(_)
                        | EntryKind::SacdIso
                        | EntryKind::DvdAudioIso
                        | EntryKind::DvdAudioDir
                        | EntryKind::DvdVideoIso
                        | EntryKind::DvdVideoDir
                        | EntryKind::BlurayIso
                        | EntryKind::BlurayDir
                )
                    || is_cue_sheet_path(&entry.path)
            }
            Self::Only(fmt) => matches!(&entry.kind, EntryKind::AudioFile(f) if f == fmt),
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

/// Fingerprint used to validate the local tag-search cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagCacheFingerprint {
    pub modified: Option<SystemTime>,
    pub size: u64,
}

impl TagCacheFingerprint {
    fn for_path(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        Some(Self {
            modified: metadata.modified().ok(),
            size: metadata.len(),
        })
    }
}

/// Password identity used for archive-entry tag-cache validation. The cache
/// must not reuse an empty/failed result from an earlier password attempt after
/// the user supplies a different archive password. Store only a process-local
/// hash rather than the password text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArchiveTagPasswordIdentity {
    pub has_password: bool,
    pub hash: u64,
}

impl ArchiveTagPasswordIdentity {
    fn for_password(password: Option<&str>) -> Self {
        match password {
            Some(password) => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(password, &mut hasher);
                Self {
                    has_password: true,
                    hash: std::hash::Hasher::finish(&hasher),
                }
            }
            None => Self {
                has_password: false,
                hash: 0,
            },
        }
    }
}

/// Cached local tag-search text plus the filesystem fingerprint it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedTagSearchString {
    pub fingerprint: TagCacheFingerprint,
    pub tag_string: String,
}

/// Cached archive-entry tag data. The key is the synthetic archive-entry path
/// (`archive_path/inner/member.flac`), while the fingerprint is taken from the
/// containing archive file. That makes Tags/Both archive search independent of
/// incidental probe-cache warmth but still invalidates when the archive changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedArchiveTagSearchString {
    pub archive_fingerprint: TagCacheFingerprint,
    pub password_identity: ArchiveTagPasswordIdentity,
    pub tags: TagReadResult,
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
    /// In-memory tag cache for non-recursive tag search. Entries are
    /// fingerprinted by file size + mtime so an active search panel cannot
    /// keep matching stale tags after metadata writes or external changes.
    /// Cleared on search close and directory-context changes.
    pub tag_cache: std::collections::HashMap<PathBuf, CachedTagSearchString>,
    /// Archive-entry tag cache for non-staged archive Browse search/sort. Unlike
    /// `tag_cache`, these keys are synthetic archive-member paths and are
    /// validated against the containing archive file fingerprint.
    pub archive_tag_cache: std::collections::HashMap<PathBuf, CachedArchiveTagSearchString>,
    /// Cancel flag for async recursive search tasks.
    pub cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Monotonic identity for search launches. Recursive workers echo the value
    /// so the reducer can reject late completions from superseded searches.
    pub generation: u64,
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
            archive_tag_cache: std::collections::HashMap::new(),
            cancel: None,
            generation: 0,
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

    pub fn is_bluray_iso(&self) -> bool {
        matches!(self.kind, EntryKind::BlurayIso)
    }

    pub fn is_bluray_dir(&self) -> bool {
        matches!(self.kind, EntryKind::BlurayDir)
    }

    pub fn is_disc_source(&self) -> bool {
        matches!(
            self.kind,
            EntryKind::SacdIso
                | EntryKind::DvdAudioIso
                | EntryKind::DvdAudioDir
                | EntryKind::DvdVideoIso
                | EntryKind::DvdVideoDir
                | EntryKind::BlurayIso
                | EntryKind::BlurayDir
        )
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
            EntryKind::BlurayDir => "bluray-dir".to_string(),
            EntryKind::AudioFile(fmt) => fmt.name().to_string(),
            EntryKind::Archive => archive_label(&self.path),
            EntryKind::SacdIso => "sacd".to_string(),
            EntryKind::DvdAudioIso => "dvda".to_string(),
            EntryKind::DvdVideoIso => "dvdv".to_string(),
            EntryKind::BlurayIso => "bluray".to_string(),
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


/// Browse panes that can be collapsed or restored from title-bar controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowsePaneId {
    Explore,
    Browse,
    Info,
}

/// Configurable browse table columns. The core table renderer always keeps
/// `Name` available; audio-specific columns are optional and render as empty
/// values for non-audio entries until probe data is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowseColumn {
    Name,
    Size,
    Date,
    Type,
    Format,
    Codec,
    SampleRate,
    Channels,
    Duration,
    Artist,
    Album,
}

impl BrowseColumn {
    pub const ALL: [BrowseColumn; 11] = [
        BrowseColumn::Name,
        BrowseColumn::Size,
        BrowseColumn::Date,
        BrowseColumn::Type,
        BrowseColumn::Format,
        BrowseColumn::Codec,
        BrowseColumn::SampleRate,
        BrowseColumn::Channels,
        BrowseColumn::Duration,
        BrowseColumn::Artist,
        BrowseColumn::Album,
    ];

    pub fn config_key(self) -> &'static str {
        match self {
            BrowseColumn::Name => "name",
            BrowseColumn::Size => "size",
            BrowseColumn::Date => "date",
            BrowseColumn::Type => "type",
            BrowseColumn::Format => "format",
            BrowseColumn::Codec => "codec",
            BrowseColumn::SampleRate => "sample_rate",
            BrowseColumn::Channels => "channels",
            BrowseColumn::Duration => "duration",
            BrowseColumn::Artist => "artist",
            BrowseColumn::Album => "album",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BrowseColumn::Name => "Name",
            BrowseColumn::Size => "Size",
            BrowseColumn::Date => "Date",
            BrowseColumn::Type => "Type",
            BrowseColumn::Format => "Format",
            BrowseColumn::Codec => "Codec",
            BrowseColumn::SampleRate => "Sample rate",
            BrowseColumn::Channels => "Channels",
            BrowseColumn::Duration => "Duration",
            BrowseColumn::Artist => "Artist",
            BrowseColumn::Album => "Album",
        }
    }

    pub fn from_config_key(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace(' ', "_").replace('-', "_").as_str() {
            "name" => Some(BrowseColumn::Name),
            "size" => Some(BrowseColumn::Size),
            "date" | "modified" | "mtime" => Some(BrowseColumn::Date),
            "type" | "extension" => Some(BrowseColumn::Type),
            "format" => Some(BrowseColumn::Format),
            "codec" => Some(BrowseColumn::Codec),
            "sample_rate" | "samplerate" => Some(BrowseColumn::SampleRate),
            "channels" => Some(BrowseColumn::Channels),
            "duration" => Some(BrowseColumn::Duration),
            "artist" => Some(BrowseColumn::Artist),
            "album" => Some(BrowseColumn::Album),
            _ => None,
        }
    }

    pub fn sort_by(self) -> SortBy {
        match self {
            BrowseColumn::Name => SortBy::Name,
            BrowseColumn::Size => SortBy::Size,
            BrowseColumn::Date => SortBy::Date,
            BrowseColumn::Type => SortBy::Type,
            BrowseColumn::Format => SortBy::Format,
            BrowseColumn::Codec => SortBy::Codec,
            BrowseColumn::SampleRate => SortBy::SampleRate,
            BrowseColumn::Channels => SortBy::Channels,
            BrowseColumn::Duration => SortBy::Duration,
            BrowseColumn::Artist => SortBy::Artist,
            BrowseColumn::Album => SortBy::Album,
        }
    }

    pub fn from_sort_by(sort_by: SortBy) -> Self {
        match sort_by {
            SortBy::Name => BrowseColumn::Name,
            SortBy::Size => BrowseColumn::Size,
            SortBy::Date => BrowseColumn::Date,
            SortBy::Type => BrowseColumn::Type,
            SortBy::Format => BrowseColumn::Format,
            SortBy::Codec => BrowseColumn::Codec,
            SortBy::SampleRate => BrowseColumn::SampleRate,
            SortBy::Channels => BrowseColumn::Channels,
            SortBy::Duration => BrowseColumn::Duration,
            SortBy::Artist => BrowseColumn::Artist,
            SortBy::Album => BrowseColumn::Album,
        }
    }

    pub fn default_columns() -> Vec<Self> {
        vec![BrowseColumn::Name, BrowseColumn::Size, BrowseColumn::Date, BrowseColumn::Type]
    }

    pub fn from_config_list(values: &[String]) -> Vec<Self> {
        let mut columns = Vec::new();
        for value in values {
            let Some(column) = BrowseColumn::from_config_key(value) else { continue; };
            if !columns.contains(&column) {
                columns.push(column);
            }
        }
        if columns.is_empty() {
            BrowseColumn::default_columns()
        } else {
            if !columns.contains(&BrowseColumn::Name) {
                columns.insert(0, BrowseColumn::Name);
            }
            columns
        }
    }
}

/// Options dropdown state for the Browse toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseOptionsMenu {
    Closed,
    Root,
    Layout,
    Columns,
    Sort,
    Filter,
    ArchiveListing,
}

impl BrowseOptionsMenu {
    pub fn is_open(self) -> bool {
        !matches!(self, BrowseOptionsMenu::Closed)
    }
}

/// Explore pane tree node. Reuse the file-picker tree node type directly so
/// Browse and the picker share one filesystem-tree data model.
///
/// Use the explicit module path instead of assuming `TreeNode` is re-exported
/// at the `tui_file_picker` crate root. In the file-picker crate, `TreeNode`
/// is the shared state model consumed by `tree.rs` and the picker renderer.
pub type BrowseTreeNode = tui_file_picker::TreeNode;

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

    /// Monotonic token for async path validations. Every launch and every
    /// directory/archive navigation invalidates older completions so stale
    /// workers cannot navigate Browse after the user has moved elsewhere.
    pub path_validation_generation: u64,

    /// Filter input (when /-mode is active)
    pub filter_input: Option<TextInputState>,
    /// Committed filter text (empty = no filter)
    pub filter_text: String,
    /// Saved `filter_text` from before opening the input — used to restore on cancel.
    filter_text_prior: Option<String>,
    pub show_hidden: bool,

    /// Active sort field and direction for the current directory. Column-header
    /// clicks update these ad hoc values only; directory navigation resets them
    /// from `default_sort_by` / `default_sort_dir`.
    pub sort_by: SortBy,
    pub sort_dir: SortDir,

    /// Persisted default sort applied when entering a new directory. Kept
    /// separate from the active sort so column-click sorting remains local to
    /// the current directory, matching the Browse options model.
    pub default_sort_by: SortBy,
    pub default_sort_dir: SortDir,

    /// Format filter (cycle with `f`)
    pub format_filter: FormatFilter,

    /// Probe cache: path → identity-validated hit/failure entry.
    probe_cache: HashMap<PathBuf, ProbeCacheEntry>,

    /// Paths loaded from SQLite that should receive worker-side metadata
    /// enrichment the next time they become cursor-focused. This keeps the
    /// UI instant on DB hits without doing tag/CUE/catalog I/O on the TUI path.
    pub probe_cache_needs_metadata_enrichment: HashSet<PathBuf>,

    /// Pending cold filesystem probe for Browse cursor debouncing.
    pub probe_debounce: Option<BrowseProbeDebounce>,

    /// Coalesced expensive work requested by async completions during the
    /// current reducer batch. The event loop flushes these flags once after
    /// draining messages so bursts of probe/search/classification updates do
    /// not repeatedly re-sort, re-search, or re-enter the current probe path.
    pub deferred_work: BrowseDeferredWorkFlags,

    /// Warmed SQLite rows waiting for bounded reducer-side merge. The DB query
    /// runs on a blocking worker, but merging thousands of rows can still be
    /// visible; process this queue in small frame-sized slices.
    probe_cache_warm_pending: VecDeque<ProbeCacheWarmBatch>,

    /// Cold filesystem Browse probes waiting for an in-flight slot. Cache hits,
    /// SQLite hits, archive probes, and worker-side metadata enrichment bypass
    /// this queue and remain immediate.
    browse_cold_probe_queue: VecDeque<BrowseColdProbeRequest>,

    /// Cold filesystem Browse probes that have actually started. This is a
    /// subset of `probe_pending` and provides backpressure across distinct
    /// paths during rapid scrolling.
    browse_cold_probe_active: HashSet<PathBuf>,

    /// Set of paths whose probe is currently in flight on a background task.
    /// Prevents duplicate spawns when the cursor moves rapidly.
    pub probe_pending: std::collections::HashSet<PathBuf>,

    /// Short-lived in-memory backoff for transient probe failures. Deterministic
    /// negatives use `probe_cache` with file identity; transient failures stay
    /// out of SQLite and long-lived caches so later readability/success is not
    /// suppressed.
    transient_probe_failures: HashMap<PathBuf, (ProbeCacheIdentity, Instant)>,

    /// Per-archive mutation/probe epoch. Archive-entry probes capture this
    /// value when launched and completions are accepted only if it still
    /// matches. Successful archive metadata repack/rename and explicit
    /// archive probe invalidation bump the affected archive's epoch, so stale
    /// in-flight workers cannot repopulate synthetic-path metadata afterward.
    pub archive_probe_epochs: HashMap<PathBuf, u64>,

    /// Directory stats cache: path → identity-validated recursive stats.
    dir_stats_cache: HashMap<PathBuf, DirStatsCacheEntry>,

    /// Set of directory paths whose stats are queued or currently being
    /// computed. Used by the info pane to show the existing "computing..."
    /// state without launching duplicates.
    pub dir_stats_pending: std::collections::HashSet<PathBuf>,

    /// Recursive directory stats walks that have actually started. This is a
    /// subset of `dir_stats_pending` and provides backpressure across distinct
    /// directories when the cursor moves rapidly over folders.
    dir_stats_active: HashMap<PathBuf, DirStatsActiveJob>,

    /// Recursive directory stats requests waiting for an in-flight slot.
    /// Current-selection requests are kept ahead of stale cursor positions.
    dir_stats_queue: VecDeque<DirStatsRequest>,

    /// Cache of SACD-ISO classifications keyed by path + len + mtime. The
    /// verdict may be negative only when the ISO was readable, so permission or
    /// transient read failures do not poison future scans.
    pub sacd_classify_cache: HashMap<PathBuf, (ClassificationFingerprint, bool)>,

    /// Cache of DVD-Audio ISO classifications keyed by path + len + mtime.
    /// This is used only by scan/upgrade code, never by render code.
    pub dvda_iso_classify_cache: HashMap<PathBuf, (ClassificationFingerprint, bool)>,

    /// Cache of DVD-Audio directory classifications keyed by path + IFO len + mtime.
    pub dvda_dir_classify_cache: HashMap<PathBuf, (ClassificationFingerprint, bool)>,

    /// Cache of DVD-Video ISO classifications keyed by path + len + mtime.
    pub dvdv_iso_classify_cache: HashMap<PathBuf, (ClassificationFingerprint, bool)>,

    /// Cache of DVD-Video directory classifications keyed by path + IFO len + mtime.
    pub dvdv_dir_classify_cache: HashMap<PathBuf, (ClassificationFingerprint, bool)>,

    /// Cache of Blu-ray ISO classifications keyed by path + len + mtime.
    pub bluray_iso_classify_cache: HashMap<PathBuf, (ClassificationFingerprint, bool)>,

    /// Cache of Blu-ray directory classifications keyed by path plus detection-relevant BDMV marker fingerprints.
    pub bluray_dir_classify_cache: HashMap<PathBuf, (ClassificationFingerprint, bool)>,

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

    /// Monotonic token for async directory scans. Every async scan gets a
    /// unique generation; reducers must accept only the currently pending
    /// generation so stale same-directory completions cannot publish old
    /// listings or consume deferred inline-rename continuations.
    pub scan_generation: u64,

    /// After `go_parent`, the name of the directory we came from — so the
    /// DirScanComplete handler can position the cursor on it.
    pub cursor_restore_target: Option<String>,

    /// Type-ahead navigation buffer: accumulated keystrokes for prefix jump.
    pub type_ahead_buffer: String,
    /// Instant of the last type-ahead keystroke, for timeout reset.
    pub type_ahead_last_keystroke: Option<Instant>,

    /// Explore pane directory tree. Expanded/collapsed state is session-local.
    pub tree_nodes: Vec<BrowseTreeNode>,
    pub tree_cursor: usize,
    pub tree_scroll: usize,
    pub tree_visible_height: usize,

    /// Three-pane Browse layout state. Browse itself is never fully collapsed.
    /// `*_enabled` controls whether a side pane participates in layout at all;
    /// `*_collapsed` controls the enabled pane's 3-column collapsed rail state.
    pub explore_enabled: bool,
    pub info_enabled: bool,
    pub explore_collapsed: bool,
    pub info_collapsed: bool,
    pub browse_maximized: bool,
    pub browse_title_last_click: Option<Instant>,

    /// Configurable visible columns and toolbar options-menu state.
    pub columns: Vec<BrowseColumn>,
    pub options_menu: BrowseOptionsMenu,

    /// Last frame area used to render Browse. Mouse hit-testing for floating
    /// Browse overlays must use this rendered coordinate space rather than
    /// assuming Browse starts at the terminal origin.
    pub last_render_area: Option<Rect>,

    /// Directory navigation history backing toolbar Back/Fwd.
    pub nav_history: Vec<PathBuf>,
    pub nav_history_index: usize,

    /// Cap for search results. Recursive search applies this only after global
    /// scoring and sorting so walk order cannot hide better later matches.
    pub search_result_cap: usize,

    /// Pending sequential inline-rename target captured before a filesystem
    /// rename triggers an async refresh that temporarily clears `entries`.
    pub pending_inline_rename_after_scan: Option<PendingBrowseInlineRenameAfterScan>,

    /// Channel sender for async messages. Set after construction by the
    /// event loop. `None` during the initial synchronous scan.
    scan_tx: Option<tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
}

/// Handle to a cancellable background directory scan.
#[derive(Debug, Clone)]
pub struct ScanHandle {
    generation: u64,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ScanHandle {
    pub fn new(generation: u64) -> (Self, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        (
            Self {
                generation,
                cancel: flag.clone(),
            },
            flag,
        )
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn cancel(&self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// State for browsing inside an archive.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum ArchiveEdit {
    Rename { from: String, to: String },
    /// Replayable single-field tag write.
    ///
    /// Older recovery rows may contain generic values such as
    /// `field=metadata,value=updated`; new inline writes store the actual
    /// edited metadata field and final value. Repackage still treats the
    /// staged tree as the source of truth, but the log is now meaningful for
    /// audit, recovery review, and future replay.
    MetadataWrite { inner_path: String, field: String, value: String },
    /// Non-field-specific file content change.
    ///
    /// The metadata editor, artwork writer, and ReplayGain writer can update
    /// multiple tags and sidecar-derived fields in one save. When the caller
    /// cannot supply exact field-level deltas, record that the staged member
    /// content changed instead of fabricating a fake `metadata=updated` edit.
    ContentModified { inner_path: String, kind: String },
    Delete { inner_path: String },
}

#[derive(Debug, Clone)]
pub struct ArchiveStagingSession {
    pub staging_dir: PathBuf,
    pub archive_path: PathBuf,
    pub archive_mtime_secs: i64,
    pub archive_mtime_nanos: u32,
    pub archive_size: u64,
    pub edits: Vec<ArchiveEdit>,
    pub dirty: bool,
}

impl ArchiveStagingSession {
    pub fn new(
        staging_dir: PathBuf,
        archive_path: PathBuf,
        archive_mtime_secs: i64,
        archive_mtime_nanos: u32,
        archive_size: u64,
    ) -> Self {
        Self {
            staging_dir,
            archive_path,
            archive_mtime_secs,
            archive_mtime_nanos,
            archive_size,
            edits: Vec::new(),
            dirty: false,
        }
    }

    pub fn append_edit(&mut self, edit: ArchiveEdit) {
        self.edits.push(edit);
        self.dirty = true;
    }

    pub fn append_metadata_write(&mut self, inner_path: String, field: String, value: String) {
        if let Some(ArchiveEdit::MetadataWrite {
            value: existing_value,
            ..
        }) = self.edits.iter_mut().rev().find(|edit| {
            matches!(
                edit,
                ArchiveEdit::MetadataWrite {
                    inner_path: existing_inner,
                    field: existing_field,
                    ..
                } if existing_inner == &inner_path && existing_field == &field
            )
        }) {
            *existing_value = value;
            self.dirty = true;
            return;
        }

        self.edits.push(ArchiveEdit::MetadataWrite {
            inner_path,
            field,
            value,
        });
        self.dirty = true;
    }

    pub fn append_content_modified(&mut self, inner_path: String, kind: String) {
        if self.edits.iter().any(|edit| {
            matches!(
                edit,
                ArchiveEdit::ContentModified {
                    inner_path: existing_inner,
                    kind: existing_kind,
                } if existing_inner == &inner_path && existing_kind == &kind
            )
        }) {
            self.dirty = true;
            return;
        }

        self.edits.push(ArchiveEdit::ContentModified { inner_path, kind });
        self.dirty = true;
    }
}

/// State for browsing inside an archive.
#[derive(Debug, Clone)]
pub struct ArchiveBrowseState {
    /// The parsed archive listing.
    pub listing: crate::tui::archive_listing::ArchiveListing,
    /// Current directory path inside the archive ("" = root).
    pub inner_path: String,
    /// Password used to open this archive (for re-listing / extraction).
    pub password: Option<String>,
    /// Persistent staging session for deferred archive saves.
    pub staging: Option<ArchiveStagingSession>,
}

/// Continuation for Tab/Shift+Tab sequential rename across an async filesystem refresh.
///
/// Filesystem renames refresh the directory after the on-disk rename. In the normal
/// interactive app that refresh is async and clears `entries` immediately, so the
/// next/previous entry must be captured before committing and resumed when the
/// scan for the same directory completes.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingBrowseInlineRenameAfterScan {
    pub scan_generation: u64,
    pub directory: PathBuf,
    pub target_path: PathBuf,
}

impl BrowseState {
    pub fn new() -> Self {
        Self::new_with_config(&crate::config::BrowsingConfig::default())
    }

    pub fn new_with_config(config: &crate::config::BrowsingConfig) -> Self {
        let start_dir = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/"));
        let config = config.normalized();
        let sort_by = SortBy::from_label(&config.default_sort).unwrap_or(SortBy::Name);
        let sort_dir = SortDir::from_label(&config.default_sort_dir).unwrap_or(SortDir::Asc);
        let format_filter = FormatFilter::from_label(&config.default_filter).unwrap_or(FormatFilter::Off);
        let columns = BrowseColumn::from_config_list(&config.columns);
        let explore_enabled = config.layout_explore_enabled;
        let info_enabled = config.layout_info_enabled;
        let explore_collapsed = config.layout_explore == "collapsed";
        let info_collapsed = config.layout_info == "collapsed";

        let mut state = Self {
            current_dir: start_dir.clone(),
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
            path_validation_generation: 0,
            filter_input: None,
            filter_text: String::new(),
            filter_text_prior: None,
            show_hidden: config.show_hidden,
            sort_by,
            sort_dir,
            default_sort_by: sort_by,
            default_sort_dir: sort_dir,
            format_filter,
            probe_cache: HashMap::new(),
            probe_cache_needs_metadata_enrichment: HashSet::new(),
            probe_debounce: None,
            deferred_work: BrowseDeferredWorkFlags::default(),
            probe_cache_warm_pending: VecDeque::new(),
            browse_cold_probe_queue: VecDeque::new(),
            browse_cold_probe_active: HashSet::new(),
            probe_pending: std::collections::HashSet::new(),
            transient_probe_failures: HashMap::new(),
            archive_probe_epochs: HashMap::new(),
            dir_stats_cache: HashMap::new(),
            dir_stats_pending: std::collections::HashSet::new(),
            dir_stats_active: HashMap::new(),
            dir_stats_queue: VecDeque::new(),
            sacd_classify_cache: HashMap::new(),
            dvda_iso_classify_cache: HashMap::new(),
            dvda_dir_classify_cache: HashMap::new(),
            dvdv_iso_classify_cache: HashMap::new(),
            dvdv_dir_classify_cache: HashMap::new(),
            bluray_iso_classify_cache: HashMap::new(),
            bluray_dir_classify_cache: HashMap::new(),
            disc_probe_cache: HashMap::new(),
            disc_probe_pending: std::collections::HashSet::new(),
            disc_probe_followup: HashMap::new(),
            return_target: BrowseReturnTarget::None,
            error: None,
            archive: None,
            scan_pending: None,
            scan_generation: 0,
            cursor_restore_target: None,
            type_ahead_buffer: String::new(),
            type_ahead_last_keystroke: None,
            tree_nodes: initial_browse_tree_nodes(&start_dir, config.show_hidden),
            tree_cursor: 0,
            tree_scroll: 0,
            tree_visible_height: 0,
            explore_enabled,
            info_enabled,
            explore_collapsed,
            info_collapsed,
            browse_maximized: explore_collapsed && info_collapsed,
            browse_title_last_click: None,
            columns,
            options_menu: BrowseOptionsMenu::Closed,
            last_render_area: None,
            nav_history: vec![start_dir],
            nav_history_index: 0,
            search_result_cap: config.search_result_cap,
            pending_inline_rename_after_scan: None,
            scan_tx: None,
        };
        state.sync_tree_to_current_dir();
        state.refresh(); // Initial scan is synchronous (no tx yet).
        state
    }

    /// Set the message channel sender (called once from the event loop).
    pub fn set_tx(&mut self, tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>) {
        self.scan_tx = Some(tx);
    }

    /// Whether async scanning is enabled (tx has been set).
    pub fn is_async_enabled(&self) -> bool {
        self.scan_tx.is_some()
    }

    /// Persistable snapshot of Browse view preferences.
    pub fn capture_browsing_config(&self) -> crate::config::BrowsingConfig {
        crate::config::BrowsingConfig {
            show_hidden: self.show_hidden,
            columns: self
                .columns
                .iter()
                .map(|column| column.config_key().to_string())
                .collect(),
            default_sort: self.default_sort_by.label().to_string(),
            default_sort_dir: self.default_sort_dir.label().to_string(),
            default_filter: self.format_filter.config_label(),
            layout_explore_enabled: self.explore_enabled,
            layout_info_enabled: self.info_enabled,
            layout_explore: if self.explore_collapsed { "collapsed" } else { "open" }.to_string(),
            layout_info: if self.info_collapsed { "collapsed" } else { "open" }.to_string(),
            search_result_cap: self.search_result_cap,
        }
        .normalized()
    }

    pub fn can_go_back(&self) -> bool {
        self.nav_history_index > 0 && self.nav_history_index < self.nav_history.len()
    }

    pub fn can_go_forward(&self) -> bool {
        self.nav_history_index + 1 < self.nav_history.len()
    }

    pub fn go_back(&mut self) -> bool {
        if !self.can_go_back() {
            return false;
        }
        self.nav_history_index -= 1;
        if let Some(path) = self.nav_history.get(self.nav_history_index).cloned() {
            self.navigate_without_history(path);
            true
        } else {
            false
        }
    }

    pub fn go_forward(&mut self) -> bool {
        if !self.can_go_forward() {
            return false;
        }
        self.nav_history_index += 1;
        if let Some(path) = self.nav_history.get(self.nav_history_index).cloned() {
            self.navigate_without_history(path);
            true
        } else {
            false
        }
    }

    fn push_nav_history(&mut self, path: PathBuf) {
        if self.nav_history.get(self.nav_history_index).is_some_and(|current| same_path(current, &path)) {
            return;
        }
        if self.nav_history_index + 1 < self.nav_history.len() {
            self.nav_history.truncate(self.nav_history_index + 1);
        }
        self.nav_history.push(path);
        self.nav_history_index = self.nav_history.len().saturating_sub(1);
    }

    fn navigate_without_history(&mut self, path: PathBuf) {
        if !path.is_dir() {
            return;
        }
        self.invalidate_path_validation();
        self.current_dir = path;
        self.selected_index = 0;
        self.reset_nav_state();
        self.reset_sort_to_default();
        self.sync_tree_to_current_dir();
        self.refresh();
    }

    pub fn toggle_pane(&mut self, pane: BrowsePaneId) {
        match pane {
            BrowsePaneId::Explore => self.explore_collapsed = !self.explore_collapsed,
            BrowsePaneId::Info => self.info_collapsed = !self.info_collapsed,
            // Browse never fully collapses. Maximization is a distinct
            // double-click action on the Browse title, not a single-click
            // pane-toggle action. Keep this no-op as a defensive guard for
            // stale or future hit targets.
            BrowsePaneId::Browse => return,
        }
        self.browse_maximized = self.explore_collapsed && self.info_collapsed;
    }

    pub fn toggle_pane_enabled(&mut self, pane: BrowsePaneId) {
        match pane {
            BrowsePaneId::Explore => self.explore_enabled = !self.explore_enabled,
            BrowsePaneId::Info => self.info_enabled = !self.info_enabled,
            BrowsePaneId::Browse => return,
        }
    }

    pub fn toggle_browse_maximized(&mut self) {
        if self.explore_collapsed && self.info_collapsed {
            self.explore_collapsed = false;
            self.info_collapsed = false;
            self.browse_maximized = false;
        } else {
            self.explore_collapsed = true;
            self.info_collapsed = true;
            self.browse_maximized = true;
        }
    }

    pub fn reset_browse_layout(&mut self) {
        self.explore_enabled = true;
        self.info_enabled = true;
        self.explore_collapsed = false;
        self.info_collapsed = false;
        self.browse_maximized = false;
    }

    /// Apply persisted Browse preferences to the live session without replacing
    /// BrowseState. This preserves current directory, archive/session state,
    /// navigation history, pending scans, caches, and selection context.
    pub fn apply_browsing_config(&mut self, config: &crate::config::BrowsingConfig) {
        self.apply_browsing_config_with_search(config, None);
    }

    pub fn apply_browsing_config_with_search(
        &mut self,
        config: &crate::config::BrowsingConfig,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        let config = config.normalized();
        let previous_hidden = self.show_hidden;
        self.show_hidden = config.show_hidden;
        self.columns = BrowseColumn::from_config_list(&config.columns);
        self.default_sort_by = SortBy::from_label(&config.default_sort).unwrap_or(SortBy::Name);
        self.default_sort_dir = SortDir::from_label(&config.default_sort_dir).unwrap_or(SortDir::Asc);
        self.reset_sort_to_default();
        self.format_filter = FormatFilter::from_label(&config.default_filter).unwrap_or(FormatFilter::Off);
        self.explore_enabled = config.layout_explore_enabled;
        self.info_enabled = config.layout_info_enabled;
        self.explore_collapsed = config.layout_explore == "collapsed";
        self.info_collapsed = config.layout_info == "collapsed";
        self.browse_maximized = self.explore_collapsed && self.info_collapsed;
        self.search_result_cap = config.search_result_cap;
        self.close_options_menu();

        if previous_hidden != self.show_hidden {
            self.rebuild_tree_preserving_expansion();
        } else {
            self.sync_tree_to_current_dir();
        }
        self.reapply_after_browse_preference_change(tx);
    }

    pub fn toggle_column(&mut self, column: BrowseColumn) {
        if column == BrowseColumn::Name {
            return;
        }
        if let Some(index) = self.columns.iter().position(|existing| *existing == column) {
            self.columns.remove(index);
        } else {
            self.columns.push(column);
        }
        if !self.columns.contains(&BrowseColumn::Name) {
            self.columns.insert(0, BrowseColumn::Name);
        }
    }

    pub fn close_options_menu(&mut self) {
        self.options_menu = BrowseOptionsMenu::Closed;
    }

    pub fn back_or_close_options_menu(&mut self) {
        self.options_menu = match self.options_menu {
            BrowseOptionsMenu::Closed => BrowseOptionsMenu::Closed,
            BrowseOptionsMenu::Root => BrowseOptionsMenu::Closed,
            _ => BrowseOptionsMenu::Root,
        };
    }

    pub fn toggle_options_menu(&mut self) {
        self.options_menu = if self.options_menu.is_open() {
            BrowseOptionsMenu::Closed
        } else {
            BrowseOptionsMenu::Root
        };
    }

    pub fn set_tree_visible_height(&mut self, height: usize) {
        self.tree_visible_height = height;
        self.ensure_tree_visible();
    }

    pub fn tree_node_path(&self, index: usize) -> Option<PathBuf> {
        self.tree_nodes.get(index).map(|node| node.path.clone())
    }

    pub fn select_tree_index(&mut self, index: usize) -> Option<PathBuf> {
        if index >= self.tree_nodes.len() {
            return None;
        }
        self.tree_cursor = index;
        self.ensure_tree_visible();
        self.tree_nodes.get(index).map(|node| node.path.clone())
    }

    pub fn toggle_tree_node(&mut self, index: usize) {
        if index >= self.tree_nodes.len() {
            return;
        }
        if self.tree_nodes[index].expanded {
            let depth = self.tree_nodes[index].depth;
            self.tree_nodes[index].expanded = false;
            let remove_start = index + 1;
            let remove_end = self.tree_nodes[remove_start..]
                .iter()
                .position(|node| node.depth <= depth)
                .map(|offset| remove_start + offset)
                .unwrap_or(self.tree_nodes.len());
            self.tree_nodes.drain(remove_start..remove_end);
            if self.tree_cursor >= self.tree_nodes.len() {
                self.tree_cursor = self.tree_nodes.len().saturating_sub(1);
            }
            self.ensure_tree_visible();
            return;
        }
        let children = tui_file_picker::child_directories(&self.tree_nodes[index].path, self.tree_nodes[index].depth + 1, self.show_hidden);
        self.tree_nodes[index].expanded = true;
        for (offset, child) in children.into_iter().enumerate() {
            self.tree_nodes.insert(index + 1 + offset, child);
        }
    }

    pub fn sync_tree_to_current_dir(&mut self) {
        let root_misses_current = self
            .tree_nodes
            .first()
            .map(|node| !self.current_dir.starts_with(&node.path))
            .unwrap_or(true);
        if root_misses_current {
            self.tree_nodes = initial_browse_tree_nodes(&self.current_dir, self.show_hidden);
        }
        browse_tree_expand_ancestors(&mut self.tree_nodes, &self.current_dir, self.show_hidden);
        if let Some(index) = self
            .tree_nodes
            .iter()
            .position(|node| same_path(&node.path, &self.current_dir))
        {
            self.tree_cursor = index;
            self.ensure_tree_visible();
        }
    }

    fn ensure_tree_visible(&mut self) {
        if self.tree_visible_height == 0 {
            return;
        }
        if self.tree_cursor < self.tree_scroll {
            self.tree_scroll = self.tree_cursor;
        } else if self.tree_cursor >= self.tree_scroll + self.tree_visible_height {
            self.tree_scroll = self.tree_cursor + 1 - self.tree_visible_height;
        }
    }

    /// Full refresh: re-scan disk, then re-apply the view filters/sort.
    /// Uses async scan if tx is available, otherwise falls back to synchronous.
    pub fn refresh(&mut self) {
        self.refresh_with_search(None);
    }

    /// Refresh while preserving the active-search ownership invariant. Interactive
    /// callers should pass `tx` so archive tag searches that require extraction
    /// restart through the async worker instead of falling back to synchronous
    /// tag extraction on the TUI path.
    pub fn refresh_with_search(
        &mut self,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        if self.archive.is_some() {
            self.refresh_archive_view_with_search(tx);
            return;
        }
        self.sync_tree_to_current_dir();
        self.invalidate_recursive_search_for_refresh();
        if self.scan_tx.is_some() {
            self.begin_async_scan();
        } else {
            // Synchronous fallback (initial scan before tx is set).
            self.scan();
            self.classify_dvda_directory_entries();
            self.upgrade_iso_kinds();
            self.reapply_after_directory_scan_complete(tx);
        }
    }

    /// Start an async directory scan. Cancels any in-flight scan first.
    /// Clears entries immediately (renderer shows "Loading...").
    fn begin_async_scan(&mut self) {
        // Cancel previous scan if still running.
        if let Some(handle) = self.scan_pending.take() {
            handle.cancel();
        }
        self.clear_pending_inline_rename_after_scan();

        // Clear display state.
        self.parent_entry = None;
        self.all_dirs.clear();
        self.all_files.clear();
        self.entries.clear();
        self.error = None;
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.probe_debounce = None;
        self.clear_probe_cache_warm_backlog();
        self.clear_browse_cold_probe_queue();
        self.clear_dir_stats_work_queue();

        let tx = match &self.scan_tx {
            Some(tx) => tx.clone(),
            None => return, // No channel — shouldn't happen after set_tx.
        };

        self.scan_generation = self.scan_generation.wrapping_add(1);
        let generation = self.scan_generation;
        let (handle, cancel_flag) = ScanHandle::new(generation);
        self.scan_pending = Some(handle);

        let classification_cache = self.classification_cache_snapshot();
        spawn_dir_scan(self.current_dir.clone(), generation, cancel_flag, classification_cache, tx);
    }

    /// Whether we're currently browsing inside an archive.
    pub fn is_in_archive(&self) -> bool {
        self.archive.is_some()
    }

    /// Enter an archive: set archive state and populate entries from listing.
    pub fn enter_archive(
        &mut self,
        listing: crate::tui::archive_listing::ArchiveListing,
        password: Option<String>,
    ) {
        self.close_search_for_navigation();
        self.clear_archive_tag_cache_for_archive(&listing.archive_path);
        self.archive = Some(ArchiveBrowseState {
            listing,
            inner_path: String::new(),
            password,
            staging: None,
        });
        self.multi_selected.clear();
        self.multi_select_anchor = None;
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.clear_dir_stats_work_queue();
        self.clear_type_ahead();
        self.reset_sort_to_default();
        self.refresh_archive_view();
        self.selected_index = 0;
        self.scroll_offset = 0;
    }

    /// Replace the active archive listing after the archive changed on disk,
    /// preserving the current archive directory and cursor when those entries
    /// still exist. Used after metadata edits and repackages so file sizes and
    /// packed sizes do not remain stale while the user stays inside the archive.
    pub fn replace_active_archive_listing(
        &mut self,
        listing: crate::tui::archive_listing::ArchiveListing,
        password: Option<String>,
    ) {
        self.replace_active_archive_listing_with_search(listing, password, None);
    }

    pub fn replace_active_archive_listing_with_search(
        &mut self,
        listing: crate::tui::archive_listing::ArchiveListing,
        password: Option<String>,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        let active_same_archive = self
            .archive
            .as_ref()
            .is_some_and(|arc| arc.listing.archive_path == listing.archive_path);
        if !active_same_archive {
            self.enter_archive(listing, password);
            return;
        }

        let old_password_identity = self
            .archive
            .as_ref()
            .map(|arc| ArchiveTagPasswordIdentity::for_password(arc.password.as_deref()))
            .unwrap_or_else(|| ArchiveTagPasswordIdentity::for_password(None));
        let new_password_identity = ArchiveTagPasswordIdentity::for_password(password.as_deref());
        if old_password_identity != new_password_identity {
            self.clear_archive_tag_cache_for_archive(&listing.archive_path);
        }

        let previous_inner = self
            .archive
            .as_ref()
            .map(|arc| arc.inner_path.clone())
            .unwrap_or_default();
        let previous_selected_inner = self
            .entries
            .get(self.selected_index)
            .and_then(|entry| self.archive_inner_path_for_entry(entry));

        let inner_still_exists = previous_inner.is_empty()
            || listing.entries.iter().any(|entry| {
                entry.path == previous_inner
                    || entry
                        .path
                        .strip_prefix(previous_inner.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            });

        let previous_staging = self.archive.as_ref().and_then(|arc| arc.staging.clone());
        self.archive = Some(ArchiveBrowseState {
            listing,
            inner_path: if inner_still_exists {
                previous_inner
            } else {
                String::new()
            },
            password,
            staging: previous_staging,
        });
        self.multi_selected.clear();
        self.multi_select_anchor = None;
        self.clear_dir_stats_work_queue();
        self.clear_type_ahead();
        self.reset_sort_to_default();
        self.refresh_archive_view_with_search(tx);

        if let Some(inner) = previous_selected_inner {
            if let Some(archive_path) = self.archive.as_ref().map(|arc| arc.listing.archive_path.clone()) {
                let selected_path = archive_path.join(inner);
                if let Some(idx) = self.entries.iter().position(|entry| entry.path == selected_path) {
                    self.selected_index = idx;
                    self.ensure_visible();
                }
            }
        }
    }

    pub fn active_archive_staging(&self) -> Option<&ArchiveStagingSession> {
        self.archive.as_ref().and_then(|arc| arc.staging.as_ref())
    }

    pub fn active_archive_staging_mut(&mut self) -> Option<&mut ArchiveStagingSession> {
        self.archive.as_mut().and_then(|arc| arc.staging.as_mut())
    }

    pub fn take_active_archive_staging(&mut self) -> Option<ArchiveStagingSession> {
        self.archive.as_mut().and_then(|arc| arc.staging.take())
    }

    /// Exit the archive and return to filesystem browsing.
    pub fn exit_archive(&mut self) {
        self.invalidate_path_validation();
        self.close_search_for_navigation();
        self.archive = None;
        self.multi_selected.clear();
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.reset_sort_to_default();
        self.refresh();
    }

    /// Navigate into a subdirectory inside the archive.
    pub fn enter_archive_dir(&mut self, dir_path: &str) {
        self.invalidate_path_validation();
        self.close_search_for_navigation();
        if let Some(ref mut arc) = self.archive {
            arc.inner_path = dir_path.to_string();
        }
        self.multi_selected.clear();
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.clear_dir_stats_work_queue();
        self.reset_sort_to_default();
        self.refresh_archive_view();
    }

    /// Navigate up one level inside the archive. Returns false if already
    /// at archive root (caller should exit the archive entirely).
    pub fn go_up_in_archive(&mut self) -> bool {
        self.invalidate_path_validation();
        self.close_search_for_navigation();
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
            self.clear_dir_stats_work_queue();
            self.reset_sort_to_default();
            self.refresh_archive_view();
            return true;
        }
        false
    }

    /// Rebuild the archive-local raw model for the current inner path and then
    /// publish it through the same view/search pipeline as filesystem Browse.
    ///
    /// Archive Browse must not write visible rows directly into `entries`: doing
    /// so bypasses Show Hidden, format filters, ad hoc sorting, configured
    /// columns, and the active-search ownership invariant. The archive path now
    /// mirrors filesystem scans: `parent_entry` + `all_dirs` + `all_files` are
    /// the raw model, and `apply_view()` / search produce the visible list.
    pub(crate) fn refresh_archive_view(&mut self) {
        self.refresh_archive_view_with_search(None);
    }

    pub(crate) fn refresh_archive_view_with_search(
        &mut self,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        let prev_path = self
            .entries
            .get(self.selected_index)
            .map(|entry| entry.path.clone());

        self.rebuild_archive_raw_entries();
        self.reapply_after_directory_scan_complete(tx);
        self.restore_cursor_on_path(prev_path);

        if self.entries.is_empty() {
            self.selected_index = 0;
            self.scroll_offset = 0;
        } else {
            if self.selected_index >= self.entries.len() {
                self.selected_index = self.entries.len() - 1;
            }
            let max_offset = self.entries.len().saturating_sub(self.visible_height.max(1));
            self.scroll_offset = self.scroll_offset.min(max_offset);
            self.ensure_visible();
        }
    }

    /// Populate `parent_entry`, `all_dirs`, and `all_files` from the active
    /// archive directory without applying visibility, format, text, or search
    /// filters. This is the archive-mode equivalent of `scan()`.
    fn rebuild_archive_raw_entries(&mut self) {
        self.parent_entry = None;
        self.all_dirs.clear();
        self.all_files.clear();
        self.entries.clear();

        let arc = match &self.archive {
            Some(a) => a,
            None => return,
        };

        // Preserve the historical archive Browse affordance: `..` is always
        // visible and lets callers either move up inside the archive or exit the
        // archive from its root.
        self.parent_entry = Some(BrowseEntry::new(
            PathBuf::from(".."),
            "..".to_string(),
            EntryKind::ParentDir,
            0,
            None,
        ));

        let mut dirs = Vec::new();
        let mut files = Vec::new();
        if let Some(staging) = arc.staging.as_ref() {
            let current_dir = staging.staging_dir.join(&arc.inner_path);
            if let Ok(read_dir) = fs::read_dir(&current_dir) {
                for child in read_dir.flatten() {
                    let path = child.path();
                    let Ok(meta) = child.metadata() else { continue };
                    let is_dir = meta.is_dir();
                    let name = child.file_name().to_string_lossy().into_owned();
                    let inner = if arc.inner_path.is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{}", arc.inner_path, name)
                    };
                    let kind = if is_dir { EntryKind::Directory } else { classify_file(&path) };
                    let modified = meta.modified().ok();
                    let entry = BrowseEntry::new(
                        arc.listing.archive_path.join(inner),
                        name,
                        kind,
                        if is_dir { 0 } else { meta.len() },
                        modified,
                    );
                    if is_dir { dirs.push(entry); } else { files.push(entry); }
                }
            }
        } else {
            let items = arc.listing.entries_at(&arc.inner_path);
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
        }

        self.all_dirs = dirs;
        self.all_files = files;
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

                    if matches!(
                        kind,
                        EntryKind::Directory
                            | EntryKind::DvdAudioDir
                            | EntryKind::DvdVideoDir
                            | EntryKind::BlurayDir
                    ) {
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
    /// whose extension is `.iso` to a supported disc-image kind when
    /// the corresponding bounded source check succeeds.
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

            // SACD first: the probe is three tiny magic-byte reads. Cache by
            // file identity, not just mtime, so coarse timestamp filesystems
            // cannot reuse a stale negative after a same-tick size change.
            let sacd_fingerprint = ClassificationFingerprint::from_entry(entry);
            let cache_hit = self
                .sacd_classify_cache
                .get(&entry.path)
                .filter(|(cached, _)| *cached == sacd_fingerprint)
                .map(|(_, verdict)| *verdict);
            let is_sacd = if let Some(v) = cache_hit {
                v
            } else {
                let v = super::sacd::is_sacd_iso(&entry.path);
                if v || should_cache_file_classification_negative(&entry.path) {
                    self.sacd_classify_cache
                        .insert(entry.path.clone(), (sacd_fingerprint.clone(), v));
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
                    if verdict || should_cache_file_classification_negative(&entry.path) {
                        self.dvda_iso_classify_cache
                            .insert(entry.path.clone(), (fingerprint.clone(), verdict));
                    }
                    verdict
                });
            if is_dvda {
                entry.kind = EntryKind::DvdAudioIso;
                continue;
            }

            // DVD-Video third: hybrids are intentionally excluded by dvdv_utils,
            // so DVD-Audio wins when both AUDIO_TS and VIDEO_TS exist.
            let is_dvdv = self
                .dvdv_iso_classify_cache
                .get(&entry.path)
                .filter(|(cached, _)| *cached == fingerprint)
                .map(|(_, verdict)| *verdict)
                .unwrap_or_else(|| {
                    let verdict = crate::disc::dvdv_utils::is_dvdv_iso(&entry.path);
                    if verdict || should_cache_file_classification_negative(&entry.path) {
                        self.dvdv_iso_classify_cache
                            .insert(entry.path.clone(), (fingerprint.clone(), verdict));
                    }
                    verdict
                });
            if is_dvdv {
                entry.kind = EntryKind::DvdVideoIso;
                continue;
            }

            // Blu-ray last so DVD-Audio/DVD-Video keep priority on any
            // malformed or hybrid image that happens to expose Blu-ray markers.
            let is_bluray = self
                .bluray_iso_classify_cache
                .get(&entry.path)
                .filter(|(cached, _)| *cached == fingerprint)
                .map(|(_, verdict)| *verdict)
                .unwrap_or_else(|| {
                    let verdict = crate::disc::bluray_utils::is_bluray_iso(&entry.path);
                    if verdict || should_cache_file_classification_negative(&entry.path) {
                        self.bluray_iso_classify_cache
                            .insert(entry.path.clone(), (fingerprint.clone(), verdict));
                    }
                    verdict
                });
            if is_bluray {
                entry.kind = EntryKind::BlurayIso;
            }
        }
    }

    /// Classify scanned directory entries that are disc roots.
    pub(super) fn classify_dvda_directory_entries(&mut self) {
        for entry in self.all_dirs.iter_mut() {
            classify_dvda_directory_entry(entry, &mut self.dvda_dir_classify_cache);
            classify_dvdv_directory_entry(entry, &mut self.dvdv_dir_classify_cache);
            classify_bluray_directory_entry(entry, &mut self.bluray_dir_classify_cache);
        }
    }


    fn classification_cache_snapshot(&self) -> BrowseClassificationCacheSnapshot {
        BrowseClassificationCacheSnapshot::from_state(self)
    }

    pub fn apply_classification_cache_updates(&mut self, updates: BrowseClassificationCacheUpdates) {
        for (path, fingerprint, verdict) in updates.sacd_iso {
            self.sacd_classify_cache.insert(path, (fingerprint, verdict));
        }
        for (path, fingerprint, verdict) in updates.dvda_iso {
            self.dvda_iso_classify_cache.insert(path, (fingerprint, verdict));
        }
        for (path, fingerprint, verdict) in updates.dvdv_iso {
            self.dvdv_iso_classify_cache.insert(path, (fingerprint, verdict));
        }
        for (path, fingerprint, verdict) in updates.bluray_iso {
            self.bluray_iso_classify_cache.insert(path, (fingerprint, verdict));
        }
        for (path, fingerprint, verdict) in updates.dvda_dir {
            self.dvda_dir_classify_cache.insert(path, (fingerprint, verdict));
        }
        for (path, fingerprint, verdict) in updates.dvdv_dir {
            self.dvdv_dir_classify_cache.insert(path, (fingerprint, verdict));
        }
        for (path, fingerprint, verdict) in updates.bluray_dir {
            self.bluray_dir_classify_cache.insert(path, (fingerprint, verdict));
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

        sort_entries(&mut dirs, self.sort_by, self.sort_dir, &self.probe_cache);
        sort_entries(&mut files, self.sort_by, self.sort_dir, &self.probe_cache);

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

    /// Reset the active directory sort to the persisted default. Call this only
    /// on directory-entry transitions; explicit refresh preserves ad hoc sorting
    /// in the current directory.
    fn reset_sort_to_default(&mut self) {
        self.sort_by = self.default_sort_by;
        self.sort_dir = self.default_sort_dir;
    }

    /// Update the persisted default sort. When `apply_current_directory` is true
    /// (for the Options submenu), the current directory is re-sorted immediately;
    /// column-header clicks must use `set_sort` instead so they remain ad hoc.
    pub fn set_default_sort(
        &mut self,
        by: SortBy,
        dir: SortDir,
        apply_current_directory: bool,
    ) {
        self.set_default_sort_with_search(by, dir, apply_current_directory, None);
    }

    pub fn set_default_sort_with_search(
        &mut self,
        by: SortBy,
        dir: SortDir,
        apply_current_directory: bool,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        self.default_sort_by = by;
        self.default_sort_dir = dir;
        if apply_current_directory {
            self.sort_by = by;
            self.sort_dir = dir;
            self.reapply_after_browse_preference_change(tx);
        }
    }

    /// Cycle to the next sort field and re-apply, preserving cursor on current entry
    pub fn cycle_sort_by(&mut self) {
        self.cycle_sort_by_with_search(None);
    }

    pub fn cycle_sort_by_with_search(
        &mut self,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        self.sort_by = self.sort_by.next();
        self.reapply_after_browse_preference_change(tx);
    }

    /// Toggle sort direction and re-apply, preserving cursor on current entry
    pub fn toggle_sort_dir(&mut self) {
        self.toggle_sort_dir_with_search(None);
    }

    pub fn toggle_sort_dir_with_search(
        &mut self,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        self.sort_dir = self.sort_dir.toggle();
        self.reapply_after_browse_preference_change(tx);
    }

    /// Set sort field and direction explicitly, preserving cursor
    pub fn set_sort(&mut self, by: SortBy, dir: SortDir) {
        self.set_sort_with_search(by, dir, None);
    }

    pub fn set_sort_with_search(
        &mut self,
        by: SortBy,
        dir: SortDir,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        self.sort_by = by;
        self.sort_dir = dir;
        self.reapply_after_browse_preference_change(tx);
    }

    /// Re-sort when probe-backed fields arrive. No-op for path-only sort keys,
    /// so late probe completions cannot perturb ordinary name/date/type/size order.
    pub fn resort_after_probe_cache_update(&mut self) {
        self.resort_after_probe_cache_update_with_search(None);
    }

    pub fn resort_after_probe_cache_update_with_search(
        &mut self,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        if self.active_search_depends_on_tag_or_probe_metadata() || self.sort_by.uses_probe_cache() {
            self.reapply_after_browse_preference_change(tx);
        }
    }

    fn active_search_depends_on_tag_or_probe_metadata(&self) -> bool {
        self.search.active
            && (matches!(self.search.mode, SearchMode::Tags | SearchMode::Both)
                || self.search.sort.is_tag_sort())
    }

    /// Cycle to the next format filter and re-apply, preserving cursor if possible
    pub fn cycle_format_filter(&mut self) {
        self.cycle_format_filter_with_search(None);
    }

    pub fn cycle_format_filter_with_search(
        &mut self,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        self.format_filter = self.format_filter.next();
        self.reapply_after_browse_preference_change(tx);
    }

    /// Set format filter explicitly, preserving cursor
    pub fn set_format_filter(&mut self, filter: FormatFilter) {
        self.set_format_filter_with_search(filter, None);
    }

    pub fn set_format_filter_with_search(
        &mut self,
        filter: FormatFilter,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        self.format_filter = filter;
        self.reapply_after_browse_preference_change(tx);
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
                let path = entry.path.clone();
                self.invalidate_path_validation();
                self.push_nav_history(path.clone());
                self.current_dir = path;
                self.selected_index = 0;
                self.reset_nav_state();
                self.reset_sort_to_default();
                self.sync_tree_to_current_dir();
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
            let parent = parent.to_path_buf();
            self.invalidate_path_validation();
            self.push_nav_history(parent.clone());
            self.current_dir = parent;
            self.reset_nav_state();
            self.reset_sort_to_default();
            self.sync_tree_to_current_dir();
            self.refresh();
            return true;
        }
        false
    }

    /// Navigate directly to a given path
    pub fn navigate_to(&mut self, path: PathBuf) {
        if path.is_dir() {
            self.invalidate_path_validation();
            self.push_nav_history(path.clone());
            self.current_dir = path;
            self.selected_index = 0;
            self.reset_nav_state();
            self.reset_sort_to_default();
            self.sync_tree_to_current_dir();
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
        if let Some(tx) = self.scan_tx.clone() {
            let input_str = input.to_string();
            let origin_dir = self.current_dir.clone();
            let generation = self.next_path_validation_generation();
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
                    .send(crate::tui::message::AppMessage::PathValidationComplete {
                        generation,
                        origin_dir,
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
            self.invalidate_path_validation();
            self.push_nav_history(final_path.clone());
            self.current_dir = final_path;
            self.selected_index = 0;
            self.reset_nav_state();
            self.reset_sort_to_default();
            self.sync_tree_to_current_dir();
            self.refresh();
            Ok(())
        }
    }


    /// Resolve a synthetic archive-entry path back to the relative path stored
    /// in the archive listing. Returns `None` outside archive-browse mode or
    /// for the synthetic parent (`..`) row.
    pub fn archive_inner_path_for_path(&self, path: &Path) -> Option<String> {
        let arc = self.archive.as_ref()?;
        if path == Path::new("..") {
            return None;
        }
        if let Ok(relative) = path.strip_prefix(&arc.listing.archive_path) {
            if let Some(normalized) = normalize_archive_relative_path(relative) {
                if !normalized.is_empty() {
                    return Some(normalized);
                }
            }
        }

        // Fallback for callers holding a BrowseEntry whose synthetic path could
        // not be stripped byte-for-byte (for example after a relative/absolute
        // path normalization difference). Rebuild it from the archive-local
        // directory plus the visible entry name, and accept it only if the
        // listing contains that exact file or directory.
        let name = path.file_name().and_then(|name| name.to_str())?;
        let candidate = if arc.inner_path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", arc.inner_path, name)
        };
        arc.listing
            .entries
            .iter()
            .any(|entry| entry.path == candidate)
            .then_some(candidate)
    }

    pub fn archive_inner_path_for_entry(&self, entry: &BrowseEntry) -> Option<String> {
        if matches!(entry.kind, EntryKind::ParentDir) {
            return None;
        }
        self.archive_inner_path_for_path(&entry.path)
    }

    pub fn archive_entry_for_path(&self, path: &Path) -> Option<&crate::tui::archive_listing::ArchiveEntry> {
        let inner = self.archive_inner_path_for_path(path)?;
        self.archive
            .as_ref()?
            .listing
            .entries
            .iter()
            .find(|entry| entry.path == inner)
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
        self.toggle_hidden_with_search(None);
    }

    /// Toggle hidden-file visibility while preserving the active search
    /// contract. If the search panel is active, changing visibility is a
    /// search-identity change, not an ordinary directory-filter change: cancel
    /// any recursive worker and re-run the current query with the new setting.
    pub fn toggle_hidden_with_search(
        &mut self,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        self.show_hidden = !self.show_hidden;
        self.rebuild_tree_preserving_expansion();

        if self.search.active {
            self.restart_active_search_after_preference_change(tx);
        } else {
            // Hidden files were captured by scan(); just re-apply the view layer.
            self.apply_view_preserving_cursor();
        }
    }

    /// Re-run the active search after a visibility/filter identity change.
    /// This intentionally bypasses `apply_view_preserving_cursor()`: while the
    /// search panel is active, `entries` must remain a search result set, not
    /// the ordinary directory listing.
    fn restart_active_search_after_preference_change(
        &mut self,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        if self.archive.is_some() {
            if self.archive_search_requires_async(self.search.mode, self.search.sort) && tx.is_none() {
                if let Some(ref flag) = self.search.cancel {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                self.search.cancel = None;
                self.search.searching = false;
                self.search.generation = self.search.generation.wrapping_add(1);
                self.search.last_keystroke = Some(std::time::Instant::now());
            } else {
                self.execute_search(tx);
            }
            return;
        }

        if !self.search.recursive {
            self.execute_search(tx);
            return;
        }

        if let Some(tx) = tx {
            self.execute_search(Some(tx));
        } else {
            if let Some(ref flag) = self.search.cancel {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            self.search.cancel = None;
            self.search.searching = false;
            self.search.generation = self.search.generation.wrapping_add(1);
            self.search.last_keystroke = Some(std::time::Instant::now());
        }
    }

    /// Re-apply the visible Browse entries after a view preference changes.
    /// The key invariant is that an open search panel owns `entries`: while
    /// search is active, preferences such as Show Hidden, Filter, Default sort,
    /// and Restore defaults must re-run the active search (or schedule its
    /// debounce) instead of repopulating the ordinary directory listing under
    /// an apparently active search UI.
    pub fn reapply_after_browse_preference_change(
        &mut self,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        if self.search.active {
            self.restart_active_search_after_preference_change(tx);
        } else {
            self.apply_view_preserving_cursor();
        }
    }

    /// Apply freshly scanned directory contents to the visible Browse list.
    ///
    /// Search is an ownership boundary for `entries`: while the search panel is
    /// open, a directory scan completion must re-run the active search against
    /// the refreshed raw scan data rather than publishing the ordinary Browse
    /// listing under an apparently active search UI.
    pub fn reapply_after_directory_scan_complete(
        &mut self,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        if self.search.active {
            self.restart_active_search_after_preference_change(tx);
        } else {
            self.apply_view();
        }
    }

    /// Invalidate an in-flight recursive search before a refresh rescan. The
    /// replacement is launched after the scan publishes fresh raw entries.
    fn invalidate_recursive_search_for_refresh(&mut self) {
        if !(self.search.active && self.search.recursive) {
            return;
        }
        if let Some(ref flag) = self.search.cancel {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.search.cancel = None;
        self.search.searching = false;
        self.search.last_keystroke = None;
        self.search.generation = self.search.generation.wrapping_add(1);
    }

    /// Close search without applying the ordinary listing. Directory/archive
    /// navigation calls this before it refreshes the destination, so applying
    /// the old directory's raw entries here would briefly create an incoherent
    /// state.
    fn close_search_for_navigation(&mut self) {
        if let Some(ref flag) = self.search.cancel {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.search.active = false;
        self.search.input = TextInputState::new(String::new());
        self.search.focus = SearchFocus::Input;
        self.search.last_keystroke = None;
        self.search.searching = false;
        self.clear_search_tag_cache();
        self.search.cancel = None;
        self.search.generation = self.search.generation.wrapping_add(1);
    }

    fn clear_archive_tag_cache_for_archive(&mut self, archive_path: &Path) {
        self.search
            .archive_tag_cache
            .retain(|synthetic_path, _| !synthetic_path.starts_with(archive_path));
    }

    fn rebuild_tree_preserving_expansion(&mut self) {
        let expanded_paths = self
            .tree_nodes
            .iter()
            .filter(|node| node.expanded)
            .map(|node| node.path.clone())
            .collect::<Vec<_>>();
        let cursor_path = self.tree_nodes.get(self.tree_cursor).map(|node| node.path.clone());
        let root = self
            .tree_nodes
            .first()
            .map(|node| node.path.clone())
            .filter(|root| self.current_dir.starts_with(root))
            .unwrap_or_else(|| self.current_dir.clone());

        self.tree_nodes = initial_browse_tree_nodes(&root, self.show_hidden);
        for path in expanded_paths {
            browse_tree_expand_path(&mut self.tree_nodes, &path, self.show_hidden);
        }
        self.sync_tree_to_current_dir();
        if let Some(cursor_path) = cursor_path {
            if let Some(index) = self
                .tree_nodes
                .iter()
                .position(|node| same_path(&node.path, &cursor_path))
            {
                self.tree_cursor = index;
                self.ensure_tree_visible();
            }
        }
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

    /// Invalidate any in-flight async path validation.
    fn invalidate_path_validation(&mut self) {
        self.path_validation_generation = self.path_validation_generation.wrapping_add(1);
    }

    /// Allocate the next async path-validation generation.
    fn next_path_validation_generation(&mut self) -> u64 {
        self.invalidate_path_validation();
        self.path_validation_generation
    }

    /// Return true only for the newest async path validation launched from the
    /// still-current directory. This protects the reducer from late workers
    /// after the user enters another path, navigates elsewhere, or reopens the
    /// path editor for a new edit.
    pub fn is_current_path_validation(&self, generation: u64, origin_dir: &Path) -> bool {
        self.path_input.is_none()
            && generation == self.path_validation_generation
            && same_path(&self.current_dir, origin_dir)
    }

    /// Open the path bar input, seeded with the current directory.
    pub fn open_path_input(&mut self) {
        // A newly opened path editor supersedes any submitted async path
        // validation. Late completions must not navigate while the user is
        // actively composing a different path.
        self.invalidate_path_validation();

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

    /// Open the search panel, or focus its input if already open.
    ///
    /// Reopening search must not blank the current query while leaving old
    /// result rows visible. Treat repeated toolbar/Search-key activation as a
    /// focus action; explicit close/toggle paths continue to call close_search().
    pub fn open_search(&mut self) {
        if self.search.active {
            self.search.focus = SearchFocus::Input;
            return;
        }
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
        self.clear_search_tag_cache();
        self.search.cancel = None;
        self.search.generation = self.search.generation.wrapping_add(1);
        // Restore normal listing.
        self.apply_view_preserving_cursor();
    }

    /// Execute a search. Non-recursive runs synchronously.
    /// Recursive spawns an async task and sends results via tx.
    pub fn execute_search(
        &mut self,
        tx: Option<&tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    ) {
        if let Some(ref flag) = self.search.cancel {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.search.cancel = None;
        self.search.searching = false;
        self.search.generation = self.search.generation.wrapping_add(1);

        let query = self.search.input.text.trim().to_ascii_lowercase();
        if query.is_empty() {
            self.apply_view_preserving_cursor();
            return;
        }

        let generation = self.search.generation;
        let show_hidden = self.show_hidden;
        let audio_only = self.search.audio_only;
        let format_filter = self.format_filter;
        let mode = self.search.mode;
        let sort = self.search.sort;
        let sort_dir = self.search.sort_dir;
        let result_cap = self.search_result_cap.max(1);

        if self.archive.is_some() {
            if self.archive_search_requires_async(mode, sort) {
                if let Some(tx) = tx {
                    self.spawn_archive_search_async(
                        generation,
                        &query,
                        show_hidden,
                        audio_only,
                        format_filter,
                        mode,
                        sort,
                        sort_dir,
                        result_cap,
                        self.search.recursive,
                        tx.clone(),
                    );
                } else {
                    // Unit-test and synchronous fallback path only. The normal
                    // interactive app has a tx and must use the async worker so
                    // archive tag extraction cannot freeze the TUI.
                    self.execute_search_archive(&query, show_hidden, audio_only, format_filter, mode, self.search.recursive);
                }
            } else {
                self.execute_search_archive(&query, show_hidden, audio_only, format_filter, mode, self.search.recursive);
            }
        } else if self.search.recursive {
            if let Some(tx) = tx {
                self.spawn_search_async(
                    generation,
                    &query,
                    show_hidden,
                    audio_only,
                    format_filter,
                    mode,
                    sort,
                    sort_dir,
                    result_cap,
                    tx.clone(),
                );
            }
        } else {
            self.execute_search_local(&query, show_hidden, audio_only, format_filter, mode);
        }
    }

    /// Non-recursive search: filter the current raw model.
    fn execute_search_local(
        &mut self,
        query: &str,
        show_hidden: bool,
        audio_only: bool,
        format_filter: FormatFilter,
        mode: SearchMode,
    ) {
        let mut sources = Vec::with_capacity(self.all_dirs.len() + self.all_files.len());
        sources.extend(self.all_dirs.iter().cloned());
        sources.extend(self.all_files.iter().cloned());
        self.execute_search_over_entries(query, show_hidden, audio_only, format_filter, mode, sources);
    }

    /// Archive search never falls back to the parent filesystem directory.
    /// Non-recursive search uses the current archive directory's raw model;
    /// recursive search builds an archive-local descendant model from the
    /// listing or active staging tree and searches that synchronously.
    fn execute_search_archive(
        &mut self,
        query: &str,
        show_hidden: bool,
        audio_only: bool,
        format_filter: FormatFilter,
        mode: SearchMode,
        recursive: bool,
    ) {
        let sources = if recursive {
            self.archive_recursive_search_entries()
        } else {
            let mut entries = Vec::with_capacity(self.all_dirs.len() + self.all_files.len());
            entries.extend(self.all_dirs.iter().cloned());
            entries.extend(self.all_files.iter().cloned());
            entries
        };
        self.search.searching = false;
        self.search.cancel = None;
        self.execute_search_over_entries(query, show_hidden, audio_only, format_filter, mode, sources);
    }

    fn archive_search_requires_async(&self, mode: SearchMode, sort: SearchSort) -> bool {
        self.archive.is_some() && (matches!(mode, SearchMode::Tags | SearchMode::Both) || sort.is_tag_sort())
    }

    fn archive_search_candidates(&self, recursive: bool) -> Vec<ArchiveSearchCandidate> {
        let sources = if recursive {
            self.archive_recursive_search_entries()
        } else {
            let mut entries = Vec::with_capacity(self.all_dirs.len() + self.all_files.len());
            entries.extend(self.all_dirs.iter().cloned());
            entries.extend(self.all_files.iter().cloned());
            entries
        };

        sources
            .into_iter()
            .map(|entry| {
                let inner_path = self.archive_inner_path_for_entry(&entry);
                let staged_path = self.archive_staged_path_for_entry(&entry);
                let fallback_metadata = self
                    .valid_probe_for_entry(&entry)
                    .map(|cached| cached.metadata.clone());
                ArchiveSearchCandidate {
                    entry,
                    inner_path,
                    staged_path,
                    fallback_metadata,
                }
            })
            .collect()
    }

    /// Spawn archive-local search when tag matching or tag sorting may require
    /// reading member metadata. This keeps extraction-backed tag reads off the
    /// TUI thread and gives the reducer the same generation/identity guard used
    /// by filesystem recursive search.
    fn spawn_archive_search_async(
        &mut self,
        generation: u64,
        query: &str,
        show_hidden: bool,
        audio_only: bool,
        format_filter: FormatFilter,
        mode: SearchMode,
        sort: SearchSort,
        sort_dir: SortDir,
        result_cap: usize,
        recursive: bool,
        tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        let Some(arc) = self.archive.as_ref() else { return; };
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.search.cancel = Some(cancel.clone());
        self.search.searching = true;

        let root = self.current_dir.clone();
        let archive_path = arc.listing.archive_path.clone();
        let archive_inner_path = arc.inner_path.clone();
        let password = arc.password.clone();
        let archive_fingerprint = TagCacheFingerprint::for_path(&archive_path);
        let cached_archive_tags = self.search.archive_tag_cache.clone();
        let candidates = self.archive_search_candidates(recursive);
        let query = query.to_string();

        tokio::spawn(async move {
            let query_for_worker = query.clone();
            let archive_path_for_worker = archive_path.clone();
            let password_for_worker = password.clone();
            let result = tokio::task::spawn_blocking(move || {
                run_archive_search_worker(
                    archive_path_for_worker,
                    password_for_worker,
                    archive_fingerprint,
                    cached_archive_tags,
                    candidates,
                    query_for_worker,
                    show_hidden,
                    audio_only,
                    format_filter,
                    mode,
                    sort,
                    sort_dir,
                    result_cap,
                    cancel,
                )
            })
            .await
            .unwrap_or(None);

            if let Some(output) = result {
                let _ = tx
                    .send(crate::tui::message::AppMessage::SearchComplete {
                        generation,
                        root,
                        recursive,
                        archive_path: Some(archive_path),
                        archive_inner_path: Some(archive_inner_path),
                        query,
                        mode,
                        show_hidden,
                        audio_only,
                        format_filter,
                        sort,
                        sort_dir,
                        result_cap,
                        total_matches: output.total_matches,
                        pre_sorted: true,
                        archive_tag_cache_updates: output.archive_tag_cache_updates,
                        results: output.results,
                    })
                    .await;
            }
        });
    }

    fn archive_staged_path_for_entry(&self, entry: &BrowseEntry) -> Option<PathBuf> {
        let inner = self.archive_inner_path_for_entry(entry)?;
        let staging = self.active_archive_staging()?;
        let staged = staging_path_for_archive_inner(&staging.staging_dir, &inner).ok()?;
        staged.is_file().then_some(staged)
    }

    fn tag_source_for_entry(&self, entry: &BrowseEntry) -> TagSearchSource {
        if let Some(arc) = self.archive.as_ref() {
            let fallback_metadata = self
                .valid_probe_for_entry(entry)
                .map(|cached| cached.metadata.clone());

            if let Some(staged_path) = self.archive_staged_path_for_entry(entry) {
                return TagSearchSource::StagedArchiveEntry {
                    staged_path,
                    fallback_metadata,
                };
            }

            if let Some(metadata) = fallback_metadata {
                return TagSearchSource::Metadata(metadata);
            }

            if entry.is_audio() {
                if let Some(inner_path) = self.archive_inner_path_for_entry(entry) {
                    return TagSearchSource::ExtractArchiveEntry {
                        archive_path: arc.listing.archive_path.clone(),
                        inner_path,
                        password: arc.password.clone(),
                        synthetic_path: entry.path.clone(),
                    };
                }
            }

            return TagSearchSource::Missing;
        }

        TagSearchSource::Filesystem(entry.path.clone())
    }

    fn tag_search_string_for_entry(&mut self, entry: &BrowseEntry) -> String {
        match self.tag_source_for_entry(entry) {
            TagSearchSource::Filesystem(path) => {
                build_tag_search_string_cached(&path, &mut self.search.tag_cache)
            }
            TagSearchSource::StagedArchiveEntry {
                staged_path,
                fallback_metadata,
            } => {
                let from_staged = build_tag_search_string_cached(&staged_path, &mut self.search.tag_cache);
                if !from_staged.is_empty() {
                    from_staged
                } else {
                    fallback_metadata
                        .as_ref()
                        .map(tag_search_string_from_metadata)
                        .unwrap_or_default()
                }
            }
            TagSearchSource::Metadata(metadata) => tag_search_string_from_metadata(&metadata),
            TagSearchSource::ExtractArchiveEntry {
                archive_path,
                inner_path,
                password,
                synthetic_path,
            } => self
                .archive_tag_read_result_cached(&archive_path, &inner_path, password.as_deref(), &synthetic_path)
                .tag_string,
            TagSearchSource::Missing => String::new(),
        }
    }

    fn tag_sort_key_for_entry(&mut self, entry: &BrowseEntry, sort: SearchSort) -> String {
        match self.tag_source_for_entry(entry) {
            TagSearchSource::Filesystem(path) => extract_tag_sort_key(&path, sort),
            TagSearchSource::StagedArchiveEntry {
                staged_path,
                fallback_metadata,
            } => {
                let from_staged = extract_tag_sort_key(&staged_path, sort);
                if !from_staged.is_empty() {
                    from_staged
                } else {
                    fallback_metadata
                        .as_ref()
                        .map(|metadata| tag_sort_key_from_metadata(metadata, sort))
                        .unwrap_or_default()
                }
            }
            TagSearchSource::Metadata(metadata) => tag_sort_key_from_metadata(&metadata, sort),
            TagSearchSource::ExtractArchiveEntry {
                archive_path,
                inner_path,
                password,
                synthetic_path,
            } => {
                let tags = self.archive_tag_read_result_cached(
                    &archive_path,
                    &inner_path,
                    password.as_deref(),
                    &synthetic_path,
                );
                tag_sort_key_from_read_result(&tags, sort)
            }
            TagSearchSource::Missing => String::new(),
        }
    }

    fn archive_tag_read_result_cached(
        &mut self,
        archive_path: &Path,
        inner_path: &str,
        password: Option<&str>,
        synthetic_path: &Path,
    ) -> TagReadResult {
        let fingerprint = match TagCacheFingerprint::for_path(archive_path) {
            Some(fingerprint) => fingerprint,
            None => {
                self.search.archive_tag_cache.remove(synthetic_path);
                return TagReadResult::empty();
            }
        };

        let password_identity = ArchiveTagPasswordIdentity::for_password(password);
        if let Some(cached) = self.search.archive_tag_cache.get(synthetic_path) {
            if cached.archive_fingerprint == fingerprint
                && cached.password_identity == password_identity
                && cached.tags.has_tag_data()
            {
                return cached.tags.clone();
            }
        }

        let tags = read_tags_from_archive_entry(archive_path, inner_path, password);
        if tags.has_tag_data() {
            self.search.archive_tag_cache.insert(
                synthetic_path.to_path_buf(),
                CachedArchiveTagSearchString {
                    archive_fingerprint: fingerprint,
                    password_identity,
                    tags: tags.clone(),
                },
            );
        } else {
            self.search.archive_tag_cache.remove(synthetic_path);
        }
        tags
    }

    fn sort_search_results_for_current_context(&mut self, scored: &mut Vec<(BrowseEntry, i64)>) {
        let sort = self.search.sort;
        let dir = self.search.sort_dir;

        if !sort.is_tag_sort() {
            sort_search_results(scored, sort, dir);
            return;
        }

        let entries: Vec<BrowseEntry> = scored.iter().map(|(entry, _)| entry.clone()).collect();
        let mut keyed: Vec<(bool, String, String, usize)> = entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                let key = self.tag_sort_key_for_entry(entry, sort);
                (key.is_empty(), key, entry.name_lower.clone(), idx)
            })
            .collect();

        keyed.sort_by(|a, b| {
            let ord = a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(&b.2));
            match dir {
                SortDir::Asc => ord,
                SortDir::Desc => ord.reverse(),
            }
        });

        let sorted: Vec<_> = keyed.iter().map(|(_, _, _, idx)| scored[*idx].clone()).collect();
        *scored = sorted;
    }

    fn execute_search_over_entries(
        &mut self,
        query: &str,
        show_hidden: bool,
        audio_only: bool,
        format_filter: FormatFilter,
        mode: SearchMode,
        sources: Vec<BrowseEntry>,
    ) {
        use fuzzy_matcher::skim::SkimMatcherV2;
        use fuzzy_matcher::FuzzyMatcher;

        let matcher = SkimMatcherV2::default();
        let min_score = search_fuzzy_min_score(query);
        let bounded_filename_score_results = matches!(mode, SearchMode::Filename)
            && matches!(self.search.sort, SearchSort::Score);
        let cap = self.search_result_cap.max(1);
        let mut scored: Vec<(BrowseEntry, i64)> = Vec::new();
        let mut bounded_scored = bounded_filename_score_results
            .then(|| BoundedScoreSearchResults::new(cap, self.search.sort_dir));
        let parent = self.parent_entry.clone();
        let search_tags = matches!(mode, SearchMode::Tags | SearchMode::Both);
        let search_filename = matches!(mode, SearchMode::Filename | SearchMode::Both);

        for e in &sources {
            if !show_hidden && browse_entry_name_is_hidden(&e.name) {
                continue;
            }
            if !entry_passes_search_effective_filters(e, show_hidden, audio_only, &format_filter) {
                continue;
            }

            let mut best_score: Option<i64> = None;

            // Directories always match on filename (for navigation), even in
            // tags-only mode.
            if search_filename || matches!(&e.kind, EntryKind::Directory) {
                if let Some(s) = matcher.fuzzy_match(&e.name_lower, query) {
                    best_score = Some(best_score.map_or(s, |prev: i64| prev.max(s)));
                }
            }

            if search_tags && matches!(&e.kind, EntryKind::AudioFile(_)) {
                let tag_str = self.tag_search_string_for_entry(e);
                if !tag_str.is_empty() {
                    if let Some(s) = matcher.fuzzy_match(&tag_str, query) {
                        best_score = Some(best_score.map_or(s, |prev: i64| prev.max(s)));
                    }
                }
            }

            if let Some(score) = best_score {
                if search_fuzzy_score_passes_threshold(score, min_score) {
                    if let Some(bounded) = bounded_scored.as_mut() {
                        bounded.push(e.clone(), score);
                    } else {
                        scored.push((e.clone(), score));
                    }
                }
            }
        }

        if let Some(bounded) = bounded_scored {
            scored = bounded.into_vec();
        }
        self.sort_search_results_for_current_context(&mut scored);
        scored.truncate(cap);

        let mut results: Vec<BrowseEntry> = Vec::new();
        if let Some(p) = parent {
            results.push(p);
        }
        results.extend(scored.into_iter().map(|(e, _)| e));

        self.entries = results;
        self.selected_index = 0;
        self.scroll_offset = 0;
    }

    fn archive_recursive_search_entries(&self) -> Vec<BrowseEntry> {
        let arc = match &self.archive {
            Some(arc) => arc,
            None => return Vec::new(),
        };
        let prefix = if arc.inner_path.is_empty() {
            String::new()
        } else {
            format!("{}/", arc.inner_path)
        };
        let mut dirs = Vec::new();
        let mut files = Vec::new();

        if let Some(staging) = arc.staging.as_ref() {
            let root = staging.staging_dir.join(&arc.inner_path);
            for entry in walkdir::WalkDir::new(&root).min_depth(1).into_iter().filter_map(Result::ok) {
                let path = entry.path().to_path_buf();
                let Ok(meta) = entry.metadata() else { continue };
                let is_dir = meta.is_dir();
                let Ok(relative_to_staging) = path.strip_prefix(&staging.staging_dir) else { continue };
                let Some(inner) = normalize_archive_relative_path(relative_to_staging) else { continue };
                if inner.is_empty() {
                    continue;
                }
                let display_name = inner
                    .strip_prefix(&prefix)
                    .unwrap_or(&inner)
                    .to_string();
                let kind = if is_dir { EntryKind::Directory } else { classify_file(&path) };
                let entry = BrowseEntry::new(
                    arc.listing.archive_path.join(&inner),
                    display_name,
                    kind,
                    if is_dir { 0 } else { meta.len() },
                    meta.modified().ok(),
                );
                if is_dir { dirs.push(entry); } else { files.push(entry); }
            }
        } else {
            for item in &arc.listing.entries {
                if !prefix.is_empty() && !item.path.starts_with(&prefix) {
                    continue;
                }
                if item.path == arc.inner_path {
                    continue;
                }
                let display_name = item
                    .path
                    .strip_prefix(&prefix)
                    .unwrap_or(&item.path)
                    .to_string();
                if display_name.is_empty() {
                    continue;
                }
                let kind = if item.is_dir {
                    EntryKind::Directory
                } else {
                    classify_file(Path::new(&item.path))
                };
                let entry = BrowseEntry::new(
                    arc.listing.archive_path.join(&item.path),
                    display_name,
                    kind,
                    item.size,
                    None,
                );
                if item.is_dir { dirs.push(entry); } else { files.push(entry); }
            }
        }

        sort_entries(&mut dirs, self.sort_by, self.sort_dir, &self.probe_cache);
        sort_entries(&mut files, self.sort_by, self.sort_dir, &self.probe_cache);
        dirs.extend(files);
        dirs
    }

    /// Spawn an async recursive search task. Results arrive via SearchComplete.
    fn spawn_search_async(
        &mut self,
        generation: u64,
        query: &str,
        show_hidden: bool,
        audio_only: bool,
        format_filter: FormatFilter,
        mode: SearchMode,
        sort: SearchSort,
        sort_dir: SortDir,
        result_cap: usize,
        tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.search.cancel = Some(cancel.clone());
        self.search.searching = true;

        let root = self.current_dir.clone();
        let query = query.to_string();
        let root_for_worker = root.clone();
        let query_for_worker = query.clone();
        let search_tags = matches!(mode, SearchMode::Tags | SearchMode::Both);
        let search_filename = matches!(mode, SearchMode::Filename | SearchMode::Both);

        tokio::spawn(async move {
            let results =
                tokio::task::spawn_blocking(move || -> Option<(Vec<(BrowseEntry, i64)>, usize)> {
                    use fuzzy_matcher::skim::SkimMatcherV2;
                    use fuzzy_matcher::FuzzyMatcher;
                    use walkdir::WalkDir;

                    let matcher = SkimMatcherV2::default();
                    let min_score = search_fuzzy_min_score(&query_for_worker);
                    let mut scored: Vec<(BrowseEntry, i64)> = Vec::new();
                    let bounded_filename_score_results = matches!(mode, SearchMode::Filename)
                        && matches!(sort, SearchSort::Score);
                    let mut bounded_scored = bounded_filename_score_results
                        .then(|| BoundedScoreSearchResults::new(result_cap, sort_dir));
                    let mut total_matches = 0usize;

                    // Open own DB connection for tag cache.
                    let db = crate::db::Database::open().ok();

                    for entry in WalkDir::new(&root_for_worker)
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
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
                        let rel = path
                            .strip_prefix(&root_for_worker)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .to_string();
                        let candidate = BrowseEntry::new(path.clone(), rel, kind, size, modified);

                        if !entry_passes_search_effective_filters(&candidate, show_hidden, audio_only, &format_filter) {
                            continue;
                        }

                        let mut best_score: Option<i64> = None;

                        if search_filename {
                            if let Some(s) = matcher.fuzzy_match(&candidate.name_lower, &query_for_worker) {
                                best_score = Some(s);
                            }
                        }

                        if search_tags && matches!(&candidate.kind, EntryKind::AudioFile(_)) {
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
                                    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                        return None;
                                    }
                                    match read_tags_from_file_checked(&path) {
                                        Ok(r) => {
                                            // A readable file with no tags is deterministic for
                                            // this identity and may be cached. Lofty/open errors
                                            // are transient and deliberately stay out of SQLite.
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
                                        Err(_) => String::new(),
                                    }
                                }
                            } else {
                                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                    return None;
                                }
                                read_tags_from_file_checked(&path)
                                    .map(|tags| tags.tag_string)
                                    .unwrap_or_default()
                            };

                            if !tag_str.is_empty() {
                                if let Some(s) = matcher.fuzzy_match(&tag_str, &query_for_worker) {
                                    best_score =
                                        Some(best_score.map_or(s, |prev: i64| prev.max(s)));
                                }
                            }
                        }

                        if let Some(score) = best_score {
                            if search_fuzzy_score_passes_threshold(score, min_score) {
                                total_matches = total_matches.saturating_add(1);
                                if let Some(bounded) = bounded_scored.as_mut() {
                                    bounded.push(candidate, score);
                                } else {
                                    scored.push((candidate, score));
                                }
                            }
                        }
                    }

                    if let Some(bounded) = bounded_scored {
                        scored = bounded.into_vec();
                    } else {
                        total_matches = scored.len();
                    }
                    Some((scored, total_matches))
                })
                .await
                .unwrap_or(None);

            // Only send results if not cancelled. The reducer still validates
            // the complete launch identity before mutating Browse state.
            if let Some((results, total_matches)) = results {
                let _ = tx
                    .send(crate::tui::message::AppMessage::SearchComplete {
                        generation,
                        root,
                        recursive: true,
                        archive_path: None,
                        archive_inner_path: None,
                        query,
                        mode,
                        show_hidden,
                        audio_only,
                        format_filter,
                        sort,
                        sort_dir,
                        result_cap,
                        total_matches,
                        pre_sorted: false,
                        archive_tag_cache_updates: Vec::new(),
                        results,
                    })
                    .await;
            }
        });
    }

    /// Reset filter state AND clear the multi-select anchor, used by navigation
    /// methods. The anchor is for range-select (Alt+click) and is a
    /// per-directory context.
    fn reset_nav_state(&mut self) {
        self.close_search_for_navigation();
        self.reset_filter_state();
        self.multi_select_anchor = None;
        self.clear_type_ahead();
        self.clear_pending_inline_rename_after_scan();
        self.probe_debounce = None;
        self.clear_browse_cold_probe_queue();
        self.clear_dir_stats_work_queue();
        self.clear_search_tag_cache();
    }

    /// Clear all tag-search text cached by the active Browse search panel.
    pub fn clear_search_tag_cache(&mut self) {
        self.search.tag_cache.clear();
        self.search.archive_tag_cache.clear();
    }

    /// Invalidate local tag-search cache entries that may correspond to a file
    /// whose metadata changed. For normal filesystem browsing this removes the
    /// file path directly. For active archive metadata editing, the completed
    /// write path is usually inside the private staging directory while Browse
    /// rows use a synthetic `archive_path/inner/path` key; remove both.
    pub fn invalidate_search_tag_cache_for_metadata_path(&mut self, path: &Path) {
        self.search.tag_cache.remove(path);

        if let Ok(canonical) = path.canonicalize() {
            if canonical != path {
                self.search.tag_cache.remove(&canonical);
            }
        }

        self.search.archive_tag_cache.remove(path);

        if let Some(staging) = self.active_archive_staging().cloned() {
            if let Ok(relative) = path.strip_prefix(&staging.staging_dir) {
                let synthetic = staging.archive_path.join(relative);
                self.search.tag_cache.remove(&synthetic);
                self.search.archive_tag_cache.remove(&synthetic);
            }
        }
    }

    /// Schedule a sequential inline rename to resume after the async scan for
    /// `directory` repopulates `entries`. The target path is captured before the
    /// current rename is committed so sorting/filtering changes cannot make the
    /// continuation depend on stale indices.
    pub fn schedule_inline_rename_after_scan(
        &mut self,
        scan_generation: u64,
        directory: PathBuf,
        target_path: PathBuf,
    ) {
        self.pending_inline_rename_after_scan = Some(PendingBrowseInlineRenameAfterScan {
            scan_generation,
            directory,
            target_path,
        });
    }

    pub fn pending_scan_generation(&self) -> Option<u64> {
        self.scan_pending.as_ref().map(ScanHandle::generation)
    }

    /// Accept only the currently pending scan for the still-current directory.
    pub fn is_current_dir_scan(&self, generation: u64, scan_path: &Path) -> bool {
        self.scan_pending
            .as_ref()
            .is_some_and(|handle| handle.generation() == generation)
            && same_path(&self.current_dir, scan_path)
    }

    /// Mark a matching scan generation as terminal. Returns true only when the
    /// completion belonged to the current pending scan.
    pub fn finish_dir_scan_if_current(&mut self, generation: u64, scan_path: &Path) -> bool {
        if !self.is_current_dir_scan(generation, scan_path) {
            return false;
        }
        self.scan_pending = None;
        true
    }

    /// Clear any deferred sequential inline rename. Used when navigation or
    /// other user actions supersede the directory refresh that created it.
    pub fn clear_pending_inline_rename_after_scan(&mut self) {
        self.pending_inline_rename_after_scan = None;
    }

    /// Clear a deferred sequential inline rename for a terminal scan generation.
    pub fn clear_pending_inline_rename_after_scan_generation(&mut self, generation: u64) {
        if self
            .pending_inline_rename_after_scan
            .as_ref()
            .is_some_and(|pending| pending.scan_generation == generation)
        {
            self.pending_inline_rename_after_scan = None;
        }
    }

    /// Consume a pending sequential inline rename after a successful scan.
    /// Returns the target path after positioning the cursor, or `None` if the
    /// pending action was stale or the target no longer exists in the filtered view.
    pub fn take_inline_rename_after_scan_target(
        &mut self,
        scan_generation: u64,
        scan_path: &Path,
    ) -> Option<PathBuf> {
        let pending = self.pending_inline_rename_after_scan.take()?;
        if pending.scan_generation != scan_generation || !same_path(&pending.directory, scan_path) {
            return None;
        }

        let idx = self.entries.iter().position(|entry| {
            entry.path == pending.target_path && !matches!(entry.kind, EntryKind::ParentDir)
        })?;
        self.selected_index = idx;
        self.ensure_visible();
        Some(pending.target_path)
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

    /// Best-effort current filesystem identity for a probeable entry. A focused
    /// cache hit pays one cheap metadata stat so external edits cannot keep
    /// stale in-memory probe data alive until a full directory refresh.
    fn current_filesystem_probe_identity(entry: &BrowseEntry) -> Option<ProbeCacheIdentity> {
        std::fs::metadata(&entry.path)
            .ok()
            .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
    }

    /// Classify whether a filesystem-backed async completion still applies.
    /// This implements the Browse stale-result invariant: same identity accepts,
    /// changed identity invalidates and may be re-evaluated once, and missing or
    /// unreadable paths stop without retrying until a scan/refresh supplies a
    /// valid entry again.
    pub fn classify_filesystem_async_completion(
        &self,
        path: &Path,
        launched_identity: ProbeCacheIdentity,
    ) -> FilesystemAsyncCompletion {
        match std::fs::metadata(path)
            .ok()
            .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
        {
            Some(current) if current == launched_identity => FilesystemAsyncCompletion::Accept,
            Some(_) => FilesystemAsyncCompletion::Changed,
            None => FilesystemAsyncCompletion::MissingOrUnstatable,
        }
    }

    /// A stale completion is allowed to trigger exactly one fresh evaluation
    /// only while the same path remains focused and is still statable.
    pub fn current_entry_is_still_statable(&self, path: &Path) -> bool {
        self.is_current_entry_path(path) && std::fs::metadata(path).is_ok()
    }

    /// Return a valid in-memory probe-cache value for `entry`, removing stale
    /// path hits whose recorded identity no longer matches the current entry.
    fn cached_probe_for_entry(
        &mut self,
        entry: &BrowseEntry,
        identity: ProbeCacheIdentity,
    ) -> Option<Option<Arc<CachedInfo>>> {
        match self.probe_cache.get(&entry.path) {
            Some(cached) if cached.is_valid_for(identity) => Some(cached.info.clone()),
            Some(_) => {
                self.remove_probe_cache_entry(&entry.path);
                None
            }
            None => None,
        }
    }

    fn has_valid_probe_cache_entry(&mut self, entry: &BrowseEntry, identity: ProbeCacheIdentity) -> bool {
        self.cached_probe_for_entry(entry, identity).is_some()
    }

    /// Identity-checked probe info for a Browse entry. This is the canonical
    /// read path for rendering, search, sorting, and info-pane consumers.
    pub fn valid_probe_for_entry(&self, entry: &BrowseEntry) -> Option<&CachedInfo> {
        self.valid_probe_arc_for_entry(entry).map(|info| info.as_ref())
    }

    /// Identity-checked probe info as an `Arc`, for callers that must keep the
    /// cache value alive after leaving the borrowed state.
    pub fn valid_probe_arc_for_entry(&self, entry: &BrowseEntry) -> Option<&Arc<CachedInfo>> {
        let identity = ProbeCacheIdentity::from_entry(entry);
        self.valid_probe_arc_for_identity(&entry.path, identity)
    }

    /// Identity-checked probe info for a path/fingerprint pair. Use this when
    /// the caller already captured the current file identity from metadata.
    pub fn valid_probe_arc_for_identity(
        &self,
        path: &Path,
        identity: ProbeCacheIdentity,
    ) -> Option<&Arc<CachedInfo>> {
        self.probe_cache
            .get(path)
            .filter(|cached| cached.is_valid_for(identity))
            .and_then(|cached| cached.info.as_ref())
    }

    /// True for either a cached hit or deterministic negative with the same
    /// identity. Cold probe scheduling uses this to avoid retrying known
    /// non-audio files until the file changes.
    pub fn has_probe_cache_entry_for_identity(
        &self,
        path: &Path,
        identity: ProbeCacheIdentity,
    ) -> bool {
        self.probe_cache
            .get(path)
            .map(|cached| cached.is_valid_for(identity))
            .unwrap_or(false)
    }

    /// Insert a probe cache hit or miss for an explicit file identity. All
    /// production writes should go through this helper so the cache cannot
    /// regress to path-only semantics.
    pub fn insert_probe_for_identity(
        &mut self,
        path: PathBuf,
        identity: ProbeCacheIdentity,
        info: Option<Arc<CachedInfo>>,
    ) {
        self.transient_probe_failures.remove(&path);
        self.probe_cache.insert(path, ProbeCacheEntry { identity, info });
    }

    fn insert_probe_cache_hit(
        &mut self,
        path: PathBuf,
        identity: ProbeCacheIdentity,
        info: CachedInfo,
    ) -> Arc<CachedInfo> {
        let info = Arc::new(info);
        self.insert_probe_for_identity(path, identity, Some(info.clone()));
        info
    }

    pub fn insert_probe_miss_for_identity(&mut self, path: PathBuf, identity: ProbeCacheIdentity) {
        self.insert_probe_for_identity(path, identity, None);
    }

    /// Remove all in-memory probe state for a path, including pending
    /// Look up cached probe duration for a path (used by gnudb disc ID).
    pub fn probe_duration_for_path(&self, path: &Path) -> Option<f64> {
        self.probe_cache
            .get(path)
            .and_then(|cached| cached.info.as_ref())
            .map(|info| info.source.duration_secs)
    }

    /// enrichment metadata and transient retry backoff. This is the canonical
    /// invalidation path for identity-bound probe state.
    /// Test-only: insert a probe result directly into the cache without
    /// identity validation.
    #[cfg(test)]
    pub fn insert_probe_cache_for_test(&mut self, path: PathBuf, info: Option<Arc<CachedInfo>>) {
        let entry = ProbeCacheEntry {
            identity: ProbeCacheIdentity { modified: None, size: 0 },
            info,
        };
        self.probe_cache.insert(path, entry);
    }

    pub fn remove_probe_cache_entry(&mut self, path: &Path) {
        self.probe_cache.remove(path);
        self.probe_cache_needs_metadata_enrichment.remove(path);
        self.transient_probe_failures.remove(path);
    }

    fn remember_transient_probe_failure(&mut self, path: PathBuf, identity: ProbeCacheIdentity) {
        self.transient_probe_failures.insert(path, (identity, Instant::now()));
    }

    fn has_recent_transient_probe_failure(
        &mut self,
        path: &Path,
        identity: ProbeCacheIdentity,
    ) -> bool {
        let Some((cached_identity, recorded_at)) = self.transient_probe_failures.get(path).copied() else {
            return false;
        };
        if cached_identity != identity {
            self.transient_probe_failures.remove(path);
            return false;
        }
        if recorded_at.elapsed() <= TRANSIENT_PROBE_FAILURE_RETRY_DELAY {
            return true;
        }
        self.transient_probe_failures.remove(path);
        false
    }

    pub fn remember_probe_failure_for_identity(
        &mut self,
        path: PathBuf,
        identity: ProbeCacheIdentity,
        error: &str,
    ) {
        match classify_probe_failure_for_negative_cache(error) {
            NegativeCacheDecision::CacheDeterministic => {
                // A file with a stable identity but no audio stream is a true
                // negative for Browse probing. Store it only in memory and only
                // for this identity; SQLite remains success-only.
                self.insert_probe_miss_for_identity(path, identity);
            }
            NegativeCacheDecision::DoNotCacheTransient => {
                // Decode/open/extraction failures may succeed later without an
                // mtime/size change. Keep only a short backoff to prevent a hot
                // retry loop while the file is locked or transiently unreadable.
                self.remember_transient_probe_failure(path, identity);
            }
        }
    }

    /// Returns true if `path` is the currently focused Browse entry.
    pub fn is_current_entry_path(&self, path: &Path) -> bool {
        self.entries
            .get(self.selected_index)
            .map(|entry| same_path(&entry.path, path))
            .unwrap_or(false)
    }

    /// A disc-probe launch can fail before a disc fingerprint is available
    /// (for example, metadata permission errors or a disappeared path). Without
    /// a fingerprint, the only safe visible applicability proof is that the
    /// same disc-like row is still selected when the failure arrives.
    pub fn current_selected_disc_source_matches(&self, path: &Path) -> bool {
        self.entries
            .get(self.selected_index)
            .map(|entry| same_path(&entry.path, path) && entry.is_disc_source())
            .unwrap_or(false)
    }

    /// Update cached probe metadata only when the cache entry still matches an
    /// explicit filesystem identity carried or revalidated by the async caller.
    /// This is the preferred path for stale-completion-safe metadata enrichment.
    pub fn update_valid_probe_for_identity(
        &mut self,
        path: &Path,
        identity: ProbeCacheIdentity,
        update: impl FnOnce(&mut CachedInfo),
    ) -> bool {
        let Some(info) = self.valid_probe_arc_for_identity(path, identity).cloned() else {
            if self.probe_cache.contains_key(path) {
                self.remove_probe_cache_entry(path);
            }
            return false;
        };
        let mut updated = (*info).clone();
        update(&mut updated);
        self.insert_probe_for_identity(path.to_path_buf(), identity, Some(Arc::new(updated)));
        true
    }

    /// Update cached probe metadata only when the cache entry still matches the
    /// current BrowseEntry identity. Use `update_valid_probe_for_identity` when
    /// the async result carries a stronger launch or result fingerprint.
    pub fn update_valid_probe_for_current_path(
        &mut self,
        path: &Path,
        update: impl FnOnce(&mut CachedInfo),
    ) -> bool {
        let Some(identity) = self.probe_identity_for_current_entry_path(path) else {
            self.remove_probe_cache_entry(path);
            return false;
        };
        self.update_valid_probe_for_identity(path, identity, update)
    }

    /// Kick off background lookup for the currently-selected entry:
    /// - Audio files → cached info immediately, otherwise a debounced cold probe
    /// - Subdirectories (not ParentDir) → `spawn_dir_stats` (file count + total size)
    /// - Other kinds → no-op
    ///
    /// Results arrive via `AppMessage::AudioProbeComplete` or
    /// `AppMessage::DirStatsComplete` and the event loop populates the
    /// respective caches. Pending sets prevent duplicate spawns when the
    /// cursor moves rapidly back and forth.
    pub fn probe_current(&mut self, tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>) {
        self.probe_current_with_db(tx, None);
    }

    /// Probe the current selection, checking the SQLite cache first.
    /// Valid in-memory/SQLite cache hits stay immediate. Cold filesystem
    /// probes launched from the interactive event loop are delayed briefly so
    /// rapid cursor motion does not fan out into probes for every crossed row.
    pub fn probe_current_with_db(
        &mut self,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
        db: Option<&crate::db::Database>,
    ) {
        let entry = match self.entries.get(self.selected_index).cloned() {
            Some(e) => e,
            None => return,
        };

        if self.archive.is_some() {
            self.probe_debounce = None;
            if entry.is_audio() {
                let path = entry.path.clone();
                let identity = ProbeCacheIdentity::from_entry(&entry);
                if self.has_valid_probe_cache_entry(&entry, identity) || self.probe_pending.contains(&path) {
                    return;
                }
                let Some(inner_path) = self.archive_inner_path_for_entry(&entry) else {
                    return;
                };
                let Some(arc) = self.archive.as_ref() else {
                    return;
                };
                let archive_path = arc.listing.archive_path.clone();
                let probe_context = self.archive_entry_probe_context_for(&archive_path);
                if let Some(staging) = arc.staging.as_ref() {
                    match staging_path_for_archive_inner(&staging.staging_dir, &inner_path) {
                        Ok(staged_path) if staged_path.is_file() => {
                            self.probe_pending.insert(path.clone());
                            spawn_staged_archive_audio_probe(staged_path, path, probe_context, tx.clone());
                        }
                        Ok(_) => {
                            self.probe_pending.insert(path.clone());
                            let _ = tx.try_send(crate::tui::message::AppMessage::AudioProbeComplete {
                                path: path.clone(),
                                context: probe_context,
                                result: Box::new(Err("staged archive entry is missing or is not a file".to_string())),
                            });
                        }
                        Err(err) => {
                            self.probe_pending.insert(path.clone());
                            let _ = tx.try_send(crate::tui::message::AppMessage::AudioProbeComplete {
                                path: path.clone(),
                                context: probe_context,
                                result: Box::new(Err(err)),
                            });
                        }
                    }
                    return;
                }
                let password = arc.password.clone();

                self.probe_pending.insert(path.clone());
                spawn_archive_entry_audio_probe(
                    archive_path,
                    inner_path,
                    path,
                    password,
                    probe_context,
                    tx.clone(),
                );
            }
            return;
        }

        if entry.is_probeable() {
            let path = entry.path.clone();
            let Some(identity) = Self::current_filesystem_probe_identity(&entry) else {
                // The selected file was removed or became unreadable after the
                // last scan. Clear stale in-memory state and stop; a later
                // scan/refresh must provide a valid entry before probing resumes.
                self.remove_probe_cache_entry(&path);
                self.probe_debounce = None;
                self.clear_browse_cold_probe_queue();
                self.clear_browse_cold_probe_tracking_for(&path);
                return;
            };

            if let Some(cached) = self.cached_probe_for_entry(&entry, identity) {
                if let Some(info) = cached {
                    self.spawn_cached_probe_metadata_completion_if_needed(&path, identity, info, tx);
                }
                return;
            }

            if self.has_recent_transient_probe_failure(&path, identity) {
                return;
            }

            if self.probe_pending.contains(&path) || self.has_browse_cold_probe_queued_or_active(&path) {
                return;
            }

            // Check SQLite probe cache before scheduling a cold probe. Use the
            // focused file's current identity, not stale scan metadata, so an
            // external edit invalidates both memory and persistent-cache hits.
            if let Some(db) = db {
                if let Some(mtime) = identity.modified {
                    let mtime_unix = crate::db::systemtime_to_unix(mtime);
                    if let Some(row) =
                        db.get_cached_probe(&path.display().to_string(), mtime_unix, identity.size)
                    {
                        if let Some(info) = row.to_cached_info(identity.size) {
                            // The row itself is cached, but PE metadata may
                            // require tag/CUE/catalog reads. Keep that work on
                            // the browse worker path so cursor movement never
                            // performs media/tag I/O in the TUI reducer.
                            let info_arc = self.insert_probe_cache_hit(path.clone(), identity, info.clone());
                            self.probe_pending.insert(path.clone());
                            spawn_cached_audio_probe_metadata_completion(path, identity, (*info_arc).clone(), tx.clone());
                            self.probe_debounce = None;
                            return;
                        }
                    }
                }

                self.probe_debounce = Some(BrowseProbeDebounce {
                    path,
                    deadline: Instant::now() + BROWSE_PROBE_DEBOUNCE,
                });
                return;
            }

            self.schedule_cursor_focused_cold_probe(path, identity, tx);
        } else if entry.is_dir() && !matches!(entry.kind, EntryKind::ParentDir) {
            self.probe_debounce = None;
            let path = entry.path.clone();
            let Some(identity) = Self::current_filesystem_probe_identity(&entry) else {
                self.remove_dir_stats_cache_entry(&path);
                self.dir_stats_pending.remove(&path);
                return;
            };
            if self.valid_dir_stats_for_entry(&entry).is_some() || self.dir_stats_pending.contains(&path) {
                return;
            }
            self.schedule_cursor_focused_dir_stats(path, identity, tx);
        } else {
            self.probe_debounce = None;
        }
    }


    fn has_browse_cold_probe_queued_or_active(&self, path: &Path) -> bool {
        self.browse_cold_probe_active
            .iter()
            .any(|active| same_path(active, path))
            || self
                .browse_cold_probe_queue
                .iter()
                .any(|request| same_path(&request.path, path))
    }

    fn current_probeable_entry_path(&self) -> Option<PathBuf> {
        self.entries
            .get(self.selected_index)
            .filter(|entry| entry.is_probeable())
            .map(|entry| entry.path.clone())
    }

    fn discard_stale_queued_browse_cold_probes(&mut self) {
        let current_path = self.current_probeable_entry_path();
        let current_generation = self.scan_generation;
        self.browse_cold_probe_queue.retain(|request| {
            if request.scan_generation != current_generation {
                return false;
            }
            if !request.cursor_focused {
                return true;
            }
            current_path
                .as_deref()
                .map(|path| same_path(path, &request.path))
                .unwrap_or(false)
        });
    }

    fn start_browse_cold_probe_now(
        &mut self,
        path: PathBuf,
        identity: ProbeCacheIdentity,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        self.probe_pending.insert(path.clone());
        self.browse_cold_probe_active.insert(path.clone());
        spawn_audio_probe(path, identity, tx.clone());
    }

    fn schedule_cursor_focused_cold_probe(
        &mut self,
        path: PathBuf,
        identity: ProbeCacheIdentity,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        self.discard_stale_queued_browse_cold_probes();
        if self.probe_pending.contains(&path) || self.has_browse_cold_probe_queued_or_active(&path) {
            return;
        }

        if self.browse_cold_probe_active.len() < BROWSE_COLD_PROBE_MAX_IN_FLIGHT {
            self.start_browse_cold_probe_now(path, identity, tx);
            return;
        }

        // Cursor-focused work is high priority: remove any older copy and put
        // the current selection at the front. The queue is intentionally small;
        // if future background prefetch requests are added, they will be the
        // first to fall off the back under sustained scroll pressure.
        self.browse_cold_probe_queue
            .retain(|request| !same_path(&request.path, &path));
        self.browse_cold_probe_queue.push_front(BrowseColdProbeRequest {
            path,
            identity,
            scan_generation: self.scan_generation,
            cursor_focused: true,
        });
        while self.browse_cold_probe_queue.len() > BROWSE_COLD_PROBE_QUEUE_MAX {
            self.browse_cold_probe_queue.pop_back();
        }
    }

    /// Called when a Browse filesystem probe finishes, regardless of whether
    /// the result is accepted or discarded. It frees a cold-probe slot and
    /// starts the highest-priority still-current queued request, if any.
    pub fn complete_browse_cold_probe(
        &mut self,
        path: &Path,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        self.browse_cold_probe_active.retain(|active| !same_path(active, path));
        self.launch_ready_browse_cold_probes(tx);
    }

    fn launch_ready_browse_cold_probes(
        &mut self,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        self.discard_stale_queued_browse_cold_probes();
        while self.browse_cold_probe_active.len() < BROWSE_COLD_PROBE_MAX_IN_FLIGHT {
            let Some(request) = self.browse_cold_probe_queue.pop_front() else {
                break;
            };
            if request.scan_generation != self.scan_generation {
                continue;
            }
            if self.probe_pending.contains(&request.path)
                || self.browse_cold_probe_active.iter().any(|active| same_path(active, &request.path))
            {
                continue;
            }
            if request.cursor_focused && !self.is_current_entry_path(&request.path) {
                continue;
            }

            let Some(current_identity) = std::fs::metadata(&request.path)
                .ok()
                .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
            else {
                self.remove_probe_cache_entry(&request.path);
                continue;
            };
            if current_identity != request.identity {
                self.remove_probe_cache_entry(&request.path);
                if request.cursor_focused && self.is_current_entry_path(&request.path) {
                    self.schedule_cursor_focused_cold_probe(request.path, current_identity, tx);
                }
                continue;
            }

            self.start_browse_cold_probe_now(request.path, request.identity, tx);
        }
    }

    pub fn clear_browse_cold_probe_tracking_for(&mut self, path: &Path) {
        self.browse_cold_probe_queue
            .retain(|request| !same_path(&request.path, path));
        self.browse_cold_probe_active
            .retain(|active| !same_path(active, path));
        self.probe_pending.remove(path);
    }

    pub fn clear_browse_cold_probe_queue(&mut self) {
        self.browse_cold_probe_queue.clear();
    }


    fn current_directory_entry_path(&self) -> Option<PathBuf> {
        self.entries
            .get(self.selected_index)
            .filter(|entry| entry.is_dir() && !matches!(entry.kind, EntryKind::ParentDir))
            .map(|entry| entry.path.clone())
    }

    fn discard_stale_queued_dir_stats(&mut self) {
        let current_path = self.current_directory_entry_path();
        let current_generation = self.scan_generation;
        let mut kept = VecDeque::with_capacity(self.dir_stats_queue.len());

        while let Some(request) = self.dir_stats_queue.pop_front() {
            let generation_current = request.scan_generation == current_generation;
            let cursor_still_focused = !request.cursor_focused
                || current_path
                    .as_deref()
                    .map(|path| same_path(path, &request.path))
                    .unwrap_or(false);

            if generation_current && cursor_still_focused {
                kept.push_back(request);
            } else {
                self.dir_stats_pending.remove(&request.path);
            }
        }

        self.dir_stats_queue = kept;
    }

    fn cancel_stale_active_dir_stats(&mut self) {
        let current_path = self.current_directory_entry_path();
        let current_generation = self.scan_generation;
        let stale_paths: Vec<PathBuf> = self
            .dir_stats_active
            .iter()
            .filter_map(|(path, job)| {
                let generation_current = job.scan_generation == current_generation;
                let cursor_still_focused = !job.cursor_focused
                    || current_path
                        .as_deref()
                        .map(|current| same_path(current, path))
                        .unwrap_or(false);
                let identity_current = std::fs::metadata(path)
                    .ok()
                    .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
                    .map(|identity| identity == job.identity)
                    .unwrap_or(false);

                if generation_current && cursor_still_focused && identity_current {
                    None
                } else {
                    Some(path.clone())
                }
            })
            .collect();

        for path in stale_paths {
            if let Some(job) = self.dir_stats_active.remove(&path) {
                // Recursive stats walks can traverse very large subtrees.
                // Cancelling stale active jobs frees the single stats slot
                // quickly instead of waiting for a completion that will be
                // discarded by identity checks anyway.
                job.cancel.store(true, Ordering::Relaxed);
            }
            self.dir_stats_pending.remove(&path);
        }
    }

    fn start_dir_stats_now(
        &mut self,
        path: PathBuf,
        identity: ProbeCacheIdentity,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.dir_stats_pending.insert(path.clone());
        self.dir_stats_active.insert(
            path.clone(),
            DirStatsActiveJob {
                identity,
                scan_generation: self.scan_generation,
                cursor_focused: true,
                cancel: cancel.clone(),
            },
        );
        spawn_dir_stats(path, identity, cancel, tx.clone());
    }

    fn schedule_cursor_focused_dir_stats(
        &mut self,
        path: PathBuf,
        identity: ProbeCacheIdentity,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        self.discard_stale_queued_dir_stats();
        self.cancel_stale_active_dir_stats();
        if self.dir_stats_pending.contains(&path) {
            return;
        }
        if self.dir_stats_active.len() < BROWSE_DIR_STATS_MAX_IN_FLIGHT {
            self.start_dir_stats_now(path, identity, tx);
            return;
        }

        self.dir_stats_pending.insert(path.clone());
        self.dir_stats_queue
            .retain(|request| !same_path(&request.path, &path));
        self.dir_stats_queue.push_front(DirStatsRequest {
            path,
            identity,
            scan_generation: self.scan_generation,
            cursor_focused: true,
        });
        while self.dir_stats_queue.len() > BROWSE_DIR_STATS_QUEUE_MAX {
            if let Some(dropped) = self.dir_stats_queue.pop_back() {
                self.dir_stats_pending.remove(&dropped.path);
            }
        }
    }

    pub fn complete_dir_stats(
        &mut self,
        path: &Path,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) -> bool {
        let was_pending = self.dir_stats_pending.remove(path);
        if let Some(job) = self.dir_stats_active.remove(path) {
            job.cancel.store(true, Ordering::Relaxed);
        }
        self.launch_ready_dir_stats(tx);
        was_pending
    }

    fn launch_ready_dir_stats(
        &mut self,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        self.discard_stale_queued_dir_stats();
        self.cancel_stale_active_dir_stats();
        while self.dir_stats_active.len() < BROWSE_DIR_STATS_MAX_IN_FLIGHT {
            let Some(request) = self.dir_stats_queue.pop_front() else {
                break;
            };
            if request.scan_generation != self.scan_generation {
                self.dir_stats_pending.remove(&request.path);
                continue;
            }
            if request.cursor_focused && !self.is_current_entry_path(&request.path) {
                self.dir_stats_pending.remove(&request.path);
                continue;
            }
            if self.has_valid_dir_stats_for_identity(&request.path, request.identity)
                || self.dir_stats_active.keys().any(|active| same_path(active, &request.path))
            {
                self.dir_stats_pending.remove(&request.path);
                continue;
            }
            let Some(current_identity) = std::fs::metadata(&request.path)
                .ok()
                .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
            else {
                self.dir_stats_pending.remove(&request.path);
                self.remove_dir_stats_cache_entry(&request.path);
                continue;
            };
            if current_identity != request.identity {
                self.dir_stats_pending.remove(&request.path);
                self.remove_dir_stats_cache_entry(&request.path);
                if request.cursor_focused && self.is_current_entry_path(&request.path) {
                    self.schedule_cursor_focused_dir_stats(request.path, current_identity, tx);
                }
                continue;
            }
            self.start_dir_stats_now(request.path, request.identity, tx);
        }
    }

    pub fn clear_dir_stats_work_queue(&mut self) {
        self.dir_stats_queue.clear();
        for (_, job) in self.dir_stats_active.drain() {
            job.cancel.store(true, Ordering::Relaxed);
        }
        self.dir_stats_pending.clear();
    }

    fn spawn_cached_probe_metadata_completion_if_needed(
        &mut self,
        path: &Path,
        identity: ProbeCacheIdentity,
        info: Arc<CachedInfo>,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        if !self.probe_cache_needs_metadata_enrichment.remove(path) {
            return;
        }
        if self.probe_pending.contains(path) {
            return;
        }
        self.probe_pending.insert(path.to_path_buf());
        spawn_cached_audio_probe_metadata_completion(
            path.to_path_buf(),
            identity,
            (*info).clone(),
            tx.clone(),
        );
    }

    /// Fire a delayed cold Browse probe once the cursor has rested on the same
    /// uncached file. This is called by the event loop before rendering.
    pub fn check_probe_debounce(
        &mut self,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        let Some(pending) = self.probe_debounce.clone() else {
            return;
        };
        if Instant::now() < pending.deadline {
            return;
        }
        self.probe_debounce = None;

        if self.archive.is_some() {
            return;
        }
        let Some(entry) = self.entries.get(self.selected_index).cloned() else {
            return;
        };
        if entry.path != pending.path || !entry.is_probeable() {
            return;
        }
        let Some(identity) = Self::current_filesystem_probe_identity(&entry) else {
            self.remove_probe_cache_entry(&pending.path);
            self.clear_browse_cold_probe_tracking_for(&pending.path);
            return;
        };
        if self.has_valid_probe_cache_entry(&entry, identity)
            || self.probe_pending.contains(&pending.path)
            || self.has_browse_cold_probe_queued_or_active(&pending.path)
        {
            return;
        }

        self.schedule_cursor_focused_cold_probe(pending.path, identity, tx);
    }

    /// Spawn bounded SQLite probe-cache warm-up for the current scan. The DB
    /// work runs entirely off the reducer path and completions carry the scan
    /// generation/path so stale workers cannot merge after navigation.
    pub fn spawn_probe_cache_warm_from_db(
        &self,
        generation: u64,
        path: PathBuf,
        tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        let mut candidates = Vec::new();
        for entry in self.all_files.iter().filter(|entry| entry.is_probeable()) {
            let identity = ProbeCacheIdentity::from_entry(entry);
            if self.has_probe_cache_entry_for_identity(&entry.path, identity) {
                continue;
            }
            let Some(mtime) = identity.modified.map(crate::db::systemtime_to_unix) else {
                continue;
            };
            candidates.push((
                entry.path.display().to_string(),
                mtime,
                identity.size,
                entry.path.clone(),
                identity,
            ));
            if candidates.len() >= PROBE_CACHE_WARM_MAX_CANDIDATES {
                break;
            }
        }

        if candidates.is_empty() {
            return;
        }

        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || -> Result<Vec<ProbeCacheWarmRow>, String> {
                let db = crate::db::Database::open()?;
                let mut path_meta = HashMap::with_capacity(candidates.len());
                let requests: Vec<(String, i64, u64)> = candidates
                    .into_iter()
                    .map(|(key, mtime, size, path, identity)| {
                        path_meta.insert(key.clone(), (path, identity));
                        (key, mtime, size)
                    })
                    .collect();

                let mut warmed = Vec::new();
                for (key, row) in db.get_cached_probes_for_files(&requests) {
                    let Some((path, identity)) = path_meta.get(&key).cloned() else {
                        continue;
                    };
                    let current_identity = std::fs::metadata(&path)
                        .ok()
                        .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata));
                    if current_identity != Some(identity) {
                        continue;
                    }
                    if let Some(info) = row.to_cached_info(identity.size) {
                        warmed.push(ProbeCacheWarmRow { path, identity, info });
                    }
                }
                Ok(warmed)
            })
            .await
            .unwrap_or_else(|join_err| Err(format!("probe-cache warm task panicked: {}", join_err)));

            let Ok(rows) = result else { return; };
            for chunk in rows.chunks(PROBE_CACHE_WARM_MESSAGE_CHUNK) {
                let _ = tx
                    .send(crate::tui::message::AppMessage::ProbeCacheWarmComplete {
                        generation,
                        path: path.clone(),
                        rows: chunk.to_vec(),
                    })
                    .await;
            }
        });
    }

    pub fn is_current_directory_generation(&self, generation: u64, path: &Path) -> bool {
        self.scan_generation == generation && same_path(&self.current_dir, path)
    }

    pub fn probe_identity_for_current_entry_path(&self, path: &Path) -> Option<ProbeCacheIdentity> {
        self.entries
            .iter()
            .chain(self.all_files.iter())
            .chain(self.all_dirs.iter())
            .find(|entry| same_path(&entry.path, path))
            .map(ProbeCacheIdentity::from_entry)
    }

    fn merge_probe_cache_warm_row(&mut self, row: ProbeCacheWarmRow) -> bool {
        let Some(current_identity) = self.probe_identity_for_current_entry_path(&row.path) else {
            return false;
        };
        if current_identity != row.identity {
            return false;
        }
        if let Some(cached) = self.probe_cache.get(&row.path) {
            if cached.is_valid_for(row.identity) || cached.is_valid_for(current_identity) {
                return false;
            }
        }
        self.insert_probe_for_identity(
            row.path.clone(),
            row.identity,
            Some(Arc::new(row.info)),
        );
        self.probe_cache_needs_metadata_enrichment.insert(row.path);
        true
    }

    pub fn merge_probe_cache_warm_rows(&mut self, rows: Vec<ProbeCacheWarmRow>) -> usize {
        let mut merged = 0usize;
        for row in rows {
            if self.merge_probe_cache_warm_row(row) {
                merged = merged.saturating_add(1);
            }
        }
        merged
    }

    pub fn enqueue_probe_cache_warm_rows(
        &mut self,
        generation: u64,
        path: PathBuf,
        rows: Vec<ProbeCacheWarmRow>,
    ) -> usize {
        if rows.is_empty() || !self.is_current_directory_generation(generation, &path) {
            return 0;
        }
        let count = rows.len();
        self.probe_cache_warm_pending.push_back(ProbeCacheWarmBatch {
            generation,
            path,
            rows: rows.into_iter().collect(),
        });
        count
    }

    pub fn drain_probe_cache_warm_rows_for_frame(&mut self) -> (usize, bool) {
        let mut inspected = 0usize;
        let mut merged = 0usize;

        while inspected < PROBE_CACHE_WARM_MERGE_MAX_PER_FRAME {
            let Some(mut batch) = self.probe_cache_warm_pending.pop_front() else {
                break;
            };

            if !self.is_current_directory_generation(batch.generation, &batch.path) {
                continue;
            }

            while inspected < PROBE_CACHE_WARM_MERGE_MAX_PER_FRAME {
                let Some(row) = batch.rows.pop_front() else {
                    break;
                };
                inspected = inspected.saturating_add(1);
                if self.merge_probe_cache_warm_row(row) {
                    merged = merged.saturating_add(1);
                }
            }

            if !batch.rows.is_empty() {
                self.probe_cache_warm_pending.push_front(batch);
                break;
            }
        }

        if merged > 0 {
            self.mark_probe_cache_update_pending(true);
        }

        (merged, !self.probe_cache_warm_pending.is_empty())
    }

    pub fn has_probe_cache_warm_backlog(&self) -> bool {
        !self.probe_cache_warm_pending.is_empty()
    }

    pub fn clear_probe_cache_warm_backlog(&mut self) {
        self.probe_cache_warm_pending.clear();
    }

    pub fn mark_probe_cache_update_pending(&mut self, refresh_current: bool) {
        self.deferred_work.probe_backed_resort_needed = true;
        if self.active_search_depends_on_tag_or_probe_metadata() {
            self.deferred_work.search_reapply_needed = true;
        }
        if !self.search.active && self.sort_by.uses_probe_cache() {
            self.deferred_work.visible_entries_changed = true;
        }
        if refresh_current {
            self.deferred_work.info_pane_changed = true;
        }
    }

    pub fn mark_visible_entries_changed_pending(&mut self) {
        self.deferred_work.visible_entries_changed = true;
    }

    pub fn mark_classification_changed_pending(&mut self) {
        self.deferred_work.classification_changed = true;
        self.deferred_work.visible_entries_changed = true;
    }

    pub fn take_browse_deferred_work(&mut self) -> BrowseDeferredWorkFlags {
        std::mem::take(&mut self.deferred_work)
    }

    pub fn take_probe_cache_deferred_work(&mut self) -> (bool, bool) {
        let work = self.take_browse_deferred_work();
        (
            work.probe_backed_resort_needed || work.search_reapply_needed || work.visible_entries_changed,
            work.info_pane_changed,
        )
    }

    /// Current archive-entry probe epoch for `archive_path`. Missing entries
    /// are epoch 0 so old bundles and newly opened archives start naturally.
    pub fn archive_probe_epoch_for(&self, archive_path: &Path) -> u64 {
        self.archive_probe_epochs
            .get(archive_path)
            .copied()
            .unwrap_or(0)
    }

    /// Build the acceptance context attached to a worker launched for a
    /// synthetic archive entry. The completion handler compares this captured
    /// epoch with the current one before it can write `probe_cache`.
    pub fn archive_entry_probe_context_for(
        &self,
        archive_path: &Path,
    ) -> crate::tui::message::AudioProbeContext {
        crate::tui::message::AudioProbeContext::ArchiveEntry {
            archive_path: archive_path.to_path_buf(),
            archive_probe_epoch: self.archive_probe_epoch_for(archive_path),
        }
    }

    /// Bump the mutation epoch for one archive and return the new value.
    pub fn bump_archive_probe_epoch_for(&mut self, archive_path: &Path) -> u64 {
        let epoch = self
            .archive_probe_epochs
            .entry(archive_path.to_path_buf())
            .or_insert(0);
        *epoch = epoch.saturating_add(1);
        *epoch
    }

    /// Remove all in-memory probe state associated with an archive and its
    /// synthetic archive-entry paths (`archive.zip/inner/file.flac`). This is
    /// intentionally prefix-based because synthetic entries are not real
    /// filesystem children but are represented as joined paths in Browse. The
    /// archive epoch is also bumped so any worker that was already extracting
    /// or probing an old archive member is rejected when it eventually reports.
    pub fn invalidate_archive_probe_cache_for(&mut self, archive_path: &Path) {
        self.bump_archive_probe_epoch_for(archive_path);
        self.probe_cache.retain(|path, _| !archive_probe_path_matches(path, archive_path));
        self.probe_pending
            .retain(|path| !archive_probe_path_matches(path, archive_path));
    }

    /// Return whether an archive-entry probe completion is still current.
    /// Requiring both a live pending marker and an unchanged archive epoch
    /// prevents pre-repackage workers from repopulating stale synthetic-path
    /// metadata after the archive has been rewritten.
    pub fn accept_archive_entry_probe_completion(
        &self,
        path: &Path,
        archive_path: &Path,
        archive_probe_epoch: u64,
        was_pending: bool,
    ) -> bool {
        was_pending
            && archive_probe_path_matches(path, archive_path)
            && self.archive_probe_epoch_for(archive_path) == archive_probe_epoch
    }

    /// Get cached info for the currently selected audio file, if probed
    pub fn current_cached_info(&self) -> Option<&Arc<CachedInfo>> {
        let entry = self.entries.get(self.selected_index)?;
        if !entry.is_probeable() {
            return None;
        }
        self.valid_probe_arc_for_entry(entry)
    }

    /// Get cached directory stats for the current selection (if it's a directory)
    pub fn current_dir_stats(&self) -> Option<&Arc<DirStats>> {
        let entry = self.entries.get(self.selected_index)?;
        self.valid_dir_stats_for_entry(entry)
    }

    pub fn valid_dir_stats_for_entry(&self, entry: &BrowseEntry) -> Option<&Arc<DirStats>> {
        if !matches!(entry.kind, EntryKind::Directory) {
            return None;
        }
        let identity = ProbeCacheIdentity::from_entry(entry);
        self.dir_stats_cache
            .get(&entry.path)
            .filter(|cached| cached.is_valid_for(identity))
            .map(|cached| &cached.stats)
    }

    pub fn has_valid_dir_stats_for_identity(&self, path: &Path, identity: ProbeCacheIdentity) -> bool {
        self.dir_stats_cache
            .get(path)
            .is_some_and(|cached| cached.is_valid_for(identity))
    }

    pub fn insert_dir_stats_for_identity(
        &mut self,
        path: PathBuf,
        identity: ProbeCacheIdentity,
        stats: DirStats,
    ) {
        self.dir_stats_cache
            .insert(path, DirStatsCacheEntry::new(identity, stats));
    }

    pub fn remove_dir_stats_cache_entry(&mut self, path: &Path) {
        self.dir_stats_cache.remove(path);
    }
}

/// Spawn a background tokio task for a valid SQLite probe-cache hit that
/// still needs worker-side metadata enrichment. This preserves the cache fast
/// path without letting tag/CUE/catalog PE checks run from cursor movement or
/// message reducers on the TUI thread.
fn spawn_cached_audio_probe_metadata_completion(
    path: PathBuf,
    identity: ProbeCacheIdentity,
    mut info: CachedInfo,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) {
    tokio::spawn(async move {
        let path_for_task = path.clone();
        let result = tokio::task::spawn_blocking(move || {
            if info.metadata.preemphasis_metadata.is_none() {
                info.metadata.preemphasis_metadata =
                    crate::tui::probe::preemphasis_metadata_check_blocking(&path_for_task);
            }
            Ok(info)
        })
        .await
        .unwrap_or_else(|join_err| Err(format!("cached probe metadata task panicked: {}", join_err)));

        let _ = tx
            .send(crate::tui::message::AppMessage::AudioProbeComplete {
                path,
                context: crate::tui::message::AudioProbeContext::Filesystem {
                    identity: Some(identity),
                },
                result: Box::new(result),
            })
            .await;
    });
}


/// Spawn a worker that probes an already-extracted staged archive member and
/// reports the result under the synthetic archive browse path. This is the
/// deferred-save path: once staging exists, the staged file is authoritative
/// for info-pane metadata, renamed paths, and deleted/missing entries.
fn spawn_staged_archive_audio_probe(
    staged_path: PathBuf,
    synthetic_path: PathBuf,
    probe_context: crate::tui::message::AudioProbeContext,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) {
    tokio::spawn(async move {
        let path_for_task = staged_path.clone();
        let result: Result<CachedInfo, String> = tokio::task::spawn_blocking(move || {
            let source = crate::tui::probe::probe_audio(&path_for_task)
                .map_err(|err| format!("{}", err))?;
            let metadata = crate::tui::probe::read_metadata(&path_for_task).unwrap_or_else(|_| {
                let mut metadata = SourceMetadata::default();
                metadata.preemphasis_metadata =
                    crate::tui::probe::preemphasis_metadata_check_blocking(&path_for_task);
                metadata
            });
            Ok(CachedInfo { source, metadata })
        })
        .await
        .unwrap_or_else(|join_err| Err(format!("staged archive probe task panicked: {}", join_err)));

        let _ = tx
            .send(crate::tui::message::AppMessage::AudioProbeComplete {
                path: synthetic_path,
                context: probe_context,
                result: Box::new(result),
            })
            .await;
    });
}

/// Spawn a worker that extracts one archive member to a private temp tree,
/// probes that real file, then reports the result under the synthetic browse
/// path. The temp tree is removed before the message is sent, so cache entries
/// never point at short-lived staging files.
fn spawn_archive_entry_audio_probe(
    archive_path: PathBuf,
    inner_path: String,
    synthetic_path: PathBuf,
    password: Option<String>,
    probe_context: crate::tui::message::AudioProbeContext,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) {
    tokio::spawn(async move {
        let result = async {
            let staging_dir = std::env::temp_dir().join(format!(
                "tonepoet-archive-entry-probe-{}",
                uuid::Uuid::new_v4()
            ));
            let extracted = extract_archive_entry_to_temp(
                &archive_path,
                &inner_path,
                password.as_deref(),
                &staging_dir,
            )
            .await;

            let result: Result<CachedInfo, String> = match extracted {
                Ok(path) => {
                    let path_for_task = path.clone();
                    tokio::task::spawn_blocking(move || {
                        let source = crate::tui::probe::probe_audio(&path_for_task)
                            .map_err(|err| format!("{}", err))?;
                        let metadata = crate::tui::probe::read_metadata(&path_for_task)
                            .unwrap_or_else(|_| {
                                let mut metadata = SourceMetadata::default();
                                metadata.preemphasis_metadata =
                                    crate::tui::probe::preemphasis_metadata_check_blocking(
                                        &path_for_task,
                                    );
                                metadata
                            });
                        Ok(CachedInfo { source, metadata })
                    })
                    .await
                    .unwrap_or_else(|join_err| {
                        Err(format!("archive entry probe task panicked: {}", join_err))
                    })
                }
                Err(err) => Err(err),
            };

            let _ = std::fs::remove_dir_all(&staging_dir);
            result
        }
        .await;

        let _ = tx
            .send(crate::tui::message::AppMessage::AudioProbeComplete {
                path: synthetic_path,
                context: probe_context,
                result: Box::new(result),
            })
            .await;
    });
}

async fn extract_archive_entry_to_temp(
    archive_path: &Path,
    inner_path: &str,
    password: Option<&str>,
    staging_dir: &Path,
) -> Result<PathBuf, String> {
    let extracted = staging_path_for_archive_inner(staging_dir, inner_path)?;
    let extraction_mode = validate_archive_entry_probe_selector(inner_path)?;
    let bin = crate::detect_7z_binary()
        .ok_or_else(|| "neither 7zz nor 7z found in PATH".to_string())?;
    std::fs::create_dir_all(staging_dir)
        .map_err(|err| format!("create archive-entry probe staging failed: {err}"))?;

    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg("x")
        .arg(archive_path)
        .arg(format!("-o{}", staging_dir.display()))
        .arg("-y")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    if let ArchiveEntryProbeExtraction::SingleMember = extraction_mode {
        cmd.arg(inner_path);
    }
    if let Some(password) = password {
        cmd.arg(format!("-p{}", password));
    }

    let output = cmd
        .output()
        .await
        .map_err(|err| format!("failed to run {}: {}", bin, err))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let operation = match extraction_mode {
            ArchiveEntryProbeExtraction::SingleMember => "archive entry extraction",
            ArchiveEntryProbeExtraction::FullArchiveFallback => "archive extraction fallback",
        };
        return Err(format!("{} failed: {}", operation, stderr.trim()));
    }

    if extracted.is_file() {
        Ok(extracted)
    } else {
        Err(format!(
            "archive entry extraction did not produce a file: {}",
            inner_path
        ))
    }
}

/// Spawn a background tokio task that probes the audio file at `path` and
/// sends the result back to the main loop via `AudioProbeComplete`. The
/// blocking probe (`probe_audio` + `read_metadata`) runs on `spawn_blocking`
/// so it doesn't tie up an async worker thread. `read_metadata()` already
/// performs external pre-emphasis metadata enrichment; only failed metadata
/// reads get a single fallback PE check so successful fresh probes do not
/// duplicate tag/CUE/catalog work.
pub fn spawn_audio_probe(
    path: PathBuf,
    identity: ProbeCacheIdentity,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) {
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
            let metadata = crate::tui::probe::read_metadata(&path_for_task).unwrap_or_else(|_| {
                let mut metadata = SourceMetadata::default();
                metadata.preemphasis_metadata =
                    crate::tui::probe::preemphasis_metadata_check_blocking(&path_for_task);
                metadata
            });
            Ok(CachedInfo { source, metadata })
        })
        .await
        .unwrap_or_else(|join_err| Err(format!("probe task panicked: {}", join_err)));

        let _ = tx
            .send(crate::tui::message::AppMessage::AudioProbeComplete {
                path,
                context: crate::tui::message::AudioProbeContext::Filesystem {
                    identity: Some(identity),
                },
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
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
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
            .send(crate::tui::message::AppMessage::AudioProbeComplete {
                path,
                context: crate::tui::message::AudioProbeContext::Filesystem { identity: None },
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
    identity: ProbeCacheIdentity,
    cancel: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) {
    tokio::spawn(async move {
        let path_for_task = path.clone();
        let cancel_for_task = cancel.clone();
        let stats = tokio::task::spawn_blocking(move || {
            compute_dir_stats(&path_for_task, &cancel_for_task)
        })
        .await
        .unwrap_or(None);

        let (stats, cancelled) = match stats {
            Some(stats) => (stats, false),
            None => (DirStats::default(), true),
        };

        let _ = tx
            .send(crate::tui::message::AppMessage::DirStatsComplete {
                path,
                identity,
                stats,
                cancelled,
            })
            .await;
    });
}

/// Spawn a background directory scan. The blocking I/O (readdir + lstat per
/// entry) runs on `spawn_blocking`. Respects the cancel flag — checks every
/// 50 entries and aborts early if set. Sends `DirScanComplete` when done.
/// Wrapped in a 30-second timeout.
fn spawn_dir_scan(
    path: PathBuf,
    generation: u64,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    classification_cache: BrowseClassificationCacheSnapshot,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) {
    tokio::spawn(async move {
        let scan_path = path.clone();
        let cancel_flag = cancel.clone();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::task::spawn_blocking(move || {
                scan_directory_blocking(&scan_path, &cancel_flag, &classification_cache)
            }),
        )
        .await;

        let (parent_entry, dirs, files, classification_updates, error) = match result {
            Ok(Ok(Ok((parent, dirs, files, updates)))) => (parent, dirs, files, updates, None),
            Ok(Ok(Err(e))) => (None, Vec::new(), Vec::new(), BrowseClassificationCacheUpdates::default(), Some(e)),
            Ok(Err(join_err)) => (
                None,
                Vec::new(),
                Vec::new(),
                BrowseClassificationCacheUpdates::default(),
                Some(format!("scan task panicked: {}", join_err)),
            ),
            Err(_timeout) => {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                (
                    None,
                    Vec::new(),
                    Vec::new(),
                    BrowseClassificationCacheUpdates::default(),
                    Some("scan timed out (30s)".into()),
                )
            }
        };

        let _ = tx
            .send(crate::tui::message::AppMessage::DirScanComplete {
                generation,
                path,
                parent_entry,
                dirs,
                files,
                classification_updates,
                error,
            })
            .await;
    });
}


fn classify_scanned_entry_blocking(
    entry: &mut BrowseEntry,
    cache: &BrowseClassificationCacheSnapshot,
    updates: &mut BrowseClassificationCacheUpdates,
) {
    if matches!(&entry.kind, EntryKind::Directory) {
        classify_scanned_directory_entry_blocking(entry, cache, updates);
    } else if matches!(&entry.kind, EntryKind::Archive) {
        classify_scanned_iso_entry_blocking(entry, cache, updates);
    }
}

fn classify_scanned_iso_entry_blocking(
    entry: &mut BrowseEntry,
    cache: &BrowseClassificationCacheSnapshot,
    updates: &mut BrowseClassificationCacheUpdates,
) {
    let is_iso = entry
        .path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("iso"))
        .unwrap_or(false);
    if !is_iso {
        return;
    }

    let sacd_fingerprint = ClassificationFingerprint::from_entry(entry);
    let is_sacd = cache
        .sacd_iso
        .get(&entry.path)
        .filter(|(cached, _)| *cached == sacd_fingerprint)
        .map(|(_, verdict)| *verdict)
        .unwrap_or_else(|| {
            let verdict = crate::tui::sacd::is_sacd_iso(&entry.path);
            if verdict || should_cache_file_classification_negative(&entry.path) {
                updates
                    .sacd_iso
                    .push((entry.path.clone(), sacd_fingerprint.clone(), verdict));
            }
            verdict
        });
    if is_sacd {
        entry.kind = EntryKind::SacdIso;
        return;
    }

    let fingerprint = ClassificationFingerprint::from_entry(entry);
    let is_dvda = cache
        .dvda_iso
        .get(&entry.path)
        .filter(|(cached, _)| *cached == fingerprint)
        .map(|(_, verdict)| *verdict)
        .unwrap_or_else(|| {
            let verdict = crate::disc::dvda_utils::is_dvda_iso(&entry.path);
            if verdict || should_cache_file_classification_negative(&entry.path) {
                updates.dvda_iso.push((entry.path.clone(), fingerprint.clone(), verdict));
            }
            verdict
        });
    if is_dvda {
        entry.kind = EntryKind::DvdAudioIso;
        return;
    }

    let is_dvdv = cache
        .dvdv_iso
        .get(&entry.path)
        .filter(|(cached, _)| *cached == fingerprint)
        .map(|(_, verdict)| *verdict)
        .unwrap_or_else(|| {
            let verdict = crate::disc::dvdv_utils::is_dvdv_iso(&entry.path);
            if verdict || should_cache_file_classification_negative(&entry.path) {
                updates.dvdv_iso.push((entry.path.clone(), fingerprint.clone(), verdict));
            }
            verdict
        });
    if is_dvdv {
        entry.kind = EntryKind::DvdVideoIso;
        return;
    }

    let is_bluray = cache
        .bluray_iso
        .get(&entry.path)
        .filter(|(cached, _)| *cached == fingerprint)
        .map(|(_, verdict)| *verdict)
        .unwrap_or_else(|| {
            let verdict = crate::disc::bluray_utils::is_bluray_iso(&entry.path);
            if verdict || should_cache_file_classification_negative(&entry.path) {
                updates.bluray_iso.push((entry.path.clone(), fingerprint.clone(), verdict));
            }
            verdict
        });
    if is_bluray {
        entry.kind = EntryKind::BlurayIso;
    }
}

fn classify_scanned_directory_entry_blocking(
    entry: &mut BrowseEntry,
    cache: &BrowseClassificationCacheSnapshot,
    updates: &mut BrowseClassificationCacheUpdates,
) {
    let dvda_fingerprint = dvda_directory_classification_fingerprint(entry);
    let is_dvda = cache
        .dvda_dir
        .get(&entry.path)
        .filter(|(cached, _)| *cached == dvda_fingerprint)
        .map(|(_, verdict)| *verdict)
        .unwrap_or_else(|| {
            let verdict = crate::disc::dvda_utils::is_dvda_directory(&entry.path);
            if verdict || should_cache_directory_classification_negative(&entry.path) {
                updates.dvda_dir.push((entry.path.clone(), dvda_fingerprint.clone(), verdict));
            }
            verdict
        });
    if is_dvda {
        entry.kind = EntryKind::DvdAudioDir;
        return;
    }

    let dvdv_fingerprint = dvdv_directory_classification_fingerprint(entry);
    let is_dvdv = cache
        .dvdv_dir
        .get(&entry.path)
        .filter(|(cached, _)| *cached == dvdv_fingerprint)
        .map(|(_, verdict)| *verdict)
        .unwrap_or_else(|| {
            let verdict = crate::disc::dvdv_utils::is_dvdv_directory(&entry.path);
            if verdict || should_cache_directory_classification_negative(&entry.path) {
                updates.dvdv_dir.push((entry.path.clone(), dvdv_fingerprint.clone(), verdict));
            }
            verdict
        });
    if is_dvdv {
        entry.kind = EntryKind::DvdVideoDir;
        return;
    }

    let bluray_fingerprint = bluray_directory_classification_fingerprint(entry);
    let is_bluray = cache
        .bluray_dir
        .get(&entry.path)
        .filter(|(cached, _)| *cached == bluray_fingerprint)
        .map(|(_, verdict)| *verdict)
        .unwrap_or_else(|| {
            let verdict = crate::disc::bluray_utils::is_bluray_directory(&entry.path);
            if verdict || should_cache_directory_classification_negative(&entry.path) {
                updates.bluray_dir.push((entry.path.clone(), bluray_fingerprint.clone(), verdict));
            }
            verdict
        });
    if is_bluray {
        entry.kind = EntryKind::BlurayDir;
    }
}

/// Blocking directory scan — runs on a `spawn_blocking` thread.
/// Returns (parent_entry, dirs, files) or an error string.
fn scan_directory_blocking(
    dir: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    classification_cache: &BrowseClassificationCacheSnapshot,
) -> Result<(
    Option<BrowseEntry>,
    Vec<BrowseEntry>,
    Vec<BrowseEntry>,
    BrowseClassificationCacheUpdates,
), String> {
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
    let mut classification_updates = BrowseClassificationCacheUpdates::default();

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

        let mut browse_entry = BrowseEntry::new_with_symlink(
            path,
            name,
            kind.clone(),
            size,
            modified,
            is_symlink,
            is_broken_symlink,
        );

        classify_scanned_entry_blocking(
            &mut browse_entry,
            classification_cache,
            &mut classification_updates,
        );

        if matches!(
            kind,
            EntryKind::Directory
                | EntryKind::DvdAudioDir
                | EntryKind::DvdVideoDir
                | EntryKind::BlurayDir
        ) {
            dirs.push(browse_entry);
        } else {
            files.push(browse_entry);
        }
    }

    Ok((parent_entry, dirs, files, classification_updates))
}

fn browse_entry_name_is_hidden(name: &str) -> bool {
    name.split('/')
        .filter(|segment| !segment.is_empty())
        .any(|segment| segment.starts_with('.') && segment != "." && segment != "..")
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
    if !show_hidden && browse_entry_name_is_hidden(&entry.name) {
        return false;
    }
    // Format filter (only applies to non-directory entries). Use the
    // path-aware check so `.cue` stays visible as a convertible source under
    // AudioOnly without widening the filter to all `OtherFile` entries.
    if !matches!(
        &entry.kind,
        EntryKind::Directory
            | EntryKind::DvdAudioDir
            | EntryKind::DvdVideoDir
            | EntryKind::BlurayDir
    ) && !format_filter.allows_entry(entry) {
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

fn entry_passes_search_effective_filters(
    entry: &BrowseEntry,
    show_hidden: bool,
    audio_only: bool,
    format_filter: &FormatFilter,
) -> bool {
    if !show_hidden && browse_entry_name_is_hidden(&entry.name) {
        return false;
    }
    if audio_only && !is_audio_filter_visible_entry(entry) {
        return false;
    }
    if !matches!(
        &entry.kind,
        EntryKind::Directory
            | EntryKind::DvdAudioDir
            | EntryKind::DvdVideoDir
            | EntryKind::BlurayDir
    ) && !format_filter.allows_entry(entry) {
        return false;
    }
    true
}


fn normalize_archive_relative_path(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

fn archive_probe_path_matches(path: &Path, archive_path: &Path) -> bool {
    if path == archive_path {
        return true;
    }
    path.strip_prefix(archive_path)
        .ok()
        .is_some_and(|relative| !relative.as_os_str().is_empty())
}

fn staging_path_for_archive_inner(staging_dir: &Path, inner_path: &str) -> Result<PathBuf, String> {
    let mut out = staging_dir.to_path_buf();
    for component in Path::new(inner_path).components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            std::path::Component::CurDir => {}
            _ => {
                return Err(format!(
                    "archive entry path is unsafe: {}",
                    inner_path
                ));
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveEntryProbeExtraction {
    SingleMember,
    FullArchiveFallback,
}

fn validate_archive_entry_probe_selector(
    inner_path: &str,
) -> Result<ArchiveEntryProbeExtraction, String> {
    if inner_path.is_empty() {
        return Err("archive entry path is empty".to_string());
    }

    let mut wildcard_syntax = false;
    for component in Path::new(inner_path).components() {
        let std::path::Component::Normal(part) = component else {
            return Err(format!("archive entry path is unsafe: {}", inner_path));
        };
        let text = part.to_string_lossy();
        if text.starts_with('-') || text.starts_with('@') {
            return Err(format!(
                "archive entry path is unsafe for single-entry extraction: {}",
                inner_path
            ));
        }
        wildcard_syntax |= text.contains('*')
            || text.contains('?')
            || text.contains('[')
            || text.contains(']');
    }

    if wildcard_syntax {
        Ok(ArchiveEntryProbeExtraction::FullArchiveFallback)
    } else {
        Ok(ArchiveEntryProbeExtraction::SingleMember)
    }
}

/// Sort a vec of entries by the given field and direction.
///
/// Probe-backed columns use `probe_cache` when available. Unknown values remain
/// deterministic: they sort after known values in ascending order and then fall
/// back to the entry name, so enabling audio columns never makes list order
/// depend on filesystem walk order or hash-map iteration.
fn sort_entries(
    entries: &mut [BrowseEntry],
    by: SortBy,
    dir: SortDir,
    probe_cache: &HashMap<PathBuf, ProbeCacheEntry>,
) {
    entries.sort_by(|a, b| {
        let a_info = cached_info_for_sort(probe_cache, a);
        let b_info = cached_info_for_sort(probe_cache, b);
        let name_ord = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
        let ord = match by {
            SortBy::Name => name_ord(),
            SortBy::Date => compare_opt_ord(a.modified, b.modified).then_with(name_ord),
            SortBy::Size => a.size.cmp(&b.size).then_with(name_ord),
            SortBy::Type => {
                let a_rank = entry_type_rank(&a.kind);
                let b_rank = entry_type_rank(&b.kind);
                a_rank.cmp(&b_rank).then_with(name_ord)
            },
            SortBy::Format => compare_opt_string(
                audio_format_sort_value(a, a_info),
                audio_format_sort_value(b, b_info),
            )
            .then_with(name_ord),
            SortBy::Codec => compare_opt_string(
                a_info.map(|info| info.source.codec_display()),
                b_info.map(|info| info.source.codec_display()),
            )
            .then_with(name_ord),
            SortBy::SampleRate => compare_opt_ord(
                a_info.and_then(|info| positive_u32(info.source.sample_rate)),
                b_info.and_then(|info| positive_u32(info.source.sample_rate)),
            )
            .then_with(name_ord),
            SortBy::Channels => compare_opt_ord(
                a_info.and_then(|info| positive_u32(info.source.channels)),
                b_info.and_then(|info| positive_u32(info.source.channels)),
            )
            .then_with(name_ord),
            SortBy::Duration => compare_opt_f64(
                a_info.and_then(|info| positive_f64(info.source.duration_secs)),
                b_info.and_then(|info| positive_f64(info.source.duration_secs)),
            )
            .then_with(name_ord),
            SortBy::Artist => compare_opt_string(
                a_info.and_then(|info| info.metadata.artist.clone()),
                b_info.and_then(|info| info.metadata.artist.clone()),
            )
            .then_with(name_ord),
            SortBy::Album => compare_opt_string(
                a_info.and_then(|info| info.metadata.album.clone()),
                b_info.and_then(|info| info.metadata.album.clone()),
            )
            .then_with(name_ord),
        };
        match dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
        }
    });
}

fn cached_info_for_sort<'a>(
    probe_cache: &'a HashMap<PathBuf, ProbeCacheEntry>,
    entry: &BrowseEntry,
) -> Option<&'a CachedInfo> {
    let identity = ProbeCacheIdentity::from_entry(entry);
    probe_cache.get(&entry.path).and_then(|cached| {
        if cached.is_valid_for(identity) {
            cached.info.as_ref().map(|info| info.as_ref())
        } else {
            None
        }
    })
}

fn audio_format_sort_value(entry: &BrowseEntry, cached: Option<&CachedInfo>) -> Option<String> {
    if !entry.is_audio() {
        return None;
    }
    cached
        .map(|info| info.source.format_name.clone())
        .filter(|value| !value.trim().is_empty())
        .or_else(|| Some(entry.type_label()))
}

fn compare_opt_ord<T: Ord>(left: Option<T>, right: Option<T>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (left, right) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_opt_string(left: Option<String>, right: Option<String>) -> std::cmp::Ordering {
    compare_opt_ord(
        left.map(|value| value.to_ascii_lowercase()),
        right.map(|value| value.to_ascii_lowercase()),
    )
}

fn compare_opt_f64(left: Option<f64>, right: Option<f64>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (left, right) {
        (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn positive_u32(value: u32) -> Option<u32> {
    (value > 0).then_some(value)
}

fn positive_f64(value: f64) -> Option<f64> {
    (value.is_finite() && value > 0.0).then_some(value)
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
        EntryKind::SacdIso
        | EntryKind::DvdAudioIso
        | EntryKind::DvdAudioDir
        | EntryKind::DvdVideoIso
        | EntryKind::DvdVideoDir
        | EntryKind::BlurayIso
        | EntryKind::BlurayDir => 25,
        EntryKind::Archive => 25,
        EntryKind::OtherFile => 30,
    }
}

/// Compute stats for a directory: total file count, audio count, total size.
/// Walks recursively into all subdirectories. Symlinks are skipped (avoids
/// loops). Bounded by `MAX_WALK_DEPTH` and `MAX_WALK_FILES` to prevent
/// runaway computation on huge trees. Always called from a background task.
fn compute_dir_stats(path: &Path, cancel: &AtomicBool) -> Option<DirStats> {
    const MAX_WALK_DEPTH: u32 = 20;
    const MAX_WALK_FILES: usize = 1_000_000;

    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    let mut stats = DirStats::default();
    if walk_dir_for_stats(path, &mut stats, 0, MAX_WALK_DEPTH, MAX_WALK_FILES, cancel) {
        Some(stats)
    } else {
        None
    }
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
    cancel: &AtomicBool,
) -> bool {
    if cancel.load(Ordering::Relaxed) {
        return false;
    }
    if depth >= max_depth || stats.file_count >= max_files {
        return true;
    }
    let read = match fs::read_dir(path) {
        Ok(r) => r,
        Err(_) => return true,
    };
    for entry in read.flatten() {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
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
                    return true;
                }
            }
        } else if file_type.is_dir() {
            if !walk_dir_for_stats(&entry.path(), stats, depth + 1, max_depth, max_files, cancel) {
                return false;
            }
            if stats.file_count >= max_files {
                return true;
            }
        }
    }
    true
}

impl Default for BrowseState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod browse_pane_toggle_tests {
    use super::*;

    #[test]
    fn single_click_browse_pane_toggle_is_non_destructive() {
        let mut browse = BrowseState::new();
        browse.explore_collapsed = false;
        browse.info_collapsed = false;
        browse.browse_maximized = false;

        browse.toggle_pane(BrowsePaneId::Browse);

        assert!(!browse.explore_collapsed);
        assert!(!browse.info_collapsed);
        assert!(!browse.browse_maximized);
    }

    #[test]
    fn browse_title_double_click_path_still_maximizes_and_restores() {
        let mut browse = BrowseState::new();

        browse.toggle_browse_maximized();
        assert!(browse.explore_collapsed);
        assert!(browse.info_collapsed);
        assert!(browse.browse_maximized);

        browse.toggle_browse_maximized();
        assert!(!browse.explore_collapsed);
        assert!(!browse.info_collapsed);
        assert!(!browse.browse_maximized);
    }

    #[test]
    fn pane_enabled_state_is_independent_from_collapsed_state() {
        let mut browse = BrowseState::new();
        browse.explore_collapsed = true;
        browse.info_collapsed = false;
        browse.explore_enabled = true;
        browse.info_enabled = true;

        browse.toggle_pane_enabled(BrowsePaneId::Explore);

        assert!(!browse.explore_enabled);
        assert!(browse.info_enabled);
        assert!(browse.explore_collapsed);
        assert!(!browse.info_collapsed);
    }

    #[test]
    fn captured_browsing_config_includes_side_pane_enabled_state() {
        let mut browse = BrowseState::new();
        browse.explore_enabled = false;
        browse.info_enabled = true;
        browse.explore_collapsed = true;
        browse.info_collapsed = false;

        let captured = browse.capture_browsing_config();

        assert!(!captured.layout_explore_enabled);
        assert!(captured.layout_info_enabled);
        assert_eq!(captured.layout_explore, "collapsed");
        assert_eq!(captured.layout_info, "open");
    }

    #[test]
    fn back_or_close_options_menu_returns_from_submenu_before_closing_root() {
        let mut browse = BrowseState::new();
        browse.options_menu = BrowseOptionsMenu::Columns;

        browse.back_or_close_options_menu();
        assert_eq!(browse.options_menu, BrowseOptionsMenu::Root);

        browse.back_or_close_options_menu();
        assert_eq!(browse.options_menu, BrowseOptionsMenu::Closed);

        browse.back_or_close_options_menu();
        assert_eq!(browse.options_menu, BrowseOptionsMenu::Closed);
    }

    #[test]
    fn apply_browsing_config_preserves_enabled_and_collapsed_as_separate_states() {
        let mut browse = BrowseState::new();
        let config = crate::config::BrowsingConfig {
            layout_explore_enabled: false,
            layout_info_enabled: true,
            layout_explore: "collapsed".to_string(),
            layout_info: "open".to_string(),
            ..crate::config::BrowsingConfig::default()
        };

        browse.apply_browsing_config(&config);

        assert!(!browse.explore_enabled);
        assert!(browse.info_enabled);
        assert!(browse.explore_collapsed);
        assert!(!browse.info_collapsed);

        let captured = browse.capture_browsing_config();
        assert!(!captured.layout_explore_enabled);
        assert!(captured.layout_info_enabled);
        assert_eq!(captured.layout_explore, "collapsed");
        assert_eq!(captured.layout_info, "open");
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
/// Uses the in-memory `tag_cache` only when the current file fingerprint
/// matches the fingerprint captured when tags were read. This prevents an
/// active search panel from matching stale tags after metadata writes.
fn build_tag_search_string_cached(
    path: &Path,
    tag_cache: &mut std::collections::HashMap<PathBuf, CachedTagSearchString>,
) -> String {
    let fingerprint = match TagCacheFingerprint::for_path(path) {
        Some(fingerprint) => fingerprint,
        None => {
            tag_cache.remove(path);
            return read_tags_from_file(path).tag_string;
        }
    };

    if let Some(cached) = tag_cache.get(path) {
        if cached.fingerprint == fingerprint {
            return cached.tag_string.clone();
        }
    }

    let result = match read_tags_from_file_checked(path) {
        Ok(result) => result,
        Err(_) => {
            tag_cache.remove(path);
            return String::new();
        }
    };
    tag_cache.insert(
        path.to_path_buf(),
        CachedTagSearchString {
            fingerprint,
            tag_string: result.tag_string.clone(),
        },
    );
    result.tag_string
}

#[derive(Debug, Clone)]
enum TagSearchSource {
    Filesystem(PathBuf),
    StagedArchiveEntry {
        staged_path: PathBuf,
        fallback_metadata: Option<SourceMetadata>,
    },
    Metadata(SourceMetadata),
    ExtractArchiveEntry {
        archive_path: PathBuf,
        inner_path: String,
        password: Option<String>,
        synthetic_path: PathBuf,
    },
    Missing,
}

#[derive(Debug, Clone)]
struct ArchiveSearchCandidate {
    entry: BrowseEntry,
    inner_path: Option<String>,
    staged_path: Option<PathBuf>,
    fallback_metadata: Option<SourceMetadata>,
}

#[derive(Debug, Clone)]
struct ArchiveSearchWorkerOutput {
    results: Vec<(BrowseEntry, i64)>,
    total_matches: usize,
    archive_tag_cache_updates: Vec<(PathBuf, TagCacheFingerprint, ArchiveTagPasswordIdentity, TagReadResult)>,
}

/// Tag data read from a file for search and cache storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagReadResult {
    tag_string: String,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    genre: Option<String>,
    year: Option<String>,
}

impl TagReadResult {
    fn empty() -> Self {
        Self {
            tag_string: String::new(),
            title: None,
            artist: None,
            album: None,
            genre: None,
            year: None,
        }
    }

    fn has_tag_data(&self) -> bool {
        !self.tag_string.trim().is_empty()
            || self.title.as_ref().is_some_and(|value| !value.trim().is_empty())
            || self.artist.as_ref().is_some_and(|value| !value.trim().is_empty())
            || self.album.as_ref().is_some_and(|value| !value.trim().is_empty())
            || self.genre.as_ref().is_some_and(|value| !value.trim().is_empty())
            || self.year.as_ref().is_some_and(|value| !value.trim().is_empty())
    }
}

/// Read tag fields from a file via lofty. Returns concatenated search
/// string plus individual fields for caching and sorting. An empty successful
/// result means the file was readable but had no usable tags, which is
/// deterministic for the current identity. A read/open error is transient and
/// must not be written to SQLite or other long-lived caches.
fn read_tags_from_file_checked(path: &Path) -> Result<TagReadResult, String> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;

    let tagged = lofty::read_from_path(path).map_err(|err| format!("tag read failed: {err}"))?;
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(TagReadResult::empty());
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

    Ok(TagReadResult {
        tag_string: parts.join(" ").to_lowercase(),
        title,
        artist,
        album,
        genre,
        year,
    })
}

fn read_tags_from_file(path: &Path) -> TagReadResult {
    read_tags_from_file_checked(path).unwrap_or_else(|_| TagReadResult::empty())
}

fn read_tags_from_archive_entry(
    archive_path: &Path,
    inner_path: &str,
    password: Option<&str>,
) -> TagReadResult {
    let staging_dir = std::env::temp_dir().join(format!(
        "tonepoet-archive-tag-search-{}",
        uuid::Uuid::new_v4()
    ));

    let result = match extract_archive_entry_to_temp_blocking(
        archive_path,
        inner_path,
        password,
        &staging_dir,
    ) {
        Ok(path) => read_tags_from_file(&path),
        Err(_) => TagReadResult::empty(),
    };

    let _ = std::fs::remove_dir_all(&staging_dir);
    result
}

fn extract_archive_entry_to_temp_blocking(
    archive_path: &Path,
    inner_path: &str,
    password: Option<&str>,
    staging_dir: &Path,
) -> Result<PathBuf, String> {
    let extracted = staging_path_for_archive_inner(staging_dir, inner_path)?;
    let extraction_mode = validate_archive_entry_probe_selector(inner_path)?;
    let bin = crate::detect_7z_binary()
        .ok_or_else(|| "neither 7zz nor 7z found in PATH".to_string())?;
    std::fs::create_dir_all(staging_dir)
        .map_err(|err| format!("create archive-entry tag-search staging failed: {err}"))?;

    let mut cmd = std::process::Command::new(&bin);
    cmd.arg("x")
        .arg(archive_path)
        .arg(format!("-o{}", staging_dir.display()))
        .arg("-y")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    if let ArchiveEntryProbeExtraction::SingleMember = extraction_mode {
        cmd.arg(inner_path);
    }
    if let Some(password) = password {
        cmd.arg(format!("-p{}", password));
    }

    let output = cmd
        .output()
        .map_err(|err| format!("failed to run {}: {}", bin, err))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let operation = match extraction_mode {
            ArchiveEntryProbeExtraction::SingleMember => "archive entry tag extraction",
            ArchiveEntryProbeExtraction::FullArchiveFallback => "archive tag extraction fallback",
        };
        return Err(format!("{} failed: {}", operation, stderr.trim()));
    }

    if extracted.is_file() {
        Ok(extracted)
    } else {
        Err(format!(
            "archive entry tag extraction did not produce a file: {}",
            inner_path
        ))
    }
}


#[cfg(test)]
static TEST_ARCHIVE_TAG_FIXTURES: std::sync::OnceLock<
    std::sync::Mutex<HashMap<(PathBuf, String), TagReadResult>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn test_archive_tag_fixture(archive_path: &Path, inner_path: &str) -> Option<TagReadResult> {
    TEST_ARCHIVE_TAG_FIXTURES
        .get()
        .and_then(|fixtures| fixtures.lock().ok())
        .and_then(|fixtures| fixtures.get(&(archive_path.to_path_buf(), inner_path.to_string())).cloned())
}

struct ArchiveTagExtractionSession {
    archive_path: PathBuf,
    password: Option<String>,
    staging_dir: PathBuf,
    full_archive_extracted: bool,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

impl ArchiveTagExtractionSession {
    fn new(
        archive_path: PathBuf,
        password: Option<String>,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        let staging_dir = std::env::temp_dir().join(format!(
            "tonepoet-archive-tag-search-{}",
            uuid::Uuid::new_v4()
        ));
        Self {
            archive_path,
            password,
            staging_dir,
            full_archive_extracted: false,
            cancel,
        }
    }

    fn read_entry(&mut self, inner_path: &str) -> TagReadResult {
        #[cfg(test)]
        if let Some(tags) = test_archive_tag_fixture(&self.archive_path, inner_path) {
            return tags;
        }

        match self.extract_entry(inner_path) {
            Ok(path) => read_tags_from_file(&path),
            Err(_) => TagReadResult::empty(),
        }
    }

    fn extract_entry(&mut self, inner_path: &str) -> Result<PathBuf, String> {
        let extracted = staging_path_for_archive_inner(&self.staging_dir, inner_path)?;
        let extraction_mode = validate_archive_entry_probe_selector(inner_path)?;
        match extraction_mode {
            ArchiveEntryProbeExtraction::SingleMember => {
                if !extracted.is_file() {
                    run_7z_extract_to_dir(
                        &self.archive_path,
                        self.password.as_deref(),
                        &self.staging_dir,
                        Some(inner_path),
                        "archive entry tag extraction",
                        Some(&self.cancel),
                    )?;
                }
            }
            ArchiveEntryProbeExtraction::FullArchiveFallback => {
                if !self.full_archive_extracted {
                    run_7z_extract_to_dir(
                        &self.archive_path,
                        self.password.as_deref(),
                        &self.staging_dir,
                        None,
                        "archive tag extraction fallback",
                        Some(&self.cancel),
                    )?;
                    self.full_archive_extracted = true;
                }
            }
        }

        if extracted.is_file() {
            Ok(extracted)
        } else {
            Err(format!(
                "archive entry tag extraction did not produce a file: {}",
                inner_path
            ))
        }
    }
}

impl Drop for ArchiveTagExtractionSession {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.staging_dir);
    }
}

fn run_7z_extract_to_dir(
    archive_path: &Path,
    password: Option<&str>,
    staging_dir: &Path,
    member: Option<&str>,
    operation: &str,
    cancel: Option<&Arc<std::sync::atomic::AtomicBool>>,
) -> Result<(), String> {
    let bin = crate::detect_7z_binary()
        .ok_or_else(|| "neither 7zz nor 7z found in PATH".to_string())?;
    std::fs::create_dir_all(staging_dir)
        .map_err(|err| format!("create archive-entry tag-search staging failed: {err}"))?;

    let mut cmd = std::process::Command::new(&bin);
    cmd.arg("x")
        .arg(archive_path)
        .arg(format!("-o{}", staging_dir.display()))
        .arg("-y")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    if let Some(member) = member {
        cmd.arg(member);
    }
    if let Some(password) = password {
        cmd.arg(format!("-p{}", password));
    }

    if let Some(cancel) = cancel {
        use std::io::Read;
        let mut child = cmd
            .spawn()
            .map_err(|err| format!("failed to run {}: {}", bin, err))?;
        loop {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} cancelled", operation));
            }
            match child
                .try_wait()
                .map_err(|err| format!("failed to wait for {}: {}", bin, err))?
            {
                Some(status) => {
                    let mut stderr = String::new();
                    if let Some(mut pipe) = child.stderr.take() {
                        let _ = pipe.read_to_string(&mut stderr);
                    }
                    if !status.success() {
                        return Err(format!("{} failed: {}", operation, stderr.trim()));
                    }
                    return Ok(());
                }
                None => std::thread::sleep(std::time::Duration::from_millis(25)),
            }
        }
    }

    let output = cmd
        .output()
        .map_err(|err| format!("failed to run {}: {}", bin, err))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{} failed: {}", operation, stderr.trim()));
    }
    Ok(())
}

fn run_archive_search_worker(
    archive_path: PathBuf,
    password: Option<String>,
    archive_fingerprint: Option<TagCacheFingerprint>,
    cached_archive_tags: HashMap<PathBuf, CachedArchiveTagSearchString>,
    candidates: Vec<ArchiveSearchCandidate>,
    query: String,
    show_hidden: bool,
    audio_only: bool,
    format_filter: FormatFilter,
    mode: SearchMode,
    sort: SearchSort,
    sort_dir: SortDir,
    result_cap: usize,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> Option<ArchiveSearchWorkerOutput> {
    use fuzzy_matcher::skim::SkimMatcherV2;
    use fuzzy_matcher::FuzzyMatcher;

    let matcher = SkimMatcherV2::default();
    let min_score = search_fuzzy_min_score(&query);
    let search_tags = matches!(mode, SearchMode::Tags | SearchMode::Both);
    let search_filename = matches!(mode, SearchMode::Filename | SearchMode::Both);
    let mut scored: Vec<(BrowseEntry, i64)> = Vec::new();
    let mut resolved_tags: HashMap<PathBuf, TagReadResult> = HashMap::new();
    let mut cache_updates: Vec<(PathBuf, TagCacheFingerprint, ArchiveTagPasswordIdentity, TagReadResult)> = Vec::new();
    let password_identity = ArchiveTagPasswordIdentity::for_password(password.as_deref());
    let mut extraction = ArchiveTagExtractionSession::new(archive_path, password, cancel.clone());

    for candidate in &candidates {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        let e = &candidate.entry;
        if !show_hidden && browse_entry_name_is_hidden(&e.name) {
            continue;
        }
        if !entry_passes_search_effective_filters(e, show_hidden, audio_only, &format_filter) {
            continue;
        }

        let mut best_score: Option<i64> = None;
        if search_filename || matches!(&e.kind, EntryKind::Directory) {
            if let Some(s) = matcher.fuzzy_match(&e.name_lower, &query) {
                best_score = Some(best_score.map_or(s, |prev: i64| prev.max(s)));
            }
        }
        if search_tags && matches!(&e.kind, EntryKind::AudioFile(_)) {
            let tags = archive_tags_for_candidate_worker(
                candidate,
                archive_fingerprint.as_ref(),
                &cached_archive_tags,
                &mut resolved_tags,
                password_identity,
                &mut cache_updates,
                &mut extraction,
            );
            if !tags.tag_string.is_empty() {
                if let Some(s) = matcher.fuzzy_match(&tags.tag_string, &query) {
                    best_score = Some(best_score.map_or(s, |prev: i64| prev.max(s)));
                }
            }
        }

        if let Some(score) = best_score {
            if search_fuzzy_score_passes_threshold(score, min_score) {
                scored.push((e.clone(), score));
            }
        }
    }

    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }

    if sort.is_tag_sort() {
        let mut keyed: Vec<(bool, String, String, usize)> = scored
            .iter()
            .enumerate()
            .map(|(idx, (entry, _))| {
                let key = candidates
                    .iter()
                    .find(|candidate| candidate.entry.path == entry.path)
                    .map(|candidate| {
                        let tags = archive_tags_for_candidate_worker(
                            candidate,
                            archive_fingerprint.as_ref(),
                            &cached_archive_tags,
                            &mut resolved_tags,
                            password_identity,
                            &mut cache_updates,
                            &mut extraction,
                        );
                        tag_sort_key_from_read_result(&tags, sort)
                    })
                    .unwrap_or_default();
                (key.is_empty(), key, entry.name_lower.clone(), idx)
            })
            .collect();
        keyed.sort_by(|a, b| {
            let ord = a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(&b.2));
            match sort_dir {
                SortDir::Asc => ord,
                SortDir::Desc => ord.reverse(),
            }
        });
        let sorted: Vec<_> = keyed.into_iter().map(|(_, _, _, idx)| scored[idx].clone()).collect();
        scored = sorted;
    } else {
        sort_search_results(&mut scored, sort, sort_dir);
    }

    let total_matches = scored.len();
    scored.truncate(result_cap.max(1));

    Some(ArchiveSearchWorkerOutput {
        results: scored,
        total_matches,
        archive_tag_cache_updates: cache_updates,
    })
}

fn archive_tags_for_candidate_worker(
    candidate: &ArchiveSearchCandidate,
    archive_fingerprint: Option<&TagCacheFingerprint>,
    cached_archive_tags: &HashMap<PathBuf, CachedArchiveTagSearchString>,
    resolved_tags: &mut HashMap<PathBuf, TagReadResult>,
    password_identity: ArchiveTagPasswordIdentity,
    cache_updates: &mut Vec<(PathBuf, TagCacheFingerprint, ArchiveTagPasswordIdentity, TagReadResult)>,
    extraction: &mut ArchiveTagExtractionSession,
) -> TagReadResult {
    let synthetic_path = &candidate.entry.path;
    if let Some(tags) = resolved_tags.get(synthetic_path) {
        return tags.clone();
    }

    if let Some(staged_path) = candidate.staged_path.as_ref() {
        let tags = read_tags_from_file(staged_path);
        if !tags.tag_string.is_empty() {
            resolved_tags.insert(synthetic_path.clone(), tags.clone());
            return tags;
        }
    }

    if let Some(metadata) = candidate.fallback_metadata.as_ref() {
        let tags = tag_read_result_from_metadata(metadata);
        if !tags.tag_string.is_empty() {
            resolved_tags.insert(synthetic_path.clone(), tags.clone());
            return tags;
        }
    }

    if let (Some(fingerprint), Some(cached)) = (archive_fingerprint, cached_archive_tags.get(synthetic_path)) {
        if &cached.archive_fingerprint == fingerprint
            && cached.password_identity == password_identity
            && cached.tags.has_tag_data()
        {
            resolved_tags.insert(synthetic_path.clone(), cached.tags.clone());
            return cached.tags.clone();
        }
    }

    let tags = if matches!(candidate.entry.kind, EntryKind::AudioFile(_)) {
        candidate
            .inner_path
            .as_deref()
            .map(|inner| extraction.read_entry(inner))
            .unwrap_or_else(TagReadResult::empty)
    } else {
        TagReadResult::empty()
    };

    if let Some(fingerprint) = archive_fingerprint {
        if tags.has_tag_data() {
            cache_updates.push((synthetic_path.clone(), fingerprint.clone(), password_identity, tags.clone()));
        }
    }
    resolved_tags.insert(synthetic_path.clone(), tags.clone());
    tags
}

fn tag_search_string_from_metadata(metadata: &SourceMetadata) -> String {
    tag_read_result_from_metadata(metadata).tag_string
}

fn tag_read_result_from_metadata(metadata: &SourceMetadata) -> TagReadResult {
    let title = metadata.title.clone().filter(|value| !value.trim().is_empty());
    let artist = metadata.artist.clone().filter(|value| !value.trim().is_empty());
    let album = metadata.album.clone().filter(|value| !value.trim().is_empty());
    let genre = metadata.genre.clone().filter(|value| !value.trim().is_empty());
    let year = metadata.year.clone().filter(|value| !value.trim().is_empty());

    let mut parts: Vec<&str> = Vec::new();
    if let Some(ref value) = title {
        parts.push(value);
    }
    if let Some(ref value) = artist {
        parts.push(value);
    }
    if let Some(ref value) = album {
        parts.push(value);
    }
    if let Some(ref value) = genre {
        parts.push(value);
    }
    if let Some(ref value) = year {
        parts.push(value);
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

fn tag_sort_key_from_read_result(tags: &TagReadResult, sort: SearchSort) -> String {
    let value = match sort {
        SearchSort::Artist => tags.artist.as_deref(),
        SearchSort::Album => tags.album.as_deref(),
        SearchSort::Year => tags.year.as_deref(),
        SearchSort::Title => tags.title.as_deref(),
        _ => None,
    };

    match sort {
        SearchSort::Year => value.map(normalize_year_sort_key).unwrap_or_default(),
        _ => value
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .unwrap_or_default(),
    }
}

fn tag_sort_key_from_metadata(metadata: &SourceMetadata, sort: SearchSort) -> String {
    let value = match sort {
        SearchSort::Artist => metadata.artist.as_deref(),
        SearchSort::Album => metadata.album.as_deref(),
        SearchSort::Year => metadata.year.as_deref(),
        SearchSort::Title => metadata.title.as_deref(),
        _ => None,
    };

    match sort {
        SearchSort::Year => value.map(normalize_year_sort_key).unwrap_or_default(),
        _ => value
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .unwrap_or_default(),
    }
}

fn normalize_year_sort_key(value: &str) -> String {
    let trimmed = value.trim();
    if let Ok(year) = trimmed.parse::<u32>() {
        format!("{:04}", year)
    } else {
        trimmed.to_ascii_lowercase()
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
fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn initial_browse_tree_nodes(current_dir: &Path, show_hidden: bool) -> Vec<BrowseTreeNode> {
    tui_file_picker::initial_tree_nodes_with_hidden(current_dir, show_hidden)
}

fn browse_tree_expand_ancestors(nodes: &mut Vec<BrowseTreeNode>, target: &Path, show_hidden: bool) {
    if nodes.is_empty() {
        *nodes = initial_browse_tree_nodes(target, show_hidden);
    }
    tui_file_picker::expand_tree_to_path(nodes, target, show_hidden);
}

fn browse_tree_expand_path(nodes: &mut Vec<BrowseTreeNode>, target: &Path, show_hidden: bool) {
    browse_tree_expand_ancestors(nodes, target, show_hidden);
    let Some(index) = nodes.iter().position(|node| same_path(&node.path, target)) else {
        return;
    };
    if nodes[index].expanded || !nodes[index].has_children {
        return;
    }
    nodes[index].expanded = true;
    tui_file_picker::refresh_tree_children(nodes, target, show_hidden);
}

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



fn dvda_directory_classification_fingerprint(entry: &BrowseEntry) -> ClassificationFingerprint {
    let marker = entry.path.join("AUDIO_TS").join("AUDIO_TS.IFO");
    std::fs::metadata(&marker)
        .ok()
        .map(|m| ClassificationFingerprint::from_metadata(&m))
        .unwrap_or_else(|| ClassificationFingerprint::from_entry(entry))
}

fn dvdv_directory_classification_fingerprint(entry: &BrowseEntry) -> ClassificationFingerprint {
    let marker = crate::disc::dvdv_utils::directory_video_ts_file_path(&entry.path, "VIDEO_TS.IFO");
    marker
        .as_ref()
        .and_then(|marker| std::fs::metadata(marker).ok())
        .map(|m| ClassificationFingerprint::from_metadata(&m))
        .unwrap_or_else(|| ClassificationFingerprint::from_entry(entry))
}

fn classify_dvda_directory_entry(
    entry: &mut BrowseEntry,
    cache: &mut HashMap<PathBuf, (ClassificationFingerprint, bool)>,
) {
    if !matches!(entry.kind, EntryKind::Directory | EntryKind::DvdAudioDir) {
        return;
    }
    let fingerprint = dvda_directory_classification_fingerprint(entry);

    let is_dvda = cache
        .get(&entry.path)
        .filter(|(cached, _)| *cached == fingerprint)
        .map(|(_, verdict)| *verdict)
        .unwrap_or_else(|| {
            let verdict = crate::disc::dvda_utils::is_dvda_directory(&entry.path);
            if verdict || should_cache_directory_classification_negative(&entry.path) {
                cache.insert(entry.path.clone(), (fingerprint.clone(), verdict));
            }
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
    let fingerprint = dvdv_directory_classification_fingerprint(entry);

    let is_dvdv = cache
        .get(&entry.path)
        .filter(|(cached, _)| *cached == fingerprint)
        .map(|(_, verdict)| *verdict)
        .unwrap_or_else(|| {
            let verdict = crate::disc::dvdv_utils::is_dvdv_directory(&entry.path);
            if verdict || should_cache_directory_classification_negative(&entry.path) {
                cache.insert(entry.path.clone(), (fingerprint.clone(), verdict));
            }
            verdict
        });
    if is_dvdv {
        entry.kind = EntryKind::DvdVideoDir;
    } else if matches!(entry.kind, EntryKind::DvdVideoDir) {
        entry.kind = EntryKind::Directory;
    }
}

fn classify_bluray_directory_entry(
    entry: &mut BrowseEntry,
    cache: &mut HashMap<PathBuf, (ClassificationFingerprint, bool)>,
) {
    if !matches!(entry.kind, EntryKind::Directory | EntryKind::BlurayDir) {
        return;
    }
    let fingerprint = bluray_directory_classification_fingerprint(entry);

    let is_bluray = cache
        .get(&entry.path)
        .filter(|(cached, _)| *cached == fingerprint)
        .map(|(_, verdict)| *verdict)
        .unwrap_or_else(|| {
            let verdict = crate::disc::bluray_utils::is_bluray_directory(&entry.path);
            if verdict || should_cache_directory_classification_negative(&entry.path) {
                cache.insert(entry.path.clone(), (fingerprint.clone(), verdict));
            }
            verdict
        });
    if is_bluray {
        entry.kind = EntryKind::BlurayDir;
    } else if matches!(entry.kind, EntryKind::BlurayDir) {
        entry.kind = EntryKind::Directory;
    }
}

fn bluray_directory_classification_fingerprint(entry: &BrowseEntry) -> ClassificationFingerprint {
    let Some(paths) = crate::disc::bluray_utils::bluray_directory_layout_paths(&entry.path) else {
        return ClassificationFingerprint::from_entry(entry);
    };

    let mut fingerprint = paths
        .index
        .as_ref()
        .and_then(|marker| std::fs::metadata(marker).ok())
        .or_else(|| std::fs::metadata(&paths.bdmv).ok())
        .map(|metadata| ClassificationFingerprint::from_metadata(&metadata))
        .unwrap_or_else(|| ClassificationFingerprint::from_entry(entry));

    fingerprint.markers = vec![
        classification_marker_fingerprint("BDMV", Some(paths.bdmv)),
        classification_marker_fingerprint("index.bdmv", paths.index),
        classification_marker_fingerprint("MovieObject.bdmv", paths.movie_object),
        classification_marker_fingerprint("PLAYLIST", paths.playlist_dir),
        classification_marker_fingerprint("STREAM", paths.stream_dir),
        classification_marker_fingerprint("first .mpls", paths.first_playlist),
        classification_marker_fingerprint("first .m2ts", paths.first_stream),
    ];

    fingerprint
}

fn classification_marker_fingerprint(
    label: &'static str,
    path: Option<PathBuf>,
) -> ClassificationMarkerFingerprint {
    let metadata = path.as_ref().and_then(|path| std::fs::metadata(path).ok());
    ClassificationMarkerFingerprint {
        label,
        path,
        len: metadata.as_ref().map(std::fs::Metadata::len),
        modified: metadata.and_then(|metadata| metadata.modified().ok()),
    }
}

fn is_audio_filter_visible_entry(entry: &BrowseEntry) -> bool {
    matches!(
        entry.kind,
        EntryKind::AudioFile(_)
            | EntryKind::SacdIso
            | EntryKind::DvdAudioIso
            | EntryKind::DvdAudioDir
            | EntryKind::DvdVideoIso
            | EntryKind::DvdVideoDir
            | EntryKind::BlurayIso
            | EntryKind::BlurayDir
            | EntryKind::Directory
    )
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


pub fn classify_probe_failure_for_negative_cache(error: &str) -> NegativeCacheDecision {
    let lower = error.to_ascii_lowercase();
    if lower.contains("no audio stream found") {
        NegativeCacheDecision::CacheDeterministic
    } else {
        NegativeCacheDecision::DoNotCacheTransient
    }
}

fn file_classification_negative_cache_decision(path: &Path) -> NegativeCacheDecision {
    match std::fs::File::open(path) {
        Ok(_) => NegativeCacheDecision::CacheDeterministic,
        Err(_) => NegativeCacheDecision::DoNotCacheTransient,
    }
}

fn directory_classification_negative_cache_decision(path: &Path) -> NegativeCacheDecision {
    match std::fs::read_dir(path) {
        Ok(_) => NegativeCacheDecision::CacheDeterministic,
        Err(_) => NegativeCacheDecision::DoNotCacheTransient,
    }
}

fn should_cache_file_classification_negative(path: &Path) -> bool {
    matches!(
        file_classification_negative_cache_decision(path),
        NegativeCacheDecision::CacheDeterministic
    )
}

fn should_cache_directory_classification_negative(path: &Path) -> bool {
    matches!(
        directory_classification_negative_cache_decision(path),
        NegativeCacheDecision::CacheDeterministic
    )
}

/// A file is queueable for conversion if it's an audio file, a CUE sheet,
/// a supported archive (7z), or a supported disc ISO. Generic ISOs, zips,
/// rars, etc. that the pipeline can't handle are excluded to avoid noisy queue errors.
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
                // ISOs are only queueable if they're supported disc images.
                Some("iso") => crate::tui::sacd::is_sacd_iso(path)
                    || crate::disc::dvda_utils::is_dvda_iso(path)
                    || crate::disc::dvdv_utils::is_dvdv_iso(path)
                    || crate::disc::bluray_utils::is_bluray_iso(path),
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
    use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
    use std::time::Duration;

    fn test_cached_info(file_size: u64, title: &str) -> CachedInfo {
        CachedInfo {
            source: crate::tui::probe::SourceInfo {
                format_name: "FLAC".to_string(),
                codec: "flac".to_string(),
                bit_depth: Some(16),
                sample_rate: 44_100,
                channels: 2,
                channel_layout: "stereo".to_string(),
                duration_secs: 1.0,
                file_size,
            },
            metadata: crate::tui::probe::SourceMetadata {
                title: Some(title.to_string()),
                ..Default::default()
            },
        }
    }

    fn probe_file_fixture(name: &str, contents: &[u8]) -> (tempfile::TempDir, PathBuf, BrowseEntry, ProbeCacheIdentity, i64) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::write(&path, contents).expect("probe file");
        let metadata = std::fs::metadata(&path).expect("metadata");
        let identity = ProbeCacheIdentity::from_metadata(&metadata);
        let mtime_unix = identity
            .modified
            .map(crate::db::systemtime_to_unix)
            .unwrap_or(0);
        let entry = BrowseEntry::new(
            path.clone(),
            name.to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            identity.size,
            identity.modified,
        );
        (dir, path, entry, identity, mtime_unix)
    }

    #[test]
    fn filesystem_async_completion_classifier_enforces_accept_changed_missing_policy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("track.flac");
        std::fs::write(&path, b"old").expect("write old");
        let old = ProbeCacheIdentity::from_metadata(&std::fs::metadata(&path).expect("old meta"));
        let state = BrowseState::new();

        assert_eq!(
            state.classify_filesystem_async_completion(&path, old),
            FilesystemAsyncCompletion::Accept
        );

        std::fs::write(&path, b"new contents with different size").expect("write new");
        assert_eq!(
            state.classify_filesystem_async_completion(&path, old),
            FilesystemAsyncCompletion::Changed
        );

        std::fs::remove_file(&path).expect("remove");
        assert_eq!(
            state.classify_filesystem_async_completion(&path, old),
            FilesystemAsyncCompletion::MissingOrUnstatable
        );
    }

    #[test]
    fn probe_current_stops_when_selected_file_is_unstatable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("deleted.flac");
        std::fs::write(&path, b"old").expect("write");
        let meta = std::fs::metadata(&path).expect("metadata");
        let identity = ProbeCacheIdentity::from_metadata(&meta);
        let mut state = BrowseState::new();
        state.entries = vec![BrowseEntry::new(
            path.clone(),
            "deleted.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            identity.size,
            identity.modified,
        )];
        state.selected_index = 0;
        state.probe_pending.insert(path.clone());
        state.insert_probe_miss_for_identity(path.clone(), identity);
        std::fs::remove_file(&path).expect("delete");

        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        state.probe_current(&tx);

        assert!(state.probe_debounce.is_none());
        assert!(!state.probe_pending.contains(&path));
        assert!(!state.has_probe_cache_entry_for_identity(&path, identity));
    }

    #[test]
    fn expired_debounce_stops_when_file_disappeared() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("deleted.flac");
        std::fs::write(&path, b"old").expect("write");
        let meta = std::fs::metadata(&path).expect("metadata");
        let identity = ProbeCacheIdentity::from_metadata(&meta);
        let mut state = BrowseState::new();
        state.entries = vec![BrowseEntry::new(
            path.clone(),
            "deleted.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            identity.size,
            identity.modified,
        )];
        state.selected_index = 0;
        state.probe_debounce = Some(BrowseProbeDebounce {
            path: path.clone(),
            deadline: Instant::now() - Duration::from_millis(1),
        });
        std::fs::remove_file(&path).expect("delete");

        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        state.check_probe_debounce(&tx);

        assert!(state.probe_debounce.is_none());
        assert!(!state.probe_pending.contains(&path));
    }

    #[test]
    fn current_dir_stats_rejects_stale_directory_identity() {
        let path = std::path::PathBuf::from("/tmp/tonepoet-stale-dir-stats");
        let old_identity = ProbeCacheIdentity { modified: None, size: 10 };
        let new_identity = ProbeCacheIdentity { modified: None, size: 20 };

        let mut state = BrowseState::new();
        state.entries = vec![BrowseEntry::new(
            path.clone(),
            "tonepoet-stale-dir-stats".to_string(),
            EntryKind::Directory,
            new_identity.size,
            new_identity.modified,
        )];
        state.selected_index = 0;
        state.insert_dir_stats_for_identity(
            path.clone(),
            old_identity,
            DirStats {
                file_count: 99,
                audio_count: 9,
                total_size: 999,
            },
        );

        assert!(
            state.current_dir_stats().is_none(),
            "directory stats must not be reused after same-path identity changes"
        );

        state.insert_dir_stats_for_identity(
            path,
            new_identity,
            DirStats {
                file_count: 3,
                audio_count: 2,
                total_size: 200,
            },
        );
        let stats = state.current_dir_stats().expect("fresh stats");
        assert_eq!(stats.file_count, 3);
        assert_eq!(stats.audio_count, 2);
        assert_eq!(stats.total_size, 200);
    }

    #[test]
    fn current_cached_info_rejects_stale_probe_identity() {
        let path = std::path::PathBuf::from("/tmp/tonepoet-stale-track.flac");
        let old_identity = ProbeCacheIdentity {
            modified: Some(std::time::SystemTime::UNIX_EPOCH),
            size: 100,
        };
        let new_identity = ProbeCacheIdentity {
            modified: Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
            size: 200,
        };

        let mut state = BrowseState::new();
        state.entries = vec![BrowseEntry::new(
            path.clone(),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            new_identity.size,
            new_identity.modified,
        )];
        state.selected_index = 0;
        state.probe_cache.insert(
            path,
            ProbeCacheEntry::hit(old_identity, Arc::new(test_cached_info(100, "stale"))),
        );

        assert!(state.current_cached_info().is_none());
    }

    #[test]
    fn probe_cache_warm_merge_replaces_stale_identity() {
        let path = std::path::PathBuf::from("/tmp/tonepoet-warmed-track.flac");
        let old_identity = ProbeCacheIdentity { modified: None, size: 100 };
        let new_identity = ProbeCacheIdentity { modified: None, size: 200 };

        let mut state = BrowseState::new();
        state.entries = vec![BrowseEntry::new(
            path.clone(),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            new_identity.size,
            new_identity.modified,
        )];
        state.selected_index = 0;
        state.probe_cache.insert(path.clone(), ProbeCacheEntry::miss(old_identity));

        let merged = state.merge_probe_cache_warm_rows(vec![ProbeCacheWarmRow {
            path,
            identity: new_identity,
            info: test_cached_info(200, "fresh"),
        }]);

        assert_eq!(merged, 1);
        assert_eq!(
            state
                .current_cached_info()
                .and_then(|info| info.metadata.title.as_deref()),
            Some("fresh")
        );
    }

    #[test]
    fn canonical_probe_cache_helpers_validate_identity_and_remove_enrichment() {
        let path = std::path::PathBuf::from("/tmp/tonepoet-helper-track.flac");
        let stale = ProbeCacheIdentity { modified: None, size: 100 };
        let fresh = ProbeCacheIdentity { modified: None, size: 200 };
        let entry = BrowseEntry::new(
            path.clone(),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            fresh.size,
            fresh.modified,
        );
        let mut state = BrowseState::new();

        state.insert_probe_for_identity(
            path.clone(),
            stale,
            Some(Arc::new(test_cached_info(stale.size, "stale"))),
        );
        assert!(state.valid_probe_for_entry(&entry).is_none());
        assert!(!state.has_probe_cache_entry_for_identity(&path, fresh));

        state.insert_probe_for_identity(
            path.clone(),
            fresh,
            Some(Arc::new(test_cached_info(fresh.size, "fresh"))),
        );
        state.probe_cache_needs_metadata_enrichment.insert(path.clone());
        assert_eq!(
            state
                .valid_probe_for_entry(&entry)
                .and_then(|info| info.metadata.title.as_deref()),
            Some("fresh")
        );
        assert!(state.has_probe_cache_entry_for_identity(&path, fresh));

        state.remove_probe_cache_entry(&path);
        assert!(state.valid_probe_for_entry(&entry).is_none());
        assert!(!state.probe_cache_needs_metadata_enrichment.contains(&path));
    }

    #[test]
    fn probe_cache_metadata_update_requires_current_identity() {
        let path = std::path::PathBuf::from("/tmp/tonepoet-update-track.flac");
        let stale = ProbeCacheIdentity { modified: None, size: 100 };
        let fresh = ProbeCacheIdentity { modified: None, size: 200 };
        let mut state = BrowseState::new();
        state.entries = vec![BrowseEntry::new(
            path.clone(),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            fresh.size,
            fresh.modified,
        )];
        state.selected_index = 0;
        state.insert_probe_for_identity(
            path.clone(),
            stale,
            Some(Arc::new(test_cached_info(stale.size, "stale"))),
        );

        assert!(!state.update_valid_probe_for_current_path(&path, |cached| {
            cached.metadata.album = Some("wrong".to_string());
        }));
        assert!(state.current_cached_info().is_none());

        state.insert_probe_for_identity(
            path.clone(),
            fresh,
            Some(Arc::new(test_cached_info(fresh.size, "fresh"))),
        );
        assert!(state.update_valid_probe_for_current_path(&path, |cached| {
            cached.metadata.album = Some("album".to_string());
        }));
        assert_eq!(
            state
                .current_cached_info()
                .and_then(|info| info.metadata.album.as_deref()),
            Some("album")
        );
    }


    #[test]
    fn cold_probe_scheduler_backpressures_and_drops_stale_cursor_queue() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stale = dir.path().join("stale.flac");
        let current = dir.path().join("current.flac");
        std::fs::write(&stale, b"stale").expect("stale");
        std::fs::write(&current, b"current").expect("current");
        let stale_identity = ProbeCacheIdentity::from_metadata(&std::fs::metadata(&stale).expect("stale meta"));
        let current_identity = ProbeCacheIdentity::from_metadata(&std::fs::metadata(&current).expect("current meta"));

        let mut state = BrowseState::new();
        state.entries = vec![
            BrowseEntry::new(
                stale.clone(),
                "stale.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                stale_identity.size,
                stale_identity.modified,
            ),
            BrowseEntry::new(
                current.clone(),
                "current.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                current_identity.size,
                current_identity.modified,
            ),
        ];
        state.selected_index = 1;
        for idx in 0..BROWSE_COLD_PROBE_MAX_IN_FLIGHT {
            state
                .browse_cold_probe_active
                .insert(dir.path().join(format!("active-{idx}.flac")));
        }
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        state.schedule_cursor_focused_cold_probe(current.clone(), current_identity, &tx);

        assert_eq!(state.browse_cold_probe_queue.len(), 1);
        assert_eq!(state.browse_cold_probe_queue.front().map(|request| request.path.as_path()), Some(current.as_path()));
        assert!(!state.probe_pending.contains(&current));

        state.selected_index = 0;
        state.complete_browse_cold_probe(&dir.path().join("active-0.flac"), &tx);

        assert!(state.browse_cold_probe_queue.is_empty());
        assert!(!state.probe_pending.contains(&current));
        assert!(!state.browse_cold_probe_active.contains(&current));
    }

    #[tokio::test]
    async fn queued_cursor_cold_probe_starts_when_slot_opens() {
        let (_dir, path, entry, identity, _mtime) = probe_file_fixture("queued-current.flac", b"queued");
        let mut state = BrowseState::new();
        state.entries = vec![entry];
        state.selected_index = 0;
        for idx in 0..BROWSE_COLD_PROBE_MAX_IN_FLIGHT {
            state
                .browse_cold_probe_active
                .insert(PathBuf::from(format!("/tmp/tonepoet-active-{idx}.flac")));
        }
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        state.schedule_cursor_focused_cold_probe(path.clone(), identity, &tx);
        assert_eq!(state.browse_cold_probe_queue.len(), 1);

        state.complete_browse_cold_probe(Path::new("/tmp/tonepoet-active-0.flac"), &tx);

        assert!(state.browse_cold_probe_active.contains(&path));
        assert!(state.probe_pending.contains(&path));
        assert!(state.browse_cold_probe_queue.is_empty());
    }

    #[tokio::test]
    async fn cold_probe_miss_is_debounced() {
        let (_dir, path, entry, _identity, _mtime) = probe_file_fixture("cold.flac", b"not really flac");
        let db = crate::db::Database::open_memory().expect("db");
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let mut state = BrowseState::new();
        state.entries = vec![entry];
        state.selected_index = 0;

        state.probe_current_with_db(&tx, Some(&db));

        assert_eq!(state.probe_debounce.as_ref().map(|d| d.path.as_path()), Some(path.as_path()));
        assert!(state.probe_pending.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn in_memory_probe_hit_is_immediate() {
        let (_dir, path, entry, identity, _mtime) = probe_file_fixture("hit.flac", b"cached");
        let db = crate::db::Database::open_memory().expect("db");
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let mut state = BrowseState::new();
        state.entries = vec![entry];
        state.selected_index = 0;
        state.probe_cache.insert(
            path.clone(),
            ProbeCacheEntry::hit(identity, Arc::new(test_cached_info(identity.size, "memory"))),
        );

        state.probe_current_with_db(&tx, Some(&db));

        assert!(state.probe_debounce.is_none());
        assert!(state.probe_pending.is_empty());
        assert!(rx.try_recv().is_err());
        assert_eq!(
            state.current_cached_info().and_then(|info| info.metadata.title.as_deref()),
            Some("memory")
        );
    }

    #[tokio::test]
    async fn sqlite_probe_hit_is_immediate_and_enrichment_is_worker_side() {
        let (_dir, path, entry, identity, mtime) = probe_file_fixture("sqlite.flac", b"cached db");
        let db = crate::db::Database::open_memory().expect("db");
        let info = test_cached_info(identity.size, "sqlite");
        let row = crate::db::CachedProbeRow::from_cached_info(&info);
        db.store_probe(&path.display().to_string(), mtime, identity.size, &row)
            .expect("store probe");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = BrowseState::new();
        state.entries = vec![entry];
        state.selected_index = 0;

        state.probe_current_with_db(&tx, Some(&db));

        assert!(state.probe_debounce.is_none());
        assert!(state.probe_pending.contains(&path));
        assert_eq!(
            state.current_cached_info().and_then(|info| info.metadata.title.as_deref()),
            Some("sqlite")
        );
    }

    #[tokio::test]
    async fn cursor_movement_replaces_stale_probe_debounce() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("first.flac");
        let second = dir.path().join("second.flac");
        std::fs::write(&first, b"first").expect("first");
        std::fs::write(&second, b"second").expect("second");
        let first_meta = std::fs::metadata(&first).expect("first metadata");
        let second_meta = std::fs::metadata(&second).expect("second metadata");
        let db = crate::db::Database::open_memory().expect("db");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = BrowseState::new();
        state.entries = vec![
            BrowseEntry::new(
                first.clone(),
                "first.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                first_meta.len(),
                first_meta.modified().ok(),
            ),
            BrowseEntry::new(
                second.clone(),
                "second.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                second_meta.len(),
                second_meta.modified().ok(),
            ),
        ];

        state.selected_index = 0;
        state.probe_current_with_db(&tx, Some(&db));
        assert_eq!(state.probe_debounce.as_ref().map(|d| d.path.as_path()), Some(first.as_path()));

        state.selected_index = 1;
        state.probe_current_with_db(&tx, Some(&db));
        assert_eq!(state.probe_debounce.as_ref().map(|d| d.path.as_path()), Some(second.as_path()));
        assert!(!state.probe_pending.contains(&first));
    }

    #[tokio::test]
    async fn stale_cursor_debounce_does_not_fire_after_selection_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("first-stale.flac");
        let second = dir.path().join("second-current.flac");
        std::fs::write(&first, b"first").expect("first");
        std::fs::write(&second, b"second").expect("second");
        let first_meta = std::fs::metadata(&first).expect("first metadata");
        let second_meta = std::fs::metadata(&second).expect("second metadata");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = BrowseState::new();
        state.entries = vec![
            BrowseEntry::new(
                first.clone(),
                "first-stale.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                first_meta.len(),
                first_meta.modified().ok(),
            ),
            BrowseEntry::new(
                second,
                "second-current.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                second_meta.len(),
                second_meta.modified().ok(),
            ),
        ];
        state.selected_index = 1;
        state.probe_debounce = Some(BrowseProbeDebounce {
            path: first.clone(),
            deadline: std::time::Instant::now() - Duration::from_millis(1),
        });

        state.check_probe_debounce(&tx);

        assert!(state.probe_debounce.is_none());
        assert!(!state.probe_pending.contains(&first));
    }

    #[test]
    fn directory_navigation_clears_probe_debounce_and_cold_probe_queue() {
        let mut state = BrowseState::new();
        let stale = PathBuf::from("/tmp/stale.flac");
        state.probe_debounce = Some(BrowseProbeDebounce {
            path: stale.clone(),
            deadline: std::time::Instant::now() + Duration::from_secs(60),
        });
        state.browse_cold_probe_queue.push_back(BrowseColdProbeRequest {
            path: stale,
            identity: ProbeCacheIdentity { modified: None, size: 1 },
            scan_generation: state.scan_generation,
            cursor_focused: true,
        });

        state.reset_nav_state();

        assert!(state.probe_debounce.is_none());
        assert!(state.browse_cold_probe_queue.is_empty());
    }

    #[test]
    fn batch_warm_merge_ignores_rows_with_stale_listing_identity() {
        let path = PathBuf::from("/tmp/tonepoet-warm-stale.flac");
        let fresh = ProbeCacheIdentity { modified: None, size: 200 };
        let stale = ProbeCacheIdentity { modified: None, size: 100 };
        let mut state = BrowseState::new();
        state.entries = vec![BrowseEntry::new(
            path.clone(),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            fresh.size,
            fresh.modified,
        )];

        let merged = state.merge_probe_cache_warm_rows(vec![ProbeCacheWarmRow {
            path,
            identity: stale,
            info: test_cached_info(stale.size, "stale"),
        }]);

        assert_eq!(merged, 0);
        assert!(state.probe_cache.is_empty());
    }

    #[test]
    fn warmed_rows_do_not_override_fresher_in_memory_rows() {
        let path = PathBuf::from("/tmp/tonepoet-warm-fresh.flac");
        let fresh = ProbeCacheIdentity { modified: None, size: 200 };
        let stale = ProbeCacheIdentity { modified: None, size: 100 };
        let mut state = BrowseState::new();
        state.entries = vec![BrowseEntry::new(
            path.clone(),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            fresh.size,
            fresh.modified,
        )];
        state.selected_index = 0;
        state.probe_cache.insert(
            path.clone(),
            ProbeCacheEntry::hit(fresh, Arc::new(test_cached_info(fresh.size, "fresh"))),
        );

        let merged = state.merge_probe_cache_warm_rows(vec![ProbeCacheWarmRow {
            path,
            identity: stale,
            info: test_cached_info(stale.size, "stale"),
        }]);

        assert_eq!(merged, 0);
        assert_eq!(
            state.current_cached_info().and_then(|info| info.metadata.title.as_deref()),
            Some("fresh")
        );
    }

    #[test]
    fn probe_cache_update_flags_coalesce_until_taken() {
        let mut state = BrowseState::new();

        state.mark_probe_cache_update_pending(false);
        state.mark_probe_cache_update_pending(false);
        state.mark_probe_cache_update_pending(true);

        assert_eq!(state.take_probe_cache_deferred_work(), (true, true));
        assert_eq!(state.take_probe_cache_deferred_work(), (false, false));
    }

    #[test]
    fn warm_cache_queue_merges_only_bounded_rows_per_frame() {
        let mut state = BrowseState::new();
        let count = PROBE_CACHE_WARM_MERGE_MAX_PER_FRAME + 3;
        let mut rows = Vec::new();
        let mut entries = Vec::new();

        for idx in 0..count {
            let path = PathBuf::from(format!("/tmp/tonepoet-bounded-warm-{idx}.flac"));
            let identity = ProbeCacheIdentity { modified: None, size: idx as u64 + 1 };
            entries.push(BrowseEntry::new(
                path.clone(),
                format!("bounded-warm-{idx}.flac"),
                EntryKind::AudioFile(AudioFormat::Flac),
                identity.size,
                identity.modified,
            ));
            rows.push(ProbeCacheWarmRow {
                path,
                identity,
                info: test_cached_info(identity.size, &format!("warm-{idx}")),
            });
        }

        state.entries = entries;
        let generation = state.scan_generation;
        let directory = state.current_dir.clone();
        assert_eq!(state.enqueue_probe_cache_warm_rows(generation, directory, rows), count);

        let (merged, has_more) = state.drain_probe_cache_warm_rows_for_frame();
        assert_eq!(merged, PROBE_CACHE_WARM_MERGE_MAX_PER_FRAME);
        assert!(has_more);
        assert_eq!(state.probe_cache.len(), PROBE_CACHE_WARM_MERGE_MAX_PER_FRAME);
        assert!(state.take_browse_deferred_work().probe_backed_resort_needed);

        let (merged, has_more) = state.drain_probe_cache_warm_rows_for_frame();
        assert_eq!(merged, 3);
        assert!(!has_more);
        assert_eq!(state.probe_cache.len(), count);
    }

    #[test]
    fn warm_cache_queue_drops_stale_generation_before_merge() {
        let mut state = BrowseState::new();
        let path = PathBuf::from("/tmp/tonepoet-stale-generation-warm.flac");
        let identity = ProbeCacheIdentity { modified: None, size: 1 };
        state.entries = vec![BrowseEntry::new(
            path.clone(),
            "stale-generation-warm.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            identity.size,
            identity.modified,
        )];

        let generation = state.scan_generation;
        let directory = state.current_dir.clone();
        assert_eq!(
            state.enqueue_probe_cache_warm_rows(
                generation,
                directory,
                vec![ProbeCacheWarmRow {
                    path,
                    identity,
                    info: test_cached_info(identity.size, "stale"),
                }],
            ),
            1,
        );

        state.scan_generation = state.scan_generation.wrapping_add(1);
        let (merged, has_more) = state.drain_probe_cache_warm_rows_for_frame();
        assert_eq!(merged, 0);
        assert!(!has_more);
        assert!(state.probe_cache.is_empty());
    }

    #[test]
    fn browse_deferred_work_flags_coalesce_until_flush() {
        let mut state = BrowseState::new();

        state.mark_probe_cache_update_pending(false);
        state.mark_probe_cache_update_pending(true);
        state.mark_visible_entries_changed_pending();
        state.mark_classification_changed_pending();

        let work = state.take_browse_deferred_work();
        assert!(work.probe_backed_resort_needed);
        assert!(work.visible_entries_changed);
        assert!(work.info_pane_changed);
        assert!(work.classification_changed);
        assert!(!state.take_browse_deferred_work().has_expensive_work());
    }

    #[test]
    fn search_fuzzy_threshold_rejects_garbage_subsequence_matches() {
        use fuzzy_matcher::skim::SkimMatcherV2;
        use fuzzy_matcher::FuzzyMatcher;

        let matcher = SkimMatcherV2::default();
        let query = "epping";
        let genuine = matcher
            .fuzzy_match("the battle of epping forest.flac", query)
            .expect("genuine title should fuzzy match");
        let garbage = matcher
            .fuzzy_match(
                "genesis - spot the pigeon ep (1977) [flac] {uk  virgin cdf 40} [nimbus]",
                query,
            )
            .expect("garbage subsequence should still produce a raw skim score");

        assert!(
            search_fuzzy_score_passes_threshold(genuine, search_fuzzy_min_score(query)),
            "genuine match score {genuine} should pass threshold {}",
            search_fuzzy_min_score(query)
        );
        assert!(
            !search_fuzzy_score_passes_threshold(garbage, search_fuzzy_min_score(query)),
            "garbage subsequence score {garbage} should fail threshold {}",
            search_fuzzy_min_score(query)
        );
    }

    #[test]
    fn search_fuzzy_threshold_keeps_short_query_matches_useful() {
        use fuzzy_matcher::skim::SkimMatcherV2;
        use fuzzy_matcher::FuzzyMatcher;

        let matcher = SkimMatcherV2::default();
        let query = "ab";
        let score = matcher
            .fuzzy_match("abacab", query)
            .expect("short direct subsequence should fuzzy match");

        assert!(
            search_fuzzy_score_passes_threshold(score, search_fuzzy_min_score(query)),
            "short query score {score} should pass threshold {}",
            search_fuzzy_min_score(query)
        );
    }

    /// Build an `all_files` list with one Archive entry pointing at
    /// `path`, mtime taken from the file. Other BrowseState fields
    /// stay at their defaults via `BrowseState::new()` then we
    /// overwrite the relevant bits.
    fn make_browse_with_iso(path: &std::path::Path) -> BrowseState {
        let mut state = BrowseState::new();
        state.all_files.clear();
        state.sacd_classify_cache.clear();
        state.dvda_iso_classify_cache.clear();
        state.dvdv_iso_classify_cache.clear();
        state.bluray_iso_classify_cache.clear();
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
    fn path_validation_acceptance_rejects_superseded_generation() {
        let mut state = BrowseState::new();
        let origin = std::path::PathBuf::from("/tmp/browse-origin");
        state.current_dir = origin.clone();

        let first_generation = state.next_path_validation_generation();
        assert!(state.is_current_path_validation(first_generation, &origin));

        let second_generation = state.next_path_validation_generation();
        assert_ne!(first_generation, second_generation);
        assert!(!state.is_current_path_validation(first_generation, &origin));
        assert!(state.is_current_path_validation(second_generation, &origin));
    }

    #[test]
    fn path_validation_acceptance_rejects_changed_origin_directory() {
        let mut state = BrowseState::new();
        let origin = std::path::PathBuf::from("/tmp/browse-origin");
        let other = std::path::PathBuf::from("/tmp/browse-other");
        state.current_dir = origin.clone();

        let generation = state.next_path_validation_generation();
        assert!(state.is_current_path_validation(generation, &origin));

        state.current_dir = other.clone();
        assert!(!state.is_current_path_validation(generation, &origin));
        assert!(same_path(&state.current_dir, &other));
    }


    #[test]
    fn path_validation_acceptance_rejects_reopened_path_editor() {
        let mut state = BrowseState::new();
        let origin = std::path::PathBuf::from("/tmp/browse-origin");
        state.current_dir = origin.clone();

        let generation = state.next_path_validation_generation();
        assert!(state.is_current_path_validation(generation, &origin));

        state.open_path_input();
        assert!(state.path_input.is_some());
        assert!(!state.is_current_path_validation(generation, &origin));
    }

    #[test]
    fn path_validation_acceptance_rejects_active_path_editor_even_with_matching_generation() {
        let mut state = BrowseState::new();
        let origin = std::path::PathBuf::from("/tmp/browse-origin");
        state.current_dir = origin.clone();

        let generation = state.next_path_validation_generation();
        state.path_input = Some(TextInputState::new("/tmp/other".to_string()));

        assert!(!state.is_current_path_validation(generation, &origin));
    }

    fn unique_test_audio_path(name: &str) -> std::path::PathBuf {
        let unique = format!(
            "tonepoet-browse-{name}-{}-{}.flac",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        );
        std::env::temp_dir().join(unique)
    }

    #[test]
    fn local_tag_search_cache_is_fingerprint_validated() {
        let path = unique_test_audio_path("fingerprint-cache");
        std::fs::write(&path, b"not real audio").expect("write test file");
        let fingerprint = TagCacheFingerprint::for_path(&path).expect("fingerprint");

        let mut cache = std::collections::HashMap::new();
        cache.insert(
            path.clone(),
            CachedTagSearchString {
                fingerprint,
                tag_string: "old artist old album".to_string(),
            },
        );

        assert_eq!(build_tag_search_string_cached(&path, &mut cache), "old artist old album");

        // Changing the file size simulates a metadata rewrite and must make the
        // path-only cached tag text unusable even while the search panel stays open.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open for append");
        f.write_all(b"changed").expect("append");
        drop(f);

        assert_eq!(build_tag_search_string_cached(&path, &mut cache), "");
        assert_ne!(
            cache.get(&path).map(|cached| cached.tag_string.as_str()),
            Some("old artist old album")
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn metadata_write_invalidation_prevents_active_local_tag_search_from_using_stale_text() {
        let path = unique_test_audio_path("metadata-invalidation");
        std::fs::write(&path, b"not real audio").expect("write test file");
        let metadata = std::fs::metadata(&path).expect("metadata");
        let fingerprint = TagCacheFingerprint::for_path(&path).expect("fingerprint");

        let mut state = BrowseState::new();
        state.search.active = true;
        state.search.mode = SearchMode::Tags;
        state.all_dirs.clear();
        state.parent_entry = None;
        state.all_files = vec![BrowseEntry::new(
            path.clone(),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            metadata.len(),
            metadata.modified().ok(),
        )];
        state.search.tag_cache.insert(
            path.clone(),
            CachedTagSearchString {
                fingerprint,
                tag_string: "staleartist stalealbum".to_string(),
            },
        );

        state.execute_search_local("staleartist", true, false, FormatFilter::Off, SearchMode::Tags);
        assert_eq!(state.entries.len(), 1, "cached old tag text should initially match");

        state.invalidate_search_tag_cache_for_metadata_path(&path);
        state.execute_search_local("staleartist", true, false, FormatFilter::Off, SearchMode::Tags);
        assert!(
            state.entries.is_empty(),
            "after metadata-write invalidation, active local tag search must not reuse stale tag text"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn navigation_clears_local_tag_search_cache() {
        let path = unique_test_audio_path("nav-clear");
        std::fs::write(&path, b"not real audio").expect("write test file");
        let fingerprint = TagCacheFingerprint::for_path(&path).expect("fingerprint");

        let mut state = BrowseState::new();
        state.search.tag_cache.insert(
            path.clone(),
            CachedTagSearchString {
                fingerprint,
                tag_string: "old artist".to_string(),
            },
        );

        state.reset_nav_state();
        assert!(state.search.tag_cache.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn archive_staged_metadata_write_invalidates_synthetic_archive_entry_tag_cache() {
        let archive_path = std::path::PathBuf::from("/tmp/test-archive.zip");
        let staging_dir = std::env::temp_dir().join(format!(
            "tonepoet-staging-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let staged_file = staging_dir.join("Disc 1").join("01.flac");
        std::fs::create_dir_all(staged_file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&staged_file, b"not real audio").expect("write staged file");
        let fingerprint = TagCacheFingerprint::for_path(&staged_file).expect("fingerprint");
        let synthetic = archive_path.join("Disc 1").join("01.flac");

        let mut state = BrowseState::new();
        state.archive = Some(ArchiveBrowseState {
            listing: crate::tui::archive_listing::ArchiveListing {
                archive_path: archive_path.clone(),
                format: "zip".to_string(),
                physical_size: 0,
                entries: Vec::new(),
            },
            inner_path: String::new(),
            password: None,
            staging: Some(ArchiveStagingSession::new(
                staging_dir.clone(),
                archive_path.clone(),
                0,
                0,
                0,
            )),
        });
        state.search.tag_cache.insert(
            synthetic.clone(),
            CachedTagSearchString {
                fingerprint,
                tag_string: "stale archive artist".to_string(),
            },
        );

        state.invalidate_search_tag_cache_for_metadata_path(&staged_file);
        assert!(!state.search.tag_cache.contains_key(&synthetic));

        let _ = std::fs::remove_dir_all(&staging_dir);
    }


    #[test]
    fn show_hidden_toggle_preserves_active_local_search_results() {
        let visible_path = std::path::PathBuf::from("/tmp/needle-visible.flac");
        let hidden_path = std::path::PathBuf::from("/tmp/.needle-hidden.flac");
        let other_path = std::path::PathBuf::from("/tmp/other.flac");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        let mut state = BrowseState::new();
        state.show_hidden = false;
        state.search.active = true;
        state.search.recursive = false;
        state.search.mode = SearchMode::Filename;
        state.search.audio_only = false;
        state.search.input = TextInputState::new("needle".to_string());
        state.parent_entry = None;
        state.all_dirs.clear();
        state.all_files = vec![
            BrowseEntry::new(
                visible_path.clone(),
                "needle-visible.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                0,
                None,
            ),
            BrowseEntry::new(
                hidden_path.clone(),
                ".needle-hidden.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                0,
                None,
            ),
            BrowseEntry::new(
                other_path,
                "other.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                0,
                None,
            ),
        ];

        state.execute_search(Some(&tx));
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].path, visible_path);

        state.toggle_hidden_with_search(Some(&tx));
        let result_paths = state
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<std::collections::HashSet<_>>();

        assert!(state.search.active);
        assert!(state.show_hidden);
        assert_eq!(result_paths.len(), 2);
        assert!(result_paths.contains(&visible_path));
        assert!(result_paths.contains(&hidden_path));
        assert!(state
            .entries
            .iter()
            .all(|entry| entry.name_lower.contains("needle")));
    }

    #[test]
    fn active_local_search_filter_change_keeps_query_constrained_results() {
        let flac_path = std::path::PathBuf::from("/tmp/needle.flac");
        let mp3_path = std::path::PathBuf::from("/tmp/needle.mp3");
        let other_path = std::path::PathBuf::from("/tmp/other.flac");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        let mut state = BrowseState::new();
        state.search.active = true;
        state.search.recursive = false;
        state.search.mode = SearchMode::Filename;
        state.search.audio_only = false;
        state.search.input = TextInputState::new("needle".to_string());
        state.parent_entry = None;
        state.all_dirs.clear();
        state.all_files = vec![
            BrowseEntry::new(
                flac_path.clone(),
                "needle.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                0,
                None,
            ),
            BrowseEntry::new(
                mp3_path,
                "needle.mp3".to_string(),
                EntryKind::AudioFile(AudioFormat::Mp3),
                0,
                None,
            ),
            BrowseEntry::new(
                other_path,
                "other.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                0,
                None,
            ),
        ];

        state.execute_search(Some(&tx));
        assert_eq!(state.entries.len(), 2);

        state.set_format_filter_with_search(FormatFilter::Only(AudioFormat::Flac), Some(&tx));

        assert!(state.search.active);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].path, flac_path);
        assert!(state
            .entries
            .iter()
            .all(|entry| entry.name_lower.contains("needle")));
    }

    #[test]
    fn active_local_search_default_sort_change_does_not_restore_directory_listing() {
        let needle_a = std::path::PathBuf::from("/tmp/needle-a.flac");
        let needle_b = std::path::PathBuf::from("/tmp/needle-b.flac");
        let other = std::path::PathBuf::from("/tmp/other.flac");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        let mut state = BrowseState::new();
        state.search.active = true;
        state.search.recursive = false;
        state.search.mode = SearchMode::Filename;
        state.search.audio_only = false;
        state.search.input = TextInputState::new("needle".to_string());
        state.parent_entry = None;
        state.all_dirs.clear();
        state.all_files = vec![
            BrowseEntry::new(
                needle_a,
                "needle-a.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                1,
                None,
            ),
            BrowseEntry::new(
                needle_b,
                "needle-b.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                2,
                None,
            ),
            BrowseEntry::new(
                other,
                "other.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                3,
                None,
            ),
        ];

        state.execute_search(Some(&tx));
        assert_eq!(state.entries.len(), 2);

        state.set_default_sort_with_search(SortBy::Size, SortDir::Asc, true, Some(&tx));

        assert!(state.search.active);
        assert_eq!(state.entries.len(), 2);
        assert!(state
            .entries
            .iter()
            .all(|entry| entry.name_lower.contains("needle")));
        assert_eq!(state.default_sort_by, SortBy::Size);
        assert_eq!(state.default_sort_dir, SortDir::Asc);
    }

    #[test]
    fn active_local_search_restore_defaults_does_not_restore_directory_listing() {
        let needle = std::path::PathBuf::from("/tmp/needle.flac");
        let other = std::path::PathBuf::from("/tmp/other.flac");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        let mut state = BrowseState::new();
        state.show_hidden = true;
        state.format_filter = FormatFilter::Only(AudioFormat::Flac);
        state.search.active = true;
        state.search.recursive = false;
        state.search.mode = SearchMode::Filename;
        state.search.audio_only = false;
        state.search.input = TextInputState::new("needle".to_string());
        state.parent_entry = None;
        state.all_dirs.clear();
        state.all_files = vec![
            BrowseEntry::new(
                needle.clone(),
                "needle.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                0,
                None,
            ),
            BrowseEntry::new(
                other,
                "other.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                0,
                None,
            ),
        ];

        state.execute_search(Some(&tx));
        assert_eq!(state.entries.len(), 1);

        state.apply_browsing_config_with_search(&crate::config::BrowsingConfig::default(), Some(&tx));

        assert!(state.search.active);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].path, needle);
        assert!(state
            .entries
            .iter()
            .all(|entry| entry.name_lower.contains("needle")));
        assert_eq!(state.show_hidden, crate::config::BrowsingConfig::default().show_hidden);
    }

    #[test]
    fn hidden_toggle_captured_config_matches_keyboard_and_context_persistence_payload() {
        let mut state = BrowseState::new();
        state.show_hidden = false;
        state.toggle_hidden_with_search(None);

        let captured = state.capture_browsing_config();
        assert!(captured.show_hidden);
    }

    #[tokio::test]
    async fn show_hidden_toggle_replaces_active_recursive_search() {
        let root = std::env::temp_dir().join(format!(
            "tonepoet-recursive-hidden-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("needle-visible.flac"), b"not real audio").expect("write visible");
        std::fs::write(root.join(".needle-hidden.flac"), b"not real audio").expect("write hidden");

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let old_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut state = BrowseState::new();
        state.current_dir = root.clone();
        state.show_hidden = false;
        state.search.active = true;
        state.search.recursive = true;
        state.search.mode = SearchMode::Filename;
        state.search.audio_only = false;
        state.search.input = TextInputState::new("needle".to_string());
        state.search.cancel = Some(old_cancel.clone());
        state.search.searching = true;
        let old_generation = state.search.generation;

        state.toggle_hidden_with_search(Some(&tx));

        assert!(old_cancel.load(std::sync::atomic::Ordering::Relaxed));
        assert!(state.show_hidden);
        assert!(state.search.searching);
        assert!(state.search.cancel.is_some());
        assert!(state.search.generation > old_generation);

        let message = rx.recv().await.expect("recursive search completion");
        match message {
            crate::tui::message::AppMessage::SearchComplete {
                generation,
                root: completed_root,
                query,
                show_hidden,
                results,
                ..
            } => {
                assert_eq!(generation, state.search.generation);
                assert_eq!(completed_root, root);
                assert_eq!(query, "needle");
                assert!(show_hidden);
                assert!(results
                    .iter()
                    .any(|(entry, _)| entry.name == ".needle-hidden.flac"));
            }
            other => panic!("unexpected message: {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn active_local_search_refresh_reapplies_query_after_scan_data_changes() {
        let needle = std::path::PathBuf::from("/tmp/needle-refresh.flac");
        let other = std::path::PathBuf::from("/tmp/other-refresh.flac");

        let mut state = BrowseState::new();
        state.search.active = true;
        state.search.recursive = false;
        state.search.mode = SearchMode::Filename;
        state.search.audio_only = false;
        state.search.input = TextInputState::new("needle".to_string());
        state.parent_entry = None;
        state.all_dirs.clear();
        state.all_files = vec![
            BrowseEntry::new(
                needle.clone(),
                "needle-refresh.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                0,
                None,
            ),
            BrowseEntry::new(
                other.clone(),
                "other-refresh.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                0,
                None,
            ),
        ];
        state.entries = state.all_files.clone();

        state.reapply_after_directory_scan_complete(None);

        assert!(state.search.active);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].path, needle);
        assert!(state
            .entries
            .iter()
            .all(|entry| entry.name_lower.contains("needle")));
    }

    #[tokio::test]
    async fn active_recursive_search_refresh_invalidates_and_restarts_after_scan() {
        let root = std::env::temp_dir().join(format!(
            "tonepoet-recursive-refresh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("needle-refresh.flac"), b"not real audio").expect("write needle");
        std::fs::write(root.join("other-refresh.flac"), b"not real audio").expect("write other");

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let old_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut state = BrowseState::new();
        state.current_dir = root.clone();
        state.set_tx(tx.clone());
        state.search.active = true;
        state.search.recursive = true;
        state.search.mode = SearchMode::Filename;
        state.search.audio_only = false;
        state.search.input = TextInputState::new("needle".to_string());
        state.search.cancel = Some(old_cancel.clone());
        state.search.searching = true;
        let old_generation = state.search.generation;

        state.refresh();

        assert!(old_cancel.load(std::sync::atomic::Ordering::Relaxed));
        assert!(state.search.active);
        assert!(state.search.recursive);
        assert!(state.search.generation > old_generation);
        assert!(state.pending_scan_generation().is_some());

        state.parent_entry = None;
        state.all_dirs.clear();
        state.all_files = vec![
            BrowseEntry::new(
                root.join("needle-refresh.flac"),
                "needle-refresh.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                0,
                None,
            ),
            BrowseEntry::new(
                root.join("other-refresh.flac"),
                "other-refresh.flac".to_string(),
                EntryKind::AudioFile(AudioFormat::Flac),
                0,
                None,
            ),
        ];

        state.reapply_after_directory_scan_complete(Some(&tx));
        assert!(state.search.searching);

        let mut saw_replacement = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let Some(message) = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("message timeout")
            else {
                break;
            };
            if let crate::tui::message::AppMessage::SearchComplete {
                root: completed_root,
                query,
                show_hidden,
                results,
                ..
            } = message
            {
                if completed_root == root && query == "needle" {
                    assert_eq!(show_hidden, state.show_hidden);
                    assert!(results
                        .iter()
                        .all(|(entry, _)| entry.name_lower.contains("needle")));
                    saw_replacement = true;
                    break;
                }
            }
        }
        assert!(saw_replacement, "refresh should launch a replacement recursive search");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn opening_search_when_active_focuses_without_blank_query_or_stale_results() {
        let result_path = std::path::PathBuf::from("/tmp/needle-existing.flac");
        let mut state = BrowseState::new();
        state.search.active = true;
        state.search.focus = SearchFocus::Results;
        state.search.input = TextInputState::new("needle".to_string());
        state.entries = vec![BrowseEntry::new(
            result_path.clone(),
            "needle-existing.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            0,
            None,
        )];

        state.open_search();

        assert!(state.search.active);
        assert_eq!(state.search.focus, SearchFocus::Input);
        assert_eq!(state.search.input.text, "needle");
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].path, result_path);
    }

    #[test]
    fn directory_navigation_closes_search_instead_of_carrying_stale_panel() {
        let root = std::env::temp_dir().join(format!(
            "tonepoet-search-nav-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let child = root.join("child");
        std::fs::create_dir_all(&child).expect("mkdir child");
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut state = BrowseState::new();
        state.current_dir = root.clone();
        state.search.active = true;
        state.search.input = TextInputState::new("needle".to_string());
        state.search.cancel = Some(cancel.clone());
        state.search.searching = true;

        state.navigate_to(child.clone());

        assert_eq!(state.current_dir, child);
        assert!(!state.search.active);
        assert!(state.search.input.text.is_empty());
        assert!(state.search.cancel.is_none());
        assert!(!state.search.searching);
        assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn archive_probe_selector_rejects_unsafe_path_structure() {
        for inner in ["", "/abs.flac", "../escape.flac", "dir/../escape.flac"] {
            assert!(
                validate_archive_entry_probe_selector(inner).is_err(),
                "unsafe archive entry path should be rejected: {inner}"
            );
        }
    }

    #[test]
    fn archive_probe_selector_rejects_switch_and_listfile_components() {
        for inner in [
            "-track.flac",
            "dir/-track.flac",
            "@listfile.flac",
            "dir/@listfile.flac",
        ] {
            assert!(
                validate_archive_entry_probe_selector(inner).is_err(),
                "selector should be rejected: {inner}"
            );
        }
    }

    #[test]
    fn archive_probe_selector_uses_full_archive_fallback_for_wildcard_like_names() {
        for inner in [
            "disc/*.flac",
            "disc/track?.flac",
            "Disc 1/01 - Song [Live].flac",
        ] {
            assert_eq!(
                validate_archive_entry_probe_selector(inner),
                Ok(ArchiveEntryProbeExtraction::FullArchiveFallback),
                "wildcard-looking archive entry should use full extraction fallback: {inner}"
            );
        }
    }

    #[test]
    fn archive_probe_selector_accepts_normal_archive_paths() {
        assert_eq!(
            validate_archive_entry_probe_selector("Disc 1/01 - Song.flac"),
            Ok(ArchiveEntryProbeExtraction::SingleMember)
        );
    }

    fn make_valid_bluray_layout(
        root: &std::path::Path,
        bdmv_name: &str,
        index_name: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let bdmv = root.join(bdmv_name);
        let playlist = bdmv.join("PLAYLIST");
        let stream = bdmv.join("STREAM");
        std::fs::create_dir_all(&playlist).expect("create PLAYLIST");
        std::fs::create_dir_all(&stream).expect("create STREAM");
        let index = bdmv.join(index_name);
        std::fs::write(&index, b"index").expect("write index");
        std::fs::write(bdmv.join("MovieObject.bdmv"), b"movie object")
            .expect("write MovieObject");
        std::fs::write(playlist.join("00000.mpls"), b"playlist").expect("write playlist");
        std::fs::write(stream.join("00000.m2ts"), b"stream").expect("write stream");
        (bdmv, index)
    }

    fn marker_len(fingerprint: &ClassificationFingerprint, label: &'static str) -> Option<u64> {
        fingerprint
            .markers
            .iter()
            .find(|marker| marker.label == label)
            .and_then(|marker| marker.len)
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
    fn sacd_negative_cache_is_identity_bound_by_size_and_mtime() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("data.iso");
        let total = (crate::tui::sacd::MASTER_TOC_LSNS[0] + 1) * crate::tui::sacd::SECTOR_SIZE;
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(total).unwrap();
        drop(f);

        let mut state = make_browse_with_iso(&path);
        state.upgrade_iso_kinds();
        let first = state
            .sacd_classify_cache
            .get(&path)
            .expect("first negative")
            .0
            .clone();
        assert_eq!(state.sacd_classify_cache.get(&path).map(|(_, v)| *v), Some(false));

        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(total + crate::tui::sacd::SECTOR_SIZE).unwrap();
        drop(f);
        let meta = std::fs::metadata(&path).expect("metadata after resize");
        state.all_files[0].size = meta.len();
        state.all_files[0].modified = meta.modified().ok();
        state.all_files[0].kind = EntryKind::Archive;

        state.upgrade_iso_kinds();
        let second = state
            .sacd_classify_cache
            .get(&path)
            .expect("second negative")
            .0
            .clone();
        assert_ne!(first, second, "same-path negative must be invalidated by identity change");
    }

    #[test]
    fn probe_failures_cache_only_deterministic_not_audio_negatives() {
        let mut state = BrowseState::new();
        let path = PathBuf::from("/tmp/not-audio.flac");
        let identity = ProbeCacheIdentity { modified: None, size: 123 };

        state.remember_probe_failure_for_identity(
            path.clone(),
            identity,
            "No audio stream found in '/tmp/not-audio.flac'",
        );
        assert!(state.has_probe_cache_entry_for_identity(&path, identity));

        let transient = PathBuf::from("/tmp/locked.flac");
        state.remember_probe_failure_for_identity(
            transient.clone(),
            identity,
            "Failed to open '/tmp/locked.flac': Permission denied",
        );
        assert!(!state.has_probe_cache_entry_for_identity(&transient, identity));
        assert!(state.has_recent_transient_probe_failure(&transient, identity));

        state.insert_probe_for_identity(
            transient.clone(),
            identity,
            Some(Arc::new(test_cached_info(identity.size, "later success"))),
        );
        assert!(state.valid_probe_arc_for_identity(&transient, identity).is_some());
        assert!(!state.has_recent_transient_probe_failure(&transient, identity));
    }

    #[test]
    fn scan_worker_applies_cached_iso_classification_before_publication() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("disc.iso");
        std::fs::write(&path, b"not really an iso").unwrap();
        let meta = std::fs::metadata(&path).expect("metadata");
        let fingerprint = ClassificationFingerprint {
            len: meta.len(),
            modified: meta.modified().ok(),
            markers: Vec::new(),
        };
        let mut snapshot = BrowseClassificationCacheSnapshot::default();
        snapshot.sacd_iso.insert(path.clone(), (fingerprint, true));
        let cancel = std::sync::atomic::AtomicBool::new(false);

        let (_parent, _dirs, files, updates) =
            scan_directory_blocking(td.path(), &cancel, &snapshot).expect("scan");

        let entry = files.iter().find(|entry| entry.path == path).expect("iso entry");
        assert!(matches!(entry.kind, EntryKind::SacdIso));
        assert!(updates.sacd_iso.is_empty(), "cache hit should not emit a cold update");
    }

    #[test]
    fn scan_worker_reports_identity_bound_negative_classification_updates() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("data.iso");
        std::fs::write(&path, b"not really an iso").unwrap();
        let snapshot = BrowseClassificationCacheSnapshot::default();
        let cancel = std::sync::atomic::AtomicBool::new(false);

        let (_parent, _dirs, files, updates) =
            scan_directory_blocking(td.path(), &cancel, &snapshot).expect("scan");

        let entry = files.iter().find(|entry| entry.path == path).expect("iso entry");
        assert!(matches!(entry.kind, EntryKind::Archive));
        assert!(updates
            .sacd_iso
            .iter()
            .any(|(update_path, _fingerprint, verdict)| update_path == &path && !*verdict));
    }

    #[test]
    fn is_probeable_covers_audio_files_and_disc_sources() {
        let entry_with_kind = |kind: EntryKind| {
            BrowseEntry::new(
                std::path::PathBuf::from("/tmp/x"),
                "x".to_string(),
                kind,
                0,
                None,
            )
        };

        // Probeable: audio files (any format) and supported disc sources.
        assert!(entry_with_kind(EntryKind::AudioFile(AudioFormat::Flac)).is_probeable());
        assert!(entry_with_kind(EntryKind::AudioFile(AudioFormat::Wav)).is_probeable());
        assert!(entry_with_kind(EntryKind::AudioFile(AudioFormat::Mp3)).is_probeable());
        assert!(entry_with_kind(EntryKind::SacdIso).is_probeable());
        assert!(entry_with_kind(EntryKind::DvdAudioIso).is_probeable());
        assert!(entry_with_kind(EntryKind::DvdAudioDir).is_probeable());
        assert!(entry_with_kind(EntryKind::DvdVideoIso).is_probeable());
        assert!(entry_with_kind(EntryKind::DvdVideoDir).is_probeable());
        assert!(entry_with_kind(EntryKind::BlurayIso).is_probeable());
        assert!(entry_with_kind(EntryKind::BlurayDir).is_probeable());

        // Not probeable: directories, archives (data ISOs included here),
        // other files. The probe pipeline produces no useful output for
        // these and the InfoPane has no SourceMetadata to render.
        assert!(!entry_with_kind(EntryKind::Directory).is_probeable());
        assert!(!entry_with_kind(EntryKind::Archive).is_probeable());
        assert!(!entry_with_kind(EntryKind::OtherFile).is_probeable());
        assert!(!entry_with_kind(EntryKind::ParentDir).is_probeable());
    }

    #[test]
    fn audio_only_filter_keeps_bluray_sources_visible() {
        assert!(FormatFilter::AudioOnly.allows(&EntryKind::BlurayIso));
        assert!(FormatFilter::AudioOnly.allows(&EntryKind::BlurayDir));

        let bluray_dir = BrowseEntry::new(
            std::path::PathBuf::from("/tmp/movie"),
            "movie".to_string(),
            EntryKind::BlurayDir,
            0,
            None,
        );
        assert!(FormatFilter::AudioOnly.allows_entry(&bluray_dir));
        assert!(is_audio_filter_visible_entry(&bluray_dir));
    }

    #[test]
    fn blu_ray_iso_classification_runs_after_dvd_audio_and_dvd_video() {
        let source = include_str!("browse.rs");
        let start = source
            .find("pub(super) fn upgrade_iso_kinds")
            .expect("upgrade_iso_kinds source");
        let tail = &source[start..];
        let end = tail
            .find("/// Classify scanned directory entries")
            .expect("end of upgrade_iso_kinds source");
        let upgrade_iso_kinds = &tail[..end];

        let sacd = upgrade_iso_kinds.find("is_sacd_iso").expect("SACD classifier");
        let dvda = upgrade_iso_kinds.find("is_dvda_iso").expect("DVD-Audio classifier");
        let dvdv = upgrade_iso_kinds.find("is_dvdv_iso").expect("DVD-Video classifier");
        let bluray = upgrade_iso_kinds.find("is_bluray_iso").expect("Blu-ray classifier");

        assert!(sacd < dvda, "SACD must keep first priority");
        assert!(dvda < dvdv, "DVD-Audio must win DVD-Audio/DVD-Video hybrids");
        assert!(dvdv < bluray, "Blu-ray ISO classification must run after DVD-Video");
    }

    #[test]
    fn classify_bluray_directory_entry_marks_valid_root() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path().join("movie");
        make_valid_bluray_layout(&root, "BDMV", "index.bdmv");

        let meta = std::fs::metadata(&root).expect("metadata");
        let mut entry = BrowseEntry::new(
            root.clone(),
            "movie".to_string(),
            EntryKind::Directory,
            meta.len(),
            meta.modified().ok(),
        );
        let mut cache = std::collections::HashMap::new();

        classify_bluray_directory_entry(&mut entry, &mut cache);

        assert!(matches!(entry.kind, EntryKind::BlurayDir));
        assert_eq!(cache.get(&root).map(|(_, verdict)| *verdict), Some(true));
    }

    #[test]
    fn classify_bluray_directory_cache_invalidates_negative_to_positive() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path().join("movie");
        let bdmv = root.join("BDMV");
        std::fs::create_dir_all(&bdmv).expect("create partial BDMV");

        let meta = std::fs::metadata(&root).expect("metadata");
        let mut entry = BrowseEntry::new(
            root.clone(),
            "movie".to_string(),
            EntryKind::Directory,
            meta.len(),
            meta.modified().ok(),
        );
        let mut cache = std::collections::HashMap::new();

        classify_bluray_directory_entry(&mut entry, &mut cache);
        assert!(matches!(entry.kind, EntryKind::Directory));
        let first = cache
            .get(&root)
            .map(|(fingerprint, verdict)| (fingerprint.clone(), *verdict))
            .expect("cached negative verdict");
        assert!(!first.1);

        make_valid_bluray_layout(&root, "BDMV", "index.bdmv");
        classify_bluray_directory_entry(&mut entry, &mut cache);
        let second = cache
            .get(&root)
            .map(|(fingerprint, verdict)| (fingerprint.clone(), *verdict))
            .expect("cached positive verdict");

        assert!(second.1);
        assert_ne!(first.0, second.0);
        assert!(matches!(entry.kind, EntryKind::BlurayDir));
    }

    #[test]
    fn classify_bluray_directory_cache_invalidates_negative_to_positive_with_index_unchanged() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path().join("movie");
        let bdmv = root.join("BDMV");
        std::fs::create_dir_all(&bdmv).expect("create BDMV");
        let index = bdmv.join("index.bdmv");
        std::fs::write(&index, b"index").expect("write index before layout is complete");

        let meta = std::fs::metadata(&root).expect("metadata");
        let mut entry = BrowseEntry::new(
            root.clone(),
            "movie".to_string(),
            EntryKind::Directory,
            meta.len(),
            meta.modified().ok(),
        );
        let mut cache = std::collections::HashMap::new();

        classify_bluray_directory_entry(&mut entry, &mut cache);
        let first = cache
            .get(&root)
            .map(|(fingerprint, verdict)| (fingerprint.clone(), *verdict))
            .expect("cached negative verdict");
        assert!(!first.1);
        assert!(matches!(entry.kind, EntryKind::Directory));

        std::fs::write(bdmv.join("MovieObject.bdmv"), b"movie object")
            .expect("write MovieObject");
        let playlist = bdmv.join("PLAYLIST");
        let stream = bdmv.join("STREAM");
        std::fs::create_dir_all(&playlist).expect("create PLAYLIST");
        std::fs::create_dir_all(&stream).expect("create STREAM");
        std::fs::write(playlist.join("00000.mpls"), b"playlist").expect("write playlist");
        std::fs::write(stream.join("00000.m2ts"), b"stream").expect("write stream");

        classify_bluray_directory_entry(&mut entry, &mut cache);
        let second = cache
            .get(&root)
            .map(|(fingerprint, verdict)| (fingerprint.clone(), *verdict))
            .expect("cached positive verdict");

        assert!(second.1);
        assert_ne!(first.0, second.0);
        assert_eq!(
            marker_len(&first.0, "index.bdmv"),
            marker_len(&second.0, "index.bdmv"),
            "index metadata did not need to change"
        );
        assert!(matches!(entry.kind, EntryKind::BlurayDir));
    }

    #[test]
    fn classify_bluray_directory_cache_invalidates_positive_to_negative() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path().join("movie");
        let (_bdmv, index) = make_valid_bluray_layout(&root, "BDMV", "index.bdmv");

        let meta = std::fs::metadata(&root).expect("metadata");
        let mut entry = BrowseEntry::new(
            root.clone(),
            "movie".to_string(),
            EntryKind::Directory,
            meta.len(),
            meta.modified().ok(),
        );
        let mut cache = std::collections::HashMap::new();

        classify_bluray_directory_entry(&mut entry, &mut cache);
        let first = cache
            .get(&root)
            .map(|(fingerprint, verdict)| (fingerprint.clone(), *verdict))
            .expect("cached positive verdict");
        assert!(first.1);
        assert!(matches!(entry.kind, EntryKind::BlurayDir));

        std::fs::remove_file(&index).expect("delete index marker");
        classify_bluray_directory_entry(&mut entry, &mut cache);
        let second = cache
            .get(&root)
            .map(|(fingerprint, verdict)| (fingerprint.clone(), *verdict))
            .expect("cached negative verdict");

        assert!(!second.1);
        assert_ne!(first.0, second.0);
        assert!(matches!(entry.kind, EntryKind::Directory));
    }

    #[test]
    fn classify_bluray_directory_cache_invalidates_positive_to_negative_with_index_unchanged() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path().join("movie");
        let bdmv = root.join("BDMV");
        let stream_file = bdmv.join("STREAM").join("00000.m2ts");
        make_valid_bluray_layout(&root, "BDMV", "index.bdmv");

        let meta = std::fs::metadata(&root).expect("metadata");
        let mut entry = BrowseEntry::new(
            root.clone(),
            "movie".to_string(),
            EntryKind::Directory,
            meta.len(),
            meta.modified().ok(),
        );
        let mut cache = std::collections::HashMap::new();

        classify_bluray_directory_entry(&mut entry, &mut cache);
        let first = cache
            .get(&root)
            .map(|(fingerprint, verdict)| (fingerprint.clone(), *verdict))
            .expect("cached positive verdict");
        assert!(first.1);
        assert!(matches!(entry.kind, EntryKind::BlurayDir));

        std::fs::remove_file(&stream_file).expect("delete stream file");
        classify_bluray_directory_entry(&mut entry, &mut cache);
        let second = cache
            .get(&root)
            .map(|(fingerprint, verdict)| (fingerprint.clone(), *verdict))
            .expect("cached negative verdict");

        assert!(!second.1);
        assert_ne!(first.0, second.0);
        assert_eq!(
            marker_len(&first.0, "index.bdmv"),
            marker_len(&second.0, "index.bdmv"),
            "index metadata did not need to change"
        );
        assert!(matches!(entry.kind, EntryKind::Directory));
    }

    #[test]
    fn classify_bluray_directory_entry_fingerprints_case_insensitive_marker() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path().join("movie");
        let (_bdmv, index) = make_valid_bluray_layout(&root, "bdmv", "INDEX.BDMV");

        let meta = std::fs::metadata(&root).expect("metadata");
        let mut entry = BrowseEntry::new(
            root.clone(),
            "movie".to_string(),
            EntryKind::Directory,
            meta.len(),
            meta.modified().ok(),
        );
        let mut cache = std::collections::HashMap::new();

        classify_bluray_directory_entry(&mut entry, &mut cache);
        let first = cache
            .get(&root)
            .map(|(fingerprint, verdict)| (fingerprint.clone(), *verdict))
            .expect("cached initial verdict");
        assert!(first.1);

        std::fs::write(&index, b"index-with-different-length").expect("replace index");
        entry.kind = EntryKind::Directory;
        classify_bluray_directory_entry(&mut entry, &mut cache);
        let second = cache
            .get(&root)
            .map(|(fingerprint, verdict)| (fingerprint.clone(), *verdict))
            .expect("cached replacement verdict");

        assert!(second.1);
        assert_ne!(first.0, second.0);
        assert!(matches!(entry.kind, EntryKind::BlurayDir));
    }

    #[test]
    fn classify_bluray_directory_entry_accepts_bdmv_directory_as_source() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path().join("movie");
        let (bdmv, index) = make_valid_bluray_layout(&root, "BDMV", "index.bdmv");

        let meta = std::fs::metadata(&bdmv).expect("metadata");
        let mut entry = BrowseEntry::new(
            bdmv.clone(),
            "BDMV".to_string(),
            EntryKind::Directory,
            meta.len(),
            meta.modified().ok(),
        );
        let mut cache = std::collections::HashMap::new();

        classify_bluray_directory_entry(&mut entry, &mut cache);

        let index_meta = std::fs::metadata(&index).expect("index metadata");
        let cached = cache.get(&bdmv).expect("cached BDMV-source verdict");
        assert!(matches!(entry.kind, EntryKind::BlurayDir));
        assert_eq!(cached.1, true);
        assert_eq!(cached.0.len, index_meta.len());
    }

    #[test]
    fn bluray_directory_fingerprint_tracks_existing_bdmv_before_marker_exists() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path().join("movie");
        let bdmv = root.join("BDMV");
        std::fs::create_dir_all(&bdmv).expect("create BDMV");

        let meta = std::fs::metadata(&root).expect("metadata");
        let entry = BrowseEntry::new(
            root.clone(),
            "movie".to_string(),
            EntryKind::Directory,
            meta.len(),
            meta.modified().ok(),
        );

        let fingerprint = bluray_directory_classification_fingerprint(&entry);
        let bdmv_meta = std::fs::metadata(&bdmv).expect("BDMV metadata");
        assert_eq!(fingerprint.len, bdmv_meta.len());
        assert_eq!(fingerprint.modified, bdmv_meta.modified().ok());
    }

    #[test]
    fn scan_directory_blocking_classifies_bluray_dirs_off_ui_path() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path().join("movie");
        make_valid_bluray_layout(&root, "BDMV", "index.bdmv");

        let cancel = std::sync::atomic::AtomicBool::new(false);
        let snapshot = BrowseClassificationCacheSnapshot::default();
        let (_parent, dirs, _files, updates) =
            scan_directory_blocking(td.path(), &cancel, &snapshot).expect("scan directory");
        let entry = dirs
            .iter()
            .find(|entry| entry.path == root)
            .expect("scanned movie dir");

        assert!(matches!(entry.kind, EntryKind::BlurayDir));
        assert!(updates
            .bluray_dir
            .iter()
            .any(|(update_path, _fingerprint, verdict)| update_path == &root && *verdict));
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

    #[test]
    fn archive_relative_path_normalization_rejects_unsafe_components() {
        assert_eq!(
            normalize_archive_relative_path(std::path::Path::new("disc 1/track.flac")).as_deref(),
            Some("disc 1/track.flac")
        );
        assert_eq!(
            normalize_archive_relative_path(std::path::Path::new("./disc 1/track.flac")).as_deref(),
            Some("disc 1/track.flac")
        );
        assert!(normalize_archive_relative_path(std::path::Path::new("../track.flac")).is_none());
        assert!(normalize_archive_relative_path(std::path::Path::new("disc/../track.flac")).is_none());
        assert!(normalize_archive_relative_path(std::path::Path::new("/absolute/track.flac")).is_none());
    }

    #[test]
    fn archive_probe_invalidation_removes_synthetic_entries_only_for_that_archive() {
        let archive = std::path::PathBuf::from("/tmp/Album.zip");
        let synthetic = archive.join("Disc 1/01.flac");
        let pending = archive.join("Disc 1/02.flac");
        let sibling = std::path::PathBuf::from("/tmp/Album.zipx/01.flac");

        let mut state = BrowseState::new();
        let identity = ProbeCacheIdentity { modified: None, size: 0 };
        state.probe_cache.insert(archive.clone(), ProbeCacheEntry::miss(identity));
        state.probe_cache.insert(synthetic.clone(), ProbeCacheEntry::miss(identity));
        state.probe_cache.insert(sibling.clone(), ProbeCacheEntry::miss(identity));
        state.probe_pending.insert(pending.clone());
        state.probe_pending.insert(sibling.clone());

        state.invalidate_archive_probe_cache_for(&archive);

        assert!(!state.probe_cache.contains_key(&archive));
        assert!(!state.probe_cache.contains_key(&synthetic));
        assert!(!state.probe_pending.contains(&pending));
        assert!(state.probe_cache.contains_key(&sibling));
        assert!(state.probe_pending.contains(&sibling));
        assert_eq!(state.archive_probe_epoch_for(&archive), 1);
    }

    #[test]
    fn archive_probe_completion_acceptance_rejects_stale_epoch() {
        let archive = std::path::PathBuf::from("/tmp/Album.zip");
        let synthetic = archive.join("Disc 1/01.flac");
        let mut state = BrowseState::new();

        let captured_epoch = state.archive_probe_epoch_for(&archive);
        state.probe_pending.insert(synthetic.clone());
        state.invalidate_archive_probe_cache_for(&archive);
        let was_pending = state.probe_pending.remove(&synthetic);

        assert!(!state.accept_archive_entry_probe_completion(
            &synthetic,
            &archive,
            captured_epoch,
            was_pending,
        ));
    }

    #[test]
    fn archive_probe_completion_acceptance_requires_pending_marker() {
        let archive = std::path::PathBuf::from("/tmp/Album.zip");
        let synthetic = archive.join("Disc 1/01.flac");
        let state = BrowseState::new();
        let captured_epoch = state.archive_probe_epoch_for(&archive);

        assert!(!state.accept_archive_entry_probe_completion(
            &synthetic,
            &archive,
            captured_epoch,
            false,
        ));
    }

    #[test]
    fn archive_probe_completion_acceptance_allows_current_pending_probe() {
        let archive = std::path::PathBuf::from("/tmp/Album.zip");
        let synthetic = archive.join("Disc 1/01.flac");
        let state = BrowseState::new();
        let captured_epoch = state.archive_probe_epoch_for(&archive);

        assert!(state.accept_archive_entry_probe_completion(
            &synthetic,
            &archive,
            captured_epoch,
            true,
        ));
    }

    #[test]
    fn browse_preemphasis_checks_are_worker_side_only() {
        let source = include_str!("browse.rs");
        let public_wrapper_name = format!(
            "{}{}",
            "preemphasis_metadata_check_",
            "pub"
        );

        assert!(
            !source.contains(&public_wrapper_name),
            "browse code must not use the compatibility PE wrapper; use the explicit blocking API only from worker closures"
        );
        assert!(
            source.contains("spawn_cached_audio_probe_metadata_completion"),
            "SQLite probe-cache hits must use the same worker-side metadata completion path"
        );
        assert!(
            source.matches("preemphasis_metadata_check_blocking").count() >= 1,
            "probe-cache hits and failed fresh metadata reads should enrich PE metadata only on blocking workers"
        );
        assert!(
            !source.contains("read_metadata(&path_for_task).unwrap_or_default()"),
            "fresh browse probes must not drop read_metadata errors and then repeat PE checks unconditionally"
        );

        let fresh_probe_start = source
            .find("pub fn spawn_audio_probe")
            .expect("spawn_audio_probe source");
        let fresh_probe_tail = &source[fresh_probe_start..];
        let fresh_probe_end = fresh_probe_tail
            .find("/// Spawn a background tokio task that probes the audio image")
            .expect("end of spawn_audio_probe source");
        let fresh_probe = &fresh_probe_tail[..fresh_probe_end];
        assert!(
            !fresh_probe.contains("metadata.preemphasis_metadata.is_none()"),
            "successful fresh browse metadata reads already perform PE enrichment and must not repeat it"
        );
    }


    fn archive_listing_for_tests(entries: Vec<crate::tui::archive_listing::ArchiveEntry>) -> crate::tui::archive_listing::ArchiveListing {
        archive_listing_for_tests_at(std::path::PathBuf::from("/tmp/test-album.zip"), entries)
    }

    fn archive_listing_for_tests_at(
        archive_path: std::path::PathBuf,
        entries: Vec<crate::tui::archive_listing::ArchiveEntry>,
    ) -> crate::tui::archive_listing::ArchiveListing {
        crate::tui::archive_listing::ArchiveListing {
            archive_path,
            format: "zip".to_string(),
            physical_size: 4096,
            entries,
        }
    }

    fn archive_entry_for_tests(path: &str, is_dir: bool) -> crate::tui::archive_listing::ArchiveEntry {
        crate::tui::archive_listing::ArchiveEntry {
            path: path.to_string(),
            size: if is_dir { 0 } else { 100 },
            packed_size: if is_dir { 0 } else { 80 },
            is_dir,
            encrypted: false,
        }
    }

    #[test]
    fn archive_view_audio_filter_hides_non_audio_entries() {
        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests(vec![
                archive_entry_for_tests("track.flac", false),
                archive_entry_for_tests("cover.jpg", false),
                archive_entry_for_tests("notes.txt", false),
            ]),
            None,
        );

        state.set_format_filter(FormatFilter::AudioOnly);

        let names: Vec<_> = state.entries.iter().map(|entry| entry.name.as_str()).collect();
        assert!(names.contains(&"track.flac"));
        assert!(!names.contains(&"cover.jpg"));
        assert!(!names.contains(&"notes.txt"));
        assert!(state.all_files.iter().any(|entry| entry.name == "cover.jpg"));
    }

    #[test]
    fn archive_view_honors_hidden_filter_without_losing_raw_model() {
        let mut state = BrowseState::new();
        state.show_hidden = false;
        state.enter_archive(
            archive_listing_for_tests(vec![
                archive_entry_for_tests(".hidden.flac", false),
                archive_entry_for_tests("visible.flac", false),
            ]),
            None,
        );

        let visible_names: Vec<_> = state.entries.iter().map(|entry| entry.name.as_str()).collect();
        assert!(visible_names.contains(&"visible.flac"));
        assert!(!visible_names.contains(&".hidden.flac"));
        assert!(state.all_files.iter().any(|entry| entry.name == ".hidden.flac"));

        state.toggle_hidden();
        let visible_names: Vec<_> = state.entries.iter().map(|entry| entry.name.as_str()).collect();
        assert!(visible_names.contains(&".hidden.flac"));
    }

    #[test]
    fn archive_local_search_uses_archive_model_not_parent_directory_model() {
        let mut state = BrowseState::new();
        state.all_files = vec![BrowseEntry::new(
            std::path::PathBuf::from("/parent/needle.flac"),
            "needle.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            100,
            None,
        )];
        state.enter_archive(
            archive_listing_for_tests(vec![archive_entry_for_tests("archive-track.flac", false)]),
            None,
        );

        state.search.active = true;
        state.search.mode = SearchMode::Filename;
        state.search.input = TextInputState::new("archive".to_string());
        state.execute_search(None);

        let names: Vec<_> = state.entries.iter().map(|entry| entry.name.as_str()).collect();
        assert!(names.contains(&"archive-track.flac"));
        assert!(!names.contains(&"needle.flac"));
        assert!(state.entries.iter().any(|entry| entry.path == std::path::PathBuf::from("/tmp/test-album.zip/archive-track.flac")));
    }

    #[test]
    fn archive_recursive_search_is_archive_local() {
        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests(vec![
                archive_entry_for_tests("Disc 1", true),
                archive_entry_for_tests("Disc 1/01 - Song.flac", false),
                archive_entry_for_tests("Disc 1/cover.jpg", false),
            ]),
            None,
        );

        state.search.active = true;
        state.search.recursive = true;
        state.search.mode = SearchMode::Filename;
        state.search.input = TextInputState::new("song".to_string());
        state.execute_search(None);

        let names: Vec<_> = state.entries.iter().map(|entry| entry.name.as_str()).collect();
        assert!(names.contains(&"Disc 1/01 - Song.flac"));
        assert!(!names.contains(&"Disc 1/cover.jpg"));
    }

    fn cache_archive_probe_metadata(
        state: &mut BrowseState,
        inner_path: &str,
        title: Option<&str>,
        artist: Option<&str>,
        album: Option<&str>,
        year: Option<&str>,
    ) {
        let archive_path = state
            .archive
            .as_ref()
            .expect("archive state")
            .listing
            .archive_path
            .clone();
        state.probe_cache.insert(
            archive_path.join(inner_path),
            ProbeCacheEntry::hit(
                ProbeCacheIdentity { modified: None, size: 4096 },
                std::sync::Arc::new(CachedInfo {
                    source: crate::tui::probe::SourceInfo {
                        format_name: "FLAC".to_string(),
                        codec: "flac".to_string(),
                        bit_depth: Some(16),
                        sample_rate: 44_100,
                        channels: 2,
                        channel_layout: "stereo".to_string(),
                        duration_secs: 1.0,
                        file_size: 4096,
                    },
                    metadata: crate::tui::probe::SourceMetadata {
                        title: title.map(str::to_string),
                        artist: artist.map(str::to_string),
                        album: album.map(str::to_string),
                        year: year.map(str::to_string),
                        ..Default::default()
                    },
                }),
            ),
        );
    }

    fn result_names_without_parent(state: &BrowseState) -> Vec<String> {
        state
            .entries
            .iter()
            .filter(|entry| !matches!(entry.kind, EntryKind::ParentDir))
            .map(|entry| entry.name.clone())
            .collect()
    }

    fn cached_archive_tags(
        state: &mut BrowseState,
        archive_path: &std::path::Path,
        inner_path: &str,
        title: Option<&str>,
        artist: Option<&str>,
        album: Option<&str>,
        year: Option<&str>,
    ) {
        let synthetic = archive_path.join(inner_path);
        let fingerprint = TagCacheFingerprint::for_path(archive_path).expect("archive fingerprint");
        let mut tags = TagReadResult::empty();
        tags.title = title.map(str::to_string);
        tags.artist = artist.map(str::to_string);
        tags.album = album.map(str::to_string);
        tags.year = year.map(str::to_string);
        let mut parts = Vec::new();
        if let Some(value) = tags.title.as_deref() { parts.push(value); }
        if let Some(value) = tags.artist.as_deref() { parts.push(value); }
        if let Some(value) = tags.album.as_deref() { parts.push(value); }
        if let Some(value) = tags.year.as_deref() { parts.push(value); }
        tags.tag_string = parts.join(" ").to_ascii_lowercase();
        state.search.archive_tag_cache.insert(
            synthetic,
            CachedArchiveTagSearchString {
                archive_fingerprint: fingerprint,
                password_identity: ArchiveTagPasswordIdentity::for_password(None),
                tags,
            },
        );
    }

    fn install_test_archive_tags(
        archive_path: &std::path::Path,
        inner_path: &str,
        title: Option<&str>,
        artist: Option<&str>,
        album: Option<&str>,
        year: Option<&str>,
    ) {
        let mut tags = TagReadResult::empty();
        tags.title = title.map(str::to_string);
        tags.artist = artist.map(str::to_string);
        tags.album = album.map(str::to_string);
        tags.year = year.map(str::to_string);
        let mut parts = Vec::new();
        if let Some(value) = tags.title.as_deref() { parts.push(value); }
        if let Some(value) = tags.artist.as_deref() { parts.push(value); }
        if let Some(value) = tags.album.as_deref() { parts.push(value); }
        if let Some(value) = tags.year.as_deref() { parts.push(value); }
        tags.tag_string = parts.join(" ").to_ascii_lowercase();

        let fixtures = TEST_ARCHIVE_TAG_FIXTURES.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
        fixtures
            .lock()
            .expect("test archive tag fixtures")
            .insert((archive_path.to_path_buf(), inner_path.to_string()), tags);
    }

    #[tokio::test]
    async fn archive_async_tags_search_finds_unprobed_entry_without_preseeded_cache() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        std::fs::write(&archive, b"archive placeholder").expect("archive file");
        install_test_archive_tags(
            &archive,
            "track.flac",
            Some("Async Title"),
            Some("Async Artist"),
            Some("Async Album"),
            Some("1986"),
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests_at(
                archive.clone(),
                vec![archive_entry_for_tests("track.flac", false)],
            ),
            None,
        );
        state.search.active = true;
        state.search.mode = SearchMode::Tags;
        state.search.input = TextInputState::new("async artist".to_string());

        assert!(state.probe_cache.is_empty());
        assert!(state.search.archive_tag_cache.is_empty());
        state.execute_search(Some(&tx));
        assert!(state.search.searching, "archive tag search should run asynchronously");

        let message = rx.recv().await.expect("archive search completion");
        match message {
            crate::tui::message::AppMessage::SearchComplete {
                archive_path,
                archive_inner_path,
                pre_sorted,
                archive_tag_cache_updates,
                results,
                ..
            } => {
                assert_eq!(archive_path.as_deref(), Some(archive.as_path()));
                assert_eq!(archive_inner_path.as_deref(), Some(""));
                assert!(pre_sorted);
                assert!(!archive_tag_cache_updates.is_empty());
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].0.name, "track.flac");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn archive_unprobed_tag_source_is_extraction_backed_not_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        std::fs::write(&archive, b"archive placeholder").expect("archive file");

        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests_at(
                archive.clone(),
                vec![archive_entry_for_tests("track.flac", false)],
            ),
            Some("secret".to_string()),
        );

        let entry = state
            .all_files
            .iter()
            .find(|entry| entry.name == "track.flac")
            .expect("archive file entry")
            .clone();
        assert!(!state.probe_cache.contains_key(&entry.path));

        match state.tag_source_for_entry(&entry) {
            TagSearchSource::ExtractArchiveEntry { archive_path, inner_path, password, synthetic_path } => {
                assert_eq!(archive_path, archive);
                assert_eq!(inner_path, "track.flac");
                assert_eq!(password.as_deref(), Some("secret"));
                assert_eq!(synthetic_path, entry.path);
            }
            other => panic!("unprobed archive audio entry should be extraction-backed, got {other:?}"),
        }
    }

    #[test]
    fn archive_tags_search_uses_archive_tag_cache_without_probe_cache() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        std::fs::write(&archive, b"archive placeholder").expect("archive file");

        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests_at(
                archive.clone(),
                vec![archive_entry_for_tests("track.flac", false)],
            ),
            None,
        );
        cached_archive_tags(
            &mut state,
            &archive,
            "track.flac",
            Some("Unprobed Title"),
            Some("Unprobed Artist"),
            Some("Unprobed Album"),
            Some("1984"),
        );

        state.search.active = true;
        state.search.mode = SearchMode::Tags;
        state.search.input = TextInputState::new("unprobed artist".to_string());
        state.execute_search(None);

        assert!(state.probe_cache.is_empty());
        assert_eq!(result_names_without_parent(&state), vec!["track.flac".to_string()]);
    }

    #[test]
    fn archive_recursive_tags_search_uses_archive_tag_cache_for_descendants() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        std::fs::write(&archive, b"archive placeholder").expect("archive file");

        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests_at(
                archive.clone(),
                vec![
                    archive_entry_for_tests("Disc 1", true),
                    archive_entry_for_tests("Disc 1/01.flac", false),
                ],
            ),
            None,
        );
        cached_archive_tags(
            &mut state,
            &archive,
            "Disc 1/01.flac",
            Some("Recursive Title"),
            Some("Deep Artist"),
            Some("Album"),
            None,
        );

        state.search.active = true;
        state.search.recursive = true;
        state.search.mode = SearchMode::Tags;
        state.search.input = TextInputState::new("deep artist".to_string());
        state.execute_search(None);

        assert_eq!(result_names_without_parent(&state), vec!["Disc 1/01.flac".to_string()]);
    }

    #[test]
    fn active_archive_both_search_reapplies_when_probe_metadata_arrives() {
        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests(vec![archive_entry_for_tests("track.flac", false)]),
            None,
        );

        state.search.active = true;
        state.search.mode = SearchMode::Both;
        state.search.input = TextInputState::new("late artist".to_string());
        state.execute_search(None);
        assert!(result_names_without_parent(&state).is_empty());

        cache_archive_probe_metadata(
            &mut state,
            "track.flac",
            Some("Late Title"),
            Some("Late Artist"),
            Some("Album"),
            None,
        );
        state.resort_after_probe_cache_update();

        assert_eq!(result_names_without_parent(&state), vec!["track.flac".to_string()]);
    }

    #[test]
    fn active_archive_tag_sort_reorders_when_probe_metadata_arrives() {
        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests(vec![
                archive_entry_for_tests("a.flac", false),
                archive_entry_for_tests("b.flac", false),
            ]),
            None,
        );

        state.search.active = true;
        state.search.mode = SearchMode::Both;
        state.search.sort = SearchSort::Artist;
        state.search.sort_dir = SortDir::Asc;
        state.search.input = TextInputState::new("flac".to_string());
        state.execute_search(None);

        cache_archive_probe_metadata(&mut state, "a.flac", Some("A"), Some("Zulu"), None, None);
        cache_archive_probe_metadata(&mut state, "b.flac", Some("B"), Some("Alpha"), None, None);
        state.resort_after_probe_cache_update();

        assert_eq!(result_names_without_parent(&state), vec!["b.flac".to_string(), "a.flac".to_string()]);
    }

    #[test]
    fn archive_tags_search_uses_probe_cache_metadata() {
        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests(vec![archive_entry_for_tests("track.flac", false)]),
            None,
        );
        cache_archive_probe_metadata(
            &mut state,
            "track.flac",
            Some("Hidden Title"),
            Some("Needle Artist"),
            Some("Archive Album"),
            Some("1984"),
        );

        state.search.active = true;
        state.search.mode = SearchMode::Tags;
        state.search.input = TextInputState::new("needle artist".to_string());
        state.execute_search(None);

        let names = result_names_without_parent(&state);
        assert_eq!(names, vec!["track.flac".to_string()]);
    }

    #[test]
    fn archive_both_search_matches_filename_or_archive_metadata() {
        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests(vec![
                archive_entry_for_tests("boring.flac", false),
                archive_entry_for_tests("filename-needle.flac", false),
                archive_entry_for_tests("miss.flac", false),
            ]),
            None,
        );
        cache_archive_probe_metadata(
            &mut state,
            "boring.flac",
            Some("Needle Title"),
            Some("Artist"),
            Some("Album"),
            None,
        );

        state.search.active = true;
        state.search.mode = SearchMode::Both;
        state.search.input = TextInputState::new("needle".to_string());
        state.execute_search(None);

        let names = result_names_without_parent(&state);
        assert!(names.contains(&"boring.flac".to_string()));
        assert!(names.contains(&"filename-needle.flac".to_string()));
        assert!(!names.contains(&"miss.flac".to_string()));
    }

    #[test]
    fn archive_tag_sort_uses_probe_cache_metadata_not_synthetic_paths() {
        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests(vec![
                archive_entry_for_tests("zeta.flac", false),
                archive_entry_for_tests("alpha.flac", false),
            ]),
            None,
        );
        cache_archive_probe_metadata(&mut state, "zeta.flac", Some("Z"), Some("Zulu"), Some("B"), Some("1999"));
        cache_archive_probe_metadata(&mut state, "alpha.flac", Some("A"), Some("Alpha"), Some("A"), Some("2001"));

        state.search.active = true;
        state.search.mode = SearchMode::Both;
        state.search.sort = SearchSort::Artist;
        state.search.sort_dir = SortDir::Asc;
        state.search.input = TextInputState::new("flac".to_string());
        state.execute_search(None);

        let names = result_names_without_parent(&state);
        assert_eq!(names, vec!["alpha.flac".to_string(), "zeta.flac".to_string()]);
    }

    #[test]
    fn archive_staging_tag_search_falls_back_to_probe_metadata_for_synthetic_entry() {
        let staging_dir = std::env::temp_dir().join(format!(
            "tonepoet-archive-tag-search-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let staged_file = staging_dir.join("Disc 1").join("01.flac");
        std::fs::create_dir_all(staged_file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&staged_file, b"not real audio, forcing probe-cache fallback").expect("write staged file");

        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests(vec![archive_entry_for_tests("Disc 1/01.flac", false)]),
            None,
        );
        let archive_path = state.archive.as_ref().expect("archive").listing.archive_path.clone();
        state.archive.as_mut().expect("archive").staging = Some(ArchiveStagingSession::new(
            staging_dir.clone(),
            archive_path,
            0,
            0,
            0,
        ));
        cache_archive_probe_metadata(
            &mut state,
            "Disc 1/01.flac",
            Some("Staged Needle"),
            Some("Staged Artist"),
            Some("Staged Album"),
            None,
        );

        state.search.active = true;
        state.search.mode = SearchMode::Tags;
        state.search.input = TextInputState::new("staged needle".to_string());
        state.execute_search(None);

        let names = result_names_without_parent(&state);
        assert_eq!(names, vec!["Disc 1/01.flac".to_string()]);
        let _ = std::fs::remove_dir_all(&staging_dir);
    }

    #[tokio::test]
    async fn archive_refresh_with_active_tag_search_launches_async_worker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        std::fs::write(&archive, b"archive placeholder").expect("archive file");
        install_test_archive_tags(
            &archive,
            "track.flac",
            Some("Needle Title"),
            Some("Needle Artist"),
            Some("Album"),
            None,
        );

        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests_at(
                archive.clone(),
                vec![archive_entry_for_tests("track.flac", false)],
            ),
            None,
        );
        state.search.active = true;
        state.search.mode = SearchMode::Tags;
        state.search.input = TextInputState::new("needle artist".to_string());

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        state.refresh_with_search(Some(&tx));

        assert!(state.search.searching, "archive tag refresh should launch an async worker");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("search completion timeout")
            .expect("search completion");
        match msg {
            crate::tui::message::AppMessage::SearchComplete {
                archive_path,
                mode,
                archive_tag_cache_updates,
                results,
                ..
            } => {
                assert_eq!(archive_path, Some(archive.clone()));
                assert_eq!(mode, SearchMode::Tags);
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].0.name, "track.flac");
                assert!(!archive_tag_cache_updates.is_empty());
            }
            other => panic!("expected archive SearchComplete, got {other:?}"),
        }
    }

    #[test]
    fn archive_tag_cache_is_password_identity_scoped() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("encrypted.zip");
        std::fs::write(&archive, b"archive placeholder").expect("archive file");
        install_test_archive_tags(
            &archive,
            "track.flac",
            Some("Correct Password Title"),
            Some("Correct Artist"),
            Some("Album"),
            None,
        );

        let mut state = BrowseState::new();
        let synthetic = archive.join("track.flac");
        let fingerprint = TagCacheFingerprint::for_path(&archive).expect("archive fingerprint");
        let mut stale = TagReadResult::empty();
        stale.title = Some("Stale Title".to_string());
        stale.artist = Some("Stale Artist".to_string());
        stale.tag_string = "stale title stale artist".to_string();
        state.search.archive_tag_cache.insert(
            synthetic.clone(),
            CachedArchiveTagSearchString {
                archive_fingerprint: fingerprint,
                password_identity: ArchiveTagPasswordIdentity::for_password(Some("wrong")),
                tags: stale,
            },
        );

        let tags = state.archive_tag_read_result_cached(
            &archive,
            "track.flac",
            Some("correct"),
            &synthetic,
        );

        assert!(tags.tag_string.contains("correct artist"));
        let cached = state.search.archive_tag_cache.get(&synthetic).expect("refreshed cache");
        assert_eq!(
            cached.password_identity,
            ArchiveTagPasswordIdentity::for_password(Some("correct"))
        );
        assert!(cached.tags.tag_string.contains("correct artist"));
    }

    #[test]
    fn changing_archive_password_clears_archive_tag_cache_for_archive() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("encrypted.zip");
        std::fs::write(&archive, b"archive placeholder").expect("archive file");

        let mut state = BrowseState::new();
        state.enter_archive(
            archive_listing_for_tests_at(
                archive.clone(),
                vec![archive_entry_for_tests("track.flac", false)],
            ),
            Some("wrong".to_string()),
        );
        cached_archive_tags(
            &mut state,
            &archive,
            "track.flac",
            Some("Cached"),
            Some("Artist"),
            None,
            None,
        );
        assert!(!state.search.archive_tag_cache.is_empty());

        state.replace_active_archive_listing(
            archive_listing_for_tests_at(
                archive.clone(),
                vec![archive_entry_for_tests("track.flac", false)],
            ),
            Some("correct".to_string()),
        );

        assert!(state.search.archive_tag_cache.is_empty());
    }

}

#[cfg(test)]
mod archive_edit_log_semantics_tests {
    use super::*;

    fn session() -> ArchiveStagingSession {
        ArchiveStagingSession::new(
            std::path::PathBuf::from("/tmp/staging"),
            std::path::PathBuf::from("/tmp/archive.zip"),
            1,
            2,
            3,
        )
    }

    #[test]
    fn metadata_field_edits_are_replayable_and_coalesced_to_final_value() {
        let mut staging = session();

        staging.append_metadata_write(
            "Disc 1/01.flac".to_string(),
            "Title".to_string(),
            "Old title".to_string(),
        );
        staging.append_metadata_write(
            "Disc 1/01.flac".to_string(),
            "Title".to_string(),
            "Final title".to_string(),
        );

        assert!(staging.dirty);
        assert_eq!(staging.edits.len(), 1);
        assert_eq!(
            staging.edits[0],
            ArchiveEdit::MetadataWrite {
                inner_path: "Disc 1/01.flac".to_string(),
                field: "Title".to_string(),
                value: "Final title".to_string(),
            }
        );
    }

    #[test]
    fn content_modified_dirty_markers_do_not_fabricate_metadata_fields() {
        let mut staging = session();

        staging.append_content_modified(
            "Disc 1/01.flac".to_string(),
            "metadata-editor-save".to_string(),
        );
        staging.append_content_modified(
            "Disc 1/01.flac".to_string(),
            "metadata-editor-save".to_string(),
        );

        assert!(staging.dirty);
        assert_eq!(staging.edits.len(), 1);
        assert_eq!(
            staging.edits[0],
            ArchiveEdit::ContentModified {
                inner_path: "Disc 1/01.flac".to_string(),
                kind: "metadata-editor-save".to_string(),
            }
        );
    }
}
#[cfg(test)]
mod browse_perf_followup_v10_tests {
    use super::*;

    fn scored_entry(name: &str) -> BrowseEntry {
        BrowseEntry::new(
            PathBuf::from(format!("/tmp/{name}")),
            name.to_string(),
            EntryKind::OtherFile,
            0,
            None,
        )
    }

    #[test]
    fn bounded_score_search_matches_full_sort_desc_top_cap() {
        let mut bounded = BoundedScoreSearchResults::new(3, SortDir::Desc);
        for (name, score) in [("a", 10), ("b", 40), ("c", 20), ("d", 30), ("e", 50)] {
            bounded.push(scored_entry(name), score);
        }
        let mut retained = bounded.into_vec();
        sort_search_results(&mut retained, SearchSort::Score, SortDir::Desc);
        let scores: Vec<i64> = retained.into_iter().map(|(_, score)| score).collect();
        assert_eq!(scores, vec![50, 40, 30]);
    }

    #[test]
    fn bounded_score_search_matches_full_sort_asc_top_cap() {
        let mut bounded = BoundedScoreSearchResults::new(2, SortDir::Asc);
        for (name, score) in [("a", 10), ("b", 40), ("c", 20), ("d", 30), ("e", 50)] {
            bounded.push(scored_entry(name), score);
        }
        let mut retained = bounded.into_vec();
        sort_search_results(&mut retained, SearchSort::Score, SortDir::Asc);
        let scores: Vec<i64> = retained.into_iter().map(|(_, score)| score).collect();
        assert_eq!(scores, vec![10, 20]);
    }

    #[test]
    fn bounded_score_search_keeps_older_equal_score_matches() {
        let mut bounded = BoundedScoreSearchResults::new(2, SortDir::Desc);
        for name in ["first", "second", "third"] {
            bounded.push(scored_entry(name), 100);
        }
        let mut retained = bounded.into_vec();
        sort_search_results(&mut retained, SearchSort::Score, SortDir::Desc);
        let names: Vec<String> = retained.into_iter().map(|(entry, _)| entry.name).collect();
        assert_eq!(names, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn bounded_score_heap_matches_full_sort_for_broad_desc_matches() {
        let inputs: Vec<(BrowseEntry, i64)> = (0..256)
            .map(|idx| {
                let score = ((idx * 37) % 101) as i64;
                (scored_entry(&format!("item-{idx:03}")), score)
            })
            .collect();
        let mut full = inputs.clone();
        sort_search_results(&mut full, SearchSort::Score, SortDir::Desc);
        full.truncate(17);

        let mut bounded = BoundedScoreSearchResults::new(17, SortDir::Desc);
        for (entry, score) in inputs {
            bounded.push(entry, score);
        }
        let mut retained = bounded.into_vec();
        sort_search_results(&mut retained, SearchSort::Score, SortDir::Desc);

        let expected: Vec<(String, i64)> = full.into_iter().map(|(entry, score)| (entry.name, score)).collect();
        let actual: Vec<(String, i64)> = retained.into_iter().map(|(entry, score)| (entry.name, score)).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn disc_probe_launch_error_without_fingerprint_requires_current_disc_selection() {
        let mut state = BrowseState::new();
        let disc = BrowseEntry::new(
            PathBuf::from("/tmp/current.iso"),
            "current.iso".to_string(),
            EntryKind::SacdIso,
            0,
            None,
        );
        let other = BrowseEntry::new(
            PathBuf::from("/tmp/other.txt"),
            "other.txt".to_string(),
            EntryKind::OtherFile,
            0,
            None,
        );
        state.entries = vec![disc.clone(), other.clone()];
        state.selected_index = 0;
        assert!(state.current_selected_disc_source_matches(&disc.path));

        state.selected_index = 1;
        assert!(!state.current_selected_disc_source_matches(&disc.path));
        assert!(!state.current_selected_disc_source_matches(&other.path));
    }

    #[test]
    fn recursive_dir_stats_queue_is_bounded_and_current_selection_first() {
        let mut state = BrowseState::new();
        let active = std::env::temp_dir();
        let active_identity = std::fs::metadata(&active)
            .ok()
            .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
            .unwrap_or(ProbeCacheIdentity { modified: None, size: 0 });
        state.dir_stats_active.insert(
            active.clone(),
            DirStatsActiveJob {
                identity: active_identity,
                scan_generation: state.scan_generation,
                cursor_focused: false,
                cancel: Arc::new(AtomicBool::new(false)),
            },
        );
        state.dir_stats_pending.insert(active);

        let first = BrowseEntry::new(
            PathBuf::from("/tmp/first-dir"),
            "first-dir".to_string(),
            EntryKind::Directory,
            0,
            None,
        );
        let second = BrowseEntry::new(
            PathBuf::from("/tmp/second-dir"),
            "second-dir".to_string(),
            EntryKind::Directory,
            0,
            None,
        );
        state.entries = vec![first.clone(), second.clone()];
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        state.selected_index = 0;
        state.schedule_cursor_focused_dir_stats(
            first.path.clone(),
            ProbeCacheIdentity::from_entry(&first),
            &tx,
        );
        state.selected_index = 1;
        state.schedule_cursor_focused_dir_stats(
            second.path.clone(),
            ProbeCacheIdentity::from_entry(&second),
            &tx,
        );

        assert_eq!(state.dir_stats_queue.len(), 1);
        assert!(same_path(&state.dir_stats_queue[0].path, &second.path));
        assert!(!state.dir_stats_pending.contains(&first.path));
        assert!(state.dir_stats_pending.contains(&second.path));
    }

    #[test]
    fn recursive_dir_stats_queue_clears_on_navigation_reset() {
        let mut state = BrowseState::new();
        let path = PathBuf::from("/tmp/queued-dir");
        state.dir_stats_pending.insert(path.clone());
        state.dir_stats_queue.push_back(DirStatsRequest {
            path: path.clone(),
            identity: ProbeCacheIdentity { modified: None, size: 0 },
            scan_generation: state.scan_generation,
            cursor_focused: true,
        });
        let active = std::env::temp_dir();
        let cancel = Arc::new(AtomicBool::new(false));
        state.dir_stats_active.insert(
            active,
            DirStatsActiveJob {
                identity: ProbeCacheIdentity { modified: None, size: 0 },
                scan_generation: state.scan_generation,
                cursor_focused: false,
                cancel: cancel.clone(),
            },
        );

        state.reset_nav_state();

        assert!(cancel.load(Ordering::Relaxed));

        assert!(state.dir_stats_queue.is_empty());
        assert!(state.dir_stats_active.is_empty());
        assert!(state.dir_stats_pending.is_empty());
    }

    #[test]
    fn stale_active_recursive_dir_stats_are_cancelled_before_queueing_current() {
        let mut state = BrowseState::new();
        let stale_path = PathBuf::from("/tmp/stale-active-dir");
        let stale_cancel = Arc::new(AtomicBool::new(false));
        state.dir_stats_active.insert(
            stale_path.clone(),
            DirStatsActiveJob {
                identity: ProbeCacheIdentity { modified: None, size: 0 },
                scan_generation: state.scan_generation + 1,
                cursor_focused: true,
                cancel: stale_cancel.clone(),
            },
        );
        state.dir_stats_pending.insert(stale_path.clone());

        let current = BrowseEntry::new(
            std::env::temp_dir(),
            "current-dir".to_string(),
            EntryKind::Directory,
            0,
            None,
        );
        let current_identity = std::fs::metadata(&current.path)
            .ok()
            .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
            .unwrap_or(ProbeCacheIdentity { modified: None, size: 0 });
        state.entries = vec![current.clone()];
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        state.schedule_cursor_focused_dir_stats(current.path.clone(), current_identity, &tx);

        assert!(stale_cancel.load(Ordering::Relaxed));
        assert!(!state.dir_stats_pending.contains(&stale_path));
        assert!(state.dir_stats_active.keys().any(|path| same_path(path, &current.path)));
    }
 }
