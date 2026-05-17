//! Button position tracking for mouse click detection in the TUI

use ratatui::layout::Rect;
use super::app::ConvertFocus;

/// Which metadata field is being referenced (for clicks and edit overlays)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MetadataFieldKind {
    Title,
    Artist,
    Album,
    Genre,
    Year,
}

/// Sortable column in the browse screen
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColumnKind {
    Name,
    Size,
    Date,
    Type,
}

/// Identifies a clickable element in the TUI
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TuiButton {
    // Tab bar (footer)
    Tab(u8), // 1-5

    // Convert screen panes (click to focus)
    Pane(ConvertFocus),

    // Convert screen pills
    FormatPill(usize),
    RatePill(usize),
    DepthPill(usize),
    DitherPill(usize),
    ReplayGainPill(usize),
    MergePill(usize),

    // Convert screen controls
    PresetsButton,
    SaveButton,
    AdvancedToggle(ConvertFocus),

    // Convert screen editable fields
    DestPathField,
    FolderTemplateField,
    FilenameTemplateField,
    MetadataField(MetadataFieldKind),

    // Convert screen: "browse files..." pill on source pane → opens browse screen
    SourceBrowseButton,
    // Convert screen: "expand" pill on source pane in Batch mode → opens
    // the BatchList overlay to view/manage the full file list.
    SourceExpandButton,
    // Convert screen: "analyze" pill on source pane → runs audio analysis.
    SourceAnalyzeButton,
    // Convert screen: "enqueue" pill → :commit (queue only).
    SourceEnqueueButton,
    // Convert screen: "enqueue + start" pill → :Commit (queue + start).
    SourceEnqueueStartButton,

    // Queue action bar
    AddFiles,
    AddFolder,
    Configure,
    Convert,
    Pause,
    Stop,
    ClearCompleted,
    ClearFinished,
    ClearAll,
    RetryFailed,

    // Queue items
    QueueItem(usize),

    // Overlay buttons
    OverlayConfirm,
    OverlayCancel,

    /// MetadataEditor per-row revert/use-MB pill. Argument is the
    /// 0-based index into `MetadataEditorState.entries`.
    MetadataEntryRevert(usize),
    /// MetadataEditor per-row `[view]` pill on synthetic-preview rows
    /// (currently CUESHEET). Click opens a read-only CuePreview
    /// overlay seeded with the entry's value. Argument is the same
    /// row index as MetadataEntryRevert.
    MetadataEntryView(usize),
    /// MetadataEditor detail-overlay field-level revert/use-MB pill.
    MetadataDetailRevert,
    /// MetadataEditor detail-overlay restore pill (per-file values
    /// snap back to the as-retrieved MB proposal).
    MetadataDetailRestore,

    /// MbSelect overlay: clickable row (0-based index into `releases`).
    MbSelectRow(usize),
    /// MbSelect footer "Accept" pill.
    MbSelectAccept,
    /// MbSelect footer "Cancel" pill.
    MbSelectCancel,

    /// CuePreview overlay: clickable content line (0-based line index).
    CuePreviewLine(usize),
    /// CuePreview footer pills (browsing mode — not currently editing
    /// a single line). Note: distinct from the overlay's `read_only`
    /// flag, which is a separate axis (no-Save / no-edit / blocked-`:`).
    CuePreviewSave,
    CuePreviewCancel,
    CuePreviewTop,
    CuePreviewBottom,
    /// CuePreview footer pills (line-edit mode).
    CuePreviewEditCommit,
    CuePreviewEditCancel,

    // Browse screen
    BrowseEntry(usize),
    BrowseColumn(ColumnKind),
    BrowseList, // catch-all region for scroll wheel routing

    // Browse info pane: clickable metadata field (click → edit tag).
    BrowseInfoMeta(crate::tui::probe::MetadataField),
    // Browse info pane: analyze pill (click → run audio analysis).
    BrowseInfoAnalyze,
    // Browse info pane: edit tags pill (click → open metadata editor).
    BrowseInfoEditTags,
    // Browse list: "search" label in border (click → toggle search panel).
    BrowseSearchToggle,
    // Browse search panel: toggle pills.
    BrowseSearchRecursive,
    BrowseSearchMode,
    BrowseSearchSort,
    BrowseSearchAudioOnly,
}

