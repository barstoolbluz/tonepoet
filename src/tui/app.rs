//! Application state for the standalone TUI

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::config::TonepoetConfig;
use crate::convert::formats::AudioFormat;
use crate::convert::simple_wizard::DitherType;
use tonepoet_pipeline::enums::{
    DsdFilterPreset, DsdNoiseShaper, ModulatorOrder,
};
use tonepoet_pipeline::{DbNano, DsdReconstructionSelection, DsdSourcePathway};
use crate::convert::{ConversionConfig, ConversionItem, ConversionManager};
use crate::tui::button_map::{ButtonRenderMap, DoubleClickState};
use crate::tui::pill::PillState;
use crate::tui::probe::{SourceInfo, SourceMetadata};

/// Upper bound for retained Browse archive listings. Listing large archives can
/// allocate substantial path metadata, so the cache is deliberately small and
/// session-scoped.
const ARCHIVE_LISTING_CACHE_MAX_ENTRIES: usize = 32;

/// Soft heap budget for retained Browse archive listings. Entries larger than
/// this are still usable, but they are not retained after the first open.
const ARCHIVE_LISTING_CACHE_MAX_BYTES: usize = 32 * 1024 * 1024;

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
    AlbumIfMissing,
    TrackIfMissing,
    BothIfMissing,
}

/// Bit depth options including float formats
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BitDepthChoice {
    Source,
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
            Self::Source => 0,
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
            Self::Source => 0,
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

    pub fn is_source(&self) -> bool {
        matches!(self, Self::Source)
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

/// Native-v2 DSD-source gain policy exposed in the format pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdGainMode {
    /// Exact legacy no-gain behavior.
    Disabled,
    /// Exact legacy peak-normalize behavior.
    Auto,
    Reference,
    NativeLevel,
    /// Native fixed gain or legacy manual gain, depending on settings origin.
    Fixed,
    NormalizePeak,
}

impl DsdGainMode {
    /// Stable preset key independent of presentation labels.
    pub const fn preset_key(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Auto => "auto",
            Self::Reference => "reference",
            Self::NativeLevel => "native",
            Self::Fixed => "fixed",
            Self::NormalizePeak => "normalize",
        }
    }
}

/// User-editable manual gain range for DSD-to-PCM conversions.
/// Matches `DsdSettings::validate()` so the TUI cannot stage an invalid value.
pub const DSD_TO_PCM_GAIN_DB_MIN: f32 = -24.0;
pub const DSD_TO_PCM_GAIN_DB_MAX: f32 = 24.0;
/// Keyboard step for the manual gain row. Fine enough for mastering-level
/// adjustments while still making large changes practical with repeats.
pub const DSD_TO_PCM_GAIN_DB_STEP: f32 = 0.25;
const DSD_TO_PCM_GAIN_DB_MIN_NANO: i64 = -24_000_000_000;
const DSD_TO_PCM_GAIN_DB_MAX_NANO: i64 = 24_000_000_000;
const DSD_TO_PCM_GAIN_DB_STEP_NANO: i64 = 250_000_000;
const DSD_AUTO_GAIN_MARGIN_DB_MIN_NANO: i64 = 0;
const DSD_AUTO_GAIN_MARGIN_DB_MAX_NANO: i64 = 6_000_000_000;
const DSD_AUTO_GAIN_MARGIN_DB_STEP_NANO: i64 = 50_000_000;

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
    crate::tui::probe::recover_flac_metadata_before_read(path).ok()?;
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


pub(crate) fn is_cue_sheet_path_for_preview(path: &Path) -> bool {
    crate::convert::classify::is_cue_sheet_path(path)
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
            crate::tui::browse::CueReferenceResolution::UnsupportedTarget(path) => {
                resolution_errors.push(format!(
                    "FILE {:?} exists but is not supported audio ({})",
                    file_ref,
                    path.display()
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

/// Durable source-pane text used while a Convert-source probe is in flight.
pub(crate) const PROBE_IN_PROGRESS_NOTICE: &str = "Probing...";
pub(crate) const ARCHIVE_PREVIEW_EXTRACTING_NOTICE: &str = "Extracting archive...";


/// Return true when the authoritative source-admission policy routes `path`
/// through archive extraction/preview rather than ordinary audio probing.
///
/// This wrapper exists for the probe scheduler; it deliberately owns no
/// extension list so `:e`, Browse, recent-source, file-input, and queue paths
/// cannot drift from archive-preview admission.
pub(crate) fn is_nonprobeable_source_for_probe(path: &Path) -> bool {
    crate::convert::source_admission::is_archive_preview_source_path(path)
}

#[cfg(test)]
mod direct_source_probe_policy_tests {
    use super::is_nonprobeable_source_for_probe;
    use std::path::Path;

    #[test]
    fn every_supported_archive_preview_source_bypasses_audio_probe() {
        for name in [
            "album.7z",
            "album.zip",
            "album.rar",
            "album.tar",
            "album.cab",
            "album.dmg",
            "album.tgz",
            "album.tbz2",
            "album.txz",
            "album.tar.gz",
            "album.tar.bz2",
            "album.tar.xz",
            "album.tar.zst",
            "album.tar.lz",
            "album.tar.lzma",
        ] {
            assert!(is_nonprobeable_source_for_probe(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn audio_cue_and_generic_iso_do_not_enter_archive_preview() {
        for name in ["track.flac", "disc.cue", "disc.iso", "notes.txt"] {
            assert!(!is_nonprobeable_source_for_probe(Path::new(name)), "{name}");
        }
    }
}

pub(crate) fn source_probe_initial_notice(path: &Path) -> Option<String> {
    if is_nonprobeable_source_for_probe(path) {
        None
    } else {
        Some(PROBE_IN_PROGRESS_NOTICE.to_string())
    }
}

/// Editable Convert metadata fields captured when an async source probe starts.
/// Probe completion applies discovered tags only if these fields are still
/// unchanged, preventing late Lofty results from overwriting user edits made
/// while the UI stayed responsive.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvertProbeMetadataSnapshot {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist_for_conversion: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
}

impl ConvertProbeMetadataSnapshot {
    pub fn capture(metadata: &MetadataState) -> Self {
        Self {
            title: metadata.title.clone(),
            artist: metadata.artist.clone(),
            album: metadata.album.clone(),
            album_artist_for_conversion: metadata.album_artist_for_conversion.clone(),
            genre: metadata.genre.clone(),
            year: metadata.year.clone(),
        }
    }
}

/// Output-format fields that source probing is allowed to auto-default. The
/// completion reducer compares this snapshot before applying source-derived
/// defaults so user choices made during a probe are not reset on completion.
/// How much the TUI currently knows about the probed source's DSD/PCM
/// identity. This drives the same-as-source rate pill's availability on DSD
/// targets: fresh/unstaged state is permissive (presets can stage
/// source-relative policy with no source loaded), a KNOWN PCM source makes
/// rate=source invalid for a DSD target (PCM->DSD needs an explicit rate),
/// and a LOST source retains the deliberate selection disabled until a new
/// probe revalidates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRateIdentity {
    Unstaged,
    Known,
    Lost,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConvertProbeFormatSnapshot {
    pub format: AudioFormat,
    pub sample_rate: u32,
    pub bit_depth: BitDepthChoice,
    pub resampler: ResamplerChoice,
    pub dither: DitherType,
    pub source_is_dsd: bool,
    pub source_rate_identity: SourceRateIdentity,
    pub source_derived_sample_rate: Option<u32>,
    pub source_derived_bit_depth: Option<BitDepthChoice>,
    pub sample_rate_overridden: bool,
    pub bit_depth_overridden: bool,
    pub dither_overridden: bool,
    pub resampler_overridden: bool,
}

impl ConvertProbeFormatSnapshot {
    pub fn capture(format: &FormatState) -> Self {
        Self {
            format: *format.format.selected_value(),
            sample_rate: *format.sample_rate.selected_value(),
            bit_depth: *format.bit_depth.selected_value(),
            resampler: *format.resampler.selected_value(),
            dither: *format.dither.selected_value(),
            source_is_dsd: format.source_is_dsd,
            source_rate_identity: format.source_rate_identity,
            source_derived_sample_rate: format.source_derived_sample_rate,
            source_derived_bit_depth: format.source_derived_bit_depth,
            sample_rate_overridden: format.sample_rate_overridden,
            bit_depth_overridden: format.bit_depth_overridden,
            dither_overridden: format.dither_overridden,
            resampler_overridden: format.resampler_overridden,
        }
    }
}

/// User-edit snapshot captured at async source-probe dispatch time.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvertProbeBaseline {
    pub metadata: ConvertProbeMetadataSnapshot,
    pub format: ConvertProbeFormatSnapshot,
}

impl ConvertProbeBaseline {
    pub fn capture(convert: &ConvertState) -> Self {
        Self {
            metadata: ConvertProbeMetadataSnapshot::capture(&convert.metadata),
            format: ConvertProbeFormatSnapshot::capture(&convert.format),
        }
    }
}

pub(crate) fn apply_source_metadata_to_convert(
    convert: &mut ConvertState,
    metadata: &SourceMetadata,
) {
    convert.metadata.title = metadata.title.clone();
    convert.metadata.artist = metadata.artist.clone();
    convert.metadata.album = metadata.album.clone();
    convert.metadata.genre = metadata.genre.clone();
    convert.metadata.year = metadata.year.clone();
}

pub(crate) fn clear_source_metadata_in_convert(convert: &mut ConvertState) {
    apply_source_metadata_to_convert(convert, &SourceMetadata::default());
}

fn probe_convert_source_for_message(
    path: &Path,
) -> (Option<SourceInfo>, SourceMetadata, Option<String>) {
    if is_cue_sheet_path_for_preview(path) {
        return match probe_cue_proxy_source(path) {
            Ok(result) => (result.info, result.metadata, result.probe_notice),
            Err(err) => (
                None,
                SourceMetadata::default(),
                Some(format!(
                    "CUE proxy probe failed: {}; set format manually",
                    err
                )),
            ),
        };
    }

    match crate::tui::probe::probe_audio(path) {
        Ok(info) => {
            let metadata = crate::tui::probe::read_metadata(path).unwrap_or_default();
            (Some(info), metadata, None)
        }
        Err(err) => (
            None,
            SourceMetadata::default(),
            Some(format!("Probe failed: {}; set format manually", err)),
        ),
    }
}

pub(crate) fn spawn_convert_source_probe(
    generation: u64,
    path: PathBuf,
    baseline: ConvertProbeBaseline,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) {
    if is_nonprobeable_source_for_probe(&path) {
        return;
    }

    tokio::spawn(async move {
        let result_path = path.clone();
        let source_mode = tokio::task::spawn_blocking(move || {
            let (info, metadata, probe_notice) = probe_convert_source_for_message(&path);
            SourceMode::from_single_with_probe_notice(path, info, metadata, probe_notice)
        })
        .await
        .unwrap_or_else(|err| {
            SourceMode::Single {
                path: result_path.clone(),
                info: None,
                metadata: SourceMetadata::default(),
                probe_notice: Some(format!(
                    "Probe task failed: {}; set format manually",
                    err
                )),
            }
        });

        let _ = tx
            .send(crate::tui::message::AppMessage::ProbeResult {
                generation,
                path: result_path,
                source_mode,
                baseline,
            })
            .await;
    });
}

pub(crate) fn source_mode_from_archive_preview(preview: ArchivePreview) -> SourceMode {
    let track_count = preview.tracks.len();
    let info = preview.tracks.first().map(|track| track.info.clone());
    let metadata = preview.album_metadata.clone();
    let tracks = preview
        .tracks
        .iter()
        .enumerate()
        .map(|(idx, track)| MultiTrackEntry {
            number: (idx + 1) as u32,
            title: track.metadata.title.clone().or_else(|| Some(track.original_name.clone())),
            performer: track.metadata.artist.clone(),
            duration_display: Some(track.info.duration_display()),
        })
        .collect();
    let archive_path = preview.archive_path.clone();
    let album_title = metadata.album.clone();
    let album_artist = metadata.artist.clone();

    SourceMode::MultiTrack {
        path: archive_path,
        info,
        metadata,
        tracks,
        area_label: Some("Archive".to_string()),
        album_title,
        album_artist,
        probe_notice: None,
        scroll: 0,
        cursor: 0,
        selected: vec![true; track_count],
        archive_preview: Some(preview),
        disc_contents: None,
        selected_presentation_id: None,
    }
}

pub(crate) fn archive_preview_album_metadata(tracks: &[PreviewTrack]) -> SourceMetadata {
    let mut metadata = tracks
        .first()
        .map(|track| track.metadata.clone())
        .unwrap_or_default();
    metadata.title = None;

    let same_album = common_nonempty_metadata_value(tracks, |metadata| metadata.album.as_ref());
    let same_artist = common_nonempty_metadata_value(tracks, |metadata| metadata.artist.as_ref());
    let same_year = common_nonempty_metadata_value(tracks, |metadata| metadata.year.as_ref());
    let same_genre = common_nonempty_metadata_value(tracks, |metadata| metadata.genre.as_ref());

    metadata.album = same_album;
    metadata.artist = same_artist;
    metadata.year = same_year;
    metadata.genre = same_genre;
    metadata
}

fn common_nonempty_metadata_value<F>(tracks: &[PreviewTrack], f: F) -> Option<String>
where
    F: for<'a> Fn(&'a SourceMetadata) -> Option<&'a String>,
{
    let mut values = tracks
        .iter()
        .filter_map(|track| f(&track.metadata))
        .filter(|value| !value.trim().is_empty());
    let first = values.next()?.clone();
    if values.all(|value| value == &first) {
        Some(first)
    } else {
        None
    }
}

pub(crate) fn create_pending_archive_preview(
    generation: u64,
    archive_path: PathBuf,
) -> PendingArchivePreview {
    PendingArchivePreview {
        generation,
        archive_path,
        staging_dir: std::env::temp_dir().join(format!(
            "tonepoet-archive-preview-{}",
            uuid::Uuid::new_v4()
        )),
        cancel: tokio_util::sync::CancellationToken::new(),
    }
}

pub(crate) fn stored_archive_password(app: &mut AppState) -> Result<Option<String>, String> {
    if let Some(password) = app.config.conversion.archive_password.clone() {
        return Ok(Some(password));
    }
    if let Some(reference) = app.config.conversion.archive_password_ref.as_deref() {
        return crate::secret_store::get(reference)
            .map(Some)
            .map_err(|error| format!("cannot resolve configured archive password: {error}"));
    }
    app.keychain
        .ensure_loaded()
        .map_err(|error| format!("cannot resolve stored archive passwords: {error}"))?;
    Ok(app.keychain.passwords.first().cloned())
}

pub(crate) fn archive_password_for_path(
    app: &mut AppState,
    path: &Path,
) -> Result<Option<String>, String> {
    if let Some(password) = app.archive_passwords.get(path).cloned() {
        return Ok(Some(password));
    }
    stored_archive_password(app).map_err(|error| {
        format!("{error} for '{}'; the operation was not started", path.display())
    })
}

pub(crate) fn archive_preview_password_for_path(
    app: &mut AppState,
    path: &Path,
) -> Result<Option<String>, String> {
    if let Some(password) = app.archive_passwords.get(path).cloned() {
        return Ok(Some(password));
    }
    stored_archive_password(app)
        .map_err(|error| format!("{error} for '{}'", path.display()))
}

/// Successful archive-preview dispatch. The generation/path pair is the exact
/// ownership identity installed into `ConvertState` before the worker starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArchivePreviewStarted {
    pub generation: u64,
    pub archive_path: PathBuf,
}

/// Preflight failures for archive-preview activation. Every variant is raised
/// before the current Convert source, metadata, screen, generation, recents, or
/// pending-preview ownership is changed.
#[derive(Debug)]
pub(crate) enum ArchivePreviewStartError {
    UnsupportedPath(PathBuf),
    WorkerChannelClosed,
    RuntimeUnavailable(String),
    GenerationExhausted,
    PasswordResolution(String),
    StagingAllocation {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for ArchivePreviewStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPath(path) => write!(
                formatter,
                "Archive-preview activation refused for unsupported path: {}",
                path.display()
            ),
            Self::WorkerChannelClosed => formatter.write_str(
                "Cannot open archive preview: the TUI worker channel is closed; the operation was not started",
            ),
            Self::RuntimeUnavailable(error) => write!(
                formatter,
                "Cannot open archive preview: no active asynchronous runtime is available ({error}); the operation was not started",
            ),
            Self::GenerationExhausted => formatter.write_str(
                "Cannot open archive preview: source generation counter is exhausted; the operation was not started",
            ),
            Self::PasswordResolution(error) => write!(
                formatter,
                "Cannot open archive preview: {error}; the operation was not started"
            ),
            Self::StagingAllocation { path, source } => write!(
                formatter,
                "Cannot open archive preview: failed to allocate staging directory {}: {}; the operation was not started",
                path.display(),
                source
            ),
        }
    }
}

impl std::error::Error for ArchivePreviewStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StagingAllocation { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Removes a preflight-created staging directory unless ownership is
/// transferred to the archive-preview worker.
struct ArchivePreviewStagingGuard {
    path: PathBuf,
    armed: bool,
}

impl ArchivePreviewStagingGuard {
    fn create(path: PathBuf) -> Result<Self, ArchivePreviewStartError> {
        std::fs::create_dir(&path).map_err(|source| {
            ArchivePreviewStartError::StagingAllocation {
                path: path.clone(),
                source,
            }
        })?;
        Ok(Self { path, armed: true })
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ArchivePreviewStagingGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

pub(crate) fn install_archive_preview_convert_source(
    app: &mut AppState,
    path: PathBuf,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) -> Result<ArchivePreviewStarted, ArchivePreviewStartError> {
    install_archive_preview_convert_source_with_password_resolver(
        app,
        path,
        tx,
        archive_preview_password_for_path,
    )
}

fn install_archive_preview_convert_source_with_password_resolver<F>(
    app: &mut AppState,
    path: PathBuf,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    resolve_password: F,
) -> Result<ArchivePreviewStarted, ArchivePreviewStartError>
where
    F: FnOnce(&mut AppState, &Path) -> Result<Option<String>, String>,
{
    // Complete every fallible prerequisite before touching the currently
    // installed source. This makes activation failure-atomic even when secret
    // storage, runtime ownership, channel ownership, or staging allocation is
    // unavailable.
    if !crate::convert::source_admission::is_archive_preview_source_path(&path) {
        return Err(ArchivePreviewStartError::UnsupportedPath(path));
    }
    if tx.is_closed() {
        return Err(ArchivePreviewStartError::WorkerChannelClosed);
    }
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|error| ArchivePreviewStartError::RuntimeUnavailable(error.to_string()))?;
    let generation = app
        .probe_generation
        .checked_add(1)
        .ok_or(ArchivePreviewStartError::GenerationExhausted)?;
    let password = resolve_password(app, &path)
        .map_err(ArchivePreviewStartError::PasswordResolution)?;
    let pending = create_pending_archive_preview(generation, path.clone());
    let staging_dir = pending.staging_dir.clone();
    let mut staging_guard = ArchivePreviewStagingGuard::create(staging_dir.clone())?;
    let cancel = pending.cancel.clone();
    let tool_paths = app.manager.config.tool_paths.clone();

    // Commit section: all operations below are in-memory/infallible. The
    // receiver cannot observe a worker result until the pending owner and
    // generation have been installed.
    app.cancel_browse_convert_expansion();
    app.probe_generation = generation;
    clear_source_metadata_in_convert(&mut app.convert);
    app.convert.set_source_mode(SourceMode::from_single_pending_probe(
        path.clone(),
        Some(ARCHIVE_PREVIEW_EXTRACTING_NOTICE.to_string()),
    ));
    app.convert.apply_source_defaults();
    let baseline = ConvertProbeBaseline::capture(&app.convert);
    app.current_screen = AppScreen::Convert;
    app.convert.install_pending_archive_preview(pending);

    let _ = spawn_archive_preview(
        &runtime,
        generation,
        path.clone(),
        baseline,
        staging_dir,
        cancel,
        password,
        tool_paths,
        tx,
    );
    staging_guard.disarm();

    // Recent-history persistence is deliberately post-dispatch: a source is
    // never recorded when preflight says that no operation started.
    app.recent.record_use_with_db(&path, &app.db);
    app.set_status(format!(
        "Extracting archive: {}",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));

    Ok(ArchivePreviewStarted {
        generation,
        archive_path: path,
    })
}

pub(crate) fn spawn_archive_preview(
    runtime: &tokio::runtime::Handle,
    generation: u64,
    archive_path: PathBuf,
    baseline: ConvertProbeBaseline,
    staging_dir: PathBuf,
    cancel: tokio_util::sync::CancellationToken,
    archive_password: Option<String>,
    tool_paths: std::collections::HashMap<String, PathBuf>,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) -> tokio::task::JoinHandle<()> {
    runtime.spawn(async move {
        let result_path = archive_path.clone();
        let runner = crate::convert::pipeline::tool::RealToolRunner::new(tool_paths);
        let item_id = format!("archive-preview-{}", generation);

        let result: Result<ArchivePreview, String> = async {
            if cancel.is_cancelled() {
                return Err("archive preview cancelled".to_string());
            }

            std::fs::create_dir_all(&staging_dir)
                .map_err(|err| format!("create preview staging failed: {err}"))?;

            if cancel.is_cancelled() {
                return Err("archive preview cancelled".to_string());
            }

            crate::convert::pipeline::materializer_archive::extract_archive_to_staging(
                &archive_path,
                &staging_dir,
                item_id.as_str(),
                archive_password.as_deref(),
                &runner,
                None,
                &cancel,
            )
            .await
            .map_err(|err| format!("{err}"))?;

            let audio_files = crate::convert::pipeline::materializer_archive::discover_archive_audio_files(&staging_dir)
                .map_err(|err| format!("audio discovery failed: {err}"))?;
            if audio_files.is_empty() {
                return Err("no audio files found in archive".to_string());
            }

            let track_count = audio_files.len();
            let _ = tx
                .send(crate::tui::message::AppMessage::ArchivePreviewProgress {
                    generation,
                    archive_path: archive_path.clone(),
                    message: format!(
                        "Probing {} track{}...",
                        track_count,
                        if track_count == 1 { "" } else { "s" }
                    ),
                })
                .await;

            let cancel_for_probe = cancel.clone();
            let tracks = tokio::task::spawn_blocking(move || {
                let mut tracks = Vec::with_capacity(audio_files.len());
                for path in audio_files {
                    if cancel_for_probe.is_cancelled() {
                        return Err("archive preview cancelled".to_string());
                    }
                    let info = crate::tui::probe::probe_audio(&path)
                        .map_err(|err| format!("probe failed for {}: {err}", path.display()))?;
                    let metadata = crate::tui::probe::read_metadata(&path).unwrap_or_default();
                    let original_name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| name.to_string())
                        .unwrap_or_else(|| path.display().to_string());
                    tracks.push(PreviewTrack {
                        path,
                        original_name,
                        info,
                        original_metadata: metadata.clone(),
                        metadata,
                    });
                }
                Ok::<Vec<PreviewTrack>, String>(tracks)
            })
            .await
            .map_err(|err| format!("archive preview task failed: {err}"))??;

            if cancel.is_cancelled() {
                return Err("archive preview cancelled".to_string());
            }

            let album_metadata = archive_preview_album_metadata(&tracks);
            Ok(ArchivePreview {
                staging_dir: staging_dir.clone(),
                archive_path,
                tracks,
                album_metadata,
            })
        }
        .await;

        if result.is_err() {
            let _ = std::fs::remove_dir_all(&staging_dir);
        }

        let _ = tx
            .send(crate::tui::message::AppMessage::ArchivePreviewResult {
                generation,
                archive_path: result_path,
                result,
                baseline,
            })
            .await;
    })
}

/// Spawn a Convert-owned cursor probe for a batch preview path. Generic browse
/// probes intentionally do not mutate Convert state; this message carries the
/// source generation and user-edit baseline needed to merge late results safely.
pub(crate) fn spawn_convert_batch_cursor_probe(
    generation: u64,
    path: PathBuf,
    baseline: ConvertProbeBaseline,
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
) {
    if is_nonprobeable_source_for_probe(&path) {
        return;
    }

    tokio::spawn(async move {
        let result_path = path.clone();
        let (info, metadata, probe_notice) = tokio::task::spawn_blocking(move || {
            probe_convert_source_for_message(&path)
        })
        .await
        .unwrap_or_else(|err| {
            (
                None,
                SourceMetadata::default(),
                Some(format!("Probe task failed: {}; set format manually", err)),
            )
        });

        let _ = tx
            .send(crate::tui::message::AppMessage::ConvertAudioProbeComplete {
                generation,
                path: result_path,
                info,
                metadata,
                probe_notice,
                baseline,
            })
            .await;
    });
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

/// Queue-time preview of an archive extraction. The staging directory is owned
/// by the Convert source until commit transfers ownership to the queue or a
/// source change removes it.
#[derive(Debug, Clone)]
pub struct ArchivePreview {
    pub staging_dir: PathBuf,
    pub archive_path: PathBuf,
    pub tracks: Vec<PreviewTrack>,
    pub album_metadata: SourceMetadata,
}

#[derive(Debug, Clone)]
pub struct PreviewTrack {
    pub path: PathBuf,
    pub original_name: String,
    pub info: SourceInfo,
    /// Metadata exactly as read during preview. Compact inline edits compare
    /// against this baseline so commit can carry only intentional overrides,
    /// including explicit clears, without masking later materializer tag reads.
    pub original_metadata: SourceMetadata,
    pub metadata: SourceMetadata,
}

/// Convert-owned handle for an archive preview that is still extracting or
/// probing. Unlike a completed `ArchivePreview`, this exists before the worker
/// can return a populated track list, so it is the only app-state owner that
/// can cancel extraction and remove the temporary staging directory on source
/// replacement, navigation away, or shutdown.
#[derive(Clone)]
pub struct PendingArchivePreview {
    pub generation: u64,
    pub archive_path: PathBuf,
    pub staging_dir: PathBuf,
    pub cancel: tokio_util::sync::CancellationToken,
}

impl fmt::Debug for PendingArchivePreview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingArchivePreview")
            .field("generation", &self.generation)
            .field("archive_path", &self.archive_path)
            .field("staging_dir", &self.staging_dir)
            .finish_non_exhaustive()
    }
}

impl PendingArchivePreview {
    pub fn matches(&self, generation: u64, archive_path: &Path) -> bool {
        self.generation == generation && self.archive_path.as_path() == archive_path
    }

    pub fn cancel_and_cleanup(self) {
        self.cancel.cancel();
        match std::fs::remove_dir_all(&self.staging_dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {}
        }
    }
}

/// Lifecycle handle for Browse Convert folder expansion. It exists so a
/// recursive filesystem walk can run on a blocking worker while the raw-mode
/// reducer retains an explicit generation/cancellation owner.
#[derive(Clone)]
pub struct PendingBrowseConvertExpansion {
    pub generation: u64,
    pub request: crate::tui::command::BrowseConvertExpansionRequest,
    pub cancel: tokio_util::sync::CancellationToken,
}

impl fmt::Debug for PendingBrowseConvertExpansion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingBrowseConvertExpansion")
            .field("generation", &self.generation)
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

impl PendingBrowseConvertExpansion {
    pub fn matches(
        &self,
        generation: u64,
        request: &crate::tui::command::BrowseConvertExpansionRequest,
    ) -> bool {
        self.generation == generation && &self.request == request
    }

    pub fn cancel(self) {
        self.cancel.cancel();
    }
}

/// Lifecycle handle for Browse-screen archive metadata editing while extraction
/// and tag reads are still running. The staging directory remains owned by this
/// handle until the editor opens or the worker reports failure/staleness.
#[derive(Clone)]
pub struct PendingBrowseArchiveMetadataEdit {
    pub archive_path: PathBuf,
    pub staging_dir: PathBuf,
    pub archive_mtime_secs: i64,
    pub archive_mtime_nanos: u32,
    pub archive_size: u64,
    pub target_inner_paths: Option<Vec<String>>,
    pub cancel: tokio_util::sync::CancellationToken,
    pub owns_staging: bool,
}

impl fmt::Debug for PendingBrowseArchiveMetadataEdit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingBrowseArchiveMetadataEdit")
            .field("archive_path", &self.archive_path)
            .field("staging_dir", &self.staging_dir)
            .field("archive_mtime_secs", &self.archive_mtime_secs)
            .field("archive_mtime_nanos", &self.archive_mtime_nanos)
            .field("archive_size", &self.archive_size)
            .field("target_inner_paths", &self.target_inner_paths)
            .field("owns_staging", &self.owns_staging)
            .finish_non_exhaustive()
    }
}

impl PendingBrowseArchiveMetadataEdit {
    pub fn new(
        archive_path: PathBuf,
        archive_mtime_secs: i64,
        archive_mtime_nanos: u32,
        archive_size: u64,
        target_inner_paths: Option<Vec<String>>,
    ) -> Self {
        Self {
            archive_path,
            staging_dir: std::env::temp_dir().join(format!(
                "tonepoet-archive-metadata-{}",
                uuid::Uuid::new_v4()
            )),
            archive_mtime_secs,
            archive_mtime_nanos,
            archive_size,
            target_inner_paths,
            cancel: tokio_util::sync::CancellationToken::new(),
            owns_staging: true,
        }
    }

    pub fn from_existing(
        archive_path: PathBuf,
        staging_dir: PathBuf,
        target_inner_paths: Option<Vec<String>>,
    ) -> Self {
        let (archive_mtime_secs, archive_mtime_nanos, archive_size) =
            archive_fingerprint(&archive_path).unwrap_or((0, 0, 0));
        Self {
            archive_path,
            staging_dir,
            archive_mtime_secs,
            archive_mtime_nanos,
            archive_size,
            target_inner_paths,
            cancel: tokio_util::sync::CancellationToken::new(),
            owns_staging: false,
        }
    }

    pub fn matches(&self, archive_path: &Path, staging_dir: &Path) -> bool {
        self.archive_path.as_path() == archive_path && self.staging_dir.as_path() == staging_dir
    }

    pub fn cancel_and_cleanup(self) {
        self.cancel.cancel();
        if self.owns_staging {
            cleanup_archive_metadata_staging_dir(&self.staging_dir);
        }
    }
}


/// Lifecycle handle for Browse-screen archive-entry rename. The operation
/// captures the archive fingerprint, extracts into staging, and performs the
/// filesystem rename inside staging. The deferred-save lifecycle owns the later
/// archive repackage/cleanup step after navigation, screen switch, quit, or an
/// explicit retry/overwrite action.
#[derive(Clone)]
pub struct PendingBrowseArchiveRename {
    pub archive_path: PathBuf,
    pub staging_dir: PathBuf,
    pub old_inner_path: String,
    pub new_inner_path: String,
    pub archive_mtime_secs: i64,
    pub archive_mtime_nanos: u32,
    pub archive_size: u64,
    pub target_inner_paths: Option<Vec<String>>,
    pub cancel: tokio_util::sync::CancellationToken,
}

impl fmt::Debug for PendingBrowseArchiveRename {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingBrowseArchiveRename")
            .field("archive_path", &self.archive_path)
            .field("staging_dir", &self.staging_dir)
            .field("old_inner_path", &self.old_inner_path)
            .field("new_inner_path", &self.new_inner_path)
            .field("archive_mtime_secs", &self.archive_mtime_secs)
            .field("archive_mtime_nanos", &self.archive_mtime_nanos)
            .field("archive_size", &self.archive_size)
            .finish_non_exhaustive()
    }
}

impl PendingBrowseArchiveRename {
    pub fn new(
        archive_path: PathBuf,
        old_inner_path: String,
        new_inner_path: String,
        archive_mtime_secs: i64,
        archive_mtime_nanos: u32,
        archive_size: u64,
        target_inner_paths: Option<Vec<String>>,
    ) -> Self {
        Self {
            archive_path,
            staging_dir: std::env::temp_dir().join(format!(
                "tonepoet-archive-rename-{}",
                uuid::Uuid::new_v4()
            )),
            old_inner_path,
            new_inner_path,
            archive_mtime_secs,
            archive_mtime_nanos,
            archive_size,
            target_inner_paths,
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    pub fn matches(&self, archive_path: &Path, staging_dir: &Path) -> bool {
        self.archive_path.as_path() == archive_path && self.staging_dir.as_path() == staging_dir
    }

    pub fn cancel_and_cleanup(self) {
        self.cancel.cancel();
        cleanup_archive_metadata_staging_dir(&self.staging_dir);
    }
}

/// Lifecycle handle for Browse-screen archive-entry delete. The operation
/// extracts the archive into persistent deferred-save staging when needed,
/// removes the selected staged member(s), and then leaves the archive dirty
/// for the normal save-on-exit path. Unlike the old per-edit archive mutation
/// model, this handle never repackages the archive itself.
#[derive(Clone)]
pub struct PendingBrowseArchiveDelete {
    pub archive_path: PathBuf,
    pub staging_dir: PathBuf,
    pub inner_paths: Vec<String>,
    pub archive_mtime_secs: i64,
    pub archive_mtime_nanos: u32,
    pub archive_size: u64,
    pub target_inner_paths: Option<Vec<String>>,
    pub cancel: tokio_util::sync::CancellationToken,
}

impl fmt::Debug for PendingBrowseArchiveDelete {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingBrowseArchiveDelete")
            .field("archive_path", &self.archive_path)
            .field("staging_dir", &self.staging_dir)
            .field("inner_paths", &self.inner_paths)
            .field("archive_mtime_secs", &self.archive_mtime_secs)
            .field("archive_mtime_nanos", &self.archive_mtime_nanos)
            .field("archive_size", &self.archive_size)
            .finish_non_exhaustive()
    }
}

impl PendingBrowseArchiveDelete {
    pub fn new(
        archive_path: PathBuf,
        inner_paths: Vec<String>,
        archive_mtime_secs: i64,
        archive_mtime_nanos: u32,
        archive_size: u64,
        target_inner_paths: Option<Vec<String>>,
    ) -> Self {
        Self {
            archive_path,
            staging_dir: std::env::temp_dir().join(format!(
                "tonepoet-archive-delete-{}",
                uuid::Uuid::new_v4()
            )),
            inner_paths,
            archive_mtime_secs,
            archive_mtime_nanos,
            archive_size,
            target_inner_paths,
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    pub fn matches(&self, archive_path: &Path, staging_dir: &Path) -> bool {
        self.archive_path.as_path() == archive_path && self.staging_dir.as_path() == staging_dir
    }

    pub fn cancel_and_cleanup(self) {
        self.cancel.cancel();
        cleanup_archive_metadata_staging_dir(&self.staging_dir);
    }
}

pub fn archive_fingerprint(path: &std::path::Path) -> Result<(i64, u32, u64), String> {
    let meta = std::fs::metadata(path)
        .map_err(|err| format!("stat archive for conflict detection failed: {err}"))?;
    let modified = meta
        .modified()
        .map_err(|err| format!("read archive mtime for conflict detection failed: {err}"))?;
    let duration = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "archive mtime predates UNIX epoch".to_string())?;
    Ok((duration.as_secs() as i64, duration.subsec_nanos(), meta.len()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveMetadataEditOwner {
    Browse,
    Convert,
}

/// Archive metadata editor ownership context. Browse-owned sessions must
/// repackage the archive after the staged audio files save successfully;
/// Convert-owned sessions only update the Convert preview model and queue-time
/// override payload, leaving the original archive untouched.
#[derive(Debug, Clone)]
pub struct ArchiveMetadataEditContext {
    pub owner: ArchiveMetadataEditOwner,
    pub archive_path: PathBuf,
    pub staging_dir: PathBuf,
    pub archive_mtime_secs: Option<i64>,
    pub archive_mtime_nanos: Option<u32>,
    pub archive_size: Option<u64>,
    /// Archive-member subset requested when the editor was opened for selected
    /// entries rather than for every audio file in the extracted archive.
    pub target_inner_paths: Option<Vec<String>>,
    /// True only when the metadata editor created this staging tree for its
    /// own transient Browse archive edit. When the editor opens against an
    /// already-active ArchiveBrowseState staging session, Browse owns the
    /// directory and the editor must never remove it on cancel/close.
    pub editor_owns_staging: bool,
}

impl ArchiveMetadataEditContext {
    pub fn browse(archive_path: PathBuf, staging_dir: PathBuf) -> Self {
        let (archive_mtime_secs, archive_mtime_nanos, archive_size) = archive_fingerprint(&archive_path)
            .map(|(secs, nanos, size)| (Some(secs), Some(nanos), Some(size)))
            .unwrap_or((None, None, None));
        Self {
            owner: ArchiveMetadataEditOwner::Browse,
            archive_path,
            staging_dir,
            archive_mtime_secs,
            archive_mtime_nanos,
            archive_size,
            target_inner_paths: None,
            editor_owns_staging: true,
        }
    }

    /// Construct a Browse archive edit context for metadata-editor-created
    /// staging, such as whole-archive editing launched from the archive file in
    /// the parent directory. Do not use this for an existing
    /// `ArchiveBrowseState::staging`; those sessions are owned by Browse and
    /// must use `browse_active_staging_with_fingerprint()` so cancellation does
    /// not install editor-owned retry/discard state.
    pub fn browse_with_fingerprint(
        archive_path: PathBuf,
        staging_dir: PathBuf,
        archive_mtime_secs: i64,
        archive_mtime_nanos: u32,
        archive_size: u64,
        target_inner_paths: Option<Vec<String>>,
    ) -> Self {
        Self {
            owner: ArchiveMetadataEditOwner::Browse,
            archive_path,
            staging_dir,
            archive_mtime_secs: Some(archive_mtime_secs),
            archive_mtime_nanos: Some(archive_mtime_nanos),
            archive_size: Some(archive_size),
            target_inner_paths,
            editor_owns_staging: true,
        }
    }

    pub fn browse_active_staging_with_fingerprint(
        archive_path: PathBuf,
        staging_dir: PathBuf,
        archive_mtime_secs: i64,
        archive_mtime_nanos: u32,
        archive_size: u64,
        target_inner_paths: Option<Vec<String>>,
    ) -> Self {
        Self {
            owner: ArchiveMetadataEditOwner::Browse,
            archive_path,
            staging_dir,
            archive_mtime_secs: Some(archive_mtime_secs),
            archive_mtime_nanos: Some(archive_mtime_nanos),
            archive_size: Some(archive_size),
            target_inner_paths,
            editor_owns_staging: false,
        }
    }

    pub fn convert(archive_path: PathBuf, staging_dir: PathBuf) -> Self {
        Self {
            owner: ArchiveMetadataEditOwner::Convert,
            archive_path,
            staging_dir,
            archive_mtime_secs: None,
            archive_mtime_nanos: None,
            archive_size: None,
            target_inner_paths: None,
            editor_owns_staging: true,
        }
    }

    pub fn archive_conflict(&self) -> Result<bool, String> {
        let Some(expected_secs) = self.archive_mtime_secs else { return Ok(false); };
        let Some(expected_nanos) = self.archive_mtime_nanos else { return Ok(false); };
        let Some(expected_size) = self.archive_size else { return Ok(false); };
        let (actual_secs, actual_nanos, actual_size) = archive_fingerprint(&self.archive_path)?;
        Ok(actual_secs != expected_secs || actual_nanos != expected_nanos || actual_size != expected_size)
    }

    pub fn cleanup_staging(&self) {
        cleanup_archive_metadata_staging_dir(&self.staging_dir);
    }
}

/// Payload returned by the Browse archive metadata worker after extraction,
/// audio discovery, and the single tag-read pass used to seed the editor.
#[derive(Debug, Clone)]
pub struct ArchiveMetadataEditorPayload {
    pub paths: Vec<PathBuf>,
    pub entries: Vec<crate::tui::probe::TagEntry>,
    pub metadata: Vec<crate::tui::probe::SourceMetadata>,
    pub metadata_errors: Vec<Option<crate::tui::probe::MetadataReadIssue>>,
}

pub fn try_cleanup_archive_metadata_staging_dir(staging_dir: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(staging_dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!(
            "failed to remove archive staging directory {}: {err}",
            staging_dir.display()
        )),
    }
}

pub fn cleanup_archive_metadata_staging_dir(staging_dir: &Path) {
    if let Err(err) = try_cleanup_archive_metadata_staging_dir(staging_dir) {
        log::warn!("{err}");
    }
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
        /// Queue-time archive preview extraction, if this multi-track source came
        /// from a generic archive rather than a CUE/SACD/DVD/Blu-ray model.
        archive_preview: Option<ArchivePreview>,
        /// Full parsed disc model for selected disc-stream sources.
        disc_contents: Option<Box<crate::disc::DiscContents>>,
        /// Selected presentation id to bridge UI stream selection into pipeline source options.
        selected_presentation_id: Option<crate::disc::PresentationId>,
    },
    /// A multi-file batch loaded for review. The cursor indexes into
    /// `paths` for the "currently previewed" file, whose probe result
    /// lives in `cursor_info` / `cursor_metadata` (lazily filled in by
    /// `ConvertAudioProbeComplete` when the cursor moves).
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
    /// - `paths.len() == 1` → `Single` with info/metadata empty and an
    ///   explicit pending-probe notice when the source is probeable. This
    ///   keeps the state model from conflating an in-flight probe with a
    ///   deliberately unprobed/nonprobeable source.
    /// - `paths.len() > 1` → `Batch` with precomputed summary
    pub fn from_paths(paths: Vec<PathBuf>) -> Self {
        match paths.len() {
            0 => Self::Empty,
            1 => {
                let path = paths
                    .into_iter()
                    .next()
                    .expect("len == 1 means one element");
                let probe_notice = source_probe_initial_notice(&path);
                Self::from_single_pending_probe(path, probe_notice)
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
            Self::Single { info, .. } => info.as_ref(),
            Self::MultiTrack { info, archive_preview, cursor, .. } => archive_preview
                .as_ref()
                .and_then(|preview| preview.tracks.get(*cursor).map(|track| &track.info))
                .or(info.as_ref()),
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
            Self::Single { info, .. } => info.as_ref().map_or(0, |i| i.file_size),
            Self::MultiTrack { info, archive_preview, .. } => archive_preview
                .as_ref()
                .map(|preview| preview.tracks.iter().map(|track| track.info.file_size).sum())
                .unwrap_or_else(|| info.as_ref().map_or(0, |i| i.file_size)),
            Self::Batch { total_size, .. } => *total_size,
        }
    }

    /// The currently previewed `SourceMetadata`. Returns an owned default
    /// for the Empty variant so the caller can always have something to
    /// display without extra matching.
    pub fn current_metadata(&self) -> SourceMetadata {
        match self {
            Self::Empty => SourceMetadata::default(),
            Self::Single { metadata, .. } => metadata.clone(),
            Self::MultiTrack { metadata, archive_preview, cursor, .. } => archive_preview
                .as_ref()
                .and_then(|preview| preview.tracks.get(*cursor).map(|track| track.metadata.clone()))
                .unwrap_or_else(|| metadata.clone()),
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

    /// True when the source is explicitly waiting on the background probe
    /// worker. `info == None` by itself is not a pending-probe signal: it may
    /// also mean the source is nonprobeable or probing has failed.
    pub fn probe_in_progress(&self) -> bool {
        self.persistent_probe_notice().is_some_and(|notice| {
            notice == PROBE_IN_PROGRESS_NOTICE
                || notice == ARCHIVE_PREVIEW_EXTRACTING_NOTICE
                || notice.starts_with("Probing ")
        })
    }

    /// Number of files (0/1/N for Empty/Single/Batch; 1 for MultiTrack).
    pub fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Single { .. } | Self::MultiTrack { .. } => 1,
            Self::Batch { paths, .. } => paths.len(),
        }
    }

    pub fn archive_preview(&self) -> Option<&ArchivePreview> {
        match self {
            Self::MultiTrack { archive_preview: Some(preview), .. } => Some(preview),
            _ => None,
        }
    }

    pub fn archive_preview_staging_dir(&self) -> Option<&PathBuf> {
        self.archive_preview().map(|preview| &preview.staging_dir)
    }

    pub fn disarm_archive_preview_cleanup(&mut self) -> Option<PathBuf> {
        match self {
            Self::MultiTrack { archive_preview, .. } => archive_preview
                .take()
                .map(|preview| preview.staging_dir),
            _ => None,
        }
    }

    pub fn cleanup_archive_preview_staging(&mut self) {
        if let Some(staging_dir) = self.disarm_archive_preview_cleanup() {
            match std::fs::remove_dir_all(&staging_dir) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {}
            }
        }
    }

    pub fn sync_archive_preview_cursor_state(&self) -> Option<(SourceInfo, SourceMetadata)> {
        match self {
            Self::MultiTrack { archive_preview: Some(preview), cursor, .. } => {
                preview
                    .tracks
                    .get(*cursor)
                    .map(|track| (track.info.clone(), track.metadata.clone()))
            }
            _ => None,
        }
    }

    /// Cheap placeholder for a single path whose expensive source discovery is
    /// still running on a blocking worker. This constructor must remain free of
    /// FFmpeg, Lofty, CUE, SACD, DVD, or Blu-ray work so event-loop callers can
    /// publish a responsive Convert source immediately.
    pub fn from_single_pending_probe(path: PathBuf, probe_notice: Option<String>) -> Self {
        Self::Single {
            path,
            info: None,
            metadata: SourceMetadata::default(),
            probe_notice,
        }
    }

    /// Build a SourceMode for a single path. Detects SACD ISOs and CUE
    /// pairs and returns MultiTrack when a track listing is available. This is
    /// intentionally heavyweight; event-loop code should install
    /// `from_single_pending_probe` and run this on a blocking worker.
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
                        archive_preview: None,
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
                    archive_preview: None,
                    disc_contents: None,
                    selected_presentation_id: None,
                };
            }
        }

        Self::Single {
            path,
            info,
            metadata,
            probe_notice,
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
/// without a reasonable represented histogram bucket (wma/amr,
/// and other unsupported formats) return `None` and fall
/// through to "(no recognised audio extensions)" if the whole batch is
/// unrecognised.
fn detect_format_from_extension(path: &std::path::Path) -> Option<AudioFormat> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    crate::convert::classify::audio_format_from_extension(ext)
}

#[cfg(test)]
mod clamp_pill_tests {
    use super::FormatState;
    use crate::convert::formats::AudioFormat;

    #[test]
    fn dsd_rate_falling_back_to_pcm_lands_on_lowest_rate_not_maximum() {
        let mut format = FormatState::new();
        format.format.select_value(&AudioFormat::Dsf);
        format.apply_format_constraints();
        assert!(
            format.sample_rate.selected_value() >= &2_822_400,
            "DSF should clamp the rate up to a DSD rate"
        );
        format.format.select_value(&AudioFormat::Wav);
        format.apply_format_constraints();
        assert_eq!(
            format.sample_rate.selected_value(),
            &44_100,
            "returning to PCM must not arm a silent upsample to the max rate"
        );
    }

    fn format_round_trip(format: &mut FormatState, to: AudioFormat, source_rate: u32) {
        use super::{BitDepthChoice, FormatField};
        let before = *format.format.selected_value();
        format.format.select_value(&to);
        format.after_user_selection(
            FormatField::Format,
            before,
            BitDepthChoice::Int24,
            Some(24),
            Some(source_rate),
        );
    }

    #[test]
    fn dsd_fallback_restores_probed_pcm_source_rate() {
        let mut format = FormatState::new();
        format.sample_rate.select_value(&96_000);
        format_round_trip(&mut format, AudioFormat::Dsf, 96_000);
        assert!(
            *format.sample_rate.selected_value() >= 2_822_400,
            "DSF leg must land on a DSD rate for this test to be meaningful"
        );
        format_round_trip(&mut format, AudioFormat::Wav, 96_000);
        assert_eq!(
            format.sample_rate.selected_value(),
            &96_000,
            "a DSF round-trip on a 96 kHz source must return to the source rate"
        );
    }

    #[test]
    fn dsd_round_trip_preserves_deliberate_manual_downsample() {
        let mut format = FormatState::new();
        // 96 kHz source, but the user deliberately staged a 44.1 downsample.
        format.sample_rate.select_value(&44_100);
        format_round_trip(&mut format, AudioFormat::Dsf, 96_000);
        format_round_trip(&mut format, AudioFormat::Wav, 96_000);
        assert_eq!(
            format.sample_rate.selected_value(),
            &44_100,
            "the round-trip must restore the user's staged rate, not the source rate"
        );
    }

    #[test]
    fn constraint_fallback_never_auto_selects_source_pills() {
        use super::SOURCE_SAMPLE_RATE_SENTINEL;
        let mut format = FormatState::new();
        // PCM -> DSF disables every PCM rate. The always-enabled source
        // sentinel sits at index 0; the clamp must skip it and land on a
        // real DSD rate.
        format.format.select_value(&AudioFormat::Dsf);
        format.apply_format_constraints();
        assert_ne!(
            *format.sample_rate.selected_value(),
            SOURCE_SAMPLE_RATE_SENTINEL,
            "constraint fallback silently rebound the rate to same-as-source"
        );
        // DSD -> PCM likewise lands on a real rate, not the sentinel.
        format.format.select_value(&AudioFormat::Wav);
        format.apply_format_constraints();
        assert_ne!(
            *format.sample_rate.selected_value(),
            SOURCE_SAMPLE_RATE_SENTINEL
        );
        // A DELIBERATE source selection survives a DSD target only when the
        // probed source is itself DSD. PCM -> DSD requires an explicit rate.
        format.source_is_dsd = true;
        format.sample_rate.select_value(&SOURCE_SAMPLE_RATE_SENTINEL);
        format.format.select_value(&AudioFormat::Dsf);
        format.apply_format_constraints();
        assert_eq!(
            *format.sample_rate.selected_value(),
            SOURCE_SAMPLE_RATE_SENTINEL,
            "an explicit source-rate choice must not be clamped away"
        );
    }

    #[test]
    fn source_relative_rate_and_depth_survive_probe_cascades() {
        use super::{BitDepthChoice, SOURCE_SAMPLE_RATE_SENTINEL};
        let mut format = FormatState::new();
        format.sample_rate.select_value(&SOURCE_SAMPLE_RATE_SENTINEL);
        format.bit_depth.select_value(&BitDepthChoice::Source);

        format.cascade_pcm_source_defaults(96_000, Some(24), false);
        assert_eq!(*format.sample_rate.selected_value(), SOURCE_SAMPLE_RATE_SENTINEL);
        assert_eq!(*format.bit_depth.selected_value(), BitDepthChoice::Source);

        format.cascade_dsd_source_to_pcm_defaults(11_289_600);
        assert_eq!(*format.sample_rate.selected_value(), SOURCE_SAMPLE_RATE_SENTINEL);
        assert_eq!(*format.bit_depth.selected_value(), BitDepthChoice::Source);
        assert_eq!(*format.resampler.selected_value(), super::ResamplerChoice::None);
    }

    fn assert_source_policy_survives_unknown_source_reset(format: &mut FormatState) {
        use super::{
            BitDepthChoice, DitherType, DsdGainMode, ResamplerChoice,
            SOURCE_SAMPLE_RATE_SENTINEL,
        };
        format.sample_rate.select_value(&SOURCE_SAMPLE_RATE_SENTINEL);
        format.bit_depth.select_value(&BitDepthChoice::Source);
        format.dither.select_value(&DitherType::Shibata);
        format.resampler.select_value(&ResamplerChoice::Soxr);
        format.dither_overridden = true;
        format.resampler_overridden = true;
        // Stage dormant native-v2 policy directly. Pre-promotion production
        // constraints keep these controls disabled, but reset/recovery must not
        // corrupt values already present in a future-version preset.
        format.source_is_dsd = true;
        format.apply_format_constraints();
        format.dsd_gain_mode.set_all_enabled(true);
        assert!(
            format.dsd_gain_mode.select_value(&DsdGainMode::Fixed),
            "fixture must be able to stage dormant native-v2 gain"
        );
        format.dsd_gain_db = "5.500000000".parse().unwrap();

        format.clear_source_derived_defaults();

        assert_eq!(*format.sample_rate.selected_value(), SOURCE_SAMPLE_RATE_SENTINEL);
        assert_eq!(*format.bit_depth.selected_value(), BitDepthChoice::Source);
        assert_eq!(*format.dither.selected_value(), DitherType::Shibata);
        assert_eq!(*format.resampler.selected_value(), ResamplerChoice::Soxr);
        assert_eq!(*format.dsd_gain_mode.selected_value(), DsdGainMode::Fixed);
        assert_eq!(format.dsd_gain_db, "5.500000000".parse().unwrap());
        assert!(format.dither_overridden);
        assert!(format.resampler_overridden);
    }

    #[test]
    fn changing_bit_depth_resets_dither_explicitness_before_auto_selection() {
        use super::{BitDepthChoice, DitherType, FormatField};

        let mut format = FormatState::new();
        format.bit_depth.select_value(&BitDepthChoice::Int24);
        format.dither.select_value(&DitherType::TPDF);
        format.dither_overridden = true;
        let before_format = format.format.selected_value().clone();
        let before_depth = *format.bit_depth.selected_value();
        format.bit_depth.select_value(&BitDepthChoice::Int32);

        format.after_user_selection(
            FormatField::BitDepth,
            before_format,
            before_depth,
            Some(32),
            Some(96_000),
        );

        assert!(!format.dither_overridden);
        assert_eq!(*format.dither.selected_value(), DitherType::None);
    }

    #[test]
    fn failed_probe_preserves_deliberate_source_policy_and_explicit_overrides() {
        let mut format = FormatState::new();
        assert_source_policy_survives_unknown_source_reset(&mut format);
    }

    #[test]
    fn failed_probe_clears_only_automatic_source_derived_decisions() {
        use super::{DitherType, DsdGainMode, ResamplerChoice};
        let mut format = FormatState::new();
        format.dither.select_value(&DitherType::Shibata);
        format.resampler.select_value(&ResamplerChoice::Soxr);
        format.dsd_gain_mode.select_value(&DsdGainMode::NormalizePeak);
        format.dsd_gain_db = "6.000000000".parse().unwrap();
        assert!(!format.dither_overridden);
        assert!(!format.resampler_overridden);

        format.clear_source_derived_defaults();

        assert_eq!(*format.dither.selected_value(), DitherType::None);
        assert_eq!(*format.resampler.selected_value(), ResamplerChoice::None);
        assert_eq!(*format.dsd_gain_mode.selected_value(), DsdGainMode::Disabled);
        assert_eq!(format.dsd_gain_db, tonepoet_pipeline::DbNano::ZERO);
        assert!(!format.dither_overridden);
        assert!(!format.resampler_overridden);
    }

    #[test]
    fn constrained_source_defaults_remain_automatic_and_clear_when_source_disappears() {
        use crate::convert::formats::AudioFormat;
        use super::BitDepthChoice;
        let mut format = FormatState::new();
        format.format.select_value(&AudioFormat::Alac);

        format.cascade_pcm_source_defaults(768_000, Some(32), false);
        format.apply_format_constraints();

        assert_eq!(*format.sample_rate.selected_value(), 384_000);
        assert_eq!(*format.bit_depth.selected_value(), BitDepthChoice::Int24);
        assert_eq!(format.source_derived_sample_rate, Some(384_000));
        assert_eq!(format.source_derived_bit_depth, Some(BitDepthChoice::Int24));

        format.clear_source_derived_defaults();

        assert_eq!(*format.sample_rate.selected_value(), 44_100);
        assert_eq!(*format.bit_depth.selected_value(), BitDepthChoice::Int16);
        assert_eq!(format.source_derived_sample_rate, None);
        assert_eq!(format.source_derived_bit_depth, None);
    }

    #[test]
    fn unresolved_pending_probe_preserves_explicit_scalar_overrides() {
        use super::{BitDepthChoice, DitherType, ResamplerChoice};
        let mut format = FormatState::new();
        format.sample_rate.select_value(&96_000);
        format.mark_sample_rate_user_policy();
        format.bit_depth.select_value(&BitDepthChoice::Int24);
        format.mark_bit_depth_user_policy();
        format.dither.select_value(&DitherType::Shibata);
        format.resampler.select_value(&ResamplerChoice::Soxr);
        format.dither_overridden = true;
        format.resampler_overridden = true;

        format.clear_source_derived_defaults();

        assert_eq!(*format.sample_rate.selected_value(), 96_000);
        assert_eq!(*format.bit_depth.selected_value(), BitDepthChoice::Int24);
        assert_eq!(*format.dither.selected_value(), DitherType::Shibata);
        assert_eq!(*format.resampler.selected_value(), ResamplerChoice::Soxr);
        assert!(format.dither_overridden);
        assert!(format.resampler_overridden);
    }

    #[test]
    fn valid_probe_preserves_explicit_scalar_rate_and_depth_overrides() {
        use super::BitDepthChoice;
        let mut format = FormatState::new();
        format.sample_rate.select_value(&96_000);
        format.mark_sample_rate_user_policy();
        format.bit_depth.select_value(&BitDepthChoice::Int24);
        format.mark_bit_depth_user_policy();

        format.cascade_pcm_source_defaults(192_000, Some(32), false);

        assert_eq!(*format.sample_rate.selected_value(), 96_000);
        assert_eq!(*format.bit_depth.selected_value(), BitDepthChoice::Int24);
        assert!(format.sample_rate_overridden);
        assert!(format.bit_depth_overridden);
        assert_eq!(format.source_derived_sample_rate, None);
        assert_eq!(format.source_derived_bit_depth, None);
    }

    #[test]
    fn mouse_click_on_disabled_rate_pill_preserves_automatic_provenance() {
        let mut format = FormatState::new();
        format.cascade_pcm_source_defaults(96_000, Some(24), false);
        assert_eq!(*format.sample_rate.selected_value(), 96_000);
        assert!(!format.sample_rate_overridden);
        assert_eq!(format.source_derived_sample_rate, Some(96_000));

        let disabled = format
            .sample_rate
            .options
            .iter()
            .position(|option| option.value == 192_000)
            .expect("192 kHz pill must exist");
        format.sample_rate.options[disabled].enabled = false;

        assert!(!crate::tui::format_interactions::handle_format_button(
            &mut format,
            crate::tui::button_map::TuiButton::RatePill(disabled),
            Some(24),
            Some(96_000),
        ));

        assert_eq!(*format.sample_rate.selected_value(), 96_000);
        assert!(!format.sample_rate_overridden);
        assert_eq!(format.source_derived_sample_rate, Some(96_000));
    }

    #[test]
    fn mouse_click_on_disabled_depth_pill_preserves_automatic_provenance() {
        use super::BitDepthChoice;

        let mut format = FormatState::new();
        format.cascade_pcm_source_defaults(96_000, Some(24), false);
        assert_eq!(*format.bit_depth.selected_value(), BitDepthChoice::Int24);
        assert!(!format.bit_depth_overridden);
        assert_eq!(
            format.source_derived_bit_depth,
            Some(BitDepthChoice::Int24)
        );

        let disabled = format
            .bit_depth
            .options
            .iter()
            .position(|option| option.value == BitDepthChoice::Int32)
            .expect("32-bit pill must exist");
        format.bit_depth.options[disabled].enabled = false;

        assert!(!crate::tui::format_interactions::handle_format_button(
            &mut format,
            crate::tui::button_map::TuiButton::DepthPill(disabled),
            Some(24),
            Some(96_000),
        ));

        assert_eq!(*format.bit_depth.selected_value(), BitDepthChoice::Int24);
        assert!(!format.bit_depth_overridden);
        assert_eq!(
            format.source_derived_bit_depth,
            Some(BitDepthChoice::Int24)
        );
    }

    #[test]
    fn mouse_invalid_pill_index_does_not_create_rate_policy() {
        let mut format = FormatState::new();
        format.cascade_pcm_source_defaults(96_000, Some(24), false);
        let invalid = format.sample_rate.options.len();

        assert!(!crate::tui::format_interactions::handle_format_button(
            &mut format,
            crate::tui::button_map::TuiButton::RatePill(invalid),
            Some(24),
            Some(96_000),
        ));

        assert_eq!(*format.sample_rate.selected_value(), 96_000);
        assert!(!format.sample_rate_overridden);
        assert_eq!(format.source_derived_sample_rate, Some(96_000));
    }

    #[test]
    fn mixed_or_unavailable_source_facts_preserve_source_policy() {
        let mut format = FormatState::new();
        assert_source_policy_survives_unknown_source_reset(&mut format);
        format.cascade_pcm_source_defaults(192_000, Some(32), false);
        assert_eq!(*format.sample_rate.selected_value(), super::SOURCE_SAMPLE_RATE_SENTINEL);
        assert_eq!(*format.bit_depth.selected_value(), super::BitDepthChoice::Source);
    }

    #[test]
    fn source_removal_reset_preserves_source_policy_for_later_batch() {
        let mut format = FormatState::new();
        assert_source_policy_survives_unknown_source_reset(&mut format);
        format.cascade_pcm_source_defaults(44_100, Some(16), false);
        assert_eq!(*format.sample_rate.selected_value(), super::SOURCE_SAMPLE_RATE_SENTINEL);
        assert_eq!(*format.bit_depth.selected_value(), super::BitDepthChoice::Source);
    }

    #[test]
    fn dsd_target_with_unknown_source_keeps_source_rate_selected_but_unavailable() {
        use crate::convert::formats::AudioFormat;
        use super::{BitDepthChoice, SOURCE_SAMPLE_RATE_SENTINEL};
        let mut format = FormatState::new();
        format.source_is_dsd = true;
        format.format.select_value(&AudioFormat::Dsf);
        format.sample_rate.select_value(&SOURCE_SAMPLE_RATE_SENTINEL);
        format.bit_depth.select_value(&BitDepthChoice::Source);
        format.apply_format_constraints();
        assert!(format.sample_rate.options.iter().any(|option| {
            option.value == SOURCE_SAMPLE_RATE_SENTINEL && option.enabled
        }));

        format.clear_source_derived_defaults();

        assert_eq!(*format.sample_rate.selected_value(), SOURCE_SAMPLE_RATE_SENTINEL);
        assert_eq!(*format.bit_depth.selected_value(), BitDepthChoice::Source);
        assert!(format.sample_rate.options.iter().any(|option| {
            option.value == SOURCE_SAMPLE_RATE_SENTINEL && !option.enabled
        }));
    }

    #[test]
    fn pcm_source_disables_source_rate_for_dsd_target() {
        use super::SOURCE_SAMPLE_RATE_SENTINEL;
        let mut format = FormatState::new();
        // A PROBED PCM source (the production setter records identity as
        // Known); a raw `source_is_dsd = false` would model the unstaged
        // fresh state, where the sentinel deliberately stays available.
        format.set_source_is_dsd(false);
        format.sample_rate.select_value(&SOURCE_SAMPLE_RATE_SENTINEL);
        format.format.select_value(&AudioFormat::Dsf);
        format.apply_format_constraints();

        assert_ne!(*format.sample_rate.selected_value(), SOURCE_SAMPLE_RATE_SENTINEL);
        assert!(
            format
                .sample_rate
                .options
                .iter()
                .find(|option| option.value == SOURCE_SAMPLE_RATE_SENTINEL)
                .is_some_and(|option| !option.enabled)
        );
    }

    #[test]
    fn pcm_rate_cap_still_degrades_to_nearest_lower_rate() {
        let mut format = FormatState::new();
        format.sample_rate.select_value(&96_000);
        format.format.select_value(&AudioFormat::Mp3);
        format.apply_format_constraints();
        assert_eq!(
            format.sample_rate.selected_value(),
            &48_000,
            "MP3 cap should land on 48 kHz, not wrap to 44.1"
        );
    }
}

#[cfg(test)]
mod batch_format_extension_tests {
    use super::{compute_format_histogram, detect_format_from_extension};
    use crate::convert::formats::AudioFormat;
    use std::path::{Path, PathBuf};

    #[test]
    fn cheap_extension_detector_recognizes_dsf_dff_ogg_and_tta() {
        assert_eq!(
            detect_format_from_extension(Path::new("Album.DSF")),
            Some(AudioFormat::Dsf)
        );
        assert_eq!(
            detect_format_from_extension(Path::new("Album.DFF")),
            Some(AudioFormat::Dff)
        );
        assert_eq!(
            detect_format_from_extension(Path::new("Album.OGG")),
            Some(AudioFormat::Ogg)
        );
        assert_eq!(
            detect_format_from_extension(Path::new("Album.TTA")),
            Some(AudioFormat::Tta)
        );
    }

    #[test]
    fn cheap_batch_histogram_counts_dsd_sources() {
        let paths = vec![
            PathBuf::from("a.dsf"),
            PathBuf::from("b.dff"),
            PathBuf::from("c.dff"),
            PathBuf::from("d.ogg"),
            PathBuf::from("e.tta"),
            PathBuf::from("cover.jpg"),
        ];

        let histogram = compute_format_histogram(&paths);

        assert!(histogram.contains(&(AudioFormat::Dsf, 1)));
        assert!(histogram.contains(&(AudioFormat::Dff, 2)));
        assert!(histogram.contains(&(AudioFormat::Ogg, 1)));
        assert!(histogram.contains(&(AudioFormat::Tta, 1)));
    }
}

/// State for the source pane.
#[derive(Debug)]
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
    /// Synthetic CUE queue inputs staged for a merged split-CUE album while the
    /// Convert screen is in review. Commit transfers these artifacts to the
    /// conversion manager; replacing or clearing the source removes them.
    pub synthetic_cue_artifacts: std::collections::HashSet<PathBuf>,
}

impl SourceState {
    pub fn cleanup_synthetic_cue_artifacts(&mut self) {
        let artifacts = std::mem::take(&mut self.synthetic_cue_artifacts);
        crate::convert::queue_expansion::cleanup_synthetic_cue_artifacts(&artifacts);
    }

    /// Relinquish source-side cleanup ownership without deleting the files.
    ///
    /// This is intentionally narrow: it is used only when queue admission has
    /// already succeeded and a later ownership-inspection step fails. At that
    /// point returning artifacts to `SourceState` would create duplicate
    /// ownership: ordinary source replacement/drop cleanup could delete files
    /// that queued conversion items now reference. Dropping source ownership is
    /// therefore safer than deleting or retaining ambiguous ownership locally;
    /// any true orphan is left for the manager/scavenger rather than risking a
    /// live queue input.
    pub fn release_synthetic_cue_artifacts_without_cleanup(
        &mut self,
    ) -> std::collections::HashSet<PathBuf> {
        std::mem::take(&mut self.synthetic_cue_artifacts)
    }

    pub fn cleanup_synthetic_cue_artifacts_not_in(&mut self, retained_paths: &[PathBuf]) {
        let mut removed = std::collections::HashSet::new();
        self.synthetic_cue_artifacts.retain(|path| {
            let keep = crate::convert::queue_expansion::path_list_contains_queue_identity(retained_paths, path);
            if !keep {
                removed.insert(path.clone());
            }
            keep
        });
        crate::convert::queue_expansion::cleanup_synthetic_cue_artifacts(&removed);
    }
}

impl Clone for SourceState {
    fn clone(&self) -> Self {
        // Synthetic CUE artifacts are owned resources, not copyable metadata.
        // Cloned app/test snapshots must not acquire duplicate cleanup
        // responsibility for source-owned temporary files.
        Self {
            mode: self.mode.clone(),
            advanced_open: self.advanced_open,
            batch_probe_pending: self.batch_probe_pending.clone(),
            batch_probe_debounce: self.batch_probe_debounce.clone(),
            cue_artifact_audio: self.cue_artifact_audio.clone(),
            synthetic_cue_artifacts: std::collections::HashSet::new(),
        }
    }
}

impl Drop for SourceState {
    fn drop(&mut self) {
        self.cleanup_synthetic_cue_artifacts();
    }
}

impl Default for SourceState {
    fn default() -> Self {
        Self {
            mode: SourceMode::Empty,
            advanced_open: false,
            batch_probe_pending: None,
            batch_probe_debounce: None,
            cue_artifact_audio: std::collections::HashSet::new(),
            synthetic_cue_artifacts: std::collections::HashSet::new(),
        }
    }
}

#[cfg(test)]
mod source_state_synthetic_artifact_ownership_tests {
    use super::SourceState;
    use std::fs;

    #[test]
    fn release_synthetic_artifacts_without_cleanup_disarms_source_drop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("album.cue");
        fs::write(&artifact, b"FILE \"a.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n")
            .expect("write synthetic cue");

        let released = {
            let mut source = SourceState::default();
            source.synthetic_cue_artifacts.insert(artifact.clone());
            let released = source.release_synthetic_cue_artifacts_without_cleanup();
            assert!(released.contains(&artifact));
            assert!(source.synthetic_cue_artifacts.is_empty());
            released
        };

        assert!(released.contains(&artifact));
        assert!(
            artifact.exists(),
            "released artifacts must survive SourceState drop/replacement cleanup"
        );
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
    /// Reference or reserved Manual DSD-source pathway.
    DsdPath,
    /// Standard or explicit Wideband Reference reconstruction profile.
    DsdProfile,
    DsdGain,
    /// Fixed DSD-to-PCM gain value, edited with left/right controls.
    DsdGainDb,
    /// Native NormalizePeak target or exact legacy Auto safety margin.
    DsdNormalizeTarget,
}

impl FormatField {
    /// Rows visible in the format pane. Legacy gain remains visible for every
    /// DSD-to-PCM conversion; native Reference-only rows are independently
    /// promotion-gated.
    pub fn visible_rows(
        is_dsd_target: bool,
        show_dsd_to_pcm_gain: bool,
        show_reference_controls: bool,
    ) -> &'static [Self] {
        if is_dsd_target {
            &[
                Self::Format,
                Self::DsdRate,
                Self::BitDepth,
                Self::NoiseShaper,
                Self::ModulatorOrder,
                Self::ConversionPreset,
            ]
        } else if show_reference_controls {
            &[
                Self::Format, Self::SampleRate, Self::BitDepth, Self::Resampler,
                Self::Dither, Self::ReplayGain, Self::DsdPath, Self::DsdProfile,
                Self::DsdGain, Self::DsdGainDb, Self::DsdNormalizeTarget,
            ]
        } else if show_dsd_to_pcm_gain {
            &[
                Self::Format, Self::SampleRate, Self::BitDepth, Self::Resampler,
                Self::Dither, Self::ReplayGain, Self::DsdGain, Self::DsdGainDb,
                Self::DsdNormalizeTarget,
            ]
        } else {
            &[
                Self::Format, Self::SampleRate, Self::BitDepth, Self::Resampler,
                Self::Dither, Self::ReplayGain,
            ]
        }
    }

    pub fn next_for(
        self,
        is_dsd_target: bool,
        show_dsd_to_pcm_gain: bool,
        show_reference_controls: bool,
    ) -> Self {
        let rows = Self::visible_rows(
            is_dsd_target, show_dsd_to_pcm_gain, show_reference_controls,
        );
        let idx = rows.iter().position(|row| *row == self).unwrap_or(0);
        rows[(idx + 1) % rows.len()]
    }

    pub fn prev_for(
        self,
        is_dsd_target: bool,
        show_dsd_to_pcm_gain: bool,
        show_reference_controls: bool,
    ) -> Self {
        let rows = Self::visible_rows(
            is_dsd_target, show_dsd_to_pcm_gain, show_reference_controls,
        );
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
    CompanionExtensions,
    CompanionFolders,
    ExcludeFiles,
    ForceEncode,
    DiscSubfolders,
    WriteLog,
    Actions,
}

impl OutputOptionsField {
    const COLLAPSED_FIELDS: [Self; 4] = [
        Self::DestPath,
        Self::FolderTemplate,
        Self::FilenameTemplate,
        Self::MergeMode,
    ];
    const MAXIMIZED_FIELDS: [Self; 11] = [
        Self::DestPath,
        Self::FolderTemplate,
        Self::FilenameTemplate,
        Self::MergeMode,
        Self::CompanionExtensions,
        Self::CompanionFolders,
        Self::ExcludeFiles,
        Self::ForceEncode,
        Self::DiscSubfolders,
        Self::WriteLog,
        Self::Actions,
    ];
    const MAXIMIZED_FIELDS_WITHOUT_ACTIONS: [Self; 10] = [
        Self::DestPath,
        Self::FolderTemplate,
        Self::FilenameTemplate,
        Self::MergeMode,
        Self::CompanionExtensions,
        Self::CompanionFolders,
        Self::ExcludeFiles,
        Self::ForceEncode,
        Self::DiscSubfolders,
        Self::WriteLog,
    ];
    const MAXIMIZED_FIELDS_WITHOUT_CONVERSION_OR_ACTIONS: [Self; 7] = [
        Self::DestPath,
        Self::FolderTemplate,
        Self::FilenameTemplate,
        Self::MergeMode,
        Self::CompanionExtensions,
        Self::CompanionFolders,
        Self::ExcludeFiles,
    ];
    const MAXIMIZED_FIELDS_WITHOUT_EXCLUDE_CONVERSION_OR_ACTIONS: [Self; 6] = [
        Self::DestPath,
        Self::FolderTemplate,
        Self::FilenameTemplate,
        Self::MergeMode,
        Self::CompanionExtensions,
        Self::CompanionFolders,
    ];

    fn visible_fields(maximized: bool) -> &'static [Self] {
        if maximized {
            &Self::MAXIMIZED_FIELDS
        } else {
            &Self::COLLAPSED_FIELDS
        }
    }

    /// Fields whose rows are actually rendered for a maximized Output Options
    /// pane of the given height. Keep these thresholds in sync with
    /// draw_output_options.rs; the Actions row exists only at height >= 20.
    pub fn visible_fields_for_area(
        maximized: bool,
        area_height: u16,
        show_actions: bool,
    ) -> &'static [Self] {
        if !maximized {
            return &Self::COLLAPSED_FIELDS;
        }
        if area_height >= 20 && show_actions {
            &Self::MAXIMIZED_FIELDS
        } else if area_height >= 17 {
            &Self::MAXIMIZED_FIELDS_WITHOUT_ACTIONS
        } else if area_height >= 12 {
            &Self::MAXIMIZED_FIELDS_WITHOUT_CONVERSION_OR_ACTIONS
        } else if area_height >= 11 {
            &Self::MAXIMIZED_FIELDS_WITHOUT_EXCLUDE_CONVERSION_OR_ACTIONS
        } else {
            &Self::COLLAPSED_FIELDS
        }
    }

    pub fn next(&self) -> Self {
        (*self).next_for(true)
    }

    pub fn prev(&self) -> Self {
        (*self).prev_for(true)
    }

    pub fn next_for(self, maximized: bool) -> Self {
        let fields = Self::visible_fields(maximized);
        let idx = fields.iter().position(|field| *field == self).unwrap_or(0);
        fields[(idx + 1) % fields.len()]
    }

    pub fn prev_for(self, maximized: bool) -> Self {
        let fields = Self::visible_fields(maximized);
        let idx = fields.iter().position(|field| *field == self).unwrap_or(0);
        fields[(idx + fields.len() - 1) % fields.len()]
    }

    pub fn next_for_area(self, maximized: bool, area_height: u16, show_actions: bool) -> Self {
        let fields = Self::visible_fields_for_area(maximized, area_height, show_actions);
        let idx = fields.iter().position(|field| *field == self).unwrap_or(0);
        fields[(idx + 1) % fields.len()]
    }

    pub fn prev_for_area(self, maximized: bool, area_height: u16, show_actions: bool) -> Self {
        let fields = Self::visible_fields_for_area(maximized, area_height, show_actions);
        let idx = fields.iter().position(|field| *field == self).unwrap_or(0);
        fields[(idx + fields.len() - 1) % fields.len()]
    }

    pub fn clamp_for(self, maximized: bool) -> Self {
        if Self::visible_fields(maximized).contains(&self) {
            self
        } else {
            Self::MergeMode
        }
    }

    pub fn clamp_for_area(self, maximized: bool, area_height: u16, show_actions: bool) -> Self {
        if Self::visible_fields_for_area(maximized, area_height, show_actions).contains(&self) {
            self
        } else {
            Self::MergeMode
        }
    }

    pub fn is_text_field(self) -> bool {
        matches!(
            self,
            Self::DestPath
                | Self::FolderTemplate
                | Self::FilenameTemplate
                | Self::CompanionExtensions
                | Self::CompanionFolders
                | Self::ExcludeFiles
        )
    }

    pub fn completion_mode(self) -> crate::tui::text_input::CompletionMode<'static> {
        match self {
            Self::DestPath => crate::tui::text_input::CompletionMode::Path,
            Self::FolderTemplate | Self::FilenameTemplate => {
                crate::tui::text_input::CompletionMode::TemplateVariable
            }
            // Companion folders are logical source-relative names, not an
            // editable absolute/relative filesystem path from the current CWD.
            // Leave completion off rather than suggesting misleading paths.
            Self::CompanionFolders | Self::CompanionExtensions | Self::ExcludeFiles => {
                crate::tui::text_input::CompletionMode::None
            }
            _ => crate::tui::text_input::CompletionMode::None,
        }
    }
}

/// Sentinel used only by the sample-rate pill to represent the pipeline's
/// typed `RateTarget::Source` choice. Zero is not a valid audio sample rate.
pub const SOURCE_SAMPLE_RATE_SENTINEL: u32 = 0;

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
    /// Native-v2 DSD-source pathway. Manual is reserved and rejected in P0.
    pub dsd_pathway: PillState<DsdSourcePathway>,
    /// Native-v2 Reference reconstruction profile.
    pub dsd_profile: PillState<DsdReconstructionSelection>,
    pub dsd_gain_mode: PillState<DsdGainMode>,
    /// Fixed DSD-to-PCM gain in dB used when `dsd_gain_mode` is Manual.
    pub dsd_gain_db: DbNano,
    /// NormalizePeak target in dBFS. Stored as fixed-point text authority.
    pub dsd_normalize_target_dbfs: DbNano,
    /// Exact legacy Auto-mode safety margin in dB.
    pub dsd_auto_gain_margin_db: DbNano,
    /// Whether the currently previewed source is DSD. Drives visibility and
    /// activation of DSD-to-PCM gain controls so they never appear for PCM sources.
    pub source_is_dsd: bool,
    /// Probe-established DSD source sample rate used to disable impossible
    /// profile choices without replacing planner-grade validation.
    pub source_dsd_rate_hz: Option<u32>,
    /// See [`SourceRateIdentity`]: fresh/known/lost source identity, driving
    /// the same-as-source rate pill's availability and clamp retention.
    pub source_rate_identity: SourceRateIdentity,
    pub field_focus: FormatField,
    pub advanced_open: bool,
    /// False until the user or a preset explicitly picks a dither algorithm.
    /// Automatic source/bit-depth decisions may update the row only while false.
    pub dither_overridden: bool,
    /// False until the user or a preset explicitly picks a resampler.
    /// Automatic source/rate decisions may update the row only while false.
    pub resampler_overridden: bool,
    /// True after an explicit keyboard, mouse, command, or preset rate choice.
    pub sample_rate_overridden: bool,
    /// True after an explicit keyboard, mouse, command, or preset depth choice.
    pub bit_depth_overridden: bool,
    /// Concrete rate most recently installed by a source-default cascade.
    /// `None` means the selected rate is user/preset policy or the Source sentinel.
    pub(crate) source_derived_sample_rate: Option<u32>,
    /// Concrete depth most recently installed by a source-default cascade.
    /// `None` means the selected depth is user/preset policy or Source.
    pub(crate) source_derived_bit_depth: Option<BitDepthChoice>,
    /// PCM rate selected before the user switched to a DSD format, so a
    /// DSD round-trip restores the exact prior selection (deliberate
    /// downsamples included) instead of guessing from the source rate.
    pub pcm_rate_before_dsd: Option<u32>,
    /// Selected container index into `AudioFormat::available_containers()`.
    /// 0 = codec default. Reset to 0 when the format pill changes.
    pub selected_container_index: usize,
    /// Native DSD admission requires an exact user-confirmed codec/container identity.
    /// Legacy v2/v3 presets preserve their visible selection but clear this bit until
    /// the user explicitly re-selects a format or container.
    pub reference_target_confirmed: bool,
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
            (SOURCE_SAMPLE_RATE_SENTINEL, "source"),
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
            (BitDepthChoice::Source, "source"),
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
            (ReplayGainChoice::BothIfMissing, "both if missing"),
            (ReplayGainChoice::Album, "album"),
            (ReplayGainChoice::AlbumIfMissing, "album if missing"),
            (ReplayGainChoice::Track, "track"),
            (ReplayGainChoice::TrackIfMissing, "track if missing"),
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
            (DsdGainMode::Disabled, "disabled"),
            (DsdGainMode::Auto, "auto"),
            (DsdGainMode::Reference, "reference"),
            (DsdGainMode::NativeLevel, "native"),
            (DsdGainMode::Fixed, "manual"),
            (DsdGainMode::NormalizePeak, "normalize"),
        ]);
        let mut dsd_pathway = PillState::new(vec![
            (DsdSourcePathway::Reference, "reference"),
            (DsdSourcePathway::Manual, "manual (not yet available)"),
        ]);
        dsd_pathway.set_enabled(&DsdSourcePathway::Manual, false);
        let mut dsd_profile = PillState::new(vec![
            (DsdReconstructionSelection::Reference, "reference"),
            (DsdReconstructionSelection::Wideband, "wideband"),
        ]);
        dsd_profile.set_enabled(&DsdReconstructionSelection::Wideband, false);

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
            dsd_pathway,
            dsd_profile,
            dsd_gain_mode,
            dsd_gain_db: DbNano(0),
            dsd_normalize_target_dbfs: DbNano::DEFAULT_NORMALIZE_TARGET,
            dsd_auto_gain_margin_db: DbNano(150_000_000),
            source_is_dsd: false,
            source_dsd_rate_hz: None,
            source_rate_identity: SourceRateIdentity::Unstaged,
            field_focus: FormatField::Format,
            advanced_open: false,
            dither_overridden: false,
            resampler_overridden: false,
            sample_rate_overridden: false,
            bit_depth_overridden: false,
            source_derived_sample_rate: None,
            source_derived_bit_depth: None,
            pcm_rate_before_dsd: None,
            selected_container_index: 0,
            reference_target_confirmed: true,
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
        // Keep historical defaults while presenting source-coupled choices first.
        state.sample_rate.select_value(&44_100);
        state.bit_depth.select_value(&BitDepthChoice::Int16);
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

    /// True only for the conversion direction where native DSD-source controls
    /// could have meaning: DSD source material rendered to a PCM target format.
    pub fn dsd_to_pcm_gain_available(&self) -> bool {
        self.source_is_dsd && !self.is_dsd_selected()
    }

    /// Whether the native-v2 DSD Reference controls are exposed in this release.
    ///
    /// The pre-promotion default is the exact legacy wire, so merely opening a
    /// DSD source must not opt the TUI into an unqualified native policy, require
    /// Reference confirmation, or lock the generic legacy resampler/dither rows.
    /// The promotion release flips `DsdSettings::default()` to native v2 and this
    /// gate opens automatically.
    pub fn dsd_reference_controls_available(&self) -> bool {
        self.dsd_to_pcm_gain_available()
            && tonepoet_pipeline::DsdSettings::default().is_native_v2()
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
            && *self.sample_rate.selected_value() != SOURCE_SAMPLE_RATE_SENTINEL
            && !self.bit_depth.selected_value().is_source()
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

    /// Record a COMPLETED probe's DSD/PCM identity. Only real probe facts may
    /// promote to Known — guesses and emptiness go through
    /// [`Self::set_pending_source_hint`] instead.
    pub fn set_source_is_dsd(&mut self, source_is_dsd: bool) {
        self.source_is_dsd = source_is_dsd;
        if !source_is_dsd {
            self.source_dsd_rate_hz = None;
        }
        self.source_rate_identity = SourceRateIdentity::Known;
        self.apply_format_constraints();
    }

    /// Record a DSD/PCM HINT for a source whose identity is not probe-proven
    /// (Empty mode, or a pending-probe placeholder where only the file
    /// extension is known). The hint drives row visibility, but the identity
    /// is deliberately NOT promoted to Known. The guarantee this buys is
    /// RETENTION: a deliberate same-as-source selection survives (disabled
    /// under Lost) until a completed probe revalidates it, instead of being
    /// clamped away by a guess. Note the Unstaged branch is narrow in
    /// practice — staging paths run apply_source_defaults immediately after,
    /// whose info-less arm degrades to Lost — so mid-probe preset staging of
    /// a DSD source-rate still refuses until the probe completes; Unstaged
    /// permissiveness holds only on a screen that never staged a source.
    pub fn set_pending_source_hint(&mut self, source_is_dsd_hint: bool) {
        self.source_is_dsd = source_is_dsd_hint;
        if self.source_rate_identity != SourceRateIdentity::Unstaged {
            self.source_rate_identity = SourceRateIdentity::Lost;
        }
        self.apply_format_constraints();
    }

    pub fn focus_next(&mut self) {
        self.field_focus = self
            .field_focus
            .next_for(
                self.is_dsd_selected(),
                self.dsd_to_pcm_gain_available(),
                self.dsd_reference_controls_available(),
            );
    }

    pub fn focus_prev(&mut self) {
        self.field_focus = self
            .field_focus
            .prev_for(
                self.is_dsd_selected(),
                self.dsd_to_pcm_gain_available(),
                self.dsd_reference_controls_available(),
            );
    }

    pub fn mark_dither_overridden(&mut self) {
        self.dither_overridden = true;
    }

    pub(crate) fn mark_sample_rate_user_policy(&mut self) {
        self.sample_rate_overridden = true;
        self.source_derived_sample_rate = None;
    }

    pub(crate) fn mark_bit_depth_user_policy(&mut self) {
        self.bit_depth_overridden = true;
        self.source_derived_bit_depth = None;
    }

    pub fn select_bit_depth(&mut self, bit_depth: BitDepthChoice, source_bits: Option<u32>) {
        self.mark_bit_depth_user_policy();
        self.bit_depth.select_value(&bit_depth);
        self.apply_auto_dither(source_bits);
        self.apply_format_constraints();
    }

    pub fn is_lossy_codec_selected(&self) -> bool {
        matches!(
            *self.format.selected_value(),
            AudioFormat::Mp3 | AudioFormat::Aac | AudioFormat::Opus
        )
    }

    pub fn lossy_preset_labels(&self) -> Option<Vec<String>> {
        match *self.format.selected_value() {
            AudioFormat::Mp3 => Some(vec![
                "V0 (245kbps)".to_string(),
                "V2 (190kbps)".to_string(),
                "320 CBR".to_string(),
                "custom".to_string(),
            ]),
            AudioFormat::Aac => Some(vec![
                "256 VBR".to_string(),
                "192 VBR".to_string(),
                "128 VBR".to_string(),
                "custom".to_string(),
            ]),
            AudioFormat::Opus => Some(vec![
                "128".to_string(),
                "96".to_string(),
                "64".to_string(),
                "custom".to_string(),
            ]),
            _ => None,
        }
    }

    pub fn lossy_preset_index(&self) -> Option<usize> {
        use tonepoet_pipeline::enums::Mp3Mode;
        match *self.format.selected_value() {
            AudioFormat::Mp3 => {
                if self.mp3_mode == Mp3Mode::Vbr && self.mp3_vbr_quality == 0 {
                    Some(0)
                } else if self.mp3_mode == Mp3Mode::Vbr && self.mp3_vbr_quality == 2 {
                    Some(1)
                } else if self.mp3_mode == Mp3Mode::Cbr && self.mp3_bitrate_kbps == 320 {
                    Some(2)
                } else {
                    Some(3)
                }
            }
            AudioFormat::Aac => match self.aac_bitrate_kbps {
                256 if self.aac_profile == tonepoet_pipeline::enums::AacProfile::LcAac => Some(0),
                192 if self.aac_profile == tonepoet_pipeline::enums::AacProfile::LcAac => Some(1),
                128 if self.aac_profile == tonepoet_pipeline::enums::AacProfile::LcAac => Some(2),
                _ => Some(3),
            },
            AudioFormat::Opus => match self.opus_bitrate_kbps {
                128 => Some(0),
                96 => Some(1),
                64 => Some(2),
                _ => Some(3),
            },
            _ => None,
        }
    }

    pub fn select_lossy_preset_index(&mut self, index: usize) -> bool {
        use tonepoet_pipeline::enums::{AacProfile, Mp3Mode};
        match *self.format.selected_value() {
            AudioFormat::Mp3 => match index.min(3) {
                0 => {
                    self.mp3_mode = Mp3Mode::Vbr;
                    self.mp3_vbr_quality = 0;
                    self.mp3_bitrate_kbps = 245;
                    self.mp3_quality_preset = None;
                    true
                }
                1 => {
                    self.mp3_mode = Mp3Mode::Vbr;
                    self.mp3_vbr_quality = 2;
                    self.mp3_bitrate_kbps = 190;
                    self.mp3_quality_preset = None;
                    true
                }
                2 => {
                    self.mp3_mode = Mp3Mode::Cbr;
                    self.mp3_bitrate_kbps = 320;
                    self.mp3_quality_preset = Some(0);
                    true
                }
                _ => false,
            },
            AudioFormat::Aac => match index.min(3) {
                0 => {
                    self.aac_profile = AacProfile::LcAac;
                    self.aac_bitrate_kbps = 256;
                    self.aac_quality_preset = Some(1);
                    true
                }
                1 => {
                    self.aac_profile = AacProfile::LcAac;
                    self.aac_bitrate_kbps = 192;
                    self.aac_quality_preset = Some(2);
                    true
                }
                2 => {
                    self.aac_profile = AacProfile::LcAac;
                    self.aac_bitrate_kbps = 128;
                    self.aac_quality_preset = Some(3);
                    true
                }
                _ => false,
            },
            AudioFormat::Opus => match index.min(3) {
                0 => {
                    self.opus_bitrate_kbps = 128;
                    self.opus_quality_preset = Some(2);
                    true
                }
                1 => {
                    self.opus_bitrate_kbps = 96;
                    self.opus_quality_preset = Some(3);
                    true
                }
                2 => {
                    self.opus_bitrate_kbps = 64;
                    self.opus_quality_preset = Some(4);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    pub fn select_lossy_preset_next(&mut self) -> bool {
        let Some(labels) = self.lossy_preset_labels() else {
            return false;
        };
        let current = self.lossy_preset_index().unwrap_or(labels.len() - 1);
        self.select_lossy_preset_index((current + 1) % labels.len())
    }

    pub fn select_lossy_preset_prev(&mut self) -> bool {
        let Some(labels) = self.lossy_preset_labels() else {
            return false;
        };
        let current = self.lossy_preset_index().unwrap_or(labels.len() - 1);
        self.select_lossy_preset_index((current + labels.len() - 1) % labels.len())
    }

    /// Select the next enabled pill in the focused row and run row-specific side effects.
    /// Key and mouse handlers should use this instead of calling `focused_pill_mut()` directly.
    pub fn select_focused_next(&mut self, source_bits: Option<u32>, source_rate: Option<u32>) {
        if self.field_focus == FormatField::BitDepth && self.is_lossy_codec_selected() {
            self.select_lossy_preset_next();
            self.apply_format_constraints();
            return;
        }
        let before_depth = *self.bit_depth.selected_value();
        let before_format = *self.format.selected_value();
        let focused = self.field_focus;
        self.focused_pill_mut().select_next();
        self.after_user_selection(focused, before_format, before_depth, source_bits, source_rate);
    }

    /// Select the previous enabled pill in the focused row and run row-specific side effects.
    pub fn select_focused_prev(&mut self, source_bits: Option<u32>, source_rate: Option<u32>) {
        if self.field_focus == FormatField::BitDepth && self.is_lossy_codec_selected() {
            self.select_lossy_preset_prev();
            self.apply_format_constraints();
            return;
        }
        let before_depth = *self.bit_depth.selected_value();
        let before_format = *self.format.selected_value();
        let focused = self.field_focus;
        self.focused_pill_mut().select_prev();
        self.after_user_selection(focused, before_format, before_depth, source_bits, source_rate);
    }

    /// Select a concrete pill index for mouse handlers and run row-specific side effects.
    ///
    /// A disabled or out-of-range pill is a rejected interaction, not a user
    /// policy decision.  In particular, it must not clear source-derived
    /// provenance for the currently selected rate/depth.  Re-clicking an
    /// enabled, already-selected pill is accepted and therefore may make that
    /// value explicit.
    pub fn select_row_index(
        &mut self,
        row: FormatField,
        index: usize,
        source_bits: Option<u32>,
        source_rate: Option<u32>,
    ) -> bool {
        let before_depth = *self.bit_depth.selected_value();
        let before_format = *self.format.selected_value();
        self.field_focus = row;
        let accepted = match row {
            FormatField::Format => select_enabled_index(&mut self.format, index),
            FormatField::SampleRate | FormatField::DsdRate => {
                select_enabled_index(&mut self.sample_rate, index)
            }
            FormatField::BitDepth if self.is_lossy_codec_selected() => {
                self.select_lossy_preset_index(index)
            }
            FormatField::BitDepth => select_enabled_index(&mut self.bit_depth, index),
            FormatField::Resampler => select_enabled_index(&mut self.resampler, index),
            FormatField::Dither => select_enabled_index(&mut self.dither, index),
            FormatField::ReplayGain => select_enabled_index(&mut self.replaygain, index),
            FormatField::NoiseShaper => select_enabled_index(&mut self.noise_shaper, index),
            FormatField::ModulatorOrder => select_enabled_index(&mut self.modulator_order, index),
            FormatField::ConversionPreset => {
                select_enabled_index(&mut self.conversion_preset, index)
            }
            FormatField::DsdPath => select_enabled_index(&mut self.dsd_pathway, index),
            FormatField::DsdProfile => select_enabled_index(&mut self.dsd_profile, index),
            FormatField::DsdGain => select_enabled_index(&mut self.dsd_gain_mode, index),
            FormatField::DsdGainDb => {
                // Clicking/focusing the value row makes Fixed explicit;
                // keyboard left/right then adjusts the staged dB value.
                self.dsd_gain_mode.select_value(&DsdGainMode::Fixed);
                self.dsd_gain_db = clamp_dsd_to_pcm_gain_db(self.dsd_gain_db);
                true
            }
            FormatField::DsdNormalizeTarget => {
                if self.dsd_reference_controls_available() {
                    if !self.dsd_gain_mode.select_value(&DsdGainMode::NormalizePeak) {
                        return false;
                    }
                    self.dsd_normalize_target_dbfs =
                        clamp_dsd_normalize_target_dbfs(self.dsd_normalize_target_dbfs);
                } else {
                    if !self.dsd_gain_mode.select_value(&DsdGainMode::Auto) {
                        return false;
                    }
                    self.dsd_auto_gain_margin_db =
                        clamp_dsd_auto_gain_margin_db(self.dsd_auto_gain_margin_db);
                }
                true
            }
        };
        if accepted {
            self.after_user_selection(row, before_format, before_depth, source_bits, source_rate);
        }
        accepted
    }

    pub(crate) fn after_user_selection(
        &mut self,
        row: FormatField,
        before_format: AudioFormat,
        before_depth: BitDepthChoice,
        source_bits: Option<u32>,
        source_rate: Option<u32>,
    ) {
        if matches!(row, FormatField::SampleRate | FormatField::DsdRate) {
            self.mark_sample_rate_user_policy();
        }
        if row == FormatField::BitDepth {
            self.mark_bit_depth_user_policy();
        }
        if row == FormatField::Dither {
            self.mark_dither_overridden();
        }
        if row == FormatField::Resampler {
            self.resampler_overridden = true;
        }

        if row == FormatField::Format && before_format != *self.format.selected_value() {
            self.selected_container_index = 0;
            self.reference_target_confirmed = true;
            self.resampler_overridden = false;
            let rate_before = *self.sample_rate.selected_value();
            let rate_before_was_dsd = rate_before != SOURCE_SAMPLE_RATE_SENTINEL
                && tonepoet_pipeline::DsdRate::from_hz(rate_before).is_some();
            self.apply_format_constraints();
            if self.is_dsd_selected() {
                if !rate_before_was_dsd && rate_before != SOURCE_SAMPLE_RATE_SENTINEL {
                    self.pcm_rate_before_dsd = Some(rate_before);
                }
                self.dither.select_value(&DitherType::None);
                self.cascade_dsd_rate_defaults();
            } else {
                // A DSD-rate selection just fell back to the lowest PCM rate
                // (clamp_sample_rate_pill). Restore the PCM rate the user had
                // before entering DSD (preserving deliberate downsamples),
                // falling back to the probed source rate — a DSF round-trip
                // on a 96 kHz source must not arm a silent 96 -> 44.1
                // downsample. DSD-source rates refuse here (not PCM options)
                // and are handled by cascade_dsd_source_to_pcm_defaults.
                if rate_before_was_dsd {
                    if let Some(rate) = self.pcm_rate_before_dsd.take().or(source_rate) {
                        self.sample_rate.select_value(&rate);
                    }
                }
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

        let target = *self.bit_depth.selected_value();
        if target.is_source() {
            // Source-coupled depth does not prove a reduction at the TUI layer.
            // The planner resolves the actual depth and owns any required
            // conversion-specific dither decision.
            self.dither.select_value(&DitherType::None);
            return;
        }

        // DSD and PCM are incommensurable encoding schemes — the conversion
        // is a reconstruction, not a truncation. Always dither at the PCM
        // output stage: TPDF for ≥24-bit, Shibata for ≤16-bit.
        if source_bits == 1 {
            let desired = if target.bits() <= 16 {
                DitherType::Shibata
            } else {
                DitherType::TPDF
            };
            self.dither.select_value(&desired);
            return;
        }

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
        if target_rate == SOURCE_SAMPLE_RATE_SENTINEL
            || source_rate == Some(target_rate)
            || source_rate.is_none()
        {
            // Same rate or unknown source → no resampling needed
            self.resampler.select_value(&ResamplerChoice::None);
        } else {
            // Rate change → default to soxr
            self.resampler.select_value(&ResamplerChoice::Soxr);
        }
    }

    /// Clear only decisions that were derived from source facts when the newly
    /// installed source has no reliable probe information.
    ///
    /// Sample-rate and bit-depth selections are output policy. In particular,
    /// `source` is a deliberate sentinel that must survive failed, pending,
    /// mixed, removed, or otherwise unavailable source probes. Explicit scalar
    /// overrides must survive for the same reason. For a DSD target whose source
    /// identity is temporarily unknown, the selected `source` rate remains
    /// selected but disabled by `apply_format_constraints`; that is an honest
    /// unavailable state, not permission to silently substitute DSD64.
    ///
    /// Dither, resampler, and DSD gain are reset only when their current value
    /// is automatic. Explicit dither/resampler overrides and Manual DSD gain are
    /// output policy and therefore survive the same source-fact gap.
    pub fn clear_source_derived_defaults(&mut self) {
        self.source_is_dsd = false;
        self.source_dsd_rate_hz = None;
        self.source_rate_identity = SourceRateIdentity::Lost;

        let selected_rate = *self.sample_rate.selected_value();
        if self.source_derived_sample_rate == Some(selected_rate) {
            let fallback_rate = if self.is_dsd_selected() { 2_822_400 } else { 44_100 };
            self.sample_rate.select_value(&fallback_rate);
        }
        self.source_derived_sample_rate = None;

        let selected_depth = *self.bit_depth.selected_value();
        if self.source_derived_bit_depth == Some(selected_depth) {
            self.bit_depth.select_value(&BitDepthChoice::Int16);
        }
        self.source_derived_bit_depth = None;

        if !self.dither_overridden {
            self.dither.select_value(&DitherType::None);
        }
        if !self.resampler_overridden {
            self.resampler.select_value(&ResamplerChoice::None);
        }
        if *self.dsd_gain_mode.selected_value() != DsdGainMode::Fixed {
            let default_mode = if self.dsd_reference_controls_available() {
                DsdGainMode::Reference
            } else {
                DsdGainMode::Disabled
            };
            self.dsd_gain_mode.select_value(&default_mode);
            self.dsd_gain_db = DbNano(0);
        }

        // Constraints update option availability, but clamp_sample_rate_pill
        // deliberately retains the source sentinel even while disabled. This
        // keeps the user's policy visible until a later probe can validate it.
        self.apply_format_constraints();
        self.apply_auto_dither(None);
        self.apply_auto_resampler(None);
    }

    /// Set PCM output defaults to match a PCM source. Called when a source is
    /// first probed or when the output format is PCM and source info becomes
    /// available. Selects source-derived defaults only for rows the user has
    /// not explicitly overridden.
    pub fn cascade_pcm_source_defaults(
        &mut self,
        source_sample_rate: u32,
        source_bit_depth: Option<u32>,
        source_is_float: bool,
    ) {
        if self.is_dsd_selected() {
            return;
        }
        // A deliberate Source selection or explicit scalar selection is output
        // policy, not a stale default. Preserve it across probe cascades.
        if *self.sample_rate.selected_value() == SOURCE_SAMPLE_RATE_SENTINEL
            || self.sample_rate_overridden
        {
            self.source_derived_sample_rate = None;
        } else if let Some(idx) = self
            .sample_rate
            .options
            .iter()
            .position(|option| option.value == source_sample_rate)
        {
            // Install even when the option is currently disabled: every
            // production caller follows with apply_format_constraints, whose
            // clamp moves an out-of-range automatic default to the nearest
            // allowed scalar (768k -> 384k under ALAC) and whose provenance
            // sync keeps the clamped value automatic.
            self.sample_rate.selected = idx;
            self.source_derived_sample_rate = Some(source_sample_rate);
        } else {
            self.source_derived_sample_rate = None;
        }

        if self.bit_depth.selected_value().is_source() || self.bit_depth_overridden {
            self.source_derived_bit_depth = None;
        } else if let Some(bits) = source_bit_depth {
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
            if let Some(idx) = self
                .bit_depth
                .options
                .iter()
                .position(|option| option.value == depth)
            {
                self.bit_depth.selected = idx;
            }
            self.source_derived_bit_depth = Some(depth);
        } else {
            self.source_derived_bit_depth = None;
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
        self.source_dsd_rate_hz = Some(source_sample_rate);
        let preserve_source_rate =
            *self.sample_rate.selected_value() == SOURCE_SAMPLE_RATE_SENTINEL
                || self.sample_rate_overridden;
        if !preserve_source_rate {
            let target_hz = dsd_rate.default_pcm_target_hz();
            self.sample_rate.select_value(&target_hz);
            self.source_derived_sample_rate = Some(target_hz);
        } else {
            self.source_derived_sample_rate = None;
        }
        if !self.bit_depth.selected_value().is_source() && !self.bit_depth_overridden {
            self.bit_depth.select_value(&BitDepthChoice::Int24);
            self.source_derived_bit_depth = Some(BitDepthChoice::Int24);
        } else {
            self.source_derived_bit_depth = None;
        }
        if !self.resampler_overridden {
            self.resampler.select_value(if preserve_source_rate {
                &ResamplerChoice::None
            } else {
                &ResamplerChoice::Sox
            });
        }
    }

    /// Set noise shaper and modulator order to the recommended defaults for the
    /// current DSD rate. Called when the user switches to a DSD format or changes
    /// the DSD rate pill — not during constraint reapplication, so preset values
    /// and manual overrides are preserved.
    fn cascade_dsd_rate_defaults(&mut self) {
        let selected_rate = *self.sample_rate.selected_value();
        if selected_rate == SOURCE_SAMPLE_RATE_SENTINEL {
            return;
        }
        if let Some(dsd_rate) = tonepoet_pipeline::DsdRate::from_hz(selected_rate) {
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
        self.dsd_pathway.set_all_enabled(false);
        self.dsd_pathway
            .set_enabled(&DsdSourcePathway::Reference, self.dsd_reference_controls_available());
        self.dsd_profile.set_all_enabled(false);
        self.dsd_profile.set_enabled(
            &DsdReconstructionSelection::Reference,
            self.dsd_reference_controls_available(),
        );
        let target_rate_hz = *self.sample_rate.selected_value();
        let wideband_available = self.dsd_reference_controls_available()
            && self.source_dsd_rate_hz == Some(5_644_800)
            && target_rate_hz >= 176_400
            && target_rate_hz != SOURCE_SAMPLE_RATE_SENTINEL;
        self.dsd_profile
            .set_enabled(&DsdReconstructionSelection::Wideband, wideband_available);
        // Pre-promotion exposes the exact legacy Disabled/Auto/Manual family.
        // Promotion switches this same row to the native Reference family; the
        // two settings origins are never mixed.
        let gain_available = self.dsd_to_pcm_gain_available();
        let reference_available = self.dsd_reference_controls_available();
        self.dsd_gain_mode.set_all_enabled(false);
        self.dsd_gain_mode
            .set_enabled(&DsdGainMode::Disabled, gain_available && !reference_available);
        self.dsd_gain_mode
            .set_enabled(&DsdGainMode::Auto, gain_available && !reference_available);
        self.dsd_gain_mode
            .set_enabled(&DsdGainMode::Reference, reference_available);
        self.dsd_gain_mode
            .set_enabled(&DsdGainMode::NativeLevel, reference_available);
        self.dsd_gain_mode
            .set_enabled(&DsdGainMode::Fixed, gain_available);
        self.dsd_gain_mode
            .set_enabled(&DsdGainMode::NormalizePeak, reference_available);
        for option in &mut self.dsd_gain_mode.options {
            option.label = match option.value {
                DsdGainMode::Disabled => "disabled",
                DsdGainMode::Auto => "auto",
                DsdGainMode::Reference => "reference",
                DsdGainMode::NativeLevel => "native",
                DsdGainMode::Fixed if reference_available => "fixed",
                DsdGainMode::Fixed => "manual",
                DsdGainMode::NormalizePeak => "normalize",
            }
            .to_string();
        }

        // DSD rate threshold: rates at or above this are DSD, below are PCM.
        const DSD_RATE_MIN: u32 = 2_822_400;
        let is_dsd = is_dsd_format(fmt);

        for opt in &mut self.sample_rate.options {
            opt.enabled = if opt.value == SOURCE_SAMPLE_RATE_SENTINEL {
                // Valid for every PCM target. For DSD targets it needs a DSD
                // source — but an UNSTAGED state (no probe yet: fresh screen,
                // preset staging) stays permissive so source-relative presets
                // are not coupled to a loaded source. Only a KNOWN PCM source
                // or LOST facts disable it (Lost keeps a deliberate selection
                // visible via clamp retention).
                !is_dsd
                    || self.source_is_dsd
                    || self.source_rate_identity == SourceRateIdentity::Unstaged
            } else if is_dsd {
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
            if self.dsd_reference_controls_available() {
                // Reference owns these effects. Preserve the generic selections
                // but make them inactive so they cannot be mistaken for policy.
                self.resampler.set_all_enabled(false);
                self.dither.set_all_enabled(false);
            }
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
                    if opt.value != SOURCE_SAMPLE_RATE_SENTINEL && opt.value > 192_000 {
                        opt.enabled = false;
                    }
                }
            }
            AudioFormat::Mp3 | AudioFormat::Ogg => {
                self.bit_depth.set_all_enabled(false);
                self.dither.set_all_enabled(false);
                for opt in &mut self.sample_rate.options {
                    if opt.value != SOURCE_SAMPLE_RATE_SENTINEL && opt.value > 48_000 {
                        opt.enabled = false;
                    }
                }
            }
            AudioFormat::Flac | AudioFormat::Alac => {
                self.bit_depth.set_enabled(&BitDepthChoice::Float32, false);
                self.bit_depth.set_enabled(&BitDepthChoice::Float64, false);
                if fmt == AudioFormat::Alac {
                    self.bit_depth.set_enabled(&BitDepthChoice::Int32, false);
                }
                for opt in &mut self.sample_rate.options {
                    if opt.value != SOURCE_SAMPLE_RATE_SENTINEL && opt.value > 384_000 {
                        opt.enabled = false;
                    }
                }
            }
            AudioFormat::WavPack => {
                // The current conversion carrier integerizes float sources.
                // Disable both float targets until float WAV is preserved end-to-end.
                self.bit_depth.set_enabled(&BitDepthChoice::Float32, false);
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
            // Ape/Shorten/TTA are lossless but not encodable; same constraints as FLAC.
            AudioFormat::Ape | AudioFormat::Musepack | AudioFormat::Shorten | AudioFormat::Tta => {
                self.bit_depth.set_enabled(&BitDepthChoice::Float32, false);
                self.bit_depth.set_enabled(&BitDepthChoice::Float64, false);
                for opt in &mut self.sample_rate.options {
                    if opt.value != SOURCE_SAMPLE_RATE_SENTINEL && opt.value > 384_000 {
                        opt.enabled = false;
                    }
                }
            }
        }

        // Native-v2 Reference owns resampling and dither, but the pre-promotion
        // legacy DSD-to-PCM route still exposes the generic controls.  Do not
        // disable them merely because the source is DSD.

        self.clamp_disabled_selections();
        // Constraint clamping does not change provenance. When a source-derived
        // 768 kHz/32-bit default is constrained to 384 kHz/24-bit, the clamped
        // scalar is still automatic and must remain removable if source facts
        // later disappear.
        if self.source_derived_sample_rate.is_some() && !self.sample_rate_overridden {
            let selected = *self.sample_rate.selected_value();
            if selected != SOURCE_SAMPLE_RATE_SENTINEL {
                self.source_derived_sample_rate = Some(selected);
            }
        }
        if self.source_derived_bit_depth.is_some() && !self.bit_depth_overridden {
            let selected = *self.bit_depth.selected_value();
            if !selected.is_source() {
                self.source_derived_bit_depth = Some(selected);
            }
        }
        if !FormatField::visible_rows(
            self.is_dsd_selected(),
            self.dsd_to_pcm_gain_available(),
            self.dsd_reference_controls_available(),
        )
        .contains(&self.field_focus)
        {
            self.field_focus = FormatField::Format;
        }
    }

    fn clamp_disabled_selections(&mut self) {
        let retain_disabled_sentinel =
            self.source_rate_identity != SourceRateIdentity::Known;
        clamp_sample_rate_pill(&mut self.sample_rate, retain_disabled_sentinel);
        clamp_pill_excluding(&mut self.bit_depth, |option| option.value.is_source());
        clamp_pill(&mut self.resampler);
        clamp_pill(&mut self.dither);
        clamp_pill(&mut self.replaygain);
        clamp_pill(&mut self.noise_shaper);
        clamp_pill(&mut self.modulator_order);
        clamp_pill(&mut self.conversion_preset);
        clamp_pill(&mut self.dsd_pathway);
        clamp_pill(&mut self.dsd_profile);
        // Manual gain is an explicit output-policy override. Keep it selected
        // while source identity is temporarily unavailable; the disabled option
        // communicates that it cannot currently be applied without erasing it.
        clamp_pill_excluding(&mut self.dsd_gain_mode, |option| {
            option.value == DsdGainMode::Fixed
        });
        self.dsd_gain_db = clamp_dsd_to_pcm_gain_db(self.dsd_gain_db);
        self.dsd_normalize_target_dbfs =
            clamp_dsd_normalize_target_dbfs(self.dsd_normalize_target_dbfs);
        self.dsd_auto_gain_margin_db =
            clamp_dsd_auto_gain_margin_db(self.dsd_auto_gain_margin_db);
    }

    pub fn focused_pill_mut(&mut self) -> FocusedPill<'_> {
        let reference_available = self.dsd_reference_controls_available();
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
            FormatField::DsdPath => FocusedPill::DsdPath(&mut self.dsd_pathway),
            FormatField::DsdProfile => FocusedPill::DsdProfile(&mut self.dsd_profile),
            FormatField::DsdGain => FocusedPill::DsdGain(&mut self.dsd_gain_mode),
            FormatField::DsdGainDb => FocusedPill::DsdGainDb {
                gain_db: &mut self.dsd_gain_db,
                gain_mode: &mut self.dsd_gain_mode,
            },
            FormatField::DsdNormalizeTarget if reference_available => {
                FocusedPill::DsdNormalizeTarget {
                    target_dbfs: &mut self.dsd_normalize_target_dbfs,
                    gain_mode: &mut self.dsd_gain_mode,
                }
            }
            FormatField::DsdNormalizeTarget => FocusedPill::DsdAutoGainMargin {
                margin_db: &mut self.dsd_auto_gain_margin_db,
                gain_mode: &mut self.dsd_gain_mode,
            },
        }
    }
}

fn clamp_dsd_to_pcm_gain_db(value: DbNano) -> DbNano {
    DbNano(value.0.clamp(
        DSD_TO_PCM_GAIN_DB_MIN_NANO,
        DSD_TO_PCM_GAIN_DB_MAX_NANO,
    ))
}

fn step_dsd_to_pcm_gain_db(value: &mut DbNano, delta_nano: i64) {
    *value = clamp_dsd_to_pcm_gain_db(DbNano(value.0.saturating_add(delta_nano)));
}

fn clamp_dsd_normalize_target_dbfs(value: DbNano) -> DbNano {
    DbNano(value.0.clamp(
        DbNano::MIN_NORMALIZE_TARGET.0,
        DbNano::MAX_NORMALIZE_TARGET.0,
    ))
}

fn clamp_dsd_auto_gain_margin_db(value: DbNano) -> DbNano {
    DbNano(value.0.clamp(
        DSD_AUTO_GAIN_MARGIN_DB_MIN_NANO,
        DSD_AUTO_GAIN_MARGIN_DB_MAX_NANO,
    ))
}

fn clamp_pill<T: Clone + PartialEq>(pill: &mut PillState<T>) {
    clamp_pill_excluding(pill, |_| false);
}

/// Clamp like `clamp_pill`, but never AUTO-select an option matching
/// `auto_excluded`. Source-coupled pills ("same as source") sit first on
/// their rows and are enabled for every format; they must remain a
/// deliberate user choice — constraint fallback landing on them would
/// silently rebind the conversion to source-relative semantics.
fn clamp_pill_excluding<T: Clone + PartialEq>(
    pill: &mut PillState<T>,
    auto_excluded: impl Fn(&crate::tui::pill::PillOption<T>) -> bool,
) {
    if !pill.options[pill.selected].enabled {
        // Prefer the nearest enabled option BELOW the disabled selection
        // (quality-ordered pills like bit depth degrade gracefully: FLAC+32
        // switching to ALAC lands on 24, not wrapped-around 16), then scan
        // upward.
        for idx in (0..pill.selected).rev() {
            if pill.options[idx].enabled && !auto_excluded(&pill.options[idx]) {
                pill.selected = idx;
                return;
            }
        }
        let len = pill.options.len();
        for idx in (pill.selected + 1)..len {
            if pill.options[idx].enabled && !auto_excluded(&pill.options[idx]) {
                pill.selected = idx;
                return;
            }
        }
        // Last resort: an excluded option is still better than a disabled
        // selection (unreachable today — every format enables at least one
        // ordinary option).
        if let Some(idx) = pill.options.iter().position(|o| o.enabled) {
            pill.selected = idx;
        }
    }
}

/// Sample-rate clamping: PCM caps degrade to the nearest lower rate
/// (MP3 with 96 kHz selected lands on 48), but a DSD-rate selection falling
/// back to PCM must NOT land on the maximum PCM rate — DSD64 scanning
/// downward would select 768 kHz and silently arm a large upsample from a
/// typical 44.1/48 kHz source. Land on the lowest enabled rate instead.
/// The same-as-source sentinel is never an automatic landing spot.
fn clamp_sample_rate_pill(pill: &mut PillState<u32>, retain_disabled_sentinel: bool) {
    if pill.options[pill.selected].value == SOURCE_SAMPLE_RATE_SENTINEL {
        if pill.options[pill.selected].enabled || retain_disabled_sentinel {
            // A deliberate source selection remains selected even when LOST
            // source facts temporarily make it unavailable. Availability is
            // rendered separately; no fallback scalar may silently replace
            // pending policy. A KNOWN PCM source is different: rate=source
            // is then INVALID for a DSD target and must clamp to a real rate.
            return;
        }
    }
    if !pill.options[pill.selected].enabled
        && tonepoet_pipeline::DsdRate::from_hz(pill.options[pill.selected].value).is_some()
    {
        if let Some(idx) = pill
            .options
            .iter()
            .position(|o| o.enabled && o.value != SOURCE_SAMPLE_RATE_SENTINEL)
        {
            pill.selected = idx;
            return;
        }
    }
    clamp_pill_excluding(pill, |option| option.value == SOURCE_SAMPLE_RATE_SENTINEL);
}

fn select_enabled_index<T: Clone + PartialEq>(pill: &mut PillState<T>, index: usize) -> bool {
    let Some(option) = pill.options.get(index) else {
        return false;
    };
    if !option.enabled {
        return false;
    }
    pill.selected = index;
    true
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
    DsdPath(&'a mut PillState<DsdSourcePathway>),
    DsdProfile(&'a mut PillState<DsdReconstructionSelection>),
    DsdGain(&'a mut PillState<DsdGainMode>),
    DsdGainDb {
        gain_db: &'a mut DbNano,
        gain_mode: &'a mut PillState<DsdGainMode>,
    },
    DsdNormalizeTarget {
        target_dbfs: &'a mut DbNano,
        gain_mode: &'a mut PillState<DsdGainMode>,
    },
    DsdAutoGainMargin {
        margin_db: &'a mut DbNano,
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
            Self::DsdPath(p) => p.select_next(),
            Self::DsdProfile(p) => p.select_next(),
            Self::DsdGain(p) => p.select_next(),
            Self::DsdGainDb { gain_db, gain_mode } => {
                (*gain_mode).select_value(&DsdGainMode::Fixed);
                step_dsd_to_pcm_gain_db(*gain_db, DSD_TO_PCM_GAIN_DB_STEP_NANO);
            }
            Self::DsdNormalizeTarget { target_dbfs, gain_mode } => {
                (*gain_mode).select_value(&DsdGainMode::NormalizePeak);
                step_dsd_normalize_target_dbfs(*target_dbfs, DSD_TO_PCM_GAIN_DB_STEP_NANO);
            }
            Self::DsdAutoGainMargin { margin_db, gain_mode } => {
                (*gain_mode).select_value(&DsdGainMode::Auto);
                step_dsd_auto_gain_margin_db(*margin_db, DSD_AUTO_GAIN_MARGIN_DB_STEP_NANO);
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
            Self::DsdPath(p) => p.select_prev(),
            Self::DsdProfile(p) => p.select_prev(),
            Self::DsdGain(p) => p.select_prev(),
            Self::DsdGainDb { gain_db, gain_mode } => {
                (*gain_mode).select_value(&DsdGainMode::Fixed);
                step_dsd_to_pcm_gain_db(*gain_db, -DSD_TO_PCM_GAIN_DB_STEP_NANO);
            }
            Self::DsdNormalizeTarget { target_dbfs, gain_mode } => {
                (*gain_mode).select_value(&DsdGainMode::NormalizePeak);
                step_dsd_normalize_target_dbfs(*target_dbfs, -DSD_TO_PCM_GAIN_DB_STEP_NANO);
            }
            Self::DsdAutoGainMargin { margin_db, gain_mode } => {
                (*gain_mode).select_value(&DsdGainMode::Auto);
                step_dsd_auto_gain_margin_db(*margin_db, -DSD_AUTO_GAIN_MARGIN_DB_STEP_NANO);
            }
        }
    }
}

fn step_dsd_normalize_target_dbfs(value: &mut DbNano, delta_nano: i64) {
    *value = clamp_dsd_normalize_target_dbfs(DbNano(value.0.saturating_add(delta_nano)));
}

fn step_dsd_auto_gain_margin_db(value: &mut DbNano, delta_nano: i64) {
    *value = clamp_dsd_auto_gain_margin_db(DbNano(value.0.saturating_add(delta_nano)));
}

/// Which single-file metadata field in the Convert metadata pane is focused
/// or being edited inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertMetadataField {
    Title,
    Artist,
    Album,
    AlbumArtist,
    Genre,
    Year,
}

impl ConvertMetadataField {
    const FIELDS: [Self; 6] = [
        Self::Title,
        Self::Artist,
        Self::Album,
        Self::AlbumArtist,
        Self::Genre,
        Self::Year,
    ];

    pub fn next(self) -> Self {
        let idx = Self::FIELDS
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        Self::FIELDS[(idx + 1) % Self::FIELDS.len()]
    }

    pub fn prev(self) -> Self {
        let idx = Self::FIELDS
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        Self::FIELDS[(idx + Self::FIELDS.len() - 1) % Self::FIELDS.len()]
    }

    pub fn from_button_kind(kind: crate::tui::button_map::MetadataFieldKind) -> Self {
        match kind {
            crate::tui::button_map::MetadataFieldKind::Title => Self::Title,
            crate::tui::button_map::MetadataFieldKind::Artist => Self::Artist,
            crate::tui::button_map::MetadataFieldKind::Album => Self::Album,
            crate::tui::button_map::MetadataFieldKind::AlbumArtist => Self::AlbumArtist,
            crate::tui::button_map::MetadataFieldKind::Genre => Self::Genre,
            crate::tui::button_map::MetadataFieldKind::Year => Self::Year,
        }
    }
}

/// State for the metadata pane
#[derive(Debug, Clone)]
pub struct MetadataState {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Request-scope ALBUMARTIST override for this conversion. Unlike the
    /// source-derived preview fields above, this is user intent: when set it
    /// drives output tags and album identity/folder rendering for the batch.
    pub album_artist_for_conversion: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
    pub field_focus: ConvertMetadataField,
    pub editing: Option<ConvertMetadataField>,
    pub edit_input: crate::tui::text_input::TextInputState,
    pub advanced_open: bool,
    /// Scroll offset for the convert-screen metadata file list. The cursor
    /// itself lives on SourceMode::Batch / SourceMode::MultiTrack.
    pub file_scroll: usize,
}

impl Default for MetadataState {
    fn default() -> Self {
        Self {
            title: None,
            artist: None,
            album: None,
            album_artist_for_conversion: None,
            genre: None,
            year: None,
            field_focus: ConvertMetadataField::Title,
            editing: None,
            edit_input: crate::tui::text_input::TextInputState::empty(),
            advanced_open: false,
            file_scroll: 0,
        }
    }
}

/// State for the output options pane
#[derive(Debug, Clone)]
pub struct OutputOptionsState {
    pub dest_path: Option<PathBuf>,
    pub folder_template: String,
    pub filename_template: String,
    pub merge: PillState<MergeMode>,
    pub companion_extensions: String,
    pub companion_folders: String,
    pub companion_exclude_files: String,
    pub force_encode: PillState<bool>,
    pub disc_subfolders: PillState<bool>,
    pub write_log: PillState<bool>,
    pub actions: crate::convert::pipeline::ActionPipeline,
    pub field_focus: OutputOptionsField,
    pub editing: Option<OutputOptionsField>,
    pub edit_input: crate::tui::text_input::TextInputState,
    pub advanced_open: bool,
}

impl OutputOptionsState {
    pub fn new() -> Self {
        let merge = PillState::new(vec![
            (MergeMode::MultiFile, "multi-file"),
            (MergeMode::SingleImage, "single image"),
        ]);
        let mut force_encode = PillState::new(vec![(false, "off"), (true, "on")]);
        force_encode.select_value(&false);
        let mut disc_subfolders = PillState::new(vec![(false, "off"), (true, "on")]);
        disc_subfolders.select_value(&false);
        let mut write_log = PillState::new(vec![(true, "yes"), (false, "no")]);
        write_log.select_value(&false);

        Self {
            dest_path: None,
            folder_template: "%ARTIST%/%ALBUM% (%YEAR%)".to_string(),
            filename_template: "%TRACKNN% - %TITLE%.%EXT%".to_string(),
            merge,
            companion_extensions: crate::convert::formats::default_companion_extensions().join(", "),
            companion_folders: String::new(),
            companion_exclude_files: String::new(),
            force_encode,
            disc_subfolders,
            write_log,
            actions: crate::convert::pipeline::ActionPipeline::default(),
            field_focus: OutputOptionsField::DestPath,
            editing: None,
            edit_input: crate::tui::text_input::TextInputState::empty(),
            advanced_open: false,
        }
    }

    /// Return the normalized loose companion-file extensions represented by the
    /// editable TUI field. Empty input intentionally means "copy no loose
    /// companion files" and must be preserved when queuing conversions.
    #[must_use]
    pub fn parsed_companion_extensions(&self) -> Vec<String> {
        crate::convert::formats::parse_companion_extensions(&self.companion_extensions)
    }

    /// Return the validated bare companion-folder names represented by the
    /// editable TUI field. Empty input intentionally means "copy no folders".
    #[must_use]
    pub fn parsed_companion_folders(&self) -> Vec<String> {
        crate::convert::formats::parse_companion_folders(&self.companion_folders)
    }

    /// Return the normalized exact file names excluded from loose companion
    /// copying. Empty input intentionally excludes nothing.
    #[must_use]
    pub fn parsed_companion_exclude_files(&self) -> Vec<String> {
        crate::convert::formats::parse_companion_exclude_files(&self.companion_exclude_files)
    }

    /// Apply the output-options companion-copy fields to a queued conversion.
    ///
    /// The copy stage reads `ConversionOptions::effective_companion_*()`, so the
    /// editable TUI strings must be projected into both the new list fields and
    /// the legacy boolean gates. Keeping this projection in one helper prevents
    /// add-to-queue, browse-return, and command-commit paths from silently
    /// falling back to `ConversionOptions::default()` companion behavior.
    pub fn apply_companion_copying_to_conversion_options(
        &self,
        options: &mut crate::convert::ConversionOptions,
    ) {
        let extensions = self.parsed_companion_extensions();
        let folders = self.parsed_companion_folders();

        // Empty strings are a real user choice, not "missing config". Mirror
        // that choice into the backwards-compatible gates so effective_*()
        // cannot resurrect defaults later in the processor/pipeline bridge.
        options.copy_auxiliary_files = !extensions.is_empty();
        options.copy_subdirectories = !folders.is_empty();
        options.companion_extensions = extensions;
        options.companion_folders = folders;
        options.companion_exclude_files = self.parsed_companion_exclude_files();
        options.actions = self.actions.clone();
        options.force_encode = *self.force_encode.selected_value();
        options.create_disc_subfolders = *self.disc_subfolders.selected_value();
        // Do not mutate the raw naming template here. Disc subfolders are a
        // first-class ConversionOptions flag and are projected canonically by
        // ConversionOptions::effective_naming_template at the request-building
        // boundary. Keeping this helper side-effect-free prevents double-prefix
        // bugs when presets, queue reloads, or non-TUI entrypoints reuse the
        // same options.
        if let Some(settings) = options.pipeline_settings.as_mut() {
            settings.force_encode = *self.force_encode.selected_value();
        }
        options.write_log_file = *self.write_log.selected_value();
    }
}

#[cfg(test)]
mod output_options_companion_projection_tests {
    use super::OutputOptionsState;

    #[test]
    fn output_options_project_custom_companion_values_into_conversion_options() {
        let mut state = OutputOptionsState::new();
        state.companion_extensions = "jpg, .PDF, .jpg".to_string();
        state.companion_folders = "Scans, Artwork, ../escape".to_string();
        let mut options = crate::convert::ConversionOptions::default();

        state.apply_companion_copying_to_conversion_options(&mut options);

        assert!(options.copy_auxiliary_files);
        assert!(options.copy_subdirectories);
        assert_eq!(options.companion_extensions, vec![".jpg", ".pdf"]);
        assert_eq!(options.companion_folders, vec!["Scans", "Artwork"]);
        assert!(!options.force_encode);
        assert_eq!(options.effective_companion_extensions(), vec![".jpg", ".pdf"]);
        assert_eq!(options.effective_companion_folders(), vec!["Scans", "Artwork"]);
        assert!(!options.write_log_file);
    }

    #[test]
    fn output_options_project_empty_companion_values_as_explicit_disable() {
        let mut state = OutputOptionsState::new();
        state.companion_extensions.clear();
        state.companion_folders.clear();
        state.write_log.select_value(&true);
        let mut options = crate::convert::ConversionOptions::default();

        state.apply_companion_copying_to_conversion_options(&mut options);

        assert!(!options.copy_auxiliary_files);
        assert!(!options.copy_subdirectories);
        assert!(options.companion_extensions.is_empty());
        assert!(options.companion_folders.is_empty());
        assert!(options.effective_companion_extensions().is_empty());
        assert!(options.effective_companion_folders().is_empty());
        assert!(options.write_log_file);
    }

    #[test]
    fn output_options_field_cycle_skips_below_fold_fields_when_collapsed() {
        use super::OutputOptionsField::*;

        assert_eq!(DestPath.next_for(false), FolderTemplate);
        assert_eq!(FolderTemplate.next_for(false), FilenameTemplate);
        assert_eq!(FilenameTemplate.next_for(false), MergeMode);
        assert_eq!(MergeMode.next_for(false), DestPath);
        assert_eq!(DestPath.prev_for(false), MergeMode);
        assert_eq!(CompanionExtensions.clamp_for(false), MergeMode);
        assert_eq!(CompanionFolders.clamp_for(false), MergeMode);
        assert_eq!(Actions.clamp_for(false), MergeMode);
    }

    #[test]
    fn output_options_field_cycle_includes_companion_fields_when_maximized() {
        use super::OutputOptionsField::*;

        assert_eq!(MergeMode.next_for(true), CompanionExtensions);
        assert_eq!(CompanionExtensions.next_for(true), CompanionFolders);
        assert_eq!(CompanionFolders.next_for(true), ExcludeFiles);
        assert_eq!(ExcludeFiles.next_for(true), ForceEncode);
        assert_eq!(ForceEncode.next_for(true), DiscSubfolders);
        assert_eq!(DiscSubfolders.next_for(true), WriteLog);
        assert_eq!(WriteLog.next_for(true), Actions);
        assert_eq!(Actions.next_for(true), DestPath);
        assert_eq!(DestPath.prev_for(true), Actions);
        assert_eq!(Actions.prev_for(true), WriteLog);
        assert_eq!(WriteLog.prev_for(true), DiscSubfolders);
        assert_eq!(DiscSubfolders.prev_for(true), ForceEncode);
        assert_eq!(CompanionExtensions.clamp_for(true), CompanionExtensions);
    }

    #[test]
    fn output_options_field_cycle_matches_rows_rendered_for_small_maximized_panes() {
        use super::OutputOptionsField::*;

        assert_eq!(ExcludeFiles.next_for_area(true, 13, true), DestPath);
        assert_eq!(DestPath.prev_for_area(true, 13, true), ExcludeFiles);
        assert_eq!(WriteLog.clamp_for_area(true, 13, true), MergeMode);
        assert_eq!(Actions.clamp_for_area(true, 13, true), MergeMode);

        assert_eq!(WriteLog.next_for_area(true, 17, true), DestPath);
        assert_eq!(DestPath.prev_for_area(true, 17, true), WriteLog);
        assert_eq!(Actions.clamp_for_area(true, 17, true), MergeMode);

        assert_eq!(WriteLog.next_for_area(true, 20, true), Actions);
        assert_eq!(Actions.next_for_area(true, 20, true), DestPath);

        // Feature gate OFF: the Actions row never joins the cycle, even at
        // full height, and stale Actions focus clamps away.
        assert_eq!(WriteLog.next_for_area(true, 20, false), DestPath);
        assert_eq!(Actions.clamp_for_area(true, 20, false), MergeMode);
        assert_eq!(DestPath.prev_for_area(true, 20, false), WriteLog);
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
    /// Last compact metadata field click used for double-click full-editor entry.
    pub metadata_field_last_click: Option<(ConvertMetadataField, std::time::Instant)>,
    /// Last destination-path field click used for double-click directory picking.
    pub dest_path_last_click: Option<std::time::Instant>,
    /// In-flight archive preview extraction/probe, if any. Completed previews
    /// move into `SourceMode::MultiTrack::archive_preview`; pending previews
    /// live here so source changes can cancel and clean staging immediately.
    pub pending_archive_preview: Option<PendingArchivePreview>,
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
            metadata_field_last_click: None,
            dest_path_last_click: None,
            pending_archive_preview: None,
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
        self.metadata_field_last_click = None;
        self.dest_path_last_click = None;
    }

    /// Recompute source-dependent format constraints without selecting source
    /// defaults. This is the safe path for late async probe completions when
    /// the user has already changed output controls: facts such as "source is
    /// DSD" still affect enabled/disabled state, but sample-rate, bit-depth,
    /// dither, and resampler selections are not cascaded from the source.
    ///
    /// Source-identity policy lives here — this is the single choke point for
    /// every mode install (`set_source_mode*` both route through it). Only a
    /// COMPLETED probe (`current_info()` = Some) may promote the identity to
    /// Known; an Empty source or a pending-probe placeholder supplies at most
    /// an extension HINT for row visibility and demotes the identity instead.
    /// Promoting a guess to Known clamped away deliberate same-as-source
    /// selections at exactly the moments the retention design exists for
    /// (emptying the batch; staging an .iso whose DSD-ness the probe has not
    /// yet discovered).
    pub fn refresh_source_constraints_preserving_format_selection(&mut self) {
        match self.source.mode.current_info() {
            Some(info) => {
                let source_is_dsd = source_info_is_dsd(info);
                self.format.set_source_is_dsd(source_is_dsd);
            }
            None => {
                let hint = self
                    .source
                    .mode
                    .current_path()
                    .map(|path| source_path_is_dsd(path))
                    .unwrap_or(false);
                self.format.set_pending_source_hint(hint);
            }
        }
        self.format.apply_format_constraints();
    }

    /// Recompute constraints from a newly probed source without selecting
    /// source-derived defaults. Batch first-file probes use this when the
    /// preview cursor has moved away from the file that defines batch output
    /// defaults, so DSD/PCM source facts still come from the completed probe.
    pub fn refresh_source_info_constraints_preserving_format_selection(
        &mut self,
        info: &SourceInfo,
    ) {
        self.format.set_source_is_dsd(source_info_is_dsd(info));
        self.format.apply_format_constraints();
    }

    pub fn install_pending_archive_preview(&mut self, pending: PendingArchivePreview) {
        self.clear_pending_archive_preview();
        self.pending_archive_preview = Some(pending);
    }

    pub fn pending_archive_preview_matches(&self, generation: u64, archive_path: &Path) -> bool {
        self.pending_archive_preview
            .as_ref()
            .is_some_and(|pending| pending.matches(generation, archive_path))
    }

    pub fn take_pending_archive_preview(
        &mut self,
        generation: u64,
        archive_path: &Path,
    ) -> Option<PendingArchivePreview> {
        if self.pending_archive_preview_matches(generation, archive_path) {
            self.pending_archive_preview.take()
        } else {
            None
        }
    }

    pub fn clear_pending_archive_preview(&mut self) {
        if let Some(pending) = self.pending_archive_preview.take() {
            pending.cancel_and_cleanup();
        }
    }

    /// Replace the convert source mode and reset metadata list state in one
    /// place so source changes cannot leave stale scroll or double-click state.
    pub fn set_source_mode(&mut self, mode: SourceMode) {
        self.clear_pending_archive_preview();
        self.source.mode.cleanup_archive_preview_staging();
        let retained_paths = mode.all_paths();
        self.source.cue_artifact_audio.retain(|path| {
            crate::convert::queue_expansion::path_list_contains_queue_identity(
                &retained_paths,
                path,
            )
        });
        self.source.cleanup_synthetic_cue_artifacts_not_in(&retained_paths);
        self.source.batch_probe_pending = None;
        self.source.batch_probe_debounce = None;
        self.reset_metadata_file_list_state();
        self.source.mode = mode;
        self.refresh_source_constraints_preserving_format_selection();
    }

    pub fn sync_archive_preview_cursor_metadata_and_defaults(&mut self) {
        let Some((info, metadata)) = self.source.mode.sync_archive_preview_cursor_state() else {
            return;
        };
        apply_source_metadata_to_convert(self, &metadata);
        self.refresh_source_info_constraints_preserving_format_selection(&info);
    }

    /// Source bit depth for format-pane side effects such as auto-dither.
    pub fn current_source_bit_depth(&self) -> Option<u32> {
        self.source.mode.current_bit_depth()
    }

    pub fn current_source_sample_rate(&self) -> Option<u32> {
        self.source.mode.current_info().map(|info| info.sample_rate)
    }

    /// Apply source-aware format pane defaults from an already-known probe
    /// result. Batch-level first-file probes use this directly so moving the
    /// batch preview cursor cannot make a valid probe completion look absent.
    pub fn apply_source_info_defaults(&mut self, info: &SourceInfo) {
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
        // Constraints must run BEFORE the auto rules: cascades deliberately
        // force-install out-of-range automatic defaults (768k/Int32) that the
        // constraint pass clamps to the nearest allowed scalar (384k/Int24).
        // The auto rules read the SELECTED rate/depth — run against unclamped
        // values they armed a real 768->384 resample with resampler=None and
        // a 32->24 truncation with dither=None. The auto rules only select
        // values (never enablement) and cannot pick disabled options, so no
        // trailing constraints pass is needed.
        self.format.apply_format_constraints();
        self.format.apply_auto_dither(source_bits);
        self.format.apply_auto_resampler(Some(source_rate));
    }

    /// Apply source-aware format pane defaults after a probe completes.
    /// For PCM sources: matches sample rate and bit depth to source.
    /// For DSD sources with PCM output: sets recommended target rate and 24-bit.
    /// For sources without reliable probe info, clears source-derived controls
    /// so stale defaults from a previous source cannot survive the source swap.
    pub fn apply_source_defaults(&mut self) {
        let Some(info) = self.source.mode.current_info().cloned() else {
            self.format.clear_source_derived_defaults();
            return;
        };
        self.apply_source_info_defaults(&info);
    }
}

// ── Preset state ─────────────────────────────────────────────────────

/// State for preset management
#[derive(Debug, Clone)]
pub struct PresetState {
    pub active_preset: Option<String>,
    /// Full path that owns the active preset. This keeps picker-based load/save
    /// flows honest when the user navigates outside the default presets directory.
    pub active_preset_path: Option<PathBuf>,
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

    /// Record both the display name and the owning path for the active preset.
    pub fn set_active_preset_path(&mut self, name: impl Into<String>, path: PathBuf) {
        self.active_preset = Some(name.into());
        self.active_preset_path = Some(path);
    }

    /// Clear the active preset identity and its owning path together.
    pub fn clear_active_preset(&mut self) {
        self.active_preset = None;
        self.active_preset_path = None;
        self.modified = false;
    }

    /// Return the path to use for an overwrite of the active preset.
    pub fn active_preset_save_path(&self) -> Option<PathBuf> {
        let name = self.active_preset.as_deref()?;
        Some(
            self.active_preset_path
                .clone()
                .unwrap_or_else(|| crate::tui::presets::preset_file_path(name)),
        )
    }
}

impl Default for PresetState {
    fn default() -> Self {
        Self {
            active_preset: None,
            active_preset_path: None,
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
    /// Three-view theme builder overlay.
    ThemeBuilder(Box<crate::tui::theme_builder::ThemeBuilderState>),
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
        /// Anchor for the root level (right-click position or footer edge).
        origin: (u16, u16),
        /// Place the root panel immediately above `origin.1` instead of below it.
        anchor_bottom: bool,
    },
    /// Bulk rename wizard overlay. Boxed because the state is large.
    BulkRename(Box<BulkRenameState>),
    /// Ordered pre/post conversion-actions editor.
    ConversionActionsWizard(Box<crate::tui::conversion_actions_ui::ConversionActionsWizardState>),
    /// Real-directory dry-run/apply surface for `:actions-run`.
    ActionsRun(Box<crate::tui::conversion_actions_ui::ActionsRunState>),
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
    /// Reusable global file picker overlay used outside the metadata editor.
    FilePicker(MetadataFilePickerState),
    /// Reusable file-task progress overlay hosted by Tonepoet. The state and
    /// renderer live in the file-picker crate; this wrapper only stores the
    /// app-side control channel and session identity.
    FileTaskProgress(FileTaskProgressSession),
    /// Full metadata tag editor overlay.
    MetadataEditor(Box<MetadataEditorState>),
    /// Custom metadata auto-number preview/editor. The owning metadata editor
    /// remains parked in `pending_metadata_editor` and is rendered behind it.
    MetadataAutoNumber(Box<crate::tui::metadata_autonumber::AutoNumberOverlayState>),
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
    /// Operation-scoped chooser for equally-ranked viable CUE descriptions.
    /// The selected path is fed back into the exact edit/convert continuation;
    /// it is never persisted as a global preference.
    CueSelect(Box<CueSelectState>),
    /// GNUDB match selection overlay (when multiple matches are returned).
    GnudbSelect {
        operation_id: crate::tui::message::TagsMbOperationId,
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
    /// Advanced file-operation settings opened from the Config screen. The
    /// overlay owns a draft and commits all values together on Enter.
    FileOperationSettings(FileOperationSettingsState),
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

#[derive(Debug, Clone)]
pub enum CueSelectOperation {
    Metadata {
        selection_snapshot: Vec<std::path::PathBuf>,
        cue_policy: crate::convert::pipeline::CueSidecarPolicy,
        cue_selection_overrides:
            crate::convert::queue_expansion::QueueCueSelectionOverrides,
    },
    BrowseConvert {
        request: crate::tui::command::BrowseConvertExpansionRequest,
    },
}

#[derive(Debug, Clone)]
pub struct CueSelectState {
    pub parent: std::path::PathBuf,
    pub candidates: Vec<std::path::PathBuf>,
    pub selected: usize,
    pub scroll: usize,
    pub operation: CueSelectOperation,
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
    /// Exact parked metadata-editor session this review may restore on cancel.
    /// `None` means the review did not originate from an editor and must never
    /// consume an unrelated editor that happened to become pending later.
    pub editor_session: Option<crate::tui::message::MetadataEditorSessionGuard>,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataInvalidApeRepairOperation {
    pub session_id: u64,
    pub generation: u64,
    pub targets: Vec<(std::path::PathBuf, Vec<String>)>,
}

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

/// Which embedded-tag rows the Metadata tab surfaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MetadataEditorView {
    #[default]
    Canonical,
    All,
}

impl MetadataEditorView {
    pub fn label(self) -> &'static str {
        match self {
            Self::Canonical => "Canonical",
            Self::All => "All",
        }
    }
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


/// ReplayGain scan mode launched from the metadata editor's ReplayGain tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataReplayGainScanMode {
    /// Recalculate per-track ReplayGain tags for all active files.
    Track,
    /// Recalculate album + track ReplayGain tags for the whole active album.
    Album,
}

impl MetadataReplayGainScanMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album + track",
        }
    }
}

/// In-flight ReplayGain scan identity for the metadata editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataReplayGainScanState {
    /// Details/session id of the presentation surface that launched the scan.
    pub session_id: u64,
    pub generation: u64,
    pub mode: MetadataReplayGainScanMode,
    pub file_count: usize,
}

/// In-flight Details-tab HDCD/PRE analysis identity for the metadata editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataDetailsAnalysisState {
    /// Details/session id of the presentation surface that launched the scan.
    pub session_id: u64,
    pub generation: u64,
    pub file_count: usize,
}

/// Result for one file from the Details-tab HDCD/PRE analyzer.
#[derive(Debug, Clone)]
pub struct MetadataDetailsAnalysisFileResult {
    /// Index captured at dispatch time. Reducers must verify path identity too.
    pub index: usize,
    pub path: std::path::PathBuf,
    /// File metadata captured at dispatch time. Used to reject stale results and
    /// to key partial analysis-cache updates without re-statting in reducers.
    pub modified: Option<std::time::SystemTime>,
    pub file_size: Option<u64>,
    /// Only HDCD and metadata/CUE/catalog PRE facts are populated. This is
    /// intentionally not a full DR/peak/RMS analysis result.
    pub facts: MetadataAnalysisFacts,
    /// Non-fatal per-file analyzer issues. PRE can still succeed when HDCD
    /// fails, and vice versa.
    pub issues: Vec<String>,
}

/// Artwork write/remove mode launched from the metadata editor Artwork tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataArtworkWriteMode {
    Write,
    Remove,
}

impl MetadataArtworkWriteMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Write => "artwork write",
            Self::Remove => "artwork removal",
        }
    }
}

/// In-flight artwork write/remove identity for the metadata editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataArtworkWriteState {
    /// Details/session id of the presentation surface that launched the write.
    pub session_id: u64,
    pub generation: u64,
    pub mode: MetadataArtworkWriteMode,
    pub file_count: usize,
}


/// App-side control requests for a hosted file task.
///
/// These are intentionally app-local: the reusable picker crate emits semantic
/// [`tui_file_picker::FileTaskUserAction`] values, and Tonepoet maps them to
/// these concrete worker controls.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FileTaskControl {
    Pause,
    Resume,
    SkipCurrent,
    Abort,
    ConflictResolution {
        request_id: u64,
        resolution: tui_file_picker::ConflictResolution,
    },
}

/// Ownership role for a file-task progress surface.
///
/// A retained-results viewer is presentation-only. It deliberately carries the
/// original task session id for correlation, but it never participates in task
/// ordering, sends worker controls, or writes its clone back into authoritative
/// retained state when dismissed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileTaskProgressSessionRole {
    LiveTask,
    RetainedViewer,
}

/// Host wrapper around the reusable crate-owned progress overlay.
#[derive(Debug, Clone)]
pub struct FileTaskProgressSession {
    pub session_id: u64,
    pub role: FileTaskProgressSessionRole,
    pub progress: tui_file_picker::FileTaskProgressState,
    pub controls: mpsc::Sender<FileTaskControl>,
}

impl FileTaskProgressSession {
    pub fn new(
        progress: tui_file_picker::FileTaskProgressState,
        controls: mpsc::Sender<FileTaskControl>,
    ) -> Self {
        Self {
            session_id: next_file_task_session_id(),
            role: FileTaskProgressSessionRole::LiveTask,
            progress,
            controls,
        }
    }

    pub fn retained_viewer(
        session_id: u64,
        progress: tui_file_picker::FileTaskProgressState,
    ) -> Self {
        let (controls, receiver) = mpsc::channel();
        drop(receiver);
        Self {
            session_id,
            role: FileTaskProgressSessionRole::RetainedViewer,
            progress,
            controls,
        }
    }

    #[must_use]
    pub const fn is_live_task(&self) -> bool {
        matches!(self.role, FileTaskProgressSessionRole::LiveTask)
    }

    #[must_use]
    pub const fn is_retained_viewer(&self) -> bool {
        matches!(self.role, FileTaskProgressSessionRole::RetainedViewer)
    }

    pub fn set_theme(&mut self, theme: tui_file_picker::FilePickerTheme) {
        self.progress.set_theme(theme);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagTransferDirection {
    To,
    From,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagTransferScope {
    Canonical,
    All,
}

/// Tonepoet-specific purpose for a reusable file-picker session.
///
/// The picker crate intentionally does not know what a selected path means.
/// Tonepoet keeps that purpose here and pairs it with the crate-owned picker
/// state in [`MetadataFilePickerState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePickerPurpose {
    SelectArtwork {
        picture_type: lofty::picture::PictureType,
    },
    SelectFile,
    SelectDirectory,
    /// Convert output-options destination picker.
    SelectDestination,
    /// Convert preset loader; selects an existing TOML preset.
    SelectPreset,
    /// Convert preset save-as picker; returns the composed target preset path.
    SavePreset,
    /// Browse-screen destination picker for copy operations.
    ///
    /// `sources` is captured before the picker opens so later cursor or
    /// selection changes cannot redirect the operation.
    CopyTo {
        sources: Vec<PathBuf>,
        force: bool,
    },
    /// Browse-screen destination picker for move operations.
    ///
    /// `sources` is captured before the picker opens so later cursor or
    /// selection changes cannot redirect the operation.
    MoveTo {
        sources: Vec<PathBuf>,
        force: bool,
    },
    /// Browse-side explicit tag transfer. `fixed_roots` is the side captured
    /// before the picker opens; the selected path supplies the opposite side.
    BrowseTagTransfer {
        direction: TagTransferDirection,
        scope: TagTransferScope,
        fixed_roots: Vec<PathBuf>,
        /// Frozen active order for classifying a directory selected by this
        /// transfer operation.
        metadata_target_priority: Vec<crate::config::AggregateMetadataTarget>,
    },
    /// Read field blocks from a text file into the currently open editor.
    MetadataTagBlocksFile,
    /// Editor-side transfer picker. The active editor surface is the fixed side.
    MetadataTagTransfer {
        direction: TagTransferDirection,
        scope: TagTransferScope,
        /// Frozen active order for classifying a directory selected by this
        /// transfer operation.
        metadata_target_priority: Vec<crate::config::AggregateMetadataTarget>,
    },
    Generic {
        id: String,
    },
}

/// Host wrapper for the reusable `tui-file-picker` crate.
///
/// This is not a second picker implementation. It exists only to retain the
/// app-specific completion purpose while all navigation, rendering, hit-testing,
/// filtering, menu state, and file operations remain owned by the reusable
/// crate.
#[derive(Debug, Clone)]
pub struct MetadataFilePickerState {
    pub session_id: u64,
    pub purpose: FilePickerPurpose,
    pub picker: tui_file_picker::FilePickerState,
}

impl MetadataFilePickerState {
    pub fn new(purpose: FilePickerPurpose, picker: tui_file_picker::FilePickerState) -> Self {
        Self {
            session_id: next_file_picker_session_id(),
            purpose,
            picker,
        }
    }

    pub fn selected_path(&self) -> Option<&std::path::Path> {
        self.picker.selected_path()
    }

    pub fn current_dir(&self) -> &std::path::Path {
        self.picker.current_dir()
    }

    pub fn set_theme(&mut self, theme: tui_file_picker::FilePickerTheme) {
        self.picker.set_theme(theme);
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
    pub sample_format_is_float: Option<bool>,
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
            sample_format_is_float: info.sample_format_is_float,
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
            sample_format_is_float: info.sample_format_is_float,
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
            sample_format_is_float: facts.sample_format_is_float,
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

/// Result of an asynchronous embedded-artwork decode request.
pub struct ArtworkPreviewLoadResult {
    pub path: std::path::PathBuf,
    pub picture_type: lofty::picture::PictureType,
    pub generation: usize,
    pub result: Result<image::DynamicImage, String>,
}

/// Cached decoded artwork preview for the Artwork tab.
///
/// The protocol state is intentionally not cloned; stale clones should reload
/// lazily from the file path and picture type rather than sharing terminal
/// protocol state across editor instances.
pub struct ArtworkPreviewCache {
    pub path: std::path::PathBuf,
    pub picture_type: lofty::picture::PictureType,
    /// Most recent preview content area requested by render. This is desired
    /// geometry only; it is not evidence that the cached protocol was encoded
    /// for this area.
    pub desired_preview_area: ratatui::layout::Rect,
    /// Preview area for which `image_protocol` was actually prepared.
    pub encoded_preview_area: ratatui::layout::Rect,
    /// App-level terminal picker generation requested by render. Incremented
    /// when terminal resize/cell-size changes require image protocol state to
    /// be rebuilt.
    pub desired_protocol_generation: usize,
    /// App generation for which `image_protocol` was actually prepared.
    pub encoded_protocol_generation: usize,
    /// Kitty-only graphics retransmit generation for which `image_protocol`
    /// was prepared. Separate from resize/cell-metric generation.
    pub encoded_retransmit_generation: usize,
    pub generation: usize,
    pub decoded_generation: Option<usize>,
    pub decoded_image: Option<image::DynamicImage>,
    pub receiver: Option<mpsc::Receiver<ArtworkPreviewLoadResult>>,
    pub image_protocol: Option<Box<dyn ratatui_image::protocol::StatefulProtocol>>,
    pub error: Option<String>,
}

impl fmt::Debug for ArtworkPreviewCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArtworkPreviewCache")
            .field("path", &self.path)
            .field("picture_type", &self.picture_type)
            .field("desired_preview_area", &self.desired_preview_area)
            .field("encoded_preview_area", &self.encoded_preview_area)
            .field("desired_protocol_generation", &self.desired_protocol_generation)
            .field("encoded_protocol_generation", &self.encoded_protocol_generation)
            .field("encoded_retransmit_generation", &self.encoded_retransmit_generation)
            .field("generation", &self.generation)
            .field("decoded_generation", &self.decoded_generation)
            .field("has_decoded_image", &self.decoded_image.is_some())
            .field("has_receiver", &self.receiver.is_some())
            .field("has_image_protocol", &self.image_protocol.is_some())
            .field("error", &self.error)
            .finish()
    }
}

impl Clone for ArtworkPreviewCache {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            picture_type: self.picture_type.clone(),
            desired_preview_area: self.desired_preview_area,
            encoded_preview_area: self.encoded_preview_area,
            desired_protocol_generation: self.desired_protocol_generation,
            encoded_protocol_generation: self.encoded_protocol_generation,
            encoded_retransmit_generation: self.encoded_retransmit_generation,
            generation: self.generation,
            decoded_generation: self.decoded_generation,
            decoded_image: None,
            receiver: None,
            image_protocol: None,
            error: self.error.clone(),
        }
    }
}

/// First-class issue reporting for read-only tabs and save eligibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataIssue {
    Filesystem { path: std::path::PathBuf, reason: String },
    TagRead { path: std::path::PathBuf, reason: String },
    RecoverableTagWarning { path: std::path::PathBuf, reason: String },
    Unsupported { path: std::path::PathBuf, reason: String },
    Probe { path: std::path::PathBuf, reason: String, retryable: bool },
    SaveBlocked { path: std::path::PathBuf, reason: String },
    Write { path: std::path::PathBuf, reason: String },
}

static METADATA_EDITOR_DETAILS_SESSION_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

static FILE_PICKER_SESSION_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

static FILE_TASK_SESSION_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// Allocate a process-unique file-picker session id.
///
/// Every picker completion carries this id back to the event loop. The reducer
/// must match it against the currently open picker before closing the overlay or
/// dispatching any host-side action. Purpose alone is not sufficient because a
/// stale completion for `SelectArtwork` could otherwise race with a newer
/// artwork picker session.
pub fn next_file_picker_session_id() -> u64 {
    FILE_PICKER_SESSION_COUNTER.fetch_add(
        1,
        std::sync::atomic::Ordering::Relaxed,
    )
}

/// Allocate a process-unique file-task progress session id.
pub fn next_file_task_session_id() -> u64 {
    FILE_TASK_SESSION_COUNTER.fetch_add(
        1,
        std::sync::atomic::Ordering::Relaxed,
    )
}

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
pub enum MetadataInvalidApeRepairOutcome {
    NotModified {
        reason: String,
    },
    CancelledBeforeCommit {
        reason: String,
    },
    CommitStateUnknown {
        reason: String,
    },
    CommittedAndVerified {
        removed_keys: Vec<String>,
        durability_warnings: Vec<String>,
    },
    CommittedButVerificationFailed {
        removed_keys: Vec<String>,
        durability_warnings: Vec<String>,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataEditorWriteOutcome {
    Saved,
    /// The file mutation committed and should be treated as saved for all
    /// semantic follow-up work (including CUE sidecar writeback), but one or
    /// more post-commit durability operations could not be fully confirmed.
    SavedWithWarnings { warnings: Vec<String> },
    Failed { reason: String },
    Skipped { reason: String },
    SidecarCueSaved {
        cue_path: std::path::PathBuf,
        unchanged: bool,
        rewritten_as_utf8: bool,
    },
    SidecarCueFailed { cue_path: std::path::PathBuf, reason: String },
    /// Typed result from the invalid-APEv2 repair worker. This remains a
    /// dedicated outcome through the worker/message boundary so committed,
    /// unverified, cancelled, and unknown-commit states are never inferred
    /// from human-readable warning strings.
    InvalidApeRepair(MetadataInvalidApeRepairOutcome),
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

    pub fn saved_with_warnings(path: std::path::PathBuf, warnings: Vec<String>) -> Self {
        if warnings.is_empty() {
            Self::saved(path)
        } else {
            Self { path, outcome: MetadataEditorWriteOutcome::SavedWithWarnings { warnings } }
        }
    }

    pub fn failed(path: std::path::PathBuf, reason: impl Into<String>) -> Self {
        Self { path, outcome: MetadataEditorWriteOutcome::Failed { reason: reason.into() } }
    }

    pub fn skipped(path: std::path::PathBuf, reason: impl Into<String>) -> Self {
        Self { path, outcome: MetadataEditorWriteOutcome::Skipped { reason: reason.into() } }
    }

    pub fn sidecar_cue_saved(
        audio_path: std::path::PathBuf,
        cue_path: std::path::PathBuf,
        unchanged: bool,
        rewritten_as_utf8: bool,
    ) -> Self {
        Self {
            path: audio_path,
            outcome: MetadataEditorWriteOutcome::SidecarCueSaved {
                cue_path,
                unchanged,
                rewritten_as_utf8,
            },
        }
    }

    pub fn sidecar_cue_failed(
        audio_path: std::path::PathBuf,
        cue_path: std::path::PathBuf,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            path: audio_path,
            outcome: MetadataEditorWriteOutcome::SidecarCueFailed {
                cue_path,
                reason: reason.into(),
            },
        }
    }

    pub fn invalid_ape_repair(
        path: std::path::PathBuf,
        outcome: MetadataInvalidApeRepairOutcome,
    ) -> Self {
        Self {
            path,
            outcome: MetadataEditorWriteOutcome::InvalidApeRepair(outcome),
        }
    }

    pub fn into_legacy_result(self) -> (std::path::PathBuf, Result<(), String>) {
        match self.outcome {
            MetadataEditorWriteOutcome::Saved
            | MetadataEditorWriteOutcome::SavedWithWarnings { .. }
            | MetadataEditorWriteOutcome::SidecarCueSaved { .. }
            | MetadataEditorWriteOutcome::InvalidApeRepair(
                MetadataInvalidApeRepairOutcome::CommittedAndVerified { .. },
            ) => (self.path, Ok(())),
            MetadataEditorWriteOutcome::Failed { reason }
            | MetadataEditorWriteOutcome::Skipped { reason }
            | MetadataEditorWriteOutcome::SidecarCueFailed { reason, .. } => (self.path, Err(reason)),
            MetadataEditorWriteOutcome::InvalidApeRepair(outcome) => {
                let reason = match outcome {
                    MetadataInvalidApeRepairOutcome::NotModified { reason }
                    | MetadataInvalidApeRepairOutcome::CancelledBeforeCommit { reason }
                    | MetadataInvalidApeRepairOutcome::CommitStateUnknown { reason }
                    | MetadataInvalidApeRepairOutcome::CommittedButVerificationFailed {
                        reason,
                        ..
                    } => reason,
                    MetadataInvalidApeRepairOutcome::CommittedAndVerified { .. } => {
                        unreachable!("committed-and-verified repair handled above")
                    }
                };
                (self.path, Err(reason))
            }
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
    pub sidecar_cue_saved: usize,
    pub sidecar_cue_unchanged: usize,
    pub sidecar_cue_failed: usize,
    pub sidecar_cue_utf8_fallback: usize,
    pub durability_warnings: usize,
    pub first_durability_warning: Option<String>,
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
    fn sidecar_cue_saved_status(&self) -> String {
        let suffix = if self.sidecar_cue_saved == 1 { "" } else { "s" };
        if self.sidecar_cue_utf8_fallback == 0 {
            format!("{} CUE sidecar{} updated", self.sidecar_cue_saved, suffix)
        } else if self.sidecar_cue_utf8_fallback == self.sidecar_cue_saved {
            format!(
                "{} CUE sidecar{} updated as UTF-8",
                self.sidecar_cue_saved,
                suffix
            )
        } else {
            format!(
                "{} CUE sidecar{} updated ({} rewritten as UTF-8)",
                self.sidecar_cue_saved,
                suffix,
                self.sidecar_cue_utf8_fallback
            )
        }
    }

    pub fn all_saved(&self) -> bool {
        self.failed == 0
            && self.skipped == 0
            && self.ignored == 0
            && self.sidecar_cue_failed == 0
            && !self.remaining_dirty
    }

    pub fn status_line(&self) -> String {
        if self.all_saved() {
            let mut suffixes = Vec::new();
            if self.sidecar_cue_saved > 0 {
                suffixes.push(self.sidecar_cue_saved_status());
            }
            if self.sidecar_cue_unchanged > 0 {
                suffixes.push(format!(
                    "{} CUE sidecar{} already current",
                    self.sidecar_cue_unchanged,
                    if self.sidecar_cue_unchanged == 1 { "" } else { "s" }
                ));
            }
            if self.durability_warnings > 0 {
                suffixes.push(format!(
                    "{} durability warning{}",
                    self.durability_warnings,
                    if self.durability_warnings == 1 { "" } else { "s" }
                ));
            }
            let suffix = if suffixes.is_empty() {
                String::new()
            } else {
                format!("; {}", suffixes.join(", "))
            };
            let line = format!(
                "Metadata saved ({} file{}{})",
                self.saved,
                if self.saved == 1 { "" } else { "s" },
                suffix,
            );
            if let Some(warning) = &self.first_durability_warning {
                if !warning.trim().is_empty() {
                    return format!("{line} — {warning}");
                }
            }
            return line;
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
        if self.sidecar_cue_saved > 0 {
            parts.push(self.sidecar_cue_saved_status());
        }
        if self.sidecar_cue_unchanged > 0 {
            parts.push(format!("{} CUE sidecar already current", self.sidecar_cue_unchanged));
        }
        if self.sidecar_cue_failed > 0 {
            parts.push(format!("{} CUE sidecar stale", self.sidecar_cue_failed));
        }
        if self.durability_warnings > 0 {
            parts.push(format!(
                "{} durability warning{}",
                self.durability_warnings,
                if self.durability_warnings == 1 { "" } else { "s" }
            ));
        }
        if self.remaining_dirty {
            parts.push("unsaved changes remain".to_string());
        }
        match (&self.first_problem, &self.first_durability_warning) {
            (Some(problem), _) if !problem.trim().is_empty() => {
                format!("Metadata: {} — {}", parts.join(", "), problem)
            }
            (_, Some(warning)) if !warning.trim().is_empty() => {
                format!("Metadata: {} — {}", parts.join(", "), warning)
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



/// Cached analysis/detection facts surfaced in the Details tab.
#[derive(Debug, Clone, Default)]
pub struct MetadataAnalysisFacts {
    /// None means HDCD has not been scanned or is not applicable.
    pub hdcd_detected: Option<bool>,
    pub hdcd_detail: Option<String>,
    /// None means pre-emphasis has not been scanned.
    pub preemphasis: Option<crate::tui::preemphasis::PreemphasisConfidence>,
    pub preemphasis_detail: Option<String>,
}

impl MetadataAnalysisFacts {
    pub fn has_any_result(&self) -> bool {
        self.hdcd_detected.is_some() || self.preemphasis.is_some()
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
    /// Cached HDCD / pre-emphasis analysis facts.
    pub analysis_facts: MetadataAnalysisFacts,
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
        metadata_issue_kind: Option<crate::tui::probe::MetadataReadIssueKind>,
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
            metadata_issue_kind,
            None,
        );
        Self {
            file_facts,
            media_facts: ProbeState::NotLoaded,
            artwork_facts,
            analysis_facts: MetadataAnalysisFacts::default(),
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
        metadata_issue_kind: Option<crate::tui::probe::MetadataReadIssueKind>,
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
            metadata_issue_kind,
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

    pub fn set_analysis_result(&mut self, result: &crate::tui::analyze::AnalysisResult) {
        self.analysis_facts.hdcd_detected = result.hdcd_detected;
        self.analysis_facts.hdcd_detail = result.hdcd_detail.clone();
        if let Some(confidence) = result.preemphasis {
            let raw = crate::tui::preemphasis::PreemphasisResult {
                path: result.path.clone(),
                confidence,
                cue_confirmed: false,
                llr_m2_vs_m0: f64::NAN,
                llr_m2_vs_m1: f64::NAN,
                fitted_alpha: f64::NAN,
                frames_scored: 0,
                deemph_distance_delta: 0.0,
                gates_fired: vec![],
                detail: result.preemphasis_detail.clone().unwrap_or_default(),
                spectral_rms_error: 0.0,
                crest_improvement: 0.0,
            };
            let safe = crate::tui::preemphasis::metadata_editor_safe_result(&raw);
            self.analysis_facts.preemphasis = Some(safe.confidence);
            self.analysis_facts.preemphasis_detail = if safe.detail.is_empty() {
                None
            } else {
                Some(safe.detail)
            };
        }
    }

    pub fn set_preemphasis_result(&mut self, result: &crate::tui::preemphasis::PreemphasisResult) {
        let safe = crate::tui::preemphasis::metadata_editor_safe_result(result);
        self.analysis_facts.preemphasis = Some(safe.confidence);
        self.analysis_facts.preemphasis_detail = if safe.detail.is_empty() {
            None
        } else {
            Some(safe.detail)
        };
    }

    pub fn set_preemphasis_advisory(
        &mut self,
        advisory: &crate::tui::preemphasis::PreemphasisAdvisory,
    ) {
        let safe = crate::tui::preemphasis::result_from_advisory(
            self.file_facts.path.clone(),
            Some(advisory),
        );
        self.analysis_facts.preemphasis = Some(safe.confidence);
        self.analysis_facts.preemphasis_detail = if safe.detail.is_empty() {
            None
        } else {
            Some(safe.detail)
        };
    }

    /// Compatibility path for legacy string caches. Ambiguous catalog labels
    /// are intentionally interpreted as `Possible`, never promoted to strong.
    pub fn set_preemphasis_metadata_label(&mut self, label: &str) {
        let safe = crate::tui::preemphasis::result_from_metadata_label(
            self.file_facts.path.clone(),
            label,
        );
        self.analysis_facts.preemphasis = Some(safe.confidence);
        self.analysis_facts.preemphasis_detail = if safe.detail.is_empty() {
            None
        } else {
            Some(safe.detail)
        };
    }

    /// Merge the narrow Details-tab analyzer facts without touching unrelated
    /// DR/peak/RMS/loudness state. `None` means the analyzer did not attempt or
    /// could not complete that specific detector, so existing facts are kept.
    pub fn merge_analysis_facts(&mut self, facts: &MetadataAnalysisFacts) {
        if facts.hdcd_detected.is_some() {
            self.analysis_facts.hdcd_detected = facts.hdcd_detected;
            self.analysis_facts.hdcd_detail = facts.hdcd_detail.clone();
        }
        if facts.preemphasis.is_some() {
            self.analysis_facts.preemphasis = facts.preemphasis;
            self.analysis_facts.preemphasis_detail = facts.preemphasis_detail.clone();
        }
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
    metadata_issue_kind: Option<crate::tui::probe::MetadataReadIssueKind>,
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
            let issue = match metadata_issue_kind {
                Some(crate::tui::probe::MetadataReadIssueKind::RecoverableTagWarning) => {
                    MetadataIssue::RecoverableTagWarning {
                        path: path.clone(),
                        reason: reason.trim().to_string(),
                    }
                }
                _ => MetadataIssue::TagRead {
                    path: path.clone(),
                    reason: reason.trim().to_string(),
                },
            };
            issues.push(issue);
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

/// Stable source identity for one row in a unified split-CUE album surface.
///
/// The visible metadata grid is deliberately flattened to album track order,
/// but save, edit, delete, and synthetic-CUE regeneration still need to know
/// which sidecar/image/local track each row came from. This mapping is stored
/// on the presentation surface rather than inferred from row position at save
/// time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueAlbumTrackSource {
    pub cue_path: std::path::PathBuf,
    pub audio_path: std::path::PathBuf,
    pub local_track_index: usize,
    pub original_track_number: u32,
    pub file_ref: String,
    pub index00_frames: Option<u32>,
    pub index01_frames: Option<u32>,
    pub isrc: Option<String>,
    /// Track-scoped CUE directives retained verbatim in parse-normal form
    /// (for example `FLAGS PRE` and track-level `REM` lines).
    pub directives: Vec<String>,
}

/// State carried by a unified synthetic split-CUE album surface.
///
/// `audio_paths` is the save dimension and `track_sources` is the row
/// dimension. Ordinary unified sidecar albums regenerate one shared sheet;
/// aggregate embedded-CUE sets retain one generated sheet per member image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueAlbumSyntheticSheet {
    pub cue_paths: Vec<std::path::PathBuf>,
    pub audio_paths: Vec<std::path::PathBuf>,
    pub track_sources: Vec<CueAlbumTrackSource>,
    pub album_title: Option<String>,
    pub album_performer: Option<String>,
    pub album_date: Option<String>,
    pub album_genre: Option<String>,
    pub album_catalog: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataCueSource {
    Sidecar(std::path::PathBuf),
    Embedded(std::path::PathBuf),
}

impl MetadataCueSource {
    pub fn sidecar_path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Sidecar(path) => Some(path.as_path()),
            Self::Embedded(_) => None,
        }
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
    /// Row selection belongs to this presentation surface so switching tabs
    /// cannot leave indices pointing into another surface's entry vector.
    pub selected_rows: std::collections::BTreeSet<usize>,
    pub dirty: bool,
    /// Sticky unresolved state set when a mandatory post-save re-read fails.
    /// It survives ordinary dirty recomputation and is cleared only after a
    /// successful refresh replaces the surface entries from disk.
    pub refresh_failed: bool,
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
    /// True when the active CUESHEET row represents an embedded tag that existed
    /// in the edited audio file, not a sidecar-derived synthetic row. This keeps
    /// embedded-CUESHEET commands from treating sidecar structure as an embedded
    /// tag.
    pub embedded_cuesheet_present: bool,
    /// True when the visible CUESHEET row is a read-only sidecar shadow used to
    /// shape a split-CUE presentation. It must not be treated as an embedded tag
    /// creation/update by the ordinary lofty save diff.
    pub sidecar_cuesheet_shadow_present: bool,
    /// Save-path tombstone for deleting an embedded CUESHEET while leaving any
    /// sidecar-derived synthetic row visible in the editor.
    pub pending_embedded_cuesheet_delete: bool,
    /// Stable policy-selected CUE authority and save target for this surface.
    /// A sidecar authority may render a structurally-matched embedded metadata
    /// upgrade, but save-time routing still follows this retained identity
    /// instead of rediscovering or guessing another source.
    pub cue_source: Option<MetadataCueSource>,
    /// Unified multi-surface CUE state. Used by grouped sidecar albums and by
    /// selected sets of independent embedded-CUE carriers.
    pub cue_album_synthetic_sheet: Option<CueAlbumSyntheticSheet>,
    /// Regenerate one CUESHEET per member image rather than projecting one
    /// unified physical sheet to every path. Used only for selected sets of
    /// independent embedded-CUE carriers.
    pub per_carrier_embedded_cuesheets: bool,
    /// File-indexed managed whole-file track-scoped tags observed on load that
    /// must be deleted during the next successful unified-album save. This is a
    /// migration/cleanup plan for F2-era polluted files, not an unconditional
    /// per-save tombstone list.
    pub cue_album_forced_cleanup: Vec<(usize, lofty::tag::ItemKey)>,
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
            selected_rows: std::collections::BTreeSet::new(),
            dirty: false,
            refresh_failed: false,
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
            embedded_cuesheet_present: false,
            sidecar_cuesheet_shadow_present: false,
            pending_embedded_cuesheet_delete: false,
            cue_source: None,
            cue_album_synthetic_sheet: None,
            per_carrier_embedded_cuesheets: false,
            cue_album_forced_cleanup: Vec::new(),
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
        tab.selected_rows = active.selected_rows.clone();
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
        tab.embedded_cuesheet_present = active.embedded_cuesheet_present;
        tab.sidecar_cuesheet_shadow_present = active.sidecar_cuesheet_shadow_present;
        tab.pending_embedded_cuesheet_delete = active.pending_embedded_cuesheet_delete;
        tab.cue_source = active.cue_source.clone();
        tab.cue_album_synthetic_sheet = active.cue_album_synthetic_sheet.clone();
        tab.per_carrier_embedded_cuesheets = active.per_carrier_embedded_cuesheets;
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
/// - Rendering never performs broad filesystem/tag/media-probe/save I/O; the Artwork tab
///   may lazily read and cache only the selected embedded picture for previews.
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
    pub metadata_view: MetadataEditorView,
    pub maximized: bool,
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
    /// One MusicBrainz lookup may own an editor session at a time.
    pub tags_mb_in_flight: bool,
    /// Editor-wide mutation generation; every surface save increments it.
    pub editor_save_generation: u64,
    /// Session-guarded worker progress rendered in the Saving footer.
    pub metadata_save_progress: Option<String>,
    pub replaygain_cursor: usize,
    pub replaygain_scan_generation: u64,
    pub replaygain_scan: Option<MetadataReplayGainScanState>,
    pub details_analysis_generation: u64,
    pub details_analysis: Option<MetadataDetailsAnalysisState>,
    pub artwork_cursor: usize,
    pub artwork_preview_generation: usize,
    pub artwork_preview_cache: Option<ArtworkPreviewCache>,
    pub artwork_write_generation: u64,
    pub artwork_write: Option<MetadataArtworkWriteState>,
    pub file_picker: Option<MetadataFilePickerState>,
    /// Monotonic ownership token for asynchronous editor tag-interchange
    /// preparation. Only the most recently accepted file-import or transfer
    /// request may reduce its completion into the editor.
    pub tag_transfer_prepare_generation: u64,
    /// Cooperative cancellation for the currently owned file-import or
    /// source/target preparation worker. This is intentionally one slot: a
    /// newer request supersedes the older one rather than allowing parallel
    /// preparation work.
    pub tag_transfer_prepare_cancel: Option<crate::tui::probe::MetadataWriteCancelFlag>,
    pub pending_artwork_type: Option<lofty::picture::PictureType>,
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
            metadata_view: MetadataEditorView::Canonical,
            maximized: false,
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
            tags_mb_in_flight: false,
            editor_save_generation: 0,
            metadata_save_progress: None,
            replaygain_cursor: 0,
            replaygain_scan_generation: 0,
            replaygain_scan: None,
            details_analysis_generation: 0,
            details_analysis: None,
            artwork_cursor: 0,
            artwork_preview_generation: 0,
            artwork_preview_cache: None,
            artwork_write_generation: 0,
            artwork_write: None,
            file_picker: None,
            tag_transfer_prepare_generation: 0,
            tag_transfer_prepare_cancel: None,
            pending_artwork_type: None,
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

    pub fn metadata_entry_is_visible(&self, index: usize) -> bool {
        let Some(entry) = self.active_surface().entries.get(index) else {
            return false;
        };
        self.metadata_view == MetadataEditorView::All
            || crate::tui::probe::STANDARD_KEY_ORDER.iter().any(|known| {
                *known == crate::tui::probe::canonical_metadata_display_key(&entry.display_key)
            })
    }

    pub fn visible_metadata_entry_indices(&self) -> Vec<usize> {
        (0..self.active_surface().entries.len())
            .filter(|index| self.metadata_entry_is_visible(*index))
            .collect()
    }

    pub fn visible_metadata_rows(&self) -> Vec<usize> {
        let mut rows = self.visible_metadata_entry_indices();
        rows.push(self.active_surface().entries.len());
        rows
    }

    pub fn set_metadata_view(&mut self, view: MetadataEditorView) {
        if self.metadata_view == view {
            return;
        }
        self.metadata_view = view;
        if view == MetadataEditorView::All {
            self.maximized = true;
        }
        let rows = self.visible_metadata_rows();
        if !rows.contains(&self.cursor) {
            self.cursor = rows.first().copied().unwrap_or(0);
        }
        self.scroll = 0;
    }

    pub fn toggle_metadata_view(&mut self) {
        let next = match self.metadata_view {
            MetadataEditorView::Canonical => MetadataEditorView::All,
            MetadataEditorView::All => MetadataEditorView::Canonical,
        };
        self.set_metadata_view(next);
    }

    pub fn toggle_metadata_editor_maximized(&mut self) {
        self.maximized = !self.maximized;
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
/// - rendering never performs broad filesystem/tag/media-probe/save I/O; the Artwork tab
///   may lazily read and cache only the selected embedded picture for previews.
/// - read-only tab scroll values are clamped before storage.
#[derive(Debug, Clone)]
pub struct MetadataEditorState {
    /// True when the presentation tabs are single-image CUE surfaces (one
    /// tab per cue/image pair of an album). Split-cue MB population slices
    /// the release tracklist ACROSS tabs, which is only correct for cue
    /// surfaces — disc editors (SACD areas, DVD-A groups) repeat the same
    /// tracks per tab and must keep the apply-to-matching-presentations
    /// flow instead.
    pub cue_surface_tabs: bool,

    /// Archive-edit ownership context when the editor is working against
    /// extracted archive staging instead of ordinary source files.
    pub archive_edit_context: Option<ArchiveMetadataEditContext>,

    /// True after an operation has already written to archive staging files.
    /// Browse-owned archive editors must repackage when this is set even if
    /// the ordinary tag grid has no unsaved edits left. Artwork writes and
    /// ReplayGain scans write through immediately, so the dirty bit is the
    /// durable close-time signal that prevents cleanup from discarding those
    /// staged edits.
    pub archive_staging_dirty: bool,

    /// True when a successful explicit save should close the overlay. The
    /// Apply command deliberately sets this false so users can save from any
    /// tab without losing editor context.
    pub close_after_successful_save: bool,

    /// Cooperative cancellation flag for the active tag-grid save or invalid-APE
    /// repair, if any. Both operations use the same worker completion protocol
    /// and safe cancellation points.
    pub metadata_write_cancel: Option<crate::tui::probe::MetadataWriteCancelFlag>,

    /// Identity and frozen target snapshot for an invalid-APE repair currently
    /// running through the metadata-write worker protocol. The snapshot is kept
    /// in editor state so ordinary save completions cannot be mistaken for repair
    /// completions, and stale worker messages cannot mutate a reopened editor.
    pub invalid_ape_repair: Option<MetadataInvalidApeRepairOperation>,

    /// Cooperative cancellation flag for the active artwork write/remove, if any.
    pub artwork_write_cancel: Option<crate::tui::probe::MetadataWriteCancelFlag>,

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MusicBrainzPresentationApplyResult {
    pub changed_presentations: usize,
    pub mutation_report: crate::tui::probe::MetadataMutationReport,
}

fn read_only_max_scroll(total_lines: usize, visible_rows: usize) -> usize {
    total_lines.saturating_sub(visible_rows.max(1))
}

impl MetadataEditorState {
    pub fn from_model(model: MetadataEditorModel) -> Self {
        Self {
            cue_surface_tabs: false,
            archive_edit_context: None,
            archive_staging_dirty: false,
            close_after_successful_save: true,
            metadata_write_cancel: None,
            invalid_ape_repair: None,
            artwork_write_cancel: None,
            model,
        }
    }

    pub fn mark_archive_staging_dirty(&mut self) {
        if self.archive_edit_context.is_some() {
            self.archive_staging_dirty = true;
        }
    }

    pub fn has_browse_archive_staged_changes(&self) -> bool {
        self.archive_staging_dirty
            && self
                .archive_edit_context
                .as_ref()
                .is_some_and(|context| context.owner == ArchiveMetadataEditOwner::Browse)
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

    /// Supersede any older editor tag-interchange preparation and return the
    /// ownership token and cancellation flag for the new worker.
    pub fn begin_tag_transfer_preparation(
        &mut self,
    ) -> (u64, crate::tui::probe::MetadataWriteCancelFlag) {
        if let Some(cancel) = self.tag_transfer_prepare_cancel.take() {
            cancel.cancel();
        }
        let request_id = self
            .tag_transfer_prepare_generation
            .checked_add(1)
            .unwrap_or(1);
        self.tag_transfer_prepare_generation = request_id;
        let cancel = crate::tui::probe::MetadataWriteCancelFlag::new();
        self.tag_transfer_prepare_cancel = Some(cancel.clone());
        (request_id, cancel)
    }

    /// Cancel and invalidate any pending tag-interchange preparation.
    ///
    /// This is called when the editor begins a competing close workflow. The
    /// generation advance makes every already-enqueued completion stale, while
    /// cancelling the flag stops bounded file reads and directory/metadata
    /// preparation at their existing cooperative checkpoints.
    pub fn invalidate_tag_interchange_preparation(&mut self) {
        if let Some(cancel) = self.tag_transfer_prepare_cancel.take() {
            cancel.cancel();
        }
        self.tag_transfer_prepare_generation = self
            .tag_transfer_prepare_generation
            .checked_add(1)
            .unwrap_or(1);
    }

    pub fn owns_tag_transfer_preparation(&self, request_id: u64) -> bool {
        self.tag_transfer_prepare_generation == request_id
            && self.tag_transfer_prepare_cancel.is_some()
    }

    /// Consume the preparation slot only when the completion still owns it.
    /// A consumed request cannot be reduced twice, so duplicate channel
    /// delivery is harmless.
    pub fn take_tag_transfer_preparation(&mut self, request_id: u64) -> bool {
        if !self.owns_tag_transfer_preparation(request_id) {
            return false;
        }
        self.tag_transfer_prepare_cancel = None;
        true
    }

    pub fn apply_analysis_result(&mut self, result: &crate::tui::analyze::AnalysisResult) -> bool {
        let mut changed = false;
        for file in &mut self.active_surface_mut().technical_details.files {
            if file.file_facts.path == result.path {
                file.set_analysis_result(result);
                changed = true;
            }
        }
        changed
    }

    pub fn apply_preemphasis_result(
        &mut self,
        result: &crate::tui::preemphasis::PreemphasisResult,
    ) -> bool {
        let mut changed = false;
        for file in &mut self.active_surface_mut().technical_details.files {
            if file.file_facts.path == result.path {
                file.set_preemphasis_result(result);
                changed = true;
            }
        }
        changed
    }

    pub fn begin_replaygain_scan(
        &mut self,
        mode: MetadataReplayGainScanMode,
        file_count: usize,
    ) -> (u64, u64) {
        let session_id = self.active_surface().technical_details.session_id;
        let generation = self.replaygain_scan_generation.saturating_add(1);
        self.replaygain_scan_generation = generation;
        self.replaygain_scan = Some(MetadataReplayGainScanState {
            session_id,
            generation,
            mode,
            file_count,
        });
        (session_id, generation)
    }

    pub fn complete_replaygain_scan(&mut self, session_id: u64, generation: u64) -> bool {
        if self
            .replaygain_scan
            .as_ref()
            .map(|scan| scan.session_id == session_id && scan.generation == generation)
            .unwrap_or(false)
        {
            self.replaygain_scan = None;
            true
        } else {
            false
        }
    }

    pub fn begin_details_analysis(&mut self, file_count: usize) -> (u64, u64) {
        let session_id = self.active_surface().technical_details.session_id;
        let generation = self.details_analysis_generation.saturating_add(1);
        self.details_analysis_generation = generation;
        self.details_analysis = Some(MetadataDetailsAnalysisState {
            session_id,
            generation,
            file_count,
        });
        (session_id, generation)
    }

    pub fn complete_details_analysis(&mut self, session_id: u64, generation: u64) -> bool {
        if self
            .details_analysis
            .as_ref()
            .map(|scan| scan.session_id == session_id && scan.generation == generation)
            .unwrap_or(false)
        {
            self.details_analysis = None;
            true
        } else {
            false
        }
    }

    pub fn begin_artwork_write(
        &mut self,
        mode: MetadataArtworkWriteMode,
        file_count: usize,
    ) -> (u64, u64) {
        let session_id = self.active_surface().technical_details.session_id;
        let generation = self.artwork_write_generation.saturating_add(1);
        self.artwork_write_generation = generation;
        self.artwork_write = Some(MetadataArtworkWriteState {
            session_id,
            generation,
            mode,
            file_count,
        });
        (session_id, generation)
    }

    pub fn begin_cancellable_artwork_write(
        &mut self,
        mode: MetadataArtworkWriteMode,
        file_count: usize,
    ) -> (u64, u64, crate::tui::probe::MetadataWriteCancelFlag) {
        let (session_id, generation) = self.begin_artwork_write(mode, file_count);
        let cancel = crate::tui::probe::MetadataWriteCancelFlag::new();
        self.artwork_write_cancel = Some(cancel.clone());
        (session_id, generation, cancel)
    }

    pub fn cancel_artwork_write(&self) -> bool {
        if let Some(cancel) = &self.artwork_write_cancel {
            cancel.cancel();
            true
        } else {
            false
        }
    }

    pub fn complete_artwork_write(&mut self, session_id: u64, generation: u64) -> bool {
        if self
            .artwork_write
            .as_ref()
            .map(|write| write.session_id == session_id && write.generation == generation)
            .unwrap_or(false)
        {
            self.artwork_write = None;
            self.artwork_write_cancel = None;
            true
        } else {
            false
        }
    }

    pub fn surface_mut_for_session(&mut self, session_id: u64) -> Option<&mut PresentationTab> {
        if self.model.presentation_tabs.is_empty() {
            if self.model.file_surface.technical_details.session_id == session_id {
                Some(&mut self.model.file_surface)
            } else {
                None
            }
        } else {
            self.model
                .presentation_tabs
                .iter_mut()
                .find(|tab| tab.technical_details.session_id == session_id)
        }
    }

    pub fn move_replaygain_cursor(&mut self, delta: isize) -> bool {
        let len = crate::tui::metadata_view_models::replaygain_action_row_count(self);
        if len == 0 {
            self.replaygain_cursor = 0;
            return false;
        }
        let old = self.replaygain_cursor.min(len - 1);
        let step = delta.checked_abs().unwrap_or(isize::MAX) as usize;
        let next = if delta < 0 {
            old.saturating_sub(step)
        } else {
            old.saturating_add(step).min(len - 1)
        };
        self.replaygain_cursor = next;
        old != next
    }

    pub fn invalidate_artwork_preview_cache(&mut self) {
        self.artwork_preview_generation = self.artwork_preview_generation.saturating_add(1);
        self.artwork_preview_cache = None;
    }


    pub fn request_artwork_preview_load(
        &mut self,
        path: std::path::PathBuf,
        picture_type: lofty::picture::PictureType,
    ) {
        if self
            .artwork_preview_cache
            .as_ref()
            .map(|cache| {
                cache.path == path
                    && cache.picture_type == picture_type
                    && (cache.receiver.is_some()
                        || cache.decoded_image.is_some()
                        || cache.error.is_some())
            })
            .unwrap_or(false)
        {
            return;
        }

        let generation = self.artwork_preview_generation.saturating_add(1);
        self.artwork_preview_generation = generation;
        let (tx, rx) = mpsc::channel();
        self.artwork_preview_cache = Some(ArtworkPreviewCache {
            path: path.clone(),
            picture_type: picture_type.clone(),
            desired_preview_area: ratatui::layout::Rect::default(),
            encoded_preview_area: ratatui::layout::Rect::default(),
            desired_protocol_generation: 0,
            encoded_protocol_generation: 0,
            encoded_retransmit_generation: 0,
            generation,
            decoded_generation: None,
            decoded_image: None,
            receiver: Some(rx),
            image_protocol: None,
            error: None,
        });

        thread::spawn(move || {
            let result = crate::tui::probe::read_embedded_picture_bytes(&path, picture_type.clone())
                .and_then(|bytes| image::load_from_memory(&bytes).map_err(|e| format!("Failed to decode artwork: {e}")));
            let _ = tx.send(ArtworkPreviewLoadResult {
                path,
                picture_type,
                generation,
                result,
            });
        });
    }

    pub fn poll_artwork_preview_load(&mut self) {
        let Some(cache) = self.artwork_preview_cache.as_mut() else {
            return;
        };
        let Some(rx) = cache.receiver.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                if cache.path == result.path
                    && cache.picture_type == result.picture_type
                    && cache.generation == result.generation
                {
                    cache.image_protocol = None;
                    cache.encoded_preview_area = ratatui::layout::Rect::default();
                    cache.encoded_protocol_generation = 0;
                    cache.encoded_retransmit_generation = 0;
                    match result.result {
                        Ok(image) => {
                            cache.decoded_generation = Some(result.generation);
                            cache.decoded_image = Some(image);
                            cache.error = None;
                        }
                        Err(error) => {
                            cache.decoded_generation = Some(result.generation);
                            cache.decoded_image = None;
                            cache.error = Some(error);
                        }
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                cache.receiver = Some(rx);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                cache.error = Some("Artwork preview worker exited before completing".to_string());
            }
        }
    }


    pub fn prepare_artwork_preview_protocol(
        &mut self,
        image_picker: &mut ratatui_image::picker::Picker,
        protocol_generation: usize,
        retransmit_generation: usize,
    ) -> bool {
        let had_receiver = self
            .artwork_preview_cache
            .as_ref()
            .map(|cache| cache.receiver.is_some())
            .unwrap_or(false);
        self.poll_artwork_preview_load();
        let decode_completed = had_receiver
            && self
                .artwork_preview_cache
                .as_ref()
                .map(|cache| cache.receiver.is_none())
                .unwrap_or(false);

        let Some(cache) = self.artwork_preview_cache.as_mut() else {
            return decode_completed;
        };
        let Some(decoded) = cache.decoded_image.as_ref() else {
            return decode_completed;
        };
        let desired_area = cache.desired_preview_area;
        if desired_area.width == 0 || desired_area.height == 0 {
            return decode_completed;
        }
        let desired_retransmit_generation = if image_picker.protocol_type
            == ratatui_image::picker::ProtocolType::Kitty
        {
            retransmit_generation
        } else {
            0
        };
        cache.desired_protocol_generation = protocol_generation;
        if cache.image_protocol.is_some()
            && cache.encoded_protocol_generation == protocol_generation
            && cache.encoded_retransmit_generation == desired_retransmit_generation
            && cache.encoded_preview_area == desired_area
        {
            return decode_completed;
        }

        cache.image_protocol = Some(image_picker.new_resize_protocol(decoded.clone()));
        cache.encoded_protocol_generation = protocol_generation;
        cache.encoded_retransmit_generation = desired_retransmit_generation;
        cache.encoded_preview_area = desired_area;
        cache.error = None;
        true
    }

    pub fn set_artwork_cursor(&mut self, next: usize) -> bool {
        let len = crate::tui::metadata_view_models::artwork_action_row_count(self);
        if len == 0 {
            let changed = self.artwork_cursor != 0 || self.artwork_preview_cache.is_some();
            self.artwork_cursor = 0;
            if self.artwork_preview_cache.is_some() {
                self.invalidate_artwork_preview_cache();
            }
            return changed;
        }
        let next = next.min(len - 1);
        let changed = self.artwork_cursor != next;
        self.artwork_cursor = next;
        if changed {
            self.invalidate_artwork_preview_cache();
        }
        changed
    }

    pub fn move_artwork_cursor(&mut self, delta: isize) -> bool {
        let len = crate::tui::metadata_view_models::artwork_action_row_count(self);
        if len == 0 {
            let changed = self.artwork_cursor != 0 || self.artwork_preview_cache.is_some();
            self.artwork_cursor = 0;
            if self.artwork_preview_cache.is_some() {
                self.invalidate_artwork_preview_cache();
            }
            return changed;
        }
        let old = self.artwork_cursor.min(len - 1);
        let step = delta.checked_abs().unwrap_or(isize::MAX) as usize;
        let next = if delta < 0 {
            old.saturating_sub(step)
        } else {
            old.saturating_add(step).min(len - 1)
        };
        self.artwork_cursor = next;
        let changed = old != next;
        if changed {
            self.invalidate_artwork_preview_cache();
        }
        changed
    }

    /// Recompute dirty state for the active surface from authoritative row data.
    pub fn recompute_active_dirty(&mut self) -> bool {
        let dirty = presentation_tab_has_changes(self.active_surface());
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
        self.invalidate_artwork_preview_cache();
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
        self.invalidate_artwork_preview_cache();
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
        self.file_picker = None;
        self.pending_artwork_type = None;
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
        self.invalidate_artwork_preview_cache();
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

    pub fn apply_active_musicbrainz_values_to_matching_presentations(
        &mut self,
    ) -> MusicBrainzPresentationApplyResult {
        let mut result = MusicBrainzPresentationApplyResult::default();
        if self.presentation_tabs.len() <= 1 {
            return result;
        }
        let Some(active) = self.presentation_tabs.get(self.active_tab).cloned() else {
            return result;
        };
        let track_count = active.paths.len();
        let active_tab = self.active_tab;
        for (idx, tab) in self.presentation_tabs.iter_mut().enumerate() {
            if idx == active_tab || tab.paths.len() != track_count {
                continue;
            }
            let before_entries = tab.entries.clone();
            let copied = copy_musicbrainz_entries_preserving_originals(
                &active.entries,
                &mut tab.entries,
                track_count,
            );
            if copied == 0 {
                continue;
            }
            result.mutation_report.merge(
                crate::tui::probe::MetadataMutationReport::between(
                    &before_entries,
                    &tab.entries,
                ),
            );
            tab.deleted.clear();
            tab.dirty = true;
            result.changed_presentations += 1;
        }
        result
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
        self.model.editor_save_generation = self.model.editor_save_generation.saturating_add(1);
        self.model.active_surface_mut().technical_details.begin_write()
    }

    pub fn begin_cancellable_write(&mut self) -> (u64, u64, crate::tui::probe::MetadataWriteCancelFlag) {
        self.model.metadata_save_progress = None;
        let (session_id, generation) = self.begin_write();
        let cancel = crate::tui::probe::MetadataWriteCancelFlag::new();
        self.metadata_write_cancel = Some(cancel.clone());
        (session_id, generation, cancel)
    }

    pub fn begin_invalid_ape_repair(
        &mut self,
        targets: Vec<(std::path::PathBuf, Vec<String>)>,
    ) -> (u64, u64, crate::tui::probe::MetadataWriteCancelFlag) {
        self.phase = MetadataEditorPhase::Saving;
        let (session_id, generation, cancel) = self.begin_cancellable_write();
        self.invalid_ape_repair = Some(MetadataInvalidApeRepairOperation {
            session_id,
            generation,
            targets,
        });
        (session_id, generation, cancel)
    }

    pub fn invalid_ape_repair_is_current(&self, session_id: u64, generation: u64) -> bool {
        self.invalid_ape_repair.as_ref().is_some_and(|operation| {
            operation.session_id == session_id && operation.generation == generation
        })
    }

    pub fn finish_invalid_ape_repair(
        &mut self,
        session_id: u64,
        generation: u64,
    ) -> Option<MetadataInvalidApeRepairOperation> {
        if !self.invalid_ape_repair_is_current(session_id, generation) {
            return None;
        }
        let operation = self.invalid_ape_repair.take();
        self.clear_metadata_write_cancel();
        self.model.metadata_save_progress = None;
        self.phase = MetadataEditorPhase::Editing;
        if self.active_surface().technical_details.active_save_generation == Some(generation) {
            self.active_surface_mut().technical_details.active_save_generation = None;
        }
        operation
    }

    pub fn apply_metadata_save_progress(
        &mut self,
        session_id: u64,
        save_generation: u64,
        detail: String,
    ) -> bool {
        if self.phase != MetadataEditorPhase::Saving
            || self.model.editor_save_generation != save_generation
            || self.active_surface().technical_details.session_id != session_id
        {
            return false;
        }
        self.model.metadata_save_progress = Some(detail);
        true
    }

    pub fn cancel_metadata_write(&self) -> bool {
        if let Some(cancel) = &self.metadata_write_cancel {
            cancel.cancel();
            true
        } else {
            false
        }
    }

    pub fn clear_metadata_write_cancel(&mut self) {
        self.metadata_write_cancel = None;
    }

    /// Reduce save results into the matching editor surface.
    ///
    /// Invariant: async write completions must match the active editor session
    /// and save generation before applying; stale sessions/generations cannot
    /// close or mutate another editor. Save reduction updates model state only
    /// for files that actually saved.
    pub fn replace_saved_surface_entries(
        &mut self,
        session_id: u64,
        entries: Vec<crate::tui::probe::TagEntry>,
    ) -> bool {
        let entries_len = {
            let tab = if self.presentation_tabs.is_empty() {
                (self.model.file_surface.technical_details.session_id == session_id)
                    .then_some(&mut self.model.file_surface)
            } else {
                self.presentation_tabs
                    .iter_mut()
                    .find(|tab| tab.technical_details.session_id == session_id)
            };
            let Some(tab) = tab else {
                return false;
            };
            tab.entries = entries;
            tab.deleted.clear();
            tab.selected_rows.clear();
            tab.dirty = false;
            tab.refresh_failed = false;
            tab.embedded_cuesheet_present = false;
            tab.sidecar_cuesheet_shadow_present = false;
            tab.pending_embedded_cuesheet_delete = false;
            tab.cue_album_synthetic_sheet = None;
            tab.cue_album_forced_cleanup.clear();
            tab.entries.len()
        };
        self.cursor = self.cursor.min(entries_len.saturating_sub(1));
        true
    }

    /// Keep the exact saved surface visibly unresolved when the mandatory
    /// post-save re-read fails. The carrier write may have succeeded, but the
    /// editor must not present its pre-save projection as a clean reflection
    /// of disk state.
    pub fn mark_saved_surface_refresh_failed(&mut self, session_id: u64) -> bool {
        let tab = if self.presentation_tabs.is_empty() {
            (self.model.file_surface.technical_details.session_id == session_id)
                .then_some(&mut self.model.file_surface)
        } else {
            self.presentation_tabs
                .iter_mut()
                .find(|tab| tab.technical_details.session_id == session_id)
        };
        let Some(tab) = tab else {
            return false;
        };
        tab.refresh_failed = true;
        tab.dirty = true;
        true
    }

    pub fn apply_write_results(
        &mut self,
        session_id: u64,
        save_generation: u64,
        results: Vec<MetadataEditorWriteResult>,
    ) -> Option<MetadataEditorWriteSummary> {
        let summary = if self.presentation_tabs.is_empty() {
            if self.model.file_surface.technical_details.session_id != session_id {
                return None;
            }
            apply_write_results_to_tab(
                &mut self.model.file_surface,
                save_generation,
                results,
            )
        } else {
            let idx = self
                .presentation_tabs
                .iter()
                .position(|tab| tab.technical_details.session_id == session_id)?;
            apply_write_results_to_tab(&mut self.presentation_tabs[idx], save_generation, results)
        };
        if summary.is_some() {
            self.clear_metadata_write_cancel();
            self.model.metadata_save_progress = None;
        }
        summary
    }
}

fn mark_presentation_tab_saved(tab: &mut PresentationTab) {
    tab.dirty = false;
    tab.pending_embedded_cuesheet_delete = false;
    for entry in &mut tab.entries {
        mark_tag_entry_saved(entry);
    }
    tab.deleted.clear();
}

fn scalar_stored_value_count(value: &str) -> usize {
    if value.trim().is_empty() {
        0
    } else {
        1
    }
}

fn refresh_stored_value_summary(entry: &mut crate::tui::probe::TagEntry) {
    if !entry.per_file_stored_value_counts.is_empty() {
        entry.has_multiple_stored_values = entry
            .per_file_stored_value_counts
            .iter()
            .any(|count| *count > 1);
    }
}

fn mark_tag_entry_saved(entry: &mut crate::tui::probe::TagEntry) {
    if entry.per_file_stored_value_counts.len() == entry.per_file_values.len()
        && entry.per_file_originals.len() == entry.per_file_values.len()
    {
        for idx in 0..entry.per_file_values.len() {
            if entry.per_file_values[idx] != entry.per_file_originals[idx] {
                entry.per_file_stored_value_counts[idx] =
                    scalar_stored_value_count(&entry.per_file_values[idx]);
            }
        }
        refresh_stored_value_summary(entry);
    }
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

    for MetadataEditorWriteResult { path, outcome } in results {
        match outcome {
            MetadataEditorWriteOutcome::SidecarCueSaved {
                cue_path: _,
                unchanged,
                rewritten_as_utf8,
            } => {
                if unchanged {
                    summary.sidecar_cue_unchanged = summary.sidecar_cue_unchanged.saturating_add(1);
                } else {
                    summary.sidecar_cue_saved = summary.sidecar_cue_saved.saturating_add(1);
                    if rewritten_as_utf8 {
                        summary.sidecar_cue_utf8_fallback = summary
                            .sidecar_cue_utf8_fallback
                            .saturating_add(1);
                    }
                }
                mark_sidecar_cue_writeback_saved(tab);
            }
            MetadataEditorWriteOutcome::SidecarCueFailed { cue_path, reason } => {
                summary.sidecar_cue_failed = summary.sidecar_cue_failed.saturating_add(1);
                if summary.first_problem.is_none() {
                    summary.first_problem = Some(format!("{}: {}", cue_path.display(), reason));
                }
                if let Some(&idx) = path_to_index.get(&path) {
                    attach_write_issue(tab, idx, MetadataIssue::Write {
                        path: cue_path,
                        reason,
                    });
                }
            }
            MetadataEditorWriteOutcome::SavedWithWarnings { warnings } => {
                let Some(&idx) = path_to_index.get(&path) else {
                    summary.ignored = summary.ignored.saturating_add(1);
                    if summary.first_problem.is_none() {
                        summary.first_problem = Some(format!(
                            "ignored stale save result for '{}'",
                            path.display()
                        ));
                    }
                    continue;
                };
                summary.saved = summary.saved.saturating_add(1);
                summary.saved_paths.push(path.clone());
                saved_slots.insert(idx);
                if let Some(file) = tab.technical_details.files.get_mut(idx) {
                    file.issues.retain(|issue| !matches!(issue, MetadataIssue::Write { .. }));
                }
                summary.durability_warnings = summary
                    .durability_warnings
                    .saturating_add(warnings.len());
                if summary.first_durability_warning.is_none() {
                    summary.first_durability_warning = warnings
                        .iter()
                        .find(|warning| !warning.trim().is_empty())
                        .cloned();
                }
            }
            MetadataEditorWriteOutcome::Saved => {
                let Some(&idx) = path_to_index.get(&path) else {
                    summary.ignored = summary.ignored.saturating_add(1);
                    if summary.first_problem.is_none() {
                        summary.first_problem = Some(format!(
                            "ignored stale save result for '{}'",
                            path.display()
                        ));
                    }
                    continue;
                };
                summary.saved = summary.saved.saturating_add(1);
                summary.saved_paths.push(path.clone());
                saved_slots.insert(idx);
                if let Some(file) = tab.technical_details.files.get_mut(idx) {
                    file.issues.retain(|issue| !matches!(issue, MetadataIssue::Write { .. }));
                }
            }
            MetadataEditorWriteOutcome::Failed { reason } => {
                let Some(&idx) = path_to_index.get(&path) else {
                    summary.ignored = summary.ignored.saturating_add(1);
                    if summary.first_problem.is_none() {
                        summary.first_problem = Some(format!(
                            "ignored stale save result for '{}'",
                            path.display()
                        ));
                    }
                    continue;
                };
                summary.failed = summary.failed.saturating_add(1);
                if summary.first_problem.is_none() {
                    summary.first_problem = Some(reason.clone());
                }
                attach_write_issue(tab, idx, MetadataIssue::Write { path, reason });
            }
            MetadataEditorWriteOutcome::Skipped { reason } => {
                let Some(&idx) = path_to_index.get(&path) else {
                    summary.ignored = summary.ignored.saturating_add(1);
                    if summary.first_problem.is_none() {
                        summary.first_problem = Some(format!(
                            "ignored stale save result for '{}'",
                            path.display()
                        ));
                    }
                    continue;
                };
                summary.skipped = summary.skipped.saturating_add(1);
                if summary.first_problem.is_none() {
                    summary.first_problem = Some(reason.clone());
                }
                attach_write_issue(tab, idx, MetadataIssue::SaveBlocked { path, reason });
            }
            MetadataEditorWriteOutcome::InvalidApeRepair(_) => {
                // Repair completions are reduced by the operation-specific
                // event-loop path. Reaching the ordinary save reducer means the
                // operation identity is stale or missing; never mutate editor
                // rows from such a result.
                summary.ignored = summary.ignored.saturating_add(1);
                if summary.first_problem.is_none() {
                    summary.first_problem = Some(format!(
                        "ignored stale invalid-APE repair result for '{}'",
                        path.display()
                    ));
                }
            }
        }
    }

    reduce_saved_slots(tab, &saved_slots);
    if tab.pending_embedded_cuesheet_delete
        && pending_embedded_cuesheet_delete_fully_saved(tab, &saved_slots)
    {
        tab.pending_embedded_cuesheet_delete = false;
        tab.embedded_cuesheet_present = false;
    }
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

fn native_multi_file_sidecar_authority(tab: &PresentationTab) -> bool {
    tab.paths.len() > 1
        && tab.cue_album_synthetic_sheet.is_some()
        && matches!(&tab.cue_source, Some(MetadataCueSource::Sidecar(_)))
}

fn cue_sidecar_representable_entry_for_path_count(
    path_count: usize,
    entry: &crate::tui::probe::TagEntry,
) -> bool {
    let key = entry.display_key.to_ascii_uppercase();
    if key == "CUESHEET" {
        return true;
    }
    if entry.is_track_scoped(path_count) {
        return matches!(key.as_str(), "TITLE" | "ARTIST" | "ISRC");
    }
    matches!(
        key.as_str(),
        "ALBUM" | "ALBUMARTIST" | "DATE" | "GENRE" | "CATALOGNUMBER"
    )
}

fn cue_sidecar_representable_entry(
    tab: &PresentationTab,
    entry: &crate::tui::probe::TagEntry,
) -> bool {
    cue_sidecar_representable_entry_for_path_count(tab.paths.len(), entry)
}

fn cue_sidecar_standard_owned_entry(
    path_count: usize,
    entry: &crate::tui::probe::TagEntry,
) -> bool {
    let key = entry.display_key.to_ascii_uppercase();
    if entry.is_track_scoped(path_count) {
        matches!(key.as_str(), "TITLE" | "ARTIST" | "ISRC")
    } else {
        matches!(
            key.as_str(),
            "ALBUM" | "ALBUMARTIST" | "DATE" | "GENRE" | "CATALOGNUMBER"
        )
    }
}

fn mark_tag_entry_saved_empty(entry: &mut crate::tui::probe::TagEntry) {
    for value in &mut entry.per_file_values {
        value.clear();
    }
    entry.per_file_originals = entry.per_file_values.clone();
    entry.value.clear();
    entry.original.clear();
    entry.is_mixed = false;
    entry.has_multiple_stored_values = false;
    entry.per_file_stored_value_counts.clear();
    entry.mb_proposed_value = None;
    entry.mb_proposed_per_file = None;
}

fn mark_sidecar_cue_writeback_saved(tab: &mut PresentationTab) {
    let path_count = tab.paths.len();
    if !native_multi_file_sidecar_authority(tab) {
        for entry in &mut tab.entries {
            let is_cuesheet_shadow = tab.sidecar_cuesheet_shadow_present
                && entry.display_key.eq_ignore_ascii_case("CUESHEET");
            let is_sidecar_track_row = (entry.display_key.eq_ignore_ascii_case("TITLE")
                || entry.display_key.eq_ignore_ascii_case("ARTIST")
                || entry.display_key.eq_ignore_ascii_case("ISRC"))
                && entry.is_track_scoped(path_count);
            if is_cuesheet_shadow || is_sidecar_track_row {
                mark_tag_entry_saved(entry);
            }
        }
        return;
    }

    // A native multi-FILE sidecar-authoritative save deliberately produces no
    // image results. Consume exactly the CUE-representable edits here after the
    // sidecar write succeeds; unsupported changes are rejected before I/O.
    let deleted = tab
        .deleted
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let consumed_deletions = deleted
        .iter()
        .copied()
        .filter(|index| {
            tab.entries
                .get(*index)
                .is_some_and(|entry| cue_sidecar_standard_owned_entry(path_count, entry))
        })
        .collect::<std::collections::BTreeSet<_>>();
    for (index, entry) in tab.entries.iter_mut().enumerate() {
        if !cue_sidecar_representable_entry_for_path_count(path_count, entry) {
            continue;
        }
        if consumed_deletions.contains(&index) {
            // Standard sidecar-owned rows remain present as an editable empty
            // surface after deletion. This allows users to repopulate an
            // initially absent or just-deleted CUE field without reopening or
            // relying on carrier-image writability.
            mark_tag_entry_saved_empty(entry);
        } else if !deleted.contains(&index) {
            mark_tag_entry_saved(entry);
        }
    }
    tab.deleted = tab
        .deleted
        .iter()
        .copied()
        .filter(|index| !consumed_deletions.contains(index))
        .collect();
    tab.cue_album_forced_cleanup.clear();
}

fn pending_embedded_cuesheet_delete_fully_saved(
    tab: &PresentationTab,
    saved_slots: &std::collections::BTreeSet<usize>,
) -> bool {
    let path_count = tab.paths.len();
    path_count > 0 && (0..path_count).all(|idx| saved_slots.contains(&idx))
}

fn unified_cue_album_per_track_key_is_persistable_for_dirty_clear(display_key: &str) -> bool {
    matches!(
        display_key.to_ascii_uppercase().as_str(),
        "TITLE" | "ARTIST" | "ISRC"
    )
}

fn unified_cue_album_fully_saved_for_dirty_clear(
    tab: &PresentationTab,
    saved_slots: &std::collections::BTreeSet<usize>,
) -> bool {
    let path_count = tab.paths.len();
    if tab.cue_album_synthetic_sheet.is_none()
        || tab.pending_embedded_cuesheet_delete
        || path_count == 0
    {
        return false;
    }
    let Some(cuesheet) = tab.entries.iter().find(|entry| {
        entry.display_key.eq_ignore_ascii_case("CUESHEET")
            && entry.per_file_values.len() == path_count
            && entry.per_file_originals.len() == path_count
    }) else {
        return false;
    };
    (0..path_count).all(|idx| {
        saved_slots.contains(&idx) || cuesheet.per_file_values[idx] == cuesheet.per_file_originals[idx]
    })
}

fn reduce_saved_slots(tab: &mut PresentationTab, saved_slots: &std::collections::BTreeSet<usize>) {
    if saved_slots.is_empty() {
        return;
    }

    let path_count = tab.paths.len();
    let deleted: std::collections::BTreeSet<usize> = tab.deleted.iter().copied().collect();
    let unified_cue_album_fully_saved = unified_cue_album_fully_saved_for_dirty_clear(tab, saved_slots);
    let mut remove_entries = Vec::new();
    let mut retained_deleted = Vec::new();

    for (entry_idx, entry) in tab.entries.iter_mut().enumerate() {
        let file_aligned = !entry.is_track_scoped(path_count)
            && entry.per_file_values.len() == path_count
            && entry.per_file_originals.len() == path_count;

        if !file_aligned {
            let unified_row_persistable = unified_cue_album_per_track_key_is_persistable_for_dirty_clear(
                &entry.display_key,
            );
            if deleted.contains(&entry_idx) {
                if unified_cue_album_fully_saved && unified_row_persistable {
                    // Unified CUE-album per-track rows are not written as
                    // independent tag vectors; their delete operation is
                    // consumed by the regenerated embedded CUESHEET that was
                    // just written to every member image.  Only rows with a
                    // defined generator mapping may clear their tombstone here.
                    remove_entries.push(entry_idx);
                } else {
                    // A row-level delete for a non-file-aligned entry cannot be
                    // safely reduced from partial path-keyed write results, and
                    // unsupported unified rows must not be reported saved. Keep
                    // the marker so retry/error state is preserved.
                    retained_deleted.push(entry_idx);
                }
            } else if path_count == 1 && saved_slots.contains(&0) {
                // Non-deleted single-file synthetic/display entries have no
                // per-slot retry state to preserve. Once the sole file saved,
                // advance their originals with the rest of the surface.
                mark_tag_entry_saved(entry);
            } else if unified_cue_album_fully_saved && unified_row_persistable {
                // Unified CUE-album per-track rows with a defined CUE mapping
                // persist through the regenerated embedded CUESHEET that was
                // just written to every member image. Unknown row-dimensioned
                // keys must stay dirty; otherwise edits would appear saved
                // while being skipped by both the tag writer and CUE generator.
                mark_tag_entry_saved(entry);
            }
            continue;
        }

        if deleted.contains(&entry_idx) {
            for idx in 0..path_count {
                if saved_slots.contains(&idx) {
                    entry.per_file_values[idx].clear();
                    entry.per_file_originals[idx].clear();
                    if entry.per_file_stored_value_counts.len() == path_count {
                        entry.per_file_stored_value_counts[idx] = 0;
                    }
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
                    let changed =
                        entry.per_file_values[idx] != entry.per_file_originals[idx];
                    entry.per_file_originals[idx] = entry.per_file_values[idx].clone();
                    if changed && entry.per_file_stored_value_counts.len() == path_count {
                        entry.per_file_stored_value_counts[idx] =
                            scalar_stored_value_count(&entry.per_file_values[idx]);
                    }
                }
            }
        }

        refresh_stored_value_summary(entry);
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
        .filter_map(|idx| {
            if removed.contains(&idx) {
                None
            } else {
                Some(idx - removed.range(..idx).count())
            }
        })
        .collect();
    tab.selected_rows = tab
        .selected_rows
        .iter()
        .filter_map(|idx| {
            if removed.contains(idx) {
                None
            } else {
                Some(*idx - removed.range(..*idx).count())
            }
        })
        .collect();
    for idx in remove_entries.into_iter().rev() {
        if idx < tab.entries.len() {
            tab.entries.remove(idx);
        }
    }

    if tab.cue_album_synthetic_sheet.is_some() && !tab.cue_album_forced_cleanup.is_empty() {
        // Whole-file track-scoped pollution is recorded per member image. A
        // successful write result for a member image consumes only that image's
        // cleanup entries; skipped or failed slots must keep their cleanup work
        // for retry. This keeps cleanup-only saves idempotent when pollution is
        // present on a subset of member images, and avoids rewriting already
        // cleaned files after a partial save.
        tab.cue_album_forced_cleanup
            .retain(|(idx, _key)| !saved_slots.contains(idx));
    }
}

fn presentation_tab_has_changes(tab: &PresentationTab) -> bool {
    if tab.refresh_failed || tab.pending_embedded_cuesheet_delete || !tab.deleted.is_empty() {
        return true;
    }

    let path_count = tab.paths.len();
    let sidecar_authority = native_multi_file_sidecar_authority(tab);
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
        let sidecar_authoritative = sidecar_authority
            && cue_sidecar_representable_entry(tab, entry);
        if sidecar_authoritative {
            return entry.per_file_values != entry.per_file_originals
                || entry.value != entry.original;
        }
        if !entry.is_track_scoped(path_count)
            && entry.per_file_values.len() == path_count
            && entry.per_file_originals.len() == path_count
        {
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
                dst.row_scope = src.row_scope;
                if dst.row_scope == crate::tui::probe::RowScope::Track {
                    dst.clear_stored_value_provenance();
                }
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
                // This row is synthetic in the destination presentation. The
                // source row's stored-item counts describe different files and
                // must not be inherited by a destination that had no carrier
                // for this field.
                entry.clear_stored_value_provenance();
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

/// Interaction phase for the MusicBrainz release picker.
///
/// Once a release is accepted, the picker remains visible only as a
/// non-interactive verification surface. Enter and navigation are ignored;
/// Esc/Cancel invalidates the identified operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MbSelectPhase {
    Selecting,
    Verifying {
        operation_id: crate::tui::message::TagsMbOperationId,
    },
}

impl MbSelectPhase {
    pub fn verifying_operation(self) -> Option<crate::tui::message::TagsMbOperationId> {
        match self {
            Self::Selecting => None,
            Self::Verifying { operation_id } => Some(operation_id),
        }
    }
}

/// Lifecycle phase owned by one asynchronous MusicBrainz workflow.
///
/// Pre-lookup phases are explicit so duplicate discovery/grouping completions
/// can be rejected before they write caches, status, overlays, or editor state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagsMbOperationPhase {
    Discovery,
    Grouping,
    Lookup,
    /// TOC-zero-match text fallback has been dispatched and its one
    /// completion is the only accepted next transition.
    LookupTextFallback,
    Selecting,
    Verifying,
}

/// App-wide owner record for one complete asynchronous MusicBrainz workflow.
///
/// The same identity owns discovery, grouping, lookup, fallback, picker,
/// verification, and apply. `picker_owned` means replacement or dismissal of
/// the picker invalidates the operation immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveTagsMbOperation {
    pub operation_id: crate::tui::message::TagsMbOperationId,
    pub picker_owned: bool,
    pub phase: TagsMbOperationPhase,
}

/// App-wide owner record for one asynchronous GNUDB workflow. When
/// `editor_session` is present, the operation owns the matching editor parked
/// in `pending_metadata_editor`; completions may replace the overlay only while
/// that exact parked session remains authoritative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveGnudbOperation {
    pub operation_id: crate::tui::message::TagsMbOperationId,
    pub editor_session: Option<crate::tui::message::MetadataEditorSessionGuard>,
}

/// Authority for one asynchronous CUE-generation/fill or split-CUE editor-open
/// workflow. These operations do not mutate an existing editor, but their
/// completions may open an overlay, so they must prove both identity and an
/// unobstructed overlay slot before doing so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveCueOperation {
    pub operation_id: crate::tui::message::TagsMbOperationId,
}

/// Completion families whose workers may publish a result overlay or retire a
/// confirmation-owned mutation. Each family has independent authority so, for
/// example, an analysis and an AccurateRip verification may coexist without
/// allowing either completion to supersede the other's surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompletionOperationKind {
    Analysis,
    Verify,
    Compare,
    Preemphasis,
    AccurateRip,
    Ctdb,
    ArBatch,
    OffsetCorrection,
    CtdbRepair,
}

/// Operation-scoped authority for asynchronous completion handlers that can
/// otherwise replace an unrelated overlay. `editor_session` records the exact
/// metadata editor present at dispatch; such operations may enrich that same
/// editor but never replace it with a result overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionBatchProgress {
    pub total: usize,
    pub remaining: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveCompletionOperation {
    pub operation_id: crate::tui::message::TagsMbOperationId,
    pub editor_session: Option<crate::tui::message::MetadataEditorSessionGuard>,
    /// Present for fan-out completion families whose workers each emit one
    /// terminal message. The operation identity, not a process-global counter,
    /// owns this progress state.
    pub batch: Option<CompletionBatchProgress>,
}

/// One in-flight Browse inline metadata write. The generation and exact path
/// guard progress/completion messages against stale workers after navigation or
/// a later edit, while the shared flag gives Esc a cooperative cancellation
/// handle for DSF/FLAC bounded copy loops.
pub struct InlineMetadataWriteState {
    pub operation_id: u64,
    pub path: std::path::PathBuf,
    pub cancel: crate::tui::probe::MetadataWriteCancelFlag,
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
    /// Identity of the lookup workflow that produced this picker. Pickers
    /// reconstructed by `:mb-back` use `UNASSIGNED` and acquire a fresh ID on
    /// acceptance; live lookup pickers preserve their original authority.
    pub operation_id: crate::tui::message::TagsMbOperationId,
    /// Metadata-editor session that initiated this picker, when the picker
    /// came from an in-editor asynchronous MusicBrainz lookup.  Accepting a
    /// release must still match this session before mutating the parked editor.
    pub editor_session: Option<crate::tui::message::MetadataEditorSessionGuard>,
    /// Selecting until the user accepts a release. Verifying is deliberately
    /// non-interactive and carries the operation identity expected by the
    /// eventual async completion.
    pub phase: MbSelectPhase,
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
        Self::new_with_editor_session(releases, paths, None)
    }

    pub fn new_with_editor_session(
        releases: Vec<crate::tui::musicbrainz::MbRelease>,
        paths: Vec<std::path::PathBuf>,
        editor_session: Option<crate::tui::message::MetadataEditorSessionGuard>,
    ) -> Self {
        Self {
            releases,
            selected: 0,
            scroll: 0,
            paths,
            operation_id: crate::tui::message::TagsMbOperationId::UNASSIGNED,
            editor_session,
            phase: MbSelectPhase::Selecting,
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

    pub fn with_operation_id(
        mut self,
        operation_id: crate::tui::message::TagsMbOperationId,
    ) -> Self {
        self.operation_id = operation_id;
        self
    }

    pub fn is_selecting(&self) -> bool {
        matches!(self.phase, MbSelectPhase::Selecting)
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
    /// One-line summary shown beneath the title bar
    /// (e.g., `"Filled CUE: 7 ISRCs, 1 catalog"`).
    pub summary: String,
    /// Optional generic title for read-only informational previews. `None`
    /// preserves the historical CUE-preview title derived from `write_path`.
    pub title_override: Option<String>,
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
            title_override: None,
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
            title_override: None,
            scroll: 0,
            cursor: None,
            edit: None,
            last_click: None,
            read_only: true,
        }
    }

    /// Build a read-only informational help surface using the mature preview
    /// renderer and its scrolling/close controls without inventing a second
    /// generic text-overlay implementation.
    pub fn new_readonly_help(title: String, content: String, summary: String) -> Self {
        let mut state = Self::new_readonly(content, summary);
        state.title_override = Some(title);
        state
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseCreateKind {
    File,
    Folder,
}

/// Which browse-surface field is being edited inline.
#[derive(Debug, Clone, PartialEq)]
pub enum BrowseInlineEditTarget {
    Rename { path: std::path::PathBuf },
    Create {
        dir: std::path::PathBuf,
        kind: BrowseCreateKind,
    },
    Metadata {
        path: std::path::PathBuf,
        field: crate::tui::probe::MetadataField,
    },
}

/// Active inline editor for the Browse screen.
#[derive(Debug, Clone)]
pub struct BrowseInlineEditState {
    pub target: BrowseInlineEditTarget,
    pub input: crate::tui::text_input::TextInputState,
}

/// Text editor surface targeted by the shared mouse/context-menu contract.
///
/// The target is semantic rather than coordinate-based: renderers register a
/// `TuiButton`, the dispatcher resolves that button to one of these variants,
/// and every operation then reaches the authoritative `TextInputState`. This
/// keeps selection, clipboard, case transforms, and drag behavior identical
/// across base-screen and overlay editors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorTextTarget {
    BrowseFileInlineEdit,
    BrowseTreeInlineEdit,
    BrowsePath,
    BrowseSearch,
    BrowseFilter,
    ConvertMetadata,
    ConvertOutputOptions,
    MetadataInline,
    MetadataAddKey,
    MetadataDetail,
    MetadataAutoNumberPrefix,
    GnudbInline,
    ThemeHex,
    ThemeSwatchName,
    ThemeGalleryFilter,
    ThemeFilePath,
    BulkRenameTemplate,
    TemplateBuilder,
    OverlayFileInput,
    OverlayCommandInput,
    OverlayTextEdit,
}

/// Text field that currently owns a left-button drag selection.
pub type BrowseTextMouseTarget = EditorTextTarget;

/// Keyboard focus inside the Browse info pane.
///
/// The browse list keeps its existing type-ahead navigation until the user
/// explicitly moves focus into the metadata rows. Once focused, Enter or any
/// printable key starts the same reusable inline editor used by mouse clicks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BrowseInfoFocus {
    Metadata(crate::tui::probe::MetadataField),
}

/// Which field a TextEdit overlay is editing
#[derive(Debug, Clone, PartialEq)]
pub enum TextEditTarget {
    DestPath,
    FolderTemplate,
    FilenameTemplate,
    CompanionExtensions,
    CompanionFolders,
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
    /// Retry browse archive metadata extraction after collecting a password.
    ArchivePasswordForMetadataEdit {
        archive_path: std::path::PathBuf,
        target_inner_paths: Option<Vec<String>>,
    },
    /// Retry convert-preview probing after collecting a password.
    ArchivePasswordForConvertPreview(std::path::PathBuf),
}

/// Return true when an archive tool error is likely asking for a password.
/// The exact wording differs between archive backends, so keep this intentionally
/// conservative but broader than the listing path's old `password`-only check.
pub(crate) fn looks_like_archive_password_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "password",
        "passphrase",
        "encrypted",
        "encryption",
        "wrong password",
        "requires password",
        "unsupported encryption",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}


/// Explicit focus target for the Config screen.
///
/// Keep this separate from `KeychainState`: password-list state should not also
/// decide whether unrelated Appearance or Conversion controls receive input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFocus {
    Appearance,
    Conversion,
    Performance,
    Keychain,
}

impl Default for ConfigFocus {
    fn default() -> Self {
        Self::Appearance
    }
}

/// Keyboard focus within the advanced file-operation settings overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOperationSettingsFocus {
    Verification,
    StatusVerbosity,
    AutoCloseProgress,
}

impl Default for FileOperationSettingsFocus {
    fn default() -> Self {
        Self::Verification
    }
}

impl FileOperationSettingsFocus {
    pub fn next(self) -> Self {
        match self {
            Self::Verification => Self::StatusVerbosity,
            Self::StatusVerbosity => Self::AutoCloseProgress,
            Self::AutoCloseProgress => Self::Verification,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Verification => Self::AutoCloseProgress,
            Self::StatusVerbosity => Self::Verification,
            Self::AutoCloseProgress => Self::StatusVerbosity,
        }
    }
}

/// Draft values owned by the Config-screen advanced file-operation control.
/// Nothing is persisted until the user accepts the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileOperationSettingsState {
    pub focus: FileOperationSettingsFocus,
    pub verification: tui_file_picker::VerificationMode,
    pub status_verbosity: crate::config::FileOperationStatusVerbosity,
    pub auto_close_progress: bool,
}

impl FileOperationSettingsState {
    pub fn from_config(config: &crate::config::FileOperationsConfig) -> Self {
        Self {
            focus: FileOperationSettingsFocus::default(),
            verification: config.verification,
            status_verbosity: config.status_verbosity,
            auto_close_progress: config.auto_close_progress,
        }
    }

    pub fn cycle_focused_value(&mut self) {
        match self.focus {
            FileOperationSettingsFocus::Verification => {
                self.verification = match self.verification {
                    tui_file_picker::VerificationMode::Standard => {
                        tui_file_picker::VerificationMode::Strong
                    }
                    tui_file_picker::VerificationMode::Strong => {
                        tui_file_picker::VerificationMode::Standard
                    }
                };
            }
            FileOperationSettingsFocus::StatusVerbosity => {
                self.status_verbosity = match self.status_verbosity {
                    crate::config::FileOperationStatusVerbosity::Quiet => {
                        crate::config::FileOperationStatusVerbosity::Verbose
                    }
                    crate::config::FileOperationStatusVerbosity::Verbose => {
                        crate::config::FileOperationStatusVerbosity::Quiet
                    }
                };
            }
            FileOperationSettingsFocus::AutoCloseProgress => {
                self.auto_close_progress = !self.auto_close_progress;
            }
        }
    }
}

impl ConfigFocus {
    pub fn next(self) -> Self {
        match self {
            Self::Appearance => Self::Conversion,
            Self::Conversion => Self::Performance,
            Self::Performance => Self::Keychain,
            Self::Keychain => Self::Appearance,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Appearance => Self::Keychain,
            Self::Conversion => Self::Appearance,
            Self::Performance => Self::Conversion,
            Self::Keychain => Self::Performance,
        }
    }

    pub fn keychain_focused(self) -> bool {
        matches!(self, Self::Keychain)
    }
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
    /// Deprecated compatibility mirror for older tests/helpers.
    ///
    /// `AppState::config_focus` is the source of truth for Config-screen focus.
    pub focused: bool,
    /// Whether passwords have been loaded from disk.
    pub loaded: bool,
    /// Last explicit load/migration/backend failure. Callers must surface this
    /// rather than treating an unavailable secret store as an empty MRU.
    pub load_error: Option<String>,
    /// Non-fatal per-reference resolution failures. Valid entries remain usable.
    pub load_warning: Option<String>,
}

impl Default for KeychainState {
    fn default() -> Self {
        Self {
            passwords: Vec::new(),
            selected: 0,
            reveal: false,
            focused: false,
            loaded: false,
            load_error: None,
            load_warning: None,
        }
    }
}

/// In-flight Browse archive listing request.
///
/// The generation id lets late completions from cancelled/stale workers be
/// ignored without comparing result payloads. The cancellation token owns the
/// child-process cancellation path inside `archive_listing`.
pub struct PendingArchiveListing {
    pub id: u64,
    pub archive_path: std::path::PathBuf,
    pub cancel: tokio_util::sync::CancellationToken,
    pub started_at: std::time::Instant,
}


impl KeychainState {
    /// Load passwords from disk if not already loaded. A failed backend access
    /// remains visible in `load_error`, but does not mark the state loaded: the
    /// next explicit user action retries so unlocking the platform keychain can
    /// recover without restarting tonepoet.
    pub fn ensure_loaded(&mut self) -> Result<(), String> {
        if self.loaded {
            return Ok(());
        }
        self.reload()
    }

    #[cfg(test)] // production loading routes through load_keychain_with_warnings; the injectable pair remains for the retry-semantics pins
    fn ensure_loaded_with<F>(&mut self, load: F) -> Result<(), String>
    where
        F: FnOnce() -> Result<Vec<String>, String>,
    {
        if self.loaded {
            return Ok(());
        }
        self.reload_with(load)
    }

    /// Reload from disk (e.g., after add/remove).
    pub fn reload(&mut self) -> Result<(), String> {
        match crate::tui::keychain::load_keychain_with_warnings() {
            Ok(result) => {
                self.passwords = result.passwords;
                self.loaded = true;
                self.load_error = None;
                self.load_warning = (!result.warnings.is_empty()).then(|| result.warnings.join("; "));
                if self.selected >= self.passwords.len() && !self.passwords.is_empty() {
                    self.selected = self.passwords.len() - 1;
                }
                Ok(())
            }
            Err(error) => {
                self.passwords.clear();
                self.selected = 0;
                self.loaded = false;
                self.load_warning = None;
                self.load_error = Some(error.clone());
                Err(error)
            }
        }
    }

    #[cfg(test)]
    fn reload_with<F>(&mut self, load: F) -> Result<(), String>
    where
        F: FnOnce() -> Result<Vec<String>, String>,
    {
        match load() {
            Ok(passwords) => {
                self.passwords = passwords;
                self.loaded = true;
                self.load_error = None;
                self.load_warning = None;
                if self.selected >= self.passwords.len() && !self.passwords.is_empty() {
                    self.selected = self.passwords.len() - 1;
                }
                Ok(())
            }
            Err(error) => {
                self.passwords.clear();
                self.selected = 0;
                self.loaded = false;
                self.load_warning = None;
                self.load_error = Some(error.clone());
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod keychain_state_retry_tests {
    use super::KeychainState;

    #[test]
    fn failed_load_is_retried_and_recovers_after_backend_unlock() {
        let mut state = KeychainState::default();
        let first = state.ensure_loaded_with(|| Err("secret service is locked".to_string()));

        assert_eq!(first, Err("secret service is locked".to_string()));
        assert_eq!(state.loaded, false);
        assert_eq!(state.passwords, Vec::<String>::new());
        assert_eq!(state.load_error.as_deref(), Some("secret service is locked"));

        let second = state.ensure_loaded_with(|| Ok(vec!["recovered-secret".to_string()]));

        assert_eq!(second, Ok(()));
        assert_eq!(state.loaded, true);
        assert_eq!(state.passwords, vec!["recovered-secret"]);
        assert_eq!(state.load_error, None);
    }
}

/// Expensive bulk operation guarded by an audio-file count confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkOperationKind {
    EditMetadata,
    Analyze,
    VerifyIntegrity,
    AccurateRipVerify,
    AccurateRipFullScan,
    AccurateRipBatch,
    AccurateRipFixOffset,
    CtdbVerify,
    MusicBrainzTagging,
    GnudbTagging,
    PreemphasisDetection,
}

impl BulkOperationKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::EditMetadata => "edit metadata for",
            Self::Analyze => "analyze",
            Self::VerifyIntegrity => "verify",
            Self::AccurateRipVerify => "verify with AccurateRip",
            Self::AccurateRipFullScan => "run a full AccurateRip scan on",
            Self::AccurateRipBatch => "batch-verify with AccurateRip",
            Self::AccurateRipFixOffset => "apply AccurateRip offset correction to",
            Self::CtdbVerify => "verify with CUETools DB",
            Self::MusicBrainzTagging => "look up MusicBrainz tags for",
            Self::GnudbTagging => "look up GNUDB tags for",
            Self::PreemphasisDetection => "detect pre-emphasis on",
        }
    }
}

/// Cloneable command payload used after the user confirms a guarded bulk
/// operation. It intentionally covers only operations with stable replay
/// semantics and avoids storing `Command`, which has non-cloneable closures.
#[derive(Debug, Clone)]
pub enum BulkGuardCommand {
    OpenMetadataEditor,
    Analyze { force: bool },
    Verify,
    AccurateRip { force: bool },
    AccurateRipBatch,
    AccurateRipFixOffset,
    Ctdb,
    TagsFromMb {
        query: Option<String>,
        catno: Option<String>,
        year: Option<String>,
    },
    Gnudb,
    DetectPreemphasis,
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
    /// Write the current editor snapshot to explicitly selected target files.
    /// Dirty editors are parked while this blocking confirmation is open, so
    /// cancellation restores the exact unsaved state and confirmation writes
    /// the exact snapshot named by the prompt.
    MetadataTransferUnsaved {
        source_entries: Vec<crate::tui::probe::TagEntry>,
        source_dimension: crate::tui::tag_interchange::TransferDimension,
        target: crate::tui::tag_interchange::TransferCarrier,
        scope: TagTransferScope,
        edit_count: usize,
    },
    /// Browse-side tag transfer prepared and dry-run planned before any
    /// target mutation. Confirmation executes exactly this frozen snapshot.
    BrowseTagTransfer {
        prepared: crate::tui::browse::PreparedTagTransfer,
    },
    /// Close the metadata editor and discard unsaved changes after an
    /// explicit confirmation. The editor itself is parked in
    /// `AppState::pending_metadata_editor` so cancellation restores it.
    DiscardMetadataEditorChanges,
    /// Stage deletion of the active embedded CUESHEET after an explicit
    /// destructive-action confirmation. The editor itself is parked in
    /// `AppState::pending_metadata_editor`; confirmation only stages the
    /// tombstone, and persistence still flows through the metadata-editor save
    /// path.
    DeleteEmbeddedCueSheet { path: PathBuf },
    /// Atomically remove invalid-key APEv2 items from the frozen open-set
    /// targets after a blocking confirmation.
    RemoveInvalidApeKeys {
        targets: Vec<(PathBuf, Vec<String>)>,
        verification: tui_file_picker::VerificationMode,
    },
    TagMaintenance {
        kind: crate::tui::probe::TagMaintenanceKind,
        roots: Vec<PathBuf>,
        from_metadata_editor: bool,
        verification: tui_file_picker::VerificationMode,
    },
    RemoveSelected,
    ClearCompleted,
    ClearFinished,
    ClearAll,
    StopAll,
    ClearQueue,
    /// Confirm an expensive bulk operation before replaying it once.
    ///
    /// The resolved path payload and bounded count are captured when the
    /// prompt is opened, so confirmation always executes exactly the same
    /// set the user was warned about even if Browse selection/cursor state
    /// changes before Enter/Y is processed.
    BulkOperation {
        operation: BulkOperationKind,
        command: BulkGuardCommand,
        paths: Vec<PathBuf>,
        count: usize,
    },
    /// Overwrite the currently active preset after user confirmation.
    SavePresetOverwrite { name: String, path: PathBuf },
    /// Confirm removal of artifacts created by the most recent copy operation.
    /// The entry id prevents a stale confirmation from acting on a newer top
    /// journal entry.
    UndoCopy { entry_id: u64 },
    /// Permanently delete the given filesystem paths after explicit confirmation.
    DeleteSelection(Vec<PathBuf>),
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
    /// Resolve a deferred Browse archive save after the original archive was
    /// modified outside the app. Confirming overwrites the archive with the
    /// staged edits; cancelling keeps the live staging session for later
    /// retry/discard; pressing D discards the staged edits explicitly.
    ArchiveExternalConflict {
        context: ArchiveMetadataEditContext,
    },
    /// Resolve a failed deferred Browse archive save. Confirming retries the
    /// save; cancelling keeps the live staging session; pressing D discards
    /// the staged edits explicitly.
    ArchiveRepackageFailure {
        context: ArchiveMetadataEditContext,
        error: String,
    },
    /// Mouse-accessible destructive discard confirmation for a staged Browse
    /// archive session. This exists because the generic confirmation overlay
    /// exposes only confirm/cancel buttons; the primary conflict/failure
    /// dialogs keep cancel non-destructive and route mouse users through this
    /// second explicit confirmation before deleting staged edits.
    ArchiveDiscardStaging {
        context: ArchiveMetadataEditContext,
        quit_after_discard: bool,
    },
    /// Explicit destructive discard confirmation for a recovered startup archive
    /// session. Opened by keyboard No/D or by the overlay's mouse cancel button.
    ArchiveDiscardStartupRecovery {
        session: crate::db::PendingArchiveSessionRecovery,
    },
    /// Resolve a durable archive staging session found on startup. Confirming
    /// resumes it in Browse; declining opens an explicit discard confirmation;
    /// Esc keeps it in the database for a later startup.
    ArchiveStartupRecovery {
        session: crate::db::PendingArchiveSessionRecovery,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmationFooterHint {
    pub label: &'static str,
    pub key: &'static str,
}

/// Footer-button labels for confirmation overlays.
///
/// The renderer should use this instead of hard-coded `Y yes` / `N no` text so
/// visible chips, keyboard handlers, and mouse handlers stay aligned for
/// multi-action confirmations such as archive startup recovery.
pub fn confirmation_footer_hints(action: &ConfirmAction) -> &'static [ConfirmationFooterHint] {
    const DEFAULT: &[ConfirmationFooterHint] = &[
        ConfirmationFooterHint { label: "Y yes", key: "y" },
        ConfirmationFooterHint { label: "N no", key: "n" },
    ];
    const ARCHIVE_STARTUP_RECOVERY: &[ConfirmationFooterHint] = &[
        ConfirmationFooterHint { label: "Y resume", key: "y" },
        ConfirmationFooterHint { label: "N discard...", key: "n" },
        ConfirmationFooterHint { label: "D discard...", key: "d" },
        ConfirmationFooterHint { label: "Esc later", key: "esc" },
    ];
    const ARCHIVE_DISCARD_STARTUP_RECOVERY: &[ConfirmationFooterHint] = &[
        ConfirmationFooterHint { label: "Y discard", key: "y" },
        ConfirmationFooterHint { label: "N keep", key: "n" },
        ConfirmationFooterHint { label: "Esc keep", key: "esc" },
    ];
    const ARCHIVE_RETRY_OR_DISCARD: &[ConfirmationFooterHint] = &[
        ConfirmationFooterHint { label: "Y retry", key: "y" },
        ConfirmationFooterHint { label: "D discard...", key: "d" },
        ConfirmationFooterHint { label: "N keep", key: "n" },
        ConfirmationFooterHint { label: "Esc keep", key: "esc" },
    ];
    const ARCHIVE_DISCARD_STAGING: &[ConfirmationFooterHint] = &[
        ConfirmationFooterHint { label: "Y discard", key: "y" },
        ConfirmationFooterHint { label: "N keep", key: "n" },
        ConfirmationFooterHint { label: "Esc keep", key: "esc" },
    ];
    const DELETE_EMBEDDED_CUESHEET: &[ConfirmationFooterHint] = &[
        ConfirmationFooterHint { label: "Y delete", key: "y" },
        ConfirmationFooterHint { label: "N cancel", key: "n" },
        ConfirmationFooterHint { label: "Esc cancel", key: "esc" },
    ];
    const REMOVE_INVALID_APE_KEYS: &[ConfirmationFooterHint] = &[
        ConfirmationFooterHint { label: "Y remove", key: "y" },
        ConfirmationFooterHint { label: "N cancel", key: "n" },
        ConfirmationFooterHint { label: "Esc cancel", key: "esc" },
    ];
    const REPAIR_TAGS: &[ConfirmationFooterHint] = &[
        ConfirmationFooterHint { label: "Y repair", key: "y" },
        ConfirmationFooterHint { label: "N cancel", key: "n" },
        ConfirmationFooterHint { label: "Esc cancel", key: "esc" },
    ];
    const REMOVE_ALL_TAGS: &[ConfirmationFooterHint] = &[
        ConfirmationFooterHint { label: "Y remove", key: "y" },
        ConfirmationFooterHint { label: "N cancel", key: "n" },
        ConfirmationFooterHint { label: "Esc cancel", key: "esc" },
    ];

    match action {
        ConfirmAction::ArchiveStartupRecovery { .. } => ARCHIVE_STARTUP_RECOVERY,
        ConfirmAction::ArchiveDiscardStartupRecovery { .. } => ARCHIVE_DISCARD_STARTUP_RECOVERY,
        ConfirmAction::ArchiveExternalConflict { .. }
        | ConfirmAction::ArchiveRepackageFailure { .. } => ARCHIVE_RETRY_OR_DISCARD,
        ConfirmAction::ArchiveDiscardStaging { .. } => ARCHIVE_DISCARD_STAGING,
        ConfirmAction::DeleteEmbeddedCueSheet { .. } => DELETE_EMBEDDED_CUESHEET,
        ConfirmAction::RemoveInvalidApeKeys { .. } => REMOVE_INVALID_APE_KEYS,
        ConfirmAction::TagMaintenance {
            kind: crate::tui::probe::TagMaintenanceKind::Repair,
            ..
        } => REPAIR_TAGS,
        ConfirmAction::TagMaintenance {
            kind: crate::tui::probe::TagMaintenanceKind::RemoveAll,
            ..
        } => REMOVE_ALL_TAGS,
        _ => DEFAULT,
    }
}

/// Human-readable footer hint text for confirmation overlays.
///
/// Renderers that cannot or do not want to style each hint individually can
/// use this string form. Prefer [`confirmation_footer_hints`] when drawing
/// pill/chip styled footer controls.
pub fn confirmation_footer_hint_text(action: &ConfirmAction) -> String {
    confirmation_footer_hints(action)
        .iter()
        .map(|hint| hint.label)
        .collect::<Vec<_>>()
        .join("     ")
}

/// User-facing text for the startup archive recovery confirmation.
///
/// Keep this in one place so the first prompt opened by `AppState::new*` and
/// the prompts opened after resolving earlier recovered sessions cannot drift.
pub fn archive_startup_recovery_prompt_message(
    session: &crate::db::PendingArchiveSessionRecovery,
) -> String {
    let reason = session.conflict_reason.as_deref().unwrap_or("none");
    let edits = archive_recovery_edits_summary(&session.edits_json);
    format!(
        "Recovered staged archive edits from a previous run:\n{}\n\nStaging: {}\nEdits: {}\nConflict: {}\n\nY/Enter resumes the staged archive view. N/No opens a discard confirmation. D also opens discard confirmation. Esc keeps them for next startup.",
        session.archive_path.display(),
        session.staging_dir.display(),
        edits,
        reason,
    )
}

fn archive_recovery_edits_summary(edits_json: &str) -> String {
    const PREVIEW_LIMIT: usize = 180;

    let compact = edits_json.trim().replace('\n', " ");
    let mut chars = compact.chars();
    let mut preview: String = chars.by_ref().take(PREVIEW_LIMIT).collect();
    if chars.next().is_some() {
        preview.push_str("...");
    }

    match serde_json::from_str::<serde_json::Value>(edits_json) {
        Ok(serde_json::Value::Array(edits)) => {
            let count = edits.len();
            if count == 0 {
                "0 edit operations".to_string()
            } else {
                format!("{count} edit operation(s); preview: {preview}")
            }
        }
        Ok(_) => format!("1 edit payload; preview: {preview}"),
        Err(_) if preview.is_empty() => "unavailable".to_string(),
        Err(_) => format!("unparseable edit payload; preview: {preview}"),
    }
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

// ── Reversible Browse file operations ───────────────────────────────

/// Bounded in-session undo depth. Entries retain whole-tree manifests, so an
/// intentionally modest cap prevents a long session from retaining unbounded
/// metadata while still covering normal interactive use.
pub const FILE_OPERATION_UNDO_LIMIT: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOperationUndoKind {
    Copy,
    Move,
    Rename,
}

impl FileOperationUndoKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Move => "move",
            Self::Rename => "rename",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileOperationUndoMapping {
    pub source: PathBuf,
    pub destination: PathBuf,
    /// Authoritative proof for the object currently at `source`, when one is
    /// expected. For copy entries this is retained from the worker's copy-time
    /// source manifest; replayed move/rename entries refresh it after commit.
    pub source_proof: Option<tui_file_picker::FileTaskRootProof>,
    /// Authoritative proof for the object currently at `destination`, when one
    /// is expected. Initial copy/move entries receive this directly from the
    /// worker completion report, never from a UI-thread recapture.
    pub destination_proof: Option<tui_file_picker::FileTaskRootProof>,
}

#[derive(Debug, Clone)]
pub struct FileOperationUndoEntry {
    pub id: u64,
    pub kind: FileOperationUndoKind,
    /// Transaction root used by the authoritative rename planner. Present only
    /// for `Rename` entries so undo/redo can replay case-only names, swaps,
    /// cycles, and nested target paths through the same staged transaction.
    pub rename_base_dir: Option<PathBuf>,
    pub mappings: Vec<FileOperationUndoMapping>,
}

#[derive(Debug, Default)]
pub struct FileOperationUndoJournal {
    undo: std::collections::VecDeque<FileOperationUndoEntry>,
    redo: std::collections::VecDeque<FileOperationUndoEntry>,
    recorded_task_sessions: std::collections::VecDeque<u64>,
    next_id: u64,
}

impl FileOperationUndoJournal {
    pub fn allocate_id(&mut self) -> u64 {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.next_id
    }

    pub fn record(&mut self, entry: FileOperationUndoEntry) {
        self.redo.clear();
        self.undo.push_back(entry);
        while self.undo.len() > FILE_OPERATION_UNDO_LIMIT {
            self.undo.pop_front();
        }
    }

    pub fn has_recorded_task_session(&self, session_id: u64) -> bool {
        self.recorded_task_sessions.contains(&session_id)
    }

    pub fn record_task_once(&mut self, session_id: u64, entry: FileOperationUndoEntry) -> bool {
        if self.recorded_task_sessions.contains(&session_id) {
            return false;
        }
        self.recorded_task_sessions.push_back(session_id);
        while self.recorded_task_sessions.len() > FILE_OPERATION_UNDO_LIMIT * 2 {
            self.recorded_task_sessions.pop_front();
        }
        self.record(entry);
        true
    }

    pub fn undo_entry(&self) -> Option<&FileOperationUndoEntry> {
        self.undo.back()
    }

    pub fn redo_entry(&self) -> Option<&FileOperationUndoEntry> {
        self.redo.back()
    }

    pub fn take_undo(&mut self, expected_id: u64) -> Option<FileOperationUndoEntry> {
        (self.undo.back().is_some_and(|entry| entry.id == expected_id))
            .then(|| self.undo.pop_back())
            .flatten()
    }

    pub fn take_redo(&mut self, expected_id: u64) -> Option<FileOperationUndoEntry> {
        (self.redo.back().is_some_and(|entry| entry.id == expected_id))
            .then(|| self.redo.pop_back())
            .flatten()
    }

    pub fn restore_undo(&mut self, entry: FileOperationUndoEntry) {
        self.undo.push_back(entry);
    }

    pub fn restore_redo(&mut self, entry: FileOperationUndoEntry) {
        self.redo.push_back(entry);
    }

    pub fn finish_undo(&mut self, entry: FileOperationUndoEntry) {
        self.redo.push_back(entry);
        while self.redo.len() > FILE_OPERATION_UNDO_LIMIT {
            self.redo.pop_front();
        }
    }

    pub fn finish_redo(&mut self, entry: FileOperationUndoEntry) {
        self.undo.push_back(entry);
        while self.undo.len() > FILE_OPERATION_UNDO_LIMIT {
            self.undo.pop_front();
        }
    }

    #[cfg(test)]
    pub fn depths(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }
}

// ── Main application state ───────────────────────────────────────────

/// Main application state
pub struct AppState {
    pub config: TonepoetConfig,
    /// Resolved runtime TUI theme. Config stores only the slug; render-time
    /// ownership lives here so tests and future renderers do not need to
    /// consult process-global theme state.
    pub theme: crate::tui::theme::Theme,
    pub theme_overrides: crate::tui::theme::ThemeOverrides,
    /// Cached theme-library metadata and preview colors. Renderers read this
    /// snapshot instead of repeatedly scanning/parsing custom theme files. The
    /// cache is refreshed on explicit theme-library actions such as opening the
    /// gallery, saving a theme, or deleting a custom theme.
    pub theme_library: crate::tui::theme::ThemeLibrarySnapshot,
    pub manager: ConversionManager,
    pub db: crate::db::Database,
    _owned_database_dir: AppOwnedDatabaseDir,

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

    /// Pre-emphasis detection results.
    pub preemph_results: Vec<crate::tui::preemphasis::PreemphasisResult>,

    /// Reference paths for bit-compare (marked by user, persists until cleared).
    pub compare_reference: Vec<std::path::PathBuf>,

    /// Bit-compare results from the last comparison.
    pub compare_results: Vec<crate::tui::bit_compare::CompareResult>,

    // Navigation
    pub current_screen: AppScreen,
    pub previous_screen: Option<AppScreen>,

    // Convert screen
    pub convert: ConvertState,
    /// Monotonic Convert-source probe generation. Increment before installing
    /// any new source so late background completions cannot mutate it.
    pub probe_generation: u64,
    /// Main TUI message sender, installed by the event loop. Helper paths that
    /// are not passed `tx` directly use this to launch async source probes.
    pub tui_tx: Option<tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>>,
    /// Browse Convert folder expansion currently running on a blocking worker.
    /// New source/queue requests cancel and replace this handle; completion
    /// reducers accept only the matching generation and request snapshot.
    pub pending_browse_convert_expansion: Option<PendingBrowseConvertExpansion>,
    pub preset: PresetState,

    // Browse screen state
    pub browse: crate::tui::browse::BrowseState,
    pub inline_metadata_write_generation: u64,
    pub inline_metadata_write: Option<InlineMetadataWriteState>,

    // Queue screen state
    pub queue_focus: QueueFocus,
    pub selected_index: usize,
    pub scroll_offset: usize,
    pub visible_height: usize,
    pub items_snapshot: Vec<ConversionItem>,
    pub button_map: ButtonRenderMap,
    /// Shared double-click tracking for button-map targets.
    pub double_click: DoubleClickState,

    // Wizard (when active)
    pub wizard: Option<tonepoet_wizard::SimpleWizard>,
    pub wizard_mouse_areas: Option<tonepoet_wizard::MouseAreas>,
    pub wizard_target: WizardTarget,

    // Overlays
    pub active_overlay: ActiveOverlay,

    /// Overlay temporarily replaced by a text-editor context menu. Unlike the
    /// older feature-specific parking slots, this owns the complete overlay so
    /// right-click cannot commit, cancel, or reconstruct an edit session.
    pub pending_editor_context_overlay: Option<Box<ActiveOverlay>>,

    /// Authoritative text field under an open editor context menu.
    pub editor_context_target: Option<EditorTextTarget>,

    /// Last-request-wins ownership for asynchronous host-clipboard reads.
    pub host_clipboard_paste_generation: u64,

    /// Most recent terminal file-task state, retained after its live overlay is
    /// dismissed so full warnings/failures remain inspectable via `:messages`.
    pub last_file_task_progress: Option<(u64, tui_file_picker::FileTaskProgressState)>,

    /// Bounded, in-session undo/redo journal for completed copy, move, and
    /// rename operations. Delete operations are intentionally never recorded.
    pub file_operation_undo: FileOperationUndoJournal,

    /// Test-only capture seam for proving that Browse activation selected the
    /// semantic View flow without launching an external pager.
    #[cfg(test)]
    pub test_view_file_dispatches: Option<Vec<std::path::PathBuf>>,

    /// Show routine capability-degradation notices for file operations. Quiet
    /// is the safe default; failures and data-affecting warnings are unaffected.
    pub file_task_verbose_degrade_notices: bool,

    /// One-shot bypass consumed when replaying a bulk operation after the
    /// user accepted its confirmation dialog.
    pub bulk_guard_bypass: Option<BulkOperationKind>,

    /// Frozen Browse/Convert path payload for the command currently being
    /// replayed from a bulk-operation confirmation. This prevents the
    /// confirmed action from observing later cursor/selection drift.
    pub bulk_guard_frozen_paths: Option<Vec<PathBuf>>,

    /// Transient path payload owned by an open Browse entry context menu.
    /// Right-click never mutates persistent marks: an unmarked row freezes
    /// only that row, while a marked row freezes the current marked set.
    pub browse_context_action_paths: Option<Vec<PathBuf>>,

    /// Parked BulkRenameState while a per-line TextEdit is open.
    /// Set when `e` is pressed on a BulkRename row; consumed when
    /// the TextEdit commits or is cancelled.
    pub pending_bulk_rename: Option<Box<BulkRenameState>>,

    /// Parked MetadataEditorState while command mode or CUE import
    /// review is open. Set when `:` is pressed in the metadata editor;
    /// restored after the command executes or review completes.
    pub pending_metadata_editor: Option<Box<MetadataEditorState>>,

    /// Browse-screen archive metadata extraction currently in flight. This owns
    /// the temporary staging directory until a matching completion opens the
    /// editor or an error/stale result cleans it up.
    pub pending_browse_archive_metadata: Option<PendingBrowseArchiveMetadataEdit>,

    /// Browse-screen archive-entry rename currently in flight. This owns the
    /// temporary staging tree until the matching completion arrives.
    pub pending_browse_archive_rename: Option<PendingBrowseArchiveRename>,

    /// Browse-screen archive-entry delete currently in flight. This owns the
    /// initial extraction staging tree until the matching completion attaches
    /// it as the active deferred-save session or cleans it on failure.
    pub pending_browse_archive_delete: Option<PendingBrowseArchiveDelete>,

    /// Browse-screen archive repackage currently in flight for deferred archive
    /// saves. Staging is removed only after a successful archive replacement;
    /// failures keep staged edits available for retry/discard.
    pub browse_archive_repackage: Option<ArchiveMetadataEditContext>,

    /// File-task overlay session owned by `browse_archive_repackage`. Terminal
    /// failure/cancellation prompts may replace only this exact progress
    /// surface; a newer overlay must never be clobbered by a late completion.
    pub browse_archive_repackage_progress_session_id: Option<u64>,

    /// Editor-owned whole-archive metadata staging preserved after a cancelled
    /// or failed repackage. Unlike in-archive Browse staging, parent-directory
    /// archive edits have no live ArchiveBrowseState owner, so the AppState must
    /// retain the retry/discard context until the user explicitly resolves it.
    pub preserved_editor_archive_repackage: Option<ArchiveMetadataEditContext>,

    /// Durable pending archive sessions discovered on startup and awaiting
    /// explicit resume/discard/keep decisions.
    pub pending_archive_recovery: std::collections::VecDeque<crate::db::PendingArchiveSessionRecovery>,

    /// Recovery session selected for resume while its archive listing worker is
    /// running. The listing completion attaches this staging session to Browse.
    pub pending_archive_recovery_resume: Option<crate::tui::browse::ArchiveStagingSession>,

    /// True when the recovery session being resumed already conflicted with an
    /// externally changed archive. The session is still resumable, but the next
    /// save must present the normal overwrite/discard choice.
    pub pending_archive_recovery_resume_conflicted: bool,

    /// True when a startup recovery confirmation is open. Used to advance to
    /// the next retained session after the user keeps/discards/resumes one.
    pub archive_recovery_prompt_active: bool,

    /// True when global quit has been requested while a Browse archive metadata
    /// editor still needs close-time reconciliation. The event loop defers the
    /// actual exit until dirty staging has been repackaged or the user resolves
    /// the unsaved-editor confirmation.
    pub quit_after_browse_archive_metadata_resolution: bool,

    /// True when quit is waiting for an already-started Browse archive repackage
    /// to finish. Successful completion resumes shutdown; failure keeps the app
    /// open long enough for the user to see the error.
    pub quit_after_browse_archive_repackage: bool,

    /// True when quit is waiting for an in-flight Browse archive-entry rename to
    /// finish. The rename worker owns active extraction/repackage I/O, so quit
    /// must never remove its staging directory out from under it.
    pub quit_after_browse_archive_rename: bool,

    /// True when quit is waiting for an in-flight Browse archive-entry delete to
    /// finish. Successful delete completion resumes quit, which then runs the
    /// normal dirty-staging repackage path; failure cancels quit.
    pub quit_after_browse_archive_delete: bool,

    /// Target screen requested while a first archive edit is still extracting.
    /// The screen switch is not considered complete until the edit is either
    /// saved, discarded, or cancelled without staged changes.
    pub deferred_browse_archive_screen_switch: Option<AppScreen>,

    /// True when the user has requested ordinary archive exit (Esc/Left/Back/.. )
    /// while first-edit archive staging is still in flight. Completion must attach
    /// the staged session and run the same deferred-save path rather than strand
    /// the edit for startup recovery.
    pub deferred_browse_archive_exit: bool,

    /// Parked CuePreviewState while command mode is open. Set when `:`
    /// is pressed in the CUE preview overlay; consumed by `:w` (writes
    /// the CUE) and `:q` (cancels), or restored unchanged if neither.
    pub pending_cue_preview: Option<Box<CuePreviewState>>,

    /// Parked MbSelectState while a context menu is open over the
    /// MusicBrainz release picker. Restored when the menu closes
    /// without consuming the picker.
    pub pending_mb_select: Option<Box<MbSelectState>>,

    /// Monotonic source for opaque MusicBrainz/GNUDB operation identities.
    pub tags_mb_operation_generation: u64,
    /// Currently authoritative selected-release apply, if any. Async
    /// completions must match this record before mutating an editor.
    pub active_tags_mb_operation: Option<ActiveTagsMbOperation>,
    /// Currently authoritative GNUDB lookup/read workflow, if any. GNUDB
    /// completions must match this identity and must never run while a
    /// MusicBrainz workflow owns the metadata-editor authority.
    pub active_gnudb_operation: Option<ActiveGnudbOperation>,
    /// Currently authoritative asynchronous CUE/split-CUE overlay workflow.
    pub active_cue_operation: Option<ActiveCueOperation>,
    /// Independent operation authority for async analysis/verification/repair
    /// completion families that may publish or dismiss overlays.
    pub active_completion_operations:
        std::collections::BTreeMap<CompletionOperationKind, ActiveCompletionOperation>,

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

    /// Active inline editor in the Browse screen (file-list rename or info-pane metadata).
    pub browse_inline_edit: Option<BrowseInlineEditState>,

    /// Mouse-selection ownership for Browse text fields. This prevents a drag
    /// inside an editor from leaking through to list range selection.
    pub browse_text_mouse_target: Option<BrowseTextMouseTarget>,
    /// Sequential inline rename (Tab/Shift+Tab) continuation parked while the
    /// committed rename executes on its worker. `complete_rename_plan` binds it
    /// to the post-commit refresh scan; `(directory, next target path)`.
    pub pending_inline_rename_resume: Option<(std::path::PathBuf, std::path::PathBuf)>,

    /// Keyboard focus in the Browse info pane. This is deliberately separate
    /// from `browse_inline_edit`: focus may sit on a metadata row before the
    /// user presses Enter or starts typing.
    pub browse_info_focus: Option<BrowseInfoFocus>,

    /// Recent files list + overlay state (persisted to ~/.cache/tonepoet/recent.json).
    pub recent: crate::tui::recent_files::RecentFilesState,

    /// Bookmarks list + overlay state (persisted to ~/.config/tonepoet/bookmarks.toml).
    pub bookmarks: crate::tui::bookmarks::BookmarksState,

    /// Focus target on the Config screen.
    pub config_focus: ConfigFocus,

    /// Password keychain state for the Config screen.
    pub keychain: KeychainState,

    /// Session-level archive password overrides (archive path → password).
    /// Set via the `:password` command or interactive prompt. Takes
    /// priority over keychain MRU when committing archives.
    pub archive_passwords: std::collections::HashMap<std::path::PathBuf, String>,

    /// In-flight Browse archive listing, if any. Esc, a new listing, or screen
    /// changes cancel this token and stale completions are ignored by id.
    pub pending_archive_listing: Option<PendingArchiveListing>,

    /// Monotonic generation for Browse archive listing workers.
    pub archive_listing_generation: u64,

    /// In-memory archive listing cache keyed by path + size + mtime.
    /// Access is intentionally funneled through helper methods so LRU order and
    /// byte-budget invariants cannot be bypassed by other TUI modules.
    archive_listing_cache: std::collections::HashMap<
        crate::tui::archive_listing::ArchiveListingCacheKey,
        crate::tui::archive_listing::ArchiveListing,
    >,

    /// Least-recently-used order for `archive_listing_cache`. The newest key is
    /// at the back.
    archive_listing_cache_lru: std::collections::VecDeque<
        crate::tui::archive_listing::ArchiveListingCacheKey,
    >,

    /// Approximate retained heap footprint for archive listings.
    archive_listing_cache_bytes: usize,

    /// Session-scoped same-folder split-CUE album grouping decisions. The
    /// grouping ladder may spend MusicBrainz TOC probes to decide whether cue
    /// surfaces form one album or several; retaining the decision here reuses
    /// that answer across Browse dispatch, in-editor `:tags-mb`, and subsequent
    /// metadata-editor opens in the same run.
    pub(crate) split_cue_album_grouping_cache: std::collections::HashMap<
        crate::tui::command::SplitCueAlbumGroupingKey,
        crate::tui::command::SplitCueAlbumGroupingDecision,
    >,

    /// Last directory visited by the Artwork-tab file picker.
    ///
    /// This is host-owned state rather than crate state because the reusable
    /// picker has no knowledge of tonepoet artwork workflows. It is updated
    /// whenever an artwork picker session completes or is cancelled, and used
    /// as the preferred start directory for the next artwork picker session.
    pub last_artwork_picker_dir: Option<std::path::PathBuf>,

    /// Terminal image protocol picker detected once at startup and refreshed on resize.
    pub image_picker: ratatui_image::picker::Picker,
    /// Monotonic generation for terminal image protocol/cell-size changes.
    pub image_picker_generation: usize,
    /// Monotonic generation used to force terminal image command re-emission
    /// after mouse movement or other terminal-side graphics damage.
    pub image_repaint_generation: usize,
    /// Kitty-only retransmit generation used when Ghostty/Kitty mouse damage
    /// requires protocol re-creation/retransmission. This is intentionally
    /// separate from `image_picker_generation`, which tracks real terminal
    /// size/cell-metric changes.
    pub image_kitty_retransmit_generation: usize,
    /// Last time a Kitty/Ghostty retransmit was requested. Used to rate-limit
    /// protocol rebuilds during high-frequency mouse motion.
    pub last_image_kitty_retransmit_at: Option<Instant>,

    // Caches
    pub tool_check_cache: once_cell::sync::OnceCell<Vec<(String, String, bool)>>,
}

fn new_terminal_image_picker() -> ratatui_image::picker::Picker {
    let mut picker = ratatui_image::picker::Picker::from_termios()
        .unwrap_or_else(|_| ratatui_image::picker::Picker::new((8, 12)));
    configure_terminal_image_picker_protocol_for_current_environment(&mut picker);
    picker
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalImageProtocolProbeDecision {
    ForceHalfblocks,
    GuessProtocol,
}

trait TerminalImagePickerProtocolProbe {
    fn protocol_type(&self) -> ratatui_image::picker::ProtocolType;
    fn force_halfblocks_protocol(&mut self);
    fn guess_terminal_protocol(&mut self);
}

impl TerminalImagePickerProtocolProbe for ratatui_image::picker::Picker {
    fn protocol_type(&self) -> ratatui_image::picker::ProtocolType {
        self.protocol_type
    }

    fn force_halfblocks_protocol(&mut self) {
        self.protocol_type = ratatui_image::picker::ProtocolType::Halfblocks;
    }

    fn guess_terminal_protocol(&mut self) {
        ratatui_image::picker::Picker::guess_protocol(self);
    }
}

fn configure_terminal_image_picker_protocol_for_current_environment<P>(picker: &mut P)
where
    P: TerminalImagePickerProtocolProbe,
{
    configure_terminal_image_picker_protocol(
        picker,
        terminal_image_protocol_probe_decision_for_current_environment(),
    );
}

fn configure_terminal_image_picker_protocol<P>(
    picker: &mut P,
    decision: TerminalImageProtocolProbeDecision,
)
where
    P: TerminalImagePickerProtocolProbe,
{
    match decision {
        TerminalImageProtocolProbeDecision::ForceHalfblocks => picker.force_halfblocks_protocol(),
        TerminalImageProtocolProbeDecision::GuessProtocol => picker.guess_terminal_protocol(),
    }
}

fn terminal_image_protocol_probe_decision_for_current_environment(
) -> TerminalImageProtocolProbeDecision {
    terminal_image_protocol_probe_decision_for_environment(
        std::env::var_os("TMUX").as_deref(),
        std::env::var_os("TERM_PROGRAM").as_deref(),
        std::env::var_os("TERM").as_deref(),
        std::env::var_os("BYOBU_BACKEND").as_deref(),
    )
}

fn terminal_image_protocol_probe_decision_for_environment(
    tmux: Option<&std::ffi::OsStr>,
    term_program: Option<&std::ffi::OsStr>,
    term: Option<&std::ffi::OsStr>,
    byobu_backend: Option<&std::ffi::OsStr>,
) -> TerminalImageProtocolProbeDecision {
    if tmux_like_environment(tmux, term_program, term, byobu_backend) {
        TerminalImageProtocolProbeDecision::ForceHalfblocks
    } else {
        TerminalImageProtocolProbeDecision::GuessProtocol
    }
}

fn enforce_safe_terminal_image_picker_protocol_for_current_environment<P>(picker: &mut P) -> bool
where
    P: TerminalImagePickerProtocolProbe,
{
    enforce_safe_terminal_image_picker_protocol(
        picker,
        terminal_image_protocol_probe_decision_for_current_environment(),
    )
}

fn enforce_safe_terminal_image_picker_protocol<P>(
    picker: &mut P,
    decision: TerminalImageProtocolProbeDecision,
) -> bool
where
    P: TerminalImagePickerProtocolProbe,
{
    if decision != TerminalImageProtocolProbeDecision::ForceHalfblocks
        || picker.protocol_type() == ratatui_image::picker::ProtocolType::Halfblocks
    {
        return false;
    }

    picker.force_halfblocks_protocol();
    true
}

fn tmux_like_environment(
    tmux: Option<&std::ffi::OsStr>,
    term_program: Option<&std::ffi::OsStr>,
    term: Option<&std::ffi::OsStr>,
    byobu_backend: Option<&std::ffi::OsStr>,
) -> bool {
    if non_empty_env_value(tmux) {
        return true;
    }

    let term_program = lower_env_value(term_program);
    if term_program.as_deref() == Some("tmux") {
        return true;
    }

    let byobu_backend = lower_env_value(byobu_backend);
    if byobu_backend.as_deref() == Some("tmux") {
        return true;
    }

    let term = lower_env_value(term);
    term.as_deref()
        .map(|term| term == "tmux" || term.starts_with("tmux-"))
        .unwrap_or(false)
}

fn non_empty_env_value(value: Option<&std::ffi::OsStr>) -> bool {
    value.map(|value| !value.is_empty()).unwrap_or(false)
}

fn lower_env_value(value: Option<&std::ffi::OsStr>) -> Option<String> {
    let value = value?;
    if value.is_empty() {
        return None;
    }
    Some(value.to_string_lossy().to_ascii_lowercase())
}

const KITTY_IMAGE_RETRANSMIT_MIN_INTERVAL: Duration = Duration::from_millis(33);

/// Database source used while constructing [`AppState`].
///
/// Production remains explicit and file-backed. Tests and integration tests can
/// inject either a concrete SQLite path or an isolated temp-file database without
/// relying on `cfg(test)` behavior in this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppDatabaseSource {
    Production,
    InMemory,
    File(std::path::PathBuf),
    IsolatedTempFile,
}

impl Default for AppDatabaseSource {
    fn default() -> Self {
        #[cfg(test)]
        {
            Self::IsolatedTempFile
        }
        #[cfg(not(test))]
        {
            Self::Production
        }
    }
}

/// Startup behaviors whose production defaults must remain explicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppStartupOptions {
    pub recover_pending_archives: bool,
    pub recover_pending_file_operations: bool,
    pub database_source: AppDatabaseSource,
}

impl Default for AppStartupOptions {
    fn default() -> Self {
        Self {
            recover_pending_archives: true,
            recover_pending_file_operations: true,
            database_source: AppDatabaseSource::default(),
        }
    }
}

impl AppStartupOptions {
    pub fn without_archive_recovery() -> Self {
        Self {
            recover_pending_archives: false,
            ..Self::default()
        }
    }

    pub fn with_archive_recovery() -> Self {
        Self {
            recover_pending_archives: true,
            ..Self::default()
        }
    }

    pub fn without_file_operation_recovery(mut self) -> Self {
        self.recover_pending_file_operations = false;
        self
    }

    /// Use an explicitly supplied SQLite file. This is intentionally available
    /// outside `cfg(test)` so external integration tests can use production-like
    /// file-backed DB behavior without touching the user's XDG database.
    pub fn with_database_path(mut self, database_path: impl Into<std::path::PathBuf>) -> Self {
        self.database_source = AppDatabaseSource::File(database_path.into());
        self
    }

    /// Use an isolated SQLite temp-file database owned by the constructed
    /// AppState. This exercises path creation and file-backed SQLite behavior
    /// while still cleaning up on drop.
    pub fn with_isolated_temp_database(mut self) -> Self {
        self.database_source = AppDatabaseSource::IsolatedTempFile;
        self
    }

    /// Use in-memory SQLite. Kept for fast smoke tests that do not need
    /// file-backed persistence, WAL, or migration-on-file behavior.
    pub fn with_in_memory_database(mut self) -> Self {
        self.database_source = AppDatabaseSource::InMemory;
        self
    }

    /// Explicitly opt into the production XDG database. Tests should almost
    /// never use this; the method is named to make such use obvious in review.
    pub fn with_production_database(mut self) -> Self {
        self.database_source = AppDatabaseSource::Production;
        self
    }

    #[cfg(test)]
    pub fn without_archive_recovery_for_tests() -> Self {
        Self::without_archive_recovery()
            .without_file_operation_recovery()
            .with_isolated_temp_database()
    }

    #[cfg(test)]
    pub fn with_archive_recovery_for_tests() -> Self {
        Self::with_archive_recovery()
            .without_file_operation_recovery()
            .with_isolated_temp_database()
    }
}

#[derive(Debug, Default)]
struct AppOwnedDatabaseDir(Option<std::path::PathBuf>);

impl AppOwnedDatabaseDir {
    fn none() -> Self {
        Self(None)
    }

    fn new(path: std::path::PathBuf) -> Self {
        Self(Some(path))
    }
}

impl Drop for AppOwnedDatabaseDir {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            if let Err(err) = std::fs::remove_dir_all(&path) {
                if err.kind() != std::io::ErrorKind::NotFound {
                    log::debug!("failed to remove isolated AppState database dir {}: {err}", path.display());
                }
            }
        }
    }
}

struct OpenedAppDatabase {
    db: crate::db::Database,
    owned_database_dir: AppOwnedDatabaseDir,
}

fn open_app_startup_database(startup_options: &AppStartupOptions) -> OpenedAppDatabase {
    let opened = match &startup_options.database_source {
        AppDatabaseSource::Production => crate::db::Database::open()
            .map(|db| OpenedAppDatabase { db, owned_database_dir: AppOwnedDatabaseDir::none() }),
        AppDatabaseSource::InMemory => crate::db::Database::open_memory()
            .map(|db| OpenedAppDatabase { db, owned_database_dir: AppOwnedDatabaseDir::none() }),
        AppDatabaseSource::File(path) => crate::db::Database::open_path(path)
            .map(|db| OpenedAppDatabase { db, owned_database_dir: AppOwnedDatabaseDir::none() }),
        AppDatabaseSource::IsolatedTempFile => open_isolated_app_database(),
    };

    match opened {
        Ok(opened) => {
            opened.db.prune_search_tag_cache(30);
            opened
        }
        Err(err) => {
            log::warn!("failed to open SQLite database ({err}); falling back to in-memory DB");
            let db = crate::db::Database::open_memory().unwrap_or_else(|memory_err| {
                panic!("failed to open fallback in-memory SQLite database: {memory_err}")
            });
            OpenedAppDatabase {
                db,
                owned_database_dir: AppOwnedDatabaseDir::none(),
            }
        }
    }
}

fn open_isolated_app_database() -> Result<OpenedAppDatabase, String> {
    let base = std::env::temp_dir().join(format!(
        "tonepoet-test-db-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&base)
        .map_err(|err| format!("create isolated AppState database dir {}: {err}", base.display()))?;
    let db_path = base.join("tonepoet.db");
    match crate::db::Database::open_path(&db_path) {
        Ok(db) => Ok(OpenedAppDatabase {
            db,
            owned_database_dir: AppOwnedDatabaseDir::new(base),
        }),
        Err(err) => {
            if let Err(cleanup_err) = std::fs::remove_dir_all(&base) {
                if cleanup_err.kind() != std::io::ErrorKind::NotFound {
                    log::debug!(
                        "failed to remove isolated AppState database dir after open failure {}: {cleanup_err}",
                        base.display()
                    );
                }
            }
            Err(err)
        }
    }
}

#[cfg(test)]
impl AppState {
    fn cleanup_test_archive_staging_on_drop(&mut self) {
        if let Some(pending) = self.pending_browse_archive_metadata.take() {
            pending.cancel_and_cleanup();
        }
        if let Some(pending) = self.pending_browse_archive_rename.take() {
            pending.cancel_and_cleanup();
        }
        if let Some(pending) = self.pending_browse_archive_delete.take() {
            pending.cancel_and_cleanup();
        }
        if let Some(context) = self.preserved_editor_archive_repackage.take() {
            context.cleanup_staging();
            if let Err(err) = self.db.delete_pending_archive_session(&context.archive_path) {
                log::warn!(
                    "test cleanup failed to delete pending archive session for {}: {err}",
                    context.archive_path.display()
                );
            }
        }
        if let Some(staging) = self.pending_archive_recovery_resume.take() {
            cleanup_archive_metadata_staging_dir(&staging.staging_dir);
            if let Err(err) = self.db.delete_pending_archive_session(&staging.archive_path) {
                log::warn!(
                    "test cleanup failed to delete recovered archive session for {}: {err}",
                    staging.archive_path.display()
                );
            }
        }
        self.pending_archive_recovery.clear();
    }
}

#[cfg(test)]
impl Drop for AppState {
    fn drop(&mut self) {
        self.cleanup_test_archive_staging_on_drop();
    }
}

fn app_state_default_startup_options() -> AppStartupOptions {
    // Keep `AppState::new(...)` as the production-default constructor. Unit-test
    // builds still inherit isolated database defaults through
    // `AppDatabaseSource::default()` under `cfg(test)`, but integration tests
    // link this library as normal production code. Those tests must call
    // `AppState::new_for_test(...)`, `new_for_test_with_db_path(...)`,
    // `new_with_database_path(...)`, or `new_with_database(...)` explicitly
    // instead of relying on process-path heuristics such as `target/.../deps`.
    AppStartupOptions::default()
}

impl AppState {
    pub fn new(config: TonepoetConfig) -> Self {
        Self::new_with_startup_options(config, app_state_default_startup_options())
    }

    pub fn new_with_startup_options(
        config: TonepoetConfig,
        startup_options: AppStartupOptions,
    ) -> Self {
        let opened = open_app_startup_database(&startup_options);
        Self::new_with_open_database(
            config,
            startup_options,
            opened.db,
            opened.owned_database_dir,
        )
    }

    /// Construct AppState with an already-open database. This is the most direct
    /// injection seam for integration tests and specialized harnesses; callers
    /// own any tempdir lifetime required by the supplied database path.
    pub fn new_with_database(config: TonepoetConfig, db: crate::db::Database) -> Self {
        Self::new_with_open_database(
            config,
            AppStartupOptions::default(),
            db,
            AppOwnedDatabaseDir::none(),
        )
    }

    /// Construct AppState with a production-like file-backed SQLite database at
    /// a caller-supplied path. This is available to integration tests because it
    /// is not hidden behind `cfg(test)`.
    pub fn new_with_database_path(
        config: TonepoetConfig,
        database_path: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self::new_with_startup_options(
            config,
            AppStartupOptions::default().with_database_path(database_path),
        )
    }

    pub fn new_for_test_with_db_path(
        config: TonepoetConfig,
        database_path: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self::new_with_startup_options(
            config,
            // Tests must not scan the process-global file-operation journal
            // directory at construction: concurrent journal tests point it at
            // their own tempdirs and an unrelated AppState would recover a
            // foreign plan into its Browse clipboard. Recovery-path tests call
            // startup_file_task_recovery() explicitly.
            AppStartupOptions::without_archive_recovery()
                .without_file_operation_recovery()
                .with_database_path(database_path),
        )
    }

    pub fn new_for_test_with_isolated_db(mut config: TonepoetConfig) -> Self {
        // The temp SQLite database is isolated, but queue persistence would
        // still fall back to the real user-level JSON file (first-run
        // migration on an empty database) and write test items back into it.
        // Tests must never read or mutate the developer's actual queue.
        config.conversion.persist_queue = false;
        Self::new_with_startup_options(
            config,
            // See new_for_test_with_db_path: never scan the process-global
            // file-operation journal directory from a test constructor.
            AppStartupOptions::without_archive_recovery()
                .without_file_operation_recovery()
                .with_isolated_temp_database(),
        )
    }

    fn new_with_open_database(
        config: TonepoetConfig,
        startup_options: AppStartupOptions,
        db: crate::db::Database,
        owned_database_dir: AppOwnedDatabaseDir,
    ) -> Self {
        let mut config = config;
        let configured_theme_slug = config.ui.theme.clone();
        let theme_overrides = crate::tui::theme::ThemeOverrides::load_default().unwrap_or_default();
        let (theme, mut theme_startup_status) = match crate::tui::theme::load_theme_draft(&configured_theme_slug) {
            Ok(draft) => {
                let resolved = crate::tui::theme::resolve_theme_draft(
                    &draft,
                    crate::tui::theme::ThemeApplyOptions::default(),
                    &theme_overrides,
                );
                config.ui.theme = draft.slug.clone();
                (resolved, None)
            }
            Err(_) => {
                let fallback_draft = crate::tui::theme::ThemePaletteDraft::from_palette(
                    crate::tui::theme::default_palette(),
                );
                let fallback = crate::tui::theme::resolve_theme_draft(
                    &fallback_draft,
                    crate::tui::theme::ThemeApplyOptions::default(),
                    &theme_overrides,
                );
                config.ui.theme = fallback.slug.to_string();
                (
                    fallback,
                    Some(format!(
                        "Unknown configured theme '{}'; using {}",
                        configured_theme_slug, fallback.name
                    )),
                )
            }
        };
        let mut pending_archive_recovery: std::collections::VecDeque<crate::db::PendingArchiveSessionRecovery> =
            std::collections::VecDeque::new();
        if startup_options.recover_pending_archives {
            if let Ok(sessions) = db.recover_pending_archive_sessions_at_startup() {
                if !sessions.is_empty() {
                    let valid = sessions.iter().filter(|session| !session.conflicted).count();
                    let conflicted = sessions.len().saturating_sub(valid);
                    pending_archive_recovery = sessions.into();
                    let archive_status = if conflicted > 0 {
                        format!("archive recovery: {valid} resumable staged session(s), {conflicted} conflict(s) need review")
                    } else {
                        format!("archive recovery: {valid} resumable staged session(s)")
                    };
                    theme_startup_status = Some(match theme_startup_status.take() {
                        Some(existing) => format!("{existing}; {archive_status}"),
                        None => archive_status,
                    });
                }
            }
        }

        let mut active_overlay = ActiveOverlay::None;
        let mut archive_recovery_prompt_active = false;
        if let Some(session) = pending_archive_recovery.front().cloned() {
            active_overlay = ActiveOverlay::Confirmation {
                message: archive_startup_recovery_prompt_message(&session),
                action: ConfirmAction::ArchiveStartupRecovery { session },
            };
            archive_recovery_prompt_active = true;
        }

        let theme_library = crate::tui::theme::ThemeLibrarySnapshot::load();

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
                    if let Err(error) = db.sync_queue(&items) {
                        log::error!(
                            "could not import the persisted JSON queue into SQLite: {}",
                            error
                        );
                        let queue_status = format!(
                            "queue persistence degraded: JSON import was retained because SQLite publication failed: {error}"
                        );
                        theme_startup_status = Some(match theme_startup_status.take() {
                            Some(existing) => format!("{existing}; {queue_status}"),
                            None => queue_status,
                        });
                    }
                }
            }
        }

        let mut output_options = OutputOptionsState::new();
        output_options.dest_path = config.conversion.default_destination.clone();
        output_options.actions = config.conversion.actions.clone();

        let initial_screen = AppScreen::from_config_name(&config.ui.default_screen);

        // Load recent files + bookmarks from DB.
        let recent = crate::tui::recent_files::RecentFilesState::load_from_db(&db);
        let bookmarks = crate::tui::bookmarks::BookmarksState::load_from_db(&db);
        // Import TOML presets into DB on first run.
        crate::tui::presets::import_presets_to_db(&db);
        let mut browse = crate::tui::browse::BrowseState::new_with_config(&config.browsing);
        if startup_options.recover_pending_file_operations {
            if let Some(recovery) = crate::tui::file_task_runtime::startup_file_task_recovery() {
                let destination = recovery
                    .destination_dir
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "the original destination".to_string());
                browse.filesystem_clipboard = Some(recovery.clipboard);
                browse.filesystem_clipboard_retry_plan = Some(recovery.retry_plan);
                let recovery_status = format!(
                    "file-operation recovery: {} interrupted job(s); restored the newest exact plan. Navigate to {} and paste to reconcile ({} deferred temp artifact(s), {} source quarantine(s)); journal {}",
                    recovery.total_pending_jobs,
                    destination,
                    recovery.temp_artifact_count,
                    recovery.quarantine_artifact_count,
                    recovery.journal_path.display(),
                );
                theme_startup_status = Some(match theme_startup_status.take() {
                    Some(existing) => format!("{existing}; {recovery_status}"),
                    None => recovery_status,
                });
            }
        }
        let file_task_verbose_degrade_notices = matches!(
            config.file_operations.status_verbosity,
            crate::config::FileOperationStatusVerbosity::Verbose
        );

        Self {
            config,
            theme,
            theme_overrides,
            theme_library,
            manager,
            db,
            _owned_database_dir: owned_database_dir,
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
                dest_path_last_click: None,
                metadata_field_last_click: None,
                pending_archive_preview: None,
            },
            probe_generation: 0,
            tui_tx: None,
            pending_browse_convert_expansion: None,
            preset: PresetState::default(),
            browse,
            inline_metadata_write_generation: 0,
            inline_metadata_write: None,
            queue_focus: QueueFocus::FileList,
            selected_index: 0,
            scroll_offset: 0,
            visible_height: 0,
            items_snapshot: Vec::new(),
            button_map: ButtonRenderMap::new(),
            double_click: DoubleClickState::default(),
            wizard: None,
            wizard_mouse_areas: None,
            wizard_target: WizardTarget::ConfigureAll,
            active_overlay,
            pending_editor_context_overlay: None,
            editor_context_target: None,
            host_clipboard_paste_generation: 0,
            last_file_task_progress: None,
            file_operation_undo: FileOperationUndoJournal::default(),
            #[cfg(test)]
            test_view_file_dispatches: None,
            file_task_verbose_degrade_notices,
            bulk_guard_bypass: None,
            bulk_guard_frozen_paths: None,
            browse_context_action_paths: None,
            pending_bulk_rename: None,
            pending_metadata_editor: None,
            pending_browse_archive_metadata: None,
            pending_browse_archive_rename: None,
            pending_browse_archive_delete: None,
            browse_archive_repackage: None,
            browse_archive_repackage_progress_session_id: None,
            preserved_editor_archive_repackage: None,
            pending_archive_recovery,
            pending_archive_recovery_resume: None,
            pending_archive_recovery_resume_conflicted: false,
            archive_recovery_prompt_active,
            quit_after_browse_archive_metadata_resolution: false,
            quit_after_browse_archive_repackage: false,
            quit_after_browse_archive_rename: false,
            quit_after_browse_archive_delete: false,
            deferred_browse_archive_screen_switch: None,
            deferred_browse_archive_exit: false,
            pending_cue_preview: None,
            pending_mb_select: None,
            tags_mb_operation_generation: 0,
            active_tags_mb_operation: None,
            active_gnudb_operation: None,
            active_cue_operation: None,
            active_completion_operations: std::collections::BTreeMap::new(),
            status_message: theme_startup_status.map(|message| (message, std::time::Instant::now())),
            processing_active: false,
            should_quit: false,
            force_redraw: false,
            auto_fix_on_complete: false,
            pending_ctdb_repair: None,
            auto_repair_on_ctdb_complete: false,
            last_browse_click: None,
            last_disc_browser_stream_click: None,
            pending_browse_rename: None,
            browse_inline_edit: None,
            browse_text_mouse_target: None,
            pending_inline_rename_resume: None,
            browse_info_focus: None,
            recent,
            bookmarks,
            config_focus: ConfigFocus::default(),
            keychain: KeychainState::default(),
            archive_passwords: std::collections::HashMap::new(),
            pending_archive_listing: None,
            archive_listing_generation: 0,
            archive_listing_cache: std::collections::HashMap::new(),
            archive_listing_cache_lru: std::collections::VecDeque::new(),
            archive_listing_cache_bytes: 0,
            split_cue_album_grouping_cache: std::collections::HashMap::new(),
            last_artwork_picker_dir: None,
            image_picker: new_terminal_image_picker(),
            image_picker_generation: 0,
            image_repaint_generation: 0,
            image_kitty_retransmit_generation: 0,
            last_image_kitty_retransmit_at: None,
            hover_target: None,
            analysis_results: Vec::new(),
            analysis_pending: 0,
            analysis_temp_dir: None,
            verify_results: Vec::new(),
            preemph_results: Vec::new(),
            compare_reference: Vec::new(),
            compare_results: Vec::new(),
            tool_check_cache: once_cell::sync::OnceCell::new(),
        }
    }

    /// Construct AppState with test-safe defaults in normal and integration-test
    /// builds. This is intentionally not gated by `cfg(test)`: Rust integration
    /// tests link the library as an ordinary dependency, so their harnesses need
    /// an explicit constructor that cannot touch the user's production XDG DB.
    pub fn new_for_test(config: TonepoetConfig) -> Self {
        Self::new_for_test_with_isolated_db(config)
    }

    pub fn set_config_focus(&mut self, focus: ConfigFocus) {
        self.config_focus = focus;
        self.keychain.focused = focus.keychain_focused();
    }

    pub fn cycle_config_focus(&mut self, forward: bool) {
        let next = if forward {
            self.config_focus.next()
        } else {
            self.config_focus.previous()
        };
        self.set_config_focus(next);
    }


    pub fn begin_browse_convert_expansion(
        &mut self,
        request: crate::tui::command::BrowseConvertExpansionRequest,
    ) -> (u64, tokio_util::sync::CancellationToken) {
        self.cancel_browse_convert_expansion();
        self.probe_generation = self.probe_generation.saturating_add(1);
        let generation = self.probe_generation;
        let cancel = tokio_util::sync::CancellationToken::new();
        self.pending_browse_convert_expansion = Some(PendingBrowseConvertExpansion {
            generation,
            request,
            cancel: cancel.clone(),
        });
        (generation, cancel)
    }

    pub fn browse_convert_expansion_pending_for(
        &self,
        generation: u64,
        request: &crate::tui::command::BrowseConvertExpansionRequest,
    ) -> bool {
        self.pending_browse_convert_expansion
            .as_ref()
            .map(|pending| pending.matches(generation, request))
            .unwrap_or(false)
    }

    pub fn complete_browse_convert_expansion(
        &mut self,
        generation: u64,
        request: &crate::tui::command::BrowseConvertExpansionRequest,
    ) -> bool {
        if self.browse_convert_expansion_pending_for(generation, request) {
            self.pending_browse_convert_expansion = None;
            true
        } else {
            false
        }
    }

    pub fn cancel_browse_convert_expansion(&mut self) -> bool {
        if let Some(pending) = self.pending_browse_convert_expansion.take() {
            pending.cancel();
            true
        } else {
            false
        }
    }

    /// Cancel a pending Browse Convert folder expansion because the Browse
    /// selection/navigation context that authorized it changed. This is more
    /// aggressive than stale-result rejection: it asks the blocking walker to
    /// stop early instead of letting a large scan continue until completion.
    pub fn cancel_browse_convert_expansion_for_browse_change(&mut self, reason: &str) -> bool {
        if self.cancel_browse_convert_expansion() {
            self.set_status(format!("folder expansion cancelled: {reason}"));
            true
        } else {
            false
        }
    }

    pub fn begin_inline_metadata_write(
        &mut self,
        path: std::path::PathBuf,
    ) -> (u64, crate::tui::probe::MetadataWriteCancelFlag) {
        if let Some(previous) = self.inline_metadata_write.take() {
            previous.cancel.cancel();
        }
        self.inline_metadata_write_generation =
            self.inline_metadata_write_generation.saturating_add(1);
        let operation_id = self.inline_metadata_write_generation;
        let cancel = crate::tui::probe::MetadataWriteCancelFlag::new();
        self.inline_metadata_write = Some(InlineMetadataWriteState {
            operation_id,
            path,
            cancel: cancel.clone(),
        });
        (operation_id, cancel)
    }

    pub fn inline_metadata_write_is_current(
        &self,
        operation_id: u64,
        path: &std::path::Path,
    ) -> bool {
        self.inline_metadata_write.as_ref().is_some_and(|state| {
            state.operation_id == operation_id && state.path.as_path() == path
        })
    }

    pub fn complete_inline_metadata_write(
        &mut self,
        operation_id: u64,
        path: &std::path::Path,
    ) -> bool {
        if self.inline_metadata_write_is_current(operation_id, path) {
            self.inline_metadata_write = None;
            true
        } else {
            false
        }
    }

    pub fn cancel_inline_metadata_write(&self) -> bool {
        if let Some(state) = &self.inline_metadata_write {
            state.cancel.cancel();
            true
        } else {
            false
        }
    }

    pub fn begin_archive_listing(
        &mut self,
        archive_path: std::path::PathBuf,
    ) -> (u64, tokio_util::sync::CancellationToken) {
        self.cancel_archive_listing();
        self.archive_listing_generation = self.archive_listing_generation.saturating_add(1);
        let id = self.archive_listing_generation;
        let cancel = tokio_util::sync::CancellationToken::new();
        self.pending_archive_listing = Some(PendingArchiveListing {
            id,
            archive_path,
            cancel: cancel.clone(),
            started_at: std::time::Instant::now(),
        });
        (id, cancel)
    }

    pub fn archive_listing_pending_for(&self, id: u64, archive_path: &std::path::Path) -> bool {
        self.pending_archive_listing
            .as_ref()
            .map(|pending| pending.id == id && pending.archive_path == archive_path)
            .unwrap_or(false)
    }

    pub fn complete_archive_listing(&mut self, id: u64, archive_path: &std::path::Path) -> bool {
        if self.archive_listing_pending_for(id, archive_path) {
            self.pending_archive_listing = None;
            true
        } else {
            false
        }
    }

    pub fn cancel_archive_listing(&mut self) -> bool {
        if let Some(pending) = self.pending_archive_listing.take() {
            pending.cancel.cancel();
            true
        } else {
            false
        }
    }

    pub fn cached_archive_listing(
        &mut self,
        key: &crate::tui::archive_listing::ArchiveListingCacheKey,
    ) -> Option<crate::tui::archive_listing::ArchiveListing> {
        let listing = self.archive_listing_cache.get(key).cloned();
        if listing.is_some() {
            self.touch_archive_listing_cache_key(key);
        }
        listing
    }

    pub fn insert_archive_listing_cache(
        &mut self,
        key: crate::tui::archive_listing::ArchiveListingCacheKey,
        listing: crate::tui::archive_listing::ArchiveListing,
    ) -> bool {
        let bytes = listing.estimated_cache_bytes();

        // A single enormous archive listing is useful once, but retaining it
        // would defeat the cache's memory bound. Remove any stale copy and
        // decline to cache the replacement.
        if bytes > ARCHIVE_LISTING_CACHE_MAX_BYTES {
            self.remove_archive_listing_cache_key(&key);
            return false;
        }

        if let Some(old) = self.archive_listing_cache.insert(key.clone(), listing) {
            self.archive_listing_cache_bytes = self
                .archive_listing_cache_bytes
                .saturating_sub(old.estimated_cache_bytes());
        }
        self.archive_listing_cache_bytes = self.archive_listing_cache_bytes.saturating_add(bytes);
        self.touch_archive_listing_cache_key(&key);
        self.evict_archive_listing_cache_over_budget();
        true
    }


    pub fn invalidate_archive_listing_cache_for_path(&mut self, path: &Path) {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let keys: Vec<_> = self
            .archive_listing_cache
            .keys()
            .filter(|key| key.path.as_path() == canonical.as_path() || key.path.as_path() == path)
            .cloned()
            .collect();
        for key in keys {
            self.remove_archive_listing_cache_key(&key);
        }
    }

    #[cfg(test)]
    fn archive_listing_cache_debug_state(&self) -> (usize, usize, usize) {
        (
            self.archive_listing_cache.len(),
            self.archive_listing_cache_lru.len(),
            self.archive_listing_cache_bytes,
        )
    }

    fn touch_archive_listing_cache_key(
        &mut self,
        key: &crate::tui::archive_listing::ArchiveListingCacheKey,
    ) {
        self.archive_listing_cache_lru.retain(|candidate| candidate != key);
        self.archive_listing_cache_lru.push_back(key.clone());
    }

    fn remove_archive_listing_cache_key(
        &mut self,
        key: &crate::tui::archive_listing::ArchiveListingCacheKey,
    ) -> bool {
        self.archive_listing_cache_lru.retain(|candidate| candidate != key);
        if let Some(old) = self.archive_listing_cache.remove(key) {
            self.archive_listing_cache_bytes = self
                .archive_listing_cache_bytes
                .saturating_sub(old.estimated_cache_bytes());
            true
        } else {
            false
        }
    }

    fn evict_archive_listing_cache_over_budget(&mut self) {
        while self.archive_listing_cache.len() > ARCHIVE_LISTING_CACHE_MAX_ENTRIES
            || self.archive_listing_cache_bytes > ARCHIVE_LISTING_CACHE_MAX_BYTES
        {
            let Some(oldest_key) = self.archive_listing_cache_lru.pop_front() else {
                self.archive_listing_cache.clear();
                self.archive_listing_cache_bytes = 0;
                break;
            };
            if let Some(old) = self.archive_listing_cache.remove(&oldest_key) {
                self.archive_listing_cache_bytes = self
                    .archive_listing_cache_bytes
                    .saturating_sub(old.estimated_cache_bytes());
            }
        }
    }

    pub fn refresh_theme_library(&mut self) {
        self.theme_library = crate::tui::theme::ThemeLibrarySnapshot::load();
    }

    pub fn set_ui_theme(&mut self, theme_slug: &str) {
        let draft = match crate::tui::theme::load_theme_draft(theme_slug) {
            Ok(draft) => draft,
            Err(_) => {
                self.set_status(format!("Unknown theme: {}", theme_slug));
                return;
            }
        };
        let next_theme = crate::tui::theme::resolve_theme_draft(
            &draft,
            crate::tui::theme::ThemeApplyOptions::default(),
            &self.theme_overrides,
        );

        self.theme = next_theme;
        let theme_slug = draft.slug.clone();
        self.config.ui.theme = theme_slug.clone();
        self.retheme_open_file_picker_surfaces();
        if let Err(err) = self.config.update(|latest| latest.ui.theme = theme_slug.clone()) {
            self.set_status(format!("Theme changed, but config save failed: {}", err));
        } else {
            self.set_status(format!("Theme: {}", draft.name));
        }
        self.force_redraw = true;
    }

    /// Re-derive concrete file-picker/progress-dialog themes for retained
    /// picker-owned state after the Tonepoet theme changes.
    ///
    /// New picker sessions receive `AppState::theme` through construction, but
    /// open picker and progress sessions intentionally own concrete
    /// `FilePickerTheme` values. Retint them in place so Config-screen theme
    /// changes apply immediately to already-open overlays as well as future
    /// ones.
    pub fn retheme_open_file_picker_surfaces(&mut self) {
        let picker_theme = crate::tui::keybindings::file_picker_theme_from_theme(&self.theme);

        match &mut self.active_overlay {
            ActiveOverlay::MetadataEditor(state) => {
                if let Some(file_picker) = state.file_picker.as_mut() {
                    file_picker.set_theme(picker_theme.clone());
                }
            }
            ActiveOverlay::FilePicker(session) => {
                session.set_theme(picker_theme.clone());
            }
            ActiveOverlay::FileTaskProgress(session) => {
                session.set_theme(picker_theme.clone());
            }
            _ => {}
        }

        if let Some((_, progress)) = self.last_file_task_progress.as_mut() {
            progress.set_theme(picker_theme.clone());
        }

        if let Some(state) = self.pending_metadata_editor.as_mut() {
            if let Some(file_picker) = state.file_picker.as_mut() {
                file_picker.set_theme(picker_theme);
            }
        }
    }

    /// Install a live file-task overlay and seed its session-owned retained
    /// state before any worker update can race with presentation changes.
    ///
    /// `last_file_task_progress` therefore tracks the newest task from launch,
    /// not only after a terminal overlay happens to remain open.
    pub fn install_file_task_progress(&mut self, session: FileTaskProgressSession) {
        debug_assert!(session.is_live_task());
        self.last_file_task_progress = Some((session.session_id, session.progress.clone()));
        self.active_overlay = ActiveOverlay::FileTaskProgress(session);
    }

    pub fn cycle_ui_theme(&mut self, forward: bool) {
        let slug = crate::tui::theme::next_theme_slug_in_library(self.theme.slug, forward);
        self.set_ui_theme(&slug);
    }

    /// Refresh terminal graphics protocol detection after a terminal resize.
    ///
    /// `ratatui-image` protocol state is encoded for terminal cell geometry.
    /// Re-detecting the picker and advancing this generation forces metadata
    /// and file-picker preview caches to rebuild instead of reusing stale
    /// StatefulProtocol values after the terminal size/cell metrics change.
    pub fn refresh_image_picker_after_resize(&mut self) {
        self.image_picker = new_terminal_image_picker();
        self.image_picker_generation = self.image_picker_generation.saturating_add(1);
        self.last_image_kitty_retransmit_at = None;

        self.invalidate_terminal_image_preview_caches();
        self.request_image_preview_repaint();
        self.force_redraw = true;
    }

    /// Advance image-preview repaint/retransmit generations after terminal-side
    /// graphics damage, such as mouse movement over Ghostty's Kitty graphics
    /// layer.
    ///
    /// The repaint generation remains as a cheap ratatui-buffer nudge for
    /// non-Kitty protocol cells. Kitty/Ghostty additionally gets a separate,
    /// rate-limited retransmit generation so cached decoded pixels can be
    /// re-wrapped in a fresh StatefulProtocol without conflating mouse damage
    /// with terminal resize/cell-metric changes.
    pub fn request_image_preview_repaint(&mut self) {
        self.image_repaint_generation = self.image_repaint_generation.saturating_add(1);
        self.request_kitty_image_preview_retransmit();
    }

    fn request_kitty_image_preview_retransmit(&mut self) {
        if self.image_picker.protocol_type != ratatui_image::picker::ProtocolType::Kitty {
            return;
        }

        let now = Instant::now();
        let should_retransmit = self
            .last_image_kitty_retransmit_at
            .map(|last| now.duration_since(last) >= KITTY_IMAGE_RETRANSMIT_MIN_INTERVAL)
            .unwrap_or(true);
        if should_retransmit {
            self.image_kitty_retransmit_generation =
                self.image_kitty_retransmit_generation.saturating_add(1);
            self.last_image_kitty_retransmit_at = Some(now);
        }
    }


    fn force_halfblocks_for_unsafe_terminal_image_protocol(&mut self) {
        if !enforce_safe_terminal_image_picker_protocol_for_current_environment(
            &mut self.image_picker,
        ) {
            return;
        }

        self.image_picker_generation = self.image_picker_generation.saturating_add(1);
        self.image_kitty_retransmit_generation = 0;
        self.last_image_kitty_retransmit_at = None;
        self.invalidate_terminal_image_preview_caches();
    }

    fn invalidate_terminal_image_preview_caches(&mut self) {
        match &mut self.active_overlay {
            ActiveOverlay::MetadataEditor(state) => {
                state.invalidate_artwork_preview_cache();
                if let Some(file_picker) = state.file_picker.as_mut() {
                    file_picker.picker.invalidate_image_preview_cache();
                }
            }
            ActiveOverlay::FilePicker(session) => {
                session.picker.invalidate_image_preview_cache();
            }
            _ => {}
        }

        if let Some(state) = self.pending_metadata_editor.as_mut() {
            state.invalidate_artwork_preview_cache();
            if let Some(file_picker) = state.file_picker.as_mut() {
                file_picker.picker.invalidate_image_preview_cache();
            }
        }
    }

    /// Advance image-preview loading/encoding outside the render path.
    ///
    /// Render records the desired preview pane geometry. This update pass polls
    /// non-blocking decode workers and, once decoded pixels and geometry are
    /// both available, builds terminal protocol state from the startup-owned
    /// image picker so the next frame can render without synchronous disk/tag/
    /// image decode or protocol creation work. For Kitty/Ghostty, a separate
    /// rate-limited retransmit generation can also rebuild protocol state from
    /// cached decoded pixels after mouse-driven graphics-layer damage.
    pub fn prepare_image_preview_protocols(&mut self) {
        // This must run before any preview path can wrap decoded pixels in a
        // terminal protocol. In tmux/byobu it downgrades any cached Kitty picker
        // state to Halfblocks and invalidates old protocol caches first.
        self.force_halfblocks_for_unsafe_terminal_image_protocol();

        let protocol_generation = self.image_picker_generation;
        let retransmit_generation = self.image_kitty_retransmit_generation;
        let mut changed = false;
        match &mut self.active_overlay {
            ActiveOverlay::MetadataEditor(state) => {
                changed |= state.prepare_artwork_preview_protocol(
                    &mut self.image_picker,
                    protocol_generation,
                    retransmit_generation,
                );
                if let Some(file_picker) = state.file_picker.as_mut() {
                    changed |= file_picker
                        .picker
                        .prepare_image_preview_protocol_with_retransmit_generation(
                            &mut self.image_picker,
                            protocol_generation,
                            retransmit_generation,
                        );
                }
            }
            ActiveOverlay::FilePicker(session) => {
                changed |= session
                    .picker
                    .prepare_image_preview_protocol_with_retransmit_generation(
                        &mut self.image_picker,
                        protocol_generation,
                        retransmit_generation,
                    );
            }
            _ => {}
        }
        // No force_redraw — the next normal render cycle picks up the new
        // protocol. force_redraw triggers terminal.clear() which causes a
        // visible full-screen flash.
        let _ = changed;
    }

    /// Set a status message that will auto-clear after 5 seconds
    /// Save the conversion queue to both JSON (legacy) and SQLite.
    pub fn save_queue(&mut self) {
        if !self.config.conversion.persist_queue {
            return;
        }
        let mut errors = Vec::new();
        // Legacy JSON save (kept for backward compat during migration).
        if let Err(error) = self.manager.save_queue(true) {
            log::error!("could not persist conversion queue JSON: {}", error);
            errors.push(format!("JSON: {error}"));
        }
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
            if let Err(error) = self.db.sync_queue(&items) {
                log::error!("could not persist conversion queue SQLite state: {}", error);
                errors.push(format!("SQLite: {error}"));
            }
        } else {
            errors.push("queue is busy".to_string());
        }
        if !errors.is_empty() {
            self.set_status(format!(
                "Queue persistence degraded; in-memory work is unchanged ({})",
                errors.join("; ")
            ));
        }
    }

    /// Feature gate for the conversion-actions UI surfaces (Output Options
    /// row, :actions family). Config-defined pipelines still run.
    pub fn conversion_actions_ui_enabled(&self) -> bool {
        self.config.ui.show_conversion_actions
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), std::time::Instant::now()));
    }

    #[must_use]
    pub fn file_operation_status_is_verbose(&self) -> bool {
        matches!(
            self.config.file_operations.status_verbosity,
            crate::config::FileOperationStatusVerbosity::Verbose
        )
    }

    /// Emit routine file-operation narration only when the persisted verbosity
    /// preference requests it. Errors, partial results, and degraded-operation
    /// warnings must continue to use `set_status` directly so quiet mode never
    /// hides a result that needs attention. Full diagnostics remain available
    /// through `:messages` independently of this presentation preference.
    pub fn set_routine_file_operation_status(&mut self, msg: impl Into<String>) {
        if self.file_operation_status_is_verbose() {
            self.set_status(msg);
        }
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
        if let Some(progress) = self
            .active_completion_operations
            .get(&CompletionOperationKind::Verify)
            .and_then(|operation| operation.batch)
            .filter(|progress| progress.remaining > 0)
        {
            self.status_message = Some((
                format!(
                    "Verifying... ({}/{})",
                    progress.total.saturating_sub(progress.remaining),
                    progress.total
                ),
                std::time::Instant::now(),
            ));
            return;
        }
        if let Some(progress) = self
            .active_completion_operations
            .get(&CompletionOperationKind::Compare)
            .and_then(|operation| operation.batch)
            .filter(|progress| progress.remaining > 0)
        {
            self.status_message = Some((
                format!(
                    "Comparing... ({}/{})",
                    progress.total.saturating_sub(progress.remaining),
                    progress.total
                ),
                std::time::Instant::now(),
            ));
            return;
        }
        if let Some(progress) = self
            .active_completion_operations
            .get(&CompletionOperationKind::Preemphasis)
            .and_then(|operation| operation.batch)
            .filter(|progress| progress.remaining > 0)
        {
            self.status_message = Some((
                format!(
                    "Detecting pre-emphasis... ({}/{})",
                    progress.total.saturating_sub(progress.remaining),
                    progress.total
                ),
                std::time::Instant::now(),
            ));
            return;
        }
        if let Some(pending) = &self.pending_archive_listing {
            let elapsed = pending.started_at.elapsed().as_secs();
            let name = pending
                .archive_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| pending.archive_path.display().to_string());
            self.status_message = Some((
                format!("Listing archive {}... {}s elapsed; Esc cancels", name, elapsed),
                std::time::Instant::now(),
            ));
            return;
        }
        if let Some(pending) = &self.pending_browse_convert_expansion {
            let folder_count = pending
                .request
                .selection_snapshot
                .iter()
                .filter(|path| path.is_dir())
                .count();
            let label = if folder_count > 1 {
                "selected folders"
            } else {
                "folder"
            };
            self.status_message = Some((
                format!("Expanding {label}... Esc or navigation cancels"),
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

    /// Seed the Convert screen from CLI `tonepoet tui <paths>` arguments.
    ///
    /// Every concrete path is classified by the authoritative direct-source
    /// admission policy before any source or screen state changes. Ordinary
    /// audio files may form a batch. Archive previews, CUE sheets, and disc
    /// images are singleton workflows and are therefore rejected atomically
    /// when mixed with any other supported path. Unsupported, missing, and
    /// directory paths are logged and skipped; if none remain, this method does
    /// not mutate the current source or screen.
    pub fn seed_from_cli_paths(&mut self, paths: Vec<PathBuf>) {
        self.seed_from_cli_paths_with_archive_starter(
            paths,
            install_archive_preview_convert_source,
        );
    }

    fn seed_from_cli_paths_with_archive_starter<F>(
        &mut self,
        paths: Vec<PathBuf>,
        start_archive: F,
    ) where
        F: FnOnce(
            &mut AppState,
            PathBuf,
            tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
        ) -> Result<ArchivePreviewStarted, ArchivePreviewStartError>,
    {
        use crate::convert::source_admission::{direct_source_kind, DirectSourceKind};

        let original_count = paths.len();
        let mut admitted: Vec<(PathBuf, DirectSourceKind)> = Vec::new();

        for path in paths {
            if !path.exists() {
                log::warn!("cli: path does not exist: {}", path.display());
                continue;
            }
            if path.is_dir() {
                log::warn!(
                    "cli: directories not supported in TUI mode — use `tonepoet convert <dir>` or navigate via `:cd` on the Browse screen: {}",
                    path.display()
                );
                continue;
            }
            if !path.is_file() {
                log::warn!(
                    "cli: non-regular source path skipped: {}",
                    path.display()
                );
                continue;
            }

            match direct_source_kind(&path) {
                Some(kind) => admitted.push((path, kind)),
                None => log::warn!(
                    "cli: unsupported conversion source skipped: {}",
                    path.display()
                ),
            }
        }

        let skipped = original_count.saturating_sub(admitted.len());
        if admitted.is_empty() {
            if original_count > 0 {
                self.set_status(format!(
                    "cli: {} unsupported or invalid path(s) skipped; no source loaded; see log",
                    original_count
                ));
            }
            return;
        }

        // Archives, CUE sheets, and disc images each expand into a specialized
        // source model. Combining one with another path would either discard
        // its track/container semantics or silently ignore a valid argument.
        // Refuse the entire supported set before any state mutation instead.
        if admitted.len() > 1
            && admitted
                .iter()
                .any(|(_, kind)| *kind != DirectSourceKind::Audio)
        {
            let supported_count = admitted.len();
            log::warn!(
                "cli: incompatible supported source mix refused atomically: {:?}",
                admitted
                    .iter()
                    .map(|(path, kind)| (path.display().to_string(), *kind))
                    .collect::<Vec<_>>()
            );
            self.set_status(format!(
                "cli: archives, CUE sheets, and disc images must be opened one at a time; no source loaded ({} supported, {} skipped)",
                supported_count, skipped
            ));
            return;
        }

        if admitted.len() == 1 && admitted[0].1 == DirectSourceKind::ArchivePreview {
            let (archive_path, _) = admitted
                .pop()
                .expect("one admitted CLI path must contain one element");
            let Some(tx) = self.tui_tx.clone() else {
                self.set_status(
                    "cli: cannot open archive preview because the TUI worker channel is unavailable",
                );
                return;
            };

            let archive_name = archive_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            match start_archive(self, archive_path, tx) {
                Ok(started) => {
                    debug_assert_eq!(self.probe_generation, started.generation);
                    debug_assert_eq!(
                        started.archive_path.file_name().and_then(|name| name.to_str()),
                        Some(archive_name.as_str())
                    );
                    if skipped > 0 {
                        self.set_status(format!(
                            "Extracting archive: {} ({} skipped, see log)",
                            archive_name, skipped
                        ));
                    }
                }
                Err(error) => {
                    log::warn!("cli: archive preview was not started: {error}");
                    // Preserve the exact preflight failure. In particular, do
                    // not replace secret-store errors with a false extraction
                    // status merely because other CLI arguments were skipped.
                    self.set_status(error.to_string());
                }
            }
            return;
        }

        let mut valid: Vec<PathBuf> = admitted.into_iter().map(|(path, _)| path).collect();
        if valid.len() > 1 {
            // `SourceMode::from_paths` sorts batch paths. Sort before probing so
            // the first path's info/metadata is attached to the same cursor-0
            // path that the resulting batch exposes.
            valid.sort();
        }
        let valid_count = valid.len();
        let first = valid[0].clone();
        let (info, metadata, probe_notice) = if is_cue_sheet_path_for_preview(&first) {
            match probe_cue_proxy_source(&first) {
                Ok(result) => (result.info, result.metadata, result.probe_notice),
                Err(error) => {
                    log::warn!(
                        "cli: CUE proxy probe failed for {}: {}",
                        first.display(),
                        error
                    );
                    (
                        None,
                        crate::tui::probe::SourceMetadata::default(),
                        Some(format!(
                            "CUE proxy probe failed: {}; set format manually",
                            error
                        )),
                    )
                }
            }
        } else {
            let info = match crate::tui::probe::probe_audio(&first) {
                Ok(info) => Some(info),
                Err(error) => {
                    log::warn!("cli: probe failed for {}: {}", first.display(), error);
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

        // Build the mode (Single for one direct source, Batch for an all-audio
        // set) and populate first-file probe/metadata in the correct variant.
        let mut mode = if valid_count == 1 {
            SourceMode::from_single_with_probe_notice(
                first.clone(),
                None,
                SourceMetadata::default(),
                probe_notice.clone(),
            )
        } else {
            debug_assert!(valid.iter().all(|path| {
                direct_source_kind(path) == Some(DirectSourceKind::Audio)
            }));
            SourceMode::from_paths(valid)
        };
        match &mut mode {
            SourceMode::Single {
                info: slot,
                metadata: metadata_slot,
                probe_notice: single_probe_notice,
                ..
            } => {
                *slot = info;
                *metadata_slot = metadata;
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
                metadata: metadata_slot,
                ..
            } => {
                *slot = info;
                *metadata_slot = metadata;
            }
            SourceMode::Empty => {
                unreachable!("non-empty admitted CLI paths cannot produce an empty source")
            }
        }
        self.convert.set_source_mode(mode);
        // `set_source_mode` only installs the source and updates DSD side effects.
        // CLI-seeded CUE proxy info must drive the same sample-rate, bit-depth,
        // dither, and resampler defaults as Browse/queue probe completion.
        self.convert.apply_source_defaults();

        // Record the first file in the recent-files history.
        self.recent.record_use_with_db(&first, &self.db);

        // CLI file arguments intentionally land on Convert for review. This is
        // a permanent load intent, matching :e, Browse activation, and recent
        // sources rather than a cancelable return-to-previous-screen flow.
        self.current_screen = AppScreen::Convert;

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

#[cfg(test)]
mod cli_seed_admission_tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    fn write_file(path: &Path, contents: &[u8]) {
        std::fs::write(path, contents).expect("write CLI seed fixture");
    }

    fn install_existing_source(app: &mut AppState, path: PathBuf) {
        app.convert.set_source_mode(SourceMode::Single {
            path,
            info: None,
            metadata: SourceMetadata::default(),
            probe_notice: None,
        });
    }

    fn write_sacd_iso_fixture(path: &Path) {
        const SECTOR_SIZE: u64 = 2_048;
        const MASTER_TOC_LSN: u64 = 510;
        const MASTER_TOC_MAGIC: &[u8; 8] = b"SACDMTOC";
        let mut file = std::fs::File::create(path).expect("create SACD ISO fixture");
        file.set_len((MASTER_TOC_LSN + 1) * SECTOR_SIZE)
            .expect("size SACD ISO fixture");
        file.seek(SeekFrom::Start(MASTER_TOC_LSN * SECTOR_SIZE))
            .expect("seek SACD ISO fixture");
        file.write_all(MASTER_TOC_MAGIC)
            .expect("write SACD ISO magic");
    }

    #[test]
    fn unsupported_cli_paths_preserve_existing_source_and_screen() {
        for name in ["cover.jpg", "notes.txt", "unknown.bin", "generic.iso"] {
            let temp = tempfile::tempdir().expect("tempdir");
            let rejected = temp.path().join(name);
            write_file(&rejected, b"not a supported source");
            let existing = temp.path().join("existing.flac");
            write_file(&existing, b"existing source");

            let mut app = AppState::new_for_test(TonepoetConfig::default());
            app.current_screen = AppScreen::Queue;
            install_existing_source(&mut app, existing.clone());

            app.seed_from_cli_paths(vec![rejected]);

            assert_eq!(app.current_screen, AppScreen::Queue, "{name}");
            assert_eq!(
                app.convert.source.mode.current_path(),
                Some(&existing),
                "{name}"
            );
            let status = app
                .status_message
                .as_ref()
                .map(|(message, _)| message.as_str())
                .unwrap_or("");
            assert!(status.contains("no source loaded"), "{name}: {status}");
        }
    }

    #[test]
    fn every_supported_archive_cli_seed_requires_the_archive_preview_channel() {
        for name in [
            "album.7z",
            "album.zip",
            "album.rar",
            "album.tar",
            "album.cab",
            "album.dmg",
            "album.tgz",
            "album.tbz2",
            "album.txz",
            "album.tar.gz",
            "album.tar.bz2",
            "album.tar.xz",
            "album.tar.zst",
            "album.tar.lz",
            "album.tar.lzma",
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let archive = temp.path().join(name);
            write_file(&archive, b"archive fixture");
            let existing = temp.path().join("existing.flac");
            write_file(&existing, b"existing source");

            let mut app = AppState::new_for_test(TonepoetConfig::default());
            app.current_screen = AppScreen::Queue;
            install_existing_source(&mut app, existing.clone());

            app.seed_from_cli_paths(vec![archive]);

            assert_eq!(app.current_screen, AppScreen::Queue, "{name}");
            assert_eq!(
                app.convert.source.mode.current_path(),
                Some(&existing),
                "{name}"
            );
            assert!(app.convert.pending_archive_preview.is_none(), "{name}");
            let status = app
                .status_message
                .as_ref()
                .map(|(message, _)| message.as_str())
                .unwrap_or("");
            assert!(
                status.contains("worker channel is unavailable"),
                "{name}: {status}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn single_archive_cli_seed_enters_archive_preview_workflow() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        let rejected = temp.path().join("cover.jpg");
        write_file(&archive, b"archive fixture");
        write_file(&rejected, b"image fixture");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        app.tui_tx = Some(tx);

        app.seed_from_cli_paths(vec![rejected, archive.clone()]);

        assert_eq!(app.current_screen, AppScreen::Convert);
        assert_eq!(app.convert.source.mode.current_path(), Some(&archive));
        assert!(
            app.convert
                .pending_archive_preview_matches(app.probe_generation, &archive),
            "CLI archive must use the asynchronous archive-preview owner"
        );
        assert_eq!(
            app.convert.source.mode.persistent_probe_notice(),
            Some(ARCHIVE_PREVIEW_EXTRACTING_NOTICE)
        );
        let status = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.as_str())
            .unwrap_or("");
        assert!(status.contains("1 skipped"), "{status}");
        app.convert.clear_pending_archive_preview();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cli_archive_password_failure_is_failure_atomic_and_preserves_the_real_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        let rejected = temp.path().join("cover.jpg");
        let existing = temp.path().join("existing.flac");
        write_file(&archive, b"archive fixture");
        write_file(&rejected, b"image fixture");
        write_file(&existing, b"existing source");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Queue;
        app.previous_screen = Some(AppScreen::Browse);
        app.probe_generation = 41;
        install_existing_source(&mut app, existing.clone());
        app.convert.metadata.title = Some("Existing title".to_string());
        app.convert.metadata.artist = Some("Existing artist".to_string());
        app.convert.metadata.album = Some("Existing album".to_string());
        app.convert.metadata.album_artist_for_conversion =
            Some("Existing album artist".to_string());
        app.convert.metadata.genre = Some("Existing genre".to_string());
        app.convert.metadata.year = Some("1973".to_string());
        app.recent.record_use_with_db(&existing, &app.db);

        let source_before = format!("{:?}", app.convert.source.mode);
        let metadata_before = (
            app.convert.metadata.title.clone(),
            app.convert.metadata.artist.clone(),
            app.convert.metadata.album.clone(),
            app.convert.metadata.album_artist_for_conversion.clone(),
            app.convert.metadata.genre.clone(),
            app.convert.metadata.year.clone(),
        );
        let format_before = ConvertProbeFormatSnapshot::capture(&app.convert.format);
        let recent_before = app
            .recent
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let generation_before = app.probe_generation;
        let screen_before = app.current_screen;
        let previous_screen_before = app.previous_screen;

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        app.tui_tx = Some(tx);
        app.seed_from_cli_paths_with_archive_starter(
            vec![rejected, archive.clone()],
            |app, path, tx| {
                install_archive_preview_convert_source_with_password_resolver(
                    app,
                    path,
                    tx,
                    |_app, path| {
                        Err(format!(
                            "injected secret-store failure for '{}'",
                            path.display()
                        ))
                    },
                )
            },
        );

        assert_eq!(app.current_screen, screen_before);
        assert_eq!(app.previous_screen, previous_screen_before);
        assert_eq!(app.convert.source.mode.current_path(), Some(&existing));
        assert_eq!(format!("{:?}", app.convert.source.mode), source_before);
        assert_eq!(
            (
                app.convert.metadata.title.clone(),
                app.convert.metadata.artist.clone(),
                app.convert.metadata.album.clone(),
                app.convert.metadata.album_artist_for_conversion.clone(),
                app.convert.metadata.genre.clone(),
                app.convert.metadata.year.clone(),
            ),
            metadata_before
        );
        assert_eq!(
            ConvertProbeFormatSnapshot::capture(&app.convert.format),
            format_before
        );
        assert_eq!(app.probe_generation, generation_before);
        assert!(app.convert.pending_archive_preview.is_none());
        assert_eq!(
            app.recent
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>(),
            recent_before
        );
        assert!(rx.try_recv().is_err(), "no preview worker message may be emitted");

        let status = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.as_str())
            .unwrap_or("");
        assert!(status.contains("injected secret-store failure"), "{status}");
        assert!(status.contains("operation was not started"), "{status}");
        assert!(!status.contains("Extracting archive"), "{status}");
        assert!(!status.contains("skipped"), "{status}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closed_archive_worker_channel_is_failure_atomic() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        let existing = temp.path().join("existing.flac");
        write_file(&archive, b"archive fixture");
        write_file(&existing, b"existing source");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Queue;
        app.previous_screen = Some(AppScreen::Browse);
        app.probe_generation = 73;
        install_existing_source(&mut app, existing.clone());
        app.convert.metadata.title = Some("Existing title".to_string());
        app.recent.record_use_with_db(&existing, &app.db);

        let source_before = format!("{:?}", app.convert.source.mode);
        let metadata_before = (
            app.convert.metadata.title.clone(),
            app.convert.metadata.artist.clone(),
            app.convert.metadata.album.clone(),
            app.convert.metadata.album_artist_for_conversion.clone(),
            app.convert.metadata.genre.clone(),
            app.convert.metadata.year.clone(),
        );
        let recent_before = app
            .recent
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let generation_before = app.probe_generation;
        let screen_before = app.current_screen;
        let previous_screen_before = app.previous_screen;

        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx);
        let error = install_archive_preview_convert_source(&mut app, archive, tx)
            .expect_err("closed worker channel must reject archive activation");

        assert!(matches!(error, ArchivePreviewStartError::WorkerChannelClosed));
        assert_eq!(app.current_screen, screen_before);
        assert_eq!(app.previous_screen, previous_screen_before);
        assert_eq!(app.convert.source.mode.current_path(), Some(&existing));
        assert_eq!(format!("{:?}", app.convert.source.mode), source_before);
        assert_eq!(
            (
                app.convert.metadata.title.clone(),
                app.convert.metadata.artist.clone(),
                app.convert.metadata.album.clone(),
                app.convert.metadata.album_artist_for_conversion.clone(),
                app.convert.metadata.genre.clone(),
                app.convert.metadata.year.clone(),
            ),
            metadata_before
        );
        assert_eq!(app.probe_generation, generation_before);
        assert!(app.convert.pending_archive_preview.is_none());
        assert_eq!(
            app.recent
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>(),
            recent_before
        );
    }

    #[test]
    fn rejected_paths_may_be_skipped_around_one_audio_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let audio = temp.path().join("track.flac");
        let image = temp.path().join("cover.jpg");
        write_file(&audio, b"audio fixture");
        write_file(&image, b"image fixture");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.seed_from_cli_paths(vec![image, audio.clone()]);

        assert_eq!(app.current_screen, AppScreen::Convert);
        assert_eq!(app.convert.source.mode.current_path(), Some(&audio));
        let status = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.as_str())
            .unwrap_or("");
        assert!(status.contains("1 skipped"), "{status}");
    }

    #[test]
    fn multiple_audio_cli_paths_form_an_audio_only_batch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("z-last.flac");
        let second = temp.path().join("a-first.wav");
        write_file(&first, b"first audio fixture");
        write_file(&second, b"second audio fixture");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.seed_from_cli_paths(vec![first.clone(), second.clone()]);

        assert_eq!(app.current_screen, AppScreen::Convert);
        match &app.convert.source.mode {
            SourceMode::Batch { paths, .. } => {
                assert_eq!(paths, &vec![second, first]);
            }
            other => panic!("expected CLI audio batch, got {other:?}"),
        }
    }

    #[test]
    fn specialized_cli_sources_are_singleton_and_mixed_sets_are_atomic() {
        let temp = tempfile::tempdir().expect("tempdir");
        let audio = temp.path().join("track.flac");
        let archive = temp.path().join("album.zip");
        let cue = temp.path().join("album.cue");
        let disc = temp.path().join("album.iso");
        let existing = temp.path().join("existing.flac");
        write_file(&audio, b"audio fixture");
        write_file(&archive, b"archive fixture");
        write_file(
            &cue,
            b"FILE \"track.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        );
        write_sacd_iso_fixture(&disc);
        write_file(&existing, b"existing source");

        for paths in [
            vec![archive.clone(), audio.clone()],
            vec![cue.clone(), audio.clone()],
            vec![disc.clone(), audio.clone()],
            vec![archive.clone(), cue.clone()],
        ] {
            let mut app = AppState::new_for_test(TonepoetConfig::default());
            app.current_screen = AppScreen::Queue;
            install_existing_source(&mut app, existing.clone());

            app.seed_from_cli_paths(paths);

            assert_eq!(app.current_screen, AppScreen::Queue);
            assert_eq!(app.convert.source.mode.current_path(), Some(&existing));
            assert!(app.convert.pending_archive_preview.is_none());
            let status = app
                .status_message
                .as_ref()
                .map(|(message, _)| message.as_str())
                .unwrap_or("");
            assert!(status.contains("must be opened one at a time"), "{status}");
        }
    }

    #[test]
    fn single_supported_disc_image_is_not_rejected_by_cli_admission() {
        let temp = tempfile::tempdir().expect("tempdir");
        let disc = temp.path().join("album.iso");
        write_sacd_iso_fixture(&disc);

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.seed_from_cli_paths(vec![disc.clone()]);

        assert_eq!(app.current_screen, AppScreen::Convert);
        assert_eq!(app.convert.source.mode.current_path(), Some(&disc));
        let status = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.as_str())
            .unwrap_or("");
        assert!(!status.contains("unsupported"), "{status}");
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
    if target_rate_hz == SOURCE_SAMPLE_RATE_SENTINEL {
        // The concrete rate is unavailable at the TUI boundary. The pipeline
        // performs the same validation after resolving RateTarget::Source, so
        // rejecting here would make every shaped ID unusable in a source-coupled
        // preset without adding safety.
        return Ok(());
    }

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
mod source_coupled_format_pill_tests {
    use super::*;

    #[test]
    fn source_choices_are_first_without_changing_historical_defaults() {
        let format = FormatState::new();

        assert_eq!(
            format.sample_rate.options.first().map(|option| (option.value, option.label.as_str())),
            Some((SOURCE_SAMPLE_RATE_SENTINEL, "source"))
        );
        assert_eq!(
            format.bit_depth.options.first().map(|option| (option.value, option.label.as_str())),
            Some((BitDepthChoice::Source, "source"))
        );
        assert_eq!(*format.sample_rate.selected_value(), 44_100);
        assert_eq!(*format.bit_depth.selected_value(), BitDepthChoice::Int16);
    }

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
    fn source_coupled_rate_defers_ssrc_dither_validation_to_the_pipeline() {
        assert_eq!(
            validate_ssrc_dither_id_for_target_rate(16, SOURCE_SAMPLE_RATE_SENTINEL),
            Ok(())
        );
    }

    #[test]
    fn concrete_rate_still_rejects_an_unavailable_ssrc_dither_id() {
        assert_eq!(
            validate_ssrc_dither_id_for_target_rate(16, 96_000)
                .expect_err("96 kHz must reject shaped id 16"),
            "SSRC dither id 16 is not available for target sample rate 96000 Hz"
        );
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
mod app_startup_options_tests {
    use super::*;

    #[test]
    fn production_startup_options_enable_recovery_by_default() {
        let options = AppStartupOptions::default();
        assert!(options.recover_pending_archives);
        assert!(options.recover_pending_file_operations);
    }

    #[test]
    fn test_startup_options_use_isolated_temp_database_by_default() {
        assert_eq!(
            AppStartupOptions::default().database_source,
            AppDatabaseSource::IsolatedTempFile
        );
        assert_eq!(
            AppStartupOptions::with_archive_recovery_for_tests().database_source,
            AppDatabaseSource::IsolatedTempFile
        );
    }

    #[test]
    fn test_constructor_options_explicitly_disable_external_recovery() {
        let options = AppStartupOptions::without_archive_recovery_for_tests();
        assert!(!options.recover_pending_archives);
        assert!(!options.recover_pending_file_operations);
        assert_eq!(options.database_source, AppDatabaseSource::IsolatedTempFile);
    }

    #[test]
    fn app_state_instances_do_not_share_pending_archive_session_rows_in_tests() {
        let app = AppState::new_for_test(TonepoetConfig::default());
        let temp = tempfile::tempdir().expect("temp dir");
        let archive = temp.path().join("album.zip");
        let staging = temp.path().join("tonepoet-archive-rename-test");
        std::fs::write(&archive, b"archive bytes").expect("archive fixture");
        std::fs::create_dir_all(&staging).expect("staging fixture");
        app.db
            .upsert_pending_archive_session(&archive, &staging, 0, 0, 13, "[]")
            .expect("upsert pending archive session");
        assert_eq!(app.db.pending_archive_session_count_for_tests().unwrap(), 1);

        let other = AppState::new_for_test(TonepoetConfig::default());
        assert_eq!(other.db.pending_archive_session_count_for_tests().unwrap(), 0);
    }

    #[test]
    fn explicit_test_database_path_is_file_backed_and_reusable() {
        let temp = tempfile::tempdir().expect("temp dir");
        let db_path = temp.path().join("state").join("tonepoet.db");
        let archive = temp.path().join("album.zip");
        let staging = temp.path().join("tonepoet-archive-rename-explicit-db");
        std::fs::write(&archive, b"archive bytes").expect("archive fixture");
        std::fs::create_dir_all(&staging).expect("staging fixture");

        {
            let app = AppState::new_for_test_with_db_path(TonepoetConfig::default(), &db_path);
            app.db
                .upsert_pending_archive_session(&archive, &staging, 0, 0, 13, "[]")
                .expect("upsert pending archive session");
            assert_eq!(app.db.pending_archive_session_count_for_tests().unwrap(), 1);
            assert!(db_path.exists(), "explicit test DB should be file-backed");
        }

        let reopened = AppState::new_for_test_with_db_path(TonepoetConfig::default(), &db_path);
        assert_eq!(
            reopened.db.pending_archive_session_count_for_tests().unwrap(),
            1,
            "explicit file-backed test DB should persist across AppState instances"
        );
    }
}

#[cfg(test)]
mod dsd_gain_format_state_tests {
    use super::*;

    #[test]
    fn dsd_gain_rows_are_visible_only_for_dsd_sources_targeting_pcm() {
        assert!(!FormatField::visible_rows(false, false, false).contains(&FormatField::DsdGain));
        assert!(!FormatField::visible_rows(false, false, false).contains(&FormatField::DsdGainDb));
        assert!(FormatField::visible_rows(false, true, true).contains(&FormatField::DsdGain));
        assert!(FormatField::visible_rows(false, true, true).contains(&FormatField::DsdGainDb));
        assert!(FormatField::visible_rows(false, true, true).contains(&FormatField::DsdPath));
        assert!(FormatField::visible_rows(false, true, true).contains(&FormatField::DsdProfile));
        assert!(FormatField::visible_rows(false, true, true)
            .contains(&FormatField::DsdNormalizeTarget));
        assert!(!FormatField::visible_rows(true, false, false).contains(&FormatField::DsdGain));
        assert!(!FormatField::visible_rows(true, true, true).contains(&FormatField::DsdGain));
    }

    #[test]
    fn pre_promotion_reference_controls_remain_hidden_for_dsd_to_pcm() {
        let mut s = FormatState::new();
        assert!(!s.dsd_to_pcm_gain_available());
        assert!(!s.dsd_reference_controls_available());

        s.set_source_is_dsd(true);
        assert!(s.dsd_to_pcm_gain_available());
        assert!(!s.dsd_reference_controls_available());
        assert!(s.dsd_gain_mode.options.iter().any(|option| option.value == DsdGainMode::Disabled && option.enabled));
        assert!(s.dsd_gain_mode.options.iter().any(|option| option.value == DsdGainMode::Auto && option.enabled));
        assert!(s.dsd_gain_mode.options.iter().any(|option| option.value == DsdGainMode::Fixed && option.enabled));
        assert!(s.dsd_gain_mode.options.iter().any(|option| option.value == DsdGainMode::Reference && !option.enabled));
        let rows = FormatField::visible_rows(
            s.is_dsd_selected(),
            s.dsd_to_pcm_gain_available(),
            s.dsd_reference_controls_available(),
        );
        assert!(rows.contains(&FormatField::DsdGain));
        assert!(rows.contains(&FormatField::DsdGainDb));
        assert!(rows.contains(&FormatField::DsdNormalizeTarget));
        assert!(!rows.contains(&FormatField::DsdPath));
        assert!(!rows.contains(&FormatField::DsdProfile));
        assert!(s.resampler.options.iter().any(|option| option.enabled));
        assert!(s.dither.options.iter().any(|option| option.enabled));
    }

    #[test]
    fn dsd_target_hides_reference_controls_even_for_dsd_source() {
        let mut s = FormatState::new();
        s.set_source_is_dsd(true);
        s.format.select_value(&AudioFormat::Dsf);
        s.apply_format_constraints();

        assert!(!s.dsd_to_pcm_gain_available());
        assert!(!s.dsd_reference_controls_available());
        assert!(!FormatField::visible_rows(
            s.is_dsd_selected(),
            s.dsd_to_pcm_gain_available(),
            s.dsd_reference_controls_available(),
        )
        .contains(&FormatField::DsdGain));
    }


    #[test]
    fn focus_navigation_skips_reference_rows_before_promotion() {
        let mut s = FormatState::new();
        s.field_focus = FormatField::ReplayGain;
        s.focus_next();
        assert_eq!(s.field_focus, FormatField::Format);

        s.set_source_is_dsd(true);
        s.field_focus = FormatField::ReplayGain;
        s.focus_next();
        assert_eq!(s.field_focus, FormatField::DsdGain);
    }

    #[test]
    fn pre_promotion_dsd_gain_defaults_are_exact_legacy_disabled_and_point_15_margin() {
        let s = FormatState::new();
        assert_eq!(*s.dsd_gain_mode.selected_value(), DsdGainMode::Disabled);
        assert_eq!(
            s.dsd_normalize_target_dbfs,
            "-0.150000000".parse().unwrap()
        );
        assert_eq!(s.dsd_auto_gain_margin_db, "0.150000000".parse().unwrap());
        assert_eq!(s.dsd_gain_db, DbNano::ZERO);
        assert!(!s.source_is_dsd);
    }

    fn enable_native_reference_controls_for_unit_test(s: &mut FormatState) {
        s.set_source_is_dsd(true);
        s.dsd_pathway.set_all_enabled(true);
        s.dsd_profile.set_all_enabled(true);
        s.dsd_gain_mode.set_all_enabled(true);
    }

    #[test]
    fn manual_dsd_gain_row_adjusts_value_and_selects_manual_mode() {
        let mut s = FormatState::new();
        enable_native_reference_controls_for_unit_test(&mut s);
        s.field_focus = FormatField::DsdGainDb;

        s.select_focused_next(None, None);
        assert_eq!(*s.dsd_gain_mode.selected_value(), DsdGainMode::Fixed);
        assert_eq!(s.dsd_gain_db, DbNano(DSD_TO_PCM_GAIN_DB_STEP_NANO));

        s.select_focused_prev(None, None);
        assert_eq!(s.dsd_gain_db, DbNano::ZERO);
    }

    #[test]
    fn manual_dsd_gain_row_clamps_to_valid_settings_range() {
        let mut s = FormatState::new();
        enable_native_reference_controls_for_unit_test(&mut s);
        s.field_focus = FormatField::DsdGainDb;
        s.dsd_gain_db = DbNano(DSD_TO_PCM_GAIN_DB_MAX_NANO);
        s.select_focused_next(None, None);
        assert_eq!(s.dsd_gain_db, DbNano(DSD_TO_PCM_GAIN_DB_MAX_NANO));

        s.dsd_gain_db = DbNano(DSD_TO_PCM_GAIN_DB_MIN_NANO);
        s.select_focused_prev(None, None);
        assert_eq!(s.dsd_gain_db, DbNano(DSD_TO_PCM_GAIN_DB_MIN_NANO));
    }

    #[test]
    fn native_normalize_target_value_preserves_fixed_point() {
        let mut mode = PillState::new(vec![
            (DsdGainMode::NormalizePeak, "normalize"),
            (DsdGainMode::Fixed, "fixed"),
        ]);
        let mut target = DbNano::DEFAULT_NORMALIZE_TARGET;
        let mut focused = FocusedPill::DsdNormalizeTarget {
            target_dbfs: &mut target,
            gain_mode: &mut mode,
        };
        focused.select_prev();
        assert_eq!(*mode.selected_value(), DsdGainMode::NormalizePeak);
        assert_eq!(target, "-0.400000000".parse().unwrap());
    }

    #[test]
    fn pre_promotion_auto_margin_row_selects_auto_and_steps_exact_margin() {
        let mut s = FormatState::new();
        s.set_source_is_dsd(true);
        s.field_focus = FormatField::DsdNormalizeTarget;
        s.select_focused_next(None, None);
        assert_eq!(*s.dsd_gain_mode.selected_value(), DsdGainMode::Auto);
        assert_eq!(s.dsd_auto_gain_margin_db, "0.200000000".parse().unwrap());
    }

    #[test]
    fn pre_promotion_wideband_profile_remains_disabled() {
        let mut s = FormatState::new();
        s.set_source_is_dsd(true);
        s.cascade_dsd_source_to_pcm_defaults(5_644_800);
        s.sample_rate.select_value(&176_400);
        s.apply_format_constraints();

        assert!(!s.dsd_reference_controls_available());
        assert!(!s
            .dsd_profile
            .options
            .iter()
            .find(|option| option.value == DsdReconstructionSelection::Wideband)
            .unwrap()
            .enabled);
    }
}

#[cfg(test)]
mod archive_listing_cache_tests {
    use super::*;
    use crate::tui::archive_listing::{ArchiveEntry, ArchiveListing, ArchiveListingCacheKey};
    use std::path::PathBuf;

    fn cache_key(idx: usize) -> ArchiveListingCacheKey {
        ArchiveListingCacheKey {
            path: PathBuf::from(format!("/tmp/archive-{idx}.zip")),
            size: idx as u64,
            modified_secs: idx as u64,
            modified_nanos: idx as u32,
        }
    }

    fn listing(idx: usize) -> ArchiveListing {
        ArchiveListing {
            archive_path: PathBuf::from(format!("/tmp/archive-{idx}.zip")),
            format: "zip".to_string(),
            physical_size: 0,
            entries: vec![ArchiveEntry {
                path: format!("track-{idx}.flac"),
                size: 1,
                packed_size: 1,
                is_dir: false,
                encrypted: false,
            }],
        }
    }

    #[test]
    fn archive_listing_cache_evicts_lru_entry() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        for idx in 0..ARCHIVE_LISTING_CACHE_MAX_ENTRIES {
            assert!(app.insert_archive_listing_cache(cache_key(idx), listing(idx)));
        }

        let refreshed = cache_key(0);
        assert!(app.cached_archive_listing(&refreshed).is_some());
        assert!(app.insert_archive_listing_cache(
            cache_key(ARCHIVE_LISTING_CACHE_MAX_ENTRIES),
            listing(ARCHIVE_LISTING_CACHE_MAX_ENTRIES),
        ));

        assert!(app.cached_archive_listing(&refreshed).is_some());
        assert!(app.cached_archive_listing(&cache_key(1)).is_none());
        let (cache_len, lru_len, _) = app.archive_listing_cache_debug_state();
        assert_eq!(cache_len, ARCHIVE_LISTING_CACHE_MAX_ENTRIES);
        assert_eq!(lru_len, ARCHIVE_LISTING_CACHE_MAX_ENTRIES);
    }

    #[test]
    fn archive_listing_cache_replaces_without_duplicate_lru_keys() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let key = cache_key(7);
        assert!(app.insert_archive_listing_cache(key.clone(), listing(7)));
        assert!(app.insert_archive_listing_cache(key.clone(), listing(8)));

        let (cache_len, lru_len, cache_bytes) = app.archive_listing_cache_debug_state();
        assert_eq!(cache_len, 1);
        assert_eq!(lru_len, 1);
        assert!(cache_bytes > 0);
        assert!(app.cached_archive_listing(&key).is_some());
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
            sample_format_is_float: None,
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
    fn single_pending_probe_has_explicit_state() {
        let path = PathBuf::from("/tmp/pending.flac");
        let mode = SourceMode::from_single_pending_probe(
            path.clone(),
            source_probe_initial_notice(&path),
        );

        match &mode {
            SourceMode::Single {
                path: stored_path,
                info,
                probe_notice,
                ..
            } => {
                assert_eq!(stored_path, &path);
                assert!(info.is_none());
                assert_eq!(probe_notice.as_deref(), Some(PROBE_IN_PROGRESS_NOTICE));
            }
            other => panic!("expected pending single source, got {:?}", other),
        }
        assert!(mode.probe_in_progress());
        assert_eq!(mode.persistent_probe_notice(), Some(PROBE_IN_PROGRESS_NOTICE));
    }

    #[test]
    fn from_paths_single_probeable_source_has_pending_notice() {
        let path = PathBuf::from("/tmp/from_paths.flac");
        let mode = SourceMode::from_paths(vec![path.clone()]);

        match &mode {
            SourceMode::Single {
                path: stored_path,
                info,
                probe_notice,
                ..
            } => {
                assert_eq!(stored_path, &path);
                assert!(info.is_none());
                assert_eq!(probe_notice.as_deref(), Some(PROBE_IN_PROGRESS_NOTICE));
            }
            other => panic!("expected single pending-probe source, got {:?}", other),
        }
        assert!(mode.probe_in_progress());
    }

    #[test]
    fn ordinary_single_preserves_probe_notice_after_worker_completion_failure() {
        let dir = temp_test_dir("ordinary_notice");
        let path = dir.join("broken.flac");
        write_test_file(&path, "not real flac");
        let notice = "Probe failed: not an audio stream; set format manually".to_string();

        let mode = SourceMode::from_single_with_probe_notice(
            path.clone(),
            None,
            SourceMetadata::default(),
            Some(notice.clone()),
        );

        match &mode {
            SourceMode::Single {
                path: stored_path,
                info,
                probe_notice,
                ..
            } => {
                assert_eq!(stored_path, &path);
                assert!(info.is_none());
                assert_eq!(probe_notice.as_deref(), Some(notice.as_str()));
            }
            other => panic!("expected ordinary single source to preserve notice, got {:?}", other),
        }
        assert_eq!(mode.persistent_probe_notice(), Some(notice.as_str()));
        assert!(!mode.probe_in_progress());

        let _ = std::fs::remove_dir_all(dir);
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
        let hook = CueProxyProbeTestHook::default();
        let (non_audio_result, hook) = with_cue_proxy_probe_test_hook(hook, || {
            probe_cue_proxy_source(&non_audio_cue).expect("non-audio reference should be a warning result")
        });
        assert!(non_audio_result.info.is_none());
        // The unified FILE-ref resolver rejects non-audio references at
        // resolution, so no probe is attempted — and the message names the
        // real problem instead of claiming the file "was not found".
        let notice = non_audio_result.probe_notice.expect("non-audio reference should warn");
        assert!(notice.contains("exists but is not supported audio"), "{notice}");
        assert!(notice.contains("set format manually"), "{notice}");
        assert!(hook.probed_paths.is_empty(), "{:?}", hook.probed_paths);
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
            let mut app = AppState::new_for_test(TonepoetConfig::default());
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

        let mut app = AppState::new_for_test(TonepoetConfig::default());
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

    #[test]
    fn emptying_the_source_preserves_a_deliberate_source_rate() {
        // Audit H1: set_source_mode(Empty) used to promote identity to
        // Known(PCM) and clamp away a retained same-as-source selection.
        let mut convert = ConvertState::new();
        convert.format.set_source_is_dsd(true); // probe-proven DSD source
        convert.format.format.select_value(&crate::convert::formats::AudioFormat::Dsf);
        convert.format.apply_format_constraints();
        assert!(convert
            .format
            .sample_rate
            .select_value(&SOURCE_SAMPLE_RATE_SENTINEL));

        convert.set_source_mode(SourceMode::Empty);

        assert_eq!(
            *convert.format.sample_rate.selected_value(),
            SOURCE_SAMPLE_RATE_SENTINEL,
            "emptying the batch must not clamp deliberate source-rate policy"
        );
        assert_eq!(
            convert.format.source_rate_identity,
            SourceRateIdentity::Lost
        );
    }

    #[test]
    fn pending_probe_placeholder_does_not_clamp_a_staged_source_rate() {
        // Audit H2: a placeholder install (info: None) used to promote the
        // file-extension guess to Known — staging an .iso transiently
        // clamped a DSD source-rate selection before the probe could prove
        // the source is DSD.
        let mut convert = ConvertState::new();
        convert.format.set_source_is_dsd(true);
        convert.format.format.select_value(&crate::convert::formats::AudioFormat::Dsf);
        convert.format.apply_format_constraints();
        assert!(convert
            .format
            .sample_rate
            .select_value(&SOURCE_SAMPLE_RATE_SENTINEL));

        convert.set_source_mode(SourceMode::Single {
            path: std::path::PathBuf::from("/library/album.iso"),
            info: None,
            metadata: crate::tui::probe::SourceMetadata::default(),
            probe_notice: None,
        });

        assert_eq!(
            *convert.format.sample_rate.selected_value(),
            SOURCE_SAMPLE_RATE_SENTINEL,
            "an extension guess must not clamp deliberate source-rate policy"
        );
        assert_eq!(
            convert.format.source_rate_identity,
            SourceRateIdentity::Lost
        );

        // The probe completes and proves DSD: the sentinel is valid again.
        convert.format.set_source_is_dsd(true);
        assert!(convert.format.sample_rate.options.iter().any(|option| {
            option.value == SOURCE_SAMPLE_RATE_SENTINEL && option.enabled
        }));
    }

    #[test]
    fn clamped_automatic_defaults_still_get_resampler_and_dither() {
        // Audit M7: the auto rules used to run BEFORE constraints clamped a
        // force-installed 768k/Int32 automatic default, arming a real
        // 768->384 resample with resampler=None and a 32->24 truncation
        // with dither=None.
        let mut convert = ConvertState::new();
        convert
            .format
            .format
            .select_value(&crate::convert::formats::AudioFormat::Alac);
        convert.format.apply_format_constraints();

        convert.apply_source_info_defaults(&source_info(768_000, Some(32)));

        assert_eq!(*convert.format.sample_rate.selected_value(), 384_000);
        assert_eq!(*convert.format.bit_depth.selected_value(), BitDepthChoice::Int24);
        assert_ne!(
            *convert.format.resampler.selected_value(),
            ResamplerChoice::None,
            "a clamped 768->384 conversion must arm a resampler"
        );
        assert_ne!(
            *convert.format.dither.selected_value(),
            DitherType::None,
            "a clamped 32->24 reduction must arm dither"
        );
    }

    fn source_info(sample_rate: u32, bit_depth: Option<u32>) -> SourceInfo {
        SourceInfo {
            format_name: "FLAC".to_string(),
            codec: "FLAC".to_string(),
            bit_depth,
            sample_format_is_float: None,
            sample_rate,
            channels: 2,
            channel_layout: "stereo".to_string(),
            duration_secs: 10.0,
            file_size: 100,
        }
    }

    #[test]
    fn apply_source_defaults_preserves_source_sentinels_when_probe_is_unresolved() {
        let mut convert = ConvertState::new();
        convert
            .format
            .sample_rate
            .select_value(&SOURCE_SAMPLE_RATE_SENTINEL);
        convert
            .format
            .bit_depth
            .select_value(&BitDepthChoice::Source);
        convert.format.dither.select_value(&DitherType::Shibata);
        convert.format.resampler.select_value(&ResamplerChoice::Soxr);
        convert.format.dither_overridden = true;
        convert.format.resampler_overridden = true;
        convert.format.source_is_dsd = true;
        convert.format.apply_format_constraints();
        assert!(convert
            .format
            .dsd_gain_mode
            .select_value(&DsdGainMode::Fixed));
        convert.format.dsd_gain_db = "4.500000000".parse().unwrap();
        convert.set_source_mode(SourceMode::MultiTrack {
            path: PathBuf::from("/tmp/pending.cue"),
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
            probe_notice: Some("probe pending".to_string()),
            scroll: 0,
            cursor: 0,
            selected: vec![true],
            archive_preview: None,
            disc_contents: None,
            selected_presentation_id: None,
        });

        convert.apply_source_defaults();

        assert_eq!(
            *convert.format.sample_rate.selected_value(),
            SOURCE_SAMPLE_RATE_SENTINEL
        );
        assert_eq!(
            *convert.format.bit_depth.selected_value(),
            BitDepthChoice::Source
        );
        assert_eq!(*convert.format.dither.selected_value(), DitherType::Shibata);
        assert_eq!(
            *convert.format.resampler.selected_value(),
            ResamplerChoice::Soxr
        );
        assert!(convert.format.dither_overridden);
        assert!(convert.format.resampler_overridden);
        assert_eq!(
            *convert.format.dsd_gain_mode.selected_value(),
            DsdGainMode::Fixed
        );
        assert_eq!(
            convert.format.dsd_gain_db,
            "4.500000000".parse().unwrap()
        );
    }

    #[test]
    fn apply_source_defaults_clears_automatic_source_defaults_when_info_is_absent() {
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
            archive_preview: None,
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
            row_scope: crate::tui::probe::RowScope::File,
            display_key: display_key.to_string(),
            item_key: ItemKey::TrackTitle,
            value: value.to_string(),
            original: value.to_string(),
            is_binary: false,
            is_mixed: false,
            has_multiple_stored_values: false,
            per_file_stored_value_counts: Vec::new(),
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
    fn saved_cardinality_updates_only_slots_that_were_rewritten() {
        let mut changed = tag("ARTIST", "<multiple values>", vec!["Alpha; Beta", "Gamma"]);
        changed.is_mixed = true;
        changed.has_multiple_stored_values = true;
        changed.per_file_stored_value_counts = vec![2, 1];
        changed.per_file_values[0] = "Solo".to_string();
        mark_tag_entry_saved(&mut changed);
        assert_eq!(changed.per_file_stored_value_counts, vec![1, 1]);
        assert!(!changed.has_multiple_stored_values);

        let mut untouched = tag("ARTIST", "<multiple values>", vec!["Alpha; Beta", "Gamma"]);
        untouched.is_mixed = true;
        untouched.has_multiple_stored_values = true;
        untouched.per_file_stored_value_counts = vec![2, 1];
        mark_tag_entry_saved(&mut untouched);
        assert_eq!(untouched.per_file_stored_value_counts, vec![2, 1]);
        assert!(untouched.has_multiple_stored_values);
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
    fn failed_mandatory_refresh_remains_dirty_until_successful_reread() {
        let mut surface = tab(
            PresentationId::DvdAudioGroup(1),
            "Group 1",
            vec![tag("TITLE", "saved", vec!["saved"])],
            1,
        );
        surface.technical_details.session_id = 77;
        let mut state = state_with_tabs(vec![surface], 0);

        assert!(state.mark_saved_surface_refresh_failed(77));
        assert!(state.active_surface().refresh_failed);
        assert!(state.recompute_active_dirty());
        assert!(state.active_surface().dirty);

        let reread = vec![tag("TITLE", "saved", vec!["saved"])];
        assert!(state.replace_saved_surface_entries(77, reread));
        assert!(!state.active_surface().refresh_failed);
        assert!(!state.recompute_active_dirty());
        assert!(!state.active_surface().dirty);
    }

    #[test]
    fn successful_surface_reread_clears_row_selection_bound_to_old_entries() {
        let mut surface = tab(
            PresentationId::DvdAudioGroup(1),
            "Group 1",
            vec![tag("TITLE", "before", vec!["before"])],
            1,
        );
        surface.technical_details.session_id = 78;
        surface.selected_rows.insert(0);
        let mut state = state_with_tabs(vec![surface], 0);

        assert!(state.replace_saved_surface_entries(
            78,
            vec![tag("ARTIST", "after", vec!["after"])],
        ));
        assert!(state.active_surface().selected_rows.is_empty());
    }

    #[test]
    fn saved_row_removal_remaps_selection_indices_without_retargeting() {
        let mut surface = tab(
            PresentationId::DvdAudioGroup(1),
            "Group 1",
            vec![
                tag("TITLE", "one", vec!["one"]),
                tag("ARTIST", "two", vec!["two"]),
                tag("ALBUM", "three", vec!["three"]),
            ],
            1,
        );
        surface.deleted = vec![1];
        surface.selected_rows.extend([0, 1, 2]);

        reduce_saved_slots(&mut surface, &[0].into_iter().collect());

        assert_eq!(surface.entries.len(), 2);
        assert_eq!(surface.entries[0].display_key, "TITLE");
        assert_eq!(surface.entries[1].display_key, "ALBUM");
        assert_eq!(surface.selected_rows, [0, 1].into_iter().collect());
        assert!(surface.deleted.is_empty());
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

        let result = state.apply_active_musicbrainz_values_to_matching_presentations();

        assert_eq!(result.changed_presentations, 1);
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
    fn apply_active_musicbrainz_values_reports_sibling_cardinality_loss() {
        let active_artist = mb_tag(
            "ARTIST",
            "<multiple values>",
            vec!["New Artist", "New Scalar"],
        );
        let mut destination_artist = tag(
            "ARTIST",
            "<multiple values>",
            vec!["Alpha; Beta", "Gamma"],
        );
        destination_artist.is_mixed = true;
        destination_artist.has_multiple_stored_values = true;
        destination_artist.per_file_stored_value_counts = vec![2, 1];
        let tabs = vec![
            tab(
                PresentationId::DvdAudioGroup(1),
                "Group 1",
                vec![active_artist],
                2,
            ),
            tab(
                PresentationId::DvdAudioGroup(2),
                "Group 2",
                vec![destination_artist],
                2,
            ),
        ];
        let mut state = state_with_tabs(tabs, 0);

        let result = state.apply_active_musicbrainz_values_to_matching_presentations();

        assert_eq!(result.changed_presentations, 1);
        assert_eq!(result.mutation_report.collapsed_carrier_count(), 1);
        assert_eq!(result.mutation_report.collapsed_fields.len(), 1);
        assert_eq!(result.mutation_report.collapsed_fields[0].display_key, "ARTIST");
        assert_eq!(result.mutation_report.collapsed_fields[0].slots, vec![0]);
    }

    #[test]
    fn apply_active_musicbrainz_values_clears_provenance_for_new_sibling_row() {
        let mut active_artist = mb_tag(
            "ARTIST",
            "<multiple values>",
            vec!["New Artist", "New Scalar"],
        );
        active_artist.is_mixed = true;
        active_artist.has_multiple_stored_values = true;
        active_artist.per_file_stored_value_counts = vec![2, 1];
        let tabs = vec![
            tab(
                PresentationId::DvdAudioGroup(1),
                "Group 1",
                vec![active_artist],
                2,
            ),
            tab(
                PresentationId::DvdAudioGroup(2),
                "Group 2",
                vec![tag("TITLE", "Sibling title", vec!["One", "Two"])],
                2,
            ),
        ];
        let mut state = state_with_tabs(tabs, 0);

        let result = state.apply_active_musicbrainz_values_to_matching_presentations();

        assert_eq!(result.changed_presentations, 1);
        assert_eq!(result.mutation_report.collapsed_carrier_count(), 0);
        let created = state.presentation_tabs[1]
            .entries
            .iter()
            .find(|entry| entry.display_key.eq_ignore_ascii_case("ARTIST"))
            .expect("MusicBrainz ARTIST row should be created in matching sibling");
        assert!(!created.has_multiple_stored_values);
        assert!(created.per_file_stored_value_counts.is_empty());
        assert!(created
            .stored_value_collapse_slots([(0, "Manual Artist")])
            .is_empty());
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

        let result = state.apply_active_musicbrainz_values_to_matching_presentations();

        assert_eq!(result.changed_presentations, 1);
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
    fn metadata_save_progress_requires_current_session_generation_and_saving_phase() {
        let mut state = write_state();
        let (session_id, generation, _cancel) = state.begin_cancellable_write();
        state.phase = MetadataEditorPhase::Saving;

        assert!(state.apply_metadata_save_progress(
            session_id,
            generation,
            "Saving 1/2: one.dsf - rewriting 1.0 MiB / 2.0 MiB".to_string(),
        ));
        assert_eq!(
            state.model.metadata_save_progress.as_deref(),
            Some("Saving 1/2: one.dsf - rewriting 1.0 MiB / 2.0 MiB")
        );

        assert!(!state.apply_metadata_save_progress(
            session_id.saturating_add(1),
            generation,
            "stale session".to_string(),
        ));
        assert!(!state.apply_metadata_save_progress(
            session_id,
            generation.saturating_add(1),
            "stale generation".to_string(),
        ));
        state.phase = MetadataEditorPhase::Editing;
        assert!(!state.apply_metadata_save_progress(
            session_id,
            generation,
            "wrong phase".to_string(),
        ));
        assert_eq!(
            state.model.metadata_save_progress.as_deref(),
            Some("Saving 1/2: one.dsf - rewriting 1.0 MiB / 2.0 MiB")
        );
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
    fn unified_cue_album_row_entries_clear_dirty_after_all_member_images_save() {
        let mut state = write_state();
        // Unified surface: 2 member images, but per-track rows dimensioned by
        // track count (4), persisted via the regenerated embedded CUESHEET.
        state.active_surface_mut().cue_album_synthetic_sheet = Some(CueAlbumSyntheticSheet {
            cue_paths: Vec::new(),
            audio_paths: vec![
                std::path::PathBuf::from("/tmp/one.flac"),
                std::path::PathBuf::from("/tmp/two.flac"),
            ],
            track_sources: Vec::new(),
            album_title: None,
            album_performer: None,
            album_date: None,
            album_genre: None,
            album_catalog: None,
        });
        state
            .active_surface_mut()
            .entries
            .push(tag("CUESHEET", "[CUE sheet]", vec!["SHEET-A", "SHEET-B"]));
        state
            .active_surface_mut()
            .entries
            .push(tag("TITLE", "<multiple values>", vec!["T1", "T2", "T3", "T4"]));
        let row_idx = state.active_surface().entries.len() - 1;
        state.active_surface_mut().entries[row_idx].per_file_values[2] = "Edited".to_string();
        state.active_surface_mut().dirty = true;
        let (session_id, generation) = state.begin_write();

        let summary = state
            .apply_write_results(
                session_id,
                generation,
                vec![
                    MetadataEditorWriteResult::saved(state.active_surface().paths[0].clone()),
                    MetadataEditorWriteResult::saved(state.active_surface().paths[1].clone()),
                ],
            )
            .expect("matching save result should reduce");

        assert_eq!(summary.saved, 2);
        assert_eq!(
            state.active_surface().entries[row_idx].per_file_originals[2],
            "Edited",
            "row-dimensioned entries advance originals once every member image saved"
        );
        assert!(
            !state.active_surface().dirty,
            "unified surface must not stay dirty after a fully successful save (editor could never close)"
        );
        assert!(!summary.remaining_dirty);
    }

    #[test]
    fn unified_cue_album_row_entries_stay_dirty_after_partial_save() {
        let mut state = write_state();
        state.active_surface_mut().cue_album_synthetic_sheet = Some(CueAlbumSyntheticSheet {
            cue_paths: Vec::new(),
            audio_paths: vec![
                std::path::PathBuf::from("/tmp/one.flac"),
                std::path::PathBuf::from("/tmp/two.flac"),
            ],
            track_sources: Vec::new(),
            album_title: None,
            album_performer: None,
            album_date: None,
            album_genre: None,
            album_catalog: None,
        });
        state
            .active_surface_mut()
            .entries
            .push(tag("CUESHEET", "[CUE sheet]", vec!["SHEET-A", "SHEET-B"]));
        {
            // A regenerated sheet is STAGED but not yet written anywhere: the
            // H1 disk-equivalence rule must not treat the unsaved member as
            // already persisted.
            let cue_idx = state.active_surface().entries.len() - 1;
            let entry = &mut state.active_surface_mut().entries[cue_idx];
            entry.per_file_values = vec!["SHEET-NEW-A".to_string(), "SHEET-NEW-B".to_string()];
        }
        state
            .active_surface_mut()
            .entries
            .push(tag("TITLE", "<multiple values>", vec!["T1", "T2", "T3", "T4"]));
        let row_idx = state.active_surface().entries.len() - 1;
        state.active_surface_mut().entries[row_idx].per_file_values[2] = "Edited".to_string();
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
        assert_eq!(
            state.active_surface().entries[row_idx].per_file_originals[2],
            "T3",
            "row edits must stay pending until every member image carries the regenerated sheet"
        );
        assert!(state.active_surface().dirty, "partial save keeps the surface dirty for retry");
    }



    #[test]
    fn unified_cue_album_partial_save_retry_only_remaining_member_clears_dirty() {
        let mut state = write_state();
        state.active_surface_mut().cue_album_synthetic_sheet = Some(CueAlbumSyntheticSheet {
            cue_paths: Vec::new(),
            audio_paths: vec![
                std::path::PathBuf::from("/tmp/one.flac"),
                std::path::PathBuf::from("/tmp/two.flac"),
            ],
            track_sources: Vec::new(),
            album_title: Some("Album".to_string()),
            album_performer: Some("Artist".to_string()),
            album_date: None,
            album_genre: None,
            album_catalog: None,
        });
        state.active_surface_mut().entries = vec![
            TagEntry {
                row_scope: crate::tui::probe::RowScope::File,
                display_key: "CUESHEET".to_string(),
                item_key: ItemKey::Unknown("CUESHEET".to_string()),
                value: "new synthetic sheet".to_string(),
                original: "old synthetic sheet".to_string(),
                is_binary: true,
                is_mixed: false,
                has_multiple_stored_values: false,
                per_file_stored_value_counts: Vec::new(),
                per_file_values: vec!["new synthetic sheet".to_string(), "new synthetic sheet".to_string()],
                per_file_originals: vec!["old synthetic sheet".to_string(), "old synthetic sheet".to_string()],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            },
            tag("TITLE", "<multiple values>", vec!["A1", "A2", "B1", "B2"]),
        ];
        state.active_surface_mut().entries[1].per_file_values[2] = "B1 edited".to_string();
        state.active_surface_mut().dirty = true;

        let (session_id, generation) = state.begin_write();
        let partial = state
            .apply_write_results(
                session_id,
                generation,
                vec![
                    MetadataEditorWriteResult::saved(state.active_surface().paths[0].clone()),
                    MetadataEditorWriteResult::failed(state.active_surface().paths[1].clone(), "locked"),
                ],
            )
            .expect("partial save should reduce");

        assert_eq!(partial.saved, 1);
        assert_eq!(partial.failed, 1);
        assert_eq!(
            state.active_surface().entries[0].per_file_originals,
            vec!["new synthetic sheet".to_string(), "old synthetic sheet".to_string()],
            "successful member's embedded CUESHEET original must advance before retry"
        );
        assert_eq!(
            state.active_surface().entries[1].per_file_originals[2],
            "B1",
            "row-dimensioned unified edits must not advance on a partial member save"
        );
        assert!(state.active_surface().dirty);

        let (session_id, generation) = state.begin_write();
        let retry = state
            .apply_write_results(
                session_id,
                generation,
                vec![MetadataEditorWriteResult::saved(
                    state.active_surface().paths[1].clone(),
                )],
            )
            .expect("remaining-member retry should reduce");

        assert_eq!(retry.saved, 1);
        assert_eq!(retry.failed, 0);
        assert!(!retry.remaining_dirty);
        assert_eq!(
            state.active_surface().entries[0].per_file_originals,
            vec!["new synthetic sheet".to_string(), "new synthetic sheet".to_string()],
            "retry only writes the remaining member but the sheet is now persisted for every slot"
        );
        assert_eq!(
            state.active_surface().entries[1].per_file_originals[2],
            "B1 edited",
            "row originals must advance once all member images have the regenerated sheet, even across batches"
        );
        assert!(!state.active_surface().dirty);
        assert!(
            !crate::tui::probe::metadata_editor_has_changes(&state),
            "a subsequent :w should find no metadata changes to save"
        );
    }

    #[test]
    fn unified_cue_album_partial_save_failing_retry_remains_dirty() {
        let mut state = write_state();
        state.active_surface_mut().cue_album_synthetic_sheet = Some(CueAlbumSyntheticSheet {
            cue_paths: Vec::new(),
            audio_paths: vec![
                std::path::PathBuf::from("/tmp/one.flac"),
                std::path::PathBuf::from("/tmp/two.flac"),
            ],
            track_sources: Vec::new(),
            album_title: Some("Album".to_string()),
            album_performer: Some("Artist".to_string()),
            album_date: None,
            album_genre: None,
            album_catalog: None,
        });
        state.active_surface_mut().entries = vec![
            TagEntry {
                row_scope: crate::tui::probe::RowScope::File,
                display_key: "CUESHEET".to_string(),
                item_key: ItemKey::Unknown("CUESHEET".to_string()),
                value: "new synthetic sheet".to_string(),
                original: "old synthetic sheet".to_string(),
                is_binary: true,
                is_mixed: false,
                has_multiple_stored_values: false,
                per_file_stored_value_counts: Vec::new(),
                per_file_values: vec!["new synthetic sheet".to_string(), "new synthetic sheet".to_string()],
                per_file_originals: vec!["old synthetic sheet".to_string(), "old synthetic sheet".to_string()],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            },
            tag("TITLE", "<multiple values>", vec!["A1", "A2", "B1", "B2"]),
        ];
        state.active_surface_mut().entries[1].per_file_values[2] = "B1 edited".to_string();
        state.active_surface_mut().dirty = true;

        let (session_id, generation) = state.begin_write();
        state
            .apply_write_results(
                session_id,
                generation,
                vec![
                    MetadataEditorWriteResult::saved(state.active_surface().paths[0].clone()),
                    MetadataEditorWriteResult::failed(state.active_surface().paths[1].clone(), "locked"),
                ],
            )
            .expect("partial save should reduce");

        let (session_id, generation) = state.begin_write();
        let retry = state
            .apply_write_results(
                session_id,
                generation,
                vec![MetadataEditorWriteResult::failed(
                    state.active_surface().paths[1].clone(),
                    "still locked",
                )],
            )
            .expect("failing retry should reduce");

        assert_eq!(retry.saved, 0);
        assert_eq!(retry.failed, 1);
        assert!(retry.remaining_dirty);
        assert_eq!(
            state.active_surface().entries[0].per_file_originals,
            vec!["new synthetic sheet".to_string(), "old synthetic sheet".to_string()],
            "failed retry must not claim the remaining member's embedded CUESHEET is persisted"
        );
        assert_eq!(
            state.active_surface().entries[1].per_file_originals[2],
            "B1",
            "row originals remain unadvanced while any member image still failed"
        );
        assert!(state.active_surface().dirty);
        assert!(crate::tui::probe::metadata_editor_has_changes(&state));
    }

    #[test]
    fn unified_cue_album_tracknumber_edits_do_not_clear_as_saved() {
        let mut state = write_state();
        state.active_surface_mut().cue_album_synthetic_sheet = Some(CueAlbumSyntheticSheet {
            cue_paths: Vec::new(),
            audio_paths: vec![
                std::path::PathBuf::from("/tmp/one.flac"),
                std::path::PathBuf::from("/tmp/two.flac"),
            ],
            track_sources: Vec::new(),
            album_title: None,
            album_performer: None,
            album_date: None,
            album_genre: None,
            album_catalog: None,
        });
        state.active_surface_mut().entries.push(tag(
            "TRACKNUMBER",
            "<multiple values>",
            vec!["01", "02", "03", "04"],
        ));
        let row_idx = state.active_surface().entries.len() - 1;
        state.active_surface_mut().entries[row_idx].per_file_values[2] = "99".to_string();
        state.active_surface_mut().dirty = true;
        let (session_id, generation) = state.begin_write();

        let summary = state
            .apply_write_results(
                session_id,
                generation,
                vec![
                    MetadataEditorWriteResult::saved(state.active_surface().paths[0].clone()),
                    MetadataEditorWriteResult::saved(state.active_surface().paths[1].clone()),
                ],
            )
            .expect("matching save result should reduce");

        assert_eq!(summary.saved, 2);
        assert_eq!(
            state.active_surface().entries[row_idx].per_file_originals[2],
            "03",
            "TRACKNUMBER is positional in regenerated CUE sheets and must not be reported saved"
        );
        assert!(summary.remaining_dirty);
        assert!(state.active_surface().dirty);
    }

    #[test]
    fn unified_cue_album_unsupported_deleted_row_is_not_removed_after_full_save() {
        let mut state = write_state();
        state.active_surface_mut().cue_album_synthetic_sheet = Some(CueAlbumSyntheticSheet {
            cue_paths: Vec::new(),
            audio_paths: vec![
                std::path::PathBuf::from("/tmp/one.flac"),
                std::path::PathBuf::from("/tmp/two.flac"),
            ],
            track_sources: Vec::new(),
            album_title: None,
            album_performer: None,
            album_date: None,
            album_genre: None,
            album_catalog: None,
        });
        state.active_surface_mut().entries.push(tag(
            "COMPOSER",
            "<multiple values>",
            vec!["A", "B", "C", "D"],
        ));
        let row_idx = state.active_surface().entries.len() - 1;
        state.active_surface_mut().deleted.push(row_idx);
        state.active_surface_mut().dirty = true;
        let (session_id, generation) = state.begin_write();

        let summary = state
            .apply_write_results(
                session_id,
                generation,
                vec![
                    MetadataEditorWriteResult::saved(state.active_surface().paths[0].clone()),
                    MetadataEditorWriteResult::saved(state.active_surface().paths[1].clone()),
                ],
            )
            .expect("matching save result should reduce");

        assert_eq!(summary.saved, 2);
        assert!(
            state
                .active_surface()
                .entries
                .iter()
                .any(|entry| entry.display_key == "COMPOSER"),
            "unsupported per-track deletion must not disappear as if the CUE writer persisted it"
        );
        assert!(state.active_surface().deleted.contains(&row_idx));
        assert!(summary.remaining_dirty);
        assert!(state.active_surface().dirty);
    }

    #[test]
    fn unified_forced_cleanup_consumes_successful_saved_slots_individually() {
        let mut state = write_state();
        state.active_surface_mut().cue_album_synthetic_sheet = Some(CueAlbumSyntheticSheet {
            cue_paths: Vec::new(),
            audio_paths: vec![
                std::path::PathBuf::from("/tmp/one.flac"),
                std::path::PathBuf::from("/tmp/two.flac"),
            ],
            track_sources: Vec::new(),
            album_title: None,
            album_performer: None,
            album_date: None,
            album_genre: None,
            album_catalog: None,
        });
        state.active_surface_mut().cue_album_forced_cleanup = vec![
            (0, lofty::tag::ItemKey::Isrc),
            (0, lofty::tag::ItemKey::TrackNumber),
        ];
        state.active_surface_mut().dirty = false;
        let (session_id, generation) = state.begin_write();

        let summary = state
            .apply_write_results(
                session_id,
                generation,
                vec![MetadataEditorWriteResult::saved(state.active_surface().paths[0].clone())],
            )
            .expect("matching cleanup-only save result should reduce");

        assert_eq!(summary.saved, 1);
        assert!(
            state.active_surface().cue_album_forced_cleanup.is_empty(),
            "cleanup entries for a successfully written member image must be consumed even when other member images had no cleanup work"
        );
        assert!(
            !state.active_surface().dirty,
            "cleanup-only save for a single polluted member should not leave repeated work"
        );
    }

    #[test]
    fn unified_forced_cleanup_retains_only_failed_slots_after_partial_save() {
        let mut state = write_state();
        state.active_surface_mut().cue_album_synthetic_sheet = Some(CueAlbumSyntheticSheet {
            cue_paths: Vec::new(),
            audio_paths: vec![
                std::path::PathBuf::from("/tmp/one.flac"),
                std::path::PathBuf::from("/tmp/two.flac"),
            ],
            track_sources: Vec::new(),
            album_title: None,
            album_performer: None,
            album_date: None,
            album_genre: None,
            album_catalog: None,
        });
        state.active_surface_mut().cue_album_forced_cleanup = vec![
            (0, lofty::tag::ItemKey::Isrc),
            (1, lofty::tag::ItemKey::Isrc),
            (1, lofty::tag::ItemKey::TrackNumber),
        ];
        state.active_surface_mut().dirty = false;
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
            .expect("matching partial cleanup save result should reduce");

        assert_eq!(summary.saved, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(
            state.active_surface().cue_album_forced_cleanup,
            vec![
                (1, lofty::tag::ItemKey::Isrc),
                (1, lofty::tag::ItemKey::TrackNumber),
            ],
            "successful cleanup slots must be retired while failed slots stay pending for retry"
        );
    }

    #[test]
    fn partial_embedded_cuesheet_delete_keeps_retry_tombstone_until_all_member_images_save() {
        let mut state = write_state();
        state.active_surface_mut().pending_embedded_cuesheet_delete = true;
        state.active_surface_mut().embedded_cuesheet_present = true;
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
        assert!(state.active_surface().pending_embedded_cuesheet_delete);
        assert!(state.active_surface().embedded_cuesheet_present);
        assert!(state.active_surface().dirty, "remaining member image still needs CUESHEET deletion");

        // Retry keeps the album-level tombstone and stages deletion through the
        // same multi-file save path. Only after every member image reports a
        // successful save may the tombstone be cleared.
        let (session_id, generation) = state.begin_write();
        let retry_summary = state
            .apply_write_results(
                session_id,
                generation,
                vec![
                    MetadataEditorWriteResult::saved(state.active_surface().paths[0].clone()),
                    MetadataEditorWriteResult::saved(state.active_surface().paths[1].clone()),
                ],
            )
            .expect("retry save result should reduce");

        assert_eq!(retry_summary.saved, 2);
        assert!(!retry_summary.remaining_dirty);
        assert!(!state.active_surface().pending_embedded_cuesheet_delete);
        assert!(!state.active_surface().embedded_cuesheet_present);
        assert!(!state.active_surface().dirty);
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
            sample_format_is_float: None,
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
    fn replaygain_completion_requires_matching_surface_session() {
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
        let mut state = state_with_tabs(tabs, 0);
        let (session_id, generation) = state.begin_replaygain_scan(MetadataReplayGainScanMode::Track, 1);

        assert!(!state.complete_replaygain_scan(session_id.saturating_add(1), generation));
        assert!(state.replaygain_scan.is_some());
        assert!(state.complete_replaygain_scan(session_id, generation));
        assert!(state.replaygain_scan.is_none());
    }

    #[test]
    fn artwork_completion_requires_matching_surface_session() {
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
        let mut state = state_with_tabs(tabs, 0);
        let (session_id, generation) = state.begin_artwork_write(MetadataArtworkWriteMode::Write, 1);

        assert!(!state.complete_artwork_write(session_id, generation.saturating_add(1)));
        assert!(state.artwork_write.is_some());
        assert!(state.complete_artwork_write(session_id, generation));
        assert!(state.artwork_write.is_none());
    }

    #[test]
    fn replaygain_completion_can_target_non_active_presentation() {
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
        let mut state = state_with_tabs(tabs, 0);
        let (session_id, generation) = state.begin_replaygain_scan(MetadataReplayGainScanMode::Album, 1);

        state.active_tab = 1;

        assert!(state.complete_replaygain_scan(session_id, generation));
        let surface = state
            .surface_mut_for_session(session_id)
            .expect("originating surface should still be addressable after tab switch");
        surface.entries[0].value = "updated origin".to_string();

        assert_eq!(state.presentation_tabs[0].entries[0].value, "updated origin");
        assert_eq!(state.presentation_tabs[1].entries[0].value, "two");
        assert_eq!(state.active_tab, 1);
    }

    #[test]
    fn file_picker_generic_target_wraps_crate_state_without_artwork_coupling() {
        let temp = tempfile::tempdir().expect("tempdir");
        let selected = temp.path().join("selection.txt");
        std::fs::write(&selected, "ok").expect("write fixture");
        let picker = tui_file_picker::FilePickerState::new(tui_file_picker::FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: tui_file_picker::FilePickerFilter::All,
            title: "Pick anything".to_string(),
            ..tui_file_picker::FilePickerConfig::default()
        });
        let mut session = MetadataFilePickerState::new(
            FilePickerPurpose::Generic {
                id: "test-client".to_string(),
            },
            picker,
        );
        session.picker.refresh();
        let index = session
            .picker
            .entries()
            .iter()
            .position(|entry| entry.path == selected)
            .expect("fixture should be visible");
        session.picker.set_file_cursor(index, 10);

        assert_eq!(
            session.picker.accept_current_selection(),
            tui_file_picker::FilePickerAction::Selected(selected.clone())
        );
        assert_eq!(
            session.purpose,
            FilePickerPurpose::Generic {
                id: "test-client".to_string(),
            }
        );
    }

    #[test]
    fn file_picker_directory_mode_enter_navigates_ok_selects_current_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let child = temp.path().join("child");
        std::fs::create_dir(&child).expect("mkdir fixture");
        let picker = tui_file_picker::FilePickerState::new(tui_file_picker::FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: tui_file_picker::FilePickerFilter::All,
            title: "Pick folder".to_string(),
            ..tui_file_picker::FilePickerConfig::default()
        });
        let mut session = MetadataFilePickerState::new(
            FilePickerPurpose::Generic { id: "folder-client".to_string() },
            picker,
        );
        session.picker.set_selection_mode(tui_file_picker::FilePickerSelectionMode::Directories);
        session.picker.refresh();
        let index = session.picker.entries().iter().position(|entry| entry.path == child).expect("child visible");
        session.picker.set_file_cursor(index, 10);

        assert_eq!(session.picker.open_or_select_current(), tui_file_picker::FilePickerAction::None);
        assert_eq!(session.picker.current_dir(), child.as_path());
        assert_eq!(
            session.picker.accept_current_selection(),
            tui_file_picker::FilePickerAction::Selected(session.picker.current_dir().to_path_buf())
        );
    }

    #[test]
    fn file_picker_hidden_and_sort_are_crate_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("zeta.txt"), "z").expect("write zeta");
        std::fs::write(temp.path().join("alpha.txt"), "a").expect("write alpha");
        std::fs::write(temp.path().join(".hidden.txt"), "h").expect("write hidden");
        let picker = tui_file_picker::FilePickerState::new(tui_file_picker::FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: tui_file_picker::FilePickerFilter::All,
            title: "Pick anything".to_string(),
            ..tui_file_picker::FilePickerConfig::default()
        });
        let mut session = MetadataFilePickerState::new(
            FilePickerPurpose::Generic { id: "generic-client".to_string() },
            picker,
        );

        assert!(session.picker.entries().iter().all(|entry| !entry.name.starts_with('.')));
        session.picker.set_show_hidden(true);
        assert!(session.picker.entries().iter().any(|entry| entry.name == ".hidden.txt"));
        session.picker.set_sort(tui_file_picker::FilePickerSortKey::Size);
        assert_eq!(session.picker.sort_key(), tui_file_picker::FilePickerSortKey::Size);
    }

    #[test]
    fn surface_mut_for_session_finds_non_active_presentation() {
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
        let mut state = state_with_tabs(tabs, 1);

        let surface = state
            .surface_mut_for_session(session)
            .expect("non-active session should still be addressable");
        surface.entries[0].value = "updated non-active".to_string();

        assert_eq!(state.presentation_tabs[0].entries[0].value, "updated non-active");
        assert_eq!(state.active_tab, 1);
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
    fn artwork_preview_cache_clone_drops_terminal_protocol_and_worker_state() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let cache = ArtworkPreviewCache {
            path: std::path::PathBuf::from("/tmp/source.flac"),
            picture_type: lofty::picture::PictureType::LeadArtist,
            desired_preview_area: ratatui::layout::Rect::new(1, 2, 20, 10),
            encoded_preview_area: ratatui::layout::Rect::new(1, 2, 20, 10),
            desired_protocol_generation: 7,
            encoded_protocol_generation: 7,
            encoded_retransmit_generation: 3,
            generation: 42,
            decoded_generation: Some(42),
            decoded_image: None,
            receiver: Some(rx),
            image_protocol: None,
            error: Some("decode failed".to_string()),
        };

        let cloned = cache.clone();

        assert_eq!(cloned.path, cache.path);
        assert_eq!(cloned.picture_type, lofty::picture::PictureType::LeadArtist);
        assert_eq!(cloned.desired_preview_area, cache.desired_preview_area);
        assert_eq!(cloned.encoded_preview_area, cache.encoded_preview_area);
        assert_eq!(cloned.desired_protocol_generation, 7);
        assert_eq!(cloned.encoded_protocol_generation, 7);
        assert_eq!(cloned.encoded_retransmit_generation, 3);
        assert_eq!(cloned.generation, 42);
        assert_eq!(cloned.decoded_generation, Some(42));
        assert!(cloned.decoded_image.is_none());
        assert!(cloned.receiver.is_none());
        assert!(cloned.image_protocol.is_none());
        assert_eq!(cloned.error.as_deref(), Some("decode failed"));
    }

    #[test]
    fn artwork_preview_invalidation_clears_cache_and_advances_generation() {
        let mut state = write_state();
        let before = state.artwork_preview_generation;
        let (_tx, rx) = std::sync::mpsc::channel();
        state.artwork_preview_cache = Some(ArtworkPreviewCache {
            path: std::path::PathBuf::from("/tmp/source.flac"),
            picture_type: lofty::picture::PictureType::Composer,
            desired_preview_area: ratatui::layout::Rect::new(1, 1, 16, 8),
            encoded_preview_area: ratatui::layout::Rect::new(1, 1, 16, 8),
            desired_protocol_generation: 1,
            encoded_protocol_generation: 1,
            encoded_retransmit_generation: 1,
            generation: before,
            decoded_generation: None,
            decoded_image: None,
            receiver: Some(rx),
            image_protocol: None,
            error: None,
        });

        state.invalidate_artwork_preview_cache();

        assert!(state.artwork_preview_cache.is_none());
        assert!(state.artwork_preview_generation > before);
    }

    #[test]
    fn retry_failed_details_probes_clears_only_failed_files() {
        let ok_info = crate::tui::probe::SourceInfo {
            format_name: "flac".to_string(),
            codec: "FLAC".to_string(),
            bit_depth: Some(24),
            sample_format_is_float: None,
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

    #[test]
    fn canonical_metadata_view_promotes_present_preemphasis_and_cue_flags_only() {
        let mut state = MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/album/track.flac")],
            vec![
                tag("TITLE", "Song", vec!["Song"]),
                tag("PRE_EMPHASIS", "1", vec!["1"]),
                tag("CUE_FLAGS", "PRE", vec!["PRE"]),
                tag("X-CUSTOM", "diagnostic", vec!["diagnostic"]),
            ],
            vec!["track.flac".to_string()],
            MetadataTechnicalDetails::default(),
        );

        assert_eq!(state.metadata_view, MetadataEditorView::Canonical);
        assert_eq!(state.visible_metadata_entry_indices(), vec![0, 1, 2]);

        state.set_metadata_view(MetadataEditorView::All);
        assert_eq!(state.visible_metadata_entry_indices(), vec![0, 1, 2, 3]);

        let untagged = MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/album/untagged.flac")],
            vec![tag("TITLE", "Song", vec!["Song"])],
            vec!["untagged.flac".to_string()],
            MetadataTechnicalDetails::default(),
        );
        assert_eq!(untagged.visible_metadata_entry_indices(), vec![0]);
    }

    #[test]
    fn metadata_editor_view_projects_canonical_rows_and_all_expands() {
        let mut state = MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/album/track.flac")],
            vec![
                tag("TITLE", "Song", vec!["Song"]),
                tag("TITLE [ID3v1]", "Legacy Song", vec!["Legacy Song"]),
                tag("X-CUSTOM", "diagnostic", vec!["diagnostic"]),
            ],
            vec!["track.flac".to_string()],
            MetadataTechnicalDetails::default(),
        );

        assert_eq!(state.metadata_view, MetadataEditorView::Canonical);
        assert_eq!(state.visible_metadata_entry_indices(), vec![0]);
        assert!(!state.maximized);

        state.set_metadata_view(MetadataEditorView::All);
        assert_eq!(state.visible_metadata_entry_indices(), vec![0, 1, 2]);
        assert!(state.maximized, "All view must expand the editor");
    }

    #[test]
    fn switching_to_canonical_rehomes_a_hidden_custom_cursor() {
        let mut state = MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/album/track.flac")],
            vec![
                tag("TITLE", "Song", vec!["Song"]),
                tag("X-CUSTOM", "diagnostic", vec!["diagnostic"]),
            ],
            vec!["track.flac".to_string()],
            MetadataTechnicalDetails::default(),
        );
        state.set_metadata_view(MetadataEditorView::All);
        state.cursor = 1;

        state.set_metadata_view(MetadataEditorView::Canonical);

        assert_eq!(state.cursor, 0);
        assert_eq!(state.visible_metadata_rows(), vec![0, 2]);
    }

}

#[cfg(test)]
mod terminal_image_protocol_environment_tests {
    use super::{
        configure_terminal_image_picker_protocol, enforce_safe_terminal_image_picker_protocol,
        terminal_image_protocol_probe_decision_for_environment, tmux_like_environment,
        TerminalImagePickerProtocolProbe, TerminalImageProtocolProbeDecision,
    };
    use std::ffi::OsStr;

    struct RecordingPicker {
        protocol_type: ratatui_image::picker::ProtocolType,
        force_halfblocks_calls: usize,
        guess_protocol_calls: usize,
    }

    impl RecordingPicker {
        fn new(protocol_type: ratatui_image::picker::ProtocolType) -> Self {
            Self {
                protocol_type,
                force_halfblocks_calls: 0,
                guess_protocol_calls: 0,
            }
        }
    }

    impl TerminalImagePickerProtocolProbe for RecordingPicker {
        fn protocol_type(&self) -> ratatui_image::picker::ProtocolType {
            self.protocol_type
        }

        fn force_halfblocks_protocol(&mut self) {
            self.force_halfblocks_calls += 1;
            self.protocol_type = ratatui_image::picker::ProtocolType::Halfblocks;
        }

        fn guess_terminal_protocol(&mut self) {
            self.guess_protocol_calls += 1;
            self.protocol_type = ratatui_image::picker::ProtocolType::Kitty;
        }
    }

    #[test]
    fn tmux_variable_forces_safe_terminal_image_protocol() {
        assert!(tmux_like_environment(
            Some(OsStr::new("/tmp/tmux-1000/default,123,0")),
            None,
            Some(OsStr::new("xterm-kitty")),
            None,
        ));
    }

    #[test]
    fn byobu_tmux_backend_forces_safe_terminal_image_protocol() {
        assert!(tmux_like_environment(
            None,
            None,
            Some(OsStr::new("screen-256color")),
            Some(OsStr::new("tmux")),
        ));
    }

    #[test]
    fn tmux_term_program_and_term_names_force_safe_terminal_image_protocol() {
        assert!(tmux_like_environment(
            None,
            Some(OsStr::new("tmux")),
            Some(OsStr::new("xterm-256color")),
            None,
        ));
        assert!(tmux_like_environment(
            None,
            None,
            Some(OsStr::new("tmux-256color")),
            None,
        ));
        assert!(tmux_like_environment(
            None,
            None,
            Some(OsStr::new("tmux-direct")),
            None,
        ));
    }

    #[test]
    fn direct_kitty_ghostty_or_wezterm_hosts_are_not_forced_to_halfblocks() {
        assert!(!tmux_like_environment(
            None,
            Some(OsStr::new("kitty")),
            Some(OsStr::new("xterm-kitty")),
            None,
        ));
        assert!(!tmux_like_environment(
            None,
            Some(OsStr::new("ghostty")),
            Some(OsStr::new("xterm-ghostty")),
            None,
        ));
        assert!(!tmux_like_environment(
            None,
            Some(OsStr::new("WezTerm")),
            Some(OsStr::new("wezterm")),
            None,
        ));
    }

    #[test]
    fn empty_tmux_variable_does_not_create_a_false_positive() {
        assert!(!tmux_like_environment(
            Some(OsStr::new("")),
            None,
            Some(OsStr::new("xterm-kitty")),
            None,
        ));
    }

    #[test]
    fn tmux_protocol_configuration_never_calls_terminal_probe() {
        let decision = terminal_image_protocol_probe_decision_for_environment(
            Some(OsStr::new("/tmp/tmux-1000/default,123,0")),
            Some(OsStr::new("ghostty")),
            Some(OsStr::new("xterm-ghostty")),
            None,
        );
        assert_eq!(decision, TerminalImageProtocolProbeDecision::ForceHalfblocks);

        let mut picker = RecordingPicker::new(ratatui_image::picker::ProtocolType::Kitty);
        configure_terminal_image_picker_protocol(&mut picker, decision);

        assert_eq!(picker.guess_protocol_calls, 0);
        assert_eq!(picker.force_halfblocks_calls, 1);
        assert_eq!(
            picker.protocol_type,
            ratatui_image::picker::ProtocolType::Halfblocks
        );
    }

    #[test]
    fn direct_terminal_protocol_configuration_calls_terminal_probe_once() {
        let decision = terminal_image_protocol_probe_decision_for_environment(
            None,
            Some(OsStr::new("ghostty")),
            Some(OsStr::new("xterm-ghostty")),
            None,
        );
        assert_eq!(decision, TerminalImageProtocolProbeDecision::GuessProtocol);

        let mut picker = RecordingPicker::new(ratatui_image::picker::ProtocolType::Halfblocks);
        configure_terminal_image_picker_protocol(&mut picker, decision);

        assert_eq!(picker.force_halfblocks_calls, 0);
        assert_eq!(picker.guess_protocol_calls, 1);
        assert_eq!(picker.protocol_type, ratatui_image::picker::ProtocolType::Kitty);
    }

    #[test]
    fn tmux_prepare_guard_replaces_cached_kitty_before_protocol_creation() {
        let mut picker = RecordingPicker::new(ratatui_image::picker::ProtocolType::Kitty);
        let changed = enforce_safe_terminal_image_picker_protocol(
            &mut picker,
            TerminalImageProtocolProbeDecision::ForceHalfblocks,
        );

        assert!(changed);
        assert_eq!(picker.guess_protocol_calls, 0);
        assert_eq!(picker.force_halfblocks_calls, 1);
        assert_eq!(
            picker.protocol_type,
            ratatui_image::picker::ProtocolType::Halfblocks
        );
    }

    #[test]
    fn tmux_prepare_guard_is_idempotent_when_picker_is_already_halfblocks() {
        let mut picker = RecordingPicker::new(ratatui_image::picker::ProtocolType::Halfblocks);
        let changed = enforce_safe_terminal_image_picker_protocol(
            &mut picker,
            TerminalImageProtocolProbeDecision::ForceHalfblocks,
        );

        assert!(!changed);
        assert_eq!(picker.guess_protocol_calls, 0);
        assert_eq!(picker.force_halfblocks_calls, 0);
        assert_eq!(
            picker.protocol_type,
            ratatui_image::picker::ProtocolType::Halfblocks
        );
    }
}

#[cfg(test)]
mod app_state_theme_ownership_tests {
    use super::*;
    use crate::tui::test_support::XdgConfigHomeGuard;

    fn isolated_config_home() -> XdgConfigHomeGuard {
        XdgConfigHomeGuard::new("tonepoet-app-theme-test")
    }

    fn config_with_theme(slug: &str) -> TonepoetConfig {
        let mut config = TonepoetConfig::default();
        config.conversion.persist_queue = false;
        config.ui.theme = slug.to_string();
        config
    }

    #[test]
    fn app_state_new_resolves_configured_theme() {
        let _home = isolated_config_home();

        let app = AppState::new_for_test(config_with_theme("rose-pine-dawn"));

        assert_eq!(app.theme.slug, "rose-pine-dawn");
        assert_eq!(app.config.ui.theme, "rose-pine-dawn");
    }

    #[test]
    fn unknown_theme_slug_falls_back_at_startup_and_canonicalizes_config() {
        let _home = isolated_config_home();

        let app = AppState::new_for_test(config_with_theme("not-a-theme"));

        assert_eq!(app.theme.slug, crate::tui::theme::default_theme_slug());
        assert_eq!(app.config.ui.theme, crate::tui::theme::default_theme_slug());
        assert!(app
            .status_message
            .as_ref()
            .map(|(message, _)| message.contains("Unknown configured theme"))
            .unwrap_or(false));
    }

    #[test]
    fn set_ui_theme_updates_runtime_theme_and_config_slug_together() {
        let home = isolated_config_home();
        let mut app = AppState::new_for_test(config_with_theme("tokyo-night"));

        app.set_ui_theme("kanagawa-lotus");

        assert_eq!(app.theme.slug, "kanagawa-lotus");
        assert_eq!(app.config.ui.theme, "kanagawa-lotus");
        assert!(app.force_redraw);

        let persisted = std::fs::read_to_string(home.path().join("tonepoet/config.toml"))
            .expect("persisted config");
        assert!(persisted.contains("theme = \"kanagawa-lotus\""));
    }

    #[test]
    fn set_ui_theme_rejects_unknown_slugs_without_mutating_theme_or_config() {
        let _home = isolated_config_home();
        let mut app = AppState::new_for_test(config_with_theme("catppuccin"));
        app.force_redraw = false;

        app.set_ui_theme("not-a-theme");

        assert_eq!(app.theme.slug, "catppuccin");
        assert_eq!(app.config.ui.theme, "catppuccin");
        assert!(!app.force_redraw);
        assert!(app
            .status_message
            .as_ref()
            .map(|(message, _)| message.contains("Unknown theme"))
            .unwrap_or(false));
    }



    #[test]
    fn unknown_theme_slug_is_not_persisted_through_ui_actions() {
        let home = isolated_config_home();
        let mut app = AppState::new_for_test(config_with_theme("tokyo-night"));

        app.set_ui_theme("gruvbox");
        let before = std::fs::read_to_string(home.path().join("tonepoet/config.toml"))
            .expect("persisted config after valid theme");
        assert!(before.contains("theme = \"gruvbox\""));

        app.force_redraw = false;
        app.set_ui_theme("not-a-theme");

        assert_eq!(app.theme.slug, "gruvbox");
        assert_eq!(app.config.ui.theme, "gruvbox");
        assert!(!app.force_redraw);
        let after = std::fs::read_to_string(home.path().join("tonepoet/config.toml"))
            .expect("persisted config after invalid theme");
        assert_eq!(after, before, "invalid theme slugs must not be written as valid UI choices");
    }

    #[test]
    fn set_ui_theme_rethemes_existing_active_file_picker() {
        let _home = isolated_config_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new_for_test(config_with_theme("tokyo-night"));
        let initial_picker_theme = crate::tui::keybindings::file_picker_theme_from_theme(&app.theme);
        let picker = tui_file_picker::FilePickerState::new(tui_file_picker::FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            theme: initial_picker_theme.clone(),
            ..tui_file_picker::FilePickerConfig::default()
        });
        app.active_overlay = ActiveOverlay::FilePicker(MetadataFilePickerState::new(
            FilePickerPurpose::Generic { id: "theme-test".to_string() },
            picker,
        ));

        app.set_ui_theme("catppuccin-latte");

        match &app.active_overlay {
            ActiveOverlay::FilePicker(session) => {
                assert_ne!(session.picker.theme().selected.bg, initial_picker_theme.selected.bg);
                assert_eq!(session.picker.theme().selected.bg, Some(app.theme.cyan));
                assert_eq!(session.picker.theme().selected.fg, Some(app.theme.bg));
                assert_eq!(session.picker.theme().progress_dialog.bg, Some(app.theme.progress_dialog_bg));
            }
            other => panic!("expected active file picker, got {other:?}"),
        }
    }

    #[test]
    fn set_ui_theme_rethemes_existing_active_file_task_progress() {
        let _home = isolated_config_home();
        let mut app = AppState::new_for_test(config_with_theme("tokyo-night"));
        let initial_picker_theme = crate::tui::keybindings::file_picker_theme_from_theme(&app.theme);
        let progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Copy,
            "Copying files",
            initial_picker_theme.clone(),
        );
        let (control_tx, _control_rx) = std::sync::mpsc::channel();
        app.active_overlay = ActiveOverlay::FileTaskProgress(FileTaskProgressSession::new(progress, control_tx));

        app.set_ui_theme("rose-pine-dawn");

        match &app.active_overlay {
            ActiveOverlay::FileTaskProgress(session) => {
                assert_ne!(session.progress.theme().progress_dialog.bg, initial_picker_theme.progress_dialog.bg);
                assert_eq!(session.progress.theme().progress_dialog.bg, Some(app.theme.progress_dialog_bg));
                assert_eq!(session.progress.theme().progress_button.fg, Some(app.theme.progress_dialog_button_fg));
                assert_eq!(session.progress.theme().progress_destructive.bg, Some(app.theme.progress_dialog_abort_bg));
            }
            other => panic!("expected active file-task progress, got {other:?}"),
        }
    }

    #[test]
    fn new_file_picker_after_set_ui_theme_uses_app_theme() {
        let _home = isolated_config_home();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new_for_test(config_with_theme("tokyo-night"));

        app.set_ui_theme("kanagawa-lotus");
        let picker = tui_file_picker::FilePickerState::new(tui_file_picker::FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            theme: crate::tui::keybindings::file_picker_theme_from_theme(&app.theme),
            ..tui_file_picker::FilePickerConfig::default()
        });

        assert_eq!(picker.theme().selected.bg, Some(app.theme.cyan));
        assert_eq!(picker.theme().selected.fg, Some(app.theme.bg));
        assert_eq!(picker.theme().progress_dialog.bg, Some(app.theme.progress_dialog_bg));
    }

    #[test]
    fn tokyo_night_remains_the_default_runtime_theme() {
        let _home = isolated_config_home();

        let app = AppState::new_for_test(TonepoetConfig::default());

        assert_eq!(app.config.ui.theme, crate::tui::theme::default_theme_slug());
        assert_eq!(app.theme.slug, crate::tui::theme::default_theme_slug());
    }
}

#[cfg(test)]
mod browse_convert_expansion_lifecycle_tests {
    use super::*;

    fn expansion_request(path: &str) -> crate::tui::command::BrowseConvertExpansionRequest {
        crate::tui::command::BrowseConvertExpansionRequest {
            target: crate::tui::command::BrowseConvertExpansionTarget::ConvertReview {
                preset: None,
                post_load: crate::tui::command::BrowseConvertPostLoad::ReviewOnly,
            },
            selection_snapshot: vec![PathBuf::from(path)],
            browse_in_archive: false,
            dropped_stale_selection_count: 0,
            cue_selection_overrides:
                crate::convert::queue_expansion::QueueCueSelectionOverrides::new(),
        }
    }

    #[test]
    fn starting_browse_convert_expansion_records_pending_generation_and_request() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let request = expansion_request("album");

        let (generation, cancel) = app.begin_browse_convert_expansion(request.clone());

        assert_eq!(generation, app.probe_generation);
        assert!(!cancel.is_cancelled());
        assert!(app.browse_convert_expansion_pending_for(generation, &request));
    }

    #[test]
    fn newer_browse_convert_expansion_cancels_and_replaces_older_one() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let first = expansion_request("album-a");
        let second = expansion_request("album-b");

        let (first_generation, first_cancel) = app.begin_browse_convert_expansion(first.clone());
        let (second_generation, second_cancel) = app.begin_browse_convert_expansion(second.clone());

        assert!(second_generation > first_generation);
        assert!(first_cancel.is_cancelled());
        assert!(!second_cancel.is_cancelled());
        assert!(!app.browse_convert_expansion_pending_for(first_generation, &first));
        assert!(app.browse_convert_expansion_pending_for(second_generation, &second));
    }

    #[test]
    fn stale_completion_cannot_clear_current_browse_convert_expansion() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let first = expansion_request("album-a");
        let second = expansion_request("album-b");

        let (first_generation, _first_cancel) = app.begin_browse_convert_expansion(first.clone());
        let (second_generation, _second_cancel) = app.begin_browse_convert_expansion(second.clone());

        assert!(!app.complete_browse_convert_expansion(first_generation, &first));
        assert!(app.browse_convert_expansion_pending_for(second_generation, &second));
        assert!(app.complete_browse_convert_expansion(second_generation, &second));
        assert!(!app.browse_convert_expansion_pending_for(second_generation, &second));
    }

    #[test]
    fn browse_change_cancellation_cancels_pending_worker_token() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let request = expansion_request("album");
        let (generation, cancel) = app.begin_browse_convert_expansion(request.clone());

        assert!(app.browse_convert_expansion_pending_for(generation, &request));
        assert!(app.cancel_browse_convert_expansion_for_browse_change("browse selection changed"));

        assert!(cancel.is_cancelled());
        assert!(!app.browse_convert_expansion_pending_for(generation, &request));
        assert!(app
            .status_message
            .as_ref()
            .map(|(message, _)| message.contains("browse selection changed"))
            .unwrap_or(false));
    }
}
