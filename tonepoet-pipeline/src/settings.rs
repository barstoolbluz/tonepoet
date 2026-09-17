// append-only v15 checker source marker: SoxNg14801V15
//! Unified conversion settings for the pipeline crate.
//!
//! Runtime source facts live in [`crate::SourceInfo`]. User config such as
//! worker count or UI defaults belongs outside this crate.

use crate::enums::{
    AacProfile, AudioFormat, BitDepthTarget, DitherType, DsdFilterPreset, DsdLowpassMethod,
    DsdNoiseShaper, GainCompensation,
    ModulatorOrder, Mp3Mode,
    NyquistTransition, OpusContentType,
    PcmBitDepth, PreferredTool, RateTarget, ReplayGainMode, ResampleQuality, SoxSincPhase,
    SsrcPdfType, SsrcProfile, WavPackMode,
};
use crate::dsd_reference::{
    reference_error_text, DbNano, DsdReferencePolicyVersion,
    DsdSourceGainMode, DsdSourcePathway, DsdSourceSettings, ReferenceErrorCode,
};
use crate::error::{PlanningError, Result};

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
    /// Whether the current dither selection was explicitly chosen by the user
    /// (including preset restoration), rather than selected by policy.
    #[cfg_attr(feature = "serde", serde(default))]
    pub dither_explicit: bool,
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
    /// True-peak measurement and linear-gain policy for ordinary PCM conversions.
    #[cfg_attr(feature = "serde", serde(default))]
    pub pcm_true_peak: PcmTruePeakGainSettings,
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
            dither_explicit: false,
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
            pcm_true_peak: PcmTruePeakGainSettings::default(),
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
        validate_pcm_true_peak_settings(&self.pcm_true_peak, &self.target_format)?;
        // WavPack hybrid is lossy for true-peak policy, but its qualified path
        // realizes bounded integer PCM before native encoding, so configured
        // terminal dither is covered there. This rejection remains specific to
        // direct lossy encoder-input paths whose dither has no terminal bound.
        if self.pcm_true_peak.is_true_peak()
            && self.target_format.is_lossy()
            && self.dither_type != DitherType::None
        {
            return Err(PlanningError::invalid_settings(
                "dither_type",
                "PCM certified true-peak gain with a lossy target requires dither off: the governed contract ends at encoder-input PCM, and the configured lossy dither/noise-shaping path has no proved terminal-error bound",
            ));
        }

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
    if settings.runtime_album_gain_db().is_some() && !settings.album_true_peak_gain_selected() {
        return Err(PlanningError::invalid_settings(
            "dsd.runtime_album_gain_db",
            "runtime album gain may be bound only for an active album-scoped certified DSD true-peak policy",
        ));
    }
    if settings.runtime_album_track_count().is_some() && settings.runtime_album_gain_db().is_none() {
        return Err(PlanningError::invalid_settings(
            "dsd.runtime_album_track_count",
            "runtime album measurement context requires a bound album gain",
        ));
    }
    if settings.runtime_album_loudest_peak_dbfs().is_some()
        && settings.runtime_album_gain_db().is_none()
    {
        return Err(PlanningError::invalid_settings(
            "dsd.runtime_album_loudest_peak_dbfs",
            "runtime album measurement context requires a bound album gain",
        ));
    }
    if settings.runtime_album_track_count() == Some(0) {
        return Err(PlanningError::invalid_settings(
            "dsd.runtime_album_track_count",
            "runtime album measurement context must represent at least one DSD track",
        ));
    }
    if settings.runtime_album_loudest_peak_dbfs().is_some()
        && settings.runtime_album_track_count().is_none()
    {
        return Err(PlanningError::invalid_settings(
            "dsd.runtime_album_track_count",
            "a finite album peak requires the measured DSD track count",
        ));
    }

    validate_sample_gain_policy("dsd.general_from_dsd.gain", settings.general_from_dsd.gain)?;
    if settings.general_from_dsd.reconstruction == DsdGeneralReconstruction::General
        && settings.general_from_dsd.export_level == DsdGeneralExportLevel::ProtectedR64
    {
        return Err(PlanningError::invalid_settings(
            "dsd.general_from_dsd.export_level",
            "protected_r64 export requires reference_protected reconstruction",
        ));
    }
    if let DsdGeneralExportLevel::NativeWithOffset { offset_db } = settings.general_from_dsd.export_level {
        if !(DbNano::MIN_FIXED_GAIN..=DbNano::MAX_FIXED_GAIN).contains(&offset_db) {
            return Err(PlanningError::invalid_settings(
                "dsd.general_from_dsd.export_level.offset_db",
                "native export offset must be between -24.000000000 and +24.000000000 dB",
            ));
        }
    }

    match settings.from_dsd.pathway {
        DsdSourcePathway::General => {}
        DsdSourcePathway::Manual => {
            return Err(PlanningError::invalid_settings(
                "dsd.from_dsd.pathway",
                reference_error_text(ReferenceErrorCode::ManualUnavailable),
            ));
        }
        DsdSourcePathway::Reference => {
            if settings.general_from_dsd.gain != SampleGainPolicy::Off {
                return Err(PlanningError::invalid_settings(
                    "dsd.general_from_dsd.gain",
                    "general DSD sample-domain gain is incompatible with explicit Reference delivery",
                ));
            }
            if settings.from_dsd.reference_policy != DsdReferencePolicyVersion::SoxNg14801V16 {
                return Err(PlanningError::invalid_settings(
                    "dsd.from_dsd.reference_policy",
                    reference_error_text(ReferenceErrorCode::Toolchain),
                ));
            }
            match settings.from_dsd.gain_mode {
                DsdSourceGainMode::Fixed => {
                    let value = settings.from_dsd.fixed_gain_db.ok_or_else(|| {
                        PlanningError::invalid_settings(
                            "dsd.from_dsd.fixed_gain_db",
                            "fixed Reference gain requires a dB value",
                        )
                    })?;
                    if !(DbNano::MIN_FIXED_GAIN..=DbNano::MAX_FIXED_GAIN).contains(&value) {
                        return Err(PlanningError::invalid_settings(
                            "dsd.from_dsd.fixed_gain_db",
                            "fixed Reference gain must be between -24.000000000 and +24.000000000 dB",
                        ));
                    }
                }
                DsdSourceGainMode::Reference
                | DsdSourceGainMode::NativeLevel
                | DsdSourceGainMode::NormalizePeak => {
                    if settings.from_dsd.fixed_gain_db.is_some() {
                        return Err(PlanningError::invalid_settings(
                            "dsd.from_dsd.fixed_gain_db",
                            "fixed gain is valid only when Reference gain mode is fixed",
                        ));
                    }
                }
            }
            if !(DbNano::MIN_NORMALIZE_TARGET..=DbNano::MAX_NORMALIZE_TARGET)
                .contains(&settings.from_dsd.normalize_peak_target_dbfs)
            {
                return Err(PlanningError::invalid_settings(
                    "dsd.from_dsd.normalize_peak_target_dbfs",
                    "sample-peak normalize target must be between -12.000000000 and 0.000000000 dBFS",
                ));
            }
        }
    }

    match settings.pcm_to_dsd.gain_compensation {
        GainCompensation::Linear(value) => {
            validate_finite_f32("dsd.pcm_to_dsd.gain_compensation", value)?;
            if !(0.0..=64.0).contains(&value) {
                return Err(PlanningError::invalid_settings(
                    "dsd.pcm_to_dsd.gain_compensation",
                    "linear gain must be between 0 and 64",
                ));
            }
        }
        GainCompensation::Decibels(value) => {
            validate_finite_f32("dsd.pcm_to_dsd.gain_compensation", value)?;
            if !(-48.0..=48.0).contains(&value) {
                return Err(PlanningError::invalid_settings(
                    "dsd.pcm_to_dsd.gain_compensation",
                    "decibel gain must be between -48 and +48 dB",
                ));
            }
        }
        GainCompensation::Auto | GainCompensation::Disabled => {}
    }
    if let Some(trellis) = settings.pcm_to_dsd.trellis {
        if trellis.lookahead == 0 || trellis.nodes == 0 {
            return Err(PlanningError::invalid_settings(
                "dsd.pcm_to_dsd.trellis",
                "lookahead and nodes must be greater than zero",
            ));
        }
        if trellis.lookahead > 64 || trellis.nodes > 64 {
            return Err(PlanningError::invalid_settings(
                "dsd.pcm_to_dsd.trellis",
                "lookahead and nodes must be at most 64",
            ));
        }
    }
    let to_dsd = settings.pcm_to_dsd.sinc;
    if to_dsd.oversample_factor == 0 || to_dsd.oversample_factor.count_ones() != 1 {
        return Err(PlanningError::invalid_settings(
            "dsd.pcm_to_dsd.sinc.oversample_factor",
            "oversample factor must be a positive power of two",
        ));
    }
    validate_sinc_common(
        "dsd.pcm_to_dsd.sinc",
        to_dsd.taps,
        to_dsd.passband_hz,
        to_dsd.transition_hz,
        to_dsd.kaiser_beta,
    )?;
    let from_dsd = settings.general_from_dsd.sinc;
    validate_sinc_common(
        "dsd.general_from_dsd.sinc",
        from_dsd.taps,
        from_dsd.passband_hz,
        from_dsd.transition_hz,
        from_dsd.kaiser_beta,
    )?;
    Ok(())
}

