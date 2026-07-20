//! Qualified P0 Reference DSD-to-PCM policy and pure planning primitives.
//!
//! This module intentionally contains no filesystem or process I/O. Runtime
//! source materialization, tool attestation, measurement execution, publication,
//! and qualification reporting live in the orchestrator crate.

use crate::enums::{AudioFormat, BitDepthTarget, DsdRate, PcmBitDepth, RateTarget, SampleKind};
use crate::error::{PlanningError, Result};
use crate::plan::{
    CommandEnvironmentPolicy, ConversionPlan, Finalization, InputSource, OutputSink, PlanRequest,
    PlannedCommand, PlannedExecutionStep,
};
use crate::tools::ToolIdentifier;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Stable historical policy key for the original commissioned contract.
pub const DSD_REFERENCE_POLICY_V1_KEY: &str = "sox_ng_14_8_0_1_v1";
/// Stable policy key for the corrected v2 SoX-ng Reference contract.
pub const DSD_REFERENCE_POLICY_V2_KEY: &str = "sox_ng_14_8_0_1_v2";
/// Stable policy key for the corrected v3 evidence and admission contract.
pub const DSD_REFERENCE_POLICY_V3_KEY: &str = "sox_ng_14_8_0_1_v3";
/// Stable policy key for the corrected v4 exact streamed-analyzer contract.
pub const DSD_REFERENCE_POLICY_V4_KEY: &str = "sox_ng_14_8_0_1_v4";
/// Commissioned SoX-ng source revision.
pub const DSD_REFERENCE_SOX_NG_REVISION: &str =
    "324b8cf873fd7836e8848bd87f7a90d8faa6f849";
/// Expected SoX-ng version string fragment.
pub const DSD_REFERENCE_SOX_NG_VERSION: &str = "14.8.0.1";
/// Stable policy qualification artifact path.
pub const DSD_REFERENCE_QUALIFICATION_MANIFEST_PATH: &str =
    "qualification/dsd_reference_sox_ng_14_8_0_1_v4.json";

/// Signed nanodecibels used for policy arithmetic and canonical serialization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DbNano(pub i64);

impl DbNano {
    /// One decibel in nanodecibels.
    pub const ONE_DB: Self = Self(1_000_000_000);
    /// Zero decibels.
    pub const ZERO: Self = Self(0);
    /// Reference headroom applied before reconstruction.
    pub const REFERENCE_HEADROOM: Self = Self(-12_000_000_000);
    /// Restoration of the explicit 12 dB headroom.
    pub const HEADROOM_RESTORATION: Self = Self(12_000_000_000);
    /// Exact 2x amplitude compensation in decibels.
    pub const DSD_COMPENSATION: Self = Self(6_020_599_913);
    /// Reference true-peak ceiling.
    pub const REFERENCE_CEILING: Self = Self(-1_000_000_000);
    /// Default NormalizePeak target.
    pub const DEFAULT_NORMALIZE_TARGET: Self = Self(-150_000_000);
    /// Lowest accepted Fixed gain.
    pub const MIN_FIXED_GAIN: Self = Self(-24_000_000_000);
    /// Highest accepted Fixed gain.
    pub const MAX_FIXED_GAIN: Self = Self(24_000_000_000);
    /// Lowest accepted NormalizePeak target.
    pub const MIN_NORMALIZE_TARGET: Self = Self(-12_000_000_000);
    /// Highest accepted NormalizePeak target.
    pub const MAX_NORMALIZE_TARGET: Self = Self(0);

    /// Checked addition.
    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    /// Checked subtraction.
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    /// Render exactly nine fractional digits, optionally requiring a leading plus sign.
    #[must_use]
    pub fn render(self, mandatory_sign: bool) -> String {
        let negative = self.0 < 0;
        let magnitude = self.0.unsigned_abs();
        let whole = magnitude / 1_000_000_000;
        let fractional = magnitude % 1_000_000_000;
        let sign = if negative {
            "-"
        } else if mandatory_sign {
            "+"
        } else {
            ""
        };
        format!("{sign}{whole}.{fractional:09}")
    }
}

impl fmt::Display for DbNano {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render(false))
    }
}

impl FromStr for DbNano {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        let value = raw.trim();
        if value.is_empty() || value.contains('e') || value.contains('E') || value.contains(',') {
            return Err("dB value must be a plain decimal".to_string());
        }
        let (negative, unsigned) = match value.as_bytes()[0] {
            b'-' => (true, &value[1..]),
            b'+' => (false, &value[1..]),
            _ => (false, value),
        };
        if unsigned.is_empty() {
            return Err("dB value is missing digits".to_string());
        }
        let mut parts = unsigned.split('.');
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next();
        if parts.next().is_some()
            || whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("dB value must contain one decimal number".to_string());
        }
        let fraction = fraction.unwrap_or("");
        if fraction.len() > 9 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("dB value supports at most nine fractional digits".to_string());
        }
        let whole: i128 = whole
            .parse()
            .map_err(|_| "dB whole component is out of range".to_string())?;
        let mut fractional = fraction.to_string();
        while fractional.len() < 9 {
            fractional.push('0');
        }
        let fractional: i128 = if fractional.is_empty() {
            0
        } else {
            fractional
                .parse()
                .map_err(|_| "dB fractional component is out of range".to_string())?
        };
        let magnitude = whole
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(fractional))
            .ok_or_else(|| "dB value is out of range".to_string())?;
        let signed = if negative {
            magnitude
                .checked_neg()
                .ok_or_else(|| "dB value is out of range".to_string())?
        } else {
            magnitude
        };
        i64::try_from(signed)
            .map(Self)
            .map_err(|_| "dB value is out of range".to_string())
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for DbNano {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.render(false))
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for DbNano {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// Canonical SHA-256 digest used by persisted policy and source identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256Digest(pub [u8; 32]);

impl Sha256Digest {
    /// Hash bytes into a digest.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut out = [0_u8; 32];
        out.copy_from_slice(&digest);
        Self(out)
    }

    /// Parse a lowercase or uppercase 64-character hexadecimal digest.
    pub fn from_hex(value: &str) -> std::result::Result<Self, String> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("SHA-256 digest must contain exactly 64 hexadecimal characters".to_string());
        }
        let mut out = [0_u8; 32];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            let text = std::str::from_utf8(chunk).map_err(|_| "invalid SHA-256 text")?;
            out[index] = u8::from_str_radix(text, 16)
                .map_err(|_| "invalid SHA-256 hexadecimal digit".to_string())?;
        }
        Ok(Self(out))
    }

    /// Lowercase hexadecimal form.
    #[must_use]
    pub fn to_hex(self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::from_hex(&raw).map_err(serde::de::Error::custom)
    }
}

/// Immutable Reference policy IDs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum DsdReferencePolicyVersion {
    /// Historical commissioned v1 contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v1"))]
    SoxNg14801V1,
    /// Corrected v2 command contract retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v2"))]
    SoxNg14801V2,
    /// Corrected v3 evidence, source-admission, and terminal contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v3"))]
    SoxNg14801V3,
    /// Corrected v4 exact streamed-analyzer contract.
    #[default]
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v4"))]
    SoxNg14801V4,
}

impl DsdReferencePolicyVersion {
    /// Stable serialized policy key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::SoxNg14801V1 => DSD_REFERENCE_POLICY_V1_KEY,
            Self::SoxNg14801V2 => DSD_REFERENCE_POLICY_V2_KEY,
            Self::SoxNg14801V3 => DSD_REFERENCE_POLICY_V3_KEY,
            Self::SoxNg14801V4 => DSD_REFERENCE_POLICY_V4_KEY,
        }
    }
}

/// User-selected DSD-source pathway.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum DsdSourcePathway {
    /// Qualified Reference pathway.
    #[default]
    Reference,
    /// Reserved future Manual pathway; P0 rejects it deterministically.
    Manual,
}

/// Reference reconstruction profile selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum DsdReconstructionSelection {
    /// Standard profile matrix.
    #[default]
    Reference,
    /// Explicit DSD128 wideband profile; all other v3/v4 cells reject.
    Wideband,
}

/// DSD-source output gain selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum DsdSourceGainMode {
    /// Reference restoration plus 2x amplitude compensation, ceiling constrained.
    #[default]
    Reference,
    /// Exact restoration of the explicit 12 dB headroom.
    NativeLevel,
    /// Exact headroom restoration plus a user fixed gain.
    Fixed,
    /// SoX peak normalization with modified/unqualified semantics.
    NormalizePeak,
}

/// Native-v2 settings for DSD-source conversions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct DsdSourceSettings {
    /// Reference or reserved Manual pathway.
    pub pathway: DsdSourcePathway,
    /// Immutable Reference policy ID.
    pub reference_policy: DsdReferencePolicyVersion,
    /// Standard or explicit Wideband reconstruction selection.
    pub profile: DsdReconstructionSelection,
    /// Output gain mode.
    pub gain_mode: DsdSourceGainMode,
    /// Fixed gain used only by `Fixed`.
    pub fixed_gain_db: Option<DbNano>,
    /// NormalizePeak target used only by `NormalizePeak`.
    pub normalize_peak_target_dbfs: DbNano,
}

impl Default for DsdSourceSettings {
    fn default() -> Self {
        Self {
            pathway: DsdSourcePathway::Reference,
            reference_policy: DsdReferencePolicyVersion::SoxNg14801V4,
            profile: DsdReconstructionSelection::Reference,
            gain_mode: DsdSourceGainMode::Reference,
            fixed_gain_db: None,
            normalize_peak_target_dbfs: DbNano::DEFAULT_NORMALIZE_TARGET,
        }
    }
}

/// Exact product/container identity after catalog resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
#[allow(missing_docs)]
pub enum ResolvedOutputTarget {
    FlacNative,
    FlacOgg,
    FlacMka,
    FlacMkv,
    WavRiff,
    WavRf64,
    WavW64,
    WavMka,
    WavMkv,
    AiffNative,
    AiffMka,
    AiffMkv,
    WavPackNative,
    WavPackMka,
    WavPackMkv,
    Mp3Native,
    Mp3Mka,
    Mp3Mkv,
    AacM4a,
    AacMp4,
    AacM4b,
    AacMka,
    AacMkv,
    OpusNative,
    OpusWebM,
    OpusWebA,
    OpusMka,
    OpusMkv,
    AlacM4a,
    AlacMp4,
    DsfNative,
    DsfAsDff,
    DffNative,
    DtsNative,
    DtsMka,
    DtsMkv,
    DtsMp4,
    Ac3Native,
    Ac3Mka,
    Ac3Mkv,
    Ac3Mp4,
    LpcmRiff,
    LpcmAiff,
}

impl ResolvedOutputTarget {
    /// True for the seven P0 Reference lossless targets.
    #[must_use]
    pub const fn is_p0_reference_lossless(self) -> bool {
        matches!(
            self,
            Self::FlacNative
                | Self::WavRiff
                | Self::WavRf64
                | Self::WavW64
                | Self::AiffNative
                | Self::WavPackNative
                | Self::AlacM4a
        )
    }

    /// True for a lossy delivery target reserved for future Reference-front-end use.
    #[must_use]
    pub const fn is_lossy(self) -> bool {
        matches!(
            self,
            Self::Mp3Native
                | Self::Mp3Mka
                | Self::Mp3Mkv
                | Self::AacM4a
                | Self::AacMp4
                | Self::AacM4b
                | Self::AacMka
                | Self::AacMkv
                | Self::OpusNative
                | Self::OpusWebM
                | Self::OpusWebA
                | Self::OpusMka
                | Self::OpusMkv
                | Self::DtsNative
                | Self::DtsMka
                | Self::DtsMkv
                | Self::DtsMp4
                | Self::Ac3Native
                | Self::Ac3Mka
                | Self::Ac3Mkv
                | Self::Ac3Mp4
        )
    }

    /// Canonical key used by presets and fingerprints.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::FlacNative => "flac_native",
            Self::FlacOgg => "flac_ogg",
            Self::FlacMka => "flac_mka",
            Self::FlacMkv => "flac_mkv",
            Self::WavRiff => "wav_riff",
            Self::WavRf64 => "wav_rf64",
            Self::WavW64 => "wav_w64",
            Self::WavMka => "wav_mka",
            Self::WavMkv => "wav_mkv",
            Self::AiffNative => "aiff_native",
            Self::AiffMka => "aiff_mka",
            Self::AiffMkv => "aiff_mkv",
            Self::WavPackNative => "wavpack_native",
            Self::WavPackMka => "wavpack_mka",
            Self::WavPackMkv => "wavpack_mkv",
            Self::Mp3Native => "mp3_native",
            Self::Mp3Mka => "mp3_mka",
            Self::Mp3Mkv => "mp3_mkv",
            Self::AacM4a => "aac_m4a",
            Self::AacMp4 => "aac_mp4",
            Self::AacM4b => "aac_m4b",
            Self::AacMka => "aac_mka",
            Self::AacMkv => "aac_mkv",
            Self::OpusNative => "opus_native",
            Self::OpusWebM => "opus_webm",
            Self::OpusWebA => "opus_weba",
            Self::OpusMka => "opus_mka",
            Self::OpusMkv => "opus_mkv",
            Self::AlacM4a => "alac_m4a",
            Self::AlacMp4 => "alac_mp4",
            Self::DsfNative => "dsf_native",
            Self::DsfAsDff => "dsf_as_dff",
            Self::DffNative => "dff_native",
            Self::DtsNative => "dts_native",
            Self::DtsMka => "dts_mka",
            Self::DtsMkv => "dts_mkv",
            Self::DtsMp4 => "dts_mp4",
            Self::Ac3Native => "ac3_native",
            Self::Ac3Mka => "ac3_mka",
            Self::Ac3Mkv => "ac3_mkv",
            Self::Ac3Mp4 => "ac3_mp4",
            Self::LpcmRiff => "lpcm_riff",
            Self::LpcmAiff => "lpcm_aiff",
        }
    }

    /// Resolve a planner target from format, extension, and trusted catalog flags.
    pub fn resolve(
        format: &AudioFormat,
        extension: &str,
        flags: &[String],
    ) -> Result<Self> {
        let extension = extension.trim_start_matches('.').to_ascii_lowercase();
        let empty = flags.is_empty();
        let rf64_auto = flags == ["-rf64".to_string(), "auto".to_string()];
        let weba = flags == ["-f".to_string(), "webm".to_string()];
        let target = match (format, extension.as_str()) {
            (AudioFormat::Flac, "flac") if empty => Self::FlacNative,
            (AudioFormat::Flac, "ogg") if empty => Self::FlacOgg,
            (AudioFormat::Flac, "mka") if empty => Self::FlacMka,
            (AudioFormat::Flac, "mkv") if empty => Self::FlacMkv,
            (AudioFormat::Wav, "wav") if empty => Self::WavRiff,
            (AudioFormat::Wav, "wav") if rf64_auto => Self::WavRf64,
            (AudioFormat::Wav, "w64") if empty => Self::WavW64,
            (AudioFormat::Wav, "mka") if empty => Self::WavMka,
            (AudioFormat::Wav, "mkv") if empty => Self::WavMkv,
            (AudioFormat::Aiff, "aiff" | "aif") if empty => Self::AiffNative,
            (AudioFormat::Aiff, "mka") if empty => Self::AiffMka,
            (AudioFormat::Aiff, "mkv") if empty => Self::AiffMkv,
            (AudioFormat::WavPack, "wv") if empty => Self::WavPackNative,
            (AudioFormat::WavPack, "mka") if empty => Self::WavPackMka,
            (AudioFormat::WavPack, "mkv") if empty => Self::WavPackMkv,
            (AudioFormat::Mp3, "mp3") if empty => Self::Mp3Native,
            (AudioFormat::Mp3, "mka") if empty => Self::Mp3Mka,
            (AudioFormat::Mp3, "mkv") if empty => Self::Mp3Mkv,
            (AudioFormat::Aac, "m4a") if empty => Self::AacM4a,
            (AudioFormat::Aac, "mp4") if empty => Self::AacMp4,
            (AudioFormat::Aac, "m4b") if empty => Self::AacM4b,
            (AudioFormat::Aac, "mka") if empty => Self::AacMka,
            (AudioFormat::Aac, "mkv") if empty => Self::AacMkv,
            (AudioFormat::Opus, "opus") if empty => Self::OpusNative,
            (AudioFormat::Opus, "webm") if empty => Self::OpusWebM,
            (AudioFormat::Opus, "weba") if weba => Self::OpusWebA,
            (AudioFormat::Opus, "mka") if empty => Self::OpusMka,
            (AudioFormat::Opus, "mkv") if empty => Self::OpusMkv,
            (AudioFormat::Alac, "m4a") if empty => Self::AlacM4a,
            (AudioFormat::Alac, "mp4") if empty => Self::AlacMp4,
            (AudioFormat::Dsf, "dsf") if empty => Self::DsfNative,
            (AudioFormat::Dsf, "dff") if empty => Self::DsfAsDff,
            (AudioFormat::Dff, "dff") if empty => Self::DffNative,
            (AudioFormat::Dts, "dts") if empty => Self::DtsNative,
            (AudioFormat::Dts, "mka") if empty => Self::DtsMka,
            (AudioFormat::Dts, "mkv") if empty => Self::DtsMkv,
            (AudioFormat::Dts, "mp4") if empty => Self::DtsMp4,
            (AudioFormat::Ac3, "ac3") if empty => Self::Ac3Native,
            (AudioFormat::Ac3, "mka") if empty => Self::Ac3Mka,
            (AudioFormat::Ac3, "mkv") if empty => Self::Ac3Mkv,
            (AudioFormat::Ac3, "mp4") if empty => Self::Ac3Mp4,
            _ => {
                return Err(PlanningError::invalid_settings(
                    "resolved_output_target",
                    reference_error_text(ReferenceErrorCode::CanonicalTarget),
                ));
            }
        };
        Ok(target)
    }
}

