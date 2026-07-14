//! File browser state and directory scanning

use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
use std::time::{Duration, Instant, SystemTime};

use ratatui::layout::Rect;

/// Type-ahead buffer resets after this duration of inactivity.
const TYPE_AHEAD_TIMEOUT: Duration = Duration::from_millis(1500);

/// Expensive Browse focus work is delayed briefly while the cursor is moving.
/// Immediate focus handling is limited to cheap cache reads and identity checks;
/// filesystem walks, disc probes, ffmpeg probes, and tag/CUE/catalog metadata
/// enrichment run only after the cursor rests on the same row.
const BROWSE_PROBE_DEBOUNCE: Duration = Duration::from_millis(175);

/// Folder-content classification must stay cheap enough to run from Browse
/// cursor focus. The walk answers only "one album/disc or a collection?" and
/// never calls audio/media probes.
pub const FOLDER_CLASSIFY_FAN_OUT_THRESHOLD: usize = 8;
pub const FOLDER_CLASSIFY_IO_BUDGET: usize = 100;
pub const FOLDER_CLASSIFY_MAX_DEPTH: usize = 2;

/// Keep scan-completion cache warming bounded. Cursor-focused lookups still
/// fall back to per-file SQLite/fresh probes for entries beyond the warm set.
const PROBE_CACHE_WARM_MAX_CANDIDATES: usize = 4096;
const PROBE_CACHE_WARM_MESSAGE_CHUNK: usize = 128;
const PROBE_CACHE_WARM_MERGE_MIN_ROWS_PER_TICK: usize = 128;
const PROBE_CACHE_WARM_MERGE_MAX_ROWS_PER_TICK: usize = 2048;
const PROBE_CACHE_WARM_MERGE_TIME_BUDGET: Duration = Duration::from_millis(2);
const PROBE_CACHE_WARM_MERGE_TIME_CHECK_INTERVAL: usize = 64;
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

fn search_exact_substring_score(haystack_lower: &str, query_lower: &str) -> Option<i64> {
    if query_lower.is_empty() {
        return None;
    }
    haystack_lower.contains(query_lower).then_some(
        search_fuzzy_min_score(query_lower).saturating_add(query_lower.len() as i64),
    )
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
pub use crate::convert::classify::EntryKind;
pub(super) use crate::convert::classify::{classify_file, is_cue_sheet_path};
pub use crate::convert::queue_expansion::{
    count_audio_files_bounded, expand_paths_to_audio, expand_paths_to_audio_with_metadata,
    QueueExpansionResult,
};
use crate::convert::queue_expansion::expand_paths_to_audio_with_preserved_disc_roots;
use crate::convert::queue_expansion::push_unique_path_with_keys;
pub(crate) use crate::convert::queue_expansion::{
    resolve_cue_file_reference_for_queue, CueReferenceResolution,
};
#[cfg(test)]
pub(crate) use crate::convert::queue_expansion::path_list_contains;
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

/// Reducer-side budget for merging SQLite probe-cache warm rows.
///
/// The reducer may process many more than the historical 128 rows after Browse
/// focus has settled, but it must stay bounded by both rows and wall time so a
/// huge cache-warm backlog cannot monopolize the UI thread. Time is checked only
/// at chunk boundaries to avoid paying an `Instant::now()` cost for every row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProbeCacheWarmDrainBudget {
    pub min_rows: usize,
    pub max_rows: usize,
    pub time_budget: Duration,
    pub time_check_interval: usize,
}

impl ProbeCacheWarmDrainBudget {
    pub(crate) const fn settled_focus() -> Self {
        Self {
            min_rows: PROBE_CACHE_WARM_MERGE_MIN_ROWS_PER_TICK,
            max_rows: PROBE_CACHE_WARM_MERGE_MAX_ROWS_PER_TICK,
            time_budget: PROBE_CACHE_WARM_MERGE_TIME_BUDGET,
            time_check_interval: PROBE_CACHE_WARM_MERGE_TIME_CHECK_INTERVAL,
        }
    }

    fn normalized(self) -> Self {
        let max_rows = self.max_rows.max(1);
        Self {
            min_rows: self.min_rows.min(max_rows),
            max_rows,
            time_budget: self.time_budget,
            time_check_interval: self.time_check_interval.max(1),
        }
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

#[derive(Debug, Clone)]
struct BrowseColdProbeActiveJob {
    scan_generation: u64,
    cursor_focused: bool,
    cancel: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub struct DiscProbeActiveJob {
    scan_generation: u64,
    cursor_focused: bool,
    cancel: Arc<AtomicBool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FolderClassifyRequest {
    path: PathBuf,
    identity: ProbeCacheIdentity,
    scan_generation: u64,
    cursor_focused: bool,
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

    pub fn merge(&mut self, other: Self) {
        self.probe_backed_resort_needed |= other.probe_backed_resort_needed;
        self.search_reapply_needed |= other.search_reapply_needed;
        self.visible_entries_changed |= other.visible_entries_changed;
        self.info_pane_changed |= other.info_pane_changed;
        self.classification_changed |= other.classification_changed;
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
    /// Number of child directories encountered during the bounded recursive
    /// walk. This intentionally excludes the focused directory itself and
    /// matches the old info-pane behavior of describing the directory's
    /// contents rather than the selected node.
    pub folder_count: usize,
    pub file_count: usize,
    pub audio_count: usize,
    /// Total bytes attributable to audio files in this directory walk.
    pub audio_size: u64,
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

/// Policy for cold directory-summary work started by Browse focus.
///
/// Cache reads and identity checks are always allowed on cursor movement. This
/// policy controls only uncached filesystem/disc work launched after focus has
/// rested on the same entry: the shallow folder-classification walk, native
/// disc-source parsing, and the recursive directory-stats walk. Keeping this
/// explicit prevents future changes from silently reintroducing raw-cursor
/// filesystem scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseDirectorySummaryColdWorkPolicy {
    /// Default behavior: cold shallow classification may run after the Browse
    /// probe debounce; bounded recursive stats may then run after classification
    /// so the info pane can restore concrete file/audio/size counts.
    DebouncedHighlight,
    /// Performance mode: render only identity-valid cached directory/disc
    /// summaries while hovering. The user must descend/refresh or use another
    /// explicit action before cold subdirectory scans or native disc probes are
    /// allowed.
    CachedOnly,
    /// Conservative performance mode: do not start uncached subdirectory
    /// summary walks from a child-row hover. This is intentionally equivalent
    /// to `CachedOnly` for Browse rows because a highlighted child directory is
    /// not yet the descended current directory; it exists as a separate named
    /// mode so persisted/configured settings can express the desired UX.
    AfterDescendOnly,
}

impl Default for BrowseDirectorySummaryColdWorkPolicy {
    fn default() -> Self {
        Self::DebouncedHighlight
    }
}

impl BrowseDirectorySummaryColdWorkPolicy {
    fn allows_uncached_highlight_scans(self) -> bool {
        matches!(self, Self::DebouncedHighlight)
    }
}

/// Separate policy for recursive directory-stats work.
///
/// Folder classification is already shallow, depth/budget bounded, and useful
/// for deciding whether a highlighted folder is one logical album/disc.
/// Recursive stats are different: even after classification says "album-like",
/// a malformed or unexpectedly huge tree can still be expensive. Keeping this
/// as an independent policy lets performance mode disable size/count walks
/// without disabling cheap classification or cached summary rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseDirectoryStatsColdWorkPolicy {
    /// Default: run a shallow-first, time/entry/file-budgeted stats walk only
    /// after the normal settled-focus debounce and only for classifications
    /// that are useful to summarize.
    BoundedAfterDebounce,
    /// Never launch uncached recursive stats from hover/focus. Existing cached
    /// stats may still render if identity-valid.
    CachedOnly,
}

impl Default for BrowseDirectoryStatsColdWorkPolicy {
    fn default() -> Self {
        Self::BoundedAfterDebounce
    }
}

impl BrowseDirectoryStatsColdWorkPolicy {
    fn allows_hover_stats(self) -> bool {
        matches!(self, Self::BoundedAfterDebounce)
    }
}

/// Explicit validation scope for directory-summary cache data.
///
/// `Immediate` facts are about the focused directory's direct children and are
/// valid under the focused directory identity. `ShallowDepth2` facts come from
/// the bounded classification walk and are also keyed by the focused directory
/// identity; callers must not treat them as a strong recursive subtree
/// fingerprint. `RecursiveBestEffort` facts are produced by the legacy
/// directory-stats walker and are intentionally displayed only as a
/// best-effort size/count cache because many filesystems do not update ancestor
/// directory mtimes when deep descendants change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DirectorySummaryScope {
    Immediate,
    ShallowDepth2,
    RecursiveBestEffort,
}

#[derive(Debug, Clone, Default)]
pub struct DirectorySummaryFacts {
    pub classification: Option<Arc<FolderContentClassification>>,
    pub classification_scope: Option<DirectorySummaryScope>,
    pub stats: Option<Arc<DirStats>>,
    pub stats_scope: Option<DirectorySummaryScope>,
}

/// Identity-scoped rollup cache for info-pane directory summaries.
///
/// This does not replace the per-file audio probe cache or the disc probe
/// cache. It provides one cheap "do we already know enough for this focused
/// directory identity?" abstraction so the raw cursor path can render cached
/// summary facts without deciding to descend, rescan, or recursively stat the
/// subtree.
#[derive(Debug, Clone)]
pub struct DirectorySummaryCacheEntry {
    pub identity: ProbeCacheIdentity,
    pub facts: DirectorySummaryFacts,
}

impl DirectorySummaryCacheEntry {
    fn new(identity: ProbeCacheIdentity) -> Self {
        Self {
            identity,
            facts: DirectorySummaryFacts::default(),
        }
    }

    pub fn is_valid_for(&self, identity: ProbeCacheIdentity) -> bool {
        self.identity == identity
    }

    fn set_classification(&mut self, classification: Arc<FolderContentClassification>) {
        self.facts.classification_scope = Some(classification_summary_scope(classification.as_ref()));
        self.facts.classification = Some(classification);
    }

    fn set_stats(&mut self, stats: Arc<DirStats>) {
        self.facts.stats_scope = Some(DirectorySummaryScope::RecursiveBestEffort);
        self.facts.stats = Some(stats);
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectorySummaryPersistentCacheMode {
    /// Do not use any cross-session directory-summary persistence.
    Disabled,
    /// Persist identity-keyed summaries in the application's SQLite database.
    /// This is the normal mode now that the database schema has a dedicated
    /// table. Reads are still settled-focus work, not raw cursor movement.
    DatabaseBacked,
    /// Legacy/test-only file-backed cache used by focused unit tests and by
    /// callers that deliberately want an isolated cache file.
    FileBacked,
}

impl Default for DirectorySummaryPersistentCacheMode {
    fn default() -> Self {
        Self::DatabaseBacked
    }
}

const DIRECTORY_SUMMARY_PERSISTENCE_VERSION: u32 = 3;

fn directory_summary_encode_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b' ' | b':') {
            out.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(&mut out, "%{byte:02X}");
        }
    }
    out
}

fn directory_summary_decode_bytes(encoded: &str) -> Option<Vec<u8>> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%' {
            let hi = *bytes.get(idx + 1)?;
            let lo = *bytes.get(idx + 2)?;
            let hex = [hi, lo];
            let s = std::str::from_utf8(&hex).ok()?;
            out.push(u8::from_str_radix(s, 16).ok()?);
            idx += 3;
        } else {
            out.push(bytes[idx]);
            idx += 1;
        }
    }
    Some(out)
}

fn directory_summary_encode_path(path: &Path) -> String {
    directory_summary_encode_bytes(path.to_string_lossy().as_bytes())
}

fn directory_summary_decode_path(encoded: &str) -> Option<PathBuf> {
    let bytes = directory_summary_decode_bytes(encoded)?;
    Some(PathBuf::from(String::from_utf8_lossy(&bytes).to_string()))
}

fn directory_summary_encode_string(value: &str) -> String {
    directory_summary_encode_bytes(value.as_bytes())
}

fn directory_summary_decode_string(value: &str) -> Option<String> {
    String::from_utf8(directory_summary_decode_bytes(value)?).ok()
}

fn directory_summary_time_to_nanos(time: Option<SystemTime>) -> i128 {
    match time.and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok()) {
        Some(duration) => (duration.as_secs() as i128)
            .saturating_mul(1_000_000_000)
            .saturating_add(duration.subsec_nanos() as i128),
        None => -1,
    }
}

fn directory_summary_time_from_nanos(nanos: i128) -> Option<SystemTime> {
    if nanos < 0 {
        return None;
    }
    let secs = (nanos / 1_000_000_000) as u64;
    let sub = (nanos % 1_000_000_000) as u32;
    Some(SystemTime::UNIX_EPOCH + Duration::new(secs, sub))
}

fn directory_summary_scope_code(scope: DirectorySummaryScope) -> &'static str {
    match scope {
        DirectorySummaryScope::Immediate => "immediate",
        DirectorySummaryScope::ShallowDepth2 => "shallow2",
        DirectorySummaryScope::RecursiveBestEffort => "recursive-best-effort",
    }
}

fn directory_summary_scope_from_code(code: &str) -> Option<DirectorySummaryScope> {
    match code {
        "immediate" => Some(DirectorySummaryScope::Immediate),
        "shallow2" => Some(DirectorySummaryScope::ShallowDepth2),
        "recursive-best-effort" => Some(DirectorySummaryScope::RecursiveBestEffort),
        _ => None,
    }
}

fn directory_summary_kind_code(kind: FolderClassificationKind) -> &'static str {
    match kind {
        FolderClassificationKind::Album => "album",
        FolderClassificationKind::Disc => "disc",
        FolderClassificationKind::MultiDisc => "multidisc",
        FolderClassificationKind::Collection => "collection",
        FolderClassificationKind::Unknown => "unknown",
    }
}

fn directory_summary_kind_from_code(code: &str) -> Option<FolderClassificationKind> {
    match code {
        "album" => Some(FolderClassificationKind::Album),
        "disc" => Some(FolderClassificationKind::Disc),
        "multidisc" => Some(FolderClassificationKind::MultiDisc),
        "collection" => Some(FolderClassificationKind::Collection),
        "unknown" => Some(FolderClassificationKind::Unknown),
        _ => None,
    }
}

fn directory_summary_marker_code(marker: FolderDiscMarkerKind) -> &'static str {
    match marker {
        FolderDiscMarkerKind::BluRay => "bluray",
        FolderDiscMarkerKind::DvdVideo => "dvdv",
        FolderDiscMarkerKind::DvdAudio => "dvda",
        FolderDiscMarkerKind::Sacd => "sacd",
        FolderDiscMarkerKind::Iso => "iso",
    }
}

fn directory_summary_marker_from_code(code: &str) -> Option<FolderDiscMarkerKind> {
    match code {
        "bluray" => Some(FolderDiscMarkerKind::BluRay),
        "dvdv" => Some(FolderDiscMarkerKind::DvdVideo),
        "dvda" => Some(FolderDiscMarkerKind::DvdAudio),
        "sacd" => Some(FolderDiscMarkerKind::Sacd),
        "iso" => Some(FolderDiscMarkerKind::Iso),
        "-" => None,
        _ => None,
    }
}

fn directory_summary_format_counts_to_string(counts: &BTreeMap<String, usize>) -> String {
    counts
        .iter()
        .map(|(format, count)| format!("{}={count}", directory_summary_encode_string(format)))
        .collect::<Vec<_>>()
        .join(",")
}

fn directory_summary_format_counts_from_string(encoded: &str) -> Option<BTreeMap<String, usize>> {
    let mut counts = BTreeMap::new();
    if encoded.is_empty() || encoded == "-" {
        return Some(counts);
    }
    for item in encoded.split(',') {
        let (format, count) = item.split_once('=')?;
        counts.insert(directory_summary_decode_string(format)?, count.parse().ok()?);
    }
    Some(counts)
}

fn directory_summary_audio_to_fields(audio: &FolderAudioSummary) -> (String, String) {
    let formats = directory_summary_format_counts_to_string(&audio.format_counts);
    let paths = audio
        .file_paths
        .iter()
        .map(|path| directory_summary_encode_path(path))
        .collect::<Vec<_>>()
        .join(",");
    (if formats.is_empty() { "-".to_string() } else { formats }, if paths.is_empty() { "-".to_string() } else { paths })
}

fn directory_summary_audio_from_fields(track_count: usize, formats: &str, paths: &str) -> Option<FolderAudioSummary> {
    let format_counts = directory_summary_format_counts_from_string(formats)?;
    let file_paths = if paths.is_empty() || paths == "-" {
        Vec::new()
    } else {
        paths
            .split(',')
            .map(directory_summary_decode_path)
            .collect::<Option<Vec<_>>>()?
    };
    Some(FolderAudioSummary {
        track_count,
        format_counts,
        file_paths,
    })
}

fn directory_summary_unit_to_string(unit: &FolderUnitSummary) -> String {
    let (formats, paths) = directory_summary_audio_to_fields(&unit.audio);
    format!(
        "{}~{}~{}~{}~{}~{}~{}",
        directory_summary_encode_path(&unit.path),
        directory_summary_encode_path(&unit.parent),
        directory_summary_encode_string(&unit.name),
        unit.disc_marker.map(directory_summary_marker_code).unwrap_or("-"),
        unit.audio.track_count,
        formats,
        paths,
    )
}

fn directory_summary_unit_from_string(encoded: &str) -> Option<FolderUnitSummary> {
    let parts: Vec<&str> = encoded.split('~').collect();
    if parts.len() != 7 {
        return None;
    }
    Some(FolderUnitSummary {
        path: directory_summary_decode_path(parts[0])?,
        parent: directory_summary_decode_path(parts[1])?,
        name: directory_summary_decode_string(parts[2])?,
        disc_marker: directory_summary_marker_from_code(parts[3]),
        audio: directory_summary_audio_from_fields(parts[4].parse().ok()?, parts[5], parts[6])?,
    })
}

fn directory_summary_units_to_string(units: &[FolderUnitSummary]) -> String {
    if units.is_empty() {
        return "-".to_string();
    }
    units
        .iter()
        .map(directory_summary_unit_to_string)
        .collect::<Vec<_>>()
        .join(";")
}

fn directory_summary_units_from_string(encoded: &str) -> Option<Vec<FolderUnitSummary>> {
    if encoded.is_empty() || encoded == "-" {
        return Some(Vec::new());
    }
    encoded
        .split(';')
        .map(directory_summary_unit_from_string)
        .collect::<Option<Vec<_>>>()
}

impl DirectorySummaryCacheEntry {
    pub(crate) fn to_persistent_line(&self, path: &Path) -> String {
        let mut fields = vec![
            "DIRSUM".to_string(),
            DIRECTORY_SUMMARY_PERSISTENCE_VERSION.to_string(),
            directory_summary_encode_path(path),
            self.identity.size.to_string(),
            directory_summary_time_to_nanos(self.identity.modified).to_string(),
        ];

        if let Some(classification) = self.facts.classification.as_ref() {
            let scope = self
                .facts
                .classification_scope
                .unwrap_or_else(|| classification_summary_scope(classification.as_ref()));
            let (formats, paths) = directory_summary_audio_to_fields(&classification.audio);
            fields.extend([
                directory_summary_scope_code(scope).to_string(),
                directory_summary_kind_code(classification.kind).to_string(),
                classification.audio.track_count.to_string(),
                formats,
                paths,
                directory_summary_units_to_string(&classification.units),
                classification.unit_count.to_string(),
                if classification.collection_many { "1" } else { "0" }.to_string(),
                if classification.io_budget_exhausted { "1" } else { "0" }.to_string(),
                classification.disc_marker.map(directory_summary_marker_code).unwrap_or("-").to_string(),
            ]);
        } else {
            fields.extend(["-", "-", "0", "-", "-", "-", "0", "0", "0", "-"].into_iter().map(|value| value.to_string()));
        }

        if let Some(stats) = self.facts.stats.as_ref() {
            let scope = self
                .facts
                .stats_scope
                .unwrap_or(DirectorySummaryScope::RecursiveBestEffort);
            fields.extend([
                directory_summary_scope_code(scope).to_string(),
                stats.folder_count.to_string(),
                stats.file_count.to_string(),
                stats.audio_count.to_string(),
                stats.audio_size.to_string(),
                stats.total_size.to_string(),
            ]);
        } else {
            fields.extend(["-", "0", "0", "0", "0", "0"].into_iter().map(|value| value.to_string()));
        }

        fields.join("\t")
    }

