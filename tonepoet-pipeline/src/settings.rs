//! Unified conversion settings for the pipeline crate.
//!
//! Runtime source facts live in [`crate::SourceInfo`]. User config such as
//! worker count or UI defaults belongs outside this crate.

use crate::enums::{
    AacProfile, AudioFormat, BitDepthTarget, DitherType, DsdFilterPreset, DsdLowpassMethod,
    DsdNoiseShaper, DsdToPcmGainMode, GainCompensation, ModulatorOrder, Mp3Mode,
    NyquistTransition, OpusContentType,
    PcmBitDepth, PreferredTool, RateTarget, ReplayGainMode, ResampleQuality, SoxSincPhase,
    SsrcPdfType, SsrcProfile, WavPackMode,
};
use crate::error::{PlanningError, Result};
use crate::mapping;

/// Single source of truth for all conversion parameters.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PipelineSettings {
    /// Target output format.
    pub target_format: AudioFormat,
    /// Target sample-rate request.
    pub target_sample_rate: RateTarget,
    /// Target PCM bit-depth request.
    pub target_bit_depth: BitDepthTarget,
    /// User-facing resampling quality.
    pub resample_quality: ResampleQuality,
    /// Nyquist transition preference. `BrickWall` routes through SSRC by default.
    pub nyquist_transition: NyquistTransition,
    /// Exact dither algorithm. No wrapper collapses this value.
    pub dither_type: DitherType,
    /// Tool preference used by the registry when the preferred tool supports the operation.
    pub preferred_tool: PreferredTool,
    /// Encode even when the planner would otherwise choose passthrough copy.
    pub force_encode: bool,
    /// FLAC-specific encoder options.
    pub flac: FlacSettings,
    /// MP3-specific encoder options.
    pub mp3: Mp3Settings,
    /// AAC-specific encoder options.
    pub aac: AacSettings,
    /// Opus-specific encoder options.
    pub opus: OpusSettings,
    /// WavPack-specific encoder options.
    pub wavpack: WavPackSettings,
    /// SSRC brick-wall resampling options.
    pub ssrc: SsrcSettings,
    /// Sox rate-effect resampler overrides.
    pub sox_resampler: SoxResamplerSettings,
    /// Soxr (ffmpeg aresample) resampler overrides.
    pub soxr_resampler: SoxrResamplerSettings,
    /// DSD-specific conversion options.
    pub dsd: DsdSettings,
    /// Metadata and tag behavior.
    pub metadata: MetadataSettings,
    /// Post-encode verification behavior.
    pub verification: VerificationSettings,
    /// ReplayGain scanning behavior.
    pub replay_gain: ReplayGainSettings,
}

impl Default for PipelineSettings {
    fn default() -> Self {
        Self {
            target_format: AudioFormat::Flac,
            target_sample_rate: RateTarget::Source,
            target_bit_depth: BitDepthTarget::Source,
            resample_quality: ResampleQuality::Ultra,
            nyquist_transition: NyquistTransition::Gentle,
            dither_type: DitherType::None,
            preferred_tool: PreferredTool::Auto,
            force_encode: false,
            flac: FlacSettings::default(),
            mp3: Mp3Settings::default(),
            aac: AacSettings::default(),
            opus: OpusSettings::default(),
            wavpack: WavPackSettings::default(),
            ssrc: SsrcSettings::default(),
            sox_resampler: SoxResamplerSettings::default(),
            soxr_resampler: SoxrResamplerSettings::default(),
            dsd: DsdSettings::default(),
            metadata: MetadataSettings::default(),
            verification: VerificationSettings::default(),
            replay_gain: ReplayGainSettings::default(),
        }
    }
}

