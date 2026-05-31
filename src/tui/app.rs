//! Application state for the standalone TUI

use std::path::PathBuf;

use crate::config::TonepoetConfig;
use crate::convert::formats::AudioFormat;
use crate::convert::simple_wizard::DitherType;
use tonepoet_pipeline::enums::{DsdFilterPreset, DsdNoiseShaper, ModulatorOrder};
use crate::convert::{ConversionConfig, ConversionItem, ConversionManager};
use crate::tui::button_map::ButtonRenderMap;
use crate::tui::pill::PillState;
use crate::tui::probe::{SourceInfo, SourceMetadata};

// ── Screen / tab navigation ──────────────────────────────────────────

/// Which screen is currently displayed
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppScreen {
    Browse,  // Tab 1 — default home; file browsing + selection
    Library, // Tab 2 — placeholder
    Convert, // Tab 3 — conversion settings / staging area for new batches
    Queue,   // Tab 4 — file queue
    Config,  // Tab 5 — settings
    Wizard,  // Full-screen overlay (not a tab)
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


/// Which field is focused in the format-specific settings overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FormatSettingsFocus {
    // FLAC
    Compression,
    Verify,
    Md5,
    // AAC
    AacProfile,
    AacQuality,
    AacBitrate,
}

/// Format-specific overlay state, keyed by codec.
#[derive(Debug, Clone)]
pub enum FormatSettingsKind {
    Flac {
        compression_input: crate::tui::text_input::TextInputState,
        verify: bool,
        md5: bool,
    },
    Aac {
        profile: tonepoet_pipeline::enums::AacProfile,
        quality_preset: Option<usize>,
        bitrate_input: crate::tui::text_input::TextInputState,
    },
}

/// Convert screen layout mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConvertLayout {
    /// All four panes use their standard fixed heights.
    Default,
    /// One pane is maximized; the other panes collapse to title bars.
    Maximized(ConvertFocus),
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
/// PCM resampler preference exposed in the format pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResamplerChoice {
    Sox,
    Ssrc,
    Soxr,
}

/// DSD conversion preset exposed in the format pane.
/// Kept as a local UI enum so labels can stay stable even if pipeline names evolve.
pub type DsdConversionPreset = DsdFilterPreset;

impl ResamplerChoice {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sox => "sox",
            Self::Ssrc => "ssrc",
            Self::Soxr => "soxr",
        }
    }
}

fn is_dsd_format(fmt: AudioFormat) -> bool {
    matches!(fmt, AudioFormat::Dsf | AudioFormat::Dff)
}



/// Merge mode for the output options pane
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MergeMode {
    MultiFile,
    SingleImage,
}

/// A track entry from a parsed multi-track source (SACD TOC or CUE sheet).
#[derive(Debug, Clone)]
pub struct MultiTrackEntry {
    pub number: u32,
    pub title: Option<String>,
    pub performer: Option<String>,
    pub duration_display: Option<String>,
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
    /// A multi-track source (SACD ISO, CUE+image) loaded for review.
    /// The track list comes from TOC/CUE parsing, not audio probing.
    MultiTrack {
        path: PathBuf,
        info: Option<SourceInfo>,
        metadata: SourceMetadata,
        tracks: Vec<MultiTrackEntry>,
        /// "Stereo" / "Multichannel" for SACD, None for CUE.
        area_label: Option<String>,
        album_title: Option<String>,
        album_artist: Option<String>,
        scroll: usize,
        /// Cursor position in the track list (0-based).
        cursor: usize,
        /// Per-track selection (all true initially).
        selected: Vec<bool>,
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
            1 => {
                let path = paths
                    .into_iter()
                    .next()
                    .expect("len == 1 means one element");
                Self::from_single(path, None, SourceMetadata::default())
            }
            _ => {
                let mut paths = paths;
                paths.sort();
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

    /// True if this is a multi-track source (SACD ISO, CUE+image).
    pub fn is_multi_track(&self) -> bool {
        matches!(self, Self::MultiTrack { .. })
    }

    /// All paths in this source (0 for Empty, 1 for Single/MultiTrack, N for Batch).
    pub fn all_paths(&self) -> Vec<PathBuf> {
        match self {
            Self::Empty => Vec::new(),
            Self::Single { path, .. } | Self::MultiTrack { path, .. } => vec![path.clone()],
            Self::Batch { paths, .. } => paths.clone(),
        }
    }

    /// The currently previewed path (Single/MultiTrack path, or Batch cursor).
    pub fn current_path(&self) -> Option<&PathBuf> {
        match self {
            Self::Empty => None,
            Self::Single { path, .. } | Self::MultiTrack { path, .. } => Some(path),
            Self::Batch { paths, cursor, .. } => paths.get(*cursor),
        }
    }

    /// The currently previewed `SourceInfo` (None if not yet probed).
    pub fn current_info(&self) -> Option<&SourceInfo> {
        match self {
            Self::Empty => None,
            Self::Single { info, .. } | Self::MultiTrack { info, .. } => info.as_ref(),
            Self::Batch { cursor_info, .. } => cursor_info.as_ref(),
        }
    }

    /// Bit depth for the currently previewed source, when probing has completed.
    ///
    /// Auto-dither must not guess when this returns `None`: a 24-bit source
    /// targeting 24-bit requires no dither, while a guessed reduction would
    /// incorrectly select TPDF.
    pub fn current_bit_depth(&self) -> Option<u32> {
        self.current_info().and_then(|info| info.bit_depth)
    }

    /// Total source size across all files in the current mode.
    /// Single/MultiTrack: the one file's size. Batch: sum of all file sizes.
    pub fn total_source_size(&self) -> u64 {
        match self {
            Self::Empty => 0,
            Self::Single { info, .. } | Self::MultiTrack { info, .. } => {
                info.as_ref().map_or(0, |i| i.file_size)
            }
            Self::Batch { total_size, .. } => *total_size,
        }
    }

    /// The currently previewed `SourceMetadata`. Returns an owned default
    /// for the Empty variant so the caller can always have something to
    /// display without extra matching.
    pub fn current_metadata(&self) -> SourceMetadata {
        match self {
            Self::Empty => SourceMetadata::default(),
            Self::Single { metadata, .. } | Self::MultiTrack { metadata, .. } => metadata.clone(),
            Self::Batch {
                cursor_metadata, ..
            } => cursor_metadata.clone(),
        }
    }

    /// Number of files (0/1/N for Empty/Single/Batch; 1 for MultiTrack).
    pub fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Single { .. } | Self::MultiTrack { .. } => 1,
            Self::Batch { paths, .. } => paths.len(),
        }
    }

