//! Message types for async communication in the TUI event loop

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
    /// Result of an async AR batch verification.
    ArBatchComplete {
        result: Box<crate::tui::accuraterip::ArBatchResult>,
    },
    /// Result of an async AR offset correction.
    OffsetCorrectionComplete {
        result: Result<String, String>,
    },
}
