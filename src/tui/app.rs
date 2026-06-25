//! Application state for the standalone TUI

use std::path::{Path, PathBuf};

use crate::config::TonepoetConfig;
use crate::convert::formats::AudioFormat;
use crate::convert::simple_wizard::DitherType;
use tonepoet_pipeline::enums::{
    DsdFilterPreset, DsdNoiseShaper, DsdToPcmGainMode, ModulatorOrder,
};
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
    // Opus
    OpusContentType,
    OpusQuality,
    OpusBitrate,
    OpusComplexity,
    // MP3
    Mp3Mode,
    Mp3VbrQuality,
    Mp3Preset,
    Mp3Bitrate,
    // WavPack
    WavPackMode,
    WavPackHybrid,
    WavPackBitrate,
    WavPackCorrection,
    // SSRC
    SsrcAttenuation,
    SsrcMinPhase,
    SsrcDitherId,
    SsrcPdf,
    // Sox (rate effect)
    SoxChebyshev,
    SoxBandwidth,
    SoxPhase,
    SoxAliasing,
    // Sox (sinc FIR pre-filter)
    SoxSincTaps,
    SoxSincAttenuation,
    SoxSincPassband,
    SoxSincTransition,
    SoxSincKaiserBeta,
    SoxSincPhase,
    // Soxr
    SoxrChebyshev,
    SoxrCutoff,
    SoxrPhase,
}



/// Which settings pill requested the format-settings overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatSettingsOpenTarget {
    /// Codec/container settings such as FLAC compression or AAC bitrate.
    Codec,
    /// Resampler-specific settings such as SSRC dither/PDF or Sox phase.
    Resampler,
}

impl FormatSettingsOpenTarget {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Codec => "codec",
            Self::Resampler => "resampler",
        }
    }
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
    Opus {
        content_type: tonepoet_pipeline::enums::OpusContentType,
        quality_preset: Option<usize>,
        bitrate_input: crate::tui::text_input::TextInputState,
        complexity_input: crate::tui::text_input::TextInputState,
    },
    Mp3 {
        mode: tonepoet_pipeline::enums::Mp3Mode,
        vbr_quality_input: crate::tui::text_input::TextInputState,
        quality_preset: Option<usize>,
        bitrate_input: crate::tui::text_input::TextInputState,
    },
    WavPack {
        mode: tonepoet_pipeline::enums::WavPackMode,
        hybrid: bool,
        bitrate_input: crate::tui::text_input::TextInputState,
        correction: bool,
    },
    Ssrc {
        attenuation_input: crate::tui::text_input::TextInputState,
        min_phase: bool,
        dither_id_input: crate::tui::text_input::TextInputState,
        pdf_type_input: crate::tui::text_input::TextInputState,
    },
    Sox {
        chebyshev: bool,
        bandwidth_input: crate::tui::text_input::TextInputState,
        phase_input: crate::tui::text_input::TextInputState,
        allow_aliasing: bool,
        sinc_taps_input: crate::tui::text_input::TextInputState,
        sinc_attenuation_input: crate::tui::text_input::TextInputState,
        sinc_passband_input: crate::tui::text_input::TextInputState,
        sinc_transition_input: crate::tui::text_input::TextInputState,
        sinc_kaiser_beta_input: crate::tui::text_input::TextInputState,
        sinc_phase: Option<tonepoet_pipeline::enums::SoxSincPhase>,
    },
    Soxr {
        chebyshev: bool,
        cutoff_input: crate::tui::text_input::TextInputState,
        phase_input: crate::tui::text_input::TextInputState,
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
    None,
    Sox,
    Ssrc,
    Soxr,
}

/// DSD conversion preset exposed in the format pane.
/// Kept as a local UI enum so labels can stay stable even if pipeline names evolve.
pub type DsdConversionPreset = DsdFilterPreset;

/// DSD-to-PCM gain mode exposed in the format pane.
pub type DsdGainMode = DsdToPcmGainMode;

/// User-editable manual gain range for DSD-to-PCM conversions.
/// Matches `DsdSettings::validate()` so the TUI cannot stage an invalid value.
pub const DSD_TO_PCM_GAIN_DB_MIN: f32 = -24.0;
pub const DSD_TO_PCM_GAIN_DB_MAX: f32 = 24.0;
/// Keyboard step for the manual gain row. Fine enough for mastering-level
/// adjustments while still making large changes practical with repeats.
pub const DSD_TO_PCM_GAIN_DB_STEP: f32 = 0.25;

impl ResamplerChoice {
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Sox => "sox",
            Self::Ssrc => "ssrc",
            Self::Soxr => "soxr",
        }
    }
}

fn is_dsd_format(fmt: AudioFormat) -> bool {
    matches!(fmt, AudioFormat::Dsf | AudioFormat::Dff)
}

fn source_info_is_dsd(info: &SourceInfo) -> bool {
    info.bit_depth == Some(1)
        || tonepoet_pipeline::DsdRate::from_hz(info.sample_rate).is_some()
        || info.codec.to_ascii_lowercase().contains("dsd")
}

fn source_path_is_dsd(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "dsf" | "dff"))
        .unwrap_or(false)
}

/// Read an embedded CUESHEET tag from a FLAC file for Convert-screen preview.
/// Returns None if the file has no embedded cue or lofty can't read it.
fn read_embedded_cuesheet_for_preview(path: &Path) -> Option<crate::tui::cue_parser::CueSheet> {
    use lofty::prelude::*;
    let tagged = lofty::read_from_path(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let cue_text = tag.items()
        .find(|item| {
            matches!(item.key(), lofty::tag::ItemKey::Unknown(key) if key.eq_ignore_ascii_case("CUESHEET"))
        })
        .and_then(|item| item.value().text().map(|s| s.to_string()))?;
    let sheet = crate::tui::cue_parser::parse_cue(&cue_text);
    if sheet.tracks.len() >= 2 { Some(sheet) } else { None }
}


fn is_cue_sheet_path_for_preview(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("cue"))
        .unwrap_or(false)
}

fn should_render_cue_sheet_as_multitrack(
    source_is_cue_path: bool,
    sheet: &crate::tui::cue_parser::CueSheet,
) -> bool {
    if source_is_cue_path {
        // A queued `.cue` is itself the logical source. Preserve CUE semantics,
        // track rendering, and persistent probe notices even for valid one-track
        // sheets; otherwise warnings can fall back to transient Single-source
        // paths. Audio files with embedded/sidecar CUEs keep the historical
        // split-preview behavior unless the sheet actually describes multiple
        // tracks.
        !sheet.tracks.is_empty()
    } else {
        sheet.tracks.len() >= 2
    }
}

/// Result of probing the audio image(s) referenced by a CUE control file.
///
/// The CUE path remains the logical source. `info` is populated only when the
/// referenced image properties are uniform enough to drive format defaults
/// safely. `probe_notice` is surfaced in the source pane/status bar when the
/// app must avoid source-derived defaults.
#[derive(Debug, Clone)]
pub(crate) struct CueProxyProbeResult {
    pub info: Option<SourceInfo>,
    pub metadata: SourceMetadata,
    pub probe_notice: Option<String>,
}

#[cfg(not(test))]
fn cue_proxy_probe_audio_for_preview(path: &Path) -> Result<SourceInfo, String> {
    crate::tui::probe::probe_audio(path)
}

#[cfg(test)]
fn cue_proxy_probe_audio_for_preview(path: &Path) -> Result<SourceInfo, String> {
    CUE_PROXY_PROBE_TEST_HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(hook) = slot.as_mut() {
            hook.probed_paths.push(path.to_path_buf());
            hook.probe_results
                .get(path)
                .cloned()
                .unwrap_or_else(|| Err(format!("unexpected CUE proxy probe path in test: {}", path.display())))
        } else {
            crate::tui::probe::probe_audio(path)
        }
    })
}

#[cfg(not(test))]
fn cue_proxy_read_metadata_for_preview(path: &Path) -> SourceMetadata {
    crate::tui::probe::read_metadata(path).unwrap_or_default()
}

#[cfg(test)]
fn cue_proxy_read_metadata_for_preview(path: &Path) -> SourceMetadata {
    CUE_PROXY_PROBE_TEST_HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(hook) = slot.as_mut() {
            hook.metadata_paths.push(path.to_path_buf());
            hook.metadata_results.get(path).cloned().unwrap_or_default()
        } else {
            crate::tui::probe::read_metadata(path).unwrap_or_default()
        }
    })
}

#[cfg(test)]
#[derive(Default)]
struct CueProxyProbeTestHook {
    probe_results: std::collections::HashMap<PathBuf, Result<SourceInfo, String>>,
    metadata_results: std::collections::HashMap<PathBuf, SourceMetadata>,
    probed_paths: Vec<PathBuf>,
    metadata_paths: Vec<PathBuf>,
}

