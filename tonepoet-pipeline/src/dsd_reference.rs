//! Qualified P0 Reference DSD-to-PCM policy and pure planning primitives.
//!
//! This module intentionally contains no filesystem or process I/O. Runtime
//! source materialization, tool attestation, measurement execution, publication,
//! and qualification reporting live in the orchestrator crate.

use crate::enums::{AudioFormat, BitDepthTarget, DsdRate, PcmBitDepth, RateTarget, SampleKind};
use crate::error::{PlanningError, Result};
use crate::plan::{
    CommandEnvironmentPolicy, ConversionPlan, Finalization, InputSource, OutputSink, PlanRequest,
    PlannedCommand, PlannedCommandPipeline, PlannedExecutionStep,
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
/// Stable historical policy key for the v5 post-final ceiling reserve contract.
pub const DSD_REFERENCE_POLICY_V5_KEY: &str = "sox_ng_14_8_0_1_v5";
/// Stable historical policy key for the v6 carrier-sensitive analyzer contract.
pub const DSD_REFERENCE_POLICY_V6_KEY: &str = "sox_ng_14_8_0_1_v6";
/// Stable historical policy key for the v7 Float64 packaging and evidence contract.
pub const DSD_REFERENCE_POLICY_V7_KEY: &str = "sox_ng_14_8_0_1_v7";
/// Stable historical policy key for the v8 signed-32-bit terminal-bound contract.
pub const DSD_REFERENCE_POLICY_V8_KEY: &str = "sox_ng_14_8_0_1_v8";
/// Stable historical policy key for the v9 W64 metadata-mutation admission contract.
pub const DSD_REFERENCE_POLICY_V9_KEY: &str = "sox_ng_14_8_0_1_v9";
/// Stable historical policy key for the v10 exact production-metadata evidence contract.
pub const DSD_REFERENCE_POLICY_V10_KEY: &str = "sox_ng_14_8_0_1_v10";
/// Stable historical policy key for the v11 runtime-bound production-metadata mutator contract.
pub const DSD_REFERENCE_POLICY_V11_KEY: &str = "sox_ng_14_8_0_1_v11";
/// Stable historical policy key for the v12 bounded streamed-WAV carrier contract.
pub const DSD_REFERENCE_POLICY_V12_KEY: &str = "sox_ng_14_8_0_1_v12";
/// Stable historical policy key for the v13 corrected streamed-WAV header contract.
pub const DSD_REFERENCE_POLICY_V13_KEY: &str = "sox_ng_14_8_0_1_v13";
/// Stable historical policy key for the v14 oversampled true-peak analyzer contract.
pub const DSD_REFERENCE_POLICY_V14_KEY: &str = "sox_ng_14_8_0_1_v14";
/// Stable historical policy key for the v15 analyzer-evidence and workload-deadline contract.
pub const DSD_REFERENCE_POLICY_V15_KEY: &str = "sox_ng_14_8_0_1_v15";
/// Stable policy key for the v16 exact Wave64 structural-integrity contract.
pub const DSD_REFERENCE_POLICY_V16_KEY: &str = "sox_ng_14_8_0_1_v16";
/// Commissioned SoX-ng source revision.
pub const DSD_REFERENCE_SOX_NG_REVISION: &str =
    "324b8cf873fd7836e8848bd87f7a90d8faa6f849";
/// Expected SoX-ng version string fragment.
pub const DSD_REFERENCE_SOX_NG_VERSION: &str = "14.8.0.1";
/// Stable current policy qualification artifact path.
pub const DSD_REFERENCE_QUALIFICATION_MANIFEST_PATH: &str =
    "qualification/dsd_reference_sox_ng_14_8_0_1_v16.json";

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
    /// One analyzer reporting quantum reserved between gain binding and post-final acceptance.
    pub const POST_FINAL_ACCEPTANCE_RESERVE: Self = Self(10_000_000);
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
    /// Corrected v4 exact streamed-analyzer contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v4"))]
    SoxNg14801V4,
    /// Corrected v5 terminal ceiling reserve contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v5"))]
    SoxNg14801V5,
    /// Corrected v6 carrier-sensitive analyzer contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v6"))]
    SoxNg14801V6,
    /// Corrected v7 Float64 packaging and independent sample-identity contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v7"))]
    SoxNg14801V7,
    /// Corrected v8 signed-32-bit terminal-bound contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v8"))]
    SoxNg14801V8,
    /// Corrected v9 W64 metadata-mutation admission contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v9"))]
    SoxNg14801V9,
    /// Corrected v10 exact production-metadata evidence contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v10"))]
    SoxNg14801V10,
    /// Corrected v11 runtime-bound production-metadata mutator contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v11"))]
    SoxNg14801V11,
    /// Corrected v12 bounded streamed-WAV carrier contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v12"))]
    SoxNg14801V12,
    /// Corrected v13 streamed-WAV header-size and capacity contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v13"))]
    SoxNg14801V13,
    /// Corrected v14 oversampled true-peak analyzer contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v14"))]
    SoxNg14801V14,
    /// Corrected v15 analyzer evidence, deadline, and executor-liveness contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v15"))]
    SoxNg14801V15,
    /// Corrected v16 exact Wave64 structural-integrity and consumer-compatibility contract.
    #[default]
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v16"))]
    SoxNg14801V16,
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
            Self::SoxNg14801V5 => DSD_REFERENCE_POLICY_V5_KEY,
            Self::SoxNg14801V6 => DSD_REFERENCE_POLICY_V6_KEY,
            Self::SoxNg14801V7 => DSD_REFERENCE_POLICY_V7_KEY,
            Self::SoxNg14801V8 => DSD_REFERENCE_POLICY_V8_KEY,
            Self::SoxNg14801V9 => DSD_REFERENCE_POLICY_V9_KEY,
            Self::SoxNg14801V10 => DSD_REFERENCE_POLICY_V10_KEY,
            Self::SoxNg14801V11 => DSD_REFERENCE_POLICY_V11_KEY,
            Self::SoxNg14801V12 => DSD_REFERENCE_POLICY_V12_KEY,
            Self::SoxNg14801V13 => DSD_REFERENCE_POLICY_V13_KEY,
            Self::SoxNg14801V14 => DSD_REFERENCE_POLICY_V14_KEY,
            Self::SoxNg14801V15 => DSD_REFERENCE_POLICY_V15_KEY,
            Self::SoxNg14801V16 => DSD_REFERENCE_POLICY_V16_KEY,
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
    /// Explicit DSD128 wideband profile; all other v3/v4/v5 cells reject.
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
            reference_policy: DsdReferencePolicyVersion::SoxNg14801V16,
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

/// Canonical byte contract for decoded-sample SHA-256 evidence.
///
/// Samples are interleaved, little-endian, and encoded at the terminal depth:
/// Int24 as `pcm_s24le`, Float32 as `pcm_f32le`, and Float64 as `pcm_f64le`.
pub const REFERENCE_SAMPLE_HASH_FORMAT: &str = "interleaved_depth_native_le_sha256";

/// Semantic role of a carrier whose decoded samples are inspected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceDecodedSampleRole {
    /// Planner-owned Float64 W64 reconstruction carrier before terminal realization.
    ReconstructionR64W64,
    /// Planner-owned W64 terminal PCM carrier.
    TerminalQpcmW64,
    /// Planner-owned lossless package before metadata mutation.
    PackagedOutput {
        /// Exact output target and therefore exact carrier/container identity.
        target: ResolvedOutputTarget,
    },
    /// Delivered output after metadata, artwork, and ReplayGain mutation.
    PostMetadataOutput {
        /// Exact output target and therefore exact carrier/container identity.
        target: ResolvedOutputTarget,
    },
}

/// Closed selector for planner-owned carriers whose decoded samples are inspected.
///
/// The selector does not contain a path. `DsdReferencePlanSummary` resolves the
/// selector to both the exact planner-owned path and its semantic role, so callers
/// cannot pair an arbitrary path with a more permissive decode authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceDecodedCarrierSelector {
    /// Planner-owned Float64 W64 reconstruction carrier.
    ReconstructionR64,
    /// Planner-owned terminal QPCM W64 carrier.
    TerminalQpcm,
    /// Planner-owned lossless package before finalization and metadata mutation.
    PackagedOutput,
    /// Planner-owned delivered output after finalization and metadata mutation.
    PostMetadataOutput,
}

impl ReferenceDecodedCarrierSelector {
    /// Stable diagnostic key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::ReconstructionR64 => "reconstruction_r64",
            Self::TerminalQpcm => "terminal_qpcm",
            Self::PackagedOutput => "packaged_output",
            Self::PostMetadataOutput => "post_metadata_output",
        }
    }
}

/// Normalized role/carrier class used by the immutable decode rule table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReferenceDecodeRoleClass {
    /// Float64 reconstruction W64.
    ReconstructionR64W64,
    /// Terminal QPCM W64.
    TerminalQpcmW64,
    /// Packaged W64 before metadata mutation.
    PackagedW64,
    /// Packaged non-W64 output before metadata mutation.
    PackagedNonW64,
    /// Delivered W64 after metadata mutation.
    PostMetadataW64,
    /// Delivered non-W64 output after metadata mutation.
    PostMetadataNonW64,
}

impl ReferenceDecodeRoleClass {
    /// Stable evidence key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::ReconstructionR64W64 => "r64_float64_w64",
            Self::TerminalQpcmW64 => "qpcm_w64",
            Self::PackagedW64 => "packaged_w64",
            Self::PackagedNonW64 => "packaged_non_w64",
            Self::PostMetadataW64 => "post_metadata_w64",
            Self::PostMetadataNonW64 => "post_metadata_non_w64",
        }
    }
}

/// Authorized decoder mechanism for one carrier role and terminal depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReferenceDecodeMechanism {
    /// Decode the carrier directly with FFmpeg.
    DirectFfmpeg,
    /// Decode Float64 W64 with SoX-ng to headerless little-endian raw f64.
    SoxFloat64W64RawStream,
}

impl ReferenceDecodeMechanism {
    /// Stable evidence key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::DirectFfmpeg => "ffmpeg_direct",
            Self::SoxFloat64W64RawStream => "sox_f64le_raw_stream",
        }
    }
}

/// Exact depth-native encoding hashed by FFmpeg's SHA-256 sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReferenceSampleHashEncoding {
    /// Signed 24-bit little-endian PCM.
    SignedInt24Le,
    /// IEEE-754 binary32 little-endian PCM.
    Float32Le,
    /// IEEE-754 binary64 little-endian PCM.
    Float64Le,
}

impl ReferenceSampleHashEncoding {
    /// FFmpeg codec name that materializes the canonical hash bytes.
    #[must_use]
    pub const fn ffmpeg_codec(self) -> &'static str {
        match self {
            Self::SignedInt24Le => "pcm_s24le",
            Self::Float32Le => "pcm_f32le",
            Self::Float64Le => "pcm_f64le",
        }
    }

    /// Stable evidence key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::SignedInt24Le => "int24_le",
            Self::Float32Le => "float32_le",
            Self::Float64Le => "float64_le",
        }
    }
}

/// One immutable carrier-role/depth decoder rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReferenceDecodeRouteRule {
    role_class: ReferenceDecodeRoleClass,
    bit_depth: PcmBitDepth,
    mechanism: ReferenceDecodeMechanism,
    hash_encoding: ReferenceSampleHashEncoding,
}

impl ReferenceDecodeRouteRule {
    const fn new(
        role_class: ReferenceDecodeRoleClass,
        bit_depth: PcmBitDepth,
        mechanism: ReferenceDecodeMechanism,
        hash_encoding: ReferenceSampleHashEncoding,
    ) -> Self {
        Self {
            role_class,
            bit_depth,
            mechanism,
            hash_encoding,
        }
    }

    /// Normalized carrier role.
    #[must_use]
    pub const fn role_class(self) -> ReferenceDecodeRoleClass {
        self.role_class
    }

    /// Terminal depth whose decoded bytes are inspected.
    #[must_use]
    pub const fn bit_depth(self) -> PcmBitDepth {
        self.bit_depth
    }

    /// Authorized decoder mechanism.
    #[must_use]
    pub const fn mechanism(self) -> ReferenceDecodeMechanism {
        self.mechanism
    }

    /// Exact depth-native bytes hashed after decoding.
    #[must_use]
    pub const fn hash_encoding(self) -> ReferenceSampleHashEncoding {
        self.hash_encoding
    }
}

/// Complete immutable v7 decoder authority.
///
/// The rule table is deliberately exhaustive for every admitted terminal depth
/// and every production or qualification carrier role. Float64 W64 never has a
/// direct-FFmpeg rule.
pub const REFERENCE_DECODE_ROUTE_RULES: [ReferenceDecodeRouteRule; 16] = [
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::ReconstructionR64W64,
        PcmBitDepth::Float64,
        ReferenceDecodeMechanism::SoxFloat64W64RawStream,
        ReferenceSampleHashEncoding::Float64Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::TerminalQpcmW64,
        PcmBitDepth::Int24,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::SignedInt24Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::TerminalQpcmW64,
        PcmBitDepth::Float32,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::Float32Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::TerminalQpcmW64,
        PcmBitDepth::Float64,
        ReferenceDecodeMechanism::SoxFloat64W64RawStream,
        ReferenceSampleHashEncoding::Float64Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::PackagedW64,
        PcmBitDepth::Int24,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::SignedInt24Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::PackagedW64,
        PcmBitDepth::Float32,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::Float32Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::PackagedW64,
        PcmBitDepth::Float64,
        ReferenceDecodeMechanism::SoxFloat64W64RawStream,
        ReferenceSampleHashEncoding::Float64Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::PackagedNonW64,
        PcmBitDepth::Int24,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::SignedInt24Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::PackagedNonW64,
        PcmBitDepth::Float32,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::Float32Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::PackagedNonW64,
        PcmBitDepth::Float64,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::Float64Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::PostMetadataW64,
        PcmBitDepth::Int24,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::SignedInt24Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::PostMetadataW64,
        PcmBitDepth::Float32,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::Float32Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::PostMetadataW64,
        PcmBitDepth::Float64,
        ReferenceDecodeMechanism::SoxFloat64W64RawStream,
        ReferenceSampleHashEncoding::Float64Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::PostMetadataNonW64,
        PcmBitDepth::Int24,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::SignedInt24Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::PostMetadataNonW64,
        PcmBitDepth::Float32,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::Float32Le,
    ),
    ReferenceDecodeRouteRule::new(
        ReferenceDecodeRoleClass::PostMetadataNonW64,
        PcmBitDepth::Float64,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::Float64Le,
    ),
];