    pub(crate) fn from_persistent_line(line: &str) -> Option<(PathBuf, DirectorySummaryCacheEntry)> {
        let fields: Vec<&str> = line.trim_end_matches('\n').split('\t').collect();
        if fields.first().copied() != Some("DIRSUM") {
            return None;
        }
        let version = fields.get(1)?.parse::<u32>().ok()?;
        let expected_len = match version {
            1 => 19,
            2 => 20,
            DIRECTORY_SUMMARY_PERSISTENCE_VERSION => 21,
            _ => return None,
        };
        if fields.len() != expected_len {
            return None;
        }
        let path = directory_summary_decode_path(fields[2])?;
        let identity = ProbeCacheIdentity {
            size: fields[3].parse().ok()?,
            modified: directory_summary_time_from_nanos(fields[4].parse().ok()?),
        };
        let mut entry = DirectorySummaryCacheEntry::new(identity);

        if fields[5] != "-" {
            let classification_scope = directory_summary_scope_from_code(fields[5])?;
            let kind = directory_summary_kind_from_code(fields[6])?;
            let audio = directory_summary_audio_from_fields(fields[7].parse().ok()?, fields[8], fields[9])?;
            let units = directory_summary_units_from_string(fields[10])?;
            let classification = FolderContentClassification {
                kind,
                identity,
                audio,
                units,
                unit_count: fields[11].parse().ok()?,
                collection_many: fields[12] == "1",
                io_budget_exhausted: fields[13] == "1",
                disc_marker: directory_summary_marker_from_code(fields[14]),
            };
            entry.facts.classification_scope = Some(classification_scope);
            entry.facts.classification = Some(Arc::new(classification));
        }

        if fields[15] != "-" {
            if version < 3 {
                // v1/v2 rows predate folder_count. A missing folder count is
                // unknown, not zero; keep any restored classification facts,
                // but refuse to render incomplete old stats. A settled v3 stats
                // pass can repopulate the cache with truthful folder totals.
                return Some((path, entry));
            }

            entry.facts.stats_scope = Some(directory_summary_scope_from_code(fields[15])?);
            entry.facts.stats = Some(Arc::new(DirStats {
                folder_count: fields[16].parse().ok()?,
                file_count: fields[17].parse().ok()?,
                audio_count: fields[18].parse().ok()?,
                audio_size: fields[19].parse().ok()?,
                total_size: fields[20].parse().ok()?,
            }));
        }

        Some((path, entry))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderClassificationKind {
    Album,
    Disc,
    MultiDisc,
    Collection,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderDiscMarkerKind {
    BluRay,
    DvdVideo,
    DvdAudio,
    Sacd,
    Iso,
}

impl FolderDiscMarkerKind {
    pub fn label(self) -> &'static str {
        match self {
            FolderDiscMarkerKind::BluRay => "Blu-ray",
            FolderDiscMarkerKind::DvdVideo => "DVD-Video",
            FolderDiscMarkerKind::DvdAudio => "DVD-Audio",
            FolderDiscMarkerKind::Sacd => "SACD",
            FolderDiscMarkerKind::Iso => "ISO",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderAudioSummary {
    pub track_count: usize,
    pub format_counts: BTreeMap<String, usize>,
    pub file_paths: Vec<PathBuf>,
}

impl FolderAudioSummary {
    fn add_audio_file(&mut self, path: PathBuf, format_label: String) {
        self.track_count = self.track_count.saturating_add(1);
        *self.format_counts.entry(format_label).or_insert(0) += 1;
        self.file_paths.push(path);
    }

    fn merge(&mut self, other: &FolderAudioSummary) {
        self.track_count = self.track_count.saturating_add(other.track_count);
        for (format, count) in &other.format_counts {
            *self.format_counts.entry(format.clone()).or_insert(0) += count;
        }
        self.file_paths.extend(other.file_paths.iter().cloned());
    }

    pub fn dominant_format_label(&self) -> Option<&str> {
        self.format_counts
            .iter()
            .max_by(|(left_format, left_count), (right_format, right_count)| {
                left_count
                    .cmp(right_count)
                    .then_with(|| right_format.cmp(left_format))
            })
            .map(|(format, _)| format.as_str())
    }

    pub fn is_mixed_format(&self) -> bool {
        self.format_counts.len() > 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderUnitSummary {
    pub path: PathBuf,
    pub parent: PathBuf,
    pub name: String,
    pub disc_marker: Option<FolderDiscMarkerKind>,
    pub audio: FolderAudioSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderContentClassification {
    pub kind: FolderClassificationKind,
    pub identity: ProbeCacheIdentity,
    pub audio: FolderAudioSummary,
    pub units: Vec<FolderUnitSummary>,
    pub unit_count: usize,
    pub collection_many: bool,
    pub io_budget_exhausted: bool,
    pub disc_marker: Option<FolderDiscMarkerKind>,
}

fn classification_summary_scope(classification: &FolderContentClassification) -> DirectorySummaryScope {
    if classification.io_budget_exhausted {
        DirectorySummaryScope::ShallowDepth2
    } else if classification.units.is_empty() {
        DirectorySummaryScope::Immediate
    } else {
        DirectorySummaryScope::ShallowDepth2
    }
}

fn classification_allows_recursive_stats(classification: &FolderContentClassification) -> bool {
    matches!(
        classification.kind,
        FolderClassificationKind::Album
            | FolderClassificationKind::Disc
            | FolderClassificationKind::MultiDisc
            | FolderClassificationKind::Collection
            | FolderClassificationKind::Unknown
    )
}

impl FolderContentClassification {
    fn unknown(identity: ProbeCacheIdentity, io_budget_exhausted: bool) -> Self {
        Self {
            kind: FolderClassificationKind::Unknown,
            identity,
            audio: FolderAudioSummary::default(),
            units: Vec::new(),
            unit_count: 0,
            collection_many: false,
            io_budget_exhausted,
            disc_marker: None,
        }
    }

    fn collection(identity: ProbeCacheIdentity, unit_count: usize, many: bool, io_budget_exhausted: bool) -> Self {
        Self {
            kind: FolderClassificationKind::Collection,
            identity,
            audio: FolderAudioSummary::default(),
            units: Vec::new(),
            unit_count,
            collection_many: many,
            io_budget_exhausted,
            disc_marker: None,
        }
    }

    /// Return the concrete source path that the existing disc probe should use
    /// for a classified disc folder. When the highlighted directory itself is
    /// the disc root (for example `Album/BDMV`), the fallback path is correct.
    /// When the classifier found one nested disc unit (for example
    /// `Box/FRAGILE/BDMV`) or one ISO, the unit path is the only path the
    /// existing disc probe can parse.
    pub fn disc_probe_source_path<'a>(&'a self, fallback: &'a Path) -> &'a Path {
        if self.kind == FolderClassificationKind::Disc {
            self.units
                .first()
                .map(|unit| unit.path.as_path())
                .unwrap_or(fallback)
        } else {
            fallback
        }
    }
}

#[derive(Debug, Clone)]
pub struct FolderClassificationCacheEntry {
    pub identity: ProbeCacheIdentity,
    pub classification: Arc<FolderContentClassification>,
}

impl FolderClassificationCacheEntry {
    pub fn new(identity: ProbeCacheIdentity, classification: FolderContentClassification) -> Self {
        Self {
            identity,
            classification: Arc::new(classification),
        }
    }

    pub fn is_valid_for(&self, identity: ProbeCacheIdentity) -> bool {
        self.identity == identity && self.classification.identity == identity
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FolderProbeRollup {
    pub probed_count: usize,
    pub total_duration_secs: f64,
    pub profile_counts: BTreeMap<String, usize>,
}

impl FolderProbeRollup {
    pub fn dominant_profile_label(&self) -> Option<&str> {
        self.profile_counts
            .iter()
            .max_by(|(left_profile, left_count), (right_profile, right_count)| {
                left_count
                    .cmp(right_count)
                    .then_with(|| right_profile.cmp(left_profile))
            })
            .map(|(profile, _)| profile.as_str())
    }

    pub fn has_mixed_profiles(&self) -> bool {
        self.profile_counts.len() > 1
    }
}

/// Debounced expensive-work request for the current Browse cursor.
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
            Self::Only(AudioFormat::Shorten),
            Self::Only(AudioFormat::Ogg),
            Self::Only(AudioFormat::Tta),
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
            "shn" | "shorten" => Some(Self::Only(AudioFormat::Shorten)),
            "ogg" | "oga" | "vorbis" => Some(Self::Only(AudioFormat::Ogg)),
            "tta" | "trueaudio" => Some(Self::Only(AudioFormat::Tta)),
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
    /// pipeline materializes the referenced image(s) from the CUE sheet. Cheap
    /// `.iso` archive rows are lazy disc-image candidates. Keep both visible
    /// under AudioOnly so filtering cannot hide them before conversion or
    /// settled-focus disc promotion becomes possible.
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
                    || (matches!(entry.kind, EntryKind::Archive)
                        && is_disc_image_candidate_path(&entry.path))
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

    /// Returns true for entries that represent filesystem directories or the
    /// synthetic parent-directory row. Disc-directory classifications are an
    /// overlay on real directories, so they must keep directory semantics.
    pub fn is_dir(&self) -> bool {
        self.is_navigable_dir()
    }

    /// Returns true for entries that Browse may descend into. Disc source
    /// directories remain navigable even though their `EntryKind` carries
    /// richer media classification for info/probe/convert routing.
    pub fn is_navigable_dir(&self) -> bool {
        matches!(
            self.kind,
            EntryKind::Directory
                | EntryKind::ParentDir
                | EntryKind::DvdAudioDir
                | EntryKind::DvdVideoDir
                | EntryKind::BlurayDir
        )
    }

    /// Returns true for real filesystem directory entries and false for the
    /// synthetic `..` navigation row.
    pub fn is_child_dir(&self) -> bool {
        self.is_navigable_dir() && !matches!(self.kind, EntryKind::ParentDir)
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
        self.kind.is_disc_source()
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

/// Browse multi-selection interaction state.
///
/// Normal mode keeps marks independent of cursor movement. Range mode is a
/// modal preview state used by `v`, Shift+movement, and drag selection: the
/// visible mark set is temporarily replaced by the preview range, while the
/// pre-range mark set is retained so Enter/Space can merge and Esc can restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionMode {
    Normal,
    Range {
        anchor_index: usize,
        pre_range_selection: Vec<PathBuf>,
    },
}

/// Mouse drag range-selection state for Browse rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowseDragState {
    pub anchor_index: usize,
    pub active: bool,
}

impl Default for BrowseDragState {
    fn default() -> Self {
        Self {
            anchor_index: 0,
            active: false,
        }
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

    /// Exact path → identity index for entries owned by the current directory
    /// scan. SQLite warm-cache rows are emitted from these scan-owned paths,
    /// so reducer-side validation must be an O(1), filesystem-free exact-path
    /// lookup rather than a visible-list scan or canonicalizing comparison.
    probe_cache_scan_identity_index: HashMap<PathBuf, ProbeCacheIdentity>,

    // ── View result (refilled by apply_view from scan results) ───────
    pub entries: Vec<BrowseEntry>,
    pub selected_index: usize,
    pub scroll_offset: usize,
    pub visible_height: usize,

    /// Multi-selected file paths
    pub multi_selected: Vec<PathBuf>,

    /// Anchor for one-shot extend selection: the most recent toggled or
    /// clicked selectable row. Path-based so it survives refresh/sort/filter.
    pub multi_select_anchor: Option<PathBuf>,

    /// Modal range-selection state (`v`, Shift+movement, or drag).
    pub selection_mode: SelectionMode,

    /// Active mouse drag range-selection state.
    pub drag_state: BrowseDragState,

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

    /// Last focused row path observed by `probe_current_with_db()`. This lets
    /// the event loop distinguish a genuinely settled focus from rapid cursor
    /// movement across rows that do not themselves need a cold probe.
    last_focus_path: Option<PathBuf>,

    /// Timestamp of the most recent focused-row path change. While this short
    /// cooldown is active, background Browse completions and warm-cache merges
    /// are deferred so PgUp/PgDn/key-repeat scrolling cannot periodically
    /// rebuild the visible list or info pane.
    last_focus_movement_at: Option<Instant>,

    /// Coalesced expensive work requested by async completions during the
    /// current reducer batch. The event loop flushes these flags once after
    /// draining messages so bursts of probe/search/classification updates do
    /// not repeatedly re-sort, re-search, or re-enter the current probe path.
    pub deferred_work: BrowseDeferredWorkFlags,

    /// Warmed SQLite rows waiting for bounded reducer-side merge. The DB query
    /// runs on a blocking worker, but merging thousands of rows can still be
    /// visible; process this queue in small frame-sized slices.
    probe_cache_warm_pending: VecDeque<ProbeCacheWarmBatch>,

    /// Cold filesystem Browse probes waiting for an in-flight slot. Cache hits
    /// may populate display data immediately, but worker-side metadata
    /// enrichment is launched only from the settled-focus debounce path.
    browse_cold_probe_queue: VecDeque<BrowseColdProbeRequest>,

    /// Cold filesystem Browse probes that have actually started. This is a
    /// subset of `probe_pending` and provides backpressure across distinct
    /// paths during rapid scrolling. Active jobs carry cancellation handles so
    /// rapid cursor movement can ask stale ffmpeg/tag workers to stop before
    /// doing more work and suppress late completions if the worker cannot be
    /// interrupted immediately.
    browse_cold_probe_active: HashMap<PathBuf, BrowseColdProbeActiveJob>,

    /// Cancellation handles for all filesystem browse probes, including
    /// metadata-enrichment jobs for SQLite cache hits.
    probe_cancel: HashMap<PathBuf, Arc<AtomicBool>>,

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

    /// Folder-summary rollup cache with explicit validation scope. This is the
    /// fast info-pane path for performance mode: cursor movement can read this
    /// cache immediately, while any missing cold work remains debounce-gated
    /// and policy-controlled.
    directory_summary_cache: HashMap<PathBuf, DirectorySummaryCacheEntry>,

    /// Negative cache for settled-focus SQLite directory-summary misses. This
    /// prevents `CachedOnly` hover from issuing the same DB point lookup every
    /// debounce interval for unchanged paths while still allowing a changed
    /// directory identity to be checked again.
    directory_summary_db_miss_cache: HashMap<PathBuf, ProbeCacheIdentity>,

    /// Cross-session directory-summary persistence. This is deliberately
    /// identity-keyed and scope-aware: shallow facts are trusted only for the
    /// exact focused directory identity, while restored recursive stats remain
    /// labeled best-effort.
    directory_summary_persistent_cache_mode: DirectorySummaryPersistentCacheMode,
    directory_summary_persistent_cache_path: Option<PathBuf>,

    /// Policy controlling whether uncached subdirectory summary walks are
    /// allowed from hover/focus. The default preserves the rich summary UX;
    /// performance mode can switch to cached-only or descend-only behavior.
    pub directory_summary_cold_work_policy: BrowseDirectorySummaryColdWorkPolicy,

    /// Independent policy for recursive directory stats. Classification and
    /// native disc detection can remain available while stats are disabled or
    /// bounded separately.
    pub directory_stats_cold_work_policy: BrowseDirectoryStatsColdWorkPolicy,

    /// Bounded folder-content classifications keyed by path and directory
    /// identity. These classify folder shape and extension-only audio counts;
    /// they never run ffmpeg, tag readers, or disc magic-byte probes.
    folder_classification_cache: HashMap<PathBuf, FolderClassificationCacheEntry>,

    /// Folder classifications currently queued or running. Used to debounce
    /// cursor focus and avoid duplicate bounded walks for the same path.
    folder_classification_pending: std::collections::HashSet<PathBuf>,

    /// Folder classifications waiting for the same debounce/cursor-stale
    /// machinery as Browse audio probes. Kept tiny because only cursor-focused
    /// classifications are useful on highlight.
    folder_classification_queue: VecDeque<FolderClassifyRequest>,

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

    /// Active disc probe jobs with cancellation tokens and metadata.
    pub disc_probe_active: HashMap<PathBuf, DiscProbeActiveJob>,

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

    /// Construct a staging session wrapped in a panic-safe cleanup guard.
    ///
    /// This constructor is intentionally available in normal library builds:
    /// Rust integration tests link this crate as a dependency rather than
    /// compiling the library with the crate's internal `cfg(test)` items. Tests
    /// that create archive staging outside `AppState` should use this guard so
    /// panic/unwind paths do not orphan `tonepoet-archive-*` directories.
    pub fn new_test_owned(
        staging_dir: PathBuf,
        archive_path: PathBuf,
        archive_mtime_secs: i64,
        archive_mtime_nanos: u32,
        archive_size: u64,
    ) -> TestArchiveStagingSession {
        TestArchiveStagingSession::new(Self::new(
            staging_dir,
            archive_path,
            archive_mtime_secs,
            archive_mtime_nanos,
            archive_size,
        ))
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

/// Panic-safe owner for archive staging sessions created by tests.
///
/// This intentionally wraps rather than changes `ArchiveStagingSession` itself:
/// production recovery must be able to preserve dirty staging directories across
/// process exit, while tests need a local RAII owner at the true Browse staging
/// boundary. Use `into_inner()` only when handing the session to production app
/// state or recovery code that assumes responsibility for the staging tree.
#[must_use = "hold this guard for the full test scope so panic paths remove the staging directory"]
#[derive(Debug)]
pub struct TestArchiveStagingSession {
    session: Option<ArchiveStagingSession>,
    staging_dir: PathBuf,
}

impl TestArchiveStagingSession {
    pub fn new(session: ArchiveStagingSession) -> Self {
        let staging_dir = session.staging_dir.clone();
        Self {
            session: Some(session),
            staging_dir,
        }
    }

    /// Hand the session to a longer-lived production owner and disarm cleanup.
    pub fn into_inner(mut self) -> ArchiveStagingSession {
        self.session
            .take()
            .expect("test archive staging guard consumed at most once")
    }

    pub fn staging_dir(&self) -> &Path {
        &self.staging_dir
    }

    /// Clone the wrapped session while keeping this guard armed.
    ///
    /// This is the right tool for BrowseState-only tests: install the clone
    /// into `BrowseState`, keep the guard in scope until the assertion block
    /// ends, and the guard will remove the staging tree even if the test
    /// panics before explicit Browse cleanup runs.
    pub fn clone_session(&self) -> ArchiveStagingSession {
        self.session
            .as_ref()
            .expect("test archive staging guard has not been consumed")
            .clone()
    }

    /// Install a clone into an already-entered Browse archive without
    /// transferring cleanup ownership away from this guard.
    pub fn install_clone_into_browse_state(&self, state: &mut BrowseState) -> Result<(), &'static str> {
        let archive = state.archive.as_mut().ok_or("BrowseState is not inside an archive")?;
        archive.staging = Some(self.clone_session());
        Ok(())
    }
}

impl std::ops::Deref for TestArchiveStagingSession {
    type Target = ArchiveStagingSession;

    fn deref(&self) -> &Self::Target {
        self.session
            .as_ref()
            .expect("test archive staging guard has not been consumed")
    }
}

impl std::ops::DerefMut for TestArchiveStagingSession {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.session
            .as_mut()
            .expect("test archive staging guard has not been consumed")
    }
}

impl Drop for TestArchiveStagingSession {
    fn drop(&mut self) {
        if self.session.is_some() {
            match fs::remove_dir_all(&self.staging_dir) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => log::debug!(
                    "test cleanup failed to remove archive staging dir {}: {err}",
                    self.staging_dir.display()
                ),
            }
        }
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

/// Result of directory-scoping raw multi-select marks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopedMultiSelection {
    pub paths: Vec<PathBuf>,
    pub dropped_stale_count: usize,
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
            probe_cache_scan_identity_index: HashMap::new(),
            entries: Vec::new(),
            selected_index: 0,
            scroll_offset: 0,
            visible_height: 0,
            multi_selected: Vec::new(),
            multi_select_anchor: None,
            selection_mode: SelectionMode::Normal,
            drag_state: BrowseDragState::default(),
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
            last_focus_path: None,
            last_focus_movement_at: None,
            deferred_work: BrowseDeferredWorkFlags::default(),
            probe_cache_warm_pending: VecDeque::new(),
            browse_cold_probe_queue: VecDeque::new(),
            browse_cold_probe_active: HashMap::new(),
            probe_cancel: HashMap::new(),
            probe_pending: std::collections::HashSet::new(),
            transient_probe_failures: HashMap::new(),
            archive_probe_epochs: HashMap::new(),
            dir_stats_cache: HashMap::new(),
            dir_stats_pending: std::collections::HashSet::new(),
            dir_stats_active: HashMap::new(),
            dir_stats_queue: VecDeque::new(),
            directory_summary_cache: HashMap::new(),
            directory_summary_db_miss_cache: HashMap::new(),
            directory_summary_persistent_cache_mode: DirectorySummaryPersistentCacheMode::default(),
            directory_summary_persistent_cache_path: None,
            directory_summary_cold_work_policy: BrowseDirectorySummaryColdWorkPolicy::default(),
            directory_stats_cold_work_policy: BrowseDirectoryStatsColdWorkPolicy::default(),
            folder_classification_cache: HashMap::new(),
            folder_classification_pending: std::collections::HashSet::new(),
            folder_classification_queue: VecDeque::new(),
            sacd_classify_cache: HashMap::new(),
            dvda_iso_classify_cache: HashMap::new(),
            dvda_dir_classify_cache: HashMap::new(),
            dvdv_iso_classify_cache: HashMap::new(),
            dvdv_dir_classify_cache: HashMap::new(),
            bluray_iso_classify_cache: HashMap::new(),
            bluray_dir_classify_cache: HashMap::new(),
            disc_probe_cache: HashMap::new(),
            disc_probe_pending: std::collections::HashSet::new(),
            disc_probe_active: HashMap::new(),
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
        if same_path(&self.current_dir, &path) {
            self.current_dir = path;
            self.refresh();
            return;
        }
        self.invalidate_path_validation();
        self.current_dir = path;
        self.selected_index = 0;
        self.reset_nav_state();
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
            // Synchronous fallback (initial scan before tx is set). Keep this
            // path as cheap as the async scan: do not open every ISO or disc
            // marker in the directory merely to populate a listing. Native disc
            // source promotion happens lazily after focus settles.
            self.scan();
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
        self.probe_cache_scan_identity_index.clear();
        self.entries.clear();
        self.error = None;
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.probe_debounce = None;
        self.clear_probe_cache_warm_backlog();
        self.clear_browse_cold_probe_queue();
        self.clear_dir_stats_work_queue();
        self.clear_folder_classification_work_queue();

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

    fn rebuild_probe_cache_scan_identity_index(&mut self) {
        let mut index = HashMap::with_capacity(
            self.parent_entry.iter().count()
                + self.all_dirs.len()
                + self.all_files.len(),
        );
        if let Some(parent) = &self.parent_entry {
            index.insert(parent.path.clone(), ProbeCacheIdentity::from_entry(parent));
        }
        for entry in self.all_dirs.iter().chain(self.all_files.iter()) {
            index.insert(entry.path.clone(), ProbeCacheIdentity::from_entry(entry));
        }
        self.probe_cache_scan_identity_index = index;
    }

    pub(super) fn publish_scanned_entries(
        &mut self,
        parent_entry: Option<BrowseEntry>,
        dirs: Vec<BrowseEntry>,
        files: Vec<BrowseEntry>,
    ) {
        self.parent_entry = parent_entry;
        self.all_dirs = dirs;
        self.all_files = files;
        self.rebuild_probe_cache_scan_identity_index();
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
        self.clear_multi_selection();
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.clear_dir_stats_work_queue();
        self.clear_folder_classification_work_queue();
        self.clear_type_ahead();
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
        self.clear_multi_selection();
        self.clear_dir_stats_work_queue();
        self.clear_folder_classification_work_queue();
        self.clear_type_ahead();
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
        self.clear_multi_selection();
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.refresh();
    }

    /// Navigate into a subdirectory inside the archive.
    pub fn enter_archive_dir(&mut self, dir_path: &str) {
        self.invalidate_path_validation();
        self.close_search_for_navigation();
        if let Some(ref mut arc) = self.archive {
            arc.inner_path = dir_path.to_string();
        }
        self.clear_multi_selection();
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.clear_dir_stats_work_queue();
        self.clear_folder_classification_work_queue();
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
            self.clear_multi_selection();
            self.selected_index = 0;
            self.scroll_offset = 0;
            self.clear_dir_stats_work_queue();
            self.clear_folder_classification_work_queue();
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
        self.probe_cache_scan_identity_index.clear();
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
        let items = arc.listing.entries_at(&arc.inner_path);
        let mut listing_paths = HashSet::new();
        for item in &items {
            listing_paths.insert(item.full_path.clone());
            let staged_metadata = arc
                .staging
                .as_ref()
                .and_then(|staging| {
                    staging_path_for_archive_inner(&staging.staging_dir, &item.full_path).ok()
                })
                .and_then(|path| fs::metadata(path).ok());
            let kind = if item.is_dir {
                EntryKind::Directory
            } else if staged_metadata.as_ref().is_some_and(|metadata| metadata.is_dir()) {
                EntryKind::Directory
            } else if let Some(staging) = arc.staging.as_ref() {
                staging_path_for_archive_inner(&staging.staging_dir, &item.full_path)
                    .ok()
                    .map(|path| classify_file(&path))
                    .unwrap_or_else(|| classify_file(Path::new(&item.name)))
            } else {
                classify_file(Path::new(&item.name))
            };
            let size = if matches!(kind, EntryKind::Directory) {
                0
            } else {
                staged_metadata.as_ref().map_or(item.size, |metadata| metadata.len())
            };
            let modified = staged_metadata.and_then(|metadata| metadata.modified().ok());
            let entry_is_dir = item.is_dir || matches!(kind, EntryKind::Directory);
            let entry = BrowseEntry::new(
                arc.listing.archive_path.join(&item.full_path),
                item.name.clone(),
                kind,
                size,
                modified,
            );
            if entry_is_dir {
                dirs.push(entry);
            } else {
                files.push(entry);
            }
        }

        if let Some(staging) = arc.staging.as_ref() {
            let current_dir = staging.staging_dir.join(&arc.inner_path);
            if let Ok(read_dir) = fs::read_dir(&current_dir) {
                for child in read_dir.flatten() {
                    let path = child.path();
                    let Ok(meta) = child.metadata() else {
                        continue;
                    };
                    let name = child.file_name().to_string_lossy().into_owned();
                    let inner = if arc.inner_path.is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{}", arc.inner_path, name)
                    };
                    if listing_paths.contains(&inner) {
                        continue;
                    }
                    let is_dir = meta.is_dir();
                    let kind = if is_dir {
                        EntryKind::Directory
                    } else {
                        classify_file(&path)
                    };
                    let entry = BrowseEntry::new(
                        arc.listing.archive_path.join(inner),
                        name,
                        kind,
                        if is_dir { 0 } else { meta.len() },
                        meta.modified().ok(),
                    );
                    if is_dir {
                        dirs.push(entry);
                    } else {
                        files.push(entry);
                    }
                }
            }
        }

        self.all_dirs = dirs;
        self.all_files = files;
        self.rebuild_probe_cache_scan_identity_index();
    }

    /// Read the directory from disk into `parent_entry` / `all_dirs` / `all_files`.
    /// Stores ALL entries (including hidden) — view-layer filters apply later.
    /// Slow; only call on cd or explicit refresh.
    fn scan(&mut self) {
        self.parent_entry = None;
        self.all_dirs.clear();
        self.all_files.clear();
        self.probe_cache_scan_identity_index.clear();
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

        self.rebuild_probe_cache_scan_identity_index();
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
    /// This legacy helper remains for explicit callers/tests. Directory scans
    /// no longer invoke it automatically; hover-time native-disc promotion is
    /// debounce-gated instead.
    #[allow(dead_code)] // Classification moved to scan worker; retained for test coverage
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
    #[allow(dead_code)] // Classification moved to scan worker; retained for test coverage
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
                if same_path(&self.current_dir, &path) {
                    self.refresh();
                    return true;
                }
                self.invalidate_path_validation();
                self.push_nav_history(path.clone());
                self.current_dir = path;
                self.selected_index = 0;
                self.reset_nav_state();
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
            if same_path(&self.current_dir, &parent) {
                self.refresh();
                return true;
            }
            self.invalidate_path_validation();
            self.push_nav_history(parent.clone());
            self.current_dir = parent;
            self.reset_nav_state();
            self.sync_tree_to_current_dir();
            self.refresh();
            return true;
        }
        false
    }

    /// Navigate directly to a given path
    pub fn navigate_to(&mut self, path: PathBuf) {
        if path.is_dir() {
            if same_path(&self.current_dir, &path) {
                self.current_dir = path;
                self.refresh();
                return;
            }
            self.invalidate_path_validation();
            self.push_nav_history(path.clone());
            self.current_dir = path;
            self.selected_index = 0;
            self.reset_nav_state();
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
            if same_path(&self.current_dir, &final_path) {
                self.current_dir = final_path;
                self.refresh();
                return Ok(());
            }
            self.invalidate_path_validation();
            self.push_nav_history(final_path.clone());
            self.current_dir = final_path;
            self.selected_index = 0;
            self.reset_nav_state();
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

    fn selectable_entry_path_at(&self, index: usize) -> Option<PathBuf> {
        self.entries
            .get(index)
            .filter(|entry| !matches!(entry.kind, EntryKind::ParentDir))
            .map(|entry| entry.path.clone())
    }

    fn selected_path_index(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|entry| entry.path == path)
    }

    fn range_paths_between(&self, a: usize, b: usize) -> Vec<PathBuf> {
        let lo = a.min(b);
        let hi = a.max(b);
        (lo..=hi)
            .filter_map(|idx| self.selectable_entry_path_at(idx))
            .collect()
    }

    fn push_unique_selection(&mut self, path: PathBuf) {
        if !self.multi_selected.iter().any(|selected| selected == &path) {
            self.multi_selected.push(path);
        }
    }

    fn discard_multi_select_anchor_if_unselected(&mut self) {
        if self
            .multi_select_anchor
            .as_ref()
            .is_some_and(|anchor| !self.multi_selected.iter().any(|selected| selected == anchor))
        {
            self.multi_select_anchor = None;
        }
    }

    /// Toggle multi-select on the entry at `index` without changing the cursor.
    /// ParentDir is deliberately ignored because it is a navigation pseudo-row.
    pub fn toggle_selection_at_index(&mut self, index: usize) -> Option<PathBuf> {
        let path = self.selectable_entry_path_at(index)?;
        if let Some(pos) = self.multi_selected.iter().position(|p| p == &path) {
            self.multi_selected.remove(pos);
            if self.multi_select_anchor.as_ref().is_some_and(|anchor| anchor == &path) {
                self.multi_select_anchor = None;
            }
        } else {
            self.multi_selected.push(path.clone());
            self.multi_select_anchor = Some(path.clone());
        }
        Some(path)
    }

    /// Toggle multi-select on the current entry.
    pub fn toggle_selection(&mut self) {
        let _ = self.toggle_selection_at_index(self.selected_index);
    }

    pub fn is_multi_selected(&self, path: &Path) -> bool {
        self.multi_selected.iter().any(|p| p.as_path() == path)
    }

    fn path_is_in_current_mark_scope(&self, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };

        let in_directory_context = if let Some(arc) = self.archive.as_ref() {
            let expected_parent = if arc.inner_path.is_empty() {
                arc.listing.archive_path.clone()
            } else {
                arc.listing.archive_path.join(&arc.inner_path)
            };
            parent == expected_parent.as_path()
        } else {
            parent == self.current_dir.as_path()
        };
        if in_directory_context {
            return true;
        }

        // Recursive search replaces `entries` with hits from nested
        // subdirectories; a mark the user made on a listed entry is visible
        // and deliberate, so it stays actionable even though its parent is
        // not the current directory. Stale cross-directory marks are never
        // in `entries`, so this keeps the incident fix intact.
        self.entries.iter().any(|entry| entry.path.as_path() == path)
    }

    /// Return only marks that belong to the directory context currently shown
    /// by Browse. This is the consumer-side safety invariant for multi-select:
    /// a mark is only ever visible-and-actionable in the directory that
    /// contains it. The filter is deterministic and filesystem-free; it
    /// preserves mark order and reports how many stale cross-directory paths
    /// were removed from the actionable view.
    pub(crate) fn scoped_multi_selected_in_current_directory(&self) -> ScopedMultiSelection {
        let paths: Vec<PathBuf> = self
            .multi_selected
            .iter()
            .filter(|path| self.path_is_in_current_mark_scope(path))
            .cloned()
            .collect();
        let dropped_stale_count = self.multi_selected.len().saturating_sub(paths.len());
        ScopedMultiSelection {
            paths,
            dropped_stale_count,
        }
    }

    /// Backward-compatible convenience for callers that only need the scoped
    /// path list. New file-operation consumers should prefer
    /// `scoped_multi_selected_in_current_directory()` so they can surface stale
    /// drops to the user.
    pub(crate) fn multi_selected_in_current_directory(&self) -> Vec<PathBuf> {
        self.scoped_multi_selected_in_current_directory().paths
    }

    /// Remove stale cross-directory marks from raw Browse state and return the
    /// number removed. This is the mutating form of the mark-scope invariant:
    /// once any consumer detects stale marks, the raw selection state is
    /// repaired immediately so Esc, select-all/invert, anchor handling, and
    /// later cursor fallback all observe the same scoped selection.
    pub(crate) fn prune_stale_multi_selection_for_current_directory(&mut self) -> usize {
        let scoped = self.scoped_multi_selected_in_current_directory();
        if scoped.dropped_stale_count == 0 {
            return 0;
        }
        self.multi_selected = scoped.paths;
        self.discard_multi_select_anchor_if_unselected();
        self.selection_mode = SelectionMode::Normal;
        self.drag_state.active = false;
        scoped.dropped_stale_count
    }

    /// Raw, unexpanded Browse action selection for async freshness snapshots
    /// and command/file-op consumers. Pure snapshot callers preserve the
    /// historical rule that raw mark mode suppresses cursor fallback, while
    /// mutating file-operation consumers first call
    /// `prune_stale_multi_selection_for_current_directory()` so stale-only raw
    /// marks are repaired before fallback is decided.
    pub(crate) fn action_selection_in_current_directory(&self) -> Vec<PathBuf> {
        if !self.multi_selected.is_empty() {
            return self.multi_selected_in_current_directory();
        }
        if let Some(entry) = self.selected_entry() {
            if !matches!(entry.kind, EntryKind::ParentDir) {
                return vec![entry.path.clone()];
            }
        }
        Vec::new()
    }

    pub fn is_range_mode(&self) -> bool {
        matches!(self.selection_mode, SelectionMode::Range { .. })
    }

    pub fn is_range_preview_index(&self, index: usize) -> bool {
        match self.selection_mode {
            SelectionMode::Range { anchor_index, .. } => {
                let lo = anchor_index.min(self.selected_index);
                let hi = anchor_index.max(self.selected_index);
                (lo..=hi).contains(&index)
                    && self
                        .entries
                        .get(index)
                        .is_some_and(|entry| !matches!(entry.kind, EntryKind::ParentDir))
            }
            SelectionMode::Normal => false,
        }
    }

    pub fn begin_range_selection(&mut self) {
        self.begin_range_selection_at(self.selected_index);
    }

    pub fn begin_range_selection_at(&mut self, anchor_index: usize) {
        self.prune_stale_multi_selection_for_current_directory();
        if !self.is_range_mode() {
            self.selection_mode = SelectionMode::Range {
                anchor_index: anchor_index.min(self.entries.len().saturating_sub(1)),
                pre_range_selection: self.multi_selected.clone(),
            };
        }
        self.update_range_preview();
    }

    pub fn update_range_preview(&mut self) {
        let anchor_index = match self.selection_mode {
            SelectionMode::Range { anchor_index, .. } => anchor_index,
            SelectionMode::Normal => return,
        };
        self.multi_selected = self.range_paths_between(anchor_index, self.selected_index);
    }

    pub fn commit_range_selection(&mut self) -> bool {
        let (anchor_index, mut committed) = match std::mem::replace(
            &mut self.selection_mode,
            SelectionMode::Normal,
        ) {
            SelectionMode::Range {
                anchor_index,
                pre_range_selection,
            } => (anchor_index, pre_range_selection),
            SelectionMode::Normal => {
                self.selection_mode = SelectionMode::Normal;
                return false;
            }
        };
        for path in self.range_paths_between(anchor_index, self.selected_index) {
            if !committed.iter().any(|selected| selected == &path) {
                committed.push(path);
            }
        }
        self.multi_selected = committed;
        if let Some(path) = self.selectable_entry_path_at(self.selected_index) {
            self.multi_select_anchor = Some(path);
        }
        true
    }

    pub fn cancel_range_selection(&mut self) -> bool {
        match std::mem::replace(&mut self.selection_mode, SelectionMode::Normal) {
            SelectionMode::Range {
                pre_range_selection,
                ..
            } => {
                self.multi_selected = pre_range_selection;
                self.discard_multi_select_anchor_if_unselected();
                true
            }
            SelectionMode::Normal => {
                self.selection_mode = SelectionMode::Normal;
                false
            }
        }
    }

    pub fn extend_selection_from_anchor_to_index(&mut self, index: usize) {
        let anchor_index = self
            .multi_select_anchor
            .as_ref()
            .and_then(|path| self.selected_path_index(path))
            .unwrap_or(self.selected_index);
        for path in self.range_paths_between(anchor_index, index) {
            self.push_unique_selection(path);
        }
        if let Some(path) = self.selectable_entry_path_at(index) {
            self.multi_select_anchor = Some(path);
        }
    }

    pub fn toggle_all_visible_selection(&mut self) {
        self.prune_stale_multi_selection_for_current_directory();
        let visible_paths: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|entry| !matches!(entry.kind, EntryKind::ParentDir))
            .map(|entry| entry.path.clone())
            .collect();
        if visible_paths.is_empty() {
            return;
        }
        let all_selected = visible_paths
            .iter()
            .all(|path| self.multi_selected.iter().any(|selected| selected == path));
        if all_selected {
            self.multi_selected
                .retain(|selected| !visible_paths.iter().any(|path| path == selected));
        } else {
            for path in visible_paths {
                self.push_unique_selection(path);
            }
        }
        self.discard_multi_select_anchor_if_unselected();
        self.selection_mode = SelectionMode::Normal;
    }

    pub fn invert_visible_selection(&mut self) {
        self.prune_stale_multi_selection_for_current_directory();
        let visible_paths: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|entry| !matches!(entry.kind, EntryKind::ParentDir))
            .map(|entry| entry.path.clone())
            .collect();
        for path in visible_paths {
            if let Some(pos) = self.multi_selected.iter().position(|selected| selected == &path) {
                self.multi_selected.remove(pos);
            } else {
                self.multi_selected.push(path);
            }
        }
        self.discard_multi_select_anchor_if_unselected();
        self.selection_mode = SelectionMode::Normal;
    }

    pub fn clear_multi_selection(&mut self) {
        self.multi_selected.clear();
        self.multi_select_anchor = None;
        self.selection_mode = SelectionMode::Normal;
        self.drag_state.active = false;
    }

    fn multi_selected_disc_source_dir_roots_for(&self, selected_paths: &[PathBuf]) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        let mut root_keys = HashSet::new();
        for selected_path in selected_paths {
            let selected_entry = self
                .entries
                .iter()
                .find(|entry| same_path(&entry.path, selected_path));
            if selected_entry
                .map(|entry| entry.is_disc_source() && entry.is_child_dir())
                .unwrap_or(false)
            {
                push_unique_path_with_keys(&mut roots, &mut root_keys, selected_path.clone());
            }
        }
        roots
    }

    /// Backward-compatible name for older callers; range mode now owns the
    /// pre-range snapshot and preview semantics.
    pub fn update_visual_selection(&mut self) {
        self.update_range_preview();
    }

    /// Collect paths for an enqueue operation (`:queue` / `:convert` etc).
    ///
    /// - If `multi_selected` is non-empty, expands ordinary directories into
    ///   their audio file contents while preserving classified disc-source
    ///   directories as atomic disc roots for conversion routing.
    /// - Otherwise, if the cursor is on an audio file, archive, or
    ///   directory, returns it. Plain directories are expanded; classified
    ///   disc-source directories are preserved as disc roots so conversion
    ///   routing can handle them as disc sources.
    /// - Returns an empty vec if nothing valid is selected.
    ///
    /// The expansion helper (`expand_paths_to_audio`) is screen-agnostic
    /// so Library and future screens can reuse the same logic.
    pub fn collect_selection_for_queue(&self) -> QueueExpansionResult {
        if !self.multi_selected.is_empty() {
            let selected_paths = self.multi_selected_in_current_directory();
            if selected_paths.is_empty() {
                return QueueExpansionResult::default();
            }
            let preserved_disc_roots = self.multi_selected_disc_source_dir_roots_for(&selected_paths);
            if preserved_disc_roots.is_empty() {
                return expand_paths_to_audio_with_metadata(&selected_paths);
            }
            return expand_paths_to_audio_with_preserved_disc_roots(
                &selected_paths,
                &preserved_disc_roots,
            );
        }
        if let Some(entry) = self.selected_entry() {
            match &entry.kind {
                EntryKind::AudioFile(_) | EntryKind::Archive => {
                    return QueueExpansionResult {
                        paths: vec![entry.path.clone()],
                        cue_artifact_audio: HashSet::new(),
                    };
                }
                EntryKind::OtherFile if is_cue_sheet_path(&entry.path) => {
                    return QueueExpansionResult {
                        paths: vec![entry.path.clone()],
                        cue_artifact_audio: HashSet::new(),
                    };
                }
                EntryKind::Directory => {
                    return expand_paths_to_audio_with_metadata(&[entry.path.clone()]);
                }
                EntryKind::DvdAudioDir | EntryKind::DvdVideoDir | EntryKind::BlurayDir => {
                    return QueueExpansionResult {
                        paths: vec![entry.path.clone()],
                        cue_artifact_audio: HashSet::new(),
                    };
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
            // With an app message channel, tag-aware archive searches run on the
            // async extraction worker. Without one (unit tests and explicit
            // synchronous callers), `execute_search` already uses its bounded
            // synchronous fallback; do not merely mark a debounce timestamp and
            // leave stale results visible after probe metadata arrives.
            self.execute_search(tx);
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
        self.execute_search_over_entries(
            query,
            show_hidden,
            audio_only,
            format_filter,
            mode,
            sources,
        );
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
        // Staging sessions can be attached after the archive listing was first
        // rendered (for example by recovery or metadata-edit reducers). Rebuild
        // the raw archive model before synchronous search so tag fallback sees
        // the synthetic archive-member entries and their archive identities, not
        // only the transient staging directory shape.
        if self.archive.as_ref().and_then(|arc| arc.staging.as_ref()).is_some() {
            self.rebuild_archive_raw_entries();
        }

        let sources = self.archive_search_source_entries(recursive, mode);
        self.search.searching = false;
        self.search.cancel = None;
        self.execute_search_over_entries(
            query,
            show_hidden,
            audio_only,
            format_filter,
            mode,
            sources,
        );
    }

    fn archive_search_requires_async(&self, mode: SearchMode, sort: SearchSort) -> bool {
        self.archive.is_some()
            && (matches!(mode, SearchMode::Tags | SearchMode::Both) || sort.is_tag_sort())
    }

    fn archive_search_source_entries(&self, recursive: bool, mode: SearchMode) -> Vec<BrowseEntry> {
        if recursive {
            return self.archive_recursive_search_entries();
        }

        let mut entries = Vec::with_capacity(self.all_dirs.len() + self.all_files.len());
        entries.extend(self.all_dirs.iter().cloned());
        entries.extend(self.all_files.iter().cloned());

        // Archive staging exposes a filesystem tree whose immediate root may be
        // only synthetic directories while the archive listing/probe cache owns
        // the real audio-member identities below them. In that exact tag-search
        // case, widen the candidate set to listing descendants so metadata
        // fallback remains useful without weakening ordinary filename browsing.
        let staged_archive_tag_search_without_current_audio = self
            .archive
            .as_ref()
            .and_then(|arc| arc.staging.as_ref())
            .is_some()
            && matches!(mode, SearchMode::Tags | SearchMode::Both)
            && entries.iter().all(|entry| !entry.is_audio());
        if staged_archive_tag_search_without_current_audio {
            return self.archive_recursive_search_entries();
        }

        entries
    }

    fn archive_search_candidates(
        &self,
        recursive: bool,
        mode: SearchMode,
    ) -> Vec<ArchiveSearchCandidate> {
        let sources = self.archive_search_source_entries(recursive, mode);

        sources
            .into_iter()
            .map(|entry| {
                let inner_path = self.archive_inner_path_for_entry(&entry);
                let staged_path = self.archive_staged_path_for_entry(&entry);
                let fallback_metadata = self
                    .valid_archive_probe_for_entry(&entry)
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
        if self.archive.as_ref().and_then(|arc| arc.staging.as_ref()).is_some() {
            self.rebuild_archive_raw_entries();
        }
        let candidates = self.archive_search_candidates(recursive, mode);
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
                .valid_archive_probe_for_entry(entry)
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
        let query_lower = query.to_lowercase();
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
            if search_filename || e.is_navigable_dir() {
                if let Some(s) = search_exact_substring_score(&e.name_lower, &query_lower)
                    .or_else(|| matcher.fuzzy_match(&e.name_lower, query))
                {
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
        let mut listing_paths = HashSet::new();

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
            listing_paths.insert(item.path.clone());
            let staged_metadata = arc
                .staging
                .as_ref()
                .and_then(|staging| {
                    staging_path_for_archive_inner(&staging.staging_dir, &item.path).ok()
                })
                .and_then(|path| fs::metadata(path).ok());
            let kind = if item.is_dir {
                EntryKind::Directory
            } else if staged_metadata.as_ref().is_some_and(|metadata| metadata.is_dir()) {
                EntryKind::Directory
            } else if let Some(staging) = arc.staging.as_ref() {
                staging_path_for_archive_inner(&staging.staging_dir, &item.path)
                    .ok()
                    .map(|path| classify_file(&path))
                    .unwrap_or_else(|| classify_file(Path::new(&item.path)))
            } else {
                classify_file(Path::new(&item.path))
            };
            let size = if matches!(kind, EntryKind::Directory) {
                0
            } else {
                staged_metadata.as_ref().map_or(item.size, |metadata| metadata.len())
            };
            let modified = staged_metadata.and_then(|metadata| metadata.modified().ok());
            let entry_is_dir = item.is_dir || matches!(kind, EntryKind::Directory);
            let entry = BrowseEntry::new(
                arc.listing.archive_path.join(&item.path),
                display_name,
                kind,
                size,
                modified,
            );
            if entry_is_dir {
                dirs.push(entry);
            } else {
                files.push(entry);
            }
        }

        if let Some(staging) = arc.staging.as_ref() {
            let root = staging.staging_dir.join(&arc.inner_path);
            for entry in walkdir::WalkDir::new(&root)
                .min_depth(1)
                .into_iter()
                .filter_map(Result::ok)
            {
                let path = entry.path().to_path_buf();
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let Ok(relative_to_staging) = path.strip_prefix(&staging.staging_dir) else {
                    continue;
                };
                let Some(inner) = normalize_archive_relative_path(relative_to_staging) else {
                    continue;
                };
                if inner.is_empty() || listing_paths.contains(&inner) {
                    continue;
                }
                let display_name = inner
                    .strip_prefix(&prefix)
                    .unwrap_or(&inner)
                    .to_string();
                if display_name.is_empty() {
                    continue;
                }
                let is_dir = meta.is_dir();
                let kind = if is_dir {
                    EntryKind::Directory
                } else {
                    classify_file(&path)
                };
                let entry = BrowseEntry::new(
                    arc.listing.archive_path.join(&inner),
                    display_name,
                    kind,
                    if is_dir { 0 } else { meta.len() },
                    meta.modified().ok(),
                );
                if is_dir {
                    dirs.push(entry);
                } else {
                    files.push(entry);
                }
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
                    let query_lower = query_for_worker.to_lowercase();
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

                        if search_filename || candidate.is_navigable_dir() {
                            if let Some(s) = search_exact_substring_score(&candidate.name_lower, &query_lower)
                                .or_else(|| matcher.fuzzy_match(&candidate.name_lower, &query_for_worker))
                            {
                                best_score = Some(best_score.map_or(s, |prev: i64| prev.max(s)));
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

    /// Reset all per-directory Browse state after a real directory-context
    /// change. Multi-select marks, their anchor, and any in-progress range or
    /// drag selection are directory-scoped: a mark is only ever
    /// visible-and-actionable in the directory that contains it. Callers that
    /// re-enter the same directory should not call this; same-directory
    /// navigation is a no-op for marks.
    fn reset_nav_state(&mut self) {
        self.close_search_for_navigation();
        self.reset_filter_state();
        self.clear_multi_selection();
        self.clear_type_ahead();
        self.clear_pending_inline_rename_after_scan();
        self.probe_debounce = None;
        self.clear_browse_cold_probe_queue();
        self.clear_dir_stats_work_queue();
        self.clear_folder_classification_work_queue();
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

    /// Best-effort current filesystem identity for settled or asynchronous work.
    ///
    /// Raw cursor movement must not call this: it uses the identity captured in
    /// `BrowseEntry` during the directory scan. Re-statting happens only after
    /// the debounce window or when accepting async completions.
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

    /// Identity-checked probe info for an archive entry. Archive staging can
    /// expose a temporary filesystem copy whose size/mtime differs from the
    /// logical archive member; tag search must keep using the synthetic archive
    /// path and archive-member identity rather than the staging-file identity.
    fn valid_archive_probe_for_entry(&self, entry: &BrowseEntry) -> Option<&CachedInfo> {
        let Some(archive) = self.archive.as_ref() else {
            return self.valid_probe_for_entry(entry);
        };

        let inner = self.archive_inner_path_for_entry(entry)?;
        let archive_entry = archive
            .listing
            .entries
            .iter()
            .find(|candidate| candidate.path == inner)?;
        let archive_identity = ProbeCacheIdentity {
            modified: None,
            size: archive_entry.size,
        };

        self.valid_probe_arc_for_identity(&entry.path, archive_identity)
            .or_else(|| self.valid_probe_arc_for_entry(entry))
            .map(|info| info.as_ref())
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
            .map(|entry| {
                (entry.is_disc_source() && same_path(&entry.path, path))
                    || self
                        .valid_folder_classification_for_entry(entry)
                        .is_some_and(|classification| {
                            classification.kind == FolderClassificationKind::Disc
                                && same_path(classification.disc_probe_source_path(&entry.path), path)
                        })
            })
            .unwrap_or(false)
    }

    /// Raw cursor-movement variant of `current_selected_disc_source_matches`.
    /// It deliberately uses scan-owned exact paths and never canonicalizes.
    fn current_selected_disc_source_matches_scanned(&self, path: &Path) -> bool {
        self.entries
            .get(self.selected_index)
            .map(|entry| {
                (entry.is_disc_source() && same_scanned_path(&entry.path, path))
                    || self
                        .valid_folder_classification_for_entry(entry)
                        .is_some_and(|classification| {
                            classification.kind == FolderClassificationKind::Disc
                                && same_scanned_path(classification.disc_probe_source_path(&entry.path), path)
                        })
            })
            .unwrap_or(false)
    }

    fn cancel_stale_active_disc_probes(&mut self) {
        let cold_disc_work_allowed = self.native_disc_cold_work_allowed_for_focus();
        let current_generation = self.scan_generation;
        let stale_paths: Vec<PathBuf> = self
            .disc_probe_active
            .iter()
            .filter_map(|(path, job)| {
                let generation_current = job.scan_generation == current_generation;
                let cursor_still_focused = !job.cursor_focused || self.current_selected_disc_source_matches_scanned(path);
                if cold_disc_work_allowed && generation_current && cursor_still_focused {
                    None
                } else {
                    Some(path.clone())
                }
            })
            .collect();
        for path in stale_paths {
            if let Some(job) = self.disc_probe_active.remove(&path) {
                job.cancel.store(true, Ordering::Relaxed);
            }
            self.disc_probe_pending.remove(&path);
        }
    }

    /// Launch the existing disc-probe pipeline for a concrete disc source path.
    ///
    /// This is the single guarded scheduler for both native Browse disc entries
    /// (`BlurayDir`, `SacdIso`, etc.) and directories discovered by the cheap
    /// folder-classification gate. Keeping the pending/cache guard in one place
    /// prevents the info pane from rendering a passive "Analyzing disc..." line
    /// for a focused source with no worker actually in flight, while preserving
    /// idempotency when a probe is already pending or cached for the current
    /// source fingerprint.
    pub fn schedule_disc_probe_for_source_path(
        &mut self,
        probe_path: &Path,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) -> bool {
        if self.disc_probe_pending.contains(probe_path) {
            return false;
        }
        if self
            .disc_probe_cache
            .get(probe_path)
            .is_some_and(|cache| cache.is_current_for(probe_path))
        {
            return false;
        }

        let probe_path = probe_path.to_path_buf();
        let cancel = Arc::new(AtomicBool::new(false));
        self.disc_probe_pending.insert(probe_path.clone());
        self.disc_probe_active.insert(
            probe_path.clone(),
            DiscProbeActiveJob {
                scan_generation: self.scan_generation,
                cursor_focused: true,
                cancel: cancel.clone(),
            },
        );
        crate::tui::disc_browser::spawn_disc_probe(probe_path, cancel, tx.clone());
        true
    }

    pub fn complete_disc_probe(&mut self, path: &Path) -> bool {
        let was_pending = self.disc_probe_pending.remove(path);
        if let Some(job) = self.disc_probe_active.remove(path) {
            job.cancel.store(true, Ordering::Relaxed);
        }
        was_pending
    }

    /// Launch the existing disc-probe pipeline for a directory that the cheap
    /// folder classifier has identified as one disc source. The classifier only
    /// discovers the source; parsing/mapping still stays in the normal async disc
    /// probe path with its own fingerprinted cache and stale-result rejection.
    pub fn schedule_disc_probe_for_folder_classification(
        &mut self,
        folder_path: &Path,
        classification: &FolderContentClassification,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) -> bool {
        if classification.kind != FolderClassificationKind::Disc {
            return false;
        }

        let probe_path = classification.disc_probe_source_path(folder_path).to_path_buf();
        self.schedule_disc_probe_for_source_path(&probe_path, tx)
    }

    /// Bridge the folder-classification cache to the existing disc-probe path.
    ///
    /// `FolderClassifyComplete` may arrive after the cursor has moved away; in
    /// that case the classification is correctly cached but the disc probe is
    /// not launched. When the same directory becomes focused later, this helper
    /// ensures a cached `Disc` classification cannot leave the info pane stuck
    /// on a passive "Analyzing disc..." line with no pending probe.
    pub fn schedule_disc_probe_for_valid_cached_folder_classification(
        &mut self,
        entry: &BrowseEntry,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) -> bool {
        let Some(classification) = self.valid_folder_classification_for_entry(entry).cloned() else {
            return false;
        };
        self.schedule_disc_probe_for_folder_classification(&entry.path, classification.as_ref(), tx)
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

    fn record_focus_path_for_visible_work_debounce(&mut self, path: &Path) {
        if self
            .last_focus_path
            .as_ref()
            .is_some_and(|focused| same_scanned_path(focused, path))
        {
            return;
        }
        self.last_focus_path = Some(path.to_path_buf());
        self.last_focus_movement_at = Some(Instant::now());
    }

    pub fn focus_visible_work_deferred(&self) -> bool {
        let now = Instant::now();
        let focus_recently_moved = self
            .last_focus_movement_at
            .is_some_and(|moved_at| {
                now.saturating_duration_since(moved_at) < BROWSE_PROBE_DEBOUNCE
            });
        let focus_debounce_pending = self
            .probe_debounce
            .as_ref()
            .is_some_and(|pending| now < pending.deadline);
        focus_recently_moved || focus_debounce_pending
    }

    pub fn has_browse_deferred_work(&self) -> bool {
        self.deferred_work.has_expensive_work()
    }

    /// Refresh cheap cache/identity state for the currently selected entry and
    /// arm debounced expensive work when needed.
    ///
    /// Results arrive via `AppMessage::AudioProbeComplete` or
    /// `AppMessage::DirStatsComplete` and the event loop populates the
    /// respective caches. Pending sets prevent duplicate spawns when the
    /// cursor moves rapidly back and forth.
    pub fn probe_current(&mut self, tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>) {
        self.probe_current_with_db(tx, None);
    }

    fn arm_probe_debounce(&mut self, path: PathBuf) {
        if self
            .probe_debounce
            .as_ref()
            .is_some_and(|pending| same_scanned_path(&pending.path, &path))
        {
            return;
        }
        self.probe_debounce = Some(BrowseProbeDebounce {
            path,
            deadline: Instant::now() + BROWSE_PROBE_DEBOUNCE,
        });
    }

    fn probe_current_archive_entry(
        &mut self,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        let Some(entry) = self.entries.get(self.selected_index).cloned() else {
            return;
        };
        if !entry.is_audio() {
            return;
        }

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

    /// Probe the current selection's cheap in-memory state and arm debounced work.
    ///
    /// Raw cursor movement is intentionally filesystem- and SQLite-free: it uses
    /// the identity captured in `BrowseEntry` during the last directory scan and
    /// only consults in-memory caches/pending sets. Recursive directory stats,
    /// folder classification, native disc probes, SQLite probe-cache lookups,
    /// cold ffmpeg probes, and cached-hit metadata enrichment all start from
    /// `check_probe_debounce_with_db()` after focus has rested on the same entry.
    pub fn probe_current_with_db(
        &mut self,
        _tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
        db: Option<&crate::db::Database>,
    ) {
        let mut entry = match self.entries.get(self.selected_index).cloned() {
            Some(e) => e,
            None => return,
        };
        self.record_focus_path_for_visible_work_debounce(&entry.path);

        // Raw cursor movement is the earliest point where we know a previous
        // cursor-focused job has become stale. Cancel what can be cancelled and
        // clear pending markers so stale completions are ignored by reducers.
        self.cancel_stale_active_browse_cold_probes();
        self.cancel_stale_active_disc_probes();
        self.cancel_stale_active_dir_stats_for_focus_movement();

        if self.archive.is_some() {
            if entry.is_audio() {
                let path = entry.path.clone();
                let identity = ProbeCacheIdentity::from_entry(&entry);
                if self.has_valid_probe_cache_entry(&entry, identity)
                    || self.probe_pending.contains(&path)
                {
                    self.probe_debounce = None;
                    return;
                }
                self.arm_probe_debounce(path);
            } else {
                self.probe_debounce = None;
            }
            return;
        }

        let scanned_identity = ProbeCacheIdentity::from_entry(&entry);
        entry = self.lazy_promote_native_disc_source_for_focus(&entry, scanned_identity, false, false);

        if self.archive.is_none()
            && self.probe_pending.contains(&entry.path)
            && std::fs::metadata(&entry.path).is_err()
        {
            let path = entry.path.clone();
            self.remove_probe_cache_entry(&path);
            self.probe_pending.remove(&path);
            self.probe_cancel.remove(&path);
            self.clear_browse_cold_probe_tracking_for(&path);
            self.probe_debounce = None;
            return;
        }

        if entry.is_probeable() {
            let path = entry.path.clone();
            let identity = ProbeCacheIdentity::from_entry(&entry);

            let native_disc_cold_work_allowed = !entry.is_disc_source()
                || self.native_disc_cold_work_allowed_for_focus();

            if let Some(cached) = self.cached_probe_for_entry(&entry, identity) {
                let needs_metadata_enrichment = cached.is_some()
                    && self.probe_cache_needs_metadata_enrichment.contains(&path);
                if needs_metadata_enrichment && native_disc_cold_work_allowed {
                    self.arm_probe_debounce(path);
                } else if entry.is_disc_source() {
                    if self.native_disc_cold_work_allowed_for_focus() {
                        self.arm_probe_debounce(path);
                    } else {
                        self.probe_debounce = None;
                    }
                } else if entry.is_child_dir() {
                    self.arm_probe_debounce(path);
                } else {
                    self.probe_debounce = None;
                }
                return;
            }

            if self.has_recent_transient_probe_failure(&path, identity) {
                self.probe_debounce = None;
                return;
            }

            if self.probe_pending.contains(&path)
                || self.has_browse_cold_probe_queued_or_active(&path)
            {
                self.probe_debounce = None;
                return;
            }

            if native_disc_cold_work_allowed {
                self.arm_probe_debounce(path);
            } else {
                self.probe_debounce = None;
            }
        } else if entry.is_child_dir() {
            let path = entry.path.clone();
            let identity = ProbeCacheIdentity::from_entry(&entry);

            let classification = self.valid_folder_classification_for_entry(&entry).cloned();
            let classification_valid = classification.is_some();
            if !classification_valid {
                self.remove_folder_classification_cache_entry(&path);
            }

            let cold_directory_work_allowed = self.uncached_directory_summary_work_allowed_for_focus();
            let classification_needed = cold_directory_work_allowed
                && !classification_valid
                && !self.folder_classification_pending.contains(&path);
            let stats_needed = self.recursive_dir_stats_allowed_for_focus()
                && classification
                    .as_deref()
                    .is_some_and(classification_allows_recursive_stats)
                && self.valid_dir_stats_for_entry(&entry).is_none()
                && !self.dir_stats_pending.contains(&path);
            let cached_disc_probe_may_be_needed = cold_directory_work_allowed
                && classification
                    .as_deref()
                    .is_some_and(|classification| classification.kind == FolderClassificationKind::Disc);
            let persistent_summary_lookup_needed = db.is_some()
                && self.directory_summary_database_lookup_may_be_useful(&path, identity);

            if stats_needed
                || classification_needed
                || cached_disc_probe_may_be_needed
                || persistent_summary_lookup_needed
            {
                self.arm_probe_debounce(path);
            } else {
                self.probe_debounce = None;
            }
        } else if is_lazy_native_disc_source_candidate(&entry)
            && self.native_disc_cold_work_allowed_for_focus()
        {
            self.arm_probe_debounce(entry.path.clone());
        } else {
            self.probe_debounce = None;
        }
    }


    fn has_browse_cold_probe_queued_or_active(&self, path: &Path) -> bool {
        self.browse_cold_probe_active
            .keys()
            .any(|active| same_scanned_path(active, path))
            || self
                .browse_cold_probe_queue
                .iter()
                .any(|request| same_scanned_path(&request.path, path))
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
                .map(|path| same_scanned_path(path, &request.path))
                .unwrap_or(false)
        });
    }

    fn start_browse_cold_probe_now(
        &mut self,
        path: PathBuf,
        identity: ProbeCacheIdentity,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.probe_pending.insert(path.clone());
        self.probe_cancel.insert(path.clone(), cancel.clone());
        self.browse_cold_probe_active.insert(
            path.clone(),
            BrowseColdProbeActiveJob {
                scan_generation: self.scan_generation,
                cursor_focused: true,
                cancel: cancel.clone(),
            },
        );
        spawn_audio_probe(path, identity, cancel, tx.clone());
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
            .retain(|request| !same_scanned_path(&request.path, &path));
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
        self.browse_cold_probe_active.retain(|active, _| !same_path(active, path));
        self.probe_cancel.remove(path);
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
                || self.browse_cold_probe_active.keys().any(|active| same_scanned_path(active, &request.path))
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
            .retain(|request| !same_scanned_path(&request.path, path));
        let stale: Vec<PathBuf> = self
            .browse_cold_probe_active
            .keys()
            .filter(|active| same_scanned_path(active, path))
            .cloned()
            .collect();
        for active in stale {
            if let Some(job) = self.browse_cold_probe_active.remove(&active) {
                job.cancel.store(true, Ordering::Relaxed);
            }
            if let Some(cancel) = self.probe_cancel.remove(&active) {
                cancel.store(true, Ordering::Relaxed);
            }
            self.probe_pending.remove(&active);
        }
        if let Some(cancel) = self.probe_cancel.remove(path) {
            cancel.store(true, Ordering::Relaxed);
        }
        self.probe_pending.remove(path);
    }

    pub fn clear_browse_cold_probe_queue(&mut self) {
        self.browse_cold_probe_queue.clear();
    }

    fn cancel_stale_active_browse_cold_probes(&mut self) {
        let current_path = self.current_probeable_entry_path();
        let current_generation = self.scan_generation;
        let stale_paths: Vec<PathBuf> = self
            .browse_cold_probe_active
            .iter()
            .filter_map(|(path, job)| {
                let generation_current = job.scan_generation == current_generation;
                let cursor_still_focused = !job.cursor_focused
                    || current_path
                        .as_deref()
                        .map(|current| same_scanned_path(current, path))
                        .unwrap_or(false);
                if generation_current && cursor_still_focused {
                    None
                } else {
                    Some(path.clone())
                }
            })
            .collect();
        for path in stale_paths {
            if let Some(job) = self.browse_cold_probe_active.remove(&path) {
                job.cancel.store(true, Ordering::Relaxed);
            }
            if let Some(cancel) = self.probe_cancel.remove(&path) {
                cancel.store(true, Ordering::Relaxed);
            }
            self.probe_pending.remove(&path);
        }
    }


    fn current_directory_entry_path(&self) -> Option<PathBuf> {
        self.entries
            .get(self.selected_index)
            .filter(|entry| entry.is_child_dir())
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
                    .map(|path| same_scanned_path(path, &request.path))
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
                        .map(|current| same_scanned_path(current, path))
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

    /// Raw cursor movement must not stat active job paths. This variant uses
    /// only scan generation and the currently selected directory path, and is
    /// therefore safe to call from `probe_current_with_db()` on every cursor
    /// move. Identity revalidation remains in `cancel_stale_active_dir_stats()`
    /// for settled/scheduling paths where a fresh metadata read is acceptable.
    fn cancel_stale_active_dir_stats_for_focus_movement(&mut self) {
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
                        .map(|current| same_scanned_path(current, path))
                        .unwrap_or(false);

                if generation_current && cursor_still_focused {
                    None
                } else {
                    Some(path.clone())
                }
            })
            .collect();

        for path in stale_paths {
            if let Some(job) = self.dir_stats_active.remove(&path) {
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

    fn uncached_directory_summary_work_allowed_for_focus(&self) -> bool {
        self.directory_summary_cold_work_policy
            .allows_uncached_highlight_scans()
    }

    /// Native disc-source probes can walk disc directory trees or parse full
    /// disc images. Treat them as directory-summary cold work for hover policy:
    /// cached results may render immediately, but CachedOnly/AfterDescendOnly
    /// must not launch new disc parsing merely because the cursor rested there.
    fn native_disc_cold_work_allowed_for_focus(&self) -> bool {
        self.uncached_directory_summary_work_allowed_for_focus()
    }

    fn recursive_dir_stats_allowed_for_focus(&self) -> bool {
        self.uncached_directory_summary_work_allowed_for_focus()
            && self.directory_stats_cold_work_policy.allows_hover_stats()
    }

    pub fn set_directory_stats_cold_work_policy(
        &mut self,
        policy: BrowseDirectoryStatsColdWorkPolicy,
    ) {
        self.directory_stats_cold_work_policy = policy;
        if !policy.allows_hover_stats() {
            self.clear_dir_stats_work_queue();
        }
    }

    fn lazy_promote_native_disc_source_for_focus(
        &mut self,
        entry: &BrowseEntry,
        identity: ProbeCacheIdentity,
        allow_cold_detection: bool,
        allow_visible_mutation: bool,
    ) -> BrowseEntry {
        if entry.is_disc_source() {
            return entry.clone();
        }
        if !matches!(entry.kind, EntryKind::Directory | EntryKind::Archive) {
            return entry.clone();
        }

        let mut promoted = entry.clone();
        promoted.size = identity.size;
        promoted.modified = identity.modified;

        if allow_cold_detection {
            let snapshot = self.classification_cache_snapshot();
            let mut updates = BrowseClassificationCacheUpdates::default();
            classify_scanned_entry_blocking(&mut promoted, &snapshot, &mut updates);
            self.apply_classification_cache_updates(updates);
        } else {
            classify_scanned_entry_from_cache_only(&mut promoted, self);
        }

        if allow_visible_mutation && promoted.kind != entry.kind {
            self.replace_entry_kind_for_path(&promoted.path, promoted.kind.clone());
            self.mark_visible_entries_changed_pending();
            self.deferred_work.info_pane_changed = true;
        }

        promoted
    }

    fn replace_entry_kind_for_path(&mut self, path: &Path, kind: EntryKind) {
        for entry in self
            .entries
            .iter_mut()
            .chain(self.all_dirs.iter_mut())
            .chain(self.all_files.iter_mut())
        {
            if same_scanned_path(&entry.path, path) {
                entry.kind = kind.clone();
            }
        }
    }

    fn schedule_dir_stats_for_focused_entry(
        &mut self,
        entry: &BrowseEntry,
        identity: ProbeCacheIdentity,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        debug_assert!(entry.is_child_dir());
        if !self.recursive_dir_stats_allowed_for_focus() {
            return;
        }
        let path = entry.path.clone();
        if self.valid_dir_stats_for_entry(entry).is_some()
            || self.dir_stats_pending.contains(&path)
        {
            return;
        }
        self.schedule_cursor_focused_dir_stats(path, identity, tx);
    }

    fn schedule_dir_stats_after_folder_classification_if_useful(
        &mut self,
        entry: &BrowseEntry,
        identity: ProbeCacheIdentity,
        classification: &FolderContentClassification,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        if classification_allows_recursive_stats(classification) {
            self.schedule_dir_stats_for_focused_entry(entry, identity, tx);
        }
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
            .retain(|request| !same_scanned_path(&request.path, &path));
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
                || self.dir_stats_active.keys().any(|active| same_scanned_path(active, &request.path))
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

    fn schedule_cursor_focused_folder_classification(
        &mut self,
        path: PathBuf,
        identity: ProbeCacheIdentity,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        self.discard_stale_queued_folder_classifications();
        if self.has_valid_folder_classification_for_identity(&path, identity)
            || self.folder_classification_pending.contains(&path)
        {
            return;
        }

        self.folder_classification_pending.insert(path.clone());
        self.folder_classification_queue
            .retain(|request| !same_scanned_path(&request.path, &path));
        self.folder_classification_queue.push_front(FolderClassifyRequest {
            path,
            identity,
            scan_generation: self.scan_generation,
            cursor_focused: true,
        });
        self.launch_ready_folder_classifications(tx);
    }

    fn discard_stale_queued_folder_classifications(&mut self) {
        let current_path = self.current_directory_entry_path();
        let current_generation = self.scan_generation;
        let mut kept = VecDeque::with_capacity(self.folder_classification_queue.len());
        while let Some(request) = self.folder_classification_queue.pop_front() {
            let generation_current = request.scan_generation == current_generation;
            let cursor_still_focused = !request.cursor_focused
                || current_path
                    .as_deref()
                    .map(|path| same_scanned_path(path, &request.path))
                    .unwrap_or(false);
            if generation_current && cursor_still_focused {
                kept.push_back(request);
            } else {
                self.folder_classification_pending.remove(&request.path);
            }
        }
        self.folder_classification_queue = kept;
    }

    fn launch_ready_folder_classifications(
        &mut self,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        self.discard_stale_queued_folder_classifications();
        while let Some(request) = self.folder_classification_queue.pop_front() {
            if request.scan_generation != self.scan_generation {
                self.folder_classification_pending.remove(&request.path);
                continue;
            }
            if request.cursor_focused && !self.is_current_entry_path(&request.path) {
                self.folder_classification_pending.remove(&request.path);
                continue;
            }
            if self.has_valid_folder_classification_for_identity(&request.path, request.identity) {
                self.folder_classification_pending.remove(&request.path);
                continue;
            }
            let Some(current_identity) = std::fs::metadata(&request.path)
                .ok()
                .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
            else {
                self.folder_classification_pending.remove(&request.path);
                self.remove_folder_classification_cache_entry(&request.path);
                continue;
            };
            if current_identity != request.identity {
                self.folder_classification_pending.remove(&request.path);
                self.remove_folder_classification_cache_entry(&request.path);
                if request.cursor_focused && self.is_current_entry_path(&request.path) {
                    self.schedule_cursor_focused_folder_classification(request.path, current_identity, tx);
                }
                continue;
            }
            spawn_folder_classification(request.path, request.identity, tx.clone());
        }
    }

    pub fn complete_folder_classification(&mut self, path: &Path) -> bool {
        self.folder_classification_pending.remove(path)
    }

    pub fn clear_folder_classification_work_queue(&mut self) {
        self.folder_classification_queue.clear();
        self.folder_classification_pending.clear();
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
        let cancel = Arc::new(AtomicBool::new(false));
        self.probe_pending.insert(path.to_path_buf());
        self.probe_cancel.insert(path.to_path_buf(), cancel.clone());
        spawn_cached_audio_probe_metadata_completion(
            path.to_path_buf(),
            identity,
            (*info).clone(),
            cancel,
            tx.clone(),
        );
    }

    /// Fire delayed expensive Browse focus work once the cursor has rested on
    /// the same entry. This compatibility wrapper is used by tests and callers
    /// that do not have a persistent probe cache available.
    pub fn check_probe_debounce(
        &mut self,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    ) {
        self.check_probe_debounce_with_db(tx, None);
    }

    /// Fire delayed expensive Browse focus work once the cursor has rested on
    /// the same entry. This is called by the event loop before rendering.
    pub fn check_probe_debounce_with_db(
        &mut self,
        tx: &tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
        db: Option<&crate::db::Database>,
    ) {
        let Some(pending) = self.probe_debounce.clone() else {
            return;
        };
        if Instant::now() < pending.deadline {
            return;
        }
        self.probe_debounce = None;

        let Some(mut entry) = self.entries.get(self.selected_index).cloned() else {
            return;
        };
        if entry.path != pending.path {
            return;
        }

        if self.archive.is_some() {
            self.probe_current_archive_entry(tx);
            return;
        }

        let Some(identity) = Self::current_filesystem_probe_identity(&entry) else {
            self.remove_probe_cache_entry(&pending.path);
            self.remove_folder_classification_cache_entry(&pending.path);
            self.clear_browse_cold_probe_tracking_for(&pending.path);
            self.dir_stats_pending.remove(&pending.path);
            self.folder_classification_pending.remove(&pending.path);
            return;
        };

        entry = self.lazy_promote_native_disc_source_for_focus(
            &entry,
            identity,
            self.native_disc_cold_work_allowed_for_focus(),
            true,
        );

        if entry.is_child_dir() {
            if let Some(db) = db {
                self.load_directory_summary_from_database_for_entry(&entry, identity, db);
            }
        }

        if entry.is_disc_source() && self.native_disc_cold_work_allowed_for_focus() {
            self.schedule_disc_probe_for_source_path(&entry.path, tx);
        }

        if entry.is_child_dir() {
            if entry.is_disc_source() {
                self.schedule_dir_stats_for_focused_entry(&entry, identity, tx);
            } else if let Some(classification) = self.valid_folder_classification_for_entry(&entry).cloned() {
                if self.uncached_directory_summary_work_allowed_for_focus() {
                    self.schedule_disc_probe_for_folder_classification(&entry.path, classification.as_ref(), tx);
                    self.schedule_dir_stats_after_folder_classification_if_useful(
                        &entry,
                        identity,
                        classification.as_ref(),
                        tx,
                    );
                }
            } else if self.uncached_directory_summary_work_allowed_for_focus()
                && !self.folder_classification_pending.contains(&pending.path)
            {
                self.schedule_cursor_focused_folder_classification(pending.path.clone(), identity, tx);
            }
        }

        if entry.is_probeable() {
            let native_disc_cold_work_allowed = !entry.is_disc_source()
                || self.native_disc_cold_work_allowed_for_focus();
            if let Some(cached) = self.cached_probe_for_entry(&entry, identity) {
                if native_disc_cold_work_allowed {
                    if let Some(info) = cached {
                        self.spawn_cached_probe_metadata_completion_if_needed(&pending.path, identity, info, tx);
                    }
                }
                return;
            }

            if !native_disc_cold_work_allowed {
                return;
            }

            if self.has_recent_transient_probe_failure(&pending.path, identity)
                || self.probe_pending.contains(&pending.path)
                || self.has_browse_cold_probe_queued_or_active(&pending.path)
            {
                return;
            }

            // Persistent SQLite cache lookups are intentionally settled-focus
            // work, not raw cursor-movement work. This prevents holding a
            // movement key through a long album from issuing hundreds of DB
            // point lookups while still allowing a focused row to avoid an
            // ffmpeg probe when a valid persistent row exists.
            if let Some(db) = db {
                if let Some(mtime) = identity.modified {
                    let mtime_unix = crate::db::systemtime_to_unix(mtime);
                    if let Some(row) = db.get_cached_probe(
                        &pending.path.display().to_string(),
                        mtime_unix,
                        identity.size,
                    ) {
                        if let Some(info) = row.to_cached_info(identity.size) {
                            let info = self.insert_probe_cache_hit(pending.path.clone(), identity, info);
                            self.probe_cache_needs_metadata_enrichment.insert(pending.path.clone());
                            self.spawn_cached_probe_metadata_completion_if_needed(
                                &pending.path,
                                identity,
                                info,
                                tx,
                            );
                            return;
                        }
                    }
                }
            }

            self.schedule_cursor_focused_cold_probe(pending.path, identity, tx);
        }
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

    fn probe_identity_for_current_scanned_entry_path(
        &self,
        path: &Path,
    ) -> Option<ProbeCacheIdentity> {
        self.probe_cache_scan_identity_index.get(path).copied()
    }

    fn merge_probe_cache_warm_row(&mut self, row: ProbeCacheWarmRow) -> bool {
        // Warm rows are consumed by the reducer during large backlog drains; do
        // not use `same_path()` here. The rows were created from scan-owned
        // paths, so exact path identity is both the stale-safe contract and the
        // only acceptable performance profile for aggressive post-scroll drains.
        let Some(current_identity) = self.probe_identity_for_current_scanned_entry_path(&row.path) else {
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
        self.drain_probe_cache_warm_rows_for_frame_with_budget(
            ProbeCacheWarmDrainBudget::settled_focus(),
        )
    }

    pub(crate) fn drain_probe_cache_warm_rows_for_frame_with_budget(
        &mut self,
        budget: ProbeCacheWarmDrainBudget,
    ) -> (usize, bool) {
        let budget = budget.normalized();
        let started = Instant::now();
        let mut inspected = 0usize;
        let mut inspected_since_time_check = 0usize;
        let mut merged = 0usize;
        let mut stop_for_budget = false;

        while inspected < budget.max_rows && !stop_for_budget {
            let Some(mut batch) = self.probe_cache_warm_pending.pop_front() else {
                break;
            };

            if !self.is_current_directory_generation(batch.generation, &batch.path) {
                continue;
            }

            while inspected < budget.max_rows {
                let Some(row) = batch.rows.pop_front() else {
                    break;
                };

                inspected = inspected.saturating_add(1);
                inspected_since_time_check = inspected_since_time_check.saturating_add(1);
                if self.merge_probe_cache_warm_row(row) {
                    merged = merged.saturating_add(1);
                }

                if inspected >= budget.min_rows
                    && inspected_since_time_check >= budget.time_check_interval
                {
                    inspected_since_time_check = 0;
                    if started.elapsed() >= budget.time_budget {
                        stop_for_budget = true;
                        break;
                    }
                }
            }

            if !batch.rows.is_empty() {
                self.probe_cache_warm_pending.push_front(batch);
                break;
            }
        }

        let backlog_remaining = !self.probe_cache_warm_pending.is_empty();
        if merged > 0 {
            // Warm-cache rows can arrive in very large batches. They should make
            // probe-backed sorting/searching accurate, but not force the info
            // pane to refresh once per bounded merge slice; refresh the current
            // row when the backlog drains and the merged facts are coherent.
            self.mark_probe_cache_update_pending(!backlog_remaining);
        }

        (merged, backlog_remaining)
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

    pub fn defer_browse_deferred_work(&mut self, work: BrowseDeferredWorkFlags) {
        self.deferred_work.merge(work);
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

    pub fn valid_folder_classification_for_entry(
        &self,
        entry: &BrowseEntry,
    ) -> Option<&Arc<FolderContentClassification>> {
        if !entry.is_child_dir() {
            return None;
        }
        let identity = ProbeCacheIdentity::from_entry(entry);
        self.folder_classification_cache
            .get(&entry.path)
            .filter(|cached| cached.is_valid_for(identity))
            .map(|cached| &cached.classification)
    }

    pub fn current_folder_classification(&self) -> Option<&Arc<FolderContentClassification>> {
        let entry = self.entries.get(self.selected_index)?;
        self.valid_folder_classification_for_entry(entry)
    }

    pub fn has_valid_folder_classification_for_identity(
        &self,
        path: &Path,
        identity: ProbeCacheIdentity,
    ) -> bool {
        self.folder_classification_cache
            .get(path)
            .is_some_and(|cached| cached.is_valid_for(identity))
    }

    pub fn enable_file_backed_directory_summary_cache(&mut self, cache_path: PathBuf) -> std::io::Result<usize> {
        self.directory_summary_persistent_cache_mode = DirectorySummaryPersistentCacheMode::FileBacked;
        self.directory_summary_persistent_cache_path = Some(cache_path);
        self.load_directory_summary_persistent_cache()
    }

    pub fn disable_directory_summary_persistent_cache(&mut self) {
        self.directory_summary_persistent_cache_mode = DirectorySummaryPersistentCacheMode::Disabled;
        self.directory_summary_persistent_cache_path = None;
    }

    pub fn enable_database_directory_summary_cache(&mut self) {
        self.directory_summary_persistent_cache_mode = DirectorySummaryPersistentCacheMode::DatabaseBacked;
        self.directory_summary_persistent_cache_path = None;
    }

    pub fn load_directory_summary_persistent_cache(&mut self) -> std::io::Result<usize> {
        if self.directory_summary_persistent_cache_mode != DirectorySummaryPersistentCacheMode::FileBacked {
            return Ok(0);
        }
        let Some(cache_path) = self.directory_summary_persistent_cache_path.clone() else {
            return Ok(0);
        };
        let contents = match std::fs::read_to_string(&cache_path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(err) => return Err(err),
        };
        let mut loaded = 0usize;
        for line in contents.lines() {
            let Some((path, entry)) = DirectorySummaryCacheEntry::from_persistent_line(line) else {
                continue;
            };
            self.install_directory_summary_cache_entry(path, entry);
            loaded = loaded.saturating_add(1);
        }
        Ok(loaded)
    }

    fn install_directory_summary_cache_entry(
        &mut self,
        path: PathBuf,
        entry: DirectorySummaryCacheEntry,
    ) {
        if let Some(classification) = entry.facts.classification.as_ref() {
            self.folder_classification_cache.insert(
                path.clone(),
                FolderClassificationCacheEntry {
                    identity: entry.identity,
                    classification: classification.clone(),
                },
            );
        }
        if let Some(stats) = entry.facts.stats.as_ref() {
            self.dir_stats_cache.insert(
                path.clone(),
                DirStatsCacheEntry {
                    identity: entry.identity,
                    stats: stats.clone(),
                },
            );
        }
        self.directory_summary_db_miss_cache.remove(&path);
        self.directory_summary_cache.insert(path, entry);
    }

    fn directory_summary_database_lookup_enabled(&self) -> bool {
        self.directory_summary_persistent_cache_mode == DirectorySummaryPersistentCacheMode::DatabaseBacked
    }

    fn directory_summary_database_lookup_may_be_useful(
        &self,
        path: &Path,
        identity: ProbeCacheIdentity,
    ) -> bool {
        self.directory_summary_database_lookup_enabled()
            && self
                .directory_summary_cache
                .get(path)
                .map(|cached| !cached.is_valid_for(identity))
                .unwrap_or(true)
            && self
                .directory_summary_db_miss_cache
                .get(path)
                .map(|miss_identity| *miss_identity != identity)
                .unwrap_or(true)
    }

    fn load_directory_summary_from_database_for_entry(
        &mut self,
        entry: &BrowseEntry,
        identity: ProbeCacheIdentity,
        db: &crate::db::Database,
    ) -> bool {
        if !self.directory_summary_database_lookup_may_be_useful(&entry.path, identity) {
            return self
                .directory_summary_cache
                .get(&entry.path)
                .is_some_and(|cached| cached.is_valid_for(identity));
        }
        let Some(summary) = db.get_cached_directory_summary(&entry.path, identity) else {
            self.directory_summary_db_miss_cache.insert(entry.path.clone(), identity);
            return false;
        };
        self.directory_summary_db_miss_cache.remove(&entry.path);
        self.install_directory_summary_cache_entry(entry.path.clone(), summary);
        true
    }

    pub fn store_directory_summary_for_identity_best_effort(
        &mut self,
        path: &Path,
        identity: ProbeCacheIdentity,
        db: &crate::db::Database,
    ) {
        if self.directory_summary_persistent_cache_mode != DirectorySummaryPersistentCacheMode::DatabaseBacked {
            return;
        }
        let Some(entry) = self
            .directory_summary_cache
            .get(path)
            .filter(|entry| entry.is_valid_for(identity))
            .cloned()
        else {
            return;
        };
        self.directory_summary_db_miss_cache.remove(path);
        let _ = db.store_directory_summary(path, &entry);
    }

    pub fn invalidate_directory_summary_persistent_cache_best_effort(
        &self,
        path: &Path,
        db: &crate::db::Database,
    ) {
        if self.directory_summary_persistent_cache_mode == DirectorySummaryPersistentCacheMode::DatabaseBacked {
            let _ = db.invalidate_directory_summary(path);
        }
    }

    pub fn flush_directory_summary_persistent_cache(&self) -> std::io::Result<()> {
        if self.directory_summary_persistent_cache_mode != DirectorySummaryPersistentCacheMode::FileBacked {
            return Ok(());
        }
        let Some(cache_path) = self.directory_summary_persistent_cache_path.as_ref() else {
            return Ok(());
        };
        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut rows = self
            .directory_summary_cache
            .iter()
            .map(|(path, entry)| entry.to_persistent_line(path))
            .collect::<Vec<_>>();
        rows.sort();
        std::fs::write(cache_path, rows.join("\n"))
    }

    fn persist_directory_summary_cache_best_effort(&self) {
        let _ = self.flush_directory_summary_persistent_cache();
    }

    fn directory_summary_entry_mut_for_identity(
        &mut self,
        path: PathBuf,
        identity: ProbeCacheIdentity,
    ) -> &mut DirectorySummaryCacheEntry {
        let replace = self
            .directory_summary_cache
            .get(&path)
            .map(|entry| !entry.is_valid_for(identity))
            .unwrap_or(true);
        if replace {
            self.directory_summary_cache
                .insert(path.clone(), DirectorySummaryCacheEntry::new(identity));
        }
        self.directory_summary_cache
            .get_mut(&path)
            .expect("directory summary entry inserted above")
    }

    pub fn insert_folder_classification_for_identity(
        &mut self,
        path: PathBuf,
        identity: ProbeCacheIdentity,
        classification: FolderContentClassification,
    ) {
        let entry = FolderClassificationCacheEntry::new(identity, classification);
        let classification = entry.classification.clone();
        self.folder_classification_cache.insert(path.clone(), entry);
        self.directory_summary_entry_mut_for_identity(path, identity)
            .set_classification(classification);
        self.persist_directory_summary_cache_best_effort();
    }

    pub fn remove_folder_classification_cache_entry(&mut self, path: &Path) {
        self.folder_classification_cache.remove(path);
        self.directory_summary_cache.remove(path);
        self.directory_summary_db_miss_cache.remove(path);
        self.persist_directory_summary_cache_best_effort();
    }

    pub fn valid_directory_summary_for_entry(
        &self,
        entry: &BrowseEntry,
    ) -> Option<&DirectorySummaryFacts> {
        if !entry.is_child_dir() {
            return None;
        }
        let identity = ProbeCacheIdentity::from_entry(entry);
        self.directory_summary_cache
            .get(&entry.path)
            .filter(|cached| cached.is_valid_for(identity))
            .map(|cached| &cached.facts)
    }

    pub fn current_directory_summary(&self) -> Option<&DirectorySummaryFacts> {
        let entry = self.entries.get(self.selected_index)?;
        self.valid_directory_summary_for_entry(entry)
    }

    pub fn set_directory_summary_cold_work_policy(
        &mut self,
        policy: BrowseDirectorySummaryColdWorkPolicy,
    ) {
        self.directory_summary_cold_work_policy = policy;
        if !policy.allows_uncached_highlight_scans() {
            self.clear_dir_stats_work_queue();
            self.clear_folder_classification_work_queue();
            self.cancel_stale_active_browse_cold_probes();
            self.cancel_stale_active_disc_probes();
            let clear_directory_debounce = self
                .entries
                .get(self.selected_index)
                .is_some_and(|entry| entry.is_child_dir() && !entry.is_disc_source());
            if clear_directory_debounce {
                self.probe_debounce = None;
            }
        }
    }

    pub fn folder_classification_pending_for(&self, path: &Path) -> bool {
        self.folder_classification_pending.contains(path)
    }

    /// Test helper for exercising async reducers without reaching into private
    /// debounce internals. Production code must still mark pending work only
    /// through the focused classification scheduler.
    #[cfg(test)]
    pub fn mark_folder_classification_pending_for_test(&mut self, path: PathBuf) {
        self.folder_classification_pending.insert(path);
    }

    pub fn folder_audio_summary_probe_work_in_flight(&self, audio: &FolderAudioSummary) -> bool {
        audio.file_paths.iter().any(|path| {
            self.probe_pending.contains(path)
                || self
                    .browse_cold_probe_active
                    .keys()
                    .any(|active| same_scanned_path(active, path))
                || self
                    .browse_cold_probe_queue
                    .iter()
                    .any(|request| same_scanned_path(&request.path, path))
        })
    }

    pub fn folder_probe_rollup(&self, audio: &FolderAudioSummary) -> FolderProbeRollup {
        let mut rollup = FolderProbeRollup::default();
        for path in &audio.file_paths {
            let Some(info) = self.probe_cache.get(path).and_then(|cached| cached.info.as_ref()) else {
                continue;
            };
            rollup.probed_count = rollup.probed_count.saturating_add(1);
            if info.source.duration_secs.is_finite() && info.source.duration_secs > 0.0 {
                rollup.total_duration_secs += info.source.duration_secs;
            }
            let profile = folder_probe_profile_label(&info.source);
            if !profile.is_empty() {
                *rollup.profile_counts.entry(profile).or_insert(0) += 1;
            }
        }
        rollup
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
        if !entry.is_child_dir() {
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
        let entry = DirStatsCacheEntry::new(identity, stats);
        let stats = entry.stats.clone();
        self.dir_stats_cache.insert(path.clone(), entry);
        self.directory_summary_entry_mut_for_identity(path, identity)
            .set_stats(stats);
        self.persist_directory_summary_cache_best_effort();
    }

    pub fn remove_dir_stats_cache_entry(&mut self, path: &Path) {
        self.dir_stats_cache.remove(path);
        self.directory_summary_cache.remove(path);
        self.directory_summary_db_miss_cache.remove(path);
        self.persist_directory_summary_cache_best_effort();
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
    cancel: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) {
    tokio::spawn(async move {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let path_for_task = path.clone();
        let cancel_for_task = cancel.clone();
        let result = tokio::task::spawn_blocking(move || {
            if cancel_for_task.load(Ordering::Relaxed) {
                return Err("cached probe metadata task cancelled".to_string());
            }
            if info.metadata.preemphasis_metadata.is_none()
                && !cancel_for_task.load(Ordering::Relaxed)
            {
                info.metadata.preemphasis_metadata =
                    crate::tui::probe::preemphasis_metadata_check_blocking(&path_for_task);
            }
            if cancel_for_task.load(Ordering::Relaxed) {
                Err("cached probe metadata task cancelled".to_string())
            } else {
                Ok(info)
            }
        })
        .await
        .unwrap_or_else(|join_err| Err(format!("cached probe metadata task panicked: {}", join_err)));

        if cancel.load(Ordering::Relaxed) {
            return;
        }
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
    cancel: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) {
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    if is_cue_sheet_path(&path) {
        // Defense in depth: callers should route CUE preview through
        // `spawn_cue_proxy_audio_probe`, but never allow a `.cue` text file to
        // reach ffmpeg probing through this generic audio helper.
        spawn_cue_proxy_audio_probe(path, tx);
        return;
    }

    tokio::spawn(async move {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let path_for_task = path.clone();
        let cancel_for_task = cancel.clone();
        let result: Result<CachedInfo, String> = tokio::task::spawn_blocking(move || {
            if cancel_for_task.load(Ordering::Relaxed) {
                return Err("audio probe cancelled".to_string());
            }
            let source =
                crate::tui::probe::probe_audio(&path_for_task).map_err(|e| format!("{}", e))?;
            if cancel_for_task.load(Ordering::Relaxed) {
                return Err("audio probe cancelled".to_string());
            }
            let metadata = crate::tui::probe::read_metadata(&path_for_task).unwrap_or_else(|_| {
                let mut metadata = SourceMetadata::default();
                if !cancel_for_task.load(Ordering::Relaxed) {
                    metadata.preemphasis_metadata =
                        crate::tui::probe::preemphasis_metadata_check_blocking(&path_for_task);
                }
                metadata
            });
            if cancel_for_task.load(Ordering::Relaxed) {
                Err("audio probe cancelled".to_string())
            } else {
                Ok(CachedInfo { source, metadata })
            }
        })
        .await
        .unwrap_or_else(|join_err| Err(format!("probe task panicked: {}", join_err)));

        if cancel.load(Ordering::Relaxed) {
            return;
        }
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

fn classify_scanned_entry_from_cache_only(entry: &mut BrowseEntry, state: &BrowseState) {
    if matches!(&entry.kind, EntryKind::Archive) {
        classify_scanned_iso_entry_from_cache_only(entry, state);
    } else if matches!(&entry.kind, EntryKind::Directory) {
        classify_scanned_directory_entry_from_cache_only(entry, state);
    }
}

fn classify_scanned_iso_entry_from_cache_only(entry: &mut BrowseEntry, state: &BrowseState) {
    let is_iso = entry
        .path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("iso"))
        .unwrap_or(false);
    if !is_iso {
        return;
    }

    let fingerprint = ClassificationFingerprint::from_entry(entry);
    if state
        .sacd_classify_cache
        .get(&entry.path)
        .filter(|(cached, verdict)| *verdict && *cached == fingerprint)
        .is_some()
    {
        entry.kind = EntryKind::SacdIso;
        return;
    }
    if state
        .dvda_iso_classify_cache
        .get(&entry.path)
        .filter(|(cached, verdict)| *verdict && *cached == fingerprint)
        .is_some()
    {
        entry.kind = EntryKind::DvdAudioIso;
        return;
    }
    if state
        .dvdv_iso_classify_cache
        .get(&entry.path)
        .filter(|(cached, verdict)| *verdict && *cached == fingerprint)
        .is_some()
    {
        entry.kind = EntryKind::DvdVideoIso;
        return;
    }
    if state
        .bluray_iso_classify_cache
        .get(&entry.path)
        .filter(|(cached, verdict)| *verdict && *cached == fingerprint)
        .is_some()
    {
        entry.kind = EntryKind::BlurayIso;
    }
}

fn classify_scanned_directory_entry_from_cache_only(entry: &mut BrowseEntry, state: &BrowseState) {
    // CachedOnly intentionally avoids opening marker files or descending into
    // disc directory structures. For directory sources, only exact identity
    // matches against the direct entry fingerprint are promoted. Marker-rich
    // fingerprints are refreshed by the settled-focus cold path when allowed.
    let fingerprint = ClassificationFingerprint::from_entry(entry);
    if state
        .dvda_dir_classify_cache
        .get(&entry.path)
        .filter(|(cached, verdict)| *verdict && *cached == fingerprint)
        .is_some()
    {
        entry.kind = EntryKind::DvdAudioDir;
        return;
    }
    if state
        .dvdv_dir_classify_cache
        .get(&entry.path)
        .filter(|(cached, verdict)| *verdict && *cached == fingerprint)
        .is_some()
    {
        entry.kind = EntryKind::DvdVideoDir;
        return;
    }
    if state
        .bluray_dir_classify_cache
        .get(&entry.path)
        .filter(|(cached, verdict)| *verdict && *cached == fingerprint)
        .is_some()
    {
        entry.kind = EntryKind::BlurayDir;
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
    _classification_cache: &BrowseClassificationCacheSnapshot,
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
    let classification_updates = BrowseClassificationCacheUpdates::default();

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
            path,
            name,
            kind.clone(),
            size,
            modified,
            is_symlink,
            is_broken_symlink,
        );

        // Keep directory scans cheap and predictable: do not open ISO images
        // or disc-directory structures for every row. Supported native disc
        // sources are promoted lazily from the settled-focus debounce path.

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
    // path-aware check so `.cue` and lazy `.iso` disc-image candidates stay
    // visible under AudioOnly without widening the filter to all unsupported
    // `OtherFile` or archive entries.
    if !entry.is_navigable_dir() && !format_filter.allows_entry(entry) {
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
    if audio_only && !entry.is_navigable_dir() && !is_audio_filter_visible_entry(entry) {
        return false;
    }
    if !entry.is_navigable_dir() && !format_filter.allows_entry(entry) {
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
        EntryKind::AudioFile(AudioFormat::Shorten) => 23,
        EntryKind::AudioFile(AudioFormat::Ogg) => 24,
        EntryKind::AudioFile(AudioFormat::Tta) => 25,
        EntryKind::AudioFile(AudioFormat::Lpcm) => 26,
        EntryKind::SacdIso
        | EntryKind::DvdAudioIso
        | EntryKind::DvdAudioDir
        | EntryKind::DvdVideoIso
        | EntryKind::DvdVideoDir
        | EntryKind::BlurayIso
        | EntryKind::BlurayDir => 27,
        EntryKind::Archive => 27,
        EntryKind::OtherFile => 30,
    }
}

fn folder_probe_profile_label(info: &SourceInfo) -> String {
    let mut parts = Vec::new();
    if let Some(bits) = info.bit_depth {
        parts.push(format!("{bits}-bit"));
    }
    if info.sample_rate > 0 {
        parts.push(info.sample_rate_display());
    }
    if info.channels > 0 {
        parts.push(info.channels_display());
    }
    parts.join("/")
}

pub fn spawn_folder_classification(
    path: PathBuf,
    identity: ProbeCacheIdentity,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) {
    tokio::spawn(async move {
        let classify_path = path.clone();
        let classification = tokio::task::spawn_blocking(move || {
            classify_folder_content_blocking(&classify_path, identity)
        })
        .await
        .unwrap_or_else(|err| {
            log::warn!("folder classification task failed for {}: {err}", path.display());
            FolderContentClassification::unknown(identity, true)
        });

        let _ = tx
            .send(crate::tui::message::AppMessage::FolderClassifyComplete {
                path,
                identity,
                classification,
            })
            .await;
    });
}

#[derive(Debug)]
struct FolderClassifyBudget {
    remaining: usize,
    exhausted: bool,
}

impl FolderClassifyBudget {
    fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            exhausted: false,
        }
    }

    fn consume(&mut self) -> bool {
        if self.remaining == 0 {
            self.exhausted = true;
            return false;
        }
        self.remaining -= 1;
        true
    }

    fn consume_dir_read(&mut self) -> bool {
        self.consume()
    }

    /// Consume one budget unit for inspecting a streamed directory entry. This
    /// deliberately covers both the `read_dir` iteration work and the
    /// subsequent `DirEntry::file_type()` call, which may require a stat-like
    /// syscall on filesystems that do not provide d_type. Counting per entry
    /// makes the classification budget a real hard cap: a huge flat directory
    /// cannot be fully enumerated after spending only one directory-open unit.
    fn consume_entry_inspection(&mut self) -> bool {
        self.consume()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FolderIsoUnit {
    path: PathBuf,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FolderClassifySibling {
    name: String,
}

#[derive(Debug, Clone)]
struct FolderClassifyScan {
    child_dirs: Vec<PathBuf>,
    child_siblings: Vec<FolderClassifySibling>,
    audio: FolderAudioSummary,
    disc_marker: Option<FolderDiscMarkerKind>,
    iso_units: Vec<FolderIsoUnit>,
}

impl FolderClassifyScan {
    fn empty() -> Self {
        Self {
            child_dirs: Vec::new(),
            child_siblings: Vec::new(),
            audio: FolderAudioSummary::default(),
            disc_marker: None,
            iso_units: Vec::new(),
        }
    }
}

fn classify_folder_content_blocking(
    path: &Path,
    identity: ProbeCacheIdentity,
) -> FolderContentClassification {
    let mut budget = FolderClassifyBudget::new(FOLDER_CLASSIFY_IO_BUDGET);
    let Some(root_scan) = scan_folder_for_classification(path, &mut budget, true) else {
        return FolderContentClassification::unknown(identity, budget.exhausted);
    };

    // Direct audio at the highlighted root is the cheapest and strongest album
    // signal. Do not descend: this keeps artist folders from becoming expensive
    // merely because one nested album has known metadata.
    if root_scan.audio.track_count > 0 {
        return FolderContentClassification {
            kind: FolderClassificationKind::Album,
            identity,
            audio: root_scan.audio,
            units: Vec::new(),
            unit_count: 1,
            collection_many: false,
            io_budget_exhausted: budget.exhausted,
            disc_marker: None,
        };
    }

    if let Some(marker) = root_scan.disc_marker {
        return FolderContentClassification {
            kind: FolderClassificationKind::Disc,
            identity,
            audio: FolderAudioSummary::default(),
            units: Vec::new(),
            unit_count: 1,
            collection_many: false,
            io_budget_exhausted: budget.exhausted,
            disc_marker: Some(marker),
        };
    }

    if root_scan.child_dirs.len() >= FOLDER_CLASSIFY_FAN_OUT_THRESHOLD {
        return FolderContentClassification::collection(
            identity,
            root_scan.child_dirs.len(),
            true,
            budget.exhausted,
        );
    }

    let mut children_by_parent: HashMap<PathBuf, Vec<FolderClassifySibling>> = HashMap::new();
    children_by_parent.insert(path.to_path_buf(), root_scan.child_siblings.clone());

    let mut units: Vec<FolderUnitSummary> = Vec::new();
    let no_scanned_non_unit_dirs: HashSet<PathBuf> = HashSet::new();
    push_root_iso_units(path, &root_scan, &mut units);
    if let Some(classification) = classify_units_if_decided(
        identity,
        &units,
        &children_by_parent,
        &no_scanned_non_unit_dirs,
        budget.exhausted,
    ) {
        return classification;
    }

    let mut queue: VecDeque<(PathBuf, usize)> = root_scan
        .child_dirs
        .iter()
        .cloned()
        .map(|child| (child, 1usize))
        .collect();
    let mut scanned_non_unit_dirs: HashSet<PathBuf> = HashSet::new();

    while let Some((dir, depth)) = queue.pop_front() {
        if depth > FOLDER_CLASSIFY_MAX_DEPTH {
            continue;
        }

        let Some(scan) = scan_folder_for_classification(&dir, &mut budget, false) else {
            if budget.exhausted {
                break;
            }
            scanned_non_unit_dirs.insert(dir);
            continue;
        };

        children_by_parent.insert(dir.clone(), scan.child_siblings.clone());

        let parent = dir.parent().map(Path::to_path_buf).unwrap_or_else(|| path.to_path_buf());
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();

        if push_units_from_scanned_directory(&dir, parent, name, &scan, &mut units) {
            if let Some(classification) = classify_units_if_decided(
                identity,
                &units,
                &children_by_parent,
                &scanned_non_unit_dirs,
                budget.exhausted,
            ) {
                return classification;
            }
            continue;
        }

        scanned_non_unit_dirs.insert(dir.clone());
        if depth < FOLDER_CLASSIFY_MAX_DEPTH {
            for child in scan.child_dirs {
                queue.push_back((child, depth + 1));
            }
        }

        if budget.exhausted {
            break;
        }
    }

    if budget.exhausted {
        return if units.is_empty() {
            FolderContentClassification::unknown(identity, true)
        } else {
            FolderContentClassification::collection(identity, units.len(), true, true)
        };
    }

    finalize_folder_units(identity, units, &children_by_parent, &scanned_non_unit_dirs, false)
}

fn push_root_iso_units(root: &Path, scan: &FolderClassifyScan, units: &mut Vec<FolderUnitSummary>) {
    for iso in &scan.iso_units {
        units.push(FolderUnitSummary {
            path: iso.path.clone(),
            parent: root.to_path_buf(),
            name: iso.name.clone(),
            disc_marker: Some(FolderDiscMarkerKind::Iso),
            audio: FolderAudioSummary::default(),
        });
    }
}

fn push_units_from_scanned_directory(
    dir: &Path,
    parent: PathBuf,
    name: String,
    scan: &FolderClassifyScan,
    units: &mut Vec<FolderUnitSummary>,
) -> bool {
    if scan.disc_marker.is_some() || scan.audio.track_count > 0 {
        units.push(FolderUnitSummary {
            path: dir.to_path_buf(),
            parent,
            name,
            disc_marker: scan.disc_marker,
            audio: scan.audio.clone(),
        });
        return true;
    }

    match scan.iso_units.len() {
        0 => false,
        1 => {
            units.push(FolderUnitSummary {
                path: scan.iso_units[0].path.clone(),
                parent,
                name,
                disc_marker: Some(FolderDiscMarkerKind::Iso),
                audio: FolderAudioSummary::default(),
            });
            true
        }
        _ => {
            for iso in &scan.iso_units {
                units.push(FolderUnitSummary {
                    path: iso.path.clone(),
                    parent: dir.to_path_buf(),
                    name: iso.name.clone(),
                    disc_marker: Some(FolderDiscMarkerKind::Iso),
                    audio: FolderAudioSummary::default(),
                });
            }
            true
        }
    }
}

fn common_unit_disc_marker(units: &[FolderUnitSummary]) -> Option<FolderDiscMarkerKind> {
    let mut markers = units.iter().filter_map(|unit| unit.disc_marker);
    let first = markers.next()?;
    if markers.all(|marker| marker == first) {
        Some(first)
    } else {
        None
    }
}

fn classify_units_if_decided(
    identity: ProbeCacheIdentity,
    units: &[FolderUnitSummary],
    children_by_parent: &HashMap<PathBuf, Vec<FolderClassifySibling>>,
    scanned_non_unit_dirs: &HashSet<PathBuf>,
    io_budget_exhausted: bool,
) -> Option<FolderContentClassification> {
    match classify_multiple_units_if_ready(units, children_by_parent, scanned_non_unit_dirs) {
        MultiDiscDecision::Complete => {
            let mut audio = FolderAudioSummary::default();
            for unit in units {
                audio.merge(&unit.audio);
            }
            Some(FolderContentClassification {
                kind: FolderClassificationKind::MultiDisc,
                identity,
                audio,
                unit_count: units.len(),
                units: units.to_vec(),
                collection_many: false,
                io_budget_exhausted,
                disc_marker: common_unit_disc_marker(units),
            })
        }
        MultiDiscDecision::Collection => Some(FolderContentClassification::collection(
            identity,
            units.len(),
            false,
            io_budget_exhausted,
        )),
        MultiDiscDecision::NeedMore => None,
    }
}

fn finalize_folder_units(
    identity: ProbeCacheIdentity,
    units: Vec<FolderUnitSummary>,
    children_by_parent: &HashMap<PathBuf, Vec<FolderClassifySibling>>,
    scanned_non_unit_dirs: &HashSet<PathBuf>,
    io_budget_exhausted: bool,
) -> FolderContentClassification {
    match units.len() {
        0 => FolderContentClassification::unknown(identity, io_budget_exhausted),
        1 => {
            let mut unit = units.into_iter().next().expect("len checked");
            let kind = if unit.disc_marker.is_some() {
                FolderClassificationKind::Disc
            } else {
                FolderClassificationKind::Album
            };
            let audio = std::mem::take(&mut unit.audio);
            FolderContentClassification {
                kind,
                identity,
                audio,
                disc_marker: unit.disc_marker,
                unit_count: 1,
                units: vec![unit],
                collection_many: false,
                io_budget_exhausted,
            }
        }
        _ => classify_units_if_decided(
            identity,
            &units,
            children_by_parent,
            scanned_non_unit_dirs,
            io_budget_exhausted,
        )
        .unwrap_or_else(|| {
            FolderContentClassification::collection(identity, units.len(), false, io_budget_exhausted)
        }),
    }
}

fn scan_folder_for_classification(
    path: &Path,
    budget: &mut FolderClassifyBudget,
    root_fanout_early_out: bool,
) -> Option<FolderClassifyScan> {
    if !budget.consume_dir_read() {
        return None;
    }
    let read_dir = match fs::read_dir(path) {
        Ok(read_dir) => read_dir,
        Err(_) => return None,
    };

    let mut scan = FolderClassifyScan::empty();
    for entry_result in read_dir {
        if !budget.consume_entry_inspection() {
            break;
        }

        let Ok(entry) = entry_result else {
            continue;
        };
        let child_path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            if let Some(marker) = folder_disc_marker_kind(&name) {
                scan.disc_marker = scan.disc_marker.or(Some(marker));
            }
            scan.child_siblings.push(FolderClassifySibling { name: name.clone() });
            scan.child_dirs.push(child_path);
            if root_fanout_early_out
                && scan.audio.track_count == 0
                && scan.disc_marker.is_none()
                && scan.child_dirs.len() >= FOLDER_CLASSIFY_FAN_OUT_THRESHOLD
            {
                break;
            }
            continue;
        }

        if file_type.is_file() {
            match classify_file(&child_path) {
                EntryKind::AudioFile(format) => {
                    scan.audio.add_audio_file(child_path, format.name().to_string());
                }
                EntryKind::Archive if is_iso_path(&child_path) => {
                    let unit_name = iso_unit_name(&child_path, &name);
                    scan.child_siblings.push(FolderClassifySibling { name: unit_name.clone() });
                    scan.iso_units.push(FolderIsoUnit {
                        path: child_path,
                        name: unit_name,
                    });
                }
                _ => {}
            }
        }
    }

    Some(scan)
}

fn iso_unit_name(path: &Path, fallback_name: &str) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .map(|stem| stem.to_string())
        .unwrap_or_else(|| fallback_name.to_string())
}

fn folder_disc_marker_kind(name: &str) -> Option<FolderDiscMarkerKind> {
    match name.trim().to_ascii_lowercase().as_str() {
        "bdmv" => Some(FolderDiscMarkerKind::BluRay),
        "video_ts" => Some(FolderDiscMarkerKind::DvdVideo),
        "audio_ts" => Some(FolderDiscMarkerKind::DvdAudio),
        // Do not treat a directory literally named "SACD" as a disc marker.
        // Practical SACD handling in this application is ISO-based and requires
        // the existing ScarletBook magic-byte probe, which the cheap folder
        // classifier is explicitly forbidden to run. A plain SACD/ folder is
        // therefore just another child directory unless it contains an ISO unit.
        _ => None,
    }
}

fn is_iso_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("iso"))
}

/// Cheap lazy-disc-image candidate test. Directory scans intentionally leave
/// `.iso` rows as `EntryKind::Archive` so browsing a directory full of images
/// never opens every file. Filters must still keep those candidates visible;
/// otherwise a supported SACD/DVD/Blu-ray ISO can be hidden before the
/// settled-focus lazy promotion path has any chance to classify it.
fn is_disc_image_candidate_path(path: &Path) -> bool {
    is_iso_path(path)
}

fn is_lazy_native_disc_source_candidate(entry: &BrowseEntry) -> bool {
    matches!(entry.kind, EntryKind::Directory)
        || (matches!(entry.kind, EntryKind::Archive) && is_disc_image_candidate_path(&entry.path))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MultiDiscDecision {
    Complete,
    NeedMore,
    Collection,
}

fn classify_multiple_units_if_ready(
    units: &[FolderUnitSummary],
    children_by_parent: &HashMap<PathBuf, Vec<FolderClassifySibling>>,
    scanned_non_unit_dirs: &HashSet<PathBuf>,
) -> MultiDiscDecision {
    if units.len() < 2 {
        return MultiDiscDecision::NeedMore;
    }

    let parent = &units[0].parent;
    if !units.iter().all(|unit| same_path(&unit.parent, parent)) {
        return MultiDiscDecision::Collection;
    }
    if !units.iter().all(|unit| is_disc_like_folder_name(&unit.name)) {
        return MultiDiscDecision::Collection;
    }

    let Some(sibling_names) = children_by_parent.get(parent) else {
        return MultiDiscDecision::NeedMore;
    };

    let unit_names: HashSet<String> = units
        .iter()
        .map(|unit| normalized_folder_name(&unit.name))
        .collect();
    let mut required_count = 0usize;
    for sibling in sibling_names {
        if is_ignorable_multidisc_sibling(&sibling.name) {
            continue;
        }
        if !is_disc_like_folder_name(&sibling.name) {
            return MultiDiscDecision::Collection;
        }
        required_count = required_count.saturating_add(1);
        if !unit_names.contains(&normalized_folder_name(&sibling.name)) {
            return MultiDiscDecision::NeedMore;
        }
    }

    for scanned in scanned_non_unit_dirs {
        if scanned.parent().is_some_and(|candidate_parent| same_path(candidate_parent, parent)) {
            let scanned_name = scanned
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if !is_ignorable_multidisc_sibling(scanned_name) {
                return MultiDiscDecision::Collection;
            }
        }
    }

    if required_count >= 2 && unit_names.len() >= required_count {
        MultiDiscDecision::Complete
    } else {
        MultiDiscDecision::NeedMore
    }
}

fn normalized_folder_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn is_ignorable_multidisc_sibling(name: &str) -> bool {
    matches!(
        normalized_folder_name(name).as_str(),
        "artwork"
            | "art"
            | "scans"
            | "scan"
            | "covers"
            | "cover"
            | "booklet"
            | "booklets"
            | "extras"
            | "bonus"
            | "images"
            | "photos"
            | "logo"
            | "liner notes"
            | "liner-notes"
            | "liners"
            | "book"
            | "pdf"
            | "docs"
    )
}

fn is_disc_like_folder_name(name: &str) -> bool {
    let lower = normalized_folder_name(name);
    let compact: String = lower
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if compact.is_empty() {
        return false;
    }

    if compact.chars().all(|ch| ch.is_ascii_digit()) {
        return compact.len() <= 3;
    }

    const PREFIXES: &[&str] = &["disc", "disk", "cd", "dvd", "bd", "bluray", "blu"];
    PREFIXES.iter().any(|prefix| {
        compact == *prefix
            || compact.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.chars().all(|ch| ch.is_ascii_digit())
                    || matches!(suffix, "one" | "two" | "three" | "four" | "five" | "six" | "seven")
            })
    })
}

#[derive(Debug, Clone, Copy)]
struct DirStatsWalkBudget {
    max_depth: u32,
    max_files: usize,
    max_entries: usize,
    max_millis: u64,
}

impl Default for DirStatsWalkBudget {
    fn default() -> Self {
        Self {
            max_depth: 4,
            max_files: 50_000,
            max_entries: 75_000,
            max_millis: 200,
        }
    }
}

fn compute_dir_stats(path: &Path, cancel: &AtomicBool) -> Option<DirStats> {
    compute_dir_stats_with_budget(path, cancel, DirStatsWalkBudget::default())
}

fn compute_dir_stats_with_budget(
    path: &Path,
    cancel: &AtomicBool,
    budget: DirStatsWalkBudget,
) -> Option<DirStats> {
    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    let deadline = Instant::now() + Duration::from_millis(budget.max_millis);
    let mut stats = DirStats::default();
    let mut entries_seen = 0usize;
    let mut queue = VecDeque::from([(path.to_path_buf(), 0u32)]);

    while let Some((dir, depth)) = queue.pop_front() {
        if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
            return None;
        }
        if depth >= budget.max_depth {
            continue;
        }

        let read = match fs::read_dir(&dir) {
            Ok(read) => read,
            Err(_) => continue,
        };

        for entry in read.flatten() {
            if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
                return None;
            }
            entries_seen = entries_seen.saturating_add(1);
            if entries_seen > budget.max_entries || stats.file_count >= budget.max_files {
                return None;
            }

            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_file() {
                if let Ok(meta) = entry.metadata() {
                    stats.file_count = stats.file_count.saturating_add(1);
                    let file_len = meta.len();
                    stats.total_size = stats.total_size.saturating_add(file_len);
                    if matches!(classify_file(&entry.path()), EntryKind::AudioFile(_)) {
                        stats.audio_count = stats.audio_count.saturating_add(1);
                        stats.audio_size = stats.audio_size.saturating_add(file_len);
                    }
                }
            } else if file_type.is_dir() {
                stats.folder_count = stats.folder_count.saturating_add(1);
                queue.push_back((entry.path(), depth + 1));
            }
        }
    }

    Some(stats)
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
    #[cfg(test)]
    if let Some(tags) = test_archive_tag_fixture(archive_path, inner_path) {
        let _ = password;
        return tags;
    }

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
    let query_lower = query.to_lowercase();
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
        if search_filename || e.is_navigable_dir() {
            if let Some(s) = search_exact_substring_score(&e.name_lower, &query_lower)
                .or_else(|| matcher.fuzzy_match(&e.name_lower, &query))
            {
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

/// Compare paths captured from the same directory-scan snapshot.
///
/// Raw cursor movement must remain filesystem-free, so focus-movement
/// cancellation and debounce bookkeeping use this exact comparison instead of
/// `same_path()`. Canonicalizing here would stat path components on every row
/// while the user scrolls through a directory, which defeats the Browse
/// performance-mode invariant. Use `same_path()` only in slower paths where
/// filesystem I/O is already acceptable, such as navigation, explicit actions,
/// completion acceptance, or settled-focus scheduling.
fn same_scanned_path(left: &Path, right: &Path) -> bool {
    left == right
}

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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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
        || (matches!(entry.kind, EntryKind::Archive)
            && is_disc_image_candidate_path(&entry.path))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};
    use std::sync::{atomic::AtomicBool, Arc};
    use std::time::Duration;

    fn canonical_equivalent_focus_path(real_path: &Path) -> PathBuf {
        let parent = real_path.parent().expect("fixture parent");
        let alias_dir = parent.join("alias-for-canonicalize-test");
        std::fs::create_dir_all(&alias_dir).expect("alias fixture dir");
        alias_dir
            .join("..")
            .join(real_path.file_name().expect("fixture file name"))
    }

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

    fn set_scan_files_for_test(state: &mut BrowseState, files: Vec<BrowseEntry>) {
        state.all_files = files.clone();
        state.entries = files;
        state.rebuild_probe_cache_scan_identity_index();
    }

    fn enqueue_test_warm_rows(state: &mut BrowseState, count: usize, prefix: &str) -> Vec<PathBuf> {
        let mut rows = Vec::new();
        let mut paths = Vec::new();

        for idx in 0..count {
            let path = PathBuf::from(format!("/tmp/{prefix}-{idx}.flac"));
            let identity = ProbeCacheIdentity { modified: None, size: idx as u64 + 1 };
            let entry = BrowseEntry::new(
                path.clone(),
                format!("{prefix}-{idx}.flac"),
                EntryKind::AudioFile(AudioFormat::Flac),
                identity.size,
                identity.modified,
            );
            state.all_files.push(entry.clone());
            state.entries.push(entry);
            rows.push(ProbeCacheWarmRow {
                path: path.clone(),
                identity,
                info: test_cached_info(identity.size, &format!("{prefix}-{idx}")),
            });
            paths.push(path);
        }

        state.rebuild_probe_cache_scan_identity_index();

        let generation = state.scan_generation;
        let directory = state.current_dir.clone();
        assert_eq!(state.enqueue_probe_cache_warm_rows(generation, directory, rows), count);
        paths
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
                folder_count: 0,
                file_count: 99,
                audio_count: 9,
                audio_size: 0,
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
                folder_count: 0,
                file_count: 3,
                audio_count: 2,
                audio_size: 0,
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
        set_scan_files_for_test(&mut state, vec![BrowseEntry::new(
            path.clone(),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            new_identity.size,
            new_identity.modified,
        )]);
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
        set_scan_files_for_test(&mut state, vec![BrowseEntry::new(
            path.clone(),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            new_identity.size,
            new_identity.modified,
        )]);
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
        set_scan_files_for_test(&mut state, vec![BrowseEntry::new(
            path.clone(),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            fresh.size,
            fresh.modified,
        )]);
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
            let active_path = dir.path().join(format!("active-{idx}.flac"));
            state.browse_cold_probe_active.insert(
                active_path,
                BrowseColdProbeActiveJob {
                    scan_generation: state.scan_generation,
                    cursor_focused: false,
                    cancel: Arc::new(AtomicBool::new(false)),
                },
            );
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
        assert!(!state.browse_cold_probe_active.contains_key(&current));
    }

    #[tokio::test]
    async fn queued_cursor_cold_probe_starts_when_slot_opens() {
        let (_dir, path, entry, identity, _mtime) = probe_file_fixture("queued-current.flac", b"queued");
        let mut state = BrowseState::new();
        state.entries = vec![entry];
        state.selected_index = 0;
        for idx in 0..BROWSE_COLD_PROBE_MAX_IN_FLIGHT {
            let active_path = PathBuf::from(format!("/tmp/tonepoet-active-{idx}.flac"));
            state.browse_cold_probe_active.insert(
                active_path,
                BrowseColdProbeActiveJob {
                    scan_generation: state.scan_generation,
                    cursor_focused: false,
                    cancel: Arc::new(AtomicBool::new(false)),
                },
            );
        }
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        state.schedule_cursor_focused_cold_probe(path.clone(), identity, &tx);
        assert_eq!(state.browse_cold_probe_queue.len(), 1);

        state.complete_browse_cold_probe(Path::new("/tmp/tonepoet-active-0.flac"), &tx);

        assert!(state.browse_cold_probe_active.contains_key(&path));
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
    async fn raw_cursor_movement_uses_scanned_entry_identity_without_restat() {
        let (_dir, path, entry, identity, _mtime) = probe_file_fixture("deleted-after-scan.flac", b"cached");
        let db = crate::db::Database::open_memory().expect("db");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = BrowseState::new();
        state.entries = vec![entry];
        state.selected_index = 0;
        state.probe_cache.insert(
            path.clone(),
            ProbeCacheEntry::hit(identity, Arc::new(test_cached_info(identity.size, "scanned identity"))),
        );
        std::fs::remove_file(&path).expect("remove after scan");

        state.probe_current_with_db(&tx, Some(&db));

        assert!(
            state.probe_cache.contains_key(&path),
            "raw cursor movement must not evict identity-valid in-memory cache via metadata()"
        );
        assert!(state.probe_debounce.is_none());
        assert_eq!(
            state.current_cached_info().and_then(|info| info.metadata.title.as_deref()),
            Some("scanned identity")
        );
    }

    #[tokio::test]
    async fn sqlite_probe_hit_is_debounced_not_raw_cursor_work() {
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

        assert_eq!(state.probe_debounce.as_ref().map(|d| d.path.as_path()), Some(path.as_path()));
        assert!(!state.probe_pending.contains(&path));
        assert!(
            !state.probe_cache.contains_key(&path),
            "raw cursor movement must not perform a SQLite point lookup"
        );
        assert!(state.probe_cache_needs_metadata_enrichment.is_empty());
        assert!(state.current_cached_info().is_none());

        state.probe_debounce.as_mut().expect("debounce").deadline = Instant::now() - Duration::from_millis(1);
        state.check_probe_debounce_with_db(&tx, Some(&db));

        assert!(state.probe_pending.contains(&path));
        assert!(!state.probe_cache_needs_metadata_enrichment.contains(&path));
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
    fn warm_cache_merge_uses_scan_owned_exact_path_identity_lookup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let real_path = temp.path().join("warm-identity.flac");
        std::fs::write(&real_path, b"warm cache identity fixture").expect("fixture");
        let scanned_path = canonical_equivalent_focus_path(&real_path);
        assert!(same_path(&scanned_path, &real_path), "fixture must be canonical-equivalent");
        assert!(
            !same_scanned_path(&scanned_path, &real_path),
            "fixture must be syntactically distinct so warm-row merge proves it does not canonicalize",
        );

        let metadata = std::fs::metadata(&real_path).expect("metadata");
        let identity = ProbeCacheIdentity::from_metadata(&metadata);
        let mut state = BrowseState::new();
        set_scan_files_for_test(&mut state, vec![BrowseEntry::new(
            scanned_path.clone(),
            "warm-identity.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            identity.size,
            identity.modified,
        )]);

        let merged = state.merge_probe_cache_warm_rows(vec![ProbeCacheWarmRow {
            path: real_path.clone(),
            identity,
            info: test_cached_info(identity.size, "canonical-equivalent but not scan-owned"),
        }]);
        assert_eq!(
            merged, 0,
            "warm-cache merge must not canonicalize row paths to find current scan identity",
        );
        assert!(state.probe_cache.is_empty());

        let merged = state.merge_probe_cache_warm_rows(vec![ProbeCacheWarmRow {
            path: scanned_path.clone(),
            identity,
            info: test_cached_info(identity.size, "scan-owned exact path"),
        }]);
        assert_eq!(merged, 1);
        assert!(state.probe_cache.contains_key(&scanned_path));
        assert!(!state.probe_cache.contains_key(&real_path));
    }

    #[test]
    fn warm_cache_merge_uses_scan_owned_exact_path_identity_index() {
        let path = PathBuf::from("/tmp/tonepoet-indexed-warm.flac");
        let identity = ProbeCacheIdentity { modified: None, size: 4096 };
        let entry = BrowseEntry::new(
            path.clone(),
            "tonepoet-indexed-warm.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            identity.size,
            identity.modified,
        );

        let mut state = BrowseState::new();
        state.entries = vec![entry.clone()];
        let merged = state.merge_probe_cache_warm_rows(vec![ProbeCacheWarmRow {
            path: path.clone(),
            identity,
            info: test_cached_info(identity.size, "visible-only"),
        }]);
        assert_eq!(
            merged, 0,
            "warm-cache identity validation must not fall back to scanning the visible entries vector",
        );

        set_scan_files_for_test(&mut state, vec![entry]);
        let merged = state.merge_probe_cache_warm_rows(vec![ProbeCacheWarmRow {
            path: path.clone(),
            identity,
            info: test_cached_info(identity.size, "scan-indexed"),
        }]);
        assert_eq!(merged, 1);
        assert!(state.probe_cache.contains_key(&path));
    }

    #[test]
    fn batch_warm_merge_ignores_rows_with_stale_listing_identity() {
        let path = PathBuf::from("/tmp/tonepoet-warm-stale.flac");
        let fresh = ProbeCacheIdentity { modified: None, size: 200 };
        let stale = ProbeCacheIdentity { modified: None, size: 100 };
        let mut state = BrowseState::new();
        set_scan_files_for_test(&mut state, vec![BrowseEntry::new(
            path.clone(),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            fresh.size,
            fresh.modified,
        )]);

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
        set_scan_files_for_test(&mut state, vec![BrowseEntry::new(
            path.clone(),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            fresh.size,
            fresh.modified,
        )]);
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
    fn settled_warm_cache_drain_can_exceed_legacy_slice_but_obeys_row_cap() {
        let mut state = BrowseState::new();
        let row_cap = PROBE_CACHE_WARM_MERGE_MIN_ROWS_PER_TICK * 3;
        let count = row_cap + 7;
        enqueue_test_warm_rows(&mut state, count, "tonepoet-aggressive-warm");

        let (merged, has_more) = state.drain_probe_cache_warm_rows_for_frame_with_budget(
            ProbeCacheWarmDrainBudget {
                min_rows: PROBE_CACHE_WARM_MERGE_MIN_ROWS_PER_TICK,
                max_rows: row_cap,
                time_budget: Duration::from_secs(60),
                time_check_interval: PROBE_CACHE_WARM_MERGE_TIME_CHECK_INTERVAL,
            },
        );

        assert!(
            merged > PROBE_CACHE_WARM_MERGE_MIN_ROWS_PER_TICK,
            "settled warm-cache draining should process more than the legacy 128-row slice when budget allows"
        );
        assert_eq!(merged, row_cap);
        assert!(has_more);
        assert_eq!(state.probe_cache.len(), row_cap);
    }

    #[test]
    fn tiny_time_budget_still_merges_minimum_warm_rows() {
        let mut state = BrowseState::new();
        let count = PROBE_CACHE_WARM_MERGE_MIN_ROWS_PER_TICK + 10;
        enqueue_test_warm_rows(&mut state, count, "tonepoet-minimum-warm");

        let (merged, has_more) = state.drain_probe_cache_warm_rows_for_frame_with_budget(
            ProbeCacheWarmDrainBudget {
                min_rows: PROBE_CACHE_WARM_MERGE_MIN_ROWS_PER_TICK,
                max_rows: PROBE_CACHE_WARM_MERGE_MAX_ROWS_PER_TICK,
                time_budget: Duration::from_nanos(0),
                time_check_interval: 1,
            },
        );

        assert_eq!(merged, PROBE_CACHE_WARM_MERGE_MIN_ROWS_PER_TICK);
        assert!(has_more);
        assert_eq!(state.probe_cache.len(), PROBE_CACHE_WARM_MERGE_MIN_ROWS_PER_TICK);
    }

    #[test]
    fn partially_consumed_warm_batch_is_requeued_at_front() {
        let mut state = BrowseState::new();
        let first_batch = enqueue_test_warm_rows(&mut state, 5, "tonepoet-front-warm-a");
        let second_batch = enqueue_test_warm_rows(&mut state, 2, "tonepoet-front-warm-b");
        let budget = ProbeCacheWarmDrainBudget {
            min_rows: 3,
            max_rows: 3,
            time_budget: Duration::from_secs(60),
            time_check_interval: 64,
        };

        let (merged, has_more) = state.drain_probe_cache_warm_rows_for_frame_with_budget(budget);
        assert_eq!(merged, 3);
        assert!(has_more);
        assert!(state.probe_cache.contains_key(&first_batch[0]));
        assert!(state.probe_cache.contains_key(&first_batch[2]));
        assert!(!state.probe_cache.contains_key(&first_batch[3]));
        assert!(
            !state.probe_cache.contains_key(&second_batch[0]),
            "a partially consumed first batch must remain at the front ahead of later batches"
        );

        let (merged, has_more) = state.drain_probe_cache_warm_rows_for_frame_with_budget(budget);
        assert_eq!(merged, 3);
        assert!(has_more);
        assert!(state.probe_cache.contains_key(&first_batch[3]));
        assert!(state.probe_cache.contains_key(&first_batch[4]));
        assert!(state.probe_cache.contains_key(&second_batch[0]));
    }

    #[test]
    fn warm_cache_probe_backed_refresh_remains_coalesced_until_backlog_drains() {
        let mut state = BrowseState::new();
        let count = PROBE_CACHE_WARM_MERGE_MIN_ROWS_PER_TICK + 3;
        enqueue_test_warm_rows(&mut state, count, "tonepoet-coalesced-warm");
        let budget = ProbeCacheWarmDrainBudget {
            min_rows: PROBE_CACHE_WARM_MERGE_MIN_ROWS_PER_TICK,
            max_rows: PROBE_CACHE_WARM_MERGE_MIN_ROWS_PER_TICK,
            time_budget: Duration::from_secs(60),
            time_check_interval: PROBE_CACHE_WARM_MERGE_TIME_CHECK_INTERVAL,
        };

        let (merged, has_more) = state.drain_probe_cache_warm_rows_for_frame_with_budget(budget);
        assert_eq!(merged, PROBE_CACHE_WARM_MERGE_MIN_ROWS_PER_TICK);
        assert!(has_more);
        let work = state.take_browse_deferred_work();
        assert!(work.probe_backed_resort_needed);
        assert!(
            !work.info_pane_changed,
            "warm-cache backlog should refresh the current info pane only once the backlog drains"
        );

        let (merged, has_more) = state.drain_probe_cache_warm_rows_for_frame_with_budget(budget);
        assert_eq!(merged, 3);
        assert!(!has_more);
        let work = state.take_browse_deferred_work();
        assert!(work.probe_backed_resort_needed);
        assert!(work.info_pane_changed);
    }

    #[test]
    fn warm_cache_queue_drops_stale_generation_before_merge() {
        let mut state = BrowseState::new();
        let path = PathBuf::from("/tmp/tonepoet-stale-generation-warm.flac");
        let identity = ProbeCacheIdentity { modified: None, size: 1 };
        set_scan_files_for_test(&mut state, vec![BrowseEntry::new(
            path.clone(),
            "stale-generation-warm.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            identity.size,
            identity.modified,
        )]);

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

        let staging_guard = ArchiveStagingSession::new_test_owned(
            staging_dir.clone(),
            archive_path.clone(),
            0,
            0,
            0,
        );
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
            staging: Some(staging_guard.clone_session()),
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
    fn scan_worker_does_not_apply_cached_iso_classification_before_publication() {
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
        assert!(matches!(entry.kind, EntryKind::Archive));
        assert!(updates.sacd_iso.is_empty(), "scan publication must not do native-disc promotion work");
    }

    #[test]
    fn scan_worker_does_not_probe_iso_or_emit_negative_classification_updates() {
        let td = tempfile::tempdir().expect("tempdir");
        let path = td.path().join("data.iso");
        std::fs::write(&path, b"not really an iso").unwrap();
        let snapshot = BrowseClassificationCacheSnapshot::default();
        let cancel = std::sync::atomic::AtomicBool::new(false);

        let (_parent, _dirs, files, updates) =
            scan_directory_blocking(td.path(), &cancel, &snapshot).expect("scan");

        let entry = files.iter().find(|entry| entry.path == path).expect("iso entry");
        assert!(matches!(entry.kind, EntryKind::Archive));
        assert!(updates.sacd_iso.is_empty());
        assert!(updates.dvda_iso.is_empty());
        assert!(updates.dvdv_iso.is_empty());
        assert!(updates.bluray_iso.is_empty());
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
    fn audio_only_filter_keeps_lazy_iso_candidates_visible() {
        let sacd_candidate = BrowseEntry::new(
            std::path::PathBuf::from("/tmp/album.iso"),
            "album.iso".to_string(),
            EntryKind::Archive,
            0,
            None,
        );
        let zip_archive = BrowseEntry::new(
            std::path::PathBuf::from("/tmp/album.zip"),
            "album.zip".to_string(),
            EntryKind::Archive,
            0,
            None,
        );

        assert!(
            FormatFilter::AudioOnly.allows_entry(&sacd_candidate),
            "lazy .iso disc candidates must remain visible under AudioOnly until settled-focus promotion can classify them",
        );
        assert!(is_audio_filter_visible_entry(&sacd_candidate));
        assert!(
            !FormatFilter::AudioOnly.allows_entry(&zip_archive),
            "AudioOnly should not widen to every generic archive",
        );
        assert!(!is_audio_filter_visible_entry(&zip_archive));
    }

    #[test]
    fn search_audio_only_keeps_lazy_iso_candidates_visible() {
        let mut state = BrowseState::new();
        let iso = BrowseEntry::new(
            std::path::PathBuf::from("/tmp/Cached SACD.iso"),
            "Cached SACD.iso".to_string(),
            EntryKind::Archive,
            0,
            None,
        );
        let zip = BrowseEntry::new(
            std::path::PathBuf::from("/tmp/Cached SACD.zip"),
            "Cached SACD.zip".to_string(),
            EntryKind::Archive,
            0,
            None,
        );

        state.execute_search_over_entries(
            "cached sacd",
            true,
            true,
            FormatFilter::Off,
            SearchMode::Filename,
            vec![iso.clone(), zip],
        );

        let names = result_names_without_parent(&state);
        assert_eq!(names, vec![iso.name]);
    }

    #[test]
    fn apply_view_audio_only_keeps_cached_lazy_sacd_iso_candidate_visible() {
        let mut state = BrowseState::new();
        let iso_path = std::path::PathBuf::from("/tmp/Cached SACD.iso");
        let fingerprint = ClassificationFingerprint {
            len: 0,
            modified: None,
            markers: Vec::new(),
        };
        state
            .sacd_classify_cache
            .insert(iso_path.clone(), (fingerprint, true));
        state.all_files = vec![BrowseEntry::new(
            iso_path.clone(),
            "Cached SACD.iso".to_string(),
            EntryKind::Archive,
            0,
            None,
        )];
        state.format_filter = FormatFilter::AudioOnly;

        state.apply_view();

        assert!(
            state.entries.iter().any(|entry| entry.path == iso_path),
            "a cached-positive SACD ISO that is still lazily typed as Archive must not be filtered out before focus promotion",
        );
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
    fn compute_dir_stats_returns_none_when_entry_budget_is_exhausted() {
        let td = tempfile::tempdir().expect("tempdir");
        for idx in 0..5usize {
            std::fs::write(td.path().join(format!("file-{idx}.txt")), b"x").expect("file");
        }
        let cancel = AtomicBool::new(false);
        let budget = DirStatsWalkBudget {
            max_depth: 4,
            max_files: 100,
            max_entries: 2,
            max_millis: 1_000,
        };
        assert!(
            compute_dir_stats_with_budget(td.path(), &cancel, budget).is_none(),
            "stats walks should stop at the hard entry budget instead of traversing everything"
        );
    }

    #[test]
    fn compute_dir_stats_tracks_folder_count_and_audio_size_separately_from_total_size() {
        let td = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(td.path().join("nested")).expect("nested dir");
        std::fs::write(td.path().join("nested").join("track.flac"), b"audio-bytes").expect("audio file");
        std::fs::write(td.path().join("cover.jpg"), b"art").expect("non-audio file");
        let cancel = AtomicBool::new(false);
        let budget = DirStatsWalkBudget {
            max_depth: 4,
            max_files: 100,
            max_entries: 100,
            max_millis: 1_000,
        };

        let stats = compute_dir_stats_with_budget(td.path(), &cancel, budget).expect("stats");

        assert_eq!(stats.folder_count, 1);
        assert_eq!(stats.file_count, 2);
        assert_eq!(stats.audio_count, 1);
        assert_eq!(stats.audio_size, b"audio-bytes".len() as u64);
        assert_eq!(stats.total_size, (b"audio-bytes".len() + b"art".len()) as u64);
    }

    #[test]
    fn scan_directory_blocking_leaves_bluray_dirs_unpromoted() {
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

        assert!(matches!(entry.kind, EntryKind::Directory));
        assert!(updates.bluray_dir.is_empty());
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
        let forbidden_fresh_probe_drop = format!(
            "{}{}",
            "read_metadata(&path_for_task)",
            ".unwrap_or_default()"
        );
        assert!(
            !source.contains(&forbidden_fresh_probe_drop),
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
        let synthetic_path = archive_path.join(inner_path);
        let identity = state
            .all_files
            .iter()
            .chain(state.all_dirs.iter())
            .find(|entry| entry.path == synthetic_path)
            .map(ProbeCacheIdentity::from_entry)
            .or_else(|| {
                state
                    .archive
                    .as_ref()
                    .and_then(|arc| {
                        arc.listing
                            .entries
                            .iter()
                            .find(|entry| entry.path == inner_path)
                    })
                    .map(|entry| ProbeCacheIdentity {
                        modified: None,
                        size: entry.size,
                    })
            })
            .unwrap_or(ProbeCacheIdentity { modified: None, size: 4096 });
        state.probe_cache.insert(
            synthetic_path,
            ProbeCacheEntry::hit(
                identity,
                std::sync::Arc::new(CachedInfo {
                    source: crate::tui::probe::SourceInfo {
                        format_name: "FLAC".to_string(),
                        codec: "flac".to_string(),
                        bit_depth: Some(16),
                        sample_rate: 44_100,
                        channels: 2,
                        channel_layout: "stereo".to_string(),
                        duration_secs: 1.0,
                        file_size: identity.size,
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
        let staging_guard = ArchiveStagingSession::new_test_owned(
            staging_dir.clone(),
            archive_path,
            0,
            0,
            0,
        );
        staging_guard
            .install_clone_into_browse_state(&mut state)
            .expect("archive entered");
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
        // Log-coalescing tests do not create a real staging directory, but use
        // the test-owned constructor anyway so the test surface consistently
        // avoids direct staging construction.
        ArchiveStagingSession::new_test_owned(
            std::path::PathBuf::from("/tmp/staging"),
            std::path::PathBuf::from("/tmp/archive.zip"),
            1,
            2,
            3,
        )
        .into_inner()
    }

    #[test]
    fn test_owned_archive_staging_removes_directory_on_drop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let staging_dir = temp.path().join("tonepoet-archive-rename-owned");
        std::fs::create_dir_all(staging_dir.join("nested")).expect("create staging");
        std::fs::write(staging_dir.join("nested/file.flac"), b"fixture").expect("write fixture");

        {
            let _guard = ArchiveStagingSession::new_test_owned(
                staging_dir.clone(),
                temp.path().join("album.zip"),
                1,
                2,
                3,
            );
            assert!(staging_dir.exists());
        }

        assert!(!staging_dir.exists());
    }

    #[test]
    fn test_owned_archive_staging_into_inner_disarms_cleanup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let staging_dir = temp.path().join("tonepoet-archive-rename-handoff");
        std::fs::create_dir_all(&staging_dir).expect("create staging");

        let session = ArchiveStagingSession::new_test_owned(
            staging_dir.clone(),
            temp.path().join("album.zip"),
            1,
            2,
            3,
        )
        .into_inner();
        drop(session);

        assert!(staging_dir.exists());
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

    fn focused_state_for_existing_entry(path: PathBuf, name: &str, kind: EntryKind) -> BrowseState {
        let metadata = std::fs::metadata(&path).expect("entry metadata");
        let mut state = BrowseState::new();
        state.entries = vec![BrowseEntry::new(
            path,
            name.to_string(),
            kind,
            metadata.len(),
            metadata.modified().ok(),
        )];
        state.selected_index = 0;
        state
    }

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
        // An isolated directory: using a shared location like env::temp_dir()
        // makes the captured identity race with unrelated tests mutating it.
        let active_dir = tempfile::tempdir().expect("isolated active dir");
        let active = active_dir.path().to_path_buf();
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

    #[tokio::test]
    async fn stale_active_recursive_dir_stats_are_cancelled_before_queueing_current() {
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


    #[test]
    fn file_backed_directory_summary_cache_restores_identity_scoped_facts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("Persistent Album");
        std::fs::create_dir_all(&album).expect("album fixture");
        std::fs::write(album.join("01.flac"), b"not real audio").expect("track fixture");
        let cache_path = temp.path().join("directory-summary-cache.tsv");
        let identity = ProbeCacheIdentity::from_metadata(&std::fs::metadata(&album).expect("album metadata"));
        let classification = classify_folder_content_blocking(&album, identity);

        let mut writer = BrowseState::new();
        writer
            .enable_file_backed_directory_summary_cache(cache_path.clone())
            .expect("enable persistent cache");
        writer.insert_folder_classification_for_identity(album.clone(), identity, classification);
        writer.insert_dir_stats_for_identity(
            album.clone(),
            identity,
            DirStats {
                folder_count: 0,
                file_count: 1,
                audio_count: 1,
                audio_size: 0,
                total_size: 123,
            },
        );
        writer.flush_directory_summary_persistent_cache().expect("flush cache");

        let entry = BrowseEntry::new(
            album.clone(),
            "Persistent Album".to_string(),
            EntryKind::Directory,
            identity.size,
            identity.modified,
        );
        let mut reader = BrowseState::new();
        reader.entries = vec![entry];
        reader.selected_index = 0;
        let loaded = reader
            .enable_file_backed_directory_summary_cache(cache_path)
            .expect("load persistent cache");

        assert_eq!(loaded, 1);
        assert_eq!(
            reader.current_folder_classification().map(|classification| classification.kind),
            Some(FolderClassificationKind::Album)
        );
        let facts = reader.current_directory_summary().expect("restored facts");
        assert_eq!(facts.classification_scope, Some(DirectorySummaryScope::Immediate));
        assert_eq!(facts.stats_scope, Some(DirectorySummaryScope::RecursiveBestEffort));
        assert_eq!(facts.stats.as_ref().map(|stats| stats.audio_count), Some(1));
    }

    #[test]
    fn v2_directory_summary_cache_does_not_render_unknown_folder_count_as_zero() {
        let path = PathBuf::from("/music/legacy-artist");
        let identity = ProbeCacheIdentity { modified: None, size: 42 };
        let mut entry = DirectorySummaryCacheEntry::new(identity);
        entry.facts.classification_scope = Some(DirectorySummaryScope::Immediate);
        entry.facts.classification = Some(Arc::new(FolderContentClassification {
            kind: FolderClassificationKind::Collection,
            identity,
            audio: FolderAudioSummary::default(),
            units: Vec::new(),
            unit_count: 24,
            collection_many: true,
            io_budget_exhausted: false,
            disc_marker: None,
        }));
        entry.facts.stats_scope = Some(DirectorySummaryScope::RecursiveBestEffort);
        entry.facts.stats = Some(Arc::new(DirStats {
            folder_count: 24,
            file_count: 312,
            audio_count: 247,
            audio_size: 12_400_000_000,
            total_size: 14_100_000_000,
        }));

        let mut fields: Vec<String> = entry
            .to_persistent_line(&path)
            .split('\t')
            .map(ToString::to_string)
            .collect();
        fields[1] = "2".to_string();
        fields.remove(16); // v2 did not record folder_count.
        let legacy_v2_line = fields.join("\t");

        let (restored_path, restored) = DirectorySummaryCacheEntry::from_persistent_line(&legacy_v2_line)
            .expect("v2 row should still restore classification facts");

        assert_eq!(restored_path, path);
        assert_eq!(
            restored.facts.classification.as_ref().map(|classification| classification.kind),
            Some(FolderClassificationKind::Collection)
        );
        assert!(
            restored.facts.stats.is_none(),
            "v2 stats must not be restored as folder_count = 0 because folder_count was unknown"
        );
        assert_eq!(restored.facts.stats_scope, None);
    }

    #[test]
    fn v3_directory_summary_cache_round_trips_folder_count() {
        let path = PathBuf::from("/music/v3-artist");
        let identity = ProbeCacheIdentity { modified: None, size: 42 };
        let mut entry = DirectorySummaryCacheEntry::new(identity);
        entry.facts.stats_scope = Some(DirectorySummaryScope::RecursiveBestEffort);
        entry.facts.stats = Some(Arc::new(DirStats {
            folder_count: 24,
            file_count: 312,
            audio_count: 247,
            audio_size: 12_400_000_000,
            total_size: 14_100_000_000,
        }));

        let (restored_path, restored) = DirectorySummaryCacheEntry::from_persistent_line(
            &entry.to_persistent_line(&path),
        )
        .expect("v3 row should restore stats");
        let stats = restored.facts.stats.as_ref().expect("v3 stats");

        assert_eq!(restored_path, path);
        assert_eq!(restored.facts.stats_scope, Some(DirectorySummaryScope::RecursiveBestEffort));
        assert_eq!(stats.folder_count, 24);
        assert_eq!(stats.file_count, 312);
        assert_eq!(stats.audio_count, 247);
        assert_eq!(stats.audio_size, 12_400_000_000);
        assert_eq!(stats.total_size, 14_100_000_000);
    }

    #[tokio::test]
    async fn database_directory_summary_cache_restores_after_debounce_in_cached_only_policy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("Persistent SQLite Album");
        std::fs::create_dir_all(&album).expect("album fixture");
        std::fs::write(album.join("01.flac"), b"not real audio").expect("track fixture");
        let identity = ProbeCacheIdentity::from_metadata(&std::fs::metadata(&album).expect("album metadata"));
        let classification = classify_folder_content_blocking(&album, identity);
        assert_eq!(classification.kind, FolderClassificationKind::Album);

        let db = crate::db::Database::open_memory().expect("db");
        let mut writer = BrowseState::new();
        writer.enable_database_directory_summary_cache();
        writer.insert_folder_classification_for_identity(album.clone(), identity, classification);
        writer.insert_dir_stats_for_identity(
            album.clone(),
            identity,
            DirStats {
                folder_count: 0,
                file_count: 1,
                audio_count: 1,
                audio_size: 0,
                total_size: 123,
            },
        );
        writer.store_directory_summary_for_identity_best_effort(&album, identity, &db);

        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut reader = focused_state_for_existing_entry(
            album.clone(),
            "Persistent SQLite Album",
            EntryKind::Directory,
        );
        reader.enable_database_directory_summary_cache();
        reader.set_directory_summary_cold_work_policy(BrowseDirectorySummaryColdWorkPolicy::CachedOnly);

        reader.probe_current_with_db(&tx, Some(&db));
        assert!(
            reader.probe_debounce.is_some(),
            "raw cursor movement may arm a settled-focus DB summary lookup but must not read SQLite immediately",
        );
        assert!(reader.current_directory_summary().is_none());
        reader
            .probe_debounce
            .as_mut()
            .expect("debounce armed")
            .deadline = std::time::Instant::now() - Duration::from_millis(1);
        reader.check_probe_debounce_with_db(&tx, Some(&db));

        assert_eq!(
            reader.current_folder_classification().map(|classification| classification.kind),
            Some(FolderClassificationKind::Album),
        );
        let facts = reader.current_directory_summary().expect("restored db-backed facts");
        assert_eq!(facts.classification_scope, Some(DirectorySummaryScope::Immediate));
        assert_eq!(facts.stats_scope, Some(DirectorySummaryScope::RecursiveBestEffort));
        assert_eq!(facts.stats.as_ref().map(|stats| stats.audio_count), Some(1));
        assert!(!reader.folder_classification_pending_for(&album));
        assert!(!reader.dir_stats_pending.contains(&album));
    }

    #[test]
    fn cursor_movement_cancels_stale_active_audio_probe() {
        let mut state = BrowseState::new();
        let stale = PathBuf::from("/tmp/stale-active-audio.flac");
        let stale_cancel = Arc::new(AtomicBool::new(false));
        state.browse_cold_probe_active.insert(
            stale.clone(),
            BrowseColdProbeActiveJob {
                scan_generation: state.scan_generation,
                cursor_focused: true,
                cancel: stale_cancel.clone(),
            },
        );
        state.probe_pending.insert(stale.clone());
        state.probe_cancel.insert(stale.clone(), stale_cancel.clone());
        state.entries = vec![BrowseEntry::new(
            PathBuf::from("/tmp/not-a-probe.txt"),
            "not-a-probe.txt".to_string(),
            EntryKind::OtherFile,
            0,
            None,
        )];
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        state.probe_current_with_db(&tx, None);

        assert!(stale_cancel.load(Ordering::Relaxed));
        assert!(!state.browse_cold_probe_active.contains_key(&stale));
        assert!(!state.probe_pending.contains(&stale));
    }

    #[test]
    fn cursor_movement_cancels_stale_active_dir_stats_when_focus_moves_to_file() {
        let mut state = BrowseState::new();
        let stale_dir = PathBuf::from("/tmp/stale-active-stats");
        let file = PathBuf::from("/tmp/focused-file.flac");
        let stale_cancel = Arc::new(AtomicBool::new(false));
        state.dir_stats_active.insert(
            stale_dir.clone(),
            DirStatsActiveJob {
                identity: ProbeCacheIdentity { modified: None, size: 0 },
                scan_generation: state.scan_generation,
                cursor_focused: true,
                cancel: stale_cancel.clone(),
            },
        );
        state.dir_stats_pending.insert(stale_dir.clone());
        state.entries = vec![BrowseEntry::new(
            file.clone(),
            "focused-file.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            0,
            None,
        )];
        state.selected_index = 0;

        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        state.probe_current_with_db(&tx, None);

        assert!(
            stale_cancel.load(Ordering::Relaxed),
            "moving focus away must cancel stale recursive stats immediately",
        );
        assert!(!state.dir_stats_active.contains_key(&stale_dir));
        assert!(!state.dir_stats_pending.contains(&stale_dir));
    }

    #[test]
    fn cursor_movement_cancels_stale_active_disc_probe() {
        let mut state = BrowseState::new();
        let stale = PathBuf::from("/tmp/stale-active-disc.iso");
        let stale_cancel = Arc::new(AtomicBool::new(false));
        state.disc_probe_active.insert(
            stale.clone(),
            DiscProbeActiveJob {
                scan_generation: state.scan_generation,
                cursor_focused: true,
                cancel: stale_cancel.clone(),
            },
        );
        state.disc_probe_pending.insert(stale.clone());
        state.entries = vec![BrowseEntry::new(
            PathBuf::from("/tmp/not-a-disc.txt"),
            "not-a-disc.txt".to_string(),
            EntryKind::OtherFile,
            0,
            None,
        )];
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        state.probe_current_with_db(&tx, None);

        assert!(stale_cancel.load(Ordering::Relaxed));
        assert!(!state.disc_probe_active.contains_key(&stale));
        assert!(!state.disc_probe_pending.contains(&stale));
    }

    fn canonical_equivalent_focus_path(real_path: &Path) -> PathBuf {
        let parent = real_path.parent().expect("fixture parent");
        let alias_dir = parent.join("alias-for-canonicalize-test");
        std::fs::create_dir_all(&alias_dir).expect("alias fixture dir");
        alias_dir
            .join("..")
            .join(real_path.file_name().expect("fixture file name"))
    }

    #[test]
    fn raw_cursor_movement_audio_cancellation_does_not_canonicalize_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let active_path = temp.path().join("track.flac");
        std::fs::write(&active_path, b"not real audio").expect("audio fixture");
        let focused_path = canonical_equivalent_focus_path(&active_path);
        assert!(same_path(&active_path, &focused_path), "fixture must be canonical-equivalent");
        assert!(
            !same_scanned_path(&active_path, &focused_path),
            "fixture must be syntactically distinct so raw comparison proves it does not canonicalize",
        );

        let mut state = BrowseState::new();
        let cancel = Arc::new(AtomicBool::new(false));
        state.browse_cold_probe_active.insert(
            active_path.clone(),
            BrowseColdProbeActiveJob {
                scan_generation: state.scan_generation,
                cursor_focused: true,
                cancel: cancel.clone(),
            },
        );
        state.probe_pending.insert(active_path.clone());
        state.probe_cancel.insert(active_path.clone(), cancel.clone());
        state.entries = vec![BrowseEntry::new(
            focused_path.clone(),
            "track.flac".to_string(),
            EntryKind::AudioFile(AudioFormat::Flac),
            0,
            None,
        )];
        state.selected_index = 0;
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        state.probe_current_with_db(&tx, None);

        assert!(
            cancel.load(Ordering::Relaxed),
            "raw cursor movement must treat only exact scan-owned paths as current; canonicalizing would keep this stale job alive",
        );
        assert!(!state.browse_cold_probe_active.contains_key(&active_path));
        assert!(!state.probe_pending.contains(&active_path));
    }

    #[test]
    fn raw_cursor_movement_disc_cancellation_does_not_canonicalize_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let active_path = temp.path().join("disc.iso");
        std::fs::write(&active_path, b"not a real disc image").expect("disc fixture");
        let focused_path = canonical_equivalent_focus_path(&active_path);
        assert!(same_path(&active_path, &focused_path));
        assert!(!same_scanned_path(&active_path, &focused_path));

        let mut state = BrowseState::new();
        let cancel = Arc::new(AtomicBool::new(false));
        state.disc_probe_active.insert(
            active_path.clone(),
            DiscProbeActiveJob {
                scan_generation: state.scan_generation,
                cursor_focused: true,
                cancel: cancel.clone(),
            },
        );
        state.disc_probe_pending.insert(active_path.clone());
        state.entries = vec![BrowseEntry::new(
            focused_path.clone(),
            "disc.iso".to_string(),
            EntryKind::SacdIso,
            0,
            None,
        )];
        state.selected_index = 0;
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        state.probe_current_with_db(&tx, None);

        assert!(
            cancel.load(Ordering::Relaxed),
            "raw disc-probe cancellation must not canonicalize current/active paths",
        );
        assert!(!state.disc_probe_active.contains_key(&active_path));
        assert!(!state.disc_probe_pending.contains(&active_path));
    }

    #[test]
    fn raw_cursor_movement_dir_stats_cancellation_does_not_canonicalize_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let active_path = temp.path().join("Album");
        std::fs::create_dir_all(&active_path).expect("album fixture");
        let focused_path = canonical_equivalent_focus_path(&active_path);
        assert!(same_path(&active_path, &focused_path));
        assert!(!same_scanned_path(&active_path, &focused_path));

        let mut state = BrowseState::new();
        let cancel = Arc::new(AtomicBool::new(false));
        state.dir_stats_active.insert(
            active_path.clone(),
            DirStatsActiveJob {
                identity: ProbeCacheIdentity { modified: None, size: 0 },
                scan_generation: state.scan_generation,
                cursor_focused: true,
                cancel: cancel.clone(),
            },
        );
        state.dir_stats_pending.insert(active_path.clone());
        state.entries = vec![BrowseEntry::new(
            focused_path.clone(),
            "Album".to_string(),
            EntryKind::Directory,
            0,
            None,
        )];
        state.selected_index = 0;
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        state.probe_current_with_db(&tx, None);

        assert!(
            cancel.load(Ordering::Relaxed),
            "raw directory-stats cancellation must not canonicalize current/active paths",
        );
        assert!(!state.dir_stats_active.contains_key(&active_path));
        assert!(!state.dir_stats_pending.contains(&active_path));
    }

    #[test]
    fn cached_only_policy_cancels_active_hover_disc_probe() {
        let mut state = BrowseState::new();
        let disc = PathBuf::from("/tmp/active-hover-disc.iso");
        let cancel = Arc::new(AtomicBool::new(false));
        state.entries = vec![BrowseEntry::new(
            disc.clone(),
            "active-hover-disc.iso".to_string(),
            EntryKind::SacdIso,
            0,
            None,
        )];
        state.disc_probe_active.insert(
            disc.clone(),
            DiscProbeActiveJob {
                scan_generation: state.scan_generation,
                cursor_focused: true,
                cancel: cancel.clone(),
            },
        );
        state.disc_probe_pending.insert(disc.clone());

        state.set_directory_summary_cold_work_policy(BrowseDirectorySummaryColdWorkPolicy::CachedOnly);

        assert!(cancel.load(Ordering::Relaxed));
        assert!(!state.disc_probe_active.contains_key(&disc));
        assert!(!state.disc_probe_pending.contains(&disc));
    }
 }

#[cfg(test)]
mod folder_content_classification_tests {
    use super::*;

    fn identity_for(path: &Path) -> ProbeCacheIdentity {
        ProbeCacheIdentity::from_metadata(&std::fs::metadata(path).expect("fixture metadata"))
    }

    fn touch(path: &Path) {
        std::fs::write(path, b"not real audio; extension classification only").expect("fixture file");
    }

    #[test]
    fn direct_audio_classifies_as_album_without_descending() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        touch(&root.join("01 - opener.flac"));
        std::fs::create_dir_all(root.join("nested")).expect("nested fixture");
        touch(&root.join("nested").join("hidden.mp3"));

        let classification = classify_folder_content_blocking(root, identity_for(root));

        assert_eq!(classification.kind, FolderClassificationKind::Album);
        assert_eq!(classification.audio.track_count, 1);
        assert_eq!(classification.audio.dominant_format_label(), Some("FLAC"));
    }

    #[test]
    fn fanout_threshold_classifies_collection_before_deep_walk() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        for idx in 0..FOLDER_CLASSIFY_FAN_OUT_THRESHOLD {
            let album = root.join(format!("Album {idx:02}"));
            std::fs::create_dir_all(&album).expect("album fixture");
            touch(&album.join("track.flac"));
        }

        let classification = classify_folder_content_blocking(root, identity_for(root));

        assert_eq!(classification.kind, FolderClassificationKind::Collection);
        assert_eq!(classification.unit_count, FOLDER_CLASSIFY_FAN_OUT_THRESHOLD);
        assert!(classification.collection_many);
        assert!(classification.units.is_empty());
    }

    #[test]
    fn shallow_walk_distinguishes_unrelated_album_collection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        for album_name in ["Selling England by the Pound", "The Lamb Lies Down on Broadway"] {
            let album = root.join(album_name);
            std::fs::create_dir_all(&album).expect("album fixture");
            touch(&album.join("01.flac"));
        }

        let classification = classify_folder_content_blocking(root, identity_for(root));

        assert_eq!(classification.kind, FolderClassificationKind::Collection);
        assert_eq!(classification.unit_count, 2);
    }

    #[test]
    fn multidisc_siblings_are_one_logical_album() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        for disc_name in ["Disc 01", "Disc 02", "scans"] {
            std::fs::create_dir_all(root.join(disc_name)).expect("disc fixture");
        }
        touch(&root.join("Disc 01").join("01.flac"));
        touch(&root.join("Disc 02").join("01.flac"));

        let classification = classify_folder_content_blocking(root, identity_for(root));

        assert_eq!(classification.kind, FolderClassificationKind::MultiDisc);
        assert_eq!(classification.unit_count, 2);
        assert_eq!(classification.audio.track_count, 2);
    }

    #[test]
    fn depth_two_disc_marker_is_detected_as_single_disc() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let fragile = root.join("FRAGILE");
        std::fs::create_dir_all(fragile.join("BDMV")).expect("bdmv fixture");

        let classification = classify_folder_content_blocking(root, identity_for(root));

        assert_eq!(classification.kind, FolderClassificationKind::Disc);
        assert_eq!(classification.disc_marker, Some(FolderDiscMarkerKind::BluRay));
        assert_eq!(classification.disc_probe_source_path(root), fragile.as_path());
    }

    #[test]
    fn sacd_directory_name_is_not_a_disc_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        std::fs::create_dir_all(root.join("SACD")).expect("sacd-like fixture dir");

        let classification = classify_folder_content_blocking(root, identity_for(root));

        assert_eq!(classification.kind, FolderClassificationKind::Unknown);
        assert_eq!(classification.disc_marker, None);
        assert_eq!(classification.unit_count, 0);
    }

    #[tokio::test]
    async fn cached_classified_disc_folder_schedules_disc_probe_when_refocused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let fragile = root.join("FRAGILE");
        std::fs::create_dir_all(fragile.join("BDMV")).expect("bdmv fixture");

        let metadata = std::fs::metadata(root).expect("root metadata");
        let entry = BrowseEntry::new(
            root.to_path_buf(),
            "Yes - Fragile BD 2015".to_string(),
            EntryKind::Directory,
            metadata.len(),
            metadata.modified().ok(),
        );
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let classification = classify_folder_content_blocking(root, identity);
        assert_eq!(classification.kind, FolderClassificationKind::Disc);
        assert_eq!(classification.disc_probe_source_path(root), fragile.as_path());

        let mut state = BrowseState::new();
        state.entries = vec![entry.clone()];
        state.selected_index = 0;
        state.insert_folder_classification_for_identity(root.to_path_buf(), identity, classification);
        assert!(!state.disc_probe_pending.contains(&fragile));

        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        state.probe_current_with_db(&tx, None);

        assert_eq!(state.probe_debounce.as_ref().map(|d| d.path.as_path()), Some(root));
        assert!(
            !state.disc_probe_pending.contains(&fragile),
            "cached Disc classification must not launch the disc probe until focus settles"
        );
        assert!(state.has_valid_folder_classification_for_identity(root, identity));

        state.probe_debounce.as_mut().expect("debounce").deadline = Instant::now() - Duration::from_millis(1);
        state.check_probe_debounce(&tx);

        assert!(
            state.disc_probe_pending.contains(&fragile),
            "valid cached Disc classification must launch the existing disc probe after settled refocus"
        );
    }

    #[test]
    fn disc_marker_multidisc_siblings_are_one_logical_album() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        for disc_name in ["Disc 1", "Disc 2", "scans"] {
            std::fs::create_dir_all(root.join(disc_name)).expect("disc fixture");
        }
        std::fs::create_dir_all(root.join("Disc 1").join("BDMV")).expect("bdmv disc 1");
        std::fs::create_dir_all(root.join("Disc 2").join("BDMV")).expect("bdmv disc 2");

        let classification = classify_folder_content_blocking(root, identity_for(root));

        assert_eq!(classification.kind, FolderClassificationKind::MultiDisc);
        assert_eq!(classification.unit_count, 2);
        assert_eq!(classification.disc_marker, Some(FolderDiscMarkerKind::BluRay));
        assert!(classification.units.iter().all(|unit| {
            unit.disc_marker == Some(FolderDiscMarkerKind::BluRay)
        }));
    }

    #[test]
    fn selected_classified_disc_folder_matches_nested_disc_probe_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let fragile = root.join("FRAGILE");
        std::fs::create_dir_all(fragile.join("BDMV")).expect("bdmv fixture");

        let metadata = std::fs::metadata(root).expect("root metadata");
        let entry = BrowseEntry::new(
            root.to_path_buf(),
            "Yes - Fragile BD 2015".to_string(),
            EntryKind::Directory,
            metadata.len(),
            metadata.modified().ok(),
        );
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let classification = classify_folder_content_blocking(root, identity);
        let mut state = BrowseState::new();
        state.entries = vec![entry];
        state.selected_index = 0;
        state.insert_folder_classification_for_identity(root.to_path_buf(), identity, classification);

        assert!(state.current_selected_disc_source_matches(&fragile));
        assert!(!state.current_selected_disc_source_matches(root));
    }

    #[test]
    fn multidisc_requires_all_non_ignorable_siblings_to_be_disc_units() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        for name in ["CD1", "CD2", "Live Bonus"] {
            std::fs::create_dir_all(root.join(name)).expect("fixture dir");
        }
        touch(&root.join("CD1").join("01.flac"));
        touch(&root.join("CD2").join("01.flac"));

        let classification = classify_folder_content_blocking(root, identity_for(root));

        assert_eq!(classification.kind, FolderClassificationKind::Collection);
    }


    #[test]
    fn classification_entry_budget_caps_huge_flat_directory_scans() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        for idx in 0..50 {
            touch(&root.join(format!("file-{idx:02}.txt")));
        }

        let mut budget = FolderClassifyBudget::new(10);
        let scan = scan_folder_for_classification(root, &mut budget, false).expect("scan");

        assert!(budget.exhausted);
        assert_eq!(scan.audio.track_count, 0);
        assert!(scan.child_dirs.len() + scan.iso_units.len() < 50);
    }

    #[test]
    fn single_root_iso_classifies_as_disc() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        touch(&root.join("album.iso"));

        let classification = classify_folder_content_blocking(root, identity_for(root));

        assert_eq!(classification.kind, FolderClassificationKind::Disc);
        assert_eq!(classification.disc_marker, Some(FolderDiscMarkerKind::Iso));
        assert_eq!(classification.unit_count, 1);
    }

    #[test]
    fn root_disc_numbered_isos_classify_as_multidisc() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        touch(&root.join("Disc 1.iso"));
        touch(&root.join("Disc 2.iso"));
        std::fs::create_dir_all(root.join("scans")).expect("scans fixture");

        let classification = classify_folder_content_blocking(root, identity_for(root));

        assert_eq!(classification.kind, FolderClassificationKind::MultiDisc);
        assert_eq!(classification.unit_count, 2);
        assert!(classification.units.iter().all(|unit| {
            unit.disc_marker == Some(FolderDiscMarkerKind::Iso)
        }));
    }

    #[test]
    fn multiple_unrelated_root_isos_classify_as_collection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        touch(&root.join("Selling England by the Pound.iso"));
        touch(&root.join("The Lamb Lies Down on Broadway.iso"));

        let classification = classify_folder_content_blocking(root, identity_for(root));

        assert_eq!(classification.kind, FolderClassificationKind::Collection);
        assert_eq!(classification.unit_count, 2);
    }

    #[test]
    fn entry_budget_of_one_opens_directory_but_inspects_no_children() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        touch(&root.join("01.flac"));
        touch(&root.join("Disc 1.iso"));
        std::fs::create_dir_all(root.join("Disc 1")).expect("disc dir");

        let mut budget = FolderClassifyBudget::new(1);
        let scan = scan_folder_for_classification(root, &mut budget, false).expect("scan");

        assert!(budget.exhausted);
        assert_eq!(budget.remaining, 0);
        assert_eq!(scan.audio.track_count, 0);
        assert!(scan.iso_units.is_empty());
        assert!(scan.child_dirs.is_empty());
    }

    #[test]
    fn classification_budget_marks_unknown_or_many_when_walk_is_exhausted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        for idx in 0..6 {
            std::fs::create_dir_all(root.join(format!("maybe-album-{idx}"))).expect("fixture dir");
        }

        let classification = {
            let mut budget = FolderClassifyBudget::new(3);
            let Some(scan) = scan_folder_for_classification(root, &mut budget, false) else {
                return;
            };
            assert!(budget.exhausted);
            if scan.child_dirs.is_empty() {
                FolderContentClassification::unknown(identity_for(root), true)
            } else {
                FolderContentClassification::collection(identity_for(root), scan.child_dirs.len(), true, true)
            }
        };

        assert!(classification.io_budget_exhausted);
        assert!(matches!(classification.kind, FolderClassificationKind::Unknown | FolderClassificationKind::Collection));
    }

    #[test]
    fn nested_disc_numbered_isos_classify_as_multidisc() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let images = root.join("images");
        std::fs::create_dir_all(&images).expect("images dir");
        touch(&images.join("Disc 1.iso"));
        touch(&images.join("Disc 2.iso"));
        std::fs::create_dir_all(images.join("covers")).expect("ignorable sibling");

        let classification = classify_folder_content_blocking(root, identity_for(root));

        assert_eq!(classification.kind, FolderClassificationKind::MultiDisc);
        assert_eq!(classification.unit_count, 2);
        assert_eq!(classification.units[0].parent, images);
        assert!(classification.units.iter().all(|unit| {
            unit.disc_marker == Some(FolderDiscMarkerKind::Iso)
        }));
    }
}

#[cfg(test)]
mod disc_directory_navigation_tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

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
            .expect("write MovieObject.bdmv");
        std::fs::write(playlist.join("00000.mpls"), b"mpls").expect("write mpls");
        std::fs::write(stream.join("00001.m2ts"), b"m2ts").expect("write m2ts");
        (bdmv, index)
    }

    fn disc_entry(kind: EntryKind) -> BrowseEntry {
        BrowseEntry::new(
            PathBuf::from("/tmp/tonepoet-disc-dir"),
            "tonepoet-disc-dir".to_string(),
            kind,
            0,
            None,
        )
    }

    #[test]
    fn disc_directory_kinds_keep_directory_navigation_semantics() {
        for kind in [EntryKind::DvdAudioDir, EntryKind::DvdVideoDir, EntryKind::BlurayDir] {
            let entry = disc_entry(kind);
            assert!(entry.is_disc_source());
            assert!(entry.is_dir());
            assert!(entry.is_navigable_dir());
            assert!(entry.is_child_dir());
        }

        let parent = disc_entry(EntryKind::ParentDir);
        assert!(parent.is_dir());
        assert!(parent.is_navigable_dir());
        assert!(!parent.is_child_dir());
    }

    #[test]
    fn enter_selected_descends_into_disc_directory_kinds() {
        for kind in [EntryKind::DvdAudioDir, EntryKind::DvdVideoDir, EntryKind::BlurayDir] {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = temp.path();
            let disc_dir = root.join("disc-source");
            let child_dir = disc_dir.join("artwork");
            std::fs::create_dir_all(&child_dir).expect("disc directory fixture");
            let metadata = std::fs::metadata(&disc_dir).expect("disc metadata");

            let mut state = BrowseState::new();
            state.current_dir = root.to_path_buf();
            state.entries = vec![BrowseEntry::new(
                disc_dir.clone(),
                "disc-source".to_string(),
                kind,
                metadata.len(),
                metadata.modified().ok(),
            )];
            state.selected_index = 0;

            assert!(state.enter_selected());
            assert!(same_path(&state.current_dir, &disc_dir));
            assert!(
                state.entries.iter().any(|entry| entry.name == "artwork" && entry.is_child_dir()),
                "destination directory contents should be displayed after navigation"
            );
        }
    }

    #[test]
    fn directory_stats_cache_accepts_disc_directory_entries() {
        for kind in [EntryKind::DvdAudioDir, EntryKind::DvdVideoDir, EntryKind::BlurayDir] {
            let entry = disc_entry(kind);
            let identity = ProbeCacheIdentity::from_entry(&entry);
            let mut state = BrowseState::new();
            state.insert_dir_stats_for_identity(
                entry.path.clone(),
                identity,
                DirStats {
                    folder_count: 0,
                    file_count: 7,
                    audio_count: 2,
                    audio_size: 0,
                    total_size: 4096,
                },
            );

            let stats = state
                .valid_dir_stats_for_entry(&entry)
                .expect("disc directories should use the directory-stats cache");
            assert_eq!(stats.file_count, 7);
            assert_eq!(stats.audio_count, 2);
            assert_eq!(stats.total_size, 4096);
        }

        let parent = disc_entry(EntryKind::ParentDir);
        let mut state = BrowseState::new();
        state.insert_dir_stats_for_identity(
            parent.path.clone(),
            ProbeCacheIdentity::from_entry(&parent),
            DirStats {
                folder_count: 0,
                file_count: 1,
                audio_count: 0,
                audio_size: 0,
                total_size: 1,
            },
        );
        assert!(state.valid_dir_stats_for_entry(&parent).is_none());
    }

    #[test]
    fn queue_collection_preserves_disc_directory_roots_for_convert_routing() {
        for kind in [EntryKind::DvdAudioDir, EntryKind::DvdVideoDir, EntryKind::BlurayDir] {
            let entry = disc_entry(kind);
            let mut state = BrowseState::new();
            state.entries = vec![entry.clone()];
            state.selected_index = 0;

            let selection = state.collect_selection_for_queue();

            assert_eq!(selection.paths, vec![entry.path]);
            assert!(selection.cue_artifact_audio.is_empty());
        }
    }

    #[test]
    fn queue_collection_preserves_multi_selected_disc_directory_roots() {
        for kind in [EntryKind::DvdAudioDir, EntryKind::DvdVideoDir, EntryKind::BlurayDir] {
            let temp = tempfile::tempdir().expect("tempdir");
            let disc_dir = temp.path().join("disc-source");
            let nested_audio = disc_dir.join("VIDEO_TS").join("track.flac");
            std::fs::create_dir_all(nested_audio.parent().unwrap()).expect("disc audio fixture");
            std::fs::write(&nested_audio, b"not real audio, extension is enough for queue classification")
                .expect("disc nested audio fixture");

            let normal_dir = temp.path().join("ordinary-folder");
            let normal_audio = normal_dir.join("track.flac");
            std::fs::create_dir_all(&normal_dir).expect("ordinary folder fixture");
            std::fs::write(&normal_audio, b"not real audio, extension is enough for queue classification")
                .expect("ordinary audio fixture");

            let disc_entry = BrowseEntry::new(
                disc_dir.clone(),
                "disc-source".to_string(),
                kind,
                0,
                None,
            );
            let normal_entry = BrowseEntry::new(
                normal_dir.clone(),
                "ordinary-folder".to_string(),
                EntryKind::Directory,
                0,
                None,
            );

            let mut state = BrowseState::new();
            state.current_dir = temp.path().to_path_buf();
            state.entries = vec![disc_entry, normal_entry];
            state.multi_selected = vec![disc_dir.clone(), normal_dir.clone(), nested_audio.clone()];

            let selection = state.collect_selection_for_queue();

            assert!(path_list_contains(&selection.paths, &disc_dir));
            assert!(path_list_contains(&selection.paths, &normal_audio));
            assert!(
                !path_list_contains(&selection.paths, &nested_audio),
                "a selected disc root must own its nested contents so conversion does not flatten or double-queue it"
            );
            assert!(selection.cue_artifact_audio.is_empty());
        }
    }

    #[test]
    fn tag_only_search_keeps_disc_directories_navigable_by_filename() {
        let mut state = BrowseState::new();
        let disc = BrowseEntry::new(
            PathBuf::from("/tmp/Led Zeppelin DVD"),
            "Led Zeppelin DVD".to_string(),
            EntryKind::DvdVideoDir,
            0,
            None,
        );

        state.execute_search_over_entries(
            "Zeppelin",
            true,
            false,
            FormatFilter::Off,
            SearchMode::Tags,
            vec![disc.clone()],
        );

        assert!(
            state
                .entries
                .iter()
                .any(|entry| entry.path == disc.path && entry.is_navigable_dir()),
            "directories, including disc directories, must remain filename-searchable for navigation"
        );
    }

    fn focused_state_for_existing_entry(path: PathBuf, name: &str, kind: EntryKind) -> BrowseState {
        let metadata = std::fs::metadata(&path).expect("entry metadata");
        let mut state = BrowseState::new();
        state.entries = vec![BrowseEntry::new(
            path,
            name.to_string(),
            kind,
            metadata.len(),
            metadata.modified().ok(),
        )];
        state.selected_index = 0;
        state
    }

    #[tokio::test]
    async fn settled_focus_lazily_promotes_bluray_directory_and_schedules_disc_probe() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("movie");
        make_valid_bluray_layout(&root, "BDMV", "index.bdmv");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            root.clone(),
            "movie",
            EntryKind::Directory,
        );

        state.probe_current_with_db(&tx, None);
        assert!(matches!(state.entries[0].kind, EntryKind::Directory));
        assert!(!state.disc_probe_pending.contains(&root));

        state.probe_debounce.as_mut().expect("debounce").deadline =
            Instant::now() - Duration::from_millis(1);
        state.check_probe_debounce(&tx);

        assert!(matches!(state.entries[0].kind, EntryKind::BlurayDir));
        assert!(
            state.disc_probe_pending.contains(&root),
            "settled focus should lazily promote and schedule the native disc probe"
        );
        assert!(
            state
                .bluray_dir_classify_cache
                .get(&root)
                .map(|(_, verdict)| *verdict)
                .unwrap_or(false),
            "lazy promotion should populate the identity-bound native-disc cache"
        );
    }

    #[tokio::test]
    async fn raw_cursor_cached_native_disc_promotion_does_not_mutate_visible_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("cached-movie");
        make_valid_bluray_layout(&root, "BDMV", "index.bdmv");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            root.clone(),
            "cached-movie",
            EntryKind::Directory,
        );
        let fingerprint = bluray_directory_classification_fingerprint(&state.entries[0]);
        state
            .bluray_dir_classify_cache
            .insert(root.clone(), (fingerprint, true));

        state.probe_current_with_db(&tx, None);

        assert!(matches!(state.entries[0].kind, EntryKind::Directory));
        assert!(
            !state.deferred_work.visible_entries_changed && !state.deferred_work.info_pane_changed,
            "raw cursor movement may consult cached disc classification for policy decisions, but must not mutate visible Browse state"
        );
        assert!(!state.disc_probe_pending.contains(&root));

        state.probe_debounce.as_mut().expect("debounce").deadline =
            Instant::now() - Duration::from_millis(1);
        state.check_probe_debounce(&tx);

        assert!(matches!(state.entries[0].kind, EntryKind::BlurayDir));
        assert!(
            state.deferred_work.visible_entries_changed && state.deferred_work.info_pane_changed,
            "settled focus may publish cached promotion as a visible state change"
        );
    }

    #[tokio::test]
    async fn settled_focus_lazily_promotes_sacd_iso_after_scan_publishes_generic_archive() {
        let temp = tempfile::tempdir().expect("tempdir");
        let iso = temp.path().join("album.iso");
        let total = (crate::tui::sacd::MASTER_TOC_LSNS[0] + 1) * crate::tui::sacd::SECTOR_SIZE;
        let mut file = std::fs::File::create(&iso).expect("iso");
        file.set_len(total).expect("set len");
        file.seek(SeekFrom::Start(
            crate::tui::sacd::MASTER_TOC_LSNS[0] * crate::tui::sacd::SECTOR_SIZE,
        ))
        .expect("seek magic");
        file.write_all(crate::tui::sacd::MASTER_TOC_MAGIC)
            .expect("write magic");
        drop(file);

        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            iso.clone(),
            "album.iso",
            EntryKind::Archive,
        );

        state.probe_current_with_db(&tx, None);
        assert!(matches!(state.entries[0].kind, EntryKind::Archive));
        assert!(state.probe_debounce.is_some());
        assert!(!state.disc_probe_pending.contains(&iso));

        state.probe_debounce.as_mut().expect("debounce").deadline =
            Instant::now() - Duration::from_millis(1);
        state.check_probe_debounce(&tx);

        assert!(matches!(state.entries[0].kind, EntryKind::SacdIso));
        assert!(state.disc_probe_pending.contains(&iso));
    }

    #[tokio::test]
    async fn cached_only_policy_does_not_lazily_promote_uncached_bluray_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("movie");
        make_valid_bluray_layout(&root, "BDMV", "index.bdmv");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            root.clone(),
            "movie",
            EntryKind::Directory,
        );
        state.set_directory_summary_cold_work_policy(BrowseDirectorySummaryColdWorkPolicy::CachedOnly);

        state.probe_current_with_db(&tx, None);

        assert!(state.probe_debounce.is_none());
        assert!(matches!(state.entries[0].kind, EntryKind::Directory));
        assert!(!state.disc_probe_pending.contains(&root));
        assert!(state.bluray_dir_classify_cache.is_empty());
    }

    #[tokio::test]
    async fn focusing_regular_directory_debounces_classification_before_recursive_stats() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("Album Folder");
        std::fs::create_dir_all(&album).expect("album fixture");
        std::fs::write(album.join("01.flac"), b"not real audio").expect("track fixture");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            album.clone(),
            "Album Folder",
            EntryKind::Directory,
        );

        state.probe_current_with_db(&tx, None);

        assert_eq!(state.probe_debounce.as_ref().map(|d| d.path.as_path()), Some(album.as_path()));
        assert!(
            !state.dir_stats_pending.contains(&album),
            "recursive directory stats must not start on cursor movement"
        );
        assert!(
            !state.folder_classification_pending_for(&album),
            "folder classification must not start on cursor movement"
        );

        state.probe_debounce.as_mut().expect("debounce").deadline = Instant::now() - Duration::from_millis(1);
        state.check_probe_debounce(&tx);

        assert!(
            !state.dir_stats_pending.contains(&album),
            "recursive directory stats must not start until the shallow classifier says this is one logical album/disc"
        );
        assert!(
            state.folder_classification_pending_for(&album),
            "folder classification should start after settled focus"
        );
    }

    #[tokio::test]
    async fn recursive_stats_policy_can_disable_stats_without_disabling_classification() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("Stats Disabled Album");
        std::fs::create_dir_all(&album).expect("album fixture");
        std::fs::write(album.join("01.flac"), b"not real audio").expect("track fixture");
        let identity = std::fs::metadata(&album)
            .ok()
            .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
            .expect("album identity");
        let classification = classify_folder_content_blocking(&album, identity);
        assert_eq!(classification.kind, FolderClassificationKind::Album);

        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            album.clone(),
            "Stats Disabled Album",
            EntryKind::Directory,
        );
        state.insert_folder_classification_for_identity(album.clone(), identity, classification);
        state.set_directory_stats_cold_work_policy(BrowseDirectoryStatsColdWorkPolicy::CachedOnly);

        state.probe_current_with_db(&tx, None);

        assert!(
            state.valid_folder_classification_for_entry(&state.entries[0]).is_some(),
            "classification cache should remain usable"
        );
        assert!(
            state.probe_debounce.is_none(),
            "stats-only cold work should not arm the debounce when stats are disabled"
        );
        assert!(
            !state.dir_stats_pending.contains(&album),
            "recursive stats policy should suppress the stats walk independently"
        );
    }

    #[tokio::test]
    async fn cached_album_classification_allows_recursive_stats_after_debounce() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("Cached Album");
        std::fs::create_dir_all(&album).expect("album fixture");
        std::fs::write(album.join("01.flac"), b"not real audio").expect("track fixture");
        let identity = std::fs::metadata(&album)
            .ok()
            .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
            .expect("album identity");
        let classification = classify_folder_content_blocking(&album, identity);
        assert_eq!(classification.kind, FolderClassificationKind::Album);

        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            album.clone(),
            "Cached Album",
            EntryKind::Directory,
        );
        state.insert_folder_classification_for_identity(album.clone(), identity, classification);

        state.probe_current_with_db(&tx, None);

        assert_eq!(state.probe_debounce.as_ref().map(|d| d.path.as_path()), Some(album.as_path()));
        assert!(!state.dir_stats_pending.contains(&album));

        state.probe_debounce.as_mut().expect("debounce").deadline = Instant::now() - Duration::from_millis(1);
        state.check_probe_debounce(&tx);

        assert!(
            state.dir_stats_pending.contains(&album),
            "recursive stats should be allowed after a valid cached/shallow Album classification"
        );
    }

