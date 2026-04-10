//! Application state for the standalone TUI

use std::path::PathBuf;

use crate::config::TonepoetConfig;
use crate::convert::formats::AudioFormat;
use crate::convert::simple_wizard::DitherType;
use crate::convert::{ConversionConfig, ConversionItem, ConversionManager};
use crate::tui::button_map::ButtonRenderMap;
use crate::tui::pill::PillState;
use crate::tui::probe::{SourceInfo, SourceMetadata};

// ── Screen / tab navigation ──────────────────────────────────────────

/// Which screen is currently displayed
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppScreen {
    Convert,   // Tab 1 — main convert view
    Browse,    // Tab 2 — placeholder
    Library,   // Tab 3 — placeholder
    Queue,     // Tab 4 — file queue
    Config,    // Tab 5 — settings
    Wizard,    // Full-screen overlay (not a tab)
}

impl AppScreen {
    /// Tab number (1-5), or None for overlays like Wizard
    pub fn tab_number(&self) -> Option<u8> {
        match self {
            Self::Convert => Some(1),
            Self::Browse => Some(2),
            Self::Library => Some(3),
            Self::Queue => Some(4),
            Self::Config => Some(5),
            Self::Wizard => None,
        }
    }

    pub fn tab_label(&self) -> &'static str {
        match self {
            Self::Convert => "convert",
            Self::Browse => "browse",
            Self::Library => "library",
            Self::Queue => "queue",
            Self::Config => "config",
            Self::Wizard => "",
        }
    }

    /// All tab screens in order
    pub fn tabs() -> &'static [AppScreen] {
        &[
            Self::Convert,
            Self::Browse,
            Self::Library,
            Self::Queue,
            Self::Config,
        ]
    }
}

// ── Convert screen state ─────────────────────────────────────────────

/// Which pane in the convert screen has focus
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConvertFocus {
    Source,
    Metadata,
    Format,
    OutputOptions,
}

impl ConvertFocus {
    pub fn next(&self) -> Self {
        match self {
            Self::Source => Self::Metadata,
            Self::Metadata => Self::Format,
            Self::Format => Self::OutputOptions,
            Self::OutputOptions => Self::Source,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::Source => Self::OutputOptions,
            Self::Metadata => Self::Source,
            Self::Format => Self::Metadata,
            Self::OutputOptions => Self::Format,
        }
    }
}

/// ReplayGain mode for the format pane pill
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReplayGainChoice {
    Off,
    Album,
    Track,
    Both,
}

/// Bit depth options including float formats
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BitDepthChoice {
    Int16,
    Int24,
    Int32,
    Float32,
    Float64,
}

impl BitDepthChoice {
    /// Get the integer bit depth (for formats that use integer encoding)
    pub fn bits(&self) -> u32 {
        match self {
            Self::Int16 => 16,
            Self::Int24 => 24,
            Self::Int32 => 32,
            Self::Float32 => 32,
            Self::Float64 => 64,
        }
    }

    /// Map to the backend's bit depth convention (320 = float32)
    pub fn to_backend_depth(&self) -> u32 {
        match self {
            Self::Int16 => 16,
            Self::Int24 => 24,
            Self::Int32 => 32,
            Self::Float32 => 320,
            Self::Float64 => 640, // not yet supported by backend
        }
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Self::Float32 | Self::Float64)
    }
}

/// Merge mode for the output options pane
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MergeMode {
    MultiFile,
    SingleImage,
}

/// State for the source pane
#[derive(Debug, Clone)]
pub struct SourceState {
    pub file_path: Option<PathBuf>,
    pub info: Option<SourceInfo>,
    pub metadata: SourceMetadata,
    pub advanced_open: bool,
}

impl Default for SourceState {
    fn default() -> Self {
        Self {
            file_path: None,
            info: None,
            metadata: SourceMetadata::default(),
            advanced_open: false,
        }
    }
}