/// Failure to authorize a decoded-sample route under the immutable v7 table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceDecodeAuthorityError {
    message: String,
}

impl ReferenceDecodeAuthorityError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ReferenceDecodeAuthorityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ReferenceDecodeAuthorityError {}

/// Opaque proof that one carrier role, target, and depth has an admitted route.
///
/// Callers cannot construct this value directly. Executable command builders do
/// not accept this mechanism proof on its own; they accept a
/// `ReferenceDecodedCarrier`, which additionally binds the exact planner-owned
/// path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReferenceDecodeAuthority {
    role: ReferenceDecodedSampleRole,
    role_class: ReferenceDecodeRoleClass,
    contract: FinalPcmContract,
    mechanism: ReferenceDecodeMechanism,
    hash_encoding: ReferenceSampleHashEncoding,
}

impl ReferenceDecodeAuthority {
    /// Original semantic carrier role.
    #[must_use]
    pub const fn role(self) -> ReferenceDecodedSampleRole {
        self.role
    }

    /// Normalized role/carrier class selected by the rule table.
    #[must_use]
    pub const fn role_class(self) -> ReferenceDecodeRoleClass {
        self.role_class
    }

    /// Exact PCM contract bound into the authority.
    #[must_use]
    pub const fn contract(self) -> FinalPcmContract {
        self.contract
    }

    /// Authorized decoder mechanism.
    #[must_use]
    pub const fn mechanism(self) -> ReferenceDecodeMechanism {
        self.mechanism
    }

    /// Exact depth-native bytes hashed after decoding.
    #[must_use]
    pub const fn hash_encoding(self) -> ReferenceSampleHashEncoding {
        self.hash_encoding
    }

    /// Canonical hash-format identifier.
    #[must_use]
    pub const fn hash_format(self) -> &'static str {
        REFERENCE_SAMPLE_HASH_FORMAT
    }
}

/// Opaque binding between one exact planner-owned carrier path and its route authority.
///
/// Fields are private and construction is available only through
/// `DsdReferencePlanSummary`, which selects the path, semantic role, and PCM
/// contract as one operation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReferenceDecodedCarrier {
    path: PathBuf,
    authority: ReferenceDecodeAuthority,
}

impl ReferenceDecodedCarrier {
    /// Exact path selected by the trusted plan summary.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Opaque route authority bound to this exact path.
    #[must_use]
    pub const fn authority(&self) -> ReferenceDecodeAuthority {
        self.authority
    }
}

fn reference_decode_role_class(
    role: ReferenceDecodedSampleRole,
    contract: FinalPcmContract,
) -> std::result::Result<ReferenceDecodeRoleClass, ReferenceDecodeAuthorityError> {
    match role {
        ReferenceDecodedSampleRole::ReconstructionR64W64 => {
            if contract.bit_depth != PcmBitDepth::Float64
                || contract.sample_kind != SampleKind::Float
                || contract.dither != ReferenceDither::None
            {
                return Err(ReferenceDecodeAuthorityError::new(
                    "Reference R64 decode authority requires undithered Float64 PCM",
                ));
            }
            Ok(ReferenceDecodeRoleClass::ReconstructionR64W64)
        }
        ReferenceDecodedSampleRole::TerminalQpcmW64 => {
            Ok(ReferenceDecodeRoleClass::TerminalQpcmW64)
        }
        ReferenceDecodedSampleRole::PackagedOutput { target } => {
            validate_reference_target_depth(target, contract.bit_depth).map_err(|error| {
                ReferenceDecodeAuthorityError::new(format!(
                    "Reference packaged-output decode authority rejected {}/{}: {error}",
                    target.key(),
                    contract.bit_depth.bits(),
                ))
            })?;
            Ok(if target == ResolvedOutputTarget::WavW64 {
                ReferenceDecodeRoleClass::PackagedW64
            } else {
                ReferenceDecodeRoleClass::PackagedNonW64
            })
        }
        ReferenceDecodedSampleRole::PostMetadataOutput { target } => {
            validate_reference_target_depth(target, contract.bit_depth).map_err(|error| {
                ReferenceDecodeAuthorityError::new(format!(
                    "Reference post-metadata decode authority rejected {}/{}: {error}",
                    target.key(),
                    contract.bit_depth.bits(),
                ))
            })?;
            Ok(if target == ResolvedOutputTarget::WavW64 {
                ReferenceDecodeRoleClass::PostMetadataW64
            } else {
                ReferenceDecodeRoleClass::PostMetadataNonW64
            })
        }
    }
}

/// Authorize the only decoder route admitted for a carrier role and PCM contract.
pub fn reference_decode_authority(
    role: ReferenceDecodedSampleRole,
    contract: FinalPcmContract,
) -> std::result::Result<ReferenceDecodeAuthority, ReferenceDecodeAuthorityError> {
    if contract.sample_rate_hz == 0 || contract.channels == 0 {
        return Err(ReferenceDecodeAuthorityError::new(
            "Reference decode contract requires a nonzero sample rate and channel count",
        ));
    }
    if contract.sample_kind != contract.bit_depth.sample_kind() {
        return Err(ReferenceDecodeAuthorityError::new(format!(
            "Reference decode contract sample kind {:?} disagrees with {:?}",
            contract.sample_kind, contract.bit_depth,
        )));
    }
    let expected_dither = match contract.bit_depth {
        PcmBitDepth::Int24 => ReferenceDither::Tpdf,
        PcmBitDepth::Float32 | PcmBitDepth::Float64 => ReferenceDither::None,
        PcmBitDepth::Int8 | PcmBitDepth::Int16 | PcmBitDepth::Int32 => {
            return Err(ReferenceDecodeAuthorityError::new(format!(
                "Reference v7 has no decoded-sample route for {:?}",
                contract.bit_depth,
            )));
        }
    };
    if contract.dither != expected_dither {
        return Err(ReferenceDecodeAuthorityError::new(format!(
            "Reference decode contract dither {:?} disagrees with {:?} for {:?}",
            contract.dither, expected_dither, contract.bit_depth,
        )));
    }

    let role_class = reference_decode_role_class(role, contract)?;
    let mut rules = REFERENCE_DECODE_ROUTE_RULES
        .iter()
        .copied()
        .filter(|rule| rule.role_class == role_class && rule.bit_depth == contract.bit_depth);
    let rule = rules.next().ok_or_else(|| {
        ReferenceDecodeAuthorityError::new(format!(
            "Reference v7 has no decoded-sample rule for {}/{}",
            role_class.key(),
            contract.bit_depth.bits(),
        ))
    })?;
    if rules.next().is_some() {
        return Err(ReferenceDecodeAuthorityError::new(format!(
            "Reference v7 has ambiguous decoded-sample rules for {}/{}",
            role_class.key(),
            contract.bit_depth.bits(),
        )));
    }
    Ok(ReferenceDecodeAuthority {
        role,
        role_class,
        contract,
        mechanism: rule.mechanism,
        hash_encoding: rule.hash_encoding,
    })
}

/// Validate an externally proposed decoder mechanism against the immutable table.
///
/// This entry point exists for manifest/report validation and the mandatory
/// negative regression. It returns an opaque authority only when the proposed
/// mechanism exactly matches the carrier-role-aware rule.
pub fn validate_reference_decode_mechanism(
    role: ReferenceDecodedSampleRole,
    contract: FinalPcmContract,
    proposed: ReferenceDecodeMechanism,
) -> std::result::Result<ReferenceDecodeAuthority, ReferenceDecodeAuthorityError> {
    let authority = reference_decode_authority(role, contract)?;
    if authority.mechanism != proposed {
        return Err(ReferenceDecodeAuthorityError::new(format!(
            "Reference v7 rejects {} for {}/{}; required route is {}",
            proposed.key(),
            authority.role_class.key(),
            contract.bit_depth.bits(),
            authority.mechanism.key(),
        )));
    }
    Ok(authority)
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
    /// Carrier-sensitive v6 loudnorm contract: f64 W64 is streamed through SoX;
    /// Float32 W64 is decoded directly by FFmpeg to avoid SoX-ng's f32 W64 readback defect.
    FfmpegLoudnormInputTpV3,
    /// Policy-v14 SoX `stats` peak over a qualified 16x oversampled measurement view.
    SoxStatsPkLevDbV1,
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
    /// Workload-derived timeout bound shared by every true-peak analyzer process.
    #[cfg_attr(feature = "serde", serde(default))]
    pub analyzer_deadline: std::time::Duration,
    /// Planner-owned 64-bit floating reconstruction carrier.
    pub r64_path: PathBuf,
    /// Planner-owned one-and-only terminal PCM carrier.
    pub qpcm_path: PathBuf,
    /// Planner-owned staged lossless package before atomic finalization; equal to
    /// `qpcm_path` for W64.
    pub packaged_path: PathBuf,
    /// Planner-owned delivered carrier after atomic finalization. Metadata, artwork,
    /// and ReplayGain mutation operate on this exact path.
    #[cfg_attr(feature = "serde", serde(default))]
    pub delivered_path: PathBuf,
    /// Semantic plan hash with path roles normalized.
    pub semantic_plan_hash_v1: Sha256Digest,
    /// Ordered operation summaries.
    pub operations: Vec<DsdReferenceOperation>,
}

impl DsdReferencePlanSummary {
    fn decoded_carrier_spec(
        &self,
        selector: ReferenceDecodedCarrierSelector,
    ) -> (&Path, ReferenceDecodedSampleRole, FinalPcmContract) {
        match selector {
            ReferenceDecodedCarrierSelector::ReconstructionR64 => (
                self.r64_path.as_path(),
                ReferenceDecodedSampleRole::ReconstructionR64W64,
                FinalPcmContract {
                    sample_rate_hz: self.final_pcm.sample_rate_hz,
                    channels: self.final_pcm.channels,
                    sample_kind: SampleKind::Float,
                    bit_depth: PcmBitDepth::Float64,
                    dither: ReferenceDither::None,
                },
            ),
            ReferenceDecodedCarrierSelector::TerminalQpcm => (
                self.qpcm_path.as_path(),
                ReferenceDecodedSampleRole::TerminalQpcmW64,
                self.final_pcm,
            ),
            ReferenceDecodedCarrierSelector::PackagedOutput => (
                self.packaged_path.as_path(),
                ReferenceDecodedSampleRole::PackagedOutput { target: self.target },
                self.final_pcm,
            ),
            ReferenceDecodedCarrierSelector::PostMetadataOutput => (
                self.delivered_path.as_path(),
                ReferenceDecodedSampleRole::PostMetadataOutput { target: self.target },
                self.final_pcm,
            ),
        }
    }

    /// Resolve one closed carrier selector to an opaque exact-path binding.
    pub fn decoded_carrier(
        &self,
        selector: ReferenceDecodedCarrierSelector,
    ) -> std::result::Result<ReferenceDecodedCarrier, ReferenceDecodeAuthorityError> {
        let (path, role, contract) = self.decoded_carrier_spec(selector);
        let authority = reference_decode_authority(role, contract)?;
        Ok(ReferenceDecodedCarrier {
            path: path.to_path_buf(),
            authority,
        })
    }

    /// Bind an externally held artifact path to a closed plan carrier selector.
    ///
    /// This is the fail-closed boundary used by post-metadata verification. The
    /// candidate must equal the exact planner-owned path before any decode command
    /// can be constructed.
    pub fn bind_decoded_carrier(
        &self,
        selector: ReferenceDecodedCarrierSelector,
        candidate_path: &Path,
    ) -> std::result::Result<ReferenceDecodedCarrier, ReferenceDecodeAuthorityError> {
        let carrier = self.decoded_carrier(selector)?;
        if carrier.path() != candidate_path {
            return Err(ReferenceDecodeAuthorityError::new(format!(
                "Reference {} carrier path mismatch: expected {}, got {}",
                selector.key(),
                carrier.path().display(),
                candidate_path.display(),
            )));
        }
        Ok(carrier)
    }
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
    /// W64 metadata mutation is unsafe with the pinned muxer and has no qualified alternative.
    W64MetadataMutationUnqualified,
    /// The required unseekable Float64 WAV carrier exceeds the proven 32-bit RIFF-size capacity.
    StreamedWavCapacity,
    /// Managed destination authority mismatch.
    ManagedDestination,
    /// Wave64 declared extents or exact PCM structure are invalid.
    W64StructuralIntegrity,
}

