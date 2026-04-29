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
    Browse,    // Tab 1 — default home; file browsing + selection
    Library,   // Tab 2 — placeholder
    Convert,   // Tab 3 — conversion settings / staging area for new batches
    Queue,     // Tab 4 — file queue
    Config,    // Tab 5 — settings
    Wizard,    // Full-screen overlay (not a tab)
}

impl AppScreen {
    /// Tab number (1-5), or None for overlays like Wizard.
    /// Order: Browse=1, Library=2, Convert=3, Queue=4, Config=5.
    /// Convert sits between Library and Queue because it's the conversion-
    /// settings staging area that committed items pass through on their way
    /// to the queue.
    pub fn tab_number(&self) -> Option<u8> {
        match self {
            Self::Browse => Some(1),
            Self::Library => Some(2),
            Self::Convert => Some(3),
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

    /// All tab screens in display order.
    pub fn tabs() -> &'static [AppScreen] {
        &[
            Self::Browse,
            Self::Library,
            Self::Convert,
            Self::Queue,
            Self::Config,
        ]
    }

    /// Map a `ui.default_screen` config string to an `AppScreen`.
    /// Case-insensitive; unknown values fall back to `Browse` (the default).
    pub fn from_config_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "library" => Self::Library,
            "convert" | "settings" | "conversion" => Self::Convert,
            "queue" => Self::Queue,
            "config" => Self::Config,
            _ => Self::Browse,
        }
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

/// Source state: empty, a single reviewed file, or a multi-file batch.
/// Phase 6d: replaces the flat `file_path / info / metadata / batch_queue`
/// fields with a proper type-safe enum so callers must explicitly handle
/// each mode.
#[derive(Debug, Clone)]
pub enum SourceMode {
    /// No source loaded yet.
    Empty,
    /// A single file loaded for review.
    Single {
        path: PathBuf,
        info: Option<SourceInfo>,
        metadata: SourceMetadata,
    },
    /// A multi-file batch loaded for review. The cursor indexes into
    /// `paths` for the "currently previewed" file, whose probe result
    /// lives in `cursor_info` / `cursor_metadata` (lazily filled in by
    /// `AudioProbeComplete` when the cursor moves).
    Batch {
        paths: Vec<PathBuf>,
        cursor: usize,
        cursor_info: Option<SourceInfo>,
        cursor_metadata: SourceMetadata,
        /// Sum of file sizes (cheap: `fs::metadata` per path).
        total_size: u64,
        /// Distinct parent-directory count — a rough "album" heuristic.
        album_count: usize,
        /// Format distribution based on file extension. Sorted descending
        /// by count. Cheap: no ffmpeg probe needed.
        format_histogram: Vec<(AudioFormat, usize)>,
    },
}

impl SourceMode {
    /// Construct from a vec of paths. Precomputes the batch summary
    /// (total size, album count, format histogram) synchronously — all
    /// from `fs::metadata` and file extensions, no ffmpeg probes.
    ///
    /// - `paths.len() == 0` → `Empty`
    /// - `paths.len() == 1` → `Single` with info/metadata empty (caller
    ///   populates via probe + read_metadata)
    /// - `paths.len() > 1` → `Batch` with precomputed summary
    pub fn from_paths(paths: Vec<PathBuf>) -> Self {
        match paths.len() {
            0 => Self::Empty,
            1 => Self::Single {
                path: paths
                    .into_iter()
                    .next()
                    .expect("len == 1 means one element"),
                info: None,
                metadata: SourceMetadata::default(),
            },
            _ => {
                let total_size: u64 = paths
                    .iter()
                    .filter_map(|p| std::fs::metadata(p).ok())
                    .map(|m| m.len())
                    .sum();
                let album_count = paths
                    .iter()
                    .filter_map(|p| p.parent())
                    .collect::<std::collections::HashSet<_>>()
                    .len();
                let format_histogram = compute_format_histogram(&paths);
                Self::Batch {
                    paths,
                    cursor: 0,
                    cursor_info: None,
                    cursor_metadata: SourceMetadata::default(),
                    total_size,
                    album_count,
                    format_histogram,
                }
            }
        }
    }

    /// True if no source is loaded.
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// True if this is a multi-file batch.
    pub fn is_batch(&self) -> bool {
        matches!(self, Self::Batch { .. })
    }

    /// All paths in this source (0 for Empty, 1 for Single, N for Batch).
    pub fn all_paths(&self) -> Vec<PathBuf> {
        match self {
            Self::Empty => Vec::new(),
            Self::Single { path, .. } => vec![path.clone()],
            Self::Batch { paths, .. } => paths.clone(),
        }
    }