/// Which row in the format pane is focused
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FormatField {
    Format,
    SampleRate,
    BitDepth,
    Dither,
    ReplayGain,
}

impl FormatField {
    pub fn next(&self) -> Self {
        match self {
            Self::Format => Self::SampleRate,
            Self::SampleRate => Self::BitDepth,
            Self::BitDepth => Self::Dither,
            Self::Dither => Self::ReplayGain,
            Self::ReplayGain => Self::Format,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::Format => Self::ReplayGain,
            Self::SampleRate => Self::Format,
            Self::BitDepth => Self::SampleRate,
            Self::Dither => Self::BitDepth,
            Self::ReplayGain => Self::Dither,
        }
    }
}

/// Which row in the output options pane is focused
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputOptionsField {
    DestPath,
    FolderTemplate,
    FilenameTemplate,
    MergeMode,
}

impl OutputOptionsField {
    pub fn next(&self) -> Self {
        match self {
            Self::DestPath => Self::FolderTemplate,
            Self::FolderTemplate => Self::FilenameTemplate,
            Self::FilenameTemplate => Self::MergeMode,
            Self::MergeMode => Self::DestPath,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::DestPath => Self::MergeMode,
            Self::FolderTemplate => Self::DestPath,
            Self::FilenameTemplate => Self::FolderTemplate,
            Self::MergeMode => Self::FilenameTemplate,
        }
    }
}

/// State for the format pane (formerly "output")
#[derive(Debug, Clone)]
pub struct FormatState {
    pub format: PillState<AudioFormat>,
    pub sample_rate: PillState<u32>,
    pub bit_depth: PillState<BitDepthChoice>,
    pub dither: PillState<DitherType>,
    pub replaygain: PillState<ReplayGainChoice>,
    pub field_focus: FormatField,
    pub advanced_open: bool,
}

impl FormatState {
    pub fn new() -> Self {
        let format = PillState::new(
            AudioFormat::common_output()
                .into_iter()
                .map(|f| (f, f.name()))
                .collect(),
        );

        let sample_rate = PillState::new(vec![
            (44_100, "44.1"),
            (48_000, "48"),
            (88_200, "88.2"),
            (96_000, "96"),
            (176_400, "176.4"),
            (192_000, "192"),
            (352_800, "352.8"),
            (384_000, "384"),
            (705_600, "705.6"),
            (768_000, "768"),
        ]);

        let bit_depth = PillState::new(vec![
            (BitDepthChoice::Int16, "16"),
            (BitDepthChoice::Int24, "24"),
            (BitDepthChoice::Int32, "32"),
            (BitDepthChoice::Float32, "32f"),
            (BitDepthChoice::Float64, "64f"),
        ]);

        let dither = PillState::new(vec![
            (DitherType::TPDF, "TPDF"),
            (DitherType::None, "none"),
            (DitherType::Shibata, "shaped"),
        ]);

        let replaygain = PillState::new(vec![
            (ReplayGainChoice::Album, "album"),
            (ReplayGainChoice::Track, "track"),
            (ReplayGainChoice::Both, "both"),
            (ReplayGainChoice::Off, "off"),
        ]);

        let mut state = Self {
            format,
            sample_rate,
            bit_depth,
            dither,
            replaygain,
            field_focus: FormatField::Format,
            advanced_open: false,
        };
        state.apply_format_constraints();
        state
    }