/// Stable exact message for one P0 Reference failure.
#[must_use]
pub fn reference_error_text(code: ReferenceErrorCode) -> &'static str {
    match code {
        ReferenceErrorCode::ManualUnavailable => "DSD-REF-P0-001: Manual DSD workflows are not available in this P0 build. Use Reference with a supported lossless target, or wait for Manual workflow support.",
        ReferenceErrorCode::LossyUnavailable => "DSD-REF-P0-002: Reference DSD reconstruction currently supports lossless delivery only. Choose FLAC, RIFF/WAV, RF64, W64, AIFF, WavPack, or ALAC/M4A, or wait for Reference-front-end Opus/MP3/AAC delivery.",
        ReferenceErrorCode::UnsupportedDsdRate => "DSD-REF-P0-003: Reference policy sox_ng_14_8_0_1_v16 supports DSD64, DSD128, and DSD256 only. Use a supported-rate source or wait for expanded-rate/Manual support.",
        ReferenceErrorCode::UnknownEncoding => "DSD-REF-P0-004: The DSD container or compression mode could not be identified as DSF/DSD, DSDIFF/DSD, DSDIFF/DST, or a supported SACD area. Reference will not guess the decoder path.",
        ReferenceErrorCode::UnsupportedChannels => "DSD-REF-P0-005: Reference policy sox_ng_14_8_0_1_v16 supports qualified mono and stereo cells only. Select a mono/stereo track or wait for multichannel qualification.",
        ReferenceErrorCode::Target882 => "DSD-REF-P0-006: Reference policy sox_ng_14_8_0_1_v16 has no qualified target-limited profile for {DSD128|DSD256} \u{2192} 88.2 kHz. Choose 44.1/48 kHz, choose 176.4 kHz or higher, or wait for a new policy.",
        ReferenceErrorCode::Target96 => "DSD-REF-P0-007: Reference policy sox_ng_14_8_0_1_v16 has no direct 96 kHz qualification for {DSD128|DSD256}. Choose 48 kHz, choose 176.4 kHz or higher, or wait for a new policy.",
        ReferenceErrorCode::WidebandDsd64 => "DSD-REF-P0-008: No Wideband profile is defined for DSD64. Select the Reference profile.",
        ReferenceErrorCode::WidebandDsd128Target => "DSD-REF-P0-008: DSD128 Wideband uses B4W and requires a target rate of at least 176.4 kHz. Select the Reference profile or choose 176.4 kHz or higher.",
        ReferenceErrorCode::WidebandDsd256Target => "DSD-REF-P0-008: DSD256 Wideband uses B6, whose 140 kHz stopband edge cannot fit this target; B6 is also unavailable under policy sox_ng_14_8_0_1_v16. Select Reference/B5.",
        ReferenceErrorCode::B6Unavailable => "DSD-REF-P0-009: B6 is represented but unqualified and unavailable under policy sox_ng_14_8_0_1_v16. Select Reference/B5 or wait for a later immutable policy.",
        ReferenceErrorCode::TerminalInt8 => "DSD-REF-P0-010: Reference policy sox_ng_14_8_0_1_v16 has no qualified 8-bit terminal realization. Choose 24-bit, Float32, or Float64 where supported.",
        ReferenceErrorCode::TerminalInt32 => "DSD-REF-P0-010: Reference policy sox_ng_14_8_0_1_v16 has no qualified 32-bit integer terminal realization. Choose 24-bit, Float32, or Float64 where supported.",
        ReferenceErrorCode::TargetDepth => "DSD-REF-P0-011: {target} does not support {depth} under Reference policy sox_ng_14_8_0_1_v16. Choose a target/depth pair listed by the policy.",
        ReferenceErrorCode::SingletonBatch => "DSD-REF-P0-012: Reference P0 supports singleton conversions only. Convert the selected files one at a time as independent singletons with independent gain, or wait for programme-wide Reference support.",
        ReferenceErrorCode::ContinuousProgramme => "DSD-REF-P0-013: Reference P0 cannot split a continuous DSD programme before reconstruction. This source must be processed as one programme before splitting; wait for programme-wide Reference support. Already independent files may be converted one at a time with independent gain.",
        ReferenceErrorCode::FrontEndUnattested => "DSD-REF-P0-014: Reference requires the qualified DST/SACD decode front-end for this source, but the decoder/extractor identity or qualification manifest does not match. Install the qualified toolchain or use an uncompressed DSF/DSDIFF source.",
        ReferenceErrorCode::Toolchain => "DSD-REF-P0-015: The installed Reference toolchain does not match policy sox_ng_14_8_0_1_v16 or failed its behavior probes. Activate/install the qualified toolchain; tonepoet will not substitute another decoder, analyzer, resampler, or encoder.",
        ReferenceErrorCode::UnsafeExactGain => "DSD-REF-P0-016: The requested {native-level|fixed} gain cannot satisfy the Reference \u{2212}1.000000000 dBTP ceiling for this measured source and terminal format. Reduce the fixed gain, choose Reference gain, or choose NormalizePeak with its modified/unqualified semantics.",
        ReferenceErrorCode::UnsupportedTargetRate => "DSD-REF-P0-017: Reference policy sox_ng_14_8_0_1_v16 supports target sample rates 44.1, 48, 88.2, 96, 176.4, 192, 352.8, 384, 705.6, and 768 kHz only. Choose one of those rates or wait for a later immutable policy.",
        ReferenceErrorCode::RiffSize => "DSD-REF-P0-018: The predicted RIFF/WAV output exceeds the qualified RIFF size limit. Choose RF64, W64, or another supported lossless target.",
        ReferenceErrorCode::CanonicalTarget => "DSD-REF-P0-019: The selected output container does not match the canonical Reference target or contains unrecognized output flags. Re-select the target.",
        ReferenceErrorCode::CompressedDstRateUnqualified => "DSD-REF-P0-021: Reference policy sox_ng_14_8_0_1_v16 qualifies predictive compressed DST only for stereo DSD64. Mono DSD64 and all DSD128/DSD256 predictive-DST cells remain unavailable because no matching independent-oracle corpus is present. Use an uncompressed DSF/DSDIFF source, decode with an independently verified tool outside Reference, or wait for a later immutable policy.",
        ReferenceErrorCode::Int16TerminalUnqualified => "DSD-REF-P0-022: Reference policy sox_ng_14_8_0_1_v16 does not enable Int16 because the commissioned SoX-ng Shibata realization has no qualified conservative worst-case peak bound. Choose Int24, Float32, or Float64, or wait for a later immutable policy with a derived Shibata bound.",
        ReferenceErrorCode::SacdFrontEndIntegrationUnqualified => "DSD-REF-P0-023: Reference policy sox_ng_14_8_0_1_v16 does not enable SACD DSD or DST extraction because the production extraction/materialization path is not yet qualified by pinned end-to-end SACD fixtures. Extract to a qualified DSF/DSDIFF source first or wait for a later immutable policy.",
        ReferenceErrorCode::W64MetadataMutationUnqualified => "DSD-REF-P0-024: Reference policy sox_ng_14_8_0_1_v16 cannot mutate metadata in W64 outputs because the pinned FFmpeg W64 muxer folds 8-byte alignment padding into the data chunk and can append a phantom sample. Disable the metadata stage for W64 delivery or choose another qualified lossless container; tonepoet will not invoke the unsafe muxer route.",
        ReferenceErrorCode::StreamedWavCapacity => "DSD-REF-P0-025: This programme exceeds the conservative streamed-WAV capacity admission retained by Reference policy sox_ng_14_8_0_1_v16. The pinned SoX-ng writer wraps RIFF/data sizes past the 32-bit boundary, so the inherited transport authority does not admit this duration even though the v15 analyzer itself is path-backed or headerless raw. Shorten or split the source before Reference conversion, reduce the target sample rate, or wait for a later append-only policy that lifts this retained bound.",
        ReferenceErrorCode::ManagedDestination => "DSD-REF-P0-020: The destination album has incompatible or incomplete tonepoet manifest authority. Choose a different output directory, repair/recover the existing transaction, or reconvert the album under one compatible Reference route; tonepoet will not merge or replace authority implicitly.",
        ReferenceErrorCode::W64StructuralIntegrity => "DSD-REF-P0-026: Reference policy sox_ng_14_8_0_1_v16 rejected a Wave64 carrier before publication because its declared RIFF/data extents, chunk traversal, alignment, PCM format, or exact frame count did not match its physical contents and upstream exact-frame authority. Re-run under the qualified writer closure or choose another lossless target; tonepoet will not publish malformed Wave64.",
    }
}

/// Return the stable policy rejection for a metadata mutation that has no qualified route.
#[must_use]
pub fn reference_metadata_mutation_rejection(
    target: ResolvedOutputTarget,
) -> Option<&'static str> {
    match target {
        ResolvedOutputTarget::WavW64 => Some(reference_error_text(
            ReferenceErrorCode::W64MetadataMutationUnqualified,
        )),
        _ => None,
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
            "DSD-REF-P0-006: Reference policy sox_ng_14_8_0_1_v16 has no qualified target-limited profile for {source} \u{2192} 88.2 kHz. Choose 44.1/48 kHz, choose 176.4 kHz or higher, or wait for a new policy."
        ),
        ReferenceErrorCode::Target96 => format!(
            "DSD-REF-P0-007: Reference policy sox_ng_14_8_0_1_v16 has no direct 96 kHz qualification for {source}. Choose 48 kHz, choose 176.4 kHz or higher, or wait for a new policy."
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
            "DSD-REF-P0-011: {} does not support {depth:?} under Reference policy sox_ng_14_8_0_1_v16. Choose a target/depth pair listed by the policy.",
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
/// widen an already-persisted cell. Policies v5 and later also reserve one analyzer
/// reporting quantum between the pre-terminal gain authority and the independent post-final
/// acceptance measurement; the public -1 dBTP ceiling itself is unchanged.
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
        PcmBitDepth::Int24 => (2_199_023_255_552, -1_010_002_327, "int24-tpdf-2lsb"),
        PcmBitDepth::Float32 => (1_099_511_627_776, -1_010_001_164, "float32-2^-23"),
        PcmBitDepth::Float64 => (
            2_147_487_744,
            -1_010_000_003,
            "float64-sox-s32-effects-half-lsb-plus-f64-2^-51",
        ),
        PcmBitDepth::Int8 | PcmBitDepth::Int32 => (u64::MAX, i64::MIN, "unsupported"),
    };
    let derivation = format!(
        "tonepoet-reference-terminal-bound/v3\0policy={}\0rate={}\0depth={:?}\0realization={}\0q63={}\0post_final_acceptance_reserve_dbnano={}\0safe_dbnano={}",
        DsdReferencePolicyVersion::SoxNg14801V16.key(),
        target_rate_hz,
        depth,
        realization,
        q63,
        DbNano::POST_FINAL_ACCEPTANCE_RESERVE.0,
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

fn sox_stats_peak_token_is_supported(token: &str) -> bool {
    if token == "-inf" {
        return true;
    }
    !token.contains(',')
        && !token.contains('e')
        && !token.contains('E')
        && !token.starts_with('+')
        && token != "inf"
        && token != "+inf"
        && !token.eq_ignore_ascii_case("nan")
        && token.parse::<DbNano>().is_ok()
}

/// Extract exactly one SoX `stats` peak-level line.
///
/// The pinned analyzer runs with `LC_ALL=C`. Mono output has one peak token;
/// multichannel output has an Overall token followed by one token per channel.
/// This function validates that exact shape and returns the Overall token.
pub fn extract_single_sox_stats_peak_report(
    stderr: &str,
    channels: u16,
) -> std::result::Result<String, String> {
    if channels == 0 {
        return Err("Reference SoX stats peak extraction requires at least one channel".to_string());
    }
    let mut reports = stderr
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            trimmed
                .strip_prefix("Pk lev dB")
                .map(str::split_ascii_whitespace)
                .map(|values| values.collect::<Vec<_>>())
                .filter(|values| !values.is_empty())
        })
        .collect::<Vec<_>>();
    match reports.len() {
        1 => {
            let values = reports.remove(0);
            let expected = if channels == 1 {
                1
            } else {
                usize::from(channels) + 1
            };
            if values.len() != expected {
                return Err(format!(
                    "Reference SoX stats peak report has {} value columns; expected {expected} for {channels} channel(s)",
                    values.len(),
                ));
            }
            if !values
                .iter()
                .all(|value| sox_stats_peak_token_is_supported(value))
            {
                return Err(
                    "Reference SoX stats peak report uses unsupported numeric syntax".to_string(),
                );
            }
            Ok(values[0].to_string())
        }
        0 => Err("Reference SoX stats output did not contain one Pk lev dB report".to_string()),
        _ => Err("Reference SoX stats output contained duplicate Pk lev dB reports".to_string()),
    }
}

