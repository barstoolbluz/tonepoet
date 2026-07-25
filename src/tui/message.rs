//! Message types for async communication in the TUI event loop

/// Outcome envelope for the unified `:tags-mb` flow. The handler
/// switches on this to apply TOC-specific (cache by `toc_string`)
/// vs. search-specific (cache by `cache_writes`) persistence and to
/// pick the right zero-match status text; the downstream 0/1/N
/// branching is the same shape for both.
#[derive(Debug)]
pub enum MbOutcome {
    /// Result of a cascading `/ws/2/discid/-?toc=…` lookup (stub-drop
    /// cascade for synthesized TOCs; a single exact candidate for real CD
    /// rips). Cache writes carry their own TOC-string keys.
    Toc {
        outcome: Result<crate::tui::musicbrainz::MbCascadeOutcome, String>,
    },
    /// Result of a `/ws/2/release/?query=…` search, spawned by the
    /// handler as a zero-match fallback from `Toc`. Entering this
    /// variant means a TOC attempt already missed; the zero-match
    /// status reflects that breadcrumb. `query_label` is a short
    /// human rendering of the seed for the status line.
    Search {
        outcome: Result<crate::tui::musicbrainz::MbSearchOutcome, String>,
        query_label: String,
    },
}


/// Probe-completion acceptance context for Browse audio probes.
///
/// Filesystem probes carry the file identity captured when the worker was
/// launched. Archive-entry probes carry the archive mutation epoch captured at
/// launch, so completions from workers that started before a repack/rename/cache
/// invalidation cannot repopulate stale synthetic-path metadata afterward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioProbeContext {
    Filesystem {
        identity: Option<crate::tui::browse::ProbeCacheIdentity>,
    },
    ArchiveEntry {
        archive_path: std::path::PathBuf,
        archive_probe_epoch: u64,
    },
}

/// Per-dispatch context for the unified `:tags-mb` flow. Three entry
/// points build one of these:
///
/// 1. **Browse audio-file selection** (no editor open):
///    `editor_park = false`, `fallback_seed = None`.
/// 2. **SACD editor in-place**: `editor_park = true`,
///    `fallback_seed = Some(…)` because TOC misses are common for
///    SACD-only releases that lack a same-geometry CD reissue in MB.
/// 3. **Regular file editor in-place**: `editor_park = true`,
///    `fallback_seed = None` because audio-file TOCs are sample-exact
///    so the fallback rarely helps and adds an unhelpful second hop.
///
/// `fallback_seed` is captured at dispatch time (NOT re-read at
/// handler time) so a search reflects what the user saw when they
/// triggered `:tags-mb`, even if they edit values during the wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataEditorSessionGuard {
    pub session_id: u64,
    pub save_generation: u64,
    pub editor_generation: u64,
}

/// Opaque identity for one complete MusicBrainz tagging workflow.
///
/// The ID is allocated before split-CUE discovery or grouping begins and is
/// preserved through TOC fallback, text search, picker selection, verification,
/// and application. Every asynchronous completion must prove that it still owns
/// the active ID before it may change status, overlays, latches, or editor data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TagsMbOperationId(pub u64);

impl TagsMbOperationId {
    /// Sentinel used only while a caller is assembling a pre-lookup or direct
    /// lookup request. Spawn helpers replace it with an allocated identity before
    /// any worker is launched.
    pub const UNASSIGNED: Self = Self(0);

    pub fn is_assigned(self) -> bool {
        self != Self::UNASSIGNED
    }
}

#[derive(Debug, Clone)]
pub struct TagsMbContext {
    /// Authority for the complete lookup-to-apply lifecycle.
    pub operation_id: TagsMbOperationId,
    pub paths: Vec<std::path::PathBuf>,
    /// `true` = the dispatch left a metadata editor in
    /// `active_overlay` that should be populated in place; the
    /// handler manages parking for the multi-match case.
    /// `false` = no editor in scope; the handler opens a fresh
    /// editor on single-match / pick.
    pub editor_park: bool,
    pub fallback_seed: Option<crate::tui::command::SacdMbSeed>,
    /// Identity of the metadata-editor surface that initiated an in-editor
    /// lookup.  Async completions must match this before parking, populating,
    /// or otherwise mutating an editor; path equality alone is insufficient
    /// because users may close and reopen the same album while work is in
    /// flight.
    pub editor_session: Option<MetadataEditorSessionGuard>,
}

