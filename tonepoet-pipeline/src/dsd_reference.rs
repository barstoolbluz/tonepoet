//! Qualified P0 Reference DSD-to-PCM policy and pure planning primitives.
//!
//! This module intentionally contains no filesystem or process I/O. Runtime
//! source materialization, tool attestation, measurement execution, publication,
//! and qualification reporting live in the orchestrator crate.

use crate::enums::{
    AudioFormat, BitDepthTarget, DsdRate, PcmBitDepth, RateTarget, SampleKind, TruePeakScanTier,
    TruePeakScope,
};
use crate::error::{PlanningError, Result};
use crate::plan::{
    CommandEnvironmentPolicy, ConversionPlan, Finalization, InputSource, OutputSink, PlanRequest,
    PlannedCommand, PlannedCommandPipeline,
};
use crate::qualification_schema::{
    REFERENCE_CERTIFIED_AUTHORITY_ENDPOINT, REFERENCE_CERTIFIED_EDGE_POLICY,
    REFERENCE_CERTIFIED_RECONSTRUCTION, REFERENCE_QPCM_READER_ID, REFERENCE_R64_READER_ID,
};
use crate::settings::SampleGainPolicy;
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
/// Stable historical policy key for the v16 exact Wave64 structural-integrity contract.
pub const DSD_REFERENCE_POLICY_V16_KEY: &str = "sox_ng_14_8_0_1_v16";
/// Stable policy key for the v17 SoX-ng source-lock correction.
pub const DSD_REFERENCE_POLICY_V17_KEY: &str = "sox_ng_14_8_0_1_v17";
/// Commissioned SoX-ng source revision.
pub const DSD_REFERENCE_SOX_NG_REVISION: &str =
    "9ed22fb3d813d6c02f67c254e57d162cee014a30";
/// Expected SoX-ng version string fragment.
pub const DSD_REFERENCE_SOX_NG_VERSION: &str = "14.8.0.1";
/// Stable current policy qualification artifact path.
pub const DSD_REFERENCE_QUALIFICATION_MANIFEST_PATH: &str =
    "qualification/dsd_reference_sox_ng_14_8_0_1_v17.json";

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
    /// Default Reference true-peak target.
    pub const DEFAULT_REFERENCE_TRUE_PEAK_TARGET: Self = Self(-1_000_000_000);
    /// Historical default NormalizePeak target retained for non-Reference callers.
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
    /// Corrected v16 exact Wave64 structural-integrity and consumer-compatibility contract. Retained for append-only decoding only.
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v16"))]
    SoxNg14801V16,
    /// Corrected v17 SoX-ng source-lock contract for Wave64-finalization-safe Reference execution.
    #[default]
    #[cfg_attr(feature = "serde", serde(rename = "sox_ng_14_8_0_1_v17"))]
    SoxNg14801V17,
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
            Self::SoxNg14801V17 => DSD_REFERENCE_POLICY_V17_KEY,
        }
    }
}

/// User-selected DSD-source pathway.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum DsdSourcePathway {
    /// Ordinary user-configurable DSD-to-PCM conversion. This is the raw settings default.
    #[default]
    Custom,
    /// Qualified Reference delivery pathway. Admission remains deliberately narrow.
    Reference,
    /// Reserved future Manual pathway; current planners refuse it deterministically.
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

/// Directional settings for DSD-source conversions.
///
/// Reference gain deliberately reuses [`SampleGainPolicy`], the same gain
/// vocabulary as Custom DSD and ordinary PCM. Reference accepts only `Off` or
/// `TruePeakNormalize`; normalization selects the same certified scan tiers as
/// Custom and defaults to [`TruePeakScanTier::Standard`]. Reference stores the
/// user-selected [`TruePeakScope`] directly in the shared gain policy. Album scope
/// resolves to Album only for an independent submitted album; a singleton or a
/// continuous image that requires pre-split processing resolves to Track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct DsdSourceSettings {
    /// Custom, Reference, or reserved Manual pathway.
    pub pathway: DsdSourcePathway,
    /// Immutable Reference policy ID.
    pub reference_policy: DsdReferencePolicyVersion,
    /// Standard or explicit Wideband reconstruction selection.
    pub profile: DsdReconstructionSelection,
    /// Reference gain policy. Qualified Reference accepts only Off or
    /// TruePeakNormalize. Custom DSD gain remains in `general_from_dsd.gain`.
    pub gain: SampleGainPolicy,
}

impl Default for DsdSourceSettings {
    fn default() -> Self {
        Self {
            pathway: DsdSourcePathway::Custom,
            reference_policy: DsdReferencePolicyVersion::SoxNg14801V17,
            profile: DsdReconstructionSelection::Reference,
            gain: Self::reference_auto_gain_default(),
        }
    }
}

impl DsdSourceSettings {
    /// Default qualified Reference normalization. Album is the user-facing
    /// default; programme resolution falls back to Track when no independent
    /// submitted album exists.
    #[must_use]
    pub const fn reference_auto_gain_default() -> SampleGainPolicy {
        SampleGainPolicy::TruePeakNormalize {
            target_dbtp: DbNano::DEFAULT_REFERENCE_TRUE_PEAK_TARGET,
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Standard,
        }
    }

    /// Validate and return the signed Reference true-peak target. Reference uses
    /// the same target domain as the Custom true-peak path.
    pub fn reference_true_peak_target_dbtp(self) -> Result<DbNano> {
        let SampleGainPolicy::TruePeakNormalize { target_dbtp, .. } = self.gain else {
            return Err(PlanningError::invalid_settings(
                "dsd.from_dsd.gain",
                "Reference automatic gain requires true-peak normalize",
            ));
        };
        if target_dbtp < DbNano::MIN_NORMALIZE_TARGET
            || target_dbtp > DbNano::MAX_NORMALIZE_TARGET
        {
            return Err(PlanningError::invalid_settings(
                "dsd.from_dsd.gain.target_dbtp",
                "Reference true-peak target must be between -12.000000000 and 0.000000000 dBTP",
            ));
        }
        Ok(target_dbtp)
    }

    /// Certified scan tier selected for the active Reference observer. Gain-off
    /// has no embedded gain policy, so it uses the Reference default (Standard).
    #[must_use]
    pub const fn reference_certified_scan_tier(self) -> TruePeakScanTier {
        match self.gain {
            SampleGainPolicy::TruePeakNormalize { scan, .. } => scan,
            _ => TruePeakScanTier::Standard,
        }
    }

    /// Resolve Reference gain scope from submitted programme shape without
    /// introducing a DSD-specific scope vocabulary.
    #[must_use]
    pub fn resolved_reference_gain_scope(
        self,
        programme: &ReferenceProgrammeScope,
    ) -> Option<TruePeakScope> {
        let scope = self.gain.scope()?;
        Some(match (scope, programme) {
            (
                TruePeakScope::Album,
                ReferenceProgrammeScope::IndependentAlbumBatch { .. },
            ) => TruePeakScope::Album,
            _ => TruePeakScope::Track,
        })
    }

    /// True when qualified Reference normalization is selected.
    #[must_use]
    pub const fn reference_auto_gain_selected(self) -> bool {
        matches!(self.gain, SampleGainPolicy::TruePeakNormalize { .. })
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
    /// Signed 32-bit little-endian PCM.
    SignedInt32Le,
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
            Self::SignedInt32Le => "pcm_s32le",
            Self::Float32Le => "pcm_f32le",
            Self::Float64Le => "pcm_f64le",
        }
    }

    /// Stable evidence key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::SignedInt24Le => "int24_le",
            Self::SignedInt32Le => "int32_le",
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
pub const REFERENCE_DECODE_ROUTE_RULES: [ReferenceDecodeRouteRule; 21] = [
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
        PcmBitDepth::Int32,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::SignedInt32Le,
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
        PcmBitDepth::Int32,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::SignedInt32Le,
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
        PcmBitDepth::Int32,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::SignedInt32Le,
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
        PcmBitDepth::Int32,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::SignedInt32Le,
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
        PcmBitDepth::Int32,
        ReferenceDecodeMechanism::DirectFfmpeg,
        ReferenceSampleHashEncoding::SignedInt32Le,
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
        PcmBitDepth::Int32 | PcmBitDepth::Float32 | PcmBitDepth::Float64 => ReferenceDither::None,
        PcmBitDepth::Int8 | PcmBitDepth::Int16 => {
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
    /// Certified true-peak normalization. Album scope remains unbound until
    /// the submitted-batch barrier derives one common scalar.
    TruePeakNormalize {
        /// Requested post-terminal true-peak target.
        target_dbtp: DbNano,
        /// Track or submitted-album authority.
        scope: TruePeakScope,
        /// Certified scan tier used for gain authority and post-final acceptance.
        scan: TruePeakScanTier,
        /// Runtime common scalar for Album scope; absent before the barrier and
        /// for Track scope.
        bound_gain: Option<DbNano>,
        /// Frozen terminal error bound for the selected PCM depth.
        terminal_bound: TerminalRealizationBound,
    },
    /// Unity post-reconstruction gain. The qualified Reference recipe still
    /// enforces its fixed -1 dBTP post-terminal acceptance ceiling.
    Off {
        /// Fixed Reference acceptance ceiling.
        ceiling: DbNano,
        /// Frozen terminal error bound for the selected PCM depth.
        terminal_bound: TerminalRealizationBound,
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

/// Exact Reference signal boundary observed by the certified in-process meter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum ReferenceObservationSubject {
    /// Protected Float64 Wave64 reconstruction before the Reference scalar.
    ProtectedR64,
    /// Authoritative terminal QPCM after gain/dither/format realization.
    TerminalQpcm,
}

/// Search completion state retained in Reference evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum ReferenceCertifiedSearchStatus {
    /// The certified search fully resolved every competitive region.
    Complete,
    /// Deterministic bounded work ended with a still-conservative finite interval.
    WorkLimited,
}

/// Typed result of one complete Reference certified-peak observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum ReferenceCertifiedPeakResult {
    /// Every sample in the complete declared programme was exactly signed zero.
    VerifiedSilence,
    /// Finite certified interval. Binary64 bit patterns are preserved exactly in evidence.
    Finite {
        /// Reporting-only selected-tier point estimate.
        point_linear_bits: u64,
        /// Conservative finite-peak lower endpoint.
        lower_linear_bits: u64,
        /// Conservative finite-peak upper endpoint; this is the ceiling authority.
        upper_linear_bits: u64,
        /// Deterministic certified-search completion state.
        status: ReferenceCertifiedSearchStatus,
    },
}

impl ReferenceCertifiedPeakResult {
    /// Conservative finite upper endpoint, or zero for verified silence.
    pub fn conservative_upper_linear(self) -> std::result::Result<f64, String> {
        match self {
            Self::VerifiedSilence => Ok(0.0),
            Self::Finite { upper_linear_bits, .. } => {
                let value = f64::from_bits(upper_linear_bits);
                if value.is_finite() && value > 0.0 {
                    Ok(value)
                } else {
                    Err("Reference certified peak has an invalid finite upper endpoint".to_string())
                }
            }
        }
    }

    /// Conservative finite lower endpoint, or zero for verified silence.
    pub fn conservative_lower_linear(self) -> std::result::Result<f64, String> {
        match self {
            Self::VerifiedSilence => Ok(0.0),
            Self::Finite { lower_linear_bits, .. } => {
                let value = f64::from_bits(lower_linear_bits);
                if value.is_finite() && value >= 0.0 {
                    Ok(value)
                } else {
                    Err("Reference certified peak has an invalid finite lower endpoint".to_string())
                }
            }
        }
    }
}

/// Complete evidence record for one Reference certified observation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct ReferenceCertifiedPeakObservation {
    /// Plan-local observation identity.
    pub id: MeasurementId,
    /// Singleton plan scope.
    pub scope: MeasurementScope,
    /// Gain-authority or post-terminal-acceptance purpose.
    pub purpose: TruePeakPurpose,
    /// Exact observed Reference boundary.
    pub subject: ReferenceObservationSubject,
    /// Certified observer implementation identity.
    pub observer_identity: String,
    /// Named finite reconstruction target.
    pub reconstruction: String,
    /// Edge policy.
    pub edge_policy: String,
    /// Search tier.
    pub scan_tier: String,
    /// Certificate endpoint with hard-ceiling authority.
    pub authority_endpoint: String,
    /// Independent carrier reader/decode authority.
    pub reader_authority: String,
    /// Exact sample rate observed.
    pub sample_rate_hz: u32,
    /// Exact channel count observed.
    pub channels: u16,
    /// Complete interleaved programme frame count.
    pub sample_frames: u64,
    /// SHA-256 of the exact validated Wave64 sample payload consumed by this reader.
    pub programme_sha256: Sha256Digest,
    /// True only after clean EOF and all structural/extent checks have succeeded.
    pub complete_reader: bool,
    /// Certified result.
    pub result: ReferenceCertifiedPeakResult,
    /// Digest of the canonical certificate/evidence payload produced by the adapter.
    pub certificate_sha256: Sha256Digest,
}