fn validate_sinc_common(
    field: &'static str,
    taps: u32,
    passband_hz: f32,
    transition_hz: f32,
    kaiser_beta: f32,
) -> Result<()> {
    if taps < 1024 || taps.count_ones() != 1 {
        return Err(PlanningError::invalid_settings(
            field,
            "tap count must be a power of two and at least 1024",
        ));
    }
    validate_finite_f32(field, passband_hz)?;
    validate_finite_f32(field, transition_hz)?;
    validate_finite_f32(field, kaiser_beta)?;
    if !(0.0..=220_000.0).contains(&passband_hz) || passband_hz == 0.0 {
        return Err(PlanningError::invalid_settings(field, "passband must be greater than zero and no more than 220000 Hz"));
    }
    if !(1.0..=5_000.0).contains(&transition_hz) {
        return Err(PlanningError::invalid_settings(field, "transition must be between 1 and 5000 Hz"));
    }
    if !(0.0..=32.0).contains(&kaiser_beta) {
        return Err(PlanningError::invalid_settings(field, "kaiser beta must be between 0 and 32"));
    }
    Ok(())
}

fn validate_sample_gain_policy(field: &'static str, policy: SampleGainPolicy) -> Result<()> {
    match policy {
        SampleGainPolicy::Off => Ok(()),
        SampleGainPolicy::FixedGain { gain_db } => {
            if (DbNano::MIN_FIXED_GAIN..=DbNano::MAX_FIXED_GAIN).contains(&gain_db) {
                Ok(())
            } else {
                Err(PlanningError::invalid_settings(
                    field,
                    "fixed gain must be between -24.000000000 and +24.000000000 dB",
                ))
            }
        }
        SampleGainPolicy::TruePeakGuard { target_dbtp, .. }
        | SampleGainPolicy::TruePeakNormalize { target_dbtp, .. } => {
            if (DbNano::MIN_NORMALIZE_TARGET..=DbNano::MAX_NORMALIZE_TARGET).contains(&target_dbtp) {
                Ok(())
            } else {
                Err(PlanningError::invalid_settings(
                    field,
                    "true-peak target must be between -12.000000000 and 0.000000000 dBTP",
                ))
            }
        }
    }
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
    /// Explicitly require SSRC to own an actual PCM rate change.
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

/// Mutually exclusive ordinary sample-domain gain policy.
///
/// This is the only persisted authority for Guard/Normalize/Fixed/Off. A
/// private numerical adapter may derive `allow_boost` from the active variant,
/// but no independent boost setting exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)
)]
pub enum SampleGainPolicy {
    /// Apply no additional ordinary sample-domain gain.
    Off,
    /// Certified attenuation-only true-peak protection.
    TruePeakGuard {
        /// Requested true-peak ceiling in dBTP.
        target_dbtp: DbNano,
        /// Track-local or common-album gain authority.
        scope: crate::enums::TruePeakScope,
        /// Certified scan tier.
        scan: crate::enums::TruePeakScanTier,
    },
    /// Certified true-peak normalization that may boost or attenuate.
    TruePeakNormalize {
        /// Requested true-peak target/ceiling in dBTP.
        target_dbtp: DbNano,
        /// Track-local or common-album gain authority.
        scope: crate::enums::TruePeakScope,
        /// Certified scan tier.
        scan: crate::enums::TruePeakScanTier,
    },
    /// Apply the requested scalar without a clipping-prevention promise.
    FixedGain {
        /// User-requested scalar in dB.
        gain_db: DbNano,
    },
}