    /// Build a SourceMode for a single path. Detects SACD ISOs and CUE
    /// pairs and returns MultiTrack when a track listing is available.
    pub fn from_single(path: PathBuf, info: Option<SourceInfo>, metadata: SourceMetadata) -> Self {
        // SACD ISO detection
        if crate::tui::sacd::is_sacd_iso(&path) {
            if let Ok(sacd) = crate::tui::sacd::parse_sacd_iso(&path) {
                let area = sacd.stereo.as_ref().or(sacd.multi_channel.as_ref());
                if let Some(area_info) = area {
                    let area_label = if sacd.stereo.is_some() {
                        "Stereo"
                    } else {
                        "Multichannel"
                    };
                    let mut tracks: Vec<MultiTrackEntry> = area_info
                        .tracks
                        .iter()
                        .enumerate()
                        .map(|(i, t)| {
                            let dur = format!("{}:{:02}", t.duration.minutes, t.duration.seconds);
                            MultiTrackEntry {
                                number: (i + 1) as u32,
                                title: t.text.title.clone(),
                                performer: t.text.performer.clone(),
                                duration_display: Some(dur),
                            }
                        })
                        .collect();

                    // Merge sidecar metadata (titles, artists) into the track
                    // list and album metadata. ScarletBook text fields are
                    // often empty; the XML sidecar has the real metadata.
                    let sidecar = crate::tui::sacd_sidecar::find_sidecar_for_iso(&path)
                        .and_then(|p| crate::tui::sacd_sidecar::parse_sidecar(&p).ok());
                    if let Some(ref sc) = sidecar {
                        for st in &sc.tracks {
                            if let Some(track) = tracks.iter_mut().find(|t| t.number == st.id) {
                                if let Some(title) = st.meta.get("TITLE") {
                                    if track.title.is_none() || track.title.as_deref() == Some("") {
                                        track.title = Some(title.clone());
                                    }
                                }
                                if let Some(artist) = st.meta.get("ARTIST") {
                                    if track.performer.is_none() || track.performer.as_deref() == Some("") {
                                        track.performer = Some(artist.clone());
                                    }
                                }
                            }
                        }
                    }

                    let album_title = sacd.album_title().map(|s| s.to_string());
                    let album_artist = sacd.album_artist().map(|s| s.to_string());

                    let mut meta = metadata;
                    if let Some(ref sc) = sidecar {
                        if let Some(first) = sc.tracks.first() {
                            if let Some(album) = first.meta.get("ALBUM") {
                                meta.album = Some(album.clone());
                            }
                            if let Some(artist) = first.meta.get("ARTIST") {
                                meta.artist = Some(artist.clone());
                            }
                        }
                    }
                    if meta.album.is_none() {
                        meta.album = album_title.clone();
                    }
                    if meta.artist.is_none() {
                        meta.artist = album_artist.clone();
                    }

                    let track_count = tracks.len();
                    return Self::MultiTrack {
                        path,
                        info,
                        metadata: meta,
                        tracks,
                        area_label: Some(area_label.to_string()),
                        album_title,
                        album_artist,
                        scroll: 0,
                        cursor: 0,
                        selected: vec![true; track_count],
                    };
                }
            }
        }

        // CUE sidecar detection
        if let Some(cue_path) = crate::tui::cue_parser::find_sidecar_cue(&path) {
            if let Ok(sheet) = crate::tui::cue_parser::parse_cue_file(&cue_path) {
                if sheet.tracks.len() >= 2 {
                    let tracks: Vec<MultiTrackEntry> = sheet
                        .tracks
                        .iter()
                        .map(|t| MultiTrackEntry {
                            number: t.number,
                            title: t.title.clone(),
                            performer: t.performer.clone().or_else(|| sheet.performer.clone()),
                            duration_display: None,
                        })
                        .collect();

                    let mut meta = metadata;
                    if meta.album.is_none() {
                        meta.album = sheet.title.clone();
                    }
                    if meta.artist.is_none() {
                        meta.artist = sheet.performer.clone();
                    }

                    let track_count = tracks.len();
                    return Self::MultiTrack {
                        path,
                        info,
                        metadata: meta,
                        tracks,
                        area_label: None,
                        album_title: sheet.title,
                        album_artist: sheet.performer,
                        scroll: 0,
                        cursor: 0,
                        selected: vec![true; track_count],
                    };
                }
            }
        }

        Self::Single {
            path,
            info,
            metadata,
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

/// Which row in the format pane is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatField {
    Format,
    // PCM rows
    SampleRate,
    BitDepth,
    Resampler,
    Dither,
    ReplayGain,
    // DSD rows
    DsdRate,
    NoiseShaper,
    ModulatorOrder,
    ConversionPreset,
}

impl FormatField {
    pub fn visible_rows(is_dsd: bool) -> &'static [Self] {
        if is_dsd {
            &[
                Self::Format,
                Self::DsdRate,
                Self::BitDepth,
                Self::NoiseShaper,
                Self::ModulatorOrder,
                Self::ConversionPreset,
            ]
        } else {
            &[
                Self::Format,
                Self::SampleRate,
                Self::BitDepth,
                Self::Resampler,
                Self::Dither,
                Self::ReplayGain,
            ]
        }
    }

    pub fn next_for(self, is_dsd: bool) -> Self {
        let rows = Self::visible_rows(is_dsd);
        let idx = rows.iter().position(|row| *row == self).unwrap_or(0);
        rows[(idx + 1) % rows.len()]
    }

    pub fn prev_for(self, is_dsd: bool) -> Self {
        let rows = Self::visible_rows(is_dsd);
        let idx = rows.iter().position(|row| *row == self).unwrap_or(0);
        rows[(idx + rows.len() - 1) % rows.len()]
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
    /// Mixed PCM/DSD rate row. Constraints expose PCM rates for PCM formats and DSD rates for DSD formats.
    pub sample_rate: PillState<u32>,
    pub bit_depth: PillState<BitDepthChoice>,
    pub resampler: PillState<ResamplerChoice>,
    pub dither: PillState<DitherType>,
    pub replaygain: PillState<ReplayGainChoice>,
    pub noise_shaper: PillState<DsdNoiseShaper>,
    pub modulator_order: PillState<ModulatorOrder>,
    pub conversion_preset: PillState<DsdConversionPreset>,
    pub field_focus: FormatField,
    pub advanced_open: bool,
    /// False until the user explicitly picks a dither algorithm. Bit-depth changes may update it.
    pub dither_overridden: bool,
    /// Selected container index into `AudioFormat::available_containers()`.
    /// 0 = codec default. Reset to 0 when the format pill changes.
    pub selected_container_index: usize,
    /// FLAC compression level (0-8). Visible below-the-fold when FLAC + maximized.
    pub flac_compression_level: u8,
    /// FLAC verify-during-encode toggle.
    pub flac_verify: PillState<bool>,
    /// FLAC MD5 checksum toggle (default: on).
    pub flac_md5: PillState<bool>,
    /// AAC profile (LC, HE, HEv2). Default: LC.
    pub aac_profile: tonepoet_pipeline::enums::AacProfile,
    /// AAC quality preset index into the profile-specific preset table.
    /// None = custom (user manually entered a bitrate).
    pub aac_quality_preset: Option<usize>,
    /// AAC bitrate in kbps (8-1024). Default: 256.
    pub aac_bitrate_kbps: u32,
}

// AAC quality preset tables keyed by profile.
pub const AAC_LC_PRESETS: &[(u32, &str)] = &[
    (320, "insane"),
    (256, "high"),
    (192, "standard"),
    (128, "portable"),
];
pub const AAC_HE_PRESETS: &[(u32, &str)] = &[
    (128, "high"),
    (80, "standard"),
    (64, "portable"),
];
pub const AAC_HEV2_PRESETS: &[(u32, &str)] = &[
    (64, "high"),
    (48, "standard"),
    (32, "portable"),
];

/// Get the quality preset table for the given AAC profile.
pub fn aac_presets_for_profile(
    profile: tonepoet_pipeline::enums::AacProfile,
) -> &'static [(u32, &'static str)] {
    use tonepoet_pipeline::enums::AacProfile;
    match profile {
        AacProfile::LcAac => AAC_LC_PRESETS,
        AacProfile::HeAac => AAC_HE_PRESETS,
        AacProfile::HeAacV2 => AAC_HEV2_PRESETS,
    }
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
            // PCM rates (kHz)
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
            // DSD rates
            (2_822_400, "DSD64"),
            (5_644_800, "DSD128"),
            (11_289_600, "DSD256"),
            (22_579_200, "DSD512"),
        ]);

        let bit_depth = PillState::new(vec![
            (BitDepthChoice::Int16, "16"),
            (BitDepthChoice::Int24, "24"),
            (BitDepthChoice::Int32, "32"),
            (BitDepthChoice::Float32, "32f"),
            (BitDepthChoice::Float64, "64f"),
        ]);

        let resampler = PillState::new(vec![
            (ResamplerChoice::Soxr, "soxr"),
            (ResamplerChoice::Sox, "sox"),
            (ResamplerChoice::Ssrc, "ssrc"),
        ]);

        let dither = PillState::new(vec![
            (DitherType::TPDF, "TPDF"),
            (DitherType::None, "none"),
            (DitherType::Shibata, "Shibata"),
            (DitherType::LowShibata, "Low-Shibata"),
            (DitherType::HighShibata, "High-Shibata"),
            (DitherType::Gesemann, "Gesemann"),
            (DitherType::Lipshitz, "Lipshitz"),
        ]);

        let replaygain = PillState::new(vec![
            (ReplayGainChoice::Album, "album"),
            (ReplayGainChoice::Track, "track"),
            (ReplayGainChoice::Both, "both"),
            (ReplayGainChoice::Off, "off"),
        ]);

        let noise_shaper = PillState::new(vec![
            (DsdNoiseShaper::Clans, "CLANS"),
            (DsdNoiseShaper::Sdm, "SDM"),
            (DsdNoiseShaper::Crfb, "CRFB"),
        ]);

        let modulator_order = PillState::new(vec![
            (ModulatorOrder::Order4, "4th"),
            (ModulatorOrder::Order5, "5th"),
            (ModulatorOrder::Order6, "6th"),
            (ModulatorOrder::Order7, "7th"),
            (ModulatorOrder::Order8, "8th"),
        ]);

        let conversion_preset = PillState::new(vec![
            (DsdConversionPreset::Auto, "Auto"),
            (DsdConversionPreset::Sinc, "Sinc"),
        ]);

        let mut state = Self {
            format,
            sample_rate,
            bit_depth,
            resampler,
            dither,
            replaygain,
            noise_shaper,
            modulator_order,
            conversion_preset,
            field_focus: FormatField::Format,
            advanced_open: false,
            dither_overridden: false,
            selected_container_index: 0,
            flac_compression_level: 8,
            flac_verify: PillState::new(vec![
                (false, "off"), (true, "on"),
            ]),
            flac_md5: PillState::new(vec![
                (true, "on"), (false, "off"),
            ]),
            aac_profile: tonepoet_pipeline::enums::AacProfile::LcAac,
            aac_quality_preset: Some(1), // "high" = index 1 in LC presets
            aac_bitrate_kbps: 256,
        };
        state.apply_format_constraints();
        state
    }

    /// The currently selected container for the active codec.
    pub fn selected_container(&self) -> &'static crate::convert::formats::ContainerOption {
        let containers = self.format.selected_value().available_containers();
        containers.get(self.selected_container_index).unwrap_or(&containers[0])
    }

    /// The file extension for the currently selected container.
    pub fn selected_extension(&self) -> &'static str {
        self.selected_container().extension
    }

    pub fn is_dsd_selected(&self) -> bool {
        is_dsd_format(*self.format.selected_value())
    }

    pub fn focus_next(&mut self) {
        self.field_focus = self.field_focus.next_for(self.is_dsd_selected());
    }

    pub fn focus_prev(&mut self) {
        self.field_focus = self.field_focus.prev_for(self.is_dsd_selected());
    }

    pub fn mark_dither_overridden(&mut self) {
        self.dither_overridden = true;
    }

    pub fn select_bit_depth(&mut self, bit_depth: BitDepthChoice, source_bits: Option<u32>) {
        self.bit_depth.select_value(&bit_depth);
        self.apply_auto_dither(source_bits);
        self.apply_format_constraints();
    }

    /// Select the next enabled pill in the focused row and run row-specific side effects.
    /// Key and mouse handlers should use this instead of calling `focused_pill_mut()` directly.
    pub fn select_focused_next(&mut self, source_bits: Option<u32>) {
        let before_depth = *self.bit_depth.selected_value();
        let before_format = *self.format.selected_value();
        let focused = self.field_focus;
        self.focused_pill_mut().select_next();
        self.after_user_selection(focused, before_format, before_depth, source_bits);
    }

    /// Select the previous enabled pill in the focused row and run row-specific side effects.
    pub fn select_focused_prev(&mut self, source_bits: Option<u32>) {
        let before_depth = *self.bit_depth.selected_value();
        let before_format = *self.format.selected_value();
        let focused = self.field_focus;
        self.focused_pill_mut().select_prev();
        self.after_user_selection(focused, before_format, before_depth, source_bits);
    }

    /// Select a concrete pill index for mouse handlers and run row-specific side effects.
    pub fn select_row_index(&mut self, row: FormatField, index: usize, source_bits: Option<u32>) {
        let before_depth = *self.bit_depth.selected_value();
        let before_format = *self.format.selected_value();
        self.field_focus = row;
        match row {
            FormatField::Format => select_enabled_index(&mut self.format, index),
            FormatField::SampleRate | FormatField::DsdRate => select_enabled_index(&mut self.sample_rate, index),
            FormatField::BitDepth => select_enabled_index(&mut self.bit_depth, index),
            FormatField::Resampler => select_enabled_index(&mut self.resampler, index),
            FormatField::Dither => select_enabled_index(&mut self.dither, index),
            FormatField::ReplayGain => select_enabled_index(&mut self.replaygain, index),
            FormatField::NoiseShaper => select_enabled_index(&mut self.noise_shaper, index),
            FormatField::ModulatorOrder => select_enabled_index(&mut self.modulator_order, index),
            FormatField::ConversionPreset => select_enabled_index(&mut self.conversion_preset, index),
        }
        self.after_user_selection(row, before_format, before_depth, source_bits);
    }

    fn after_user_selection(
        &mut self,
        row: FormatField,
        before_format: AudioFormat,
        before_depth: BitDepthChoice,
        source_bits: Option<u32>,
    ) {
        if row == FormatField::Dither {
            self.mark_dither_overridden();
        }

        if row == FormatField::Format && before_format != *self.format.selected_value() {
            self.selected_container_index = 0;
            self.apply_format_constraints();
            if self.is_dsd_selected() {
                self.dither.select_value(&DitherType::None);
                self.cascade_dsd_rate_defaults();
            } else if !self.dither_overridden {
                self.apply_auto_dither(source_bits);
            }
            return;
        }

        if row == FormatField::DsdRate {
            self.cascade_dsd_rate_defaults();
        }

        if row == FormatField::BitDepth && before_depth != *self.bit_depth.selected_value() {
            self.dither_overridden = false;
            self.apply_auto_dither(source_bits);
        }

        self.apply_format_constraints();
    }

    /// Applies the default dither rule while preserving manual user choice.
    /// `source_bits` should come from the selected source probe when available.
    pub fn apply_auto_dither(&mut self, source_bits: Option<u32>) {
        if self.dither_overridden || self.is_dsd_selected() {
            return;
        }

        let Some(source_bits) = source_bits else {
            // Unknown source depth is not evidence of bit-depth reduction.
            // Prefer the non-destructive default until probing supplies a value.
            self.dither.select_value(&DitherType::None);
            return;
        };

        // DSD and PCM are incommensurable encoding schemes — the conversion
        // is a reconstruction, not a truncation. Always dither at the PCM
        // output stage: TPDF for ≥24-bit, Shibata for ≤16-bit.
        if source_bits == 1 {
            let target = *self.bit_depth.selected_value();
            let desired = if target.bits() <= 16 {
                DitherType::Shibata
            } else {
                DitherType::TPDF
            };
            self.dither.select_value(&desired);
            return;
        }

        let target = *self.bit_depth.selected_value();
        let target_bits = target.bits();
        let desired = if source_bits > target_bits && target_bits <= 16 {
            DitherType::Shibata
        } else if source_bits > target_bits && target_bits == 24 {
            DitherType::TPDF
        } else {
            DitherType::None
        };
        self.dither.select_value(&desired);
    }

    /// Set PCM output defaults to match a PCM source. Called when a source is
    /// first probed or when the output format is PCM and source info becomes
    /// available. Selects the closest available sample rate and bit depth pills,
    /// then applies auto-dither based on the resulting source/target combination.
    pub fn cascade_pcm_source_defaults(
        &mut self,
        source_sample_rate: u32,
        source_bit_depth: Option<u32>,
        source_is_float: bool,
    ) {
        if self.is_dsd_selected() {
            return;
        }
        // Match source sample rate if it's in the pill options.
        self.sample_rate.select_value(&source_sample_rate);
        // Match source bit depth, preserving float vs integer distinction.
        if let Some(bits) = source_bit_depth {
            let depth = if source_is_float {
                match bits {
                    0..=32 => BitDepthChoice::Float32,
                    _ => BitDepthChoice::Float64,
                }
            } else {
                match bits {
                    0..=16 => BitDepthChoice::Int16,
                    17..=24 => BitDepthChoice::Int24,
                    _ => BitDepthChoice::Int32,
                }
            };
            self.bit_depth.select_value(&depth);
        }
    }

    /// Set PCM defaults when the source is DSD. Called when the user switches
    /// from a DSD output format to a PCM output format while viewing a DSD source.
    /// Sets the recommended target sample rate and 24-bit depth.
    pub fn cascade_dsd_source_to_pcm_defaults(&mut self, source_sample_rate: u32) {
        if self.is_dsd_selected() {
            return;
        }
        let Some(dsd_rate) = tonepoet_pipeline::DsdRate::from_hz(source_sample_rate) else {
            return;
        };
        let target_hz = dsd_rate.default_pcm_target_hz();
        self.sample_rate.select_value(&target_hz);
        self.bit_depth.select_value(&BitDepthChoice::Int24);
        self.resampler.select_value(&ResamplerChoice::Sox);
    }

    /// Set noise shaper and modulator order to the recommended defaults for the
    /// current DSD rate. Called when the user switches to a DSD format or changes
    /// the DSD rate pill — not during constraint reapplication, so preset values
    /// and manual overrides are preserved.
    fn cascade_dsd_rate_defaults(&mut self) {
        if let Some(dsd_rate) = tonepoet_pipeline::DsdRate::from_hz(*self.sample_rate.selected_value()) {
            self.noise_shaper.select_value(&dsd_rate.default_noise_shaper());
            self.modulator_order.select_value(&dsd_rate.default_modulator_order());
        }
    }

    /// Recalculate which options are enabled based on the selected format.
    pub fn apply_format_constraints(&mut self) {
        let fmt = *self.format.selected_value();

        self.sample_rate.set_all_enabled(true);
        self.bit_depth.set_all_enabled(true);
        self.resampler.set_all_enabled(true);
        self.dither.set_all_enabled(true);
        self.replaygain.set_all_enabled(true);
        self.noise_shaper.set_all_enabled(true);
        self.modulator_order.set_all_enabled(true);
        self.conversion_preset.set_all_enabled(true);

        // DSD rate threshold: rates at or above this are DSD, below are PCM.
        const DSD_RATE_MIN: u32 = 2_822_400;
        let is_dsd = is_dsd_format(fmt);

        for opt in &mut self.sample_rate.options {
            opt.enabled = if is_dsd {
                opt.value >= DSD_RATE_MIN
            } else {
                opt.value < DSD_RATE_MIN
            };
        }

        if is_dsd {
            self.bit_depth.set_all_enabled(false);
            self.resampler.set_all_enabled(false);
            self.dither.set_all_enabled(false);
            self.replaygain.set_all_enabled(false);
        } else {
            self.noise_shaper.set_all_enabled(false);
            self.modulator_order.set_all_enabled(false);
            self.conversion_preset.set_all_enabled(false);
        }

        match fmt {
            AudioFormat::Dsf | AudioFormat::Dff => {}
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
            AudioFormat::Flac | AudioFormat::Alac => {
                self.bit_depth.set_enabled(&BitDepthChoice::Float32, false);
                self.bit_depth.set_enabled(&BitDepthChoice::Float64, false);
                for opt in &mut self.sample_rate.options {
                    if opt.value > 384_000 {
                        opt.enabled = false;
                    }
                }
            }
            AudioFormat::WavPack => {
                // Float32 supported, Float64 rejected by WavPack encoder.
                self.bit_depth.set_enabled(&BitDepthChoice::Float64, false);
            }
            AudioFormat::Wav | AudioFormat::Aiff | AudioFormat::Lpcm => {
                // Full range including float32 and float64.
            }
            // Dts/Ac3 are lossy, 48kHz only
            AudioFormat::Dts | AudioFormat::Ac3 => {
                self.bit_depth.set_all_enabled(false);
                self.dither.set_all_enabled(false);
                self.sample_rate.select_value(&48_000);
                for opt in &mut self.sample_rate.options {
                    if opt.value != 48_000 {
                        opt.enabled = false;
                    }
                }
            }
            // Ape is lossless but not encodable; same constraints as FLAC
            AudioFormat::Ape => {
                self.bit_depth.set_enabled(&BitDepthChoice::Float32, false);
                self.bit_depth.set_enabled(&BitDepthChoice::Float64, false);
                for opt in &mut self.sample_rate.options {
                    if opt.value > 384_000 {
                        opt.enabled = false;
                    }
                }
            }
        }

        self.clamp_disabled_selections();
        if !FormatField::visible_rows(self.is_dsd_selected()).contains(&self.field_focus) {
            self.field_focus = FormatField::Format;
        }
    }

    fn clamp_disabled_selections(&mut self) {
        clamp_pill(&mut self.sample_rate);
        clamp_pill(&mut self.bit_depth);
        clamp_pill(&mut self.resampler);
        clamp_pill(&mut self.dither);
        clamp_pill(&mut self.replaygain);
        clamp_pill(&mut self.noise_shaper);
        clamp_pill(&mut self.modulator_order);
        clamp_pill(&mut self.conversion_preset);
    }

    pub fn focused_pill_mut(&mut self) -> FocusedPill<'_> {
        match self.field_focus {
            FormatField::Format => FocusedPill::Format(&mut self.format),
            FormatField::SampleRate | FormatField::DsdRate => FocusedPill::SampleRate(&mut self.sample_rate),
            FormatField::BitDepth => FocusedPill::BitDepth(&mut self.bit_depth),
            FormatField::Resampler => FocusedPill::Resampler(&mut self.resampler),
            FormatField::Dither => FocusedPill::Dither(&mut self.dither),
            FormatField::ReplayGain => FocusedPill::ReplayGain(&mut self.replaygain),
            FormatField::NoiseShaper => FocusedPill::NoiseShaper(&mut self.noise_shaper),
            FormatField::ModulatorOrder => FocusedPill::ModulatorOrder(&mut self.modulator_order),
            FormatField::ConversionPreset => FocusedPill::ConversionPreset(&mut self.conversion_preset),
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

fn select_enabled_index<T: Clone + PartialEq>(pill: &mut PillState<T>, index: usize) {
    if let Some(option) = pill.options.get(index) {
        if option.enabled {
            pill.selected = index;
        }
    }
}

/// Enum to allow generic prev/next on whichever pill is focused
pub enum FocusedPill<'a> {
    Format(&'a mut PillState<AudioFormat>),
    SampleRate(&'a mut PillState<u32>),
    BitDepth(&'a mut PillState<BitDepthChoice>),
    Resampler(&'a mut PillState<ResamplerChoice>),
    Dither(&'a mut PillState<DitherType>),
    ReplayGain(&'a mut PillState<ReplayGainChoice>),
    NoiseShaper(&'a mut PillState<DsdNoiseShaper>),
    ModulatorOrder(&'a mut PillState<ModulatorOrder>),
    ConversionPreset(&'a mut PillState<DsdConversionPreset>),
}

impl FocusedPill<'_> {
    pub fn select_next(&mut self) {
        match self {
            Self::Format(p) => p.select_next(),
            Self::SampleRate(p) => p.select_next(),
            Self::BitDepth(p) => p.select_next(),
            Self::Resampler(p) => p.select_next(),
            Self::Dither(p) => p.select_next(),
            Self::ReplayGain(p) => p.select_next(),
            Self::NoiseShaper(p) => p.select_next(),
            Self::ModulatorOrder(p) => p.select_next(),
            Self::ConversionPreset(p) => p.select_next(),
        }
    }

    pub fn select_prev(&mut self) {
        match self {
            Self::Format(p) => p.select_prev(),
            Self::SampleRate(p) => p.select_prev(),
            Self::BitDepth(p) => p.select_prev(),
            Self::Resampler(p) => p.select_prev(),
            Self::Dither(p) => p.select_prev(),
            Self::ReplayGain(p) => p.select_prev(),
            Self::NoiseShaper(p) => p.select_prev(),
            Self::ModulatorOrder(p) => p.select_prev(),
            Self::ConversionPreset(p) => p.select_prev(),
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
    /// Scroll offset for the convert-screen metadata file list. The cursor
    /// itself lives on SourceMode::Batch / SourceMode::MultiTrack.
    pub file_scroll: usize,
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
    pub layout: ConvertLayout,
    /// Last pane title-bar click used for double-click maximize/restore.
    pub pane_title_last_click: Option<(ConvertFocus, std::time::Instant)>,
    /// Last metadata file-row click used for double-click edit.
    pub metadata_file_last_click: Option<(usize, std::time::Instant)>,
}

impl ConvertState {
    pub fn new() -> Self {
        Self {
            source: SourceState::default(),
            metadata: MetadataState::default(),
            format: FormatState::new(),
            output_options: OutputOptionsState::new(),
            focus: ConvertFocus::Source,
            layout: ConvertLayout::Default,
            pane_title_last_click: None,
            metadata_file_last_click: None,
        }
    }

    /// Whether a specific pane is currently collapsed to its title bar.
    pub fn is_collapsed(&self, pane: ConvertFocus) -> bool {
        match self.layout {
            ConvertLayout::Default => false,
            ConvertLayout::Maximized(maximized) => pane != maximized,
        }
    }

    /// Whether a specific pane is currently maximized.
    pub fn is_maximized(&self, pane: ConvertFocus) -> bool {
        self.layout == ConvertLayout::Maximized(pane)
    }

    /// Toggle a pane between maximized and default layout.
    pub fn toggle_maximize(&mut self, pane: ConvertFocus) {
        self.layout = match self.layout {
            ConvertLayout::Maximized(current) if current == pane => ConvertLayout::Default,
            _ => ConvertLayout::Maximized(pane),
        };
    }

    /// Reset metadata file-list view state after the convert source changes.
    /// The logical cursor lives on SourceMode, so source replacement invalidates
    /// the metadata list scroll window and any pending row double-click state.
    pub fn reset_metadata_file_list_state(&mut self) {
        self.metadata.file_scroll = 0;
        self.metadata_file_last_click = None;
    }

    /// Replace the convert source mode and reset metadata list state in one
    /// place so source changes cannot leave stale scroll or double-click state.
    pub fn set_source_mode(&mut self, mode: SourceMode) {
        self.reset_metadata_file_list_state();
        self.source.mode = mode;
    }

    /// Source bit depth for format-pane side effects such as auto-dither.
    pub fn current_source_bit_depth(&self) -> Option<u32> {
        self.source.mode.current_bit_depth()
    }

    pub fn current_source_sample_rate(&self) -> Option<u32> {
        self.source.mode.current_info().map(|info| info.sample_rate)
    }

    /// Apply source-aware format pane defaults after a probe completes.
    /// For PCM sources: matches sample rate and bit depth to source.
    /// For DSD sources with PCM output: sets recommended target rate and 24-bit.
    pub fn apply_source_defaults(&mut self) {
        let Some(info) = self.source.mode.current_info() else {
            return;
        };
        let source_rate = info.sample_rate;
        let source_bits = info.bit_depth;
        let source_is_float = info.codec.contains("Float");
        let is_dsd_source = tonepoet_pipeline::DsdRate::from_hz(source_rate).is_some();

        if is_dsd_source {
            if !self.format.is_dsd_selected() {
                self.format.cascade_dsd_source_to_pcm_defaults(source_rate);
            }
        } else {
            self.format.cascade_pcm_source_defaults(source_rate, source_bits, source_is_float);
        }
        self.format.dither_overridden = false;
        self.format.apply_auto_dither(source_bits);
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
    /// Stack-based cascade model: each level appears as a panel to the
    /// right of its parent (hexload-tui pattern, generalized to N
    /// levels). The deepest level is the one with keyboard focus;
    /// ancestor panels stay visible (Windows-style). Depth is capped
    /// at `MAX_CONTEXT_MENU_DEPTH` (4).
    ContextMenu {
        levels: Vec<crate::tui::context_menu::MenuLevel>,
        /// Anchor for the root level (right-click position).
        origin: (u16, u16),
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
    /// Read-only preview of a proposed CUE sheet (from `:cue-mb` /
    /// `:cue-mb!` / `:cue-fill`). User reviews + presses `s` to commit
    /// the write, `q`/`Esc` to cancel.
    CuePreview(Box<CuePreviewState>),
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
    /// MusicBrainz release selection overlay (when MB returns >1 match).
    /// User picks one; on accept the chosen release flows into the
    /// metadata editor.
    MbSelect(Box<MbSelectState>),
    /// GNUDB review overlay — editable preview of GNUDB tags before
    /// accepting into the metadata editor.
    GnudbReview(Box<GnudbReviewState>),
    /// AccurateRip verification results overlay (supports multi-disc).
    AccurateRipVerify(Box<ArVerifyState>),
    /// CUETools DB verification results overlay (supports multi-disc).
    CtdbVerify(Box<CtdbVerifyState>),
    /// AccurateRip batch verification report overlay.
    ArBatchReport {
        result: Box<crate::tui::accuraterip::ArBatchResult>,
        scroll: usize,
    },
    /// Template builder overlay for composing folder/filename templates.
    TemplateBuilder(Box<TemplateBuilderState>),
    /// Template picker overlay for loading saved templates.
    TemplatePicker {
        target: TemplateTarget,
        templates: Vec<String>,
        selected: usize,
        scroll: usize,
        /// Precomputed preview of the selected template.
        preview: String,
        /// Current field value (for "active" badge).
        active_template: Option<String>,
    },
    /// Format-specific settings overlay (e.g. FLAC compression/verify/md5,
    /// AAC profile/quality/bitrate). Owns temporary copies; committed on
    /// Enter, discarded on Esc.
    FormatSettings {
        kind: FormatSettingsKind,
        focus: FormatSettingsFocus,
    },
}

/// Which field the template builder is editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateTarget {
    Folder,
    Filename,
}

/// Which section of the template builder has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateBuilderFocus {
    TemplateInput,
    SavedList,
    TokenGrid,
}

/// State for the template builder overlay.
#[derive(Debug, Clone)]
pub struct TemplateBuilderState {
    /// The editable template line.
    pub template_input: crate::tui::text_input::TextInputState,
    /// Whether this builds a folder or filename template.
    pub target: TemplateTarget,
    /// Which section has keyboard focus.
    pub focus: TemplateBuilderFocus,
    /// Cursor position in the token/separator grid.
    pub grid_cursor: usize,
    /// Saved template strings loaded from disk.
    pub saved_templates: Vec<String>,
    /// Selected index in the saved templates list.
    pub saved_selected: usize,
    /// Scroll offset for the saved templates list.
    pub saved_scroll: usize,
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

/// State for the CUETools DB verification overlay.
/// Supports multi-disc: each page is one disc's results.
#[derive(Debug, Clone)]
pub struct CtdbVerifyState {
    pub pages: Vec<CtdbVerifyPage>,
    pub active_page: usize,
    pub scroll: usize,
}

/// A single page (disc) in the CTDB verification overlay.
#[derive(Debug, Clone)]
pub struct CtdbVerifyPage {
    pub label: String,
    pub result: crate::tui::ctdb::CtdbVerifyResult,
}

/// Origin of a `GnudbReviewState` instance — drives the overlay
/// title so the user can tell whether they're reviewing a gnudb
/// query result vs. a CUE-imported metadata set, even though both
/// land in the same review surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewSource {
    /// State built from a gnudb HTTP query (`:tags-gnudb`).
    Gnudb,
    /// State built from a sidecar CUE file (`:import-cue`).
    CueImport,
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
    /// Where this review came from. Drives the overlay title prefix
    /// ("GNUDB Review" vs. "CUE Import Review") so the user can
    /// disambiguate post-:import-cue from a real gnudb match.
    pub source: ReviewSource,
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
    TrackField {
        track_idx: usize,
        field: &'static str,
    },
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
    /// Cached MusicBrainz lookup result + paths so the user can run
    /// `:mb-back` to pick a different release without re-querying.
    /// `None` when the editor wasn't reached through MB picker (or
    /// the lookup had a single match and the picker was skipped).
    pub mb_back: Option<MbBackCache>,
    /// Cached GnudbReviewState so the user can run `:gnudb-back`
    /// to return from the populated editor to the per-track review
    /// surface without re-querying gnudb. `None` when the editor
    /// wasn't reached via the gnudb flow.
    pub gnudb_back: Option<Box<GnudbReviewState>>,
    /// When true, the editor is opened in display-only mode: edits,
    /// deletions, additions, and saves are all refused with a status
    /// message. Set for SACD ISOs without a writable sidecar; once
    /// C5c discovered a sidecar to write to, this flips to false.
    pub read_only: bool,
    /// SACD-only: the sidecar XML path that `:w` will read-modify-
    /// write into. `None` for normal lofty-backed editors.
    pub sacd_sidecar_path: Option<std::path::PathBuf>,
    /// SACD-only: which area's tracks the editor is currently
    /// surfacing, so the save path knows which sidecar track IDs
    /// to update (`Stereo` → area 1, `MultiChannel` → area 2).
    pub sacd_area_kind: Option<crate::tui::sacd::AreaKind>,
    /// SACD-only: per-track durations (seconds) for the stereo area,
    /// stashed at editor-open time so `:tags-mb` can synthesize a
    /// CD-equivalent TOC for MusicBrainz disc-id lookup without
    /// re-reading the ISO. `None` when the disc has no stereo area
    /// or its TRL1/TRL2 sectors failed to parse.
    pub sacd_stereo_durations: Option<Vec<f64>>,
    /// SACD-only: same as `sacd_stereo_durations` for the
    /// multi-channel area. `None` when absent or unparseable.
    /// Both fields are populated regardless of which area the
    /// editor is currently surfacing — the sibling-area mirror
    /// (future C-2c) needs both available.
    pub sacd_multi_channel_durations: Option<Vec<f64>>,
}

/// State cached on `MetadataEditorState::mb_back` to support the
/// `:mb-back` colon command. After the user picks a release in
/// `MbSelect` and lands in the metadata editor, the full release
/// list + paths are stashed here. `:mb-back` re-constructs the
/// `MbSelectState` from this cache (preserving the prior selection)
/// and transitions the overlay back, no MB requery needed.
#[derive(Debug, Clone)]
pub struct MbBackCache {
    pub releases: Vec<crate::tui::musicbrainz::MbRelease>,
    pub paths: Vec<std::path::PathBuf>,
    pub selected: usize,
}

/// State for the MusicBrainz release-selection overlay shown when MB
/// returns >1 candidate release for a disc TOC. Lists releases sorted
/// by descending score; user picks one to advance to the metadata
/// editor.
#[derive(Debug, Clone)]
pub struct MbSelectState {
    /// Candidate releases (highest-scoring first).
    pub releases: Vec<crate::tui::musicbrainz::MbRelease>,
    /// Cursor position (0-based index into `releases`).
    pub selected: usize,
    /// Top row of the visible window (vim-smooth scroll).
    pub scroll: usize,
    /// Audio file paths the lookup was computed for (used to populate
    /// the metadata editor after the user accepts).
    pub paths: Vec<std::path::PathBuf>,
    /// Last left-click on a row, used for double-click-to-accept
    /// detection. Skipped from Clone-derived semantics by being
    /// reset on each click cycle.
    pub last_click: Option<(usize, std::time::Instant)>,
    /// Per-release detail cache populated by Phase B-4 prefetch. Keyed
    /// by `MbRelease.release_id`. Search-endpoint rows in `releases`
    /// are shallow (no per-track titles); when a row is highlighted
    /// and not yet cached, a debounced detail fetch fires and the
    /// result lands here for the renderer to consume.
    pub prefetch: std::collections::BTreeMap<String, crate::tui::musicbrainz::MbRelease>,
    /// Monotonically-increasing generation counter. Incremented each
    /// time the highlighted row changes; spawned prefetch tasks
    /// capture the value at spawn time and re-check the atomic after
    /// the 150 ms debounce — a mismatch means the user moved on, so
    /// the task exits without firing HTTP and without consuming an
    /// MB rate-limit token.
    pub generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl MbSelectState {
    pub fn new(
        releases: Vec<crate::tui::musicbrainz::MbRelease>,
        paths: Vec<std::path::PathBuf>,
    ) -> Self {
        Self {
            releases,
            selected: 0,
            scroll: 0,
            paths,
            last_click: None,
            prefetch: std::collections::BTreeMap::new(),
            generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Bump the prefetch generation, returning the new value for the
    /// caller to pass into a freshly-spawned prefetch task. Use after
    /// any highlight change (Up/Down/PgUp/PgDn/click) — older in-flight
    /// tasks will observe the mismatch on wake and exit cleanly.
    pub fn bump_generation(&self) -> u64 {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1
    }
}

/// State for the CUE preview overlay opened by `:cue-mb` / `:cue-mb!` /
/// `:cue-fill`. Read-only by default; in-place line editing is reachable
/// via the `:e <N>` command, which seeds `edit` with a `TextInputState`
/// scoped to the chosen line.
#[derive(Debug, Clone)]
pub struct CuePreviewState {
    /// Rendered CUE content to be written on save.
    pub content: String,
    /// Destination path (existing CUE for `:cue-fill`, derived filename
    /// for `:cue-mb`).
    pub write_path: std::path::PathBuf,
    /// One-line summary shown in the title bar
    /// (e.g., `"Filled CUE: 7 ISRCs, 1 catalog"`).
    pub summary: String,
    /// Top row of the visible window for vim-smooth scroll.
    pub scroll: usize,
    /// 0-based line being edited (for renderer highlight + commit
    /// splice). `None` when not in edit mode.
    pub cursor: Option<usize>,
    /// Active text input when editing a single line. `Enter` commits;
    /// `Esc` cancels without splicing.
    pub edit: Option<crate::tui::text_input::TextInputState>,
    /// Last left-click on a content line, used for double-click-to-edit
    /// detection.
    pub last_click: Option<(usize, std::time::Instant)>,
    /// Read-only mode: no [Save] pill, no inline edit, `:` and
    /// right-click are blocked. Used when the overlay is showing an
    /// embedded CUESHEET tag (opened via `[view]` from the metadata
    /// editor).
    pub read_only: bool,
}

impl CuePreviewState {
    pub fn new(content: String, write_path: std::path::PathBuf, summary: String) -> Self {
        Self {
            content,
            write_path,
            summary,
            scroll: 0,
            cursor: None,
            edit: None,
            last_click: None,
            read_only: false,
        }
    }

    /// Build a read-only preview (used by the metadata editor's `[view]`
    /// pill on synthetic-preview rows like CUESHEET). No write path is
    /// needed because the overlay can't save back to disk in this mode.
    pub fn new_readonly(content: String, summary: String) -> Self {
        Self {
            content,
            write_path: std::path::PathBuf::new(),
            summary,
            scroll: 0,
            cursor: None,
            edit: None,
            last_click: None,
            read_only: true,
        }
    }

    /// Number of content lines (cached for scroll bounds + display).
    pub fn line_count(&self) -> usize {
        self.content.lines().count()
    }

    /// True while a single line is being edited via `:e <N>`.
    pub fn is_editing(&self) -> bool {
        self.edit.is_some()
    }

    /// Begin editing the 0-based `line_idx`. Seeds the `TextInputState`
    /// with that line's current text and parks the cursor on it. No-op
    /// when `line_idx` is out of range.
    pub fn begin_edit(&mut self, line_idx: usize) -> bool {
        let Some(line) = self.content.lines().nth(line_idx) else {
            return false;
        };
        self.edit = Some(crate::tui::text_input::TextInputState::new(
            line.to_string(),
        ));
        self.cursor = Some(line_idx);
        true
    }

    /// Commit the current edit by splicing the input's text in place of
    /// the cursor's line. Clears `edit` and `cursor` on success.
    pub fn commit_edit(&mut self) {
        let (Some(idx), Some(input)) = (self.cursor, self.edit.take()) else {
            return;
        };
        let new_text = input.text;
        let mut lines: Vec<String> = self.content.lines().map(str::to_string).collect();
        if idx < lines.len() {
            lines[idx] = new_text;
        }
        // Preserve trailing newline if the source had one.
        let trailing = self.content.ends_with('\n');
        self.content = lines.join("\n");
        if trailing {
            self.content.push('\n');
        }
        self.cursor = None;
    }

    /// Discard the current edit without splicing.
    pub fn cancel_edit(&mut self) {
        self.edit = None;
        self.cursor = None;
    }
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
        let plan = crate::tui::rename_plan::RenamePlan::new(base_dir, Vec::new());
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
    /// Return from the metadata editor to the MbSelect picker,
    /// discarding any edits. Carries the cached release list + paths
    /// so the picker can be reconstructed.
    MbBack(MbBackCache),
    /// Return from the metadata editor to the GnudbReview surface,
    /// discarding any edits. Carries the cached review state so the
    /// per-track edits are preserved on re-entry.
    GnudbBack(Box<GnudbReviewState>),
    RemoveSelected,
    ClearCompleted,
    ClearFinished,
    ClearAll,
    StopAll,
    ClearQueue,
    /// Move the given paths to the system trash (XDG Trash / Finder Trash).
    TrashSelection(Vec<PathBuf>),
    /// Apply AccurateRip offset correction to a set of tracks.
    OffsetCorrection {
        paths: Vec<PathBuf>,
        offset: i32,
    },
    /// Apply CTDB Reed-Solomon repair to a set of tracks.
    CtdbRepair {
        paths: Vec<PathBuf>,
        parity_url: String,
        npar: usize,
        offset: i32,
        /// Expected per-track CRC32 values from the CTDB entry.
        /// Used to verify the repair before replacing originals.
        expected_crcs: Vec<u32>,
    },
    /// Apply CTDB Reed-Solomon repair to a single-image CUE album
    /// (one audio file containing all tracks). The `info` carries the
    /// CUE-derived track boundaries needed for per-track CRC verification.
    CtdbRepairSingleImage {
        info: Box<crate::tui::cue_parser::SingleImageInfo>,
        parity_url: String,
        npar: usize,
        offset: i32,
        expected_crcs: Vec<u32>,
    },
}

/// Repair parameters parked while AR verification runs to resolve
/// the drive read offset. Populated by `Command::CtdbRepair` when the
/// AR cache is empty/inconclusive; consumed by the `AccurateRipComplete`
/// handler which then opens the CTDB repair confirmation dialog.
#[derive(Debug, Clone)]
pub struct PendingCtdbRepair {
    pub paths: Vec<PathBuf>,
    pub parity_url: String,
    pub npar: usize,
    pub expected_crcs: Vec<u32>,
    /// `Some` when the source is a single-image CUE; the AR-complete
    /// handler will dispatch to `CtdbRepairSingleImage` instead of
    /// `CtdbRepair`.
    pub single_image: Option<Box<crate::tui::cue_parser::SingleImageInfo>>,
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
    /// Temp directory for single-image analysis segment extraction.
    /// Cleaned up when `analysis_pending` reaches 0.
    pub analysis_temp_dir: Option<PathBuf>,

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

    /// Parked CuePreviewState while command mode is open. Set when `:`
    /// is pressed in the CUE preview overlay; consumed by `:w` (writes
    /// the CUE) and `:q` (cancels), or restored unchanged if neither.
    pub pending_cue_preview: Option<Box<CuePreviewState>>,

    /// Parked MbSelectState while a context menu is open over the
    /// MusicBrainz release picker. Restored when the menu closes
    /// without consuming the picker.
    pub pending_mb_select: Option<Box<MbSelectState>>,

    // Status
    pub status_message: Option<(String, std::time::Instant)>,
    pub processing_active: bool,
    pub should_quit: bool,
    /// Set after returning from an external editor to force ratatui
    /// to repaint the entire screen (diff-based rendering would
    /// otherwise leave stale regions blank).
    pub force_redraw: bool,
    /// When true, the next `AccurateRipComplete` handler will auto-check
    /// for a fixable offset and show the correction confirmation dialog
    /// instead of the normal results overlay.
    pub auto_fix_on_complete: bool,

    /// Set when `:ctdb-repair` was invoked but the AR cache had no usable
    /// offset data, so AR was kicked off first. The next
    /// `AccurateRipComplete` handler consumes this, derives the offset
    /// from the AR results, and opens the CTDB repair confirmation
    /// dialog instead of the normal results overlay.
    pub pending_ctdb_repair: Option<PendingCtdbRepair>,

    /// Set when `:ctdb-repair` was invoked without a CTDB overlay open
    /// (e.g. via the "CUETools DB repair" context menu item). The next
    /// `CtdbComplete` handler consumes this and re-dispatches
    /// `Command::CtdbRepair` so the existing repair flow can run against
    /// the just-installed verification overlay.
    pub auto_repair_on_ctdb_complete: bool,

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
                crate::db::Database::open_memory().expect("in-memory DB should never fail")
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
                layout: ConvertLayout::Default,
                pane_title_last_click: None,
                metadata_file_last_click: None,
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
            pending_cue_preview: None,
            pending_mb_select: None,
            status_message: None,
            processing_active: false,
            should_quit: false,
            force_redraw: false,
            auto_fix_on_complete: false,
            pending_ctdb_repair: None,
            auto_repair_on_ctdb_complete: false,
            last_browse_click: None,
            pending_browse_rename: None,
            recent,
            bookmarks,
            keychain: KeychainState::default(),
            archive_passwords: std::collections::HashMap::new(),
            hover_target: None,
            analysis_results: Vec::new(),
            analysis_pending: 0,
            analysis_temp_dir: None,
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
            let items: Vec<&crate::convert::ConversionItem> = q
                .all_items()
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
            SourceMode::Single {
                info: slot,
                metadata: meta_slot,
                ..
            } => {
                *slot = info;
                *meta_slot = metadata;
            }
            SourceMode::Batch {
                cursor_info,
                cursor_metadata,
                ..
            } => {
                *cursor_info = info;
                *cursor_metadata = metadata;
            }
            SourceMode::MultiTrack {
                info: slot,
                metadata: meta_slot,
                ..
            } => {
                *slot = info;
                *meta_slot = metadata;
            }
            SourceMode::Empty => {
                // Unreachable — valid.is_empty() check guards against 0 paths.
            }
        }
        self.convert.set_source_mode(mode);

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

#[cfg(test)]
mod cue_preview_state_tests {
    use super::*;

    #[test]
    fn new_defaults_to_writable() {
        let s = CuePreviewState::new(
            "x".into(),
            std::path::PathBuf::from("/tmp/x.cue"),
            "s".into(),
        );
        assert!(!s.read_only, "::new must default to writable");
    }

    #[test]
    fn new_readonly_sets_flag_and_clears_write_path() {
        let s = CuePreviewState::new_readonly("content\nline\n".into(), "summary".into());
        assert!(s.read_only, "new_readonly must set read_only");
        assert_eq!(
            s.write_path,
            std::path::PathBuf::new(),
            "new_readonly must use empty write_path (no disk target)"
        );
        assert!(s.edit.is_none());
        assert!(s.cursor.is_none());
    }

    #[test]
    fn line_count_counts_lines_correctly() {
        let s = CuePreviewState::new(
            "FILE \"x.flac\" FLAC\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n".to_string(),
            std::path::PathBuf::from("/tmp/test.cue"),
            "summary".to_string(),
        );
        assert_eq!(s.line_count(), 3);
    }

    #[test]
    fn line_count_handles_empty_and_no_trailing_newline() {
        let empty = CuePreviewState::new(
            String::new(),
            std::path::PathBuf::from("/tmp/test.cue"),
            String::new(),
        );
        assert_eq!(empty.line_count(), 0);

        let no_trailing = CuePreviewState::new(
            "one\ntwo".to_string(),
            std::path::PathBuf::from("/tmp/test.cue"),
            String::new(),
        );
        assert_eq!(no_trailing.line_count(), 2);
    }

    #[test]
    fn new_starts_at_scroll_zero() {
        let s = CuePreviewState::new(
            "x".to_string(),
            std::path::PathBuf::from("/tmp/test.cue"),
            String::new(),
        );
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn begin_edit_seeds_input_with_line_text() {
        let mut s = CuePreviewState::new(
            "FILE \"x.flac\" FLAC\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n".to_string(),
            std::path::PathBuf::from("/tmp/test.cue"),
            String::new(),
        );
        assert!(s.begin_edit(1));
        assert!(s.is_editing());
        assert_eq!(s.cursor, Some(1));
        assert_eq!(s.edit.as_ref().unwrap().text, "  TRACK 01 AUDIO");
    }

    #[test]
    fn begin_edit_out_of_range_returns_false() {
        let mut s = CuePreviewState::new(
            "one\ntwo\n".to_string(),
            std::path::PathBuf::from("/tmp/test.cue"),
            String::new(),
        );
        assert!(!s.begin_edit(99));
        assert!(!s.is_editing());
    }

    #[test]
    fn commit_edit_splices_input_text_into_content() {
        let mut s = CuePreviewState::new(
            "alpha\nbeta\ngamma\n".to_string(),
            std::path::PathBuf::from("/tmp/test.cue"),
            String::new(),
        );
        s.begin_edit(1);
        s.edit.as_mut().unwrap().text = "BETA-EDITED".to_string();
        s.commit_edit();
        assert_eq!(s.content, "alpha\nBETA-EDITED\ngamma\n");
        assert!(!s.is_editing());
        assert_eq!(s.cursor, None);
    }

    #[test]
    fn cancel_edit_does_not_modify_content() {
        let mut s = CuePreviewState::new(
            "alpha\nbeta\n".to_string(),
            std::path::PathBuf::from("/tmp/test.cue"),
            String::new(),
        );
        s.begin_edit(1);
        s.edit.as_mut().unwrap().text = "WOULD-CHANGE".to_string();
        s.cancel_edit();
        assert_eq!(s.content, "alpha\nbeta\n");
        assert!(!s.is_editing());
    }
}

#[cfg(test)]
mod mb_select_state_tests {
    use super::*;
    use crate::tui::musicbrainz::MbRelease;

    fn rel(id: &str) -> MbRelease {
        MbRelease {
            release_id: id.into(),
            title: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn new_starts_at_first_release_no_scroll() {
        let s = MbSelectState::new(
            vec![rel("a"), rel("b"), rel("c")],
            vec![std::path::PathBuf::from("/x.flac")],
        );
        assert_eq!(s.selected, 0);
        assert_eq!(s.scroll, 0);
        assert_eq!(s.releases.len(), 3);
    }

    #[test]
    fn new_with_empty_releases_handles_gracefully() {
        let s = MbSelectState::new(vec![], vec![]);
        assert_eq!(s.releases.len(), 0);
        assert_eq!(s.selected, 0);
    }
}