    /// The currently previewed path (Single's path, or Batch's cursor).
    pub fn current_path(&self) -> Option<&PathBuf> {
        match self {
            Self::Empty => None,
            Self::Single { path, .. } => Some(path),
            Self::Batch { paths, cursor, .. } => paths.get(*cursor),
        }
    }

    /// The currently previewed `SourceInfo` (None if not yet probed).
    pub fn current_info(&self) -> Option<&SourceInfo> {
        match self {
            Self::Empty => None,
            Self::Single { info, .. } => info.as_ref(),
            Self::Batch { cursor_info, .. } => cursor_info.as_ref(),
        }
    }

    /// The currently previewed `SourceMetadata`. Returns an owned default
    /// for the Empty variant so the caller can always have something to
    /// display without extra matching.
    pub fn current_metadata(&self) -> SourceMetadata {
        match self {
            Self::Empty => SourceMetadata::default(),
            Self::Single { metadata, .. } => metadata.clone(),
            Self::Batch { cursor_metadata, .. } => cursor_metadata.clone(),
        }
    }

    /// Number of files (0/1/N for Empty/Single/Batch).
    pub fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Single { .. } => 1,
            Self::Batch { paths, .. } => paths.len(),
        }
    }
}

/// Cheap format detection from file extension — used for batch histograms
/// where probing each file with ffmpeg would be wasteful.
fn compute_format_histogram(paths: &[PathBuf]) -> Vec<(AudioFormat, usize)> {
    use std::collections::HashMap;
    let mut counts: HashMap<AudioFormat, usize> = HashMap::new();
    for p in paths {
        if let Some(fmt) = detect_format_from_extension(p) {
            *counts.entry(fmt).or_insert(0) += 1;
        }
    }
    let mut vec: Vec<(AudioFormat, usize)> = counts.into_iter().collect();
    vec.sort_by(|a, b| b.1.cmp(&a.1));
    vec
}

/// Map a file extension to an `AudioFormat`, if recognised. Used only
/// by the cheap batch format histogram — the real conversion pipeline
/// uses `FormatDetector::detect` which does magic-byte sniffing and
/// supports a wider range of formats.
///
/// Mappings pick the closest existing `AudioFormat` variant; some are
/// best-effort (e.g. `.ogg` is typically Vorbis but could also be Opus
/// or FLAC — we map to Opus as the most common modern case). Extensions
/// without a reasonable variant (dsf/dff for DSD, ape/wma/shn/tta/amr
/// for formats we don't represent) return `None` and fall through to
/// "(no recognised audio extensions)" if the whole batch is unrecognised.
fn detect_format_from_extension(path: &std::path::Path) -> Option<AudioFormat> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    match ext.to_lowercase().as_str() {
        // FLAC — lossless
        "flac" => Some(AudioFormat::Flac),
        // WAV family (includes Wave64 and RF64 >4GB variants)
        "wav" | "w64" | "rf64" | "bwf" => Some(AudioFormat::Wav),
        // AIFF / AIFF-C
        "aiff" | "aif" | "aifc" => Some(AudioFormat::Aiff),
        // WavPack
        "wv" => Some(AudioFormat::WavPack),
        // MP3
        "mp3" => Some(AudioFormat::Mp3),
        // AAC family (m4a, adts .aac, .mp4 audio)
        "m4a" | "aac" | "mp4" | "m4b" | "m4r" => Some(AudioFormat::Aac),
        // ALAC — typically carried in m4a but sometimes standalone
        "alac" | "caf" => Some(AudioFormat::Alac),
        // Opus (.opus is unambiguous; .oga is Ogg Opus; .ogg is
        // ambiguous but maps here as best-effort)
        "opus" | "oga" | "ogg" => Some(AudioFormat::Opus),
        _ => None,
    }
}

/// State for the source pane.
#[derive(Debug, Clone)]
pub struct SourceState {
    pub mode: SourceMode,
    pub advanced_open: bool,
    /// Path of the batch cursor probe currently in flight. Prevents
    /// duplicate spawns when the user holds an arrow key.
    pub batch_probe_pending: Option<PathBuf>,
    /// Debounce for batch cursor probes: (target_path, fire_at).
    /// Set by move_batch_cursor, checked by the event loop tick.
    /// Prevents N probes during rapid navigation — only fires once
    /// the cursor has been still for 150ms.
    pub batch_probe_debounce: Option<(PathBuf, std::time::Instant)>,
}