impl Default for SampleGainPolicy {
    fn default() -> Self {
        Self::Off
    }
}

impl SampleGainPolicy {
    /// Construct PCM Guard with the raw PCM defaults.
    #[must_use]
    pub const fn pcm_guard_default() -> Self {
        Self::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: crate::enums::TruePeakScope::Track,
            scan: crate::enums::TruePeakScanTier::Fast,
        }
    }

    /// Construct PCM true-peak normalization with the raw PCM defaults.
    #[must_use]
    pub const fn pcm_normalize_default() -> Self {
        Self::TruePeakNormalize {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: crate::enums::TruePeakScope::Track,
            scan: crate::enums::TruePeakScanTier::Fast,
        }
    }

    /// Construct general DSD Guard with the configured DSD defaults.
    #[must_use]
    pub const fn dsd_guard_default() -> Self {
        Self::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: crate::enums::TruePeakScope::Track,
            scan: crate::enums::TruePeakScanTier::Reference,
        }
    }

    /// Construct general DSD true-peak normalization with configured defaults.
    #[must_use]
    pub const fn dsd_normalize_default() -> Self {
        Self::TruePeakNormalize {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: crate::enums::TruePeakScope::Track,
            scan: crate::enums::TruePeakScanTier::Reference,
        }
    }

    /// True for either certified true-peak policy.
    #[must_use]
    pub const fn is_true_peak(self) -> bool {
        matches!(self, Self::TruePeakGuard { .. } | Self::TruePeakNormalize { .. })
    }

    /// True when any ordinary sample-domain gain policy is active.
    #[must_use]
    pub const fn is_active(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// True when a certified true-peak policy uses common Album authority.
    #[must_use]
    pub const fn is_album_true_peak(self) -> bool {
        matches!(
            self,
            Self::TruePeakGuard { scope: crate::enums::TruePeakScope::Album, .. }
                | Self::TruePeakNormalize { scope: crate::enums::TruePeakScope::Album, .. }
        )
    }

    /// True only when the policy may increase gain above its declared base.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn allow_boost(self) -> bool {
        matches!(self, Self::TruePeakNormalize { .. })
    }

    /// Active true-peak target.
    #[must_use]
    pub const fn target_dbtp(self) -> Option<DbNano> {
        match self {
            Self::TruePeakGuard { target_dbtp, .. }
            | Self::TruePeakNormalize { target_dbtp, .. } => Some(target_dbtp),
            Self::Off | Self::FixedGain { .. } => None,
        }
    }

    /// Active true-peak scope.
    #[must_use]
    pub const fn scope(self) -> Option<crate::enums::TruePeakScope> {
        match self {
            Self::TruePeakGuard { scope, .. } | Self::TruePeakNormalize { scope, .. } => Some(scope),
            Self::Off | Self::FixedGain { .. } => None,
        }
    }

    /// Active certified scan tier.
    #[must_use]
    pub const fn scan(self) -> Option<crate::enums::TruePeakScanTier> {
        match self {
            Self::TruePeakGuard { scan, .. } | Self::TruePeakNormalize { scan, .. } => Some(scan),
            Self::Off | Self::FixedGain { .. } => None,
        }
    }

    /// Active unguarded fixed gain.
    #[must_use]
    pub const fn fixed_gain_db(self) -> Option<DbNano> {
        match self {
            Self::FixedGain { gain_db } => Some(gain_db),
            Self::Off | Self::TruePeakGuard { .. } | Self::TruePeakNormalize { .. } => None,
        }
    }

    /// Change only scope while preserving mode, target and scan tier.
    #[must_use]
    pub const fn with_scope(self, scope: crate::enums::TruePeakScope) -> Self {
        match self {
            Self::TruePeakGuard { target_dbtp, scan, .. } => Self::TruePeakGuard { target_dbtp, scope, scan },
            Self::TruePeakNormalize { target_dbtp, scan, .. } => Self::TruePeakNormalize { target_dbtp, scope, scan },
            other => other,
        }
    }

    /// Change only scan tier while preserving mode, target and scope.
    #[must_use]
    pub const fn with_scan(self, scan: crate::enums::TruePeakScanTier) -> Self {
        match self {
            Self::TruePeakGuard { target_dbtp, scope, .. } => Self::TruePeakGuard { target_dbtp, scope, scan },
            Self::TruePeakNormalize { target_dbtp, scope, .. } => Self::TruePeakNormalize { target_dbtp, scope, scan },
            other => other,
        }
    }

    /// Change only true-peak target while preserving mode, scope and scan tier.
    #[must_use]
    pub const fn with_target(self, target_dbtp: DbNano) -> Self {
        match self {
            Self::TruePeakGuard { scope, scan, .. } => Self::TruePeakGuard { target_dbtp, scope, scan },
            Self::TruePeakNormalize { scope, scan, .. } => Self::TruePeakNormalize { target_dbtp, scope, scan },
            other => other,
        }
    }
}