    /// Recalculate which options are enabled based on the selected format.
    pub fn apply_format_constraints(&mut self) {
        let fmt = *self.format.selected_value();

        self.sample_rate.set_all_enabled(true);
        self.bit_depth.set_all_enabled(true);
        self.dither.set_all_enabled(true);

        match fmt {
            AudioFormat::Opus => {
                self.sample_rate.select_value(&48_000);
                for opt in &mut self.sample_rate.options {
                    opt.enabled = opt.value == 48_000;
                }
                self.bit_depth.set_all_enabled(false);
                self.dither.set_all_enabled(false);
            }
            AudioFormat::Aac => {
                self.bit_depth.set_all_enabled(false);
                self.dither.set_all_enabled(false);
                for opt in &mut self.sample_rate.options {
                    if opt.value > 192_000 {
                        opt.enabled = false;
                    }
                }
            }
            AudioFormat::Mp3 => {
                self.bit_depth.set_all_enabled(false);
                self.dither.set_all_enabled(false);
                for opt in &mut self.sample_rate.options {
                    if opt.value > 48_000 {
                        opt.enabled = false;
                    }
                }
            }
            AudioFormat::Flac => {
                // FLAC: up to 32-bit integer, no float
                self.bit_depth.set_enabled(&BitDepthChoice::Float32, false);
                self.bit_depth.set_enabled(&BitDepthChoice::Float64, false);
            }
            AudioFormat::Aiff => {
                // AIFF: up to 32-bit integer, no float (standard AIFF)
                self.bit_depth.set_enabled(&BitDepthChoice::Float32, false);
                self.bit_depth.set_enabled(&BitDepthChoice::Float64, false);
            }
            AudioFormat::Alac => {
                // ALAC: up to 32-bit integer, no float
                self.bit_depth.set_enabled(&BitDepthChoice::Float32, false);
                self.bit_depth.set_enabled(&BitDepthChoice::Float64, false);
            }
            AudioFormat::Wav | AudioFormat::WavPack => {
                // WAV and WavPack: support all depths including float
            }
        }

        // Always disabled — backend doesn't support 64-bit float yet
        self.bit_depth.set_enabled(&BitDepthChoice::Float64, false);

        self.clamp_disabled_selections();
    }

    fn clamp_disabled_selections(&mut self) {
        clamp_pill(&mut self.sample_rate);
        clamp_pill(&mut self.bit_depth);
        clamp_pill(&mut self.dither);
    }

    pub fn focused_pill_mut(&mut self) -> FocusedPill<'_> {
        match self.field_focus {
            FormatField::Format => FocusedPill::Format(&mut self.format),
            FormatField::SampleRate => FocusedPill::SampleRate(&mut self.sample_rate),
            FormatField::BitDepth => FocusedPill::BitDepth(&mut self.bit_depth),
            FormatField::Dither => FocusedPill::Dither(&mut self.dither),
            FormatField::ReplayGain => FocusedPill::ReplayGain(&mut self.replaygain),
        }
    }
}

fn clamp_pill<T: Clone + PartialEq>(pill: &mut PillState<T>) {
    if !pill.options[pill.selected].enabled {
        let len = pill.options.len();
        for i in 1..len {
            let idx = (pill.selected + i) % len;
            if pill.options[idx].enabled {
                pill.selected = idx;
                return;
            }
        }
    }
}

