//! Stable fingerprints for conversion settings.
//!
//! The fingerprint covers every setting that can alter conversion output. It
//! uses explicit field names and enum encodings so the digest stays independent
//! of Rust struct layout, declaration order, serde output, or debug formatting.

use sha2::{Digest, Sha256};

use crate::enums::{
    AacProfile, AudioFormat, BitDepthTarget, DitherType, DsdFilterPreset, DsdLowpassMethod,
    DsdNoiseShaper, GainCompensation, ModulatorOrder, Mp3Mode, NyquistTransition, OpusContentType,
    PcmBitDepth, PreferredTool, RateTarget, ReplayGainMode, ResampleQuality, SsrcProfile,
    WavPackMode,
};
use crate::settings::{
    AacSettings, DsdSettings, FlacSettings, MetadataSettings, Mp3Settings, OpusSettings,
    PipelineSettings, ReplayGainSettings, SincFilterSettings, SsrcSettings, TrellisSettings,
    VerificationSettings, WavPackSettings,
};

/// Deterministic SHA-256 digest for [`PipelineSettings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SettingsFingerprint([u8; 32]);

impl SettingsFingerprint {
    /// Returns the raw 32-byte SHA-256 digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns the digest as lowercase hexadecimal text.
    #[must_use]
    pub fn to_hex(self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            push_hex_byte(&mut out, byte);
        }
        out
    }
}

impl std::fmt::Display for SettingsFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}


/// Canonical field-path inventory covered by [`settings_fingerprint`].
///
/// The list is public so integration tests can compare handoff, legacy, and
/// mutation coverage against the same conversion-affecting field set.
pub const SETTINGS_FINGERPRINT_FIELD_PATHS: &[&str] = &[
    "target_format",
    "target_sample_rate",
    "target_bit_depth",
    "resample_quality",
    "nyquist_transition",
    "dither_type",
    "preferred_tool",
    "force_encode",
    "flac.compression_level",
    "flac.verify",
    "flac.write_md5",
    "mp3.mode",
    "mp3.bitrate_kbps",
    "mp3.vbr_quality",
    "aac.profile",
    "aac.bitrate_kbps",
    "opus.content_type",
    "opus.bitrate_kbps",
    "opus.complexity",
    "wavpack.mode",
    "wavpack.hybrid",
    "wavpack.hybrid_bitrate_kbps",
    "wavpack.correction_file",
    "ssrc.force",
    "ssrc.two_pass",
    "ssrc.insane_mode",
    "ssrc.profile",
    "dsd.noise_shaper",
    "dsd.modulator_order",
    "dsd.trellis",
    "dsd.trellis.lookahead",
    "dsd.trellis.nodes",
    "dsd.trellis.latency",
    "dsd.pcm_to_dsd_filter",
    "dsd.dsd_to_pcm_lowpass",
    "dsd.dsd_to_pcm_gain_db",
    "dsd.sinc.oversample_factor",
    "dsd.sinc.taps",
    "dsd.sinc.passband_hz",
    "dsd.sinc.transition_hz",
    "dsd.sinc.kaiser_beta",
    "dsd.sinc.linear_phase",
    "dsd.sinc.allow_aliasing",
    "dsd.gain_compensation",
    "metadata.transfer_tags",
    "metadata.preserve_artwork",
    "metadata.store_source_audio_md5",
    "verification.verify_after_encode",
    "verification.prefer_native_flac_verify",
    "replay_gain.mode",
    "replay_gain.prevent_clipping",
];

/// Number of conversion-affecting field paths in [`SETTINGS_FINGERPRINT_FIELD_PATHS`].
pub const SETTINGS_FINGERPRINT_FIELD_COUNT: usize = SETTINGS_FINGERPRINT_FIELD_PATHS.len();

/// Returns a deterministic content fingerprint for all conversion-affecting
/// fields in [`PipelineSettings`].
#[must_use]
pub fn settings_fingerprint(settings: &PipelineSettings) -> SettingsFingerprint {
    let mut writer = FingerprintWriter::new();
    writer.field_static("schema", "tonepoet-pipeline-settings-fingerprint/v1");
    push_pipeline_settings(&mut writer, settings);
    SettingsFingerprint(writer.finish())
}

