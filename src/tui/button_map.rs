//! Button position tracking for mouse click detection in the TUI

use super::app::ConvertFocus;
use ratatui::layout::Rect;

/// Which metadata field is being referenced (for clicks and edit overlays)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MetadataFieldKind {
    Title,
    Artist,
    Album,
    AlbumArtist,
    Genre,
    Year,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarSurface {
    BrowseList,
    BrowseTree,
    BookmarkManager,
}

/// Identifies a clickable element in the TUI
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TuiButton {
    // Tab bar (footer)
    Tab(u8), // 1-5
    /// Reopen the most recent completed file-task details.
    FileTaskMessages,

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
    DsdPathPill(usize),
    DsdProfilePill(usize),
    DsdGainPill(usize),
    DsdGainDbField,
    DsdNormalizeTargetField,
    MergePill(usize),
    ForceEncodePill(usize),
    DiscSubfoldersPill(usize),
    WriteLogPill(usize),
    ContainerPill(usize),
    /// Settings pill on the container row (below the fold).
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
    CompanionExtensionsField,
    ExcludeFilesField,
    CompanionFoldersField,
    /// Output Options Actions row. Click opens the conversion-actions wizard.
    ActionsPipelineField,
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
    /// Active single-line input in FileInput, CommandInput, or TextEdit.
    OverlayTextInput,

    /// MetadataEditor presentation-tab selector. Argument is the
    /// 0-based index into `MetadataEditorState.presentation_tabs`.
    MetadataEditorTab(usize),
    /// MetadataEditor content-tab selector. Argument is:
    /// 0=Metadata, 1=Details, 2=ReplayGain, 3=Artwork.
    MetadataEditorContentTab(usize),
    /// MetadataEditor 3+-presentation dropdown toggle row.
    MetadataPresentationSelectorToggle,
    /// MetadataEditor 3+-presentation dropdown row. Argument is the
    /// 0-based index into `MetadataEditorState.presentation_tabs`.
    MetadataPresentationSelectorRow(usize),

    /// Active MetadataEditor inline/add/detail input field.
    MetadataEditorInput,
    /// Active GNUDB/CUE-review inline input field.
    GnudbEditorInput,
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
    /// MetadataEditor Details tab: analyze HDCD + non-spectral PRE facts.
    MetadataDetailsAnalyze,
    /// MetadataEditor ReplayGain tab: scan all active tracks.
    MetadataReplayGainScanTrack,
    /// MetadataEditor ReplayGain tab: scan album + tracks.
    MetadataReplayGainScanAlbum,
    /// MetadataEditor Artwork tab row hit target. Argument is the
    /// 0-based artwork coverage row index.
    MetadataArtworkRow(usize),
    /// MetadataEditor Artwork tab add button for a missing artwork type.
    MetadataArtworkAdd(usize),
    /// MetadataEditor Artwork tab replace button for an existing artwork type.
    MetadataArtworkReplace(usize),
    /// MetadataEditor Artwork tab remove button for an existing artwork type.
    MetadataArtworkRemove(usize),
    /// Metadata auto-number overlay scheme pill.
    MetadataAutoNumberScheme(usize),
    /// Metadata auto-number overlay prefix input.
    MetadataAutoNumberPrefix,
    /// Metadata auto-number overlay row.
    MetadataAutoNumberRow(usize),
    /// Metadata auto-number overlay footer actions.
    MetadataAutoNumberApply,
    MetadataAutoNumberCancel,

    /// MbSelect overlay: clickable row (0-based index into `releases`).
    MbSelectRow(usize),
    /// MbSelect footer "Accept" pill.
    MbSelectAccept,
    /// MbSelect footer "Cancel" pill.
    MbSelectCancel,

    /// CUE selection overlay: clickable candidate row.
    CueSelectRow(usize),
    /// CUE selection overlay footer actions.
    CueSelectAccept,
    CueSelectCancel,

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
    /// Browse row selection gutter (checkbox column). Click toggles mark without moving cursor.
    BrowseEntryGutter(usize),
    BrowseColumn(crate::tui::browse::BrowseColumn),
    BrowseList, // catch-all region for scroll wheel routing
    /// The Browse pane's inline create-name row (focus-preserving click target).
    BrowseCreateRow,
    BrowseBreadcrumb, // click to edit path
    /// Precise editable text cells inside a Browse list rename/create row.
    BrowseFileInlineEdit,
    /// Precise editable text cells inside the Browse path bar.
    BrowsePathInlineEdit,
    BrowseToolbarBack,
    BrowseToolbarForward,
    BrowseToolbarUp,
    BrowseToolbarRefresh,
    BrowseToolbarOptions,
    BrowseToolbarSearch,
    BrowseToolbarShowHidden,
    BrowsePathGo,
    BrowseBookmarksToggle,
    BrowseBookmarkDropdownRow(usize),
    BrowseBookmarkDropdownAdd,
    BrowseBookmarkDropdownManage,
    BookmarkManagerRow(usize),
    ScrollbarTrack(ScrollbarSurface),
    ScrollbarThumb(ScrollbarSurface),
    BrowsePaneToggle(crate::tui::browse::BrowsePaneId),
    BrowsePaneTitle(crate::tui::browse::BrowsePaneId),
    BrowseTreeNode(usize),
    /// Disclosure glyph within a Browse tree row; toggles expansion without navigation.
    BrowseTreeDisclosure(usize),
    /// Inline name editor rendered inside the Browse tree pane.
    BrowseTreeInlineEdit,
    BrowseOptionsShowHidden,
    BrowseOptionsLayout,
    BrowseOptionsToggleExplore,
    BrowseOptionsToggleInfo,
    BrowseOptionsColumns,
    BrowseOptionsSort,
    BrowseOptionsFilter,
    BrowseOptionsArchiveListing,
    BrowseOptionsSaveLayout,
    BrowseOptionsRestoreDefaults,
    BrowseOptionsColumn(crate::tui::browse::BrowseColumn),
    BrowseOptionsSortChoice(crate::tui::browse::SortBy, crate::tui::browse::SortDir),
    BrowseOptionsFilterChoice(usize),
    BrowseOptionsArchiveChoice(usize),

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
    // Browse search panel: input + toggle pills.
    BrowseSearchInput,
    BrowseFilterInput,
    BrowseSearchRecursive,
    BrowseSearchMode,
    BrowseSearchSort,
    BrowseSearchAudioOnly,

    // Disc browser overlay.
    DiscBrowserStream(usize),
    DiscBrowserExpand(usize),
    DiscBrowserConvert,
    DiscBrowserClose,

    // Conversion actions wizard overlay.
    ActionsAvailable(usize),
    /// Available pane body. Used for blank-space hover/scroll.
    ActionsAvailablePane,
    /// Pipeline pane body. Used for blank-space hover/scroll.
    ActionsPipelinePane,
    /// Pipeline row. bool=true means pre-conversion, false means post-conversion.
    ActionsPipelineRow(bool, usize),
    ActionsPipelineNudgeUp(bool, usize),
    ActionsPipelineNudgeDown(bool, usize),
    ActionsAddingPhase(bool),
    ActionsFooterAdd,
    ActionsFooterConfigure,
    ActionsFooterSave,
    ActionsFooterSaveDefault,
    ActionsFooterDone,
    ActionsConfigField(usize),
    ActionsConfigMode(usize),
    ActionsConfigToken(usize),
    ActionsConfigApply,
    ActionsConfigCancel,
    ActionsConfigPreview,
    /// Catch-all surface for Dialog B so modal blank space never falls through.
    ActionsConfigModal,

    // Template builder: open pills on output options pane.
    TemplateBuildFolderButton,
    TemplateBuildFilenameButton,
    TemplateLoadFolderButton,
    TemplateLoadFilenameButton,
    // Template builder overlay: clickable elements.
    TemplateBuilderInput,
    TemplateBuilderToken(usize),
    TemplateBuilderSavedItem(usize),
    TemplateBuilderApply,
    TemplateBuilderSave,
    TemplateBuilderClear,
    TemplateBuilderDelete,

    /// Bulk rename template input.
    BulkRenameTemplateInput,

    // Template picker overlay: clickable elements.
    TemplatePickerRow(usize),
    TemplatePickerApply,
    TemplatePickerDelete,
    TemplatePickerClose,

    // Config screen appearance pane.
    ConfigThemePrev,
    ConfigThemeNext,
    ConfigThemeMode,
    ConfigThemeDark,
    ConfigThemeLight,
    ConfigThemeBrowse,

    // Theme Builder overlay.
    ThemeBuilderPreset,
    ThemeBuilderTab(usize),
    ThemeBuilderMoreMenu,
    ThemeBuilderMoreMenuItem(usize),
    ThemeBuilderGalleryMode,
    ThemeBuilderGalleryFilter,
    ThemeBuilderMode,
    ThemeBuilderSlot(crate::tui::theme::BuilderSlot),
    ThemeBuilderHexField,
    ThemeBuilderRgbSlider(usize),
    ThemeBuilderDepth(crate::tui::theme::ColorDepth),
    ThemeBuilderInlineSwatchName,
    ThemeBuilderSavedSwatch(usize),
    ThemeBuilderSaveSwatch,
    ThemeBuilderSave,
    ThemeBuilderApply,
    ThemeBuilderDeleteConfirm,
    ThemeBuilderDeleteCancel,
    ThemeBuilderCancel,
    ThemeBuilderPresetRow(usize),
    ThemeBuilderPresetCancel,
    ThemeBuilderDerivedRow(usize),
    ThemeBuilderDerivedLock,
    ThemeBuilderApplyThemeLocks,
    ThemeBuilderApplyUserOverrides,
    ThemeBuilderApplyConfirm,
    ThemeBuilderApplyCancel,
    ThemeBuilderFilePath,
    ThemeBuilderFileConfirm,
    ThemeBuilderFileCancel,
}

