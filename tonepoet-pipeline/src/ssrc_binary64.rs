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

/// Exact Shibatch source revision commissioned on 2026-09-20: the rev4-audited
/// base b769add0 plus the vendored finite-stream / floating-fact patch.
pub const PINNED_SSRC_SOURCE_REV: &str = "6b0bbfe1fff79c0399347f4e1fb027c9931ef6ea";
/// Nix archive identity of that revision as locked in `flake.lock`.
pub const PINNED_SSRC_NAR_HASH: &str = "sha256-S1AODgERVoo8mKLEJN9gj4d+EW/ABfxkZqz1p14prPI=";
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
    /// Registered authority identifier.
    pub authority_id: String,
    /// Input container named by this qualification scope.
    pub input_container: String,
    /// Input sample format named by this qualification scope.
    pub input_sample_format: String,
    /// Maximum physical classic-RIFF extent admitted by this authority.
    pub max_physical_bytes: u64,
    /// Conservative non-audio RIFF/WAVE structure budget used at planning time.
    pub muxer_structure_upper_bound_bytes: u64,
}

impl ProtectedFloat64IngressAuthority {
    /// Construct the retained protected Float64 RIFF/WAV ingress authority.
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
    /// Binary64 preservation contract identifier.
    pub contract_id: String,
    /// Registered authority identifier.
    pub authority_id: String,
    /// Qualification evidence identifier.
    pub evidence_id: String,
    /// SHA-256 digest of the qualification report.
    pub qualification_report_sha256: String,
    /// Expected SHA-256 digest of the qualified SSRC executable.
    pub expected_executable_sha256: String,
    /// Runtime architecture covered by the attestation.
    pub runtime_architecture: String,
    /// SSRC source revision covered by the attestation.
    pub source_revision: String,
    /// Build identity covered by the attestation.
    pub build_identity: String,
    /// Qualified Binary64 evidence scope.
    pub evidence_scope: Binary64ResampleEvidenceScope,
    /// Output container admitted by the protected Binary64 contract.
    pub protected_output_container: String,
}

/// Exact SSRC controls frozen alongside a commissioned strong binding.  The
/// executor must consume these values rather than reinterpret user settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SelectedStrongSsrcResamplerParameters {
    /// Source sample rate in hertz.
    pub source_rate_hz: u32,
    /// Target sample rate in hertz.
    pub target_rate_hz: u32,
    /// SSRC profile selected or qualified by this contract.
    pub profile: SsrcProfile,
    /// SSRC attenuation in decibels.
    pub attenuation_db: Option<String>,
    /// Whether minimum-phase SSRC processing is selected.
    pub min_phase: bool,
    /// SSRC output depth.
    pub output_depth: PcmBitDepth,
    /// Whether the qualified SSRC realization disables SSRC-owned dither.
    pub dither_none: bool,
}

/// Complete frozen physical truth for one selected protected SSRC resampler.
///
/// This is carried only by an Established strong-preservation route. Ordinary
/// SSRC selections deliberately leave it absent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SelectedStrongSsrcResamplerBinding {
    /// Selected Binary64 preservation authority.
    pub binary64_preservation: SelectedBinary64ResamplePreservationBinding,
    /// Protected Float64 ingress authority.
    pub protected_ingress: ProtectedFloat64IngressAuthority,
    /// Fully resolved SSRC parameters bound to the authority.
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
    /// SSRC profile selected or qualified by this contract.
    pub profile: SsrcProfile,
    /// Exact source/current rate at the protected SSRC ingress.
    pub source_rate_hz: u32,
    /// Target sample rate in hertz.
    pub target_rate_hz: u32,
    /// Canonical command rendering, not the original request's binary float bits.
    pub attenuation_db: Option<String>,
    /// Whether minimum-phase SSRC processing is selected.
    pub min_phase: bool,
    /// Architecture named by this qualification scope.
    pub architecture: String,
    /// Input container named by this qualification scope.
    pub input_container: String,
    /// Input sample format named by this qualification scope.
    pub input_sample_format: String,
    /// Output container named by this qualification scope.
    pub output_container: String,
    /// Output sample format named by this qualification scope.
    pub output_sample_format: String,
}