impl ReferenceCertifiedPeakObservation {
    /// Validate the immutable active observer/reader binding for this subject.
    pub fn validate_active_contract(&self) -> std::result::Result<(), String> {
        let expected_reader = match self.subject {
            ReferenceObservationSubject::ProtectedR64 => REFERENCE_R64_READER_ID,
            ReferenceObservationSubject::TerminalQpcm => REFERENCE_QPCM_READER_ID,
        };
        let Some(scan_tier) = crate::qualification_schema::reference_certified_scan_tier(
            self.scan_tier.as_str(),
        ) else {
            return Err("Reference certified observation uses an unknown scan tier".to_string());
        };
        if self.scope != MeasurementScope::Plan
            || self.observer_identity
                != crate::qualification_schema::reference_certified_observer_id(scan_tier)
            || self.reconstruction != REFERENCE_CERTIFIED_RECONSTRUCTION
            || self.edge_policy != REFERENCE_CERTIFIED_EDGE_POLICY
            || self.authority_endpoint != REFERENCE_CERTIFIED_AUTHORITY_ENDPOINT
            || self.reader_authority != expected_reader
            || !self.complete_reader
            || self.sample_rate_hz == 0
            || self.channels == 0
            || self.sample_frames == 0
        {
            return Err("Reference certified observation does not match the active finite-target/reader contract".to_string());
        }
        if let ReferenceCertifiedPeakResult::Finite {
            point_linear_bits,
            lower_linear_bits,
            upper_linear_bits,
            ..
        } = self.result
        {
            let point = f64::from_bits(point_linear_bits);
            let lower = f64::from_bits(lower_linear_bits);
            let upper = f64::from_bits(upper_linear_bits);
            if !point.is_finite()
                || point < 0.0
                || !lower.is_finite()
                || lower < 0.0
                || !upper.is_finite()
                || upper <= 0.0
                || lower > upper
            {
                return Err("Reference certified observation has an invalid finite interval".to_string());
            }
        }
        Ok(())
    }
}

/// Historical parser tag retained only to reproduce append-only v16 evidence
/// normalization. It is not an active production observer contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
enum MeasurementParser {
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

/// Historical planned measurement shape used only by v16 evidence normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
struct PlannedMeasurement {
    /// Unique measurement ID.
    id: MeasurementId,
    /// Singleton plan scope.
    scope: MeasurementScope,
    /// Gain or acceptance purpose.
    purpose: TruePeakPurpose,
    /// Optional typed producer whose stdout is connected directly to the analyzer stdin.
    /// Historical v1 measurements omit this field and read their path-backed carrier directly.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
    input_stage: Option<PlannedCommand>,
    /// Exact analyzer command.
    command: PlannedCommand,
    /// Strict parser.
    parser: MeasurementParser,
}

/// Historical deferred argv token used only by v16 evidence normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
enum PlannedArg {
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

/// Historical deferred command shape used only by v16 evidence normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
struct PlannedDeferredCommand {
    /// Built-in tool.
    tool: ToolIdentifier,
    /// Literal and bound argv tokens.
    args: Vec<PlannedArg>,
    /// Logical input.
    input: InputSource,
    /// Logical output.
    output: OutputSink,
    /// Environment inheritance policy.
    #[cfg_attr(feature = "serde", serde(default))]
    environment_policy: CommandEnvironmentPolicy,
    /// Stable environment.
    environment: BTreeMap<String, String>,
    /// User-facing description.
    description: String,
}

/// Historical v16 command/measurement vector retained only to validate
/// append-only evidence normalization and old qualification fixtures. It is not
/// part of [`ConversionPlan`] and cannot be executed by production.
#[derive(Debug, Clone, PartialEq, Eq)]
// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
enum LegacyReferenceExecutionStep {
    Command(PlannedCommand),
    Pipeline(PlannedCommandPipeline),
    Measurement(PlannedMeasurement),
    DeferredCommand(PlannedDeferredCommand),
}

/// High-level Reference operation summary used by provenance and diagnostics.
///
/// These entries are derived from the authoritative common typed plan. They
/// are not independently executable commands.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(rename_all = "snake_case"))]
pub enum DsdReferenceOperation {
    /// Decode/extract to canonical uncompressed DSD while retaining original-source provenance.
    DsdLosslessDecodeMaterialize {
        /// Selected front-end.
        front_end: DsdInputFrontEnd,
        /// Output contract.
        output_contract: CanonicalDsdContract,
    },
    /// Qualified protected reconstruction into the R64 proof carrier.
    DsdReferenceRender {
        /// Target rate.
        target_rate_hz: u32,
        /// Resolved profile.
        profile: ResolvedDsdProfile,
        /// Policy.
        policy: DsdReferencePolicyVersion,
    },
    /// Validate the protected R64 structure, extent, lattice, and reader premises.
    ValidateProtectedR64,
    /// Observe one exact boundary with the certified finite-target meter.
    ObserveCertifiedTruePeak {
        /// Observation identity.
        measurement_id: MeasurementId,
        /// Exact observed boundary.
        subject: ReferenceObservationSubject,
        /// Purpose.
        purpose: TruePeakPurpose,
        /// Independent complete-reader authority.
        reader_authority: String,
        /// Certified observer implementation identity.
        observer_identity: String,
    },
    /// Resolve the qualified Reference gain from the pre-terminal certificate.
    ResolveReferenceGain {
        /// Qualified gain policy.
        gain_policy: ResolvedGainPolicy,
        /// Pre-terminal observation.
        pre_terminal_measurement: MeasurementId,
    },
    /// One terminal gain/dither/format realization into QPCM.
    DsdReferenceFinalize {
        /// Final PCM contract.
        sample_contract: FinalPcmContract,
        /// Gain authority.
        gain_policy: ResolvedGainPolicy,
        /// Pre-terminal observation.
        pre_terminal_measurement: MeasurementId,
    },
    /// Validate QPCM structure, exact extent relation, and terminal lattice premises.
    ValidateTerminalQpcm,
    /// Lossless packaging.
    PackageLossless {
        /// Exact target.
        target: ResolvedOutputTarget,
        /// Final PCM contract.
        sample_contract: FinalPcmContract,
    },
    /// Verify format-specific packaged sample identity.
    VerifyPackageIdentity,
    /// Apply admitted native metadata/ReplayGain/artwork mutation.
    MutateMetadata,
    /// Revalidate structure and decoded sample identity after metadata mutation.
    VerifyPostMetadataIdentity,
    /// Production publication barrier, including matching release qualification.
    PublicationBarrier,
}

/// Pure Reference plan facts retained alongside the common typed plan.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct DsdReferencePlanSummary {
    /// Immutable sealed Reference policy ID.
    pub policy: DsdReferencePolicyVersion,
    /// Source-controlled Phase-5 candidate manifest digest. This does not imply promotion.
    pub qualification_candidate_manifest_digest: Sha256Digest,
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
    /// Planner-owned staged lossless package before atomic finalization; equal to
    /// `qpcm_path` for W64.
    pub packaged_path: PathBuf,
    /// Planner-owned delivered carrier after atomic finalization. Metadata, artwork,
    /// and ReplayGain mutation operate on this exact path.
    #[cfg_attr(feature = "serde", serde(default))]
    pub delivered_path: PathBuf,
    /// Common semantic-plan fingerprint for this exact normalized request.
    pub semantic_plan_hash_v1: Sha256Digest,
    /// Ordered non-executable operation summary.
    pub operations: Vec<DsdReferenceOperation>,
}

impl DsdReferencePlanSummary {
    /// Certified HQ1024 tier used by both Reference gain authority and
    /// post-terminal acceptance. Gain-off has no embedded scan setting and
    /// therefore uses the Reference default (Standard).
    #[must_use]
    pub const fn certified_scan_tier(&self) -> TruePeakScanTier {
        match self.gain_policy {
            ResolvedGainPolicy::TruePeakNormalize { scan, .. } => scan,
            ResolvedGainPolicy::Off { .. } => TruePeakScanTier::Standard,
        }
    }

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
    /// Target/depth mismatch.
    TargetDepth,
    /// Continuous programme rejected.
    ContinuousProgramme,
    /// DST/SACD front-end unattested.
    FrontEndUnattested,
    /// Toolchain mismatch.
    Toolchain,
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
        ReferenceErrorCode::UnsupportedDsdRate => "DSD-REF-P0-003: Reference policy sox_ng_14_8_0_1_v17 supports DSD64, DSD128, and DSD256 only. Use a supported-rate source or wait for expanded-rate/Manual support.",
        ReferenceErrorCode::UnknownEncoding => "DSD-REF-P0-004: The DSD container or compression mode could not be identified as DSF/DSD, DSDIFF/DSD, DSDIFF/DST, or a supported SACD area. Reference will not guess the decoder path.",
        ReferenceErrorCode::UnsupportedChannels => "DSD-REF-P0-005: Reference policy sox_ng_14_8_0_1_v17 supports qualified mono and stereo cells only. Select a mono/stereo track or wait for multichannel qualification.",
        ReferenceErrorCode::Target882 => "DSD-REF-P0-006: Reference policy sox_ng_14_8_0_1_v17 has no qualified target-limited profile for {DSD128|DSD256} \u{2192} 88.2 kHz. Choose 44.1/48 kHz, choose 176.4 kHz or higher, or wait for a new policy.",
        ReferenceErrorCode::Target96 => "DSD-REF-P0-007: Reference policy sox_ng_14_8_0_1_v17 has no direct 96 kHz qualification for {DSD128|DSD256}. Choose 48 kHz, choose 176.4 kHz or higher, or wait for a new policy.",
        ReferenceErrorCode::WidebandDsd64 => "DSD-REF-P0-008: No Wideband profile is defined for DSD64. Select the Reference profile.",
        ReferenceErrorCode::WidebandDsd128Target => "DSD-REF-P0-008: DSD128 Wideband uses B4W and requires a target rate of at least 176.4 kHz. Select the Reference profile or choose 176.4 kHz or higher.",
        ReferenceErrorCode::WidebandDsd256Target => "DSD-REF-P0-008: DSD256 Wideband uses B6, whose 140 kHz stopband edge cannot fit this target; B6 is also unavailable under policy sox_ng_14_8_0_1_v17. Select Reference/B5.",
        ReferenceErrorCode::B6Unavailable => "DSD-REF-P0-009: B6 is represented but unqualified and unavailable under policy sox_ng_14_8_0_1_v17. Select Reference/B5 or wait for a later immutable policy.",
        ReferenceErrorCode::TerminalInt8 => "DSD-REF-P0-010: Reference policy sox_ng_14_8_0_1_v17 has no qualified 8-bit terminal realization. Choose 24-bit, Float32, or Float64 where supported.",
        ReferenceErrorCode::TargetDepth => "DSD-REF-P0-011: {target} does not support {depth} under Reference policy sox_ng_14_8_0_1_v17. Choose a target/depth pair listed by the policy.",
        ReferenceErrorCode::ContinuousProgramme => "DSD-REF-P0-013: Reference P0 cannot split a continuous DSD programme before reconstruction. This source must be processed as one programme before splitting; wait for programme-wide Reference support. Already independent files may be converted one at a time with independent gain.",
        ReferenceErrorCode::FrontEndUnattested => "DSD-REF-P0-014: Reference requires the qualified DST/SACD decode front-end for this source, but the decoder/extractor identity or qualification manifest does not match. Install the qualified toolchain or use an uncompressed DSF/DSDIFF source.",
        ReferenceErrorCode::Toolchain => "DSD-REF-P0-015: The installed Reference toolchain does not match policy sox_ng_14_8_0_1_v17 or failed its behavior probes. Activate/install the qualified toolchain; tonepoet will not substitute another decoder, analyzer, resampler, or encoder.",
        ReferenceErrorCode::UnsupportedTargetRate => "DSD-REF-P0-017: Reference policy sox_ng_14_8_0_1_v17 supports target sample rates 44.1, 48, 88.2, 96, 176.4, 192, 352.8, 384, 705.6, and 768 kHz only. Choose one of those rates or wait for a later immutable policy.",
        ReferenceErrorCode::RiffSize => "DSD-REF-P0-018: The predicted RIFF/WAV output exceeds the qualified RIFF size limit. Choose RF64, W64, or another supported lossless target.",
        ReferenceErrorCode::CanonicalTarget => "DSD-REF-P0-019: The selected output container does not match the canonical Reference target or contains unrecognized output flags. Re-select the target.",
        ReferenceErrorCode::CompressedDstRateUnqualified => "DSD-REF-P0-021: Reference policy sox_ng_14_8_0_1_v17 qualifies predictive compressed DST only for stereo DSD64. Mono DSD64 and all DSD128/DSD256 predictive-DST cells remain unavailable because no matching independent-oracle corpus is present. Use an uncompressed DSF/DSDIFF source, decode with an independently verified tool outside Reference, or wait for a later immutable policy.",
        ReferenceErrorCode::Int16TerminalUnqualified => "DSD-REF-P0-022: Reference policy sox_ng_14_8_0_1_v17 does not enable Int16 because the commissioned SoX-ng Shibata realization has no qualified conservative worst-case peak bound. Choose Int24, Float32, or Float64, or wait for a later immutable policy with a derived Shibata bound.",
        ReferenceErrorCode::SacdFrontEndIntegrationUnqualified => "DSD-REF-P0-023: Reference policy sox_ng_14_8_0_1_v17 does not enable SACD DSD or DST extraction because the production extraction/materialization path is not yet qualified by pinned end-to-end SACD fixtures. Extract to a qualified DSF/DSDIFF source first or wait for a later immutable policy.",
        ReferenceErrorCode::W64MetadataMutationUnqualified => "DSD-REF-P0-024: Reference policy sox_ng_14_8_0_1_v17 cannot mutate metadata in W64 outputs because the pinned FFmpeg W64 muxer folds 8-byte alignment padding into the data chunk and can append a phantom sample. Disable the metadata stage for W64 delivery or choose another qualified lossless container; tonepoet will not invoke the unsafe muxer route.",
        ReferenceErrorCode::StreamedWavCapacity => "DSD-REF-P0-025: This programme exceeds the conservative streamed-WAV capacity admission retained by Reference policy sox_ng_14_8_0_1_v17. The pinned SoX-ng writer wraps RIFF/data sizes past the 32-bit boundary, so the inherited transport authority does not admit this duration even though the v15 analyzer itself is path-backed or headerless raw. Shorten or split the source before Reference conversion, reduce the target sample rate, or wait for a later append-only policy that lifts this retained bound.",
        ReferenceErrorCode::ManagedDestination => "DSD-REF-P0-020: The destination album has incompatible or incomplete tonepoet manifest authority. Choose a different output directory, repair/recover the existing transaction, or reconvert the album under one compatible Reference route; tonepoet will not merge or replace authority implicitly.",
        ReferenceErrorCode::W64StructuralIntegrity => "DSD-REF-P0-026: Reference policy sox_ng_14_8_0_1_v17 rejected a Wave64 carrier before publication because its declared RIFF/data extents, chunk traversal, alignment, PCM format, or exact frame count did not match its physical contents and upstream exact-frame authority. Re-run under the qualified writer closure or choose another lossless target; tonepoet will not publish malformed Wave64.",
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
            "DSD-REF-P0-006: Reference policy sox_ng_14_8_0_1_v17 has no qualified target-limited profile for {source} \u{2192} 88.2 kHz. Choose 44.1/48 kHz, choose 176.4 kHz or higher, or wait for a new policy."
        ),
        ReferenceErrorCode::Target96 => format!(
            "DSD-REF-P0-007: Reference policy sox_ng_14_8_0_1_v17 has no direct 96 kHz qualification for {source}. Choose 48 kHz, choose 176.4 kHz or higher, or wait for a new policy."
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
            "DSD-REF-P0-011: {} does not support {depth:?} under Reference policy sox_ng_14_8_0_1_v17. Choose a target/depth pair listed by the policy.",
            target.key()
        ),
    )
}