    #[tokio::test]
    async fn cached_collection_classification_allows_bounded_recursive_stats_after_debounce() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artist = temp.path().join("Artist");
        std::fs::create_dir_all(&artist).expect("artist fixture");
        for idx in 0..FOLDER_CLASSIFY_FAN_OUT_THRESHOLD {
            std::fs::create_dir_all(artist.join(format!("Album {idx:02}"))).expect("album dir");
        }
        let identity = std::fs::metadata(&artist)
            .ok()
            .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
            .expect("artist identity");
        let classification = classify_folder_content_blocking(&artist, identity);
        assert_eq!(classification.kind, FolderClassificationKind::Collection);

        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            artist.clone(),
            "Artist",
            EntryKind::Directory,
        );
        state.insert_folder_classification_for_identity(artist.clone(), identity, classification);

        state.probe_current_with_db(&tx, None);

        assert_eq!(
            state.probe_debounce.as_ref().map(|d| d.path.as_path()),
            Some(artist.as_path())
        );
        assert!(!state.dir_stats_pending.contains(&artist));

        state.probe_debounce.as_mut().expect("debounce").deadline = Instant::now() - Duration::from_millis(1);
        state.check_probe_debounce(&tx);

        assert!(
            state.dir_stats_pending.contains(&artist),
            "Collection folders use bounded recursive stats as the primary info-pane summary"
        );
    }

    #[test]
    fn directory_summary_cache_records_validation_scopes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("Scoped Album");
        std::fs::create_dir_all(&album).expect("album fixture");
        std::fs::write(album.join("01.flac"), b"not real audio").expect("track fixture");
        let identity = std::fs::metadata(&album)
            .ok()
            .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
            .expect("album identity");
        let classification = classify_folder_content_blocking(&album, identity);
        let entry = BrowseEntry::new(
            album.clone(),
            "Scoped Album".to_string(),
            EntryKind::Directory,
            std::fs::metadata(&album).expect("metadata").len(),
            std::fs::metadata(&album).expect("metadata").modified().ok(),
        );
        let mut state = BrowseState::new();
        state.insert_folder_classification_for_identity(album.clone(), identity, classification);

        let facts = state
            .valid_directory_summary_for_entry(&entry)
            .expect("classification summary facts");
        assert_eq!(facts.classification_scope, Some(DirectorySummaryScope::Immediate));
        assert!(facts.classification.is_some());
        assert!(facts.stats.is_none());

        state.insert_dir_stats_for_identity(
            album.clone(),
            identity,
            DirStats {
                folder_count: 0,
                file_count: 1,
                audio_count: 1,
                audio_size: 0,
                total_size: 128,
            },
        );

        let facts = state
            .valid_directory_summary_for_entry(&entry)
            .expect("combined summary facts");
        assert_eq!(facts.classification_scope, Some(DirectorySummaryScope::Immediate));
        assert_eq!(facts.stats_scope, Some(DirectorySummaryScope::RecursiveBestEffort));
        assert_eq!(facts.stats.as_ref().map(|stats| stats.audio_count), Some(1));
    }

    #[tokio::test]
    async fn cached_only_directory_summary_policy_blocks_uncached_hover_scans() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("Cold Album");
        std::fs::create_dir_all(&album).expect("album fixture");
        std::fs::write(album.join("01.flac"), b"not real audio").expect("track fixture");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            album.clone(),
            "Cold Album",
            EntryKind::Directory,
        );
        state.set_directory_summary_cold_work_policy(BrowseDirectorySummaryColdWorkPolicy::CachedOnly);

        state.probe_current_with_db(&tx, None);

        assert!(state.probe_debounce.is_none());
        assert!(!state.folder_classification_pending_for(&album));
        assert!(!state.dir_stats_pending.contains(&album));
        assert!(state.current_directory_summary().is_none());
    }

    #[tokio::test]
    async fn after_descend_only_policy_skips_child_row_hover_scans() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("Hover Album");
        std::fs::create_dir_all(&album).expect("album fixture");
        std::fs::write(album.join("01.flac"), b"not real audio").expect("track fixture");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            album.clone(),
            "Hover Album",
            EntryKind::Directory,
        );
        state.set_directory_summary_cold_work_policy(BrowseDirectorySummaryColdWorkPolicy::AfterDescendOnly);

        state.probe_current_with_db(&tx, None);

        assert!(state.probe_debounce.is_none());
        assert!(!state.folder_classification_pending_for(&album));
        assert!(!state.dir_stats_pending.contains(&album));
    }

    #[tokio::test]
    async fn cached_only_policy_does_not_launch_classified_folder_disc_probe() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("Cached Disc Folder");
        let fragile = root.join("FRAGILE");
        std::fs::create_dir_all(fragile.join("BDMV")).expect("bdmv fixture");
        let identity = std::fs::metadata(&root)
            .ok()
            .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
            .expect("disc folder identity");
        let classification = classify_folder_content_blocking(&root, identity);
        assert_eq!(classification.kind, FolderClassificationKind::Disc);
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            root.clone(),
            "Cached Disc Folder",
            EntryKind::Directory,
        );
        state.insert_folder_classification_for_identity(root.clone(), identity, classification);
        state.set_directory_summary_cold_work_policy(BrowseDirectorySummaryColdWorkPolicy::CachedOnly);

        state.probe_current_with_db(&tx, None);

        assert!(state.probe_debounce.is_none());
        assert!(!state.disc_probe_pending.contains(&fragile));
        assert!(state.current_directory_summary().is_some());
    }

    #[tokio::test]
    async fn cached_only_policy_does_not_launch_native_bluray_disc_probe() {
        let temp = tempfile::tempdir().expect("tempdir");
        let disc = temp.path().join("Cached Only Blu-ray");
        std::fs::create_dir_all(disc.join("BDMV")).expect("BDMV fixture");
        std::fs::write(disc.join("BDMV").join("index.bdmv"), b"index").expect("BDMV index fixture");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            disc.clone(),
            "Cached Only Blu-ray",
            EntryKind::BlurayDir,
        );
        state.set_directory_summary_cold_work_policy(BrowseDirectorySummaryColdWorkPolicy::CachedOnly);

        state.probe_current_with_db(&tx, None);

        assert!(
            state.probe_debounce.is_none(),
            "cached-only native disc hover must not arm a settled-focus worker"
        );
        assert!(
            !state.disc_probe_pending.contains(&disc),
            "cached-only native disc hover must not launch disc parsing"
        );
        assert!(
            !state.dir_stats_pending.contains(&disc),
            "cached-only native disc hover must not launch recursive stats"
        );
    }

    #[tokio::test]
    async fn cached_only_policy_does_not_launch_native_sacd_iso_disc_probe() {
        let temp = tempfile::tempdir().expect("tempdir");
        let iso = temp.path().join("cached-only-sacd.iso");
        std::fs::write(&iso, b"synthetic sacd fixture").expect("iso fixture");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            iso.clone(),
            "cached-only-sacd.iso",
            EntryKind::SacdIso,
        );
        state.set_directory_summary_cold_work_policy(BrowseDirectorySummaryColdWorkPolicy::CachedOnly);

        state.probe_current_with_db(&tx, None);

        assert!(
            state.probe_debounce.is_none(),
            "cached-only SACD hover must not arm a settled-focus worker"
        );
        assert!(
            !state.disc_probe_pending.contains(&iso),
            "cached-only SACD hover must not launch disc parsing"
        );
    }

    #[tokio::test]
    async fn cached_only_policy_still_renders_identity_valid_cached_summary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("Cached-Only Album");
        std::fs::create_dir_all(&album).expect("album fixture");
        std::fs::write(album.join("01.flac"), b"not real audio").expect("track fixture");
        let identity = std::fs::metadata(&album)
            .ok()
            .map(|metadata| ProbeCacheIdentity::from_metadata(&metadata))
            .expect("album identity");
        let classification = classify_folder_content_blocking(&album, identity);
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            album.clone(),
            "Cached-Only Album",
            EntryKind::Directory,
        );
        state.insert_folder_classification_for_identity(album.clone(), identity, classification);
        state.set_directory_summary_cold_work_policy(BrowseDirectorySummaryColdWorkPolicy::CachedOnly);

        state.probe_current_with_db(&tx, None);

        assert!(state.probe_debounce.is_none());
        assert!(!state.folder_classification_pending_for(&album));
        assert!(!state.dir_stats_pending.contains(&album));
        let facts = state.current_directory_summary().expect("cached summary facts");
        assert_eq!(facts.classification.as_ref().map(|c| c.kind), Some(FolderClassificationKind::Album));
    }

    #[tokio::test]
    async fn focusing_native_bluray_directory_debounces_disc_probe_and_dir_stats() {
        let temp = tempfile::tempdir().expect("tempdir");
        let disc = temp.path().join("Blu-ray Album");
        std::fs::create_dir_all(disc.join("BDMV")).expect("BDMV fixture");
        std::fs::write(disc.join("BDMV").join("index.bdmv"), b"index").expect("BDMV index fixture");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            disc.clone(),
            "Blu-ray Album",
            EntryKind::BlurayDir,
        );

        state.probe_current_with_db(&tx, None);

        assert_eq!(state.probe_debounce.as_ref().map(|d| d.path.as_path()), Some(disc.as_path()));
        assert!(
            !state.disc_probe_pending.contains(&disc),
            "native disc probing must wait for settled focus instead of launching on cursor movement"
        );
        assert!(
            !state.dir_stats_pending.contains(&disc),
            "recursive directory stats must wait for settled focus instead of launching on cursor movement"
        );

        state.probe_debounce.as_mut().expect("debounce").deadline = Instant::now() - Duration::from_millis(1);
        state.check_probe_debounce(&tx);

        assert!(
            state.disc_probe_pending.contains(&disc),
            "a settled native Blu-ray directory must launch the disc-summary probe"
        );
        assert!(
            state.dir_stats_pending.contains(&disc),
            "a settled native disc directory should also launch directory stats for size/count display"
        );
    }

    #[tokio::test]
    async fn focusing_native_sacd_iso_debounces_disc_probe() {
        let temp = tempfile::tempdir().expect("tempdir");
        let iso = temp.path().join("album.iso");
        std::fs::write(&iso, b"not a real SACD fixture; scheduling is independent of parse success")
            .expect("iso fixture");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            iso.clone(),
            "album.iso",
            EntryKind::SacdIso,
        );

        state.probe_current_with_db(&tx, None);

        assert_eq!(state.probe_debounce.as_ref().map(|d| d.path.as_path()), Some(iso.as_path()));
        assert!(
            !state.disc_probe_pending.contains(&iso),
            "a native SACD ISO must not launch the disc-summary probe on cursor movement"
        );

        state.probe_debounce.as_mut().expect("debounce").deadline = Instant::now() - Duration::from_millis(1);
        state.check_probe_debounce(&tx);

        assert!(
            state.disc_probe_pending.contains(&iso),
            "a settled native SACD ISO must launch the disc-summary probe"
        );
    }

    #[tokio::test]
    async fn native_disc_probe_scheduler_is_idempotent_for_pending_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let iso = temp.path().join("already-pending.iso");
        std::fs::write(&iso, b"pending fixture").expect("iso fixture");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = BrowseState::new();
        state.disc_probe_pending.insert(iso.clone());

        assert!(
            !state.schedule_disc_probe_for_source_path(&iso, &tx),
            "scheduling an already-pending disc source must be a no-op"
        );
        assert_eq!(state.disc_probe_pending.len(), 1);
    }

    #[tokio::test]
    async fn focusing_native_disc_source_with_current_disc_cache_does_not_relaunch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let disc = temp.path().join("Cached Blu-ray");
        std::fs::create_dir_all(disc.join("BDMV")).expect("BDMV fixture");
        std::fs::write(disc.join("BDMV").join("index.bdmv"), b"index").expect("BDMV index fixture");
        let fingerprint = crate::tui::disc_browser::disc_probe_fingerprint(&disc)
            .expect("disc fingerprint");
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut state = focused_state_for_existing_entry(
            disc.clone(),
            "Cached Blu-ray",
            EntryKind::BlurayDir,
        );
        state.disc_probe_cache.insert(
            disc.clone(),
            DiscProbeCacheEntry::from_error(fingerprint, "cached parse result".to_string()),
        );

        state.probe_current_with_db(&tx, None);

        assert_eq!(state.probe_debounce.as_ref().map(|d| d.path.as_path()), Some(disc.as_path()));
        assert!(
            !state.disc_probe_pending.contains(&disc),
            "a current disc cache entry must suppress duplicate native disc probe launch before debounce"
        );

        state.probe_debounce.as_mut().expect("debounce").deadline = Instant::now() - Duration::from_millis(1);
        state.check_probe_debounce(&tx);

        assert!(
            !state.disc_probe_pending.contains(&disc),
            "a current disc cache entry must suppress duplicate native disc probe launch after debounce"
        );
    }
}