/// Reconstruction authority for ordinary general DSD-to-PCM processing.
///
/// `General` starts at the renderer's native DSD reconstruction level.
/// `ReferenceProtected` reuses the qualified Reference reconstruction prefix at
/// its protected R64 level, but does not turn the request into Reference
/// delivery; the explicit export-level boundary still precedes ordinary effects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum DsdGeneralReconstruction {
    /// Ordinary native-level general reconstruction.
    #[default]
    General,
    /// Qualified Reference reconstruction prefix retained at protected R64.
    ReferenceProtected,
}

/// Explicit level exported from a protected Reference reconstruction before
/// ordinary general processing. This is distinct from the final ordinary gain
/// policy and from qualified Reference delivery gain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(tag = "level", rename_all = "snake_case", deny_unknown_fields))]
pub enum DsdGeneralExportLevel {
    /// General processing begins at native reconstructed level. This is the default.
    #[default]
    Native,
    /// Export the nominal compensated level (+18.020599913 dB from protected R64).
    NominalCompensated,
    /// Retain the protected R64 level as the ordinary processing base.
    ProtectedR64,
    /// Export native level plus an explicit user offset.
    NativeWithOffset {
        /// Additional offset after native restoration.
        offset_db: DbNano,
    },
}

/// Directional sinc parameters for general DSD-to-PCM reconstruction.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct DsdToPcmSincSettings {
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
    /// Allow aliasing for explicitly non-transparent workflows.
    pub allow_aliasing: bool,
}

impl Default for DsdToPcmSincSettings {
    fn default() -> Self {
        Self {
            taps: 262_144,
            passband_hz: 25_000.0,
            transition_hz: 500.0,
            kaiser_beta: 16.0,
            linear_phase: true,
            allow_aliasing: false,
        }
    }
}

/// Ordinary general DSD-to-PCM controls. These settings are never Reference
/// qualification evidence.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct DsdToPcmSettings {
    /// Reconstruction authority before the ordinary export-level boundary.
    pub reconstruction: DsdGeneralReconstruction,
    /// General reconstruction low-pass strategy.
    pub lowpass: DsdLowpassMethod,
    /// Directional sinc parameters, used only by `lowpass = Sinc`.
    pub sinc: DsdToPcmSincSettings,
    /// Explicit base level for ordinary processing after a protected reconstruction.
    pub export_level: DsdGeneralExportLevel,
    /// Ordinary mutually exclusive sample-domain gain policy.
    pub gain: SampleGainPolicy,
}

impl Default for DsdToPcmSettings {
    fn default() -> Self {
        Self {
            reconstruction: DsdGeneralReconstruction::General,
            lowpass: DsdLowpassMethod::Auto,
            sinc: DsdToPcmSincSettings::default(),
            export_level: DsdGeneralExportLevel::Native,
            gain: SampleGainPolicy::Off,
        }
    }
}

/// DSD-specific conversion settings split by direction and semantic authority.
///
/// There is one strict persisted representation. The former dual-origin DSD
/// distinction is intentionally gone; obsolete flat/schema-versioned forms
/// fail deserialization instead of being guessed or migrated.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct DsdSettings {
    /// PCM-to-DSD controls.
    pub pcm_to_dsd: PcmToDsdSettings,
    /// DSD-source pathway and qualified Reference controls.
    pub from_dsd: DsdSourceSettings,
    /// Ordinary general DSD-to-PCM controls.
    pub general_from_dsd: DsdToPcmSettings,
    /// Runtime-only common album gain after submitted-batch analysis.
    #[cfg_attr(feature = "serde", serde(skip))]
    runtime_album_gain_db: Option<DbNano>,
    /// Runtime-only loudest reported point that authorized the shared gain.
    #[cfg_attr(feature = "serde", serde(skip))]
    runtime_album_loudest_peak_dbfs: Option<DbNano>,
    /// Runtime-only number of measured DSD tracks in the submitted scope.
    #[cfg_attr(feature = "serde", serde(skip))]
    runtime_album_track_count: Option<usize>,
}

/// Existing PCM-to-DSD controls separated from DSD-source policy.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct PcmToDsdSettings {
    /// SoX-DSD noise-shaper family.
    pub noise_shaper: DsdNoiseShaper,
    /// Modulator order.
    pub modulator_order: ModulatorOrder,
    /// Optional trellis optimization.
    pub trellis: Option<TrellisSettings>,
    /// PCM-to-DSD filter preset.
    pub filter: DsdFilterPreset,
    /// PCM-to-DSD sinc filter parameters.
    pub sinc: PcmToDsdSincSettings,
    /// Gain compensation for PCM-to-DSD sinc upsampling.
    pub gain_compensation: GainCompensation,
}

impl Default for PcmToDsdSettings {
    fn default() -> Self {
        Self {
            noise_shaper: DsdNoiseShaper::Clans,
            modulator_order: ModulatorOrder::Order8,
            trellis: None,
            filter: DsdFilterPreset::Auto,
            sinc: PcmToDsdSincSettings::default(),
            gain_compensation: GainCompensation::Auto,
        }
    }
}

impl Default for DsdSettings {
    fn default() -> Self {
        Self {
            pcm_to_dsd: PcmToDsdSettings::default(),
            from_dsd: DsdSourceSettings::default(),
            general_from_dsd: DsdToPcmSettings::default(),
            runtime_album_gain_db: None,
            runtime_album_loudest_peak_dbfs: None,
            runtime_album_track_count: None,
        }
    }
}

impl DsdSettings {
    /// Construct settings for an explicit qualified Reference delivery request.
    ///
    /// This is a semantic convenience constructor, not a schema-version marker.
    /// The persisted DSD representation is the same strict representation used
    /// by general DSD conversions.
    #[must_use]
    pub fn reference() -> Self {
        let mut settings = Self::default();
        settings.from_dsd.pathway = DsdSourcePathway::Reference;
        settings
    }

    /// True when qualified Reference delivery is explicitly requested.
    #[must_use]
    pub const fn reference_delivery_selected(&self) -> bool {
        matches!(self.from_dsd.pathway, DsdSourcePathway::Reference)
    }

    /// Ordinary DSD gain policy.
    #[must_use]
    pub const fn gain_policy(&self) -> SampleGainPolicy {
        self.general_from_dsd.gain
    }