/// Runtime executable identity bound by an Established record.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Binary64RuntimeAttestation {
    /// Expected SHA-256 digest of the qualified SSRC executable.
    pub expected_executable_sha256: String,
    /// Architecture named by this qualification scope.
    pub architecture: String,
    /// SSRC source revision covered by the attestation.
    pub source_revision: String,
    /// Build identity covered by the attestation.
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
        /// Registered authority identifier.
        authority_id: String,
    },
    /// Binary64 preservation is established by qualified evidence.
    Established {
        /// Registered authority identifier.
        authority_id: String,
        /// Qualification evidence identifier.
        evidence_id: String,
        /// SHA-256 digest of the qualification report.
        qualification_report_sha256: String,
        /// Runtime executable/build attestation for this evidence.
        runtime_attestation: Binary64RuntimeAttestation,
        /// Planning scope that owns this value.
        scope: Binary64ResampleEvidenceScope,
    },
    /// Binary64 preservation remains gated on missing evidence.
    PendingEvidence {
        /// Reason the requested evidence is pending, unavailable, or refuted.
        reason: Binary64PreservationEvidenceReason,
    },
    /// Binary64 preservation evidence has been explicitly refuted.
    Refuted {
        /// Reason the requested evidence is pending, unavailable, or refuted.
        reason: Binary64PreservationEvidenceReason,
    },
}

impl Binary64ResamplePreservationEvidence {
    /// Return whether this evidence state establishes Binary64 preservation.
    #[must_use]
    pub fn is_established(&self) -> bool {
        matches!(
            self,
            Self::EstablishedExistingAuthority { .. } | Self::Established { .. }
        )
    }

    /// Return the reason Binary64 preservation is unavailable, when applicable.
    #[must_use]
    pub fn unavailable_reason(&self) -> Option<&Binary64PreservationEvidenceReason> {
        match self {
            Self::EstablishedExistingAuthority { .. } | Self::Established { .. } => None,
            Self::PendingEvidence { reason } | Self::Refuted { reason } => Some(reason),
        }
    }
}

/// Commissioned x86_64 evidence, 2026-09-20 (42-pair grid run).
///
/// Source: `tonepoet-pipeline/qualification/ssrc_binary64/outcome_grid42_2026-09-20.json`
/// (identity file beside it), produced by `qualify_ssrc_binary64.py` against the
/// executable tonepoet's flake builds from [`PINNED_SSRC_SOURCE_REV`]. The earlier
/// five-pair run, `outcome_2026-09-20.json`, is kept as lineage.
pub const COMMISSIONED_X86_64_EVIDENCE_ID: &str =
    "sha256:6fd0d95e3f8c314d25c206c4cb42dc8f74ec570e61d0f86618384480bc6d0fd9";
/// SHA-256 of the qualification report bytes.
pub const COMMISSIONED_X86_64_REPORT_SHA256: &str =
    "6fd0d95e3f8c314d25c206c4cb42dc8f74ec570e61d0f86618384480bc6d0fd9";
/// SHA-256 of the commissioned SSRC executable.
pub const COMMISSIONED_X86_64_EXECUTABLE_SHA256: &str =
    "502af76669c554dcc8745a34c1032a6b789beb322755dd8ad827118f77d44adc";
/// Nix build closure that produced the commissioned executable.
pub const COMMISSIONED_X86_64_BUILD_IDENTITY: &str =
    "/nix/store/v4gglyvf800bvzv1l0mjx77f3hi9yxkg-ssrc-2.4.2.drv";
/// Rate pairs characterized by the commissioned run, as (source, target) hertz:
/// every ordered pair among the six library PCM rates, plus the two DXD rates
/// down to each of them (42 pairs, 2026-09-20 grid run).
pub const COMMISSIONED_X86_64_RATE_PAIRS: [(u32, u32); 42] = [
    (44_100, 48_000),
    (44_100, 88_200),
    (44_100, 96_000),
    (44_100, 176_400),
    (44_100, 192_000),
    (48_000, 44_100),
    (48_000, 88_200),
    (48_000, 96_000),
    (48_000, 176_400),
    (48_000, 192_000),
    (88_200, 44_100),
    (88_200, 48_000),
    (88_200, 96_000),
    (88_200, 176_400),
    (88_200, 192_000),
    (96_000, 44_100),
    (96_000, 48_000),
    (96_000, 88_200),
    (96_000, 176_400),
    (96_000, 192_000),
    (176_400, 44_100),
    (176_400, 48_000),
    (176_400, 88_200),
    (176_400, 96_000),
    (176_400, 192_000),
    (192_000, 44_100),
    (192_000, 48_000),
    (192_000, 88_200),
    (192_000, 96_000),
    (192_000, 176_400),
    (352_800, 44_100),
    (352_800, 48_000),
    (352_800, 88_200),
    (352_800, 96_000),
    (352_800, 176_400),
    (352_800, 192_000),
    (384_000, 44_100),
    (384_000, 48_000),
    (384_000, 88_200),
    (384_000, 96_000),
    (384_000, 176_400),
    (384_000, 192_000),
];