struct FingerprintWriter {
    hasher: Sha256,
}

impl FingerprintWriter {
    fn new() -> Self {
        Self {
            hasher: Sha256::new(),
        }
    }

    fn field_static(&mut self, path: &str, value: &str) {
        self.hasher.update(path.as_bytes());
        self.hasher.update(b"=");
        self.hasher.update(value.len().to_string().as_bytes());
        self.hasher.update(b":");
        self.hasher.update(value.as_bytes());
        self.hasher.update(b"\n");
    }

    fn field_string(&mut self, path: &str, value: String) {
        self.field_static(path, &value);
    }

    fn finish(self) -> [u8; 32] {
        self.hasher.finalize().into()
    }
}

fn push_hex_byte(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(HEX[(byte >> 4) as usize] as char);
    out.push(HEX[(byte & 0x0f) as usize] as char);
}

fn push_pipeline_settings(writer: &mut FingerprintWriter, settings: &PipelineSettings) {
    writer.field_string("target_format", audio_format(&settings.target_format));
    writer.field_string("target_sample_rate", rate_target(settings.target_sample_rate));
    writer.field_string("target_bit_depth", bit_depth_target(settings.target_bit_depth));
    writer.field_static("resample_quality", resample_quality(settings.resample_quality));
    writer.field_static(
        "nyquist_transition",
        nyquist_transition(settings.nyquist_transition),
    );
    writer.field_static("dither_type", dither_type(settings.dither_type));
    writer.field_string("preferred_tool", preferred_tool(&settings.preferred_tool));
    writer.field_static("force_encode", bool_value(settings.force_encode));
    push_flac(writer, &settings.flac);
    push_mp3(writer, &settings.mp3);
    push_aac(writer, &settings.aac);
    push_opus(writer, &settings.opus);
    push_wavpack(writer, &settings.wavpack);
    push_ssrc(writer, &settings.ssrc);
    push_dsd(writer, &settings.dsd);
    push_metadata(writer, &settings.metadata);
    push_verification(writer, &settings.verification);
    push_replay_gain(writer, &settings.replay_gain);
}

fn push_flac(writer: &mut FingerprintWriter, settings: &FlacSettings) {
    writer.field_string("flac.compression_level", settings.compression_level.to_string());
    writer.field_static("flac.verify", bool_value(settings.verify));
    writer.field_static("flac.write_md5", bool_value(settings.write_md5));
}

fn push_mp3(writer: &mut FingerprintWriter, settings: &Mp3Settings) {
    writer.field_static("mp3.mode", mp3_mode(settings.mode));
    writer.field_string("mp3.bitrate_kbps", settings.bitrate_kbps.to_string());
    writer.field_string("mp3.vbr_quality", settings.vbr_quality.to_string());
}

fn push_aac(writer: &mut FingerprintWriter, settings: &AacSettings) {
    writer.field_static("aac.profile", aac_profile(settings.profile));
    writer.field_string("aac.bitrate_kbps", settings.bitrate_kbps.to_string());
}

fn push_opus(writer: &mut FingerprintWriter, settings: &OpusSettings) {
    writer.field_static("opus.content_type", opus_content_type(settings.content_type));
    writer.field_string("opus.bitrate_kbps", settings.bitrate_kbps.to_string());
    writer.field_string("opus.complexity", settings.complexity.to_string());
}

fn push_wavpack(writer: &mut FingerprintWriter, settings: &WavPackSettings) {
    writer.field_static("wavpack.mode", wavpack_mode(settings.mode));
    writer.field_static("wavpack.hybrid", bool_value(settings.hybrid));
    writer.field_string(
        "wavpack.hybrid_bitrate_kbps",
        settings.hybrid_bitrate_kbps.to_string(),
    );
    writer.field_static("wavpack.correction_file", bool_value(settings.correction_file));
}