impl PipelineSettings {
    /// Validate value ranges and incompatible target combinations.
    pub fn validate(&self) -> Result<()> {
        validate_target_format(&self.target_format)?;
        validate_preferred_tool(&self.preferred_tool)?;
        validate_target_rate(self.target_sample_rate)?;
        validate_encoder_settings(self)?;
        validate_metadata(self)?;
        validate_dsd_settings(&self.dsd)?;

        if self.target_format.is_dsd() {
            if matches!(self.target_sample_rate, RateTarget::PcmHz(_)) {
                return Err(PlanningError::invalid_settings(
                    "target_sample_rate",
                    "DSD targets require RateTarget::Dsd or RateTarget::Source",
                ));
            }
            if !matches!(self.target_bit_depth, BitDepthTarget::Source) {
                return Err(PlanningError::invalid_settings(
                    "target_bit_depth",
                    "DSD targets do not accept PCM bit-depth requests",
                ));
            }
        } else if matches!(self.target_sample_rate, RateTarget::Dsd(_)) {
            return Err(PlanningError::invalid_settings(
                "target_sample_rate",
                "PCM targets cannot use RateTarget::Dsd",
            ));
        }

        match (&self.target_format, self.target_bit_depth) {
            (AudioFormat::Alac, BitDepthTarget::Pcm(PcmBitDepth::Int32)) => {
                return Err(PlanningError::invalid_settings(
                    "target_bit_depth",
                    "ALAC 32-bit is not supported by available encoders; choose 24-bit or WavPack/WAV",
                ));
            }
            (
                AudioFormat::Flac | AudioFormat::Alac,
                BitDepthTarget::Pcm(PcmBitDepth::Float32 | PcmBitDepth::Float64),
            ) => {
                return Err(PlanningError::invalid_settings(
                    "target_bit_depth",
                    "FLAC/ALAC floating-point output is not supported; choose 24-bit integer or WAV",
                ));
            }
            (AudioFormat::WavPack, BitDepthTarget::Pcm(PcmBitDepth::Float32 | PcmBitDepth::Float64)) => {
                return Err(PlanningError::invalid_settings(
                    "target_bit_depth",
                    "floating-point WavPack is not yet supported by the conversion carrier; choose 32-bit integer or WAV",
                ));
            }
            _ => {}
        }

        if self.metadata.store_source_audio_md5 && !matches!(&self.target_format, AudioFormat::Flac)
        {
            return Err(PlanningError::invalid_settings(
                "metadata.store_source_audio_md5",
                "source audio MD5 storage is supported only for FLAC targets; FLAC uses format-native STREAMINFO/Vorbis-comment behavior, not ID3v2",
            ));
        }

        if self.flac.verify && self.target_format != AudioFormat::Flac {
            return Err(PlanningError::invalid_settings(
                "flac.verify",
                "native FLAC verification applies only to FLAC targets; use verification.verify_after_encode for generic decode verification",
            ));
        }

        Ok(())
    }

    /// Resolve the requested DSD target rate. When the target rate is `Source`,
    /// callers must supply source facts so the planner can keep the current DSD rate.
    #[must_use]
    pub fn explicit_dsd_rate(&self) -> Option<crate::enums::DsdRate> {
        match self.target_sample_rate {
            RateTarget::Dsd(rate) => Some(rate),
            RateTarget::Source | RateTarget::PcmHz(_) => None,
        }
    }
}

fn validate_target_format(format: &AudioFormat) -> Result<()> {
    if let AudioFormat::Custom {
        extension,
        display_name,
    } = format
    {
        if extension.is_empty() {
            return Err(PlanningError::invalid_settings(
                "target_format.extension",
                "custom format extension cannot be empty",
            ));
        }
        if extension.starts_with('.') || extension.contains('/') || extension.contains('\\') {
            return Err(PlanningError::invalid_settings(
                "target_format.extension",
                "custom format extension must not include a dot, slash, or backslash",
            ));
        }
        if display_name.trim().is_empty() {
            return Err(PlanningError::invalid_settings(
                "target_format.display_name",
                "custom format display name cannot be empty",
            ));
        }
    }
    Ok(())
}

fn validate_preferred_tool(preference: &PreferredTool) -> Result<()> {
    if let PreferredTool::Custom(name) = preference {
        if name.trim().is_empty() {
            return Err(PlanningError::invalid_settings(
                "preferred_tool",
                "custom preferred tool name cannot be empty",
            ));
        }
        if name.contains('/') || name.contains('\\') {
            return Err(PlanningError::invalid_settings(
                "preferred_tool",
                "custom preferred tool must be a binary name, not a path",
            ));
        }
    }
    Ok(())
}

fn validate_target_rate(rate: RateTarget) -> Result<()> {
    match rate {
        RateTarget::Source | RateTarget::Dsd(_) => Ok(()),
        RateTarget::PcmHz(hz) if (8_000..=1_536_000).contains(&hz) => Ok(()),
        RateTarget::PcmHz(_) => Err(PlanningError::invalid_settings(
            "target_sample_rate",
            "PCM sample rate must be between 8000 and 1536000 Hz",
        )),
    }
}