impl Default for SourceState {
    fn default() -> Self {
        Self {
            mode: SourceMode::Empty,
            advanced_open: false,
            batch_probe_pending: None,
            batch_probe_debounce: None,
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
        /// Active tab-completion state. `None` on initial open and after
        /// any non-Tab keypress (so the next Tab re-parses). `Some` while
        /// cycling through candidates.
        completion: Option<CompletionState>,
    },
    TextEdit {
        input: crate::tui::text_input::TextInputState,
        target: TextEditTarget,
        label: String,
    },
    /// Expand overlay for a multi-file batch: full path list with
    /// keyboard navigation (↑/↓ move cursor, `d` removes, Enter/Esc
    /// closes). Mirrors `source.mode.Batch.cursor` while open.
    ///
    /// `scroll` is the top row of the visible slice — persisted so
    /// that the cursor only scrolls the list when it exits the
    /// visible range (vim-smooth behaviour), rather than snapping to
    /// the bottom on every keystroke. The renderer clamps defensively
    /// so the cursor is always in view even if this value is stale.
    BatchList {
        scroll: usize,
    },
    /// Context menu triggered by right-click or `m` keybinding.
    /// Two-level side-by-side model (hexload-tui pattern): the parent
    /// menu is always visible; when the cursor is on a `Submenu` entry,
    /// its children appear as a second panel to the right. Both are
    /// visible simultaneously.
    ContextMenu {
        entries: Vec<crate::tui::context_menu::ContextMenuEntry>,
        selected: usize,
        origin: (u16, u16),
        /// Child submenu (populated when the selected parent entry is
        /// a `Submenu` variant). Cleared when cursor moves to a
        /// non-Submenu item.
        submenu_entries: Vec<crate::tui::context_menu::ContextMenuEntry>,
        submenu_selected: usize,
        show_submenu: bool,
        /// True when keyboard focus is in the submenu (Right opened it).
        /// False when focus is in the parent menu.
        focus_submenu: bool,
    },
    /// Bulk rename wizard overlay. Boxed because the state is large.
    BulkRename(Box<BulkRenameState>),
    /// Analysis results overlay showing DR, peak, RMS, etc.
    Analysis {
        scroll: usize,
    },
    /// Help overlay showing keybindings for the current screen.
    Help {
        /// The screen that was active when help was opened.
        screen: AppScreen,
        /// Scroll offset in the help content.
        scroll: usize,
    },
    /// Full metadata tag editor overlay.
    MetadataEditor(Box<MetadataEditorState>),
    /// Verify results overlay showing pass/fail per file.
    Verify {
        scroll: usize,
    },
    /// Bit-compare results overlay showing identical/differ per pair.
    BitCompare {
        scroll: usize,
    },
    /// Pre-emphasis detection results overlay.
    Preemphasis {
        scroll: usize,
    },
    /// CUE import review: shows proposed changes before merging into
    /// the metadata editor. The editor state is parked in
    /// `AppState::pending_metadata_editor` during review.
    CueImportReview {
        changes: Vec<CueImportChange>,
        scroll: usize,
    },
    /// GNUDB match selection overlay (when multiple matches are returned).
    GnudbSelect {
        matches: Vec<crate::tui::gnudb::GnudbMatch>,
        selected: usize,
        scroll: usize,
        /// Audio file paths for populating the metadata editor after selection.
        paths: Vec<std::path::PathBuf>,
    },
    /// GNUDB review overlay — editable preview of GNUDB tags before
    /// accepting into the metadata editor.
    GnudbReview(Box<GnudbReviewState>),
    /// AccurateRip verification results overlay (supports multi-disc).
    AccurateRipVerify(Box<ArVerifyState>),
}

/// State for the AccurateRip verification overlay.
/// Supports multi-disc: each page is one disc's results.
#[derive(Debug, Clone)]
pub struct ArVerifyState {
    /// Per-disc result pages. Single-disc albums have one page.
    pub pages: Vec<ArVerifyPage>,
    /// Active page index (0-based).
    pub active_page: usize,
    /// Scroll offset within the active page.
    pub scroll: usize,
}

/// A single page (disc) in the AccurateRip verification overlay.
#[derive(Debug, Clone)]
pub struct ArVerifyPage {
    /// Disc label (e.g., "disc 01"). Empty for single-disc albums.
    pub label: String,
    /// Verification result for this disc.
    pub result: crate::tui::accuraterip::ArVerifyResult,
}

/// State for the GNUDB review overlay.
#[derive(Debug, Clone)]
pub struct GnudbReviewState {
    /// Per-disc pages. Single-disc albums have one page.
    pub pages: Vec<GnudbReviewPage>,
    /// Active page index (0-based).
    pub active_page: usize,
    /// Current cursor position in active page's rows.
    pub cursor: usize,
    /// Scroll offset.
    pub scroll: usize,
    /// Active inline edit (if any).
    pub edit_input: Option<crate::tui::text_input::TextInputState>,
    /// Last click for double-click detection: (row_index, timestamp).
    pub last_click: Option<(usize, std::time::Instant)>,
    /// ALL audio file paths across ALL discs.
    pub paths: Vec<std::path::PathBuf>,
    /// Original match list for "back" navigation (None for single-match queries).
    pub origin_matches: Option<Vec<crate::tui::gnudb::GnudbMatch>>,
}

/// A single page (disc) in the GNUDB review overlay.
#[derive(Debug, Clone)]
pub struct GnudbReviewPage {
    /// Disc label (e.g., "disc 01"). Empty for single-disc albums.
    pub label: String,
    /// Album title for this disc (editable).
    pub album: String,
    /// Release year (editable).
    pub year: String,
    /// Genre (editable).
    pub genre: String,
    /// Tracks on this disc.
    pub tracks: Vec<GnudbReviewTrack>,
    /// Flattened row map for this page.
    pub rows: Vec<GnudbRowKind>,
}

/// A track within the GNUDB review.
#[derive(Debug, Clone)]
pub struct GnudbReviewTrack {
    /// Track title (editable).
    pub title: String,
    /// Track artist (editable).
    pub artist: String,
    /// Track number (1-based).
    pub track_number: u32,
    /// Index into `GnudbReviewState.paths` for this track's file.
    pub file_index: usize,
}

/// What each row in a GNUDB review page represents.
#[derive(Debug, Clone)]
pub enum GnudbRowKind {
    /// Album-level field: "Album", "Year", or "Genre".
    AlbumField(&'static str),
    /// Per-track header line (non-selectable, cursor skips).
    TrackHeader { track_idx: usize },
    /// Per-track editable field: "Title" or "Artist".
    TrackField { track_idx: usize, field: &'static str },
}

/// A single proposed change from a CUE import.
#[derive(Debug, Clone)]
pub struct CueImportChange {
    /// Index into the metadata editor's file list.
    pub file_index: usize,
    /// Display filename for the change.
    pub filename: String,
    /// Tag field name (e.g. "TITLE", "ARTIST").
    pub field: String,
    /// Current tag value (empty string if absent).
    pub old_value: String,
    /// New value from the edited CUE sheet.
    pub new_value: String,
}

/// Phase of the metadata editor workflow.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataEditorPhase {
    /// Browsing / editing fields.
    Editing,
    /// Currently editing a field's value inline.
    InlineEdit,
    /// Entering a new custom field name.
    AddingKey,
    /// Per-file detail overlay for a mixed field.
    DetailEdit,
    /// Saving changes to disk.
    Saving,
}

/// State for the metadata editor overlay.
#[derive(Debug, Clone)]
pub struct MetadataEditorState {
    /// Files being edited.
    pub paths: Vec<std::path::PathBuf>,
    /// All tag entries (ordered for display).
    pub entries: Vec<crate::tui::probe::TagEntry>,
    /// Cursor position in the entries list.
    pub cursor: usize,
    /// Scroll offset for the visible window.
    pub scroll: usize,
    /// Last left-click: (row_index, timestamp) for double-click detection.
    pub last_click: Option<(usize, std::time::Instant)>,
    /// Text input for inline field editing.
    pub edit_input: Option<crate::tui::text_input::TextInputState>,
    /// Text input for adding a new field key.
    pub add_key_input: Option<crate::tui::text_input::TextInputState>,
    /// Current phase.
    pub phase: MetadataEditorPhase,
    /// Whether any entries have been modified.
    pub dirty: bool,
    /// Entries marked for deletion (by index). Tracked separately so
    /// the user can see them struck through before saving.
    pub deleted: Vec<usize>,
    /// Per-file context labels for the detail overlay (e.g., "01 filename").
    pub file_labels: Vec<String>,
    /// Which entry index is being detail-edited.
    pub detail_field_idx: usize,
    /// Cursor within the detail overlay.
    pub detail_cursor: usize,
    /// Scroll offset in the detail overlay.
    pub detail_scroll: usize,
    /// Inline edit within the detail overlay.
    pub detail_edit: Option<crate::tui::text_input::TextInputState>,
}

/// Focus area within the bulk rename overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BulkRenameFocus {
    /// Editing the template input field.
    Template,
    /// Navigating the preview list.
    List,
}

/// State for the bulk rename wizard overlay.
#[derive(Debug, Clone)]
pub struct BulkRenameState {
    /// Source file paths (absolute), index-aligned with metadata/stems/extensions.
    pub sources: Vec<std::path::PathBuf>,
    /// Cached metadata per source.
    pub metadata: Vec<SourceMetadata>,
    /// Original filename stems (no extension).
    pub original_stems: Vec<String>,
    /// File extensions (without dot).
    pub extensions: Vec<String>,
    /// Template input field.
    pub template_input: crate::tui::text_input::TextInputState,
    /// The current rename plan (rebuilt on every template change).
    pub plan: crate::tui::rename_plan::RenamePlan,
    /// Currently selected row in the preview list.
    pub selected: usize,
    /// Scroll offset.
    pub scroll: usize,
    /// Which part has focus.
    pub focus: BulkRenameFocus,
}

impl BulkRenameState {
    /// Create a new bulk rename state from source files + their metadata.
    pub fn new(
        base_dir: std::path::PathBuf,
        sources: Vec<std::path::PathBuf>,
        metadata: Vec<SourceMetadata>,
    ) -> Self {
        let original_stems: Vec<String> = sources
            .iter()
            .map(|p| {
                p.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            })
            .collect();
        let extensions: Vec<String> = sources
            .iter()
            .map(|p| {
                p.extension()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            })
            .collect();
        let default_template = "%NN% - %TITLE%".to_string();
        let plan = crate::tui::rename_plan::RenamePlan::new(
            base_dir,
            Vec::new(),
        );
        let mut state = Self {
            sources,
            metadata,
            original_stems,
            extensions,
            template_input: crate::tui::text_input::TextInputState::new(default_template),
            plan,
            selected: 0,
            scroll: 0,
            focus: BulkRenameFocus::Template,
        };
        state.rebuild_plan();
        state
    }