/// Messages sent to the TUI event loop via mpsc channel
#[derive(Debug)]
pub enum AppMessage {
    /// A conversion item or one of its concurrent tracks reported progress.
    /// When `track_index` is `Some(idx)`, the update describes one track
    /// inside a multi-track source. The TUI routes these to per-track
    /// sub-lines below the parent item row. When `None`, the update
    /// applies to the item itself.
    ConversionProgress {
        item_id: String,
        track_index: Option<u32>,
        track_epoch: Option<u64>,
        progress: f32,
        status: crate::convert::ConversionStatus,
    },
    /// A track-scoped worker ended without relying on broadcast delivery.
    ClearTrackProgress {
        item_id: String,
        track_index: u32,
        track_epoch: u64,
    },
    /// All conversions completed
    ConversionComplete { completed: usize, failed: usize },
    /// A conversion error occurred
    ConversionError { message: String },
    /// Files were scanned and should be added to the queue
    FilesScanned { paths: Vec<std::path::PathBuf> },
    /// Status message to show in the status bar
    StatusMessage(String),
    /// Progress from background explicit-action preview preparation.
    ActionsRunPreparationProgress {
        preparation_id: String,
        detail: String,
    },
    /// Completion of background explicit-action preview preparation.
    ActionsRunPrepared {
        preparation_id: String,
        result: Result<crate::tui::conversion_actions_ui::ActionsRunState, String>,
    },
    /// Completion of an explicit `:actions-run` durable apply.
    ActionsRunComplete {
        invocation_id: String,
        result: Result<crate::convert::pipeline::ActionPhaseReport, String>,
    },
    /// Force a redraw
    Redraw,
    /// Bookmark target reachability loaded off the event thread.
    BookmarkTargetsLoaded {
        generation: u64,
        statuses: Vec<(
            std::path::PathBuf,
            crate::tui::bookmarks::BookmarkTargetStatus,
        )>,
    },
    /// Bookmark activation revalidated on the dedicated filesystem worker pool.
    BookmarkActivationResolved {
        generation: u64,
        request_id: u64,
        path: std::path::PathBuf,
        result: Result<(), String>,
    },
    /// A dedicated bookmark detail worker has begun the filesystem scan.
    BookmarkDetailStarted {
        generation: u64,
        path: std::path::PathBuf,
    },
    /// Lazy, non-recursive bookmark detail loaded off the event thread.
    BookmarkDetailLoaded {
        generation: u64,
        path: std::path::PathBuf,
        result: Result<crate::tui::bookmarks::BookmarkDetail, String>,
    },
    /// Result of asynchronous Browse regular-folder expansion for Convert/Queue
    /// handoff. The reducer accepts this only when the generation and captured
    /// selection still match the active pending expansion.
    BrowseConvertExpansionComplete {
        generation: u64,
        request: crate::tui::command::BrowseConvertExpansionRequest,
        expansion: crate::tui::command::BrowseConvertExpansion,
    },
    /// Result of an asynchronous Convert-source probe launched from command
    /// handlers or picker returns. `generation` is captured at dispatch time;
    /// the event-loop reducer drops stale completions when the source has
    /// changed since launch. `source_mode` is the fully discovered mode built
    /// on a blocking worker; `baseline` protects in-flight user edits.
    ProbeResult {
        generation: u64,
        path: std::path::PathBuf,
        source_mode: crate::tui::app::SourceMode,
        baseline: crate::tui::app::ConvertProbeBaseline,
    },
    /// Progress from queue-time archive extraction/probing on the Convert screen.
    ArchivePreviewProgress {
        generation: u64,
        archive_path: std::path::PathBuf,
        message: String,
    },
    /// Completed queue-time archive preview. Stale completions are discarded by
    /// generation and current source path; successful stale previews clean their
    /// staging directory before returning.
    ArchivePreviewResult {
        generation: u64,
        archive_path: std::path::PathBuf,
        result: Result<crate::tui::app::ArchivePreview, String>,
        baseline: crate::tui::app::ConvertProbeBaseline,
    },
    /// Milestone from Browse-screen archive metadata extraction/tag-read.
    /// The event loop displays it only while the matching pending edit is
    /// still current and the user remains on Browse.
    ArchiveMetadataEditorProgress {
        archive_path: std::path::PathBuf,
        staging_dir: std::path::PathBuf,
        message: String,
    },
    /// Completed Browse-screen archive metadata extraction/tag-read. The event
    /// loop opens the editor only when this still matches the pending app-state
    /// handle; stale completions clean their staging directory.
    ArchiveMetadataEditorPrepared {
        archive_path: std::path::PathBuf,
        staging_dir: std::path::PathBuf,
        result: Result<crate::tui::app::ArchiveMetadataEditorPayload, String>,
    },
    /// Typed progress snapshot from Browse-screen archive repackage after staged
    /// metadata writes. Routed into the active FileTaskProgress overlay only
    /// while it matches the active repackage context.
    ArchiveRepackageProgress {
        archive_path: std::path::PathBuf,
        staging_dir: std::path::PathBuf,
        progress_session_id: u64,
        snapshot: crate::convert::pipeline::materializer_archive::ArchiveRepackageProgressSnapshot,
    },
    /// Completed Browse-screen archive repackage after staged metadata writes.
    /// Staging is cleaned up only after a successful archive replacement;
    /// failure leaves the staged edits durable for retry/discard.
    ArchiveRepackageResult {
        archive_path: std::path::PathBuf,
        staging_dir: std::path::PathBuf,
        progress_session_id: u64,
        result: Result<crate::convert::pipeline::materializer_archive::ArchiveRepackageReport, String>,
    },