fn validate_encoder_settings(settings: &PipelineSettings) -> Result<()> {
    if settings.flac.compression_level > 8 {
        return Err(PlanningError::invalid_settings(
            "flac.compression_level",
            "expected 0 through 8",
        ));
    }
    if settings.mp3.bitrate_kbps == 0 || settings.aac.bitrate_kbps == 0 {
        return Err(PlanningError::invalid_settings(
            "bitrate_kbps",
            "bitrate must be greater than zero",
        ));
    }
    if !(8..=1000).contains(&settings.mp3.bitrate_kbps) {
        return Err(PlanningError::invalid_settings(
            "mp3.bitrate_kbps",
            "expected 8 through 1000 kbps",
        ));
    }
    if settings.mp3.vbr_quality > 9 {
        return Err(PlanningError::invalid_settings(
            "mp3.vbr_quality",
            "expected 0 through 9",
        ));
    }
    if !(8..=1024).contains(&settings.aac.bitrate_kbps) {
        return Err(PlanningError::invalid_settings(
            "aac.bitrate_kbps",
            "expected 8 through 1024 kbps",
        ));
    }
    if !(6..=510).contains(&settings.opus.bitrate_kbps) {
        return Err(PlanningError::invalid_settings(
            "opus.bitrate_kbps",
            "expected 6 through 510 kbps",
        ));
    }
    if settings.opus.complexity > 10 {
        return Err(PlanningError::invalid_settings(
            "opus.complexity",
            "expected 0 through 10",
        ));
    }
    if settings.wavpack.hybrid && !(24..=9600).contains(&settings.wavpack.hybrid_bitrate_kbps) {
        return Err(PlanningError::invalid_settings(
            "wavpack.hybrid_bitrate_kbps",
            "expected 24 through 9600 kbps/ch",
        ));
    }
    if let Some(bw) = settings.sox_resampler.bandwidth_pct {
        if !(74.0..=99.7).contains(&bw) {
            return Err(PlanningError::invalid_settings(
                "sox_resampler.bandwidth_pct",
                "expected 74.0 through 99.7",
            ));
        }
    }
    if settings.sox_resampler.chebyshev
        && matches!(
            settings.resample_quality,
            ResampleQuality::Low | ResampleQuality::Medium
        )
    {
        return Err(PlanningError::invalid_settings(
            "sox_resampler.chebyshev",
            "chebyshev (-s) requires resample_quality >= High",
        ));
    }
    if let Some(ph) = settings.sox_resampler.phase {
        if ph > 100 {
            return Err(PlanningError::invalid_settings(
                "sox_resampler.phase",
                "expected 0 through 100",
            ));
        }
    }
    if let Some(cutoff) = settings.soxr_resampler.cutoff {
        if !(0.0..=1.0).contains(&cutoff) {
            return Err(PlanningError::invalid_settings(
                "soxr_resampler.cutoff",
                "expected 0.0 through 1.0",
            ));
        }
    }
    if let Some(ph) = settings.soxr_resampler.phase {
        if ph > 100 {
            return Err(PlanningError::invalid_settings(
                "soxr_resampler.phase",
                "expected 0 through 100",
            ));
        }
    }
    if let Some(att) = settings.ssrc.attenuation_db {
        if !(0.0..=99.9).contains(&att) {
            return Err(PlanningError::invalid_settings(
                "ssrc.attenuation_db",
                "expected 0.0 through 99.9 dB",
            ));
        }
    }
    validate_ssrc_dither_settings(settings)?;
    if let Some(taps) = settings.sox_resampler.sinc_taps {
        if !taps.is_power_of_two() || !(1024..=67_108_864).contains(&taps) {
            return Err(PlanningError::invalid_settings(
                "sox_resampler.sinc_taps",
                "must be a power of 2 between 1024 and 67108864",
            ));
        }
    }
    if let Some(att) = settings.sox_resampler.sinc_attenuation_db {
        if !(80..=200).contains(&att) {
            return Err(PlanningError::invalid_settings(
                "sox_resampler.sinc_attenuation_db",
                "expected 80 through 200 dB",
            ));
        }
    }
    if let Some(pb) = settings.sox_resampler.sinc_passband_hz {
        if !(1.0..=220_000.0).contains(&pb) {
            return Err(PlanningError::invalid_settings(
                "sox_resampler.sinc_passband_hz",
                "expected 1 through 220000 Hz",
            ));
        }
    }
    if let Some(tr) = settings.sox_resampler.sinc_transition_hz {
        if !(1.0..=5000.0).contains(&tr) {
            return Err(PlanningError::invalid_settings(
                "sox_resampler.sinc_transition_hz",
                "expected 1 through 5000 Hz",
            ));
        }
    }
    if let Some(beta) = settings.sox_resampler.sinc_kaiser_beta {
        if !(0.0..=32.0).contains(&beta) {
            return Err(PlanningError::invalid_settings(
                "sox_resampler.sinc_kaiser_beta",
                "expected 0 through 32",
            ));
        }
    }
    Ok(())
}

fn validate_metadata(settings: &PipelineSettings) -> Result<()> {
    // Format-specific metadata and ReplayGain feasibility is decided by the
    // selected plugin during registry dispatch. This validation only checks
    // relationships that are intrinsic to the unified settings themselves.
    // That keeps custom tool plugins plannable for formats the built-ins do
    // not handle.
    if settings.metadata.store_source_audio_md5 && !settings.metadata.transfer_tags {
        return Err(PlanningError::invalid_settings(
            "metadata.store_source_audio_md5",
            "storing a source MD5 tag requires metadata.transfer_tags",
        ));
    }
    Ok(())
}