/// Delivery classification reserved independently from reconstruction policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum DsdDeliveryClass {
    /// P0 lossless Reference finalization.
    LosslessReference,
    /// Reserved future lossy finalization; P0 rejects.
    LossyReferenceFrontEnd,
}

/// Original DSD container/encoding fact.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum DsdSourceKind {
    /// Native uncompressed DSF.
    DsfUncompressed,
    /// Native uncompressed DSDIFF/DSD.
    DsdiffUncompressed,
    /// DSDIFF/DST requiring qualified decode.
    DsdiffDst,
    /// One selected SACD track.
    SacdTrack {
        /// DSD or DST area encoding.
        frame_format: SacdFrameEncoding,
        /// Immutable TOC selection authority.
        selection: SacdTrackSelection,
    },
    /// Unknown DSD container/encoding.
    UnknownDsdContainer,
}

/// SACD frame encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum SacdFrameEncoding {
    /// Uncompressed DSD frames.
    Dsd,
    /// Losslessly compressed DST frames.
    Dst,
}

/// SACD area kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum SacdAreaKind {
    /// Stereo area.
    Stereo,
    /// Multichannel area, represented but rejected by P0 Reference.
    Multichannel,
}

/// Immutable selected SACD track identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct SacdTrackSelection {
    /// Selected SACD area.
    pub area: SacdAreaKind,
    /// Zero-based TOC track index.
    pub track_index_zero_based: u32,
    /// Source start frame.
    pub start_frame: u64,
    /// Source frame count.
    pub frame_count: u64,
    /// Digest of the authoritative TOC facts.
    pub toc_digest: Sha256Digest,
}

/// Qualified in-process DST decoder versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum QualifiedDstDecoderVersion {
    /// In-tree sacd-rs decoder bound to the build and fixture manifest.
    SacdRsP0V1,
}

/// Qualified SACD extraction versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum QualifiedSacdExtractorVersion {
    /// In-tree sacd-rs per-track extractor.
    SacdRsP0V1,
}

/// Input front-end selected by the pure planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum DsdInputFrontEnd {
    /// Verified source materialization with no decode.
    NativeUncompressed,
    /// DSDIFF/DST decode.
    DsdiffDst {
        /// Qualified decoder identity.
        decoder: QualifiedDstDecoderVersion,
    },
    /// SACD DSD extraction.
    SacdDsd {
        /// Qualified extractor identity.
        extractor: QualifiedSacdExtractorVersion,
    },
    /// SACD DST extraction plus decode.
    SacdDst {
        /// Qualified extractor identity.
        extractor: QualifiedSacdExtractorVersion,
        /// Qualified decoder identity.
        decoder: QualifiedDstDecoderVersion,
    },
}

/// P0 programme classification seam.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum ReferenceProgrammeScope {
    /// One independent source.
    Singleton,
    /// Independent album batch, rejected by P0.
    IndependentAlbumBatch {
        /// Dispatcher-authored batch ID.
        conversion_log_batch_id: String,
        /// Expected member count.
        expected_members: NonZeroUsize,
        /// Digest of ordered source paths/content identities.
        ordered_source_paths_digest: Sha256Digest,
    },
    /// Continuous image would be split before Reference processing, rejected by P0.
    ContinuousImageRequiresPreSplitProcessing,
}

impl Default for ReferenceProgrammeScope {
    fn default() -> Self {
        Self::Singleton
    }
}

/// Resolved immutable reconstruction profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum ResolvedDsdProfile {
    /// Integrated rate response for 44.1 kHz.
    B1RateOnly,
    /// Integrated rate response for 48 kHz.
    B2RateOnly,
    /// 25–35 kHz profile.
    B3 {
        /// Flat passband edge.
        passband_hz: u32,
        /// Transition width.
        transition_hz: u32,
        /// SoX sinc \u{2212}6 dB center.
        center_hz: u32,
    },
    /// 30–45 kHz profile.
    B4 {
        /// Flat passband edge.
        passband_hz: u32,
        /// Transition width.
        transition_hz: u32,
        /// SoX sinc \u{2212}6 dB center.
        center_hz: u32,
    },
    /// 35–50 kHz DSD128 explicit Wideband profile.
    B4W {
        /// Flat passband edge.
        passband_hz: u32,
        /// Transition width.
        transition_hz: u32,
        /// SoX sinc \u{2212}6 dB center.
        center_hz: u32,
    },
    /// 48–70 kHz profile.
    B5 {
        /// Flat passband edge.
        passband_hz: u32,
        /// Transition width.
        transition_hz: u32,
        /// SoX sinc \u{2212}6 dB center.
        center_hz: u32,
    },
    /// 88.2–140 kHz profile, typed but rejected under v2, v3, and v4.
    B6 {
        /// Flat passband edge.
        passband_hz: u32,
        /// Transition width.
        transition_hz: u32,
        /// SoX sinc \u{2212}6 dB center.
        center_hz: u32,
    },
}

impl ResolvedDsdProfile {
    /// Stable profile key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::B1RateOnly => "b1",
            Self::B2RateOnly => "b2",
            Self::B3 { .. } => "b3",
            Self::B4 { .. } => "b4",
            Self::B4W { .. } => "b4w",
            Self::B5 { .. } => "b5",
            Self::B6 { .. } => "b6",
        }
    }

    /// Optional explicit sinc parameters `(transition_width, center_frequency)`.
    #[must_use]
    pub const fn sinc(self) -> Option<(u32, u32)> {
        match self {
            Self::B1RateOnly | Self::B2RateOnly => None,
            Self::B3 {
                transition_hz,
                center_hz,
                ..
            }
            | Self::B4 {
                transition_hz,
                center_hz,
                ..
            }
            | Self::B4W {
                transition_hz,
                center_hz,
                ..
            }
            | Self::B5 {
                transition_hz,
                center_hz,
                ..
            }
            | Self::B6 {
                transition_hz,
                center_hz,
                ..
            } => Some((transition_hz, center_hz)),
        }
    }


    /// Frozen flat-passband edge for explicit-sinc profiles.
    #[must_use]
    pub const fn passband_hz(self) -> Option<u32> {
        match self {
            Self::B1RateOnly | Self::B2RateOnly => None,
            Self::B3 { passband_hz, .. }
            | Self::B4 { passband_hz, .. }
            | Self::B4W { passband_hz, .. }
            | Self::B5 { passband_hz, .. }
            | Self::B6 { passband_hz, .. } => Some(passband_hz),
        }
    }

    /// Frozen stopband edge for explicit-sinc profiles.
    #[must_use]
    pub const fn stopband_hz(self) -> Option<u32> {
        match self {
            Self::B1RateOnly | Self::B2RateOnly => None,
            Self::B3 {
                passband_hz,
                transition_hz,
                ..
            }
            | Self::B4 {
                passband_hz,
                transition_hz,
                ..
            }
            | Self::B4W {
                passband_hz,
                transition_hz,
                ..
            }
            | Self::B5 {
                passband_hz,
                transition_hz,
                ..
            }
            | Self::B6 {
                passband_hz,
                transition_hz,
                ..
            } => passband_hz.checked_add(transition_hz),
        }
    }
}

/// Typed-but-disabled B6 profile retained by policies v2 and v3 for forward-compatible
/// diagnostics and qualification-artifact consistency checks.
#[must_use]
pub const fn typed_b6_profile() -> ResolvedDsdProfile {
    ResolvedDsdProfile::B6 {
        passband_hz: 88_200,
        transition_hz: 51_800,
        center_hz: 114_100,
    }
}

/// Reference terminal dither policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum ReferenceDither {
    /// No dither for floating point.
    None,
    /// Plain SoX TPDF for Int24.
    Tpdf,
    /// SoX Shibata for Int16.
    Shibata,
}

/// Fully resolved PCM terminal contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct FinalPcmContract {
    /// Target sample rate.
    pub sample_rate_hz: u32,
    /// Channel count.
    pub channels: u16,
    /// Integer or floating sample kind.
    pub sample_kind: SampleKind,
    /// Terminal PCM depth.
    pub bit_depth: PcmBitDepth,
    /// Locked Reference dither.
    pub dither: ReferenceDither,
}

/// Conservative additive terminal realization error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct TerminalRealizationBound {
    /// Upward-rounded Q1.63 maximum added full-scale peak.
    pub max_added_peak_fs_q63_ceil: u64,
    /// Downward-rounded safe pre-terminal true-peak ceiling.
    pub safe_pre_terminal_ceiling_dbtp: DbNano,
    /// Digest of derivation inputs and algorithm.
    pub derivation_digest: Sha256Digest,
}

/// Resolved gain authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum ResolvedGainPolicy {
    /// Requested Reference gain, allowed to reduce to the terminal-safe ceiling.
    ReferenceCompensated {
        /// Requested exact gain.
        requested_gain: DbNano,
        /// Reference post-final ceiling.
        ceiling: DbNano,
        /// Frozen terminal bound.
        terminal_bound: TerminalRealizationBound,
    },
    /// Exact native-level restoration.
    NativeLevelExact {
        /// Exact gain.
        gain: DbNano,
        /// Reference ceiling.
        ceiling: DbNano,
        /// Frozen terminal bound.
        terminal_bound: TerminalRealizationBound,
    },
    /// Exact user fixed gain plus headroom restoration.
    FixedExact {
        /// Exact gain.
        gain: DbNano,
        /// Reference ceiling.
        ceiling: DbNano,
        /// Frozen terminal bound.
        terminal_bound: TerminalRealizationBound,
    },
    /// Modified/unqualified SoX peak normalization.
    NormalizePeak {
        /// Literal SoX norm target.
        target_dbfs: DbNano,
    },
}

/// Canonical uncompressed DSD materialization contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct CanonicalDsdContract {
    /// DSD rate.
    pub rate: DsdRate,
    /// Mono/stereo channels.
    pub channels: u16,
}

/// Measurement identity local to one plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(transparent))]
pub struct MeasurementId(pub u32);

/// Measurement scope. P0 supports only one plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum MeasurementScope {
    /// Current singleton plan.
    Plan,
}

/// True-peak measurement purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum TruePeakPurpose {
    /// Pre-final gain authority.
    GainAuthority,
    /// Post-final acceptance/provenance.
    PostFinalAcceptance,
}

/// Strict true-peak value used by deferred binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum TruePeakValue {
    /// Conservative finite true-peak upper bound in dBTP.
    Finite(DbNano),
    /// The analyzer reported negative infinity and an independent scan proved
    /// every finite sample was signed zero.
    VerifiedSilence,
}

/// Parsed true-peak measurement and its conservative authority.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct TruePeakMeasurement {
    /// Plan-local identity.
    pub id: MeasurementId,
    /// Scope required by the consuming deferred expression.
    pub scope: MeasurementScope,
    /// Purpose required by the consuming operation.
    pub purpose: TruePeakPurpose,
    /// Exact analyzer JSON object retained for provenance.
    pub raw_json: String,
    /// Analyzer-reported input true peak, or verified silence.
    pub reported: TruePeakValue,
    /// Frozen one-sided textual/reporting quantization allowance.
    pub reporting_uncertainty: DbNano,
    /// Frozen one-sided analyzer residual allowance.
    pub analyzer_residual: DbNano,
    /// Conservative upper bound used for all finite arithmetic.
    pub conservative_upper: TruePeakValue,
}

/// Strict parser selected for a measurement command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum MeasurementParser {
    /// Historical direct-W64 FFmpeg loudnorm parser contract. Retained for append-only decoding only.
    FfmpegLoudnormInputTpV1,
    /// FFmpeg loudnorm final JSON over the exact f64 streamed-WAV analyzer carrier, using only `input_tp`.
    FfmpegLoudnormInputTpV2,
}

/// One planned measurement step.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct PlannedMeasurement {
    /// Unique measurement ID.
    pub id: MeasurementId,
    /// Singleton plan scope.
    pub scope: MeasurementScope,
    /// Gain or acceptance purpose.
    pub purpose: TruePeakPurpose,
    /// Optional typed producer whose stdout is connected directly to the analyzer stdin.
    /// Historical v1 measurements omit this field and read their path-backed carrier directly.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
    pub input_stage: Option<PlannedCommand>,
    /// Exact analyzer command.
    pub command: PlannedCommand,
    /// Strict parser.
    pub parser: MeasurementParser,
}

impl PlannedMeasurement {
    /// Return the durable path-backed carrier that the measurement observes.
    #[must_use]
    pub fn carrier_path(&self) -> Option<&Path> {
        self.input_stage
            .as_ref()
            .and_then(|stage| stage.input.as_path())
            .or_else(|| self.command.input.as_path())
    }
}

/// Deferred command argument.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum PlannedArg {
    /// Literal argv token.
    Literal(String),
    /// Gain resolved from one true-peak measurement and immutable policy.
    BoundGainDb {
        /// Measurement authority.
        true_peak: MeasurementId,
        /// Gain policy.
        policy: ResolvedGainPolicy,
    },
}

/// Command resolved only after a typed measurement exists.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct PlannedDeferredCommand {
    /// Built-in tool.
    pub tool: ToolIdentifier,
    /// Literal and bound argv tokens.
    pub args: Vec<PlannedArg>,
    /// Logical input.
    pub input: InputSource,
    /// Logical output.
    pub output: OutputSink,
    /// Environment inheritance policy.
    #[cfg_attr(feature = "serde", serde(default))]
    pub environment_policy: CommandEnvironmentPolicy,
    /// Stable environment.
    pub environment: BTreeMap<String, String>,
    /// User-facing description.
    pub description: String,
}

/// High-level P0 operation summary used by provenance and fingerprints.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum DsdReferenceOperation {
    /// Decode/extract to canonical uncompressed DSD.
    DsdLosslessDecodeMaterialize {
        /// Selected front-end.
        front_end: DsdInputFrontEnd,
        /// Output contract.
        output_contract: CanonicalDsdContract,
    },
    /// SoX Reference reconstruction render.
    DsdReferenceRender {
        /// Target rate.
        target_rate_hz: u32,
        /// Resolved profile.
        profile: ResolvedDsdProfile,
        /// Policy.
        policy: DsdReferencePolicyVersion,
    },
    /// True-peak measurement.
    MeasureTruePeak {
        /// Measurement identity.
        measurement_id: MeasurementId,
        /// Plan scope.
        scope: MeasurementScope,
        /// Purpose.
        purpose: TruePeakPurpose,
    },
    /// Terminal realization.
    DsdReferenceFinalize {
        /// Final PCM contract.
        sample_contract: FinalPcmContract,
        /// Gain authority.
        gain_policy: ResolvedGainPolicy,
        /// Pre-final measurement.
        pre_final_measurement: MeasurementId,
    },
    /// Lossless packaging.
    PackageLossless {
        /// Exact target.
        target: ResolvedOutputTarget,
        /// Final PCM contract.
        sample_contract: FinalPcmContract,
    },
}

/// Pure Reference plan facts retained alongside executable steps.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct DsdReferencePlanSummary {
    /// Immutable policy ID.
    pub policy: DsdReferencePolicyVersion,
    /// Qualification artifact digest.
    pub qualification_manifest_digest: Sha256Digest,
    /// Exact product target.
    pub target: ResolvedOutputTarget,
    /// Resolved profile.
    pub profile: ResolvedDsdProfile,
    /// Input front-end.
    pub front_end: DsdInputFrontEnd,
    /// Final PCM contract.
    pub final_pcm: FinalPcmContract,
    /// Gain authority.
    pub gain_policy: ResolvedGainPolicy,
    /// Canonical byte-affecting package compression level, when applicable.
    pub package_compression_level: Option<u8>,
    /// Planner-owned 64-bit floating reconstruction carrier.
    pub r64_path: PathBuf,
    /// Planner-owned one-and-only terminal PCM carrier.
    pub qpcm_path: PathBuf,
    /// Planner-owned staged lossless package; equal to `qpcm_path` for W64.
    pub packaged_path: PathBuf,
    /// Semantic plan hash with path roles normalized.
    pub semantic_plan_hash_v1: Sha256Digest,
    /// Ordered operation summaries.
    pub operations: Vec<DsdReferenceOperation>,
}

/// Stable P0 error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceErrorCode {
    /// Manual unavailable.
    ManualUnavailable,
    /// Lossy delivery unavailable.
    LossyUnavailable,
    /// Unsupported DSD rate.
    UnsupportedDsdRate,
    /// Unknown encoding.
    UnknownEncoding,
    /// Unsupported channels.
    UnsupportedChannels,
    /// DSD128/256\u{2192}88.2 missing target-limited policy.
    Target882,
    /// DSD128/256\u{2192}96 missing direct qualification.
    Target96,
    /// No Wideband profile exists for DSD64.
    WidebandDsd64,
    /// DSD128 Wideband target is below 176.4 kHz.
    WidebandDsd128Target,
    /// DSD256 B6 stopband cannot fit this target.
    WidebandDsd256Target,
    /// B6 unavailable.
    B6Unavailable,
    /// Unsupported 8-bit terminal depth.
    TerminalInt8,
    /// Unsupported 32-bit integer terminal depth.
    TerminalInt32,
    /// Target/depth mismatch.
    TargetDepth,
    /// Independent batch rejected.
    SingletonBatch,
    /// Continuous programme rejected.
    ContinuousProgramme,
    /// DST/SACD front-end unattested.
    FrontEndUnattested,
    /// Toolchain mismatch.
    Toolchain,
    /// Exact gain unsafe.
    UnsafeExactGain,
    /// Unsupported target sample rate.
    UnsupportedTargetRate,
    /// RIFF size overflow.
    RiffSize,
    /// Canonical target mismatch.
    CanonicalTarget,
    /// Predictive compressed DST lacks an independent oracle for this rate/channel cell.
    CompressedDstRateUnqualified,
    /// Int16 Shibata has no defensible implementation-specific peak bound.
    Int16TerminalUnqualified,
    /// SACD extraction/decode lacks production-path release qualification.
    SacdFrontEndIntegrationUnqualified,
    /// Managed destination authority mismatch.
    ManagedDestination,
}