    /// Rebuild the rename plan from the current template + metadata.
    pub fn rebuild_plan(&mut self) {
        let template = &self.template_input.text;
        let has_ext_placeholder = template.contains("%EXT%");
        let base_dir = self.plan.base_dir.clone();
        let items: Vec<(std::path::PathBuf, String)> = self
            .sources
            .iter()
            .enumerate()
            .map(|(i, src)| {
                let resolved = crate::tui::rename_template::resolve_template(
                    template,
                    &self.metadata[i],
                    &self.original_stems[i],
                    &self.extensions[i],
                );
                // Append extension if the template doesn't include %EXT%.
                let with_ext = if !has_ext_placeholder && !self.extensions[i].is_empty() {
                    format!("{}.{}", resolved, self.extensions[i])
                } else {
                    resolved
                };
                let sanitized = match crate::tui::rename_plan::sanitize_path(&with_ext) {
                    Ok(s) => s,
                    Err(_) => {
                        // Sanitization rejected the path (traversal, empty, etc.).
                        // Use the original filename so the op becomes a no-op
                        // (source == target → Skipped by validate_plan). Never
                        // fall back to the raw unsanitized string — it could
                        // contain `..` traversal.
                        src.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default()
                    }
                };
                (src.clone(), sanitized)
            })
            .collect();
        self.plan = crate::tui::rename_plan::RenamePlan::new(base_dir, items);
        crate::tui::rename_plan::validate_plan(&mut self.plan);
    }
}

/// Tab-completion state for the command input overlay.
/// Stores the candidate list, the current cycle index, and the byte
/// offset in the input text where the completed word begins (so cycles
/// replace only the prefix, not the whole input).
#[derive(Debug, Clone)]
pub struct CompletionState {
    pub candidates: Vec<String>,
    pub cursor: usize,
    pub prefix_start: usize,
}

/// Which field a TextEdit overlay is editing
#[derive(Debug, Clone, PartialEq)]
pub enum TextEditTarget {
    DestPath,
    FolderTemplate,
    FilenameTemplate,
    MetaTitle,
    MetaArtist,
    MetaAlbum,
    MetaGenre,
    MetaYear,
    /// Rename a browse entry. Carries the original path (not index) so a
    /// directory refresh between open and commit can't corrupt the target.
    BrowseRename(std::path::PathBuf),
    /// Edit a metadata tag on an audio file in the Browse info pane.
    /// Carries the file path + which field to write via lofty.
    BrowseMetadata {
        path: std::path::PathBuf,
        field: crate::tui::probe::MetadataField,
    },
    /// Copy selected files to the entered destination path.
    BrowseCopy {
        sources: Vec<std::path::PathBuf>,
        force: bool,
    },
    /// Move selected files to the entered destination path. Falls back
    /// to copy+delete across filesystems (ACID: copy first, verify
    /// size, then delete original).
    BrowseMove {
        sources: Vec<std::path::PathBuf>,
        force: bool,
    },
    /// Edit a single line in the bulk rename preview. The full
    /// BulkRenameState is parked in `AppState::pending_bulk_rename`
    /// while the TextEdit is open; the index identifies which op to
    /// update on commit.
    BulkRenameLine(usize),
    /// Save the current rename template. Carries the template string;
    /// the TextEdit input is the user-chosen name for the template.
    /// BulkRenameState parked in `pending_bulk_rename`.
    SaveRenameTemplate(String),
    /// Add a new password to the keychain. The TextEdit input is the
    /// password itself.
    KeychainAdd,
    /// Set an archive password for the currently selected archive in Browse.
    /// The TextEdit input is the password. On commit, it's stored as a
    /// session override and added to the keychain.
    ArchivePassword(std::path::PathBuf),
}

/// State for the password keychain section on the Config screen.
#[derive(Debug, Clone)]
pub struct KeychainState {
    /// Cached password list (loaded on first visit to Config screen).
    pub passwords: Vec<String>,
    /// Selected index in the password list.
    pub selected: usize,
    /// Whether passwords are shown in cleartext or masked.
    pub reveal: bool,
    /// Whether the keychain section is focused (vs the settings section).
    pub focused: bool,
    /// Whether passwords have been loaded from disk.
    pub loaded: bool,
}

impl Default for KeychainState {
    fn default() -> Self {
        Self {
            passwords: Vec::new(),
            selected: 0,
            reveal: false,
            focused: false,
            loaded: false,
        }
    }
}

impl KeychainState {
    /// Load passwords from disk if not already loaded.
    pub fn ensure_loaded(&mut self) {
        if !self.loaded {
            self.passwords = crate::tui::keychain::load_keychain();
            self.loaded = true;
        }
    }