    /// Replace the ordinary DSD gain policy and invalidate stale runtime authority.
    pub fn set_gain_policy(&mut self, policy: SampleGainPolicy) {
        self.general_from_dsd.gain = policy;
        self.clear_runtime_album_gain();
    }

    /// Active true-peak scope for ordinary general DSD processing.
    #[must_use]
    pub const fn true_peak_scope(&self) -> Option<crate::enums::TruePeakScope> {
        self.general_from_dsd.gain.scope()
    }

    /// Active certified scan tier for ordinary general DSD processing.
    #[must_use]
    pub const fn true_peak_scan_tier(&self) -> Option<crate::enums::TruePeakScanTier> {
        self.general_from_dsd.gain.scan()
    }

    /// Change DSD true-peak scope without changing target or tier.
    pub fn set_true_peak_scope(&mut self, scope: crate::enums::TruePeakScope) {
        self.general_from_dsd.gain = self.general_from_dsd.gain.with_scope(scope);
        self.clear_runtime_album_gain();
    }

    /// Change DSD true-peak scan tier without changing target or scope.
    pub fn set_true_peak_scan_tier(&mut self, scan: crate::enums::TruePeakScanTier) {
        self.general_from_dsd.gain = self.general_from_dsd.gain.with_scan(scan);
        self.clear_runtime_album_gain();
    }

    /// True when an album-scoped certified ordinary DSD gain is active.
    #[must_use]
    pub const fn album_true_peak_gain_selected(&self) -> bool {
        matches!(
            self.general_from_dsd.gain,
            SampleGainPolicy::TruePeakGuard { scope: crate::enums::TruePeakScope::Album, .. }
                | SampleGainPolicy::TruePeakNormalize { scope: crate::enums::TruePeakScope::Album, .. }
        )
    }

    /// Active album true-peak target.
    #[must_use]
    pub const fn album_true_peak_target_dbtp(&self) -> Option<DbNano> {
        if self.album_true_peak_gain_selected() {
            self.general_from_dsd.gain.target_dbtp()
        } else {
            None
        }
    }

    /// Bind the complete runtime authority derived from one submitted batch.
    pub fn bind_runtime_album_gain(
        &mut self,
        gain: DbNano,
        loudest_peak_dbfs: Option<DbNano>,
        track_count: usize,
    ) {
        self.runtime_album_gain_db = Some(gain);
        self.runtime_album_loudest_peak_dbfs = loudest_peak_dbfs;
        self.runtime_album_track_count = Some(track_count);
    }

    /// Narrow test/bridge setter for the runtime common scalar.
    pub fn set_runtime_album_gain_db(&mut self, gain: Option<DbNano>) {
        self.runtime_album_gain_db = gain;
        self.runtime_album_loudest_peak_dbfs = None;
        self.runtime_album_track_count = None;
    }

    /// Clear every runtime-only album-gain authority field.
    pub fn clear_runtime_album_gain(&mut self) {
        self.runtime_album_gain_db = None;
        self.runtime_album_loudest_peak_dbfs = None;
        self.runtime_album_track_count = None;
    }

    /// Runtime fixed common album gain, if bound.
    #[must_use]
    pub const fn runtime_album_gain_db(&self) -> Option<DbNano> {
        self.runtime_album_gain_db
    }

    /// Loudest reported point in the submitted scope.
    #[must_use]
    pub const fn runtime_album_loudest_peak_dbfs(&self) -> Option<DbNano> {
        self.runtime_album_loudest_peak_dbfs
    }