/// Stable exact message for one P0 Reference failure.
#[must_use]
pub fn reference_error_text(code: ReferenceErrorCode) -> &'static str {
    match code {
        ReferenceErrorCode::ManualUnavailable => "DSD-REF-P0-001: Manual DSD workflows are not available in this P0 build. Use Reference with a supported lossless target, or wait for Manual workflow support.",
        ReferenceErrorCode::LossyUnavailable => "DSD-REF-P0-002: Reference DSD reconstruction currently supports lossless delivery only. Choose FLAC, RIFF/WAV, RF64, W64, AIFF, WavPack, or ALAC/M4A, or wait for Reference-front-end Opus/MP3/AAC delivery.",
        ReferenceErrorCode::UnsupportedDsdRate => "DSD-REF-P0-003: Reference policy sox_ng_14_8_0_1_v4 supports DSD64, DSD128, and DSD256 only. Use a supported-rate source or wait for expanded-rate/Manual support.",
        ReferenceErrorCode::UnknownEncoding => "DSD-REF-P0-004: The DSD container or compression mode could not be identified as DSF/DSD, DSDIFF/DSD, DSDIFF/DST, or a supported SACD area. Reference will not guess the decoder path.",
        ReferenceErrorCode::UnsupportedChannels => "DSD-REF-P0-005: Reference policy sox_ng_14_8_0_1_v4 supports qualified mono and stereo cells only. Select a mono/stereo track or wait for multichannel qualification.",
        ReferenceErrorCode::Target882 => "DSD-REF-P0-006: Reference policy sox_ng_14_8_0_1_v4 has no qualified target-limited profile for {DSD128|DSD256} \u{2192} 88.2 kHz. Choose 44.1/48 kHz, choose 176.4 kHz or higher, or wait for a new policy.",
        ReferenceErrorCode::Target96 => "DSD-REF-P0-007: Reference policy sox_ng_14_8_0_1_v4 has no direct 96 kHz qualification for {DSD128|DSD256}. Choose 48 kHz, choose 176.4 kHz or higher, or wait for a new policy.",
        ReferenceErrorCode::WidebandDsd64 => "DSD-REF-P0-008: No Wideband profile is defined for DSD64. Select the Reference profile.",
        ReferenceErrorCode::WidebandDsd128Target => "DSD-REF-P0-008: DSD128 Wideband uses B4W and requires a target rate of at least 176.4 kHz. Select the Reference profile or choose 176.4 kHz or higher.",
        ReferenceErrorCode::WidebandDsd256Target => "DSD-REF-P0-008: DSD256 Wideband uses B6, whose 140 kHz stopband edge cannot fit this target; B6 is also unavailable under policy sox_ng_14_8_0_1_v4. Select Reference/B5.",
        ReferenceErrorCode::B6Unavailable => "DSD-REF-P0-009: B6 is represented but unqualified and unavailable under policy sox_ng_14_8_0_1_v4. Select Reference/B5 or wait for a later immutable policy.",
        ReferenceErrorCode::TerminalInt8 => "DSD-REF-P0-010: Reference policy sox_ng_14_8_0_1_v4 has no qualified 8-bit terminal realization. Choose 24-bit, Float32, or Float64 where supported.",
        ReferenceErrorCode::TerminalInt32 => "DSD-REF-P0-010: Reference policy sox_ng_14_8_0_1_v4 has no qualified 32-bit integer terminal realization. Choose 24-bit, Float32, or Float64 where supported.",
        ReferenceErrorCode::TargetDepth => "DSD-REF-P0-011: {target} does not support {depth} under Reference policy sox_ng_14_8_0_1_v4. Choose a target/depth pair listed by the policy.",
        ReferenceErrorCode::SingletonBatch => "DSD-REF-P0-012: Reference P0 supports singleton conversions only. Convert the selected files one at a time as independent singletons with independent gain, or wait for programme-wide Reference support.",
        ReferenceErrorCode::ContinuousProgramme => "DSD-REF-P0-013: Reference P0 cannot split a continuous DSD programme before reconstruction. This source must be processed as one programme before splitting; wait for programme-wide Reference support. Already independent files may be converted one at a time with independent gain.",
        ReferenceErrorCode::FrontEndUnattested => "DSD-REF-P0-014: Reference requires the qualified DST/SACD decode front-end for this source, but the decoder/extractor identity or qualification manifest does not match. Install the qualified toolchain or use an uncompressed DSF/DSDIFF source.",
        ReferenceErrorCode::Toolchain => "DSD-REF-P0-015: The installed Reference toolchain does not match policy sox_ng_14_8_0_1_v4 or failed its behavior probes. Activate/install the qualified toolchain; tonepoet will not substitute another decoder, analyzer, resampler, or encoder.",
        ReferenceErrorCode::UnsafeExactGain => "DSD-REF-P0-016: The requested {native-level|fixed} gain cannot satisfy the Reference \u{2212}1.000000000 dBTP ceiling for this measured source and terminal format. Reduce the fixed gain, choose Reference gain, or choose NormalizePeak with its modified/unqualified semantics.",
        ReferenceErrorCode::UnsupportedTargetRate => "DSD-REF-P0-017: Reference policy sox_ng_14_8_0_1_v4 supports target sample rates 44.1, 48, 88.2, 96, 176.4, 192, 352.8, 384, 705.6, and 768 kHz only. Choose one of those rates or wait for a later immutable policy.",
        ReferenceErrorCode::RiffSize => "DSD-REF-P0-018: The predicted RIFF/WAV output exceeds the qualified RIFF size limit. Choose RF64, W64, or another supported lossless target.",
        ReferenceErrorCode::CanonicalTarget => "DSD-REF-P0-019: The selected output container does not match the canonical Reference target or contains unrecognized output flags. Re-select the target.",
        ReferenceErrorCode::CompressedDstRateUnqualified => "DSD-REF-P0-021: Reference policy sox_ng_14_8_0_1_v4 qualifies predictive compressed DST only for stereo DSD64. Mono DSD64 and all DSD128/DSD256 predictive-DST cells remain unavailable because no matching independent-oracle corpus is present. Use an uncompressed DSF/DSDIFF source, decode with an independently verified tool outside Reference, or wait for a later immutable policy.",
        ReferenceErrorCode::Int16TerminalUnqualified => "DSD-REF-P0-022: Reference policy sox_ng_14_8_0_1_v4 does not enable Int16 because the commissioned SoX-ng Shibata realization has no qualified conservative worst-case peak bound. Choose Int24, Float32, or Float64, or wait for a later immutable policy with a derived Shibata bound.",
        ReferenceErrorCode::SacdFrontEndIntegrationUnqualified => "DSD-REF-P0-023: Reference policy sox_ng_14_8_0_1_v4 does not enable SACD DSD or DST extraction because the production extraction/materialization path is not yet qualified by pinned end-to-end SACD fixtures. Extract to a qualified DSF/DSDIFF source first or wait for a later immutable policy.",
        ReferenceErrorCode::ManagedDestination => "DSD-REF-P0-020: The destination album has incompatible or incomplete tonepoet manifest authority. Choose a different output directory, repair/recover the existing transaction, or reconvert the album under one compatible Reference route; tonepoet will not merge or replace authority implicitly.",
    }
}

fn invalid_reference(field: &'static str, code: ReferenceErrorCode) -> PlanningError {
    PlanningError::invalid_settings(field, reference_error_text(code))
}

fn source_rate_name(rate: DsdRate) -> &'static str {
    match rate {
        DsdRate::Dsd64 => "DSD64",
        DsdRate::Dsd128 => "DSD128",
        DsdRate::Dsd256 => "DSD256",
        DsdRate::Dsd512 => "DSD512",
        DsdRate::Dsd1024 => "DSD1024",
    }
}

fn invalid_target_profile(
    field: &'static str,
    code: ReferenceErrorCode,
    source_rate: DsdRate,
) -> PlanningError {
    let source = source_rate_name(source_rate);
    let reason = match code {
        ReferenceErrorCode::Target882 => format!(
            "DSD-REF-P0-006: Reference policy sox_ng_14_8_0_1_v4 has no qualified target-limited profile for {source} \u{2192} 88.2 kHz. Choose 44.1/48 kHz, choose 176.4 kHz or higher, or wait for a new policy."
        ),
        ReferenceErrorCode::Target96 => format!(
            "DSD-REF-P0-007: Reference policy sox_ng_14_8_0_1_v4 has no direct 96 kHz qualification for {source}. Choose 48 kHz, choose 176.4 kHz or higher, or wait for a new policy."
        ),
        _ => return invalid_reference(field, code),
    };
    PlanningError::invalid_settings(field, reason)
}

fn invalid_target_depth(
    field: &'static str,
    target: ResolvedOutputTarget,
    depth: PcmBitDepth,
) -> PlanningError {
    PlanningError::invalid_settings(
        field,
        format!(
            "DSD-REF-P0-011: {} does not support {depth:?} under Reference policy sox_ng_14_8_0_1_v4. Choose a target/depth pair listed by the policy.",
            target.key()
        ),
    )
}

fn invalid_exact_gain(field: &'static str, policy: ResolvedGainPolicy) -> PlanningError {
    let mode = match policy {
        ResolvedGainPolicy::NativeLevelExact { .. } => "native-level",
        ResolvedGainPolicy::FixedExact { .. } => "fixed",
        ResolvedGainPolicy::ReferenceCompensated { .. }
        | ResolvedGainPolicy::NormalizePeak { .. } => {
            return invalid_reference(field, ReferenceErrorCode::UnsafeExactGain);
        }
    };
    PlanningError::invalid_settings(
        field,
        format!(
            "DSD-REF-P0-016: The requested {mode} gain cannot satisfy the Reference \u{2212}1.000000000 dBTP ceiling for this measured source and terminal format. Reduce the fixed gain, choose Reference gain, or choose NormalizePeak with its modified/unqualified semantics."
        ),
    )
}

fn invalid_terminal_depth(field: &'static str, depth: PcmBitDepth) -> PlanningError {
    let code = match depth {
        PcmBitDepth::Int8 => ReferenceErrorCode::TerminalInt8,
        PcmBitDepth::Int32 => ReferenceErrorCode::TerminalInt32,
        PcmBitDepth::Int16 => ReferenceErrorCode::Int16TerminalUnqualified,
        PcmBitDepth::Int24 | PcmBitDepth::Float32 | PcmBitDepth::Float64 => {
            ReferenceErrorCode::TargetDepth
        }
    };
    invalid_reference(field, code)
}

/// Resolve a Reference target sample rate, including the DSD Source sentinel.
pub fn resolve_reference_target_rate(source_rate: DsdRate, target: RateTarget) -> Result<u32> {
    if matches!(source_rate, DsdRate::Dsd512 | DsdRate::Dsd1024) {
        return Err(invalid_reference("source.sample_rate_hz", ReferenceErrorCode::UnsupportedDsdRate));
    }
    let rate = match target {
        RateTarget::Source => match source_rate {
            DsdRate::Dsd64 => 88_200,
            DsdRate::Dsd128 => 176_400,
            DsdRate::Dsd256 => 352_800,
            DsdRate::Dsd512 | DsdRate::Dsd1024 => unreachable!("guarded above"),
        },
        RateTarget::PcmHz(rate) => rate,
        RateTarget::Dsd(_) => {
            return Err(invalid_reference(
                "target_sample_rate",
                ReferenceErrorCode::UnsupportedTargetRate,
            ));
        }
    };
    if !matches!(
        rate,
        44_100 | 48_000 | 88_200 | 96_000 | 176_400 | 192_000 | 352_800 | 384_000 | 705_600 | 768_000
    ) {
        return Err(invalid_reference(
            "target_sample_rate",
            ReferenceErrorCode::UnsupportedTargetRate,
        ));
    }
    Ok(rate)
}

/// Resolve the immutable profile matrix or return its exact P0 error.
pub fn resolve_reference_profile(
    source_rate: DsdRate,
    target_rate_hz: u32,
    selection: DsdReconstructionSelection,
) -> Result<ResolvedDsdProfile> {
    use DsdReconstructionSelection::{Reference, Wideband};
    use DsdRate::{Dsd1024, Dsd128, Dsd256, Dsd512, Dsd64};
    if matches!(source_rate, Dsd512 | Dsd1024) {
        return Err(invalid_reference("source.sample_rate_hz", ReferenceErrorCode::UnsupportedDsdRate));
    }
    match selection {
        Reference => match (source_rate, target_rate_hz) {
            (_, 44_100) => Ok(ResolvedDsdProfile::B1RateOnly),
            (_, 48_000) => Ok(ResolvedDsdProfile::B2RateOnly),
            (Dsd64, _) => Ok(ResolvedDsdProfile::B3 {
                passband_hz: 25_000,
                transition_hz: 10_000,
                center_hz: 30_000,
            }),
            (Dsd128 | Dsd256, 88_200) => Err(invalid_target_profile(
                "dsd.from_dsd.profile",
                ReferenceErrorCode::Target882,
                source_rate,
            )),
            (Dsd128 | Dsd256, 96_000) => Err(invalid_target_profile(
                "dsd.from_dsd.profile",
                ReferenceErrorCode::Target96,
                source_rate,
            )),
            (Dsd128, _) => Ok(ResolvedDsdProfile::B4 {
                passband_hz: 30_000,
                transition_hz: 15_000,
                center_hz: 37_500,
            }),
            (Dsd256, _) => Ok(ResolvedDsdProfile::B5 {
                passband_hz: 48_000,
                transition_hz: 22_000,
                center_hz: 59_000,
            }),
            (Dsd512 | Dsd1024, _) => unreachable!("guarded above"),
        },
        Wideband => match source_rate {
            Dsd64 => Err(invalid_reference(
                "dsd.from_dsd.profile",
                ReferenceErrorCode::WidebandDsd64,
            )),
            Dsd128 if target_rate_hz >= 176_400 => Ok(ResolvedDsdProfile::B4W {
                passband_hz: 35_000,
                transition_hz: 15_000,
                center_hz: 42_500,
            }),
            Dsd128 => Err(invalid_reference(
                "dsd.from_dsd.profile",
                ReferenceErrorCode::WidebandDsd128Target,
            )),
            Dsd256 if target_rate_hz < 352_800 => Err(invalid_reference(
                "dsd.from_dsd.profile",
                ReferenceErrorCode::WidebandDsd256Target,
            )),
            Dsd256 => Err(invalid_reference(
                "dsd.from_dsd.profile",
                ReferenceErrorCode::B6Unavailable,
            )),
            Dsd512 | Dsd1024 => Err(invalid_reference(
                "source.sample_rate_hz",
                ReferenceErrorCode::UnsupportedDsdRate,
            )),
        },
    }
}

/// Resolve the target PCM depth for Reference.
pub fn resolve_reference_depth(target: BitDepthTarget) -> Result<PcmBitDepth> {
    let depth = match target {
        BitDepthTarget::Source => PcmBitDepth::Int24,
        BitDepthTarget::Pcm(depth) => depth,
    };
    match depth {
        PcmBitDepth::Int8 => {
            return Err(invalid_reference(
                "target_bit_depth",
                ReferenceErrorCode::TerminalInt8,
            ));
        }
        PcmBitDepth::Int32 => {
            return Err(invalid_reference(
                "target_bit_depth",
                ReferenceErrorCode::TerminalInt32,
            ));
        }
        PcmBitDepth::Int16 => {
            return Err(invalid_reference(
                "target_bit_depth",
                ReferenceErrorCode::Int16TerminalUnqualified,
            ));
        }
        PcmBitDepth::Int24 | PcmBitDepth::Float32 | PcmBitDepth::Float64 => {}
    }
    Ok(depth)
}

/// Check the frozen target/depth matrix.
pub fn validate_reference_target_depth(
    target: ResolvedOutputTarget,
    depth: PcmBitDepth,
) -> Result<()> {
    if depth == PcmBitDepth::Int16 {
        return Err(invalid_reference(
            "target_bit_depth",
            ReferenceErrorCode::Int16TerminalUnqualified,
        ));
    }
    let supported = match target {
        ResolvedOutputTarget::WavW64
        | ResolvedOutputTarget::WavRiff
        | ResolvedOutputTarget::WavRf64 => matches!(
            depth,
            PcmBitDepth::Int24 | PcmBitDepth::Float32 | PcmBitDepth::Float64
        ),
        ResolvedOutputTarget::FlacNative
        | ResolvedOutputTarget::AiffNative
        | ResolvedOutputTarget::WavPackNative
        | ResolvedOutputTarget::AlacM4a => depth == PcmBitDepth::Int24,
        _ => false,
    };
    if supported {
        Ok(())
    } else {
        Err(invalid_target_depth("target_bit_depth", target, depth))
    }
}

/// Resolve the front-end from immutable source facts.
pub fn resolve_reference_front_end(kind: &DsdSourceKind) -> Result<DsdInputFrontEnd> {
    match kind {
        DsdSourceKind::DsfUncompressed | DsdSourceKind::DsdiffUncompressed => {
            Ok(DsdInputFrontEnd::NativeUncompressed)
        }
        DsdSourceKind::DsdiffDst => Ok(DsdInputFrontEnd::DsdiffDst {
            decoder: QualifiedDstDecoderVersion::SacdRsP0V1,
        }),
        DsdSourceKind::SacdTrack { .. } => Err(invalid_reference(
            "source.dsd_source_kind",
            ReferenceErrorCode::SacdFrontEndIntegrationUnqualified,
        )),
        DsdSourceKind::UnknownDsdContainer => Err(invalid_reference(
            "source.dsd_source_kind",
            ReferenceErrorCode::UnknownEncoding,
        )),
    }
}