#[cfg(test)]
mod selection_behavior_tests {
    use super::*;
    use std::path::PathBuf;

    fn test_entry(name: &str, kind: EntryKind) -> BrowseEntry {
        BrowseEntry::new(
            PathBuf::from(format!("/tmp/{name}")),
            name.to_string(),
            kind,
            0,
            None,
        )
    }

    fn selection_state() -> BrowseState {
        let mut state = BrowseState::new();
        // The fixture entries live under /tmp; marks are directory-scoped, so
        // the fixture must stand in the directory that contains them.
        state.current_dir = PathBuf::from("/tmp");
        state.entries = vec![
            test_entry("..", EntryKind::ParentDir),
            test_entry("a.flac", EntryKind::OtherFile),
            test_entry("b.flac", EntryKind::OtherFile),
            test_entry("c.flac", EntryKind::OtherFile),
        ];
        state.visible_height = state.entries.len();
        state.selected_index = 1;
        state
    }

    fn selected_names(state: &BrowseState) -> Vec<String> {
        state
            .multi_selected
            .iter()
            .filter_map(|path| path.file_name().map(|name| name.to_string_lossy().to_string()))
            .collect()
    }

    #[test]
    fn modal_range_preview_commit_merges_with_pre_range_selection_and_excludes_parent_dir() {
        let mut state = selection_state();
        let existing = state.entries[3].path.clone();
        state.multi_selected = vec![existing.clone()];

        state.begin_range_selection_at(0);
        state.selected_index = 2;
        state.update_range_preview();

        assert_eq!(selected_names(&state), vec!["a.flac", "b.flac"]);
        assert!(state.is_range_preview_index(1));
        assert!(!state.is_range_preview_index(0), "ParentDir must not preview as selectable");

        assert!(state.commit_range_selection());
        assert_eq!(state.selection_mode, SelectionMode::Normal);
        assert!(state.multi_selected.iter().any(|path| path == &existing));
        assert!(state.multi_selected.iter().any(|path| path.ends_with("a.flac")));
        assert!(state.multi_selected.iter().any(|path| path.ends_with("b.flac")));
        assert!(!state.multi_selected.iter().any(|path| path.ends_with("..")));
    }

