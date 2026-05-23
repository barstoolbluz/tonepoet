//! Pure parameter-to-argument mappings used by tool plugins.
//!
//! These functions contain no ambient state and are deterministic.

use crate::enums::{
    AacProfile, AudioFormat, DitherType, DsdLowpassMethod, DsdNoiseShaper, ModulatorOrder, Mp3Mode,
    NyquistTransition, OpusContentType, PcmBitDepth, ResampleQuality, SsrcProfile, WavPackMode,
};
use crate::error::{PlanningError, Result};
use crate::settings::SsrcSettings;

/// Map resample quality to SoXR precision.
#[must_use]
pub const fn soxr_precision(quality: ResampleQuality) -> u8 {
    match quality {
        ResampleQuality::Ultra => 32,
        ResampleQuality::VeryHigh => 28,
        ResampleQuality::High => 24,
        ResampleQuality::Medium => 20,
        ResampleQuality::Low => 16,
    }
}

/// Map resample quality to SoX rate effect flag.
#[must_use]
pub const fn sox_rate_quality_flag(quality: ResampleQuality) -> &'static str {
    match quality {
        ResampleQuality::Ultra => "-v",
        ResampleQuality::VeryHigh => "-h",
        ResampleQuality::High => "-m",
        ResampleQuality::Medium => "-l",
        ResampleQuality::Low => "-q",
    }
}

/// Map DSD auto presets to SoX's ultra-quality rate flag.
#[must_use]
pub const fn sox_dsd_auto_rate_flag() -> &'static str {
    "-u"
}

/// Map a DSD low-pass policy to the SoX `rate` quality flag used when the
/// path is not using the explicit sinc filter. `Auto` follows the normal
/// resampling-quality setting; `SoxUltra` pins SoX's ultra-quality path.
#[must_use]
pub const fn sox_dsd_lowpass_rate_flag(
    lowpass: DsdLowpassMethod,
    quality: ResampleQuality,
) -> &'static str {
    match lowpass {
        DsdLowpassMethod::SoxUltra => "-u",
        DsdLowpassMethod::Auto => sox_rate_quality_flag(quality),
        DsdLowpassMethod::Sinc => "-u",
    }
}

/// Map Nyquist transition to FFmpeg/SWResampler cutoff.
#[must_use]
pub const fn ffmpeg_cutoff(transition: NyquistTransition) -> f32 {
    match transition {
        NyquistTransition::Gentle => 0.95,
        NyquistTransition::Medium => 0.97,
        NyquistTransition::Steep | NyquistTransition::Sharp | NyquistTransition::BrickWall => 0.997,
    }
}

/// Map Nyquist transition to SoX rolloff value.
#[must_use]
pub const fn sox_rolloff(transition: NyquistTransition) -> Option<&'static str> {
    match transition {
        NyquistTransition::Gentle => Some("0.95"),
        NyquistTransition::Medium => Some("0.97"),
        NyquistTransition::Steep | NyquistTransition::Sharp => Some("0.997"),
        NyquistTransition::BrickWall => None,
    }
}

/// Map dither type to SoX dither effect arguments.
#[must_use]
pub fn sox_dither_args(dither: DitherType) -> Vec<String> {
    match dither {
        DitherType::None => Vec::new(),
        DitherType::Tpdf => vec!["dither".into()],
        DitherType::SlopedTpdf => vec!["dither".into(), "-S".into()],
        DitherType::Shibata => vec!["dither".into(), "-s".into()],
        DitherType::Lipshitz => vec!["dither".into(), "-f".into(), "lipshitz".into()],
        DitherType::FWeighted => vec![
            "dither".into(),
            "-s".into(),
            "-f".into(),
            "f-weighted".into(),
        ],
        DitherType::ModifiedEWeighted => vec![
            "dither".into(),
            "-s".into(),
            "-f".into(),
            "modified-e-weighted".into(),
        ],
        DitherType::ImprovedEWeighted => vec![
            "dither".into(),
            "-s".into(),
            "-f".into(),
            "improved-e-weighted".into(),
        ],
        DitherType::Gesemann => vec!["dither".into(), "-f".into(), "gesemann".into()],
        DitherType::LowShibata => vec![
            "dither".into(),
            "-s".into(),
            "-f".into(),
            "low-shibata".into(),
        ],
        DitherType::HighShibata => vec![
            "dither".into(),
            "-s".into(),
            "-f".into(),
            "high-shibata".into(),
        ],
    }
}

/// Map dither type to SoXR dither method, or `None` if not supported.
#[must_use]
pub const fn soxr_dither_method(dither: DitherType) -> Option<&'static str> {
    match dither {
        DitherType::None => Some("none"),
        DitherType::Tpdf | DitherType::SlopedTpdf => Some("triangular"),
        DitherType::Shibata | DitherType::LowShibata | DitherType::HighShibata => Some("shibata"),
        DitherType::FWeighted => Some("f_weighted"),
        DitherType::ModifiedEWeighted | DitherType::ImprovedEWeighted => {
            Some("modified_e_weighted")
        }
        DitherType::Lipshitz | DitherType::Gesemann => None,
    }
}