/// Frozen terminal bound for one exact P0 target-rate/depth/realization cell.
///
/// The numerical bound is currently rate-invariant, but the derivation identity is
/// deliberately rate-specific so a later qualification change cannot silently
/// widen an already-persisted cell.
#[must_use]
pub fn terminal_realization_bound(
    target_rate_hz: u32,
    depth: PcmBitDepth,
) -> TerminalRealizationBound {
    let (q63, safe, realization) = match depth {
        PcmBitDepth::Int16 => (
            u64::MAX,
            i64::MIN,
            "int16-shibata-unqualified-no-conservative-bound",
        ),
        PcmBitDepth::Int24 => (2_199_023_255_552, -1_000_002_324, "int24-tpdf-2lsb"),
        PcmBitDepth::Float32 => (1_099_511_627_776, -1_000_001_162, "float32-2^-23"),
        PcmBitDepth::Float64 => (4_096, -1_000_000_001, "float64-2^-51"),
        PcmBitDepth::Int8 | PcmBitDepth::Int32 => (u64::MAX, i64::MIN, "unsupported"),
    };
    let derivation = format!(
        "tonepoet-reference-terminal-bound/v2\0policy={}\0rate={}\0depth={:?}\0realization={}\0q63={}\0safe_dbnano={}",
        DsdReferencePolicyVersion::SoxNg14801V4.key(),
        target_rate_hz,
        depth,
        realization,
        q63,
        safe,
    );
    TerminalRealizationBound {
        max_added_peak_fs_q63_ceil: q63,
        safe_pre_terminal_ceiling_dbtp: DbNano(safe),
        derivation_digest: Sha256Digest::of_bytes(derivation.as_bytes()),
    }
}

/// Validate and resolve one gain policy.
pub fn resolve_gain_policy(
    settings: DsdSourceSettings,
    target_rate_hz: u32,
    depth: PcmBitDepth,
) -> Result<ResolvedGainPolicy> {
    let bound = terminal_realization_bound(target_rate_hz, depth);
    match settings.gain_mode {
        DsdSourceGainMode::Reference => {
            if settings.fixed_gain_db.is_some() {
                return Err(PlanningError::invalid_settings(
                    "dsd.from_dsd.fixed_gain_db",
                    "fixed gain is valid only when dsd gain mode is fixed",
                ));
            }
            let requested_gain = DbNano::HEADROOM_RESTORATION
                .checked_add(DbNano::DSD_COMPENSATION)
                .ok_or_else(|| PlanningError::invalid_settings("dsd gain", "gain overflow"))?;
            Ok(ResolvedGainPolicy::ReferenceCompensated {
                requested_gain,
                ceiling: DbNano::REFERENCE_CEILING,
                terminal_bound: bound,
            })
        }
        DsdSourceGainMode::NativeLevel => {
            if settings.fixed_gain_db.is_some() {
                return Err(PlanningError::invalid_settings(
                    "dsd.from_dsd.fixed_gain_db",
                    "fixed gain is valid only when dsd gain mode is fixed",
                ));
            }
            Ok(ResolvedGainPolicy::NativeLevelExact {
                gain: DbNano::HEADROOM_RESTORATION,
                ceiling: DbNano::REFERENCE_CEILING,
                terminal_bound: bound,
            })
        }
        DsdSourceGainMode::Fixed => {
            let fixed = settings.fixed_gain_db.ok_or_else(|| {
                PlanningError::invalid_settings(
                    "dsd.from_dsd.fixed_gain_db",
                    "fixed gain mode requires a fixed gain value",
                )
            })?;
            if !(DbNano::MIN_FIXED_GAIN..=DbNano::MAX_FIXED_GAIN).contains(&fixed) {
                return Err(PlanningError::invalid_settings(
                    "dsd.from_dsd.fixed_gain_db",
                    "fixed gain must be between -24.000000000 and +24.000000000 dB",
                ));
            }
            let gain = DbNano::HEADROOM_RESTORATION
                .checked_add(fixed)
                .ok_or_else(|| PlanningError::invalid_settings("dsd gain", "gain overflow"))?;
            Ok(ResolvedGainPolicy::FixedExact {
                gain,
                ceiling: DbNano::REFERENCE_CEILING,
                terminal_bound: bound,
            })
        }
        DsdSourceGainMode::NormalizePeak => {
            if settings.fixed_gain_db.is_some() {
                return Err(PlanningError::invalid_settings(
                    "dsd.from_dsd.fixed_gain_db",
                    "fixed gain is invalid when dsd gain mode is normalize",
                ));
            }
            let target = settings.normalize_peak_target_dbfs;
            if !(DbNano::MIN_NORMALIZE_TARGET..=DbNano::MAX_NORMALIZE_TARGET).contains(&target) {
                return Err(PlanningError::invalid_settings(
                    "dsd.from_dsd.normalize_peak_target_dbfs",
                    "normalize target must be between -12.000000000 and 0.000000000 dBFS",
                ));
            }
            Ok(ResolvedGainPolicy::NormalizePeak { target_dbfs: target })
        }
    }
}

/// Extract exactly one final loudnorm JSON report carrying `input_tp`.
pub fn extract_single_loudnorm_report(stderr: &str) -> std::result::Result<String, String> {
    let mut reports = Vec::new();
    let bytes = stderr.as_bytes();
    let mut depth = 0_u32;
    let mut start = None;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' if depth > 0 => in_string = true,
            b'{' => {
                if depth == 0 {
                    start = Some(index);
                }
                depth = depth
                    .checked_add(1)
                    .ok_or_else(|| "Reference loudnorm JSON nesting overflow".to_string())?;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let begin = start.take().ok_or_else(|| {
                        "Reference loudnorm JSON boundary is malformed".to_string()
                    })?;
                    let candidate = &stderr[begin..=index];
                    if candidate.contains("\"input_tp\"") {
                        reports.push(candidate.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    if depth != 0 || in_string {
        return Err("Reference loudnorm JSON report is truncated".to_string());
    }
    match reports.len() {
        1 => Ok(reports.remove(0)),
        0 => Err("Reference loudnorm output did not contain one input_tp report".to_string()),
        _ => Err("Reference loudnorm output contained duplicate input_tp reports".to_string()),
    }
}

/// Parse the strict loudnorm report and construct the conservative measurement
/// authority used by both production execution and release qualification.
pub fn parse_reference_true_peak_measurement(
    id: MeasurementId,
    scope: MeasurementScope,
    purpose: TruePeakPurpose,
    raw_json: String,
    reporting_uncertainty: DbNano,
    analyzer_residual: DbNano,
    verified_silence: bool,
) -> std::result::Result<TruePeakMeasurement, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)] // fields exist so deny_unknown_fields validates the exact grammar
    struct StrictLoudnormReport {
        input_i: String,
        input_tp: String,
        input_lra: String,
        input_thresh: String,
        output_i: String,
        output_tp: String,
        output_lra: String,
        output_thresh: String,
        normalization_type: String,
        target_offset: String,
    }

    let report: StrictLoudnormReport = serde_json::from_str(&raw_json)
        .map_err(|err| format!("invalid Reference loudnorm JSON: {err}"))?;
    let StrictLoudnormReport {
        input_i: _,
        input_tp,
        input_lra: _,
        input_thresh: _,
        output_i: _,
        output_tp: _,
        output_lra: _,
        output_thresh: _,
        normalization_type: _,
        target_offset: _,
    } = report;
    let reported = if input_tp == "-inf" {
        if !verified_silence {
            return Err(
                "Reference loudnorm reported -inf without an independent signed-zero proof"
                    .to_string(),
            );
        }
        TruePeakValue::VerifiedSilence
    } else {
        if input_tp.contains(',')
            || input_tp.contains('e')
            || input_tp.contains('E')
            || input_tp.starts_with('+')
            || input_tp == "inf"
            || input_tp == "+inf"
            || input_tp.eq_ignore_ascii_case("nan")
        {
            return Err("Reference input_tp uses unsupported numeric syntax".to_string());
        }
        let value = input_tp
            .parse::<DbNano>()
            .map_err(|err| format!("invalid Reference input_tp: {err}"))?;
        if !(DbNano(-1_000_000_000_000)..=DbNano(100_000_000_000)).contains(&value) {
            return Err("Reference input_tp is outside -1000 to +100 dBTP".to_string());
        }
        TruePeakValue::Finite(value)
    };
    let conservative_upper = match reported {
        TruePeakValue::VerifiedSilence => TruePeakValue::VerifiedSilence,
        TruePeakValue::Finite(value) => TruePeakValue::Finite(
            value
                .checked_add(reporting_uncertainty)
                .and_then(|value| value.checked_add(analyzer_residual))
                .ok_or_else(|| {
                    "Reference true-peak uncertainty arithmetic overflow".to_string()
                })?,
        ),
    };
    Ok(TruePeakMeasurement {
        id,
        scope,
        purpose,
        raw_json,
        reported,
        reporting_uncertainty,
        analyzer_residual,
        conservative_upper,
    })
}

/// Build the exact independent signed-zero scan command used after a loudnorm `-inf` report.
#[must_use]
pub fn build_reference_silence_scan_command(input: &Path, output: &Path) -> PlannedCommand {
    let mut command = PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        vec![
            "-y".to_string(),
            "-nostdin".to_string(),
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-i".to_string(),
            input.display().to_string(),
            "-map".to_string(),
            "0:a:0".to_string(),
            "-f".to_string(),
            "f64le".to_string(),
            "-acodec".to_string(),
            "pcm_f64le".to_string(),
            output.display().to_string(),
        ],
        InputSource::Path(input.to_path_buf()),
        OutputSink::Path(output.to_path_buf()),
        None,
        "Verify Reference signed-zero silence",
    );
    command.environment_policy = CommandEnvironmentPolicy::ClearAndSet;
    command.environment.insert("LC_ALL".to_string(), "C".to_string());
    command
}

/// Validate a decoded little-endian f64 stream as finite signed zero only.
pub fn validate_signed_zero_f64le(bytes: &[u8]) -> std::result::Result<(), String> {
    if bytes.is_empty() || bytes.len() % 8 != 0 {
        return Err(
            "Reference silence scan produced an empty or truncated f64 stream".to_string(),
        );
    }
    for chunk in bytes.chunks_exact(8) {
        let array: [u8; 8] = chunk
            .try_into()
            .map_err(|_| "invalid silence scan sample width".to_string())?;
        let bits = u64::from_le_bytes(array);
        if bits != 0 && bits != (1_u64 << 63) {
            return Err(
                "loudnorm reported -inf but the independent scan found a non-zero or non-finite sample"
                    .to_string(),
            );
        }
    }
    Ok(())
}

/// Resolve one planned deferred command from typed production measurements.
pub fn resolve_reference_deferred_command(
    deferred: &PlannedDeferredCommand,
    measurements: &BTreeMap<MeasurementId, TruePeakMeasurement>,
) -> std::result::Result<PlannedCommand, String> {
    let mut args = Vec::with_capacity(deferred.args.len());
    for arg in &deferred.args {
        match arg {
            PlannedArg::Literal(value) => args.push(value.clone()),
            PlannedArg::BoundGainDb { true_peak, policy } => {
                let measurement = measurements.get(true_peak).ok_or_else(|| {
                    format!(
                        "missing Reference measurement id {} for deferred gain",
                        true_peak.0
                    )
                })?;
                if measurement.scope != MeasurementScope::Plan
                    || measurement.purpose != TruePeakPurpose::GainAuthority
                {
                    return Err(format!(
                        "Reference measurement id {} has the wrong scope or purpose",
                        true_peak.0
                    ));
                }
                let gain = resolve_bound_gain(measurement.conservative_upper, *policy)
                    .map_err(|err| format!("Reference gain binding failed: {err}"))?;
                args.push(gain.render(true));
            }
        }
    }
    let mut command = PlannedCommand::new(
        deferred.tool.clone(),
        args,
        deferred.input.clone(),
        deferred.output.clone(),
        None,
        deferred.description.clone(),
    );
    command.environment_policy = deferred.environment_policy;
    command.environment = deferred.environment.clone();
    Ok(command)
}

/// Resolve a deferred terminal gain from a conservative true-peak authority.
pub fn resolve_bound_gain(
    value: TruePeakValue,
    policy: ResolvedGainPolicy,
) -> Result<DbNano> {
    let requested = match policy {
        ResolvedGainPolicy::ReferenceCompensated { requested_gain, .. } => requested_gain,
        ResolvedGainPolicy::NativeLevelExact { gain, .. }
        | ResolvedGainPolicy::FixedExact { gain, .. } => gain,
        ResolvedGainPolicy::NormalizePeak { .. } => {
            return Err(PlanningError::invalid_settings(
                "dsd.reference.bound_gain",
                "NormalizePeak uses a literal norm target and may not be bound as a gain expression",
            ));
        }
    };

    let (ceiling, bound, may_reduce) = match policy {
        ResolvedGainPolicy::ReferenceCompensated {
            ceiling,
            terminal_bound,
            ..
        } => (ceiling, terminal_bound, true),
        ResolvedGainPolicy::NativeLevelExact {
            ceiling,
            terminal_bound,
            ..
        }
        | ResolvedGainPolicy::FixedExact {
            ceiling,
            terminal_bound,
            ..
        } => (ceiling, terminal_bound, false),
        ResolvedGainPolicy::NormalizePeak { .. } => unreachable!("guarded above"),
    };
    if bound.safe_pre_terminal_ceiling_dbtp > ceiling {
        return Err(PlanningError::invalid_settings(
            "dsd.reference.terminal_bound",
            "qualified terminal bound exceeds the Reference ceiling",
        ));
    }

    match value {
        TruePeakValue::VerifiedSilence => Ok(requested),
        TruePeakValue::Finite(true_peak_upper) => {
            let maximum_safe = bound
                .safe_pre_terminal_ceiling_dbtp
                .checked_sub(true_peak_upper)
                .ok_or_else(|| PlanningError::invalid_settings(
                    "dsd.reference.true_peak",
                    "true-peak safety arithmetic overflow",
                ))?;
            if requested <= maximum_safe {
                Ok(requested)
            } else if may_reduce {
                Ok(maximum_safe)
            } else {
                Err(invalid_exact_gain("dsd.from_dsd.gain_mode", policy))
            }
        }
    }
}

/// Validate the post-terminal true peak for qualified gain modes.
pub fn validate_post_final_true_peak(
    value: TruePeakValue,
    policy: ResolvedGainPolicy,
) -> Result<()> {
    if matches!(policy, ResolvedGainPolicy::NormalizePeak { .. }) {
        return Ok(());
    }
    match value {
        TruePeakValue::VerifiedSilence => Ok(()),
        TruePeakValue::Finite(upper) if upper <= DbNano::REFERENCE_CEILING => Ok(()),
        TruePeakValue::Finite(_) => Err(PlanningError::invalid_settings(
            "dsd.reference.post_final_true_peak",
            "post-final true peak exceeds the Reference -1.000000000 dBTP ceiling",
        )),
    }
}

/// Largest total byte size admitted for ordinary disk-backed RIFF under policy v4.
pub const REFERENCE_RIFF_MAX_FILE_BYTES: u64 = u32::MAX as u64;
/// Conservative upper bound for policy-owned FFmpeg RIFF structure/chunks.
pub const REFERENCE_RIFF_MUXER_STRUCTURE_UPPER_BOUND_BYTES: u64 = 64 * 1024;
/// Conservative UTF-8-to-RIFF metadata expansion factor used by preflight.
pub const REFERENCE_RIFF_METADATA_EXPANSION_FACTOR: u64 = 4;

fn validate_reference_riff_capacity(
    duration: Option<std::time::Duration>,
    contract: FinalPcmContract,
    planned_non_audio_upper_bound_bytes: Option<u64>,
) -> Result<()> {
    let duration = duration.ok_or_else(|| {
        invalid_reference("source.duration", ReferenceErrorCode::RiffSize)
    })?;
    let bytes_per_sample = match contract.bit_depth {
        PcmBitDepth::Int16 => 2_u64,
        PcmBitDepth::Int24 => 3_u64,
        PcmBitDepth::Float32 => 4_u64,
        PcmBitDepth::Float64 => 8_u64,
        PcmBitDepth::Int8 | PcmBitDepth::Int32 => {
            return Err(invalid_terminal_depth("target_bit_depth", contract.bit_depth));
        }
    };
    let sample_frames = (duration.as_nanos()
        .checked_mul(u128::from(contract.sample_rate_hz))
        .and_then(|value| value.checked_add(999_999_999))
        .ok_or_else(|| invalid_reference("source.duration", ReferenceErrorCode::RiffSize))?
        / 1_000_000_000) as u128;
    let audio_bytes = sample_frames
        .checked_mul(u128::from(contract.channels))
        .and_then(|value| value.checked_mul(u128::from(bytes_per_sample)))
        .ok_or_else(|| invalid_reference("source.duration", ReferenceErrorCode::RiffSize))?;
    let planned_non_audio_upper_bound_bytes = planned_non_audio_upper_bound_bytes.ok_or_else(|| {
        invalid_reference("planned_riff_non_audio_upper_bound_bytes", ReferenceErrorCode::RiffSize)
    })?;
    let predicted_file_bytes = audio_bytes
        .checked_add(u128::from(planned_non_audio_upper_bound_bytes))
        .ok_or_else(|| invalid_reference("source.duration", ReferenceErrorCode::RiffSize))?;
    if predicted_file_bytes > u128::from(REFERENCE_RIFF_MAX_FILE_BYTES) {
        return Err(invalid_reference(
            "resolved_output_target",
            ReferenceErrorCode::RiffSize,
        ));
    }
    Ok(())
}