    #[test]
    fn modal_range_cancel_restores_pre_range_selection() {
        let mut state = selection_state();
        let existing = state.entries[3].path.clone();
        state.multi_selected = vec![existing.clone()];

        state.begin_range_selection_at(1);
        state.selected_index = 2;
        state.update_range_preview();
        assert_eq!(selected_names(&state), vec!["a.flac", "b.flac"]);

        assert!(state.cancel_range_selection());
        assert_eq!(state.selection_mode, SelectionMode::Normal);
        assert_eq!(state.multi_selected, vec![existing]);
    }

    #[test]
    fn shift_style_range_continuation_keeps_original_anchor_until_commit() {
        let mut state = selection_state();

        state.begin_range_selection_at(1);
        state.selected_index = 2;
        state.update_range_preview();
        assert_eq!(selected_names(&state), vec!["a.flac", "b.flac"]);

        state.selected_index = 3;
        state.update_range_preview();
        assert_eq!(selected_names(&state), vec!["a.flac", "b.flac", "c.flac"]);

        assert!(state.commit_range_selection());
        assert_eq!(selected_names(&state), vec!["a.flac", "b.flac", "c.flac"]);
        assert_eq!(state.multi_select_anchor, Some(state.entries[3].path.clone()));
    }

    #[test]
    fn select_all_toggle_and_invert_operate_only_on_visible_non_parent_entries() {
        let mut state = selection_state();

        state.toggle_all_visible_selection();
        assert_eq!(selected_names(&state), vec!["a.flac", "b.flac", "c.flac"]);
        assert!(!state.multi_selected.iter().any(|path| path.ends_with("..")));

        state.toggle_all_visible_selection();
        assert!(state.multi_selected.is_empty());

        state.toggle_selection_at_index(1);
        assert_eq!(state.multi_select_anchor, Some(state.entries[1].path.clone()));
        state.toggle_selection_at_index(1);
        assert!(state.multi_selected.is_empty());
        assert!(state.multi_select_anchor.is_none());

        state.toggle_selection_at_index(1);
        state.invert_visible_selection();
        assert_eq!(selected_names(&state), vec!["b.flac", "c.flac"]);
        assert!(state.multi_select_anchor.is_none());
        assert!(!state.multi_selected.iter().any(|path| path.ends_with("..")));
    }