    /// Reload from disk (e.g., after add/remove).
    pub fn reload(&mut self) {
        self.passwords = crate::tui::keychain::load_keychain();
        self.loaded = true;
        if self.selected >= self.passwords.len() && !self.passwords.is_empty() {
            self.selected = self.passwords.len() - 1;
        }
    }
}

/// What action a confirmation dialog will perform
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    RemoveSelected,
    ClearCompleted,
    StopAll,
    ClearQueue,
    /// Move the given paths to the system trash (XDG Trash / Finder Trash).
    TrashSelection(Vec<PathBuf>),
}

// ── Main application state ───────────────────────────────────────────

/// Main application state
pub struct AppState {
    pub config: TonepoetConfig,
    pub manager: ConversionManager,
    pub db: crate::db::Database,

    /// The button currently under the mouse cursor (updated on MouseEventKind::Moved).
    /// Renderers check this to apply hover highlighting.
    pub hover_target: Option<crate::tui::button_map::TuiButton>,

    /// Analysis results from the last :analyze command. Displayed in an
    /// overlay when the analysis completes.
    pub analysis_results: Vec<crate::tui::analyze::AnalysisResult>,

    /// Number of analysis tasks currently in flight. While > 0, the
    /// status bar shows a persistent "Analyzing..." message.
    pub analysis_pending: usize,