fn push_ssrc(writer: &mut FingerprintWriter, settings: &SsrcSettings) {
    writer.field_static("ssrc.force", bool_value(settings.force));
    writer.field_static("ssrc.two_pass", bool_value(settings.two_pass));
    writer.field_static("ssrc.insane_mode", bool_value(settings.insane_mode));
    writer.field_string("ssrc.profile", option_static(settings.profile.map(ssrc_profile)));
}

fn push_dsd(writer: &mut FingerprintWriter, settings: &DsdSettings) {
    writer.field_static("dsd.noise_shaper", dsd_noise_shaper(settings.noise_shaper));
    writer.field_static("dsd.modulator_order", modulator_order(settings.modulator_order));
    writer.field_string("dsd.trellis", option_trellis(settings.trellis));
    if let Some(trellis) = settings.trellis {
        writer.field_string("dsd.trellis.lookahead", trellis.lookahead.to_string());
        writer.field_string("dsd.trellis.nodes", trellis.nodes.to_string());
        writer.field_string("dsd.trellis.latency", option_u16(trellis.latency));
    } else {
        writer.field_static("dsd.trellis.lookahead", "None");
        writer.field_static("dsd.trellis.nodes", "None");
        writer.field_static("dsd.trellis.latency", "None");
    }
    writer.field_static("dsd.pcm_to_dsd_filter", dsd_filter_preset(settings.pcm_to_dsd_filter));
    writer.field_static(
        "dsd.dsd_to_pcm_lowpass",
        dsd_lowpass_method(settings.dsd_to_pcm_lowpass),
    );
    writer.field_string(
        "dsd.dsd_to_pcm_gain_db",
        option_f32(settings.dsd_to_pcm_gain_db),
    );
    push_sinc(writer, &settings.sinc);
    writer.field_string(
        "dsd.gain_compensation",
        gain_compensation(settings.gain_compensation),
    );
}

fn push_sinc(writer: &mut FingerprintWriter, settings: &SincFilterSettings) {
    writer.field_string(
        "dsd.sinc.oversample_factor",
        settings.oversample_factor.to_string(),
    );
    writer.field_string("dsd.sinc.taps", settings.taps.to_string());
    writer.field_string("dsd.sinc.passband_hz", f32_value(settings.passband_hz));
    writer.field_string("dsd.sinc.transition_hz", f32_value(settings.transition_hz));
    writer.field_string("dsd.sinc.kaiser_beta", f32_value(settings.kaiser_beta));
    writer.field_static("dsd.sinc.linear_phase", bool_value(settings.linear_phase));
    writer.field_static("dsd.sinc.allow_aliasing", bool_value(settings.allow_aliasing));
}

fn push_metadata(writer: &mut FingerprintWriter, settings: &MetadataSettings) {
    writer.field_static("metadata.transfer_tags", bool_value(settings.transfer_tags));
    writer.field_static("metadata.preserve_artwork", bool_value(settings.preserve_artwork));
    writer.field_static(
        "metadata.store_source_audio_md5",
        bool_value(settings.store_source_audio_md5),
    );
}

fn push_verification(writer: &mut FingerprintWriter, settings: &VerificationSettings) {
    writer.field_static(
        "verification.verify_after_encode",
        bool_value(settings.verify_after_encode),
    );
    writer.field_static(
        "verification.prefer_native_flac_verify",
        bool_value(settings.prefer_native_flac_verify),
    );
}

fn push_replay_gain(writer: &mut FingerprintWriter, settings: &ReplayGainSettings) {
    writer.field_string(
        "replay_gain.mode",
        option_static(settings.mode.map(replay_gain_mode)),
    );
    writer.field_static(
        "replay_gain.prevent_clipping",
        bool_value(settings.prevent_clipping),
    );
}

