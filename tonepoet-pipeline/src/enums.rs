//! Unified enum domain for tonepoet conversion planning.
//!
//! These enums replace the duplicate type hierarchies that previously existed
//! in the main crate, the backend crate, and the pipeline path.

use core::fmt;

/// Audio container or target format known to tonepoet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AudioFormat {
    /// Free Lossless Audio Codec.
    #[default]
    Flac,
    /// RIFF/WAVE PCM container.
    Wav,
    /// Audio Interchange File Format.
    Aiff,
    /// WavPack container.
    WavPack,
    /// MPEG Layer III.
    Mp3,
    /// Advanced Audio Coding, normally in M4A.
    Aac,
    /// Opus audio.
    Opus,
    /// Apple Lossless Audio Codec, normally in M4A.
    Alac,
    /// DSD Stream File.
    Dsf,
    /// DSD Interchange File Format.
    Dff,
    /// Caller-defined extension point for a registered tool.
    Custom {
        /// Stable extension without the leading dot.
        extension: String,
        /// User-facing name.
        display_name: String,
    },
}

impl AudioFormat {
    /// Conventional extension without a leading dot.
    #[must_use]
    pub fn extension(&self) -> &str {
        match self {
            Self::Flac => "flac",
            Self::Wav => "wav",
            Self::Aiff => "aiff",
            Self::WavPack => "wv",
            Self::Mp3 => "mp3",
            Self::Aac => "m4a",
            Self::Opus => "opus",
            Self::Alac => "m4a",
            Self::Dsf => "dsf",
            Self::Dff => "dff",
            Self::Custom { extension, .. } => extension.as_str(),
        }
    }

    /// Human-readable format label.
    #[must_use]
    pub fn display_name(&self) -> &str {
        match self {
            Self::Flac => "FLAC",
            Self::Wav => "WAV",
            Self::Aiff => "AIFF",
            Self::WavPack => "WavPack",
            Self::Mp3 => "MP3",
            Self::Aac => "AAC",
            Self::Opus => "Opus",
            Self::Alac => "ALAC",
            Self::Dsf => "DSF",
            Self::Dff => "DFF",
            Self::Custom { display_name, .. } => display_name.as_str(),
        }
    }

    /// True for DSF and DFF.
    #[must_use]
    pub fn is_dsd(&self) -> bool {
        matches!(self, Self::Dsf | Self::Dff)
    }

    /// True for PCM-capable lossless targets.
    #[must_use]
    pub fn is_pcm_lossless(&self) -> bool {
        matches!(
            self,
            Self::Flac | Self::Wav | Self::Aiff | Self::WavPack | Self::Alac
        )
    }

    /// True for lossy targets.
    #[must_use]
    pub fn is_lossy(&self) -> bool {
        matches!(self, Self::Mp3 | Self::Aac | Self::Opus)
    }

    /// True when FFmpeg's built-in encoders can normally write this format.
    #[must_use]
    pub fn ffmpeg_encodable(&self) -> bool {
        matches!(
            self,
            Self::Flac
                | Self::Wav
                | Self::Aiff
                | Self::WavPack
                | Self::Mp3
                | Self::Aac
                | Self::Opus
                | Self::Alac
        )
    }

    /// True when SoX can normally write this target in tonepoet workflows.
    #[must_use]
    pub fn sox_encodable(&self) -> bool {
        matches!(
            self,
            Self::Flac
                | Self::Wav
                | Self::Aiff
                | Self::WavPack
                | Self::Mp3
                | Self::Opus
                | Self::Dsf
                | Self::Dff
        )
    }
}

impl fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

/// Detected codec for the primary audio stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AudioCodec {
    /// FLAC codec.
    Flac,
    /// Signed integer PCM.
    PcmSigned,
    /// Unsigned integer PCM.
    PcmUnsigned,
    /// Floating-point PCM.
    PcmFloat,
    /// WavPack codec.
    WavPack,
    /// MP3 codec.
    Mp3,
    /// AAC codec.
    Aac,
    /// Opus codec.
    Opus,
    /// ALAC codec.
    Alac,
    /// DSD codec.
    Dsd,
    /// Caller-defined codec label.
    Custom(String),
}