/// Build a deterministic P0 Reference plan.
pub fn plan_reference_dsd(request: &PlanRequest) -> Result<ConversionPlan> {
    let settings = request.settings.dsd.from_dsd;
    if settings.pathway == DsdSourcePathway::Manual {
        return Err(invalid_reference(
            "dsd.from_dsd.pathway",
            ReferenceErrorCode::ManualUnavailable,
        ));
    }
    if settings.reference_policy != DsdReferencePolicyVersion::SoxNg14801V4 {
        return Err(invalid_reference(
            "dsd.from_dsd.reference_policy",
            ReferenceErrorCode::Toolchain,
        ));
    }
    match &request.reference_programme_scope {
        ReferenceProgrammeScope::Singleton => {}
        ReferenceProgrammeScope::IndependentAlbumBatch { .. } => {
            return Err(invalid_reference(
                "reference_programme_scope",
                ReferenceErrorCode::SingletonBatch,
            ));
        }
        ReferenceProgrammeScope::ContinuousImageRequiresPreSplitProcessing => {
            return Err(invalid_reference(
                "reference_programme_scope",
                ReferenceErrorCode::ContinuousProgramme,
            ));
        }
    }

    let source_rate = request.source.dsd_rate().ok_or_else(|| {
        PlanningError::invalid_source(
            "sample_rate_hz",
            reference_error_text(ReferenceErrorCode::UnsupportedDsdRate),
        )
    })?;
    let channels = request.source.channels.ok_or_else(|| {
        PlanningError::invalid_source(
            "channels",
            reference_error_text(ReferenceErrorCode::UnsupportedChannels),
        )
    })?;
    if !matches!(channels, 1 | 2) {
        return Err(invalid_reference(
            "source.channels",
            ReferenceErrorCode::UnsupportedChannels,
        ));
    }
    let source_kind = request.source.dsd_source_kind.as_ref().ok_or_else(|| {
        PlanningError::invalid_source(
            "dsd_source_kind",
            reference_error_text(ReferenceErrorCode::UnknownEncoding),
        )
    })?;
    match source_kind {
        DsdSourceKind::DsfUncompressed | DsdSourceKind::DsdiffUncompressed => {}
        DsdSourceKind::DsdiffDst
            if source_rate == DsdRate::Dsd64 && channels == 2 => {}
        DsdSourceKind::DsdiffDst => {
            return Err(invalid_reference(
                "source.dsd_source_kind",
                ReferenceErrorCode::CompressedDstRateUnqualified,
            ));
        }
        DsdSourceKind::SacdTrack { .. } => {
            return Err(invalid_reference(
                "source.dsd_source_kind",
                ReferenceErrorCode::SacdFrontEndIntegrationUnqualified,
            ));
        }
        DsdSourceKind::UnknownDsdContainer => {
            return Err(invalid_reference(
                "source.dsd_source_kind",
                ReferenceErrorCode::UnknownEncoding,
            ));
        }
    }
    let target = request.resolved_output_target.ok_or_else(|| {
        invalid_reference("resolved_output_target", ReferenceErrorCode::CanonicalTarget)
    })?;
    if target.is_lossy() {
        return Err(invalid_reference("resolved_output_target", ReferenceErrorCode::LossyUnavailable));
    }
    let depth = resolve_reference_depth(request.settings.target_bit_depth)?;
    if !target.is_p0_reference_lossless() {
        return Err(invalid_target_depth("resolved_output_target", target, depth));
    }
    let target_rate_hz = resolve_reference_target_rate(source_rate, request.settings.target_sample_rate)?;
    let profile = resolve_reference_profile(source_rate, target_rate_hz, settings.profile)?;
    validate_reference_target_depth(target, depth)?;
    if target == ResolvedOutputTarget::FlacNative && request.settings.flac.compression_level > 8 {
        return Err(invalid_reference(
            "flac.compression_level",
            ReferenceErrorCode::CanonicalTarget,
        ));
    }
    if target == ResolvedOutputTarget::WavPackNative && request.settings.wavpack.hybrid {
        return Err(invalid_reference(
            "wavpack.hybrid",
            ReferenceErrorCode::CanonicalTarget,
        ));
    }
    if target == ResolvedOutputTarget::WavPackNative
        && request.settings.wavpack.correction_file
    {
        return Err(invalid_reference(
            "wavpack.correction_file",
            ReferenceErrorCode::CanonicalTarget,
        ));
    }
    let front_end = resolve_reference_front_end(source_kind)?;
    let gain_policy = resolve_gain_policy(settings, target_rate_hz, depth)?;
    let final_pcm = FinalPcmContract {
        sample_rate_hz: target_rate_hz,
        channels,
        sample_kind: depth.sample_kind(),
        bit_depth: depth,
        dither: match depth {
            PcmBitDepth::Int16 => {
                return Err(invalid_reference(
                    "target_bit_depth",
                    ReferenceErrorCode::Int16TerminalUnqualified,
                ));
            }
            PcmBitDepth::Int24 => ReferenceDither::Tpdf,
            PcmBitDepth::Float32 | PcmBitDepth::Float64 => ReferenceDither::None,
            PcmBitDepth::Int8 | PcmBitDepth::Int32 => {
                return Err(invalid_terminal_depth("target_bit_depth", depth));
            }
        },
    };
    if target == ResolvedOutputTarget::WavRiff {
        validate_reference_riff_capacity(
            request.source.duration,
            final_pcm,
            request.planned_riff_non_audio_upper_bound_bytes,
        )?;
    }

    let context = request.context();
    // The pure planner consumes immutable source facts and a private-path
    // placeholder without reading source bytes. The executor attests the
    // toolchain, materializes a verified private source (decoding DST/SACD when
    // required), rebinds this path, and proves that the semantic plan is
    // unchanged before any DSP command runs.
    let canonical_input = request.input_path.clone();
    let r64 = context.intermediate_path(1, "w64");
    let final_work = context.final_work_path();
    let qpcm = if target == ResolvedOutputTarget::WavW64 {
        final_work.clone()
    } else {
        context.intermediate_path(2, "w64")
    };

    let mut steps = Vec::new();
    let mut operations = Vec::new();
    if !matches!(front_end, DsdInputFrontEnd::NativeUncompressed) {
        operations.push(DsdReferenceOperation::DsdLosslessDecodeMaterialize {
            front_end,
            output_contract: CanonicalDsdContract {
                rate: source_rate,
                channels,
            },
        });
    }

    let render = build_render_command(
        &canonical_input,
        &r64,
        target_rate_hz,
        profile,
        request.source.duration,
    );
    steps.push(PlannedExecutionStep::Command(render));
    operations.push(DsdReferenceOperation::DsdReferenceRender {
        target_rate_hz,
        profile,
        policy: settings.reference_policy,
    });

    let pre_id = MeasurementId(1);
    let post_id = MeasurementId(2);
    steps.push(PlannedExecutionStep::Measurement(build_true_peak_measurement(
        pre_id,
        TruePeakPurpose::GainAuthority,
        &r64,
    )));
    operations.push(DsdReferenceOperation::MeasureTruePeak {
        measurement_id: pre_id,
        scope: MeasurementScope::Plan,
        purpose: TruePeakPurpose::GainAuthority,
    });

    steps.push(PlannedExecutionStep::DeferredCommand(build_terminal_command(
        &r64,
        &qpcm,
        final_pcm,
        gain_policy,
        pre_id,
    )?));
    operations.push(DsdReferenceOperation::DsdReferenceFinalize {
        sample_contract: final_pcm,
        gain_policy,
        pre_final_measurement: pre_id,
    });

    steps.push(PlannedExecutionStep::Measurement(build_true_peak_measurement(
        post_id,
        TruePeakPurpose::PostFinalAcceptance,
        &qpcm,
    )));
    operations.push(DsdReferenceOperation::MeasureTruePeak {
        measurement_id: post_id,
        scope: MeasurementScope::Plan,
        purpose: TruePeakPurpose::PostFinalAcceptance,
    });

    if target != ResolvedOutputTarget::WavW64 {
        steps.push(PlannedExecutionStep::Command(build_package_command(
            &qpcm,
            &final_work,
            target,
            final_pcm,
            &request.settings,
        )?));
        operations.push(DsdReferenceOperation::PackageLossless {
            target,
            sample_contract: final_pcm,
        });
    }

    let finalization = Some(Finalization::AtomicRename {
        from: final_work.clone(),
        to: request.output_path.clone(),
    });
    let mut cleanup_paths = vec![r64.clone(), qpcm.clone()];
    if final_work != request.output_path {
        cleanup_paths.push(final_work.clone());
    }
    cleanup_paths.sort();
    cleanup_paths.dedup();

    let qualification_manifest_digest = qualification_manifest_digest();
    let semantic_plan_hash_v1 = semantic_plan_hash(
        settings.reference_policy,
        source_rate,
        channels,
        target,
        target_rate_hz,
        profile,
        final_pcm,
        gain_policy,
        front_end,
        &steps,
    );
    let package_compression_level = match target {
        ResolvedOutputTarget::FlacNative => Some(request.settings.flac.compression_level),
        ResolvedOutputTarget::WavPackNative => {
            Some(wavpack_compression_level_value(request.settings.wavpack.mode))
        }
        _ => None,
    };
    let summary = DsdReferencePlanSummary {
        policy: settings.reference_policy,
        qualification_manifest_digest,
        target,
        profile,
        front_end,
        final_pcm,
        gain_policy,
        package_compression_level,
        r64_path: r64,
        qpcm_path: qpcm,
        packaged_path: final_work,
        semantic_plan_hash_v1,
        operations,
    };
    Ok(ConversionPlan::execute_steps_with_cleanup(
        steps,
        cleanup_paths,
        finalization,
        summary,
    ))
}

/// Build the exact production render transcript for a qualification-only profile fixture.
///
/// This exists so B6 response evidence can exercise the same command builder while
/// policy admission continues to reject B6 before execution.
#[must_use]
pub fn build_reference_render_transcript_fixture(
    input: &Path,
    output: &Path,
    target_rate_hz: u32,
    profile: ResolvedDsdProfile,
    duration: Option<std::time::Duration>,
) -> PlannedCommand {
    build_render_command(input, output, target_rate_hz, profile, duration)
}

fn build_render_command(
    input: &Path,
    output: &Path,
    target_rate_hz: u32,
    profile: ResolvedDsdProfile,
    duration: Option<std::time::Duration>,
) -> PlannedCommand {
    let mut args = vec![
        "-S".to_string(),
        "-D".to_string(),
        input.display().to_string(),
        "-t".to_string(),
        "w64".to_string(),
        "-e".to_string(),
        "floating-point".to_string(),
        "-b".to_string(),
        "64".to_string(),
        output.display().to_string(),
        "gain".to_string(),
        "-12.000000000".to_string(),
        "rate".to_string(),
        "-u".to_string(),
        target_rate_hz.to_string(),
    ];
    if let Some((transition_hz, center_hz)) = profile.sinc() {
        args.extend([
            "sinc".to_string(),
            "-a".to_string(),
            "180".to_string(),
            "-L".to_string(),
            "-t".to_string(),
            transition_hz.to_string(),
            format!("-{center_hz}"),
        ]);
    }
    let mut command = PlannedCommand::new(
        ToolIdentifier::Sox,
        args,
        InputSource::Path(input.to_path_buf()),
        OutputSink::Path(output.to_path_buf()),
        duration,
        "Render qualified Reference DSD reconstruction",
    );
    command.environment_policy = CommandEnvironmentPolicy::ClearAndSet;
    command.environment.insert("LC_ALL".to_string(), "C".to_string());
    command
}

fn build_true_peak_measurement(
    id: MeasurementId,
    purpose: TruePeakPurpose,
    input: &Path,
) -> PlannedMeasurement {
    // SoX round-trips its own f64 W64 carrier exactly, while the commissioned
    // FFmpeg 7.1 W64 demuxer scales that carrier by 2^31. Policy v4 therefore
    // re-containers the samples as an f64 RIFF/WAV stream and connects the
    // two processes directly; no shell and no disk-backed RIFF
    // file are involved.
    let mut input_stage = PlannedCommand::new(
        ToolIdentifier::Sox,
        vec![
            "-S".to_string(),
            "-D".to_string(),
            input.display().to_string(),
            "-t".to_string(),
            "wav".to_string(),
            "-e".to_string(),
            "floating-point".to_string(),
            "-b".to_string(),
            "64".to_string(),
            "-".to_string(),
        ],
        InputSource::Path(input.to_path_buf()),
        OutputSink::Stdout,
        None,
        "Stream exact f64 WAV analyzer carrier",
    );
    input_stage.environment_policy = CommandEnvironmentPolicy::ClearAndSet;
    input_stage
        .environment
        .insert("LC_ALL".to_string(), "C".to_string());

    let args = vec![
        "-nostdin".to_string(),
        "-hide_banner".to_string(),
        "-nostats".to_string(),
        "-loglevel".to_string(),
        "info".to_string(),
        "-f".to_string(),
        "wav".to_string(),
        "-i".to_string(),
        "pipe:0".to_string(),
        "-filter:a".to_string(),
        "loudnorm=I=-23.0:LRA=7.0:TP=-1.0:print_format=json".to_string(),
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let mut command = PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        args,
        InputSource::Stdin,
        OutputSink::Stdout,
        None,
        match purpose {
            TruePeakPurpose::GainAuthority => "Measure pre-final true peak",
            TruePeakPurpose::PostFinalAcceptance => "Measure post-final true peak",
        },
    );
    command.environment_policy = CommandEnvironmentPolicy::ClearAndSet;
    command.environment.insert("LC_ALL".to_string(), "C".to_string());
    PlannedMeasurement {
        id,
        scope: MeasurementScope::Plan,
        purpose,
        input_stage: Some(input_stage),
        command,
        parser: MeasurementParser::FfmpegLoudnormInputTpV2,
    }
}

fn build_terminal_command(
    input: &Path,
    output: &Path,
    contract: FinalPcmContract,
    gain_policy: ResolvedGainPolicy,
    pre_id: MeasurementId,
) -> Result<PlannedDeferredCommand> {
    let (encoding, bits) = match contract.bit_depth {
        PcmBitDepth::Int16 => ("signed-integer", "16"),
        PcmBitDepth::Int24 => ("signed-integer", "24"),
        PcmBitDepth::Float32 => ("floating-point", "32"),
        PcmBitDepth::Float64 => ("floating-point", "64"),
        PcmBitDepth::Int8 | PcmBitDepth::Int32 => {
            return Err(invalid_terminal_depth("target_bit_depth", contract.bit_depth));
        }
    };
    let mut args = vec![
        PlannedArg::Literal("-S".to_string()),
        PlannedArg::Literal("-D".to_string()),
        PlannedArg::Literal(input.display().to_string()),
        PlannedArg::Literal("-t".to_string()),
        PlannedArg::Literal("w64".to_string()),
        PlannedArg::Literal("-e".to_string()),
        PlannedArg::Literal(encoding.to_string()),
        PlannedArg::Literal("-b".to_string()),
        PlannedArg::Literal(bits.to_string()),
        PlannedArg::Literal(output.display().to_string()),
    ];
    match gain_policy {
        ResolvedGainPolicy::NormalizePeak { target_dbfs } => {
            args.push(PlannedArg::Literal("norm".to_string()));
            args.push(PlannedArg::Literal(target_dbfs.render(false)));
        }
        _ => {
            args.push(PlannedArg::Literal("gain".to_string()));
            args.push(PlannedArg::BoundGainDb {
                true_peak: pre_id,
                policy: gain_policy,
            });
        }
    }
    match contract.dither {
        ReferenceDither::None => {}
        ReferenceDither::Tpdf => args.push(PlannedArg::Literal("dither".to_string())),
        ReferenceDither::Shibata => {
            args.push(PlannedArg::Literal("dither".to_string()));
            args.push(PlannedArg::Literal("-s".to_string()));
        }
    }
    let mut environment = BTreeMap::new();
    environment.insert("LC_ALL".to_string(), "C".to_string());
    Ok(PlannedDeferredCommand {
        tool: ToolIdentifier::Sox,
        args,
        input: InputSource::Path(input.to_path_buf()),
        output: OutputSink::Path(output.to_path_buf()),
        environment_policy: CommandEnvironmentPolicy::ClearAndSet,
        environment,
        description: "Apply one Reference terminal realization".to_string(),
    })
}

fn build_package_command(
    input: &Path,
    output: &Path,
    target: ResolvedOutputTarget,
    contract: FinalPcmContract,
    settings: &crate::settings::PipelineSettings,
) -> Result<PlannedCommand> {
    let pcm_codec = match contract.bit_depth {
        PcmBitDepth::Int16 => "pcm_s16le",
        PcmBitDepth::Int24 => "pcm_s24le",
        PcmBitDepth::Float32 => "pcm_f32le",
        PcmBitDepth::Float64 => "pcm_f64le",
        PcmBitDepth::Int8 | PcmBitDepth::Int32 => {
            return Err(invalid_terminal_depth("target_bit_depth", contract.bit_depth));
        }
    };
    let mut args = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-i".to_string(),
        input.display().to_string(),
        "-map".to_string(),
        "0:a:0".to_string(),
        "-map_metadata".to_string(),
        "-1".to_string(),
        "-vn".to_string(),
        "-sn".to_string(),
        "-dn".to_string(),
    ];
    match target {
        ResolvedOutputTarget::WavRiff => args.extend([
            "-c:a".to_string(),
            pcm_codec.to_string(),
            "-f".to_string(),
            "wav".to_string(),
        ]),
        ResolvedOutputTarget::WavRf64 => args.extend([
            "-c:a".to_string(),
            pcm_codec.to_string(),
            "-f".to_string(),
            "wav".to_string(),
            "-rf64".to_string(),
            "always".to_string(),
        ]),
        ResolvedOutputTarget::FlacNative => args.extend([
            "-c:a".to_string(),
            "flac".to_string(),
            "-compression_level".to_string(),
            settings.flac.compression_level.to_string(),
        ]),
        ResolvedOutputTarget::AiffNative => {
            let codec = match contract.bit_depth {
                PcmBitDepth::Int16 => "pcm_s16be",
                PcmBitDepth::Int24 => "pcm_s24be",
                PcmBitDepth::Int8
                | PcmBitDepth::Int32
                | PcmBitDepth::Float32
                | PcmBitDepth::Float64 => {
                    return Err(invalid_target_depth(
                        "target_bit_depth",
                        target,
                        contract.bit_depth,
                    ));
                }
            };
            args.extend([
                "-c:a".to_string(),
                codec.to_string(),
                "-f".to_string(),
                "aiff".to_string(),
            ]);
        }
        ResolvedOutputTarget::WavPackNative => {
            args.extend(["-c:a".to_string(), "wavpack".to_string()]);
            // FFmpeg otherwise promotes a 24-bit PCM input to a 32-bit WavPack
            // stream. Freeze the raw-depth declaration into the Reference argv
            // so the qualified Int24 cell is semantically 24-bit on decode.
            if contract.bit_depth == PcmBitDepth::Int24 {
                args.extend([
                    "-bits_per_raw_sample".to_string(),
                    "24".to_string(),
                ]);
            }
            args.extend([
                "-compression_level".to_string(),
                wavpack_compression_level(settings.wavpack.mode),
            ]);
        }
        ResolvedOutputTarget::AlacM4a => args.extend([
            "-c:a".to_string(),
            "alac".to_string(),
            "-f".to_string(),
            "ipod".to_string(),
        ]),
        ResolvedOutputTarget::WavW64 => {
            return Err(PlanningError::invalid_settings(
                "resolved_output_target",
                "W64 packages directly at the QPCM boundary and must not schedule a package command",
            ));
        }
        _ => {
            return Err(invalid_target_depth(
                "resolved_output_target",
                target,
                contract.bit_depth,
            ));
        }
    }
    args.push(output.display().to_string());
    let mut command = PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        args,
        InputSource::Path(input.to_path_buf()),
        OutputSink::Path(output.to_path_buf()),
        None,
        "Package terminal PCM without sample changes",
    );
    command.environment_policy = CommandEnvironmentPolicy::ClearAndSet;
    command.environment.insert("LC_ALL".to_string(), "C".to_string());
    Ok(command)
}