/// Map dither to SSRC dither/noise-shaping numeric ID.
#[must_use]
pub const fn ssrc_dither_id(dither: DitherType) -> u8 {
    match dither {
        DitherType::None => 99,
        DitherType::Tpdf | DitherType::SlopedTpdf => 98,
        DitherType::LowShibata => 0,
        DitherType::Shibata => 2,
        DitherType::HighShibata => 6,
        DitherType::Lipshitz
        | DitherType::FWeighted
        | DitherType::ModifiedEWeighted
        | DitherType::ImprovedEWeighted
        | DitherType::Gesemann => 99,
    }
}

/// Resolve SSRC profile from explicit profile, insane mode, and quality.
#[must_use]
pub const fn ssrc_profile(settings: SsrcSettings, quality: ResampleQuality) -> SsrcProfile {
    if settings.insane_mode {
        return SsrcProfile::Insane;
    }
    if let Some(profile) = settings.profile {
        return profile;
    }
    match quality {
        ResampleQuality::Ultra => SsrcProfile::High,
        ResampleQuality::VeryHigh => SsrcProfile::Long,
        ResampleQuality::High => SsrcProfile::Standard,
        ResampleQuality::Medium => SsrcProfile::Short,
        ResampleQuality::Low => SsrcProfile::Fast,
    }
}

/// FFmpeg PCM codec for the target format and bit depth.
pub fn ffmpeg_pcm_codec(depth: PcmBitDepth, format: &AudioFormat) -> Result<&'static str> {
    let big_endian = matches!(format, AudioFormat::Aiff);
    match depth {
        PcmBitDepth::Int8 => Ok("pcm_u8"),
        PcmBitDepth::Int16 if big_endian => Ok("pcm_s16be"),
        PcmBitDepth::Int16 => Ok("pcm_s16le"),
        PcmBitDepth::Int24 if big_endian => Ok("pcm_s24be"),
        PcmBitDepth::Int24 => Ok("pcm_s24le"),
        PcmBitDepth::Int32 if big_endian => Ok("pcm_s32be"),
        PcmBitDepth::Int32 => Ok("pcm_s32le"),
        PcmBitDepth::Float32 if supports_float(format) && big_endian => Ok("pcm_f32be"),
        PcmBitDepth::Float32 if supports_float(format) => Ok("pcm_f32le"),
        PcmBitDepth::Float64 if supports_float(format) && big_endian => Ok("pcm_f64be"),
        PcmBitDepth::Float64 if supports_float(format) => Ok("pcm_f64le"),
        PcmBitDepth::Float32 | PcmBitDepth::Float64 => Err(PlanningError::invalid_settings(
            "target_bit_depth",
            format!(
                "{} does not support floating-point PCM output",
                format.display_name()
            ),
        )),
    }
}

/// True when a format can safely contain floating-point PCM in tonepoet workflows.
#[must_use]
pub fn supports_float(format: &AudioFormat) -> bool {
    matches!(format, AudioFormat::Wav | AudioFormat::WavPack)
}

/// FFmpeg sample format for a PCM bit depth.
#[must_use]
pub const fn ffmpeg_sample_fmt(depth: PcmBitDepth) -> &'static str {
    match depth {
        PcmBitDepth::Int8 => "u8",
        PcmBitDepth::Int16 => "s16",
        PcmBitDepth::Int24 | PcmBitDepth::Int32 => "s32",
        PcmBitDepth::Float32 => "flt",
        PcmBitDepth::Float64 => "dbl",
    }
}

/// FFmpeg AAC profile string.
#[must_use]
pub const fn ffmpeg_aac_profile(profile: AacProfile) -> &'static str {
    match profile {
        AacProfile::LcAac => "aac_low",
        AacProfile::HeAac => "aac_he",
        AacProfile::HeAacV2 => "aac_he_v2",
        AacProfile::LdAac => "aac_ld",
    }
}

/// FFmpeg/libopus application string.
#[must_use]
pub const fn opus_application(content: OpusContentType) -> &'static str {
    match content {
        OpusContentType::Auto | OpusContentType::Music => "audio",
        OpusContentType::Speech => "voip",
    }
}

/// SoX MP3 `-C` value.
#[must_use]
pub fn sox_mp3_compression(mode: Mp3Mode, bitrate_kbps: u32, vbr_quality: u8) -> String {
    match mode {
        Mp3Mode::Cbr => bitrate_kbps.to_string(),
        Mp3Mode::Abr => format!("~{bitrate_kbps}"),
        Mp3Mode::Vbr => format!("-{vbr_quality}"),
    }
}

/// WavPack compression argument for FFmpeg.
#[must_use]
pub const fn wavpack_compression_level(mode: WavPackMode) -> u8 {
    match mode {
        WavPackMode::Fast => 0,
        WavPackMode::Normal => 1,
        WavPackMode::High => 2,
        WavPackMode::VeryHigh => 3,
    }
}

/// SoX-DSD shaper string such as `clans-8`.
#[must_use]
pub fn dsd_shaper_name(shaper: DsdNoiseShaper, order: ModulatorOrder) -> String {
    let prefix = match shaper {
        DsdNoiseShaper::Clans => "clans",
        DsdNoiseShaper::Sdm => "sdm",
        DsdNoiseShaper::Crfb => "crfb",
    };
    format!("{prefix}-{}", order.value())
}

/// Whether the given tool should avoid FFmpeg's SoXR dither and route dither to SoX instead.
#[must_use]
pub const fn requires_sox_dither(dither: DitherType) -> bool {
    matches!(
        dither,
        DitherType::Lipshitz | DitherType::Gesemann | DitherType::SlopedTpdf
    )
}