    /// Verify results from the last :verify command.
    pub verify_results: Vec<crate::tui::verify::VerifyResult>,

    /// Number of verify tasks currently in flight.
    pub verify_pending: usize,

    /// Pre-emphasis detection results.
    pub preemph_results: Vec<crate::tui::preemphasis::PreemphasisResult>,

    /// Number of pre-emphasis detection tasks in flight.
    pub preemph_pending: usize,

    /// Reference paths for bit-compare (marked by user, persists until cleared).
    pub compare_reference: Vec<std::path::PathBuf>,

    /// Bit-compare results from the last comparison.
    pub compare_results: Vec<crate::tui::bit_compare::CompareResult>,

    /// Number of compare tasks currently in flight.
    pub compare_pending: usize,

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

    /// Parked BulkRenameState while a per-line TextEdit is open.
    /// Set when `e` is pressed on a BulkRename row; consumed when
    /// the TextEdit commits or is cancelled.
    pub pending_bulk_rename: Option<Box<BulkRenameState>>,

    /// Parked MetadataEditorState while command mode or CUE import
    /// review is open. Set when `:` is pressed in the metadata editor;
    /// restored after the command executes or review completes.
    pub pending_metadata_editor: Option<Box<MetadataEditorState>>,

    // Status
    pub status_message: Option<(String, std::time::Instant)>,
    pub processing_active: bool,
    pub should_quit: bool,

    /// Last browse-entry click: (entry_path, click_time). Used for double-click detection.
    /// Path-based rather than index-based so directory refreshes / sort changes between
    /// clicks don't trigger false double-clicks on different entries.
    pub last_browse_click: Option<(std::path::PathBuf, std::time::Instant)>,

    /// Pending rename action: a same-path click outside the double-click window
    /// schedules a rename for `deadline`. A subsequent click before the deadline
    /// cancels the pending rename (e.g. to allow the second click to complete a
    /// double-click). When the deadline passes without cancellation, the event
    /// loop tick fires the rename overlay. Matches Windows/macOS "rename-on-
    /// click-after-pause, but double-click-to-open preempts" semantics.
    pub pending_browse_rename: Option<(std::path::PathBuf, std::time::Instant)>,