impl TuiButton {
    /// Which screen this button belongs to. Returns None for global
    /// buttons (Tab, Overlay) that work on any screen.
    pub fn screen(&self) -> Option<super::app::AppScreen> {
        use super::app::AppScreen;
        match self {
            Self::Tab(_)
            | Self::OverlayConfirm
            | Self::OverlayCancel
            | Self::MetadataEntryRevert(_)
            | Self::MetadataEntryView(_)
            | Self::MetadataDetailRevert
            | Self::MetadataDetailRestore
            | Self::MbSelectRow(_)
            | Self::MbSelectAccept
            | Self::MbSelectCancel
            | Self::CuePreviewLine(_)
            | Self::CuePreviewSave
            | Self::CuePreviewCancel
            | Self::CuePreviewTop
            | Self::CuePreviewBottom
            | Self::CuePreviewEditCommit
            | Self::CuePreviewEditCancel => None,
            Self::Pane(_)
            | Self::FormatPill(_)
            | Self::RatePill(_)
            | Self::DepthPill(_)
            | Self::DitherPill(_)
            | Self::ReplayGainPill(_)
            | Self::MergePill(_)
            | Self::PresetsButton
            | Self::SaveButton
            | Self::AdvancedToggle(_)
            | Self::DestPathField
            | Self::FolderTemplateField
            | Self::FilenameTemplateField
            | Self::MetadataField(_)
            | Self::SourceBrowseButton
            | Self::SourceExpandButton
            | Self::SourceAnalyzeButton
            | Self::SourceEnqueueButton
            | Self::SourceEnqueueStartButton => Some(AppScreen::Convert),
            Self::AddFiles
            | Self::AddFolder
            | Self::Configure
            | Self::Convert
            | Self::Pause
            | Self::Stop
            | Self::ClearCompleted
            | Self::ClearFinished
            | Self::ClearAll
            | Self::RetryFailed
            | Self::QueueItem(_) => Some(AppScreen::Queue),
            Self::BrowseEntry(_)
            | Self::BrowseColumn(_)
            | Self::BrowseList
            | Self::BrowseInfoMeta(_)
            | Self::BrowseInfoAnalyze
            | Self::BrowseInfoEditTags
            | Self::BrowseSearchToggle
            | Self::BrowseSearchRecursive
            | Self::BrowseSearchMode
            | Self::BrowseSearchSort
            | Self::BrowseSearchAudioOnly => Some(AppScreen::Browse),
        }
    }
}

/// Maps rendered button positions to their identities for mouse click detection
#[derive(Debug, Clone)]
pub struct ButtonRenderMap {
    button_bounds: Vec<(TuiButton, Rect)>,
}

impl ButtonRenderMap {
    pub fn new() -> Self {
        Self {
            button_bounds: Vec::new(),
        }
    }

    /// Clear all recorded button positions (call at start of each render)
    pub fn clear(&mut self) {
        self.button_bounds.clear();
    }

    /// Record that a button was rendered at the given screen coordinates
    pub fn record_button(&mut self, button: TuiButton, screen_rect: Rect) {
        self.button_bounds.push((button, screen_rect));
    }

    /// Find the screen rect for a specific button (for cursor-relative menus).
    pub fn find_button_rect(&self, target: &TuiButton) -> Option<Rect> {
        self.button_bounds
            .iter()
            .find(|(btn, _)| btn == target)
            .map(|(_, rect)| *rect)
    }

    /// Find which button (if any) contains the given screen coordinates.
    /// Returns the LAST recorded button at that position (topmost in draw order).
    pub fn find_button_at(&self, x: u16, y: u16) -> Option<TuiButton> {
        // Iterate in reverse so overlays/later-drawn elements take priority
        for (button, rect) in self.button_bounds.iter().rev() {
            if x >= rect.x && x < rect.x + rect.width
                && y >= rect.y && y < rect.y + rect.height
            {
                return Some(*button);
            }
        }
        None
    }
}

impl Default for ButtonRenderMap {
    fn default() -> Self {
        Self::new()
    }
}
