//! Message types for async communication in the TUI event loop

/// Outcome envelope for the unified `:tags-mb` flow. The handler
/// switches on this to apply TOC-specific (cache by `toc_string`)
/// vs. search-specific (cache by `cache_writes`) persistence and to
/// pick the right zero-match status text; the downstream 0/1/N
/// branching is the same shape for both.
#[derive(Debug)]
pub enum MbOutcome {
    /// Result of a `/ws/2/discid/-?toc=…` lookup. `toc_string` is
    /// the cache key under which the handler persists
    /// `outcome.cache_response`.
    Toc {
        outcome: Result<crate::tui::musicbrainz::MbLookupOutcome, String>,
        toc_string: String,
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
#[derive(Debug, Clone)]
pub struct TagsMbContext {
    pub paths: Vec<std::path::PathBuf>,
    /// `true` = the dispatch left a metadata editor in
    /// `active_overlay` that should be populated in place; the
    /// handler manages parking for the multi-match case.
    /// `false` = no editor in scope; the handler opens a fresh
    /// editor on single-match / pick.
    pub editor_park: bool,
    pub fallback_seed: Option<crate::tui::command::SacdMbSeed>,
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
    /// Force a redraw
    Redraw,
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
    /// Result of an asynchronous audio probe (lofty + ffmpeg) launched by
    /// `BrowseState::probe_current`. The main loop updates `probe_cache` and
    /// removes the path from `probe_pending`. Reducers must not perform
    /// follow-up media/tag reads here; worker-side probe code must enrich
    /// optional metadata before sending this message.
    AudioProbeComplete {
        path: std::path::PathBuf,
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
    /// `BrowseState::probe_current` for a directory entry. The main loop
    /// updates `dir_stats_cache` and removes the path from `dir_stats_pending`.
    DirStatsComplete {
        path: std::path::PathBuf,
        stats: crate::tui::browse::DirStats,
    },
    /// Result of an async audio analysis (DR, peak, RMS, etc.).
    /// `result` is Ok on success, Err(message) on failure.
    AnalysisComplete {
        result: Result<Box<crate::tui::analyze::AnalysisResult>, String>,
    },
    /// Result of an async file integrity verification.
    VerifyComplete {
        result: crate::tui::verify::VerifyResult,
    },
    /// Result of an async pre-emphasis detection scan.
    PreemphasisComplete {
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
        result: crate::tui::bit_compare::CompareResult,
    },
    /// Result of async path validation (canonicalize + is_dir) for :cd.
    PathValidationComplete {
        input: String,
        result: Result<std::path::PathBuf, String>,
    },
    /// Result of an async directory scan (readdir + lstat per entry).
    DirScanComplete {
        path: std::path::PathBuf,
        parent_entry: Option<crate::tui::browse::BrowseEntry>,
        dirs: Vec<crate::tui::browse::BrowseEntry>,
        files: Vec<crate::tui::browse::BrowseEntry>,
        error: Option<String>,
    },
    /// Result of an async metadata tag write.
    MetadataWriteComplete {
        path: std::path::PathBuf,
        field: crate::tui::probe::MetadataField,
        result: Result<(), String>,
    },
    /// Results of an async recursive search.
    SearchComplete {
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
    /// Result of add/replace/remove artwork launched from the Artwork tab.
    MetadataEditorArtworkWriteComplete {
        session_id: u64,
        generation: u64,
        mode: crate::tui::app::MetadataArtworkWriteMode,
        paths: Vec<std::path::PathBuf>,
        result: Result<Vec<crate::tui::probe::SourceMetadata>, String>,
    },
    /// Result of an async archive listing (`7zz l -slt`).
    ArchiveListingComplete {
        archive_path: std::path::PathBuf,
        result: Box<Result<crate::tui::archive_listing::ArchiveListing, String>>,
        password: Option<String>,
    },
    /// Result of an async GNUDB query.
    GnudbQueryComplete {
        result: Result<Vec<crate::tui::gnudb::GnudbMatch>, String>,
        paths: Vec<std::path::PathBuf>,
    },
    /// Result of an async GNUDB read (single entry).
    GnudbReadComplete {
        result: Result<crate::tui::gnudb::GnudbEntry, String>,
        paths: Vec<std::path::PathBuf>,
        /// Original match list for "back" navigation (None for single/auto-read).
        origin_matches: Option<Vec<crate::tui::gnudb::GnudbMatch>>,
    },
    /// Result of a multi-disc GNUDB query (sequential queries per disc).
    GnudbMultiDiscComplete {
        /// Per-disc results: (disc_label, entry, file_paths).
        entries: Vec<(
            String,
            crate::tui::gnudb::GnudbEntry,
            Vec<std::path::PathBuf>,
        )>,
    },
    /// Result of an async AccurateRip verification (one or more discs).
    AccurateRipComplete {
        pages: Vec<crate::tui::app::ArVerifyPage>,
    },
    /// Result of an async CTDB verification (one or more discs).
    CtdbComplete {
        pages: Vec<crate::tui::app::CtdbVerifyPage>,
    },
    /// Result of an async AR batch verification.
    ArBatchComplete {
        result: Box<crate::tui::accuraterip::ArBatchResult>,
    },
    /// Result of an async AR offset correction.
    OffsetCorrectionComplete { result: Result<String, String> },
    /// Result of an async CTDB Reed-Solomon repair.
    CtdbRepairComplete { result: Result<String, String> },
    /// Result of an async MusicBrainz disc-TOC lookup driving `:cue-mb`.
    /// `outcome` is `Err` when transport/parse failed; `Ok(None)` means
    /// no release matched. `paths`, `output_dir`, `single_image` carry
    /// the original command context to the main thread for CUE writing.
    /// `toc_string` is provided so the handler can write `cache_response`
    /// back into the SQLite cache.
    CueMbComplete {
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
        result: Result<(String, std::path::PathBuf, String), String>,
    },
    /// Result of async `:cue-fill` preparation. Carries probe-derived album,
    /// track, layout, and TOC-sector data back to the event loop so DB cache
    /// lookup still happens on the main thread before the MB request is spawned.
    CueFillPrepComplete {
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
    /// `MbOutcome` envelope + `TagsMbContext`. The same handler
    /// drives all of them; populate-vs-open-fresh and fallback
    /// eligibility ride on `ctx`.
    TagsFromMbComplete {
        outcome: MbOutcome,
        ctx: TagsMbContext,
    },
    /// Result of the blocking single-image MusicBrainz guard checks used
    /// before applying a selected release to the metadata editor. The guard
    /// may read tags and probe sample counts, so it must complete on a
    /// blocking worker before the event-loop reducer mutates UI state.
    TagsMbApplyReady {
        releases: Vec<crate::tui::musicbrainz::MbRelease>,
        selected: usize,
        paths: Vec<std::path::PathBuf>,
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