fn bool_value(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn audio_format(value: &AudioFormat) -> String {
    match value {
        AudioFormat::Flac => "Flac".to_string(),
        AudioFormat::Wav => "Wav".to_string(),
        AudioFormat::Aiff => "Aiff".to_string(),
        AudioFormat::WavPack => "WavPack".to_string(),
        AudioFormat::Mp3 => "Mp3".to_string(),
        AudioFormat::Aac => "Aac".to_string(),
        AudioFormat::Opus => "Opus".to_string(),
        AudioFormat::Alac => "Alac".to_string(),
        AudioFormat::Dsf => "Dsf".to_string(),
        AudioFormat::Dff => "Dff".to_string(),
        AudioFormat::Dts => "Dts".to_string(),
        AudioFormat::Ac3 => "Ac3".to_string(),
        AudioFormat::Custom {
            extension,
            display_name,
        } => format!(
            "Custom(extension={},display_name={})",
            string_value(extension),
            string_value(display_name)
        ),
    }
}

fn string_value(value: &str) -> String {
    format!("{}:{value}", value.len())
}

fn rate_target(value: RateTarget) -> String {
    match value {
        RateTarget::Source => "Source".to_string(),
        RateTarget::PcmHz(hz) => format!("PcmHz({hz})"),
        RateTarget::Dsd(rate) => format!("Dsd({})", dsd_rate(rate)),
    }
}

fn bit_depth_target(value: BitDepthTarget) -> String {
    match value {
        BitDepthTarget::Source => "Source".to_string(),
        BitDepthTarget::Pcm(depth) => format!("Pcm({})", pcm_bit_depth(depth)),
    }
}

fn preferred_tool(value: &PreferredTool) -> String {
    match value {
        PreferredTool::Auto => "Auto".to_string(),
        PreferredTool::Ffmpeg => "Ffmpeg".to_string(),
        PreferredTool::Sox => "Sox".to_string(),
        PreferredTool::Ssrc => "Ssrc".to_string(),
        PreferredTool::Custom(name) => format!("Custom({})", string_value(name)),
    }
}

fn option_static(value: Option<&'static str>) -> String {
    match value {
        Some(value) => format!("Some({value})"),
        None => "None".to_string(),
    }
}

fn option_u16(value: Option<u16>) -> String {
    match value {
        Some(value) => format!("Some({value})"),
        None => "None".to_string(),
    }
}

fn option_f32(value: Option<f32>) -> String {
    match value {
        Some(value) => format!("Some({})", f32_value(value)),
        None => "None".to_string(),
    }
}

fn option_trellis(value: Option<TrellisSettings>) -> String {
    match value {
        Some(_) => "Some".to_string(),
        None => "None".to_string(),
    }
}

fn f32_value(value: f32) -> String {
    format!("f32bits:{:08x}", value.to_bits())
}

fn resample_quality(value: ResampleQuality) -> &'static str {
    match value {
        ResampleQuality::Low => "Low",
        ResampleQuality::Medium => "Medium",
        ResampleQuality::High => "High",
        ResampleQuality::VeryHigh => "VeryHigh",
        ResampleQuality::Ultra => "Ultra",
        ResampleQuality::Insane => "Insane",
    }
}

fn nyquist_transition(value: NyquistTransition) -> &'static str {
    match value {
        NyquistTransition::Gentle => "Gentle",
        NyquistTransition::Medium => "Medium",
        NyquistTransition::Steep => "Steep",
        NyquistTransition::Sharp => "Sharp",
        NyquistTransition::BrickWall => "BrickWall",
    }
}

fn dither_type(value: DitherType) -> &'static str {
    match value {
        DitherType::None => "None",
        DitherType::Tpdf => "Tpdf",
        DitherType::SlopedTpdf => "SlopedTpdf",
        DitherType::Shibata => "Shibata",
        DitherType::Lipshitz => "Lipshitz",
        DitherType::FWeighted => "FWeighted",
        DitherType::ModifiedEWeighted => "ModifiedEWeighted",
        DitherType::ImprovedEWeighted => "ImprovedEWeighted",
        DitherType::Gesemann => "Gesemann",
        DitherType::LowShibata => "LowShibata",
        DitherType::HighShibata => "HighShibata",
    }
}