fn invalid_terminal_depth(field: &'static str, depth: PcmBitDepth) -> PlanningError {
    let code = match depth {
        PcmBitDepth::Int8 => ReferenceErrorCode::TerminalInt8,
        PcmBitDepth::Int32 => ReferenceErrorCode::TargetDepth,
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
        PcmBitDepth::Int32 => {}
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
            PcmBitDepth::Int24
                | PcmBitDepth::Int32
                | PcmBitDepth::Float32
                | PcmBitDepth::Float64
        ),
        ResolvedOutputTarget::AiffNative | ResolvedOutputTarget::WavPackNative => {
            matches!(depth, PcmBitDepth::Int24 | PcmBitDepth::Int32)
        }
        ResolvedOutputTarget::FlacNative => {
            matches!(depth, PcmBitDepth::Int24 | PcmBitDepth::Int32)
        }
        ResolvedOutputTarget::AlacM4a => depth == PcmBitDepth::Int24,
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
        PcmBitDepth::Int32 => (
            2_147_483_648,
            -1_010_000_003,
            "int32-sox-s32-effects-half-lsb",
        ),
        PcmBitDepth::Float64 => (
            2_147_487_744,
            -1_010_000_003,
            "float64-sox-s32-effects-half-lsb-plus-f64-2^-51",
        ),
        PcmBitDepth::Int8 => (u64::MAX, i64::MIN, "unsupported"),
    };
    let derivation = format!(
        "tonepoet-reference-terminal-bound/v3\0policy={}\0rate={}\0depth={:?}\0realization={}\0q63={}\0post_final_acceptance_reserve_dbnano={}\0safe_dbnano={}",
        DsdReferencePolicyVersion::SoxNg14801V17.key(),
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

/// Validate and resolve one gain policy for a concrete programme shape.
pub fn resolve_gain_policy_for_programme(
    settings: DsdSourceSettings,
    programme: &ReferenceProgrammeScope,
    runtime_album_gain_db: Option<DbNano>,
    target_rate_hz: u32,
    depth: PcmBitDepth,
) -> Result<ResolvedGainPolicy> {
    let terminal_bound = terminal_realization_bound(target_rate_hz, depth);
    match settings.gain {
        SampleGainPolicy::TruePeakNormalize { target_dbtp, scan, .. } => {
            settings.reference_true_peak_target_dbtp()?;
            let scope = settings
                .resolved_reference_gain_scope(programme)
                .expect("true-peak normalize has a scope");
            let bound_gain = match scope {
                TruePeakScope::Track => {
                    if runtime_album_gain_db.is_some() {
                        return Err(PlanningError::invalid_settings(
                            "dsd.runtime_album_gain_db",
                            "Track-scoped Reference true-peak normalization cannot consume submitted-album runtime authority",
                        ));
                    }
                    None
                }
                TruePeakScope::Album => runtime_album_gain_db,
            };
            Ok(ResolvedGainPolicy::TruePeakNormalize {
                target_dbtp,
                scope,
                scan,
                bound_gain,
                terminal_bound,
            })
        }
        SampleGainPolicy::Off => {
            if runtime_album_gain_db.is_some() {
                return Err(PlanningError::invalid_settings(
                    "dsd.runtime_album_gain_db",
                    "Reference gain-off cannot consume submitted-album runtime authority",
                ));
            }
            Ok(ResolvedGainPolicy::Off {
                ceiling: DbNano::REFERENCE_CEILING,
                terminal_bound,
            })
        }
        SampleGainPolicy::TruePeakGuard { .. } | SampleGainPolicy::FixedGain { .. } => {
            Err(PlanningError::invalid_settings(
                "dsd.from_dsd.gain",
                "Reference delivery accepts only true-peak normalize or off",
            ))
        }
    }
}

/// Resolve a standalone Reference policy. This helper is used by qualification
/// and unit tests that intentionally have no submitted-batch context.
pub fn resolve_gain_policy(
    settings: DsdSourceSettings,
    target_rate_hz: u32,
    depth: PcmBitDepth,
) -> Result<ResolvedGainPolicy> {
    resolve_gain_policy_for_programme(
        settings,
        &ReferenceProgrammeScope::Singleton,
        None,
        target_rate_hz,
        depth,
    )
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

/// Common-model Reference gain authority derived from one certified finite-target observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct ReferenceCertifiedGainAuthority {
    /// Requested Reference policy scalar before any compensated-mode reduction.
    pub requested_gain: DbNano,
    /// Scalar selected for the one terminal realization.
    pub selected_gain: DbNano,
    /// Active Reference policy ceiling.
    pub ceiling: DbNano,
    /// Physical terminal sample-error bound before reconstruction lifting.
    pub terminal_sample_error_linear_bits: u64,
    /// Same terminal error lifted into the certified reconstruction target.
    pub terminal_reconstructed_error_linear_bits: u64,
    /// Finite-programme ceiling-limited linear gain, absent for verified silence.
    pub maximum_linear_gain_bits: Option<u64>,
    /// True only when compensated Reference restoration was reduced for the ceiling.
    pub reduced_for_ceiling: bool,
}

fn reference_policy_ceiling_and_bound(
    policy: ResolvedGainPolicy,
) -> (DbNano, TerminalRealizationBound) {
    match policy {
        ResolvedGainPolicy::TruePeakNormalize {
            target_dbtp,
            terminal_bound,
            ..
        } => (target_dbtp, terminal_bound),
        ResolvedGainPolicy::Off {
            ceiling,
            terminal_bound,
        } => (ceiling, terminal_bound),
    }
}

fn reference_policy_certified_scan_tier(policy: ResolvedGainPolicy) -> TruePeakScanTier {
    match policy {
        ResolvedGainPolicy::TruePeakNormalize { scan, .. } => scan,
        // Gain-off still requires post-terminal Reference ceiling acceptance;
        // use the pathway default observer for both required observations.
        ResolvedGainPolicy::Off { .. } => TruePeakScanTier::Standard,
    }
}

fn validate_reference_observation_policy_scan(
    observation: &ReferenceCertifiedPeakObservation,
    policy: ResolvedGainPolicy,
) -> std::result::Result<(), String> {
    let scan = reference_policy_certified_scan_tier(policy);
    let expected_scan = crate::qualification_schema::reference_certified_scan_tier_name(scan);
    let expected_observer = crate::qualification_schema::reference_certified_observer_id(scan);
    if observation.scan_tier != expected_scan || observation.observer_identity != expected_observer {
        return Err(
            "Reference certified observation scan tier does not match the resolved Reference policy"
                .to_string(),
        );
    }
    Ok(())
}

fn reference_album_terminal_bound(
    terminal_bound: TerminalRealizationBound,
    reconstruction_linf_gain_upper: f64,
) -> Result<crate::dsd_album_gain::AlbumTerminalBound> {
    if !reconstruction_linf_gain_upper.is_finite() || reconstruction_linf_gain_upper <= 0.0 {
        return Err(PlanningError::invalid_settings(
            "dsd.reference.terminal_bound",
            "certified reconstruction operator bound is invalid",
        ));
    }
    let q63 = terminal_bound.max_added_peak_fs_q63_ceil;
    if q63 == u64::MAX || q63 >= (1_u64 << 53) {
        return Err(PlanningError::invalid_settings(
            "dsd.reference.terminal_bound",
            "qualified Reference terminal has no usable physical error bound",
        ));
    }
    let terminal_sample_error = (q63 as f64) / 9_223_372_036_854_775_808.0;
    let terminal_reconstructed_error =
        crate::dsd_album_gain::conservative_product_upper_nonnegative(
            terminal_sample_error,
            reconstruction_linf_gain_upper,
        )
        .map_err(|reason| PlanningError::invalid_settings("dsd.reference.terminal_bound", reason))?;
    Ok(crate::dsd_album_gain::AlbumTerminalBound {
        pre_gain_reconstructed_error_linear: 0.0,
        stored_sample_error_linear: Some(terminal_sample_error),
        post_gain_reconstructed_error_linear: terminal_reconstructed_error,
        domain: crate::dsd_album_gain::AlbumCeilingDomain::LosslessStoredPcm,
    })
}

/// Convert a protected-R64 certified observation into the exact constraint
/// consumed by the submitted-album gain barrier. This is the same terminal
/// error algebra used by `resolve_reference_certified_gain`; callers must not
/// derive a second Reference ceiling model.
pub fn reference_album_gain_constraint(
    observation: &ReferenceCertifiedPeakObservation,
    policy: ResolvedGainPolicy,
    reconstruction_linf_gain_upper: f64,
) -> Result<(crate::dsd_album_gain::AlbumPeakMeasurement, crate::dsd_album_gain::AlbumTerminalBound)> {
    observation
        .validate_active_contract()
        .map_err(|reason| PlanningError::invalid_settings("dsd.reference.observer", reason))?;
    validate_reference_observation_policy_scan(observation, policy)
        .map_err(|reason| PlanningError::invalid_settings("dsd.reference.observer", reason))?;
    if observation.subject != ReferenceObservationSubject::ProtectedR64
        || observation.purpose != TruePeakPurpose::GainAuthority
    {
        return Err(PlanningError::invalid_settings(
            "dsd.reference.observer",
            "Reference album gain requires the protected-R64 gain-authority observation",
        ));
    }
    let (_, terminal_bound) = reference_policy_ceiling_and_bound(policy);
    let terminal = reference_album_terminal_bound(
        terminal_bound,
        reconstruction_linf_gain_upper,
    )?;
    let measurement = match observation.result {
        ReferenceCertifiedPeakResult::VerifiedSilence => crate::dsd_album_gain::AlbumPeakMeasurement::Silence,
        ReferenceCertifiedPeakResult::Finite {
            point_linear_bits,
            ..
        } => {
            let point = f64::from_bits(point_linear_bits);
            let point_db = if point > 0.0 && point.is_finite() {
                let raw = 20.0 * point.log10();
                format!("{raw:.9}")
                    .parse::<DbNano>()
                    .map_err(|reason| PlanningError::invalid_settings("dsd.reference.observer", reason))?
            } else {
                // A zero reporting point can occur only on a finite interval
                // whose upper endpoint still proves non-silence. Preserve a
                // finite sentinel for reporting; it never participates in the
                // ceiling calculation.
                DbNano(i64::MIN)
            };
            crate::dsd_album_gain::AlbumPeakMeasurement::Finite {
                point_db,
                signal_upper_linear: observation
                    .result
                    .conservative_upper_linear()
                    .map_err(|reason| PlanningError::invalid_settings("dsd.reference.observer", reason))?,
            }
        }
    };
    Ok((measurement, terminal))
}

/// Resolve the sealed Reference scalar from the certified pre-terminal upper
/// endpoint using the common directed linear ceiling solver.
///
/// `reconstruction_linf_gain_upper` must be the exported operator bound of the
/// exact certified reconstruction named by the observation (HQ1024V1 in the
/// active closure). Album-scoped `Auto` may carry a scalar already bound by the
/// submitted-batch barrier; this function independently proves that scalar is
/// no larger than this participant permits.
pub fn resolve_reference_certified_gain(
    observation: &ReferenceCertifiedPeakObservation,
    policy: ResolvedGainPolicy,
    reconstruction_linf_gain_upper: f64,
) -> Result<ReferenceCertifiedGainAuthority> {
    observation
        .validate_active_contract()
        .map_err(|reason| PlanningError::invalid_settings("dsd.reference.observer", reason))?;
    validate_reference_observation_policy_scan(observation, policy)
        .map_err(|reason| PlanningError::invalid_settings("dsd.reference.observer", reason))?;
    if observation.subject != ReferenceObservationSubject::ProtectedR64
        || observation.purpose != TruePeakPurpose::GainAuthority
    {
        return Err(PlanningError::invalid_settings(
            "dsd.reference.observer",
            "Reference gain requires the protected-R64 gain-authority observation",
        ));
    }
    let (ceiling, terminal_bound) = reference_policy_ceiling_and_bound(policy);
    let terminal = reference_album_terminal_bound(terminal_bound, reconstruction_linf_gain_upper)?;
    let terminal_sample_error = terminal
        .stored_sample_error_linear
        .ok_or_else(|| PlanningError::invalid_settings(
            "dsd.reference.terminal_bound",
            "Reference lossless terminal omitted stored-sample error authority",
        ))?;
    let terminal_reconstructed_error = terminal.post_gain_reconstructed_error_linear;

    let fixed_gain = match policy {
        ResolvedGainPolicy::Off { .. } => Some(DbNano::HEADROOM_RESTORATION),
        ResolvedGainPolicy::TruePeakNormalize {
            scope: TruePeakScope::Album,
            bound_gain,
            ..
        } => bound_gain,
        ResolvedGainPolicy::TruePeakNormalize {
            scope: TruePeakScope::Track,
            ..
        } => None,
    };

    let ceiling_linear = crate::dsd_album_gain::conservative_linear_gain_lower(ceiling)
        .map_err(|reason| PlanningError::invalid_settings("dsd.reference.ceiling", reason))?;
    if terminal_reconstructed_error >= ceiling_linear {
        return Err(PlanningError::invalid_settings(
            "dsd.reference.terminal_bound",
            "Reference terminal error leaves no room beneath the policy ceiling",
        ));
    }

    let (selected_gain, maximum_linear_gain_bits) = match observation.result {
        ReferenceCertifiedPeakResult::VerifiedSilence => {
            (fixed_gain.unwrap_or(DbNano::ZERO), None)
        }
        ReferenceCertifiedPeakResult::Finite { .. } => {
            let upper = observation
                .result
                .conservative_upper_linear()
                .map_err(|reason| PlanningError::invalid_settings("dsd.reference.observer", reason))?;
            let participant = crate::dsd_album_gain::AlbumPeakMeasurement::Finite {
                point_db: DbNano::ZERO,
                signal_upper_linear: upper,
            };
            let authority = crate::dsd_album_gain::resolve_true_peak_gain_constraints(
                ceiling,
                &[(participant, terminal)],
                true,
            )
            .map_err(|reason| PlanningError::invalid_settings("dsd.reference.true_peak", reason))?;
            let selected = if let Some(gain) = fixed_gain {
                let selected_upper = crate::dsd_album_gain::conservative_linear_gain_upper(gain)
                    .map_err(|reason| PlanningError::invalid_settings("dsd.reference.gain", reason))?;
                if selected_upper > authority.maximum_linear_gain {
                    let message = match policy {
                        ResolvedGainPolicy::Off { .. } => {
                            "Reference gain-off cannot satisfy the fixed -1 dBTP acceptance ceiling without attenuation"
                        }
                        ResolvedGainPolicy::TruePeakNormalize { .. } => {
                            "submitted-album Reference gain exceeds this participant's certified terminal-safe maximum"
                        }
                    };
                    return Err(PlanningError::invalid_settings("dsd.from_dsd.gain", message));
                }
                gain
            } else {
                authority.gain_db
            };
            (selected, Some(authority.maximum_linear_gain.to_bits()))
        }
    };

    Ok(ReferenceCertifiedGainAuthority {
        requested_gain: selected_gain,
        selected_gain,
        ceiling,
        terminal_sample_error_linear_bits: terminal_sample_error.to_bits(),
        terminal_reconstructed_error_linear_bits: terminal_reconstructed_error.to_bits(),
        maximum_linear_gain_bits,
        reduced_for_ceiling: false,
    })
}

/// Fail-closed result of independent post-terminal Reference acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferencePostTerminalAcceptanceError {
    /// The conservative lower endpoint is proved above the Reference ceiling.
    CeilingViolated,
    /// The certified interval straddles the ceiling, so acceptance is unproved.
    CeilingNotProven,
}