impl AudioCodec {
    /// True when this codec carries DSD data.
    #[must_use]
    pub fn is_dsd(&self) -> bool {
        matches!(self, Self::Dsd)
    }
}

/// Sample representation for a decoded or target stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SampleKind {
    /// Signed integer PCM.
    SignedInteger,
    /// Unsigned integer PCM.
    UnsignedInteger,
    /// Floating-point PCM.
    Float,
    /// One-bit DSD.
    Dsd,
}

/// Supported PCM bit-depth targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PcmBitDepth {
    /// 8-bit integer PCM.
    Int8,
    /// 16-bit integer PCM.
    Int16,
    /// 24-bit integer PCM.
    Int24,
    /// 32-bit integer PCM.
    Int32,
    /// 32-bit floating-point PCM.
    Float32,
    /// 64-bit floating-point PCM, intended for intermediates.
    Float64,
}

impl PcmBitDepth {
    /// Numeric bit count.
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Self::Int8 => 8,
            Self::Int16 => 16,
            Self::Int24 => 24,
            Self::Int32 | Self::Float32 => 32,
            Self::Float64 => 64,
        }
    }

    /// True for floating-point PCM.
    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self, Self::Float32 | Self::Float64)
    }

    /// Sample kind implied by this bit-depth target.
    #[must_use]
    pub const fn sample_kind(self) -> SampleKind {
        if self.is_float() {
            SampleKind::Float
        } else {
            SampleKind::SignedInteger
        }
    }
}

/// Bit-depth target semantics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BitDepthTarget {
    /// Keep source depth where the selected format permits it.
    #[default]
    Source,
    /// Convert to the given PCM depth.
    Pcm(PcmBitDepth),
}

/// Sample-rate target semantics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RateTarget {
    /// Keep source sample rate where possible.
    #[default]
    Source,
    /// Convert to a PCM sample rate in Hz.
    PcmHz(u32),
    /// Convert to a DSD rate.
    Dsd(DsdRate),
}

/// DSD target rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DsdRate {
    /// DSD64, 2.8224 MHz.
    Dsd64,
    /// DSD128, 5.6448 MHz.
    Dsd128,
    /// DSD256, 11.2896 MHz.
    Dsd256,
    /// DSD512, 22.5792 MHz.
    Dsd512,
    /// DSD1024, 45.1584 MHz.
    Dsd1024,
}

impl DsdRate {
    /// DSD sample rate in Hz.
    #[must_use]
    pub const fn hz(self) -> u32 {
        match self {
            Self::Dsd64 => 2_822_400,
            Self::Dsd128 => 5_644_800,
            Self::Dsd256 => 11_289_600,
            Self::Dsd512 => 22_579_200,
            Self::Dsd1024 => 45_158_400,
        }
    }