/// Parse the strict SoX `stats` peak token and construct the conservative
/// measurement authority used by both production execution and release qualification.
pub fn parse_reference_sox_stats_true_peak_measurement(
    id: MeasurementId,
    scope: MeasurementScope,
    purpose: TruePeakPurpose,
    raw_peak_db: String,
    reporting_uncertainty: DbNano,
    analyzer_residual: DbNano,
    verified_silence: bool,
) -> std::result::Result<TruePeakMeasurement, String> {
    let reported = if raw_peak_db == "-inf" {
        if !verified_silence {
            return Err(
                "Reference SoX stats reported -inf without an independent signed-zero proof"
                    .to_string(),
            );
        }
        TruePeakValue::VerifiedSilence
    } else {
        if raw_peak_db.contains(',')
            || raw_peak_db.contains('e')
            || raw_peak_db.contains('E')
            || raw_peak_db.starts_with('+')
            || raw_peak_db == "inf"
            || raw_peak_db == "+inf"
            || raw_peak_db.eq_ignore_ascii_case("nan")
        {
            return Err("Reference SoX stats peak uses unsupported numeric syntax".to_string());
        }
        let value = raw_peak_db
            .parse::<DbNano>()
            .map_err(|err| format!("invalid Reference SoX stats peak: {err}"))?;
        if !(DbNano(-1_000_000_000_000)..=DbNano(100_000_000_000)).contains(&value) {
            return Err("Reference SoX stats peak is outside -1000 to +100 dBTP".to_string());
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
    let raw_json = format!(r#"{{"pk_lev_db":"{raw_peak_db}"}}"#);
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

/// Build the exact independent signed-zero scan command used after a `-inf` report.
///
/// The opaque carrier binds both the planner-owned path and the immutable decode
/// route. Float64 W64 is decoded only through the qualified SoX-ng raw-stream
/// mechanism; direct FFmpeg remains authorized for the other route-table cells.
#[must_use]
pub fn build_reference_silence_scan_command(
    carrier: &ReferenceDecodedCarrier,
    output: &Path,
) -> PlannedCommand {
    let input = carrier.path();
    let mut command = match carrier.authority().mechanism() {
        ReferenceDecodeMechanism::DirectFfmpeg => PlannedCommand::new(
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
            "Verify Reference signed-zero silence through FFmpeg",
        ),
        ReferenceDecodeMechanism::SoxFloat64W64RawStream => PlannedCommand::new(
            ToolIdentifier::Sox,
            vec![
                "-S".to_string(),
                "-D".to_string(),
                input.display().to_string(),
                "-t".to_string(),
                "raw".to_string(),
                "-e".to_string(),
                "floating-point".to_string(),
                "-b".to_string(),
                "64".to_string(),
                "-L".to_string(),
                output.display().to_string(),
            ],
            InputSource::Path(input.to_path_buf()),
            OutputSink::Path(output.to_path_buf()),
            None,
            "Verify Reference signed-zero silence through SoX-ng",
        ),
    };
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

/// Fixed oversampling factor used by policy v14+ true-peak measurement.
pub const REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR: u32 = 16;
/// Conservative analytic grid under-read bound for 16x sampling of a signal
/// bandlimited to the original Nyquist frequency, rounded upward to nanodecibels.
pub const REFERENCE_TRUE_PEAK_GRID_BOUND: DbNano = DbNano(41_925_957);
/// Empirically qualified residual allowance for the exact pinned SoX-ng resampler.
pub const REFERENCE_TRUE_PEAK_RESAMPLER_COMPONENT_LIMIT: DbNano = DbNano(58_074_043);
/// Complete analyzer residual: ideal grid plus pinned-resampler components.
pub const REFERENCE_TRUE_PEAK_ANALYZER_RESIDUAL: DbNano = DbNano(100_000_000);
/// Analyzer residual plus the existing one-sided reporting-quantization reserve.
pub const REFERENCE_TRUE_PEAK_ONE_SIDED_AUTHORITY: DbNano = DbNano(110_000_000);
/// Fixed startup reserve for one policy-v15 true-peak analyzer invocation.
pub const REFERENCE_TRUE_PEAK_DEADLINE_STARTUP_SECONDS: u64 = 120;
/// Conservative qualified throughput floor used by the policy-v15 deadline model.
pub const REFERENCE_TRUE_PEAK_MIN_OVERSAMPLED_SAMPLE_VALUES_PER_SECOND: u64 = 1_000_000;
/// Largest admitted analyzer workload after the streamed-WAV capacity gate.
pub const REFERENCE_TRUE_PEAK_MAX_ADMITTED_WORKLOAD_SAMPLE_VALUES: u64 = 8_589_934_480;
/// Largest policy-v15 analyzer deadline for any admitted Reference programme.
pub const REFERENCE_TRUE_PEAK_MAX_DEADLINE_SECONDS: u64 = 8_710;

/// Largest RIFF chunk-size field value representable by the streamed WAV carrier.
pub const REFERENCE_STREAMED_WAV_RIFF_SIZE_FIELD_MAX: u64 = u32::MAX as u64;
/// Measured bytes preceding the audio payload in SoX-ng's streamed Float64 WAV carrier.
pub const REFERENCE_STREAMED_WAV_HEADER_BYTES: u64 = 58;
/// Fixed non-audio contribution to SoX-ng's streamed Float64 WAV RIFF-size field.
///
/// RIFF size excludes the leading eight bytes of the measured streamed-WAV
/// header, so the size field is `audio_payload_bytes + 50`.
pub const REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES: u64 =
    REFERENCE_STREAMED_WAV_HEADER_BYTES - 8;
/// Largest admitted Float64 WAV audio payload before either 32-bit size field can wrap.
pub const REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES: u64 =
    REFERENCE_STREAMED_WAV_RIFF_SIZE_FIELD_MAX
        - REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES;
/// Bytes per sample in the required little-endian Float64 WAV carrier.
pub const REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE: u64 = 8;
/// One output frame reserved for nanosecond duration quantization and resampler endpoint rounding.
pub const REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES: u64 = 1;

fn validate_reference_streamed_wav_capacity(
    duration: Option<std::time::Duration>,
    contract: FinalPcmContract,
) -> Result<()> {
    let duration = duration.ok_or_else(|| {
        invalid_reference("source.duration", ReferenceErrorCode::StreamedWavCapacity)
    })?;
    let sample_frames = duration
        .as_nanos()
        .checked_mul(u128::from(contract.sample_rate_hz))
        .and_then(|value| value.checked_add(999_999_999))
        .map(|value| value / 1_000_000_000)
        .and_then(|value| {
            value.checked_add(u128::from(
                REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES,
            ))
        })
        .ok_or_else(|| {
            invalid_reference("source.duration", ReferenceErrorCode::StreamedWavCapacity)
        })?;
    let audio_payload_bytes = sample_frames
        .checked_mul(u128::from(contract.channels))
        .and_then(|value| {
            value.checked_mul(u128::from(REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE))
        })
        .ok_or_else(|| {
            invalid_reference("source.duration", ReferenceErrorCode::StreamedWavCapacity)
        })?;
    if audio_payload_bytes > u128::from(REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES) {
        return Err(invalid_reference(
            "source.duration",
            ReferenceErrorCode::StreamedWavCapacity,
        ));
    }
    Ok(())
}

/// Derive the policy-v15 true-peak analyzer deadline from admitted workload.
///
/// The workload is the guarded source-frame count multiplied by channels and
/// the frozen 16x measurement factor. One second is reserved for every started
/// block of one million oversampled sample values, in addition to a fixed
/// process-startup reserve. The same value is bound to both processes in the
/// Float32 FFmpeg-to-SoX route so the pipeline cannot fall back to the generic
/// one-hour command timeout.
pub fn reference_true_peak_measurement_deadline(
    duration: Option<std::time::Duration>,
    sample_rate_hz: u32,
    channels: u16,
) -> Result<std::time::Duration> {
    let duration = duration.ok_or_else(|| {
        invalid_reference("source.duration", ReferenceErrorCode::StreamedWavCapacity)
    })?;
    let guarded_frames = duration
        .as_nanos()
        .checked_mul(u128::from(sample_rate_hz))
        .and_then(|value| value.checked_add(999_999_999))
        .map(|value| value / 1_000_000_000)
        .and_then(|value| {
            value.checked_add(u128::from(
                REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES,
            ))
        })
        .ok_or_else(|| {
            invalid_reference("source.duration", ReferenceErrorCode::StreamedWavCapacity)
        })?;
    let workload_sample_values = guarded_frames
        .checked_mul(u128::from(channels))
        .and_then(|value| {
            value.checked_mul(u128::from(REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR))
        })
        .ok_or_else(|| {
            invalid_reference("source.duration", ReferenceErrorCode::StreamedWavCapacity)
        })?;
    if workload_sample_values
        > u128::from(REFERENCE_TRUE_PEAK_MAX_ADMITTED_WORKLOAD_SAMPLE_VALUES)
    {
        return Err(invalid_reference(
            "source.duration",
            ReferenceErrorCode::StreamedWavCapacity,
        ));
    }
    let throughput = u128::from(
        REFERENCE_TRUE_PEAK_MIN_OVERSAMPLED_SAMPLE_VALUES_PER_SECOND,
    );
    let workload_seconds = workload_sample_values
        .checked_add(throughput - 1)
        .map(|value| value / throughput)
        .ok_or_else(|| {
            invalid_reference("source.duration", ReferenceErrorCode::StreamedWavCapacity)
        })?;
    let deadline_seconds = workload_seconds
        .checked_add(u128::from(REFERENCE_TRUE_PEAK_DEADLINE_STARTUP_SECONDS))
        .ok_or_else(|| {
            invalid_reference("source.duration", ReferenceErrorCode::StreamedWavCapacity)
        })?;
    if deadline_seconds > u128::from(REFERENCE_TRUE_PEAK_MAX_DEADLINE_SECONDS) {
        return Err(invalid_reference(
            "source.duration",
            ReferenceErrorCode::StreamedWavCapacity,
        ));
    }
    let deadline_seconds = u64::try_from(deadline_seconds).map_err(|_| {
        invalid_reference("source.duration", ReferenceErrorCode::StreamedWavCapacity)
    })?;
    Ok(std::time::Duration::from_secs(deadline_seconds))
}

/// Largest total byte size admitted for ordinary disk-backed RIFF under policy v6.
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

/// Planner-owned deterministic scratch paths for one Reference execution.
///
/// Runtime materialization and verification must consume these exact paths;
/// they are included in [`ConversionPlan::cleanup_paths`] so no executor-only
/// temporary can escape the pure plan's cleanup authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceScratchPaths {
    /// Verified private copy of the admitted DSF/DSDIFF carrier.
    pub admitted_source: PathBuf,
    /// Publication temporary for the admitted private copy.
    pub admitted_source_temporary: PathBuf,
    /// Canonical uncompressed DSDIFF produced from an admitted DST source.
    pub canonical_dsd: PathBuf,
    /// Publication temporary for canonical DST decoding.
    pub canonical_dsd_temporary: PathBuf,
    /// Deterministic DSF extracted from a selected SACD track.
    pub sacd_extracted_source: PathBuf,
    /// Publication temporary for SACD extraction.
    pub sacd_extracted_source_temporary: PathBuf,
    /// Headerless f64le stream used for signed-zero verification.
    pub silence_scan: PathBuf,
}

impl ReferenceScratchPaths {
    /// Build the deterministic namespace for a work directory and admitted
    /// source class. Qualification uses this constructor to exercise the exact
    /// production paths without synthesizing a full conversion request.
    #[must_use]
    pub fn for_source_kind(work_dir: &Path, source_kind: &DsdSourceKind) -> Self {
        let extension = match source_kind {
            DsdSourceKind::DsfUncompressed | DsdSourceKind::SacdTrack { .. } => "dsf",
            DsdSourceKind::DsdiffUncompressed | DsdSourceKind::DsdiffDst => "dff",
            DsdSourceKind::UnknownDsdContainer => "dsd",
        };
        Self {
            admitted_source: work_dir.join(format!("reference-admitted-source.{extension}")),
            admitted_source_temporary: work_dir
                .join(format!("reference-admitted-source.tmp.{extension}")),
            canonical_dsd: work_dir.join("reference-canonical-dsd.dff"),
            canonical_dsd_temporary: work_dir.join("reference-canonical-dsd.tmp.dff"),
            sacd_extracted_source: work_dir.join("reference-sacd-track.dsf"),
            sacd_extracted_source_temporary: work_dir.join("reference-sacd-track.tmp.dsf"),
            silence_scan: work_dir.join("reference-silence-scan.f64le"),
        }
    }

    /// Return every deterministic scratch file in stable order.
    #[must_use]
    pub fn all(&self) -> [&Path; 7] {
        [
            self.admitted_source.as_path(),
            self.admitted_source_temporary.as_path(),
            self.canonical_dsd.as_path(),
            self.canonical_dsd_temporary.as_path(),
            self.sacd_extracted_source.as_path(),
            self.sacd_extracted_source_temporary.as_path(),
            self.silence_scan.as_path(),
        ]
    }
}

/// Derive the complete deterministic Reference scratch namespace from trusted
/// planner inputs. Runtime code must not invent sibling paths or PID-derived
/// names.
pub fn reference_scratch_paths(request: &PlanRequest) -> Result<ReferenceScratchPaths> {
    let work_dir = request.intermediate_dir.clone().ok_or_else(|| {
        invalid_reference("intermediate_dir", ReferenceErrorCode::CanonicalTarget)
    })?;
    let source_kind = request.source.dsd_source_kind.as_ref().ok_or_else(|| {
        invalid_reference("source.dsd_source_kind", ReferenceErrorCode::UnknownEncoding)
    })?;
    Ok(ReferenceScratchPaths::for_source_kind(&work_dir, source_kind))
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
    if settings.reference_policy != DsdReferencePolicyVersion::SoxNg14801V16 {
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
    validate_reference_streamed_wav_capacity(request.source.duration, final_pcm)?;
    let analyzer_rate_hz = target_rate_hz
        .checked_mul(REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR)
        .ok_or_else(|| {
            PlanningError::invalid_settings(
                "target_sample_rate",
                "Reference true-peak oversampling rate exceeds the planner's integer range",
            )
        })?;
    let analyzer_deadline = reference_true_peak_measurement_deadline(
        request.source.duration,
        target_rate_hz,
        channels,
    )?;

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
        target_rate_hz,
        channels,
        analyzer_rate_hz,
        analyzer_deadline,
        AnalyzerCarrierRoute::SoxPathOversampledStats,
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

    let post_measurement_route = if final_pcm.bit_depth == PcmBitDepth::Float32 {
        AnalyzerCarrierRoute::Float32FfmpegRawToSoxOversampledStats
    } else {
        AnalyzerCarrierRoute::SoxPathOversampledStats
    };
    steps.push(PlannedExecutionStep::Measurement(build_true_peak_measurement(
        post_id,
        TruePeakPurpose::PostFinalAcceptance,
        &qpcm,
        target_rate_hz,
        channels,
        analyzer_rate_hz,
        analyzer_deadline,
        post_measurement_route,
    )));
    operations.push(DsdReferenceOperation::MeasureTruePeak {
        measurement_id: post_id,
        scope: MeasurementScope::Plan,
        purpose: TruePeakPurpose::PostFinalAcceptance,
    });

    if target != ResolvedOutputTarget::WavW64 {
        steps.push(build_package_step(
            &qpcm,
            &final_work,
            target,
            final_pcm,
            &request.settings,
        )?);
        operations.push(DsdReferenceOperation::PackageLossless {
            target,
            sample_contract: final_pcm,
        });
    }

    let finalization = Some(Finalization::AtomicRename {
        from: final_work.clone(),
        to: request.output_path.clone(),
    });
    let scratch_paths = reference_scratch_paths(request)?;
    let mut cleanup_paths = vec![r64.clone(), qpcm.clone()];
    cleanup_paths.extend(
        scratch_paths
            .all()
            .into_iter()
            .map(Path::to_path_buf),
    );
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
        analyzer_deadline,
        r64_path: r64,
        qpcm_path: qpcm,
        packaged_path: final_work,
        delivered_path: request.output_path.clone(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnalyzerCarrierRoute {
    /// SoX-ng reads the path-backed W64 carrier, creates the qualified 16x
    /// measurement view, and reports its sample peak with `stats`.
    SoxPathOversampledStats,
    /// FFmpeg decodes Float32 W64 to headerless f64le because SoX-ng 14.8.0.1
    /// mis-scales that carrier; SoX-ng then creates and measures the same 16x view.
    Float32FfmpegRawToSoxOversampledStats,
}

fn build_true_peak_measurement(
    id: MeasurementId,
    purpose: TruePeakPurpose,
    input: &Path,
    sample_rate_hz: u32,
    channels: u16,
    oversampled_rate_hz: u32,
    expected_duration: std::time::Duration,
    route: AnalyzerCarrierRoute,
) -> PlannedMeasurement {
    let description = match purpose {
        TruePeakPurpose::GainAuthority => "Measure pre-final true peak",
        TruePeakPurpose::PostFinalAcceptance => "Measure post-final true peak",
    };
    let mut environment = BTreeMap::new();
    environment.insert("LC_ALL".to_string(), "C".to_string());

    let (input_stage, args, command_input) = match route {
        AnalyzerCarrierRoute::SoxPathOversampledStats => (
            None,
            vec![
                "-S".to_string(),
                "-D".to_string(),
                input.display().to_string(),
                "-n".to_string(),
                "rate".to_string(),
                "-v".to_string(),
                "-L".to_string(),
                "-s".to_string(),
                oversampled_rate_hz.to_string(),
                "stats".to_string(),
            ],
            InputSource::Path(input.to_path_buf()),
        ),
        AnalyzerCarrierRoute::Float32FfmpegRawToSoxOversampledStats => {
            let mut producer = PlannedCommand::new(
                ToolIdentifier::Ffmpeg,
                vec![
                    "-nostdin".to_string(),
                    "-hide_banner".to_string(),
                    "-nostats".to_string(),
                    "-loglevel".to_string(),
                    "error".to_string(),
                    "-i".to_string(),
                    input.display().to_string(),
                    "-map".to_string(),
                    "0:a:0".to_string(),
                    "-vn".to_string(),
                    "-sn".to_string(),
                    "-dn".to_string(),
                    "-c:a".to_string(),
                    "pcm_f64le".to_string(),
                    "-f".to_string(),
                    "f64le".to_string(),
                    "pipe:1".to_string(),
                ],
                InputSource::Path(input.to_path_buf()),
                OutputSink::Stdout,
                Some(expected_duration),
                "Decode Float32 W64 to exact f64le analyzer stream",
            );
            producer.environment_policy = CommandEnvironmentPolicy::ClearAndSet;
            producer.environment = environment.clone();
            (
                Some(producer),
                vec![
                    "-S".to_string(),
                    "-D".to_string(),
                    "-t".to_string(),
                    "raw".to_string(),
                    "-e".to_string(),
                    "floating-point".to_string(),
                    "-b".to_string(),
                    "64".to_string(),
                    "-L".to_string(),
                    "-r".to_string(),
                    sample_rate_hz.to_string(),
                    "-c".to_string(),
                    channels.to_string(),
                    "-".to_string(),
                    "-n".to_string(),
                    "rate".to_string(),
                    "-v".to_string(),
                    "-L".to_string(),
                    "-s".to_string(),
                    oversampled_rate_hz.to_string(),
                    "stats".to_string(),
                ],
                InputSource::Stdin,
            )
        }
    };

    let mut command = PlannedCommand::new(
        ToolIdentifier::Sox,
        args,
        command_input,
        OutputSink::Stdout,
        Some(expected_duration),
        description,
    );
    command.environment_policy = CommandEnvironmentPolicy::ClearAndSet;
    command.environment = environment;
    PlannedMeasurement {
        id,
        scope: MeasurementScope::Plan,
        purpose,
        input_stage,
        command,
        parser: MeasurementParser::SoxStatsPkLevDbV1,
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

fn reference_command_environment() -> BTreeMap<String, String> {
    BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
}

fn build_float64_wav_package_pipeline(
    input: &Path,
    output: &Path,
    target: ResolvedOutputTarget,
    contract: FinalPcmContract,
) -> Result<PlannedCommandPipeline> {
    if !matches!(target, ResolvedOutputTarget::WavRiff | ResolvedOutputTarget::WavRf64) {
        return Err(invalid_target_depth(
            "resolved_output_target",
            target,
            PcmBitDepth::Float64,
        ));
    }

    let mut producer = PlannedCommand::new(
        ToolIdentifier::Sox,
        vec![
            "-S".to_string(),
            "-D".to_string(),
            input.display().to_string(),
            "-t".to_string(),
            "raw".to_string(),
            "-e".to_string(),
            "floating-point".to_string(),
            "-b".to_string(),
            "64".to_string(),
            "-L".to_string(),
            "-".to_string(),
        ],
        InputSource::Path(input.to_path_buf()),
        OutputSink::Stdout,
        None,
        "Stream exact Float64 QPCM for lossless packaging",
    );
    producer.environment_policy = CommandEnvironmentPolicy::ClearAndSet;
    producer.environment = reference_command_environment();

    let mut args = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-f".to_string(),
        "f64le".to_string(),
        "-ar".to_string(),
        contract.sample_rate_hz.to_string(),
        "-ac".to_string(),
        contract.channels.to_string(),
        "-i".to_string(),
        "pipe:0".to_string(),
        "-map".to_string(),
        "0:a:0".to_string(),
        "-map_metadata".to_string(),
        "-1".to_string(),
        "-vn".to_string(),
        "-sn".to_string(),
        "-dn".to_string(),
        "-c:a".to_string(),
        "pcm_f64le".to_string(),
        "-f".to_string(),
        "wav".to_string(),
    ];
    if target == ResolvedOutputTarget::WavRf64 {
        args.extend(["-rf64".to_string(), "always".to_string()]);
    }
    args.push(output.display().to_string());

    let mut consumer = PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        args,
        InputSource::Stdin,
        OutputSink::Path(output.to_path_buf()),
        None,
        "Package streamed Float64 PCM without sample changes",
    );
    consumer.environment_policy = CommandEnvironmentPolicy::ClearAndSet;
    consumer.environment = reference_command_environment();

    Ok(PlannedCommandPipeline {
        producer,
        consumer,
        description: "Package Float64 QPCM through the qualified SoX-to-FFmpeg stream"
            .to_string(),
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
        PcmBitDepth::Float64 => {
            return Err(PlanningError::invalid_settings(
                "target_bit_depth",
                "Float64 RIFF/RF64 packaging must use the qualified typed stream",
            ));
        }
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
    command.environment = reference_command_environment();
    Ok(command)
}

fn build_package_step(
    input: &Path,
    output: &Path,
    target: ResolvedOutputTarget,
    contract: FinalPcmContract,
    settings: &crate::settings::PipelineSettings,
) -> Result<PlannedExecutionStep> {
    if contract.bit_depth == PcmBitDepth::Float64 {
        return build_float64_wav_package_pipeline(input, output, target, contract)
            .map(PlannedExecutionStep::Pipeline);
    }
    build_package_command(input, output, target, contract, settings)
        .map(PlannedExecutionStep::Command)
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

/// Canonical digest of the source-controlled v16 qualification artifact schema/content.
#[must_use]
pub fn qualification_manifest_digest() -> Sha256Digest {
    Sha256Digest::of_bytes(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/qualification/dsd_reference_sox_ng_14_8_0_1_v16.json"
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
        DsdReferencePolicyVersion::SoxNg14801V4
        | DsdReferencePolicyVersion::SoxNg14801V5
        | DsdReferencePolicyVersion::SoxNg14801V6
        | DsdReferencePolicyVersion::SoxNg14801V7
        | DsdReferencePolicyVersion::SoxNg14801V8
        | DsdReferencePolicyVersion::SoxNg14801V9
        | DsdReferencePolicyVersion::SoxNg14801V10
        | DsdReferencePolicyVersion::SoxNg14801V11
        | DsdReferencePolicyVersion::SoxNg14801V12
        | DsdReferencePolicyVersion::SoxNg14801V13
        | DsdReferencePolicyVersion::SoxNg14801V14 => {
            text.push_str("environment_identity=clear_and_set/v1\n");
            normalize_step_for_hash_v4
        }
        DsdReferencePolicyVersion::SoxNg14801V15 => {
            text.push_str("environment_identity=clear_and_set/v1\n");
            text.push_str("deadline_identity=workload/v1\n");
            normalize_step_for_hash_v15
        }
        DsdReferencePolicyVersion::SoxNg14801V16 => {
            text.push_str("environment_identity=clear_and_set/v1\n");
            text.push_str("deadline_identity=workload/v1\n");
            text.push_str("w64_structure_identity=exact/v1\n");
            normalize_step_for_hash_v15
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
        PlannedExecutionStep::Pipeline(pipeline) => format!(
            "pipeline:{}:{}:{}:{}",
            pipeline.producer.tool.program(),
            normalize_args(&pipeline.producer.args),
            pipeline.consumer.tool.program(),
            normalize_args(&pipeline.consumer.args),
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
        PlannedExecutionStep::Pipeline(pipeline) => format!(
            "pipeline:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            pipeline.producer.tool.program(),
            normalize_args(&pipeline.producer.args),
            normalize_input_source(&pipeline.producer.input),
            normalize_output_sink(&pipeline.producer.output),
            normalize_environment_policy(pipeline.producer.environment_policy),
            normalize_environment(&pipeline.producer.environment),
            pipeline.consumer.tool.program(),
            normalize_args(&pipeline.consumer.args),
            normalize_input_source(&pipeline.consumer.input),
            normalize_output_sink(&pipeline.consumer.output),
            normalize_environment_policy(pipeline.consumer.environment_policy),
            normalize_environment(&pipeline.consumer.environment),
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

fn normalize_step_for_hash_v15(step: &PlannedExecutionStep) -> String {
    let deadline_identity = match step {
        PlannedExecutionStep::Command(command) => {
            normalize_expected_duration(command.expected_duration)
        }
        PlannedExecutionStep::Pipeline(pipeline) => format!(
            "producer={};consumer={}",
            normalize_expected_duration(pipeline.producer.expected_duration),
            normalize_expected_duration(pipeline.consumer.expected_duration),
        ),
        PlannedExecutionStep::Measurement(measurement) => format!(
            "producer={};consumer={}",
            measurement
                .input_stage
                .as_ref()
                .map_or_else(|| "none".to_string(), |stage| {
                    normalize_expected_duration(stage.expected_duration)
                }),
            normalize_expected_duration(measurement.command.expected_duration),
        ),
        PlannedExecutionStep::DeferredCommand(_) => "not_applicable".to_string(),
    };
    format!(
        "{}:deadline={deadline_identity}",
        normalize_step_for_hash_v4(step)
    )
}

fn normalize_expected_duration(duration: Option<std::time::Duration>) -> String {
    duration.map_or_else(
        || "none".to_string(),
        |duration| format!("{}.{:09}", duration.as_secs(), duration.subsec_nanos()),
    )
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

    fn decode_contract(bit_depth: PcmBitDepth) -> FinalPcmContract {
        FinalPcmContract {
            sample_rate_hz: 176_400,
            channels: 2,
            sample_kind: bit_depth.sample_kind(),
            bit_depth,
            dither: if bit_depth == PcmBitDepth::Int24 {
                ReferenceDither::Tpdf
            } else {
                ReferenceDither::None
            },
        }
    }

    #[test]
    fn v7_decode_route_table_is_complete_unique_and_depth_native() {
        use std::collections::BTreeSet;

        let actual = REFERENCE_DECODE_ROUTE_RULES
            .iter()
            .copied()
            .map(|rule| {
                (
                    rule.role_class(),
                    rule.bit_depth(),
                    rule.mechanism(),
                    rule.hash_encoding(),
                )
            })
            .collect::<BTreeSet<_>>();
        let expected = BTreeSet::from([
            (
                ReferenceDecodeRoleClass::ReconstructionR64W64,
                PcmBitDepth::Float64,
                ReferenceDecodeMechanism::SoxFloat64W64RawStream,
                ReferenceSampleHashEncoding::Float64Le,
            ),
            (
                ReferenceDecodeRoleClass::TerminalQpcmW64,
                PcmBitDepth::Int24,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::SignedInt24Le,
            ),
            (
                ReferenceDecodeRoleClass::TerminalQpcmW64,
                PcmBitDepth::Float32,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::Float32Le,
            ),
            (
                ReferenceDecodeRoleClass::TerminalQpcmW64,
                PcmBitDepth::Float64,
                ReferenceDecodeMechanism::SoxFloat64W64RawStream,
                ReferenceSampleHashEncoding::Float64Le,
            ),
            (
                ReferenceDecodeRoleClass::PackagedW64,
                PcmBitDepth::Int24,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::SignedInt24Le,
            ),
            (
                ReferenceDecodeRoleClass::PackagedW64,
                PcmBitDepth::Float32,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::Float32Le,
            ),
            (
                ReferenceDecodeRoleClass::PackagedW64,
                PcmBitDepth::Float64,
                ReferenceDecodeMechanism::SoxFloat64W64RawStream,
                ReferenceSampleHashEncoding::Float64Le,
            ),
            (
                ReferenceDecodeRoleClass::PackagedNonW64,
                PcmBitDepth::Int24,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::SignedInt24Le,
            ),
            (
                ReferenceDecodeRoleClass::PackagedNonW64,
                PcmBitDepth::Float32,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::Float32Le,
            ),
            (
                ReferenceDecodeRoleClass::PackagedNonW64,
                PcmBitDepth::Float64,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::Float64Le,
            ),
            (
                ReferenceDecodeRoleClass::PostMetadataW64,
                PcmBitDepth::Int24,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::SignedInt24Le,
            ),
            (
                ReferenceDecodeRoleClass::PostMetadataW64,
                PcmBitDepth::Float32,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::Float32Le,
            ),
            (
                ReferenceDecodeRoleClass::PostMetadataW64,
                PcmBitDepth::Float64,
                ReferenceDecodeMechanism::SoxFloat64W64RawStream,
                ReferenceSampleHashEncoding::Float64Le,
            ),
            (
                ReferenceDecodeRoleClass::PostMetadataNonW64,
                PcmBitDepth::Int24,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::SignedInt24Le,
            ),
            (
                ReferenceDecodeRoleClass::PostMetadataNonW64,
                PcmBitDepth::Float32,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::Float32Le,
            ),
            (
                ReferenceDecodeRoleClass::PostMetadataNonW64,
                PcmBitDepth::Float64,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::Float64Le,
            ),
        ]);
        assert_eq!(actual.len(), REFERENCE_DECODE_ROUTE_RULES.len());
        assert_eq!(actual, expected);
        assert_eq!(
            ReferenceSampleHashEncoding::SignedInt24Le.ffmpeg_codec(),
            "pcm_s24le"
        );
        assert_eq!(
            ReferenceSampleHashEncoding::Float32Le.ffmpeg_codec(),
            "pcm_f32le"
        );
        assert_eq!(
            ReferenceSampleHashEncoding::Float64Le.ffmpeg_codec(),
            "pcm_f64le"
        );
        assert_eq!(
            REFERENCE_SAMPLE_HASH_FORMAT,
            "interleaved_depth_native_le_sha256"
        );
    }

    #[test]
    fn v7_decode_authority_rejects_invalid_pcm_contracts() {
        let mut contract = decode_contract(PcmBitDepth::Int24);
        contract.dither = ReferenceDither::None;
        assert!(reference_decode_authority(
            ReferenceDecodedSampleRole::TerminalQpcmW64,
            contract,
        )
        .is_err());

        let mut contract = decode_contract(PcmBitDepth::Float32);
        contract.channels = 0;
        assert!(reference_decode_authority(
            ReferenceDecodedSampleRole::TerminalQpcmW64,
            contract,
        )
        .is_err());
    }

    #[test]
    fn v7_float64_w64_direct_ffmpeg_route_is_rejected() {
        let contract = decode_contract(PcmBitDepth::Float64);
        for role in [
            ReferenceDecodedSampleRole::ReconstructionR64W64,
            ReferenceDecodedSampleRole::TerminalQpcmW64,
            ReferenceDecodedSampleRole::PackagedOutput {
                target: ResolvedOutputTarget::WavW64,
            },
            ReferenceDecodedSampleRole::PostMetadataOutput {
                target: ResolvedOutputTarget::WavW64,
            },
        ] {
            let authority = reference_decode_authority(role, contract)
                .expect("Float64 W64 has an authorized route");
            assert_eq!(
                authority.mechanism(),
                ReferenceDecodeMechanism::SoxFloat64W64RawStream
            );
            let error = validate_reference_decode_mechanism(
                role,
                contract,
                ReferenceDecodeMechanism::DirectFfmpeg,
            )
            .expect_err("direct FFmpeg must not authorize Float64 W64");
            assert!(error.to_string().contains("required route is sox_f64le_raw_stream"));
        }
    }

    #[test]
    fn reference_silence_scan_obeys_the_decode_route_table() {
        for (depth, expected_mechanism, expected_tool) in [
            (
                PcmBitDepth::Int24,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ToolIdentifier::Ffmpeg,
            ),
            (
                PcmBitDepth::Float32,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ToolIdentifier::Ffmpeg,
            ),
            (
                PcmBitDepth::Float64,
                ReferenceDecodeMechanism::SoxFloat64W64RawStream,
                ToolIdentifier::Sox,
            ),
        ] {
            let request = reference_request(
                DsdRate::Dsd64,
                88_200,
                ResolvedOutputTarget::WavW64,
                depth,
                DsdReconstructionSelection::Reference,
            );
            let plan = plan_reference_dsd(&request).expect("Reference W64 plan");
            let summary = plan.reference.as_ref().expect("Reference summary");
            let carrier = summary
                .decoded_carrier(ReferenceDecodedCarrierSelector::TerminalQpcm)
                .expect("terminal QPCM carrier route");
            assert_eq!(carrier.authority().mechanism(), expected_mechanism);
            let output = PathBuf::from("silence-scan.f64le");
            let command = build_reference_silence_scan_command(&carrier, &output);
            assert_eq!(command.tool, expected_tool);
            assert_eq!(command.input.as_path(), Some(carrier.path()));
            assert_eq!(command.output.as_path(), Some(output.as_path()));
            let input_arg = carrier.path().display().to_string();
            let output_arg = output.display().to_string();
            match expected_mechanism {
                ReferenceDecodeMechanism::DirectFfmpeg => assert!(
                    command.args.iter().map(String::as_str).eq([
                        "-y",
                        "-nostdin",
                        "-hide_banner",
                        "-loglevel",
                        "error",
                        "-i",
                        input_arg.as_str(),
                        "-map",
                        "0:a:0",
                        "-f",
                        "f64le",
                        "-acodec",
                        "pcm_f64le",
                        output_arg.as_str(),
                    ])
                ),
                ReferenceDecodeMechanism::SoxFloat64W64RawStream => assert!(
                    command.args.iter().map(String::as_str).eq([
                        "-S",
                        "-D",
                        input_arg.as_str(),
                        "-t",
                        "raw",
                        "-e",
                        "floating-point",
                        "-b",
                        "64",
                        "-L",
                        output_arg.as_str(),
                    ])
                ),
            }
            assert_eq!(
                command.environment_policy,
                CommandEnvironmentPolicy::ClearAndSet
            );
            assert_eq!(command.environment.get("LC_ALL").map(String::as_str), Some("C"));
        }

        let request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::WavW64,
            PcmBitDepth::Int24,
            DsdReconstructionSelection::Reference,
        );
        let plan = plan_reference_dsd(&request).expect("Reference W64 plan");
        let summary = plan.reference.as_ref().expect("Reference summary");
        let reconstruction = summary
            .decoded_carrier(ReferenceDecodedCarrierSelector::ReconstructionR64)
            .expect("reconstruction R64 carrier route");
        assert_eq!(
            reconstruction.authority().mechanism(),
            ReferenceDecodeMechanism::SoxFloat64W64RawStream
        );
        let output = PathBuf::from("r64-silence-scan.f64le");
        let command = build_reference_silence_scan_command(&reconstruction, &output);
        assert_eq!(command.tool, ToolIdentifier::Sox);
        assert_eq!(command.input.as_path(), Some(reconstruction.path()));
        assert_eq!(command.output.as_path(), Some(output.as_path()));
        let input_arg = reconstruction.path().display().to_string();
        let output_arg = output.display().to_string();
        assert!(command.args.iter().map(String::as_str).eq([
            "-S",
            "-D",
            input_arg.as_str(),
            "-t",
            "raw",
            "-e",
            "floating-point",
            "-b",
            "64",
            "-L",
            output_arg.as_str(),
        ]));
    }

    #[test]
    fn v7_carrier_binding_rejects_qpcm_path_with_riff_package_identity() {
        let request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::WavRiff,
            PcmBitDepth::Float64,
            DsdReconstructionSelection::Reference,
        );
        let plan = plan_reference_dsd(&request).expect("Float64 RIFF plan");
        let summary = plan.reference.as_ref().expect("Reference summary");

        let error = summary
            .bind_decoded_carrier(
                ReferenceDecodedCarrierSelector::PackagedOutput,
                &summary.qpcm_path,
            )
            .expect_err("QPCM W64 path must not impersonate the RIFF package");
        assert!(error.to_string().contains("carrier path mismatch"));

        let packaged = summary
            .decoded_carrier(ReferenceDecodedCarrierSelector::PackagedOutput)
            .expect("planner-owned RIFF package carrier");
        assert_eq!(packaged.path(), summary.packaged_path.as_path());
        assert_eq!(
            packaged.authority().mechanism(),
            ReferenceDecodeMechanism::DirectFfmpeg
        );

        let qpcm = summary
            .decoded_carrier(ReferenceDecodedCarrierSelector::TerminalQpcm)
            .expect("planner-owned QPCM carrier");
        assert_eq!(qpcm.path(), summary.qpcm_path.as_path());
        assert_eq!(
            qpcm.authority().mechanism(),
            ReferenceDecodeMechanism::SoxFloat64W64RawStream
        );

        let post_metadata_error = summary
            .bind_decoded_carrier(
                ReferenceDecodedCarrierSelector::PostMetadataOutput,
                &summary.packaged_path,
            )
            .expect_err("pre-finalization package path must not impersonate delivered output");
        assert!(post_metadata_error
            .to_string()
            .contains("carrier path mismatch"));

        let delivered = summary
            .decoded_carrier(ReferenceDecodedCarrierSelector::PostMetadataOutput)
            .expect("planner-owned delivered RIFF carrier");
        assert_eq!(delivered.path(), request.output_path.as_path());
        assert_eq!(summary.delivered_path, request.output_path);
    }

    #[test]
    fn v7_role_authority_selects_independent_float64_package_routes() {
        let contract = decode_contract(PcmBitDepth::Float64);
        let source = reference_decode_authority(
            ReferenceDecodedSampleRole::TerminalQpcmW64,
            contract,
        )
        .expect("Float64 QPCM route");
        let riff = reference_decode_authority(
            ReferenceDecodedSampleRole::PackagedOutput {
                target: ResolvedOutputTarget::WavRiff,
            },
            contract,
        )
        .expect("Float64 RIFF route");
        let rf64 = reference_decode_authority(
            ReferenceDecodedSampleRole::PackagedOutput {
                target: ResolvedOutputTarget::WavRf64,
            },
            contract,
        )
        .expect("Float64 RF64 route");
        assert_eq!(
            source.mechanism(),
            ReferenceDecodeMechanism::SoxFloat64W64RawStream
        );
        assert_eq!(riff.mechanism(), ReferenceDecodeMechanism::DirectFfmpeg);
        assert_eq!(rf64.mechanism(), ReferenceDecodeMechanism::DirectFfmpeg);
        assert_eq!(source.hash_encoding(), ReferenceSampleHashEncoding::Float64Le);
        assert_eq!(riff.hash_encoding(), ReferenceSampleHashEncoding::Float64Le);
    }

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

    #[test]
    fn v14_sox_stats_authority_is_strict_and_conservative() {
        let stderr = "DC offset   0.000000\nPk lev dB      -6.020599913\nRMS lev dB     -9.030899870\n";
        assert_eq!(
            extract_single_sox_stats_peak_report(stderr, 1).unwrap(),
            "-6.020599913"
        );
        assert!(extract_single_sox_stats_peak_report("no peak", 1).is_err());
        assert!(
            extract_single_sox_stats_peak_report(&format!("{stderr}{stderr}"), 1).is_err()
        );
        assert!(
            extract_single_sox_stats_peak_report("Pk lev dB -6.0 trailing", 1).is_err()
        );
        assert_eq!(
            extract_single_sox_stats_peak_report(
                "             Overall     Left      Right\nPk lev dB      -6.02     -6.02     -9.03\n",
                2,
            )
            .unwrap(),
            "-6.02"
        );
        assert!(
            extract_single_sox_stats_peak_report("Pk lev dB -6.02 -6.02", 2).is_err()
        );

        let parsed = parse_reference_sox_stats_true_peak_measurement(
            MeasurementId(9),
            MeasurementScope::Plan,
            TruePeakPurpose::GainAuthority,
            "-6.020599913".to_string(),
            DbNano(10_000_000),
            DbNano(100_000_000),
            false,
        )
        .unwrap();
        assert_eq!(parsed.reported, TruePeakValue::Finite(DbNano(-6_020_599_913)));
        assert_eq!(
            parsed.conservative_upper,
            TruePeakValue::Finite(DbNano(-5_910_599_913))
        );
        assert_eq!(parsed.raw_json, r#"{"pk_lev_db":"-6.020599913"}"#);
        assert!(parse_reference_sox_stats_true_peak_measurement(
            MeasurementId(10),
            MeasurementScope::Plan,
            TruePeakPurpose::GainAuthority,
            "-inf".to_string(),
            DbNano::ZERO,
            DbNano::ZERO,
            false,
        )
        .is_err());
        assert_eq!(
            parse_reference_sox_stats_true_peak_measurement(
                MeasurementId(10),
                MeasurementScope::Plan,
                TruePeakPurpose::GainAuthority,
                "-inf".to_string(),
                DbNano::ZERO,
                DbNano::ZERO,
                true,
            )
            .unwrap()
            .reported,
            TruePeakValue::VerifiedSilence
        );
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
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V5).unwrap(),
            r#""sox_ng_14_8_0_1_v5""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V6).unwrap(),
            r#""sox_ng_14_8_0_1_v6""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V7).unwrap(),
            r#""sox_ng_14_8_0_1_v7""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V8).unwrap(),
            r#""sox_ng_14_8_0_1_v8""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V9).unwrap(),
            r#""sox_ng_14_8_0_1_v9""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V10).unwrap(),
            r#""sox_ng_14_8_0_1_v10""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V11).unwrap(),
            r#""sox_ng_14_8_0_1_v11""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V12).unwrap(),
            r#""sox_ng_14_8_0_1_v12""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V13).unwrap(),
            r#""sox_ng_14_8_0_1_v13""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V14).unwrap(),
            r#""sox_ng_14_8_0_1_v14""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V15).unwrap(),
            r#""sox_ng_14_8_0_1_v15""#
        );
        assert_eq!(
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V16).unwrap(),
            r#""sox_ng_14_8_0_1_v16""#
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
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v5""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V5
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v6""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V6
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v7""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V7
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v8""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V8
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v9""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V9
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v10""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V10
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v11""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V11
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v12""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V12
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v13""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V13
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v14""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V14
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v15""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V15
        );
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v16""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V16
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
            "invalid settings for dsd.from_dsd.profile: DSD-REF-P0-006: Reference policy sox_ng_14_8_0_1_v16 has no qualified target-limited profile for DSD128 \u{2192} 88.2 kHz. Choose 44.1/48 kHz, choose 176.4 kHz or higher, or wait for a new policy."
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
        settings.dsd = crate::settings::DsdSettings::native_v2();
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
            DsdReferencePolicyVersion::SoxNg14801V4,
            DsdReferencePolicyVersion::SoxNg14801V5,
            DsdReferencePolicyVersion::SoxNg14801V6,
            DsdReferencePolicyVersion::SoxNg14801V7,
            DsdReferencePolicyVersion::SoxNg14801V8,
            DsdReferencePolicyVersion::SoxNg14801V9,
            DsdReferencePolicyVersion::SoxNg14801V10,
            DsdReferencePolicyVersion::SoxNg14801V11,
            DsdReferencePolicyVersion::SoxNg14801V12,
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
            DsdReferencePolicyVersion::SoxNg14801V5,
            DsdReferencePolicyVersion::SoxNg14801V6,
            DsdReferencePolicyVersion::SoxNg14801V7,
            DsdReferencePolicyVersion::SoxNg14801V8,
            DsdReferencePolicyVersion::SoxNg14801V9,
            DsdReferencePolicyVersion::SoxNg14801V10,
            DsdReferencePolicyVersion::SoxNg14801V11,
            DsdReferencePolicyVersion::SoxNg14801V12,
            DsdReferencePolicyVersion::SoxNg14801V13,
            DsdReferencePolicyVersion::SoxNg14801V14,
            DsdReferencePolicyVersion::SoxNg14801V15,
            DsdReferencePolicyVersion::SoxNg14801V16,
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
            DsdReferencePolicyVersion::SoxNg14801V4,
            DsdReferencePolicyVersion::SoxNg14801V5,
            DsdReferencePolicyVersion::SoxNg14801V6,
            DsdReferencePolicyVersion::SoxNg14801V7,
            DsdReferencePolicyVersion::SoxNg14801V8,
            DsdReferencePolicyVersion::SoxNg14801V9,
            DsdReferencePolicyVersion::SoxNg14801V10,
            DsdReferencePolicyVersion::SoxNg14801V11,
            DsdReferencePolicyVersion::SoxNg14801V12,
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
    fn v15_oversampled_measurement_routes_deadlines_and_hash_identity_are_frozen() {
        let request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::WavW64,
            PcmBitDepth::Float64,
            DsdReconstructionSelection::Reference,
        );
        let plan = plan_reference_dsd(&request).unwrap();
        let summary = plan.reference.as_ref().expect("Reference summary");
        assert_eq!(
            summary.analyzer_deadline,
            std::time::Duration::from_secs(290)
        );
        let measurements = plan
            .steps()
            .iter()
            .filter_map(|step| match step {
                PlannedExecutionStep::Measurement(measurement) => Some(measurement),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(measurements.len(), 2);
        for measurement in &measurements {
            assert!(measurement.input_stage.is_none());
            assert_eq!(measurement.parser, MeasurementParser::SoxStatsPkLevDbV1);
            let carrier = measurement
                .carrier_path()
                .expect("v15 direct SoX measurement carrier is path-backed")
                .display()
                .to_string();
            assert_eq!(measurement.command.tool, ToolIdentifier::Sox);
            assert_eq!(measurement.command.input.as_path(), measurement.carrier_path());
            assert_eq!(measurement.command.output, OutputSink::Stdout);
            assert_eq!(
                measurement.command.args,
                [
                    "-S",
                    "-D",
                    carrier.as_str(),
                    "-n",
                    "rate",
                    "-v",
                    "-L",
                    "-s",
                    "1411200",
                    "stats",
                ]
                .map(str::to_string)
                .to_vec()
            );
            assert_eq!(
                measurement.command.environment_policy,
                CommandEnvironmentPolicy::ClearAndSet
            );
            assert_eq!(
                measurement.command.environment,
                BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
            );
            assert_eq!(
                measurement.command.expected_duration,
                Some(std::time::Duration::from_secs(290))
            );
        }

        let measurement = measurements[0];
        let baseline = normalize_step_for_hash_v15(&PlannedExecutionStep::Measurement(
            measurement.clone(),
        ));
        let mut changed_rate = measurement.clone();
        changed_rate.command.args[8] = "705600".to_string();
        assert_ne!(
            baseline,
            normalize_step_for_hash_v15(&PlannedExecutionStep::Measurement(changed_rate))
        );
        let mut changed_parser = measurement.clone();
        changed_parser.parser = MeasurementParser::FfmpegLoudnormInputTpV3;
        assert_ne!(
            baseline,
            normalize_step_for_hash_v15(&PlannedExecutionStep::Measurement(changed_parser))
        );
        let mut changed_transport = measurement.clone();
        changed_transport.command.input = InputSource::Stdin;
        assert_ne!(
            baseline,
            normalize_step_for_hash_v15(&PlannedExecutionStep::Measurement(changed_transport))
        );
        let mut changed_environment = measurement.clone();
        changed_environment
            .command
            .environment
            .insert("LC_ALL".to_string(), "en_US.UTF-8".to_string());
        assert_ne!(
            baseline,
            normalize_step_for_hash_v15(&PlannedExecutionStep::Measurement(changed_environment))
        );
        let mut changed_deadline = measurement.clone();
        changed_deadline.command.expected_duration = Some(std::time::Duration::from_secs(291));
        assert_ne!(
            baseline,
            normalize_step_for_hash_v15(&PlannedExecutionStep::Measurement(changed_deadline))
        );

        let f32_request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::WavW64,
            PcmBitDepth::Float32,
            DsdReconstructionSelection::Reference,
        );
        let f32_plan = plan_reference_dsd(&f32_request).unwrap();
        let post = f32_plan
            .steps()
            .iter()
            .filter_map(|step| match step {
                PlannedExecutionStep::Measurement(measurement)
                    if measurement.purpose == TruePeakPurpose::PostFinalAcceptance =>
                {
                    Some(measurement)
                }
                _ => None,
            })
            .next()
            .expect("Float32 plan has a post-terminal measurement");
        let producer = post
            .input_stage
            .as_ref()
            .expect("Float32 post measurement has a typed FFmpeg producer");
        assert_eq!(post.parser, MeasurementParser::SoxStatsPkLevDbV1);
        let carrier = post
            .carrier_path()
            .expect("Float32 measurement carrier is path-backed")
            .display()
            .to_string();
        assert_eq!(producer.tool, ToolIdentifier::Ffmpeg);
        assert_eq!(producer.input.as_path(), post.carrier_path());
        assert_eq!(producer.output, OutputSink::Stdout);
        assert_eq!(
            producer.args,
            [
                "-nostdin", "-hide_banner", "-nostats", "-loglevel", "error", "-i",
                carrier.as_str(), "-map", "0:a:0", "-vn", "-sn", "-dn", "-c:a",
                "pcm_f64le", "-f", "f64le", "pipe:1",
            ]
            .map(str::to_string)
            .to_vec()
        );
        assert_eq!(post.command.tool, ToolIdentifier::Sox);
        assert_eq!(post.command.input, InputSource::Stdin);
        assert_eq!(
            post.command.args,
            [
                "-S", "-D", "-t", "raw", "-e", "floating-point", "-b", "64", "-L",
                "-r", "88200", "-c", "2", "-", "-n", "rate", "-v", "-L", "-s",
                "1411200", "stats",
            ]
            .map(str::to_string)
            .to_vec()
        );
        assert_eq!(producer.environment_policy, CommandEnvironmentPolicy::ClearAndSet);
        assert_eq!(post.command.environment_policy, CommandEnvironmentPolicy::ClearAndSet);
        assert_eq!(
            producer.environment,
            BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
        );
        assert_eq!(post.command.environment, producer.environment);
        assert_eq!(producer.expected_duration, post.command.expected_duration);
        assert_eq!(
            post.command.expected_duration,
            Some(std::time::Duration::from_secs(290))
        );
    }

    #[test]
    fn v9_float64_riff_and_rf64_use_typed_streamed_packaging() {
        for target in [ResolvedOutputTarget::WavRiff, ResolvedOutputTarget::WavRf64] {
            let request = reference_request(
                DsdRate::Dsd64,
                88_200,
                target,
                PcmBitDepth::Float64,
                DsdReconstructionSelection::Reference,
            );
            let plan = plan_reference_dsd(&request).expect("Float64 WAV plan");
            let summary = plan.reference.as_ref().expect("Reference summary");
            assert_eq!(summary.policy, DsdReferencePolicyVersion::SoxNg14801V16);
            assert_eq!(summary.qpcm_path.extension().and_then(|value| value.to_str()), Some("w64"));
            let pipeline = plan
                .steps()
                .iter()
                .find_map(|step| match step {
                    PlannedExecutionStep::Pipeline(value) => Some(value),
                    _ => None,
                })
                .expect("Float64 RIFF/RF64 uses typed package pipeline");
            assert_eq!(pipeline.producer.tool, ToolIdentifier::Sox);
            assert_eq!(pipeline.producer.input.as_path(), Some(summary.qpcm_path.as_path()));
            assert_eq!(pipeline.producer.output, OutputSink::Stdout);
            assert_eq!(pipeline.consumer.tool, ToolIdentifier::Ffmpeg);
            assert_eq!(pipeline.consumer.input, InputSource::Stdin);
            assert_eq!(pipeline.consumer.output.as_path(), Some(summary.packaged_path.as_path()));
            let qpcm_path = summary.qpcm_path.display().to_string();
            assert!(!pipeline
                .consumer
                .args
                .windows(2)
                .any(|window| window[0] == "-i" && window[1] == qpcm_path));
            assert!(pipeline
                .producer
                .args
                .windows(2)
                .any(|window| window[0] == "-t" && window[1] == "raw"));
            assert!(pipeline.producer.args.iter().any(|arg| arg == "-L"));
            assert!(pipeline
                .consumer
                .args
                .windows(2)
                .any(|window| window[0] == "-f" && window[1] == "f64le"));
            assert!(pipeline
                .consumer
                .args
                .windows(2)
                .any(|window| window[0] == "-ar" && window[1] == "88200"));
            assert!(pipeline
                .consumer
                .args
                .windows(2)
                .any(|window| window[0] == "-ac" && window[1] == "2"));
            assert!(pipeline
                .consumer
                .args
                .windows(2)
                .any(|window| window[0] == "-i" && window[1] == "pipe:0"));
            for command in [&pipeline.producer, &pipeline.consumer] {
                assert_eq!(command.environment_policy, CommandEnvironmentPolicy::ClearAndSet);
                assert_eq!(
                    command.environment,
                    BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
                );
            }
            assert_eq!(
                pipeline.consumer.args.windows(2).any(|window| {
                    window[0] == "-rf64" && window[1] == "always"
                }),
                target == ResolvedOutputTarget::WavRf64
            );

            let baseline = normalize_step_for_hash_v4(&PlannedExecutionStep::Pipeline(
                pipeline.clone(),
            ));
            let mut changed = pipeline.clone();
            changed.producer.environment_policy = CommandEnvironmentPolicy::InheritAndSet;
            assert_ne!(
                baseline,
                normalize_step_for_hash_v4(&PlannedExecutionStep::Pipeline(changed))
            );
        }
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
            "invalid settings for dsd.from_dsd.profile: DSD-REF-P0-007: Reference policy sox_ng_14_8_0_1_v16 has no direct 96 kHz qualification for DSD256. Choose 48 kHz, choose 176.4 kHz or higher, or wait for a new policy."
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
            "invalid settings for target_bit_depth: DSD-REF-P0-011: flac_native does not support Float32 under Reference policy sox_ng_14_8_0_1_v16. Choose a target/depth pair listed by the policy."
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
    fn streamed_wav_capacity_is_fail_closed_and_boundary_exact() {
        let contract = FinalPcmContract {
            sample_rate_hz: 1,
            channels: 1,
            sample_kind: SampleKind::Float,
            bit_depth: PcmBitDepth::Float64,
            dither: ReferenceDither::None,
        };
        let largest_admitted_duration_frames =
            REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES
                / REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE
                - REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES;
        validate_reference_streamed_wav_capacity(
            Some(std::time::Duration::from_secs(largest_admitted_duration_frames)),
            contract,
        )
        .expect("the exact bounded carrier must remain admitted");
        assert_eq!(
            validate_reference_streamed_wav_capacity(
                Some(std::time::Duration::from_secs(
                    largest_admitted_duration_frames + 1,
                )),
                contract,
            )
            .unwrap_err()
            .to_string(),
            format!(
                "invalid settings for source.duration: {}",
                reference_error_text(ReferenceErrorCode::StreamedWavCapacity)
            )
        );
        assert_eq!(
            validate_reference_streamed_wav_capacity(None, contract)
                .unwrap_err()
                .to_string(),
            format!(
                "invalid settings for source.duration: {}",
                reference_error_text(ReferenceErrorCode::StreamedWavCapacity)
            )
        );

        let overflow_contract = FinalPcmContract {
            sample_rate_hz: u32::MAX,
            channels: u16::MAX,
            ..contract
        };
        assert_eq!(
            validate_reference_streamed_wav_capacity(
                Some(std::time::Duration::MAX),
                overflow_contract,
            )
            .unwrap_err()
            .to_string(),
            format!(
                "invalid settings for source.duration: {}",
                reference_error_text(ReferenceErrorCode::StreamedWavCapacity)
            )
        );
    }

    #[test]
    fn true_peak_deadline_is_workload_derived_and_bounded_by_admission() {
        assert_eq!(
            REFERENCE_TRUE_PEAK_GRID_BOUND
                .checked_add(REFERENCE_TRUE_PEAK_RESAMPLER_COMPONENT_LIMIT),
            Some(REFERENCE_TRUE_PEAK_ANALYZER_RESIDUAL)
        );
        assert_eq!(
            REFERENCE_TRUE_PEAK_ANALYZER_RESIDUAL
                .checked_add(DbNano::POST_FINAL_ACCEPTANCE_RESERVE),
            Some(REFERENCE_TRUE_PEAK_ONE_SIDED_AUTHORITY)
        );
        assert_eq!(
            reference_true_peak_measurement_deadline(
                Some(std::time::Duration::from_secs(60)),
                48_000,
                2,
            )
            .expect("ordinary analyzer deadline resolves"),
            std::time::Duration::from_secs(213)
        );

        let largest_admitted_mono_frames =
            REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES
                / REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE;
        let admitted_duration_frames = largest_admitted_mono_frames
            - REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES;
        assert_eq!(
            reference_true_peak_measurement_deadline(
                Some(std::time::Duration::from_secs(admitted_duration_frames)),
                1,
                1,
            )
            .expect("maximum admitted analyzer deadline resolves"),
            std::time::Duration::from_secs(
                REFERENCE_TRUE_PEAK_MAX_DEADLINE_SECONDS,
            )
        );
        assert_eq!(
            reference_true_peak_measurement_deadline(None, 48_000, 2)
                .unwrap_err()
                .to_string(),
            format!(
                "invalid settings for source.duration: {}",
                reference_error_text(ReferenceErrorCode::StreamedWavCapacity)
            )
        );
    }

    #[test]
    fn streamed_wav_capacity_applies_to_every_terminal_depth_and_delivery_container() {
        for (target, depth) in [
            (ResolvedOutputTarget::FlacNative, PcmBitDepth::Int24),
            (ResolvedOutputTarget::WavRf64, PcmBitDepth::Float32),
            (ResolvedOutputTarget::WavW64, PcmBitDepth::Float64),
        ] {
            let mut request = reference_request(
                DsdRate::Dsd64,
                768_000,
                target,
                depth,
                DsdReconstructionSelection::Reference,
            );
            request.source.duration = Some(std::time::Duration::from_secs(5 * 60));
            plan_reference_dsd(&request).unwrap_or_else(|error| {
                panic!("valid sub-cap {target:?}/{depth:?} plan was rejected: {error}")
            });

            request.source.duration = Some(std::time::Duration::from_secs(6 * 60));
            assert_eq!(
                plan_reference_dsd(&request).unwrap_err().to_string(),
                format!(
                    "invalid settings for source.duration: {}",
                    reference_error_text(ReferenceErrorCode::StreamedWavCapacity)
                )
            );
        }
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
            let mut request = reference_request(
                DsdRate::Dsd64,
                88_200,
                ResolvedOutputTarget::WavPackNative,
                depth,
                DsdReconstructionSelection::Reference,
            );
            // Reference WavPack is non-hybrid: clear the generic UI default
            // exactly as the canonical-target validation test above does.
            request.settings.wavpack.correction_file = false;
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
        assert_eq!(
            plan_reference_dsd(&request).unwrap_err().to_string(),
            format!(
                "invalid settings for source.duration: {}",
                reference_error_text(ReferenceErrorCode::StreamedWavCapacity)
            )
        );
    }

    #[test]
    fn float32_w64_and_rf64_do_not_inherit_a_riff_intermediate_limit() {
        for target in [ResolvedOutputTarget::WavW64, ResolvedOutputTarget::WavRf64] {
            let mut request = reference_request(
                DsdRate::Dsd64,
                768_000,
                target,
                PcmBitDepth::Float32,
                DsdReconstructionSelection::Reference,
            );
            request.source.duration = Some(std::time::Duration::from_secs(5 * 60));
            let plan = plan_reference_dsd(&request).unwrap_or_else(|error| {
                panic!("valid sub-cap high-rate Float32 {target:?} plan was rejected: {error}")
            });
            let summary = plan.reference.as_ref().expect("Reference summary");
            assert_eq!(
                summary.qpcm_path.extension().and_then(|value| value.to_str()),
                Some("w64"),
                "high-rate Float32 must retain a W64 QPCM carrier"
            );
            assert!(!summary.qpcm_path.to_string_lossy().ends_with(".wav"));
            let package_commands = plan
                .steps()
                .iter()
                .skip(1)
                .filter_map(|step| match step {
                    PlannedExecutionStep::Command(command) => Some(command),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if target == ResolvedOutputTarget::WavW64 {
                assert_eq!(summary.qpcm_path, summary.packaged_path);
                assert!(package_commands.is_empty());
            } else {
                assert_ne!(summary.qpcm_path, summary.packaged_path);
                assert_eq!(package_commands.len(), 1);
                let package = package_commands[0];
                assert_eq!(package.input, InputSource::Path(summary.qpcm_path.clone()));
                assert_eq!(package.output, OutputSink::Path(summary.packaged_path.clone()));
                assert_eq!(
                    package.args,
                    vec![
                        "-y".to_string(),
                        "-hide_banner".to_string(),
                        "-nostdin".to_string(),
                        "-i".to_string(),
                        summary.qpcm_path.display().to_string(),
                        "-map".to_string(),
                        "0:a:0".to_string(),
                        "-map_metadata".to_string(),
                        "-1".to_string(),
                        "-vn".to_string(),
                        "-sn".to_string(),
                        "-dn".to_string(),
                        "-c:a".to_string(),
                        "pcm_f32le".to_string(),
                        "-f".to_string(),
                        "wav".to_string(),
                        "-rf64".to_string(),
                        "always".to_string(),
                        summary.packaged_path.display().to_string(),
                    ]
                );
            }
        }

        let mut riff = reference_request(
            DsdRate::Dsd64,
            768_000,
            ResolvedOutputTarget::WavRiff,
            PcmBitDepth::Float32,
            DsdReconstructionSelection::Reference,
        );
        riff.source.duration = Some(std::time::Duration::from_secs(15 * 60));
        assert!(plan_reference_dsd(&riff)
            .unwrap_err()
            .to_string()
            .contains("DSD-REF-P0-018"));
    }

    #[test]
    fn float64_w64_and_rf64_use_headerless_streaming_without_a_riff_intermediate_limit() {
        for target in [ResolvedOutputTarget::WavW64, ResolvedOutputTarget::WavRf64] {
            let mut request = reference_request(
                DsdRate::Dsd64,
                768_000,
                target,
                PcmBitDepth::Float64,
                DsdReconstructionSelection::Reference,
            );
            request.source.duration = Some(std::time::Duration::from_secs(5 * 60));
            let plan = plan_reference_dsd(&request).unwrap_or_else(|error| {
                panic!("valid sub-cap high-rate Float64 {target:?} plan was rejected: {error}")
            });
            let summary = plan.reference.as_ref().expect("Reference summary");
            assert_eq!(
                summary.qpcm_path.extension().and_then(|value| value.to_str()),
                Some("w64")
            );
            assert!(!summary.qpcm_path.to_string_lossy().ends_with(".wav"));
            let package_pipelines = plan
                .steps()
                .iter()
                .filter_map(|step| match step {
                    PlannedExecutionStep::Pipeline(pipeline) => Some(pipeline),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let package_commands = plan
                .steps()
                .iter()
                .skip(1)
                .filter(|step| matches!(step, PlannedExecutionStep::Command(_)))
                .count();
            assert_eq!(package_commands, 0);
            if target == ResolvedOutputTarget::WavW64 {
                assert_eq!(summary.qpcm_path, summary.packaged_path);
                assert!(package_pipelines.is_empty());
            } else {
                assert_ne!(summary.qpcm_path, summary.packaged_path);
                assert_eq!(package_pipelines.len(), 1);
                let pipeline = package_pipelines[0];
                assert_eq!(
                    pipeline.producer.input.as_path(),
                    Some(summary.qpcm_path.as_path())
                );
                assert_eq!(pipeline.producer.output, OutputSink::Stdout);
                assert!(pipeline.producer.args.windows(2).any(|window| {
                    window[0] == "-t" && window[1] == "raw"
                }));
                assert!(pipeline.producer.args.iter().any(|arg| arg == "-L"));
                assert_eq!(pipeline.consumer.input, InputSource::Stdin);
                assert_eq!(
                    pipeline.consumer.output.as_path(),
                    Some(summary.packaged_path.as_path())
                );
                assert!(pipeline.consumer.args.windows(2).any(|window| {
                    window[0] == "-f" && window[1] == "f64le"
                }));
                assert!(pipeline.consumer.args.windows(2).any(|window| {
                    window[0] == "-ar" && window[1] == "768000"
                }));
                assert!(pipeline.consumer.args.windows(2).any(|window| {
                    window[0] == "-ac" && window[1] == "2"
                }));
                assert!(pipeline.consumer.args.windows(2).any(|window| {
                    window[0] == "-rf64" && window[1] == "always"
                }));
            }
        }
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
    fn v9_inherits_corrected_float64_effects_bound_and_preserves_other_terminal_bounds() {
        assert_eq!(DbNano::POST_FINAL_ACCEPTANCE_RESERVE, DbNano(10_000_000));
        let cases = [
            (PcmBitDepth::Int24, 2_199_023_255_552_u64, -1_010_002_327_i64),
            (PcmBitDepth::Float32, 1_099_511_627_776_u64, -1_010_001_164_i64),
            (PcmBitDepth::Float64, 2_147_487_744_u64, -1_010_000_003_i64),
        ];
        for (depth, expected_q63, expected_safe) in cases {
            let bound = terminal_realization_bound(176_400, depth);
            assert_eq!(bound.max_added_peak_fs_q63_ceil, expected_q63);
            assert_eq!(bound.safe_pre_terminal_ceiling_dbtp, DbNano(expected_safe));
            assert!(
                bound.safe_pre_terminal_ceiling_dbtp
                    <= DbNano::REFERENCE_CEILING
                        .checked_sub(DbNano::POST_FINAL_ACCEPTANCE_RESERVE)
                        .expect("reserve subtraction"),
                "{depth:?} failed to reserve the post-final analyzer quantum"
            );
        }
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
            (ReferenceErrorCode::UnsupportedDsdRate, "DSD-REF-P0-003: Reference policy sox_ng_14_8_0_1_v16 supports DSD64, DSD128, and DSD256 only. Use a supported-rate source or wait for expanded-rate/Manual support."),
            (ReferenceErrorCode::UnknownEncoding, "DSD-REF-P0-004: The DSD container or compression mode could not be identified as DSF/DSD, DSDIFF/DSD, DSDIFF/DST, or a supported SACD area. Reference will not guess the decoder path."),
            (ReferenceErrorCode::UnsupportedChannels, "DSD-REF-P0-005: Reference policy sox_ng_14_8_0_1_v16 supports qualified mono and stereo cells only. Select a mono/stereo track or wait for multichannel qualification."),
            (ReferenceErrorCode::Target882, "DSD-REF-P0-006: Reference policy sox_ng_14_8_0_1_v16 has no qualified target-limited profile for {DSD128|DSD256} \u{2192} 88.2 kHz. Choose 44.1/48 kHz, choose 176.4 kHz or higher, or wait for a new policy."),
            (ReferenceErrorCode::Target96, "DSD-REF-P0-007: Reference policy sox_ng_14_8_0_1_v16 has no direct 96 kHz qualification for {DSD128|DSD256}. Choose 48 kHz, choose 176.4 kHz or higher, or wait for a new policy."),
            (ReferenceErrorCode::WidebandDsd64, "DSD-REF-P0-008: No Wideband profile is defined for DSD64. Select the Reference profile."),
            (ReferenceErrorCode::WidebandDsd128Target, "DSD-REF-P0-008: DSD128 Wideband uses B4W and requires a target rate of at least 176.4 kHz. Select the Reference profile or choose 176.4 kHz or higher."),
            (ReferenceErrorCode::WidebandDsd256Target, "DSD-REF-P0-008: DSD256 Wideband uses B6, whose 140 kHz stopband edge cannot fit this target; B6 is also unavailable under policy sox_ng_14_8_0_1_v16. Select Reference/B5."),
            (ReferenceErrorCode::B6Unavailable, "DSD-REF-P0-009: B6 is represented but unqualified and unavailable under policy sox_ng_14_8_0_1_v16. Select Reference/B5 or wait for a later immutable policy."),
            (ReferenceErrorCode::TerminalInt8, "DSD-REF-P0-010: Reference policy sox_ng_14_8_0_1_v16 has no qualified 8-bit terminal realization. Choose 24-bit, Float32, or Float64 where supported."),
            (ReferenceErrorCode::TerminalInt32, "DSD-REF-P0-010: Reference policy sox_ng_14_8_0_1_v16 has no qualified 32-bit integer terminal realization. Choose 24-bit, Float32, or Float64 where supported."),
            (ReferenceErrorCode::TargetDepth, "DSD-REF-P0-011: {target} does not support {depth} under Reference policy sox_ng_14_8_0_1_v16. Choose a target/depth pair listed by the policy."),
            (ReferenceErrorCode::SingletonBatch, "DSD-REF-P0-012: Reference P0 supports singleton conversions only. Convert the selected files one at a time as independent singletons with independent gain, or wait for programme-wide Reference support."),
            (ReferenceErrorCode::ContinuousProgramme, "DSD-REF-P0-013: Reference P0 cannot split a continuous DSD programme before reconstruction. This source must be processed as one programme before splitting; wait for programme-wide Reference support. Already independent files may be converted one at a time with independent gain."),
            (ReferenceErrorCode::FrontEndUnattested, "DSD-REF-P0-014: Reference requires the qualified DST/SACD decode front-end for this source, but the decoder/extractor identity or qualification manifest does not match. Install the qualified toolchain or use an uncompressed DSF/DSDIFF source."),
            (ReferenceErrorCode::Toolchain, "DSD-REF-P0-015: The installed Reference toolchain does not match policy sox_ng_14_8_0_1_v16 or failed its behavior probes. Activate/install the qualified toolchain; tonepoet will not substitute another decoder, analyzer, resampler, or encoder."),
            (ReferenceErrorCode::UnsafeExactGain, "DSD-REF-P0-016: The requested {native-level|fixed} gain cannot satisfy the Reference \u{2212}1.000000000 dBTP ceiling for this measured source and terminal format. Reduce the fixed gain, choose Reference gain, or choose NormalizePeak with its modified/unqualified semantics."),
            (ReferenceErrorCode::UnsupportedTargetRate, "DSD-REF-P0-017: Reference policy sox_ng_14_8_0_1_v16 supports target sample rates 44.1, 48, 88.2, 96, 176.4, 192, 352.8, 384, 705.6, and 768 kHz only. Choose one of those rates or wait for a later immutable policy."),
            (ReferenceErrorCode::RiffSize, "DSD-REF-P0-018: The predicted RIFF/WAV output exceeds the qualified RIFF size limit. Choose RF64, W64, or another supported lossless target."),
            (ReferenceErrorCode::CanonicalTarget, "DSD-REF-P0-019: The selected output container does not match the canonical Reference target or contains unrecognized output flags. Re-select the target."),
            (ReferenceErrorCode::CompressedDstRateUnqualified, "DSD-REF-P0-021: Reference policy sox_ng_14_8_0_1_v16 qualifies predictive compressed DST only for stereo DSD64. Mono DSD64 and all DSD128/DSD256 predictive-DST cells remain unavailable because no matching independent-oracle corpus is present. Use an uncompressed DSF/DSDIFF source, decode with an independently verified tool outside Reference, or wait for a later immutable policy."),
            (ReferenceErrorCode::Int16TerminalUnqualified, "DSD-REF-P0-022: Reference policy sox_ng_14_8_0_1_v16 does not enable Int16 because the commissioned SoX-ng Shibata realization has no qualified conservative worst-case peak bound. Choose Int24, Float32, or Float64, or wait for a later immutable policy with a derived Shibata bound."),
            (ReferenceErrorCode::SacdFrontEndIntegrationUnqualified, "DSD-REF-P0-023: Reference policy sox_ng_14_8_0_1_v16 does not enable SACD DSD or DST extraction because the production extraction/materialization path is not yet qualified by pinned end-to-end SACD fixtures. Extract to a qualified DSF/DSDIFF source first or wait for a later immutable policy."),
            (ReferenceErrorCode::W64MetadataMutationUnqualified, "DSD-REF-P0-024: Reference policy sox_ng_14_8_0_1_v16 cannot mutate metadata in W64 outputs because the pinned FFmpeg W64 muxer folds 8-byte alignment padding into the data chunk and can append a phantom sample. Disable the metadata stage for W64 delivery or choose another qualified lossless container; tonepoet will not invoke the unsafe muxer route."),
            (ReferenceErrorCode::StreamedWavCapacity, "DSD-REF-P0-025: This programme exceeds the conservative streamed-WAV capacity admission retained by Reference policy sox_ng_14_8_0_1_v16. The pinned SoX-ng writer wraps RIFF/data sizes past the 32-bit boundary, so the inherited transport authority does not admit this duration even though the v15 analyzer itself is path-backed or headerless raw. Shorten or split the source before Reference conversion, reduce the target sample rate, or wait for a later append-only policy that lifts this retained bound."),
            (ReferenceErrorCode::ManagedDestination, "DSD-REF-P0-020: The destination album has incompatible or incomplete tonepoet manifest authority. Choose a different output directory, repair/recover the existing transaction, or reconvert the album under one compatible Reference route; tonepoet will not merge or replace authority implicitly."),
            (ReferenceErrorCode::W64StructuralIntegrity, "DSD-REF-P0-026: Reference policy sox_ng_14_8_0_1_v16 rejected a Wave64 carrier before publication because its declared RIFF/data extents, chunk traversal, alignment, PCM format, or exact frame count did not match its physical contents and upstream exact-frame authority. Re-run under the qualified writer closure or choose another lossless target; tonepoet will not publish malformed Wave64."),
        ];
        let mut messages = std::collections::BTreeSet::new();
        for (code, exact) in expected {
            let actual = reference_error_text(code);
            assert_eq!(actual, exact, "drifted exact text for {code:?}");
            assert!(messages.insert(actual), "duplicate exact error message: {actual}");
        }
    }
}