fn mp3_mode(value: Mp3Mode) -> &'static str {
    match value {
        Mp3Mode::Cbr => "Cbr",
        Mp3Mode::Vbr => "Vbr",
        Mp3Mode::Abr => "Abr",
    }
}

fn aac_profile(value: AacProfile) -> &'static str {
    match value {
        AacProfile::LcAac => "LcAac",
        AacProfile::HeAac => "HeAac",
        AacProfile::HeAacV2 => "HeAacV2",
    }
}

fn replay_gain_mode(value: ReplayGainMode) -> &'static str {
    match value {
        ReplayGainMode::Track => "Track",
        ReplayGainMode::Album => "Album",
        ReplayGainMode::Both => "Both",
    }
}

fn opus_content_type(value: OpusContentType) -> &'static str {
    match value {
        OpusContentType::Auto => "Auto",
        OpusContentType::Music => "Music",
        OpusContentType::Speech => "Speech",
    }
}

fn wavpack_mode(value: WavPackMode) -> &'static str {
    match value {
        WavPackMode::Normal => "Normal",
        WavPackMode::Fast => "Fast",
        WavPackMode::High => "High",
        WavPackMode::VeryHigh => "VeryHigh",
    }
}

fn ssrc_profile(value: SsrcProfile) -> &'static str {
    match value {
        SsrcProfile::Insane => "Insane",
        SsrcProfile::High => "High",
        SsrcProfile::Long => "Long",
        SsrcProfile::Standard => "Standard",
        SsrcProfile::Short => "Short",
        SsrcProfile::Fast => "Fast",
        SsrcProfile::Lightning => "Lightning",
    }
}

fn dsd_noise_shaper(value: DsdNoiseShaper) -> &'static str {
    match value {
        DsdNoiseShaper::Clans => "Clans",
        DsdNoiseShaper::Sdm => "Sdm",
        DsdNoiseShaper::Crfb => "Crfb",
    }
}

fn modulator_order(value: ModulatorOrder) -> &'static str {
    match value {
        ModulatorOrder::Order4 => "Order4",
        ModulatorOrder::Order5 => "Order5",
        ModulatorOrder::Order6 => "Order6",
        ModulatorOrder::Order7 => "Order7",
        ModulatorOrder::Order8 => "Order8",
    }
}

fn dsd_filter_preset(value: DsdFilterPreset) -> &'static str {
    match value {
        DsdFilterPreset::Auto => "Auto",
        DsdFilterPreset::Sinc => "Sinc",
    }
}

fn dsd_lowpass_method(value: DsdLowpassMethod) -> &'static str {
    match value {
        DsdLowpassMethod::Auto => "Auto",
        DsdLowpassMethod::SoxUltra => "SoxUltra",
        DsdLowpassMethod::Sinc => "Sinc",
    }
}

fn gain_compensation(value: GainCompensation) -> String {
    match value {
        GainCompensation::Auto => "Auto".to_string(),
        GainCompensation::Linear(value) => format!("Linear({})", f32_value(value)),
        GainCompensation::Decibels(value) => format!("Decibels({})", f32_value(value)),
        GainCompensation::Disabled => "Disabled".to_string(),
    }
}

fn dsd_rate(value: crate::enums::DsdRate) -> &'static str {
    match value {
        crate::enums::DsdRate::Dsd64 => "Dsd64",
        crate::enums::DsdRate::Dsd128 => "Dsd128",
        crate::enums::DsdRate::Dsd256 => "Dsd256",
        crate::enums::DsdRate::Dsd512 => "Dsd512",
        crate::enums::DsdRate::Dsd1024 => "Dsd1024",
    }
}

fn pcm_bit_depth(value: PcmBitDepth) -> &'static str {
    match value {
        PcmBitDepth::Int8 => "Int8",
        PcmBitDepth::Int16 => "Int16",
        PcmBitDepth::Int24 => "Int24",
        PcmBitDepth::Int32 => "Int32",
        PcmBitDepth::Float32 => "Float32",
        PcmBitDepth::Float64 => "Float64",
    }
}
