//! Evidence overlay for the narrow SSRC Binary64 preservation contract.
//!
//! Ordinary SSRC capability does not depend on this module.  These records
//! become relevant only when a semantic route requires the strong preservation
//! contract.  Production evidence is source-controlled and fail-closed.

use crate::enums::{PcmBitDepth, SsrcProfile};

/// Strong preservation contract admitted by Tonepoet when independently
/// commissioned evidence covers the exact selected SSRC cell.
pub const TONEPOET_BINARY64_OVERLOAD_PRESERVING_RESAMPLE_V1: &str =
    "TonepoetBinary64OverloadPreservingResampleV1";

/// Exact Shibatch source revision audited by the rev4 design bundle.
pub const PINNED_SSRC_SOURCE_REV: &str = "b769add0756157ea88d1bbf9023e06c437485a79";
/// Nix archive identity from the normative rev4 planning bundle.
pub const PINNED_SSRC_NAR_HASH: &str = "sha256-b+VYrfQ9nSRECcJR1RGOIPbqgit1giuCVdKt8GPUjZU=";
/// SLEEF source revision pinned by the audited SSRC build closure.
pub const PINNED_SLEEF_SOURCE_REV: &str = "0c063a8f0e01c22fa1e473effd2e7a0c69b4963a";
/// SSRC CLI version named by the audited source revision.
pub const PINNED_SSRC_VERSION: &str = "2.4.2";
/// Current protected-ingress cell admitted by the retained PCM materializer.
/// This authority covers only representation materialization; it does not
/// qualify the SSRC transform itself.
pub const PROTECTED_PCM_F64LE_RIFF_INGRESS_V1: &str =
    "tonepoet:protected-pcm-f64le-riff-ingress/v1";
/// Largest classic-RIFF physical extent accepted by the protected ingress cell.
/// The RIFF root size field excludes its leading eight bytes.
pub const PROTECTED_PCM_F64LE_RIFF_MAX_PHYSICAL_BYTES_V1: u64 = u32::MAX as u64 + 8;
/// Conservative upper bound reserved for the retained WAV materializer's RIFF
/// structure. Protected admission subtracts this from the physical RIFF ceiling
/// before accounting for source-rate Float64 audio payload bytes.
pub const PROTECTED_PCM_F64LE_RIFF_MUXER_STRUCTURE_UPPER_BOUND_BYTES_V1: u64 = 64 * 1024;
/// Bytes per interleaved sample in the protected Float64 ingress.
pub const PROTECTED_PCM_F64LE_BYTES_PER_SAMPLE: u64 = 8;

/// Stable identity of the arithmetic-free Wave64-to-raw adapter used only by
/// the protected SSRC execution cell.
pub const SSRC_W64_EXACT_PAYLOAD_BRIDGE_V1: &str =
    "tonepoet:ssrc-w64-exact-payload-bridge/v1";


/// Independently admitted producer of the stored Float64 boundary at which the
/// SSRC numerical contract begins.  This identity is deliberately not part of
/// the SSRC evidence key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ProtectedFloat64IngressAuthority {
    pub authority_id: String,
    pub input_container: String,
    pub input_sample_format: String,
    /// Maximum physical classic-RIFF extent admitted by this authority.
    pub max_physical_bytes: u64,
    /// Conservative non-audio RIFF/WAVE structure budget used at planning time.
    pub muxer_structure_upper_bound_bytes: u64,
}

impl ProtectedFloat64IngressAuthority {
    #[must_use]
    pub fn retained_pcm_riff_wav() -> Self {
        Self {
            authority_id: PROTECTED_PCM_F64LE_RIFF_INGRESS_V1.to_owned(),
            input_container: "wav".to_owned(),
            input_sample_format: "pcm_f64le".to_owned(),
            max_physical_bytes: PROTECTED_PCM_F64LE_RIFF_MAX_PHYSICAL_BYTES_V1,
            muxer_structure_upper_bound_bytes:
                PROTECTED_PCM_F64LE_RIFF_MUXER_STRUCTURE_UPPER_BOUND_BYTES_V1,
        }
    }
}

/// Runtime-frozen commissioned SSRC evidence.  Only a selected *strong*
/// preservation route carries this record; ordinary SSRC execution does not.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SelectedBinary64ResamplePreservationBinding {
    pub contract_id: String,
    pub authority_id: String,
    pub evidence_id: String,
    pub qualification_report_sha256: String,
    pub expected_executable_sha256: String,
    pub runtime_architecture: String,
    pub source_revision: String,
    pub build_identity: String,
    pub evidence_scope: Binary64ResampleEvidenceScope,
    pub protected_output_container: String,
}

/// Exact SSRC controls frozen alongside a commissioned strong binding.  The
/// executor must consume these values rather than reinterpret user settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SelectedStrongSsrcResamplerParameters {
    pub source_rate_hz: u32,
    pub target_rate_hz: u32,
    pub profile: SsrcProfile,
    pub attenuation_db: Option<String>,
    pub min_phase: bool,
    pub output_depth: PcmBitDepth,
    pub dither_none: bool,
}

/// Complete frozen physical truth for one selected protected SSRC resampler.
///
/// This is carried only by an Established strong-preservation route. Ordinary
/// SSRC selections deliberately leave it absent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SelectedStrongSsrcResamplerBinding {
    pub binary64_preservation: SelectedBinary64ResamplePreservationBinding,
    pub protected_ingress: ProtectedFloat64IngressAuthority,
    pub resolved_resampler: SelectedStrongSsrcResamplerParameters,
}