fn validate_dsd_settings(settings: &DsdSettings) -> Result<()> {
    validate_finite_f32(
        "dsd.dsd_to_pcm_auto_gain_margin_db",
        settings.dsd_to_pcm_auto_gain_margin_db,
    )?;
    if !(0.0..=6.0).contains(&settings.dsd_to_pcm_auto_gain_margin_db) {
        return Err(PlanningError::invalid_settings(
            "dsd.dsd_to_pcm_auto_gain_margin_db",
            "auto gain safety margin must be between 0 and 6 dB",
        ));
    }
    if let Some(gain_db) = settings.dsd_to_pcm_gain_db {
        validate_finite_f32("dsd.dsd_to_pcm_gain_db", gain_db)?;
        if !(-24.0..=24.0).contains(&gain_db) {
            return Err(PlanningError::invalid_settings(
                "dsd.dsd_to_pcm_gain_db",
                "gain must be between -24 and +24 dB",
            ));
        }
    }
    if settings.dsd_to_pcm_gain_mode == DsdToPcmGainMode::Manual
        && settings.dsd_to_pcm_gain_db.is_none()
    {
        return Err(PlanningError::invalid_settings(
            "dsd.dsd_to_pcm_gain_db",
            "manual DSD-to-PCM gain requires a dB value",
        ));
    }
    match settings.gain_compensation {
        GainCompensation::Linear(value) => {
            validate_finite_f32("dsd.gain_compensation", value)?;
            if !(0.0..=64.0).contains(&value) {
                return Err(PlanningError::invalid_settings(
                    "dsd.gain_compensation",
                    "linear gain must be between 0 and 64",
                ));
            }
        }
        GainCompensation::Decibels(value) => {
            validate_finite_f32("dsd.gain_compensation", value)?;
            if !(-48.0..=48.0).contains(&value) {
                return Err(PlanningError::invalid_settings(
                    "dsd.gain_compensation",
                    "decibel gain must be between -48 and +48 dB",
                ));
            }
        }
        GainCompensation::Auto | GainCompensation::Disabled => {}
    }
    if let Some(trellis) = settings.trellis {
        if trellis.lookahead == 0 || trellis.nodes == 0 {
            return Err(PlanningError::invalid_settings(
                "dsd.trellis",
                "lookahead and nodes must be greater than zero",
            ));
        }
        if trellis.lookahead > 64 || trellis.nodes > 64 {
            return Err(PlanningError::invalid_settings(
                "dsd.trellis",
                "lookahead and nodes must be at most 64",
            ));
        }
    }
    let sinc = settings.sinc;
    if sinc.oversample_factor == 0 || sinc.oversample_factor.count_ones() != 1 {
        return Err(PlanningError::invalid_settings(
            "dsd.sinc.oversample_factor",
            "oversample factor must be a positive power of two",
        ));
    }
    if sinc.taps < 1024 || sinc.taps.count_ones() != 1 {
        return Err(PlanningError::invalid_settings(
            "dsd.sinc.taps",
            "tap count must be a power of two and at least 1024",
        ));
    }
    validate_finite_f32("dsd.sinc.passband_hz", sinc.passband_hz)?;
    validate_finite_f32("dsd.sinc.transition_hz", sinc.transition_hz)?;
    validate_finite_f32("dsd.sinc.kaiser_beta", sinc.kaiser_beta)?;
    if !(0.0..=220_000.0).contains(&sinc.passband_hz) || sinc.passband_hz == 0.0 {
        return Err(PlanningError::invalid_settings(
            "dsd.sinc.passband_hz",
            "passband must be greater than zero and no more than 220000 Hz",
        ));
    }
    if !(1.0..=5_000.0).contains(&sinc.transition_hz) {
        return Err(PlanningError::invalid_settings(
            "dsd.sinc.transition_hz",
            "transition must be between 1 and 5000 Hz",
        ));
    }
    if !(0.0..=32.0).contains(&sinc.kaiser_beta) {
        return Err(PlanningError::invalid_settings(
            "dsd.sinc.kaiser_beta",
            "kaiser beta must be between 0 and 32",
        ));
    }
    Ok(())
}

fn validate_finite_f32(field: &'static str, value: f32) -> Result<()> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(PlanningError::invalid_settings(
            field,
            "value must be finite",
        ))
    }
}