    /// Recent files list + overlay state (persisted to ~/.cache/tonepoet/recent.json).
    pub recent: crate::tui::recent_files::RecentFilesState,

    /// Bookmarks list + overlay state (persisted to ~/.config/tonepoet/bookmarks.toml).
    pub bookmarks: crate::tui::bookmarks::BookmarksState,

    /// Password keychain state for the Config screen.
    pub keychain: KeychainState,

    /// Session-level archive password overrides (archive path → password).
    /// Set via the `:password` command or interactive prompt. Takes
    /// priority over keychain MRU when committing archives.
    pub archive_passwords: std::collections::HashMap<std::path::PathBuf, String>,

    // Caches
    pub tool_check_cache: once_cell::sync::OnceCell<Vec<(String, String, bool)>>,
}

impl AppState {
    pub fn new(config: TonepoetConfig) -> Self {
        // Open the SQLite database FIRST — needed for queue load + other init.
        let db = match crate::db::Database::open() {
            Ok(db) => {
                // Prune stale search tag cache entries (>30 days old).
                db.prune_search_tag_cache(30);
                db
            }
            Err(e) => {
                log::error!("Failed to open database: {}. Using in-memory fallback.", e);
                crate::db::Database::open_memory()
                    .expect("in-memory DB should never fail")
            }
        };

        let conv_config = ConversionConfig {
            worker_count: config.conversion.worker_count,
            ..ConversionConfig::default()
        };
        let mut manager = ConversionManager::new(conv_config);

        // Load persisted queue: try SQLite first, fall back to JSON import.
        if config.conversion.persist_queue {
            let db_items = db.load_queue_items();
            if !db_items.is_empty() {
                if let Ok(mut q) = manager.queue.try_write() {
                    for item in db_items {
                        q.items_mut().push_back(item);
                    }
                }
            } else {
                // SQLite empty — try importing from JSON (first-run migration).
                manager.load_persisted_queue();
                // Sync the imported items to SQLite.
                if let Ok(q) = manager.queue.try_read() {
                    let items: Vec<&crate::convert::ConversionItem> = q.all_items();
                    let _ = db.sync_queue(&items);
                }
            }
        }

        let mut output_options = OutputOptionsState::new();
        output_options.dest_path = config.conversion.default_destination.clone();

        let initial_screen = AppScreen::from_config_name(&config.ui.default_screen);

        // Load recent files + bookmarks from DB.
        let recent = crate::tui::recent_files::RecentFilesState::load_from_db(&db);
        let bookmarks = crate::tui::bookmarks::BookmarksState::load_from_db(&db);
        // Import TOML presets into DB on first run.
        crate::tui::presets::import_presets_to_db(&db);

        Self {
            config,
            manager,
            db,
            current_screen: initial_screen,
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
            pending_bulk_rename: None,
            pending_metadata_editor: None,
            status_message: None,
            processing_active: false,
            should_quit: false,
            last_browse_click: None,
            pending_browse_rename: None,
            recent,
            bookmarks,
            keychain: KeychainState::default(),
            archive_passwords: std::collections::HashMap::new(),
            hover_target: None,
            analysis_results: Vec::new(),
            analysis_pending: 0,
            verify_results: Vec::new(),
            verify_pending: 0,
            preemph_results: Vec::new(),
            preemph_pending: 0,
            compare_reference: Vec::new(),
            compare_results: Vec::new(),
            compare_pending: 0,
            tool_check_cache: once_cell::sync::OnceCell::new(),
        }
    }