    #[test]
    fn root_directory_mark_scope_keeps_only_root_children() {
        let mut state = BrowseState::new();
        let root_child = PathBuf::from("/album");
        let nested_child = PathBuf::from("/tmp/album");
        state.current_dir = PathBuf::from("/");
        state.multi_selected = vec![root_child.clone(), nested_child];

        assert_eq!(state.multi_selected_in_current_directory(), vec![root_child]);
    }

    #[test]
    fn marks_on_visible_recursive_search_hits_stay_actionable() {
        // Recursive search lists entries from nested subdirectories; marking
        // one is a visible, deliberate act and must stay actionable even
        // though the hit's parent is not the current directory.
        let mut state = selection_state();
        let nested_hit = PathBuf::from("/tmp/nested/deep.flac");
        state.entries.push(BrowseEntry::new(
            nested_hit.clone(),
            "deep.flac".to_string(),
            EntryKind::OtherFile,
            0,
            None,
        ));
        let truly_stale = PathBuf::from("/somewhere-else/old.flac");
        state.multi_selected = vec![nested_hit.clone(), truly_stale];

        let scoped = state.scoped_multi_selected_in_current_directory();
        assert_eq!(scoped.paths, vec![nested_hit]);
        assert_eq!(scoped.dropped_stale_count, 1);
    }