/// FLAC-specific encoder settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FlacSettings {
    /// Compression level, 0 through 8.
    pub compression_level: u8,
    /// Verify while encoding where the encoder supports it.
    pub verify: bool,
    /// Write MD5 checksum of raw audio to STREAMINFO. Default: true.
    #[cfg_attr(feature = "serde", serde(default = "default_true"))]
    pub write_md5: bool,
}

#[cfg(feature = "serde")]
fn default_true() -> bool { true }

impl Default for FlacSettings {
    fn default() -> Self {
        Self {
            compression_level: 8,
            verify: false,
            write_md5: true,
        }
    }
}

/// MP3-specific encoder settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Mp3Settings {
    /// MP3 rate-control mode.
    pub mode: Mp3Mode,
    /// CBR/ABR bitrate in kbps.
    pub bitrate_kbps: u32,
    /// VBR quality, 0 best through 9 lowest.
    pub vbr_quality: u8,
}

impl Default for Mp3Settings {
    fn default() -> Self {
        Self {
            mode: Mp3Mode::Vbr,
            bitrate_kbps: 320,
            vbr_quality: 0,
        }
    }
}

/// AAC-specific encoder settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AacSettings {
    /// AAC profile.
    pub profile: AacProfile,
    /// Target bitrate in kbps.
    pub bitrate_kbps: u32,
}

impl Default for AacSettings {
    fn default() -> Self {
        Self {
            profile: AacProfile::LcAac,
            bitrate_kbps: 256,
        }
    }
}

/// Opus-specific encoder settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OpusSettings {
    /// Encoder application mode.
    pub content_type: OpusContentType,
    /// Target bitrate in kbps.
    pub bitrate_kbps: u32,
    /// libopus complexity, 0 through 10.
    pub complexity: u8,
}

impl Default for OpusSettings {
    fn default() -> Self {
        Self {
            content_type: OpusContentType::Auto,
            bitrate_kbps: 192,
            complexity: 10,
        }
    }
}

/// WavPack-specific encoder settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WavPackSettings {
    /// Compression mode.
    pub mode: WavPackMode,
    /// Enable hybrid (lossy) mode at a target bitrate.
    pub hybrid: bool,
    /// Target bitrate in kbps per channel for hybrid mode. Valid 24–9600.
    pub hybrid_bitrate_kbps: u32,
    /// Write lossless correction (.wvc) sidecar alongside hybrid .wv.
    pub correction_file: bool,
}

impl Default for WavPackSettings {
    fn default() -> Self {
        Self {
            mode: WavPackMode::Normal,
            hybrid: false,
            hybrid_bitrate_kbps: 320,
            correction_file: true,
        }
    }
}

/// SSRC brick-wall resampler settings.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SsrcSettings {
    /// Force SSRC when `nyquist_transition` is `BrickWall`.
    pub force: bool,
    /// Force the highest SSRC profile.
    pub insane_mode: bool,
    /// Optional explicit profile. `insane_mode` wins.
    pub profile: Option<SsrcProfile>,
    /// Output attenuation in dB (0.0–99.9). None = no attenuation.
    pub attenuation_db: Option<f32>,
    /// Use minimum phase filters instead of linear phase.
    pub min_phase: bool,
    /// Explicit SSRC `--dither` ID. None derives a pair from the global dither choice.
    pub dither_id: Option<u8>,
    /// Probability distribution function for dithering. None derives from the dither choice.
    pub pdf_type: Option<SsrcPdfType>,
}

impl Default for SsrcSettings {
    fn default() -> Self {
        Self {
            force: false,
            insane_mode: false,
            profile: None,
            attenuation_db: None,
            min_phase: false,
            dither_id: None,
            pdf_type: None,
        }
    }
}

/// Sox rate-effect resampler settings. `Option` fields override derived
/// values from `ResampleQuality`/`NyquistTransition` when `Some`.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SoxResamplerSettings {
    /// Enable steep/Chebyshev filter (`-s` flag). Only valid with quality ≥ High.
    pub chebyshev: bool,
    /// Override bandwidth percentage (74.0–99.7). Mutually exclusive with chebyshev.
    pub bandwidth_pct: Option<f32>,
    /// Phase shift (0–100) for rate effect.
    pub phase: Option<u8>,
    /// Allow aliasing (`-a` flag) on rate effect.
    pub allow_aliasing: bool,
    // ── Sinc FIR pre-filter parameters (added before `rate` in effects chain) ──
    /// FIR tap count (power of 2, 1024–67108864).
    pub sinc_taps: Option<u32>,
    /// Stopband attenuation in dB (80–200).
    pub sinc_attenuation_db: Option<u16>,
    /// Lowpass passband corner frequency in Hz (1–220000).
    pub sinc_passband_hz: Option<f32>,
    /// Transition bandwidth in Hz (1–5000).
    pub sinc_transition_hz: Option<f32>,
    /// Kaiser window beta parameter (0–32).
    pub sinc_kaiser_beta: Option<f32>,
    /// Sinc filter phase mode (-L/-M/-I).
    pub sinc_phase: Option<SoxSincPhase>,
}