    /// Number of measured DSD tracks represented by the runtime authority.
    #[must_use]
    pub const fn runtime_album_track_count(&self) -> Option<usize> {
        self.runtime_album_track_count
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

/// FIR sinc parameters for PCM-to-DSD conversion.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PcmToDsdSincSettings {
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

impl Default for PcmToDsdSincSettings {
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

/// Compatibility alias for source code that has not yet adopted the directional name.
pub type SincFilterSettings = PcmToDsdSincSettings;

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
    // Settings validation is intentionally structural. Destination-rate
    // applicability is a terminal-realization question because a derived
    // global family may legitimately split to a later terminal, and native
    // overrides are inactive on floating/non-SSRC outputs.
    if let Some(dither_id) = settings.ssrc.dither_id {
        if dither_id > 99 {
            return Err(PlanningError::invalid_settings(
                "ssrc.dither_id",
                "expected 0 through 99",
            ));
        }
    }
    Ok(())
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

/// Default true-peak target for PCM automatic gain: 0.1 dB of user headroom
/// below the certified upper bound.
pub const PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP: DbNano = DbNano(-100_000_000);
/// Highest requested ceiling honored for lossy encoder-input PCM.
///
/// Lossy decode can overshoot its encoder input, so this is deliberately a
/// policy cap on the governed encoder-input waveform, not a decoded-codec
/// peak guarantee.
pub const PCM_TRUE_PEAK_LOSSY_MAX_TARGET_DBTP: DbNano = PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP;

/// Return whether PCM true-peak policy must treat this target as lossy.
///
/// WavPack is ordinarily lossless at the format level, but its explicit
/// hybrid mode produces a lossy base `.wv` stream and therefore uses the
/// same encoder-input ceiling policy as the built-in lossy codecs.
#[must_use]
pub fn pcm_true_peak_lossy_floor_applies(
    format: &AudioFormat,
    wavpack_hybrid: bool,
) -> bool {
    format.is_lossy() || (format == &AudioFormat::WavPack && wavpack_hybrid)
}

/// Effective certified true-peak target after the explicit lossy encoder-input cap.
///
/// Guard and True-peak normalize use the same physical target policy for ordinary
/// PCM and general DSD-to-PCM. This helper keeps that policy single-sourced; it
/// does not change the semantic/requested dBTP target recorded by the planner.
#[must_use]
pub fn effective_true_peak_target(
    policy: SampleGainPolicy,
    format: &AudioFormat,
    wavpack_hybrid: bool,
) -> Option<(DbNano, bool)> {
    let target_dbtp = policy.target_dbtp()?;
    if pcm_true_peak_lossy_floor_applies(format, wavpack_hybrid)
        && target_dbtp > PCM_TRUE_PEAK_LOSSY_MAX_TARGET_DBTP
    {
        Some((PCM_TRUE_PEAK_LOSSY_MAX_TARGET_DBTP, true))
    } else {
        Some((target_dbtp, false))
    }
}

/// Ordinary sample-domain gain settings for PCM conversions.
///
/// The active policy is a tagged enum, so Off/Guard/Normalize/Fixed cannot be
/// active simultaneously. Runtime album authority is deliberately separate and
/// never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct PcmTruePeakGainSettings {
    /// Mutually exclusive ordinary sample-domain gain policy.
    pub policy: SampleGainPolicy,
    /// Runtime-only submitted-batch authority. Never persisted in presets or config.
    #[cfg_attr(feature = "serde", serde(skip))]
    runtime_album_gain_db: Option<DbNano>,
}

impl Default for PcmTruePeakGainSettings {
    fn default() -> Self {
        Self {
            policy: SampleGainPolicy::Off,
            runtime_album_gain_db: None,
        }
    }
}

impl PcmTruePeakGainSettings {
    /// True for Guard or true-peak normalize.
    #[must_use]
    pub const fn is_true_peak(self) -> bool {
        self.policy.is_true_peak()
    }

    /// True when Off is not selected.
    #[must_use]
    pub const fn is_active(self) -> bool {
        self.policy.is_active()
    }

    /// True when a certified true-peak policy uses common Album authority.
    #[must_use]
    pub const fn album_true_peak_gain_selected(self) -> bool {
        self.policy.is_album_true_peak()
    }

    /// Active true-peak target.
    #[must_use]
    pub const fn target_dbtp(self) -> Option<DbNano> {
        self.policy.target_dbtp()
    }

    /// Active true-peak scope.
    #[must_use]
    pub const fn scope(self) -> Option<crate::enums::TruePeakScope> {
        self.policy.scope()
    }

    /// Active certified scan tier.
    #[must_use]
    pub const fn scan_tier(self) -> Option<crate::enums::TruePeakScanTier> {
        self.policy.scan()
    }

    /// Active unguarded fixed gain.
    #[must_use]
    pub const fn fixed_gain_db(self) -> Option<DbNano> {
        self.policy.fixed_gain_db()
    }

    /// Replace the policy and clear stale submitted-batch authority.
    pub fn set_policy(&mut self, policy: SampleGainPolicy) {
        self.policy = policy;
        self.clear_runtime_album_gain();
    }

    /// Change only scope while retaining policy mode, target, and scan tier.
    pub fn set_scope(&mut self, scope: crate::enums::TruePeakScope) {
        self.policy = self.policy.with_scope(scope);
        self.clear_runtime_album_gain();
    }

    /// Change only scan tier while retaining policy mode, target, and scope.
    pub fn set_scan_tier(&mut self, scan: crate::enums::TruePeakScanTier) {
        self.policy = self.policy.with_scan(scan);
        self.clear_runtime_album_gain();
    }

    /// Effective ceiling after the explicit lossy encoder-input cap.
    #[must_use]
    pub fn effective_target(
        self,
        format: &AudioFormat,
        wavpack_hybrid: bool,
    ) -> Option<(DbNano, bool)> {
        effective_true_peak_target(self.policy, format, wavpack_hybrid)
    }

    /// Narrow runtime-only setter used by planner/fingerprint tests and transition bridges.
    /// Persisted policy remains the tagged `SampleGainPolicy`; changing this value does
    /// not create another user authority.
    pub fn set_runtime_album_gain_db(&mut self, gain_db: Option<DbNano>) {
        self.runtime_album_gain_db = gain_db;
    }

    /// Bind the one gain derived from the complete submitted batch.
    pub fn bind_runtime_album_gain(&mut self, gain_db: DbNano) {
        self.runtime_album_gain_db = Some(gain_db);
    }

    /// Return the runtime-only album gain, if the submitted-batch barrier has resolved it.
    #[must_use]
    pub const fn runtime_album_gain_db(self) -> Option<DbNano> {
        self.runtime_album_gain_db
    }

    /// Clear any stale runtime authority before a request is persisted/reused.
    pub fn clear_runtime_album_gain(&mut self) {
        self.runtime_album_gain_db = None;
    }
}

fn validate_pcm_true_peak_settings(
    settings: &PcmTruePeakGainSettings,
    target_format: &AudioFormat,
) -> Result<()> {
    validate_sample_gain_policy("pcm_true_peak.policy", settings.policy)?;
    if !matches!(settings.policy, SampleGainPolicy::Off) && target_format.is_dsd() {
        return Err(PlanningError::invalid_settings(
            "pcm_true_peak.policy",
            "ordinary PCM gain policy cannot be applied to a DSD target",
        ));
    }
    if settings.is_true_peak() && !target_format.is_lossy() && !target_format.is_pcm_lossless() {
        return Err(PlanningError::invalid_settings(
            "pcm_true_peak.policy",
            format!("PCM true-peak hard ceiling is not defined for {target_format} output"),
        ));
    }
    if settings.runtime_album_gain_db().is_some()
        && settings.scope() != Some(crate::enums::TruePeakScope::Album)
    {
        return Err(PlanningError::invalid_settings(
            "pcm_true_peak.runtime_album_gain_db",
            "runtime album gain requires an active album-scoped certified true-peak policy",
        ));
    }
    Ok(())
}

/// ReplayGain post-processing settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ReplayGainSettings {
    /// ReplayGain mode owned by Tonepoet's native common-plan executor.
    pub mode: Option<ReplayGainMode>,
    /// Avoid clipping where the scanner supports it.
    pub prevent_clipping: bool,
    /// Whether complete existing ReplayGain tag sets may suppress rescanning.
    #[cfg_attr(feature = "serde", serde(default))]
    pub existing_tags: ReplayGainExistingTagPolicy,
}

impl ReplayGainSettings {
    /// Logical ReplayGain request owned by the native common-plan executor.
    #[must_use]
    pub const fn logical_mode(self) -> Option<ReplayGainMode> {
        self.mode
    }
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
    fn defers_explicit_ssrc_dither_rate_validation_to_active_terminal_resolution() {
        let mut settings = PipelineSettings::default();
        settings.preferred_tool = PreferredTool::Ssrc;
        settings.nyquist_transition = NyquistTransition::BrickWall;
        settings.target_sample_rate = RateTarget::PcmHz(96_000);
        settings.ssrc.dither_id = Some(16);
        assert!(settings.validate().is_ok());

        settings.ssrc.dither_id = Some(2);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn defers_low_rate_native_id_validation_until_ssrc_terminal_is_active() {
        let mut settings = PipelineSettings::default();
        settings.preferred_tool = PreferredTool::Ssrc;
        settings.nyquist_transition = NyquistTransition::BrickWall;
        settings.target_sample_rate = RateTarget::PcmHz(22_050);
        settings.ssrc.dither_id = Some(6);
        assert!(settings.validate().is_ok());

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
    fn derived_global_ssrc_mapping_unavailability_is_not_a_settings_error() {
        let mut settings = PipelineSettings::default();
        settings.preferred_tool = PreferredTool::Ssrc;
        settings.target_sample_rate = RateTarget::PcmHz(176_400);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
        settings.dither_type = DitherType::HighShibata;
        assert!(settings.validate().is_ok());

        settings.dither_type = DitherType::Tpdf;
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn lossy_fallback_validates_ssrc_dither_against_effective_encoder_rate() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Aac;
        settings.preferred_tool = PreferredTool::Ssrc;
        settings.nyquist_transition = NyquistTransition::BrickWall;
        settings.target_sample_rate = RateTarget::PcmHz(176_400);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
        settings.dither_type = DitherType::HighShibata;

        settings
            .validate()
            .expect("AAC 176.4 kHz resolves to 96 kHz, where the derived SSRC mapping is valid");
    }

    #[test]
    fn hard_ceiling_invalid_lossy_rate_defers_to_the_rate_admission_error() {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Aac;
        settings.preferred_tool = PreferredTool::Ssrc;
        settings.nyquist_transition = NyquistTransition::BrickWall;
        settings.target_sample_rate = RateTarget::PcmHz(176_400);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
        settings.dither_type = DitherType::HighShibata;
        settings.dsd.set_gain_policy(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: crate::enums::TruePeakScope::Album,
            scan: crate::enums::TruePeakScanTier::Fast,
        });

        settings
            .validate()
            .expect("unsupported hard-ceiling lossy rate belongs to request planning, not SSRC dither validation");
    }

    #[test]
    fn pdf_only_override_rate_resolution_is_deferred_to_active_terminal() {
        let mut settings = PipelineSettings::default();
        settings.preferred_tool = PreferredTool::Ssrc;
        settings.target_sample_rate = RateTarget::PcmHz(176_400);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
        settings.dither_type = DitherType::Shibata;
        settings.ssrc.pdf_type = Some(SsrcPdfType::Triangular);
        assert!(settings.validate().is_ok());
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
    fn skips_ssrc_dither_mapping_validation_for_int32_output() {
        let mut settings = PipelineSettings::default();
        // Int32 PCM is valid for WAV; keep this pin focused on SSRC dither
        // validation rather than container depth support.
        settings.target_format = AudioFormat::Wav;
        settings.preferred_tool = PreferredTool::Ssrc;
        settings.target_sample_rate = RateTarget::PcmHz(176_400);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        settings.dither_type = DitherType::HighShibata;
        settings.dither_explicit = true;
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
    fn brickwall_auto_derived_mapping_unavailability_is_not_a_settings_error() {
        let mut settings = PipelineSettings::default();
        settings.preferred_tool = PreferredTool::Auto;
        settings.nyquist_transition = NyquistTransition::BrickWall;
        settings.target_sample_rate = RateTarget::PcmHz(176_400);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
        settings.dither_type = DitherType::HighShibata;
        assert!(settings.validate().is_ok());
    }
}

#[cfg(test)]
mod phase2_gain_policy_tests {
    use super::*;
    use crate::enums::{TruePeakScanTier, TruePeakScope};

    fn guard(scope: TruePeakScope, scan: TruePeakScanTier) -> SampleGainPolicy {
        SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope,
            scan,
        }
    }

    #[test]
    fn raw_pcm_and_dsd_defaults_are_distinct_and_disabled() {
        let settings = PipelineSettings::default();
        assert_eq!(settings.pcm_true_peak.policy, SampleGainPolicy::Off);
        assert_eq!(settings.dsd.general_from_dsd.gain, SampleGainPolicy::Off);
        assert_eq!(SampleGainPolicy::pcm_guard_default().scan(), Some(TruePeakScanTier::Fast));
        assert_eq!(SampleGainPolicy::dsd_guard_default().scan(), Some(TruePeakScanTier::Reference));
    }

    #[test]
    fn guard_and_normalize_derive_boost_authority_from_the_variant() {
        let guard = SampleGainPolicy::TruePeakGuard {
            target_dbtp: "-0.100000000".parse().unwrap(),
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Fast,
        };
        let normalize = SampleGainPolicy::TruePeakNormalize {
            target_dbtp: "-0.100000000".parse().unwrap(),
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Fast,
        };
        assert!(!guard.allow_boost());
        assert!(normalize.allow_boost());
    }

    #[test]
    fn pcm_and_general_dsd_share_track_album_and_all_scan_tiers() {
        for scope in [TruePeakScope::Track, TruePeakScope::Album] {
            for scan in [TruePeakScanTier::Fast, TruePeakScanTier::Standard, TruePeakScanTier::Reference] {
                let mut pcm = PcmTruePeakGainSettings::default();
                pcm.set_policy(guard(scope, scan));
                assert_eq!(pcm.scope(), Some(scope));
                assert_eq!(pcm.scan_tier(), Some(scan));

                let mut dsd = DsdSettings::default();
                dsd.set_gain_policy(guard(scope, scan));
                assert_eq!(dsd.true_peak_scope(), Some(scope));
                assert_eq!(dsd.true_peak_scan_tier(), Some(scan));
            }
        }
    }

    #[test]
    fn fixed_gain_is_not_a_certified_true_peak_policy() {
        let policy = SampleGainPolicy::FixedGain {
            gain_db: "3.250000000".parse().unwrap(),
        };
        assert!(!policy.is_true_peak());
        assert!(!policy.allow_boost());
        assert_eq!(policy.fixed_gain_db(), Some("3.250000000".parse().unwrap()));
        assert_eq!(policy.target_dbtp(), None);
    }

    #[test]
    fn target_and_fixed_gain_ranges_are_enforced() {
        for raw in ["-12.000000001", "0.000000001"] {
            let policy = SampleGainPolicy::TruePeakGuard {
                target_dbtp: raw.parse().unwrap(),
                scope: TruePeakScope::Track,
                scan: TruePeakScanTier::Fast,
            };
            assert!(validate_sample_gain_policy("gain", policy).is_err());
        }
        for raw in ["-24.000000001", "24.000000001"] {
            let policy = SampleGainPolicy::FixedGain { gain_db: raw.parse().unwrap() };
            assert!(validate_sample_gain_policy("gain", policy).is_err());
        }
    }

    #[test]
    fn album_runtime_authority_requires_album_certified_policy() {
        let mut pcm = PipelineSettings::default();
        pcm.pcm_true_peak.bind_runtime_album_gain("-1.000000000".parse().unwrap());
        assert!(pcm.validate().is_err());
        pcm.pcm_true_peak.set_policy(guard(TruePeakScope::Album, TruePeakScanTier::Fast));
        pcm.pcm_true_peak.bind_runtime_album_gain("-1.000000000".parse().unwrap());
        assert!(pcm.validate().is_ok());

        let mut dsd = PipelineSettings::default();
        dsd.dsd.bind_runtime_album_gain("-1.000000000".parse().unwrap(), None, 2);
        assert!(dsd.validate().is_err());
        dsd.dsd.set_gain_policy(guard(TruePeakScope::Album, TruePeakScanTier::Standard));
        dsd.dsd.bind_runtime_album_gain("-1.000000000".parse().unwrap(), None, 2);
        assert!(dsd.validate().is_ok());
    }

    #[test]
    fn lossy_floor_applies_only_to_certified_pcm_target() {
        let mut settings = PcmTruePeakGainSettings::default();
        settings.set_policy(SampleGainPolicy::TruePeakNormalize {
            target_dbtp: DbNano::ZERO,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Standard,
        });
        assert_eq!(settings.effective_target(&AudioFormat::Flac, false), Some((DbNano::ZERO, false)));
        assert_eq!(settings.effective_target(&AudioFormat::Opus, false), Some((PCM_TRUE_PEAK_LOSSY_MAX_TARGET_DBTP, true)));
    }
}

#[cfg(all(test, feature = "serde"))]
mod phase2_gain_policy_serde_tests {
    use super::*;
    use crate::enums::{TruePeakScanTier, TruePeakScope};

    #[test]
    fn typed_policy_round_trips_without_runtime_album_authority() {
        let mut settings = PipelineSettings::default();
        settings.pcm_true_peak.set_policy(SampleGainPolicy::TruePeakNormalize {
            target_dbtp: "-0.375000000".parse().unwrap(),
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Reference,
        });
        settings.pcm_true_peak.bind_runtime_album_gain("-1.125000000".parse().unwrap());
        settings.dsd.set_gain_policy(SampleGainPolicy::TruePeakGuard {
            target_dbtp: "-0.200000000".parse().unwrap(),
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Standard,
        });
        settings.dsd.bind_runtime_album_gain("-0.500000000".parse().unwrap(), None, 8);

        let value = serde_json::to_value(&settings).unwrap();
        assert_eq!(value["pcm_true_peak"]["policy"]["mode"], "true_peak_normalize");
        assert_eq!(value["dsd"]["general_from_dsd"]["gain"]["mode"], "true_peak_guard");
        assert!(value["pcm_true_peak"].get("runtime_album_gain_db").is_none());
        assert!(value["dsd"].get("runtime_album_gain_db").is_none());

        let decoded: PipelineSettings = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.pcm_true_peak.runtime_album_gain_db(), None);
        assert_eq!(decoded.dsd.runtime_album_gain_db(), None);
        assert_eq!(decoded.pcm_true_peak.scope(), Some(TruePeakScope::Album));
        assert_eq!(decoded.dsd.true_peak_scan_tier(), Some(TruePeakScanTier::Standard));
    }

    #[test]
    fn obsolete_ambiguous_gain_forms_are_rejected_not_guessed() {
        let mut value = serde_json::to_value(PipelineSettings::default()).unwrap();
        value["pcm_true_peak"] = serde_json::json!({
            "enabled": true,
            "target_dbtp": "-0.100000000",
            "allow_boost": true,
            "scope": "track",
            "scan_mode": "fast066v2_fast"
        });
        assert!(serde_json::from_value::<PipelineSettings>(value).is_err());

        let mut dsd = serde_json::to_value(PipelineSettings::default()).unwrap();
        dsd["dsd"]["gain_mode"] = serde_json::Value::String("auto".to_owned());
        assert!(serde_json::from_value::<PipelineSettings>(dsd).is_err());
    }

    #[test]
    fn replaygain_has_one_persisted_native_owner() {
        let mut settings = PipelineSettings::default();
        settings.replay_gain.mode = Some(ReplayGainMode::Both);
        assert_eq!(settings.replay_gain.logical_mode(), Some(ReplayGainMode::Both));
        assert!(settings.validate().is_ok());
        let encoded = serde_json::to_value(&settings).unwrap();
        assert_eq!(encoded["replay_gain"]["mode"], "both");
        let decoded: PipelineSettings = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.replay_gain.logical_mode(), Some(ReplayGainMode::Both));
    }
}