#[cfg(test)]
thread_local! {
    static CUE_PROXY_PROBE_TEST_HOOK: std::cell::RefCell<Option<CueProxyProbeTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
struct CueProxyProbeTestHookGuard;

#[cfg(test)]
impl Drop for CueProxyProbeTestHookGuard {
    fn drop(&mut self) {
        CUE_PROXY_PROBE_TEST_HOOK.with(|slot| {
            let _ = slot.borrow_mut().take();
        });
    }
}

#[cfg(test)]
fn with_cue_proxy_probe_test_hook<T>(
    hook: CueProxyProbeTestHook,
    f: impl FnOnce() -> T,
) -> (T, CueProxyProbeTestHook) {
    CUE_PROXY_PROBE_TEST_HOOK.with(|slot| {
        assert!(slot.borrow().is_none(), "nested CUE proxy test hook");
        *slot.borrow_mut() = Some(hook);
    });
    let guard = CueProxyProbeTestHookGuard;
    let result = f();
    let hook = CUE_PROXY_PROBE_TEST_HOOK.with(|slot| {
        slot.borrow_mut()
            .take()
            .expect("CUE proxy test hook should still be installed")
    });
    std::mem::forget(guard);
    (result, hook)
}

/// Probe audio image file(s) referenced by a `.cue`, never the CUE text file.
///
/// Single-image CUEs return that image's `SourceInfo`. Track-by-track CUEs
/// probe every unique FILE target and return aggregate duration/size when the
/// codec/container/rate/depth/channel properties are uniform. Mixed,
/// unresolved, ambiguous, or unprobeable references return `info: None` plus a
/// notice so defaults are not silently derived from a guessed 16/44.1 source.
pub(crate) fn probe_cue_proxy_source(cue_path: &Path) -> Result<CueProxyProbeResult, String> {
    let sheet = crate::tui::cue_parser::parse_cue_file(cue_path)
        .map_err(|err| format!("failed to parse CUE: {err}"))?;
    let mut metadata = cue_sheet_metadata(&sheet, SourceMetadata::default());

    if sheet.tracks.is_empty() {
        return Ok(CueProxyProbeResult {
            info: None,
            metadata,
            probe_notice: Some("CUE sheet has no audio tracks".to_string()),
        });
    }

    let parent = cue_path
        .parent()
        .ok_or_else(|| "CUE path has no parent directory".to_string())?;

    let mut file_refs = Vec::<String>::new();
    let mut missing_track_refs = Vec::<u32>::new();
    for track in &sheet.tracks {
        match track.file.as_deref() {
            Some(file_ref) if !file_ref.trim().is_empty() => push_unique_string(&mut file_refs, file_ref),
            _ => missing_track_refs.push(track.number),
        }
    }

    if !missing_track_refs.is_empty() {
        return Ok(CueProxyProbeResult {
            info: None,
            metadata,
            probe_notice: Some(format!(
                "CUE track(s) {} have no FILE reference; set format manually",
                format_u32_list(&missing_track_refs)
            )),
        });
    }

    if file_refs.is_empty() {
        return Ok(CueProxyProbeResult {
            info: None,
            metadata,
            probe_notice: Some("CUE sheet has no FILE references; set format manually".to_string()),
        });
    }

    let mut image_paths = Vec::<PathBuf>::new();
    let mut resolution_errors = Vec::<String>::new();
    for file_ref in &file_refs {
        match crate::tui::browse::resolve_cue_file_reference_for_queue(parent, file_ref) {
            crate::tui::browse::CueReferenceResolution::Resolved(path) => {
                push_unique_path_for_cue_probe(&mut image_paths, path);
            }
            crate::tui::browse::CueReferenceResolution::Missing => {
                resolution_errors.push(format!("FILE {:?} was not found", file_ref));
            }
            crate::tui::browse::CueReferenceResolution::Ambiguous(candidates) => {
                resolution_errors.push(format!(
                    "FILE {:?} was ambiguous: {}",
                    file_ref,
                    format_candidate_paths_for_cue_probe(&candidates)
                ));
            }
        }
    }

    if !resolution_errors.is_empty() {
        return Ok(CueProxyProbeResult {
            info: None,
            metadata,
            probe_notice: Some(format!(
                "{}; set format manually",
                resolution_errors.join("; ")
            )),
        });
    }

    let mut probed = Vec::<(PathBuf, SourceInfo)>::new();
    let mut probe_errors = Vec::<String>::new();
    for image_path in image_paths {
        match cue_proxy_probe_audio_for_preview(&image_path) {
            Ok(info) => {
                probed.push((image_path, info));
            }
            Err(err) => {
                probe_errors.push(format!("{}: {}", image_path.display(), err));
            }
        }
    }

    if !probe_errors.is_empty() {
        return Ok(CueProxyProbeResult {
            info: None,
            metadata,
            probe_notice: Some(format!(
                "CUE image probe failed: {}; set format manually",
                probe_errors.join("; ")
            )),
        });
    }

    if probed.is_empty() {
        return Ok(CueProxyProbeResult {
            info: None,
            metadata,
            probe_notice: Some("CUE FILE references resolved to no images; set format manually".to_string()),
        });
    }

    let mut first_info = probed[0].1.clone();
    let uniform = probed
        .iter()
        .all(|(_, info)| cue_proxy_probe_properties_match(&first_info, info));

    if !uniform {
        return Ok(CueProxyProbeResult {
            info: None,
            metadata,
            probe_notice: Some(format!(
                "mixed source properties across {} CUE image files; set format manually",
                probed.len()
            )),
        });
    }

    // Only the first image's tags are ever merged into the Convert metadata
    // pane. Probe every image for uniformity, but avoid redundant tag reads for
    // multi-file CUEs and skip image-tag reads entirely when the CUE is mixed.
    let first_metadata = cue_proxy_read_metadata_for_preview(&probed[0].0);
    metadata = cue_sheet_metadata(&sheet, first_metadata);

    if probed.len() > 1 {
        first_info.duration_secs = probed.iter().map(|(_, info)| info.duration_secs).sum();
        first_info.file_size = probed.iter().map(|(_, info)| info.file_size).sum();
    }

    Ok(CueProxyProbeResult {
        info: Some(first_info),
        metadata,
        probe_notice: None,
    })
}

fn cue_sheet_metadata(
    sheet: &crate::tui::cue_parser::CueSheet,
    mut metadata: SourceMetadata,
) -> SourceMetadata {
    if metadata.album.is_none() {
        metadata.album = sheet.title.clone();
    }
    if metadata.artist.is_none() {
        metadata.artist = sheet.performer.clone();
    }
    if metadata.genre.is_none() {
        metadata.genre = sheet.genre.clone();
    }
    if metadata.year.is_none() {
        metadata.year = sheet.date.clone();
    }
    if metadata.catalog_number.is_none() {
        metadata.catalog_number = sheet.catalog.clone();
    }
    metadata
}

fn cue_proxy_probe_properties_match(left: &SourceInfo, right: &SourceInfo) -> bool {
    left.format_name == right.format_name
        && left.codec == right.codec
        && left.bit_depth == right.bit_depth
        && left.sample_rate == right.sample_rate
        && left.channels == right.channels
        && left.channel_layout == right.channel_layout
}

fn push_unique_string(values: &mut Vec<String>, candidate: &str) {
    if !values.iter().any(|value| value == candidate) {
        values.push(candidate.to_string());
    }
}

fn push_unique_path_for_cue_probe(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths.iter().any(|existing| same_path_for_cue_probe(existing, &candidate)) {
        paths.push(candidate);
    }
}

fn same_path_for_cue_probe(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn format_candidate_paths_for_cue_probe(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_u32_list(values: &[u32]) -> String {
    values
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(", ")
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
        /// Persistent warning for direct sources whose probe data is unavailable,
        /// especially malformed or empty `.cue` files that cannot be rendered as
        /// MultiTrack but still need a durable "set format manually" notice.
        probe_notice: Option<String>,
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
        /// CUE proxy-probe warning or mixed-properties notice shown in the source pane.
        probe_notice: Option<String>,
        scroll: usize,
        /// Cursor position in the track list (0-based).
        cursor: usize,
        /// Per-track selection (all true initially).
        selected: Vec<bool>,
        /// Full parsed disc model for selected disc-stream sources.
        disc_contents: Option<Box<crate::disc::DiscContents>>,
        /// Selected presentation id to bridge UI stream selection into pipeline source options.
        selected_presentation_id: Option<crate::disc::PresentationId>,
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
        /// Persistent warning for batch-level source probing, especially when
        /// the first batch item is a CUE sheet whose referenced image files
        /// are unresolved, unprobeable, or mixed-property. Unlike the status
        /// line, this stays visible in the source pane until the source changes.
        probe_notice: Option<String>,
        /// Cursor-specific warning from lazy batch preview probing. This is
        /// keyed by path so an async CUE warning cannot appear for the wrong
        /// selected batch item if the cursor moves before the probe completes.
        cursor_probe_notice: Option<(PathBuf, String)>,
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
                    probe_notice: None,
                    cursor_probe_notice: None,
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

    /// Durable probe notice for the currently installed source, when one should
    /// remain visible until the source changes. Cursor-specific batch notices are
    /// rendered by the source pane because they depend on the selected path.
    pub fn persistent_probe_notice(&self) -> Option<&str> {
        match self {
            Self::Single { probe_notice: Some(notice), .. }
            | Self::MultiTrack { probe_notice: Some(notice), .. }
            | Self::Batch { probe_notice: Some(notice), .. } => Some(notice.as_str()),
            _ => None,
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
        Self::from_single_with_probe_notice(path, info, metadata, None)
    }

    /// Build a SourceMode for a single path, optionally carrying a CUE
    /// proxy-probe notice into the MultiTrack source pane.
    pub fn from_single_with_probe_notice(
        path: PathBuf,
        info: Option<SourceInfo>,
        metadata: SourceMetadata,
        probe_notice: Option<String>,
    ) -> Self {
        // DVD-Audio ISO/directory detection. The default path uses stream 0
        // and preserves the presentation id for conversion option mapping.
        if crate::disc::dvda_utils::is_dvda_source(&path) {
            if let Ok(contents) = crate::disc::dvda_utils::map_dvda_source(&path) {
                if !contents.presentations.is_empty() {
                    let meta = crate::tui::disc_browser::metadata_for_disc(&contents);
                    if let Ok(mode) = crate::tui::disc_browser::source_mode_for_presentation(contents, 0, meta) {
                        return mode;
                    }
                }
            }
        }

        // DVD-Video ISO/directory detection. Prefer LPCM presentations,
        // especially those with sidecar metadata already populated.
        if crate::disc::dvdv_utils::is_dvdv_source(&path) {
            if let Ok(contents) = crate::disc::dvdv_utils::map_dvdv_source(&path) {
                if !contents.presentations.is_empty() {
                    let best = contents
                        .presentations
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| {
                            p.format
                                .codec
                                .as_deref()
                                .is_some_and(|c| c.eq_ignore_ascii_case("lpcm"))
                        })
                        .max_by_key(|(_, p)| {
                            // Prefer: (1) has sidecar metadata, (2) stereo,
                            // (3) higher bit depth, (4) more tracks
                            let has_meta = p.album_title.is_some() as u8;
                            let is_stereo = (p.format.channels.unwrap_or(0) <= 2) as u8;
                            let bit_depth = p.format.bit_depth.unwrap_or(0);
                            (has_meta, is_stereo, bit_depth, p.tracks.len())
                        })
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    let meta = crate::tui::disc_browser::metadata_for_disc(&contents);
                    if let Ok(mode) =
                        crate::tui::disc_browser::source_mode_for_presentation(contents, best, meta)
                    {
                        return mode;
                    }
                }
            }
        }

        // Blu-ray ISO/directory detection. Prefer the mapper-selected
        // presentation so audio-only Blu-rays start on the most likely album.
        if crate::disc::bluray_utils::is_bluray_source(&path) {
            if let Ok(contents) = crate::disc::bluray_utils::map_bluray_source(&path) {
                if !contents.presentations.is_empty() {
                    let best =
                        crate::disc::bluray_mapper::best_bluray_presentation_index(&contents)
                            .unwrap_or(0);
                    let meta = crate::tui::disc_browser::metadata_for_disc(&contents);
                    if let Ok(mode) =
                        crate::tui::disc_browser::source_mode_for_presentation(contents, best, meta)
                    {
                        return mode;
                    }
                }
            }
        }

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
                    let disc_model = crate::disc::sacd_mapper::map_sacd_disc(
                        &sacd,
                        sidecar.as_ref(),
                        &path,
                    );
                    return Self::MultiTrack {
                        path,
                        info,
                        metadata: meta,
                        tracks,
                        area_label: Some(area_label.to_string()),
                        album_title,
                        album_artist,
                        probe_notice: None,
                        scroll: 0,
                        cursor: 0,
                        selected: vec![true; track_count],
                        disc_contents: Some(Box::new(disc_model)),
                        selected_presentation_id: Some(crate::disc::PresentationId::SacdArea(
                            if area_label == "Stereo" {
                                crate::disc::SacdAreaId::Stereo
                            } else {
                                crate::disc::SacdAreaId::MultiChannel
                            },
                        )),
                    };
                }
            }
        }

        // CUE detection: parse a queued `.cue` directly. For audio images,
        // prefer embedded CUESHEET metadata, then fall back to a sidecar.
        let source_is_cue_path = is_cue_sheet_path_for_preview(&path);
        let cue_sheet = if source_is_cue_path {
            crate::tui::cue_parser::parse_cue_file(&path).ok()
        } else {
            read_embedded_cuesheet_for_preview(&path).or_else(|| {
                crate::tui::cue_parser::find_sidecar_cue(&path)
                    .and_then(|p| crate::tui::cue_parser::parse_cue_file(&p).ok())
            })
        };
        if let Some(sheet) = cue_sheet {
            if should_render_cue_sheet_as_multitrack(source_is_cue_path, &sheet) {
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
                    probe_notice,
                    scroll: 0,
                    cursor: 0,
                    selected: vec![true; track_count],
                    disc_contents: None,
                    selected_presentation_id: None,
                };
            }
        }

        Self::Single {
            path,
            info,
            metadata,
            probe_notice: if source_is_cue_path { probe_notice } else { None },
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
    /// Audio paths whose sibling sidecar CUE was already evaluated by browse
    /// queue expansion and classified as a metadata artifact. This metadata is
    /// part of the Convert source payload, not process-global state: the user
    /// reviews this exact payload and `:commit` consumes it for these paths.
    ///
    /// Commit maps these paths to `CueSidecarPolicy::EmbeddedOnly` on the
    /// resulting `ConversionItem`, so downstream detection skips sidecar CUE
    /// discovery while still honoring embedded CUESHEET tags.
    pub cue_artifact_audio: std::collections::HashSet<PathBuf>,
}

impl Default for SourceState {
    fn default() -> Self {
        Self {
            mode: SourceMode::Empty,
            advanced_open: false,
            batch_probe_pending: None,
            batch_probe_debounce: None,
            cue_artifact_audio: std::collections::HashSet::new(),
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
    DsdGain,
    /// Manual DSD-to-PCM fixed gain value, edited with left/right controls.
    DsdGainDb,
}

impl FormatField {
    /// Rows visible in the format pane. DSD-to-PCM gain controls are visible
    /// only for DSD sources targeting PCM outputs; they are hidden for PCM->PCM
    /// and PCM->DSD so the UI cannot imply that SoX normalization applies there.
    pub fn visible_rows(is_dsd_target: bool, show_dsd_to_pcm_gain: bool) -> &'static [Self] {
        if is_dsd_target {
            &[
                Self::Format,
                Self::DsdRate,
                Self::BitDepth,
                Self::NoiseShaper,
                Self::ModulatorOrder,
                Self::ConversionPreset,
            ]
        } else if show_dsd_to_pcm_gain {
            &[
                Self::Format,
                Self::SampleRate,
                Self::BitDepth,
                Self::Resampler,
                Self::Dither,
                Self::ReplayGain,
                Self::DsdGain,
                Self::DsdGainDb,
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

    pub fn next_for(self, is_dsd_target: bool, show_dsd_to_pcm_gain: bool) -> Self {
        let rows = Self::visible_rows(is_dsd_target, show_dsd_to_pcm_gain);
        let idx = rows.iter().position(|row| *row == self).unwrap_or(0);
        rows[(idx + 1) % rows.len()]
    }

    pub fn prev_for(self, is_dsd_target: bool, show_dsd_to_pcm_gain: bool) -> Self {
        let rows = Self::visible_rows(is_dsd_target, show_dsd_to_pcm_gain);
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
    pub dsd_gain_mode: PillState<DsdGainMode>,
    /// Fixed DSD-to-PCM gain in dB used when `dsd_gain_mode` is Manual.
    pub dsd_gain_db: f32,
    /// Auto DSD-to-PCM peak-normalization safety margin in dB.
    pub dsd_auto_gain_margin_db: f32,
    /// Whether the currently previewed source is DSD. Drives visibility and
    /// activation of DSD-to-PCM gain controls so they never appear for PCM sources.
    pub source_is_dsd: bool,
    pub field_focus: FormatField,
    pub advanced_open: bool,
    /// False until the user explicitly picks a dither algorithm. Bit-depth changes may update it.
    pub dither_overridden: bool,
    /// False until the user explicitly picks a resampler. Rate/source changes may reset it.
    pub resampler_overridden: bool,
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
    /// Opus content type (Auto, Music, Speech). Default: Auto.
    pub opus_content_type: tonepoet_pipeline::enums::OpusContentType,
    /// Opus quality preset index into OPUS_PRESETS. None = custom.
    pub opus_quality_preset: Option<usize>,
    /// Opus bitrate in kbps (6-510). Default: 192.
    pub opus_bitrate_kbps: u32,
    /// Opus encoder complexity (0-10). Default: 10.
    pub opus_complexity: u8,
    /// MP3 encoding mode (VBR, CBR, ABR). Default: VBR.
    pub mp3_mode: tonepoet_pipeline::enums::Mp3Mode,
    /// MP3 bitrate preset index into MP3_BITRATE_PRESETS. None = custom.
    pub mp3_quality_preset: Option<usize>,
    /// MP3 VBR quality (0-9, 0=best). Default: 0.
    pub mp3_vbr_quality: u8,
    /// MP3 CBR/ABR bitrate in kbps (8-1000). Default: 320.
    pub mp3_bitrate_kbps: u32,
    /// WavPack compression mode. Default: Normal.
    pub wavpack_mode: tonepoet_pipeline::enums::WavPackMode,
    /// WavPack hybrid (lossy) mode. Default: false.
    pub wavpack_hybrid: bool,
    /// WavPack hybrid bitrate in kbps/ch (24-9600). Default: 320.
    pub wavpack_bitrate_kbps: u32,
    /// Write .wvc correction file alongside hybrid .wv. Default: true.
    pub wavpack_correction: bool,
    /// Resampling quality preset. Default: Ultra.
    pub resample_quality: tonepoet_pipeline::enums::ResampleQuality,
    /// SSRC explicit profile override. None = derive from resample_quality.
    /// SSRC output attenuation in dB (0.0-99.9). None = no attenuation.
    pub ssrc_attenuation_db: Option<f32>,
    /// SSRC minimum phase filters. Default: false (linear phase).
    pub ssrc_min_phase: bool,
    /// Explicit SSRC `--dither` ID. None derives from the global dither pill.
    pub ssrc_dither_id: Option<u8>,
    /// SSRC probability distribution function for dithering. None derives from the global dither pill.
    pub ssrc_pdf_type: Option<tonepoet_pipeline::enums::SsrcPdfType>,
    /// Sox chebyshev/steep filter (-s). Default: false.
    pub sox_chebyshev: bool,
    /// Sox bandwidth override (74.0-99.7%). None = derived from NyquistTransition.
    pub sox_bandwidth: Option<f32>,
    /// Sox phase (0-100). None = sox default.
    pub sox_phase: Option<u8>,
    /// Sox allow aliasing (-a). Default: false.
    pub sox_allow_aliasing: bool,
    /// Sox sinc FIR taps (power of 2, 1024-67108864). None = no sinc pre-filter.
    pub sox_sinc_taps: Option<u32>,
    /// Sox sinc attenuation in dB (80-200). None = sox default.
    pub sox_sinc_attenuation: Option<u16>,
    /// Sox sinc passband corner in Hz (1-220000). None = sox default.
    pub sox_sinc_passband: Option<f32>,
    /// Sox sinc transition bandwidth in Hz (1-5000). None = sox default.
    pub sox_sinc_transition: Option<f32>,
    /// Sox sinc Kaiser beta (0-32). None = sox default.
    pub sox_sinc_kaiser_beta: Option<f32>,
    /// Sox sinc phase mode. None = linear (sox default).
    pub sox_sinc_phase: Option<tonepoet_pipeline::enums::SoxSincPhase>,
    /// Soxr chebyshev filter (cheby=1). Default: false.
    pub soxr_chebyshev: bool,
    /// Soxr cutoff override (0.0-1.0). None = derived from NyquistTransition.
    pub soxr_cutoff: Option<f32>,
    /// Soxr phase (0-100). None = soxr default.
    pub soxr_phase: Option<u8>,
}

// MP3 bitrate presets (used by CBR and ABR modes).
pub const MP3_BITRATE_PRESETS: &[(u32, &str)] = &[
    (320, "insane"),
    (256, "high"),
    (192, "standard"),
    (128, "portable"),
    (64, "voice"),
];

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

// Opus quality presets.
pub const OPUS_PRESETS: &[(u32, &str)] = &[
    (320, "insane"),
    (192, "high"),
    (128, "standard"),
    (96, "portable"),
    (64, "voice"),
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
            (ResamplerChoice::None, "none"),
            (ResamplerChoice::Soxr, "soxr"),
            (ResamplerChoice::Sox, "sox"),
            (ResamplerChoice::Ssrc, "ssrc"),
        ]);

        let dither = PillState::new(vec![
            (DitherType::None, "none"),
            (DitherType::TPDF, "TPDF"),
            (DitherType::Shibata, "Shibata"),
            (DitherType::LowShibata, "Low-Shibata"),
            (DitherType::HighShibata, "High-Shibata"),
            (DitherType::Gesemann, "Gesemann"),
            (DitherType::Lipshitz, "Lipshitz"),
        ]);

        let replaygain = PillState::new(vec![
            (ReplayGainChoice::Both, "both"),
            (ReplayGainChoice::Album, "album"),
            (ReplayGainChoice::Track, "track"),
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

        let dsd_gain_mode = PillState::new(vec![
            (DsdGainMode::Disabled, "off"),
            (DsdGainMode::Auto, "auto"),
            (DsdGainMode::Manual, "manual"),
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
            dsd_gain_mode,
            dsd_gain_db: 0.0,
            dsd_auto_gain_margin_db: 0.15,
            source_is_dsd: false,
            field_focus: FormatField::Format,
            advanced_open: false,
            dither_overridden: false,
            resampler_overridden: false,
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
            opus_content_type: tonepoet_pipeline::enums::OpusContentType::Auto,
            opus_quality_preset: Some(1), // "high" = index 1 (192 kbps)
            opus_bitrate_kbps: 192,
            opus_complexity: 10,
            mp3_mode: tonepoet_pipeline::enums::Mp3Mode::Vbr,
            mp3_quality_preset: Some(0), // "insane" = index 0 (320 kbps)
            mp3_vbr_quality: 0,
            mp3_bitrate_kbps: 320,
            wavpack_mode: tonepoet_pipeline::enums::WavPackMode::Normal,
            wavpack_hybrid: false,
            wavpack_bitrate_kbps: 320,
            wavpack_correction: true,
            resample_quality: tonepoet_pipeline::enums::ResampleQuality::Ultra,
            ssrc_attenuation_db: None,
            ssrc_min_phase: false,
            ssrc_dither_id: None,
            ssrc_pdf_type: None,
            sox_chebyshev: false,
            sox_bandwidth: None,
            sox_phase: None,
            sox_allow_aliasing: false,
            sox_sinc_taps: None,
            sox_sinc_attenuation: None,
            sox_sinc_passband: None,
            sox_sinc_transition: None,
            sox_sinc_kaiser_beta: None,
            sox_sinc_phase: None,
            soxr_chebyshev: false,
            soxr_cutoff: None,
            soxr_phase: None,
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

    /// True only for the conversion direction where this control has meaning:
    /// DSD source material rendered to a PCM target format.
    pub fn dsd_to_pcm_gain_available(&self) -> bool {
        self.source_is_dsd && !self.is_dsd_selected()
    }

    /// True when the SSRC overlay is overriding any part of the dither pair
    /// derived from the global dither pill.
    pub fn ssrc_dither_override_active(&self) -> bool {
        matches!(*self.resampler.selected_value(), ResamplerChoice::Ssrc)
            && (self.ssrc_dither_id.is_some() || self.ssrc_pdf_type.is_some())
    }

    /// True when the selected global dither pill will be translated to an
    /// SSRC-native approximation rather than emitted as the named shaper.
    ///
    /// Overlay overrides are not considered approximations here because the
    /// user has explicitly selected SSRC-native `--dither`/`--pdf` values.
    pub fn ssrc_dither_approximation_active(&self) -> bool {
        matches!(*self.resampler.selected_value(), ResamplerChoice::Ssrc)
            && !self.ssrc_dither_override_active()
            && selected_global_dither_needs_ssrc_approximation(*self.dither.selected_value())
    }

    /// True when SSRC is selected and the global dither pill would derive an
    /// SSRC shaper ID that is unavailable for the selected destination rate.
    ///
    /// Explicit SSRC overlay overrides take precedence, and float output skips
    /// dither/noise-shaping emission entirely.
    pub fn ssrc_dither_invalid_for_selected_rate(&self) -> bool {
        matches!(*self.resampler.selected_value(), ResamplerChoice::Ssrc)
            && !self.ssrc_dither_override_active()
            && !self.bit_depth.selected_value().is_float()
            && !selected_global_ssrc_dither_valid_for_rate(
                *self.dither.selected_value(),
                *self.sample_rate.selected_value(),
            )
    }

    /// Short suffix for the dither row in the format pane.
    pub fn ssrc_dither_status_label(&self) -> Option<&'static str> {
        if self.ssrc_dither_override_active() {
            Some("ssrc override")
        } else if self.ssrc_dither_invalid_for_selected_rate() {
            Some("ssrc invalid")
        } else if self.ssrc_dither_approximation_active() {
            Some("ssrc approx")
        } else {
            None
        }
    }

    pub fn set_source_is_dsd(&mut self, source_is_dsd: bool) {
        self.source_is_dsd = source_is_dsd;
        self.apply_format_constraints();
    }

    pub fn focus_next(&mut self) {
        self.field_focus = self
            .field_focus
            .next_for(self.is_dsd_selected(), self.dsd_to_pcm_gain_available());
    }

    pub fn focus_prev(&mut self) {
        self.field_focus = self
            .field_focus
            .prev_for(self.is_dsd_selected(), self.dsd_to_pcm_gain_available());
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
    pub fn select_focused_next(&mut self, source_bits: Option<u32>, source_rate: Option<u32>) {
        let before_depth = *self.bit_depth.selected_value();
        let before_format = *self.format.selected_value();
        let focused = self.field_focus;
        self.focused_pill_mut().select_next();
        self.after_user_selection(focused, before_format, before_depth, source_bits, source_rate);
    }

    /// Select the previous enabled pill in the focused row and run row-specific side effects.
    pub fn select_focused_prev(&mut self, source_bits: Option<u32>, source_rate: Option<u32>) {
        let before_depth = *self.bit_depth.selected_value();
        let before_format = *self.format.selected_value();
        let focused = self.field_focus;
        self.focused_pill_mut().select_prev();
        self.after_user_selection(focused, before_format, before_depth, source_bits, source_rate);
    }

    /// Select a concrete pill index for mouse handlers and run row-specific side effects.
    pub fn select_row_index(&mut self, row: FormatField, index: usize, source_bits: Option<u32>, source_rate: Option<u32>) {
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
            FormatField::DsdGain => select_enabled_index(&mut self.dsd_gain_mode, index),
            FormatField::DsdGainDb => {
                // Clicking/focusing the value row makes Manual explicit;
                // keyboard left/right then adjusts the staged dB value.
                self.dsd_gain_mode.select_value(&DsdGainMode::Manual);
                self.dsd_gain_db = clamp_dsd_to_pcm_gain_db(self.dsd_gain_db);
            }
        }
        self.after_user_selection(row, before_format, before_depth, source_bits, source_rate);
    }

    fn after_user_selection(
        &mut self,
        row: FormatField,
        before_format: AudioFormat,
        before_depth: BitDepthChoice,
        source_bits: Option<u32>,
        source_rate: Option<u32>,
    ) {
        if row == FormatField::Dither {
            self.mark_dither_overridden();
        }
        if row == FormatField::Resampler {
            self.resampler_overridden = true;
        }

        if row == FormatField::Format && before_format != *self.format.selected_value() {
            self.selected_container_index = 0;
            self.resampler_overridden = false;
            self.apply_format_constraints();
            if self.is_dsd_selected() {
                self.dither.select_value(&DitherType::None);
                self.cascade_dsd_rate_defaults();
            } else {
                if !self.dither_overridden {
                    self.apply_auto_dither(source_bits);
                }
                self.apply_auto_resampler(source_rate);
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

        if row == FormatField::SampleRate {
            self.resampler_overridden = false;
            self.apply_auto_resampler(source_rate);
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

    /// Auto-select resampler based on source vs target sample rate.
    /// Called when source changes, target rate changes, or format changes.
    pub fn apply_auto_resampler(&mut self, source_rate: Option<u32>) {
        if self.resampler_overridden || self.is_dsd_selected() {
            return;
        }
        let target_rate = *self.sample_rate.selected_value();
        if source_rate == Some(target_rate) || source_rate.is_none() {
            // Same rate or unknown source → no resampling needed
            self.resampler.select_value(&ResamplerChoice::None);
        } else {
            // Rate change → default to soxr
            self.resampler.select_value(&ResamplerChoice::Soxr);
        }
    }

    /// Clear source-derived choices when the newly installed source has no
    /// reliable probe info. This keeps a failed, unresolved, or mixed CUE proxy
    /// probe from inheriting sample-rate, bit-depth, dither, resampler, or DSD
    /// source-side effects from the previously viewed source. Codec/container
    /// choices and explicit codec settings are preserved because they are user
    /// output preferences, not facts derived from the source.
    pub fn clear_source_derived_defaults(&mut self) {
        self.source_is_dsd = false;
        self.dither_overridden = false;
        self.resampler_overridden = false;
        self.dsd_gain_mode.select_value(&DsdGainMode::Disabled);
        self.dsd_gain_db = 0.0;

        if self.is_dsd_selected() {
            self.sample_rate.select_value(&2_822_400);
            self.dither.select_value(&DitherType::None);
            self.resampler.select_value(&ResamplerChoice::None);
            self.apply_format_constraints();
            self.cascade_dsd_rate_defaults();
            return;
        }

        self.sample_rate.select_value(&44_100);
        self.bit_depth.select_value(&BitDepthChoice::Int16);
        self.dither.select_value(&DitherType::None);
        self.resampler.select_value(&ResamplerChoice::None);
        self.apply_format_constraints();
        self.apply_auto_dither(None);
        self.apply_auto_resampler(None);
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
        self.dsd_gain_mode.set_all_enabled(self.dsd_to_pcm_gain_available());

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
            self.dsd_gain_mode.set_all_enabled(false);
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

        // DSD sources always use sox for decode — resampler pill is irrelevant.
        if self.source_is_dsd {
            self.resampler.set_all_enabled(false);
        }

        self.clamp_disabled_selections();
        if !FormatField::visible_rows(self.is_dsd_selected(), self.dsd_to_pcm_gain_available())
            .contains(&self.field_focus)
        {
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
        clamp_pill(&mut self.dsd_gain_mode);
        self.dsd_gain_db = clamp_dsd_to_pcm_gain_db(self.dsd_gain_db);
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
            FormatField::DsdGain => FocusedPill::DsdGain(&mut self.dsd_gain_mode),
            FormatField::DsdGainDb => FocusedPill::DsdGainDb {
                gain_db: &mut self.dsd_gain_db,
                gain_mode: &mut self.dsd_gain_mode,
            },
        }
    }
}

fn clamp_dsd_to_pcm_gain_db(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(DSD_TO_PCM_GAIN_DB_MIN, DSD_TO_PCM_GAIN_DB_MAX)
    } else {
        0.0
    }
}

fn step_dsd_to_pcm_gain_db(value: &mut f32, delta: f32) {
    let next = clamp_dsd_to_pcm_gain_db(*value + delta);
    // Keep repeated key presses idempotent with respect to display precision and
    // avoid accumulating binary float noise in staged settings.
    *value = (next * 100.0).round() / 100.0;
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

fn selected_global_dither_needs_ssrc_approximation(dither: DitherType) -> bool {
    // SSRC can represent no dither and unshaped triangular PDF directly.
    // Every other global pill names a SoX/SoXR-family shaper that SSRC does
    // not expose by name and therefore maps to an SSRC-native approximation.
    !matches!(dither, DitherType::None | DitherType::TPDF)
}

fn selected_global_ssrc_dither_valid_for_rate(dither: DitherType, target_rate_hz: u32) -> bool {
    match dither {
        // These map to sample-rate-independent SSRC IDs 98/99.
        DitherType::None | DitherType::TPDF | DitherType::SloppedTPDF => true,
        // These map to SSRC ATH Curve A intensities. SSRC only publishes ATH A
        // tables for the rates below, and the pipeline clamps requested
        // intensity to the strongest ATH A ID available at that rate.
        DitherType::LowShibata
        | DitherType::Shibata
        | DitherType::HighShibata
        | DitherType::Lipshitz
        | DitherType::FWeighted
        | DitherType::ModifiedEWeighted
        | DitherType::ImprovedEWeighted
        | DitherType::Gesemann => matches!(
            target_rate_hz,
            44_100 | 48_000 | 88_200 | 96_000 | 192_000 | 8_000 | 11_025 | 22_050
        ),
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
    DsdGain(&'a mut PillState<DsdGainMode>),
    DsdGainDb {
        gain_db: &'a mut f32,
        gain_mode: &'a mut PillState<DsdGainMode>,
    },
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
            Self::DsdGain(p) => p.select_next(),
            Self::DsdGainDb { gain_db, gain_mode } => {
                (*gain_mode).select_value(&DsdGainMode::Manual);
                step_dsd_to_pcm_gain_db(*gain_db, DSD_TO_PCM_GAIN_DB_STEP);
            }
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
            Self::DsdGain(p) => p.select_prev(),
            Self::DsdGainDb { gain_db, gain_mode } => {
                (*gain_mode).select_value(&DsdGainMode::Manual);
                step_dsd_to_pcm_gain_db(*gain_db, -DSD_TO_PCM_GAIN_DB_STEP);
            }
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
        let source_is_dsd = self
            .source
            .mode
            .current_info()
            .map(source_info_is_dsd)
            .or_else(|| self.source.mode.current_path().map(|path| source_path_is_dsd(path)))
            .unwrap_or(false);
        self.format.set_source_is_dsd(source_is_dsd);
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
    /// For sources without reliable probe info, clears source-derived controls
    /// so stale defaults from a previous source cannot survive the source swap.
    pub fn apply_source_defaults(&mut self) {
        let Some(info) = self.source.mode.current_info() else {
            self.format.clear_source_derived_defaults();
            return;
        };
        let source_rate = info.sample_rate;
        let source_bits = info.bit_depth;
        let source_is_float = info.codec.contains("Float");
        let is_dsd_source = source_info_is_dsd(info);
        self.format.set_source_is_dsd(is_dsd_source);

        if is_dsd_source {
            if !self.format.is_dsd_selected() {
                self.format.cascade_dsd_source_to_pcm_defaults(source_rate);
            }
        } else {
            self.format.cascade_pcm_source_defaults(source_rate, source_bits, source_is_float);
        }
        self.format.dither_overridden = false;
        self.format.apply_auto_dither(source_bits);
        self.format.resampler_overridden = false;
        self.format.apply_auto_resampler(Some(source_rate));
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
    /// Modal browser for DVD-Audio/SACD presentations.
    DiscBrowser(Box<crate::tui::disc_browser::DiscBrowserState>),
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
        /// `None` = controls mode. `Some(scroll)` = help mode with scroll offset.
        help_scroll: Option<usize>,
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

/// Active content pane inside the metadata editor. Presentation selection
/// remains separate: disc-backed editors keep `presentation_tabs` for data
/// management, while this enum controls which read/edit surface is visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentTab {
    Metadata,
    Details,
    ReplayGain,
    Artwork,
}

impl ContentTab {
    pub const COUNT: usize = 4;

    pub const ALL: [ContentTab; Self::COUNT] = [
        ContentTab::Metadata,
        ContentTab::Details,
        ContentTab::ReplayGain,
        ContentTab::Artwork,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ContentTab::Metadata => "Metadata",
            ContentTab::Details => "Details",
            ContentTab::ReplayGain => "ReplayGain",
            ContentTab::Artwork => "Artwork",
        }
    }

    pub fn index(self) -> usize {
        match self {
            ContentTab::Metadata => 0,
            ContentTab::Details => 1,
            ContentTab::ReplayGain => 2,
            ContentTab::Artwork => 3,
        }
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }
}

/// Immutable source/file facts captured when the metadata editor opens.
///
/// Invariants:
/// - `FileFacts.path` is the canonical key used by editable rows and cached facts.
/// - `file_facts.len() == paths.len()` for every active file-backed presentation.
/// - Rendering consumes these facts only; it never performs filesystem, tag, or media probe I/O.
#[derive(Debug, Clone, Default)]
pub struct FileFacts {
    pub path: std::path::PathBuf,
    pub file_size: Option<u64>,
    pub modified: Option<std::time::SystemTime>,
    pub filesystem_error: Option<String>,
    pub tool: Option<String>,
    pub read_state: FileReadState,
    pub write_eligibility: FileWriteEligibility,
}

/// Result of the initial tag/readability phase for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileReadState {
    Readable,
    Unreadable { reason: String },
    Unsupported { reason: String },
}

impl Default for FileReadState {
    fn default() -> Self {
        Self::Readable
    }
}

/// Whether the current editor session may write this file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileWriteEligibility {
    Writable,
    Blocked { reason: String },
    Unknown { reason: String },
}

impl Default for FileWriteEligibility {
    fn default() -> Self {
        Self::Writable
    }
}

impl FileWriteEligibility {
    pub fn is_writable(&self) -> bool {
        matches!(self, Self::Writable)
    }

    pub fn block_reason(&self) -> Option<&str> {
        match self {
            Self::Writable => None,
            Self::Blocked { reason } | Self::Unknown { reason } => Some(reason.as_str()),
        }
    }
}

/// Compact media facts produced by audio probing.
///
/// This is separated from source/tag facts because probing can be slow and may
/// fail independently from tag reads. The Details tab requests it explicitly.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaFacts {
    pub format_name: String,
    pub codec: String,
    pub bit_depth: Option<u32>,
    pub sample_rate: u32,
    pub channels: u32,
    pub channel_layout: String,
    pub duration_secs: f64,
    pub file_size: u64,
}

impl From<SourceInfo> for MediaFacts {
    fn from(info: SourceInfo) -> Self {
        Self {
            format_name: info.format_name,
            codec: info.codec,
            bit_depth: info.bit_depth,
            sample_rate: info.sample_rate,
            channels: info.channels,
            channel_layout: info.channel_layout,
            duration_secs: info.duration_secs,
            file_size: info.file_size,
        }
    }
}

impl From<&SourceInfo> for MediaFacts {
    fn from(info: &SourceInfo) -> Self {
        Self {
            format_name: info.format_name.clone(),
            codec: info.codec.clone(),
            bit_depth: info.bit_depth,
            sample_rate: info.sample_rate,
            channels: info.channels,
            channel_layout: info.channel_layout.clone(),
            duration_secs: info.duration_secs,
            file_size: info.file_size,
        }
    }
}

impl From<MediaFacts> for SourceInfo {
    fn from(facts: MediaFacts) -> Self {
        Self {
            format_name: facts.format_name,
            codec: facts.codec,
            bit_depth: facts.bit_depth,
            sample_rate: facts.sample_rate,
            channels: facts.channels,
            channel_layout: facts.channel_layout,
            duration_secs: facts.duration_secs,
            file_size: facts.file_size,
        }
    }
}

/// Explicit lifecycle for one file's media probe.
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeState {
    NotLoaded,
    Loading { generation: u64 },
    Ready(MediaFacts),
    Failed { reason: String, retryable: bool },
    Cancelled { generation: u64 },
}

impl Default for ProbeState {
    fn default() -> Self {
        Self::NotLoaded
    }
}

impl ProbeState {
    pub fn needs_probe(&self) -> bool {
        matches!(self, Self::NotLoaded)
    }

    pub fn failed_reason(&self) -> Option<&str> {
        match self {
            Self::Failed { reason, .. } => Some(reason.as_str()),
            _ => None,
        }
    }
}

/// Artwork facts from the tag-read phase. Raw bytes are intentionally not kept.
#[derive(Debug, Clone, Default)]
pub struct ArtworkFacts {
    pub applicable: bool,
    pub entries: Vec<crate::tui::probe::ArtworkInfo>,
}

/// First-class issue reporting for read-only tabs and save eligibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataIssue {
    Filesystem { path: std::path::PathBuf, reason: String },
    TagRead { path: std::path::PathBuf, reason: String },
    Unsupported { path: std::path::PathBuf, reason: String },
    Probe { path: std::path::PathBuf, reason: String, retryable: bool },
    SaveBlocked { path: std::path::PathBuf, reason: String },
    Write { path: std::path::PathBuf, reason: String },
}

static METADATA_EDITOR_DETAILS_SESSION_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// Allocate a process-unique Details probe session id.
///
/// This id is stamped onto each metadata-editor Details cache and echoed by
/// background probe completion messages. It prevents stale async work from a
/// closed/reopened editor, or from another presentation surface, from mutating
/// the wrong editor instance when generations happen to match.
pub fn next_metadata_editor_details_session_id() -> u64 {
    METADATA_EDITOR_DETAILS_SESSION_COUNTER.fetch_add(
        1,
        std::sync::atomic::Ordering::Relaxed,
    )
}

/// One completed audio probe result for a metadata-editor Details load.
#[derive(Debug, Clone)]
pub struct MetadataDetailsProbeFileResult {
    pub index: usize,
    pub path: std::path::PathBuf,
    pub result: Result<SourceInfo, String>,
}

/// Typed outcome for one file in a metadata-editor save request.
///
/// The reducer uses this to distinguish successful writes from skipped files
/// and real write failures. Status text is a summary only; durable per-file
/// issues are attached to `MetadataFileDetails` for failed/skipped paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataEditorWriteOutcome {
    Saved,
    Failed { reason: String },
    Skipped { reason: String },
}

/// Path-keyed result from the async metadata-editor save worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataEditorWriteResult {
    pub path: std::path::PathBuf,
    pub outcome: MetadataEditorWriteOutcome,
}

impl MetadataEditorWriteResult {
    pub fn saved(path: std::path::PathBuf) -> Self {
        Self { path, outcome: MetadataEditorWriteOutcome::Saved }
    }

    pub fn failed(path: std::path::PathBuf, reason: impl Into<String>) -> Self {
        Self { path, outcome: MetadataEditorWriteOutcome::Failed { reason: reason.into() } }
    }

    pub fn skipped(path: std::path::PathBuf, reason: impl Into<String>) -> Self {
        Self { path, outcome: MetadataEditorWriteOutcome::Skipped { reason: reason.into() } }
    }

    pub fn into_legacy_result(self) -> (std::path::PathBuf, Result<(), String>) {
        match self.outcome {
            MetadataEditorWriteOutcome::Saved => (self.path, Ok(())),
            MetadataEditorWriteOutcome::Failed { reason }
            | MetadataEditorWriteOutcome::Skipped { reason } => (self.path, Err(reason)),
        }
    }
}

/// Structured summary returned by save-result reduction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataEditorWriteSummary {
    pub saved: usize,
    pub failed: usize,
    pub skipped: usize,
    pub ignored: usize,
    /// True when save-result reduction leaves pending model changes behind.
    ///
    /// A save can return only successful path-keyed write results while the
    /// editor still has dirty non-file-aligned state, such as a presentation-
    /// scoped or CUESHEET row-level delete that cannot be proven from per-file
    /// write results. In that case the event loop must keep the editor open.
    pub remaining_dirty: bool,
    pub saved_paths: Vec<std::path::PathBuf>,
    pub first_problem: Option<String>,
}

impl MetadataEditorWriteSummary {
    pub fn all_saved(&self) -> bool {
        self.failed == 0 && self.skipped == 0 && self.ignored == 0 && !self.remaining_dirty
    }

    pub fn status_line(&self) -> String {
        if self.all_saved() {
            return format!(
                "Metadata saved ({} file{})",
                self.saved,
                if self.saved == 1 { "" } else { "s" },
            );
        }

        let mut parts = vec![format!("{} saved", self.saved)];
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        if self.skipped > 0 {
            parts.push(format!("{} skipped", self.skipped));
        }
        if self.ignored > 0 {
            parts.push(format!("{} stale/unknown ignored", self.ignored));
        }
        if self.remaining_dirty {
            parts.push("unsaved changes remain".to_string());
        }
        match &self.first_problem {
            Some(problem) if !problem.trim().is_empty() => {
                format!("Metadata: {} — {}", parts.join(", "), problem)
            }
            _ => format!("Metadata: {}", parts.join(", ")),
        }
    }
}

/// Explicit lifecycle for the Details tab's expensive stream-probe data.
///
/// Filesystem stats and tag/artwork metadata are available at editor-open time.
/// Audio stream probing is intentionally separate because it can be slow or
/// fail on malformed media. Keep that work off the open path, off the render
/// path, and out of status-message generation.
#[derive(Debug, Clone)]
pub enum MetadataDetailsProbeState {
    Unloaded,
    Loading {
        generation: u64,
        completed: usize,
        total: usize,
    },
    Ready,
    Partial { issues: Vec<String> },
    Cancelled { generation: u64 },
}

impl Default for MetadataDetailsProbeState {
    fn default() -> Self {
        Self::Unloaded
    }
}


/// Cached, display-oriented filesystem/probe data for one selected audio file.
///
/// Invariant: this struct has a single source of truth for each fact:
/// - path/size/mtime/readability/writeability live only in `file_facts`;
/// - codec/duration/sample-rate probe data lives only in `media_facts`;
/// - compact embedded artwork metadata lives only in `artwork_facts`;
/// - user-visible read/probe/save problems live only in `issues`.
/// Rendering and editing code must not reintroduce mirror fields here.
#[derive(Debug, Clone, Default)]
pub struct MetadataFileDetails {
    /// Stable source facts captured at editor-open time.
    pub file_facts: FileFacts,
    /// Lazy media-probe facts. This is the authoritative probe state used by
    /// Details rendering and AppMessage reduction.
    pub media_facts: ProbeState,
    /// Compact artwork metadata from the tag-read phase.
    pub artwork_facts: ArtworkFacts,
    /// First-class issues for Details/Artwork/save UI.
    pub issues: Vec<MetadataIssue>,
}

impl MetadataFileDetails {
    pub fn from_open_cache(
        path: std::path::PathBuf,
        file_size: Option<u64>,
        modified: Option<std::time::SystemTime>,
        filesystem_error: Option<String>,
        metadata_error: Option<String>,
        read_state: FileReadState,
        write_eligibility: FileWriteEligibility,
        metadata: SourceMetadata,
    ) -> Self {
        let mut file_facts = FileFacts {
            path: path.clone(),
            file_size,
            modified,
            filesystem_error: filesystem_error.clone(),
            tool: metadata.tool.clone(),
            read_state,
            write_eligibility,
        };
        if matches!(file_facts.read_state, FileReadState::Unsupported { .. }) {
            file_facts.write_eligibility = FileWriteEligibility::Blocked {
                reason: file_facts
                    .read_state
                    .block_reason()
                    .unwrap_or("unsupported file")
                    .to_string(),
            };
        }
        let artwork_facts = ArtworkFacts {
            applicable: true,
            entries: metadata.artwork,
        };
        let issues = metadata_issues_from_facts(
            &file_facts,
            metadata_error.as_deref(),
            None,
        );
        Self {
            file_facts,
            media_facts: ProbeState::NotLoaded,
            artwork_facts,
            issues,
        }
    }

    /// Constructor name for call sites that are reducing the initial tag-read
    /// result into durable file facts. It delegates to `from_open_cache` so
    /// read/write eligibility, artwork facts, and first-class issues are
    /// initialized consistently.
    pub fn from_read_result(
        path: std::path::PathBuf,
        file_size: Option<u64>,
        modified: Option<std::time::SystemTime>,
        filesystem_error: Option<String>,
        metadata_error: Option<String>,
        read_state: FileReadState,
        write_eligibility: FileWriteEligibility,
        metadata: SourceMetadata,
    ) -> Self {
        Self::from_open_cache(
            path,
            file_size,
            modified,
            filesystem_error,
            metadata_error,
            read_state,
            write_eligibility,
            metadata,
        )
    }

    pub fn set_probe_ready(&mut self, info: SourceInfo) {
        self.media_facts = ProbeState::Ready(MediaFacts::from(&info));
        self.issues.retain(|issue| !matches!(issue, MetadataIssue::Probe { .. }));
    }

    pub fn set_probe_failed(&mut self, reason: String, retryable: bool) {
        self.media_facts = ProbeState::Failed {
            reason: reason.clone(),
            retryable,
        };
        self.issues.retain(|issue| !matches!(issue, MetadataIssue::Probe { .. }));
        self.issues.push(MetadataIssue::Probe {
            path: self.file_facts.path.clone(),
            reason,
            retryable,
        });
    }
}

impl FileReadState {
    pub fn is_readable(&self) -> bool {
        matches!(self, Self::Readable)
    }

    pub fn block_reason(&self) -> Option<&str> {
        match self {
            Self::Readable => None,
            Self::Unreadable { reason } | Self::Unsupported { reason } => Some(reason.as_str()),
        }
    }
}

impl FileFacts {
    pub fn save_block_reason(&self) -> Option<&str> {
        self.read_state
            .block_reason()
            .or_else(|| self.write_eligibility.block_reason())
    }
}

fn metadata_issues_from_facts(
    file_facts: &FileFacts,
    metadata_error: Option<&str>,
    probe_error: Option<&str>,
) -> Vec<MetadataIssue> {
    let mut issues = Vec::new();
    let path = &file_facts.path;
    if let Some(reason) = file_facts
        .filesystem_error
        .as_deref()
        .filter(|reason| !reason.trim().is_empty())
    {
        issues.push(MetadataIssue::Filesystem {
            path: path.clone(),
            reason: reason.trim().to_string(),
        });
    }
    match &file_facts.read_state {
        FileReadState::Readable => {}
        FileReadState::Unreadable { reason } => issues.push(MetadataIssue::TagRead {
            path: path.clone(),
            reason: reason.clone(),
        }),
        FileReadState::Unsupported { reason } => issues.push(MetadataIssue::Unsupported {
            path: path.clone(),
            reason: reason.clone(),
        }),
    }
    if let Some(reason) = metadata_error.filter(|reason| !reason.trim().is_empty()) {
        let already_reported_read_state = matches!(
            file_facts.read_state,
            FileReadState::Unreadable { .. } | FileReadState::Unsupported { .. }
        );
        if !already_reported_read_state {
            issues.push(MetadataIssue::TagRead {
                path: path.clone(),
                reason: reason.trim().to_string(),
            });
        }
    }
    if let Some(reason) = probe_error.filter(|reason| !reason.trim().is_empty()) {
        issues.push(MetadataIssue::Probe {
            path: path.clone(),
            reason: reason.trim().to_string(),
            retryable: true,
        });
    }
    if let Some(reason) = file_facts.save_block_reason() {
        issues.push(MetadataIssue::SaveBlocked {
            path: path.clone(),
            reason: reason.to_string(),
        });
    }
    issues
}

/// Cached technical data for a disc presentation. Disc-backed editors often
/// synthesize per-track paths from a single ISO/directory, so ordinary file
/// probes cannot represent the active presentation. Store the presentation
/// format explicitly instead.
#[derive(Debug, Clone, Default)]
pub struct DiscTechnicalDetails {
    pub presentation_label: String,
    pub track_count: usize,
    pub duration_secs: Option<f64>,
    pub codec: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    pub bit_depth: Option<u32>,
    pub lossless: Option<bool>,
    pub tool: Option<String>,
}

/// Cached data used by the metadata editor Details and Artwork tabs.
#[derive(Debug, Clone)]
pub struct MetadataTechnicalDetails {
    /// Unique identity for this Details cache. Background probe completions
    /// must echo this value and match it before mutating any file facts.
    pub session_id: u64,
    pub files: Vec<MetadataFileDetails>,
    pub disc: Option<DiscTechnicalDetails>,
    pub details_probe_state: MetadataDetailsProbeState,
    pub details_probe_generation: u64,
    /// Monotonic id for async save dispatches on this editor surface.
    pub save_generation: u64,
    /// The save generation currently allowed to reduce into this surface.
    pub active_save_generation: Option<u64>,
}

impl Default for MetadataTechnicalDetails {
    fn default() -> Self {
        Self {
            session_id: next_metadata_editor_details_session_id(),
            files: Vec::new(),
            disc: None,
            details_probe_state: MetadataDetailsProbeState::Unloaded,
            details_probe_generation: 0,
            save_generation: 0,
            active_save_generation: None,
        }
    }
}

impl MetadataTechnicalDetails {
    pub fn from_files(files: Vec<MetadataFileDetails>) -> Self {
        Self {
            session_id: next_metadata_editor_details_session_id(),
            files,
            disc: None,
            details_probe_state: MetadataDetailsProbeState::Unloaded,
            details_probe_generation: 0,
            save_generation: 0,
            active_save_generation: None,
        }
    }

    pub fn from_disc(disc: DiscTechnicalDetails) -> Self {
        Self {
            session_id: next_metadata_editor_details_session_id(),
            files: Vec::new(),
            disc: Some(disc),
            details_probe_state: MetadataDetailsProbeState::Ready,
            details_probe_generation: 0,
            save_generation: 0,
            active_save_generation: None,
        }
    }

    /// Start an async save for this editor surface and return the identity
    /// that the completion message must echo before it can mutate state.
    pub fn begin_write(&mut self) -> (u64, u64) {
        let generation = self.save_generation.saturating_add(1);
        self.save_generation = generation;
        self.active_save_generation = Some(generation);
        (self.session_id, generation)
    }

    pub fn cancel_details_probe(&mut self) {
        let generation = match &self.details_probe_state {
            MetadataDetailsProbeState::Loading { generation, .. } => Some(*generation),
            _ => None,
        };
        if let Some(generation) = generation {
            for file in &mut self.files {
                if matches!(file.media_facts, ProbeState::Loading { generation: g } if g == generation) {
                    file.media_facts = ProbeState::Cancelled { generation };
                }
            }
            self.details_probe_state = MetadataDetailsProbeState::Cancelled { generation };
        }
    }

    /// Clear only failed Details probe results so an explicit user retry can
    /// recover from transient I/O errors without discarding successfully cached
    /// stream data. Normal Details entry remains idempotent; this is the
    /// intentional escape hatch for sticky failures.
    pub fn retry_failed_details_probes(&mut self) -> usize {
        self.cancel_details_probe();
        let mut cleared = 0usize;
        for file in &mut self.files {
            let had_failed_probe = matches!(file.media_facts, ProbeState::Failed { .. } | ProbeState::Cancelled { .. });
            if had_failed_probe {
                file.media_facts = ProbeState::NotLoaded;
                file.issues.retain(|issue| !matches!(issue, MetadataIssue::Probe { .. }));
                cleared = cleared.saturating_add(1);
            }
        }
        if cleared > 0 || matches!(self.details_probe_state, MetadataDetailsProbeState::Cancelled { .. }) {
            self.details_probe_state = MetadataDetailsProbeState::Unloaded;
        }
        cleared
    }

    pub fn details_probe_issue_count(&self) -> usize {
        match &self.details_probe_state {
            MetadataDetailsProbeState::Partial { issues } => issues.len(),
            _ => self.files.iter().filter(|file| matches!(file.media_facts, ProbeState::Failed { .. })).count(),
        }
    }

    /// Reduce a completed background Details probe into ordinary editor state.
    ///
    /// Invariant: worker results enter the editor only through this transition
    /// after an `AppMessage`; rendering and status assembly never poll worker
    /// state or mutate probe fields. Stale generations are ignored.
    pub fn apply_details_probe_results(
        &mut self,
        session_id: u64,
        generation: u64,
        results: Vec<MetadataDetailsProbeFileResult>,
    ) -> Option<String> {
        if self.session_id != session_id {
            return None;
        }
        let MetadataDetailsProbeState::Loading { generation: active_generation, .. } = self.details_probe_state else {
            return None;
        };
        if active_generation != generation {
            return None;
        }

        let mut loaded = 0usize;
        let mut issues = Vec::new();
        for item in results {
            let Some(file) = self.files.get_mut(item.index) else {
                let reason = if item.path.as_os_str().is_empty() {
                    format!("probe result index {} is not part of this editor", item.index)
                } else {
                    format!(
                        "probe result for '{}' used stale index {}",
                        item.path.display(),
                        item.index
                    )
                };
                issues.push(reason);
                continue;
            };
            if file.file_facts.path != item.path {
                issues.push(format!(
                    "ignored stale probe result for '{}' at index {}; current file is '{}'",
                    item.path.display(),
                    item.index,
                    file.file_facts.path.display()
                ));
                continue;
            }
            match item.result {
                Ok(info) => {
                    file.set_probe_ready(info);
                    loaded = loaded.saturating_add(1);
                }
                Err(reason) => {
                    issues.push(reason.clone());
                    file.set_probe_failed(reason, true);
                }
            }
        }

        // A completed worker message must leave no file stuck in this
        // generation's Loading state. Missing/path-mismatched results become
        // retryable probe issues rather than invisible indefinite loading.
        for file in &mut self.files {
            if matches!(file.media_facts, ProbeState::Loading { generation: g } if g == generation) {
                let reason = format!(
                    "probe result missing for '{}' in Details session {} generation {}",
                    file.file_facts.path.display(),
                    session_id,
                    generation
                );
                issues.push(reason.clone());
                file.set_probe_failed(reason, true);
            }
        }

        if issues.is_empty() {
            self.details_probe_state = MetadataDetailsProbeState::Ready;
            Some(format!(
                "metadata editor: Details ready ({} file{})",
                loaded,
                if loaded == 1 { "" } else { "s" }
            ))
        } else {
            let issue_count = issues.len();
            self.details_probe_state = MetadataDetailsProbeState::Partial { issues };
            Some(format!(
                "metadata editor: Details partially loaded ({} ok, {} issue{})",
                loaded,
                issue_count,
                if issue_count == 1 { "" } else { "s" }
            ))
        }
    }

    pub fn from_disc_presentation(
        presentation_label: String,
        track_count: usize,
        duration_secs: Option<f64>,
        format: &crate::disc::model::AudioPresentationFormat,
    ) -> Self {
        Self::from_disc(DiscTechnicalDetails {
            presentation_label,
            track_count,
            duration_secs: duration_secs.filter(|duration| duration.is_finite() && *duration > 0.0),
            codec: format.codec.clone(),
            sample_rate: format.sample_rate,
            channels: format.channels.map(u32::from),
            channel_layout: format.channel_layout.clone(),
            bit_depth: format.bit_depth,
            lossless: Some(format.lossless),
            tool: None,
        })
    }
}

/// One presentation surface inside the metadata editor.
///
/// Invariant: this struct owns a presentation's editable rows, labels, dirty
/// state, and read-only facts. For disc-backed editors the active editing
/// surface is the `PresentationTab` stored directly in
/// `MetadataEditorModel.presentation_tabs[active_tab]`; no top-level active
/// clone exists.
#[derive(Debug, Clone)]
pub struct PresentationTab {
    pub id: crate::disc::model::PresentationId,
    pub label: String,
    pub paths: Vec<std::path::PathBuf>,
    pub entries: Vec<crate::tui::probe::TagEntry>,
    pub file_labels: Vec<String>,
    pub deleted: Vec<usize>,
    pub dirty: bool,
    /// Cached Details/Artwork data for this presentation.
    pub technical_details: MetadataTechnicalDetails,
    pub sacd_area_kind: Option<crate::tui::sacd::AreaKind>,
    pub sacd_stereo_durations: Option<Vec<f64>>,
    pub sacd_multi_channel_durations: Option<Vec<f64>>,
    /// DVD-Video-only: per-track source chapter numbers for the active presentation.
    /// This keeps persisted TOML source identity independent from display labels.
    pub dvdv_source_chapters: Option<Vec<u16>>,
    /// DVD-Video-only: per-chapter durations (seconds) for the active presentation.
    pub dvdv_track_durations: Option<Vec<f64>>,
    /// DVD-Video-only: selected camera angle when the title has multiple angles.
    /// Single-angle titles keep this as `None` so generated TOML stays sparse.
    pub dvdv_angle_number: Option<u8>,
    /// DVD-Video-only: number of authored angles for this title when known.
    /// Values greater than one make angle identity mandatory.
    pub dvdv_title_angle_count: Option<u8>,
    /// Blu-ray-only: authored playlist number for this presentation.
    pub bluray_playlist_number: Option<u32>,
    /// Blu-ray-only: authored audio PID for this presentation.
    pub bluray_audio_pid: Option<u16>,
    /// Blu-ray-only: zero-based authored audio stream index.
    pub bluray_audio_stream_index: Option<u8>,
    /// Blu-ray-only: one-based display angle selected for this presentation.
    pub bluray_angle_number: Option<u8>,
    /// Blu-ray-only: per-chapter durations (seconds) for MusicBrainz TOC synthesis.
    pub bluray_chapter_durations: Option<Vec<f64>>,
}

impl Default for PresentationTab {
    fn default() -> Self {
        Self {
            id: crate::disc::model::PresentationId::DvdAudioGroup(0),
            label: String::new(),
            paths: Vec::new(),
            entries: Vec::new(),
            file_labels: Vec::new(),
            deleted: Vec::new(),
            dirty: false,
            technical_details: MetadataTechnicalDetails::default(),
            sacd_area_kind: None,
            sacd_stereo_durations: None,
            sacd_multi_channel_durations: None,
            dvdv_source_chapters: None,
            dvdv_track_durations: None,
            dvdv_angle_number: None,
            dvdv_title_angle_count: None,
            bluray_playlist_number: None,
            bluray_audio_pid: None,
            bluray_audio_stream_index: None,
            bluray_angle_number: None,
            bluray_chapter_durations: None,
        }
    }
}

impl PresentationTab {
    /// Create a file-backed or presentation-backed editor surface with all
    /// invariant-carrying fields initialized in one place.
    pub fn new(
        id: crate::disc::model::PresentationId,
        label: impl Into<String>,
        paths: Vec<std::path::PathBuf>,
        entries: Vec<crate::tui::probe::TagEntry>,
        file_labels: Vec<String>,
        technical_details: MetadataTechnicalDetails,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            paths,
            entries,
            file_labels,
            technical_details,
            ..Self::default()
        }
    }

    /// Convenience constructor for the ordinary audio-file editor surface.
    pub fn for_files(
        paths: Vec<std::path::PathBuf>,
        entries: Vec<crate::tui::probe::TagEntry>,
        file_labels: Vec<String>,
        technical_details: MetadataTechnicalDetails,
    ) -> Self {
        Self::new(
            crate::disc::model::PresentationId::DvdAudioGroup(0),
            String::new(),
            paths,
            entries,
            file_labels,
            technical_details,
        )
    }

    /// Capture the active surface of an editor state as a presentation tab.
    /// This keeps disc builders from re-spelling every presentation field and
    /// preserves the authoritative model invariant as new per-surface facts are
    /// added.
    pub fn from_editor_state(
        id: crate::disc::model::PresentationId,
        label: impl Into<String>,
        state: &MetadataEditorState,
    ) -> Self {
        let active = state.active_surface();
        let mut tab = Self::new(
            id,
            label,
            active.paths.clone(),
            active.entries.clone(),
            active.file_labels.clone(),
            active.technical_details.clone(),
        );
        tab.deleted = active.deleted.clone();
        tab.dirty = active.dirty;
        tab.sacd_area_kind = active.sacd_area_kind;
        tab.sacd_stereo_durations = active.sacd_stereo_durations.clone();
        tab.sacd_multi_channel_durations = active.sacd_multi_channel_durations.clone();
        tab.dvdv_source_chapters = active.dvdv_source_chapters.clone();
        tab.dvdv_track_durations = active.dvdv_track_durations.clone();
        tab.dvdv_angle_number = active.dvdv_angle_number;
        tab.dvdv_title_angle_count = active.dvdv_title_angle_count;
        tab.bluray_playlist_number = active.bluray_playlist_number;
        tab.bluray_audio_pid = active.bluray_audio_pid;
        tab.bluray_audio_stream_index = active.bluray_audio_stream_index;
        tab.bluray_angle_number = active.bluray_angle_number;
        tab.bluray_chapter_durations = active.bluray_chapter_durations.clone();
        tab
    }
}

/// Authoritative metadata-editor model.
///
/// `MetadataEditorState` is only the application overlay wrapper. This model is
/// the source of truth for editable metadata, presentation selection, cached
/// source/media/artwork facts, issue state, and UI interaction state.
///
/// Invariants:
/// - Active editable data is always the active `PresentationTab`: `file_surface`
///   for plain file editors, or `presentation_tabs[active_tab]` for disc-backed
///   editors.
/// - There are no top-level mirrored `paths`/`entries`/`technical_details`
///   fields outside this model.
/// - `active_tab < presentation_tabs.len()` whenever `presentation_tabs` is
///   non-empty; accessors clamp defensively for stale test fixtures.
/// - Rendering never performs filesystem, tag, media-probe, or save I/O.
/// - Background worker results enter via explicit reduction methods.
/// - Read-only tab scroll values are clamped before they are stored.
#[derive(Debug, Clone)]
pub struct MetadataEditorModel {
    /// Active file-backed surface when the editor is not presentation-backed.
    pub file_surface: PresentationTab,
    /// Presentation-backed surfaces. When non-empty, the active editable surface
    /// is `presentation_tabs[active_tab]`; no separate active copy exists.
    pub presentation_tabs: Vec<PresentationTab>,
    pub active_tab: usize,

    pub cursor: usize,
    pub scroll: usize,
    pub content_tab: ContentTab,
    pub content_tab_scrolls: [usize; ContentTab::COUNT],
    pub last_click: Option<(usize, std::time::Instant)>,
    pub edit_input: Option<crate::tui::text_input::TextInputState>,
    pub add_key_input: Option<crate::tui::text_input::TextInputState>,
    pub phase: MetadataEditorPhase,
    pub detail_field_idx: usize,
    pub detail_cursor: usize,
    pub detail_scroll: usize,
    pub detail_edit: Option<crate::tui::text_input::TextInputState>,
    pub mb_back: Option<MbBackCache>,
    pub gnudb_back: Option<Box<GnudbReviewState>>,
    pub read_only: bool,
    pub sacd_sidecar_path: Option<std::path::PathBuf>,
    pub presentation_selector_open: bool,
    pub presentation_selector_cursor: usize,
    pub presentation_selector_scroll: usize,
}

impl Default for MetadataEditorModel {
    fn default() -> Self {
        Self {
            file_surface: PresentationTab::default(),
            presentation_tabs: Vec::new(),
            active_tab: 0,
            cursor: 0,
            scroll: 0,
            content_tab: ContentTab::Metadata,
            content_tab_scrolls: [0; ContentTab::COUNT],
            last_click: None,
            edit_input: None,
            add_key_input: None,
            phase: MetadataEditorPhase::Editing,
            detail_field_idx: 0,
            detail_cursor: 0,
            detail_scroll: 0,
            detail_edit: None,
            mb_back: None,
            gnudb_back: None,
            read_only: false,
            sacd_sidecar_path: None,
            presentation_selector_open: false,
            presentation_selector_cursor: 0,
            presentation_selector_scroll: 0,
        }
    }
}

impl MetadataEditorModel {
    pub fn single_surface(file_surface: PresentationTab) -> Self {
        Self {
            file_surface,
            ..Self::default()
        }
    }

    pub fn with_presentations(presentation_tabs: Vec<PresentationTab>, active_tab: usize) -> Self {
        let active_tab = active_tab.min(presentation_tabs.len().saturating_sub(1));
        Self {
            presentation_tabs,
            active_tab,
            presentation_selector_cursor: active_tab,
            presentation_selector_scroll: active_tab,
            ..Self::default()
        }
    }

    pub fn active_surface(&self) -> &PresentationTab {
        if self.presentation_tabs.is_empty() {
            &self.file_surface
        } else {
            let idx = self.active_tab.min(self.presentation_tabs.len().saturating_sub(1));
            &self.presentation_tabs[idx]
        }
    }

    pub fn active_surface_mut(&mut self) -> &mut PresentationTab {
        if self.presentation_tabs.is_empty() {
            &mut self.file_surface
        } else {
            let idx = self.active_tab.min(self.presentation_tabs.len().saturating_sub(1));
            &mut self.presentation_tabs[idx]
        }
    }

    /// Apply a background Details probe completion to the matching editor
    /// surface, not merely to whatever presentation is currently active.
    ///
    /// Invariant: session id, generation, and per-result path identity must all
    /// match before probe facts are reduced into state. This prevents stale work
    /// from a closed/reopened editor or another presentation from mutating the
    /// wrong file facts.
    pub fn apply_details_probe_results(
        &mut self,
        session_id: u64,
        generation: u64,
        results: Vec<MetadataDetailsProbeFileResult>,
    ) -> Option<String> {
        if self.presentation_tabs.is_empty() {
            if self.file_surface.technical_details.session_id == session_id {
                return self
                    .file_surface
                    .technical_details
                    .apply_details_probe_results(session_id, generation, results);
            }
            return None;
        }

        if let Some(idx) = self
            .presentation_tabs
            .iter()
            .position(|tab| tab.technical_details.session_id == session_id)
        {
            return self.presentation_tabs[idx]
                .technical_details
                .apply_details_probe_results(session_id, generation, results);
        }

        None
    }
}

impl std::ops::Deref for MetadataEditorState {
    type Target = MetadataEditorModel;

    fn deref(&self) -> &Self::Target {
        &self.model
    }
}

impl std::ops::DerefMut for MetadataEditorState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.model
    }
}


/// State for the metadata editor overlay.
///
/// Metadata editor overlay state.
///
/// This wrapper connects the authoritative `MetadataEditorModel` to the rest of
/// the TUI overlay system. Rendering consumes model facts only; it must not
/// perform filesystem, tag, media-probe, or save I/O.
///
/// Invariants:
/// - `model.file_surface` or `model.presentation_tabs[active_tab]` is the only
///   active editable source.
/// - `per_file_values.len() == paths.len()` for every file-backed editable row.
/// - `active_tab < presentation_tabs.len()` whenever `presentation_tabs` is non-empty.
/// - rendering never performs filesystem, tag, media-probe, or save I/O.
/// - read-only tab scroll values are clamped before storage.
#[derive(Debug, Clone)]
pub struct MetadataEditorState {
    /// Authoritative metadata editor model.
    ///
    /// All active editable rows, selected presentation state, source facts,
    /// media/artwork facts, issues, and UI tab/scroll state live in this model.
    /// `MetadataEditorState` is now only the overlay wrapper used by the
    /// surrounding application; it no longer owns mirrored
    /// `paths`/`entries`/`technical_details`/presentation fields. Active-surface
    /// ownership is intentionally explicit through `active_surface()` and
    /// `active_surface_mut()`; the remaining state-level `Deref` is compatibility
    /// for model-level UI fields only.
    pub model: MetadataEditorModel,
}


fn read_only_max_scroll(total_lines: usize, visible_rows: usize) -> usize {
    total_lines.saturating_sub(visible_rows.max(1))
}

impl MetadataEditorState {
    pub fn from_model(model: MetadataEditorModel) -> Self {
        Self { model }
    }

    pub fn for_files(
        paths: Vec<std::path::PathBuf>,
        entries: Vec<crate::tui::probe::TagEntry>,
        file_labels: Vec<String>,
        technical_details: MetadataTechnicalDetails,
    ) -> Self {
        Self::from_model(MetadataEditorModel::single_surface(PresentationTab::for_files(
            paths,
            entries,
            file_labels,
            technical_details,
        )))
    }

    pub fn for_disc_presentations(presentation_tabs: Vec<PresentationTab>, active_tab: usize) -> Self {
        Self::from_model(MetadataEditorModel::with_presentations(presentation_tabs, active_tab))
    }

    /// Explicit access to the active editable surface.
    ///
    /// Use this instead of relying on `Deref` to make the ownership path clear:
    /// `MetadataEditorState -> MetadataEditorModel -> active PresentationTab`.
    pub fn active_surface(&self) -> &PresentationTab {
        self.model.active_surface()
    }

    /// Explicit mutable access to the active editable surface.
    ///
    /// All edits to rows, file labels, deleted rows, dirty state, and per-surface
    /// media facts should go through this method or a reducer-style model method.
    pub fn active_surface_mut(&mut self) -> &mut PresentationTab {
        self.model.active_surface_mut()
    }

    /// Recompute dirty state for the active surface from authoritative row data.
    pub fn recompute_active_dirty(&mut self) -> bool {
        let dirty = crate::tui::probe::metadata_editor_has_changes(self);
        self.active_surface_mut().dirty = dirty;
        dirty
    }

    /// Explicit access to the authoritative editor model for model-level UI state.
    pub fn editor_model(&self) -> &MetadataEditorModel {
        &self.model
    }

    /// Explicit mutable access to the authoritative editor model.
    pub fn editor_model_mut(&mut self) -> &mut MetadataEditorModel {
        &mut self.model
    }

    /// Replace the file-backed surface with presentation-backed surfaces while
    /// preserving model-level editor state such as read-only mode and sidecar
    /// target. Active editable data then resolves through `active_surface()`.
    pub fn set_presentation_surfaces(
        &mut self,
        presentation_tabs: Vec<PresentationTab>,
        active_tab: usize,
    ) {
        let active_tab = active_tab.min(presentation_tabs.len().saturating_sub(1));
        self.model.presentation_tabs = presentation_tabs;
        self.model.active_tab = active_tab;
        self.model.presentation_selector_cursor = active_tab;
        self.model.presentation_selector_scroll = active_tab;
        self.model.presentation_selector_open = false;
    }

    pub fn shows_presentation_control(&self) -> bool {
        !self.presentation_tabs.is_empty()
    }

    pub fn has_multiple_presentations(&self) -> bool {
        self.presentation_tabs.len() > 1
    }


    pub fn set_content_tab(&mut self, tab: ContentTab) -> bool {
        if self.content_tab == tab {
            return false;
        }
        self.save_active_content_tab_scroll();
        self.content_tab = tab;
        self.restore_active_content_tab_scroll();
        self.reset_content_tab_interaction();
        true
    }

    pub fn set_content_tab_by_index(&mut self, index: usize) -> bool {
        ContentTab::from_index(index)
            .map(|tab| self.set_content_tab(tab))
            .unwrap_or(false)
    }

    pub fn next_content_tab(&mut self) -> bool {
        let next = (self.content_tab.index() + 1) % ContentTab::ALL.len();
        self.set_content_tab(ContentTab::ALL[next])
    }

    pub fn previous_content_tab(&mut self) -> bool {
        let index = self.content_tab.index();
        let next = if index == 0 {
            ContentTab::ALL.len() - 1
        } else {
            index - 1
        };
        self.set_content_tab(ContentTab::ALL[next])
    }

    fn save_active_content_tab_scroll(&mut self) {
        let idx = self.content_tab.index();
        self.content_tab_scrolls[idx] = self.scroll;
    }

    fn restore_active_content_tab_scroll(&mut self) {
        self.scroll = self.content_tab_scrolls[self.content_tab.index()];
        if self.content_tab == ContentTab::Metadata {
            // The active row may have moved while another content tab was shown
            // (for example after presentation switches or command-populated
            // edits). Without viewport dimensions here, enforce the safe half of
            // the invariant: the scroll window must never start after the
            // selected row. Input handlers call their viewport-aware
            // `ensure_cursor_visible` after user-driven tab changes.
            self.cursor = self.cursor.min(self.active_surface().entries.len());
            if self.scroll > self.cursor {
                self.scroll = self.cursor;
            }
            self.content_tab_scrolls[ContentTab::Metadata.index()] = self.scroll;
        }
    }

    fn reset_content_tab_interaction(&mut self) {
        self.last_click = None;
        self.edit_input = None;
        self.add_key_input = None;
        self.detail_edit = None;
        self.phase = MetadataEditorPhase::Editing;
    }

    pub fn scroll_read_only_content_by(
        &mut self,
        delta: isize,
        total_lines: usize,
        visible_rows: usize,
    ) -> bool {
        if self.content_tab == ContentTab::Metadata {
            return false;
        }

        let max_scroll = read_only_max_scroll(total_lines, visible_rows);
        let old = self.scroll;
        let current = old.min(max_scroll);
        let step = delta.checked_abs().unwrap_or(isize::MAX) as usize;
        let next = if delta < 0 {
            current.saturating_sub(step)
        } else {
            current.saturating_add(step).min(max_scroll)
        };
        let idx = self.content_tab.index();
        self.scroll = next;
        self.content_tab_scrolls[idx] = next;
        self.scroll != old
    }

    pub fn set_read_only_content_scroll(
        &mut self,
        scroll: usize,
        total_lines: usize,
        visible_rows: usize,
    ) -> bool {
        if self.content_tab == ContentTab::Metadata {
            return false;
        }

        let max_scroll = read_only_max_scroll(total_lines, visible_rows);
        let next = scroll.min(max_scroll);
        let changed = self.scroll != next;
        let idx = self.content_tab.index();
        self.scroll = next;
        self.content_tab_scrolls[idx] = next;
        changed
    }

    pub fn clamp_read_only_content_scroll(
        &mut self,
        total_lines: usize,
        visible_rows: usize,
    ) -> bool {
        if self.content_tab == ContentTab::Metadata {
            return false;
        }

        self.set_read_only_content_scroll(self.scroll, total_lines, visible_rows)
    }

    pub fn open_presentation_selector(&mut self) -> bool {
        if !self.shows_presentation_control() {
            return false;
        }
        self.presentation_selector_open = true;
        self.presentation_selector_cursor = self.active_tab.min(self.presentation_tabs.len() - 1);
        self.presentation_selector_scroll = self.presentation_selector_cursor;
        true
    }

    pub fn close_presentation_selector(&mut self) {
        self.presentation_selector_open = false;
        self.presentation_selector_cursor = self
            .active_tab
            .min(self.presentation_tabs.len().saturating_sub(1));
        self.presentation_selector_scroll = self
            .presentation_selector_scroll
            .min(self.presentation_tabs.len().saturating_sub(1));
    }

    pub fn move_presentation_selector_cursor(&mut self, delta: isize) -> bool {
        if !self.shows_presentation_control() {
            return false;
        }
        let len = self.presentation_tabs.len();
        let old = self.presentation_selector_cursor.min(len - 1);
        let step = delta.checked_abs().unwrap_or(isize::MAX) as usize;
        let next = if delta < 0 {
            old.saturating_sub(step)
        } else {
            old.saturating_add(step).min(len - 1)
        };
        self.presentation_selector_cursor = next;
        if next < self.presentation_selector_scroll {
            self.presentation_selector_scroll = next;
        }
        next != old
    }

    pub fn set_presentation_selector_cursor(&mut self, next: usize) -> bool {
        if !self.shows_presentation_control() || next >= self.presentation_tabs.len() {
            return false;
        }
        let changed = self.presentation_selector_cursor != next;
        self.presentation_selector_cursor = next;
        if next < self.presentation_selector_scroll {
            self.presentation_selector_scroll = next;
        }
        changed
    }

    pub fn scroll_presentation_selector(&mut self, delta: isize, visible_rows: usize) -> bool {
        if !self.shows_presentation_control() || visible_rows == 0 {
            return false;
        }
        let len = self.presentation_tabs.len();
        let visible_rows = visible_rows.min(len).max(1);
        let max_scroll = len.saturating_sub(visible_rows);
        let old_scroll = self.presentation_selector_scroll.min(max_scroll);
        let old_cursor = self.presentation_selector_cursor.min(len - 1);
        let step = delta.checked_abs().unwrap_or(isize::MAX) as usize;
        let next_scroll = if delta < 0 {
            old_scroll.saturating_sub(step)
        } else {
            old_scroll.saturating_add(step).min(max_scroll)
        };
        self.presentation_selector_scroll = next_scroll;

        let last_visible = next_scroll
            .saturating_add(visible_rows)
            .saturating_sub(1)
            .min(len - 1);
        self.presentation_selector_cursor = old_cursor.clamp(next_scroll, last_visible);

        self.presentation_selector_scroll != old_scroll
            || self.presentation_selector_cursor != old_cursor
    }

    pub fn select_presentation_selector_cursor(&mut self) -> bool {
        if !self.shows_presentation_control() {
            return false;
        }
        let next = self.presentation_selector_cursor.min(self.presentation_tabs.len() - 1);
        self.presentation_selector_open = false;
        self.switch_presentation_tab(next)
    }

    pub fn active_presentation_label(&self) -> Option<&str> {
        self.presentation_tabs
            .get(self.active_tab)
            .map(|tab| tab.label.as_str())
    }

    pub fn any_presentation_dirty(&self) -> bool {
        self.active_surface().dirty || self.presentation_tabs.iter().any(|tab| tab.dirty)
    }

    /// The active presentation is authoritative in-place.
    ///
    /// Older versions cloned top-level active fields back into
    /// `presentation_tabs`. This model stores edits directly in the active
    /// presentation surface, so there is no reconciliation step.
    pub fn active_presentation_is_authoritative(&self) -> bool {
        true
    }



    pub fn switch_presentation_tab(&mut self, next: usize) -> bool {
        if next >= self.presentation_tabs.len() || next == self.active_tab {
            return false;
        }

        // The active editable surface is `presentation_tabs[active_tab]`;
        // switching presentations changes the index only. No active copy is
        // cloned out or reconciled back.
        self.active_tab = next;
        self.cursor = self.cursor.min(self.active_surface().entries.len());
        self.content_tab_scrolls = [0; ContentTab::COUNT];
        let ct_idx = self.content_tab.index();
        self.scroll = if self.content_tab == ContentTab::Metadata {
            self.cursor
        } else {
            0
        };
        self.content_tab_scrolls[ct_idx] = self.scroll;
        self.last_click = None;
        self.edit_input = None;
        self.add_key_input = None;
        self.detail_field_idx = 0;
        self.detail_cursor = 0;
        self.detail_scroll = 0;
        self.detail_edit = None;
        self.phase = MetadataEditorPhase::Editing;
        self.presentation_selector_open = false;
        self.presentation_selector_cursor = self.active_tab;
        self.presentation_selector_scroll = self
            .presentation_selector_scroll
            .min(self.presentation_tabs.len().saturating_sub(1));
        true
    }


    pub fn next_presentation_tab(&mut self) -> bool {
        if self.presentation_tabs.len() <= 1 {
            return false;
        }
        let next = (self.active_tab + 1) % self.presentation_tabs.len();
        self.switch_presentation_tab(next)
    }

    pub fn previous_presentation_tab(&mut self) -> bool {
        if self.presentation_tabs.len() <= 1 {
            return false;
        }
        let next = if self.active_tab == 0 {
            self.presentation_tabs.len() - 1
        } else {
            self.active_tab - 1
        };
        self.switch_presentation_tab(next)
    }

    pub fn mark_active_presentation_saved(&mut self) {
        mark_presentation_tab_saved(self.model.active_surface_mut());
    }

    pub fn mark_presentation_tabs_saved(&mut self, saved_indices: &[usize]) {
        if self.presentation_tabs.is_empty() {
            if !saved_indices.is_empty() {
                mark_presentation_tab_saved(&mut self.model.file_surface);
            }
            return;
        }

        for &idx in saved_indices {
            if let Some(tab) = self.presentation_tabs.get_mut(idx) {
                mark_presentation_tab_saved(tab);
            }
        }
    }

    pub fn dirty_presentation_count(&mut self) -> usize {
        if self.presentation_tabs.is_empty() {
            if self.active_surface().dirty { 1 } else { 0 }
        } else {
            self.presentation_tabs.iter().filter(|tab| tab.dirty).count()
        }
    }

    pub fn active_presentation_is_dirty(&mut self) -> bool {
        self.presentation_tabs
            .get(self.active_tab)
            .map(|tab| tab.dirty)
            .unwrap_or_else(|| self.active_surface().dirty)
    }

    pub fn mark_all_presentations_saved(&mut self) {
        if self.presentation_tabs.is_empty() {
            mark_presentation_tab_saved(&mut self.model.file_surface);
        } else {
            for tab in &mut self.presentation_tabs {
                mark_presentation_tab_saved(tab);
            }
        }
    }

    pub fn apply_active_musicbrainz_values_to_matching_presentations(&mut self) -> usize {
        if self.presentation_tabs.len() <= 1 {
            return 0;
        }
        let Some(active) = self.presentation_tabs.get(self.active_tab).cloned() else {
            return 0;
        };
        let track_count = active.paths.len();
        let active_tab = self.active_tab;
        let mut changed_tabs = 0usize;
        for (idx, tab) in self.presentation_tabs.iter_mut().enumerate() {
            if idx == active_tab || tab.paths.len() != track_count {
                continue;
            }
            let copied = copy_musicbrainz_entries_preserving_originals(
                &active.entries,
                &mut tab.entries,
                track_count,
            );
            if copied == 0 {
                continue;
            }
            tab.deleted.clear();
            tab.dirty = true;
            changed_tabs += 1;
        }
        changed_tabs
    }

    pub fn apply_details_probe_results(
        &mut self,
        session_id: u64,
        generation: u64,
        results: Vec<MetadataDetailsProbeFileResult>,
    ) -> Option<String> {
        self.model
            .apply_details_probe_results(session_id, generation, results)
    }

    /// Start an async metadata write for the active surface. The returned
    /// `(session_id, save_generation)` must be copied into the completion
    /// message and matched before any result can reduce into this editor.
    pub fn begin_write(&mut self) -> (u64, u64) {
        self.model.active_surface_mut().technical_details.begin_write()
    }

    /// Reduce save results into the matching editor surface.
    ///
    /// Invariant: async write completions must match the active editor session
    /// and save generation before applying; stale sessions/generations cannot
    /// close or mutate another editor. Save reduction updates model state only
    /// for files that actually saved.
    pub fn apply_write_results(
        &mut self,
        session_id: u64,
        save_generation: u64,
        results: Vec<MetadataEditorWriteResult>,
    ) -> Option<MetadataEditorWriteSummary> {
        if self.presentation_tabs.is_empty() {
            if self.model.file_surface.technical_details.session_id != session_id {
                return None;
            }
            return apply_write_results_to_tab(
                &mut self.model.file_surface,
                save_generation,
                results,
            );
        }

        let idx = self
            .presentation_tabs
            .iter()
            .position(|tab| tab.technical_details.session_id == session_id)?;
        apply_write_results_to_tab(&mut self.presentation_tabs[idx], save_generation, results)
    }
}

fn mark_presentation_tab_saved(tab: &mut PresentationTab) {
    tab.dirty = false;
    for entry in &mut tab.entries {
        mark_tag_entry_saved(entry);
    }
    tab.deleted.clear();
}

fn mark_tag_entry_saved(entry: &mut crate::tui::probe::TagEntry) {
    entry.per_file_originals = entry.per_file_values.clone();
    entry.original = entry.value.clone();
    entry.mb_proposed_value = None;
    entry.mb_proposed_per_file = None;
}

fn apply_write_results_to_tab(
    tab: &mut PresentationTab,
    save_generation: u64,
    results: Vec<MetadataEditorWriteResult>,
) -> Option<MetadataEditorWriteSummary> {
    if tab.technical_details.active_save_generation != Some(save_generation) {
        return None;
    }
    tab.technical_details.active_save_generation = None;

    let path_to_index: std::collections::HashMap<std::path::PathBuf, usize> = tab
        .paths
        .iter()
        .cloned()
        .enumerate()
        .map(|(idx, path)| (path, idx))
        .collect();

    let mut summary = MetadataEditorWriteSummary::default();
    let mut saved_slots = std::collections::BTreeSet::new();

    for result in results {
        let Some(&idx) = path_to_index.get(&result.path) else {
            summary.ignored = summary.ignored.saturating_add(1);
            if summary.first_problem.is_none() {
                summary.first_problem = Some(format!(
                    "ignored stale save result for '{}'",
                    result.path.display()
                ));
            }
            continue;
        };

        match result.outcome {
            MetadataEditorWriteOutcome::Saved => {
                summary.saved = summary.saved.saturating_add(1);
                summary.saved_paths.push(result.path.clone());
                saved_slots.insert(idx);
                if let Some(file) = tab.technical_details.files.get_mut(idx) {
                    file.issues.retain(|issue| !matches!(issue, MetadataIssue::Write { .. }));
                }
            }
            MetadataEditorWriteOutcome::Failed { reason } => {
                summary.failed = summary.failed.saturating_add(1);
                if summary.first_problem.is_none() {
                    summary.first_problem = Some(reason.clone());
                }
                attach_write_issue(tab, idx, MetadataIssue::Write {
                    path: result.path,
                    reason,
                });
            }
            MetadataEditorWriteOutcome::Skipped { reason } => {
                summary.skipped = summary.skipped.saturating_add(1);
                if summary.first_problem.is_none() {
                    summary.first_problem = Some(reason.clone());
                }
                attach_write_issue(tab, idx, MetadataIssue::SaveBlocked {
                    path: result.path,
                    reason,
                });
            }
        }
    }

    reduce_saved_slots(tab, &saved_slots);
    tab.dirty = presentation_tab_has_changes(tab);
    summary.remaining_dirty = tab.dirty;
    Some(summary)
}

fn attach_write_issue(tab: &mut PresentationTab, idx: usize, issue: MetadataIssue) {
    let Some(file) = tab.technical_details.files.get_mut(idx) else {
        return;
    };
    match &issue {
        MetadataIssue::Write { .. } => {
            file.issues.retain(|existing| !matches!(existing, MetadataIssue::Write { .. }));
        }
        MetadataIssue::SaveBlocked { .. } => {
            file.issues.retain(|existing| !matches!(existing, MetadataIssue::SaveBlocked { .. }));
        }
        _ => {}
    }
    file.issues.push(issue);
}

fn reduce_saved_slots(tab: &mut PresentationTab, saved_slots: &std::collections::BTreeSet<usize>) {
    if saved_slots.is_empty() {
        return;
    }

    let path_count = tab.paths.len();
    let deleted: std::collections::BTreeSet<usize> = tab.deleted.iter().copied().collect();
    let mut remove_entries = Vec::new();
    let mut retained_deleted = Vec::new();

    for (entry_idx, entry) in tab.entries.iter_mut().enumerate() {
        let file_aligned = entry.per_file_values.len() == path_count
            && entry.per_file_originals.len() == path_count;

        if !file_aligned {
            if deleted.contains(&entry_idx) {
                // A row-level delete for a non-file-aligned entry cannot be
                // safely reduced from path-keyed write results. Examples
                // include presentation-scoped or single-image/CUESHEET data
                // where the entry does not map 1:1 onto `paths`. Keep the
                // delete marker until a dedicated owner proves and clears the
                // non-file-aligned write. Clearing it here would silently lose
                // a pending delete after an unrelated file slot saved.
                retained_deleted.push(entry_idx);
            } else if path_count == 1 && saved_slots.contains(&0) {
                // Non-deleted single-file synthetic/display entries have no
                // per-slot retry state to preserve. Once the sole file saved,
                // advance their originals with the rest of the surface.
                mark_tag_entry_saved(entry);
            }
            continue;
        }

        if deleted.contains(&entry_idx) {
            for idx in 0..path_count {
                if saved_slots.contains(&idx) {
                    entry.per_file_values[idx].clear();
                    entry.per_file_originals[idx].clear();
                } else {
                    // Convert the row-level pending delete into a per-file
                    // empty-value change for the unsaved slot. `write_all_tags`
                    // treats an empty value as a delete, so the next save will
                    // retry only the failed/skipped files.
                    entry.per_file_values[idx].clear();
                }
            }
        } else {
            for &idx in saved_slots {
                if idx < path_count {
                    entry.per_file_originals[idx] = entry.per_file_values[idx].clone();
                }
            }
        }

        recompute_tag_entry_display(entry);
        if deleted.contains(&entry_idx)
            && entry.per_file_values.iter().all(|value| value.trim().is_empty())
            && entry.per_file_originals.iter().all(|value| value.trim().is_empty())
        {
            remove_entries.push(entry_idx);
        }
    }

    let removed: std::collections::BTreeSet<usize> = remove_entries.iter().copied().collect();
    tab.deleted = retained_deleted
        .into_iter()
        .filter(|idx| !removed.contains(idx))
        .collect();
    for idx in remove_entries.into_iter().rev() {
        if idx < tab.entries.len() {
            tab.entries.remove(idx);
        }
    }
}

fn presentation_tab_has_changes(tab: &PresentationTab) -> bool {
    if !tab.deleted.is_empty() {
        return true;
    }

    let path_count = tab.paths.len();
    let has_file_details = tab.technical_details.files.len() == path_count;
    let writable = |idx: usize, tab: &PresentationTab| -> bool {
        if !has_file_details {
            return true;
        }
        tab.technical_details
            .files
            .get(idx)
            .map(|file| file.file_facts.write_eligibility.is_writable())
            .unwrap_or(true)
    };

    tab.entries.iter().any(|entry| {
        if entry.per_file_values.len() == path_count && entry.per_file_originals.len() == path_count {
            entry
                .per_file_values
                .iter()
                .zip(entry.per_file_originals.iter())
                .enumerate()
                .any(|(idx, (value, original))| writable(idx, tab) && value != original)
        } else {
            entry.per_file_values != entry.per_file_originals || entry.value != entry.original
        }
    })
}

fn recompute_tag_entry_display(entry: &mut crate::tui::probe::TagEntry) {
    let all_same = entry.per_file_values.windows(2).all(|w| w[0] == w[1]);
    entry.is_mixed = !all_same && entry.per_file_values.len() > 1;
    entry.value = if entry.is_mixed {
        "<multiple values>".to_string()
    } else {
        entry.per_file_values.first().cloned().unwrap_or_default()
    };
}

fn copy_musicbrainz_entries_preserving_originals(
    src_entries: &[crate::tui::probe::TagEntry],
    dst_entries: &mut Vec<crate::tui::probe::TagEntry>,
    track_count: usize,
) -> usize {
    let mut copied = 0usize;
    for src in src_entries {
        if !entry_was_populated_from_musicbrainz(src) {
            continue;
        }

        let idx = dst_entries
            .iter()
            .position(|entry| entry.display_key.eq_ignore_ascii_case(&src.display_key));
        match idx {
            Some(i) => {
                let dst = &mut dst_entries[i];
                dst.item_key = src.item_key.clone();
                dst.value = src.value.clone();
                dst.is_binary = src.is_binary;
                dst.is_mixed = src.is_mixed;
                dst.per_file_values = normalize_entry_values_for_track_count(src, track_count);
                dst.mb_proposed_value = Some(src.value.clone());
                dst.mb_proposed_per_file = Some(dst.per_file_values.clone());
                if dst.per_file_originals.len() != dst.per_file_values.len() {
                    dst.per_file_originals.resize(dst.per_file_values.len(), String::new());
                }
            }
            None => {
                let values = normalize_entry_values_for_track_count(src, track_count);
                let mut entry = src.clone();
                entry.per_file_values = values.clone();
                entry.per_file_originals = vec![String::new(); values.len()];
                entry.value = src.value.clone();
                entry.original = String::new();
                entry.mb_proposed_value = Some(src.value.clone());
                entry.mb_proposed_per_file = Some(values);
                dst_entries.push(entry);
            }
        }
        copied += 1;
    }
    copied
}

fn entry_was_populated_from_musicbrainz(entry: &crate::tui::probe::TagEntry) -> bool {
    entry.mb_proposed_value.is_some() || entry.mb_proposed_per_file.is_some()
}

fn normalize_entry_values_for_track_count(
    src: &crate::tui::probe::TagEntry,
    track_count: usize,
) -> Vec<String> {
    if src.per_file_values.len() == track_count {
        src.per_file_values.clone()
    } else if src.per_file_values.len() == 1 {
        vec![src.per_file_values[0].clone(); track_count]
    } else {
        (0..track_count)
            .map(|i| src.per_file_values.get(i).cloned().unwrap_or_default())
            .collect()
    }
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
    /// Copy the active tab's just-populated MusicBrainz values to every
    /// other presentation tab that has the same track count.
    ApplyMbToAllPresentations(Box<MetadataEditorState>),
    /// Close the metadata editor and discard unsaved changes after an
    /// explicit confirmation. The editor itself is parked in
    /// `AppState::pending_metadata_editor` so cancellation restores it.
    DiscardMetadataEditorChanges,
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

    /// Last Audio Streams overlay row click: (presentation index, click time).
    /// Used for double-click-to-convert on stream rows.
    pub last_disc_browser_stream_click: Option<(usize, std::time::Instant)>,

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
            last_disc_browser_stream_click: None,
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
        let (info, metadata, probe_notice) = if is_cue_sheet_path_for_preview(&first) {
            match probe_cue_proxy_source(&first) {
                Ok(result) => (result.info, result.metadata, result.probe_notice),
                Err(e) => {
                    log::warn!("cli: CUE proxy probe failed for {}: {}", first.display(), e);
                    (
                        None,
                        crate::tui::probe::SourceMetadata::default(),
                        Some(format!("CUE proxy probe failed: {}; set format manually", e)),
                    )
                }
            }
        } else {
            let info = match crate::tui::probe::probe_audio(&first) {
                Ok(i) => Some(i),
                Err(e) => {
                    log::warn!("cli: probe failed for {}: {}", first.display(), e);
                    None
                }
            };
            let metadata = crate::tui::probe::read_metadata(&first).unwrap_or_default();
            (info, metadata, None)
        };

        // Populate the editable metadata pane from the first file's tags.
        self.convert.metadata.title = metadata.title.clone();
        self.convert.metadata.artist = metadata.artist.clone();
        self.convert.metadata.album = metadata.album.clone();
        self.convert.metadata.genre = metadata.genre.clone();
        self.convert.metadata.year = metadata.year.clone();

        // Build the mode (Single for 1 file, Batch for N) and populate
        // first-file probe/metadata in the appropriate variant.
        let mut mode = if valid.len() == 1 {
            SourceMode::from_single_with_probe_notice(first.clone(), None, SourceMetadata::default(), probe_notice.clone())
        } else {
            SourceMode::from_paths(valid)
        };
        match &mut mode {
            SourceMode::Single {
                info: slot,
                metadata: meta_slot,
                probe_notice: single_probe_notice,
                ..
            } => {
                *slot = info;
                *meta_slot = metadata;
                *single_probe_notice = probe_notice.clone();
            }
            SourceMode::Batch {
                cursor_info,
                cursor_metadata,
                probe_notice: batch_probe_notice,
                cursor_probe_notice,
                ..
            } => {
                *cursor_info = info;
                *cursor_metadata = metadata;
                *batch_probe_notice = probe_notice.clone();
                *cursor_probe_notice = None;
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
        // `set_source_mode` only installs the source and updates DSD side effects.
        // CLI-seeded CUE proxy info must drive the same sample-rate, bit-depth,
        // dither, and resampler defaults as Browse/queue probe completion.
        self.convert.apply_source_defaults();

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
        let source_probe_notice = self.convert.source.mode.persistent_probe_notice();
        let status = if let Some(notice) = source_probe_notice {
            format!("loaded CUE from cli with warning: {}", notice)
        } else if valid_count == 1 {
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

// -----------------------------------------------------------------------------
// Format-settings overlay lifecycle and text-entry handlers
// -----------------------------------------------------------------------------
// These handlers intentionally live next to `ActiveOverlay`/`FormatState` so
// keyboard and mouse modules can share one path for overlay construction,
// editing, validation, and commit. Renderers should not parse user input.

impl AppState {
    /// Open codec/container-specific settings for the current format.
    pub fn open_codec_format_settings_overlay(&mut self) -> Result<(), String> {
        self.open_format_settings_overlay_for(FormatSettingsOpenTarget::Codec)
    }

    /// Open resampler-specific settings for the current resampler.
    pub fn open_resampler_format_settings_overlay(&mut self) -> Result<(), String> {
        self.open_format_settings_overlay_for(FormatSettingsOpenTarget::Resampler)
    }

    /// Open the appropriate settings overlay for the current format-pane row.
    ///
    /// This convenience entry point preserves old call sites that had only one
    /// "format settings" action. Dedicated settings-pill handlers should call
    /// `open_codec_format_settings_overlay` or
    /// `open_resampler_format_settings_overlay` so a codec settings pill and an
    /// SSRC/Sox/Soxr settings pill cannot be confused.
    pub fn open_format_settings_overlay(&mut self) -> Result<(), String> {
        let target = match self.convert.format.field_focus {
            FormatField::Resampler | FormatField::SampleRate | FormatField::BitDepth => {
                FormatSettingsOpenTarget::Resampler
            }
            _ => FormatSettingsOpenTarget::Codec,
        };
        self.open_format_settings_overlay_for(target)
    }

    pub fn open_format_settings_overlay_for(
        &mut self,
        target: FormatSettingsOpenTarget,
    ) -> Result<(), String> {
        let Some(kind) = build_format_settings_kind(&self.convert.format, target) else {
            return Err(format!(
                "no {} settings available for current format/resampler",
                target.label()
            ));
        };
        let focus = first_format_settings_focus(&kind);
        self.active_overlay = ActiveOverlay::FormatSettings {
            kind,
            focus,
            help_scroll: None,
        };
        Ok(())
    }

    /// Move focus to the next editable field in the active format settings overlay.
    pub fn format_settings_focus_next(&mut self) -> bool {
        match &mut self.active_overlay {
            ActiveOverlay::FormatSettings {
                kind,
                focus,
                help_scroll: None,
            } => {
                *focus = next_format_settings_focus(kind, *focus, 1);
                true
            }
            _ => false,
        }
    }

    /// Move focus to the previous editable field in the active format settings overlay.
    pub fn format_settings_focus_prev(&mut self) -> bool {
        match &mut self.active_overlay {
            ActiveOverlay::FormatSettings {
                kind,
                focus,
                help_scroll: None,
            } => {
                *focus = next_format_settings_focus(kind, *focus, -1);
                true
            }
            _ => false,
        }
    }

    /// Insert a character into the active text field. Returns `Ok(false)` when
    /// the focused field is not text-editable or the character is not accepted
    /// by that field's grammar.
    pub fn format_settings_insert_char(&mut self, ch: char) -> Result<bool, String> {
        match &mut self.active_overlay {
            ActiveOverlay::FormatSettings {
                kind,
                focus,
                help_scroll: None,
            } => {
                let focus = *focus;
                if !format_settings_focus_accepts_char(focus, ch) {
                    return Ok(false);
                }
                let Some(input) = format_settings_focused_input_mut(kind, focus) else {
                    return Ok(false);
                };
                text_input_insert_char(input, ch);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub fn format_settings_backspace(&mut self) -> bool {
        self.format_settings_apply_text_edit(text_input_backspace)
    }

    pub fn format_settings_delete(&mut self) -> bool {
        self.format_settings_apply_text_edit(text_input_delete)
    }

    pub fn format_settings_move_cursor_left(&mut self) -> bool {
        self.format_settings_apply_text_edit(text_input_move_left)
    }

    pub fn format_settings_move_cursor_right(&mut self) -> bool {
        self.format_settings_apply_text_edit(text_input_move_right)
    }

    pub fn format_settings_move_cursor_home(&mut self) -> bool {
        self.format_settings_apply_text_edit(text_input_move_home)
    }

    pub fn format_settings_move_cursor_end(&mut self) -> bool {
        self.format_settings_apply_text_edit(text_input_move_end)
    }

    fn format_settings_apply_text_edit(
        &mut self,
        edit: fn(&mut crate::tui::text_input::TextInputState) -> bool,
    ) -> bool {
        match &mut self.active_overlay {
            ActiveOverlay::FormatSettings {
                kind,
                focus,
                help_scroll: None,
            } => {
                let Some(input) = format_settings_focused_input_mut(kind, *focus) else {
                    return false;
                };
                edit(input)
            }
            _ => false,
        }
    }

    /// Commit the active format-settings overlay back into `FormatState`.
    ///
    /// Validation is all-or-nothing: invalid fields leave the overlay open and
    /// do not mutate the live format state. This keeps retries idempotent and
    /// prevents half-committed settings.
    pub fn commit_format_settings_overlay(&mut self) -> Result<(), String> {
        let kind = match &self.active_overlay {
            ActiveOverlay::FormatSettings {
                kind,
                help_scroll: None,
                ..
            } => kind.clone(),
            ActiveOverlay::FormatSettings { help_scroll: Some(_), .. } => {
                return Err("close SSRC/settings help before committing".to_string());
            }
            _ => return Err("no format settings overlay is active".to_string()),
        };

        if let Err(err) = apply_format_settings_kind(&mut self.convert.format, kind) {
            self.set_status(err.clone());
            return Err(err);
        }
        self.active_overlay = ActiveOverlay::None;
        self.set_status("format settings updated");
        Ok(())
    }

    pub fn cancel_format_settings_overlay(&mut self) -> bool {
        if matches!(self.active_overlay, ActiveOverlay::FormatSettings { .. }) {
            self.active_overlay = ActiveOverlay::None;
            true
        } else {
            false
        }
    }
}

pub fn build_format_settings_kind(
    format: &FormatState,
    target: FormatSettingsOpenTarget,
) -> Option<FormatSettingsKind> {
    match target {
        FormatSettingsOpenTarget::Codec => build_codec_format_settings_kind(format),
        FormatSettingsOpenTarget::Resampler => build_resampler_format_settings_kind(format),
    }
}

fn build_codec_format_settings_kind(format: &FormatState) -> Option<FormatSettingsKind> {
    match *format.format.selected_value() {
        AudioFormat::Flac => Some(FormatSettingsKind::Flac {
            compression_input: crate::tui::text_input::TextInputState::new(
                format.flac_compression_level.to_string(),
            ),
            verify: *format.flac_verify.selected_value(),
            md5: *format.flac_md5.selected_value(),
        }),
        AudioFormat::Aac => Some(FormatSettingsKind::Aac {
            profile: format.aac_profile,
            quality_preset: format.aac_quality_preset,
            bitrate_input: crate::tui::text_input::TextInputState::new(
                format.aac_bitrate_kbps.to_string(),
            ),
        }),
        AudioFormat::Opus => Some(FormatSettingsKind::Opus {
            content_type: format.opus_content_type,
            quality_preset: format.opus_quality_preset,
            bitrate_input: crate::tui::text_input::TextInputState::new(
                format.opus_bitrate_kbps.to_string(),
            ),
            complexity_input: crate::tui::text_input::TextInputState::new(
                format.opus_complexity.to_string(),
            ),
        }),
        AudioFormat::Mp3 => Some(FormatSettingsKind::Mp3 {
            mode: format.mp3_mode,
            vbr_quality_input: crate::tui::text_input::TextInputState::new(
                format.mp3_vbr_quality.to_string(),
            ),
            quality_preset: format.mp3_quality_preset,
            bitrate_input: crate::tui::text_input::TextInputState::new(
                format.mp3_bitrate_kbps.to_string(),
            ),
        }),
        AudioFormat::WavPack => Some(FormatSettingsKind::WavPack {
            mode: format.wavpack_mode,
            hybrid: format.wavpack_hybrid,
            bitrate_input: crate::tui::text_input::TextInputState::new(
                format.wavpack_bitrate_kbps.to_string(),
            ),
            correction: format.wavpack_correction,
        }),
        _ => None,
    }
}

fn build_resampler_format_settings_kind(format: &FormatState) -> Option<FormatSettingsKind> {
    match *format.resampler.selected_value() {
        ResamplerChoice::Ssrc => Some(FormatSettingsKind::Ssrc {
            attenuation_input: crate::tui::text_input::TextInputState::new(
                format
                    .ssrc_attenuation_db
                    .map(format_decimal)
                    .unwrap_or_default(),
            ),
            min_phase: format.ssrc_min_phase,
            dither_id_input: crate::tui::text_input::TextInputState::new(
                format.ssrc_dither_id.map(|value| value.to_string()).unwrap_or_default(),
            ),
            pdf_type_input: crate::tui::text_input::TextInputState::new(
                format
                    .ssrc_pdf_type
                    .map(|pdf| match pdf {
                        tonepoet_pipeline::enums::SsrcPdfType::Rectangular => "0".to_string(),
                        tonepoet_pipeline::enums::SsrcPdfType::Triangular => "1".to_string(),
                    })
                    .unwrap_or_default(),
            ),
        }),
        ResamplerChoice::Sox => Some(FormatSettingsKind::Sox {
            chebyshev: format.sox_chebyshev,
            bandwidth_input: crate::tui::text_input::TextInputState::new(
                format.sox_bandwidth.map(format_decimal).unwrap_or_default(),
            ),
            phase_input: crate::tui::text_input::TextInputState::new(
                format.sox_phase.map(|value| value.to_string()).unwrap_or_default(),
            ),
            allow_aliasing: format.sox_allow_aliasing,
            sinc_taps_input: crate::tui::text_input::TextInputState::new(
                format.sox_sinc_taps.map(|value| value.to_string()).unwrap_or_default(),
            ),
            sinc_attenuation_input: crate::tui::text_input::TextInputState::new(
                format
                    .sox_sinc_attenuation
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            sinc_passband_input: crate::tui::text_input::TextInputState::new(
                format.sox_sinc_passband.map(format_decimal).unwrap_or_default(),
            ),
            sinc_transition_input: crate::tui::text_input::TextInputState::new(
                format.sox_sinc_transition.map(format_decimal).unwrap_or_default(),
            ),
            sinc_kaiser_beta_input: crate::tui::text_input::TextInputState::new(
                format.sox_sinc_kaiser_beta.map(format_decimal).unwrap_or_default(),
            ),
            sinc_phase: format.sox_sinc_phase,
        }),
        ResamplerChoice::Soxr => Some(FormatSettingsKind::Soxr {
            chebyshev: format.soxr_chebyshev,
            cutoff_input: crate::tui::text_input::TextInputState::new(
                format.soxr_cutoff.map(format_decimal).unwrap_or_default(),
            ),
            phase_input: crate::tui::text_input::TextInputState::new(
                format.soxr_phase.map(|value| value.to_string()).unwrap_or_default(),
            ),
        }),
        ResamplerChoice::None => None,
    }
}

fn format_decimal(value: f32) -> String {
    if value.fract().abs() < f32::EPSILON {
        format!("{value:.0}")
    } else {
        let mut text = format!("{value:.3}");
        while text.contains('.') && text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
        text
    }
}

pub fn apply_format_settings_kind(
    format: &mut FormatState,
    kind: FormatSettingsKind,
) -> Result<(), String> {
    match kind {
        FormatSettingsKind::Flac {
            compression_input,
            verify,
            md5,
        } => {
            format.flac_compression_level = parse_required_u8(
                "FLAC compression",
                &compression_input.text,
                0,
                8,
            )?;
            format.flac_verify.select_value(&verify);
            format.flac_md5.select_value(&md5);
        }
        FormatSettingsKind::Aac {
            profile,
            quality_preset,
            bitrate_input,
        } => {
            format.aac_profile = profile;
            format.aac_quality_preset = quality_preset;
            format.aac_bitrate_kbps = parse_required_u32("AAC bitrate", &bitrate_input.text, 8, 1024)?;
        }
        FormatSettingsKind::Opus {
            content_type,
            quality_preset,
            bitrate_input,
            complexity_input,
        } => {
            format.opus_content_type = content_type;
            format.opus_quality_preset = quality_preset;
            format.opus_bitrate_kbps = parse_required_u32("Opus bitrate", &bitrate_input.text, 6, 510)?;
            format.opus_complexity = parse_required_u8("Opus complexity", &complexity_input.text, 0, 10)?;
        }
        FormatSettingsKind::Mp3 {
            mode,
            vbr_quality_input,
            quality_preset,
            bitrate_input,
        } => {
            format.mp3_mode = mode;
            format.mp3_quality_preset = quality_preset;
            format.mp3_vbr_quality = parse_required_u8("MP3 VBR quality", &vbr_quality_input.text, 0, 9)?;
            format.mp3_bitrate_kbps = parse_required_u32("MP3 bitrate", &bitrate_input.text, 8, 1000)?;
        }
        FormatSettingsKind::WavPack {
            mode,
            hybrid,
            bitrate_input,
            correction,
        } => {
            format.wavpack_mode = mode;
            format.wavpack_hybrid = hybrid;
            format.wavpack_bitrate_kbps = parse_required_u32("WavPack bitrate", &bitrate_input.text, 24, 9600)?;
            format.wavpack_correction = correction;
        }
        FormatSettingsKind::Ssrc {
            attenuation_input,
            min_phase,
            dither_id_input,
            pdf_type_input,
        } => {
            let attenuation_db = parse_optional_f32(
                "SSRC attenuation",
                &attenuation_input.text,
                0.0,
                99.9,
            )?;
            let dither_id = parse_optional_u8("SSRC dither id", &dither_id_input.text, 0, 99)?;
            let pdf_type = parse_optional_ssrc_pdf(&pdf_type_input.text)?;
            if let Some(dither_id) = dither_id {
                validate_ssrc_dither_id_for_target_rate(dither_id, *format.sample_rate.selected_value())?;
            }

            format.ssrc_attenuation_db = attenuation_db;
            format.ssrc_min_phase = min_phase;
            format.ssrc_dither_id = dither_id;
            format.ssrc_pdf_type = pdf_type;
        }
        FormatSettingsKind::Sox {
            chebyshev,
            bandwidth_input,
            phase_input,
            allow_aliasing,
            sinc_taps_input,
            sinc_attenuation_input,
            sinc_passband_input,
            sinc_transition_input,
            sinc_kaiser_beta_input,
            sinc_phase,
        } => {
            format.sox_chebyshev = chebyshev;
            format.sox_bandwidth = parse_optional_f32("Sox bandwidth", &bandwidth_input.text, 74.0, 99.7)?;
            format.sox_phase = parse_optional_u8("Sox phase", &phase_input.text, 0, 100)?;
            format.sox_allow_aliasing = allow_aliasing;
            format.sox_sinc_taps = parse_optional_power_of_two_u32(
                "Sox sinc taps",
                &sinc_taps_input.text,
                1024,
                67_108_864,
            )?;
            format.sox_sinc_attenuation = parse_optional_u16(
                "Sox sinc attenuation",
                &sinc_attenuation_input.text,
                80,
                200,
            )?;
            format.sox_sinc_passband = parse_optional_f32(
                "Sox sinc passband",
                &sinc_passband_input.text,
                1.0,
                220_000.0,
            )?;
            format.sox_sinc_transition = parse_optional_f32(
                "Sox sinc transition",
                &sinc_transition_input.text,
                1.0,
                5000.0,
            )?;
            format.sox_sinc_kaiser_beta = parse_optional_f32(
                "Sox sinc Kaiser beta",
                &sinc_kaiser_beta_input.text,
                0.0,
                32.0,
            )?;
            format.sox_sinc_phase = sinc_phase;
        }
        FormatSettingsKind::Soxr {
            chebyshev,
            cutoff_input,
            phase_input,
        } => {
            format.soxr_chebyshev = chebyshev;
            format.soxr_cutoff = parse_optional_f32("Soxr cutoff", &cutoff_input.text, 74.0, 99.7)?;
            format.soxr_phase = parse_optional_u8("Soxr phase", &phase_input.text, 0, 100)?;
        }
    }
    format.apply_format_constraints();
    Ok(())
}

fn first_format_settings_focus(kind: &FormatSettingsKind) -> FormatSettingsFocus {
    format_settings_focuses(kind)[0]
}

fn next_format_settings_focus(
    kind: &FormatSettingsKind,
    current: FormatSettingsFocus,
    direction: i8,
) -> FormatSettingsFocus {
    let focuses = format_settings_focuses(kind);
    let Some(current_idx) = focuses.iter().position(|focus| *focus == current) else {
        return focuses[0];
    };
    let len = focuses.len();
    let next_idx = if direction < 0 {
        (current_idx + len - 1) % len
    } else {
        (current_idx + 1) % len
    };
    focuses[next_idx]
}

fn format_settings_focuses(kind: &FormatSettingsKind) -> &'static [FormatSettingsFocus] {
    match kind {
        FormatSettingsKind::Flac { .. } => &[
            FormatSettingsFocus::Compression,
            FormatSettingsFocus::Verify,
            FormatSettingsFocus::Md5,
        ],
        FormatSettingsKind::Aac { .. } => &[
            FormatSettingsFocus::AacProfile,
            FormatSettingsFocus::AacQuality,
            FormatSettingsFocus::AacBitrate,
        ],
        FormatSettingsKind::Opus { .. } => &[
            FormatSettingsFocus::OpusContentType,
            FormatSettingsFocus::OpusQuality,
            FormatSettingsFocus::OpusBitrate,
            FormatSettingsFocus::OpusComplexity,
        ],
        FormatSettingsKind::Mp3 { .. } => &[
            FormatSettingsFocus::Mp3Mode,
            FormatSettingsFocus::Mp3VbrQuality,
            FormatSettingsFocus::Mp3Preset,
            FormatSettingsFocus::Mp3Bitrate,
        ],
        FormatSettingsKind::WavPack { .. } => &[
            FormatSettingsFocus::WavPackMode,
            FormatSettingsFocus::WavPackHybrid,
            FormatSettingsFocus::WavPackBitrate,
            FormatSettingsFocus::WavPackCorrection,
        ],
        FormatSettingsKind::Ssrc { .. } => &[
            FormatSettingsFocus::SsrcAttenuation,
            FormatSettingsFocus::SsrcMinPhase,
            FormatSettingsFocus::SsrcDitherId,
            FormatSettingsFocus::SsrcPdf,
        ],
        FormatSettingsKind::Sox { .. } => &[
            FormatSettingsFocus::SoxChebyshev,
            FormatSettingsFocus::SoxBandwidth,
            FormatSettingsFocus::SoxPhase,
            FormatSettingsFocus::SoxAliasing,
            FormatSettingsFocus::SoxSincTaps,
            FormatSettingsFocus::SoxSincAttenuation,
            FormatSettingsFocus::SoxSincPassband,
            FormatSettingsFocus::SoxSincTransition,
            FormatSettingsFocus::SoxSincKaiserBeta,
            FormatSettingsFocus::SoxSincPhase,
        ],
        FormatSettingsKind::Soxr { .. } => &[
            FormatSettingsFocus::SoxrChebyshev,
            FormatSettingsFocus::SoxrCutoff,
            FormatSettingsFocus::SoxrPhase,
        ],
    }
}

fn format_settings_focused_input_mut(
    kind: &mut FormatSettingsKind,
    focus: FormatSettingsFocus,
) -> Option<&mut crate::tui::text_input::TextInputState> {
    match (kind, focus) {
        (FormatSettingsKind::Flac { compression_input, .. }, FormatSettingsFocus::Compression) => {
            Some(compression_input)
        }
        (FormatSettingsKind::Aac { bitrate_input, .. }, FormatSettingsFocus::AacBitrate) => {
            Some(bitrate_input)
        }
        (FormatSettingsKind::Opus { bitrate_input, .. }, FormatSettingsFocus::OpusBitrate) => {
            Some(bitrate_input)
        }
        (FormatSettingsKind::Opus { complexity_input, .. }, FormatSettingsFocus::OpusComplexity) => {
            Some(complexity_input)
        }
        (FormatSettingsKind::Mp3 { vbr_quality_input, .. }, FormatSettingsFocus::Mp3VbrQuality) => {
            Some(vbr_quality_input)
        }
        (FormatSettingsKind::Mp3 { bitrate_input, .. }, FormatSettingsFocus::Mp3Bitrate) => {
            Some(bitrate_input)
        }
        (FormatSettingsKind::WavPack { bitrate_input, .. }, FormatSettingsFocus::WavPackBitrate) => {
            Some(bitrate_input)
        }
        (FormatSettingsKind::Ssrc { attenuation_input, .. }, FormatSettingsFocus::SsrcAttenuation) => {
            Some(attenuation_input)
        }
        (FormatSettingsKind::Ssrc { dither_id_input, .. }, FormatSettingsFocus::SsrcDitherId) => {
            Some(dither_id_input)
        }
        (FormatSettingsKind::Ssrc { pdf_type_input, .. }, FormatSettingsFocus::SsrcPdf) => {
            Some(pdf_type_input)
        }
        (FormatSettingsKind::Sox { bandwidth_input, .. }, FormatSettingsFocus::SoxBandwidth) => {
            Some(bandwidth_input)
        }
        (FormatSettingsKind::Sox { phase_input, .. }, FormatSettingsFocus::SoxPhase) => {
            Some(phase_input)
        }
        (FormatSettingsKind::Sox { sinc_taps_input, .. }, FormatSettingsFocus::SoxSincTaps) => {
            Some(sinc_taps_input)
        }
        (FormatSettingsKind::Sox { sinc_attenuation_input, .. }, FormatSettingsFocus::SoxSincAttenuation) => {
            Some(sinc_attenuation_input)
        }
        (FormatSettingsKind::Sox { sinc_passband_input, .. }, FormatSettingsFocus::SoxSincPassband) => {
            Some(sinc_passband_input)
        }
        (FormatSettingsKind::Sox { sinc_transition_input, .. }, FormatSettingsFocus::SoxSincTransition) => {
            Some(sinc_transition_input)
        }
        (FormatSettingsKind::Sox { sinc_kaiser_beta_input, .. }, FormatSettingsFocus::SoxSincKaiserBeta) => {
            Some(sinc_kaiser_beta_input)
        }
        (FormatSettingsKind::Soxr { cutoff_input, .. }, FormatSettingsFocus::SoxrCutoff) => {
            Some(cutoff_input)
        }
        (FormatSettingsKind::Soxr { phase_input, .. }, FormatSettingsFocus::SoxrPhase) => {
            Some(phase_input)
        }
        _ => None,
    }
}

fn format_settings_focus_accepts_char(focus: FormatSettingsFocus, ch: char) -> bool {
    if ch == '.' {
        return matches!(
            focus,
            FormatSettingsFocus::SsrcAttenuation
                | FormatSettingsFocus::SoxBandwidth
                | FormatSettingsFocus::SoxSincPassband
                | FormatSettingsFocus::SoxSincTransition
                | FormatSettingsFocus::SoxSincKaiserBeta
                | FormatSettingsFocus::SoxrCutoff
        );
    }
    ch.is_ascii_digit()
        && matches!(
            focus,
            FormatSettingsFocus::Compression
                | FormatSettingsFocus::AacBitrate
                | FormatSettingsFocus::OpusBitrate
                | FormatSettingsFocus::OpusComplexity
                | FormatSettingsFocus::Mp3VbrQuality
                | FormatSettingsFocus::Mp3Bitrate
                | FormatSettingsFocus::WavPackBitrate
                | FormatSettingsFocus::SsrcAttenuation
                | FormatSettingsFocus::SsrcDitherId
                | FormatSettingsFocus::SsrcPdf
                | FormatSettingsFocus::SoxBandwidth
                | FormatSettingsFocus::SoxPhase
                | FormatSettingsFocus::SoxSincTaps
                | FormatSettingsFocus::SoxSincAttenuation
                | FormatSettingsFocus::SoxSincPassband
                | FormatSettingsFocus::SoxSincTransition
                | FormatSettingsFocus::SoxSincKaiserBeta
                | FormatSettingsFocus::SoxrCutoff
                | FormatSettingsFocus::SoxrPhase
        )
}

fn parse_required_u8(label: &str, text: &str, min: u8, max: u8) -> Result<u8, String> {
    let value = parse_required_u32(label, text, min as u32, max as u32)?;
    Ok(value as u8)
}

fn parse_required_u32(label: &str, text: &str, min: u32, max: u32) -> Result<u32, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} is required"));
    }
    let value = trimmed
        .parse::<u32>()
        .map_err(|_| format!("{label} must be an integer from {min} through {max}"))?;
    if !(min..=max).contains(&value) {
        return Err(format!("{label} must be from {min} through {max}"));
    }
    Ok(value)
}

fn parse_optional_u8(label: &str, text: &str, min: u8, max: u8) -> Result<Option<u8>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    parse_required_u8(label, trimmed, min, max).map(Some)
}

fn parse_optional_u16(label: &str, text: &str, min: u16, max: u16) -> Result<Option<u16>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value = trimmed
        .parse::<u16>()
        .map_err(|_| format!("{label} must be an integer from {min} through {max}"))?;
    if !(min..=max).contains(&value) {
        return Err(format!("{label} must be from {min} through {max}"));
    }
    Ok(Some(value))
}

fn parse_optional_f32(label: &str, text: &str, min: f32, max: f32) -> Result<Option<f32>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value = trimmed
        .parse::<f32>()
        .map_err(|_| format!("{label} must be a number from {min} through {max}"))?;
    if !value.is_finite() || value < min || value > max {
        return Err(format!("{label} must be from {min} through {max}"));
    }
    Ok(Some(value))
}

fn parse_optional_power_of_two_u32(
    label: &str,
    text: &str,
    min: u32,
    max: u32,
) -> Result<Option<u32>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value = parse_required_u32(label, trimmed, min, max)?;
    if !value.is_power_of_two() {
        return Err(format!("{label} must be a power of two from {min} through {max}"));
    }
    Ok(Some(value))
}

fn validate_ssrc_dither_id_for_target_rate(dither_id: u8, target_rate_hz: u32) -> Result<(), String> {
    // Mirror SSRC's rate-dependent dither menu. IDs 98 and 99 are treated as
    // sample-rate-independent simple/no-shaper choices; shaped ATH and legacy
    // IDs must be available for the selected destination rate.
    let valid = if matches!(dither_id, 98 | 99) {
        true
    } else {
        match target_rate_hz {
            44_100 => matches!(dither_id, 0..=6 | 10..=16 | 90..=92),
            48_000 => matches!(dither_id, 0..=6 | 10..=16 | 90 | 91),
            88_200 | 96_000 | 192_000 => matches!(dither_id, 0..=2),
            8_000 | 11_025 | 22_050 => matches!(dither_id, 0 | 1 | 9),
            _ => false,
        }
    };

    if valid {
        Ok(())
    } else {
        Err(format!(
            "SSRC dither id {dither_id} is not available for target sample rate {target_rate_hz} Hz"
        ))
    }
}

fn parse_optional_ssrc_pdf(
    text: &str,
) -> Result<Option<tonepoet_pipeline::enums::SsrcPdfType>, String> {
    match text.trim() {
        "" => Ok(None),
        "0" => Ok(Some(tonepoet_pipeline::enums::SsrcPdfType::Rectangular)),
        "1" => Ok(Some(tonepoet_pipeline::enums::SsrcPdfType::Triangular)),
        _ => Err("SSRC pdf type must be 0 (rectangular) or 1 (triangular)".to_string()),
    }
}

fn text_input_insert_char(input: &mut crate::tui::text_input::TextInputState, ch: char) {
    let cursor = clamp_to_char_boundary(&input.text, input.cursor);
    input.text.insert(cursor, ch);
    input.cursor = cursor + ch.len_utf8();
}

fn text_input_backspace(input: &mut crate::tui::text_input::TextInputState) -> bool {
    let cursor = clamp_to_char_boundary(&input.text, input.cursor);
    if cursor == 0 {
        input.cursor = 0;
        return false;
    }
    let prev = previous_char_boundary(&input.text, cursor);
    input.text.replace_range(prev..cursor, "");
    input.cursor = prev;
    true
}

fn text_input_delete(input: &mut crate::tui::text_input::TextInputState) -> bool {
    let cursor = clamp_to_char_boundary(&input.text, input.cursor);
    if cursor >= input.text.len() {
        input.cursor = input.text.len();
        return false;
    }
    let next = next_char_boundary(&input.text, cursor);
    input.text.replace_range(cursor..next, "");
    input.cursor = cursor;
    true
}

fn text_input_move_left(input: &mut crate::tui::text_input::TextInputState) -> bool {
    let cursor = clamp_to_char_boundary(&input.text, input.cursor);
    let next = previous_char_boundary(&input.text, cursor);
    let changed = next != input.cursor;
    input.cursor = next;
    changed
}

fn text_input_move_right(input: &mut crate::tui::text_input::TextInputState) -> bool {
    let cursor = clamp_to_char_boundary(&input.text, input.cursor);
    let next = next_char_boundary(&input.text, cursor);
    let changed = next != input.cursor;
    input.cursor = next;
    changed
}

fn text_input_move_home(input: &mut crate::tui::text_input::TextInputState) -> bool {
    let changed = input.cursor != 0;
    input.cursor = 0;
    changed
}

fn text_input_move_end(input: &mut crate::tui::text_input::TextInputState) -> bool {
    let changed = input.cursor != input.text.len();
    input.cursor = input.text.len();
    changed
}

fn clamp_to_char_boundary(text: &str, cursor: usize) -> usize {
    let mut cursor = cursor.min(text.len());
    while cursor > 0 && !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

fn previous_char_boundary(text: &str, cursor: usize) -> usize {
    let mut prev = cursor.saturating_sub(1);
    while prev > 0 && !text.is_char_boundary(prev) {
        prev -= 1;
    }
    prev
}

fn next_char_boundary(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }
    let mut next = cursor + 1;
    while next < text.len() && !text.is_char_boundary(next) {
        next += 1;
    }
    next
}

#[cfg(test)]
mod ssrc_format_settings_handler_tests {
    use super::*;
    use tonepoet_pipeline::enums::SsrcPdfType;

    fn ssrc_kind(format: &FormatState) -> FormatSettingsKind {
        build_format_settings_kind(format, FormatSettingsOpenTarget::Resampler)
            .expect("SSRC settings should be available")
    }

    #[test]
    fn ssrc_overlay_creation_seeds_empty_override_fields_as_empty_text() {
        let mut format = FormatState::new();
        format.resampler.select_value(&ResamplerChoice::Ssrc);

        let FormatSettingsKind::Ssrc {
            attenuation_input,
            dither_id_input,
            pdf_type_input,
            ..
        } = ssrc_kind(&format)
        else {
            panic!("expected SSRC settings kind");
        };

        assert_eq!(attenuation_input.text, "");
        assert_eq!(dither_id_input.text, "");
        assert_eq!(pdf_type_input.text, "");
    }

    #[test]
    fn ssrc_overlay_commit_parses_dither_id_and_pdf_type() {
        let mut format = FormatState::new();
        format.resampler.select_value(&ResamplerChoice::Ssrc);
        let mut kind = ssrc_kind(&format);

        let FormatSettingsKind::Ssrc {
            ref mut dither_id_input,
            ref mut pdf_type_input,
            ..
        } = kind
        else {
            panic!("expected SSRC settings kind");
        };
        dither_id_input.text = "2".to_string();
        dither_id_input.cursor = dither_id_input.text.len();
        pdf_type_input.text = "1".to_string();
        pdf_type_input.cursor = pdf_type_input.text.len();

        apply_format_settings_kind(&mut format, kind).expect("valid SSRC settings should commit");
        assert_eq!(format.ssrc_dither_id, Some(2));
        assert_eq!(format.ssrc_pdf_type, Some(SsrcPdfType::Triangular));
        assert!(format.ssrc_dither_override_active());
    }

    #[test]
    fn ssrc_overlay_partial_override_activates_dither_override_indicator_state() {
        let mut format = FormatState::new();
        format.resampler.select_value(&ResamplerChoice::Ssrc);

        let mut dither_only = ssrc_kind(&format);
        let FormatSettingsKind::Ssrc {
            ref mut dither_id_input,
            ref mut pdf_type_input,
            ..
        } = dither_only
        else {
            panic!("expected SSRC settings kind");
        };
        dither_id_input.text = "2".to_string();
        pdf_type_input.text.clear();
        apply_format_settings_kind(&mut format, dither_only)
            .expect("valid dither-only override should commit");
        assert_eq!(format.ssrc_dither_id, Some(2));
        assert_eq!(format.ssrc_pdf_type, None);
        assert!(format.ssrc_dither_override_active());

        let mut pdf_only = ssrc_kind(&format);
        let FormatSettingsKind::Ssrc {
            ref mut dither_id_input,
            ref mut pdf_type_input,
            ..
        } = pdf_only
        else {
            panic!("expected SSRC settings kind");
        };
        dither_id_input.text.clear();
        pdf_type_input.text = "1".to_string();
        apply_format_settings_kind(&mut format, pdf_only)
            .expect("valid pdf-only override should commit");
        assert_eq!(format.ssrc_dither_id, None);
        assert_eq!(format.ssrc_pdf_type, Some(SsrcPdfType::Triangular));
        assert!(format.ssrc_dither_override_active());
    }

    #[test]
    fn ssrc_dither_override_indicator_requires_ssrc_resampler() {
        let mut format = FormatState::new();
        format.ssrc_dither_id = Some(2);
        format.ssrc_pdf_type = Some(SsrcPdfType::Triangular);

        assert!(!format.ssrc_dither_override_active());

        format.resampler.select_value(&ResamplerChoice::Ssrc);
        assert!(format.ssrc_dither_override_active());
    }

    #[test]
    fn ssrc_overlay_commit_rejects_out_of_range_dither_id_without_mutation() {
        let mut format = FormatState::new();
        format.resampler.select_value(&ResamplerChoice::Ssrc);
        format.ssrc_dither_id = Some(6);
        format.ssrc_pdf_type = Some(SsrcPdfType::Triangular);
        let mut kind = ssrc_kind(&format);

        let FormatSettingsKind::Ssrc {
            ref mut dither_id_input,
            ref mut pdf_type_input,
            ..
        } = kind
        else {
            panic!("expected SSRC settings kind");
        };
        dither_id_input.text = "100".to_string();
        pdf_type_input.text = "1".to_string();

        assert!(apply_format_settings_kind(&mut format, kind).is_err());
        assert_eq!(format.ssrc_dither_id, Some(6));
        assert_eq!(format.ssrc_pdf_type, Some(SsrcPdfType::Triangular));
    }

    #[test]
    fn ssrc_overlay_commit_rejects_rate_unavailable_dither_id_without_mutation() {
        let mut format = FormatState::new();
        format.resampler.select_value(&ResamplerChoice::Ssrc);
        format.sample_rate.select_value(&96_000);
        format.ssrc_dither_id = Some(2);
        format.ssrc_pdf_type = Some(SsrcPdfType::Triangular);
        let mut kind = ssrc_kind(&format);

        let FormatSettingsKind::Ssrc {
            ref mut dither_id_input,
            ref mut pdf_type_input,
            ..
        } = kind
        else {
            panic!("expected SSRC settings kind");
        };
        dither_id_input.text = "16".to_string();
        pdf_type_input.text = "1".to_string();

        assert!(apply_format_settings_kind(&mut format, kind).is_err());
        assert_eq!(format.ssrc_dither_id, Some(2));
        assert_eq!(format.ssrc_pdf_type, Some(SsrcPdfType::Triangular));
    }

    #[test]
    fn ssrc_overlay_empty_custom_fields_clear_overrides() {
        let mut format = FormatState::new();
        format.resampler.select_value(&ResamplerChoice::Ssrc);
        format.ssrc_dither_id = Some(2);
        format.ssrc_pdf_type = Some(SsrcPdfType::Triangular);
        let mut kind = ssrc_kind(&format);

        let FormatSettingsKind::Ssrc {
            ref mut dither_id_input,
            ref mut pdf_type_input,
            ..
        } = kind
        else {
            panic!("expected SSRC settings kind");
        };
        dither_id_input.text.clear();
        dither_id_input.cursor = 0;
        pdf_type_input.text.clear();
        pdf_type_input.cursor = 0;

        assert!(format.ssrc_dither_override_active());

        apply_format_settings_kind(&mut format, kind).expect("empty optional fields should commit");
        assert_eq!(format.ssrc_dither_id, None);
        assert_eq!(format.ssrc_pdf_type, None);
        assert!(!format.ssrc_dither_override_active());
    }

    #[test]
    fn ssrc_dither_approximation_indicator_is_user_visible_for_non_native_global_pills() {
        let mut format = FormatState::new();
        format.resampler.select_value(&ResamplerChoice::Ssrc);

        format.dither.select_value(&DitherType::TPDF);
        assert!(!format.ssrc_dither_approximation_active());
        assert_eq!(format.ssrc_dither_status_label(), None);

        format.dither.select_value(&DitherType::Shibata);
        assert!(format.ssrc_dither_approximation_active());
        assert_eq!(format.ssrc_dither_status_label(), Some("ssrc approx"));

        format.dither.select_value(&DitherType::Lipshitz);
        assert!(format.ssrc_dither_approximation_active());
        assert_eq!(format.ssrc_dither_status_label(), Some("ssrc approx"));

        format.ssrc_dither_id = Some(0);
        format.ssrc_pdf_type = Some(SsrcPdfType::Triangular);
        assert!(!format.ssrc_dither_approximation_active());
        assert_eq!(format.ssrc_dither_status_label(), Some("ssrc override"));
    }

    #[test]
    fn ssrc_dither_status_labels_invalid_global_mapping_for_selected_rate() {
        let mut format = FormatState::new();
        format.resampler.select_value(&ResamplerChoice::Ssrc);
        format.sample_rate.select_value(&176_400);
        format.bit_depth.select_value(&BitDepthChoice::Int16);
        format.dither.select_value(&DitherType::HighShibata);

        assert!(format.ssrc_dither_invalid_for_selected_rate());
        assert_eq!(format.ssrc_dither_status_label(), Some("ssrc invalid"));

        format.dither.select_value(&DitherType::TPDF);
        assert!(!format.ssrc_dither_invalid_for_selected_rate());
        assert_eq!(format.ssrc_dither_status_label(), None);
    }

    #[test]
    fn ssrc_dither_invalid_status_is_suppressed_for_float_output_and_explicit_overrides() {
        let mut format = FormatState::new();
        format.format.select_value(&AudioFormat::Wav); // WAV supports float bit depths
        format.apply_format_constraints(); // Enable Float32/Float64 for WAV
        format.resampler.select_value(&ResamplerChoice::Ssrc);
        format.sample_rate.select_value(&176_400);
        format.bit_depth.select_value(&BitDepthChoice::Float32);
        format.dither.select_value(&DitherType::HighShibata);

        assert!(!format.ssrc_dither_invalid_for_selected_rate());
        assert_eq!(format.ssrc_dither_status_label(), Some("ssrc approx"));

        format.bit_depth.select_value(&BitDepthChoice::Int16);
        format.ssrc_dither_id = Some(99);
        format.ssrc_pdf_type = Some(SsrcPdfType::Triangular);
        assert!(!format.ssrc_dither_invalid_for_selected_rate());
        assert_eq!(format.ssrc_dither_status_label(), Some("ssrc override"));
    }
}


#[cfg(test)]
mod dsd_gain_format_state_tests {
    use super::*;

    #[test]
    fn dsd_gain_rows_are_visible_only_for_dsd_sources_targeting_pcm() {
        assert!(!FormatField::visible_rows(false, false).contains(&FormatField::DsdGain));
        assert!(!FormatField::visible_rows(false, false).contains(&FormatField::DsdGainDb));
        assert!(FormatField::visible_rows(false, true).contains(&FormatField::DsdGain));
        assert!(FormatField::visible_rows(false, true).contains(&FormatField::DsdGainDb));
        assert!(!FormatField::visible_rows(true, false).contains(&FormatField::DsdGain));
        assert!(!FormatField::visible_rows(true, true).contains(&FormatField::DsdGain));
    }

    #[test]
    fn format_state_hides_dsd_gain_until_source_is_dsd() {
        let mut s = FormatState::new();
        assert!(!s.dsd_to_pcm_gain_available());
        assert!(!FormatField::visible_rows(s.is_dsd_selected(), s.dsd_to_pcm_gain_available())
            .contains(&FormatField::DsdGain));

        s.set_source_is_dsd(true);
        assert!(s.dsd_to_pcm_gain_available());
        assert!(FormatField::visible_rows(s.is_dsd_selected(), s.dsd_to_pcm_gain_available())
            .contains(&FormatField::DsdGain));
    }

    #[test]
    fn dsd_target_hides_dsd_gain_even_for_dsd_source() {
        let mut s = FormatState::new();
        s.set_source_is_dsd(true);
        s.format.select_value(&AudioFormat::Dsf);
        s.apply_format_constraints();

        assert!(!s.dsd_to_pcm_gain_available());
        assert!(!FormatField::visible_rows(s.is_dsd_selected(), s.dsd_to_pcm_gain_available())
            .contains(&FormatField::DsdGain));
    }


    #[test]
    fn focus_navigation_skips_dsd_gain_for_pcm_sources() {
        let mut s = FormatState::new();
        s.field_focus = FormatField::ReplayGain;
        s.focus_next();
        assert_eq!(s.field_focus, FormatField::Format);

        s.set_source_is_dsd(true);
        s.field_focus = FormatField::ReplayGain;
        s.focus_next();
        assert_eq!(s.field_focus, FormatField::DsdGain);
        s.focus_next();
        assert_eq!(s.field_focus, FormatField::DsdGainDb);
    }

    #[test]
    fn dsd_gain_defaults_to_disabled_with_015_db_auto_margin() {
        let s = FormatState::new();
        assert_eq!(*s.dsd_gain_mode.selected_value(), DsdGainMode::Disabled);
        assert!((s.dsd_auto_gain_margin_db - 0.15).abs() < f32::EPSILON);
        assert_eq!(s.dsd_gain_db, 0.0);
        assert!(!s.source_is_dsd);
    }

    #[test]
    fn manual_dsd_gain_row_adjusts_value_and_selects_manual_mode() {
        let mut s = FormatState::new();
        s.set_source_is_dsd(true);
        s.field_focus = FormatField::DsdGainDb;

        s.select_focused_next(None, None);
        assert_eq!(*s.dsd_gain_mode.selected_value(), DsdGainMode::Manual);
        assert_eq!(s.dsd_gain_db, DSD_TO_PCM_GAIN_DB_STEP);

        s.select_focused_prev(None, None);
        assert_eq!(s.dsd_gain_db, 0.0);
    }

    #[test]
    fn manual_dsd_gain_row_clamps_to_valid_settings_range() {
        let mut s = FormatState::new();
        s.set_source_is_dsd(true);
        s.field_focus = FormatField::DsdGainDb;
        s.dsd_gain_db = DSD_TO_PCM_GAIN_DB_MAX;
        s.select_focused_next(None, None);
        assert_eq!(s.dsd_gain_db, DSD_TO_PCM_GAIN_DB_MAX);

        s.dsd_gain_db = DSD_TO_PCM_GAIN_DB_MIN;
        s.select_focused_prev(None, None);
        assert_eq!(s.dsd_gain_db, DSD_TO_PCM_GAIN_DB_MIN);
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
mod cue_proxy_probe_tests {
    use super::*;

    fn source_info(sample_rate: u32, bit_depth: Option<u32>, channels: u32) -> SourceInfo {
        SourceInfo {
            format_name: "FLAC".to_string(),
            codec: "FLAC".to_string(),
            bit_depth,
            sample_rate,
            channels,
            channel_layout: if channels == 2 { "stereo".to_string() } else { format!("{} ch", channels) },
            duration_secs: 123.0,
            file_size: 456,
        }
    }

    #[test]
    fn cue_proxy_uniform_properties_ignore_duration_and_size() {
        let mut left = source_info(96_000, Some(24), 2);
        let mut right = source_info(96_000, Some(24), 2);
        left.duration_secs = 10.0;
        left.file_size = 100;
        right.duration_secs = 20.0;
        right.file_size = 200;

        assert!(cue_proxy_probe_properties_match(&left, &right));
    }

    #[test]
    fn cue_proxy_uniform_properties_reject_rate_depth_or_channel_mismatch() {
        let base = source_info(44_100, Some(16), 2);
        assert!(!cue_proxy_probe_properties_match(&base, &source_info(48_000, Some(16), 2)));
        assert!(!cue_proxy_probe_properties_match(&base, &source_info(44_100, Some(24), 2)));
        assert!(!cue_proxy_probe_properties_match(&base, &source_info(44_100, Some(16), 6)));
    }

    #[test]
    fn cue_sheet_metadata_fills_only_missing_fields() {
        let mut sheet = crate::tui::cue_parser::CueSheet::default();
        sheet.title = Some("Cue Album".to_string());
        sheet.performer = Some("Cue Artist".to_string());
        sheet.genre = Some("Cue Genre".to_string());
        sheet.date = Some("1984".to_string());
        sheet.catalog = Some("0123456789012".to_string());

        let mut metadata = SourceMetadata::default();
        metadata.album = Some("Tagged Album".to_string());

        let merged = cue_sheet_metadata(&sheet, metadata);
        assert_eq!(merged.album.as_deref(), Some("Tagged Album"));
        assert_eq!(merged.artist.as_deref(), Some("Cue Artist"));
        assert_eq!(merged.genre.as_deref(), Some("Cue Genre"));
        assert_eq!(merged.year.as_deref(), Some("1984"));
        assert_eq!(merged.catalog_number.as_deref(), Some("0123456789012"));
    }

    fn temp_test_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tonepoet_{}_{}_{}",
            prefix,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("test directory should be creatable");
        dir
    }

    fn write_test_file(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("test file should be writable");
    }

    #[test]
    fn direct_one_track_cue_remains_multitrack_with_probe_notice() {
        let dir = temp_test_dir("one_track");
        let cue_path = dir.join("single.cue");
        let cue_text = r#"TITLE "Single Cue Album"
PERFORMER "Album Artist"
FILE "image.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Only Track"
    PERFORMER "Track Artist"
    INDEX 01 00:00:00
"#;
        write_test_file(&cue_path, cue_text);

        let mode = SourceMode::from_single_with_probe_notice(
            cue_path.clone(),
            None,
            SourceMetadata::default(),
            Some("CUE proxy warning".to_string()),
        );

        match mode {
            SourceMode::MultiTrack {
                path,
                info,
                tracks,
                album_title,
                album_artist,
                probe_notice,
                selected,
                ..
            } => {
                assert_eq!(path, cue_path);
                assert!(info.is_none());
                assert_eq!(tracks.len(), 1);
                assert_eq!(tracks[0].number, 1);
                assert_eq!(tracks[0].title.as_deref(), Some("Only Track"));
                assert_eq!(tracks[0].performer.as_deref(), Some("Track Artist"));
                assert_eq!(album_title.as_deref(), Some("Single Cue Album"));
                assert_eq!(album_artist.as_deref(), Some("Album Artist"));
                assert_eq!(probe_notice.as_deref(), Some("CUE proxy warning"));
                assert_eq!(selected, vec![true]);
            }
            other => panic!("expected direct one-track CUE to remain MultiTrack, got {:?}", other),
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn single_image_cue_returns_source_info_and_never_probes_cue_text() {
        let dir = temp_test_dir("single_image_proxy");
        let cue_path = dir.join("album.cue");
        let image_path = dir.join("image.flac");
        write_test_file(&image_path, "not real audio; probe is mocked");
        write_test_file(
            &cue_path,
            r#"TITLE "Proxy Album"
PERFORMER "Proxy Artist"
FILE "image.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 04:00:00
"#,
        );

        let mut hook = CueProxyProbeTestHook::default();
        hook.probe_results
            .insert(image_path.clone(), Ok(source_info(96_000, Some(24), 2)));
        let (result, hook) = with_cue_proxy_probe_test_hook(hook, || {
            probe_cue_proxy_source(&cue_path).expect("CUE proxy probe should succeed")
        });

        let info = result.info.expect("single-image CUE should expose proxied SourceInfo");
        assert_eq!(info.sample_rate, 96_000);
        assert_eq!(info.bit_depth, Some(24));
        assert!(result.probe_notice.is_none());
        assert_eq!(result.metadata.album.as_deref(), Some("Proxy Album"));
        assert_eq!(result.metadata.artist.as_deref(), Some("Proxy Artist"));
        assert_eq!(hook.probed_paths, vec![image_path.clone()]);
        assert!(!hook.probed_paths.iter().any(|path| path == &cue_path));
        assert_eq!(hook.metadata_paths, vec![image_path]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn multi_file_uniform_cue_aggregates_duration_and_size() {
        let dir = temp_test_dir("uniform_aggregate");
        let cue_path = dir.join("album.cue");
        let first = dir.join("01.flac");
        let second = dir.join("02.flac");
        write_test_file(&first, "mocked");
        write_test_file(&second, "mocked");
        write_test_file(
            &cue_path,
            r#"FILE "01.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
FILE "02.flac" WAVE
  TRACK 02 AUDIO
    INDEX 01 00:00:00
"#,
        );

        let mut first_info = source_info(44_100, Some(16), 2);
        first_info.duration_secs = 11.5;
        first_info.file_size = 101;
        let mut second_info = source_info(44_100, Some(16), 2);
        second_info.duration_secs = 22.25;
        second_info.file_size = 202;

        let mut hook = CueProxyProbeTestHook::default();
        hook.probe_results.insert(first.clone(), Ok(first_info));
        hook.probe_results.insert(second.clone(), Ok(second_info));

        let (result, hook) = with_cue_proxy_probe_test_hook(hook, || {
            probe_cue_proxy_source(&cue_path).expect("CUE proxy probe should succeed")
        });

        let info = result.info.expect("uniform multi-file CUE should expose SourceInfo");
        assert!((info.duration_secs - 33.75).abs() < f64::EPSILON);
        assert_eq!(info.file_size, 303);
        assert!(result.probe_notice.is_none());
        assert_eq!(hook.probed_paths, vec![first.clone(), second]);
        assert_eq!(hook.metadata_paths, vec![first]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mixed_cue_keeps_persistent_notice_and_clears_source_defaults() {
        let dir = temp_test_dir("mixed_defaults");
        let cue_path = dir.join("mixed.cue");
        let first = dir.join("01.flac");
        let second = dir.join("02.flac");
        write_test_file(&first, "mocked");
        write_test_file(&second, "mocked");
        write_test_file(
            &cue_path,
            r#"FILE "01.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
FILE "02.flac" WAVE
  TRACK 02 AUDIO
    INDEX 01 00:00:00
"#,
        );

        let mut hook = CueProxyProbeTestHook::default();
        hook.probe_results
            .insert(first, Ok(source_info(96_000, Some(24), 2)));
        hook.probe_results
            .insert(second, Ok(source_info(44_100, Some(16), 2)));
        let (result, _hook) = with_cue_proxy_probe_test_hook(hook, || {
            probe_cue_proxy_source(&cue_path).expect("CUE proxy probe should return a warning result")
        });

        assert!(result.info.is_none());
        let notice = result.probe_notice.clone().expect("mixed CUE should warn");
        assert!(notice.contains("mixed source properties"));

        let mode = SourceMode::from_single_with_probe_notice(
            cue_path.clone(),
            result.info,
            result.metadata,
            result.probe_notice,
        );
        match &mode {
            SourceMode::MultiTrack { probe_notice, .. } => {
                assert_eq!(probe_notice.as_deref(), Some(notice.as_str()));
            }
            other => panic!("expected mixed direct CUE to remain MultiTrack, got {:?}", other),
        }

        let mut convert = ConvertState::new();
        convert.set_source_mode(SourceMode::Single {
            path: PathBuf::from("/tmp/highres.flac"),
            info: Some(source_info(96_000, Some(24), 2)),
            metadata: SourceMetadata::default(),
            probe_notice: None,
        });
        convert.apply_source_defaults();
        assert_eq!(*convert.format.sample_rate.selected_value(), 96_000);
        assert_eq!(*convert.format.bit_depth.selected_value(), BitDepthChoice::Int24);

        convert.set_source_mode(mode);
        convert.apply_source_defaults();
        assert_eq!(*convert.format.sample_rate.selected_value(), 44_100);
        assert_eq!(*convert.format.bit_depth.selected_value(), BitDepthChoice::Int16);
        assert_eq!(*convert.format.dither.selected_value(), DitherType::None);
        assert_eq!(*convert.format.resampler.selected_value(), ResamplerChoice::None);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_ambiguous_and_non_audio_cue_file_references_warn() {
        let dir = temp_test_dir("bad_refs");

        let missing_cue = dir.join("missing.cue");
        write_test_file(
            &missing_cue,
            r#"FILE "missing.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        );
        let missing = probe_cue_proxy_source(&missing_cue).expect("missing reference should be a warning result");
        assert!(missing.info.is_none());
        assert!(missing.probe_notice.unwrap().contains("was not found"));

        let ambiguous_cue = dir.join("ambiguous.cue");
        write_test_file(&dir.join("ambiguous.flac"), "mocked");
        write_test_file(&dir.join("ambiguous.wav"), "mocked");
        write_test_file(
            &ambiguous_cue,
            r#"FILE "ambiguous" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        );
        let ambiguous = probe_cue_proxy_source(&ambiguous_cue).expect("ambiguous reference should be a warning result");
        assert!(ambiguous.info.is_none());
        assert!(ambiguous.probe_notice.unwrap().contains("was ambiguous"));

        let non_audio_cue = dir.join("non_audio.cue");
        let non_audio = dir.join("notes.txt");
        write_test_file(&non_audio, "not audio");
        write_test_file(
            &non_audio_cue,
            r#"FILE "notes.txt" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        );
        let mut hook = CueProxyProbeTestHook::default();
        hook.probe_results
            .insert(non_audio.clone(), Err("not an audio stream".to_string()));
        let (non_audio_result, hook) = with_cue_proxy_probe_test_hook(hook, || {
            probe_cue_proxy_source(&non_audio_cue).expect("non-audio reference should be a warning result")
        });
        assert!(non_audio_result.info.is_none());
        let notice = non_audio_result.probe_notice.expect("non-audio probe failure should warn");
        assert!(notice.contains("CUE image probe failed"));
        assert!(notice.contains("set format manually"));
        assert_eq!(hook.probed_paths, vec![non_audio]);
        assert!(hook.metadata_paths.is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cli_seeded_single_image_cue_applies_source_defaults() {
        let dir = temp_test_dir("cli_defaults");
        let cue_path = dir.join("album.cue");
        let image_path = dir.join("image.flac");
        write_test_file(&image_path, "mocked");
        write_test_file(
            &cue_path,
            r#"FILE "image.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        );

        let mut hook = CueProxyProbeTestHook::default();
        hook.probe_results
            .insert(image_path.clone(), Ok(source_info(96_000, Some(24), 2)));

        let ((), hook) = with_cue_proxy_probe_test_hook(hook, || {
            let mut app = AppState::new(TonepoetConfig::default());
            app.seed_from_cli_paths(vec![cue_path.clone()]);

            assert_eq!(app.current_screen, AppScreen::Convert);
            match &app.convert.source.mode {
                SourceMode::MultiTrack { info: Some(info), probe_notice, .. } => {
                    assert_eq!(info.sample_rate, 96_000);
                    assert_eq!(info.bit_depth, Some(24));
                    assert!(probe_notice.is_none());
                }
                other => panic!("expected CLI-seeded CUE to become probed MultiTrack, got {:?}", other),
            }
            assert_eq!(*app.convert.format.sample_rate.selected_value(), 96_000);
            assert_eq!(*app.convert.format.bit_depth.selected_value(), BitDepthChoice::Int24);
        });

        assert_eq!(hook.probed_paths, vec![image_path]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn empty_direct_cue_keeps_persistent_single_notice_and_resets_defaults() {
        let dir = temp_test_dir("empty_cue_notice");
        let cue_path = dir.join("empty.cue");
        write_test_file(
            &cue_path,
            r#"TITLE "Empty Cue"
PERFORMER "Nobody"
"#,
        );

        let result = probe_cue_proxy_source(&cue_path)
            .expect("empty but parseable CUE should return a warning result");
        assert!(result.info.is_none());
        let notice = result
            .probe_notice
            .clone()
            .expect("empty CUE should carry a warning");
        assert!(notice.contains("no audio tracks"));

        let mode = SourceMode::from_single_with_probe_notice(
            cue_path.clone(),
            result.info,
            result.metadata,
            result.probe_notice,
        );
        match &mode {
            SourceMode::Single {
                path,
                info,
                probe_notice,
                ..
            } => {
                assert_eq!(path, &cue_path);
                assert!(info.is_none());
                assert_eq!(probe_notice.as_deref(), Some(notice.as_str()));
            }
            other => panic!("expected bad direct CUE to remain Single with notice, got {:?}", other),
        }
        assert_eq!(mode.persistent_probe_notice(), Some(notice.as_str()));

        let mut app = AppState::new(TonepoetConfig::default());
        app.convert.set_source_mode(SourceMode::Single {
            path: PathBuf::from("/tmp/highres.flac"),
            info: Some(source_info(96_000, Some(24), 2)),
            metadata: SourceMetadata::default(),
            probe_notice: None,
        });
        app.convert.apply_source_defaults();
        assert_eq!(*app.convert.format.sample_rate.selected_value(), 96_000);
        assert_eq!(*app.convert.format.bit_depth.selected_value(), BitDepthChoice::Int24);

        app.seed_from_cli_paths(vec![cue_path.clone()]);
        assert_eq!(*app.convert.format.sample_rate.selected_value(), 44_100);
        assert_eq!(*app.convert.format.bit_depth.selected_value(), BitDepthChoice::Int16);
        assert_eq!(app.convert.source.mode.persistent_probe_notice(), Some(notice.as_str()));
        let status = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.as_str())
            .unwrap_or("");
        assert!(status.contains("warning"));
        assert!(status.contains("no audio tracks"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cue_proxy_test_hook_is_cleared_when_closure_panics() {
        let panic_result = std::panic::catch_unwind(|| {
            let _ = with_cue_proxy_probe_test_hook(CueProxyProbeTestHook::default(), || -> () {
                panic!("intentional panic to verify hook cleanup");
            });
        });
        assert!(panic_result.is_err());

        let ((), hook) = with_cue_proxy_probe_test_hook(CueProxyProbeTestHook::default(), || {});
        assert!(hook.probed_paths.is_empty());
        assert!(hook.metadata_paths.is_empty());
    }
}

#[cfg(test)]
mod source_default_reset_tests {
    use super::*;

    fn source_info(sample_rate: u32, bit_depth: Option<u32>) -> SourceInfo {
        SourceInfo {
            format_name: "FLAC".to_string(),
            codec: "FLAC".to_string(),
            bit_depth,
            sample_rate,
            channels: 2,
            channel_layout: "stereo".to_string(),
            duration_secs: 10.0,
            file_size: 100,
        }
    }

    #[test]
    fn apply_source_defaults_clears_stale_source_values_when_info_is_absent() {
        let mut convert = ConvertState::new();
        convert.set_source_mode(SourceMode::Single {
            path: PathBuf::from("/tmp/highres.flac"),
            info: Some(source_info(96_000, Some(24))),
            metadata: SourceMetadata::default(),
            probe_notice: None,
        });
        convert.apply_source_defaults();

        assert_eq!(*convert.format.sample_rate.selected_value(), 96_000);
        assert_eq!(*convert.format.bit_depth.selected_value(), BitDepthChoice::Int24);

        convert.set_source_mode(SourceMode::MultiTrack {
            path: PathBuf::from("/tmp/mixed.cue"),
            info: None,
            metadata: SourceMetadata::default(),
            tracks: vec![MultiTrackEntry {
                number: 1,
                title: None,
                performer: None,
                duration_display: None,
            }],
            area_label: None,
            album_title: None,
            album_artist: None,
            probe_notice: Some("mixed source properties; set format manually".to_string()),
            scroll: 0,
            cursor: 0,
            selected: vec![true],
            disc_contents: None,
            selected_presentation_id: None,
        });
        convert.apply_source_defaults();

        assert_eq!(*convert.format.sample_rate.selected_value(), 44_100);
        assert_eq!(*convert.format.bit_depth.selected_value(), BitDepthChoice::Int16);
        assert_eq!(*convert.format.dither.selected_value(), DitherType::None);
        assert_eq!(*convert.format.resampler.selected_value(), ResamplerChoice::None);
        assert!(!convert.format.source_is_dsd);
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

#[cfg(test)]
mod metadata_presentation_tab_tests {
    use super::*;
    use crate::disc::model::PresentationId;
    use crate::tui::probe::TagEntry;
    use lofty::tag::ItemKey;

    fn tag(display_key: &str, value: &str, per_file_values: Vec<&str>) -> TagEntry {
        TagEntry {
            display_key: display_key.to_string(),
            item_key: ItemKey::TrackTitle,
            value: value.to_string(),
            original: value.to_string(),
            is_binary: false,
            is_mixed: false,
            per_file_values: per_file_values.iter().map(|v| (*v).to_string()).collect(),
            per_file_originals: per_file_values.iter().map(|v| (*v).to_string()).collect(),
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        }
    }

    fn mb_tag(display_key: &str, value: &str, per_file_values: Vec<&str>) -> TagEntry {
        let mut entry = tag(display_key, value, per_file_values);
        entry.mb_proposed_value = Some(entry.value.clone());
        entry.mb_proposed_per_file = Some(entry.per_file_values.clone());
        entry
    }

    fn tab(id: PresentationId, label: &str, entries: Vec<TagEntry>, n_paths: usize) -> PresentationTab {
        let paths: Vec<_> = (0..n_paths)
            .map(|idx| std::path::PathBuf::from(format!("/disc/track{:02}.flac", idx + 1)))
            .collect();
        let file_labels: Vec<_> = (0..n_paths)
            .map(|idx| format!("Track {:02}", idx + 1))
            .collect();
        PresentationTab::new(
            id,
            label,
            paths,
            entries,
            file_labels,
            MetadataTechnicalDetails::default(),
        )
    }

    fn state_with_tabs(tabs: Vec<PresentationTab>, active_tab: usize) -> MetadataEditorState {
        MetadataEditorState::for_disc_presentations(tabs, active_tab)
    }

    #[test]
    fn switching_tabs_persists_active_edits_and_restores_target_state() {
        let tabs = vec![
            tab(
                PresentationId::DvdAudioGroup(1),
                "Group 1",
                vec![tag("TITLE", "old mch", vec!["old mch"])],
                1,
            ),
            tab(
                PresentationId::DvdAudioGroup(3),
                "Group 3",
                vec![tag("TITLE", "old stereo", vec!["old stereo"])],
                1,
            ),
        ];
        let mut state = state_with_tabs(tabs, 0);
        state.active_surface_mut().entries[0].value = "edited mch".to_string();
        state.active_surface_mut().entries[0].per_file_values = vec!["edited mch".to_string()];
        state.active_surface_mut().dirty = true;

        assert!(state.switch_presentation_tab(1));
        assert_eq!(state.presentation_tabs[0].entries[0].value, "edited mch");
        assert!(state.presentation_tabs[0].dirty);
        assert_eq!(state.active_surface().entries[0].value, "old stereo");

        assert!(state.switch_presentation_tab(0));
        assert_eq!(state.active_surface().entries[0].value, "edited mch");
    }


    #[test]
    fn content_tabs_keep_independent_scroll_offsets() {
        let entries: Vec<TagEntry> = (0..150)
            .map(|idx| tag("TITLE", &format!("Track {idx}"), vec!["value"]))
            .collect();
        let mut state = state_with_tabs(
            vec![tab(PresentationId::DvdAudioGroup(1), "Group 1", entries, 1)],
            0,
        );

        state.cursor = 100;
        state.scroll = 92;
        assert!(state.set_content_tab(ContentTab::Details));
        assert_eq!(state.scroll, 0, "new read-only tabs should start at top");

        state.scroll = 17;
        assert!(state.set_content_tab(ContentTab::ReplayGain));
        assert_eq!(state.scroll, 0, "each read-only tab has its own scroll");

        state.scroll = 5;
        assert!(state.set_content_tab(ContentTab::Details));
        assert_eq!(state.scroll, 17, "Details scroll should be restored");

        assert!(state.set_content_tab(ContentTab::Metadata));
        assert_eq!(state.scroll, 92, "Metadata scroll should not be clobbered by read-only tab scrolling");
    }

    #[test]
    fn returning_to_metadata_never_starts_scroll_after_cursor() {
        let entries: Vec<TagEntry> = (0..20)
            .map(|idx| tag("TITLE", &format!("Track {idx}"), vec!["value"]))
            .collect();
        let mut state = state_with_tabs(
            vec![tab(PresentationId::DvdAudioGroup(1), "Group 1", entries, 1)],
            0,
        );

        state.cursor = 4;
        state.content_tab = ContentTab::Details;
        state.scroll = 11;
        state.content_tab_scrolls[ContentTab::Details.index()] = 11;
        state.content_tab_scrolls[ContentTab::Metadata.index()] = 99;

        assert!(state.set_content_tab(ContentTab::Metadata));
        assert_eq!(state.scroll, 4, "selected metadata row must remain reachable after tab switch");
    }

    #[test]
    fn mark_active_presentation_saved_does_not_mark_dirty_sibling_tabs_saved() {
        let mut dirty_active = tab(
            PresentationId::DvdAudioGroup(1),
            "Group 1",
            vec![tag("TITLE", "old active", vec!["old active"])],
            1,
        );
        dirty_active.dirty = true;
        dirty_active.entries[0].value = "edited active".to_string();
        dirty_active.entries[0].per_file_values = vec!["edited active".to_string()];

        let mut dirty_sibling = tab(
            PresentationId::DvdAudioGroup(3),
            "Group 3",
            vec![tag("TITLE", "old sibling", vec!["old sibling"])],
            1,
        );
        dirty_sibling.dirty = true;
        dirty_sibling.entries[0].value = "edited sibling".to_string();
        dirty_sibling.entries[0].per_file_values = vec!["edited sibling".to_string()];

        let mut state = state_with_tabs(vec![dirty_active, dirty_sibling], 0);
        state.active_surface_mut().entries[0].value = "edited active".to_string();
        state.active_surface_mut().entries[0].per_file_values = vec!["edited active".to_string()];
        state.active_surface_mut().dirty = true;

        state.mark_active_presentation_saved();

        assert!(!state.presentation_tabs[0].dirty);
        assert_eq!(
            state.presentation_tabs[0].entries[0].original,
            "edited active"
        );
        assert!(state.presentation_tabs[1].dirty);
        assert_eq!(
            state.presentation_tabs[1].entries[0].original,
            "old sibling"
        );
        assert_eq!(state.dirty_presentation_count(), 1);
    }

    #[test]
    fn active_presentation_is_dirty_does_not_treat_dirty_sibling_as_active() {
        let active = tab(
            PresentationId::DvdAudioGroup(1),
            "Group 1",
            vec![tag("TITLE", "active", vec!["active"])],
            1,
        );
        let mut dirty_sibling = tab(
            PresentationId::DvdAudioGroup(3),
            "Group 3",
            vec![tag("TITLE", "sibling", vec!["sibling"])],
            1,
        );
        dirty_sibling.dirty = true;
        let mut state = state_with_tabs(vec![active, dirty_sibling], 0);

        assert!(state.any_presentation_dirty());
        assert!(!state.active_presentation_is_dirty());
        assert_eq!(state.dirty_presentation_count(), 1);
    }

    #[test]
    fn apply_active_musicbrainz_values_copies_only_matching_track_counts() {
        let tabs = vec![
            tab(
                PresentationId::DvdAudioGroup(1),
                "Group 1",
                vec![mb_tag("TITLE", "MB title", vec!["MB 01", "MB 02"])],
                2,
            ),
            tab(
                PresentationId::DvdAudioGroup(3),
                "Group 3",
                vec![tag("TITLE", "old stereo", vec!["old 01", "old 02"])],
                2,
            ),
            tab(
                PresentationId::DvdAudioGroup(4),
                "Group 4",
                vec![tag("TITLE", "bonus", vec!["bonus 01"])],
                1,
            ),
        ];
        let mut state = state_with_tabs(tabs, 0);

        let copied = state.apply_active_musicbrainz_values_to_matching_presentations();

        assert_eq!(copied, 1);
        assert_eq!(
            state.presentation_tabs[1].entries[0].per_file_values,
            vec!["MB 01".to_string(), "MB 02".to_string()]
        );
        assert_eq!(
            state.presentation_tabs[1].entries[0].per_file_originals,
            vec!["old 01".to_string(), "old 02".to_string()]
        );
        assert!(state.presentation_tabs[1].dirty);
        assert_eq!(state.presentation_tabs[2].entries[0].value, "bonus");
    }



    #[test]
    fn presentation_selector_always_uses_dropdown_when_present() {
        let one = vec![tab(
            PresentationId::DvdAudioGroup(1),
            "Group 1",
            vec![tag("TITLE", "one", vec!["one"])],
            1,
        )];
        let two = vec![
            tab(
                PresentationId::DvdAudioGroup(1),
                "Group 1",
                vec![tag("TITLE", "one", vec!["one"])],
                1,
            ),
            tab(
                PresentationId::DvdAudioGroup(2),
                "Group 2",
                vec![tag("TITLE", "two", vec!["two"])],
                1,
            ),
        ];
        let three = vec![
            tab(
                PresentationId::DvdAudioGroup(1),
                "Group 1",
                vec![tag("TITLE", "one", vec!["one"])],
                1,
            ),
            tab(
                PresentationId::DvdAudioGroup(2),
                "Group 2",
                vec![tag("TITLE", "two", vec!["two"])],
                1,
            ),
            tab(
                PresentationId::DvdAudioGroup(3),
                "Group 3",
                vec![tag("TITLE", "three", vec!["three"])],
                1,
            ),
        ];

        let one_state = state_with_tabs(one, 0);
        assert!(one_state.shows_presentation_control());
        assert!(!one_state.has_multiple_presentations());

        let two_state = state_with_tabs(two, 0);
        assert!(two_state.shows_presentation_control());
        assert!(two_state.has_multiple_presentations());

        let three_state = state_with_tabs(three, 0);
        assert!(three_state.shows_presentation_control());
        assert!(three_state.has_multiple_presentations());
    }

    #[test]
    fn presentation_selector_navigation_defers_switch_until_selection() {
        let tabs = vec![
            tab(
                PresentationId::DvdAudioGroup(1),
                "Group 1",
                vec![tag("TITLE", "one", vec!["one"])],
                1,
            ),
            tab(
                PresentationId::DvdAudioGroup(2),
                "Group 2",
                vec![tag("TITLE", "two", vec!["two"])],
                1,
            ),
            tab(
                PresentationId::DvdAudioGroup(3),
                "Group 3",
                vec![tag("TITLE", "three", vec!["three"])],
                1,
            ),
        ];
        let mut state = state_with_tabs(tabs, 0);

        assert!(state.open_presentation_selector());
        assert!(state.presentation_selector_open);
        assert_eq!(state.presentation_selector_cursor, 0);
        assert!(state.move_presentation_selector_cursor(2));
        assert_eq!(state.presentation_selector_cursor, 2);
        assert_eq!(state.active_tab, 0);
        assert_eq!(state.active_surface().entries[0].value, "one");

        assert!(state.select_presentation_selector_cursor());
        assert!(!state.presentation_selector_open);
        assert_eq!(state.active_tab, 2);
        assert_eq!(state.active_surface().entries[0].value, "three");
    }

    #[test]
    fn presentation_selector_mouse_scroll_keeps_cursor_in_view_without_selecting() {
        let tabs = vec![
            tab(
                PresentationId::DvdAudioGroup(1),
                "Group 1",
                vec![tag("TITLE", "one", vec!["one"])],
                1,
            ),
            tab(
                PresentationId::DvdAudioGroup(2),
                "Group 2",
                vec![tag("TITLE", "two", vec!["two"])],
                1,
            ),
            tab(
                PresentationId::DvdAudioGroup(3),
                "Group 3",
                vec![tag("TITLE", "three", vec!["three"])],
                1,
            ),
            tab(
                PresentationId::DvdAudioGroup(4),
                "Group 4",
                vec![tag("TITLE", "four", vec!["four"])],
                1,
            ),
            tab(
                PresentationId::DvdAudioGroup(5),
                "Group 5",
                vec![tag("TITLE", "five", vec!["five"])],
                1,
            ),
        ];
        let mut state = state_with_tabs(tabs, 0);

        assert!(state.open_presentation_selector());
        assert!(state.scroll_presentation_selector(1, 3));
        assert_eq!(state.presentation_selector_scroll, 1);
        assert_eq!(state.presentation_selector_cursor, 1);
        assert_eq!(state.active_tab, 0);

        assert!(state.scroll_presentation_selector(1, 3));
        assert_eq!(state.presentation_selector_scroll, 2);
        assert_eq!(state.presentation_selector_cursor, 2);
        assert_eq!(state.active_tab, 0);

        assert!(state.scroll_presentation_selector(-1, 3));
        assert_eq!(state.presentation_selector_scroll, 1);
        assert_eq!(state.presentation_selector_cursor, 2);
        assert_eq!(state.active_tab, 0);
    }

    #[test]
    fn apply_active_musicbrainz_values_preserves_internal_and_non_mb_fields() {
        let tabs = vec![
            tab(
                PresentationId::DvdAudioGroup(1),
                "Group 1",
                vec![
                    tag("DVDA_GROUP", "1", vec!["1", "1"]),
                    mb_tag("TITLE", "MB title", vec!["MB 01", "MB 02"]),
                    tag("COMMENT", "active note", vec!["active 01", "active 02"]),
                ],
                2,
            ),
            tab(
                PresentationId::DvdAudioGroup(3),
                "Group 3",
                vec![
                    tag("DVDA_GROUP", "3", vec!["3", "3"]),
                    tag("TITLE", "old stereo", vec!["old 01", "old 02"]),
                    tag("COMMENT", "destination note", vec!["dest 01", "dest 02"]),
                ],
                2,
            ),
        ];
        let mut state = state_with_tabs(tabs, 0);

        let copied = state.apply_active_musicbrainz_values_to_matching_presentations();

        assert_eq!(copied, 1);
        let dest_entries = &state.presentation_tabs[1].entries;
        let by_key = |key: &str| {
            dest_entries
                .iter()
                .find(|entry| entry.display_key.eq_ignore_ascii_case(key))
                .expect(key)
        };

        assert_eq!(
            by_key("TITLE").per_file_values,
            vec!["MB 01".to_string(), "MB 02".to_string()]
        );
        assert_eq!(by_key("DVDA_GROUP").value, "3");
        assert_eq!(
            by_key("DVDA_GROUP").per_file_values,
            vec!["3".to_string(), "3".to_string()]
        );
        assert_eq!(by_key("COMMENT").value, "destination note");
        assert_eq!(
            by_key("COMMENT").per_file_values,
            vec!["dest 01".to_string(), "dest 02".to_string()]
        );
    }
    fn probe_file_detail(path: &str) -> MetadataFileDetails {
        MetadataFileDetails::from_open_cache(
            std::path::PathBuf::from(path),
            Some(100),
            None,
            None,
            None,
            FileReadState::Readable,
            FileWriteEligibility::Writable,
            SourceMetadata::default(),
        )
    }

    fn write_file_detail(path: &str, eligibility: FileWriteEligibility) -> MetadataFileDetails {
        MetadataFileDetails::from_open_cache(
            std::path::PathBuf::from(path),
            Some(100),
            None,
            None,
            None,
            FileReadState::Readable,
            eligibility,
            SourceMetadata::default(),
        )
    }

    fn write_state() -> MetadataEditorState {
        let paths = vec![
            std::path::PathBuf::from("/tmp/one.flac"),
            std::path::PathBuf::from("/tmp/two.flac"),
        ];
        let entries = vec![tag("TITLE", "old", vec!["old one", "old two"])];
        let details = MetadataTechnicalDetails::from_files(vec![
            write_file_detail("/tmp/one.flac", FileWriteEligibility::Writable),
            write_file_detail("/tmp/two.flac", FileWriteEligibility::Writable),
        ]);
        MetadataEditorState::for_files(
            paths,
            entries,
            vec!["one".to_string(), "two".to_string()],
            details,
        )
    }

    #[test]
    fn stale_save_completion_wrong_session_is_ignored() {
        let mut state = write_state();
        let (session_id, generation) = state.begin_write();
        state.active_surface_mut().entries[0].per_file_values[0] = "new one".to_string();

        let ignored = state.apply_write_results(
            session_id.saturating_add(10_000),
            generation,
            vec![MetadataEditorWriteResult::saved(state.active_surface().paths[0].clone())],
        );

        assert!(ignored.is_none());
        assert_eq!(state.active_surface().entries[0].per_file_originals[0], "old one");
        assert_eq!(state.active_surface().technical_details.active_save_generation, Some(generation));
    }

    #[test]
    fn stale_save_completion_wrong_generation_is_ignored() {
        let mut state = write_state();
        let (session_id, generation) = state.begin_write();
        state.active_surface_mut().entries[0].per_file_values[0] = "new one".to_string();

        let ignored = state.apply_write_results(
            session_id,
            generation.saturating_add(1),
            vec![MetadataEditorWriteResult::saved(state.active_surface().paths[0].clone())],
        );

        assert!(ignored.is_none());
        assert_eq!(state.active_surface().entries[0].per_file_originals[0], "old one");
        assert_eq!(state.active_surface().technical_details.active_save_generation, Some(generation));
    }

    #[test]
    fn partial_save_updates_originals_for_successful_files_only() {
        let mut state = write_state();
        state.active_surface_mut().entries[0].per_file_values = vec!["new one".to_string(), "new two".to_string()];
        state.active_surface_mut().dirty = true;
        let (session_id, generation) = state.begin_write();

        let summary = state
            .apply_write_results(
                session_id,
                generation,
                vec![
                    MetadataEditorWriteResult::saved(state.active_surface().paths[0].clone()),
                    MetadataEditorWriteResult::failed(state.active_surface().paths[1].clone(), "disk full"),
                ],
            )
            .expect("matching save result should reduce");

        assert_eq!(summary.saved, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(state.active_surface().entries[0].per_file_originals[0], "new one");
        assert_eq!(state.active_surface().entries[0].per_file_originals[1], "old two");
        assert!(state.active_surface().dirty, "failed file remains dirty");
    }

    #[test]
    fn failed_and_skipped_writes_become_durable_file_issues() {
        let mut state = write_state();
        state.active_surface_mut().entries[0].per_file_values = vec!["new one".to_string(), "new two".to_string()];
        let (session_id, generation) = state.begin_write();

        let summary = state
            .apply_write_results(
                session_id,
                generation,
                vec![
                    MetadataEditorWriteResult::failed(state.active_surface().paths[0].clone(), "permission denied"),
                    MetadataEditorWriteResult::skipped(state.active_surface().paths[1].clone(), "read-only"),
                ],
            )
            .expect("matching save result should reduce");

        assert_eq!(summary.failed, 1);
        assert_eq!(summary.skipped, 1);
        assert!(state.active_surface().technical_details.files[0]
            .issues
            .iter()
            .any(|issue| matches!(issue, MetadataIssue::Write { reason, .. } if reason == "permission denied")));
        assert!(state.active_surface().technical_details.files[1]
            .issues
            .iter()
            .any(|issue| matches!(issue, MetadataIssue::SaveBlocked { reason, .. } if reason == "read-only")));
    }

    #[test]
    fn partial_delete_retries_only_unsaved_slots() {
        let mut state = write_state();
        state.active_surface_mut().deleted.push(0);
        state.active_surface_mut().dirty = true;
        let (session_id, generation) = state.begin_write();

        let summary = state
            .apply_write_results(
                session_id,
                generation,
                vec![
                    MetadataEditorWriteResult::saved(state.active_surface().paths[0].clone()),
                    MetadataEditorWriteResult::failed(state.active_surface().paths[1].clone(), "locked"),
                ],
            )
            .expect("matching save result should reduce");

        assert_eq!(summary.saved, 1);
        assert_eq!(summary.failed, 1);
        assert!(state.active_surface().deleted.is_empty(), "row-level delete is reduced to per-slot state");
        assert_eq!(state.active_surface().entries[0].per_file_values[0], "");
        assert_eq!(state.active_surface().entries[0].per_file_originals[0], "");
        assert_eq!(state.active_surface().entries[0].per_file_values[1], "");
        assert_eq!(state.active_surface().entries[0].per_file_originals[1], "old two");
    }


    #[test]
    fn partial_save_preserves_non_file_aligned_deleted_rows() {
        let mut state = write_state();
        let mut synthetic = tag("CUESHEET", "cue", vec!["cue row one"]);
        synthetic.per_file_values = vec!["cue row one".to_string(), "cue row two".to_string(), "cue row three".to_string()];
        synthetic.per_file_originals = synthetic.per_file_values.clone();
        state.active_surface_mut().entries.push(synthetic);
        state.active_surface_mut().deleted.push(1);
        state.active_surface_mut().dirty = true;
        let (session_id, generation) = state.begin_write();

        let summary = state
            .apply_write_results(
                session_id,
                generation,
                vec![MetadataEditorWriteResult::saved(state.active_surface().paths[0].clone())],
            )
            .expect("matching save result should reduce");

        assert_eq!(summary.saved, 1);
        assert!(summary.remaining_dirty, "summary must report retained dirty model state");
        assert!(
            !summary.all_saved(),
            "a fully successful path-keyed save must not close the editor when non-file-aligned dirty state remains"
        );
        assert_eq!(state.active_surface().deleted, vec![1]);
        assert_eq!(state.active_surface().entries[1].display_key, "CUESHEET");
        assert!(state.active_surface().dirty, "non-file-aligned delete must remain pending");
    }

    #[test]
    fn successful_path_keyed_save_with_retained_dirty_state_is_not_all_saved() {
        let mut state = write_state();
        let mut synthetic = tag("CUESHEET", "cue", vec!["cue row one"]);
        synthetic.per_file_values = vec![
            "cue row one".to_string(),
            "cue row two".to_string(),
            "cue row three".to_string(),
        ];
        synthetic.per_file_originals = synthetic.per_file_values.clone();
        state.active_surface_mut().entries.push(synthetic);
        state.active_surface_mut().deleted.push(1);
        state.active_surface_mut().dirty = true;
        let (session_id, generation) = state.begin_write();

        let summary = state
            .apply_write_results(
                session_id,
                generation,
                vec![MetadataEditorWriteResult::saved(state.active_surface().paths[0].clone())],
            )
            .expect("matching save result should reduce");

        assert_eq!(summary.failed, 0);
        assert_eq!(summary.skipped, 0);
        assert_eq!(summary.ignored, 0);
        assert!(summary.remaining_dirty);
        assert!(!summary.all_saved());
        assert!(state.active_surface().dirty);
        assert_eq!(state.active_surface().deleted, vec![1]);
    }

    #[test]
    fn constructors_seed_file_surface_invariants() {
        let paths = vec![std::path::PathBuf::from("/tmp/one.flac")];
        let state = MetadataEditorState::for_files(
            paths.clone(),
            vec![tag("TITLE", "one", vec!["one"])],
            vec!["one".to_string()],
            MetadataTechnicalDetails::from_files(vec![write_file_detail(
                "/tmp/one.flac",
                FileWriteEligibility::Writable,
            )]),
        );

        assert_eq!(state.active_surface().paths, paths);
        assert_eq!(state.active_surface().entries.len(), 1);
        assert_eq!(state.active_surface().technical_details.files.len(), 1);
        assert_eq!(state.content_tab, ContentTab::Metadata);
    }

    fn probe_source_info() -> SourceInfo {
        SourceInfo {
            format_name: "FLAC".to_string(),
            codec: "FLAC".to_string(),
            bit_depth: Some(24),
            sample_rate: 96_000,
            channels: 2,
            channel_layout: "stereo".to_string(),
            duration_secs: 1.0,
            file_size: 100,
        }
    }

    #[test]
    fn details_probe_completion_ignores_wrong_session_id() {
        let mut details = MetadataTechnicalDetails::from_files(vec![probe_file_detail("/tmp/a.flac")]);
        let session = details.session_id;
        let generation = 1;
        details.details_probe_state = MetadataDetailsProbeState::Loading {
            generation,
            completed: 0,
            total: 1,
        };
        details.files[0].media_facts = ProbeState::Loading { generation };

        let status = details.apply_details_probe_results(
            session.saturating_add(1),
            generation,
            vec![MetadataDetailsProbeFileResult {
                index: 0,
                path: std::path::PathBuf::from("/tmp/a.flac"),
                result: Ok(probe_source_info()),
            }],
        );

        assert!(status.is_none());
        assert!(matches!(details.files[0].media_facts, ProbeState::Loading { generation: 1 }));
    }

    #[test]
    fn details_probe_completion_validates_result_path() {
        let mut details = MetadataTechnicalDetails::from_files(vec![probe_file_detail("/tmp/a.flac")]);
        let session = details.session_id;
        let generation = 1;
        details.details_probe_state = MetadataDetailsProbeState::Loading {
            generation,
            completed: 0,
            total: 1,
        };
        details.files[0].media_facts = ProbeState::Loading { generation };

        let status = details.apply_details_probe_results(
            session,
            generation,
            vec![MetadataDetailsProbeFileResult {
                index: 0,
                path: std::path::PathBuf::from("/tmp/b.flac"),
                result: Ok(probe_source_info()),
            }],
        );

        assert!(status.as_deref().unwrap_or("").contains("partially loaded"));
        assert!(matches!(details.files[0].media_facts, ProbeState::Failed { .. }));
    }

    #[test]
    fn details_probe_completion_applies_to_matching_presentation_session() {
        let mut tabs = vec![
            tab(
                PresentationId::DvdAudioGroup(1),
                "Group 1",
                vec![tag("TITLE", "one", vec!["one"])],
                1,
            ),
            tab(
                PresentationId::DvdAudioGroup(2),
                "Group 2",
                vec![tag("TITLE", "two", vec!["two"])],
                1,
            ),
        ];
        tabs[0].technical_details = MetadataTechnicalDetails::from_files(vec![probe_file_detail("/tmp/a.flac")]);
        tabs[1].technical_details = MetadataTechnicalDetails::from_files(vec![probe_file_detail("/tmp/b.flac")]);
        let session = tabs[0].technical_details.session_id;
        let generation = 1;
        tabs[0].technical_details.details_probe_state = MetadataDetailsProbeState::Loading {
            generation,
            completed: 0,
            total: 1,
        };
        tabs[0].technical_details.files[0].media_facts = ProbeState::Loading { generation };

        let mut state = state_with_tabs(tabs, 1);
        let status = state.apply_details_probe_results(
            session,
            generation,
            vec![MetadataDetailsProbeFileResult {
                index: 0,
                path: std::path::PathBuf::from("/tmp/a.flac"),
                result: Ok(probe_source_info()),
            }],
        );

        assert!(status.as_deref().unwrap_or("").contains("Details ready"));
        assert!(matches!(
            state.presentation_tabs[0].technical_details.files[0].media_facts,
            ProbeState::Ready(_)
        ));
        assert!(matches!(
            state.presentation_tabs[1].technical_details.files[0].media_facts,
            ProbeState::NotLoaded
        ));
    }

    #[test]
    fn read_only_tab_scroll_clamps_at_input_time() {
        let tabs = vec![tab(
            PresentationId::DvdAudioGroup(1),
            "Group 1",
            vec![tag("TITLE", "one", vec!["one"])],
            1,
        )];
        let mut state = state_with_tabs(tabs, 0);
        state.content_tab = ContentTab::Details;
        state.scroll = 10_000;
        state.content_tab_scrolls[ContentTab::Details.index()] = 10_000;

        assert!(state.clamp_read_only_content_scroll(12, 5));
        assert_eq!(state.scroll, 7);
        assert_eq!(state.content_tab_scrolls[ContentTab::Details.index()], 7);

        state.scroll_read_only_content_by(10, 12, 5);
        assert_eq!(state.scroll, 7);
        assert_eq!(state.content_tab_scrolls[ContentTab::Details.index()], 7);

        state.scroll_read_only_content_by(-3, 12, 5);
        assert_eq!(state.scroll, 4);
        assert_eq!(state.content_tab_scrolls[ContentTab::Details.index()], 4);
    }

    #[test]
    fn read_only_tab_scroll_noops_for_metadata_tab() {
        let tabs = vec![tab(
            PresentationId::DvdAudioGroup(1),
            "Group 1",
            vec![tag("TITLE", "one", vec!["one"])],
            1,
        )];
        let mut state = state_with_tabs(tabs, 0);
        state.content_tab = ContentTab::Metadata;
        state.scroll = 3;

        assert!(!state.scroll_read_only_content_by(1, 12, 5));
        assert_eq!(state.scroll, 3);
    }

    #[test]
    fn retry_failed_details_probes_clears_only_failed_files() {
        let ok_info = crate::tui::probe::SourceInfo {
            format_name: "flac".to_string(),
            codec: "FLAC".to_string(),
            bit_depth: Some(24),
            sample_rate: 96_000,
            channels: 2,
            channel_layout: "stereo".to_string(),
            duration_secs: 1.0,
            file_size: 1024,
        };
        let mut ok = MetadataFileDetails::default();
        ok.file_facts.path = std::path::PathBuf::from("/album/ok.flac");
        ok.media_facts = ProbeState::Ready(MediaFacts::from(&ok_info));

        let mut failed = MetadataFileDetails::default();
        failed.file_facts.path = std::path::PathBuf::from("/album/transient.flac");
        failed.media_facts = ProbeState::Failed {
            reason: "temporary I/O error".to_string(),
            retryable: true,
        };
        failed.issues.push(MetadataIssue::Probe {
            path: failed.file_facts.path.clone(),
            reason: "temporary I/O error".to_string(),
            retryable: true,
        });

        let mut details = MetadataTechnicalDetails::from_files(vec![ok, failed]);
        details.details_probe_state = MetadataDetailsProbeState::Partial {
            issues: vec!["temporary I/O error".to_string()],
        };

        let cleared = details.retry_failed_details_probes();

        assert_eq!(cleared, 1);
        assert!(matches!(details.files[0].media_facts, ProbeState::Ready(_)), "successful probe data stays cached");
        assert!(matches!(details.files[1].media_facts, ProbeState::NotLoaded), "failed probe becomes retryable");
        assert!(details.files[1].issues.iter().all(|issue| !matches!(issue, MetadataIssue::Probe { .. })));
        assert!(matches!(details.details_probe_state, MetadataDetailsProbeState::Unloaded));
    }

}