impl Default for SoxResamplerSettings {
    fn default() -> Self {
        Self {
            chebyshev: false,
            bandwidth_pct: None,
            phase: None,
            allow_aliasing: false,
            sinc_taps: None,
            sinc_attenuation_db: None,
            sinc_passband_hz: None,
            sinc_transition_hz: None,
            sinc_kaiser_beta: None,
            sinc_phase: None,
        }
    }
}

/// Soxr (ffmpeg aresample) resampler settings. `Option` fields override
/// derived values from `ResampleQuality`/`NyquistTransition` when `Some`.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SoxrResamplerSettings {
    /// Enable Chebyshev filter (`cheby=1`).
    pub chebyshev: bool,
    /// Override cutoff (0.0–1.0).
    pub cutoff: Option<f32>,
    /// Phase shift (0–100).
    pub phase: Option<u8>,
}

impl Default for SoxrResamplerSettings {
    fn default() -> Self {
        Self {
            chebyshev: false,
            cutoff: None,
            phase: None,
        }
    }
}

/// DSD-specific conversion settings. The target DSD rate lives in
/// [`PipelineSettings::target_sample_rate`] to avoid duplicate rate fields.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DsdSettings {
    /// SoX-DSD noise-shaper family.
    pub noise_shaper: DsdNoiseShaper,
    /// Modulator order.
    pub modulator_order: ModulatorOrder,
    /// Optional trellis optimization.
    pub trellis: Option<TrellisSettings>,
    /// PCM-to-DSD filter preset.
    pub pcm_to_dsd_filter: DsdFilterPreset,
    /// DSD-to-PCM low-pass method.
    pub dsd_to_pcm_lowpass: DsdLowpassMethod,
    /// DSD-to-PCM gain strategy.
    #[cfg_attr(feature = "serde", serde(default))]
    pub dsd_to_pcm_gain_mode: DsdToPcmGainMode,
    /// Auto-gain peak safety margin in dB. `0.15` means normalize to -0.15 dBFS.
    #[cfg_attr(feature = "serde", serde(default = "default_dsd_to_pcm_auto_gain_margin_db"))]
    pub dsd_to_pcm_auto_gain_margin_db: f32,
    /// Optional fixed DSD-to-PCM gain in dB for manual mode.
    ///
    /// When `dsd_to_pcm_gain_mode` is `Disabled`, a `Some` value is still honored
    /// as a compatibility path for older callers that set only this field.
    pub dsd_to_pcm_gain_db: Option<f32>,
    /// Sinc-filter parameters used when a sinc preset is selected.
    pub sinc: SincFilterSettings,
    /// Gain compensation for PCM-to-DSD sinc upsampling.
    pub gain_compensation: GainCompensation,
}

fn default_dsd_to_pcm_auto_gain_margin_db() -> f32 {
    0.15
}

impl Default for DsdSettings {
    fn default() -> Self {
        Self {
            noise_shaper: DsdNoiseShaper::Clans,
            modulator_order: ModulatorOrder::Order8,
            trellis: None,
            pcm_to_dsd_filter: DsdFilterPreset::Auto,
            dsd_to_pcm_lowpass: DsdLowpassMethod::Auto,
            dsd_to_pcm_gain_mode: DsdToPcmGainMode::Disabled,
            dsd_to_pcm_auto_gain_margin_db: default_dsd_to_pcm_auto_gain_margin_db(),
            dsd_to_pcm_gain_db: None,
            sinc: SincFilterSettings::default(),
            gain_compensation: GainCompensation::Auto,
        }
    }
}

/// Trellis optimization parameters for SoX-DSD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TrellisSettings {
    /// Lookahead depth, normally 1 through 64.
    pub lookahead: u8,
    /// Number of trellis nodes, normally 1 through 64.
    pub nodes: u8,
    /// Optional latency override.
    pub latency: Option<u16>,
}

/// FIR sinc parameters for DSD conversion paths.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SincFilterSettings {
    /// Zero-insertion upsample factor for PCM-to-DSD sinc mode.
    pub oversample_factor: u32,
    /// FIR tap count.
    pub taps: u32,
    /// Pass-band corner in Hz.
    pub passband_hz: f32,
    /// Transition-band width in Hz.
    pub transition_hz: f32,
    /// Kaiser beta.
    pub kaiser_beta: f32,
    /// Linear phase when true, minimum phase when false.
    pub linear_phase: bool,
    /// Allow aliasing for creative/non-transparent workflows.
    pub allow_aliasing: bool,
}