    /// SoX DSD effect name for this rate.
    #[must_use]
    pub const fn sox_effect(self) -> &'static str {
        match self {
            Self::Dsd64 => "dsd64",
            Self::Dsd128 => "dsd128",
            Self::Dsd256 => "dsd256",
            Self::Dsd512 => "dsd512",
            Self::Dsd1024 => "dsd1024",
        }
    }

    /// Conservative default PCM target rate for DSD to PCM conversion.
    #[must_use]
    pub const fn default_pcm_target_hz(self) -> u32 {
        match self {
            Self::Dsd64 => 88_200,
            Self::Dsd128 => 176_400,
            Self::Dsd256 => 352_800,
            Self::Dsd512 => 352_800,
            Self::Dsd1024 => 705_600,
        }
    }

    /// Low-pass cutoff for DSD→PCM Auto conversion. Strips shaped noise above
    /// the source DSD rate's clean audio bandwidth before rate conversion.
    /// Applied as `sinc -<hz>` in the Auto path only — custom/Sinc paths
    /// are unaffected.
    ///
    /// DSD512 and DSD1024 return `None` because their clean bandwidth
    /// (192/384 kHz) meets or exceeds the default target PCM Nyquist
    /// (176.4/352.8 kHz), so the rate conversion's anti-aliasing filter
    /// handles noise suppression.
    #[must_use]
    pub const fn default_pcm_lowpass_hz(self) -> Option<u32> {
        match self {
            Self::Dsd64 => Some(25_000),
            Self::Dsd128 => Some(48_000),
            Self::Dsd256 => Some(96_000),
            Self::Dsd512 | Self::Dsd1024 => None,
        }
    }

    /// Recommended noise shaper for PCM→DSD Auto preset at this rate.
    ///
    /// Higher rates allow lower-order modulators because the wider frequency
    /// space provides more room for shaped noise. DSD1024 uses SDM (simpler,
    /// perfectly stable) rather than CLANS because the noise budget is enormous.
    #[must_use]
    pub const fn default_noise_shaper(self) -> DsdNoiseShaper {
        match self {
            Self::Dsd64 | Self::Dsd128 | Self::Dsd256 | Self::Dsd512 => DsdNoiseShaper::Clans,
            Self::Dsd1024 => DsdNoiseShaper::Sdm,
        }
    }

    /// Recommended modulator order for PCM→DSD Auto preset at this rate.
    ///
    /// DSD64 needs maximum shaping (8th order) because the noise budget is
    /// tightest. Each doubling of rate allows one order less. DSD1024 uses
    /// 4th order — ample headroom with minimal feedback complexity.
    #[must_use]
    pub const fn default_modulator_order(self) -> ModulatorOrder {
        match self {
            Self::Dsd64 => ModulatorOrder::Order8,
            Self::Dsd128 => ModulatorOrder::Order7,
            Self::Dsd256 => ModulatorOrder::Order6,
            Self::Dsd512 => ModulatorOrder::Order5,
            Self::Dsd1024 => ModulatorOrder::Order4,
        }
    }

    /// Parse a DSD rate from a sample rate in Hz.
    #[must_use]
    pub const fn from_hz(hz: u32) -> Option<Self> {
        match hz {
            2_822_400 => Some(Self::Dsd64),
            5_644_800 => Some(Self::Dsd128),
            11_289_600 => Some(Self::Dsd256),
            22_579_200 => Some(Self::Dsd512),
            45_158_400 => Some(Self::Dsd1024),
            _ => None,
        }
    }
}

/// Exact dither algorithm requested by the user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DitherType {
    /// No dither.
    #[default]
    None,
    /// Standard triangular PDF dither.
    #[cfg_attr(feature = "serde", serde(alias = "TPDF"))]
    Tpdf,
    /// Sloped TPDF dither. Also accepts the legacy misspelling `SloppedTPDF` under serde.
    #[cfg_attr(feature = "serde", serde(alias = "SloppedTPDF", alias = "SlopedTPDF"))]
    SlopedTpdf,
    /// SoX Shibata noise shaping.
    Shibata,
    /// Lipshitz noise shaping.
    Lipshitz,
    /// F-weighted noise shaping.
    #[cfg_attr(feature = "serde", serde(alias = "FShaped", alias = "FWeighted"))]
    FWeighted,
    /// Modified E-weighted noise shaping.
    #[cfg_attr(
        feature = "serde",
        serde(alias = "ModifiedE", alias = "ModifiedEWeighted")
    )]
    ModifiedEWeighted,
    /// Improved E-weighted noise shaping.
    #[cfg_attr(
        feature = "serde",
        serde(alias = "ImprovedE", alias = "ImprovedEWeighted")
    )]
    ImprovedEWeighted,
    /// Gesemann dithering.
    Gesemann,
    /// Low-Shibata shaping.
    LowShibata,
    /// High-Shibata shaping.
    HighShibata,
}

/// User-facing resample quality.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ResampleQuality {
    /// Fastest, lowest quality.
    Low,
    /// Medium quality.
    Medium,
    /// High quality.
    #[default]
    High,
    /// Very high quality.
    VeryHigh,
    /// Maximum practical quality.
    Ultra,
    /// Theoretical perfection (SSRC insane profile, 200 dB stopband).
    Insane,
}