    /// Milestone from Browse-screen archive-entry rename. Displayed only while
    /// it matches the current pending rename handle.
    ArchiveEntryRenameProgress {
        archive_path: std::path::PathBuf,
        staging_dir: std::path::PathBuf,
        message: String,
    },
    /// Completed Browse-screen archive-entry rename. Staging is cleaned up by
    /// the event-loop reducer in every outcome.
    ArchiveEntryRenameResult {
        archive_path: std::path::PathBuf,
        staging_dir: std::path::PathBuf,
        old_inner_path: String,
        new_inner_path: String,
        result: Result<(), String>,
    },
    /// Milestone from Browse-screen archive-entry delete. Displayed only while
    /// it matches the current pending delete handle.
    ArchiveEntryDeleteProgress {
        archive_path: std::path::PathBuf,
        staging_dir: std::path::PathBuf,
        message: String,
    },
    /// Completed Browse-screen archive-entry delete. On success, the staging
    /// directory becomes the active deferred-save session; it is not
    /// repackaged until the user leaves the archive/screen/quits.
    ArchiveEntryDeleteResult {
        archive_path: std::path::PathBuf,
        staging_dir: std::path::PathBuf,
        inner_paths: Vec<String>,
        result: Result<(), String>,
    },
    /// Chunk of valid SQLite probe-cache rows warmed after a Browse directory
    /// scan. The reducer merges rows only while the same generation/path is
    /// still current. Large bursts are queued and merged in bounded frame-sized
    /// slices, so a slow warm worker cannot repopulate a later listing or make
    /// one reducer frame absorb thousands of cache inserts.
    ProbeCacheWarmComplete {
        generation: u64,
        path: std::path::PathBuf,
        rows: Vec<crate::tui::browse::ProbeCacheWarmRow>,
    },
    /// Result of an asynchronous audio probe (lofty + ffmpeg) launched by
    /// `BrowseState::probe_current`. The main loop updates `probe_cache` and
    /// removes the path from `probe_pending`. Reducers must not perform
    /// follow-up media/tag reads here; worker-side probe code must enrich
    /// optional metadata before sending this message.
    AudioProbeComplete {
        path: std::path::PathBuf,
        /// Acceptance guard captured when the worker was launched.
        context: AudioProbeContext,
        /// Owned cached-info on success; error string on failure.
        result: Box<Result<crate::tui::browse::CachedInfo, String>>,
    },
    /// Convert-owned batch/cursor probe completion. Unlike generic browse
    /// probes, this carries the Convert source generation plus editable-state
    /// baseline captured when the worker was launched, so late completions can
    /// update source facts without overwriting user format or metadata edits.
    ConvertAudioProbeComplete {
        generation: u64,
        path: std::path::PathBuf,
        info: Option<crate::tui::probe::SourceInfo>,
        metadata: crate::tui::probe::SourceMetadata,
        probe_notice: Option<String>,
        baseline: crate::tui::app::ConvertProbeBaseline,
    },
    /// Result of an asynchronous optical-disc probe launched by the Browse
    /// info pane or the Audio Streams action. The payload uses the unified
    /// DiscContents model shared by DVD-Audio and SACD, plus an optional
    /// fingerprint captured before parsing for stale-result rejection.
    DiscProbeComplete {
        path: std::path::PathBuf,
        fingerprint: Option<crate::tui::disc_browser::DiscProbeFingerprint>,
        result: Box<Result<crate::disc::DiscContents, String>>,
    },
    /// Result of an asynchronous directory-stats computation launched by
    /// `BrowseState::probe_current` for a directory entry. The completion
    /// carries the directory identity captured at dispatch so stale walkers
    /// cannot publish stats after the selected directory has changed or
    /// disappeared.
    DirStatsComplete {
        path: std::path::PathBuf,
        identity: crate::tui::browse::ProbeCacheIdentity,
        stats: crate::tui::browse::DirStats,
        cancelled: bool,
    },
    /// Result of a bounded folder-content classification launched after the
    /// Browse cursor debounce. The worker performs only directory reads and
    /// extension checks; reducers still validate the captured directory identity
    /// and current selection before publishing the cached classification.
    FolderClassifyComplete {
        path: std::path::PathBuf,
        identity: crate::tui::browse::ProbeCacheIdentity,
        classification: crate::tui::browse::FolderContentClassification,
    },
    /// Result of an async audio analysis (DR, peak, RMS, etc.).
    /// `result` is Ok on success, Err(message) on failure.
    AnalysisComplete {
        operation_id: TagsMbOperationId,
        result: Result<Box<crate::tui::analyze::AnalysisResult>, String>,
    },
    /// Result of an async file integrity verification.
    VerifyComplete {
        operation_id: TagsMbOperationId,
        result: crate::tui::verify::VerifyResult,
    },
    /// Result of an async pre-emphasis detection scan.
    PreemphasisComplete {
        operation_id: TagsMbOperationId,
        result: crate::tui::preemphasis::PreemphasisResult,
    },
    /// Result of corpus training completion.
    CorpusTrainComplete {
        result: Result<(u64, u64), String>, // (n_tracks, n_frames)
    },
    /// Result of LDA classifier calibration.
    CalibrationComplete {
        result: Result<(usize, usize, f64, f64, f64), String>, // (n_pe, n_non_pe, accuracy, fpr, threshold)
    },
    /// Result of an async bit-level comparison between two audio files.
    CompareComplete {
        operation_id: TagsMbOperationId,
        result: crate::tui::bit_compare::CompareResult,
    },
    /// Result of async path validation (canonicalize + is_dir) for :cd/path bar.
    /// Carries the launch generation and origin directory so late completions
    /// from superseded path navigations cannot move Browse backward.
    PathValidationComplete {
        generation: u64,
        origin_dir: std::path::PathBuf,
        input: String,
        result: Result<std::path::PathBuf, String>,
    },
    /// Result of an async directory scan (readdir + lstat per entry).
    DirScanComplete {
        generation: u64,
        path: std::path::PathBuf,
        parent_entry: Option<crate::tui::browse::BrowseEntry>,
        dirs: Vec<crate::tui::browse::BrowseEntry>,
        files: Vec<crate::tui::browse::BrowseEntry>,
        classification_updates: crate::tui::browse::BrowseClassificationCacheUpdates,
        error: Option<String>,
    },
    /// Result of an async metadata tag write.
    MetadataWriteProgress {
        operation_id: u64,
        path: std::path::PathBuf,
        detail: String,
    },
    MetadataWriteComplete {
        operation_id: u64,
        path: std::path::PathBuf,
        field: crate::tui::probe::MetadataField,
        value: String,
        result: Result<crate::tui::probe::MetadataWriteCommitReport, String>,
    },
    /// Results of an async recursive search. Carries the launch identity so
    /// the reducer can reject stale completions after query, root, mode,
    /// visibility, audio/format filter, sort, or cap changes.
    SearchComplete {
        generation: u64,
        root: std::path::PathBuf,
        /// Whether the launch was recursive. Archive-local tag search can also
        /// be async while non-recursive, so the reducer must validate this
        /// explicitly instead of assuming every SearchComplete is recursive.
        recursive: bool,
        /// Archive identity captured at launch. `None` means ordinary
        /// filesystem search; `Some` means archive-local search for the given
        /// archive and inner directory.
        archive_path: Option<std::path::PathBuf>,
        archive_inner_path: Option<String>,
        query: String,
        mode: crate::tui::browse::SearchMode,
        show_hidden: bool,
        audio_only: bool,
        format_filter: crate::tui::browse::FormatFilter,
        sort: crate::tui::browse::SearchSort,
        sort_dir: crate::tui::browse::SortDir,
        result_cap: usize,
        total_matches: usize,
        /// True when the worker already sorted using context that the reducer
        /// cannot safely reconstruct without blocking the TUI, e.g. archive
        /// tag-sort keys resolved from staged/extracted members.
        pre_sorted: bool,
        /// Archive-entry tag-cache writes produced by an async archive search.
        /// The reducer applies them only after the launch identity validates.
        archive_tag_cache_updates: Vec<(
            std::path::PathBuf,
            crate::tui::browse::TagCacheFingerprint,
            crate::tui::browse::ArchiveTagPasswordIdentity,
            crate::tui::browse::TagReadResult,
        )>,
        results: Vec<(crate::tui::browse::BrowseEntry, i64)>,
    },
    /// Result of a batch metadata write from the metadata editor.
    ///
    /// Save completions carry the editor Details/session id plus the
    /// monotonic save generation captured at dispatch time. The event-loop
    /// reducer must reject stale sessions/generations before applying results
    /// or closing an overlay.
    MetadataEditorWriteComplete {
        session_id: u64,
        save_generation: u64,
        results: Vec<crate::tui::app::MetadataEditorWriteResult>,
        /// Worker-side tag re-read performed only after a fully successful
        /// embedded-CUESHEET deletion save. This prevents the reducer from
        /// marking stale CUE-derived rows clean.
        refreshed_entries: Option<Result<Vec<crate::tui::probe::TagEntry>, String>>,
    },
    /// Byte-level progress from a session-guarded metadata write.
    MetadataEditorWriteProgress {
        session_id: u64,
        save_generation: u64,
        detail: String,
    },
    /// Result of a background Details-tab media probe from the metadata editor.
    /// Reduced into the active editor model by the event loop; rendering never
    /// polls worker-owned state. Stale generations are ignored by the reducer.
    MetadataEditorDetailsProbeComplete {
        /// Unique Details cache/session id captured when the probe request was
        /// launched. Prevents stale completions from closed/reopened editors
        /// with the same generation from mutating the wrong model.
        session_id: u64,
        generation: u64,
        total: usize,
        results: Vec<crate::tui::app::MetadataDetailsProbeFileResult>,
    },
    /// Result of a narrow Details-tab HDCD + metadata/CUE/catalog PRE scan.
    MetadataEditorDetailsAnalysisComplete {
        session_id: u64,
        generation: u64,
        total: usize,
        results: Vec<crate::tui::app::MetadataDetailsAnalysisFileResult>,
    },
    /// Result of a ReplayGain scan launched from the metadata editor.
    MetadataEditorReplayGainComplete {
        session_id: u64,
        generation: u64,
        mode: crate::tui::app::MetadataReplayGainScanMode,
        paths: Vec<std::path::PathBuf>,
        result: Result<Vec<crate::tui::probe::SourceMetadata>, String>,
    },
    /// Generic reusable file-picker completion. All picker owners send this
    /// message so purpose dispatch is centralized in the event loop instead of
    /// being coupled to a specific overlay such as the metadata editor.
    FilePickerComplete {
        session_id: u64,
        purpose: crate::tui::app::FilePickerPurpose,
        path: Option<std::path::PathBuf>,
    },
    /// Progress snapshot for a hosted long-running file task. The reusable
    /// progress state lives in `tui-file-picker`; Tonepoet only matches the
    /// session id and feeds the snapshot into the active overlay.
    FileTaskProgress {
        session_id: u64,
        update: tui_file_picker::FileTaskProgressUpdate,
    },
    /// Structured top-level completion accounting for a hosted file task.
    /// This is separate from progress presentation so callers can repair
    /// clipboards and navigation after partial moves without scraping status text.
    FileTaskComplete {
        session_id: u64,
        report: tui_file_picker::FileTaskCompletionReport,
        retry_plan: Option<crate::tui::browse::BrowsePasteRetryPlan>,
    },
    /// Result of add/replace/remove artwork launched from the Artwork tab.
    MetadataEditorArtworkWriteComplete {
        session_id: u64,
        generation: u64,
        mode: crate::tui::app::MetadataArtworkWriteMode,
        paths: Vec<std::path::PathBuf>,
        result: Result<crate::tui::probe::ArtworkWriteBatchResult, String>,
    },
    /// Progress for an async archive listing (`7zz l -slt`).
    ArchiveListingProgress {
        id: u64,
        archive_path: std::path::PathBuf,
        message: String,
    },
    /// Result of an async archive listing (`7zz l -slt`).
    ArchiveListingComplete {
        id: u64,
        archive_path: std::path::PathBuf,
        cache_key: Option<crate::tui::archive_listing::ArchiveListingCacheKey>,
        result: Box<Result<crate::tui::archive_listing::ArchiveListing, String>>,
        password: Option<String>,
    },
    /// Result of an async GNUDB query.
    GnudbQueryComplete {
        operation_id: TagsMbOperationId,
        result: Result<Vec<crate::tui::gnudb::GnudbMatch>, String>,
        paths: Vec<std::path::PathBuf>,
    },
    /// Result of an async GNUDB read (single entry).
    GnudbReadComplete {
        operation_id: TagsMbOperationId,
        result: Result<crate::tui::gnudb::GnudbEntry, String>,
        paths: Vec<std::path::PathBuf>,
        /// Original match list for "back" navigation (None for single/auto-read).
        origin_matches: Option<Vec<crate::tui::gnudb::GnudbMatch>>,
    },
    /// Result of a multi-disc GNUDB query (sequential queries per disc).
    GnudbMultiDiscComplete {
        operation_id: TagsMbOperationId,
        /// Per-disc results: (disc_label, entry, file_paths).
        entries: Vec<(
            String,
            crate::tui::gnudb::GnudbEntry,
            Vec<std::path::PathBuf>,
        )>,
        /// Query/read failures with the disc label and failed stage. Empty
        /// match sets are not failures and therefore do not appear here.
        failures: Vec<String>,
        /// Number of disc/CUE-part queries attempted.
        attempted: usize,
    },
    /// Panic/cancellation containment result for any GNUDB worker. The reducer
    /// may retire only the matching operation and restore only its owned editor.
    GnudbWorkerFailed {
        operation_id: TagsMbOperationId,
        detail: String,
    },
    /// Result of an async AccurateRip verification (one or more discs).
    AccurateRipComplete {
        operation_id: TagsMbOperationId,
        pages: Vec<crate::tui::app::ArVerifyPage>,
    },
    /// Result of an async CTDB verification (one or more discs).
    CtdbComplete {
        operation_id: TagsMbOperationId,
        pages: Vec<crate::tui::app::CtdbVerifyPage>,
    },
    /// Result of an async AR batch verification.
    ArBatchComplete {
        operation_id: TagsMbOperationId,
        result: Box<crate::tui::accuraterip::ArBatchResult>,
    },
    /// Result of an async AR offset correction.
    OffsetCorrectionComplete {
        operation_id: TagsMbOperationId,
        result: Result<String, String>,
    },
    /// Result of an async CTDB Reed-Solomon repair.
    CtdbRepairComplete {
        operation_id: TagsMbOperationId,
        result: Result<String, String>,
    },
    /// Result of an async MusicBrainz disc-TOC lookup driving `:cue-mb`.
    /// `outcome` is `Err` when transport/parse failed; `Ok(None)` means
    /// no release matched. `paths`, `output_dir`, `single_image` carry
    /// the original command context to the main thread for CUE writing.
    /// `toc_string` is provided so the handler can write `cache_response`
    /// back into the SQLite cache.
    CueMbComplete {
        operation_id: TagsMbOperationId,
        outcome: Result<crate::tui::musicbrainz::MbLookupOutcome, String>,
        paths: Vec<std::path::PathBuf>,
        output_dir: std::path::PathBuf,
        single_image: bool,
        toc_string: String,
    },
    /// Result of async CUE writing work. The event loop owns the status bar
    /// and Browse refresh, while heavyweight probing/writing runs off-thread.
    CueWriteComplete {
        result: Result<String, String>,
        refresh_browse: bool,
    },
    /// Result of async CUE preview construction. Used by MB-enriched CUE
    /// generation so probe/tag reads do not block message handling.
    CuePreviewComplete {
        operation_id: TagsMbOperationId,
        result: Result<(String, std::path::PathBuf, String), String>,
    },
    /// Result of async `:cue-fill` preparation. Carries probe-derived album,
    /// track, layout, and TOC-sector data back to the event loop so DB cache
    /// lookup still happens on the main thread before the MB request is spawned.
    CueFillPrepComplete {
        operation_id: TagsMbOperationId,
        cue_path: std::path::PathBuf,
        result: Result<
            (
                Box<crate::tui::cue_generate::CueAlbumInfo>,
                Vec<crate::tui::cue_generate::CueTrackInfo>,
                CueFillLayout,
                Vec<u32>,
            ),
            String,
        >,
    },
    /// Result of an async MusicBrainz lookup driving `:cue-fill`. Carries
    /// the path of the original `.cue` and the pre-built album/tracks
    /// (with parsed pregaps and durations applied) ready for fill+write.
    CueFillComplete {
        operation_id: TagsMbOperationId,
        outcome: Result<crate::tui::musicbrainz::MbLookupOutcome, String>,
        cue_path: std::path::PathBuf,
        album: Box<crate::tui::cue_generate::CueAlbumInfo>,
        tracks: Vec<crate::tui::cue_generate::CueTrackInfo>,
        layout: CueFillLayout,
        toc_string: String,
    },
    /// Result of an async MusicBrainz lookup driving `:tags-mb`.
    /// Unified across three entry points (Browse audio-file selection,
    /// SACD editor in-place, regular file editor in-place) via the
    /// `MbOutcome` envelope + `TagsMbContext`. The context carries the
    /// workflow identity allocated before dispatch; the reducer must reject a
    /// non-current identity before changing cache, status, overlays, latches,
    /// or editor state. Populate-vs-open-fresh and fallback eligibility also
    /// ride on `ctx`.
    TagsFromMbComplete {
        outcome: MbOutcome,
        ctx: TagsMbContext,
    },
    /// Completion of the same-folder split-CUE album grouping ladder. The boxed
    /// request carries the workflow operation ID; the reducer must reject stale
    /// or duplicate completions before cache, status, overlay, latch, or editor
    /// mutation, then continue under that unchanged ID.
    SplitCueAlbumGroupingComplete {
        request: Box<crate::tui::command::SplitCueAlbumGroupingRequest>,
        result: Result<Box<crate::tui::command::SplitCueAlbumGroupingAsyncOutcome>, String>,
    },
    /// Completion of the same grouping ladder for GNUDB dispatch. GNUDB uses
    /// the resolved album grouping to decide which same-folder CUE surfaces
    /// belong to the active tagging operation before issuing per-CUE GNUDB
    /// lookups.
    GnudbSplitCueAlbumGroupingComplete {
        operation_id: TagsMbOperationId,
        infos: Vec<crate::tui::cue_parser::SingleImageInfo>,
        active_audio_path: Option<std::path::PathBuf>,
        result: Result<Box<crate::tui::command::SplitCueAlbumGroupingAsyncOutcome>, String>,
    },
    /// Completion of the same grouping ladder before opening a split-CUE
    /// metadata editor. This keeps metadata-editor grouping on the same
    /// title/concat-TOC/per-CUE/ambiguous-merge policy used by MB dispatch.
    MetadataEditorSplitCueAlbumGroupingComplete {
        operation_id: TagsMbOperationId,
        infos: Vec<crate::tui::cue_parser::SingleImageInfo>,
        active_cue_path: Option<std::path::PathBuf>,
        ordinary_paths: Vec<std::path::PathBuf>,
        metadata_sidecar_cue_paths: Vec<std::path::PathBuf>,
        cue_admission_warnings: Vec<String>,
        cue_policy: crate::convert::pipeline::CueSidecarPolicy,
        result: Result<Box<crate::tui::command::SplitCueAlbumGroupingAsyncOutcome>, String>,
    },
    /// Completion of in-editor split-CUE discovery for `:tags-mb`. The boxed
    /// request carries the operation ID allocated before worker launch; stale or
    /// duplicate completions are total no-ops before any UI or cache mutation.
    InEditorSplitCueMusicBrainzInfoComplete {
        request: Box<crate::tui::command::InEditorSplitCueMusicBrainzInfoRequest>,
        result: Result<Vec<crate::tui::cue_parser::SingleImageInfo>, String>,
    },
    /// Result of the blocking single-image MusicBrainz guard checks used
    /// before applying a selected release to the metadata editor. The guard
    /// may read tags and probe sample counts, so it must complete on a
    /// blocking worker before the event-loop reducer mutates UI state.
    TagsMbApplyReady {
        operation_id: TagsMbOperationId,
        releases: Vec<crate::tui::musicbrainz::MbRelease>,
        selected: usize,
        paths: Vec<std::path::PathBuf>,
        editor_session: Option<MetadataEditorSessionGuard>,
        decision: crate::tui::musicbrainz::PerTrackDecision,
    },
    /// Result of an MbSelect prefetch: the detail fetch
    /// (`/ws/2/release/{mbid}?inc=…`) for a candidate currently visible
    /// in the picker. Generation-based cancellation happens *before*
    /// HTTP fire in `spawn_mb_detail_prefetch`; by the time a response
    /// reaches this handler we've already paid for it, so the handler
    /// always stamps the in-memory `prefetch` map (cache benefits a
    /// later re-open) and persists the raw body into the SQLite
    /// `musicbrainz_search_cache` (Phase B-5).
    MbDetailPrefetchComplete {
        operation_id: TagsMbOperationId,
        release_id: String,
        result: Result<crate::tui::musicbrainz::MbDetailOutcome, String>,
    },
}

/// Re-emission target form for `:cue-fill`. Captures whether the source
/// CUE was single-image (one FILE) or multi-file, and for single-image
/// the FILE name + format keyword needed to reproduce it.
#[derive(Debug, Clone)]
pub enum CueFillLayout {
    MultiFile,
    SingleImage {
        image_filename: String,
        format_tag: String,
    },
}