/// Production registry.
///
/// Single-precision profiles are source-refuted for this *strong* contract.
/// Double-precision profiles are Established only for the exact cells the
/// 2026-09-20 x86_64 commissioning characterized: linear phase, 0.0 dB
/// attenuation, Float64 RIFF in, Float64 Wave64 out, and one of
/// [`COMMISSIONED_X86_64_RATE_PAIRS`]. Every other double-precision cell stays
/// PendingEvidence. Ordinary SSRC is intentionally unaffected.
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
            if commissioned_x86_64_cell_covers(scope) {
                Binary64ResamplePreservationEvidence::Established {
                    authority_id: TONEPOET_BINARY64_OVERLOAD_PRESERVING_RESAMPLE_V1.to_owned(),
                    evidence_id: COMMISSIONED_X86_64_EVIDENCE_ID.to_owned(),
                    qualification_report_sha256: COMMISSIONED_X86_64_REPORT_SHA256.to_owned(),
                    runtime_attestation: Binary64RuntimeAttestation {
                        expected_executable_sha256: COMMISSIONED_X86_64_EXECUTABLE_SHA256
                            .to_owned(),
                        architecture: "x86_64".to_owned(),
                        source_revision: PINNED_SSRC_SOURCE_REV.to_owned(),
                        build_identity: COMMISSIONED_X86_64_BUILD_IDENTITY.to_owned(),
                    },
                    scope: scope.clone(),
                }
            } else {
                Binary64ResamplePreservationEvidence::PendingEvidence {
                    reason: Binary64PreservationEvidenceReason::NoMatchingCommissionedScope,
                }
            }
        }
    }
}

fn commissioned_x86_64_cell_covers(scope: &Binary64ResampleEvidenceScope) -> bool {
    scope.architecture == "x86_64"
        && !scope.min_phase
        && scope.attenuation_db.as_deref().map_or(true, |value| value == "0.0")
        && scope.input_container == "wav"
        && scope.input_sample_format == "pcm_f64le"
        && scope.output_container == "w64"
        && scope.output_sample_format == "pcm_f64le"
        && COMMISSIONED_X86_64_RATE_PAIRS
            .contains(&(scope.source_rate_hz, scope.target_rate_hz))
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
    fn double_precision_profiles_are_established_for_commissioned_x86_64_cells() {
        for profile in [SsrcProfile::High, SsrcProfile::Long, SsrcProfile::Insane] {
            for (source_rate_hz, target_rate_hz) in COMMISSIONED_X86_64_RATE_PAIRS {
                let mut cell = scope(profile);
                cell.source_rate_hz = source_rate_hz;
                cell.target_rate_hz = target_rate_hz;
                match production_evidence_for_scope(&cell) {
                    Binary64ResamplePreservationEvidence::Established {
                        authority_id,
                        evidence_id,
                        qualification_report_sha256,
                        runtime_attestation,
                        scope: bound,
                    } => {
                        assert_eq!(authority_id, TONEPOET_BINARY64_OVERLOAD_PRESERVING_RESAMPLE_V1);
                        assert_eq!(evidence_id, COMMISSIONED_X86_64_EVIDENCE_ID);
                        assert_eq!(
                            evidence_id,
                            format!("sha256:{qualification_report_sha256}")
                        );
                        assert_eq!(
                            runtime_attestation.expected_executable_sha256,
                            COMMISSIONED_X86_64_EXECUTABLE_SHA256
                        );
                        assert_eq!(runtime_attestation.architecture, "x86_64");
                        assert_eq!(runtime_attestation.source_revision, PINNED_SSRC_SOURCE_REV);
                        assert_eq!(bound, cell);
                    }
                    other => panic!("expected Established for {profile:?} {source_rate_hz}->{target_rate_hz}, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn double_precision_profiles_stay_pending_outside_the_commissioned_cells() {
        let pending = Binary64ResamplePreservationEvidence::PendingEvidence {
            reason: Binary64PreservationEvidenceReason::NoMatchingCommissionedScope,
        };
        let mut uncharacterized_pair = scope(SsrcProfile::High);
        uncharacterized_pair.source_rate_hz = 32_000;
        assert_eq!(production_evidence_for_scope(&uncharacterized_pair), pending);

        let mut min_phase = scope(SsrcProfile::Long);
        min_phase.min_phase = true;
        assert_eq!(production_evidence_for_scope(&min_phase), pending);

        let mut attenuated = scope(SsrcProfile::Insane);
        attenuated.attenuation_db = Some("1.0".to_owned());
        assert_eq!(production_evidence_for_scope(&attenuated), pending);

        let mut other_arch = scope(SsrcProfile::High);
        other_arch.architecture = "aarch64".to_owned();
        assert_eq!(production_evidence_for_scope(&other_arch), pending);

        let mut riff_out = scope(SsrcProfile::High);
        riff_out.output_container = "wav".to_owned();
        assert_eq!(production_evidence_for_scope(&riff_out), pending);
    }
}