/// Nyquist transition-band preference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum NyquistTransition {
    /// Gradual rolloff.
    #[default]
    Gentle,
    /// Medium rolloff.
    Medium,
    /// Steep rolloff.
    Steep,
    /// Legacy alias for steep rolloff.
    Sharp,
    /// Brick-wall path, implemented by SSRC by default.
    BrickWall,
}

/// Preferred high-level tool. The registry treats this as a hard preference
/// when the named tool supports the requested operation, then falls back to
/// capability ranking.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PreferredTool {
    /// Pick the best registered tool deterministically.
    #[default]
    Auto,
    /// Prefer FFmpeg when capable.
    Ffmpeg,
    /// Prefer SoX when capable.
    Sox,
    /// Prefer SSRC for brick-wall resampling when capable.
    Ssrc,
    /// Prefer a caller-registered binary name.
    Custom(String),
}

/// MP3 encoding mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Mp3Mode {
    /// Constant bitrate.
    Cbr,
    /// Variable bitrate.
    Vbr,
    /// Average bitrate.
    Abr,
}

/// AAC profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AacProfile {
    /// Low-complexity AAC.
    LcAac,
    /// HE-AAC.
    HeAac,
    /// HE-AAC v2.
    HeAacV2,
    /// Low-delay AAC.
    LdAac,
}

/// ReplayGain scanning mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ReplayGainMode {
    /// Track gain only.
    Track,
    /// Album gain only.
    Album,
    /// Track and album gain.
    Both,
}

/// Opus encoder application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OpusContentType {
    /// Let the planner choose a safe default.
    Auto,
    /// Music-focused settings.
    Music,
    /// Speech-focused settings.
    Speech,
}

/// WavPack compression mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum WavPackMode {
    /// Normal compression.
    Normal,
    /// Fast compression.
    Fast,
    /// High compression.
    High,
    /// Very high compression.
    VeryHigh,
}

/// SSRC resampling profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SsrcProfile {
    /// Extreme quality.
    Insane,
    /// Ultra quality.
    High,
    /// Very-high quality.
    Long,
    /// Default high quality.
    Standard,
    /// Medium quality.
    Short,
    /// Low quality.
    Fast,
    /// Real-time profile, exposed for custom workflows.
    Lightning,
}

impl SsrcProfile {
    /// SSRC profile argument.
    #[must_use]
    pub const fn as_arg(self) -> &'static str {
        match self {
            Self::Insane => "insane",
            Self::High => "high",
            Self::Long => "long",
            Self::Standard => "standard",
            Self::Short => "short",
            Self::Fast => "fast",
            Self::Lightning => "lightning",
        }
    }
}

/// SoX-DSD noise-shaper family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DsdNoiseShaper {
    /// CLANS shaper family.
    Clans,
    /// Standard SDM shaper family.
    Sdm,
    /// CRFB shaper family.
    Crfb,
}

/// Modulator order for SoX-DSD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ModulatorOrder {
    /// Fourth order.
    Order4,
    /// Fifth order.
    Order5,
    /// Sixth order.
    Order6,
    /// Seventh order.
    Order7,
    /// Eighth order.
    Order8,
}

impl ModulatorOrder {
    /// Numeric order.
    #[must_use]
    pub const fn value(self) -> u8 {
        match self {
            Self::Order4 => 4,
            Self::Order5 => 5,
            Self::Order6 => 6,
            Self::Order7 => 7,
            Self::Order8 => 8,
        }
    }
}

/// PCM/DSD filter preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DsdFilterPreset {
    /// Use practical SoX native filters.
    Auto,
    /// Use exposed FIR sinc parameters.
    Sinc,
}

/// DSD-to-PCM low-pass/filter method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DsdLowpassMethod {
    /// Use preset-selected filter.
    Auto,
    /// Use SoX `rate -u`.
    SoxUltra,
    /// Use custom sinc filter parameters.
    Sinc,
}

/// Gain compensation strategy for upsample/sinc DSD paths.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum GainCompensation {
    /// Compute gain from oversample factor.
    Auto,
    /// Linear SoX `vol` value.
    Linear(f32),
    /// Decibel SoX `gain` value.
    Decibels(f32),
    /// Do not apply compensation.
    Disabled,
}