/// Enum to allow generic prev/next on whichever pill is focused
pub enum FocusedPill<'a> {
    Format(&'a mut PillState<AudioFormat>),
    SampleRate(&'a mut PillState<u32>),
    BitDepth(&'a mut PillState<BitDepthChoice>),
    Dither(&'a mut PillState<DitherType>),
    ReplayGain(&'a mut PillState<ReplayGainChoice>),
}

impl FocusedPill<'_> {
    pub fn select_next(&mut self) {
        match self {
            Self::Format(p) => p.select_next(),
            Self::SampleRate(p) => p.select_next(),
            Self::BitDepth(p) => p.select_next(),
            Self::Dither(p) => p.select_next(),
            Self::ReplayGain(p) => p.select_next(),
        }
    }

    pub fn select_prev(&mut self) {
        match self {
            Self::Format(p) => p.select_prev(),
            Self::SampleRate(p) => p.select_prev(),
            Self::BitDepth(p) => p.select_prev(),
            Self::Dither(p) => p.select_prev(),
            Self::ReplayGain(p) => p.select_prev(),
        }
    }
}

/// State for the metadata pane
#[derive(Debug, Clone, Default)]
pub struct MetadataState {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
    pub advanced_open: bool,
}

/// State for the output options pane
#[derive(Debug, Clone)]
pub struct OutputOptionsState {
    pub dest_path: Option<PathBuf>,
    pub folder_template: String,
    pub filename_template: String,
    pub merge: PillState<MergeMode>,
    pub field_focus: OutputOptionsField,
    pub advanced_open: bool,
}

impl OutputOptionsState {
    pub fn new() -> Self {
        let merge = PillState::new(vec![
            (MergeMode::MultiFile, "multi-file"),
            (MergeMode::SingleImage, "single image"),
        ]);

        Self {
            dest_path: None,
            folder_template: "%ARTIST%/%ALBUM% (%YEAR%)".to_string(),
            filename_template: "%TRACKNN% - %TITLE%.%EXT%".to_string(),
            merge,
            field_focus: OutputOptionsField::DestPath,
            advanced_open: false,
        }
    }
}

/// Full state for the convert screen
#[derive(Debug, Clone)]
pub struct ConvertState {
    pub source: SourceState,
    pub metadata: MetadataState,
    pub format: FormatState,
    pub output_options: OutputOptionsState,
    pub focus: ConvertFocus,
}

impl ConvertState {
    pub fn new() -> Self {
        Self {
            source: SourceState::default(),
            metadata: MetadataState::default(),
            format: FormatState::new(),
            output_options: OutputOptionsState::new(),
            focus: ConvertFocus::Source,
        }
    }
}

// ── Preset state ─────────────────────────────────────────────────────

/// State for preset management
#[derive(Debug, Clone)]
pub struct PresetState {
    pub active_preset: Option<String>,
    pub modified: bool,
    pub overlay_open: bool,
    pub overlay_list: Vec<String>,
    pub overlay_selected: usize,
    /// Text input for "save as new" within the overlay
    pub naming_input: Option<crate::tui::text_input::TextInputState>,
}

impl PresetState {
    /// Mark the preset as modified, but only if a preset is actually active.
    /// Changes without an active preset don't need tracking — there's nothing
    /// to compare against.
    pub fn mark_modified(&mut self) {
        if self.active_preset.is_some() {
            self.modified = true;
        }
    }
}

impl Default for PresetState {
    fn default() -> Self {
        Self {
            active_preset: None,
            modified: false,
            overlay_open: false,
            overlay_list: Vec::new(),
            overlay_selected: 0,
            naming_input: None,
        }
    }
}

// ── Queue screen state (kept from existing) ──────────────────────────

/// Focus area within the queue screen
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QueueFocus {
    FileList,
    ActionBar,
}

/// What the wizard is being used for
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WizardTarget {
    ConfigureAll,
    ConfigureSelected,
    ConfigureNew,
}

// ── Overlay dialogs ──────────────────────────────────────────────────

/// Active overlay dialog
#[derive(Debug, Clone)]
pub enum ActiveOverlay {
    None,
    Confirmation {
        message: String,
        action: ConfirmAction,
    },
    ErrorDetail {
        item_id: String,
        error: String,
    },
    ItemInfo {
        item: ConversionItem,
    },
    FileInput {
        input: crate::tui::text_input::TextInputState,
    },
    CommandInput {
        input: crate::tui::text_input::TextInputState,
    },
    TextEdit {
        input: crate::tui::text_input::TextInputState,
        target: TextEditTarget,
        label: String,
    },
}

/// Which field a TextEdit overlay is editing
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextEditTarget {
    DestPath,
    FolderTemplate,
    FilenameTemplate,
    MetaTitle,
    MetaArtist,
    MetaAlbum,
    MetaGenre,
    MetaYear,
}

/// What action a confirmation dialog will perform
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    RemoveSelected,
    ClearCompleted,
    StopAll,
    ClearQueue,
}

// ── Main application state ───────────────────────────────────────────

/// Main application state
pub struct AppState {
    pub config: TonepoetConfig,
    pub manager: ConversionManager,

    // Navigation
    pub current_screen: AppScreen,
    pub previous_screen: Option<AppScreen>,

    // Convert screen
    pub convert: ConvertState,
    pub preset: PresetState,

    // Browse screen state
    pub browse: crate::tui::browse::BrowseState,

    // Queue screen state
    pub queue_focus: QueueFocus,
    pub selected_index: usize,
    pub scroll_offset: usize,
    pub visible_height: usize,
    pub items_snapshot: Vec<ConversionItem>,
    pub button_map: ButtonRenderMap,

    // Wizard (when active)
    pub wizard: Option<tonepoet_wizard::SimpleWizard>,
    pub wizard_mouse_areas: Option<tonepoet_wizard::MouseAreas>,
    pub wizard_target: WizardTarget,

    // Overlays
    pub active_overlay: ActiveOverlay,

    // Status
    pub status_message: Option<(String, std::time::Instant)>,
    pub processing_active: bool,
    pub should_quit: bool,

    // Caches
    pub tool_check_cache: once_cell::sync::OnceCell<Vec<(String, String, bool)>>,
}

impl AppState {
    pub fn new(config: TonepoetConfig) -> Self {
        let conv_config = ConversionConfig {
            worker_count: config.conversion.worker_count,
            ..ConversionConfig::default()
        };
        let mut manager = ConversionManager::new(conv_config);

        // Load persisted queue if enabled
        if config.conversion.persist_queue {
            manager.load_persisted_queue();
        }

        let mut output_options = OutputOptionsState::new();
        output_options.dest_path = config.conversion.default_destination.clone();

        Self {
            config,
            manager,
            current_screen: AppScreen::Convert,
            previous_screen: None,
            convert: ConvertState {
                source: SourceState::default(),
                metadata: MetadataState::default(),
                format: FormatState::new(),
                output_options,
                focus: ConvertFocus::Source,
            },
            preset: PresetState::default(),
            browse: crate::tui::browse::BrowseState::new(),
            queue_focus: QueueFocus::FileList,
            selected_index: 0,
            scroll_offset: 0,
            visible_height: 0,
            items_snapshot: Vec::new(),
            button_map: ButtonRenderMap::new(),
            wizard: None,
            wizard_mouse_areas: None,
            wizard_target: WizardTarget::ConfigureAll,
            active_overlay: ActiveOverlay::None,
            status_message: None,
            processing_active: false,
            should_quit: false,
            tool_check_cache: once_cell::sync::OnceCell::new(),
        }
    }

    /// Set a status message that will auto-clear after 5 seconds
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), std::time::Instant::now()));
    }

    /// Clear expired status messages
    pub fn clear_expired_status(&mut self) {
        if let Some((_, created)) = &self.status_message {
            if created.elapsed() > std::time::Duration::from_secs(5) {
                self.status_message = None;
            }
        }
    }

    /// Refresh the items snapshot from the manager
    pub fn refresh_items(&mut self) {
        self.items_snapshot = self.manager.get_items_clone();
    }

    /// Ensure selected_index stays within bounds
    pub fn clamp_selection(&mut self) {
        if self.items_snapshot.is_empty() {
            self.selected_index = 0;
        } else if self.selected_index >= self.items_snapshot.len() {
            self.selected_index = self.items_snapshot.len() - 1;
        }
    }

    /// Scroll to keep the selected item visible
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

    /// Toggle selection on the currently highlighted item
    pub fn toggle_current_selection(&mut self) {
        if let Some(item) = self.items_snapshot.get(self.selected_index) {
            let item_id = item.id.clone();
            if let Ok(mut queue) = self.manager.queue.try_write() {
                if let Some(real_item) = queue.find_item_mut(&item_id) {
                    real_item.selected = !real_item.selected;
                }
            }
        }
    }
}
