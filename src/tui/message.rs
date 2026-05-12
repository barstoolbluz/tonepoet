//! Message types for async communication in the TUI event loop

/// Where a `TagsFromMbSearchComplete` was spawned from. Used by the
/// shared handler to format the zero-match status line: a `Direct`
/// search reports only the failed query, while a `SacdFallback`
/// fired by `handle_tags_from_mb_toc_sacd_complete` after a TOC
/// miss reports both failure modes so the user sees the full
/// breadcrumb in one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagsMbSearchOrigin {
    /// Spawned directly by `:tags-mb` (no preceding TOC attempt).
    /// Reserved for future C-3 use; not wired today.
    Direct,
    /// Spawned by the SACD TOC handler's zero-match branch.
    SacdFallback,
}

/// Messages sent to the TUI event loop via mpsc channel
#[derive(Debug)]
pub enum AppMessage {
    /// A conversion item's progress was updated
    ConversionProgress {
        item_id: String,
        status: crate::convert::ConversionStatus,
    },
    /// All conversions completed
    ConversionComplete {
        completed: usize,
        failed: usize,
    },
    /// A conversion error occurred
    ConversionError {
        message: String,
    },
    /// Files were scanned and should be added to the queue
    FilesScanned {
        paths: Vec<std::path::PathBuf>,
    },
    /// Status message to show in the status bar
    StatusMessage(String),
    /// Force a redraw
    Redraw,
    /// Result of an asynchronous audio probe (lofty + ffmpeg) launched by
    /// `BrowseState::probe_current`. The main loop updates `probe_cache` and
    /// removes the path from `probe_pending`.
    AudioProbeComplete {
        path: std::path::PathBuf,
        /// Owned cached-info on success; error string on failure.
        result: Box<Result<crate::tui::browse::CachedInfo, String>>,
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
    MetadataEditorWriteComplete {
        /// Per-file results: (path, Ok or Err).
        results: Vec<(std::path::PathBuf, Result<(), String>)>,
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
        entries: Vec<(String, crate::tui::gnudb::GnudbEntry, Vec<std::path::PathBuf>)>,
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
    OffsetCorrectionComplete {
        result: Result<String, String>,
    },
    /// Result of an async CTDB Reed-Solomon repair.
    CtdbRepairComplete {
        result: Result<String, String>,
    },
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
    /// Result of an async MusicBrainz lookup driving `:tags-mb`. Carries
    /// the audio paths so the metadata editor can be opened on the same
    /// selection that triggered the lookup.
    TagsFromMbComplete {
        outcome: Result<crate::tui::musicbrainz::MbLookupOutcome, String>,
        paths: Vec<std::path::PathBuf>,
        toc_string: String,
    },
    /// Result of a Phase C-2a SACD `:tags-mb` **TOC** lookup. Mirrors
    /// the audio-file `TagsFromMbComplete` shape but routes through
    /// the SACD-specific handler, which adds editor parking and a
    /// zero-match text-search fallback (C-2b) on top of the standard
    /// 0/1/N branching.
    TagsFromMbTocSacdComplete {
        outcome: Result<crate::tui::musicbrainz::MbLookupOutcome, String>,
        paths: Vec<std::path::PathBuf>,
        toc_string: String,
    },
    /// Result of a Phase C-2b SACD `:tags-mb` **text/release search**
    /// fallback. Fired by the TOC handler when the primary lookup
    /// returns zero releases. Carries the `MbSearchOutcome` so the
    /// handler persists fresh response bodies into
    /// `musicbrainz_search_cache` (B-5) before the standard
    /// zero/single/multi branching. `query_label` is a short human
    /// rendering of the seed query for the status line. `origin`
    /// distinguishes the spawn site so the zero-match status can
    /// include the "TOC missed first" breadcrumb without losing it
    /// to the second `set_status` call.
    TagsFromMbSearchComplete {
        outcome: Result<crate::tui::musicbrainz::MbSearchOutcome, String>,
        paths: Vec<std::path::PathBuf>,
        query_label: String,
        origin: TagsMbSearchOrigin,
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
    SingleImage { image_filename: String, format_tag: String },
}