impl TuiButton {
    /// Which screen this button belongs to. Returns None for global
    /// buttons (Tab, Overlay) that work on any screen.
    pub fn screen(&self) -> Option<super::app::AppScreen> {
        use super::app::AppScreen;
        match self {
            Self::Tab(_)
            | Self::FileTaskMessages
            | Self::OverlayConfirm
            | Self::OverlayCancel
            | Self::OverlayTextInput
            | Self::MetadataEditorInput
            | Self::GnudbEditorInput
            | Self::MetadataEditorTab(_)
            | Self::MetadataEditorContentTab(_)
            | Self::MetadataPresentationSelectorToggle
            | Self::MetadataPresentationSelectorRow(_)
            | Self::MetadataEntryRevert(_)
            | Self::MetadataEntryView(_)
            | Self::MetadataDetailRevert
            | Self::MetadataDetailRestore
            | Self::MetadataDetailsAnalyze
            | Self::MetadataReplayGainScanTrack
            | Self::MetadataReplayGainScanAlbum
            | Self::MetadataArtworkRow(_)
            | Self::MetadataArtworkAdd(_)
            | Self::MetadataArtworkReplace(_)
            | Self::MetadataArtworkRemove(_)
            | Self::MetadataAutoNumberScheme(_)
            | Self::MetadataAutoNumberPrefix
            | Self::MetadataAutoNumberRow(_)
            | Self::MetadataAutoNumberApply
            | Self::MetadataAutoNumberCancel
            | Self::MbSelectRow(_)
            | Self::MbSelectAccept
            | Self::MbSelectCancel
            | Self::CueSelectRow(_)
            | Self::CueSelectAccept
            | Self::CueSelectCancel
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
            | Self::ActionsAvailable(_)
            | Self::ActionsAvailablePane
            | Self::ActionsPipelinePane
            | Self::ActionsPipelineRow(_, _)
            | Self::ActionsPipelineNudgeUp(_, _)
            | Self::ActionsPipelineNudgeDown(_, _)
            | Self::ActionsAddingPhase(_)
            | Self::ActionsFooterAdd
            | Self::ActionsFooterConfigure
            | Self::ActionsFooterSave
            | Self::ActionsFooterSaveDefault
            | Self::ActionsFooterDone
            | Self::ActionsConfigField(_)
            | Self::ActionsConfigMode(_)
            | Self::ActionsConfigToken(_)
            | Self::ActionsConfigApply
            | Self::ActionsConfigCancel
            | Self::ActionsConfigPreview
            | Self::ActionsConfigModal
            | Self::TemplateBuilderInput
            | Self::TemplateBuilderToken(_)
            | Self::BulkRenameTemplateInput
            | Self::TemplateBuilderSavedItem(_)
            | Self::TemplateBuilderApply
            | Self::TemplateBuilderSave
            | Self::TemplateBuilderClear
            | Self::TemplateBuilderDelete
            | Self::TemplatePickerRow(_)
            | Self::TemplatePickerApply
            | Self::TemplatePickerDelete
            | Self::TemplatePickerClose
            | Self::ThemeBuilderPreset
            | Self::ThemeBuilderTab(_)
            | Self::ThemeBuilderMoreMenu
            | Self::ThemeBuilderMoreMenuItem(_)
            | Self::ThemeBuilderGalleryMode
            | Self::ThemeBuilderGalleryFilter
            | Self::ThemeBuilderMode
            | Self::ThemeBuilderSlot(_)
            | Self::ThemeBuilderHexField
            | Self::ThemeBuilderRgbSlider(_)
            | Self::ThemeBuilderDepth(_)
            | Self::ThemeBuilderInlineSwatchName
            | Self::ThemeBuilderSavedSwatch(_)
            | Self::ThemeBuilderSaveSwatch
            | Self::ThemeBuilderSave
            | Self::ThemeBuilderApply
            | Self::ThemeBuilderDeleteConfirm
            | Self::ThemeBuilderDeleteCancel
            | Self::ThemeBuilderCancel
            | Self::ThemeBuilderPresetRow(_)
            | Self::ThemeBuilderPresetCancel
            | Self::ThemeBuilderDerivedRow(_)
            | Self::ThemeBuilderDerivedLock
            | Self::ThemeBuilderApplyThemeLocks
            | Self::ThemeBuilderApplyUserOverrides
            | Self::ThemeBuilderApplyConfirm
            | Self::ThemeBuilderApplyCancel
            | Self::ThemeBuilderFilePath
            | Self::ThemeBuilderFileConfirm
            | Self::ThemeBuilderFileCancel
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
            | Self::DsdPathPill(_)
            | Self::DsdProfilePill(_)
            | Self::DsdGainPill(_)
            | Self::DsdGainDbField
            | Self::DsdNormalizeTargetField
            | Self::MergePill(_)
            | Self::ForceEncodePill(_)
            | Self::DiscSubfoldersPill(_)
            | Self::WriteLogPill(_)
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
            | Self::CompanionExtensionsField
            | Self::ExcludeFilesField
            | Self::CompanionFoldersField
            | Self::ActionsPipelineField
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
            Self::ConfigThemePrev
            | Self::ConfigThemeNext
            | Self::ConfigThemeMode
            | Self::ConfigThemeDark
            | Self::ConfigThemeLight
            | Self::ConfigThemeBrowse => Some(AppScreen::Config),
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
            | Self::BrowseEntryGutter(_)
            | Self::BrowseColumn(_)
            | Self::BrowseList
            | Self::BrowseCreateRow
            | Self::BrowseBreadcrumb
            | Self::BrowseFileInlineEdit
            | Self::BrowsePathInlineEdit
            | Self::BrowseToolbarBack
            | Self::BrowseToolbarForward
            | Self::BrowseToolbarUp
            | Self::BrowseToolbarRefresh
            | Self::BrowseToolbarOptions
            | Self::BrowseToolbarSearch
            | Self::BrowseToolbarShowHidden
            | Self::BrowsePathGo
            | Self::BrowseBookmarksToggle
            | Self::BrowseBookmarkDropdownRow(_)
            | Self::BrowseBookmarkDropdownAdd
            | Self::BrowseBookmarkDropdownManage
            | Self::BookmarkManagerRow(_)
            | Self::ScrollbarTrack(_)
            | Self::ScrollbarThumb(_)
            | Self::BrowsePaneToggle(_)
            | Self::BrowsePaneTitle(_)
            | Self::BrowseTreeNode(_)
            | Self::BrowseTreeDisclosure(_)
            | Self::BrowseTreeInlineEdit
            | Self::BrowseOptionsShowHidden
            | Self::BrowseOptionsLayout
            | Self::BrowseOptionsToggleExplore
            | Self::BrowseOptionsToggleInfo
            | Self::BrowseOptionsColumns
            | Self::BrowseOptionsSort
            | Self::BrowseOptionsFilter
            | Self::BrowseOptionsArchiveListing
            | Self::BrowseOptionsSaveLayout
            | Self::BrowseOptionsRestoreDefaults
            | Self::BrowseOptionsColumn(_)
            | Self::BrowseOptionsSortChoice(_, _)
            | Self::BrowseOptionsFilterChoice(_)
            | Self::BrowseOptionsArchiveChoice(_)
            | Self::BrowseInfoMeta(_)
            | Self::BrowseInfoAnalyze
            | Self::BrowseInfoEditTags
            | Self::BrowseInfoAudioStreams
            | Self::BrowseSearchToggle
            | Self::BrowseSearchInput
            | Self::BrowseFilterInput
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
    output_options_layout: Option<(bool, u16)>,
}

/// Reusable double-click detector for button-map targets.
///
/// A double click is the same rendered target, at the same terminal cell, inside
/// a bounded interval. Keeping this state outside individual widgets prevents
/// each overlay from inventing subtly different timing and target semantics.
#[derive(Debug, Clone, Default)]
pub struct DoubleClickState {
    last: Option<(TuiButton, u16, u16, std::time::Instant)>,
}

impl DoubleClickState {
    pub fn register_click(
        &mut self,
        target: TuiButton,
        x: u16,
        y: u16,
        interval: std::time::Duration,
    ) -> bool {
        let now = std::time::Instant::now();
        let is_double = self
            .last
            .as_ref()
            .map(|(prior, px, py, at)| {
                *prior == target && *px == x && *py == y && now.duration_since(*at) <= interval
            })
            .unwrap_or(false);
        self.last = if is_double { None } else { Some((target, x, y, now)) };
        is_double
    }

    pub fn clear(&mut self) {
        self.last = None;
    }
}

impl ButtonRenderMap {
    pub fn new() -> Self {
        Self {
            button_bounds: Vec::new(),
            metadata_file_list_visible_rows: None,
            output_options_layout: None,
        }
    }

    /// Clear all recorded button positions (call at start of each render)
    pub fn clear(&mut self) {
        self.button_bounds.clear();
        self.metadata_file_list_visible_rows = None;
        self.output_options_layout = None;
    }

    /// Record that a button was rendered at the given screen coordinates.
    pub fn record_button(&mut self, button: TuiButton, screen_rect: Rect) {
        self.button_bounds.push((button, screen_rect));
    }

    /// Expose recorded buttons for render helpers/tests without exposing the
    /// internal vector mutably.
    pub fn recorded_buttons(&self) -> &[(TuiButton, Rect)] {
        &self.button_bounds
    }

    /// Return the most recently registered rectangle for `button`.
    pub fn button_rect(&self, button: TuiButton) -> Option<Rect> {
        self.button_bounds
            .iter()
            .rev()
            .find_map(|(candidate, rect)| (*candidate == button).then_some(*rect))
    }


    /// Record the Output Options pane geometry from the most recent Convert
    /// screen render. Keyboard focus cycling uses this to avoid selecting rows
    /// that are not rendered in small maximized panes.
    pub fn record_output_options_layout(&mut self, maximized: bool, area_height: u16) {
        self.output_options_layout = Some((maximized, area_height));
    }

    /// Return the Output Options pane geometry from the most recent render.
    pub fn output_options_layout(&self) -> Option<(bool, u16)> {
        self.output_options_layout
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
        for (button, rect) in self.button_bounds.iter().rev() {
            if x >= rect.x
                && x < rect.x.saturating_add(rect.width)
                && y >= rect.y
                && y < rect.y.saturating_add(rect.height)
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