fn wavpack_compression_level_value(mode: crate::enums::WavPackMode) -> u8 {
    use crate::enums::WavPackMode;
    match mode {
        WavPackMode::Fast => 0,
        WavPackMode::Normal => 1,
        WavPackMode::High => 2,
        WavPackMode::VeryHigh => 3,
    }
}

fn wavpack_compression_level(mode: crate::enums::WavPackMode) -> String {
    wavpack_compression_level_value(mode).to_string()
}

/// Canonical digest of the source-controlled v4 qualification artifact schema/content.
#[must_use]
pub fn qualification_manifest_digest() -> Sha256Digest {
    Sha256Digest::of_bytes(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/qualification/dsd_reference_sox_ng_14_8_0_1_v4.json"
    )))
}

fn semantic_plan_hash(
    policy: DsdReferencePolicyVersion,
    source_rate: DsdRate,
    channels: u16,
    target: ResolvedOutputTarget,
    target_rate_hz: u32,
    profile: ResolvedDsdProfile,
    final_pcm: FinalPcmContract,
    gain_policy: ResolvedGainPolicy,
    front_end: DsdInputFrontEnd,
    steps: &[PlannedExecutionStep],
) -> Sha256Digest {
    let mut text = format!(
        "tonepoet-dsd-reference-semantic-plan/v1\nsource_rate={source_rate:?}\nchannels={channels}\ntarget={}\ntarget_rate={target_rate_hz}\nprofile={profile:?}\nfinal={final_pcm:?}\ngain={gain_policy:?}\nfront_end={front_end:?}\n",
        target.key()
    );
    let normalize: fn(&PlannedExecutionStep) -> String = match policy {
        DsdReferencePolicyVersion::SoxNg14801V1
        | DsdReferencePolicyVersion::SoxNg14801V2
        | DsdReferencePolicyVersion::SoxNg14801V3 => normalize_step_for_hash_legacy,
        DsdReferencePolicyVersion::SoxNg14801V4 => {
            text.push_str("environment_identity=clear_and_set/v1\n");
            normalize_step_for_hash_v4
        }
    };
    for step in steps {
        text.push_str(&normalize(step));
        text.push('\n');
    }
    Sha256Digest::of_bytes(text.as_bytes())
}