impl Default for SincFilterSettings {
    fn default() -> Self {
        Self {
            oversample_factor: 8,
            taps: 262_144,
            passband_hz: 25_000.0,
            transition_hz: 500.0,
            kaiser_beta: 16.0,
            linear_phase: true,
            allow_aliasing: false,
        }
    }
}

/// Metadata behavior consumed by encoder and tagging stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MetadataSettings {
    /// Transfer source tags when the selected tool supports it.
    pub transfer_tags: bool,
    /// Preserve artwork/video streams where safe. Most audio encoders disable video by default.
    pub preserve_artwork: bool,
    /// Store source-audio MD5 as a format-appropriate tag. FLAC uses Vorbis comments,
    /// never ID3v2 tags.
    pub store_source_audio_md5: bool,
}

impl Default for MetadataSettings {
    fn default() -> Self {
        Self {
            transfer_tags: true,
            preserve_artwork: true,
            store_source_audio_md5: false,
        }
    }
}


fn validate_ssrc_dither_settings(settings: &PipelineSettings) -> Result<()> {
    if let Some(dither_id) = settings.ssrc.dither_id {
        if dither_id > 99 {
            return Err(PlanningError::invalid_settings(
                "ssrc.dither_id",
                "expected 0 through 99",
            ));
        }
    }

    if !settings_may_emit_ssrc_integer_dither(settings) {
        return Ok(());
    }

    let RateTarget::PcmHz(target_rate_hz) = settings.target_sample_rate else {
        return Ok(());
    };

    if let Some(dither_id) = settings.ssrc.dither_id {
        mapping::validate_ssrc_dither_id_for_rate(dither_id, target_rate_hz)
    } else {
        mapping::ssrc_dither_selection_for_rate(settings.dither_type, target_rate_hz).map(|_| ())
    }
}

fn settings_may_emit_ssrc_integer_dither(settings: &PipelineSettings) -> bool {
    let uses_ssrc = matches!(settings.preferred_tool, PreferredTool::Ssrc)
        || settings.ssrc.force
        || matches!(settings.nyquist_transition, NyquistTransition::BrickWall);
    let integer_or_unknown_output = !matches!(
        settings.target_bit_depth,
        BitDepthTarget::Pcm(PcmBitDepth::Float32) | BitDepthTarget::Pcm(PcmBitDepth::Float64)
    );

    uses_ssrc && integer_or_unknown_output
}

/// Verification settings consumed after encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VerificationSettings {
    /// Add a post-encode decoding check.
    pub verify_after_encode: bool,
    /// For FLAC, prefer `flac -t -s` over generic decode-to-null validation.
    pub prefer_native_flac_verify: bool,
}

impl Default for VerificationSettings {
    fn default() -> Self {
        Self {
            verify_after_encode: false,
            prefer_native_flac_verify: true,
        }
    }
}

/// Policy for existing ReplayGain tags on every output track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ReplayGainExistingTagPolicy {
    /// Always run the scanner, preserving historical behavior.
    Rescan,
    /// Skip only when every output carries every non-empty tag required by the selected mode.
    SkipIfComplete,
}

impl Default for ReplayGainExistingTagPolicy {
    fn default() -> Self {
        Self::Rescan
    }
}

/// ReplayGain post-processing settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ReplayGainSettings {
    /// Optional ReplayGain scanning mode.
    pub mode: Option<ReplayGainMode>,
    /// Avoid clipping where the scanner supports it.
    pub prevent_clipping: bool,
    /// Whether complete existing ReplayGain tag sets may suppress rescanning.
    #[cfg_attr(feature = "serde", serde(default))]
    pub existing_tags: ReplayGainExistingTagPolicy,
}

impl Default for ReplayGainSettings {
    fn default() -> Self {
        Self {
            mode: None,
            prevent_clipping: true,
            existing_tags: ReplayGainExistingTagPolicy::Rescan,
        }
    }
}

/// Returns the conservative default PCM depth for a target format.
#[must_use]
pub fn default_pcm_depth_for_format(format: &AudioFormat) -> PcmBitDepth {
    match format {
        AudioFormat::Wav => PcmBitDepth::Int24,
        AudioFormat::Aiff => PcmBitDepth::Int24,
        AudioFormat::Flac | AudioFormat::WavPack | AudioFormat::Alac => PcmBitDepth::Int24,
        AudioFormat::Mp3 | AudioFormat::Aac | AudioFormat::Opus | AudioFormat::Dts | AudioFormat::Ac3 => PcmBitDepth::Int16,
        AudioFormat::Dsf | AudioFormat::Dff | AudioFormat::Custom { .. } => PcmBitDepth::Int24,
    }
}