impl fmt::Display for ReferencePostTerminalAcceptanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CeilingViolated => f.write_str("post-terminal certified lower endpoint exceeds the Reference ceiling"),
            Self::CeilingNotProven => f.write_str("post-terminal certified interval does not prove the Reference ceiling"),
        }
    }
}

/// Independently accept the actual terminal QPCM against the Reference ceiling.
///
/// An upper endpoint at/below the ceiling proves acceptance. A lower endpoint
/// above the ceiling proves a violation. A straddling interval is uncertainty,
/// not a measured violation, and fails closed as `CeilingNotProven`.
pub fn validate_reference_post_terminal_certified_peak(
    observation: &ReferenceCertifiedPeakObservation,
    policy: ResolvedGainPolicy,
) -> std::result::Result<(), ReferencePostTerminalAcceptanceError> {
    observation
        .validate_active_contract()
        .map_err(|_| ReferencePostTerminalAcceptanceError::CeilingNotProven)?;
    validate_reference_observation_policy_scan(observation, policy)
        .map_err(|_| ReferencePostTerminalAcceptanceError::CeilingNotProven)?;
    if observation.subject != ReferenceObservationSubject::TerminalQpcm
        || observation.purpose != TruePeakPurpose::PostFinalAcceptance
    {
        return Err(ReferencePostTerminalAcceptanceError::CeilingNotProven);
    }
    if matches!(observation.result, ReferenceCertifiedPeakResult::VerifiedSilence) {
        return Ok(());
    }
    let (ceiling, _) = reference_policy_ceiling_and_bound(policy);
    let ceiling_lower = crate::dsd_album_gain::conservative_linear_gain_lower(ceiling)
        .map_err(|_| ReferencePostTerminalAcceptanceError::CeilingNotProven)?;
    let ceiling_upper = crate::dsd_album_gain::conservative_linear_gain_upper(ceiling)
        .map_err(|_| ReferencePostTerminalAcceptanceError::CeilingNotProven)?;
    let upper = observation
        .result
        .conservative_upper_linear()
        .map_err(|_| ReferencePostTerminalAcceptanceError::CeilingNotProven)?;
    let lower = observation
        .result
        .conservative_lower_linear()
        .map_err(|_| ReferencePostTerminalAcceptanceError::CeilingNotProven)?;
    if upper <= ceiling_lower {
        Ok(())
    } else if lower > ceiling_upper {
        Err(ReferencePostTerminalAcceptanceError::CeilingViolated)
    } else {
        Err(ReferencePostTerminalAcceptanceError::CeilingNotProven)
    }
}

// Historical policy-v14-v16 analyzer constants retained only because the
// inherited qualification/certification artifacts bind their exact values.
// Phase 5 does not use these values for gain solving or post-terminal
// acceptance; the certified HQ1024 observer is the sole active authority.
/// Historical fixed 16x oversampling factor.
pub const REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR: u32 = 16;
/// Historical conservative analytic grid under-read bound.
pub const REFERENCE_TRUE_PEAK_GRID_BOUND: DbNano = DbNano(41_925_957);
/// Historical pinned-resampler residual allowance.
pub const REFERENCE_TRUE_PEAK_RESAMPLER_COMPONENT_LIMIT: DbNano = DbNano(58_074_043);
/// Historical complete 16x analyzer residual.
pub const REFERENCE_TRUE_PEAK_ANALYZER_RESIDUAL: DbNano = DbNano(100_000_000);
/// Historical analyzer residual plus reporting-quantization reserve.
pub const REFERENCE_TRUE_PEAK_ONE_SIDED_AUTHORITY: DbNano = DbNano(110_000_000);
/// Historical policy-v15 analyzer startup reserve.
pub const REFERENCE_TRUE_PEAK_DEADLINE_STARTUP_SECONDS: u64 = 120;
/// Historical policy-v15 analyzer throughput floor.
pub const REFERENCE_TRUE_PEAK_MIN_OVERSAMPLED_SAMPLE_VALUES_PER_SECOND: u64 = 1_000_000;
/// Historical maximum analyzer workload after the streamed-WAV capacity gate.
pub const REFERENCE_TRUE_PEAK_MAX_ADMITTED_WORKLOAD_SAMPLE_VALUES: u64 = 8_589_934_480;
/// Historical policy-v15 maximum analyzer deadline.
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

