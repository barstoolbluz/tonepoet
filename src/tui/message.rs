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
}