// Preserve the commissioned v1-v3 semantic-hash byte contract. Those policy
// identifiers are decode-only, but historical plans and evidence must remain
// independently verifiable after the append-only v4 correction.
fn normalize_step_for_hash_legacy(step: &PlannedExecutionStep) -> String {
    match step {
        PlannedExecutionStep::Command(command) => format!(
            "command:{}:{}",
            command.tool.program(),
            normalize_args(&command.args)
        ),
        PlannedExecutionStep::Measurement(measurement) => {
            let input_stage = measurement.input_stage.as_ref().map_or_else(
                || "direct".to_string(),
                |stage| {
                    format!(
                        "{}:{}:{}:{}:{}",
                        stage.tool.program(),
                        normalize_args(&stage.args),
                        normalize_input_source(&stage.input),
                        normalize_output_sink(&stage.output),
                        normalize_environment(&stage.environment),
                    )
                },
            );
            format!(
                "measurement:{:?}:{:?}:{}:{}:{}:{}:{}:{}",
                measurement.purpose,
                measurement.parser,
                input_stage,
                measurement.command.tool.program(),
                normalize_args(&measurement.command.args),
                normalize_input_source(&measurement.command.input),
                normalize_output_sink(&measurement.command.output),
                normalize_environment(&measurement.command.environment),
            )
        }
        PlannedExecutionStep::DeferredCommand(command) => {
            let args = command
                .args
                .iter()
                .map(|arg| match arg {
                    PlannedArg::Literal(value) => normalize_path_token(value),
                    PlannedArg::BoundGainDb { true_peak, policy } => {
                        format!("{{BOUND_GAIN:{true_peak:?}:{policy:?}}}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\u{1f}");
            format!("deferred:{}:{args}", command.tool.program())
        }
    }
}

fn normalize_step_for_hash_v4(step: &PlannedExecutionStep) -> String {
    match step {
        PlannedExecutionStep::Command(command) => format!(
            "command:{}:{}:{}:{}:{}:{}",
            command.tool.program(),
            normalize_args(&command.args),
            normalize_input_source(&command.input),
            normalize_output_sink(&command.output),
            normalize_environment_policy(command.environment_policy),
            normalize_environment(&command.environment),
        ),
        PlannedExecutionStep::Measurement(measurement) => {
            let input_stage = measurement.input_stage.as_ref().map_or_else(
                || "direct".to_string(),
                |stage| {
                    format!(
                        "{}:{}:{}:{}:{}:{}",
                        stage.tool.program(),
                        normalize_args(&stage.args),
                        normalize_input_source(&stage.input),
                        normalize_output_sink(&stage.output),
                        normalize_environment_policy(stage.environment_policy),
                        normalize_environment(&stage.environment),
                    )
                },
            );
            format!(
                "measurement:{:?}:{:?}:{}:{}:{}:{}:{}:{}:{}",
                measurement.purpose,
                measurement.parser,
                input_stage,
                measurement.command.tool.program(),
                normalize_args(&measurement.command.args),
                normalize_input_source(&measurement.command.input),
                normalize_output_sink(&measurement.command.output),
                normalize_environment_policy(measurement.command.environment_policy),
                normalize_environment(&measurement.command.environment),
            )
        }
        PlannedExecutionStep::DeferredCommand(command) => {
            let args = command
                .args
                .iter()
                .map(|arg| match arg {
                    PlannedArg::Literal(value) => normalize_path_token(value),
                    PlannedArg::BoundGainDb { true_peak, policy } => {
                        format!("{{BOUND_GAIN:{true_peak:?}:{policy:?}}}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\u{1f}");
            format!(
                "deferred:{}:{}:{}:{}:{}:{}",
                command.tool.program(),
                args,
                normalize_input_source(&command.input),
                normalize_output_sink(&command.output),
                normalize_environment_policy(command.environment_policy),
                normalize_environment(&command.environment),
            )
        }
    }
}

fn normalize_environment_policy(policy: CommandEnvironmentPolicy) -> &'static str {
    match policy {
        CommandEnvironmentPolicy::InheritAndSet => "inherit_and_set",
        CommandEnvironmentPolicy::ClearAndSet => "clear_and_set",
    }
}

fn normalize_input_source(input: &InputSource) -> String {
    match input {
        InputSource::Path(path) => format!("path:{}", normalize_path_token(&path.display().to_string())),
        InputSource::Stdin => "stdin".to_string(),
    }
}

fn normalize_output_sink(output: &OutputSink) -> String {
    match output {
        OutputSink::Path(path) => {
            format!("path:{}", normalize_path_token(&path.display().to_string()))
        }
        OutputSink::Stdout => "stdout".to_string(),
        OutputSink::InPlace(path) => {
            format!("in_place:{}", normalize_path_token(&path.display().to_string()))
        }
    }
}

fn normalize_environment(environment: &std::collections::BTreeMap<String, String>) -> String {
    environment
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

fn normalize_args(args: &[String]) -> String {
    args.iter()
        .map(|value| normalize_path_token(value))
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

fn normalize_path_token(value: &str) -> String {
    let path = Path::new(value);
    if path.is_absolute() || value.contains(".tonepoet-") {
        let extension = path.extension().and_then(|part| part.to_str()).unwrap_or("");
        if extension.is_empty() {
            "{PATH}".to_string()
        } else {
            format!("{{PATH:{extension}}}")
        }
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_nano_is_canonical_and_strict() {
        assert_eq!("-1.25".parse::<DbNano>().unwrap().render(false), "-1.250000000");
        assert_eq!("+0.000000001".parse::<DbNano>().unwrap(), DbNano(1));
        assert!("1e-3".parse::<DbNano>().is_err());
        assert!("1.0000000001".parse::<DbNano>().is_err());
        assert!("1,2".parse::<DbNano>().is_err());
    }

    #[test]
    fn db_nano_round_trips_the_complete_i64_domain() {
        for value in [DbNano(i64::MIN), DbNano(i64::MAX)] {
            let rendered = value.render(false);
            assert_eq!(rendered.parse::<DbNano>().unwrap(), value);
        }
        assert_eq!(DbNano(i64::MIN).render(false), "-9223372036.854775808");
        assert_eq!(DbNano(i64::MAX).render(false), "9223372036.854775807");
        assert!("9223372036.854775808".parse::<DbNano>().is_err());
        assert!("-9223372036.854775809".parse::<DbNano>().is_err());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn db_nano_serde_round_trips_the_complete_i64_domain() {
        for value in [DbNano(i64::MIN), DbNano(i64::MAX)] {
            let serialized = serde_json::to_string(&value).unwrap();
            let parsed: DbNano = serde_json::from_str(&serialized).unwrap();
            assert_eq!(parsed, value);
        }
    }

    fn loudnorm_json(input_tp: &str, output_tp: &str) -> String {
        format!(
            r#"{{
                "input_i":"-23.00","input_tp":"{input_tp}","input_lra":"0.10",
                "input_thresh":"-33.00","output_i":"-23.00","output_tp":"{output_tp}",
                "output_lra":"0.10","output_thresh":"-33.00",
                "normalization_type":"linear","target_offset":"0.00"
            }}"#
        )
    }

    #[test]
    fn shared_true_peak_authority_is_strict_and_uses_input_tp() {
        let json = loudnorm_json("-3.000000000", "9.000000000");
        assert_eq!(
            extract_single_loudnorm_report(&format!("prefix\n{json}\nsuffix")).unwrap(),
            json
        );
        assert!(extract_single_loudnorm_report("no report").is_err());
        assert!(extract_single_loudnorm_report(&format!("{json}\n{json}")).is_err());
        let duplicate_key = json.replacen(
            "\"input_tp\":\"-3.000000000\"",
            "\"input_tp\":\"-3.000000000\",\"input_tp\":\"-4.000000000\"",
            1,
        );
        assert!(parse_reference_true_peak_measurement(
            MeasurementId(6),
            MeasurementScope::Plan,
            TruePeakPurpose::GainAuthority,
            duplicate_key,
            DbNano::ZERO,
            DbNano::ZERO,
            false,
        )
        .is_err());

        let parsed = parse_reference_true_peak_measurement(
            MeasurementId(7),
            MeasurementScope::Plan,
            TruePeakPurpose::GainAuthority,
            json,
            DbNano(10_000_000),
            DbNano(100_000_000),
            false,
        )
        .unwrap();
        assert_eq!(parsed.reported, TruePeakValue::Finite(DbNano(-3_000_000_000)));
        assert_eq!(
            parsed.conservative_upper,
            TruePeakValue::Finite(DbNano(-2_890_000_000))
        );

        let silence = loudnorm_json("-inf", "0.0");
        assert!(parse_reference_true_peak_measurement(
            MeasurementId(8),
            MeasurementScope::Plan,
            TruePeakPurpose::GainAuthority,
            silence.clone(),
            DbNano::ZERO,
            DbNano::ZERO,
            false,
        )
        .is_err());
        assert_eq!(
            parse_reference_true_peak_measurement(
                MeasurementId(8),
                MeasurementScope::Plan,
                TruePeakPurpose::GainAuthority,
                silence,
                DbNano::ZERO,
                DbNano::ZERO,
                true,
            )
            .unwrap()
            .reported,
            TruePeakValue::VerifiedSilence
        );

        let mut zeroes = Vec::new();
        zeroes.extend_from_slice(&0_u64.to_le_bytes());
        zeroes.extend_from_slice(&(1_u64 << 63).to_le_bytes());
        validate_signed_zero_f64le(&zeroes).unwrap();
        assert!(validate_signed_zero_f64le(&1_f64.to_le_bytes()).is_err());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn policy_ids_are_append_only_and_stably_serialized() {
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V1).unwrap(),
            r#""sox_ng_14_8_0_1_v1""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V2).unwrap(),
            r#""sox_ng_14_8_0_1_v2""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V3).unwrap(),
            r#""sox_ng_14_8_0_1_v3""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V4).unwrap(),
            r#""sox_ng_14_8_0_1_v4""#
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v1""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V1
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v2""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V2
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v3""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V3
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v4""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V4
        );
    }

    #[test]
    fn corrected_profile_centers_are_frozen() {
        assert_eq!(
            resolve_reference_profile(DsdRate::Dsd64, 88_200, DsdReconstructionSelection::Reference)
                .unwrap()
                .sinc(),
            Some((10_000, 30_000))
        );
        assert_eq!(
            resolve_reference_profile(DsdRate::Dsd128, 176_400, DsdReconstructionSelection::Reference)
                .unwrap()
                .sinc(),
            Some((15_000, 37_500))
        );
        assert_eq!(
            resolve_reference_profile(DsdRate::Dsd128, 176_400, DsdReconstructionSelection::Wideband)
                .unwrap()
                .sinc(),
            Some((15_000, 42_500))
        );
        assert_eq!(
            resolve_reference_profile(DsdRate::Dsd256, 176_400, DsdReconstructionSelection::Reference)
                .unwrap()
                .sinc(),
            Some((22_000, 59_000))
        );
    }

    #[test]
    fn unsupported_matrix_cells_fail_closed() {
        assert_eq!(
            resolve_reference_profile(DsdRate::Dsd128, 88_200, DsdReconstructionSelection::Reference)
                .unwrap_err()
                .to_string(),
            "invalid settings for dsd.from_dsd.profile: DSD-REF-P0-006: Reference policy sox_ng_14_8_0_1_v4 has no qualified target-limited profile for DSD128 \u{2192} 88.2 kHz. Choose 44.1/48 kHz, choose 176.4 kHz or higher, or wait for a new policy."
        );
        assert!(resolve_reference_profile(
            DsdRate::Dsd256,
            352_800,
            DsdReconstructionSelection::Wideband
        )
        .is_err());
    }

    fn reference_request(
        source_rate: DsdRate,
        target_rate_hz: u32,
        target: ResolvedOutputTarget,
        depth: PcmBitDepth,
        profile: DsdReconstructionSelection,
    ) -> PlanRequest {
        let (format, extension) = match target {
            ResolvedOutputTarget::FlacNative => (AudioFormat::Flac, "flac"),
            ResolvedOutputTarget::WavRiff | ResolvedOutputTarget::WavRf64 => {
                (AudioFormat::Wav, "wav")
            }
            ResolvedOutputTarget::WavW64 => (AudioFormat::Wav, "w64"),
            ResolvedOutputTarget::AiffNative => (AudioFormat::Aiff, "aiff"),
            ResolvedOutputTarget::WavPackNative => (AudioFormat::WavPack, "wv"),
            ResolvedOutputTarget::AlacM4a => (AudioFormat::Alac, "m4a"),
            _ => (AudioFormat::Flac, "bin"),
        };
        let mut settings = crate::settings::PipelineSettings::default();
        settings.target_format = format;
        settings.target_sample_rate = RateTarget::PcmHz(target_rate_hz);
        settings.target_bit_depth = BitDepthTarget::Pcm(depth);
        settings.dsd.from_dsd.profile = profile;
        PlanRequest {
            input_path: PathBuf::from("admitted.dff"),
            output_path: PathBuf::from(format!("output.{extension}")),
            source: crate::source::SourceInfo {
                format: AudioFormat::Dff,
                codec: crate::enums::AudioCodec::Dsd,
                sample_rate_hz: Some(source_rate.hz()),
                bit_depth: None,
                true_source_depth: None,
                source_representation: crate::source::SourceRepresentationKind::Dsd,
                sample_kind: Some(SampleKind::Dsd),
                channels: Some(2),
                duration: Some(std::time::Duration::from_secs(60)),
                dsd_source_kind: Some(DsdSourceKind::DsdiffUncompressed),
                audio_md5: None,
            },
            settings,
            intermediate_dir: Some(PathBuf::from("work")),
            container_ffmpeg_flags: Vec::new(),
            resolved_output_target: Some(target),
            reference_programme_scope: ReferenceProgrammeScope::Singleton,
            planned_riff_non_audio_upper_bound_bytes: (target == ResolvedOutputTarget::WavRiff)
                .then_some(64 * 1024),
        }
    }

    #[test]
    fn deferred_binding_uses_the_planner_step_and_historical_policies_cannot_execute_as_v4() {
        let request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::WavW64,
            PcmBitDepth::Int24,
            DsdReconstructionSelection::Reference,
        );
        let plan = plan_reference_dsd(&request).unwrap();
        let deferred = plan
            .steps()
            .iter()
            .find_map(|step| match step {
                PlannedExecutionStep::DeferredCommand(command) => Some(command),
                _ => None,
            })
            .expect("Reference plan has one deferred terminal command");
        let mut measurements = BTreeMap::new();
        measurements.insert(
            MeasurementId(1),
            TruePeakMeasurement {
                id: MeasurementId(1),
                scope: MeasurementScope::Plan,
                purpose: TruePeakPurpose::GainAuthority,
                raw_json: loudnorm_json("-20.000000000", "0.0"),
                reported: TruePeakValue::Finite(DbNano(-20_000_000_000)),
                reporting_uncertainty: DbNano::ZERO,
                analyzer_residual: DbNano::ZERO,
                conservative_upper: TruePeakValue::Finite(DbNano(-20_000_000_000)),
            },
        );
        let resolved = resolve_reference_deferred_command(deferred, &measurements).unwrap();
        assert!(resolved.args.windows(2).any(|window| {
            window[0] == "gain" && window[1] == "+18.020599913"
        }));

        for historical in [
            DsdReferencePolicyVersion::SoxNg14801V1,
            DsdReferencePolicyVersion::SoxNg14801V2,
            DsdReferencePolicyVersion::SoxNg14801V3,
        ] {
            let mut request = request.clone();
            request.settings.dsd.from_dsd.reference_policy = historical;
            assert!(plan_reference_dsd(&request)
                .unwrap_err()
                .to_string()
                .contains("DSD-REF-P0-015"));
        }
    }

    #[test]
    fn planner_rejection_precedence_is_cartesian_and_manual_always_wins() {
        let source_kinds = [
            None,
            Some(DsdSourceKind::UnknownDsdContainer),
            Some(DsdSourceKind::DsdiffDst),
            Some(DsdSourceKind::DsdiffUncompressed),
        ];
        let rates = [None, Some(DsdRate::Dsd64.hz()), Some(DsdRate::Dsd128.hz())];
        let channels = [None, Some(0), Some(1), Some(2), Some(6)];
        let scopes = [
            ReferenceProgrammeScope::Singleton,
            ReferenceProgrammeScope::IndependentAlbumBatch {
                conversion_log_batch_id: "attempt".to_string(),
                expected_members: std::num::NonZeroUsize::new(2).unwrap(),
                ordered_source_paths_digest: Sha256Digest::of_bytes(b"a.dff\0b.dff"),
            },
            ReferenceProgrammeScope::ContinuousImageRequiresPreSplitProcessing,
        ];

        let policies = [
            DsdReferencePolicyVersion::SoxNg14801V1,
            DsdReferencePolicyVersion::SoxNg14801V2,
            DsdReferencePolicyVersion::SoxNg14801V3,
            DsdReferencePolicyVersion::SoxNg14801V4,
        ];
        let targets = [
            None,
            Some(ResolvedOutputTarget::Mp3Native),
            Some(ResolvedOutputTarget::FlacNative),
        ];
        let depths = [PcmBitDepth::Int8, PcmBitDepth::Int24];

        for source_kind in source_kinds {
            for sample_rate_hz in rates {
                for channel_count in channels {
                    for scope in &scopes {
                        for policy in policies {
                            for target in targets {
                                for depth in depths {
                                    let mut request = reference_request(
                                        DsdRate::Dsd64,
                                        88_200,
                                        ResolvedOutputTarget::FlacNative,
                                        depth,
                                        DsdReconstructionSelection::Reference,
                                    );
                                    request.settings.dsd.from_dsd.pathway =
                                        DsdSourcePathway::Manual;
                                    request.settings.dsd.from_dsd.reference_policy = policy;
                                    request.reference_programme_scope = scope.clone();
                                    request.source.sample_rate_hz = sample_rate_hz;
                                    request.source.channels = channel_count;
                                    request.source.dsd_source_kind = source_kind.clone();
                                    request.resolved_output_target = target;
                                    assert_eq!(
                                        plan_reference_dsd(&request).unwrap_err().to_string(),
                                        format!(
                                            "invalid settings for dsd.from_dsd.pathway: {}",
                                            reference_error_text(
                                                ReferenceErrorCode::ManualUnavailable
                                            )
                                        )
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn public_plan_entrypoint_preserves_manual_and_policy_precedence() {
        let mut request = reference_request(
            DsdRate::Dsd128,
            176_400,
            ResolvedOutputTarget::FlacNative,
            PcmBitDepth::Int24,
            DsdReconstructionSelection::Reference,
        );
        request.reference_programme_scope =
            ReferenceProgrammeScope::ContinuousImageRequiresPreSplitProcessing;
        request.source.dsd_source_kind = Some(DsdSourceKind::DsdiffDst);
        request.settings.dsd.from_dsd.reference_policy =
            DsdReferencePolicyVersion::SoxNg14801V1;
        request.settings.dsd.from_dsd.pathway = DsdSourcePathway::Manual;
        assert_eq!(
            crate::plan::plan_conversion(&request).unwrap_err().to_string(),
            format!(
                "invalid settings for dsd.from_dsd.pathway: {}",
                reference_error_text(ReferenceErrorCode::ManualUnavailable)
            )
        );

        request.settings.dsd.from_dsd.pathway = DsdSourcePathway::Reference;
        assert_eq!(
            crate::plan::plan_conversion(&request).unwrap_err().to_string(),
            format!(
                "invalid settings for dsd.from_dsd.reference_policy: {}",
                reference_error_text(ReferenceErrorCode::Toolchain)
            )
        );
    }

    #[test]
    fn policy_precedes_programme_and_source_admission() {
        let mut request = reference_request(
            DsdRate::Dsd128,
            176_400,
            ResolvedOutputTarget::FlacNative,
            PcmBitDepth::Int24,
            DsdReconstructionSelection::Reference,
        );
        request.reference_programme_scope =
            ReferenceProgrammeScope::ContinuousImageRequiresPreSplitProcessing;
        request.source.dsd_source_kind = Some(DsdSourceKind::DsdiffDst);
        for policy in [
            DsdReferencePolicyVersion::SoxNg14801V1,
            DsdReferencePolicyVersion::SoxNg14801V2,
            DsdReferencePolicyVersion::SoxNg14801V3,
        ] {
            request.settings.dsd.from_dsd.reference_policy = policy;
            assert_eq!(
                plan_reference_dsd(&request).unwrap_err().to_string(),
                format!(
                    "invalid settings for dsd.from_dsd.reference_policy: {}",
                    reference_error_text(ReferenceErrorCode::Toolchain)
                )
            );
        }
    }

    #[test]
    fn predictive_dst_without_independent_oracle_is_rejected_outside_dsd64_stereo() {
        for (source_rate, channels) in [
            (DsdRate::Dsd64, 1_u16),
            (DsdRate::Dsd128, 1_u16),
            (DsdRate::Dsd128, 2_u16),
            (DsdRate::Dsd256, 1_u16),
            (DsdRate::Dsd256, 2_u16),
        ] {
            let mut request = reference_request(
                source_rate,
                176_400,
                ResolvedOutputTarget::FlacNative,
                PcmBitDepth::Int24,
                DsdReconstructionSelection::Reference,
            );
            request.source.channels = Some(channels);
            request.source.dsd_source_kind = Some(DsdSourceKind::DsdiffDst);
            assert_eq!(
                plan_reference_dsd(&request).unwrap_err().to_string(),
                format!(
                    "invalid settings for source.dsd_source_kind: {}",
                    reference_error_text(ReferenceErrorCode::CompressedDstRateUnqualified)
                )
            );
        }

        let mut request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::FlacNative,
            PcmBitDepth::Int24,
            DsdReconstructionSelection::Reference,
        );
        request.source.channels = Some(2);
        request.source.dsd_source_kind = Some(DsdSourceKind::DsdiffDst);
        assert!(plan_reference_dsd(&request).is_ok());
    }

    #[test]
    fn sacd_front_ends_remain_unavailable_until_production_path_fixtures_exist() {
        for frame_format in [SacdFrameEncoding::Dsd, SacdFrameEncoding::Dst] {
            for source_rate in [DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256] {
                for channels in [1_u16, 2_u16] {
                    let mut request = reference_request(
                        source_rate,
                        176_400,
                        ResolvedOutputTarget::FlacNative,
                        PcmBitDepth::Int24,
                        DsdReconstructionSelection::Reference,
                    );
                    request.source.channels = Some(channels);
                    request.source.dsd_source_kind = Some(DsdSourceKind::SacdTrack {
                        frame_format,
                        selection: SacdTrackSelection {
                            area: SacdAreaKind::Stereo,
                            track_index_zero_based: 0,
                            start_frame: 0,
                            frame_count: 1,
                            toc_digest: Sha256Digest([0; 32]),
                        },
                    });
                    assert_eq!(
                        plan_reference_dsd(&request).unwrap_err().to_string(),
                        format!(
                            "invalid settings for source.dsd_source_kind: {}",
                            reference_error_text(
                                ReferenceErrorCode::SacdFrontEndIntegrationUnqualified
                            )
                        )
                    );
                }
            }
        }
    }

    #[test]
    fn int16_is_rejected_until_a_conservative_shibata_bound_is_derived() {
        for target in [
            ResolvedOutputTarget::FlacNative,
            ResolvedOutputTarget::WavRiff,
            ResolvedOutputTarget::WavRf64,
            ResolvedOutputTarget::WavW64,
            ResolvedOutputTarget::AiffNative,
            ResolvedOutputTarget::WavPackNative,
            ResolvedOutputTarget::AlacM4a,
        ] {
            let request = reference_request(
                DsdRate::Dsd64,
                88_200,
                target,
                PcmBitDepth::Int16,
                DsdReconstructionSelection::Reference,
            );
            assert_eq!(
                plan_reference_dsd(&request).unwrap_err().to_string(),
                format!(
                    "invalid settings for target_bit_depth: {}",
                    reference_error_text(ReferenceErrorCode::Int16TerminalUnqualified)
                )
            );
        }
    }

    #[test]
    fn complete_reference_rate_matrix_is_pinned() {
        let rates = [
            44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
            705_600, 768_000,
        ];
        for source in [DsdRate::Dsd64, DsdRate::Dsd128, DsdRate::Dsd256] {
            for target in rates {
                let result = resolve_reference_profile(
                    source,
                    target,
                    DsdReconstructionSelection::Reference,
                );
                let should_succeed = match source {
                    DsdRate::Dsd64 => true,
                    DsdRate::Dsd128 | DsdRate::Dsd256 => {
                        !matches!(target, 88_200 | 96_000)
                    }
                    DsdRate::Dsd512 | DsdRate::Dsd1024 => false,
                };
                assert_eq!(
                    result.is_ok(),
                    should_succeed,
                    "unexpected Reference matrix result for {source:?} -> {target}"
                );
            }
        }
        for source in [DsdRate::Dsd512, DsdRate::Dsd1024] {
            assert!(resolve_reference_profile(
                source,
                176_400,
                DsdReconstructionSelection::Reference
            )
            .is_err());
        }
    }

    #[test]
    fn complete_wideband_matrix_is_pinned() {
        for target in [44_100, 48_000, 88_200, 96_000] {
            assert!(resolve_reference_profile(
                DsdRate::Dsd128,
                target,
                DsdReconstructionSelection::Wideband
            )
            .is_err());
        }
        for target in [176_400, 192_000, 352_800, 384_000, 705_600, 768_000] {
            assert!(matches!(
                resolve_reference_profile(
                    DsdRate::Dsd128,
                    target,
                    DsdReconstructionSelection::Wideband
                ),
                Ok(ResolvedDsdProfile::B4W { .. })
            ));
        }
        for source in [DsdRate::Dsd64, DsdRate::Dsd256, DsdRate::Dsd512, DsdRate::Dsd1024] {
            assert!(resolve_reference_profile(
                source,
                352_800,
                DsdReconstructionSelection::Wideband
            )
            .is_err());
        }
    }

    #[test]
    fn complete_target_depth_matrix_is_pinned() {
        let targets = [
            ResolvedOutputTarget::FlacNative,
            ResolvedOutputTarget::WavRiff,
            ResolvedOutputTarget::WavRf64,
            ResolvedOutputTarget::WavW64,
            ResolvedOutputTarget::AiffNative,
            ResolvedOutputTarget::WavPackNative,
            ResolvedOutputTarget::AlacM4a,
        ];
        let depths = [
            PcmBitDepth::Int16,
            PcmBitDepth::Int24,
            PcmBitDepth::Float32,
            PcmBitDepth::Float64,
        ];
        for target in targets {
            for depth in depths {
                let should_succeed = match depth {
                    PcmBitDepth::Int16 => false,
                    PcmBitDepth::Int24 => true,
                    PcmBitDepth::Float32 | PcmBitDepth::Float64 => matches!(
                        target,
                        ResolvedOutputTarget::WavRiff
                            | ResolvedOutputTarget::WavRf64
                            | ResolvedOutputTarget::WavW64
                    ),
                    PcmBitDepth::Int8 | PcmBitDepth::Int32 => false,
                };
                assert_eq!(
                    validate_reference_target_depth(target, depth).is_ok(),
                    should_succeed,
                    "unexpected target/depth result for {target:?}/{depth:?}"
                );
            }
        }
        assert!(resolve_reference_depth(BitDepthTarget::Pcm(PcmBitDepth::Int8)).is_err());
        assert!(resolve_reference_depth(BitDepthTarget::Pcm(PcmBitDepth::Int32)).is_err());
        assert_eq!(
            resolve_reference_depth(BitDepthTarget::Source).unwrap(),
            PcmBitDepth::Int24
        );
    }

    #[test]
    fn v4_measurement_pipeline_and_hash_identity_are_frozen() {
        let request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::WavW64,
            PcmBitDepth::Float64,
            DsdReconstructionSelection::Reference,
        );
        let plan = plan_reference_dsd(&request).unwrap();
        let measurement = plan
            .steps()
            .iter()
            .find_map(|step| match step {
                PlannedExecutionStep::Measurement(measurement) => Some(measurement),
                _ => None,
            })
            .expect("Reference plan has a measurement");
        let producer = measurement
            .input_stage
            .as_ref()
            .expect("v4 measurement has a typed producer");
        let carrier = measurement
            .carrier_path()
            .expect("v4 measurement carrier is path-backed")
            .display()
            .to_string();
        assert_eq!(
            producer.args,
            [
                "-S",
                "-D",
                carrier.as_str(),
                "-t",
                "wav",
                "-e",
                "floating-point",
                "-b",
                "64",
                "-",
            ]
            .map(str::to_string)
            .to_vec()
        );
        assert_eq!(producer.input.as_path(), measurement.carrier_path());
        assert_eq!(producer.output, OutputSink::Stdout);
        assert_eq!(producer.environment_policy, CommandEnvironmentPolicy::ClearAndSet);
        assert_eq!(
            producer.environment,
            BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
        );
        assert_eq!(measurement.parser, MeasurementParser::FfmpegLoudnormInputTpV2);
        assert_eq!(measurement.command.input, InputSource::Stdin);
        assert_eq!(
            measurement.command.environment_policy,
            CommandEnvironmentPolicy::ClearAndSet
        );
        assert_eq!(
            measurement.command.environment,
            BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
        );
        assert_eq!(
            measurement.command.args,
            [
                "-nostdin",
                "-hide_banner",
                "-nostats",
                "-loglevel",
                "info",
                "-f",
                "wav",
                "-i",
                "pipe:0",
                "-filter:a",
                "loudnorm=I=-23.0:LRA=7.0:TP=-1.0:print_format=json",
                "-f",
                "null",
                "-",
            ]
            .map(str::to_string)
            .to_vec()
        );

        let baseline = normalize_step_for_hash_v4(&PlannedExecutionStep::Measurement(
            measurement.clone(),
        ));
        let mut changed_producer = measurement.clone();
        changed_producer
            .input_stage
            .as_mut()
            .unwrap()
            .args[8] = "32".to_string();
        assert_ne!(
            baseline,
            normalize_step_for_hash_v4(&PlannedExecutionStep::Measurement(changed_producer))
        );
        let mut changed_parser = measurement.clone();
        changed_parser.parser = MeasurementParser::FfmpegLoudnormInputTpV1;
        assert_ne!(
            baseline,
            normalize_step_for_hash_v4(&PlannedExecutionStep::Measurement(changed_parser))
        );
        let mut changed_transport = measurement.clone();
        changed_transport.command.input = InputSource::Path(PathBuf::from("carrier.wav"));
        assert_ne!(
            baseline,
            normalize_step_for_hash_v4(&PlannedExecutionStep::Measurement(changed_transport))
        );
        let mut changed_environment = measurement.clone();
        changed_environment
            .input_stage
            .as_mut()
            .unwrap()
            .environment
            .insert("LC_ALL".to_string(), "en_US.UTF-8".to_string());
        assert_ne!(
            baseline,
            normalize_step_for_hash_v4(&PlannedExecutionStep::Measurement(changed_environment))
        );
        let mut changed_environment_policy = measurement.clone();
        changed_environment_policy
            .input_stage
            .as_mut()
            .unwrap()
            .environment_policy = CommandEnvironmentPolicy::InheritAndSet;
        assert_ne!(
            baseline,
            normalize_step_for_hash_v4(&PlannedExecutionStep::Measurement(
                changed_environment_policy.clone(),
            ))
        );
        assert_eq!(
            normalize_step_for_hash_legacy(&PlannedExecutionStep::Measurement(measurement.clone())),
            normalize_step_for_hash_legacy(&PlannedExecutionStep::Measurement(
                changed_environment_policy.clone(),
            )),
            "append-only v1-v3 normalization must ignore the v4 environment-policy field",
        );
    }

    #[test]
    fn every_supported_profile_renders_the_corrected_frequency_argument() {
        for (source, target, selection, expected) in [
            (DsdRate::Dsd64, 44_100, DsdReconstructionSelection::Reference, None),
            (DsdRate::Dsd64, 48_000, DsdReconstructionSelection::Reference, None),
            (DsdRate::Dsd64, 88_200, DsdReconstructionSelection::Reference, Some((10_000, 30_000))),
            (DsdRate::Dsd128, 176_400, DsdReconstructionSelection::Reference, Some((15_000, 37_500))),
            (DsdRate::Dsd128, 176_400, DsdReconstructionSelection::Wideband, Some((15_000, 42_500))),
            (DsdRate::Dsd256, 176_400, DsdReconstructionSelection::Reference, Some((22_000, 59_000))),
        ] {
            let request = reference_request(
                source,
                target,
                ResolvedOutputTarget::FlacNative,
                PcmBitDepth::Int24,
                selection,
            );
            let plan = plan_reference_dsd(&request).unwrap();
            let PlannedExecutionStep::Command(render) = &plan.steps()[0] else {
                panic!("first Reference step must be the SoX render command");
            };
            assert_eq!(render.tool, ToolIdentifier::Sox);
            assert!(render.args.windows(2).any(|pair| pair == ["rate", "-u"]));
            match expected {
                None => assert!(!render.args.iter().any(|arg| arg == "sinc")),
                Some((transition, center)) => {
                    let tail = [
                        "sinc".to_string(),
                        "-a".to_string(),
                        "180".to_string(),
                        "-L".to_string(),
                        "-t".to_string(),
                        transition.to_string(),
                        format!("-{center}"),
                    ];
                    assert!(render.args.windows(tail.len()).any(|window| window == tail));
                }
            }
        }
    }

    #[test]
    fn dynamic_policy_errors_name_the_exact_source_target_depth_and_gain_mode() {
        assert_eq!(
            resolve_reference_profile(
                DsdRate::Dsd256,
                96_000,
                DsdReconstructionSelection::Reference,
            )
            .unwrap_err()
            .to_string(),
            "invalid settings for dsd.from_dsd.profile: DSD-REF-P0-007: Reference policy sox_ng_14_8_0_1_v4 has no direct 96 kHz qualification for DSD256. Choose 48 kHz, choose 176.4 kHz or higher, or wait for a new policy."
        );

        let request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::FlacNative,
            PcmBitDepth::Float32,
            DsdReconstructionSelection::Reference,
        );
        assert_eq!(
            plan_reference_dsd(&request).unwrap_err().to_string(),
            "invalid settings for target_bit_depth: DSD-REF-P0-011: flac_native does not support Float32 under Reference policy sox_ng_14_8_0_1_v4. Choose a target/depth pair listed by the policy."
        );

        let mut source_settings = DsdSourceSettings::default();
        source_settings.gain_mode = DsdSourceGainMode::NativeLevel;
        let native_policy = resolve_gain_policy(source_settings, 176_400, PcmBitDepth::Int24)
            .expect("native-level policy resolves");
        assert_eq!(
            resolve_bound_gain(TruePeakValue::Finite(DbNano::ZERO), native_policy)
                .unwrap_err()
                .to_string(),
            "invalid settings for dsd.from_dsd.gain_mode: DSD-REF-P0-016: The requested native-level gain cannot satisfy the Reference \u{2212}1.000000000 dBTP ceiling for this measured source and terminal format. Reduce the fixed gain, choose Reference gain, or choose NormalizePeak with its modified/unqualified semantics."
        );

        source_settings.gain_mode = DsdSourceGainMode::Fixed;
        source_settings.fixed_gain_db = Some(DbNano::ZERO);
        let fixed_policy = resolve_gain_policy(source_settings, 176_400, PcmBitDepth::Int24)
            .expect("fixed policy resolves");
        assert_eq!(
            resolve_bound_gain(TruePeakValue::Finite(DbNano::ZERO), fixed_policy)
                .unwrap_err()
                .to_string(),
            "invalid settings for dsd.from_dsd.gain_mode: DSD-REF-P0-016: The requested fixed gain cannot satisfy the Reference \u{2212}1.000000000 dBTP ceiling for this measured source and terminal format. Reduce the fixed gain, choose Reference gain, or choose NormalizePeak with its modified/unqualified semantics."
        );
    }

    #[test]
    fn riff_capacity_requires_a_complete_non_audio_plan_bound() {
        let mut request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::WavRiff,
            PcmBitDepth::Int24,
            DsdReconstructionSelection::Reference,
        );
        request.planned_riff_non_audio_upper_bound_bytes = None;
        assert_eq!(
            plan_reference_dsd(&request).unwrap_err().to_string(),
            format!(
                "invalid settings for planned_riff_non_audio_upper_bound_bytes: {}",
                reference_error_text(ReferenceErrorCode::RiffSize)
            )
        );
    }

    #[test]
    fn reference_wavpack_rejects_hybrid_and_correction_modes_verbatim() {
        let mut request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::WavPackNative,
            PcmBitDepth::Int24,
            DsdReconstructionSelection::Reference,
        );
        request.settings.wavpack.hybrid = true;
        request.settings.wavpack.correction_file = false;
        assert_eq!(
            plan_reference_dsd(&request).unwrap_err().to_string(),
            format!(
                "invalid settings for wavpack.hybrid: {}",
                reference_error_text(ReferenceErrorCode::CanonicalTarget)
            )
        );

        request.settings.wavpack.hybrid = false;
        request.settings.wavpack.correction_file = true;
        assert_eq!(
            plan_reference_dsd(&request).unwrap_err().to_string(),
            format!(
                "invalid settings for wavpack.correction_file: {}",
                reference_error_text(ReferenceErrorCode::CanonicalTarget)
            )
        );

        request.settings.wavpack.correction_file = false;
        assert!(plan_reference_dsd(&request).is_ok());
    }

    #[test]
    fn wavpack_int24_package_argv_freezes_authoritative_raw_depth() {
        fn package_args(depth: PcmBitDepth) -> Vec<String> {
            let request = reference_request(
                DsdRate::Dsd64,
                88_200,
                ResolvedOutputTarget::WavPackNative,
                depth,
                DsdReconstructionSelection::Reference,
            );
            let plan = plan_reference_dsd(&request).unwrap();
            let crate::plan::PlanAction::Execute { steps, .. } = plan.action else {
                panic!("Reference plan was not executable");
            };
            steps
                .into_iter()
                .find_map(|step| match step {
                    PlannedExecutionStep::Command(command) => {
                        if command.description
                            == "Package terminal PCM without sample changes"
                        {
                            Some(command.args)
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .expect("WavPack package command")
        }

        let int24 = package_args(PcmBitDepth::Int24);
        let codec = int24
            .iter()
            .position(|arg| arg == "-c:a")
            .expect("codec option");
        assert_eq!(
            int24[codec..codec + 6]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![
                "-c:a",
                "wavpack",
                "-bits_per_raw_sample",
                "24",
                "-compression_level",
                "1",
            ]
        );

        let int16_request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::WavPackNative,
            PcmBitDepth::Int16,
            DsdReconstructionSelection::Reference,
        );
        assert!(plan_reference_dsd(&int16_request)
            .unwrap_err()
            .to_string()
            .contains("DSD-REF-P0-022"));
    }

    #[test]
    fn riff_capacity_refuses_an_unrepresentable_output_before_execution() {
        let mut request = reference_request(
            DsdRate::Dsd64,
            768_000,
            ResolvedOutputTarget::WavRiff,
            PcmBitDepth::Float64,
            DsdReconstructionSelection::Reference,
        );
        request.source.duration = Some(std::time::Duration::from_secs(24 * 60 * 60));
        assert_eq!(
            plan_reference_dsd(&request).unwrap_err().to_string(),
            format!(
                "invalid settings for resolved_output_target: {}",
                reference_error_text(ReferenceErrorCode::RiffSize)
            )
        );
        request.resolved_output_target = Some(ResolvedOutputTarget::WavRf64);
        assert!(plan_reference_dsd(&request).is_ok());
    }

    #[test]
    fn terminal_bound_identity_is_rate_specific_and_numerically_conservative() {
        let low = terminal_realization_bound(44_100, PcmBitDepth::Int24);
        let high = terminal_realization_bound(768_000, PcmBitDepth::Int24);
        assert_eq!(low.max_added_peak_fs_q63_ceil, high.max_added_peak_fs_q63_ceil);
        assert_eq!(
            low.safe_pre_terminal_ceiling_dbtp,
            high.safe_pre_terminal_ceiling_dbtp
        );
        assert_ne!(low.derivation_digest, high.derivation_digest);
        assert!(low.safe_pre_terminal_ceiling_dbtp <= DbNano::REFERENCE_CEILING);
    }

    #[test]
    fn package_compression_level_changes_native_behavior_identity() {
        use crate::enums::WavPackMode;
        use crate::fingerprint::conversion_behavior_fingerprint_v1;

        let mut flac_a = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::FlacNative,
            PcmBitDepth::Int24,
            DsdReconstructionSelection::Reference,
        );
        flac_a.settings.flac.compression_level = 0;
        let mut flac_b = flac_a.clone();
        flac_b.settings.flac.compression_level = 8;
        let plan_a = plan_reference_dsd(&flac_a).unwrap();
        let plan_b = plan_reference_dsd(&flac_b).unwrap();
        assert_ne!(
            conversion_behavior_fingerprint_v1(
                plan_a.reference.as_ref().unwrap(),
                &DsdSourceKind::DsdiffUncompressed,
            ),
            conversion_behavior_fingerprint_v1(
                plan_b.reference.as_ref().unwrap(),
                &DsdSourceKind::DsdiffUncompressed,
            )
        );

        let mut wavpack_a = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::WavPackNative,
            PcmBitDepth::Int24,
            DsdReconstructionSelection::Reference,
        );
        wavpack_a.settings.wavpack.correction_file = false;
        wavpack_a.settings.wavpack.mode = WavPackMode::Fast;
        let mut wavpack_b = wavpack_a.clone();
        wavpack_b.settings.wavpack.mode = WavPackMode::VeryHigh;
        let plan_a = plan_reference_dsd(&wavpack_a).unwrap();
        let plan_b = plan_reference_dsd(&wavpack_b).unwrap();
        assert_ne!(
            conversion_behavior_fingerprint_v1(
                plan_a.reference.as_ref().unwrap(),
                &DsdSourceKind::DsdiffUncompressed,
            ),
            conversion_behavior_fingerprint_v1(
                plan_b.reference.as_ref().unwrap(),
                &DsdSourceKind::DsdiffUncompressed,
            )
        );
    }

    #[test]
    fn every_p0_error_message_is_frozen_verbatim() {
        let expected = [
            (ReferenceErrorCode::ManualUnavailable, "DSD-REF-P0-001: Manual DSD workflows are not available in this P0 build. Use Reference with a supported lossless target, or wait for Manual workflow support."),
            (ReferenceErrorCode::LossyUnavailable, "DSD-REF-P0-002: Reference DSD reconstruction currently supports lossless delivery only. Choose FLAC, RIFF/WAV, RF64, W64, AIFF, WavPack, or ALAC/M4A, or wait for Reference-front-end Opus/MP3/AAC delivery."),
            (ReferenceErrorCode::UnsupportedDsdRate, "DSD-REF-P0-003: Reference policy sox_ng_14_8_0_1_v4 supports DSD64, DSD128, and DSD256 only. Use a supported-rate source or wait for expanded-rate/Manual support."),
            (ReferenceErrorCode::UnknownEncoding, "DSD-REF-P0-004: The DSD container or compression mode could not be identified as DSF/DSD, DSDIFF/DSD, DSDIFF/DST, or a supported SACD area. Reference will not guess the decoder path."),
            (ReferenceErrorCode::UnsupportedChannels, "DSD-REF-P0-005: Reference policy sox_ng_14_8_0_1_v4 supports qualified mono and stereo cells only. Select a mono/stereo track or wait for multichannel qualification."),
            (ReferenceErrorCode::Target882, "DSD-REF-P0-006: Reference policy sox_ng_14_8_0_1_v4 has no qualified target-limited profile for {DSD128|DSD256} \u{2192} 88.2 kHz. Choose 44.1/48 kHz, choose 176.4 kHz or higher, or wait for a new policy."),
            (ReferenceErrorCode::Target96, "DSD-REF-P0-007: Reference policy sox_ng_14_8_0_1_v4 has no direct 96 kHz qualification for {DSD128|DSD256}. Choose 48 kHz, choose 176.4 kHz or higher, or wait for a new policy."),
            (ReferenceErrorCode::WidebandDsd64, "DSD-REF-P0-008: No Wideband profile is defined for DSD64. Select the Reference profile."),
            (ReferenceErrorCode::WidebandDsd128Target, "DSD-REF-P0-008: DSD128 Wideband uses B4W and requires a target rate of at least 176.4 kHz. Select the Reference profile or choose 176.4 kHz or higher."),
            (ReferenceErrorCode::WidebandDsd256Target, "DSD-REF-P0-008: DSD256 Wideband uses B6, whose 140 kHz stopband edge cannot fit this target; B6 is also unavailable under policy sox_ng_14_8_0_1_v4. Select Reference/B5."),
            (ReferenceErrorCode::B6Unavailable, "DSD-REF-P0-009: B6 is represented but unqualified and unavailable under policy sox_ng_14_8_0_1_v4. Select Reference/B5 or wait for a later immutable policy."),
            (ReferenceErrorCode::TerminalInt8, "DSD-REF-P0-010: Reference policy sox_ng_14_8_0_1_v4 has no qualified 8-bit terminal realization. Choose 24-bit, Float32, or Float64 where supported."),
            (ReferenceErrorCode::TerminalInt32, "DSD-REF-P0-010: Reference policy sox_ng_14_8_0_1_v4 has no qualified 32-bit integer terminal realization. Choose 24-bit, Float32, or Float64 where supported."),
            (ReferenceErrorCode::TargetDepth, "DSD-REF-P0-011: {target} does not support {depth} under Reference policy sox_ng_14_8_0_1_v4. Choose a target/depth pair listed by the policy."),
            (ReferenceErrorCode::SingletonBatch, "DSD-REF-P0-012: Reference P0 supports singleton conversions only. Convert the selected files one at a time as independent singletons with independent gain, or wait for programme-wide Reference support."),
            (ReferenceErrorCode::ContinuousProgramme, "DSD-REF-P0-013: Reference P0 cannot split a continuous DSD programme before reconstruction. This source must be processed as one programme before splitting; wait for programme-wide Reference support. Already independent files may be converted one at a time with independent gain."),
            (ReferenceErrorCode::FrontEndUnattested, "DSD-REF-P0-014: Reference requires the qualified DST/SACD decode front-end for this source, but the decoder/extractor identity or qualification manifest does not match. Install the qualified toolchain or use an uncompressed DSF/DSDIFF source."),
            (ReferenceErrorCode::Toolchain, "DSD-REF-P0-015: The installed Reference toolchain does not match policy sox_ng_14_8_0_1_v4 or failed its behavior probes. Activate/install the qualified toolchain; tonepoet will not substitute another decoder, analyzer, resampler, or encoder."),
            (ReferenceErrorCode::UnsafeExactGain, "DSD-REF-P0-016: The requested {native-level|fixed} gain cannot satisfy the Reference \u{2212}1.000000000 dBTP ceiling for this measured source and terminal format. Reduce the fixed gain, choose Reference gain, or choose NormalizePeak with its modified/unqualified semantics."),
            (ReferenceErrorCode::UnsupportedTargetRate, "DSD-REF-P0-017: Reference policy sox_ng_14_8_0_1_v4 supports target sample rates 44.1, 48, 88.2, 96, 176.4, 192, 352.8, 384, 705.6, and 768 kHz only. Choose one of those rates or wait for a later immutable policy."),
            (ReferenceErrorCode::RiffSize, "DSD-REF-P0-018: The predicted RIFF/WAV output exceeds the qualified RIFF size limit. Choose RF64, W64, or another supported lossless target."),
            (ReferenceErrorCode::CanonicalTarget, "DSD-REF-P0-019: The selected output container does not match the canonical Reference target or contains unrecognized output flags. Re-select the target."),
            (ReferenceErrorCode::CompressedDstRateUnqualified, "DSD-REF-P0-021: Reference policy sox_ng_14_8_0_1_v4 qualifies predictive compressed DST only for stereo DSD64. Mono DSD64 and all DSD128/DSD256 predictive-DST cells remain unavailable because no matching independent-oracle corpus is present. Use an uncompressed DSF/DSDIFF source, decode with an independently verified tool outside Reference, or wait for a later immutable policy."),
            (ReferenceErrorCode::Int16TerminalUnqualified, "DSD-REF-P0-022: Reference policy sox_ng_14_8_0_1_v4 does not enable Int16 because the commissioned SoX-ng Shibata realization has no qualified conservative worst-case peak bound. Choose Int24, Float32, or Float64, or wait for a later immutable policy with a derived Shibata bound."),
            (ReferenceErrorCode::SacdFrontEndIntegrationUnqualified, "DSD-REF-P0-023: Reference policy sox_ng_14_8_0_1_v4 does not enable SACD DSD or DST extraction because the production extraction/materialization path is not yet qualified by pinned end-to-end SACD fixtures. Extract to a qualified DSF/DSDIFF source first or wait for a later immutable policy."),
            (ReferenceErrorCode::ManagedDestination, "DSD-REF-P0-020: The destination album has incompatible or incomplete tonepoet manifest authority. Choose a different output directory, repair/recover the existing transaction, or reconvert the album under one compatible Reference route; tonepoet will not merge or replace authority implicitly."),
        ];
        let mut messages = std::collections::BTreeSet::new();
        for (code, exact) in expected {
            let actual = reference_error_text(code);
            assert_eq!(actual, exact, "drifted exact text for {code:?}");
            assert!(messages.insert(actual), "duplicate exact error message: {actual}");
        }
    }
}