    #[test]
    fn same_directory_marks_stay_actionable_when_not_currently_listed() {
        // A filter or search may hide a marked same-directory entry from the
        // visible list; the mark still belongs to this directory and must
        // stay actionable (pre-existing behavior, preserved).
        let mut state = selection_state();
        let hidden_same_dir = PathBuf::from("/tmp/filtered-out.flac");
        state.multi_selected = vec![hidden_same_dir.clone()];

        let scoped = state.scoped_multi_selected_in_current_directory();
        assert_eq!(scoped.paths, vec![hidden_same_dir]);
        assert_eq!(scoped.dropped_stale_count, 0);
    }

    #[test]
    fn navigation_reset_clears_marks_anchor_and_range_state() {
        let mut state = selection_state();
        state.toggle_selection_at_index(1);
        state.begin_range_selection_at(1);
        state.selected_index = 3;
        state.update_range_preview();
        state.drag_state.active = true;

        state.reset_nav_state();

        assert!(state.multi_selected.is_empty());
        assert!(state.multi_select_anchor.is_none());
        assert_eq!(state.selection_mode, SelectionMode::Normal);
        assert!(!state.drag_state.active);
    }

    #[test]
    fn same_directory_navigation_preserves_marks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let current = temp.path().join("current");
        std::fs::create_dir_all(&current).expect("current dir");
        let marked = current.join("album");
        std::fs::create_dir_all(&marked).expect("marked dir");