/// Why a production SSRC cell is not currently commissioned for the strong contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Binary64PreservationEvidenceReason {
    /// The pinned source deliberately instantiates `Pipeline<float>` for this profile.
    DocumentedSinglePrecisionPipeline,
    /// Source structure is compatible with double precision, but no exact executable
    /// has been commissioned for this physical cell.
    NoCommissionedExactExecutableEvidence,
    /// The selected settings do not match a commissioned evidence cell.
    NoMatchingCommissionedScope,
    /// The required authoritative Float64 ingress producer is unavailable.
    ProtectedIngressUnavailable,
}

/// Material numerical/container facts that scope one SSRC qualification cell.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Binary64ResampleEvidenceScope {
    pub profile: SsrcProfile,
    /// Exact source/current rate at the protected SSRC ingress.
    pub source_rate_hz: u32,
    pub target_rate_hz: u32,
    /// Canonical command rendering, not the original request's binary float bits.
    pub attenuation_db: Option<String>,
    pub min_phase: bool,
    pub architecture: String,
    pub input_container: String,
    pub input_sample_format: String,
    pub output_container: String,
    pub output_sample_format: String,
}

/// Runtime executable identity bound by an Established record.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Binary64RuntimeAttestation {
    pub expected_executable_sha256: String,
    pub architecture: String,
    pub source_revision: String,
    pub build_identity: String,
}

/// Source-controlled evidence state for one SSRC strong-preservation cell.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Binary64ResamplePreservationEvidence {
    /// Existing non-SSRC protected resampler authority retained by Tonepoet.
    /// It participates in the same typed admission comparison but carries no
    /// SSRC qualification scope.
    EstablishedExistingAuthority {
        authority_id: String,
    },
    Established {
        authority_id: String,
        evidence_id: String,
        qualification_report_sha256: String,
        runtime_attestation: Binary64RuntimeAttestation,
        scope: Binary64ResampleEvidenceScope,
    },
    PendingEvidence {
        reason: Binary64PreservationEvidenceReason,
    },
    Refuted {
        reason: Binary64PreservationEvidenceReason,
    },
}

impl Binary64ResamplePreservationEvidence {
    #[must_use]
    pub fn is_established(&self) -> bool {
        matches!(
            self,
            Self::EstablishedExistingAuthority { .. } | Self::Established { .. }
        )
    }

    #[must_use]
    pub fn unavailable_reason(&self) -> Option<&Binary64PreservationEvidenceReason> {
        match self {
            Self::EstablishedExistingAuthority { .. } | Self::Established { .. } => None,
            Self::PendingEvidence { reason } | Self::Refuted { reason } => Some(reason),
        }
    }
}

/// Production registry for the supplied rev4 state.
///
/// Single-precision profiles are source-refuted for this *strong* contract.
/// Double-precision profiles remain PendingEvidence until an exact executable,
/// build closure and qualification report are commissioned.  Ordinary SSRC is
/// intentionally unaffected.
#[must_use]
pub fn production_evidence_for_scope(
    scope: &Binary64ResampleEvidenceScope,
) -> Binary64ResamplePreservationEvidence {
    match scope.profile {
        SsrcProfile::Standard
        | SsrcProfile::Short
        | SsrcProfile::Fast
        | SsrcProfile::Lightning => Binary64ResamplePreservationEvidence::Refuted {
            reason: Binary64PreservationEvidenceReason::DocumentedSinglePrecisionPipeline,
        },
        SsrcProfile::High | SsrcProfile::Long | SsrcProfile::Insane => {
            Binary64ResamplePreservationEvidence::PendingEvidence {
                reason: Binary64PreservationEvidenceReason::NoCommissionedExactExecutableEvidence,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(profile: SsrcProfile) -> Binary64ResampleEvidenceScope {
        Binary64ResampleEvidenceScope {
            profile,
            source_rate_hz: 96_000,
            target_rate_hz: 44_100,
            attenuation_db: None,
            min_phase: false,
            architecture: "x86_64".to_owned(),
            input_container: "wav".to_owned(),
            input_sample_format: "pcm_f64le".to_owned(),
            output_container: "w64".to_owned(),
            output_sample_format: "pcm_f64le".to_owned(),
        }
    }

    #[test]
    fn single_precision_profiles_are_refuted_for_the_strong_contract() {
        for profile in [
            SsrcProfile::Standard,
            SsrcProfile::Short,
            SsrcProfile::Fast,
            SsrcProfile::Lightning,
        ] {
            assert_eq!(
                production_evidence_for_scope(&scope(profile)),
                Binary64ResamplePreservationEvidence::Refuted {
                    reason: Binary64PreservationEvidenceReason::DocumentedSinglePrecisionPipeline,
                }
            );
        }
    }

    #[test]
    fn double_precision_profiles_remain_pending_under_outcome_c() {
        for profile in [SsrcProfile::High, SsrcProfile::Long, SsrcProfile::Insane] {
            assert_eq!(
                production_evidence_for_scope(&scope(profile)),
                Binary64ResamplePreservationEvidence::PendingEvidence {
                    reason: Binary64PreservationEvidenceReason::NoCommissionedExactExecutableEvidence,
                }
            );
        }
    }
}
