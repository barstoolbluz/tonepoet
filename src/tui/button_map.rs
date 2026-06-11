//! Button position tracking for mouse click detection in the TUI

use super::app::ConvertFocus;
use ratatui::layout::Rect;

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
    ResamplerPill(usize),
    DitherPill(usize),
    ReplayGainPill(usize),
    NoiseShaperPill(usize),
    ModulatorOrderPill(usize),
    ConversionPresetPill(usize),
    DsdGainPill(usize),
    MergePill(usize),
    ContainerPill(usize),
    /// ⚙ pill on the container row (below-the-fold) to open format-specific settings overlay.
    FormatSettingsButton,
    /// Verify toggle pills inside the FormatSettings overlay.
    FormatSettingsVerify(usize),
    /// MD5 toggle pills inside the FormatSettings overlay.
    FormatSettingsMd5(usize),
    /// AAC profile pills inside the FormatSettings overlay.
    FormatSettingsAacProfile(usize),
    /// AAC quality preset pills inside the FormatSettings overlay.
    FormatSettingsAacQuality(usize),
    /// Opus content type pills inside the FormatSettings overlay.
    FormatSettingsOpusContentType(usize),
    /// Opus quality preset pills inside the FormatSettings overlay.
    FormatSettingsOpusQuality(usize),
    /// MP3 mode pills inside the FormatSettings overlay.
    FormatSettingsMp3Mode(usize),
    /// MP3 bitrate preset pills inside the FormatSettings overlay.
    FormatSettingsMp3Preset(usize),
    /// WavPack mode pills inside the FormatSettings overlay.
    FormatSettingsWavPackMode(usize),
    /// WavPack hybrid toggle pills inside the FormatSettings overlay.
    FormatSettingsWavPackHybrid(usize),
    /// WavPack correction toggle pills inside the FormatSettings overlay.
    FormatSettingsWavPackCorrection(usize),
    /// Resample quality pills (below-the-fold on format pane).
    ResampleQualityPill(usize),
    /// Resampler-specific settings pill (e.g. "ssrc settings").
    ResamplerSettingsButton,
    /// SSRC min phase toggle pills inside the FormatSettings overlay.
    FormatSettingsSsrcMinPhase(usize),
    /// SSRC PDF type pills inside the FormatSettings overlay.
    FormatSettingsSsrcPdf(usize),
    /// Sox chebyshev toggle pills inside the FormatSettings overlay.
    FormatSettingsSoxChebyshev(usize),
    /// Sox aliasing toggle pills inside the FormatSettings overlay.
    FormatSettingsSoxAliasing(usize),
    /// Sox sinc phase pills inside the FormatSettings overlay.
    FormatSettingsSoxSincPhase(usize),
    /// Soxr chebyshev toggle pills inside the FormatSettings overlay.
    FormatSettingsSoxrChebyshev(usize),

    // Convert screen controls
    PresetsButton,
    SaveButton,
    AdvancedToggle(ConvertFocus),
    /// Collapse/maximize toggle indicator in pane title bars.
    MaximizeToggle(ConvertFocus),

    // Convert screen editable fields
    DestPathField,
    FolderTemplateField,
    FilenameTemplateField,
    MetadataField(MetadataFieldKind),
    /// Convert metadata file-list row (absolute source index).
    MetadataFileRow(usize),

    // Convert screen: "browse files..." pill on source pane → opens browse screen
    SourceBrowseButton,
    // Convert screen: "expand" pill on source pane in Batch mode → opens
    // the BatchList overlay to view/manage the full file list.
    SourceExpandButton,
    // Convert screen: "analyze" pill on source pane → runs audio analysis.
    SourceAnalyzeButton,
    // Convert screen: stream pill left arrow (previous presentation).
    SourceStreamPrev,
    // Convert screen: stream pill right arrow (next presentation).
    SourceStreamNext,
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
    QueueItemExpand(usize),

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
    // Browse info pane: open the unified disc stream browser overlay.
    BrowseInfoAudioStreams,
    // Browse list: "search" label in border (click → toggle search panel).
    BrowseSearchToggle,
    // Browse search panel: toggle pills.
    BrowseSearchRecursive,
    BrowseSearchMode,
    BrowseSearchSort,
    BrowseSearchAudioOnly,

    // Disc browser overlay.
    DiscBrowserStream(usize),
    DiscBrowserExpand(usize),
    DiscBrowserConvert,
    DiscBrowserClose,

    // Template builder: open pills on output options pane.
    TemplateBuildFolderButton,
    TemplateBuildFilenameButton,
    TemplateLoadFolderButton,
    TemplateLoadFilenameButton,
    // Template builder overlay: clickable elements.
    TemplateBuilderToken(usize),
    TemplateBuilderSavedItem(usize),
    TemplateBuilderApply,
    TemplateBuilderSave,
    TemplateBuilderClear,
    TemplateBuilderDelete,

    // Template picker overlay: clickable elements.
    TemplatePickerRow(usize),
    TemplatePickerApply,
    TemplatePickerDelete,
    TemplatePickerClose,
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
            | Self::CuePreviewEditCancel
            | Self::DiscBrowserStream(_)
            | Self::DiscBrowserExpand(_)
            | Self::DiscBrowserConvert
            | Self::DiscBrowserClose
            | Self::TemplateBuilderToken(_)
            | Self::TemplateBuilderSavedItem(_)
            | Self::TemplateBuilderApply
            | Self::TemplateBuilderSave
            | Self::TemplateBuilderClear
            | Self::TemplateBuilderDelete
            | Self::TemplatePickerRow(_)
            | Self::TemplatePickerApply
            | Self::TemplatePickerDelete
            | Self::TemplatePickerClose
            | Self::FormatSettingsVerify(_)
            | Self::FormatSettingsMd5(_)
            | Self::FormatSettingsAacProfile(_)
            | Self::FormatSettingsAacQuality(_)
            | Self::FormatSettingsOpusContentType(_)
            | Self::FormatSettingsOpusQuality(_)
            | Self::FormatSettingsMp3Mode(_)
            | Self::FormatSettingsMp3Preset(_)
            | Self::FormatSettingsWavPackMode(_)
            | Self::FormatSettingsWavPackHybrid(_)
            | Self::FormatSettingsWavPackCorrection(_)
            | Self::FormatSettingsSsrcMinPhase(_)
            | Self::FormatSettingsSsrcPdf(_)
            | Self::FormatSettingsSoxChebyshev(_)
            | Self::FormatSettingsSoxAliasing(_)
            | Self::FormatSettingsSoxSincPhase(_)
            | Self::FormatSettingsSoxrChebyshev(_) => None,
            Self::Pane(_)
            | Self::FormatPill(_)
            | Self::RatePill(_)
            | Self::DepthPill(_)
            | Self::ResamplerPill(_)
            | Self::DitherPill(_)
            | Self::ReplayGainPill(_)
            | Self::NoiseShaperPill(_)
            | Self::ModulatorOrderPill(_)
            | Self::ConversionPresetPill(_)
            | Self::DsdGainPill(_)
            | Self::MergePill(_)
            | Self::ContainerPill(_)
            | Self::FormatSettingsButton
            | Self::ResampleQualityPill(_)
            | Self::ResamplerSettingsButton
            | Self::PresetsButton
            | Self::SaveButton
            | Self::AdvancedToggle(_)
            | Self::MaximizeToggle(_)
            | Self::DestPathField
            | Self::FolderTemplateField
            | Self::FilenameTemplateField
            | Self::MetadataField(_)
            | Self::MetadataFileRow(_)
            | Self::SourceBrowseButton
            | Self::SourceExpandButton
            | Self::SourceAnalyzeButton
            | Self::SourceStreamPrev
            | Self::SourceStreamNext
            | Self::SourceEnqueueButton
            | Self::SourceEnqueueStartButton
            | Self::TemplateBuildFolderButton
            | Self::TemplateBuildFilenameButton
            | Self::TemplateLoadFolderButton
            | Self::TemplateLoadFilenameButton => Some(AppScreen::Convert),
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
            | Self::QueueItem(_)
            | Self::QueueItemExpand(_) => Some(AppScreen::Queue),
            Self::BrowseEntry(_)
            | Self::BrowseColumn(_)
            | Self::BrowseList
            | Self::BrowseInfoMeta(_)
            | Self::BrowseInfoAnalyze
            | Self::BrowseInfoEditTags
            | Self::BrowseInfoAudioStreams
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
    metadata_file_list_visible_rows: Option<usize>,
}

impl ButtonRenderMap {
    pub fn new() -> Self {
        Self {
            button_bounds: Vec::new(),
            metadata_file_list_visible_rows: None,
        }
    }

    /// Clear all recorded button positions (call at start of each render)
    pub fn clear(&mut self) {
        self.button_bounds.clear();
        self.metadata_file_list_visible_rows = None;
    }

    /// Record that a button was rendered at the given screen coordinates
    pub fn record_button(&mut self, button: TuiButton, screen_rect: Rect) {
        self.button_bounds.push((button, screen_rect));
    }

    /// Record the current metadata file-list viewport size. This is transient
    /// UI geometry, cleared at the start of each render alongside button
    /// hitboxes. It deliberately does not live in ConvertState.
    pub fn record_metadata_file_list_visible_rows(&mut self, visible_rows: usize) {
        self.metadata_file_list_visible_rows = Some(visible_rows);
    }

    /// Return the metadata file-list viewport size from the most recent render.
    pub fn metadata_file_list_visible_rows(&self) -> Option<usize> {
        self.metadata_file_list_visible_rows
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
            if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height {
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