        let mut state = BrowseState::new();
        state.current_dir = current.clone();
        state.multi_selected = vec![marked.clone()];
        state.multi_select_anchor = Some(marked.clone());

        state.navigate_to(current.clone());

        assert_eq!(state.current_dir, current);
        assert_eq!(state.multi_selected, vec![marked.clone()]);
        assert_eq!(state.multi_select_anchor, Some(marked));
    }

    #[test]
    fn directory_round_trip_clears_marks_each_time() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir_a = temp.path().join("a");
        let dir_b = temp.path().join("b");
        std::fs::create_dir_all(&dir_a).expect("dir a");
        std::fs::create_dir_all(&dir_b).expect("dir b");
        let a_album = dir_a.join("album-a");
        let b_album = dir_b.join("album-b");
        std::fs::create_dir_all(&a_album).expect("album a");
        std::fs::create_dir_all(&b_album).expect("album b");

        let mut state = BrowseState::new();
        state.current_dir = dir_a.clone();
        state.multi_selected = vec![a_album];
        state.multi_select_anchor = state.multi_selected.first().cloned();

        state.navigate_to(dir_b.clone());
        assert_eq!(state.current_dir, dir_b);
        assert!(state.multi_selected.is_empty());
        assert!(state.multi_select_anchor.is_none());

        state.multi_selected = vec![b_album];
        state.multi_select_anchor = state.multi_selected.first().cloned();
        state.navigate_to(dir_a.clone());
        assert_eq!(state.current_dir, dir_a);
        assert!(state.multi_selected.is_empty());
        assert!(state.multi_select_anchor.is_none());
    }

    #[test]
    fn queue_collection_drops_cross_directory_marks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir_a = temp.path().join("a");
        let dir_b = temp.path().join("b");
        std::fs::create_dir_all(&dir_a).expect("dir a");
        std::fs::create_dir_all(&dir_b).expect("dir b");
        let a_one = dir_a.join("a-one.flac");
        let a_two = dir_a.join("a-two.flac");
        let b_one = dir_b.join("b-one.flac");
        let b_two = dir_b.join("b-two.flac");
        for path in [&a_one, &a_two, &b_one, &b_two] {
            std::fs::write(path, b"fixture").expect("audio fixture");
        }

        let mut state = BrowseState::new();
        state.current_dir = dir_b;
        state.multi_selected = vec![a_one, b_one.clone(), a_two, b_two.clone()];

        let mut collected = state.collect_selection_for_queue().paths;
        collected.sort();
        let mut expected = vec![b_one, b_two];
        expected.sort();
        assert_eq!(collected, expected);
    }
}