    /// Set a status message that will auto-clear after 5 seconds
    /// Save the conversion queue to both JSON (legacy) and SQLite.
    pub fn save_queue(&mut self) {
        if !self.config.conversion.persist_queue {
            return;
        }
        // Legacy JSON save (kept for backward compat during migration).
        self.manager.save_queue(true).ok();
        // SQLite sync (ACID, transactional).
        if let Ok(q) = self.manager.queue.try_read() {
            let items: Vec<&crate::convert::ConversionItem> = q.all_items()
                .into_iter()
                .filter(|item| {
                    !matches!(
                        item.status,
                        crate::convert::ConversionStatus::Processing { .. }
                            | crate::convert::ConversionStatus::Cancelled
                    )
                })
                .collect();
            let _ = self.db.sync_queue(&items);
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), std::time::Instant::now()));
    }

    /// Clear expired status messages. While analysis is in flight,
    /// shows a persistent "Analyzing..." message.
    pub fn clear_expired_status(&mut self) {
        if self.analysis_pending > 0 {
            let pending = self.analysis_pending;
            let done = self.analysis_results.len();
            self.status_message = Some((
                format!("Analyzing... ({}/{})", done, done + pending),
                std::time::Instant::now(),
            ));
            return;
        }
        if self.verify_pending > 0 {
            let pending = self.verify_pending;
            let done = self.verify_results.len();
            self.status_message = Some((
                format!("Verifying... ({}/{})", done, done + pending),
                std::time::Instant::now(),
            ));
            return;
        }
        if self.compare_pending > 0 {
            let pending = self.compare_pending;
            let done = self.compare_results.len();
            self.status_message = Some((
                format!("Comparing... ({}/{})", done, done + pending),
                std::time::Instant::now(),
            ));
            return;
        }
        if self.preemph_pending > 0 {
            let pending = self.preemph_pending;
            let done = self.preemph_results.len();
            self.status_message = Some((
                format!("Detecting pre-emphasis... ({}/{})", done, done + pending),
                std::time::Instant::now(),
            ));
            return;
        }
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

    /// Phase 6f: seed the Convert screen from CLI `tonepoet tui <paths>`
    /// invocation. Probes the first file, builds `SourceMode::Single` or
    /// `SourceMode::Batch` from the paths, populates the editable metadata
    /// pane from the first file's tags, and lands on the Convert screen
    /// instead of the configured default screen. Routes through Convert
    /// for review — no back door to the queue.
    ///
    /// Invalid paths (missing, directories, unreadable) are logged via
    /// `log::warn` and skipped. If all paths are invalid the method is
    /// a no-op beyond setting a status message.
    pub fn seed_from_cli_paths(&mut self, paths: Vec<PathBuf>) {
        let original_count = paths.len();
        let valid: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| {
                if !p.exists() {
                    log::warn!("cli: path does not exist: {}", p.display());
                    return false;
                }
                if p.is_dir() {
                    log::warn!(
                        "cli: directories not supported in TUI mode — use `tonepoet convert <dir>` or navigate via `:cd` on the Browse screen: {}",
                        p.display()
                    );
                    return false;
                }
                true
            })
            .collect();

        let valid_count = valid.len();
        if valid.is_empty() {
            if original_count > 0 {
                self.set_status(format!(
                    "cli: {} invalid path(s) skipped; see log",
                    original_count
                ));
            }
            return;
        }

        let first = valid[0].clone();
        let info = match crate::tui::probe::probe_audio(&first) {
            Ok(i) => Some(i),
            Err(e) => {
                log::warn!("cli: probe failed for {}: {}", first.display(), e);
                None
            }
        };
        let metadata = crate::tui::probe::read_metadata(&first).unwrap_or_default();

        // Populate the editable metadata pane from the first file's tags.
        self.convert.metadata.title = metadata.title.clone();
        self.convert.metadata.artist = metadata.artist.clone();
        self.convert.metadata.album = metadata.album.clone();
        self.convert.metadata.genre = metadata.genre.clone();
        self.convert.metadata.year = metadata.year.clone();

        // Build the mode (Single for 1 file, Batch for N) and populate
        // first-file probe/metadata in the appropriate variant.
        let mut mode = SourceMode::from_paths(valid);
        match &mut mode {
            SourceMode::Single { info: slot, metadata: meta_slot, .. } => {
                *slot = info;
                *meta_slot = metadata;
            }
            SourceMode::Batch { cursor_info, cursor_metadata, .. } => {
                *cursor_info = info;
                *cursor_metadata = metadata;
            }
            SourceMode::Empty => {
                // Unreachable — valid.is_empty() check guards against 0 paths.
            }
        }
        self.convert.source.mode = mode;

        // Record the first file in the recent-files history.
        self.recent.record_use_with_db(&first, &self.db);

        // Override the configured default screen — CLI file args always
        // land on Convert. Esc is a no-op (previous_screen stays None)
        // since this is a "permanent load" intent, not a cancelable
        // review. Consistent with `:e`, Browse Enter, and recent-files
        // load paths.
        self.current_screen = AppScreen::Convert;

        // Surface how many paths were filtered out so the user knows
        // something was skipped without having to tail the log file.
        let skipped = original_count.saturating_sub(valid_count);
        let skipped_suffix = if skipped > 0 {
            format!(" ({} skipped, see log)", skipped)
        } else {
            String::new()
        };
        let status = if valid_count == 1 {
            format!(
                "loaded {}{} from cli — review, then :commit or :Commit",
                first.file_name().unwrap_or_default().to_string_lossy(),
                skipped_suffix,
            )
        } else {
            format!(
                "loaded batch of {} files{} from cli — review, then :commit or :Commit",
                valid_count, skipped_suffix,
            )
        };
        self.set_status(status);
    }
}
