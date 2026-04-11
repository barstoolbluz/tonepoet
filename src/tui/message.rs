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
}