#[cfg(test)]
mod depth_honesty_validation_tests {
    use super::*;
    use crate::enums::{AudioFormat, BitDepthTarget, PcmBitDepth};

    #[test]
    fn alac_int32_is_rejected_with_actionable_message() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Alac;
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        let err = settings.validate().expect_err("ALAC 32-bit must fail closed");
        assert!(err.to_string().contains("ALAC 32-bit"), "{err}");
    }

    #[test]
    fn wavpack_float_targets_are_rejected() {
        for depth in [PcmBitDepth::Float32, PcmBitDepth::Float64] {
            let mut settings = PipelineSettings::default();
            settings.target_format = AudioFormat::WavPack;
            settings.target_bit_depth = BitDepthTarget::Pcm(depth);
            settings.validate().expect_err("WavPack float must fail closed");
        }
    }

    #[test]
    fn flac_int32_and_aiff_float_remain_valid_settings() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        settings.validate().expect("FLAC 32-bit is honored, not rejected");

        settings.target_format = AudioFormat::Aiff;
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Float32);
        settings.validate().expect("AIFF float is honored, not rejected");
    }
}

#[cfg(test)]
mod ssrc_rate_dependent_dither_validation_tests {
    use super::*;

    #[test]
    fn validates_explicit_ssrc_dither_id_against_explicit_pcm_rate() {
        let mut settings = PipelineSettings::default();
        settings.preferred_tool = PreferredTool::Ssrc;
        settings.nyquist_transition = NyquistTransition::BrickWall;
        settings.target_sample_rate = RateTarget::PcmHz(96_000);
        settings.ssrc.dither_id = Some(16);
        assert!(settings.validate().is_err());

        settings.ssrc.dither_id = Some(2);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn rejects_low_rate_unavailable_ssrc_dither_id() {
        let mut settings = PipelineSettings::default();
        settings.preferred_tool = PreferredTool::Ssrc;
        settings.nyquist_transition = NyquistTransition::BrickWall;
        settings.target_sample_rate = RateTarget::PcmHz(22_050);
        settings.ssrc.dither_id = Some(6);
        assert!(settings.validate().is_err());

        settings.ssrc.dither_id = Some(1);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn defers_rate_dependent_ssrc_dither_validation_when_target_rate_is_source() {
        let mut settings = PipelineSettings::default();
        settings.target_sample_rate = RateTarget::Source;
        settings.ssrc.dither_id = Some(16);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn validates_derived_global_ssrc_dither_mapping_for_explicit_pcm_rate() {
        let mut settings = PipelineSettings::default();
        settings.preferred_tool = PreferredTool::Ssrc;
        settings.target_sample_rate = RateTarget::PcmHz(176_400);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
        settings.dither_type = DitherType::HighShibata;
        assert!(settings.validate().is_err());

        settings.dither_type = DitherType::Tpdf;
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn validates_derived_ssrc_mapping_when_only_pdf_is_explicit() {
        let mut settings = PipelineSettings::default();
        settings.preferred_tool = PreferredTool::Ssrc;
        settings.target_sample_rate = RateTarget::PcmHz(176_400);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
        settings.dither_type = DitherType::Shibata;
        settings.ssrc.pdf_type = Some(SsrcPdfType::Triangular);
        assert!(settings.validate().is_err());
    }

    #[test]
    fn explicit_sample_rate_independent_ssrc_dither_id_can_override_invalid_global_mapping() {
        let mut settings = PipelineSettings::default();
        settings.preferred_tool = PreferredTool::Ssrc;
        settings.target_sample_rate = RateTarget::PcmHz(176_400);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
        settings.dither_type = DitherType::HighShibata;
        settings.ssrc.dither_id = Some(99);
        settings.ssrc.pdf_type = Some(SsrcPdfType::Triangular);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn skips_derived_ssrc_dither_validation_for_explicit_float_output() {
        let mut settings = PipelineSettings::default();
        // Float targets are only valid for WAV/AIFF now (FLAC/ALAC reject).
        settings.target_format = AudioFormat::Wav;
        settings.preferred_tool = PreferredTool::Ssrc;
        settings.target_sample_rate = RateTarget::PcmHz(176_400);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Float32);
        settings.dither_type = DitherType::HighShibata;
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn validates_derived_ssrc_mapping_for_brickwall_auto_tool_settings() {
        let mut settings = PipelineSettings::default();
        settings.preferred_tool = PreferredTool::Auto;
        settings.nyquist_transition = NyquistTransition::BrickWall;
        settings.target_sample_rate = RateTarget::PcmHz(176_400);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
        settings.dither_type = DitherType::HighShibata;
        assert!(settings.validate().is_err());
    }
}