// Historical streamed-WAV/16x analyzer constants above remain public only because
// inherited v12-v16 evidence validators bind their exact values. Phase 5 has no
// executable streamed-analyzer capacity/deadline policy.

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
        PcmBitDepth::Int32 | PcmBitDepth::Float32 => 4_u64,
        PcmBitDepth::Float64 => 8_u64,
        PcmBitDepth::Int8 => {
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

/// Pure static admission/resolution result shared by the common typed planner
/// and the retained qualified Reference executor.  This contains no paths,
/// commands, process state, source reads, or qualification promotion.
#[derive(Debug, Clone)]
pub(crate) struct ReferenceStaticAdmission {
    pub source_rate: DsdRate,
    pub channels: u16,
    pub target: ResolvedOutputTarget,
    pub depth: PcmBitDepth,
    pub target_rate_hz: u32,
    pub profile: ResolvedDsdProfile,
    pub front_end: DsdInputFrontEnd,
    pub gain_policy: ResolvedGainPolicy,
    pub final_pcm: FinalPcmContract,
}

/// Resolve every static premise that gates qualified Reference delivery.
///
/// This is intentionally the single matrix authority for both
/// [`plan_reference_dsd`] and the Phase-2 common typed planner.  Missing source
/// facts are still reported as ordinary planning errors here; the common
/// planner converts facts that its caller may legitimately provide later into
/// `NeedFacts` before calling this function.
pub(crate) fn resolve_reference_static_admission(
    request: &PlanRequest,
) -> Result<ReferenceStaticAdmission> {
    let settings = request.settings.dsd.from_dsd;
    if settings.pathway != DsdSourcePathway::Reference {
        return Err(invalid_reference(
            "dsd.from_dsd.pathway",
            if settings.pathway == DsdSourcePathway::Manual {
                ReferenceErrorCode::ManualUnavailable
            } else {
                ReferenceErrorCode::CanonicalTarget
            },
        ));
    }
    if settings.reference_policy != DsdReferencePolicyVersion::SoxNg14801V17 {
        return Err(invalid_reference(
            "dsd.from_dsd.reference_policy",
            ReferenceErrorCode::Toolchain,
        ));
    }
    match &request.reference_programme_scope {
        ReferenceProgrammeScope::Singleton | ReferenceProgrammeScope::IndependentAlbumBatch { .. } => {}
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
        return Err(invalid_reference(
            "resolved_output_target",
            ReferenceErrorCode::LossyUnavailable,
        ));
    }
    let depth = resolve_reference_depth(request.settings.target_bit_depth)?;
    if !target.is_p0_reference_lossless() {
        return Err(invalid_target_depth("resolved_output_target", target, depth));
    }
    let target_rate_hz =
        resolve_reference_target_rate(source_rate, request.settings.target_sample_rate)?;
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
    if target == ResolvedOutputTarget::WavPackNative && request.settings.wavpack.correction_file {
        return Err(invalid_reference(
            "wavpack.correction_file",
            ReferenceErrorCode::CanonicalTarget,
        ));
    }
    let front_end = resolve_reference_front_end(source_kind)?;
    let gain_policy = resolve_gain_policy_for_programme(
        settings,
        &request.reference_programme_scope,
        request.settings.dsd.runtime_album_gain_db(),
        target_rate_hz,
        depth,
    )?;
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
            PcmBitDepth::Int32 | PcmBitDepth::Float32 | PcmBitDepth::Float64 => ReferenceDither::None,
            PcmBitDepth::Int8 => {
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
    // The retired 16x streamed-WAV analyzer no longer participates in active
    // Reference observation, so its RIFF-capacity and workload-deadline model
    // must not reject an otherwise admitted common-model route. Carrier-specific
    // R64/QPCM/package capacity checks remain at their actual boundaries.
    Ok(ReferenceStaticAdmission {
        source_rate,
        channels,
        target,
        depth,
        target_rate_hz,
        profile,
        front_end,
        gain_policy,
        final_pcm,
    })
}

/// Build the sealed Reference plan summary and staging namespace from the
/// authoritative common typed plan. The returned plan contains no independently
/// executable Reference command/step list; the runtime lowers the common graph.
pub fn plan_reference_dsd(request: &PlanRequest) -> Result<ConversionPlan> {
    // Preserve the sealed Reference policy's established public admission
    // precedence and diagnostics. The common planner consumes the same
    // authority after this preflight for valid routes.
    resolve_reference_static_admission(request)?;
    let typed = match crate::semantic_plan::plan_typed(request) {
        Ok(crate::semantic_plan::PlanningOutcome::Ready(plan)) => plan,
        Ok(crate::semantic_plan::PlanningOutcome::NeedFacts(facts)) => {
            let reason = facts
                .into_iter()
                .map(|fact| format!("{}: {}", fact.key, fact.reason))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(PlanningError::invalid_source(
                "semantic_plan",
                format!("planning requires authoritative source facts: {reason}"),
            ));
        }
        Ok(crate::semantic_plan::PlanningOutcome::Refused(refusal)) => {
            return Err(PlanningError::invalid_settings(
                "semantic_plan",
                format!("{}: {}", refusal.code, refusal.reason),
            ));
        }
        Err(limit) => {
            return Err(PlanningError::PlanningResourceLimit {
                resource: limit.resource,
                requested: limit.requested,
                limit: limit.limit,
            });
        }
    };
    crate::semantic_plan::require_current_executor(&typed)?;
    let semantic_plan_hash_v1 =
        crate::fingerprint::common_semantic_plan_fingerprint_v1(request, &typed).0;
    plan_reference_dsd_with_common_hash(request, semantic_plan_hash_v1)
}

/// Build the Reference staging plan when the caller has already resolved the
/// authoritative common semantic fingerprint.
pub(crate) fn plan_reference_dsd_with_common_hash(
    request: &PlanRequest,
    semantic_plan_hash_v1: Sha256Digest,
) -> Result<ConversionPlan> {
    let admission = resolve_reference_static_admission(request)?;
    let ReferenceStaticAdmission {
        source_rate,
        channels,
        target,
        depth: _,
        target_rate_hz,
        profile,
        front_end,
        gain_policy,
        final_pcm,
    } = admission;
    let settings = request.settings.dsd.from_dsd;
    let context = request.context();
    let r64 = context.intermediate_path(1, "w64");
    let final_work = context.final_work_path();
    let qpcm = if target == ResolvedOutputTarget::WavW64 {
        final_work.clone()
    } else {
        context.intermediate_path(2, "w64")
    };

    let pre_id = MeasurementId(1);
    let post_id = MeasurementId(2);
    let certified_scan_tier = match gain_policy {
        ResolvedGainPolicy::TruePeakNormalize { scan, .. } => scan,
        ResolvedGainPolicy::Off { .. } => TruePeakScanTier::Standard,
    };
    let certified_observer_identity =
        crate::qualification_schema::reference_certified_observer_id(certified_scan_tier);
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
    operations.extend([
        DsdReferenceOperation::DsdReferenceRender {
            target_rate_hz,
            profile,
            policy: settings.reference_policy,
        },
        DsdReferenceOperation::ValidateProtectedR64,
        DsdReferenceOperation::ObserveCertifiedTruePeak {
            measurement_id: pre_id,
            subject: ReferenceObservationSubject::ProtectedR64,
            purpose: TruePeakPurpose::GainAuthority,
            reader_authority: REFERENCE_R64_READER_ID.to_string(),
            observer_identity: certified_observer_identity.to_string(),
        },
        DsdReferenceOperation::ResolveReferenceGain {
            gain_policy,
            pre_terminal_measurement: pre_id,
        },
        DsdReferenceOperation::DsdReferenceFinalize {
            sample_contract: final_pcm,
            gain_policy,
            pre_terminal_measurement: pre_id,
        },
        DsdReferenceOperation::ValidateTerminalQpcm,
        DsdReferenceOperation::ObserveCertifiedTruePeak {
            measurement_id: post_id,
            subject: ReferenceObservationSubject::TerminalQpcm,
            purpose: TruePeakPurpose::PostFinalAcceptance,
            reader_authority: REFERENCE_QPCM_READER_ID.to_string(),
            observer_identity: certified_observer_identity.to_string(),
        },
    ]);
    if target != ResolvedOutputTarget::WavW64 {
        operations.push(DsdReferenceOperation::PackageLossless {
            target,
            sample_contract: final_pcm,
        });
    }
    operations.push(DsdReferenceOperation::VerifyPackageIdentity);
    let metadata_mutation_requested = request.settings.metadata.transfer_tags
        || request.settings.metadata.preserve_artwork
        || request.settings.metadata.store_source_audio_md5
        || request.settings.replay_gain.mode.is_some();
    if metadata_mutation_requested {
        operations.push(DsdReferenceOperation::MutateMetadata);
        operations.push(DsdReferenceOperation::VerifyPostMetadataIdentity);
    }
    operations.push(DsdReferenceOperation::PublicationBarrier);

    let finalization = Some(Finalization::AtomicRename {
        from: final_work.clone(),
        to: request.output_path.clone(),
    });
    let scratch_paths = reference_scratch_paths(request)?;
    let mut cleanup_paths = vec![r64.clone(), qpcm.clone()];
    cleanup_paths.extend(scratch_paths.all().into_iter().map(Path::to_path_buf));
    if final_work != request.output_path {
        cleanup_paths.push(final_work.clone());
    }
    cleanup_paths.sort();
    cleanup_paths.dedup();

    let package_compression_level = match target {
        ResolvedOutputTarget::FlacNative => Some(request.settings.flac.compression_level),
        ResolvedOutputTarget::WavPackNative => {
            Some(wavpack_compression_level_value(request.settings.wavpack.mode))
        }
        _ => None,
    };
    let summary = DsdReferencePlanSummary {
        policy: settings.reference_policy,
        qualification_candidate_manifest_digest: qualification_candidate_manifest_digest(),
        target,
        profile,
        front_end,
        final_pcm,
        gain_policy,
        package_compression_level,
        r64_path: r64,
        qpcm_path: qpcm,
        packaged_path: final_work,
        delivered_path: request.output_path.clone(),
        semantic_plan_hash_v1,
        operations,
    };
    Ok(ConversionPlan::execute_reference_with_cleanup(
        cleanup_paths,
        finalization,
        summary,
    ))
}

/// Build the exact qualified protected-R64 reconstruction prefix for ordinary
/// general processing. This reuses the Reference reconstruction command and
/// profile contract, but it does not claim qualified Reference delivery for
/// the later ordinary export/effects/gain/terminal suffix.
#[must_use]
pub fn build_reference_protected_reconstruction_command(
    input: &Path,
    output: &Path,
    target_rate_hz: u32,
    profile: ResolvedDsdProfile,
    duration: Option<std::time::Duration>,
) -> PlannedCommand {
    build_render_command(input, output, target_rate_hz, profile, duration)
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

/// Lower the one admitted Reference terminal realization from protected R64
/// to QPCM after the common gain decision has produced an exact scalar.
pub fn lower_reference_terminal_command(
    input: &Path,
    output: &Path,
    contract: FinalPcmContract,
    selected_gain: DbNano,
) -> Result<PlannedCommand> {
    let (encoding, bits) = match contract.bit_depth {
        PcmBitDepth::Int24 => ("signed-integer", "24"),
        PcmBitDepth::Int32 => ("signed-integer", "32"),
        PcmBitDepth::Float32 => ("floating-point", "32"),
        PcmBitDepth::Float64 => ("floating-point", "64"),
        PcmBitDepth::Int16 => {
            return Err(invalid_reference(
                "target_bit_depth",
                ReferenceErrorCode::Int16TerminalUnqualified,
            ));
        }
        PcmBitDepth::Int8 => {
            return Err(invalid_terminal_depth("target_bit_depth", contract.bit_depth));
        }
    };
    let mut args = vec![
        "-S".to_string(),
        "-D".to_string(),
        input.display().to_string(),
        "-t".to_string(),
        "w64".to_string(),
        "-e".to_string(),
        encoding.to_string(),
        "-b".to_string(),
        bits.to_string(),
        output.display().to_string(),
        "gain".to_string(),
        selected_gain.render(true),
    ];
    match contract.dither {
        ReferenceDither::None => {}
        ReferenceDither::Tpdf => args.push("dither".to_string()),
        ReferenceDither::Shibata => {
            return Err(PlanningError::invalid_settings(
                "target_bit_depth",
                "Reference Shibata terminal realization is not admitted",
            ));
        }
    }
    let mut command = PlannedCommand::new(
        ToolIdentifier::Sox,
        args,
        InputSource::Path(input.to_path_buf()),
        OutputSink::Path(output.to_path_buf()),
        None,
        "Apply one qualified Reference terminal realization",
    );
    command.environment_policy = CommandEnvironmentPolicy::ClearAndSet;
    command.environment = reference_command_environment();
    Ok(command)
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
        PcmBitDepth::Int32 => "pcm_s32le",
        PcmBitDepth::Float32 => "pcm_f32le",
        PcmBitDepth::Float64 => {
            return Err(PlanningError::invalid_settings(
                "target_bit_depth",
                "Float64 RIFF/RF64 packaging must use the qualified typed stream",
            ));
        }
        PcmBitDepth::Int8 => {
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
        ResolvedOutputTarget::FlacNative => {
            args.extend(["-c:a".to_string(), "flac".to_string()]);
            if contract.bit_depth == PcmBitDepth::Int32 {
                // FFmpeg otherwise silently stores signed 32-bit PCM as 24-bit FLAC.
                // Match the ordinary PCM path's explicit opt-in to true 32-bit FLAC.
                args.extend(["-strict".to_string(), "experimental".to_string()]);
            }
            args.extend([
                "-compression_level".to_string(),
                settings.flac.compression_level.to_string(),
            ]);
        }
        ResolvedOutputTarget::AiffNative => {
            let codec = match contract.bit_depth {
                PcmBitDepth::Int16 => "pcm_s16be",
                PcmBitDepth::Int24 => "pcm_s24be",
                PcmBitDepth::Int32 => "pcm_s32be",
                PcmBitDepth::Int8
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

/// Physical package lowering selected by the common Reference executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferencePackageLowering {
    /// One path-backed command.
    Command(PlannedCommand),
    /// One shell-free producer/consumer pipeline.
    Pipeline(PlannedCommandPipeline),
}

/// Lower admitted Reference packaging without creating a second executable plan.
/// Direct Wave64 delivery returns `None` because QPCM is already the package.
pub fn lower_reference_package(
    input: &Path,
    output: &Path,
    target: ResolvedOutputTarget,
    contract: FinalPcmContract,
    settings: &crate::settings::PipelineSettings,
) -> Result<Option<ReferencePackageLowering>> {
    if target == ResolvedOutputTarget::WavW64 {
        return Ok(None);
    }
    if contract.bit_depth == PcmBitDepth::Float64
        && matches!(target, ResolvedOutputTarget::WavRiff | ResolvedOutputTarget::WavRf64)
    {
        return build_float64_wav_package_pipeline(input, output, target, contract)
            .map(ReferencePackageLowering::Pipeline)
            .map(Some);
    }
    build_package_command(input, output, target, contract, settings)
        .map(ReferencePackageLowering::Command)
        .map(Some)
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

/// Canonical digest of the source-controlled Phase-5 candidate manifest.
///
/// Candidate status is intentionally non-promoting; production admission also
/// requires a matching completed report and release certification.
#[must_use]
pub fn qualification_candidate_manifest_digest() -> Sha256Digest {
    Sha256Digest::of_bytes(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/qualification/dsd_reference_common_v17_candidate.json"
    )))
}

/// Canonical digest of the source-controlled current v17 qualification artifact schema/content.
#[must_use]
pub fn qualification_manifest_digest() -> Sha256Digest {
    Sha256Digest::of_bytes(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/qualification/dsd_reference_sox_ng_14_8_0_1_v17.json"
    )))
}

// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
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
    steps: &[LegacyReferenceExecutionStep],
) -> Sha256Digest {
    let mut text = format!(
        "tonepoet-dsd-reference-semantic-plan/v1\nsource_rate={source_rate:?}\nchannels={channels}\ntarget={}\ntarget_rate={target_rate_hz}\nprofile={profile:?}\nfinal={final_pcm:?}\ngain={gain_policy:?}\nfront_end={front_end:?}\n",
        target.key()
    );
    let normalize: fn(&LegacyReferenceExecutionStep) -> String = match policy {
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
        DsdReferencePolicyVersion::SoxNg14801V16
        | DsdReferencePolicyVersion::SoxNg14801V17 => {
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
// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
fn normalize_step_for_hash_legacy(step: &LegacyReferenceExecutionStep) -> String {
    match step {
        LegacyReferenceExecutionStep::Command(command) => format!(
            "command:{}:{}",
            command.tool.program(),
            normalize_args(&command.args)
        ),
        LegacyReferenceExecutionStep::Pipeline(pipeline) => format!(
            "pipeline:{}:{}:{}:{}",
            pipeline.producer.tool.program(),
            normalize_args(&pipeline.producer.args),
            pipeline.consumer.tool.program(),
            normalize_args(&pipeline.consumer.args),
        ),
        LegacyReferenceExecutionStep::Measurement(measurement) => {
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
        LegacyReferenceExecutionStep::DeferredCommand(command) => {
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

// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
fn normalize_step_for_hash_v4(step: &LegacyReferenceExecutionStep) -> String {
    match step {
        LegacyReferenceExecutionStep::Command(command) => format!(
            "command:{}:{}:{}:{}:{}:{}",
            command.tool.program(),
            normalize_args(&command.args),
            normalize_input_source(&command.input),
            normalize_output_sink(&command.output),
            normalize_environment_policy(command.environment_policy),
            normalize_environment(&command.environment),
        ),
        LegacyReferenceExecutionStep::Pipeline(pipeline) => format!(
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
        LegacyReferenceExecutionStep::Measurement(measurement) => {
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
        LegacyReferenceExecutionStep::DeferredCommand(command) => {
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

// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
fn normalize_step_for_hash_v15(step: &LegacyReferenceExecutionStep) -> String {
    let deadline_identity = match step {
        LegacyReferenceExecutionStep::Command(command) => {
            normalize_expected_duration(command.expected_duration)
        }
        LegacyReferenceExecutionStep::Pipeline(pipeline) => format!(
            "producer={};consumer={}",
            normalize_expected_duration(pipeline.producer.expected_duration),
            normalize_expected_duration(pipeline.consumer.expected_duration),
        ),
        LegacyReferenceExecutionStep::Measurement(measurement) => format!(
            "producer={};consumer={}",
            measurement
                .input_stage
                .as_ref()
                .map_or_else(|| "none".to_string(), |stage| {
                    normalize_expected_duration(stage.expected_duration)
                }),
            normalize_expected_duration(measurement.command.expected_duration),
        ),
        LegacyReferenceExecutionStep::DeferredCommand(_) => "not_applicable".to_string(),
    };
    format!(
        "{}:deadline={deadline_identity}",
        normalize_step_for_hash_v4(step)
    )
}

// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
fn normalize_expected_duration(duration: Option<std::time::Duration>) -> String {
    duration.map_or_else(
        || "none".to_string(),
        |duration| format!("{}.{:09}", duration.as_secs(), duration.subsec_nanos()),
    )
}

// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
fn normalize_environment_policy(policy: CommandEnvironmentPolicy) -> &'static str {
    match policy {
        CommandEnvironmentPolicy::InheritAndSet => "inherit_and_set",
        CommandEnvironmentPolicy::ClearAndSet => "clear_and_set",
    }
}

// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
fn normalize_input_source(input: &InputSource) -> String {
    match input {
        InputSource::Path(path) => format!("path:{}", normalize_path_token(&path.display().to_string())),
        InputSource::Stdin => "stdin".to_string(),
    }
}

// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
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

// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
fn normalize_environment(environment: &std::collections::BTreeMap<String, String>) -> String {
    environment
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
fn normalize_args(args: &[String]) -> String {
    args.iter()
        .map(|value| normalize_path_token(value))
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

// Retained as append-only pre-common-planner hash machinery so historical
// Reference evidence remains independently verifiable.
#[allow(dead_code)]
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
    use crate::qualification_schema::{
        REFERENCE_CERTIFIED_OBSERVER_ID, REFERENCE_CERTIFIED_SCAN_TIER,
    };

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
                PcmBitDepth::Int32,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::SignedInt32Le,
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
                PcmBitDepth::Int32,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::SignedInt32Le,
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
                PcmBitDepth::Int32,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::SignedInt32Le,
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
                PcmBitDepth::Int32,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::SignedInt32Le,
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
                PcmBitDepth::Int32,
                ReferenceDecodeMechanism::DirectFfmpeg,
                ReferenceSampleHashEncoding::SignedInt32Le,
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
            serde_json::to_string(&DsdReferencePolicyVersion::SoxNg14801V17).unwrap(),
            r#""sox_ng_14_8_0_1_v17""#
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
        assert_eq!(
            serde_json::from_str::<DsdReferencePolicyVersion>(r#""sox_ng_14_8_0_1_v17""#)
                .unwrap(),
            DsdReferencePolicyVersion::SoxNg14801V17
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
            "invalid settings for dsd.from_dsd.profile: DSD-REF-P0-006: Reference policy sox_ng_14_8_0_1_v17 has no qualified target-limited profile for DSD128 \u{2192} 88.2 kHz. Choose 44.1/48 kHz, choose 176.4 kHz or higher, or wait for a new policy."
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
        settings.dsd = crate::settings::DsdSettings::reference();
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
                frame_extent: None,
                dsd_source_kind: Some(DsdSourceKind::DsdiffUncompressed),
                audio_md5: None,
            },
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: Some(PathBuf::from("work")),
            container_ffmpeg_flags: Vec::new(),
            resolved_output_target: Some(target),
            reference_programme_scope: ReferenceProgrammeScope::Singleton,
            planned_riff_non_audio_upper_bound_bytes: (target == ResolvedOutputTarget::WavRiff)
                .then_some(64 * 1024),
        }
    }

    #[test]
    fn reference_auto_scope_uses_album_for_independent_batches_and_track_otherwise() {
        let mut request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::WavW64,
            PcmBitDepth::Int24,
            DsdReconstructionSelection::Reference,
        );
        request.reference_programme_scope = ReferenceProgrammeScope::IndependentAlbumBatch {
            conversion_log_batch_id: "album-auto".to_string(),
            expected_members: std::num::NonZeroUsize::new(2).unwrap(),
            ordered_source_paths_digest: Sha256Digest::of_bytes(b"a.dff\0b.dff"),
        };
        let album = plan_reference_dsd(&request).expect("independent Reference album is admitted");
        assert!(matches!(
            album.reference.as_ref().expect("Reference summary").gain_policy,
            ResolvedGainPolicy::TruePeakNormalize {
                scope: TruePeakScope::Album,
                bound_gain: None,
                ..
            }
        ));

        request.reference_programme_scope = ReferenceProgrammeScope::Singleton;
        let track = plan_reference_dsd(&request).expect("Reference singleton is admitted");
        assert!(matches!(
            track.reference.as_ref().expect("Reference summary").gain_policy,
            ResolvedGainPolicy::TruePeakNormalize {
                scope: TruePeakScope::Track,
                bound_gain: None,
                ..
            }
        ));

        request.settings.dsd.from_dsd.gain = request
            .settings
            .dsd
            .from_dsd
            .gain
            .with_scope(TruePeakScope::Track);
        request.reference_programme_scope = ReferenceProgrammeScope::IndependentAlbumBatch {
            conversion_log_batch_id: "album-track".to_string(),
            expected_members: std::num::NonZeroUsize::new(2).unwrap(),
            ordered_source_paths_digest: Sha256Digest::of_bytes(b"a.dff\0b.dff"),
        };
        let forced_track =
            plan_reference_dsd(&request).expect("explicit Reference track scope is admitted");
        assert!(matches!(
            forced_track.reference.as_ref().expect("Reference summary").gain_policy,
            ResolvedGainPolicy::TruePeakNormalize {
                scope: TruePeakScope::Track,
                bound_gain: None,
                ..
            }
        ));
    }

    #[test]
    fn common_reference_gain_binding_drives_one_terminal_and_historical_policies_remain_refused() {
        let request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::WavW64,
            PcmBitDepth::Int24,
            DsdReconstructionSelection::Reference,
        );
        let plan = plan_reference_dsd(&request).unwrap();
        let summary = plan.reference.as_ref().expect("Reference summary");
        let pre_observation = ReferenceCertifiedPeakObservation {
            id: MeasurementId(1),
            scope: MeasurementScope::Plan,
            purpose: TruePeakPurpose::GainAuthority,
            subject: ReferenceObservationSubject::ProtectedR64,
            observer_identity: REFERENCE_CERTIFIED_OBSERVER_ID.to_string(),
            reconstruction: REFERENCE_CERTIFIED_RECONSTRUCTION.to_string(),
            edge_policy: REFERENCE_CERTIFIED_EDGE_POLICY.to_string(),
            scan_tier: REFERENCE_CERTIFIED_SCAN_TIER.to_string(),
            authority_endpoint: REFERENCE_CERTIFIED_AUTHORITY_ENDPOINT.to_string(),
            reader_authority: REFERENCE_R64_READER_ID.to_string(),
            sample_rate_hz: summary.final_pcm.sample_rate_hz,
            channels: summary.final_pcm.channels,
            sample_frames: 88_200,
            programme_sha256: Sha256Digest::of_bytes(b"phase5-reference-gain-test"),
            complete_reader: true,
            result: ReferenceCertifiedPeakResult::Finite {
                point_linear_bits: 0.099_f64.to_bits(),
                lower_linear_bits: 0.099_f64.to_bits(),
                upper_linear_bits: 0.1_f64.to_bits(),
                status: ReferenceCertifiedSearchStatus::WorkLimited,
            },
            certificate_sha256: Sha256Digest::of_bytes(b"phase5-reference-gain-certificate"),
        };
        let authority = resolve_reference_certified_gain(
            &pre_observation,
            summary.gain_policy,
            4.68,
        )
        .expect("complete-input certified observation resolves the sealed Reference gain");
        assert_eq!(authority.requested_gain, DbNano(18_999_989_109));
        assert_eq!(authority.selected_gain, authority.requested_gain);

        let terminal = lower_reference_terminal_command(
            &summary.r64_path,
            &summary.qpcm_path,
            summary.final_pcm,
            authority.selected_gain,
        )
        .expect("common terminal lowerer accepts the admitted Reference contract");
        assert_eq!(terminal.tool, ToolIdentifier::Sox);
        assert!(terminal.args.windows(2).any(|window| {
            window[0] == "gain" && window[1] == "+18.999989109"
        }));
        assert_eq!(
            summary
                .operations
                .iter()
                .filter(|operation| matches!(operation, DsdReferenceOperation::DsdReferenceFinalize { .. }))
                .count(),
            1,
            "Reference has exactly one terminal realization",
        );

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
            DsdReferencePolicyVersion::SoxNg14801V17,
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
            PcmBitDepth::Int32,
            PcmBitDepth::Float32,
            PcmBitDepth::Float64,
        ];
        for target in targets {
            for depth in depths {
                let should_succeed = match depth {
                    PcmBitDepth::Int16 => false,
                    PcmBitDepth::Int24 => true,
                    PcmBitDepth::Int32 => matches!(
                        target,
                        ResolvedOutputTarget::FlacNative
                            | ResolvedOutputTarget::WavRiff
                            | ResolvedOutputTarget::WavRf64
                            | ResolvedOutputTarget::WavW64
                            | ResolvedOutputTarget::AiffNative
                            | ResolvedOutputTarget::WavPackNative
                    ),
                    PcmBitDepth::Float32 | PcmBitDepth::Float64 => matches!(
                        target,
                        ResolvedOutputTarget::WavRiff
                            | ResolvedOutputTarget::WavRf64
                            | ResolvedOutputTarget::WavW64
                    ),
                    PcmBitDepth::Int8 => false,
                };
                assert_eq!(
                    validate_reference_target_depth(target, depth).is_ok(),
                    should_succeed,
                    "unexpected target/depth result for {target:?}/{depth:?}"
                );
            }
        }
        assert!(resolve_reference_depth(BitDepthTarget::Pcm(PcmBitDepth::Int8)).is_err());
        assert_eq!(
            resolve_reference_depth(BitDepthTarget::Pcm(PcmBitDepth::Int32)).unwrap(),
            PcmBitDepth::Int32
        );
        assert_eq!(
            resolve_reference_depth(BitDepthTarget::Source).unwrap(),
            PcmBitDepth::Int24
        );
    }

    #[test]
    fn phase5_certified_observation_routes_are_explicit_independent_and_finite_target_bound() {
        for depth in [PcmBitDepth::Float32, PcmBitDepth::Float64] {
            let request = reference_request(
                DsdRate::Dsd64,
                88_200,
                ResolvedOutputTarget::WavW64,
                depth,
                DsdReconstructionSelection::Reference,
            );
            let plan = plan_reference_dsd(&request).unwrap();
            let summary = plan.reference.as_ref().expect("Reference summary");
            let observations = summary
                .operations
                .iter()
                .filter_map(|operation| match operation {
                    DsdReferenceOperation::ObserveCertifiedTruePeak {
                        measurement_id,
                        subject,
                        purpose,
                        reader_authority,
                        observer_identity,
                    } => Some((
                        *measurement_id,
                        *subject,
                        *purpose,
                        reader_authority.as_str(),
                        observer_identity.as_str(),
                    )),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(observations.len(), 2);
            assert_eq!(
                observations[0],
                (
                    MeasurementId(1),
                    ReferenceObservationSubject::ProtectedR64,
                    TruePeakPurpose::GainAuthority,
                    REFERENCE_R64_READER_ID,
                    REFERENCE_CERTIFIED_OBSERVER_ID,
                )
            );
            assert_eq!(
                observations[1],
                (
                    MeasurementId(2),
                    ReferenceObservationSubject::TerminalQpcm,
                    TruePeakPurpose::PostFinalAcceptance,
                    REFERENCE_QPCM_READER_ID,
                    REFERENCE_CERTIFIED_OBSERVER_ID,
                )
            );
            assert_ne!(observations[0].3, observations[1].3);
        }

        let work_limited = ReferenceCertifiedPeakObservation {
            id: MeasurementId(1),
            scope: MeasurementScope::Plan,
            purpose: TruePeakPurpose::GainAuthority,
            subject: ReferenceObservationSubject::ProtectedR64,
            observer_identity: REFERENCE_CERTIFIED_OBSERVER_ID.to_string(),
            reconstruction: REFERENCE_CERTIFIED_RECONSTRUCTION.to_string(),
            edge_policy: REFERENCE_CERTIFIED_EDGE_POLICY.to_string(),
            scan_tier: REFERENCE_CERTIFIED_SCAN_TIER.to_string(),
            authority_endpoint: REFERENCE_CERTIFIED_AUTHORITY_ENDPOINT.to_string(),
            reader_authority: REFERENCE_R64_READER_ID.to_string(),
            sample_rate_hz: 88_200,
            channels: 2,
            sample_frames: 1,
            programme_sha256: Sha256Digest::of_bytes(b"phase5-work-limited"),
            complete_reader: true,
            result: ReferenceCertifiedPeakResult::Finite {
                point_linear_bits: 0.25_f64.to_bits(),
                lower_linear_bits: 0.249_f64.to_bits(),
                upper_linear_bits: 0.251_f64.to_bits(),
                status: ReferenceCertifiedSearchStatus::WorkLimited,
            },
            certificate_sha256: Sha256Digest::of_bytes(b"phase5-work-limited-cert"),
        };
        work_limited
            .validate_active_contract()
            .expect("complete-input WorkLimited evidence remains conservative authority");
        assert_eq!(work_limited.reconstruction, REFERENCE_CERTIFIED_RECONSTRUCTION);
        assert_eq!(work_limited.edge_policy, REFERENCE_CERTIFIED_EDGE_POLICY);
        assert_eq!(work_limited.scan_tier, REFERENCE_CERTIFIED_SCAN_TIER);
        assert_eq!(
            work_limited.authority_endpoint,
            REFERENCE_CERTIFIED_AUTHORITY_ENDPOINT
        );
    }

    #[test]
    fn reference_scan_tier_selects_matching_observer_identity() {
        for scan in [
            TruePeakScanTier::Reference,
            TruePeakScanTier::Standard,
            TruePeakScanTier::Fast,
        ] {
            let mut request = reference_request(
                DsdRate::Dsd64,
                88_200,
                ResolvedOutputTarget::WavW64,
                PcmBitDepth::Float64,
                DsdReconstructionSelection::Reference,
            );
            request.settings.dsd.from_dsd.gain = SampleGainPolicy::TruePeakNormalize {
                target_dbtp: DbNano::DEFAULT_REFERENCE_TRUE_PEAK_TARGET,
                scope: TruePeakScope::Track,
                scan,
            };
            let plan = plan_reference_dsd(&request).expect("Reference scan tier is admitted");
            let summary = plan.reference.as_ref().expect("Reference summary");
            assert_eq!(summary.certified_scan_tier(), scan);
            let expected = crate::qualification_schema::reference_certified_observer_id(scan);
            for operation in &summary.operations {
                if let DsdReferenceOperation::ObserveCertifiedTruePeak {
                    observer_identity, ..
                } = operation
                {
                    assert_eq!(observer_identity, expected);
                }
            }
        }
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
            assert_eq!(summary.policy, DsdReferencePolicyVersion::SoxNg14801V17);
            assert_eq!(summary.qpcm_path.extension().and_then(|value| value.to_str()), Some("w64"));
            let lowering = lower_reference_package(
                &summary.qpcm_path,
                &summary.packaged_path,
                summary.target,
                summary.final_pcm,
                &request.settings,
            )
            .expect("Float64 package lowering succeeds")
            .expect("RIFF/RF64 requires a package lowering");
            let ReferencePackageLowering::Pipeline(pipeline) = lowering else {
                panic!("Float64 RIFF/RF64 uses the shared typed package pipeline");
            };
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

            let baseline = normalize_step_for_hash_v4(&LegacyReferenceExecutionStep::Pipeline(
                pipeline.clone(),
            ));
            let mut changed = pipeline.clone();
            changed.producer.environment_policy = CommandEnvironmentPolicy::InheritAndSet;
            assert_ne!(
                baseline,
                normalize_step_for_hash_v4(&LegacyReferenceExecutionStep::Pipeline(changed))
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
            let summary = plan.reference.as_ref().expect("Reference summary");
            let render = build_reference_protected_reconstruction_command(
                &request.input_path,
                &summary.r64_path,
                summary.final_pcm.sample_rate_hz,
                summary.profile,
                request.source.duration,
            );
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
            "invalid settings for dsd.from_dsd.profile: DSD-REF-P0-007: Reference policy sox_ng_14_8_0_1_v17 has no direct 96 kHz qualification for DSD256. Choose 48 kHz, choose 176.4 kHz or higher, or wait for a new policy."
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
            "invalid settings for target_bit_depth: DSD-REF-P0-011: flac_native does not support Float32 under Reference policy sox_ng_14_8_0_1_v17. Choose a target/depth pair listed by the policy."
        );

        let exact_gain_observation = ReferenceCertifiedPeakObservation {
            id: MeasurementId(1),
            scope: MeasurementScope::Plan,
            purpose: TruePeakPurpose::GainAuthority,
            subject: ReferenceObservationSubject::ProtectedR64,
            observer_identity: REFERENCE_CERTIFIED_OBSERVER_ID.to_string(),
            reconstruction: REFERENCE_CERTIFIED_RECONSTRUCTION.to_string(),
            edge_policy: REFERENCE_CERTIFIED_EDGE_POLICY.to_string(),
            scan_tier: REFERENCE_CERTIFIED_SCAN_TIER.to_string(),
            authority_endpoint: REFERENCE_CERTIFIED_AUTHORITY_ENDPOINT.to_string(),
            reader_authority: REFERENCE_R64_READER_ID.to_string(),
            sample_rate_hz: 176_400,
            channels: 2,
            sample_frames: 1,
            programme_sha256: Sha256Digest::of_bytes(b"phase5-exact-gain-refusal"),
            complete_reader: true,
            result: ReferenceCertifiedPeakResult::Finite {
                point_linear_bits: 1.0_f64.to_bits(),
                lower_linear_bits: 1.0_f64.to_bits(),
                upper_linear_bits: 1.0_f64.to_bits(),
                status: ReferenceCertifiedSearchStatus::Complete,
            },
            certificate_sha256: Sha256Digest::of_bytes(b"phase5-exact-gain-refusal-cert"),
        };

        let mut source_settings = DsdSourceSettings::default();
        source_settings.gain = SampleGainPolicy::Off;
        let off_policy = resolve_gain_policy(source_settings, 176_400, PcmBitDepth::Int24)
            .expect("gain-off policy resolves");
        assert_eq!(
            resolve_reference_certified_gain(&exact_gain_observation, off_policy, 4.68)
                .unwrap_err()
                .to_string(),
            "invalid settings for dsd.from_dsd.gain: Reference gain-off cannot satisfy the fixed -1 dBTP acceptance ceiling without attenuation"
        );

        source_settings.gain = DsdSourceSettings::reference_auto_gain_default();
        let auto_policy = resolve_gain_policy(source_settings, 176_400, PcmBitDepth::Int24)
            .expect("automatic gain policy resolves");
        let auto = resolve_reference_certified_gain(&exact_gain_observation, auto_policy, 4.68)
            .expect("automatic gain attenuates to the certified target");
        assert!(auto.selected_gain < DbNano::ZERO);
    }

    #[test]
    fn certified_observation_tier_must_match_resolved_reference_policy() {
        let pre = ReferenceCertifiedPeakObservation {
            id: MeasurementId(1),
            scope: MeasurementScope::Plan,
            purpose: TruePeakPurpose::GainAuthority,
            subject: ReferenceObservationSubject::ProtectedR64,
            observer_identity: REFERENCE_CERTIFIED_OBSERVER_ID.to_string(),
            reconstruction: REFERENCE_CERTIFIED_RECONSTRUCTION.to_string(),
            edge_policy: REFERENCE_CERTIFIED_EDGE_POLICY.to_string(),
            scan_tier: REFERENCE_CERTIFIED_SCAN_TIER.to_string(),
            authority_endpoint: REFERENCE_CERTIFIED_AUTHORITY_ENDPOINT.to_string(),
            reader_authority: REFERENCE_R64_READER_ID.to_string(),
            sample_rate_hz: 176_400,
            channels: 2,
            sample_frames: 1,
            programme_sha256: Sha256Digest::of_bytes(b"reference-scan-binding-pre"),
            complete_reader: true,
            result: ReferenceCertifiedPeakResult::Finite {
                point_linear_bits: 0.1_f64.to_bits(),
                lower_linear_bits: 0.099_f64.to_bits(),
                upper_linear_bits: 0.101_f64.to_bits(),
                status: ReferenceCertifiedSearchStatus::Complete,
            },
            certificate_sha256: Sha256Digest::of_bytes(b"reference-scan-binding-pre-cert"),
        };
        pre.validate_active_contract()
            .expect("default Standard observation is internally valid");

        let mut settings = DsdSourceSettings::default();
        settings.gain = SampleGainPolicy::TruePeakNormalize {
            target_dbtp: DbNano::DEFAULT_REFERENCE_TRUE_PEAK_TARGET,
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Reference,
        };
        let policy = resolve_gain_policy(settings, 176_400, PcmBitDepth::Int24)
            .expect("Reference-tier policy resolves");
        let mismatch =
            "Reference certified observation scan tier does not match the resolved Reference policy";

        assert_eq!(
            resolve_reference_certified_gain(&pre, policy, 4.68)
                .unwrap_err()
                .to_string(),
            format!("invalid settings for dsd.reference.observer: {mismatch}")
        );
        assert_eq!(
            reference_album_gain_constraint(&pre, policy, 4.68)
                .unwrap_err()
                .to_string(),
            format!("invalid settings for dsd.reference.observer: {mismatch}")
        );

        let mut post = pre;
        post.id = MeasurementId(2);
        post.purpose = TruePeakPurpose::PostFinalAcceptance;
        post.subject = ReferenceObservationSubject::TerminalQpcm;
        post.reader_authority = REFERENCE_QPCM_READER_ID.to_string();
        post.programme_sha256 = Sha256Digest::of_bytes(b"reference-scan-binding-post");
        post.certificate_sha256 = Sha256Digest::of_bytes(b"reference-scan-binding-post-cert");
        assert_eq!(
            validate_reference_post_terminal_certified_peak(&post, policy),
            Err(ReferencePostTerminalAcceptanceError::CeilingNotProven)
        );
    }

    #[test]
    fn reference_gain_off_restores_private_headroom_without_ceiling_reduction() {
        let observation = ReferenceCertifiedPeakObservation {
            id: MeasurementId(1),
            scope: MeasurementScope::Plan,
            purpose: TruePeakPurpose::GainAuthority,
            subject: ReferenceObservationSubject::ProtectedR64,
            observer_identity: REFERENCE_CERTIFIED_OBSERVER_ID.to_string(),
            reconstruction: REFERENCE_CERTIFIED_RECONSTRUCTION.to_string(),
            edge_policy: REFERENCE_CERTIFIED_EDGE_POLICY.to_string(),
            scan_tier: REFERENCE_CERTIFIED_SCAN_TIER.to_string(),
            authority_endpoint: REFERENCE_CERTIFIED_AUTHORITY_ENDPOINT.to_string(),
            reader_authority: REFERENCE_R64_READER_ID.to_string(),
            sample_rate_hz: 176_400,
            channels: 2,
            sample_frames: 1,
            programme_sha256: Sha256Digest::of_bytes(b"reference-off-headroom-restoration"),
            complete_reader: true,
            result: ReferenceCertifiedPeakResult::Finite {
                point_linear_bits: 0.099_f64.to_bits(),
                lower_linear_bits: 0.099_f64.to_bits(),
                upper_linear_bits: 0.1_f64.to_bits(),
                status: ReferenceCertifiedSearchStatus::Complete,
            },
            certificate_sha256: Sha256Digest::of_bytes(
                b"reference-off-headroom-restoration-cert",
            ),
        };
        let mut source_settings = DsdSourceSettings::default();
        source_settings.gain = SampleGainPolicy::Off;
        let policy = resolve_gain_policy(source_settings, 176_400, PcmBitDepth::Int24)
            .expect("gain-off policy resolves");
        let authority = resolve_reference_certified_gain(&observation, policy, 4.68)
            .expect("native-level restoration is safe beneath the fixed ceiling");

        assert_eq!(authority.requested_gain, DbNano::HEADROOM_RESTORATION);
        assert_eq!(authority.selected_gain, DbNano::HEADROOM_RESTORATION);
        assert!(!authority.reduced_for_ceiling);

        let terminal = lower_reference_terminal_command(
            Path::new("protected.w64"),
            Path::new("terminal.w64"),
            FinalPcmContract {
                sample_rate_hz: 176_400,
                channels: 2,
                sample_kind: PcmBitDepth::Int24.sample_kind(),
                bit_depth: PcmBitDepth::Int24,
                dither: ReferenceDither::Tpdf,
            },
            authority.selected_gain,
        )
        .expect("terminal lowering accepts restored native level");
        assert!(terminal
            .args
            .windows(2)
            .any(|pair| pair == ["gain", "+12.000000000"]));
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
    fn flac_int32_package_argv_opts_into_true_32_bit_encoding() {
        let request = reference_request(
            DsdRate::Dsd64,
            88_200,
            ResolvedOutputTarget::FlacNative,
            PcmBitDepth::Int32,
            DsdReconstructionSelection::Reference,
        );
        let plan = plan_reference_dsd(&request).expect("Reference FLAC Int32 is admitted");
        let summary = plan.reference.as_ref().expect("Reference summary");
        let lowering = lower_reference_package(
            &summary.qpcm_path,
            &summary.packaged_path,
            summary.target,
            summary.final_pcm,
            &request.settings,
        )
        .expect("FLAC package lowering succeeds")
        .expect("FLAC requires a package command");
        let args = match lowering {
            ReferencePackageLowering::Command(command) => command.args,
            ReferencePackageLowering::Pipeline(_) => panic!("FLAC package must be one command"),
        };
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-strict", "experimental"]));
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
            let summary = plan.reference.as_ref().expect("Reference summary");
            let lowering = lower_reference_package(
                &summary.qpcm_path,
                &summary.packaged_path,
                summary.target,
                summary.final_pcm,
                &request.settings,
            )
            .unwrap()
            .expect("WavPack package lowering");
            match lowering {
                ReferencePackageLowering::Command(command) => command.args,
                ReferencePackageLowering::Pipeline(_) => {
                    panic!("WavPack package must be a single command")
                }
            }
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
        plan_reference_dsd(&request).expect(
            "RF64 no longer inherits the retired streamed-WAV carrier capacity bound",
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
            let package_lowering = lower_reference_package(
                &summary.qpcm_path,
                &summary.packaged_path,
                summary.target,
                summary.final_pcm,
                &request.settings,
            )
            .expect("Float32 package lowering succeeds");
            if target == ResolvedOutputTarget::WavW64 {
                assert_eq!(summary.qpcm_path, summary.packaged_path);
                assert!(package_lowering.is_none());
            } else {
                assert_ne!(summary.qpcm_path, summary.packaged_path);
                let Some(ReferencePackageLowering::Command(package)) = package_lowering else {
                    panic!("Float32 RF64 uses the shared single-command package lowering");
                };
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
            let package_lowering = lower_reference_package(
                &summary.qpcm_path,
                &summary.packaged_path,
                summary.target,
                summary.final_pcm,
                &request.settings,
            )
            .expect("Float64 package lowering succeeds");
            if target == ResolvedOutputTarget::WavW64 {
                assert_eq!(summary.qpcm_path, summary.packaged_path);
                assert!(package_lowering.is_none());
            } else {
                assert_ne!(summary.qpcm_path, summary.packaged_path);
                let Some(ReferencePackageLowering::Pipeline(pipeline)) = package_lowering else {
                    panic!("Float64 RF64 uses the shared headerless package pipeline");
                };
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
            (ReferenceErrorCode::UnsupportedDsdRate, "DSD-REF-P0-003: Reference policy sox_ng_14_8_0_1_v17 supports DSD64, DSD128, and DSD256 only. Use a supported-rate source or wait for expanded-rate/Manual support."),
            (ReferenceErrorCode::UnknownEncoding, "DSD-REF-P0-004: The DSD container or compression mode could not be identified as DSF/DSD, DSDIFF/DSD, DSDIFF/DST, or a supported SACD area. Reference will not guess the decoder path."),
            (ReferenceErrorCode::UnsupportedChannels, "DSD-REF-P0-005: Reference policy sox_ng_14_8_0_1_v17 supports qualified mono and stereo cells only. Select a mono/stereo track or wait for multichannel qualification."),
            (ReferenceErrorCode::Target882, "DSD-REF-P0-006: Reference policy sox_ng_14_8_0_1_v17 has no qualified target-limited profile for {DSD128|DSD256} \u{2192} 88.2 kHz. Choose 44.1/48 kHz, choose 176.4 kHz or higher, or wait for a new policy."),
            (ReferenceErrorCode::Target96, "DSD-REF-P0-007: Reference policy sox_ng_14_8_0_1_v17 has no direct 96 kHz qualification for {DSD128|DSD256}. Choose 48 kHz, choose 176.4 kHz or higher, or wait for a new policy."),
            (ReferenceErrorCode::WidebandDsd64, "DSD-REF-P0-008: No Wideband profile is defined for DSD64. Select the Reference profile."),
            (ReferenceErrorCode::WidebandDsd128Target, "DSD-REF-P0-008: DSD128 Wideband uses B4W and requires a target rate of at least 176.4 kHz. Select the Reference profile or choose 176.4 kHz or higher."),
            (ReferenceErrorCode::WidebandDsd256Target, "DSD-REF-P0-008: DSD256 Wideband uses B6, whose 140 kHz stopband edge cannot fit this target; B6 is also unavailable under policy sox_ng_14_8_0_1_v17. Select Reference/B5."),
            (ReferenceErrorCode::B6Unavailable, "DSD-REF-P0-009: B6 is represented but unqualified and unavailable under policy sox_ng_14_8_0_1_v17. Select Reference/B5 or wait for a later immutable policy."),
            (ReferenceErrorCode::TerminalInt8, "DSD-REF-P0-010: Reference policy sox_ng_14_8_0_1_v17 has no qualified 8-bit terminal realization. Choose 24-bit, Float32, or Float64 where supported."),
            (ReferenceErrorCode::TargetDepth, "DSD-REF-P0-011: {target} does not support {depth} under Reference policy sox_ng_14_8_0_1_v17. Choose a target/depth pair listed by the policy."),
            (ReferenceErrorCode::ContinuousProgramme, "DSD-REF-P0-013: Reference P0 cannot split a continuous DSD programme before reconstruction. This source must be processed as one programme before splitting; wait for programme-wide Reference support. Already independent files may be converted one at a time with independent gain."),
            (ReferenceErrorCode::FrontEndUnattested, "DSD-REF-P0-014: Reference requires the qualified DST/SACD decode front-end for this source, but the decoder/extractor identity or qualification manifest does not match. Install the qualified toolchain or use an uncompressed DSF/DSDIFF source."),
            (ReferenceErrorCode::Toolchain, "DSD-REF-P0-015: The installed Reference toolchain does not match policy sox_ng_14_8_0_1_v17 or failed its behavior probes. Activate/install the qualified toolchain; tonepoet will not substitute another decoder, analyzer, resampler, or encoder."),
            (ReferenceErrorCode::UnsupportedTargetRate, "DSD-REF-P0-017: Reference policy sox_ng_14_8_0_1_v17 supports target sample rates 44.1, 48, 88.2, 96, 176.4, 192, 352.8, 384, 705.6, and 768 kHz only. Choose one of those rates or wait for a later immutable policy."),
            (ReferenceErrorCode::RiffSize, "DSD-REF-P0-018: The predicted RIFF/WAV output exceeds the qualified RIFF size limit. Choose RF64, W64, or another supported lossless target."),
            (ReferenceErrorCode::CanonicalTarget, "DSD-REF-P0-019: The selected output container does not match the canonical Reference target or contains unrecognized output flags. Re-select the target."),
            (ReferenceErrorCode::CompressedDstRateUnqualified, "DSD-REF-P0-021: Reference policy sox_ng_14_8_0_1_v17 qualifies predictive compressed DST only for stereo DSD64. Mono DSD64 and all DSD128/DSD256 predictive-DST cells remain unavailable because no matching independent-oracle corpus is present. Use an uncompressed DSF/DSDIFF source, decode with an independently verified tool outside Reference, or wait for a later immutable policy."),
            (ReferenceErrorCode::Int16TerminalUnqualified, "DSD-REF-P0-022: Reference policy sox_ng_14_8_0_1_v17 does not enable Int16 because the commissioned SoX-ng Shibata realization has no qualified conservative worst-case peak bound. Choose Int24, Float32, or Float64, or wait for a later immutable policy with a derived Shibata bound."),
            (ReferenceErrorCode::SacdFrontEndIntegrationUnqualified, "DSD-REF-P0-023: Reference policy sox_ng_14_8_0_1_v17 does not enable SACD DSD or DST extraction because the production extraction/materialization path is not yet qualified by pinned end-to-end SACD fixtures. Extract to a qualified DSF/DSDIFF source first or wait for a later immutable policy."),
            (ReferenceErrorCode::W64MetadataMutationUnqualified, "DSD-REF-P0-024: Reference policy sox_ng_14_8_0_1_v17 cannot mutate metadata in W64 outputs because the pinned FFmpeg W64 muxer folds 8-byte alignment padding into the data chunk and can append a phantom sample. Disable the metadata stage for W64 delivery or choose another qualified lossless container; tonepoet will not invoke the unsafe muxer route."),
            (ReferenceErrorCode::StreamedWavCapacity, "DSD-REF-P0-025: This programme exceeds the conservative streamed-WAV capacity admission retained by Reference policy sox_ng_14_8_0_1_v17. The pinned SoX-ng writer wraps RIFF/data sizes past the 32-bit boundary, so the inherited transport authority does not admit this duration even though the v15 analyzer itself is path-backed or headerless raw. Shorten or split the source before Reference conversion, reduce the target sample rate, or wait for a later append-only policy that lifts this retained bound."),
            (ReferenceErrorCode::ManagedDestination, "DSD-REF-P0-020: The destination album has incompatible or incomplete tonepoet manifest authority. Choose a different output directory, repair/recover the existing transaction, or reconvert the album under one compatible Reference route; tonepoet will not merge or replace authority implicitly."),
            (ReferenceErrorCode::W64StructuralIntegrity, "DSD-REF-P0-026: Reference policy sox_ng_14_8_0_1_v17 rejected a Wave64 carrier before publication because its declared RIFF/data extents, chunk traversal, alignment, PCM format, or exact frame count did not match its physical contents and upstream exact-frame authority. Re-run under the qualified writer closure or choose another lossless target; tonepoet will not publish malformed Wave64."),
        ];
        let mut messages = std::collections::BTreeSet::new();
        for (code, exact) in expected {
            let actual = reference_error_text(code);
            assert_eq!(actual, exact, "drifted exact text for {code:?}");
            assert!(messages.insert(actual), "duplicate exact error message: {actual}");
        }
    }
}
