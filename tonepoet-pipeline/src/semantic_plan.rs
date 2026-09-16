//! Phase-2 normalized intent and pure typed planning model.
//!
//! This module deliberately stops before process execution.  It records the
//! requested semantics, named audio/artifact boundaries, observations,
//! decisions, claims, obligations, admissible physical candidates, and the
//! executor capability required to realize them.  The existing command plan is
//! a lowering target, not an alternative semantic authority.

use crate::dsd_reference::{
    resolve_reference_static_admission, DbNano, DsdSourcePathway, FinalPcmContract,
    ResolvedGainPolicy, ResolvedOutputTarget,
};
use crate::enums::{
    AudioCodec, AudioFormat, BitDepthTarget, DitherType, DsdFilterPreset, DsdLowpassMethod, 
    GainCompensation, Mp3Mode, PcmBitDepth, PreferredTool, RateTarget, ReplayGainMode,
    ResampleQuality, SsrcProfile, TruePeakScanTier, TruePeakScope,
};
#[cfg(test)]
use crate::enums::{DsdRate, NyquistTransition, SsrcPdfType};
use crate::error::PlanningError;
#[cfg(test)]
use crate::settings::DsdSettings;
use crate::plan::{plan_topology, validate_forced_ssrc_semantics, InputSource, OutputSink, PlanOperation, PlanParticipantId, PlanRequest, PlanScope, PlanScopeId, PlanStep, TopologyPlan};
use crate::mapping;
use crate::settings::{
    AacSettings, DsdGeneralExportLevel, DsdGeneralReconstruction, DsdToPcmSettings,
    DsdToPcmSincSettings, FlacSettings, Mp3Settings, OpusSettings, PcmToDsdSettings,
    PcmToDsdSincSettings, ReplayGainExistingTagPolicy, SampleGainPolicy, SoxResamplerSettings,
    SoxrResamplerSettings, SsrcSettings, WavPackSettings,
};
use crate::source::SourceRepresentationKind;
use crate::tools::{ToolIdentifier, ToolRegistry};
use crate::qualification_schema::{
    REFERENCE_CERTIFIED_OBSERVER_ID, REFERENCE_QPCM_READER_ID, REFERENCE_R64_READER_ID,
};
use std::collections::{BTreeMap, BTreeSet};

/// A source/planning fact whose absence must never be treated as a negative fact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Fact<T> {
    /// Authoritative value is known.
    Known(T),
    /// The executor/source-realizer can provide the named fact later.
    Pending(String),
    /// The fact is known not to exist for this representation.
    Unavailable(String),
}

/// Stable identifier for a logical audio boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SignalId(pub u32);

/// Stable identifier for a file/container artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ArtifactId(pub u32);

/// Stable identifier for an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ObservationId(pub u32);

/// Observation identity bound to the exact planning/submission owner.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ScopedObservationId {
    pub scope: PlanScopeId,
    /// Exact participant that owns the request-local observation id.
    pub participant: PlanParticipantId,
    pub observation: ObservationId,
    /// Metric purpose; scope+local id alone must not let one observation class
    /// satisfy another decision slot.
    pub purpose: ObservationClass,
}

/// Stable identifier for a planner decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DecisionId(pub u32);

/// Coding family of a named audio state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SignalCoding {
    /// Integer or floating PCM.
    Pcm,
    /// One-bit DSD.
    Dsd,
    /// Lossy compressed audio.
    Lossy(AudioCodec),
    /// Source coding could not be established yet.
    Unknown,
}

/// Declared level basis at an audio boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum LevelBasis {
    /// Ordinary PCM/codec domain with no DSD reconstruction offset.
    Ordinary,
    /// Protected Reference reconstruction at R64 headroom.
    ProtectedR64,
    /// Native reconstructed DSD level.
    DsdNative,
    /// Nominal +6.020599913 dB compensation above native.
    DsdNominalCompensated,
    /// Native level plus an explicit offset.
    DsdNativeWithOffset(DbNano),
}

/// Precision/storage description that is independent of signal meaning.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StoragePrecision {
    /// PCM precision is known.
    Pcm(PcmBitDepth),
    /// DSD has no PCM word width.
    OneBit,
    /// Encoded/storage precision is not a PCM width.
    Encoded,
    /// Precision is a later source fact.
    Pending,
}

/// Frame-count authority for one audio state.
///
/// A duration estimate is deliberately not represented as an exact frame
/// count. Exact/traversal-established extents can be introduced by a source
/// realizer without changing the semantic plan shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FrameExtent {
    /// Exact frame count established by an authoritative source contract.
    Exact(u64),
    /// Proven upper bound used for admission, never equality.
    Bounded { upper_frames: u64 },
    /// An estimate suitable for progress/non-authoritative planning only.
    /// It must never prove a bounded physical-cell capacity admission.
    EstimatedDurationNanos(u64),
    /// Full traversal must establish the extent.
    Pending(String),
    /// This representation does not expose a frame count in the current plan.
    Unavailable(String),
}

/// Programme/reset semantics attached to every audio state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ProgrammeState {
    /// One independently reset track in the request-local programme.
    IndependentTrack,
    /// A continuous programme whose reset/window semantics must be preserved.
    ContinuousProgramme,
}

/// Arithmetic domain of the producer/next registered segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ProcessingDomain {
    /// Original encoded/source domain before a decoded arithmetic contract is selected.
    Source,
    /// One-bit DSD domain.
    DsdOneBit,
    /// Integer PCM domain with known width.
    PcmInteger(PcmBitDepth),
    /// Floating PCM exists, but the exact internal precision is not yet proved.
    PcmFloating,
    /// Exact binary64 arithmetic/transport is part of the registered contract.
    Binary64,
    /// Tool/backend-specific domain named by a registered contract.
    Registered(String),
}

/// Value-lattice/range evidence, intentionally separate from storage width.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ValueDomain {
    /// One-bit DSD lattice.
    OneBit,
    /// Exact integer lattice at the named PCM width.
    IntegerLattice(PcmBitDepth),
    /// Floating PCM requiring finite values; no stronger range claim is implied.
    FiniteFloating,
    /// Binary64 transport whose finite values are known to be exact values from
    /// SoX's signed Q1.31 lattice.  This is materially stronger than arbitrary
    /// finite binary64 and is the ingress premise used by the retained
    /// Reference terminal proof.
    Q1_31DerivedBinary64,
    /// Encoded values do not themselves define a decoded-sample lattice.
    Encoded,
    /// A later realization/observation must establish the value-domain premise.
    Pending(String),
}

/// Typed facts for one named signal boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AudioState {
    /// Stable signal identity.
    pub id: SignalId,
    /// Signal coding at this boundary.
    pub coding: SignalCoding,
    /// Sample rate in Hz where meaningful.
    pub sample_rate_hz: Fact<u32>,
    /// Channel count.
    pub channels: Fact<u16>,
    /// Ordered channel roles/layout. Unknown is never silently stereo.
    pub channel_layout: Fact<Vec<String>>,
    /// Frame extent with its authority level.
    pub frame_extent: FrameExtent,
    /// Programme continuity/reset semantics.
    pub programme: ProgrammeState,
    /// Storage/realization precision.
    pub precision: StoragePrecision,
    /// Storage encoding/layout facts beyond scalar precision.
    pub storage_contract: Fact<String>,
    /// Arithmetic domain, separate from emitted storage.
    pub processing_domain: Fact<ProcessingDomain>,
    /// Value-lattice/range evidence, separate from arithmetic/storage.
    pub value_domain: ValueDomain,
    /// Level basis used by all later gain decisions.
    pub level_basis: LevelBasis,
    /// Claims that are true at exactly this boundary.
    pub claims: BTreeSet<Claim>,
    /// Runtime checks that must be discharged before publication.
    pub obligations: BTreeSet<RuntimeObligation>,
}

/// A proof-bearing statement, never a generic `qualified` bit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Claim {
    /// Qualified Reference reconstruction graph/evidence applies to this state.
    ReferenceReconstruction,
    /// Complete qualified Reference DSD delivery contract applies.
    ReferenceDsdDelivery,
    /// Upstream overload has been preserved until this signal boundary.
    OverloadPreservedUntil(SignalId),
    /// Certified true-peak ceiling applies at a named signal boundary.
    CertifiedPcmCeiling {
        /// Signal governed by the claim.
        signal: SignalId,
        /// Requested certified ceiling.
        target_dbtp: DbNano,
        /// Numerical scan tier used to establish the premise.
        scan: TruePeakScanTier,
    },
    /// Two boundaries are signal-equivalent for a particular observation class.
    DecodedSignalEquivalent {
        /// Earlier signal.
        from: SignalId,
        /// Later signal.
        to: SignalId,
        /// Observation class whose applicability may transfer.
        observation: ObservationClass,
    },
    /// Stored sample identity is exact.
    SampleIdentity {
        /// Earlier signal.
        from: SignalId,
        /// Later signal.
        to: SignalId,
    },
    /// A metadata obligation has been satisfied on the named artifact.
    MetadataEffectSatisfied(MetadataEffect),
}

/// Runtime checks kept distinct from claims.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RuntimeObligation {
    /// Complete-reader/EOF framing must be checked for this observation subject.
    CompleteReader(SignalId),
    /// Final terminal realization must obey the declared error bound.
    TerminalErrorBound(SignalId),
    /// Independent decoder/check must agree with the stored artifact.
    IndependentDecode(ArtifactId),
    /// Album gain may bind only after every declared participant in this exact submitted cohort arrives.
    AlbumParticipantBarrier {
        scope: PlanScopeId,
        participant: PlanParticipantId,
        expected_participants: Option<u32>,
    },
    /// Publication is conditional on all required observations/verification.
    PublicationBarrier,
}

/// Observation family used for applicability transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ObservationClass {
    /// Certified true peak used as a hard-ceiling premise.
    CertifiedTruePeak,
    /// Reporting/sample true peak without a ceiling certificate.
    ReportingPeak,
    /// EBU-style integrated loudness/statistics.
    Loudness,
    /// Original programme frame/geometry facts.
    ProgrammeGeometry,
    /// Original-source digest.
    SourceAudioDigest,
    /// Independent post-terminal acceptance/verification observation.
    TerminalVerification,
}

/// Selected physical read/measurement contract for an observation.
///
/// Phase 2 records one authority, its accepted input representation/value
/// domains and EOF/complete-reader requirement even when the executor that
/// will realize that authority is connected only in Phase 3.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ObservationReadContract {
    pub authority: String,
    pub accepted_processing_domains: BTreeSet<ProcessingDomain>,
    pub accepted_value_domains: BTreeSet<ValueDomain>,
    pub complete_reader: bool,
    pub connected_executor: bool,
}

/// Exact observation request and subject.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Observation {
    /// Stable request-local observation identity.
    pub id: ObservationId,
    /// Exact track/submission scope that owns this observation slot.
    pub scope: PlanScopeId,
    /// Exact participant that owns the request-local observation id.
    pub participant: PlanParticipantId,
    /// Signal being observed; never inferred from a later artifact.
    pub subject: SignalId,
    /// Concrete artifact whose decoded programme is the subject, when applicable.
    pub artifact_subject: Option<ArtifactId>,
    /// Requested metric.
    pub kind: ObservationKind,
    /// Reader completion is part of the observation contract.
    pub complete_reader_required: bool,
    /// Selected read/measurement authority for this exact subject.
    pub read_contract: ObservationReadContract,
}

/// Requested metric.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ObservationKind {
    /// Certified peak interval at the selected numerical tier.
    CertifiedTruePeak { scan: TruePeakScanTier },
    /// Native production ReplayGain/loudness observation.
    ReplayGain {
        /// Requested projection mode; kept in observation identity for compatibility auditing.
        mode: ReplayGainMode,
        /// Production loudness implementation/profile.
        profile: ReplayGainLoudnessProfile,
        /// Metric coverage constructed before meter creation.
        coverage: ReplayGainMetricCoverage,
    },
    /// Original-source audio digest.
    SourceAudioMd5,
    /// Independent terminal decode/verification.
    TerminalVerification,
}


/// Production ReplayGain loudness profile identity. Compatibility profiles are
/// deliberately excluded from production planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ReplayGainLoudnessProfile {
    /// Phase-1 native EBU R128 / BS.1770 implementation.
    NativeEbu2023,
}

/// Coverage identity for a native loudness observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ReplayGainMetricCoverage {
    /// Integrated loudness/statistics only; normal ReplayGain demand.
    IntegratedOnly,
    /// Integrated loudness plus LRA for a compatible declared consumer.
    IntegratedAndRange,
}

impl ObservationKind {
    /// Purpose/class bound into decision dependencies and observation identity.
    #[must_use]
    pub const fn class(&self) -> ObservationClass {
        match self {
            Self::CertifiedTruePeak { .. } => ObservationClass::CertifiedTruePeak,
            Self::ReplayGain { .. } => ObservationClass::Loudness,
            Self::SourceAudioMd5 => ObservationClass::SourceAudioDigest,
            Self::TerminalVerification => ObservationClass::TerminalVerification,
        }
    }
}

/// Return whether one produced observation can fill an exact decision slot.
/// This deliberately validates owner scope, local identity, and purpose.
#[must_use]
pub fn observation_satisfies_dependency(
    observation: &Observation,
    dependency: &ScopedObservationId,
) -> bool {
    observation.scope == dependency.scope
        && observation.participant == dependency.participant
        && observation.id == dependency.observation
        && observation.kind.class() == dependency.purpose
}

/// True when an already completed measurement may be rebound to a new plan
/// scope without reading audio again. Scope and local observation id are
/// intentionally excluded; the measured subject, purpose and selected reader
/// contract must be identical. The caller still creates a new scoped key for
/// the new owner before satisfying any decision dependency.
#[must_use]
pub fn observation_result_reusable_for_rebind(
    completed: &Observation,
    requested: &Observation,
) -> bool {
    completed.participant == requested.participant
        && completed.subject == requested.subject
        && completed.artifact_subject == requested.artifact_subject
        && completed.kind == requested.kind
        && completed.complete_reader_required == requested.complete_reader_required
        && completed.read_contract == requested.read_contract
}

/// Pure decision whose inputs are named observations rather than hidden I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Decision {
    /// Stable decision identity.
    pub id: DecisionId,
    /// Decision semantics.
    pub kind: DecisionKind,
    /// Observation dependencies, including exact ownership scope.
    pub observations: Vec<ScopedObservationId>,
}

/// Exact owner of a sample-gain decision.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum GainDecisionBinding {
    /// Track-local scalar.
    Track { scope: PlanScopeId },
    /// One common scalar reduced by the existing submitted-batch coordinator.
    SubmittedBatch {
        scope: PlanScopeId,
        participant: PlanParticipantId,
        expected_participants: Option<u32>,
    },
}

/// One album participant's complete input to the existing common-scalar reducer.
///
/// The scope/participant pair is the queue coordinator's submitted-batch
/// authority.  `observation` binds the certified measurement slot and
/// `terminal_subject` binds the participant-specific terminal constraint whose
/// error allowance must be paired with that measurement during reduction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AlbumGainParticipantInput {
    pub scope: PlanScopeId,
    pub participant: PlanParticipantId,
    pub expected_participants: Option<u32>,
    pub observation: ScopedObservationId,
    pub terminal_subject: SignalId,
    /// Terminal proof selected for this participant's post-gain realization.
    /// Filled before a plan may be returned Ready.
    pub terminal_proof: Option<TerminalProofContract>,
}

/// Decision semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DecisionKind {
    /// Resolve Guard/Normalize gain from certified peaks and terminal bounds.
    TruePeakGain {
        /// Track-local or common-album scalar.
        scope: TruePeakScope,
        /// Certified ceiling.
        target_dbtp: DbNano,
        /// Only TruePeakNormalize sets this true.
        allow_boost: bool,
        /// Exact track/submitted-batch owner.
        binding: GainDecisionBinding,
        /// Album-only participant/reduction slot. Track decisions have no
        /// album barrier or group-reduction input.
        album_participant: Option<AlbumGainParticipantInput>,
    },
    /// Resolve the sealed Reference gain policy from the certified pre-terminal observation.
    ReferenceGain {
        /// Fully resolved qualified Reference policy; exact modes remain exact.
        policy: ResolvedGainPolicy,
    },
    /// Native ReplayGain projection/writer decision.
    ReplayGainProjection {
        /// Resolved projection policy. Clipping prevention affects projection,
        /// not the measurement subject, so it belongs here rather than on the
        /// ReplayGain observation.
        policy: ReplayGainProjectionPolicy,
        /// Track-local or the coordinator's current submitted batch.
        group: ReplayGainGroupBinding,
    },
}

/// Resolved ReplayGain projection policy.
///
/// `prevent_clipping = true` maps explicitly to the ordinary ReplayGain
/// prevention ceiling of -1.0 dBTP. The policy is intentionally separate from the measurement
/// observation because toggling clipping prevention changes projection only;
/// it does not require a new loudness/peak measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ReplayGainProjectionPolicy {
    /// Track, Album, or Both projection.
    pub mode: ReplayGainMode,
    /// Effective projection ceiling. `None` disables clipping prevention.
    pub prevention_ceiling_dbtp: Option<DbNano>,
}

impl ReplayGainProjectionPolicy {
    /// Effective application clipping-prevention ceiling.
    pub const PREVENT_CLIPPING_CEILING: DbNano = DbNano(-1_000_000_000);

    /// Resolve application ReplayGain settings into the projection contract.
    #[must_use]
    pub fn from_request(request: &PlanRequest) -> Option<Self> {
        request.settings.replay_gain.logical_mode().map(|mode| Self {
            mode,
            prevention_ceiling_dbtp: request
                .settings
                .replay_gain
                .prevent_clipping
                .then_some(Self::PREVENT_CLIPPING_CEILING),
        })
    }
}

/// Explicit grouping authority for ReplayGain decisions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ReplayGainGroupBinding {
    /// Complete request-local group. For an ordinary one-file conversion this
    /// also represents singleton Album/Both ReplayGain semantics.
    Track { scope: PlanScopeId },
    /// Bind album fields to one exact coordinator-owned submitted participant batch.
    SubmittedBatch {
        scope: PlanScopeId,
        participant: PlanParticipantId,
        expected_participants: Option<u32>,
    },
}

/// Stable request-local identity for one registered unary effect instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EffectInstanceId(pub u32);

/// Closed Phase-2 registry of unary sample-domain effects.
///
/// This is deliberately not arbitrary command syntax. Each variant has a typed
/// parameter surface and a deterministic argument mapping owned by this crate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RegisteredUnaryEffect {
    /// SoX high-pass filter.
    SoxHighPass { frequency_hz: u32 },
    /// SoX low-pass filter.
    SoxLowPass { frequency_hz: u32 },
    /// Explicit unprotected SoX sample-peak normalization effect.
    SoxSamplePeakNormalize { target_dbfs: DbNano },
    /// FFmpeg high-pass audio filter.
    FfmpegHighPass { frequency_hz: u32 },
    /// FFmpeg low-pass audio filter.
    FfmpegLowPass { frequency_hz: u32 },
}

/// Placement of a registered effect relative to the one semantic PCM resampler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EffectPlacement {
    /// Run after the PCM resampler. This is the historical/default behavior.
    #[default]
    AfterPcmResample,
    /// Run before the PCM resampler.
    BeforePcmResample,
}

/// One effect instance plus explicit predecessor constraints.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EffectIntent {
    /// Stable request-local identity; repeated effect kinds use distinct IDs.
    pub id: EffectInstanceId,
    /// Registered effect and typed parameters.
    pub effect: RegisteredUnaryEffect,
    /// Every listed instance must precede this one.
    pub after: Vec<EffectInstanceId>,
    /// Side of the one PCM resampler on which this effect executes.
    #[cfg_attr(feature = "serde", serde(default))]
    pub placement: EffectPlacement,
}

/// Tool-specific lowering fragment for a registered unary effect.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EffectArgumentMapping {
    /// Ordered SoX effect tokens, without shell quoting or an executable name.
    SoxEffect(Vec<String>),
    /// One FFmpeg audio-filter expression to compose into the selected filtergraph.
    FfmpegAudioFilter(String),
}

/// Registered lowering and contract for an effect instance.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EffectLowering {
    /// Required tool family.
    pub tool: ToolIdentifier,
    /// Typed argument mapping.
    pub arguments: EffectArgumentMapping,
    /// Transform contract.
    pub contract: TransformContract,
}

/// Exact output-product identity used by the semantic plan and fingerprints.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OutputProductIdentity {
    /// Requested audio format/codec family.
    pub target_format: AudioFormat,
    /// Exact requested container extension after normal fallback rules.
    pub container_extension: String,
    /// Trusted catalog identity when the caller has already resolved one.
    pub catalog_target: Option<ResolvedOutputTarget>,
    /// Container-specific FFmpeg flags that distinguish products sharing an extension.
    pub container_ffmpeg_flags: Vec<String>,
    /// WavPack hybrid base stream is lossy even though the format enum is WavPack.
    pub wavpack_hybrid: bool,
    /// WavPack hybrid correction sidecar request.
    pub wavpack_correction_file: bool,
}

impl OutputProductIdentity {
    /// Whether the governed delivered product is lossy or a lossy WavPack base.
    #[must_use]
    pub fn is_lossy_or_hybrid(&self) -> bool {
        self.target_format.is_lossy()
            || (self.target_format == AudioFormat::WavPack && self.wavpack_hybrid)
    }
}

/// Lifecycle decision for automatically inherited ReplayGain/R128 comment fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum InheritedLoudnessDisposition {
    /// Metadata transfer is disabled, so optional inherited loudness fields are not copied.
    NotCopied,
    /// Source and destination samples are proven equivalent for the inherited measurement.
    PreserveApplicable,
    /// Samples changed or equivalence is unresolved; inherited closed-field families must be removed.
    DropInapplicable,
    /// A requested ReplayGain operation owns replacement/preservation under its explicit skip policy.
    Requested {
        mode: ReplayGainMode,
        existing_tags: ReplayGainExistingTagPolicy,
        inherited_applicable: bool,
    },
}

/// Role of a named artifact state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ArtifactRole {
    /// Original source carrier/provenance object.
    Source,
    /// Newly packaged audio product before/after metadata transactions.
    Output,
}

/// Typed artifact identity distinct from decoded-signal identity.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ArtifactState {
    /// Stable artifact identity.
    pub id: ArtifactId,
    /// Source or output role.
    pub role: ArtifactRole,
    /// Decoded signal known to belong to this artifact, if sample identity is established.
    pub signal: Option<SignalId>,
    /// Exact output product for output artifacts.
    pub product: Option<OutputProductIdentity>,
    /// Final disposition for optional inherited loudness metadata families.
    pub inherited_loudness: InheritedLoudnessDisposition,
    /// Metadata effects required before publication.
    pub metadata_effects: BTreeSet<MetadataEffect>,
    /// Publication/runtime obligations attached to this artifact.
    pub obligations: BTreeSet<RuntimeObligation>,
}

/// Metadata mutation represented independently from audio transforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MetadataEffect {
    /// Transfer source fields/artwork under the resolved policy.
    TransferSource(MetadataTransferPolicy),
    /// Store original-source audio MD5.
    StoreSourceAudioMd5,
    /// Write ReplayGain fields.
    ReplayGain,
    /// Remove/suppress inherited ReplayGain/R128 fields whose subject is no longer applicable.
    ResolveInheritedLoudness,
}

/// Resolved source-metadata transfer policy.
///
/// Tags and artwork are independent controls.  Keeping both bits in the
/// normalized plan prevents tags-only, artwork-only, and strip-all requests
/// from collapsing to one semantic identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MetadataTransferPolicy {
    /// Copy source tags where the selected output path supports them.
    pub transfer_tags: bool,
    /// Preserve source artwork/video streams where the selected output path supports them.
    pub preserve_artwork: bool,
}

impl MetadataTransferPolicy {
    /// Whether any source metadata transfer is requested.
    #[must_use]
    pub const fn any(self) -> bool {
        self.transfer_tags || self.preserve_artwork
    }
}

/// Physical realization family considered for a logical transform.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PhysicalCandidate {
    /// Stable physical-realization identity. It belongs to execution identity;
    /// semantic fingerprints bind the candidate contract, not this label.
    pub identity: String,
    /// Tool/backend used if this candidate is lowered today.
    pub tool: Option<ToolIdentifier>,
    /// Candidate contract.
    pub contract: TransformContract,
    /// Whether current execution has a connected lowerer for this candidate.
    pub executable: bool,
    /// Strong SSRC preservation evidence is an overlay, not an ordinary
    /// transform fact. Historical candidates deserialize with no overlay.
    #[cfg_attr(feature = "serde", serde(default))]
    pub binary64_resample_preservation_evidence:
        Option<crate::ssrc_binary64::Binary64ResamplePreservationEvidence>,
    /// Independent authoritative Float64 ingress required only when this
    /// candidate is asked to discharge the strong Binary64 preservation
    /// obligation. Ordinary SSRC execution does not depend on this authority.
    #[cfg_attr(feature = "serde", serde(default))]
    pub protected_float64_ingress_authority:
        Option<crate::ssrc_binary64::ProtectedFloat64IngressAuthority>,
}

/// Boundary representation contract owned by one physical candidate.
///
/// Empty accepted-domain sets mean that the candidate did not register that
/// premise; they are not wildcards when a caller explicitly requires one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BoundaryRepresentationContract {
    /// Processing domains this candidate can consume under the registered path.
    pub accepted_processing_domains: BTreeSet<ProcessingDomain>,
    /// Value lattices/ranges this candidate can consume without invalidating its proof.
    pub accepted_value_domains: BTreeSet<ValueDomain>,
    /// Processing domain emitted by the registered path, when proved.
    pub emitted_processing_domain: Option<ProcessingDomain>,
    /// Stored precision emitted by the registered path, when proved.
    pub emitted_precision: Option<StoragePrecision>,
    /// Value lattice/range emitted by the registered path, when proved.
    pub emitted_value_domain: Option<ValueDomain>,
}

impl BoundaryRepresentationContract {
    fn unspecified() -> Self {
        Self {
            accepted_processing_domains: BTreeSet::new(),
            accepted_value_domains: BTreeSet::new(),
            emitted_processing_domain: None,
            emitted_precision: None,
            emitted_value_domain: None,
        }
    }
}

/// The physical shape of a selected lossless PCM terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PcmTerminalRealizationKind {
    /// SSRC owns the final WAV sample realization directly.
    SsrcDirectWav,
    /// SSRC owns the final samples and FFmpeg performs a proved package-only wrap.
    SsrcPreterminalFfmpegPackage,
    /// The selected SoX command owns the final PCM realization.
    SoxDirect,
    /// The selected FFmpeg command owns the final PCM realization.
    FfmpegDirect,
    /// SoX realizes the proved PCM samples before FFmpeg packages them losslessly.
    SoxPreterminalFfmpegPackage,
    /// SoX realizes the proved integer PCM supplied to native WavPack hybrid packaging.
    SoxPreterminalWavPackHybrid,
    /// The selected registry candidate delegates directly to native WavPack hybrid packaging.
    NativeWavPackHybridPackage,
}

/// Physical owner of effective terminal dither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PcmTerminalDitherOwner {
    /// No effective terminal dither is present.
    None,
    /// The selected terminal backend owns the dither.
    SelectedTerminal,
    /// A proved SoX preterminal owns the dither before sample-preserving packaging.
    SoxPreterminal,
    /// The selected SSRC resampler owns the final integer dither/quantizer.
    SsrcResampler,
}

/// Backend-aware physical truth for one selected lossless PCM terminal.
///
/// This value is resolved from the actual candidate plus the candidate's actual
/// input signal representation. Operation parameters, proof admission, stable
/// identity, lowering checks and runtime terminal-bound revalidation consume it
/// rather than independently reinterpreting destination settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SelectedPcmTerminalRealization {
    pub kind: PcmTerminalRealizationKind,
    pub selected_tool: ToolIdentifier,
    pub input_precision: StoragePrecision,
    pub input_value_domain: ValueDomain,
    pub target_format: AudioFormat,
    pub target_rate_hz: Option<u32>,
    pub target_bit_depth: PcmBitDepth,
    pub wavpack_hybrid: bool,
    pub effective_dither: Option<DitherType>,
    /// Exact SSRC-native dither/PDF truth when the resampler owns the terminal.
    /// Older serialized terminal records omit this field.
    #[cfg_attr(feature = "serde", serde(default))]
    pub ssrc_dither: Option<crate::plugins::ResolvedSsrcDither>,
    pub dither_owner: PcmTerminalDitherOwner,
}

/// Structured terminal realization carried by a selected physical candidate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SelectedTerminalRealization {
    Pcm(SelectedPcmTerminalRealization),
    /// Hard-ceiling lossy delivery is governed at FFmpeg encoder-input PCM.
    LossyFfmpegEncoderInput {
        target_format: AudioFormat,
        target_rate_hz: Option<u32>,
        apply_processing: bool,
    },
}

/// Source/closure identity for the FFmpeg explicit-Int32-dither realization
/// whose deterministic terminal-error model has been derived and retained in
/// tree. Production admission additionally requires architecture-specific
/// commissioning evidence for the exact executable before this authority may
/// be issued.
pub const FFMPEG_INT32_TRIANGULAR_TERMINAL_AUTHORITY_ID: &str =
    "ffmpeg_7-full-n7.1.3+nixpkgs-dd9b079222d43e1943b6ebd802f04fd959dc8e61/libswresample-dbl-triangular-s32/v1";

/// Exact-closure conformance commissioning is intentionally incomplete in this
/// tree. Flip an architecture to `true` only after the strict adversarial
/// harness has executed the pinned FFmpeg 7.1.3 closure on that architecture,
/// the executable identity matched, and the focused Rust tests passed there.
pub const FFMPEG_INT32_TRIANGULAR_X86_64_COMMISSIONED: bool = true;
pub const FFMPEG_INT32_TRIANGULAR_AARCH64_COMMISSIONED: bool = false;

/// Return whether `realization` matches the narrow source-derived FFmpeg model
/// retained for future commissioning. This is deliberately separate from
/// production qualification so the proof implementation can remain in tree
/// while the certified cell stays fail closed.
#[must_use]
pub fn matches_ffmpeg_int32_triangular_terminal_model(
    realization: &SelectedPcmTerminalRealization,
) -> bool {
    realization.kind == PcmTerminalRealizationKind::FfmpegDirect
        && realization.selected_tool == ToolIdentifier::Ffmpeg
        && realization.input_precision == StoragePrecision::Pcm(PcmBitDepth::Float64)
        && matches!(
            &realization.input_value_domain,
            ValueDomain::FiniteFloating | ValueDomain::Q1_31DerivedBinary64
        )
        && realization.target_bit_depth == PcmBitDepth::Int32
        && matches!(
            &realization.target_format,
            AudioFormat::Flac | AudioFormat::Wav | AudioFormat::Aiff | AudioFormat::WavPack
        )
        && !realization.wavpack_hybrid
        && realization.effective_dither == Some(DitherType::Tpdf)
        && realization.dither_owner == PcmTerminalDitherOwner::SelectedTerminal
}

/// Return whether the current build architecture has completed the exact
/// closure conformance commissioning required before production may issue the
/// FFmpeg Int32 triangular terminal authority.
#[must_use]
pub fn ffmpeg_int32_triangular_terminal_commissioned_for_current_arch() -> bool {
    match std::env::consts::ARCH {
        "x86_64" => FFMPEG_INT32_TRIANGULAR_X86_64_COMMISSIONED,
        "aarch64" => FFMPEG_INT32_TRIANGULAR_AARCH64_COMMISSIONED,
        _ => false,
    }
}

/// Return whether `realization` may receive the production certified terminal
/// authority on this build. The source-derived model alone is insufficient:
/// architecture-specific commissioning must also be complete.
#[must_use]
pub fn is_qualified_ffmpeg_int32_triangular_terminal(
    realization: &SelectedPcmTerminalRealization,
) -> bool {
    ffmpeg_int32_triangular_terminal_commissioned_for_current_arch()
        && matches_ffmpeg_int32_triangular_terminal_model(realization)
}

/// The two retained runtime numerical terminal-bound authorities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TerminalProofAuthorityFamily {
    PcmTruePeakV2,
    AlbumGainV2,
}

impl TerminalProofAuthorityFamily {
    fn authority_name(self) -> &'static str {
        match self {
            Self::PcmTruePeakV2 => "pcm_true_peak_terminal_bound",
            Self::AlbumGainV2 => "album_gain_terminal_bound",
        }
    }
}

/// Derive the stable diagnostic/fingerprint authority from structured terminal truth.
#[must_use]
pub fn terminal_proof_authority(
    family: TerminalProofAuthorityFamily,
    realization: &SelectedTerminalRealization,
) -> String {
    let route = match realization {
        SelectedTerminalRealization::Pcm(realization) => format!(
            "{}:{}:{}:{}:dither={}",
            match realization.kind {
                PcmTerminalRealizationKind::SsrcDirectWav => "ssrc-direct-wav",
                PcmTerminalRealizationKind::SsrcPreterminalFfmpegPackage => {
                    "ssrc-preterminal+ffmpeg"
                }
                PcmTerminalRealizationKind::SoxDirect => "sox-direct",
                PcmTerminalRealizationKind::FfmpegDirect => "ffmpeg-direct",
                PcmTerminalRealizationKind::SoxPreterminalFfmpegPackage => {
                    "sox-preterminal+ffmpeg"
                }
                PcmTerminalRealizationKind::SoxPreterminalWavPackHybrid => {
                    "sox-preterminal+wavpack"
                }
                PcmTerminalRealizationKind::NativeWavPackHybridPackage => {
                    "wavpack-hybrid-native"
                }
            },
            realization.target_format.extension(),
            realization
                .target_rate_hz
                .map_or_else(|| "source".to_owned(), |rate| rate.to_string()),
            realization.target_bit_depth.bits(),
            realization
                .effective_dither
                .map(dither_contract_name)
                .unwrap_or("none"),
        ),
        SelectedTerminalRealization::LossyFfmpegEncoderInput {
            target_format,
            target_rate_hz,
            apply_processing,
        } => format!(
            "ffmpeg:{}:{}:encoder-input:processing={}",
            target_format.extension(),
            target_rate_hz.map_or_else(|| "source".to_owned(), |rate| rate.to_string()),
            apply_processing,
        ),
    };
    let implementation = match realization {
        SelectedTerminalRealization::Pcm(realization)
            if is_qualified_ffmpeg_int32_triangular_terminal(realization) =>
        {
            format!(
                ";implementation={FFMPEG_INT32_TRIANGULAR_TERMINAL_AUTHORITY_ID}"
            )
        }
        _ => String::new(),
    };
    format!(
        "application:{}/v2;route={route}{implementation}",
        family.authority_name(),
    )
}

/// Named terminal/error proof applicable to a candidate realization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TerminalProofContract {
    /// Stable diagnostic/fingerprint form derived from the candidate's structured
    /// terminal realization. The numeric bound remains runtime-owned.
    pub authority: String,
    /// Input value domains for which this proof is applicable.
    pub accepted_value_domains: BTreeSet<ValueDomain>,
    /// Whether the proof separately requires a non-clipping ingress premise.
    pub requires_non_clipping_ingress: bool,
}

/// Proof/representation premises required by a route-selection decision.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CandidateRequirements {
    /// Exact processing domain the selected candidate must admit.
    pub input_processing_domain: Option<ProcessingDomain>,
    /// Exact value lattice/range the selected candidate must admit.
    pub input_value_domain: Option<ValueDomain>,
    /// Require a registered terminal/error proof for the input value domain.
    pub terminal_proof_required: bool,
    /// Require one structured selected-terminal realization. Lossless PCM terminal
    /// selection uses this even when no certified hard-ceiling proof is requested.
    #[cfg_attr(feature = "serde", serde(default))]
    pub terminal_realization_required: bool,
    /// Require a complete-reader/EOF contract.
    pub complete_reader_required: bool,
    /// Exact runtime contracts that must be carried by the selected compound
    /// route (for example the selected certified observation reader plus the
    /// certified-observation purpose itself).
    pub required_runtime_obligations: BTreeSet<String>,
    /// Require that the selected PCM resampler has commissioned evidence for
    /// TonepoetBinary64OverloadPreservingResampleV1.
    #[cfg_attr(feature = "serde", serde(default))]
    pub binary64_resample_preservation_required: bool,
    /// Require that this individual candidate already has a connected lowerer.
    /// Semantic Phase-3 routes set this false and rely on `execution_capability`
    /// to prevent current execution after admission succeeds.
    pub connected_executor_required: bool,
}

/// Contract attached to a candidate; proof and observation transfer remain separate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TransformContract {
    /// Accepted/emitted representation facts. Storage, processing and value
    /// lattice are intentionally independent.
    pub representation: BoundaryRepresentationContract,
    /// Canonical physical terminal truth, when this candidate realizes a terminal.
    #[cfg_attr(feature = "serde", serde(default))]
    pub terminal_realization: Option<SelectedTerminalRealization>,
    /// Terminal/error proof, if this candidate has one registered.
    pub terminal_proof: Option<TerminalProofContract>,
    /// Claims that survive this transform unchanged.
    pub carries_claims: BTreeSet<ClaimKind>,
    /// Claims discharged/created by this transform.
    pub produces_claims: BTreeSet<ClaimKind>,
    /// Observation classes for which exact signal applicability transfers.
    pub signal_equivalent_for: BTreeSet<ObservationClass>,
    /// Runtime obligation names the executor must enforce.
    pub runtime_obligations: BTreeSet<String>,
}

fn all_pcm_processing_domains() -> BTreeSet<ProcessingDomain> {
    BTreeSet::from([
        ProcessingDomain::PcmInteger(PcmBitDepth::Int8),
        ProcessingDomain::PcmInteger(PcmBitDepth::Int16),
        ProcessingDomain::PcmInteger(PcmBitDepth::Int24),
        ProcessingDomain::PcmInteger(PcmBitDepth::Int32),
        ProcessingDomain::PcmFloating,
        ProcessingDomain::Binary64,
    ])
}

fn all_pcm_value_domains() -> BTreeSet<ValueDomain> {
    BTreeSet::from([
        ValueDomain::IntegerLattice(PcmBitDepth::Int8),
        ValueDomain::IntegerLattice(PcmBitDepth::Int16),
        ValueDomain::IntegerLattice(PcmBitDepth::Int24),
        ValueDomain::IntegerLattice(PcmBitDepth::Int32),
        ValueDomain::FiniteFloating,
        ValueDomain::Q1_31DerivedBinary64,
    ])
}

impl TransformContract {
    fn plain_transform() -> Self {
        Self {
            representation: BoundaryRepresentationContract::unspecified(),
            terminal_realization: None,
            terminal_proof: None,
            carries_claims: BTreeSet::new(),
            produces_claims: BTreeSet::new(),
            signal_equivalent_for: BTreeSet::new(),
            runtime_obligations: BTreeSet::new(),
        }
    }
}

/// Coarse claim family used by registered transform contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ClaimKind {
    ReferenceReconstruction,
    ReferenceDsdDelivery,
    CertifiedPcmCeiling,
    SampleIdentity,
    MetadataEffectSatisfied,
}

/// One typed semantic node.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TypedPlanNode {
    /// Existing logical operation represented in the common plan.
    Operation {
        /// Logical operation. All eleven `PlanOperation` families are admissible here.
        operation: PlanOperation,
        /// Input signal where the operation consumes audio.
        input_signal: Option<SignalId>,
        /// Output signal where the operation produces audio.
        output_signal: Option<SignalId>,
        /// Physical candidates in deterministic preference order.
        candidates: Vec<PhysicalCandidate>,
        /// Candidate selected after applying the current backend preference to
        /// the already registered/admissible set.  This is an index rather
        /// than a duplicated candidate so the plan has one contract authority.
        /// A selected candidate may still have `executable == false` when the
        /// semantic route is intentionally handed to Phase 3; callers must
        /// consult [`ExecutionCapability`] before execution.
        selected_candidate: usize,
        /// Active requested/effective lowering parameters for the selected route.
        resolved_parameters: ResolvedOperationParameters,
    },
    /// Metric observation.
    Observe(Observation),
    /// Pure policy decision.
    Decide(Decision),
    /// Apply a resolved ordinary scalar to a named signal.
    ApplyGain {
        input: SignalId,
        output: SignalId,
        policy: SampleGainPolicy,
        decision: Option<DecisionId>,
    },
    /// Decode a compressed source into a PCM processing subject.
    DecodeSourceForProcessing {
        input: SignalId,
        output: SignalId,
    },
    /// Registered unary effect in the resolved deterministic order.
    ApplyEffect {
        input: SignalId,
        output: SignalId,
        instance: EffectIntent,
        lowering: EffectLowering,
    },
    /// Explicit DSD reconstruction export-level boundary.
    ExportDsdLevel {
        input: SignalId,
        output: SignalId,
        level: DsdGeneralExportLevel,
        /// Exact scalar applied at this boundary.  It is separate from the
        /// later ordinary gain policy so Guard's unity remains relative to the
        /// declared exported base rather than the protected reconstruction.
        gain_db: DbNano,
    },
    /// One sealed Reference terminal realization from protected R64 to the
    /// authoritative terminal QPCM sequence. The scalar is supplied by the
    /// preceding ReferenceGain decision and is applied exactly once together
    /// with the admitted dither/quantization/format realization.
    ReferenceTerminalRealization {
        input: SignalId,
        output: SignalId,
        decision: DecisionId,
        sample_contract: FinalPcmContract,
    },
    /// Package the current audio boundary as the exact requested product.
    PackageOutput {
        input: SignalId,
        output: ArtifactId,
        product: OutputProductIdentity,
    },
    /// Apply one authoritative metadata mutation to produce a new artifact state.
    MutateArtifact {
        input: ArtifactId,
        output: ArtifactId,
        effect: MetadataEffect,
    },
    /// Decode a delivered artifact solely to establish the subject of an output observation.
    DecodeArtifactForObservation {
        input: ArtifactId,
        output: SignalId,
    },
    /// Verify one concrete artifact without changing its decoded signal.
    VerifyArtifact {
        artifact: ArtifactId,
        observation: ObservationId,
    },
}

/// Immediate role of the SSRC output in the selected physical chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SsrcOutputRole {
    Nonterminal,
    Terminal,
}

/// Signal-arithmetic precision selected by the pinned SSRC profile family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SsrcComputationPrecision {
    Single,
    Double,
}

/// Why SSRC owns this resampling operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SsrcAuthorityReason {
    ExplicitForce,
    CapabilitySelected,
}

/// Resolved active lowering parameters attached to one common operation.
///
/// Only the selected parameter family is recorded. Where the specification
/// requires both requested and effective resampler values, the request fields
/// are retained beside their resolved values; semantic fingerprinting still
/// excludes request fields that are dormant after precedence/lowering.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ResolvedOperationParameters {
    None,
    ResampleSsrc {
        requested: SsrcSettings,
        effective_profile: SsrcProfile,
        effective_attenuation_db: Option<f32>,
        effective_output_depth: PcmBitDepth,
        output_role: SsrcOutputRole,
        computation_precision: SsrcComputationPrecision,
        emitted_processing_domain: ProcessingDomain,
        effective_dither: crate::plugins::ResolvedSsrcDither,
        authority_reason: SsrcAuthorityReason,
    },
    ResampleSox {
        requested: SoxResamplerSettings,
        quality: ResampleQuality,
        effective_bandwidth_pct: Option<f32>,
        effective_sinc_passband_hz: Option<f32>,
        effective_dither: Option<DitherType>,
    },
    ResampleSoxr {
        requested: SoxrResamplerSettings,
        effective_precision: u8,
        effective_cutoff: f32,
        effective_phase: Option<u8>,
        effective_dither: Option<DitherType>,
    },
    DsdToPcm {
        requested: DsdToPcmSettings,
        effective_sinc: Option<DsdToPcmSincSettings>,
        effective_dither: Option<DitherType>,
    },
    PcmToDsd {
        requested: PcmToDsdSettings,
        effective_sinc: Option<PcmToDsdSincSettings>,
        effective_gain_compensation: GainCompensation,
    },
    DsdRateChange {
        from_dsd: DsdToPcmSettings,
        effective_from_sinc: Option<DsdToPcmSincSettings>,
        to_dsd: PcmToDsdSettings,
    },
    EncodeFlac {
        requested: FlacSettings,
        effective_dither: Option<DitherType>,
    },
    EncodeMp3 { requested: Mp3Settings },
    EncodeAac { requested: AacSettings },
    EncodeOpus { requested: OpusSettings },
    EncodeWavPack {
        requested: WavPackSettings,
        effective_dither: Option<DitherType>,
    },
    EncodePcm { effective_dither: Option<DitherType> },
}

/// Current executor capability for an otherwise valid semantic plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ExecutionCapability {
    /// Existing execution path can lower this semantic plan now.
    ExecutableNow,
    /// Phase 3 common realizer executes this typed plan directly. The legacy
    /// command-plan lowerer must still refuse it rather than inventing a
    /// fallback topology.
    ExecutableByPhase3CommonRealizer,
    /// Transitional Phase-2 marker retained for deserializing/diagnosing old
    /// plans. Newly planned Phase-3 DSD Track Guard/Normalize routes use
    /// `ExecutableByPhase3CommonRealizer`.
    RequiresPhase3DsdTrackTruePeak,
    /// Phase 3 must connect the common streamed/mixed realization route.
    RequiresPhase3CommonRealizer,
}

/// Normalized source-relative intent. Inactive PCM/DSD settings do not become
/// competing authorities here.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NormalizedIntent {
    /// Exact output product request.
    pub target_format: AudioFormat,
    /// Target rate policy.
    pub target_rate: RateTarget,
    /// Target depth policy.
    pub target_depth: BitDepthTarget,
    /// Explicitly ordered registered sample-domain effects.
    pub processing: Vec<EffectIntent>,
    /// Ordinary gain policy selected for the actual source domain.
    pub gain_policy: SampleGainPolicy,
    /// Reconstruction authority for general DSD-to-PCM, otherwise None.
    pub dsd_reconstruction: Option<DsdGeneralReconstruction>,
    /// DSD reconstruction/export level for general DSD-to-PCM, otherwise None.
    pub dsd_export_level: Option<DsdGeneralExportLevel>,
    /// Qualified Reference delivery is a separate intent bit, never inferred from scan tier.
    pub reference_delivery: bool,
    /// ReplayGain projection remains explicit even while the old orchestrator owns execution.
    pub replay_gain: Option<ReplayGainProjectionPolicy>,
    /// Independent source tag/artwork transfer policy.
    pub metadata: MetadataTransferPolicy,
    /// Whether source-audio MD5 is requested.
    pub store_source_audio_md5: bool,
    /// Whether terminal verification is required.
    pub verify_after_encode: bool,
}

/// Complete Phase-2 semantic plan.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TypedConversionPlan {
    /// Normalized, source-relative request.
    pub intent: NormalizedIntent,
    /// Named audio states.
    pub audio_states: Vec<AudioState>,
    /// Typed operations/observations/decisions in semantic order.
    pub nodes: Vec<TypedPlanNode>,
    /// Named source/output artifact states.
    pub artifacts: Vec<ArtifactState>,
    /// Exact existing logical operations retained only as the temporary command-plan lowering bridge.
    pub lowering_bridge_operations: Vec<PlanOperation>,
    /// Whether the current executor can realize the plan.
    pub execution_capability: ExecutionCapability,
    /// Stable bridge declarations that must remain single-owner during migration.
    pub bridges: Vec<ExecutionBridge>,
}

/// Remaining execution bridges after native ReplayGain migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ExecutionBridge {
    /// Existing static command topology lowers ordinary executable plans.
    ExistingCommandPlan,
}

/// Pure planning refusal independent of resource exhaustion.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PlanRefusal {
    /// Stable category.
    pub code: String,
    /// Actionable reason.
    pub reason: String,
}

/// Missing fact requested from the source/materialization layer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RequiredFact {
    /// Fact key.
    pub key: String,
    /// Why planning needs it.
    pub reason: String,
}

/// Semantic planning result. Resource limits are reported separately by the
/// later physical-realization admission layer.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PlanningOutcome<T> {
    Ready(T),
    NeedFacts(Vec<RequiredFact>),
    Refused(PlanRefusal),
}

/// A physical-planning resource refusal, kept separate from semantic refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PlanningResourceLimit {
    pub resource: String,
    pub requested: u64,
    pub limit: u64,
}

impl std::fmt::Display for PlanningResourceLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "planning resource '{}' requested {}, limit {}",
            self.resource, self.requested, self.limit,
        )
    }
}

/// Physical candidate selection keeps an exhausted search budget distinct
/// from a proved semantic incompatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CandidateSelectionError {
    /// Candidate contracts prove there is no admitted realization in the
    /// inspected set (or a forced candidate is incompatible).
    Refused(PlanRefusal),
    /// The bounded alternative search could not inspect the requested set.
    Resource(PlanningResourceLimit),
}

const DEFAULT_CANDIDATE_ALTERNATIVE_LIMIT: usize = 8;

#[derive(Debug, Clone)]
struct CandidateSearchPolicy {
    max_alternatives_to_inspect: usize,
    forced_tool: Option<ToolIdentifier>,
    #[cfg(test)]
    strip_terminal_proof_for_tool: Option<ToolIdentifier>,
    #[cfg(test)]
    ssrc_binary64_evidence_override: Option<TestBinary64SsrcEvidenceOverride>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct TestBinary64SsrcEvidenceOverride {
    scope: crate::ssrc_binary64::Binary64ResampleEvidenceScope,
    evidence: crate::ssrc_binary64::Binary64ResamplePreservationEvidence,
}

impl Default for CandidateSearchPolicy {
    fn default() -> Self {
        Self {
            max_alternatives_to_inspect: DEFAULT_CANDIDATE_ALTERNATIVE_LIMIT,
            forced_tool: None,
            #[cfg(test)]
            strip_terminal_proof_for_tool: None,
            #[cfg(test)]
            ssrc_binary64_evidence_override: None,
        }
    }
}

/// Normalize settings against authoritative source facts.
pub fn normalize_intent(request: &PlanRequest) -> Result<NormalizedIntent, PlanningError> {
    let source_is_dsd = request.source.is_dsd();
    let reference_delivery = source_is_dsd
        && request.settings.dsd.from_dsd.pathway == DsdSourcePathway::Reference
        && !request.settings.target_format.is_dsd();
    if reference_delivery {
        let mut active_settings = request.settings.clone();
        active_settings.dsd.general_from_dsd = Default::default();
        active_settings.validate()?;
    } else {
        request.settings.validate()?;
    }
    request.source.validate()?;
    let gain_policy = if reference_delivery {
        // Qualified Reference delivery owns its exact gain policy separately.
        // Dormant ordinary general-gain settings are not another authority.
        SampleGainPolicy::Off
    } else if source_is_dsd && !request.settings.target_format.is_dsd() {
        request.settings.dsd.gain_policy()
    } else {
        request.settings.pcm_true_peak.policy
    };

    Ok(NormalizedIntent {
        target_format: request.settings.target_format.clone(),
        target_rate: request.settings.target_sample_rate,
        target_depth: request.settings.target_bit_depth,
        processing: Vec::new(),
        gain_policy,
        dsd_reconstruction: (source_is_dsd
            && !request.settings.target_format.is_dsd()
            && !reference_delivery)
            .then_some(request.settings.dsd.general_from_dsd.reconstruction),
        dsd_export_level: (source_is_dsd
            && !request.settings.target_format.is_dsd()
            && !reference_delivery)
            .then_some(request.settings.dsd.general_from_dsd.export_level),
        reference_delivery,
        replay_gain: ReplayGainProjectionPolicy::from_request(request),
        metadata: MetadataTransferPolicy {
            transfer_tags: request.settings.metadata.transfer_tags,
            preserve_artwork: request.settings.metadata.preserve_artwork,
        },
        store_source_audio_md5: request.settings.metadata.store_source_audio_md5,
        verify_after_encode: request.settings.verification.verify_after_encode
            || request.settings.flac.verify,
    })
}

fn track_scope_id(request: &PlanRequest) -> PlanScopeId {
    match &request.plan_scope {
        PlanScope::Track { scope_id, .. } => scope_id.clone(),
        PlanScope::SubmittedBatch { participant_id, .. } => {
            // Track policy inside an album submission remains track-local.
            // Reuse the participant identity but deliberately do not bind the
            // decision to the submitted-batch barrier.
            PlanScopeId(format!("track:{}", participant_id.0))
        }
    }
}

fn certified_true_peak_read_contract(
    state: &AudioState,
    scan: TruePeakScanTier,
    connected_executor: bool,
) -> Result<ObservationReadContract, PlanRefusal> {
    let contract = ObservationReadContract {
        authority: match scan {
            TruePeakScanTier::Reference => "tonepoet-true-peak:fast066v2_reference/certified_peak_meter/v1",
            TruePeakScanTier::Standard => "tonepoet-true-peak:fast066v2_standard/certified_peak_meter/v1",
            TruePeakScanTier::Fast => "tonepoet-true-peak:fast066v2_fast/certified_peak_meter/v1",
        }
        .to_owned(),
        accepted_processing_domains: all_pcm_processing_domains(),
        accepted_value_domains: all_pcm_value_domains(),
        complete_reader: true,
        connected_executor,
    };
    let requirements = candidate_requirements_for_state(state, false, true, false);
    if let Some(required) = requirements.input_processing_domain {
        if !contract.accepted_processing_domains.contains(&required) {
            return Err(PlanRefusal {
                code: "true_peak_reader_representation".to_owned(),
                reason: format!(
                    "certified true-peak reader {} does not admit processing domain {required:?}",
                    contract.authority
                ),
            });
        }
    }
    if let Some(required) = requirements.input_value_domain {
        if !contract.accepted_value_domains.contains(&required) {
            return Err(PlanRefusal {
                code: "true_peak_reader_value_domain".to_owned(),
                reason: format!(
                    "certified true-peak reader {} does not admit value domain {required:?}",
                    contract.authority
                ),
            });
        }
    }
    Ok(contract)
}

fn reference_certified_read_contract(
    state: &AudioState,
    reader_authority: &'static str,
) -> Result<ObservationReadContract, PlanRefusal> {
    let mut contract = certified_true_peak_read_contract(state, TruePeakScanTier::Reference, true)?;
    // Reference qualification binds both the independent complete-reader route
    // and the certified observer implementation.  Encoding both identities in
    // the read authority prevents a later reader substitution from reusing an
    // otherwise identical observation slot.
    contract.authority = format!(
        "reader={reader_authority};observer={REFERENCE_CERTIFIED_OBSERVER_ID}"
    );
    Ok(contract)
}

fn generic_complete_read_contract(
    authority: &'static str,
    connected_executor: bool,
) -> ObservationReadContract {
    ObservationReadContract {
        authority: authority.to_owned(),
        accepted_processing_domains: BTreeSet::new(),
        accepted_value_domains: BTreeSet::new(),
        complete_reader: true,
        connected_executor,
    }
}

fn album_gain_binding(request: &PlanRequest) -> Result<GainDecisionBinding, PlanRefusal> {
    match &request.plan_scope {
        PlanScope::SubmittedBatch {
            scope_id,
            participant_id,
            expected_participants,
        } => {
            let Some(expected_participants) = (*expected_participants).filter(|value| *value > 0) else {
                return Err(PlanRefusal {
                    code: "album_scope_requires_participant_count".to_owned(),
                    reason: "Album true-peak gain requires the persisted submitted-batch participant count".to_owned(),
                });
            };
            Ok(GainDecisionBinding::SubmittedBatch {
                scope: scope_id.clone(),
                participant: participant_id.clone(),
                expected_participants: Some(expected_participants),
            })
        },
        PlanScope::Track { .. } => Err(PlanRefusal {
            code: "album_scope_requires_submission".to_owned(),
            reason: "Album true-peak gain requires the existing submitted-batch identity and participant contract".to_owned(),
        }),
    }
}

fn replay_gain_binding(
    request: &PlanRequest,
    mode: ReplayGainMode,
) -> Result<ReplayGainGroupBinding, PlanRefusal> {
    match &request.plan_scope {
        PlanScope::Track { .. } => {
            // A non-submitted conversion is a complete one-item group. Album
            // and Both therefore have well-defined singleton semantics: the
            // album aggregate is the same complete observation as the track.
            // Do not manufacture a submitted-batch identity merely to express
            // that fact; the Track binding deliberately has no cross-item
            // participant barrier.
            Ok(ReplayGainGroupBinding::Track {
                scope: track_scope_id(request),
            })
        }
        PlanScope::SubmittedBatch {
            scope_id,
            participant_id,
            expected_participants,
        } if mode != ReplayGainMode::Track => {
            let Some(expected_participants) = (*expected_participants).filter(|value| *value > 0) else {
                return Err(PlanRefusal {
                    code: "replaygain_album_scope_requires_participant_count".to_owned(),
                    reason: "Album/Both ReplayGain requires the persisted submitted-batch participant count".to_owned(),
                });
            };
            Ok(ReplayGainGroupBinding::SubmittedBatch {
                scope: scope_id.clone(),
                participant: participant_id.clone(),
                expected_participants: Some(expected_participants),
            })
        }
        PlanScope::SubmittedBatch { .. } => Ok(ReplayGainGroupBinding::Track {
            scope: track_scope_id(request),
        }),
    }
}

fn resolved_runtime_album_gain(request: &PlanRequest) -> Result<Option<DbNano>, PlanRefusal> {
    let dsd = request.settings.dsd.runtime_album_gain_db();
    let pcm = request.settings.pcm_true_peak.runtime_album_gain_db();
    match (dsd, pcm) {
        (Some(left), Some(right)) if left != right => Err(PlanRefusal {
            code: "conflicting_runtime_album_gain".to_owned(),
            reason: "DSD and PCM runtime album-gain authorities disagree on the resolved scalar"
                .to_owned(),
        }),
        (Some(value), _) | (_, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

fn append_registered_effect_segment(
    states: &mut Vec<AudioState>,
    nodes: &mut Vec<TypedPlanNode>,
    next_signal: &mut u32,
    working_signal: &mut SignalId,
    effects: &[EffectIntent],
) -> Result<(), PlanRefusal> {
    if effects.is_empty() {
        return Ok(());
    }
    if !matches!(
        states
            .iter()
            .find(|state| state.id == *working_signal)
            .map(|state| &state.coding),
        Some(SignalCoding::Pcm)
    ) {
        return Err(PlanRefusal {
            code: "effect_subject_not_pcm".to_owned(),
            reason: "registered unary effects require an admitted PCM subject before effect ordering"
                .to_owned(),
        });
    }

    for instance in effects {
        let lowering = lower_registered_effect(&instance.effect)?;
        let input_state = states
            .iter()
            .find(|state| state.id == *working_signal)
            .cloned()
            .expect("working signal must have a typed state");
        let effected = SignalId(*next_signal);
        *next_signal += 1;
        states.push(AudioState {
            id: effected,
            coding: SignalCoding::Pcm,
            sample_rate_hz: input_state.sample_rate_hz,
            channels: input_state.channels,
            channel_layout: input_state.channel_layout,
            frame_extent: FrameExtent::Pending(format!(
                "effect.{}.frame_extent",
                instance.id.0
            )),
            programme: input_state.programme,
            // The registered effect realizer writes a Float64 carrier. Do not
            // inherit an integer source width or a source storage contract.
            precision: StoragePrecision::Pcm(PcmBitDepth::Float64),
            storage_contract: Fact::Pending(format!(
                "effect.{}.float64_carrier",
                instance.id.0
            )),
            processing_domain: Fact::Known(ProcessingDomain::PcmFloating),
            // A Float64 carrier does not by itself establish a protected value
            // or overload-preservation proof for the effect arithmetic.
            value_domain: ValueDomain::Pending(format!(
                "effect.{}.output_value_domain",
                instance.id.0
            )),
            level_basis: input_state.level_basis,
            claims: BTreeSet::new(),
            obligations: input_state.obligations,
        });
        nodes.push(TypedPlanNode::ApplyEffect {
            input: *working_signal,
            output: effected,
            instance: instance.clone(),
            lowering,
        });
        *working_signal = effected;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SsrcImmediateOutput {
    pub(crate) depth: PcmBitDepth,
    pub(crate) role: SsrcOutputRole,
}

fn ssrc_native_dither_override_active(request: &PlanRequest) -> bool {
    request.settings.ssrc.dither_id.is_some() || request.settings.ssrc.pdf_type.is_some()
}

pub(crate) fn resolve_ssrc_immediate_output(
    request: &PlanRequest,
    target_rate_hz: u32,
    final_depth: Option<PcmBitDepth>,
    has_post_resample_effects: bool,
    gain_policy: SampleGainPolicy,
) -> Result<SsrcImmediateOutput, PlanRefusal> {
    let native_override = ssrc_native_dither_override_active(request);
    let final_integer_depth = final_depth.filter(|depth| !depth.is_float());
    let native_terminal_request = native_override
        && request.settings.target_format.is_pcm_lossless()
        && final_integer_depth.is_some();

    let later_sample_processing = has_post_resample_effects
        || !matches!(gain_policy, SampleGainPolicy::Off)
        || request.settings.target_format.is_lossy();
    let package_requires_split = request.settings.target_format.is_pcm_lossless()
        && request.settings.target_format != AudioFormat::Wav;

    if native_terminal_request && (later_sample_processing || package_requires_split) {
        return Err(PlanRefusal {
            code: "ssrc_native_terminal_override_cannot_split".to_owned(),
            reason: "an active SSRC-native dither/PDF override requires SSRC to own the final integer terminal, but later processing or an unadmitted package-only cell requires a Float64 split".to_owned(),
        });
    }

    if let Some(PcmBitDepth::Int32) = final_depth {
        let explicit_global_int32 = crate::plugins::explicit_int32_dither_requested(
            &request.settings,
            Some(PcmBitDepth::Int32),
        );
        if native_override {
            return Err(PlanRefusal {
                code: "ssrc_int32_dither_unqualified".to_owned(),
                reason: "SSRC Int32 dither/noise-shaping ownership is not commissioned; the explicit native override cannot be silently reassigned to the retained FFmpeg terminal".to_owned(),
            });
        }
        if explicit_global_int32 {
            return Ok(SsrcImmediateOutput {
                depth: PcmBitDepth::Float64,
                role: SsrcOutputRole::Nonterminal,
            });
        }
    }

    let can_direct_terminal = !later_sample_processing
        && !package_requires_split
        && request.settings.target_format == AudioFormat::Wav
        && final_depth.is_some();
    if can_direct_terminal {
        let depth = final_depth.expect("terminal predicate requires a resolved depth");
        let dither = crate::plugins::resolve_ssrc_dither_for_rate(
            &request.settings,
            request.source.authoritative_pcm_depth(),
            Some(depth),
            target_rate_hz,
        )
        .map_err(|error| PlanRefusal {
            code: "ssrc_parameter_resolution".to_owned(),
            reason: error.to_string(),
        })?;
        if matches!(
            dither.availability,
            crate::plugins::SsrcDitherAvailability::UnavailableForSsrcTerminal { .. }
        ) {
            // This is a derived global-family fusion miss, not a whole-request
            // settings error. A later admitted terminal may own the dither.
            return Ok(SsrcImmediateOutput {
                depth: PcmBitDepth::Float64,
                role: SsrcOutputRole::Nonterminal,
            });
        }
        return Ok(SsrcImmediateOutput {
            depth,
            role: SsrcOutputRole::Terminal,
        });
    }

    Ok(SsrcImmediateOutput {
        depth: PcmBitDepth::Float64,
        role: SsrcOutputRole::Nonterminal,
    })
}

fn resolve_ssrc_terminal_realization(
    request: &PlanRequest,
    input_state: &AudioState,
    target_rate_hz: u32,
    target_bit_depth: PcmBitDepth,
) -> Result<SelectedTerminalRealization, PlanRefusal> {
    if request.settings.target_format != AudioFormat::Wav {
        return Err(PlanRefusal {
            code: "ssrc_package_cell_unavailable".to_owned(),
            reason: "SSRC terminal fusion is currently admitted only for direct WAV output; non-WAV lossless delivery must use the Float64 split terminal unless a package-only cell is separately qualified".to_owned(),
        });
    }
    let dither = crate::plugins::resolve_ssrc_dither_for_rate(
        &request.settings,
        request.source.authoritative_pcm_depth(),
        Some(target_bit_depth),
        target_rate_hz,
    )
    .map_err(|error| PlanRefusal {
        code: "ssrc_parameter_resolution".to_owned(),
        reason: error.to_string(),
    })?;
    if matches!(
        dither.availability,
        crate::plugins::SsrcDitherAvailability::UnavailableForSsrcTerminal { .. }
    ) {
        return Err(PlanRefusal {
            code: "ssrc_terminal_dither_unavailable".to_owned(),
            reason: "SSRC terminal fusion was selected although its resolved dither family is unavailable at the target rate".to_owned(),
        });
    }
    let dither_active = matches!(
        dither.availability,
        crate::plugins::SsrcDitherAvailability::Active
    );
    let effective_dither = (dither_active && dither.requested_global != DitherType::None)
        .then_some(dither.requested_global);
    Ok(SelectedTerminalRealization::Pcm(
        SelectedPcmTerminalRealization {
            kind: PcmTerminalRealizationKind::SsrcDirectWav,
            selected_tool: ToolIdentifier::Ssrc,
            input_precision: input_state.precision.clone(),
            input_value_domain: input_state.value_domain.clone(),
            target_format: AudioFormat::Wav,
            target_rate_hz: Some(target_rate_hz),
            target_bit_depth,
            wavpack_hybrid: false,
            effective_dither,
            ssrc_dither: Some(dither),
            dither_owner: if dither_active {
                PcmTerminalDitherOwner::SsrcResampler
            } else {
                PcmTerminalDitherOwner::None
            },
        },
    ))
}

/// Build the Phase-2 semantic plan without process I/O or command construction.
pub fn plan_typed(
    request: &PlanRequest,
) -> Result<PlanningOutcome<TypedConversionPlan>, PlanningResourceLimit> {
    plan_typed_with_effects_and_policy(request, &[], &CandidateSearchPolicy::default())
}

/// Build a typed plan with an explicit finite chain of registered unary effects.
///
/// The ordinary settings/UI does not expose this later DSP surface yet. Keeping
/// it as a pure planner API lets Phase 3 and a future screen use the same
/// registry without adding arbitrary shell syntax or redesigning gain policy.
pub fn plan_typed_with_effects(
    request: &PlanRequest,
    effects: &[EffectIntent],
) -> Result<PlanningOutcome<TypedConversionPlan>, PlanningResourceLimit> {
    plan_typed_with_effects_and_policy(request, effects, &CandidateSearchPolicy::default())
}

fn plan_typed_with_effects_and_policy(
    request: &PlanRequest,
    effects: &[EffectIntent],
    search_policy: &CandidateSearchPolicy,
) -> Result<PlanningOutcome<TypedConversionPlan>, PlanningResourceLimit> {
    let mut intent = match normalize_intent(request) {
        Ok(intent) => intent,
        Err(error) => {
            return Ok(PlanningOutcome::Refused(PlanRefusal {
                code: "invalid_request".to_owned(),
                reason: error.to_string(),
            }));
        }
    };

    if let Err(error) = validate_forced_ssrc_semantics(request) {
        return Ok(PlanningOutcome::Refused(PlanRefusal {
            code: "invalid_request".to_owned(),
            reason: error.to_string(),
        }));
    }

    let (pre_resample_effects, post_resample_effects) =
        match order_registered_effect_partitions(effects) {
            Ok(partitions) => partitions,
            Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
        };
    let mut ordered_effects = pre_resample_effects.clone();
    ordered_effects.extend(post_resample_effects.clone());
    if intent.reference_delivery && !ordered_effects.is_empty() {
        return Ok(PlanningOutcome::Refused(PlanRefusal {
            code: "reference_effect_incompatible".to_owned(),
            reason: "qualified Reference delivery is a sealed policy region; ordinary registered effects require an explicit general-processing request".to_owned(),
        }));
    }
    intent.processing = ordered_effects.clone();

    // Qualified Reference delivery must clear the same static admission matrix
    // as the retained Reference executor before the common planner can report
    // Ready. Facts that the source/catalog bridge may legitimately provide
    // later remain NeedFacts; known unsupported cells are refused by the shared
    // resolver below.
    let reference_admission = if intent.reference_delivery {
        let mut facts = Vec::new();
        if request.source.sample_rate_hz.is_none() {
            facts.push(RequiredFact {
                key: "source.sample_rate_hz".to_owned(),
                reason: "Reference admission requires the authoritative DSD source rate".to_owned(),
            });
        }
        if request.source.channels.is_none() {
            facts.push(RequiredFact {
                key: "source.channels".to_owned(),
                reason: "Reference admission requires the authoritative source channel count".to_owned(),
            });
        }
        if request.source.dsd_source_kind.is_none() {
            facts.push(RequiredFact {
                key: "source.dsd_source_kind".to_owned(),
                reason: "Reference admission requires the authoritative DSD container/front-end classification".to_owned(),
            });
        }
        if request.resolved_output_target.is_none() {
            facts.push(RequiredFact {
                key: "resolved_output_target".to_owned(),
                reason: "Reference admission requires one trusted catalog output target".to_owned(),
            });
        }
        if request.source.duration.is_none() {
            facts.push(RequiredFact {
                key: "source.duration".to_owned(),
                reason: "Reference admission requires the authoritative duration for streamed-carrier capacity and analyzer-workload bounds".to_owned(),
            });
        }
        if request.resolved_output_target == Some(ResolvedOutputTarget::WavRiff) {
            if request.planned_riff_non_audio_upper_bound_bytes.is_none() {
                facts.push(RequiredFact {
                    key: "planned_riff_non_audio_upper_bound_bytes".to_owned(),
                    reason: "Reference RIFF admission requires the authoritative non-audio byte bound".to_owned(),
                });
            }
        }
        if !facts.is_empty() {
            facts.sort_by(|left, right| left.key.cmp(&right.key));
            facts.dedup_by(|left, right| left.key == right.key);
            return Ok(PlanningOutcome::NeedFacts(facts));
        }
        match resolve_reference_static_admission(request) {
            Ok(admission) => Some(admission),
            Err(error) => {
                return Ok(PlanningOutcome::Refused(PlanRefusal {
                    code: "reference_admission_refused".to_owned(),
                    reason: error.to_string(),
                }));
            }
        }
    } else {
        None
    };

    if request.source.is_dsd() && !request.settings.target_format.is_dsd() {
        let mut facts = Vec::new();
        if request.source.sample_rate_hz.is_none() {
            facts.push(RequiredFact {
                key: "source.sample_rate_hz".to_owned(),
                reason: "DSD reconstruction requires the authoritative source DSD rate".to_owned(),
            });
        }
        if matches!(target_pcm_rate_fact(request), Fact::Pending(_)) {
            facts.push(RequiredFact {
                key: "target_pcm_rate".to_owned(),
                reason: "the terminal PCM rate must be resolved before a DSD reconstruction contract can be registered".to_owned(),
            });
        }
        if matches!(target_pcm_depth_fact(request), Fact::Pending(_)) {
            facts.push(RequiredFact {
                key: "target_pcm_depth".to_owned(),
                reason: "the terminal PCM precision must be resolved before a DSD reconstruction contract can be registered".to_owned(),
            });
        }
        if !facts.is_empty() {
            facts.sort_by(|left, right| left.key.cmp(&right.key));
            facts.dedup_by(|left, right| left.key == right.key);
            return Ok(PlanningOutcome::NeedFacts(facts));
        }
    }

    // Resolve the retained pure topology once.  The common planner uses this
    // only as the Phase-2 lowering bridge/current-route authority; semantic
    // nodes remain typed independently.  Caching it here avoids planning the
    // same ordinary route a second time later in this function.
    let topology_attempt = if intent.reference_delivery {
        None
    } else {
        Some(plan_topology(request))
    };

    let source_signal = SignalId(0);
    let mut states = vec![source_audio_state(request, source_signal)];
    let mut nodes = Vec::new();
    let mut next_signal = 1u32;
    let mut next_observation = 0u32;
    let mut next_decision = 0u32;
    let mut capability = ExecutionCapability::ExecutableNow;
    let mut selected_true_peak_read_contract: Option<ObservationReadContract> = None;

    let mut working_signal = source_signal;
    if request.source.is_dsd() && !request.settings.target_format.is_dsd() {
        let protected_reference_reconstruction = intent.reference_delivery
            || intent.dsd_reconstruction == Some(DsdGeneralReconstruction::ReferenceProtected);
        let reconstructed = SignalId(next_signal);
        next_signal += 1;
        let target_rate_hz = match reference_admission
            .as_ref()
            .map(|admission| Fact::Known(admission.target_rate_hz))
            .unwrap_or_else(|| target_pcm_rate_fact(request))
        {
            Fact::Known(value) => value,
            Fact::Pending(key) => {
                return Ok(PlanningOutcome::NeedFacts(vec![RequiredFact {
                    key,
                    reason: "DSD reconstruction requires a concrete terminal PCM rate".to_owned(),
                }]));
            }
            Fact::Unavailable(reason) => {
                return Ok(PlanningOutcome::Refused(PlanRefusal {
                    code: "pcm_rate_unavailable".to_owned(),
                    reason,
                }));
            }
        };
        let target_bit_depth = match reference_admission
            .as_ref()
            .map(|admission| Fact::Known(admission.depth))
            .unwrap_or_else(|| target_pcm_depth_fact(request))
        {
            Fact::Known(value) => value,
            Fact::Pending(key) => {
                return Ok(PlanningOutcome::NeedFacts(vec![RequiredFact {
                    key,
                    reason: "DSD reconstruction requires a concrete terminal PCM precision".to_owned(),
                }]));
            }
            Fact::Unavailable(reason) => {
                return Ok(PlanningOutcome::Refused(PlanRefusal {
                    code: "pcm_depth_unavailable".to_owned(),
                    reason,
                }));
            }
        };
        // Any sample-domain processing after DSD reconstruction needs a named
        // pre-terminal Float64 boundary.  In particular Guard/Normalize must
        // observe and scale this signal before the target-depth quantizer and
        // its terminal error allowance.  Off with no extra processing may keep
        // the legacy fused target-depth reconstruction.
        let split_preterminal_float = protected_reference_reconstruction
            || intent.gain_policy.is_active()
            || !ordered_effects.is_empty();
        let reconstruction_precision = if split_preterminal_float {
            StoragePrecision::Pcm(PcmBitDepth::Float64)
        } else {
            StoragePrecision::Pcm(target_bit_depth)
        };
        let reconstruction_storage = if protected_reference_reconstruction {
            Fact::Known("wave64-f64le".to_owned())
        } else if split_preterminal_float {
            Fact::Pending("dsd_reconstruction.float64_carrier".to_owned())
        } else {
            Fact::Pending("dsd_reconstruction.storage_encoding".to_owned())
        };
        let reconstruction_value_domain = if protected_reference_reconstruction {
            ValueDomain::Q1_31DerivedBinary64
        } else if split_preterminal_float || target_bit_depth.is_float() {
            ValueDomain::FiniteFloating
        } else {
            ValueDomain::IntegerLattice(target_bit_depth)
        };
        states.push(AudioState {
            id: reconstructed,
            coding: SignalCoding::Pcm,
            sample_rate_hz: Fact::Known(target_rate_hz),
            channels: source_channels_fact(request),
            channel_layout: source_channel_layout_fact(),
            frame_extent: FrameExtent::Pending("dsd_reconstruction.frame_extent".to_owned()),
            programme: ProgrammeState::IndependentTrack,
            precision: reconstruction_precision,
            storage_contract: reconstruction_storage,
            processing_domain: if split_preterminal_float {
                Fact::Known(ProcessingDomain::Binary64)
            } else {
                Fact::Pending("dsd_reconstruction.processing_domain".to_owned())
            },
            value_domain: reconstruction_value_domain,
            level_basis: if protected_reference_reconstruction {
                LevelBasis::ProtectedR64
            } else {
                LevelBasis::DsdNative
            },
            claims: if protected_reference_reconstruction {
                BTreeSet::from([Claim::ReferenceReconstruction])
            } else {
                BTreeSet::new()
            },
            obligations: BTreeSet::from([RuntimeObligation::CompleteReader(reconstructed)]),
        });
        let reconstruction_operation_depth = if split_preterminal_float {
            PcmBitDepth::Float64
        } else {
            target_bit_depth
        };
        let candidates = dsd_reconstruction_candidates(
            request,
            protected_reference_reconstruction,
            target_rate_hz,
            reconstruction_operation_depth,
        );
        let reconstruction_operation = PlanOperation::DsdToPcm {
            target_format: AudioFormat::Wav,
            target_rate_hz,
            target_bit_depth: reconstruction_operation_depth,
            lowpass: if intent.reference_delivery {
                DsdLowpassMethod::Auto
            } else {
                request.settings.dsd.general_from_dsd.lowpass
            },
        };
        let source_state = states
            .iter()
            .find(|state| state.id == source_signal)
            .expect("source signal must have a typed state");
        let requirements = candidate_requirements_for_state(source_state, false, true, true);
        let current_route = if protected_reference_reconstruction {
            Some(ToolIdentifier::Sox)
        } else {
            current_registered_tool_for_operation(request, &reconstruction_operation)
        };
        let selected_candidate = match selected_candidate_index(
            &candidates,
            current_route.as_ref(),
            &requirements,
            search_policy,
            false,
        ) {
            Ok(index) => index,
            Err(CandidateSelectionError::Refused(refusal)) => {
                return Ok(PlanningOutcome::Refused(refusal));
            }
            Err(CandidateSelectionError::Resource(limit)) => return Err(limit),
        };
        let resolved_parameters = match resolved_operation_parameters(
            request,
            &reconstruction_operation,
            candidates[selected_candidate].tool.as_ref(),
            None,
            None,
        ) {
            Ok(parameters) => parameters,
            Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
        };
        nodes.push(TypedPlanNode::Operation {
            operation: reconstruction_operation,
            input_signal: Some(source_signal),
            output_signal: Some(reconstructed),
            candidates,
            selected_candidate,
            resolved_parameters,
        });
        working_signal = reconstructed;

        if !intent.reference_delivery {
            let exported = SignalId(next_signal);
            next_signal += 1;
            let level = intent.dsd_export_level.unwrap_or(DsdGeneralExportLevel::Native);
            let reconstruction = intent
                .dsd_reconstruction
                .unwrap_or(DsdGeneralReconstruction::General);
            let export_gain_db = match resolve_dsd_general_export_gain(reconstruction, level) {
                Ok(gain) => gain,
                Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
            };
            states.push(AudioState {
                id: exported,
                coding: SignalCoding::Pcm,
                sample_rate_hz: Fact::Known(target_rate_hz),
                channels: source_channels_fact(request),
                channel_layout: source_channel_layout_fact(),
                frame_extent: FrameExtent::Pending("dsd_export.frame_extent".to_owned()),
                programme: ProgrammeState::IndependentTrack,
                precision: StoragePrecision::Pcm(PcmBitDepth::Float64),
                storage_contract: Fact::Pending(
                    "dsd_export.storage_or_fused_boundary".to_owned(),
                ),
                processing_domain: Fact::Known(ProcessingDomain::Binary64),
                value_domain: ValueDomain::FiniteFloating,
                level_basis: export_level_basis(level),
                claims: BTreeSet::new(),
                obligations: BTreeSet::new(),
            });
            nodes.push(TypedPlanNode::ExportDsdLevel {
                input: reconstructed,
                output: exported,
                level,
                gain_db: export_gain_db,
            });
            working_signal = exported;
            if protected_reference_reconstruction || export_gain_db != DbNano::ZERO {
                capability = ExecutionCapability::RequiresPhase3CommonRealizer;
            }
        }
    }

    let mut pcm_resample_inserted = false;
    let mut pcm_terminal_owned_by_resampler = false;
    if !request.source.is_dsd() && !request.settings.target_format.is_dsd() {
        let working_coding = states
            .iter()
            .find(|state| state.id == working_signal)
            .map(|state| state.coding.clone())
            .unwrap_or(SignalCoding::Unknown);
        if matches!(working_coding, SignalCoding::Lossy(_)) {
            let decoded = SignalId(next_signal);
            next_signal += 1;
            states.push(AudioState {
                id: decoded,
                coding: SignalCoding::Pcm,
                sample_rate_hz: request
                    .source
                    .sample_rate_hz
                    .map(Fact::Known)
                    .unwrap_or_else(|| Fact::Pending("source.sample_rate_hz".to_owned())),
                channels: source_channels_fact(request),
                channel_layout: source_channel_layout_fact(),
                frame_extent: source_frame_extent(request),
                programme: ProgrammeState::IndependentTrack,
                precision: StoragePrecision::Pending,
                storage_contract: Fact::Pending("source_decode.storage_encoding".to_owned()),
                processing_domain: Fact::Pending("source_decode.processing_domain".to_owned()),
                value_domain: ValueDomain::Pending("source_decode.value_domain".to_owned()),
                level_basis: LevelBasis::Ordinary,
                claims: BTreeSet::new(),
                obligations: BTreeSet::from([RuntimeObligation::CompleteReader(decoded)]),
            });
            nodes.push(TypedPlanNode::DecodeSourceForProcessing {
                input: working_signal,
                output: decoded,
            });
            working_signal = decoded;
        }

        let current_rate = states
            .iter()
            .find(|state| state.id == working_signal)
            .map(|state| state.sample_rate_hz.clone())
            .unwrap_or_else(|| Fact::Pending("processing.sample_rate_hz".to_owned()));
        let target_rate = target_pcm_rate_fact(request);
        match (current_rate, target_rate) {
            (Fact::Known(current), Fact::Known(target)) if current != target => {
                if let Err(refusal) = append_registered_effect_segment(
                    &mut states,
                    &mut nodes,
                    &mut next_signal,
                    &mut working_signal,
                    &pre_resample_effects,
                ) {
                    return Ok(PlanningOutcome::Refused(refusal));
                }
                if !pre_resample_effects.is_empty() {
                    capability = ExecutionCapability::ExecutableByPhase3CommonRealizer;
                }
                pcm_resample_inserted = true;
                let resampled = SignalId(next_signal);
                next_signal += 1;
                let input_state = states
                    .iter()
                    .find(|state| state.id == working_signal)
                    .cloned()
                    .expect("working signal must have a typed state");
                let resample_source_rate_hz = match &input_state.sample_rate_hz {
                    Fact::Known(rate) => *rate,
                    Fact::Pending(key) | Fact::Unavailable(key) => {
                        return Ok(PlanningOutcome::NeedFacts(vec![RequiredFact {
                            key: key.clone(),
                            reason: "PCM resampler evidence/admission requires the actual input rate at the ResamplePcm node".to_owned(),
                        }]));
                    }
                };
                let use_ssrc = request.settings.ssrc.force
                    || matches!(
                        request.settings.nyquist_transition,
                        crate::enums::NyquistTransition::BrickWall
                    );
                let bridge_resample = topology_attempt
                    .as_ref()
                    .and_then(|attempt| attempt.as_ref().ok())
                    .and_then(|topology| match topology {
                        TopologyPlan::Execute { steps, .. } => steps.iter().find_map(|step| {
                            match &step.operation {
                                PlanOperation::ResamplePcm { target_rate_hz, .. }
                                    if *target_rate_hz == target =>
                                {
                                    Some(step.operation.clone())
                                }
                                _ => None,
                            }
                        }),
                        TopologyPlan::Passthrough { .. } => None,
                    });
                let mut operation = bridge_resample.unwrap_or_else(|| PlanOperation::ResamplePcm {
                    target_rate_hz: target,
                    target_bit_depth: None,
                    profile: use_ssrc.then(|| {
                        mapping::ssrc_profile(
                            request.settings.ssrc,
                            request.settings.resample_quality,
                        )
                    }),
                    brick_wall: use_ssrc,
                });
                let proposed_final_depth = match request.settings.target_bit_depth {
                    BitDepthTarget::Pcm(depth) => Some(depth),
                    BitDepthTarget::Source => request.source.authoritative_pcm_depth(),
                }.or_else(|| match &operation {
                    PlanOperation::ResamplePcm { target_bit_depth, .. } => *target_bit_depth,
                    _ => None,
                });
                let mut ssrc_output_role = None;
                let mut ssrc_terminal_realization = None;
                if !use_ssrc
                    && (!pre_resample_effects.is_empty() || !post_resample_effects.is_empty())
                {
                    // The Phase-3 segmented effect realizer owns the later terminal.
                    // Keep a non-SSRC resampler on the Float64 continuation carrier so
                    // no integer quantization can occur before a registered effect or
                    // be charged a second time by the downstream terminal.
                    if let PlanOperation::ResamplePcm {
                        target_bit_depth, ..
                    } = &mut operation
                    {
                        *target_bit_depth = Some(PcmBitDepth::Float64);
                    }
                }
                if use_ssrc {
                    let immediate = match resolve_ssrc_immediate_output(
                        request,
                        target,
                        proposed_final_depth,
                        !post_resample_effects.is_empty(),
                        intent.gain_policy,
                    ) {
                        Ok(output) => output,
                        Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
                    };
                    if let PlanOperation::ResamplePcm {
                        target_bit_depth,
                        profile,
                        brick_wall,
                        ..
                    } = &mut operation
                    {
                        *target_bit_depth = Some(immediate.depth);
                        if profile.is_none() {
                            *profile = Some(mapping::ssrc_profile(
                                request.settings.ssrc,
                                request.settings.resample_quality,
                            ));
                        }
                        *brick_wall = true;
                    }
                    ssrc_output_role = Some(immediate.role);
                    if immediate.role == SsrcOutputRole::Terminal {
                        ssrc_terminal_realization = Some(match resolve_ssrc_terminal_realization(
                            request,
                            &input_state,
                            target,
                            immediate.depth,
                        ) {
                            Ok(realization) => realization,
                            Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
                        });
                    }
                }
                let operation_output_depth = match &operation {
                    PlanOperation::ResamplePcm { target_bit_depth, .. } => *target_bit_depth,
                    _ => None,
                };
                states.push(AudioState {
                    id: resampled,
                    coding: SignalCoding::Pcm,
                    sample_rate_hz: Fact::Known(target),
                    channels: input_state.channels.clone(),
                    channel_layout: input_state.channel_layout.clone(),
                    frame_extent: FrameExtent::Pending("resampler.output_frame_extent".to_owned()),
                    programme: input_state.programme.clone(),
                    precision: operation_output_depth
                        .map(StoragePrecision::Pcm)
                        .unwrap_or_else(|| input_state.precision.clone()),
                    storage_contract: Fact::Pending("resampler.output_storage".to_owned()),
                    processing_domain: operation_output_depth
                        .map(processing_domain_for_pcm_depth)
                        .map(Fact::Known)
                        .unwrap_or_else(|| input_state.processing_domain.clone()),
                    value_domain: operation_output_depth
                        .map(|depth| {
                            if depth.is_float() {
                                ValueDomain::FiniteFloating
                            } else {
                                ValueDomain::IntegerLattice(depth)
                            }
                        })
                        .unwrap_or_else(|| input_state.value_domain.clone()),
                    level_basis: input_state.level_basis.clone(),
                    claims: BTreeSet::new(),
                    obligations: input_state.obligations.clone(),
                });
                let mut candidates = registered_candidates_for_operation(
                    request,
                    &operation,
                    contract_for_operation(&operation),
                    Some(resample_source_rate_hz),
                );
                // The strong SSRC contract starts at an independently admitted
                // stored Float64 boundary. The retained authority is available
                // for ordinary PCM materialization only when no pre-resample
                // registered effect owns that boundary. DSD and pre-effect
                // cells remain unavailable until separately admitted.
                if !request.source.is_dsd() && pre_resample_effects.is_empty() {
                    let authority =
                        crate::ssrc_binary64::ProtectedFloat64IngressAuthority::retained_pcm_riff_wav();
                    if protected_float64_riff_ingress_admits(
                        &authority,
                        &input_state.channels,
                        &input_state.frame_extent,
                    ) {
                        for candidate in &mut candidates {
                            if candidate.tool == Some(ToolIdentifier::Ssrc) {
                                candidate.protected_float64_ingress_authority = Some(authority.clone());
                            }
                        }
                    }
                }
                if let Some(realization) = ssrc_terminal_realization.as_ref() {
                    for candidate in &mut candidates {
                        if candidate.tool == Some(ToolIdentifier::Ssrc) {
                            candidate.contract.terminal_realization = Some(realization.clone());
                        }
                    }
                }
                #[cfg(test)]
                apply_resampler_evidence_test_override(
                    &mut candidates,
                    request,
                    &operation,
                    resample_source_rate_hz,
                    search_policy,
                );
                let pcm_hard_ceiling_resampler = !request.source.is_dsd()
                    && intent.gain_policy.is_true_peak();
                if pcm_hard_ceiling_resampler {
                    // The existing PCM hard-ceiling path requires the
                    // pre-measurement rate-change carrier to preserve finite
                    // floating overload above 0 dBFS.  Only the admitted
                    // FFmpeg/soxr carrier has that contract today; SoX and
                    // SSRC must not inherit it from the generic ResamplePcm
                    // operation merely because they accept PCM input.
                    for candidate in &mut candidates {
                        if candidate.tool == Some(ToolIdentifier::Ffmpeg) {
                            candidate.contract.runtime_obligations.insert(
                                "preserve_floating_overload_before_true_peak".to_owned(),
                            );
                        }
                    }
                }
                let mut requirements = candidate_requirements_for_state(
                    &input_state,
                    false,
                    true,
                    true,
                );
                if pcm_hard_ceiling_resampler {
                    requirements.binary64_resample_preservation_required = true;
                }
                let route_operation = topology_attempt
                    .as_ref()
                    .and_then(|attempt| attempt.as_ref().ok())
                    .and_then(|topology| match topology {
                        TopologyPlan::Execute { steps, .. } => steps.iter().find_map(|step| {
                            match &step.operation {
                                PlanOperation::ResamplePcm { target_rate_hz, .. }
                                    if *target_rate_hz == target =>
                                {
                                    Some(step.operation.clone())
                                }
                                PlanOperation::EncodePcm {
                                    target_rate_hz: Some(target_rate_hz),
                                    apply_processing: true,
                                    ..
                                }
                                | PlanOperation::EncodeLossy {
                                    target_rate_hz: Some(target_rate_hz),
                                    apply_processing: true,
                                    ..
                                } if *target_rate_hz == target => Some(step.operation.clone()),
                                _ => None,
                            }
                        }),
                        TopologyPlan::Passthrough { .. } => None,
                    })
                    .unwrap_or_else(|| operation.clone());
                let current_route = if pcm_hard_ceiling_resampler
                    && matches!(
                        request.settings.preferred_tool,
                        PreferredTool::Auto | PreferredTool::Ffmpeg
                    )
                {
                    // The retained hard-ceiling carrier path explicitly
                    // selects FFmpeg/soxr for Auto and FFmpeg requests so
                    // above-full-scale samples survive until measurement.
                    Some(ToolIdentifier::Ffmpeg)
                } else {
                    current_registered_tool_for_operation(request, &route_operation)
                };
                let selected_candidate = match selected_candidate_index(
                    &candidates,
                    current_route.as_ref(),
                    &requirements,
                    search_policy,
                    request.settings.ssrc.force,
                ) {
                    Ok(index) => index,
                    Err(CandidateSelectionError::Refused(refusal)) => {
                        return Ok(PlanningOutcome::Refused(refusal));
                    }
                    Err(CandidateSelectionError::Resource(limit)) => return Err(limit),
                };
                if pcm_hard_ceiling_resampler
                    && !pre_resample_effects.is_empty()
                    && candidates[selected_candidate].tool.as_ref() != Some(&ToolIdentifier::Ssrc)
                {
                    for effect in &pre_resample_effects {
                        let lowering = match lower_registered_effect(&effect.effect) {
                            Ok(lowering) => lowering,
                            Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
                        };
                        if !effect_contract_preserves_protected_true_peak_input(&lowering.contract) {
                            return Ok(PlanningOutcome::Refused(PlanRefusal {
                                code: "protected_pre_resample_effect_unqualified".to_owned(),
                                reason: format!(
                                    "registered effect {:?} does not carry its own protected Binary64 overload-preservation contract before the certified hard-ceiling resampler",
                                    effect.effect
                                ),
                            }));
                        }
                    }
                }
                if current_route.is_some()
                    && candidates[selected_candidate].tool.as_ref() != current_route.as_ref()
                    && capability == ExecutionCapability::ExecutableNow
                {
                    // The semantic route is admitted, but the retained command
                    // bridge would still choose a different physical backend.
                    // Phase 2 must not execute that mismatched route.
                    capability = ExecutionCapability::RequiresPhase3CommonRealizer;
                }
                let selected_ssrc_has_strong_binary64 = requirements
                    .binary64_resample_preservation_required
                    && candidates[selected_candidate].tool.as_ref() == Some(&ToolIdentifier::Ssrc)
                    && matches!(
                        candidates[selected_candidate]
                            .binary64_resample_preservation_evidence
                            .as_ref(),
                        Some(crate::ssrc_binary64::Binary64ResamplePreservationEvidence::Established { .. })
                    );
                let selected_ssrc_processing_domain = if candidates[selected_candidate]
                    .tool
                    .as_ref()
                    == Some(&ToolIdentifier::Ssrc)
                {
                    operation_output_depth.map(|depth| {
                        if selected_ssrc_has_strong_binary64 {
                            ProcessingDomain::Binary64
                        } else {
                            ordinary_ssrc_processing_domain(depth)
                        }
                    })
                } else {
                    None
                };
                if candidates[selected_candidate].tool.as_ref() == Some(&ToolIdentifier::Ssrc) {
                    pcm_terminal_owned_by_resampler = matches!(
                        ssrc_output_role,
                        Some(SsrcOutputRole::Terminal)
                    );
                    if let Some(domain) = selected_ssrc_processing_domain.clone() {
                        candidates[selected_candidate]
                            .contract
                            .representation
                            .emitted_processing_domain = Some(domain.clone());
                        if let Some(state) = states.iter_mut().find(|state| state.id == resampled) {
                            state.processing_domain = Fact::Known(domain);
                            if selected_ssrc_has_strong_binary64 {
                                state.value_domain = ValueDomain::FiniteFloating;
                                state.precision = StoragePrecision::Pcm(PcmBitDepth::Float64);
                                state.claims.insert(Claim::OverloadPreservedUntil(resampled));
                            }
                        }
                    }
                }
                let resolved_parameters = match resolved_operation_parameters(
                    request,
                    &operation,
                    candidates[selected_candidate].tool.as_ref(),
                    ssrc_output_role,
                    selected_ssrc_processing_domain,
                ) {
                    Ok(parameters) => parameters,
                    Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
                };
                nodes.push(TypedPlanNode::Operation {
                    operation: operation.clone(),
                    input_signal: Some(working_signal),
                    output_signal: Some(resampled),
                    candidates,
                    selected_candidate,
                    resolved_parameters,
                });
                working_signal = resampled;
            }
            (Fact::Known(current), Fact::Known(target)) if current == target => {
                if !pre_resample_effects.is_empty() {
                    return Ok(PlanningOutcome::Refused(PlanRefusal {
                        code: "effect_before_resample_without_resample".to_owned(),
                        reason: "BeforePcmResample effect placement requires an actual PCM sample-rate conversion"
                            .to_owned(),
                    }));
                }
            }
            (Fact::Pending(key), Fact::Known(_)) => {
                return Ok(PlanningOutcome::NeedFacts(vec![RequiredFact {
                    key,
                    reason: "an explicit/effective PCM target rate needs the source rate to decide whether resampling is part of the semantic spine".to_owned(),
                }]));
            }
            (_, Fact::Unavailable(reason)) => {
                return Ok(PlanningOutcome::Refused(PlanRefusal {
                    code: "pcm_rate_unavailable".to_owned(),
                    reason,
                }));
            }
            (_, Fact::Pending(key)) if request.settings.target_format.is_lossy() => {
                return Ok(PlanningOutcome::NeedFacts(vec![RequiredFact {
                    key,
                    reason: "lossy encoder-input rate is route-selecting and must be resolved before planning".to_owned(),
                }]));
            }
            _ => {}
        }
    }

    if !pre_resample_effects.is_empty() && !pcm_resample_inserted {
        return Ok(PlanningOutcome::Refused(PlanRefusal {
            code: "effect_before_resample_without_resample".to_owned(),
            reason: "BeforePcmResample effect placement requires an actual PCM sample-rate conversion"
                .to_owned(),
        }));
    }

    let protected_ssrc_claim_reaches_post_effects = true_peak_policy(intent.gain_policy).is_some()
        && states
            .iter()
            .find(|state| state.id == working_signal)
            .is_some_and(|state| {
                state
                    .claims
                    .contains(&Claim::OverloadPreservedUntil(working_signal))
            });
    if protected_ssrc_claim_reaches_post_effects {
        for effect in &post_resample_effects {
            let lowering = match lower_registered_effect(&effect.effect) {
                Ok(lowering) => lowering,
                Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
            };
            if !effect_contract_preserves_protected_true_peak_input(&lowering.contract) {
                return Ok(PlanningOutcome::Refused(PlanRefusal {
                    code: "protected_post_resample_effect_unqualified".to_owned(),
                    reason: format!(
                        "registered effect {:?} does not carry its own protected Binary64 overload-preservation contract before certified true-peak observation",
                        effect.effect
                    ),
                }));
            }
        }
    }

    if let Err(refusal) = append_registered_effect_segment(
        &mut states,
        &mut nodes,
        &mut next_signal,
        &mut working_signal,
        &post_resample_effects,
    ) {
        return Ok(PlanningOutcome::Refused(refusal));
    }
    if !post_resample_effects.is_empty() {
        capability = ExecutionCapability::ExecutableByPhase3CommonRealizer;
    }

    if let Some((target_dbtp, scope, scan, allow_boost)) = true_peak_policy(intent.gain_policy) {
        let runtime_album_gain = if scope == TruePeakScope::Album {
            match resolved_runtime_album_gain(request) {
                Ok(value) => value,
                Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
            }
        } else {
            None
        };

        if let Some(gain_db) = runtime_album_gain {
            // Album observation and reduction already completed at the
            // submitted-batch barrier. This downstream carrier must apply the
            // bound scalar exactly once; re-observing it as an independent
            // track would both discard album authority and permit a different
            // decision. Keep the original true-peak intent for terminal proof
            // selection, but represent the realized action as a fixed scalar.
            let gained = SignalId(next_signal);
            next_signal += 1;
            let input_state = states
                .iter()
                .find(|state| state.id == working_signal)
                .cloned()
                .expect("working signal must have a typed state");
            states.push(AudioState {
                id: gained,
                coding: SignalCoding::Pcm,
                sample_rate_hz: input_state.sample_rate_hz,
                channels: input_state.channels,
                channel_layout: input_state.channel_layout,
                frame_extent: input_state.frame_extent,
                programme: input_state.programme,
                precision: StoragePrecision::Pcm(PcmBitDepth::Float64),
                storage_contract: input_state.storage_contract,
                processing_domain: Fact::Known(ProcessingDomain::Binary64),
                value_domain: ValueDomain::FiniteFloating,
                level_basis: input_state.level_basis,
                claims: BTreeSet::new(),
                obligations: BTreeSet::from([
                    RuntimeObligation::TerminalErrorBound(gained),
                    RuntimeObligation::PublicationBarrier,
                ]),
            });
            nodes.push(TypedPlanNode::ApplyGain {
                input: working_signal,
                output: gained,
                policy: SampleGainPolicy::FixedGain { gain_db },
                decision: None,
            });
            working_signal = gained;
        } else {
            let binding = if scope == TruePeakScope::Album {
                match album_gain_binding(request) {
                    Ok(binding) => binding,
                    Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
                }
            } else {
                GainDecisionBinding::Track {
                    scope: track_scope_id(request),
                }
            };
            let observation_scope = match &binding {
                GainDecisionBinding::Track { scope }
                | GainDecisionBinding::SubmittedBatch { scope, .. } => scope.clone(),
            };
            let input_state = states
                .iter()
                .find(|state| state.id == working_signal)
                .cloned()
                .expect("working signal must have a typed state");
            // Phase 3 owns the complete certified reader for ordinary PCM and
            // general DSD-to-PCM in both Track and Album scope. The reader remains
            // part of the typed contract; execution must consume this authority
            // rather than reconstructing a scan policy from settings.
            let reader_connected = true;
            let read_contract = match certified_true_peak_read_contract(
                &input_state,
                scan,
                reader_connected,
            ) {
                Ok(contract) => contract,
                Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
            };
            selected_true_peak_read_contract = Some(read_contract.clone());
            let observation = ObservationId(next_observation);
            next_observation += 1;
            nodes.push(TypedPlanNode::Observe(Observation {
                id: observation,
                scope: observation_scope.clone(),
                participant: request.plan_scope.participant_id().clone(),
                subject: working_signal,
                artifact_subject: None,
                kind: ObservationKind::CertifiedTruePeak { scan },
                complete_reader_required: true,
                read_contract,
            }));
            let gained = SignalId(next_signal);
            next_signal += 1;
            let dependency = ScopedObservationId {
                scope: observation_scope.clone(),
                participant: request.plan_scope.participant_id().clone(),
                observation,
                purpose: ObservationClass::CertifiedTruePeak,
            };
            let album_participant = match &binding {
                GainDecisionBinding::SubmittedBatch {
                    scope,
                    participant,
                    expected_participants,
                } => Some(AlbumGainParticipantInput {
                    scope: scope.clone(),
                    participant: participant.clone(),
                    expected_participants: *expected_participants,
                    observation: dependency.clone(),
                    terminal_subject: gained,
                    terminal_proof: None,
                }),
                GainDecisionBinding::Track { .. } => None,
            };
            let decision = DecisionId(next_decision);
            next_decision += 1;
            nodes.push(TypedPlanNode::Decide(Decision {
                id: decision,
                kind: DecisionKind::TruePeakGain {
                    scope,
                    target_dbtp,
                    allow_boost,
                    binding: binding.clone(),
                    album_participant,
                },
                observations: vec![dependency],
            }));
            states.push(AudioState {
                id: gained,
                coding: SignalCoding::Pcm,
                sample_rate_hz: input_state.sample_rate_hz,
                channels: input_state.channels,
                channel_layout: input_state.channel_layout,
                frame_extent: input_state.frame_extent,
                programme: input_state.programme,
                // The controlling scalar is realized on the retained Binary64
                // carrier. Preserve source provenance separately, but make the
                // post-gain signal representation truthful for terminal selection.
                precision: StoragePrecision::Pcm(PcmBitDepth::Float64),
                storage_contract: input_state.storage_contract,
                processing_domain: Fact::Known(ProcessingDomain::Binary64),
                value_domain: ValueDomain::FiniteFloating,
                level_basis: input_state.level_basis,
                // The ceiling is a requested execution result, not a discharged
                // planning-time fact. The observation/decision plus terminal and
                // publication obligations carry the planned guarantee until the
                // executor proves it.
                claims: BTreeSet::new(),
                obligations: {
                    let mut obligations = BTreeSet::from([
                        RuntimeObligation::CompleteReader(working_signal),
                        RuntimeObligation::TerminalErrorBound(gained),
                        RuntimeObligation::PublicationBarrier,
                    ]);
                    if let GainDecisionBinding::SubmittedBatch {
                        scope,
                        participant,
                        expected_participants,
                    } = &binding
                    {
                        obligations.insert(RuntimeObligation::AlbumParticipantBarrier {
                            scope: scope.clone(),
                            participant: participant.clone(),
                            expected_participants: *expected_participants,
                        });
                    }
                    obligations
                },
            });
            nodes.push(TypedPlanNode::ApplyGain {
                input: working_signal,
                output: gained,
                policy: intent.gain_policy,
                decision: Some(decision),
            });
            working_signal = gained;

            if !intent.reference_delivery
                && intent.gain_policy.is_true_peak()
                && !request.settings.target_format.is_dsd()
            {
                let mandatory_dsd_track = request.source.is_dsd()
                    && scope == TruePeakScope::Track
                    && capability == ExecutionCapability::ExecutableNow;
                if mandatory_dsd_track
                    || capability == ExecutionCapability::RequiresPhase3DsdTrackTruePeak
                {
                    // Phase 3 gives the formerly missing general-DSD Track route a
                    // connected execution owner. Preserve the Phase-2
                    // `RequiresPhase3CommonRealizer` marker for other admitted
                    // physical-plan divergences (for example a soft SoX preference
                    // whose hard-ceiling resampler candidate is FFmpeg/soxr): the
                    // Phase-3 application consumes that selected plan directly,
                    // while the legacy command-plan lowerer must still refuse it.
                    capability = ExecutionCapability::ExecutableByPhase3CommonRealizer;
                }
            }
        }
    } else if matches!(intent.gain_policy, SampleGainPolicy::FixedGain { .. }) {
        let gained = SignalId(next_signal);
        next_signal += 1;
        let input_state = states
            .iter()
            .find(|state| state.id == working_signal)
            .cloned()
            .expect("working signal must have a typed state");
        states.push(AudioState {
            id: gained,
            coding: SignalCoding::Pcm,
            sample_rate_hz: input_state.sample_rate_hz,
            channels: input_state.channels,
            channel_layout: input_state.channel_layout,
            frame_extent: input_state.frame_extent,
            programme: input_state.programme,
            precision: input_state.precision,
            storage_contract: input_state.storage_contract,
            processing_domain: Fact::Known(ProcessingDomain::Binary64),
            value_domain: ValueDomain::FiniteFloating,
            level_basis: input_state.level_basis,
            claims: BTreeSet::new(),
            obligations: BTreeSet::new(),
        });
        nodes.push(TypedPlanNode::ApplyGain {
            input: working_signal,
            output: gained,
            policy: intent.gain_policy,
            decision: None,
        });
        working_signal = gained;
    }

    let (lowering_bridge_operations, bridge_changes_samples, bridge_passthrough) = if intent.reference_delivery {
        (Vec::new(), true, false)
    } else if capability != ExecutionCapability::ExecutableNow {
        // A capability-gated Phase-3 route has no valid Phase-2 command
        // lowering.  Do not attach a partial legacy topology that omits the
        // very operation which made the semantic plan non-executable.
        (Vec::new(), true, false)
    } else {
        match topology_attempt
            .expect("non-Reference requests cache one pure topology attempt")
        {
            Ok(TopologyPlan::Passthrough { .. }) => (Vec::new(), false, true),
            Ok(TopologyPlan::Execute { steps, .. }) => {
                let changes_samples = steps.iter().any(|step| operation_changes_samples(&step.operation));
                (
                    steps.into_iter().map(|step| step.operation).collect::<Vec<_>>(),
                    changes_samples,
                    false,
                )
            }
            Err(error) if capability == ExecutionCapability::ExecutableNow => {
                return Ok(PlanningOutcome::Refused(PlanRefusal {
                    code: "existing_lowering_refused".to_owned(),
                    reason: error.to_string(),
                }));
            }
            Err(_) => (Vec::new(), true, false),
        }
    };

    // Phase 5 expresses the sealed Reference terminal region in the common
    // model.  The protected reconstruction remains a distinct proof-bearing
    // R64 signal.  Gain authority, the one terminal realization, and the
    // independent post-terminal observation are explicit nodes; qualification
    // and publication remain runtime obligations rather than planning-time
    // claims.
    if intent.reference_delivery {
        let admission = reference_admission
            .as_ref()
            .expect("Reference delivery passed static admission before graph construction");
        let protected_state = states
            .iter()
            .find(|state| state.id == working_signal)
            .cloned()
            .expect("Reference protected R64 state must exist");

        let pre_observation = ObservationId(next_observation);
        next_observation += 1;
        let pre_scope = track_scope_id(request);
        nodes.push(TypedPlanNode::Observe(Observation {
            id: pre_observation,
            scope: pre_scope.clone(),
            participant: request.plan_scope.participant_id().clone(),
            subject: working_signal,
            artifact_subject: None,
            kind: ObservationKind::CertifiedTruePeak {
                scan: TruePeakScanTier::Reference,
            },
            complete_reader_required: true,
            read_contract: match reference_certified_read_contract(
                &protected_state,
                REFERENCE_R64_READER_ID,
            ) {
                Ok(contract) => contract,
                Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
            },
        }));

        let decision = DecisionId(next_decision);
        next_decision += 1;
        nodes.push(TypedPlanNode::Decide(Decision {
            id: decision,
            kind: DecisionKind::ReferenceGain {
                policy: admission.gain_policy,
            },
            observations: vec![ScopedObservationId {
                scope: pre_scope,
                participant: request.plan_scope.participant_id().clone(),
                observation: pre_observation,
                purpose: ObservationClass::CertifiedTruePeak,
            }],
        }));

        let qpcm = SignalId(next_signal);
        next_signal += 1;
        let (precision, processing_domain, value_domain, storage_contract) = match admission.depth {
            PcmBitDepth::Int24 => (
                StoragePrecision::Pcm(PcmBitDepth::Int24),
                ProcessingDomain::PcmInteger(PcmBitDepth::Int24),
                ValueDomain::IntegerLattice(PcmBitDepth::Int24),
                "wave64-pcm-s24le",
            ),
            PcmBitDepth::Float32 => (
                StoragePrecision::Pcm(PcmBitDepth::Float32),
                ProcessingDomain::PcmFloating,
                ValueDomain::FiniteFloating,
                "wave64-pcm-f32le",
            ),
            PcmBitDepth::Float64 => (
                StoragePrecision::Pcm(PcmBitDepth::Float64),
                ProcessingDomain::Binary64,
                ValueDomain::FiniteFloating,
                "wave64-pcm-f64le",
            ),
            unsupported => {
                return Ok(PlanningOutcome::Refused(PlanRefusal {
                    code: "reference_terminal_depth".to_owned(),
                    reason: format!(
                        "qualified Reference terminal has no common-model realization for {unsupported:?}"
                    ),
                }));
            }
        };
        states.push(AudioState {
            id: qpcm,
            coding: SignalCoding::Pcm,
            sample_rate_hz: Fact::Known(admission.target_rate_hz),
            channels: Fact::Known(admission.channels),
            channel_layout: source_channel_layout_fact(),
            // The executor establishes exact equality to the validated R64
            // programme extent after the terminal write.
            frame_extent: FrameExtent::Pending("reference_qpcm.exact_extent".to_owned()),
            programme: ProgrammeState::IndependentTrack,
            precision,
            storage_contract: Fact::Known(storage_contract.to_owned()),
            processing_domain: Fact::Known(processing_domain),
            value_domain,
            level_basis: LevelBasis::Ordinary,
            // ReferenceDsdDelivery is intentionally not granted here.  QPCM is
            // only the terminal sample authority; packaging, metadata identity,
            // qualification and the publication barrier are still outstanding.
            claims: BTreeSet::new(),
            obligations: BTreeSet::from([
                RuntimeObligation::CompleteReader(qpcm),
                RuntimeObligation::TerminalErrorBound(qpcm),
                RuntimeObligation::PublicationBarrier,
            ]),
        });
        nodes.push(TypedPlanNode::ReferenceTerminalRealization {
            input: working_signal,
            output: qpcm,
            decision,
            sample_contract: admission.final_pcm,
        });
        working_signal = qpcm;

        let post_observation = ObservationId(next_observation);
        next_observation += 1;
        let qpcm_state = states
            .iter()
            .find(|state| state.id == qpcm)
            .cloned()
            .expect("Reference QPCM state must exist");
        nodes.push(TypedPlanNode::Observe(Observation {
            id: post_observation,
            scope: track_scope_id(request),
            participant: request.plan_scope.participant_id().clone(),
            subject: qpcm,
            artifact_subject: None,
            kind: ObservationKind::CertifiedTruePeak {
                scan: TruePeakScanTier::Reference,
            },
            complete_reader_required: true,
            read_contract: match reference_certified_read_contract(
                &qpcm_state,
                REFERENCE_QPCM_READER_ID,
            ) {
                Ok(contract) => contract,
                Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
            },
        }));
    }

    // Make the terminal sample/codec conversion visible in the semantic spine.
    // The old command topology remains a temporary lowering bridge, but it is
    // not allowed to hide the quantizer/encoder/modulator boundary from
    // observations, claims, or plan identity.
    if !intent.reference_delivery && !bridge_passthrough && !pcm_terminal_owned_by_resampler {
        if let Some(operation) = semantic_terminal_operation(request, &lowering_bridge_operations) {
            let input_state = states
                .iter()
                .find(|state| state.id == working_signal)
                .cloned()
                .expect("working signal must have a typed state");
            let terminal = SignalId(next_signal);
            next_signal += 1;
            let terminal_state = terminal_audio_state(request, terminal, &input_state, &operation);
            states.push(terminal_state);
            let selected_reader_obligation = selected_true_peak_read_contract.as_ref().map(|reader| {
                format!("observation_reader:{}", reader.authority)
            });
            let mut candidates = registered_candidates_for_operation(
                request,
                &operation,
                contract_for_operation(&operation),
                None,
            );
            for candidate in &mut candidates {
                candidate.contract = terminal_contract_for_candidate(
                    request,
                    &operation,
                    intent.gain_policy,
                    request.source.is_dsd(),
                    &input_state,
                    candidate.tool.as_ref(),
                );
                if let Some(obligation) = selected_reader_obligation.as_ref() {
                    candidate.contract.runtime_obligations.insert(obligation.clone());
                }
                if selected_true_peak_read_contract.is_some() {
                    candidate
                        .contract
                        .runtime_obligations
                        .insert("certified_true_peak_observation".to_owned());
                }
            }
            #[cfg(test)]
            apply_candidate_test_overrides(&mut candidates, search_policy);
            let mut requirements = candidate_requirements_for_state(
                &input_state,
                intent.gain_policy.is_true_peak(),
                true,
                true,
            );
            requirements.terminal_realization_required =
                matches!(operation, PlanOperation::EncodePcm { .. });
            if let Some(obligation) = selected_reader_obligation {
                requirements.required_runtime_obligations.insert(obligation);
            }
            if selected_true_peak_read_contract.is_some() {
                requirements
                    .required_runtime_obligations
                    .insert("certified_true_peak_observation".to_owned());
            }
            let ssrc_split_ffmpeg_terminal = matches!(&operation, PlanOperation::EncodePcm { .. })
                && lowering_bridge_operations.iter().any(|bridge| matches!(
                    bridge,
                    PlanOperation::ResamplePcm { brick_wall: true, .. }
                ))
                && request.settings.dither_type != DitherType::None
                && !mapping::requires_sox_dither(request.settings.dither_type)
                && mapping::soxr_dither_method(request.settings.dither_type).is_some();
            // Preserve the retained SSRC split cell: after a Float64 SSRC
            // output, FFmpeg owns terminal dither families that it can realize
            // exactly. The command lowerer binds this already-selected tool
            // rather than re-ranking the terminal step a second time.
            let current_route = if ssrc_split_ffmpeg_terminal {
                Some(ToolIdentifier::Ffmpeg)
            } else {
                current_terminal_registered_tool(request, &operation, intent.gain_policy)
            };
            let selected_candidate = match selected_candidate_index(
                &candidates,
                current_route.as_ref(),
                &requirements,
                search_policy,
                false,
            ) {
                Ok(index) => index,
                Err(CandidateSelectionError::Refused(refusal)) => {
                    return Ok(PlanningOutcome::Refused(refusal));
                }
                Err(CandidateSelectionError::Resource(limit)) => return Err(limit),
            };
            if current_route.is_some()
                && candidates[selected_candidate].tool.as_ref() != current_route.as_ref()
                && capability == ExecutionCapability::ExecutableNow
            {
                capability = ExecutionCapability::RequiresPhase3CommonRealizer;
            }
            if intent.gain_policy.is_true_peak() {
                let selected_terminal_proof = candidates[selected_candidate]
                    .contract
                    .terminal_proof
                    .clone();
                for node in &mut nodes {
                    if let TypedPlanNode::Decide(Decision {
                        kind: DecisionKind::TruePeakGain {
                            album_participant: Some(participant),
                            ..
                        },
                        ..
                    }) = node
                    {
                        participant.terminal_proof = selected_terminal_proof.clone();
                    }
                }
            }
            let resolved_parameters = match resolved_terminal_operation_parameters(
                request,
                &operation,
                candidates[selected_candidate].tool.as_ref(),
                candidates[selected_candidate]
                    .contract
                    .terminal_realization
                    .as_ref(),
            ) {
                Ok(parameters) => parameters,
                Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
            };
            nodes.push(TypedPlanNode::Operation {
                operation: operation.clone(),
                input_signal: Some(working_signal),
                output_signal: Some(terminal),
                candidates,
                selected_candidate,
                resolved_parameters,
            });
            working_signal = terminal;
        }
    }

    let product = output_product_identity(request);
    let source_artifact = ArtifactId(0);
    let mut current_artifact = ArtifactId(1);
    let sample_changed = bridge_changes_samples
        || !intent.processing.is_empty()
        || intent.gain_policy.is_active()
        || intent.reference_delivery;
    let inherited_loudness = inherited_loudness_disposition(request, &intent, sample_changed);
    let output_signal_identity = (!product.is_lossy_or_hybrid() && !product.target_format.is_dsd())
        .then_some(working_signal);
    let mut artifacts = vec![
        ArtifactState {
            id: source_artifact,
            role: ArtifactRole::Source,
            signal: Some(source_signal),
            product: None,
            inherited_loudness: InheritedLoudnessDisposition::PreserveApplicable,
            metadata_effects: BTreeSet::new(),
            obligations: BTreeSet::new(),
        },
        ArtifactState {
            id: current_artifact,
            role: ArtifactRole::Output,
            signal: output_signal_identity,
            product: Some(product.clone()),
            inherited_loudness,
            metadata_effects: BTreeSet::new(),
            obligations: BTreeSet::from([RuntimeObligation::PublicationBarrier]),
        },
    ];
    nodes.push(TypedPlanNode::PackageOutput {
        input: working_signal,
        output: current_artifact,
        product: product.clone(),
    });

    let mut observation_signal = working_signal;
    if intent.replay_gain.is_some() && product.is_lossy_or_hybrid() {
        let delivered = SignalId(next_signal);
        next_signal += 1;
        states.push(AudioState {
            id: delivered,
            coding: SignalCoding::Pcm,
            sample_rate_hz: target_pcm_rate_fact(request),
            channels: source_channels_fact(request),
            channel_layout: source_channel_layout_fact(),
            frame_extent: FrameExtent::Pending("delivered_decode.frame_extent".to_owned()),
            programme: ProgrammeState::IndependentTrack,
            precision: StoragePrecision::Pending,
            storage_contract: Fact::Pending("delivered_decode.storage_encoding".to_owned()),
            processing_domain: Fact::Pending("delivered_decode.processing_domain".to_owned()),
            value_domain: ValueDomain::Pending("delivered_decode.value_domain".to_owned()),
            level_basis: LevelBasis::Ordinary,
            claims: BTreeSet::new(),
            obligations: BTreeSet::from([
                RuntimeObligation::CompleteReader(delivered),
                RuntimeObligation::IndependentDecode(current_artifact),
            ]),
        });
        nodes.push(TypedPlanNode::DecodeArtifactForObservation {
            input: current_artifact,
            output: delivered,
        });
        observation_signal = delivered;
    }

    if intent.store_source_audio_md5 {
        let observation = ObservationId(next_observation);
        next_observation += 1;
        nodes.push(TypedPlanNode::Observe(Observation {
            id: observation,
            scope: track_scope_id(request),
            participant: request.plan_scope.participant_id().clone(),
            subject: source_signal,
            artifact_subject: Some(source_artifact),
            kind: ObservationKind::SourceAudioMd5,
            complete_reader_required: true,
            read_contract: generic_complete_read_contract(
                "application:source_audio_md5/v1",
                true,
            ),
        }));
    }

    let mut required_metadata_effects = Vec::new();
    if intent.metadata.any() {
        required_metadata_effects.push(MetadataEffect::TransferSource(intent.metadata));
    }
    if intent.store_source_audio_md5 {
        required_metadata_effects.push(MetadataEffect::StoreSourceAudioMd5);
    }
    if matches!(inherited_loudness, InheritedLoudnessDisposition::DropInapplicable) {
        required_metadata_effects.push(MetadataEffect::ResolveInheritedLoudness);
    }

    if let Some(policy) = intent.replay_gain {
        let group = match replay_gain_binding(request, policy.mode) {
            Ok(group) => group,
            Err(refusal) => return Ok(PlanningOutcome::Refused(refusal)),
        };
        let observation_scope = match &group {
            ReplayGainGroupBinding::Track { scope }
            | ReplayGainGroupBinding::SubmittedBatch { scope, .. } => scope.clone(),
        };
        let observation = ObservationId(next_observation);
        next_observation += 1;
        nodes.push(TypedPlanNode::Observe(Observation {
            id: observation,
            scope: observation_scope.clone(),
            participant: request.plan_scope.participant_id().clone(),
            subject: observation_signal,
            artifact_subject: Some(current_artifact),
            kind: ObservationKind::ReplayGain {
                mode: policy.mode,
                profile: ReplayGainLoudnessProfile::NativeEbu2023,
                coverage: ReplayGainMetricCoverage::IntegratedOnly,
            },
            complete_reader_required: true,
            read_contract: generic_complete_read_contract(
                "application:ffmpeg_complete_decode+native_ebu_2023_integrated_only+reporting_peak/v1",
                true,
            ),
        }));
        let decision = DecisionId(next_decision);
        next_decision += 1;
        nodes.push(TypedPlanNode::Decide(Decision {
            id: decision,
            kind: DecisionKind::ReplayGainProjection { policy, group },
            observations: vec![ScopedObservationId {
                scope: observation_scope,
                participant: request.plan_scope.participant_id().clone(),
                observation,
                purpose: ObservationClass::Loudness,
            }],
        }));
        required_metadata_effects.push(MetadataEffect::ReplayGain);
    }

    let mut cumulative_metadata_effects = BTreeSet::new();
    for effect in required_metadata_effects {
        cumulative_metadata_effects.insert(effect);
        let next_artifact = ArtifactId(current_artifact.0 + 1);
        nodes.push(TypedPlanNode::MutateArtifact {
            input: current_artifact,
            output: next_artifact,
            effect,
        });
        artifacts.push(ArtifactState {
            id: next_artifact,
            role: ArtifactRole::Output,
            signal: output_signal_identity,
            product: Some(product.clone()),
            inherited_loudness,
            metadata_effects: cumulative_metadata_effects.clone(),
            obligations: BTreeSet::from([RuntimeObligation::PublicationBarrier]),
        });
        current_artifact = next_artifact;
    }

    if intent.verify_after_encode {
        let observation = ObservationId(next_observation);
        nodes.push(TypedPlanNode::Observe(Observation {
            id: observation,
            scope: track_scope_id(request),
            participant: request.plan_scope.participant_id().clone(),
            subject: observation_signal,
            artifact_subject: Some(current_artifact),
            kind: ObservationKind::TerminalVerification,
            complete_reader_required: true,
            read_contract: generic_complete_read_contract(
                "application:independent_terminal_decode/v1",
                true,
            ),
        }));
        nodes.push(TypedPlanNode::VerifyArtifact {
            artifact: current_artifact,
            observation,
        });
        if let Some(final_artifact) = artifacts.iter_mut().find(|artifact| artifact.id == current_artifact) {
            final_artifact
                .obligations
                .insert(RuntimeObligation::IndependentDecode(current_artifact));
        }
    }

    let bridges = if intent.reference_delivery {
        Vec::new()
    } else if capability == ExecutionCapability::ExecutableNow {
        vec![ExecutionBridge::ExistingCommandPlan]
    } else {
        Vec::new()
    };
    Ok(PlanningOutcome::Ready(TypedConversionPlan {
        intent,
        audio_states: states,
        nodes,
        artifacts,
        lowering_bridge_operations,
        execution_capability: capability,
        bridges,
    }))
}

/// Protect the legacy command-plan lowerer from typed routes owned by the
/// Phase-3 common realizer. Inspection callers should use [`plan_typed`]; the
/// application common realizer consumes these plans directly instead of
/// inventing a second legacy lowering or an unplanned fallback topology.
pub fn require_current_executor(plan: &TypedConversionPlan) -> Result<(), PlanningError> {
    match plan.execution_capability {
        ExecutionCapability::ExecutableNow => Ok(()),
        ExecutionCapability::ExecutableByPhase3CommonRealizer => Err(
            PlanningError::capability_unavailable(
                "phase3_common_realizer",
                "the typed plan is executable by the Phase-3 application common realizer and must not be lowered by the legacy command-plan executor",
            ),
        ),
        ExecutionCapability::RequiresPhase3DsdTrackTruePeak => Err(
            PlanningError::capability_unavailable(
                "dsd_track_true_peak_gain",
                "DSD Track true-peak guard/normalize is represented by the Phase-2 typed planner, but its certified executor route is connected in Phase 3; the request is not lowered to SoX norm",
            ),
        ),
        ExecutionCapability::RequiresPhase3CommonRealizer => Err(
            PlanningError::capability_unavailable(
                "common_realizer",
                "the typed plan is valid but requires the Phase-3 common realization path",
            ),
        ),
    }
}

fn current_terminal_registered_tool(
    request: &PlanRequest,
    operation: &PlanOperation,
    gain_policy: SampleGainPolicy,
) -> Option<ToolIdentifier> {
    if gain_policy.is_true_peak() && gain_policy.scope() == Some(TruePeakScope::Track)
        && request.source.is_dsd()
    {
        // DSD Track Guard/Normalize has no retained executor in Phase 2.  Do
        // not let the ordinary ungained terminal route masquerade as today's
        // implementation merely because the registry can encode the format.
        return None;
    }

    if gain_policy.is_true_peak() && gain_policy.scope() == Some(TruePeakScope::Album) {
        let mut bridged = request.clone();
        if request.source.is_dsd() {
            let expected = match &request.plan_scope {
                PlanScope::SubmittedBatch {
                    expected_participants: Some(expected),
                    ..
                } if *expected > 0 => usize::try_from(*expected).ok()?,
                _ => return None,
            };
            bridged
                .settings
                .dsd
                .bind_runtime_album_gain(DbNano::ZERO, None, expected);
        } else {
            bridged
                .settings
                .pcm_true_peak
                .bind_runtime_album_gain(DbNano::ZERO);
        }
        return current_registered_tool_for_operation(&bridged, operation);
    }

    current_registered_tool_for_operation(request, operation)
}

fn current_registered_tool_for_operation(
    request: &PlanRequest,
    operation: &PlanOperation,
) -> Option<ToolIdentifier> {
    let step = PlanStep::new(
        0,
        operation.clone(),
        InputSource::Path(request.input_path.clone()),
        OutputSink::Path(request.output_path.clone()),
        format!("semantic current route for {}", operation.label()),
    );
    ToolRegistry::with_builtin_tools()
        .selected_tool_id(&request.context(), &step)
        .ok()
}

fn candidate_requirements_for_state(
    state: &AudioState,
    terminal_proof_required: bool,
    complete_reader_required: bool,
    connected_executor_required: bool,
) -> CandidateRequirements {
    let processing = match &state.processing_domain {
        Fact::Known(value) => Some(value.clone()),
        Fact::Pending(_) | Fact::Unavailable(_) => match state.precision {
            StoragePrecision::Pcm(depth) if depth.is_float() =>
                Some(processing_domain_for_pcm_depth(depth)),
            StoragePrecision::Pcm(depth) => Some(ProcessingDomain::PcmInteger(depth)),
            StoragePrecision::OneBit => Some(ProcessingDomain::DsdOneBit),
            StoragePrecision::Encoded => Some(ProcessingDomain::Source),
            StoragePrecision::Pending => None,
        },
    };
    let value = match &state.value_domain {
        ValueDomain::Pending(_) => match state.precision {
            StoragePrecision::Pcm(depth) if depth.is_float() => Some(ValueDomain::FiniteFloating),
            StoragePrecision::Pcm(depth) => Some(ValueDomain::IntegerLattice(depth)),
            StoragePrecision::OneBit => Some(ValueDomain::OneBit),
            StoragePrecision::Encoded => Some(ValueDomain::Encoded),
            StoragePrecision::Pending => None,
        },
        value => Some(value.clone()),
    };
    CandidateRequirements {
        input_processing_domain: processing,
        input_value_domain: value,
        terminal_proof_required,
        terminal_realization_required: false,
        complete_reader_required,
        required_runtime_obligations: BTreeSet::new(),
        binary64_resample_preservation_required: false,
        connected_executor_required,
    }
}

fn protected_float64_riff_ingress_admits(
    authority: &crate::ssrc_binary64::ProtectedFloat64IngressAuthority,
    channels: &Fact<u16>,
    frame_extent: &FrameExtent,
) -> bool {
    let channels = match channels {
        Fact::Known(channels) if *channels > 0 => u128::from(*channels),
        Fact::Known(_) | Fact::Pending(_) | Fact::Unavailable(_) => return false,
    };
    let upper_frames = match frame_extent {
        FrameExtent::Exact(frames) => u128::from(*frames),
        FrameExtent::Bounded { upper_frames } => u128::from(*upper_frames),
        FrameExtent::EstimatedDurationNanos(_)
        | FrameExtent::Pending(_)
        | FrameExtent::Unavailable(_) => return false,
    };
    let Some(payload_bytes) = upper_frames
        .checked_mul(channels)
        .and_then(|value| {
            value.checked_mul(u128::from(
                crate::ssrc_binary64::PROTECTED_PCM_F64LE_BYTES_PER_SAMPLE,
            ))
        })
    else {
        return false;
    };
    let Some(predicted_physical_bytes) = payload_bytes.checked_add(u128::from(
        authority.muxer_structure_upper_bound_bytes,
    )) else {
        return false;
    };
    predicted_physical_bytes <= u128::from(authority.max_physical_bytes)
}

fn ssrc_binary64_evidence_scope(
    request: &PlanRequest,
    operation: &PlanOperation,
    source_rate_hz: u32,
) -> Option<crate::ssrc_binary64::Binary64ResampleEvidenceScope> {
    let PlanOperation::ResamplePcm {
        target_rate_hz,
        profile,
        ..
    } = operation
    else {
        return None;
    };
    let effective_profile = (*profile).unwrap_or_else(|| {
        mapping::ssrc_profile(request.settings.ssrc, request.settings.resample_quality)
    });
    Some(crate::ssrc_binary64::Binary64ResampleEvidenceScope {
        profile: effective_profile,
        source_rate_hz,
        target_rate_hz: *target_rate_hz,
        attenuation_db: Some(
            request
                .settings
                .ssrc
                .attenuation_db
                .map(crate::plugins::render_ssrc_attenuation_db)
                .unwrap_or_else(|| "0.0".to_owned()),
        ),
        min_phase: request.settings.ssrc.min_phase,
        architecture: std::env::consts::ARCH.to_owned(),
        input_container: "wav".to_owned(),
        input_sample_format: "pcm_f64le".to_owned(),
        output_container: "w64".to_owned(),
        output_sample_format: "pcm_f64le".to_owned(),
    })
}

fn registered_candidates_for_operation(
    request: &PlanRequest,
    operation: &PlanOperation,
    contract: TransformContract,
    resample_source_rate_hz: Option<u32>,
) -> Vec<PhysicalCandidate> {
    let step = PlanStep::new(
        0,
        operation.clone(),
        InputSource::Path(request.input_path.clone()),
        OutputSink::Path(request.output_path.clone()),
        format!("semantic candidate for {}", operation.label()),
    );
    ToolRegistry::with_builtin_tools()
        .supported_tool_ids(&request.context(), &step)
        .into_iter()
        .map(|tool| {
            let binary64_resample_preservation_evidence = match &tool {
                ToolIdentifier::Ssrc => resample_source_rate_hz
                    .and_then(|source_rate_hz| {
                        ssrc_binary64_evidence_scope(request, operation, source_rate_hz)
                    })
                    .map(|scope| crate::ssrc_binary64::production_evidence_for_scope(&scope)),
                ToolIdentifier::Ffmpeg if matches!(operation, PlanOperation::ResamplePcm { .. }) => {
                    Some(crate::ssrc_binary64::Binary64ResamplePreservationEvidence::EstablishedExistingAuthority {
                        authority_id: "tonepoet:ffmpeg-soxr-overload-preserving-resample/v1".to_owned(),
                    })
                }
                _ => None,
            };
            let mut candidate_contract = contract.clone();
            if tool == ToolIdentifier::Ssrc {
                if let PlanOperation::ResamplePcm {
                    target_bit_depth: Some(depth),
                    ..
                } = operation
                {
                    candidate_contract.representation.emitted_processing_domain =
                        Some(ordinary_ssrc_processing_domain(*depth));
                }
            }
            PhysicalCandidate {
                identity: format!("registered:{}:{}", operation.label(), tool),
                tool: Some(tool),
                contract: candidate_contract,
                executable: true,
                binary64_resample_preservation_evidence,
                protected_float64_ingress_authority: None,
            }
        })
        .collect()
}

#[cfg(test)]
fn apply_resampler_evidence_test_override(
    candidates: &mut [PhysicalCandidate],
    request: &PlanRequest,
    operation: &PlanOperation,
    source_rate_hz: u32,
    search_policy: &CandidateSearchPolicy,
) {
    let Some(override_record) = search_policy.ssrc_binary64_evidence_override.as_ref() else {
        return;
    };
    let Some(actual_scope) =
        ssrc_binary64_evidence_scope(request, operation, source_rate_hz)
    else {
        return;
    };
    if actual_scope != override_record.scope {
        return;
    }
    if let crate::ssrc_binary64::Binary64ResamplePreservationEvidence::Established { scope, .. } =
        &override_record.evidence
    {
        debug_assert_eq!(scope, &override_record.scope);
        if scope != &override_record.scope {
            return;
        }
    }
    for candidate in candidates {
        if candidate.tool == Some(ToolIdentifier::Ssrc) {
            candidate.binary64_resample_preservation_evidence =
                Some(override_record.evidence.clone());
        }
    }
}

#[cfg(test)]
fn apply_candidate_test_overrides(
    candidates: &mut [PhysicalCandidate],
    search_policy: &CandidateSearchPolicy,
) {
    let Some(tool) = search_policy.strip_terminal_proof_for_tool.as_ref() else {
        return;
    };
    for candidate in candidates {
        if candidate.tool.as_ref() == Some(tool) {
            candidate.contract.terminal_proof = None;
        }
    }
}

fn selected_candidate_index(
    candidates: &[PhysicalCandidate],
    current_route: Option<&ToolIdentifier>,
    requirements: &CandidateRequirements,
    search_policy: &CandidateSearchPolicy,
    operation_forces_ssrc: bool,
) -> Result<usize, CandidateSelectionError> {
    let forced_tool = search_policy
        .forced_tool
        .clone()
        .or_else(|| operation_forces_ssrc.then_some(ToolIdentifier::Ssrc));
    let selection_key = forced_tool.as_ref().or(current_route);
    let selected = select_candidate_bounded(
        candidates,
        selection_key,
        forced_tool.is_some(),
        requirements,
        search_policy.max_alternatives_to_inspect,
    )?;
    candidates
        .iter()
        .position(|candidate| std::ptr::eq(candidate, selected))
        .ok_or_else(|| CandidateSelectionError::Refused(PlanRefusal {
            code: "candidate_selection_internal".to_owned(),
            reason: "selected candidate was not present in its registered candidate set".to_owned(),
        }))
}

// Resolve dither owned by a standalone FFmpeg/SoXR resample operation.
// Final PCM terminal dither is resolved separately, once, from the selected
// terminal candidate and its actual input state.
fn resolve_ffmpeg_resample_effective_dither(
    request: &PlanRequest,
    target_depth: Option<PcmBitDepth>,
) -> Result<Option<DitherType>, PlanRefusal> {
    let effective_depth = target_depth.or_else(|| match request.settings.target_bit_depth {
        crate::enums::BitDepthTarget::Pcm(depth) => Some(depth),
        crate::enums::BitDepthTarget::Source => request.source.authoritative_pcm_depth(),
    });
    let needs_dither = match target_depth {
        Some(depth) => crate::plugins::pcm_conversion_reduces_depth(
            request.source.authoritative_pcm_depth(),
            depth,
        ),
        None => match request.settings.target_bit_depth {
            crate::enums::BitDepthTarget::Source => false,
            crate::enums::BitDepthTarget::Pcm(depth) => crate::plugins::pcm_conversion_reduces_depth(
                request.source.authoritative_pcm_depth(),
                depth,
            ),
        },
    };
    let explicit_int32 = crate::plugins::explicit_int32_dither_requested(
        &request.settings,
        effective_depth,
    );
    if request.settings.dither_type == DitherType::None || (!needs_dither && !explicit_int32) {
        return Ok(None);
    }
    if mapping::soxr_dither_method(request.settings.dither_type).is_none() {
        return Err(PlanRefusal {
            code: "soxr_parameter_resolution".to_owned(),
            reason: "selected dither has no FFmpeg/SoXR mapping for this active bit-depth reduction".to_owned(),
        });
    }
    Ok(Some(request.settings.dither_type))
}

fn terminal_input_reduces_precision(
    input_state: &AudioState,
    target_depth: PcmBitDepth,
) -> bool {
    match &input_state.value_domain {
        ValueDomain::IntegerLattice(source_depth) => target_depth.bits() < source_depth.bits(),
        ValueDomain::FiniteFloating | ValueDomain::Q1_31DerivedBinary64 => !target_depth.is_float(),
        ValueDomain::Pending(_) => match &input_state.precision {
            StoragePrecision::Pcm(source_depth) if source_depth.is_float() => !target_depth.is_float(),
            StoragePrecision::Pcm(source_depth) => target_depth.bits() < source_depth.bits(),
            StoragePrecision::OneBit | StoragePrecision::Encoded | StoragePrecision::Pending => {
                !target_depth.is_float()
            }
        },
        ValueDomain::Encoded | ValueDomain::OneBit => !target_depth.is_float(),
    }
}

fn resolve_selected_pcm_terminal_realization(
    request: &PlanRequest,
    operation: &PlanOperation,
    gain_policy: SampleGainPolicy,
    input_state: &AudioState,
    tool: &ToolIdentifier,
) -> Result<SelectedPcmTerminalRealization, PlanRefusal> {
    let PlanOperation::EncodePcm {
        target_format,
        target_rate_hz,
        target_bit_depth,
        apply_processing: _,
    } = operation
    else {
        return Err(PlanRefusal {
            code: "terminal_realization_not_pcm".to_owned(),
            reason: "selected PCM terminal realization was requested for a non-PCM operation".to_owned(),
        });
    };

    if !matches!(tool, ToolIdentifier::Sox | ToolIdentifier::Ffmpeg) {
        return Err(PlanRefusal {
            code: "terminal_realization_backend".to_owned(),
            reason: format!("{tool} has no registered selected-PCM-terminal realization"),
        });
    }

    let dither = request.settings.dither_type;
    let dither_requested = dither != DitherType::None;
    let reduces_precision = terminal_input_reduces_precision(input_state, *target_bit_depth);
    let automatic_low_depth_dither = dither_requested
        && crate::plugins::target_depth_needs_dither(*target_bit_depth)
        && reduces_precision;
    let explicit_int32_requested = crate::plugins::explicit_int32_dither_requested(
        &request.settings,
        Some(*target_bit_depth),
    );
    // FFmpeg lowering treats an explicit Int32 dither request as an explicit
    // physical aresample/dither_method realization even when the incoming PCM
    // is already Int32. Keep the selected-terminal truth aligned with that
    // command behavior; whether a particular same-lattice input changes bytes
    // is not a reason to erase the explicitly selected terminal realization.
    let explicit_int32 = explicit_int32_requested;
    let hard_ceiling = gain_policy.is_true_peak();
    let fixed_gain = matches!(gain_policy, SampleGainPolicy::FixedGain { .. });
    let wavpack_hybrid = *target_format == AudioFormat::WavPack
        && request.settings.wavpack.hybrid;
    let compound_wavpack = wavpack_hybrid && (hard_ceiling || fixed_gain);
    let compound_sox_preterminal = dither_requested
        && (mapping::requires_sox_dither(dither)
            || (hard_ceiling && automatic_low_depth_dither))
        && !target_format.sox_encodable()
        && target_format.ffmpeg_encodable();

    if wavpack_hybrid && !compound_wavpack {
        if tool != &ToolIdentifier::Ffmpeg {
            return Err(PlanRefusal {
                code: "terminal_realization_backend".to_owned(),
                reason: "native WavPack hybrid packaging is owned by the FFmpeg registry candidate".to_owned(),
            });
        }
        if automatic_low_depth_dither || explicit_int32 {
            return Err(PlanRefusal {
                code: "terminal_dither_not_realized".to_owned(),
                reason: "native WavPack hybrid packaging cannot satisfy the requested effective terminal dither without a SoX preterminal".to_owned(),
            });
        }
        return Ok(SelectedPcmTerminalRealization {
            kind: PcmTerminalRealizationKind::NativeWavPackHybridPackage,
            selected_tool: tool.clone(),
            input_precision: input_state.precision.clone(),
            input_value_domain: input_state.value_domain.clone(),
            target_format: target_format.clone(),
            target_rate_hz: *target_rate_hz,
            target_bit_depth: *target_bit_depth,
            wavpack_hybrid: true,
            effective_dither: None,
            ssrc_dither: None,
            dither_owner: PcmTerminalDitherOwner::None,
        });
    }

    if compound_wavpack || compound_sox_preterminal {
        if tool != &ToolIdentifier::Ffmpeg {
            return Err(PlanRefusal {
                code: "terminal_realization_backend".to_owned(),
                reason: "the admitted compound terminal is owned by the FFmpeg candidate with a SoX preterminal".to_owned(),
            });
        }
        if explicit_int32 {
            // SoX's ordinary Int32 path does not realize meaningful dither. A
            // compound route therefore cannot satisfy an explicit Int32 dither
            // request merely because the final package candidate is FFmpeg.
            return Err(PlanRefusal {
                code: "terminal_dither_not_realized".to_owned(),
                reason: "explicit Int32 dither cannot be assigned to an ordinary SoX preterminal".to_owned(),
            });
        }
        let effective_dither = automatic_low_depth_dither.then_some(dither);
        if dither_requested && mapping::requires_sox_dither(dither) && effective_dither.is_none() {
            return Err(PlanRefusal {
                code: "terminal_dither_not_realized".to_owned(),
                reason: "the selected SoX preterminal cannot realize the requested dither at this target depth".to_owned(),
            });
        }
        return Ok(SelectedPcmTerminalRealization {
            kind: if compound_wavpack {
                PcmTerminalRealizationKind::SoxPreterminalWavPackHybrid
            } else {
                PcmTerminalRealizationKind::SoxPreterminalFfmpegPackage
            },
            selected_tool: tool.clone(),
            input_precision: input_state.precision.clone(),
            input_value_domain: input_state.value_domain.clone(),
            target_format: target_format.clone(),
            target_rate_hz: *target_rate_hz,
            target_bit_depth: *target_bit_depth,
            wavpack_hybrid,
            effective_dither,
            ssrc_dither: None,
            dither_owner: effective_dither.map_or(
                PcmTerminalDitherOwner::None,
                |_| PcmTerminalDitherOwner::SoxPreterminal,
            ),
        });
    }

    let effective_dither = match tool {
        ToolIdentifier::Sox => {
            if explicit_int32 {
                return Err(PlanRefusal {
                    code: "terminal_dither_not_realized".to_owned(),
                    reason: "ordinary SoX Int32 output cannot satisfy an explicit Int32 dither request".to_owned(),
                });
            }
            automatic_low_depth_dither.then_some(dither)
        }
        ToolIdentifier::Ffmpeg => {
            let applies = automatic_low_depth_dither || explicit_int32;
            if applies && mapping::soxr_dither_method(dither).is_none() {
                return Err(PlanRefusal {
                    code: "terminal_dither_parameter_resolution".to_owned(),
                    reason: "selected dither has no FFmpeg/SoXR mapping for the admitted terminal route".to_owned(),
                });
            }
            applies.then_some(dither)
        }
        _ => unreachable!("non-PCM terminal backend rejected above"),
    };

    Ok(SelectedPcmTerminalRealization {
        kind: match tool {
            ToolIdentifier::Sox => PcmTerminalRealizationKind::SoxDirect,
            ToolIdentifier::Ffmpeg => PcmTerminalRealizationKind::FfmpegDirect,
            _ => unreachable!("non-PCM terminal backend rejected above"),
        },
        selected_tool: tool.clone(),
        input_precision: input_state.precision.clone(),
        input_value_domain: input_state.value_domain.clone(),
        target_format: target_format.clone(),
        target_rate_hz: *target_rate_hz,
        target_bit_depth: *target_bit_depth,
        wavpack_hybrid,
        effective_dither,
        ssrc_dither: None,
        dither_owner: effective_dither.map_or(
            PcmTerminalDitherOwner::None,
            |_| PcmTerminalDitherOwner::SelectedTerminal,
        ),
    })
}

fn resolve_selected_terminal_realization(
    request: &PlanRequest,
    operation: &PlanOperation,
    gain_policy: SampleGainPolicy,
    input_state: &AudioState,
    tool: Option<&ToolIdentifier>,
) -> Result<SelectedTerminalRealization, PlanRefusal> {
    match (operation, tool) {
        (PlanOperation::EncodePcm { .. }, Some(tool)) => {
            resolve_selected_pcm_terminal_realization(
                request,
                operation,
                gain_policy,
                input_state,
                tool,
            )
            .map(SelectedTerminalRealization::Pcm)
        }
        (
            PlanOperation::EncodeLossy {
                target_format,
                target_rate_hz,
                apply_processing,
            },
            Some(ToolIdentifier::Ffmpeg),
        ) => Ok(SelectedTerminalRealization::LossyFfmpegEncoderInput {
            target_format: target_format.clone(),
            target_rate_hz: *target_rate_hz,
            apply_processing: *apply_processing,
        }),
        _ => Err(PlanRefusal {
            code: "terminal_realization_backend".to_owned(),
            reason: "candidate has no structured terminal realization for this operation".to_owned(),
        }),
    }
}

fn normalized_active_dsd_to_pcm_settings(
    mut settings: DsdToPcmSettings,
) -> DsdToPcmSettings {
    // Reconstruction authority, export level and ordinary gain have dedicated
    // semantic nodes/intent fields.  This payload describes only the selected
    // reconstruction lowerer.
    settings.reconstruction = DsdGeneralReconstruction::General;
    settings.export_level = DsdGeneralExportLevel::Native;
    settings.gain = SampleGainPolicy::Off;
    if settings.lowpass != DsdLowpassMethod::Sinc {
        settings.sinc = Default::default();
    }
    settings
}

fn effective_dsd_to_pcm_sinc(
    settings: DsdToPcmSettings,
) -> Option<DsdToPcmSincSettings> {
    (settings.lowpass == DsdLowpassMethod::Sinc).then(|| {
        let mut sinc = settings.sinc;
        sinc.passband_hz = crate::plugins::canonicalize_sox_sinc_passband_hz(sinc.passband_hz);
        sinc.transition_hz = crate::plugins::canonicalize_sox_scalar(sinc.transition_hz);
        sinc.kaiser_beta = crate::plugins::canonicalize_sox_scalar(sinc.kaiser_beta);
        sinc
    })
}

fn effective_pcm_to_dsd_sinc(
    settings: PcmToDsdSettings,
) -> Option<PcmToDsdSincSettings> {
    (settings.filter == DsdFilterPreset::Sinc).then(|| {
        let mut sinc = settings.sinc;
        sinc.passband_hz = crate::plugins::canonicalize_sox_sinc_passband_hz(sinc.passband_hz);
        sinc.transition_hz = crate::plugins::canonicalize_sox_scalar(sinc.transition_hz);
        sinc.kaiser_beta = crate::plugins::canonicalize_sox_scalar(sinc.kaiser_beta);
        sinc
    })
}

fn effective_pcm_to_dsd_gain_compensation(settings: PcmToDsdSettings) -> GainCompensation {
    if settings.filter != DsdFilterPreset::Sinc {
        return GainCompensation::Auto;
    }
    match settings.gain_compensation {
        GainCompensation::Linear(value) => {
            GainCompensation::Linear(crate::plugins::canonicalize_sox_scalar(value))
        }
        GainCompensation::Decibels(value) => {
            GainCompensation::Decibels(crate::plugins::canonicalize_sox_gain_db(value))
        }
        other => other,
    }
}

fn reject_unsupported_dsd_to_pcm_aliasing(settings: DsdToPcmSettings) -> Result<(), PlanRefusal> {
    if settings.lowpass == DsdLowpassMethod::Sinc && settings.sinc.allow_aliasing {
        return Err(PlanRefusal {
            code: "dsd_to_pcm_sinc_aliasing_unsupported".to_owned(),
            reason: "general DSD-to-PCM sinc alias permission is not supported by the retained SoX lowerer".to_owned(),
        });
    }
    Ok(())
}

fn normalized_active_pcm_to_dsd_settings(
    mut settings: PcmToDsdSettings,
) -> PcmToDsdSettings {
    if settings.filter != DsdFilterPreset::Sinc {
        // Sinc controls and gain compensation do not reach the Auto route.
        settings.sinc = Default::default();
        settings.gain_compensation = GainCompensation::Auto;
    }
    settings
}

fn normalized_active_flac_settings(
    mut settings: FlacSettings,
    write_md5_is_lowered: bool,
) -> FlacSettings {
    // Verification is a separate semantic observation/barrier.
    settings.verify = false;
    if !write_md5_is_lowered {
        // The retained SoX and qualified Reference FLAC paths do not expose
        // the FFmpeg `-flags -md5` toggle. Keep that dormant request out of
        // resolved semantic identity when the selected lowerer ignores it.
        settings.write_md5 = FlacSettings::default().write_md5;
    }
    settings
}

fn normalized_active_mp3_settings(mut settings: Mp3Settings) -> Mp3Settings {
    match settings.mode {
        Mp3Mode::Vbr => settings.bitrate_kbps = Mp3Settings::default().bitrate_kbps,
        Mp3Mode::Cbr | Mp3Mode::Abr => {
            settings.vbr_quality = Mp3Settings::default().vbr_quality;
        }
    }
    settings
}

fn normalized_active_wavpack_settings(mut settings: WavPackSettings) -> WavPackSettings {
    if !settings.hybrid {
        let defaults = WavPackSettings::default();
        settings.hybrid_bitrate_kbps = defaults.hybrid_bitrate_kbps;
        settings.correction_file = defaults.correction_file;
    }
    settings
}

fn ssrc_computation_precision(profile: SsrcProfile) -> SsrcComputationPrecision {
    match profile {
        SsrcProfile::Insane | SsrcProfile::High | SsrcProfile::Long => {
            SsrcComputationPrecision::Double
        }
        SsrcProfile::Standard
        | SsrcProfile::Short
        | SsrcProfile::Fast
        | SsrcProfile::Lightning => SsrcComputationPrecision::Single,
    }
}

fn ordinary_ssrc_processing_domain(depth: PcmBitDepth) -> ProcessingDomain {
    if depth.is_float() {
        ProcessingDomain::PcmFloating
    } else {
        ProcessingDomain::PcmInteger(depth)
    }
}

fn resolved_operation_parameters(
    request: &PlanRequest,
    operation: &PlanOperation,
    selected_tool: Option<&ToolIdentifier>,
    ssrc_output_role: Option<SsrcOutputRole>,
    ssrc_emitted_processing_domain: Option<ProcessingDomain>,
) -> Result<ResolvedOperationParameters, PlanRefusal> {
    match operation {
        PlanOperation::ResamplePcm { target_rate_hz, target_bit_depth, profile, .. } => match selected_tool {
            Some(ToolIdentifier::Ssrc) => {
                let effective_depth = (*target_bit_depth).or_else(|| match request.settings.target_bit_depth {
                    BitDepthTarget::Pcm(depth) => Some(depth),
                    BitDepthTarget::Source => request.source.authoritative_pcm_depth(),
                });
                let resolved_dither = crate::plugins::resolve_ssrc_dither_for_rate(
                    &request.settings,
                    request.source.authoritative_pcm_depth(),
                    effective_depth,
                    *target_rate_hz,
                )
                .map_err(|error| PlanRefusal {
                    code: "ssrc_parameter_resolution".to_owned(),
                    reason: error.to_string(),
                })?;
                let effective_profile = (*profile).unwrap_or_else(|| {
                    mapping::ssrc_profile(
                        request.settings.ssrc,
                        request.settings.resample_quality,
                    )
                });
                let effective_output_depth = effective_depth.unwrap_or(PcmBitDepth::Float64);
                let output_role = ssrc_output_role.unwrap_or(SsrcOutputRole::Nonterminal);
                Ok(ResolvedOperationParameters::ResampleSsrc {
                    requested: request.settings.ssrc,
                    effective_profile,
                    effective_attenuation_db: request
                        .settings
                        .ssrc
                        .attenuation_db
                        .map(crate::plugins::canonicalize_ssrc_attenuation_db),
                    effective_output_depth,
                    output_role,
                    computation_precision: ssrc_computation_precision(effective_profile),
                    emitted_processing_domain: ssrc_emitted_processing_domain
                        .unwrap_or_else(|| ordinary_ssrc_processing_domain(effective_output_depth)),
                    effective_dither: resolved_dither,
                    authority_reason: if request.settings.ssrc.force {
                        SsrcAuthorityReason::ExplicitForce
                    } else {
                        SsrcAuthorityReason::CapabilitySelected
                    },
                })
            }
            Some(ToolIdentifier::Ffmpeg) => {
                let resolved = crate::plugins::resolve_soxr_rate_options(&request.settings);
                Ok(ResolvedOperationParameters::ResampleSoxr {
                    requested: request.settings.soxr_resampler,
                    effective_precision: resolved.precision,
                    effective_cutoff: resolved.cutoff,
                    effective_phase: resolved.phase,
                    effective_dither: resolve_ffmpeg_resample_effective_dither(request, *target_bit_depth)?,
                })
            }
            Some(ToolIdentifier::Sox) | None => {
                let effective_depth = (*target_bit_depth).or_else(|| match request.settings.target_bit_depth {
                    BitDepthTarget::Pcm(depth) => Some(depth),
                    BitDepthTarget::Source => request.source.authoritative_pcm_depth(),
                });
                let effective_dither = effective_depth.and_then(|depth| {
                    (request.settings.dither_type != DitherType::None
                        && crate::plugins::pcm_conversion_reduces_depth(
                            request.source.authoritative_pcm_depth(),
                            depth,
                        ))
                    .then_some(request.settings.dither_type)
                });
                let effective_bandwidth_pct = if request.settings.sox_resampler.chebyshev {
                    None
                } else {
                    request.settings.sox_resampler.bandwidth_pct.or_else(|| {
                        mapping::sox_bandwidth_percent(request.settings.nyquist_transition)
                            .and_then(|value| value.parse::<f32>().ok())
                    })
                };
                Ok(ResolvedOperationParameters::ResampleSox {
                    requested: request.settings.sox_resampler,
                    quality: request.settings.resample_quality,
                    effective_bandwidth_pct,
                    effective_sinc_passband_hz: request
                        .settings
                        .sox_resampler
                        .sinc_passband_hz
                        .map(crate::plugins::canonicalize_sox_sinc_passband_hz),
                    effective_dither,
                })
            },
            Some(_) => Ok(ResolvedOperationParameters::None),
        },
        PlanOperation::DsdToPcm { .. } => {
            if request.settings.dsd.reference_delivery_selected() {
                Ok(ResolvedOperationParameters::None)
            } else {
                let requested = normalized_active_dsd_to_pcm_settings(
                    request.settings.dsd.general_from_dsd,
                );
                reject_unsupported_dsd_to_pcm_aliasing(requested)?;
                let effective_dither = match operation {
                    PlanOperation::DsdToPcm { target_bit_depth, .. }
                        if crate::plugins::target_depth_needs_dither(*target_bit_depth)
                            && request.settings.dither_type != DitherType::None =>
                    {
                        Some(request.settings.dither_type)
                    }
                    _ => None,
                };
                Ok(ResolvedOperationParameters::DsdToPcm {
                    requested,
                    effective_sinc: effective_dsd_to_pcm_sinc(requested),
                    effective_dither,
                })
            }
        }
        PlanOperation::PcmToDsd { .. } => {
            let requested = normalized_active_pcm_to_dsd_settings(
                request.settings.dsd.pcm_to_dsd,
            );
            Ok(ResolvedOperationParameters::PcmToDsd {
                requested,
                effective_sinc: effective_pcm_to_dsd_sinc(requested),
                effective_gain_compensation: effective_pcm_to_dsd_gain_compensation(requested),
            })
        }
        PlanOperation::DsdRateChange { .. } => {
            let from_dsd = normalized_active_dsd_to_pcm_settings(
                request.settings.dsd.general_from_dsd,
            );
            reject_unsupported_dsd_to_pcm_aliasing(from_dsd)?;
            let to_dsd = normalized_active_pcm_to_dsd_settings(
                request.settings.dsd.pcm_to_dsd,
            );
            Ok(ResolvedOperationParameters::DsdRateChange {
                from_dsd,
                effective_from_sinc: effective_dsd_to_pcm_sinc(from_dsd),
                to_dsd,
            })
        }
        PlanOperation::EncodePcm {
            target_format,
            ..
        } => {
            // The selected terminal candidate has not been bound at this generic
            // operation-resolution layer. `resolved_terminal_operation_parameters`
            // fills this from the canonical selected-terminal realization.
            let effective_dither = None;
            Ok(match target_format {
                AudioFormat::Flac => ResolvedOperationParameters::EncodeFlac {
                    requested: normalized_active_flac_settings(
                        request.settings.flac,
                        selected_tool == Some(&ToolIdentifier::Ffmpeg),
                    ),
                    effective_dither,
                },
                AudioFormat::WavPack => ResolvedOperationParameters::EncodeWavPack {
                    requested: normalized_active_wavpack_settings(request.settings.wavpack),
                    effective_dither,
                },
                _ => ResolvedOperationParameters::EncodePcm { effective_dither },
            })
        },
        PlanOperation::EncodeLossy { target_format, .. } => Ok(match target_format {
            AudioFormat::Mp3 => ResolvedOperationParameters::EncodeMp3 {
                requested: normalized_active_mp3_settings(request.settings.mp3),
            },
            AudioFormat::Aac => ResolvedOperationParameters::EncodeAac { requested: request.settings.aac },
            AudioFormat::Opus => ResolvedOperationParameters::EncodeOpus { requested: request.settings.opus },
            _ => ResolvedOperationParameters::None,
        }),
        _ => Ok(ResolvedOperationParameters::None),
    }
}

/// Candidate selection that distinguishes preference from a forced backend.
/// A preferred candidate without required proof never blocks a later admitted
/// candidate; a forced candidate refuses rather than silently falling back.
pub fn select_candidate<'a>(
    candidates: &'a [PhysicalCandidate],
    preferred: Option<&ToolIdentifier>,
    forced: bool,
) -> Result<&'a PhysicalCandidate, PlanRefusal> {
    select_candidate_for_requirements(
        candidates,
        preferred,
        forced,
        &CandidateRequirements::default(),
    )
}

/// Candidate selection with explicit proof/representation premises.
/// Preference is evaluated only after semantic admission. A preferred backend
/// missing a required proof therefore cannot hide an admitted alternative.
fn candidate_mismatch_refusal(
    candidate_identity: Option<&str>,
    mismatch: &str,
    forced: bool,
) -> PlanRefusal {
    let (code, diagnostic) = if mismatch == "protected_float64_ingress_unavailable" {
        ("protected_float64_ingress_unavailable", mismatch.to_owned())
    } else if mismatch.starts_with("binary64 preservation evidence pending:") {
        ("binary64_resample_preservation_pending", mismatch.to_owned())
    } else if mismatch.starts_with("binary64 preservation refuted for this cell:") {
        ("binary64_resample_preservation_refuted", mismatch.to_owned())
    } else {
        (
            if forced { "forced_candidate_unavailable" } else { "no_admitted_candidate" },
            mismatch.to_owned(),
        )
    };
    let reason = match candidate_identity {
        Some(identity) if forced => format!("forced candidate {identity} is not admitted: {diagnostic}"),
        Some(identity) => format!("candidate {identity} is not admitted: {diagnostic}"),
        None => diagnostic,
    };
    PlanRefusal { code: code.to_owned(), reason }
}

pub fn select_candidate_for_requirements<'a>(
    candidates: &'a [PhysicalCandidate],
    preferred: Option<&ToolIdentifier>,
    forced: bool,
    requirements: &CandidateRequirements,
) -> Result<&'a PhysicalCandidate, PlanRefusal> {
    if let Some(preferred) = preferred {
        if let Some(candidate) = candidates.iter().find(|candidate| candidate.tool.as_ref() == Some(preferred)) {
            if candidate_contract_mismatch(candidate, requirements).is_none() {
                return Ok(candidate);
            }
            if forced {
                let missing = candidate_contract_mismatch(candidate, requirements)
                    .unwrap_or_else(|| "registered execution capability".to_owned());
                return Err(candidate_mismatch_refusal(
                    Some(&candidate.identity),
                    &missing,
                    true,
                ));
            }
        } else if forced {
            return Err(PlanRefusal {
                code: "forced_candidate_missing".to_owned(),
                reason: format!("forced tool {preferred} has no candidate for this transform"),
            });
        }
    }
    if let Some(candidate) = candidates
        .iter()
        .find(|candidate| candidate_contract_mismatch(candidate, requirements).is_none())
    {
        return Ok(candidate);
    }
    if let Some((candidate, mismatch)) = candidates.iter().find_map(|candidate| {
        candidate_contract_mismatch(candidate, requirements).and_then(|mismatch| {
            (mismatch == "protected_float64_ingress_unavailable"
                || mismatch.starts_with("binary64 preservation evidence pending:")
                || mismatch.starts_with("binary64 preservation refuted for this cell:"))
            .then_some((candidate, mismatch))
        })
    }) {
        return Err(candidate_mismatch_refusal(
            Some(&candidate.identity),
            &mismatch,
            false,
        ));
    }
    Err(PlanRefusal {
        code: "no_admitted_candidate".to_owned(),
        reason: "no candidate has every required representation, reader, terminal-proof, and executable-lowering contract".to_owned(),
    })
}

/// Bounded candidate selection with a fast check for the current/preferred
/// realization.  A valid current route is returned before alternative-search
/// accounting; otherwise a reached bound is a resource result, never a false
/// `no_admitted_candidate` conclusion.
pub fn select_candidate_bounded<'a>(
    candidates: &'a [PhysicalCandidate],
    current_or_preferred: Option<&ToolIdentifier>,
    forced: bool,
    requirements: &CandidateRequirements,
    max_alternatives_to_inspect: usize,
) -> Result<&'a PhysicalCandidate, CandidateSelectionError> {
    if let Some(preferred) = current_or_preferred {
        if let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.tool.as_ref() == Some(preferred))
        {
            if candidate_contract_mismatch(candidate, requirements).is_none() {
                return Ok(candidate);
            }
            if forced {
                return select_candidate_for_requirements(
                    candidates,
                    current_or_preferred,
                    true,
                    requirements,
                )
                .map_err(CandidateSelectionError::Refused);
            }
        } else if forced {
            return select_candidate_for_requirements(
                candidates,
                current_or_preferred,
                true,
                requirements,
            )
            .map_err(CandidateSelectionError::Refused);
        }
    }

    let alternatives: Vec<&PhysicalCandidate> = candidates
        .iter()
        .filter(|candidate| {
            current_or_preferred
                .map(|preferred| candidate.tool.as_ref() != Some(preferred))
                .unwrap_or(true)
        })
        .collect();
    let limit = max_alternatives_to_inspect;
    if let Some(candidate) = alternatives
        .iter()
        .take(limit)
        .copied()
        .find(|candidate| candidate_contract_mismatch(candidate, requirements).is_none())
    {
        return Ok(candidate);
    }
    if alternatives.len() > limit {
        return Err(CandidateSelectionError::Resource(PlanningResourceLimit {
            resource: "physical_candidate_search".to_owned(),
            requested: alternatives.len() as u64,
            limit: limit as u64,
        }));
    }

    if let Some((candidate, mismatch)) = candidates.iter().find_map(|candidate| {
        candidate_contract_mismatch(candidate, requirements).and_then(|mismatch| {
            (mismatch == "protected_float64_ingress_unavailable"
                || mismatch.starts_with("binary64 preservation evidence pending:")
                || mismatch.starts_with("binary64 preservation refuted for this cell:"))
            .then_some((candidate, mismatch))
        })
    }) {
        return Err(CandidateSelectionError::Refused(candidate_mismatch_refusal(
            Some(&candidate.identity),
            &mismatch,
            forced,
        )));
    }
    Err(CandidateSelectionError::Refused(PlanRefusal {
        code: "no_admitted_candidate".to_owned(),
        reason: "no candidate has every required representation, reader, terminal-proof, and executable-lowering contract".to_owned(),
    }))
}

fn candidate_contract_mismatch(
    candidate: &PhysicalCandidate,
    requirements: &CandidateRequirements,
) -> Option<String> {
    if requirements.connected_executor_required && !candidate.executable {
        return Some("no connected executable lowering".to_owned());
    }
    if let Some(required) = &requirements.input_processing_domain {
        if !candidate
            .contract
            .representation
            .accepted_processing_domains
            .contains(required)
        {
            return Some(format!("missing processing-domain contract for {required:?}"));
        }
    }
    if let Some(required) = &requirements.input_value_domain {
        if !candidate
            .contract
            .representation
            .accepted_value_domains
            .contains(required)
        {
            return Some(format!("missing value-domain contract for {required:?}"));
        }
    }
    if requirements.complete_reader_required
        && !candidate
            .contract
            .runtime_obligations
            .contains("complete_reader")
    {
        return Some("missing complete-reader contract".to_owned());
    }
    for required in &requirements.required_runtime_obligations {
        if !candidate.contract.runtime_obligations.contains(required) {
            return Some(format!("missing required route contract {required}"));
        }
    }
    if requirements.binary64_resample_preservation_required {
        match candidate.binary64_resample_preservation_evidence.as_ref() {
            Some(crate::ssrc_binary64::Binary64ResamplePreservationEvidence::EstablishedExistingAuthority { .. }) => {}
            Some(crate::ssrc_binary64::Binary64ResamplePreservationEvidence::Established { .. }) => {
                if candidate.protected_float64_ingress_authority.is_none() {
                    return Some("protected_float64_ingress_unavailable".to_owned());
                }
            }
            Some(crate::ssrc_binary64::Binary64ResamplePreservationEvidence::PendingEvidence { reason }) => {
                return Some(format!("binary64 preservation evidence pending: {reason:?}"));
            }
            Some(crate::ssrc_binary64::Binary64ResamplePreservationEvidence::Refuted { reason }) => {
                return Some(format!("binary64 preservation refuted for this cell: {reason:?}"));
            }
            None => return Some("missing commissioned Binary64 resampler-preservation evidence".to_owned()),
        }
    }
    if requirements.terminal_realization_required
        && candidate.contract.terminal_realization.is_none()
    {
        return Some("missing selected-terminal realization contract".to_owned());
    }
    if requirements.terminal_proof_required {
        let Some(proof) = candidate.contract.terminal_proof.as_ref() else {
            return Some("missing terminal/error proof contract".to_owned());
        };
        if let Some(required) = &requirements.input_value_domain {
            if !proof.accepted_value_domains.contains(required) {
                return Some(format!(
                    "terminal proof {} does not cover input value domain {required:?}",
                    proof.authority
                ));
            }
        }
    }
    None
}

/// Build a strict explicit sequence when a caller already has user-visible order.
/// Each generated instance depends on its immediate predecessor, making the
/// sequence unique without relying on enum/tool grouping.
#[must_use]
pub fn explicit_effect_sequence(
    effects: impl IntoIterator<Item = RegisteredUnaryEffect>,
) -> Vec<EffectIntent> {
    let mut previous = None;
    effects
        .into_iter()
        .enumerate()
        .map(|(index, effect)| {
            let id = EffectInstanceId(index as u32);
            let after = previous.into_iter().collect();
            previous = Some(id);
            EffectIntent { id, effect, after, placement: EffectPlacement::AfterPcmResample }
        })
        .collect()
}

/// Resolve effect constraints to one deterministic order.
///
/// Multiple simultaneously ready effects are deliberately refused: their
/// relative order is semantically ambiguous and may be noncommutative. This
/// also detects cycles, duplicate identities, and references to absent nodes.
fn validate_effect_dependencies(
    effects: &[EffectIntent],
) -> Result<BTreeMap<EffectInstanceId, &EffectIntent>, PlanRefusal> {
    let mut by_id = BTreeMap::new();
    for effect in effects {
        if by_id.insert(effect.id, effect).is_some() {
            return Err(PlanRefusal {
                code: "duplicate_effect_id".to_owned(),
                reason: format!("effect instance {} is declared more than once", effect.id.0),
            });
        }
    }
    for effect in effects {
        for dependency in &effect.after {
            if *dependency == effect.id {
                return Err(PlanRefusal {
                    code: "effect_order_cycle".to_owned(),
                    reason: format!("effect instance {} depends on itself", effect.id.0),
                });
            }
            let Some(predecessor) = by_id.get(dependency) else {
                return Err(PlanRefusal {
                    code: "unknown_effect_dependency".to_owned(),
                    reason: format!(
                        "effect instance {} depends on missing instance {}",
                        effect.id.0, dependency.0,
                    ),
                });
            };
            if effect.placement == EffectPlacement::BeforePcmResample
                && predecessor.placement == EffectPlacement::AfterPcmResample
            {
                return Err(PlanRefusal {
                    code: "effect_dependency_crosses_resampler_backwards".to_owned(),
                    reason: format!(
                        "pre-resample effect instance {} cannot depend on post-resample instance {}",
                        effect.id.0, dependency.0,
                    ),
                });
            }
        }
    }
    Ok(by_id)
}

fn strict_order_effect_partition(
    effects: &[EffectIntent],
    placement: EffectPlacement,
    by_id: &BTreeMap<EffectInstanceId, &EffectIntent>,
) -> Result<Vec<EffectIntent>, PlanRefusal> {
    let members = effects
        .iter()
        .filter(|effect| effect.placement == placement)
        .map(|effect| effect.id)
        .collect::<BTreeSet<_>>();
    let mut emitted = BTreeSet::new();
    let mut ordered = Vec::with_capacity(members.len());
    while ordered.len() < members.len() {
        let ready = members
            .iter()
            .filter(|id| !emitted.contains(*id))
            .filter(|id| {
                let effect = by_id.get(id).expect("partition effect must exist");
                effect.after.iter().all(|dependency| {
                    // A post-resample dependency on a pre-resample effect is
                    // satisfied by crossing the resampler barrier. Dependencies
                    // inside this partition still participate in strict order.
                    !members.contains(dependency) || emitted.contains(dependency)
                })
            })
            .copied()
            .collect::<Vec<_>>();
        match ready.as_slice() {
            [] => {
                return Err(PlanRefusal {
                    code: "effect_order_cycle".to_owned(),
                    reason: "registered effect ordering constraints contain a cycle".to_owned(),
                });
            }
            [id] => {
                emitted.insert(*id);
                ordered.push((**by_id.get(id).expect("ready effect must exist")).clone());
            }
            _ => {
                return Err(PlanRefusal {
                    code: "ambiguous_effect_order".to_owned(),
                    reason: format!(
                        "effect instances {:?} are simultaneously unordered within {:?}; declare their relative order explicitly",
                        ready.iter().map(|id| id.0).collect::<Vec<_>>(),
                        placement,
                    ),
                });
            }
        }
    }
    Ok(ordered)
}

fn order_registered_effect_partitions(
    effects: &[EffectIntent],
) -> Result<(Vec<EffectIntent>, Vec<EffectIntent>), PlanRefusal> {
    let by_id = validate_effect_dependencies(effects)?;
    let before = strict_order_effect_partition(
        effects,
        EffectPlacement::BeforePcmResample,
        &by_id,
    )?;
    let after = strict_order_effect_partition(
        effects,
        EffectPlacement::AfterPcmResample,
        &by_id,
    )?;
    Ok((before, after))
}

/// Resolve effect constraints to one deterministic semantic order.
///
/// Placement establishes the resampler barrier first. Strict ambiguity checks
/// are then applied independently on each side of that barrier.
pub fn order_registered_effects(effects: &[EffectIntent]) -> Result<Vec<EffectIntent>, PlanRefusal> {
    let (mut before, after) = order_registered_effect_partitions(effects)?;
    before.extend(after);
    Ok(before)
}

fn effect_contract_preserves_protected_true_peak_input(contract: &TransformContract) -> bool {
    contract
        .runtime_obligations
        .contains("preserve_floating_overload_before_true_peak")
        && contract
            .representation
            .accepted_processing_domains
            .contains(&ProcessingDomain::Binary64)
        && contract.representation.emitted_processing_domain
            == Some(ProcessingDomain::Binary64)
        && contract
            .representation
            .accepted_value_domains
            .contains(&ValueDomain::FiniteFloating)
        && contract.representation.emitted_value_domain == Some(ValueDomain::FiniteFloating)
}

/// Validate and lower one registered effect to its actual tool argument surface.
pub fn lower_registered_effect(effect: &RegisteredUnaryEffect) -> Result<EffectLowering, PlanRefusal> {
    let plain = TransformContract::plain_transform();
    match effect {
        RegisteredUnaryEffect::SoxHighPass { frequency_hz } => {
            validate_effect_frequency(*frequency_hz)?;
            Ok(EffectLowering {
                tool: ToolIdentifier::Sox,
                arguments: EffectArgumentMapping::SoxEffect(vec![
                    "highpass".to_owned(),
                    frequency_hz.to_string(),
                ]),
                contract: plain,
            })
        }
        RegisteredUnaryEffect::SoxLowPass { frequency_hz } => {
            validate_effect_frequency(*frequency_hz)?;
            Ok(EffectLowering {
                tool: ToolIdentifier::Sox,
                arguments: EffectArgumentMapping::SoxEffect(vec![
                    "lowpass".to_owned(),
                    frequency_hz.to_string(),
                ]),
                contract: plain,
            })
        }
        RegisteredUnaryEffect::SoxSamplePeakNormalize { target_dbfs } => {
            if !(DbNano::MIN_NORMALIZE_TARGET..=DbNano::MAX_NORMALIZE_TARGET).contains(target_dbfs) {
                return Err(PlanRefusal {
                    code: "invalid_sample_peak_target".to_owned(),
                    reason: "sample-peak normalize target must be between -12.0 and 0.0 dBFS".to_owned(),
                });
            }
            Ok(EffectLowering {
                tool: ToolIdentifier::Sox,
                arguments: EffectArgumentMapping::SoxEffect(vec![
                    "norm".to_owned(),
                    target_dbfs.render(false),
                ]),
                contract: plain,
            })
        }
        RegisteredUnaryEffect::FfmpegHighPass { frequency_hz } => {
            validate_effect_frequency(*frequency_hz)?;
            Ok(EffectLowering {
                tool: ToolIdentifier::Ffmpeg,
                arguments: EffectArgumentMapping::FfmpegAudioFilter(format!("highpass=f={frequency_hz}")),
                contract: plain,
            })
        }
        RegisteredUnaryEffect::FfmpegLowPass { frequency_hz } => {
            validate_effect_frequency(*frequency_hz)?;
            Ok(EffectLowering {
                tool: ToolIdentifier::Ffmpeg,
                arguments: EffectArgumentMapping::FfmpegAudioFilter(format!("lowpass=f={frequency_hz}")),
                contract: plain,
            })
        }
    }
}

fn validate_effect_frequency(frequency_hz: u32) -> Result<(), PlanRefusal> {
    if frequency_hz == 0 {
        return Err(PlanRefusal {
            code: "invalid_effect_frequency".to_owned(),
            reason: "filter frequency must be greater than zero".to_owned(),
        });
    }
    Ok(())
}

fn operation_changes_samples(operation: &PlanOperation) -> bool {
    !matches!(
        operation,
        PlanOperation::MetadataTransfer { .. }
            | PlanOperation::StoreSourceAudioMd5 { .. }
            | PlanOperation::Verify { .. }
    )
}

fn resolved_reference_delivery_parameters(request: &PlanRequest) -> ResolvedOperationParameters {
    let effective_dither = match request.settings.target_bit_depth {
        BitDepthTarget::Pcm(PcmBitDepth::Int24) => Some(DitherType::Tpdf),
        _ => None,
    };
    match request.settings.target_format {
        AudioFormat::Flac => ResolvedOperationParameters::EncodeFlac {
            requested: normalized_active_flac_settings(request.settings.flac, false),
            effective_dither,
        },
        AudioFormat::WavPack => ResolvedOperationParameters::EncodeWavPack {
            requested: normalized_active_wavpack_settings(request.settings.wavpack),
            effective_dither,
        },
        _ => ResolvedOperationParameters::None,
    }
}

fn output_product_identity(request: &PlanRequest) -> OutputProductIdentity {
    let container_extension = request
        .output_path
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.trim().is_empty())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| request.settings.target_format.extension().to_ascii_lowercase());
    OutputProductIdentity {
        target_format: request.settings.target_format.clone(),
        container_extension,
        catalog_target: request.resolved_output_target,
        container_ffmpeg_flags: request.container_ffmpeg_flags.clone(),
        wavpack_hybrid: request.settings.target_format == AudioFormat::WavPack
            && request.settings.wavpack.hybrid,
        wavpack_correction_file: request.settings.target_format == AudioFormat::WavPack
            && request.settings.wavpack.hybrid
            && request.settings.wavpack.correction_file,
    }
}

fn inherited_loudness_disposition(
    request: &PlanRequest,
    intent: &NormalizedIntent,
    sample_changed: bool,
) -> InheritedLoudnessDisposition {
    if !intent.metadata.transfer_tags {
        return InheritedLoudnessDisposition::NotCopied;
    }
    if let Some(policy) = intent.replay_gain {
        return InheritedLoudnessDisposition::Requested {
            mode: policy.mode,
            existing_tags: request.settings.replay_gain.existing_tags,
            inherited_applicable: !sample_changed,
        };
    }
    if sample_changed {
        InheritedLoudnessDisposition::DropInapplicable
    } else {
        InheritedLoudnessDisposition::PreserveApplicable
    }
}

fn processing_domain_for_pcm_depth(depth: PcmBitDepth) -> ProcessingDomain {
    match depth {
        PcmBitDepth::Float64 => ProcessingDomain::Binary64,
        depth if depth.is_float() => ProcessingDomain::PcmFloating,
        depth => ProcessingDomain::PcmInteger(depth),
    }
}

/// Registered contract for one existing logical operation.  This intentionally
/// does not infer a proof from the operation name: only explicitly registered
/// properties are returned.
pub fn contract_for_operation(operation: &PlanOperation) -> TransformContract {
    let mut contract = TransformContract::plain_transform();
    match operation {
        PlanOperation::MetadataTransfer { .. }
        | PlanOperation::StoreSourceAudioMd5 { .. } => {
            contract.produces_claims.insert(ClaimKind::MetadataEffectSatisfied);
        }
        PlanOperation::Verify { .. } => {
            contract.runtime_obligations.insert("independent_decode".to_owned());
        }
        PlanOperation::DecodeToPcm { .. } => {
            contract.runtime_obligations.insert("complete_reader".to_owned());
            contract.representation.accepted_processing_domains.insert(ProcessingDomain::Source);
            contract.representation.accepted_value_domains.insert(ValueDomain::Encoded);
            if let PlanOperation::DecodeToPcm { bit_depth } = operation {
                contract.representation.emitted_precision = Some(StoragePrecision::Pcm(*bit_depth));
                contract.representation.emitted_processing_domain =
                    Some(processing_domain_for_pcm_depth(*bit_depth));
                contract.representation.emitted_value_domain = Some(if bit_depth.is_float() {
                    ValueDomain::FiniteFloating
                } else {
                    ValueDomain::IntegerLattice(*bit_depth)
                });
            }
        }
        PlanOperation::ResamplePcm { target_bit_depth, .. } => {
            contract.runtime_obligations.insert("complete_reader".to_owned());
            contract.representation.accepted_processing_domains = all_pcm_processing_domains();
            contract.representation.accepted_value_domains = all_pcm_value_domains();
            if let Some(depth) = target_bit_depth {
                contract.representation.emitted_precision = Some(StoragePrecision::Pcm(*depth));
                contract.representation.emitted_processing_domain =
                    Some(processing_domain_for_pcm_depth(*depth));
                contract.representation.emitted_value_domain = Some(if depth.is_float() {
                    ValueDomain::FiniteFloating
                } else {
                    ValueDomain::IntegerLattice(*depth)
                });
            }
        }
        PlanOperation::EncodePcm { target_bit_depth, .. } => {
            contract.runtime_obligations.insert("complete_reader".to_owned());
            contract.representation.accepted_processing_domains = all_pcm_processing_domains();
            contract.representation.accepted_value_domains = all_pcm_value_domains();
            contract.representation.emitted_precision = Some(StoragePrecision::Pcm(*target_bit_depth));
            contract.representation.emitted_processing_domain =
                Some(processing_domain_for_pcm_depth(*target_bit_depth));
            contract.representation.emitted_value_domain = Some(if target_bit_depth.is_float() {
                ValueDomain::FiniteFloating
            } else {
                ValueDomain::IntegerLattice(*target_bit_depth)
            });
        }
        PlanOperation::EncodeLossy { .. } => {
            contract.runtime_obligations.insert("complete_reader".to_owned());
            contract.representation.accepted_processing_domains = all_pcm_processing_domains();
            contract.representation.accepted_value_domains = all_pcm_value_domains();
            contract.representation.emitted_precision = Some(StoragePrecision::Encoded);
            contract.representation.emitted_processing_domain = Some(ProcessingDomain::Registered("encoded-terminal".to_owned()));
            contract.representation.emitted_value_domain = Some(ValueDomain::Encoded);
        }
        PlanOperation::PcmToDsd { .. } => {
            contract.runtime_obligations.insert("complete_reader".to_owned());
            contract.representation.accepted_processing_domains = all_pcm_processing_domains();
            contract.representation.accepted_value_domains = all_pcm_value_domains();
            contract.representation.emitted_precision = Some(StoragePrecision::OneBit);
            contract.representation.emitted_processing_domain = Some(ProcessingDomain::DsdOneBit);
            contract.representation.emitted_value_domain = Some(ValueDomain::OneBit);
        }
        PlanOperation::DsdToPcm { target_bit_depth, .. } => {
            contract.runtime_obligations.insert("complete_reader".to_owned());
            contract.representation.accepted_processing_domains.insert(ProcessingDomain::DsdOneBit);
            contract.representation.accepted_value_domains.insert(ValueDomain::OneBit);
            contract.representation.emitted_precision = Some(StoragePrecision::Pcm(*target_bit_depth));
            contract.representation.emitted_processing_domain =
                Some(processing_domain_for_pcm_depth(*target_bit_depth));
            contract.representation.emitted_value_domain = Some(if target_bit_depth.is_float() {
                ValueDomain::FiniteFloating
            } else {
                ValueDomain::IntegerLattice(*target_bit_depth)
            });
        }
        PlanOperation::DsdRateChange { .. } => {
            contract.runtime_obligations.insert("complete_reader".to_owned());
            contract.representation.accepted_processing_domains.insert(ProcessingDomain::DsdOneBit);
            contract.representation.accepted_value_domains.insert(ValueDomain::OneBit);
            contract.representation.emitted_precision = Some(StoragePrecision::OneBit);
            contract.representation.emitted_processing_domain = Some(ProcessingDomain::DsdOneBit);
            contract.representation.emitted_value_domain = Some(ValueDomain::OneBit);
        }
    }
    contract
}

fn resolved_terminal_operation_parameters(
    request: &PlanRequest,
    operation: &PlanOperation,
    selected_tool: Option<&ToolIdentifier>,
    terminal_realization: Option<&SelectedTerminalRealization>,
) -> Result<ResolvedOperationParameters, PlanRefusal> {
    let mut resolved = resolved_operation_parameters(request, operation, selected_tool, None, None)?;
    let PlanOperation::EncodePcm { .. } = operation else {
        return Ok(resolved);
    };
    let Some(SelectedTerminalRealization::Pcm(realization)) = terminal_realization else {
        return Err(PlanRefusal {
            code: "terminal_realization_missing".to_owned(),
            reason: "selected PCM terminal has no canonical physical realization".to_owned(),
        });
    };
    if selected_tool != Some(&realization.selected_tool) {
        return Err(PlanRefusal {
            code: "terminal_realization_backend".to_owned(),
            reason: "selected PCM terminal backend disagrees with its canonical physical realization".to_owned(),
        });
    }
    match &mut resolved {
        ResolvedOperationParameters::EncodeFlac { effective_dither: value, .. }
        | ResolvedOperationParameters::EncodeWavPack { effective_dither: value, .. }
        | ResolvedOperationParameters::EncodePcm { effective_dither: value } => {
            *value = realization.effective_dither;
        }
        _ => {}
    }
    Ok(resolved)
}

fn terminal_contract_for_candidate(
    request: &PlanRequest,
    operation: &PlanOperation,
    gain_policy: SampleGainPolicy,
    source_is_dsd: bool,
    input_state: &AudioState,
    tool: Option<&ToolIdentifier>,
) -> TransformContract {
    let mut contract = contract_for_operation(operation);

    // Resolve the terminal's physical semantics before proof admission. Invalid
    // candidate-specific realizations are represented by absence here so the
    // normal candidate feasibility machinery can consider a permitted alternate.
    contract.terminal_realization = resolve_selected_terminal_realization(
        request,
        operation,
        gain_policy,
        input_state,
        tool,
    )
    .ok();

    if !gain_policy.is_true_peak() {
        return contract;
    }

    let Some(realization) = contract.terminal_realization.as_ref() else {
        return contract;
    };

    // The hard-ceiling implementation has numerical authority only for these
    // qualified physical terminal shapes. FFmpeg direct dither remains
    // unsupported except for the exact Float64 -> Int32 plain-triangular
    // realization after its architecture-specific exact-closure commissioning
    // gate has been completed. The source-derived model may exist in-tree
    // before that gate opens, but it is not production proof authority.
    let proof_available = match realization {
        SelectedTerminalRealization::Pcm(realization) => match realization.kind {
            PcmTerminalRealizationKind::SoxDirect => !realization.wavpack_hybrid,
            PcmTerminalRealizationKind::FfmpegDirect => {
                !realization.wavpack_hybrid
                    && (realization.effective_dither.is_none()
                        || is_qualified_ffmpeg_int32_triangular_terminal(realization))
            }
            PcmTerminalRealizationKind::SoxPreterminalFfmpegPackage => true,
            PcmTerminalRealizationKind::SoxPreterminalWavPackHybrid => {
                // Preserve the retained qualified hybrid shape: its proved
                // preterminal owns the requested dithered integer realization.
                realization.effective_dither.is_some()
            }
            PcmTerminalRealizationKind::SsrcDirectWav
            | PcmTerminalRealizationKind::SsrcPreterminalFfmpegPackage => false,
            PcmTerminalRealizationKind::NativeWavPackHybridPackage => false,
        },
        SelectedTerminalRealization::LossyFfmpegEncoderInput { .. } => true,
    };
    if !proof_available {
        return contract;
    }

    let family = if source_is_dsd {
        TerminalProofAuthorityFamily::AlbumGainV2
    } else {
        TerminalProofAuthorityFamily::PcmTruePeakV2
    };
    contract.terminal_proof = Some(TerminalProofContract {
        authority: terminal_proof_authority(family, realization),
        // Both retained authorities govern a Float64 processing carrier after
        // the policy scalar. Q1.31-derived binary64 is a distinct stronger
        // input and is listed explicitly rather than inferred from storage width.
        accepted_value_domains: BTreeSet::from([
            ValueDomain::FiniteFloating,
            ValueDomain::Q1_31DerivedBinary64,
        ]),
        requires_non_clipping_ingress: true,
    });
    contract
        .runtime_obligations
        .insert("terminal_bound_recheck".to_owned());
    if matches!(
        realization,
        SelectedTerminalRealization::Pcm(realization)
            if is_qualified_ffmpeg_int32_triangular_terminal(realization)
    ) {
        contract
            .runtime_obligations
            .insert("ffmpeg_int32_triangular_terminal_identity".to_owned());
    }
    contract
}

fn dither_contract_name(dither: DitherType) -> &'static str {
    match dither {
        DitherType::None => "none",
        DitherType::Tpdf => "tpdf",
        DitherType::SlopedTpdf => "sloped_tpdf",
        DitherType::Lipshitz => "lipshitz",
        DitherType::FWeighted => "f_weighted",
        DitherType::ModifiedEWeighted => "modified_e_weighted",
        DitherType::ImprovedEWeighted => "improved_e_weighted",
        DitherType::Gesemann => "gesemann",
        DitherType::Shibata => "shibata",
        DitherType::LowShibata => "low_shibata",
        DitherType::HighShibata => "high_shibata",
    }
}

fn source_audio_state(request: &PlanRequest, id: SignalId) -> AudioState {
    let coding = match request.source.representation_kind() {
        SourceRepresentationKind::Dsd => SignalCoding::Dsd,
        SourceRepresentationKind::Lossy => SignalCoding::Lossy(request.source.codec.clone()),
        SourceRepresentationKind::Pcm => SignalCoding::Pcm,
        SourceRepresentationKind::Unknown | SourceRepresentationKind::Unspecified => SignalCoding::Unknown,
    };
    let precision = if request.source.is_dsd() {
        StoragePrecision::OneBit
    } else if let Some(depth) = request.source.bit_depth {
        StoragePrecision::Pcm(depth)
    } else if request.source.codec.is_lossy() {
        StoragePrecision::Encoded
    } else {
        StoragePrecision::Pending
    };
    AudioState {
        id,
        coding,
        sample_rate_hz: request.source.sample_rate_hz.map(Fact::Known).unwrap_or(Fact::Pending("source.sample_rate_hz".to_owned())),
        channels: source_channels_fact(request),
        channel_layout: source_channel_layout_fact(),
        frame_extent: source_frame_extent(request),
        programme: ProgrammeState::IndependentTrack,
        precision,
        storage_contract: Fact::Pending("source.storage_encoding".to_owned()),
        processing_domain: source_processing_domain(request),
        value_domain: source_value_domain(request),
        level_basis: LevelBasis::Ordinary,
        claims: BTreeSet::new(),
        obligations: BTreeSet::new(),
    }
}

fn source_channels_fact(request: &PlanRequest) -> Fact<u16> {
    request.source.channels.map(Fact::Known).unwrap_or(Fact::Pending("source.channels".to_owned()))
}

fn source_channel_layout_fact() -> Fact<Vec<String>> {
    Fact::Pending("source.channel_layout_roles".to_owned())
}

fn source_frame_extent(request: &PlanRequest) -> FrameExtent {
    match request.source.frame_extent {
        Some(crate::source::SourceFrameExtent::Exact(frames)) => FrameExtent::Exact(frames),
        Some(crate::source::SourceFrameExtent::Bounded { upper_frames }) => {
            FrameExtent::Bounded { upper_frames }
        }
        None => request.source.duration.map_or_else(
            || FrameExtent::Pending("source.frame_extent".to_owned()),
            |duration| {
                FrameExtent::EstimatedDurationNanos(
                    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX),
                )
            },
        ),
    }
}

fn source_processing_domain(request: &PlanRequest) -> Fact<ProcessingDomain> {
    use crate::enums::SampleKind;
    match request.source.sample_kind {
        Some(SampleKind::Dsd) => Fact::Known(ProcessingDomain::DsdOneBit),
        Some(SampleKind::SignedInteger | SampleKind::UnsignedInteger) => request
            .source
            .bit_depth
            .map(ProcessingDomain::PcmInteger)
            .map(Fact::Known)
            .unwrap_or_else(|| Fact::Pending("source.bit_depth".to_owned())),
        Some(SampleKind::Float) => Fact::Known(
            request
                .source
                .bit_depth
                .map(processing_domain_for_pcm_depth)
                .unwrap_or(ProcessingDomain::PcmFloating),
        ),
        None => Fact::Known(ProcessingDomain::Source),
    }
}

fn source_value_domain(request: &PlanRequest) -> ValueDomain {
    use crate::enums::SampleKind;
    match request.source.sample_kind {
        Some(SampleKind::Dsd) => ValueDomain::OneBit,
        Some(SampleKind::SignedInteger | SampleKind::UnsignedInteger) => request
            .source
            .authoritative_pcm_depth()
            .or(request.source.bit_depth)
            .map(ValueDomain::IntegerLattice)
            .unwrap_or_else(|| ValueDomain::Pending("source.authoritative_pcm_depth".to_owned())),
        Some(SampleKind::Float) => ValueDomain::FiniteFloating,
        None if request.source.codec.is_lossy() => ValueDomain::Encoded,
        None => ValueDomain::Pending("source.value_domain".to_owned()),
    }
}

fn requested_pcm_rate_fact(request: &PlanRequest) -> Fact<u32> {
    match request.settings.target_sample_rate {
        RateTarget::PcmHz(hz) => Fact::Known(hz),
        RateTarget::Source if request.source.is_dsd() => request
            .source
            .dsd_rate()
            .map(|rate| Fact::Known(rate.default_pcm_target_hz()))
            .unwrap_or_else(|| Fact::Pending("source.sample_rate_hz".to_owned())),
        RateTarget::Source => request
            .source
            .sample_rate_hz
            .map(Fact::Known)
            .unwrap_or_else(|| Fact::Pending("source.sample_rate_hz".to_owned())),
        RateTarget::Dsd(_) => Fact::Unavailable(
            "a PCM target cannot resolve from a DSD rate target".to_owned(),
        ),
    }
}

fn target_pcm_rate_fact(request: &PlanRequest) -> Fact<u32> {
    let requested = match requested_pcm_rate_fact(request) {
        Fact::Known(value) => value,
        Fact::Pending(key) => return Fact::Pending(key),
        Fact::Unavailable(reason) => return Fact::Unavailable(reason),
    };
    if !request.settings.target_format.is_lossy() {
        return Fact::Known(requested);
    }
    match mapping::ffmpeg_lossy_encoder_accepts_rate_directly(
        &request.settings.target_format,
        requested,
    ) {
        Some(true) => Fact::Known(requested),
        Some(false) => mapping::ffmpeg_lossy_encoder_rate_for_request(
            &request.settings.target_format,
            requested,
        )
        .map(Fact::Known)
        .unwrap_or_else(|| {
            Fact::Unavailable(format!(
                "{} has no admitted encoder-input rate for {requested} Hz",
                request.settings.target_format,
            ))
        }),
        None => Fact::Unavailable(format!(
            "{} has no configured lossy encoder rate contract",
            request.settings.target_format,
        )),
    }
}

fn target_pcm_depth_fact(request: &PlanRequest) -> Fact<PcmBitDepth> {
    match request.settings.target_bit_depth {
        BitDepthTarget::Pcm(depth) => Fact::Known(depth),
        BitDepthTarget::Source if !request.settings.target_format.is_pcm_lossless() => {
            Fact::Known(
                request
                    .source
                    .authoritative_pcm_depth()
                    .unwrap_or_else(|| crate::settings::default_pcm_depth_for_format(&request.settings.target_format)),
            )
        }
        BitDepthTarget::Source => match request.source.representation_kind() {
            SourceRepresentationKind::Dsd | SourceRepresentationKind::Lossy => Fact::Known(
                crate::settings::default_pcm_depth_for_format(&request.settings.target_format),
            ),
            SourceRepresentationKind::Pcm => request
                .source
                .authoritative_pcm_depth()
                .map(Fact::Known)
                .unwrap_or_else(|| Fact::Pending("source.authoritative_pcm_depth".to_owned())),
            SourceRepresentationKind::Unknown | SourceRepresentationKind::Unspecified => {
                Fact::Pending("source.representation_kind".to_owned())
            }
        },
    }
}

fn semantic_terminal_operation(
    request: &PlanRequest,
    bridge_operations: &[PlanOperation],
) -> Option<PlanOperation> {
    if let Some(operation) = bridge_operations.iter().rev().find(|operation| {
        matches!(
            operation,
            PlanOperation::EncodePcm { .. }
                | PlanOperation::EncodeLossy { .. }
                | PlanOperation::PcmToDsd { .. }
                | PlanOperation::DsdRateChange { .. }
        )
    }) {
        return Some(operation.clone());
    }

    // The legacy DSD planner can fuse reconstruction and terminal PCM encoding
    // into one `DsdToPcm` operation.  The common semantic spine has already
    // named reconstruction separately, so expose the fused terminal as an
    // explicit PCM terminal here rather than duplicating reconstruction.
    if request.source.is_dsd() && request.settings.target_format.is_pcm_lossless() {
        if let Some(PlanOperation::DsdToPcm {
            target_format,
            target_rate_hz,
            target_bit_depth,
            ..
        }) = bridge_operations
            .iter()
            .rev()
            .find(|operation| matches!(operation, PlanOperation::DsdToPcm { .. }))
        {
            if target_format == &request.settings.target_format {
                return Some(PlanOperation::EncodePcm {
                    target_format: target_format.clone(),
                    target_rate_hz: Some(*target_rate_hz),
                    target_bit_depth: *target_bit_depth,
                    apply_processing: false,
                });
            }
        }
    }

    // Capability-gated Phase-3 plans intentionally have no Phase-2 command
    // lowering.  Their semantic graph still needs the physical terminal
    // boundary, so synthesize the same logical terminal from normalized target
    // facts rather than packaging a pre-terminal float state as if it were the
    // requested product.
    if request.settings.target_format.is_pcm_lossless() {
        let Fact::Known(target_bit_depth) = target_pcm_depth_fact(request) else {
            return None;
        };
        let target_rate_hz = match target_pcm_rate_fact(request) {
            Fact::Known(rate) => Some(rate),
            Fact::Pending(_) | Fact::Unavailable(_) => None,
        };
        return Some(PlanOperation::EncodePcm {
            target_format: request.settings.target_format.clone(),
            target_rate_hz,
            target_bit_depth,
            apply_processing: false,
        });
    }

    if request.settings.target_format.is_lossy() {
        let target_rate_hz = match target_pcm_rate_fact(request) {
            Fact::Known(rate) => Some(rate),
            Fact::Pending(_) | Fact::Unavailable(_) => None,
        };
        return Some(PlanOperation::EncodeLossy {
            target_format: request.settings.target_format.clone(),
            target_rate_hz,
            apply_processing: false,
        });
    }

    if request.settings.target_format.is_dsd() && !request.source.is_dsd() {
        let target_rate = match request.settings.target_sample_rate {
            RateTarget::Dsd(rate) => rate,
            RateTarget::Source => request.source.dsd_rate()?,
            RateTarget::PcmHz(_) => return None,
        };
        return Some(PlanOperation::PcmToDsd {
            target_format: request.settings.target_format.clone(),
            target_rate,
            filter: request.settings.dsd.pcm_to_dsd.filter,
        });
    }

    None
}

fn terminal_audio_state(
    request: &PlanRequest,
    id: SignalId,
    input: &AudioState,
    operation: &PlanOperation,
) -> AudioState {
    let mut state = input.clone();
    state.id = id;
    state.claims.clear();
    state.obligations = BTreeSet::from([RuntimeObligation::PublicationBarrier]);
    state.frame_extent = match operation {
        PlanOperation::EncodeLossy { .. }
        | PlanOperation::PcmToDsd { .. }
        | PlanOperation::DsdToPcm { .. }
        | PlanOperation::DsdRateChange { .. }
        | PlanOperation::ResamplePcm { .. } => {
            FrameExtent::Pending(format!("terminal.{}.frame_extent", operation.label()))
        }
        _ => input.frame_extent.clone(),
    };

    match operation {
        PlanOperation::EncodePcm {
            target_format,
            target_rate_hz,
            target_bit_depth,
            ..
        } => {
            state.coding = SignalCoding::Pcm;
            if let Some(rate) = target_rate_hz {
                state.sample_rate_hz = Fact::Known(*rate);
            }
            state.precision = StoragePrecision::Pcm(*target_bit_depth);
            state.storage_contract = Fact::Known(format!(
                "{}:{}",
                target_format.extension(),
                if target_bit_depth.is_float() {
                    format!("f{}", target_bit_depth.bits())
                } else {
                    format!("s{}", target_bit_depth.bits())
                }
            ));
            state.processing_domain =
                Fact::Known(processing_domain_for_pcm_depth(*target_bit_depth));
            state.value_domain = if target_bit_depth.is_float() {
                ValueDomain::FiniteFloating
            } else {
                ValueDomain::IntegerLattice(*target_bit_depth)
            };
        }
        PlanOperation::EncodeLossy {
            target_format,
            target_rate_hz,
            ..
        } => {
            state.coding = SignalCoding::Lossy(lossy_codec_for_format(target_format));
            if let Some(rate) = target_rate_hz {
                state.sample_rate_hz = Fact::Known(*rate);
            }
            state.precision = StoragePrecision::Encoded;
            state.storage_contract = Fact::Known(format!("encoded:{}", target_format.extension()));
            state.processing_domain = Fact::Known(ProcessingDomain::Registered(format!(
                "{}-encoded",
                target_format.extension()
            )));
            state.value_domain = ValueDomain::Encoded;
        }
        PlanOperation::PcmToDsd {
            target_format,
            target_rate,
            ..
        }
        | PlanOperation::DsdRateChange {
            target_format,
            target_rate,
            ..
        } => {
            state.coding = SignalCoding::Dsd;
            state.sample_rate_hz = Fact::Known(target_rate.hz());
            state.precision = StoragePrecision::OneBit;
            state.storage_contract = Fact::Known(format!("dsd:{}", target_format.extension()));
            state.processing_domain = Fact::Known(ProcessingDomain::DsdOneBit);
            state.value_domain = ValueDomain::OneBit;
            state.level_basis = LevelBasis::Ordinary;
        }
        PlanOperation::DsdToPcm {
            target_format,
            target_rate_hz,
            target_bit_depth,
            ..
        } => {
            state.coding = SignalCoding::Pcm;
            state.sample_rate_hz = Fact::Known(*target_rate_hz);
            state.precision = StoragePrecision::Pcm(*target_bit_depth);
            state.storage_contract = Fact::Known(format!(
                "{}:{}",
                target_format.extension(),
                if target_bit_depth.is_float() {
                    format!("f{}", target_bit_depth.bits())
                } else {
                    format!("s{}", target_bit_depth.bits())
                }
            ));
            state.processing_domain =
                Fact::Known(processing_domain_for_pcm_depth(*target_bit_depth));
            state.value_domain = if target_bit_depth.is_float() {
                ValueDomain::FiniteFloating
            } else {
                ValueDomain::IntegerLattice(*target_bit_depth)
            };
        }
        PlanOperation::ResamplePcm {
            target_rate_hz,
            target_bit_depth,
            ..
        } => {
            state.sample_rate_hz = Fact::Known(*target_rate_hz);
            if let Some(depth) = target_bit_depth {
                state.precision = StoragePrecision::Pcm(*depth);
                state.value_domain = if depth.is_float() {
                    ValueDomain::FiniteFloating
                } else {
                    ValueDomain::IntegerLattice(*depth)
                };
            }
            state.storage_contract = Fact::Pending("terminal.resample.storage_encoding".to_owned());
            state.processing_domain = Fact::Pending("terminal.resample.processing_domain".to_owned());
        }
        PlanOperation::DecodeToPcm { bit_depth } => {
            state.coding = SignalCoding::Pcm;
            state.precision = StoragePrecision::Pcm(*bit_depth);
            state.storage_contract = Fact::Pending("terminal.decode.storage_encoding".to_owned());
            state.processing_domain = Fact::Pending("terminal.decode.processing_domain".to_owned());
            state.value_domain = if bit_depth.is_float() {
                ValueDomain::FiniteFloating
            } else {
                ValueDomain::IntegerLattice(*bit_depth)
            };
        }
        PlanOperation::MetadataTransfer { .. }
        | PlanOperation::StoreSourceAudioMd5 { .. }
        | PlanOperation::Verify { .. } => {
            // These are artifact/observation operations and should never be
            // selected as the terminal signal producer.  Preserve the input
            // state defensively if a future caller does so.
        }
    }

    if request.settings.target_format.is_lossy() {
        state.level_basis = LevelBasis::Ordinary;
    }
    state
}

fn lossy_codec_for_format(format: &AudioFormat) -> AudioCodec {
    match format {
        AudioFormat::Mp3 => AudioCodec::Mp3,
        AudioFormat::Aac => AudioCodec::Aac,
        AudioFormat::Opus => AudioCodec::Opus,
        AudioFormat::Dts => AudioCodec::Custom("dts".to_owned()),
        AudioFormat::Ac3 => AudioCodec::Custom("ac3".to_owned()),
        other => AudioCodec::Custom(format!("{}-encoded", other.extension())),
    }
}

fn target_pcm_precision(request: &PlanRequest) -> StoragePrecision {
    match target_pcm_depth_fact(request) {
        Fact::Known(depth) => StoragePrecision::Pcm(depth),
        Fact::Pending(_) | Fact::Unavailable(_) => StoragePrecision::Pending,
    }
}

fn export_level_basis(level: DsdGeneralExportLevel) -> LevelBasis {
    match level {
        DsdGeneralExportLevel::Native => LevelBasis::DsdNative,
        DsdGeneralExportLevel::NominalCompensated => LevelBasis::DsdNominalCompensated,
        DsdGeneralExportLevel::ProtectedR64 => LevelBasis::ProtectedR64,
        DsdGeneralExportLevel::NativeWithOffset { offset_db } => LevelBasis::DsdNativeWithOffset(offset_db),
    }
}

/// Resolve the exact scalar at the explicit general DSD export-level boundary.
///
/// This is shared planner/executor policy authority. In particular, native
/// export from a Reference-protected reconstruction restores the private
/// -12 dB R64 headroom exactly once, while ordinary General reconstruction
/// starts at native level and therefore needs no such restoration.
pub fn resolve_dsd_general_export_gain(
    reconstruction: DsdGeneralReconstruction,
    level: DsdGeneralExportLevel,
) -> Result<DbNano, PlanRefusal> {
    let base = match reconstruction {
        DsdGeneralReconstruction::General => DbNano::ZERO,
        DsdGeneralReconstruction::ReferenceProtected => DbNano::HEADROOM_RESTORATION,
    };
    match level {
        DsdGeneralExportLevel::Native => Ok(base),
        DsdGeneralExportLevel::NominalCompensated => base
            .checked_add(DbNano::DSD_COMPENSATION)
            .ok_or_else(|| PlanRefusal {
                code: "dsd_export_gain_overflow".to_owned(),
                reason: "the declared DSD nominal-compensated export scalar is out of range"
                    .to_owned(),
            }),
        DsdGeneralExportLevel::ProtectedR64 => {
            if reconstruction == DsdGeneralReconstruction::ReferenceProtected {
                Ok(DbNano::ZERO)
            } else {
                Err(PlanRefusal {
                    code: "protected_export_without_protected_reconstruction".to_owned(),
                    reason: "protected R64 export requires the Reference-protected reconstruction prefix"
                        .to_owned(),
                })
            }
        }
        DsdGeneralExportLevel::NativeWithOffset { offset_db } => base
            .checked_add(offset_db)
            .ok_or_else(|| PlanRefusal {
                code: "dsd_export_gain_overflow".to_owned(),
                reason: "the declared DSD native export offset is out of range".to_owned(),
            }),
    }
}

fn true_peak_policy(policy: SampleGainPolicy) -> Option<(DbNano, TruePeakScope, TruePeakScanTier, bool)> {
    match policy {
        SampleGainPolicy::TruePeakGuard { target_dbtp, scope, scan } => Some((target_dbtp, scope, scan, false)),
        SampleGainPolicy::TruePeakNormalize { target_dbtp, scope, scan } => Some((target_dbtp, scope, scan, true)),
        SampleGainPolicy::Off | SampleGainPolicy::FixedGain { .. } => None,
    }
}

fn dsd_reconstruction_candidates(
    request: &PlanRequest,
    reference_reconstruction: bool,
    target_rate_hz: u32,
    target_bit_depth: PcmBitDepth,
) -> Vec<PhysicalCandidate> {
    if reference_reconstruction {
        vec![PhysicalCandidate {
            identity: "reference-v16-qualified".to_owned(),
            tool: Some(ToolIdentifier::Sox),
            contract: TransformContract {
                representation: BoundaryRepresentationContract {
                    accepted_processing_domains: BTreeSet::from([ProcessingDomain::DsdOneBit]),
                    accepted_value_domains: BTreeSet::from([ValueDomain::OneBit]),
                    emitted_processing_domain: Some(ProcessingDomain::Binary64),
                    emitted_precision: Some(StoragePrecision::Pcm(PcmBitDepth::Float64)),
                    emitted_value_domain: Some(ValueDomain::Q1_31DerivedBinary64),
                },
                terminal_realization: None,
                terminal_proof: None,
                carries_claims: BTreeSet::new(),
                produces_claims: BTreeSet::from([ClaimKind::ReferenceReconstruction]),
                signal_equivalent_for: BTreeSet::new(),
                runtime_obligations: BTreeSet::from(["reference_attestation".to_owned(), "complete_reader".to_owned()]),
            },
            executable: true,
            binary64_resample_preservation_evidence: None,
            protected_float64_ingress_authority: None,
        }]
    } else {
        let operation = PlanOperation::DsdToPcm {
            target_format: AudioFormat::Wav,
            target_rate_hz,
            target_bit_depth,
            lowpass: request.settings.dsd.general_from_dsd.lowpass,
        };
        registered_candidates_for_operation(
            request,
            &operation,
            contract_for_operation(&operation),
            None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsd_reference::{DsdSourceKind, SacdAreaKind, SacdFrameEncoding, SacdTrackSelection, Sha256Digest};
    use crate::enums::{AudioCodec, DsdFilterPreset, SampleKind, SsrcProfile};
    use crate::settings::PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP;
    use crate::source::SourceInfo;
    use std::path::PathBuf;
    use std::time::Duration;

    fn dsd_request(policy: SampleGainPolicy) -> PlanRequest {
        let mut settings = crate::settings::PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.target_sample_rate = RateTarget::PcmHz(176_400);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
        settings.dsd.set_gain_policy(policy);
        PlanRequest {
            input_path: PathBuf::from("in.dsf"),
            output_path: PathBuf::from("out.flac"),
            source: SourceInfo {
                format: AudioFormat::Dsf,
                codec: AudioCodec::Dsd,
                sample_rate_hz: Some(DsdRate::Dsd64.hz()),
                bit_depth: None,
                true_source_depth: None,
                source_representation: SourceRepresentationKind::Dsd,
                sample_kind: Some(SampleKind::Dsd),
                channels: Some(2),
                duration: Some(Duration::from_secs(60)),
                frame_extent: None,
                dsd_source_kind: None,
                audio_md5: None,
            },
            settings,
            plan_scope: crate::plan::PlanScope::submitted_batch("test-submission", "test-track", Some(1)),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
        }
    }


    fn pcm_request(policy: SampleGainPolicy) -> PlanRequest {
        let mut settings = crate::settings::PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.target_sample_rate = RateTarget::PcmHz(48_000);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
        settings.pcm_true_peak.policy = policy;
        PlanRequest {
            input_path: PathBuf::from("in.wav"),
            output_path: PathBuf::from("out.flac"),
            source: SourceInfo {
                format: AudioFormat::Wav,
                codec: AudioCodec::PcmSigned,
                sample_rate_hz: Some(48_000),
                bit_depth: Some(PcmBitDepth::Int24),
                true_source_depth: Some(PcmBitDepth::Int24),
                source_representation: SourceRepresentationKind::Pcm,
                sample_kind: Some(SampleKind::SignedInteger),
                channels: Some(2),
                duration: Some(Duration::from_secs(60)),
                frame_extent: None,
                dsd_source_kind: None,
                audio_md5: None,
            },
            settings,
            plan_scope: PlanScope::track("pcm-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
        }
    }

    fn admit_reference(request: &mut PlanRequest, target: ResolvedOutputTarget) {
        request.settings.dsd = DsdSettings::reference();
        request.source.dsd_source_kind = Some(DsdSourceKind::DsfUncompressed);
        request.resolved_output_target = Some(target);
        request.settings.target_sample_rate = RateTarget::PcmHz(176_400);
        request.settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
        request.settings.wavpack.hybrid = false;
        request.settings.wavpack.correction_file = false;
        request.planned_riff_non_audio_upper_bound_bytes = Some(0);
        match target {
            ResolvedOutputTarget::FlacNative => {
                request.settings.target_format = AudioFormat::Flac;
                request.output_path = PathBuf::from("out.flac");
            }
            ResolvedOutputTarget::WavPackNative => {
                request.settings.target_format = AudioFormat::WavPack;
                request.output_path = PathBuf::from("out.wv");
            }
            ResolvedOutputTarget::WavRiff
            | ResolvedOutputTarget::WavRf64
            | ResolvedOutputTarget::WavW64 => {
                request.settings.target_format = AudioFormat::Wav;
                request.output_path = PathBuf::from(match target {
                    ResolvedOutputTarget::WavW64 => "out.w64",
                    ResolvedOutputTarget::WavRf64 => "out.rf64",
                    _ => "out.wav",
                });
            }
            _ => panic!("test helper only admits the retained Reference lossless targets"),
        }
    }

    fn true_peak_observation(plan: &TypedConversionPlan) -> &Observation {
        plan.nodes
            .iter()
            .find_map(|node| match node {
                TypedPlanNode::Observe(observation)
                    if matches!(observation.kind, ObservationKind::CertifiedTruePeak { .. }) =>
                {
                    Some(observation)
                }
                _ => None,
            })
            .expect("true-peak plan must contain its certified observation")
    }

    fn true_peak_decision(plan: &TypedConversionPlan) -> &Decision {
        plan.nodes
            .iter()
            .find_map(|node| match node {
                TypedPlanNode::Decide(decision)
                    if matches!(decision.kind, DecisionKind::TruePeakGain { .. }) => Some(decision),
                _ => None,
            })
            .expect("true-peak plan must contain its gain decision")
    }

    fn float64_pcm_request(policy: SampleGainPolicy) -> PlanRequest {
        let mut request = pcm_request(policy);
        request.source.codec = AudioCodec::PcmFloat;
        request.source.bit_depth = Some(PcmBitDepth::Float64);
        request.source.true_source_depth = Some(PcmBitDepth::Float64);
        request.source.sample_kind = Some(SampleKind::Float);
        request
    }

    fn selected_pcm_terminal(
        plan: &TypedConversionPlan,
    ) -> (&PhysicalCandidate, &ResolvedOperationParameters) {
        plan.nodes
            .iter()
            .find_map(|node| match node {
                TypedPlanNode::Operation {
                    operation: PlanOperation::EncodePcm { .. },
                    candidates,
                    selected_candidate,
                    resolved_parameters,
                    ..
                } => candidates
                    .get(*selected_candidate)
                    .map(|candidate| (candidate, resolved_parameters)),
                _ => None,
            })
            .expect("plan must select a PCM terminal")
    }

    #[test]
    fn decoded_integer_carrier_keeps_physical_width_but_uses_authoritative_value_lattice() {
        let mut request = pcm_request(SampleGainPolicy::Off);
        request.source.bit_depth = Some(PcmBitDepth::Int32);
        request.source.true_source_depth = Some(PcmBitDepth::Int16);
        request.source.sample_kind = Some(SampleKind::SignedInteger);

        let state = source_audio_state(&request, SignalId(0));
        assert_eq!(state.precision, StoragePrecision::Pcm(PcmBitDepth::Int32));
        assert_eq!(
            state.processing_domain,
            Fact::Known(ProcessingDomain::PcmInteger(PcmBitDepth::Int32)),
        );
        assert_eq!(
            state.value_domain,
            ValueDomain::IntegerLattice(PcmBitDepth::Int16),
            "carrier padding must not manufacture a semantic precision reduction",
        );
    }

    #[test]
    fn dsd_track_guard_is_phase3_common_realizer_executable() {
        let request = dsd_request(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Standard,
        });
        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else { panic!("typed plan should be ready") };
        assert_eq!(plan.execution_capability, ExecutionCapability::ExecutableByPhase3CommonRealizer);
        assert!(require_current_executor(&plan).is_err());
        assert!(plan.nodes.iter().any(|node| matches!(node, TypedPlanNode::Observe(Observation { kind: ObservationKind::CertifiedTruePeak { scan: TruePeakScanTier::Standard }, .. }))));
        assert!(plan.nodes.iter().any(|node| matches!(node, TypedPlanNode::Decide(Decision { kind: DecisionKind::TruePeakGain { allow_boost: false, scope: TruePeakScope::Track, .. }, .. }))));
        let observation = true_peak_observation(&plan);
        assert!(observation.complete_reader_required);
        assert!(
            observation.read_contract.connected_executor,
            "Phase 3 common-realizer plans must expose the connected certified reader",
        );
    }

    #[test]
    fn backend_preference_ranks_only_connected_registered_dsd_candidates() {
        let mut request = dsd_request(SampleGainPolicy::Off);
        request.settings.preferred_tool = PreferredTool::Ffmpeg;
        request.settings.dsd.general_from_dsd.lowpass = DsdLowpassMethod::Sinc;

        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("general DSD Sinc should retain its admitted SoX route")
        };
        let selected = plan.nodes.iter().find_map(|node| match node {
            TypedPlanNode::Operation {
                operation: PlanOperation::DsdToPcm { .. },
                candidates,
                selected_candidate,
                ..
            } => candidates.get(*selected_candidate),
            _ => None,
        });
        assert_eq!(selected.map(|candidate| candidate.identity.as_str()), Some("registered:dsd_to_pcm:sox"));
    }

    #[test]
    fn unconnected_backend_preference_does_not_change_resolved_identity() {
        let mut sox_request = dsd_request(SampleGainPolicy::Off);
        sox_request.settings.dsd.general_from_dsd.lowpass = DsdLowpassMethod::Auto;
        sox_request.settings.preferred_tool = PreferredTool::Sox;
        let Ok(PlanningOutcome::Ready(sox_plan)) = plan_typed(&sox_request) else {
            panic!("SoX-preferred plan should be ready")
        };

        let mut ffmpeg_request = sox_request.clone();
        ffmpeg_request.settings.preferred_tool = PreferredTool::Ffmpeg;
        let Ok(PlanningOutcome::Ready(ffmpeg_plan)) = plan_typed(&ffmpeg_request) else {
            panic!("FFmpeg-preferred plan should retain the connected SoX DSD route")
        };

        assert_eq!(
            crate::fingerprint::common_semantic_plan_fingerprint_v1(&sox_request, &sox_plan),
            crate::fingerprint::common_semantic_plan_fingerprint_v1(
                &ffmpeg_request,
                &ffmpeg_plan
            ),
            "a preference for an unconnected DSD backend must not change resolved sample semantics",
        );
        assert_ne!(
            crate::fingerprint::common_execution_plan_fingerprint_v1(&sox_request, &sox_plan),
            crate::fingerprint::common_execution_plan_fingerprint_v1(
                &ffmpeg_request,
                &ffmpeg_plan
            ),
            "the preference may still change a connected physical terminal even though DSD reconstruction semantics remain SoX",
        );
    }

    #[test]
    fn reference_scan_tier_does_not_claim_reference_delivery() {
        let request = dsd_request(SampleGainPolicy::TruePeakNormalize {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Reference,
        });
        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else { panic!("typed plan should be ready") };
        assert!(!plan.intent.reference_delivery);
        assert!(!plan.audio_states.iter().flat_map(|state| state.claims.iter()).any(|claim| matches!(claim, Claim::ReferenceDsdDelivery)));
    }

    fn candidate_with_value_domain(
        identity: &str,
        tool: ToolIdentifier,
        value_domain: ValueDomain,
        terminal_proof: Option<TerminalProofContract>,
    ) -> PhysicalCandidate {
        PhysicalCandidate {
            identity: identity.to_owned(),
            tool: Some(tool),
            contract: TransformContract {
                representation: BoundaryRepresentationContract {
                    accepted_processing_domains: BTreeSet::from([ProcessingDomain::Binary64]),
                    accepted_value_domains: BTreeSet::from([value_domain]),
                    emitted_processing_domain: Some(ProcessingDomain::Binary64),
                    emitted_precision: Some(StoragePrecision::Pcm(PcmBitDepth::Float64)),
                    emitted_value_domain: Some(ValueDomain::FiniteFloating),
                },
                terminal_realization: None,
                terminal_proof,
                carries_claims: BTreeSet::new(),
                produces_claims: BTreeSet::new(),
                signal_equivalent_for: BTreeSet::new(),
                runtime_obligations: BTreeSet::from(["complete_reader".to_owned()]),
            },
            executable: true,
            binary64_resample_preservation_evidence: None,
            protected_float64_ingress_authority: None,
        }
    }

    #[test]
    fn preferred_candidate_without_terminal_proof_falls_back_but_forced_refuses() {
        let preferred = candidate_with_value_domain(
            "preferred-unproved",
            ToolIdentifier::Ffmpeg,
            ValueDomain::FiniteFloating,
            None,
        );
        let fallback = candidate_with_value_domain(
            "admitted-fallback",
            ToolIdentifier::Sox,
            ValueDomain::FiniteFloating,
            Some(TerminalProofContract {
                authority: "test-terminal-proof".to_owned(),
                accepted_value_domains: BTreeSet::from([ValueDomain::FiniteFloating]),
                requires_non_clipping_ingress: true,
            }),
        );
        let candidates = [preferred, fallback];
        let requirements = CandidateRequirements {
            input_processing_domain: Some(ProcessingDomain::Binary64),
            input_value_domain: Some(ValueDomain::FiniteFloating),
            terminal_proof_required: true,
            terminal_realization_required: false,
            complete_reader_required: true,
            required_runtime_obligations: BTreeSet::new(),
            binary64_resample_preservation_required: false,
            connected_executor_required: true,
        };
        assert_eq!(
            select_candidate_for_requirements(
                &candidates,
                Some(&ToolIdentifier::Ffmpeg),
                false,
                &requirements,
            )
            .unwrap()
            .identity,
            "admitted-fallback"
        );
        let forced = select_candidate_for_requirements(
            &candidates,
            Some(&ToolIdentifier::Ffmpeg),
            true,
            &requirements,
        )
        .expect_err("forced unproved candidate must refuse");
        assert!(forced.reason.contains("terminal/error proof"));
    }

    #[test]
    fn q1_31_derived_binary64_is_not_interchangeable_with_arbitrary_binary64() {
        let candidate = candidate_with_value_domain(
            "finite-only",
            ToolIdentifier::Sox,
            ValueDomain::FiniteFloating,
            Some(TerminalProofContract {
                authority: "finite-only-proof".to_owned(),
                accepted_value_domains: BTreeSet::from([ValueDomain::FiniteFloating]),
                requires_non_clipping_ingress: true,
            }),
        );
        let requirements = CandidateRequirements {
            input_processing_domain: Some(ProcessingDomain::Binary64),
            input_value_domain: Some(ValueDomain::Q1_31DerivedBinary64),
            terminal_proof_required: true,
            terminal_realization_required: false,
            complete_reader_required: true,
            required_runtime_obligations: BTreeSet::new(),
            binary64_resample_preservation_required: false,
            connected_executor_required: true,
        };
        let refusal = select_candidate_for_requirements(&[candidate], None, false, &requirements)
            .expect_err("finite binary64 contract must not imply Q1.31-derived lattice");
        assert_eq!(refusal.code, "no_admitted_candidate");
    }

    #[test]
    fn bounded_candidate_search_reports_resource_exhaustion_separately() {
        let candidates = [
            candidate_with_value_domain(
                "first-incompatible",
                ToolIdentifier::Ffmpeg,
                ValueDomain::Q1_31DerivedBinary64,
                None,
            ),
            candidate_with_value_domain(
                "second-admitted",
                ToolIdentifier::Sox,
                ValueDomain::FiniteFloating,
                None,
            ),
        ];
        let requirements = CandidateRequirements {
            input_processing_domain: Some(ProcessingDomain::Binary64),
            input_value_domain: Some(ValueDomain::FiniteFloating),
            terminal_proof_required: false,
            terminal_realization_required: false,
            complete_reader_required: true,
            required_runtime_obligations: BTreeSet::new(),
            binary64_resample_preservation_required: false,
            connected_executor_required: true,
        };
        let error = select_candidate_bounded(
            &candidates,
            None,
            false,
            &requirements,
            1,
        )
        .expect_err("bounded search should report resource exhaustion");
        assert!(matches!(error, CandidateSelectionError::Resource(_)));

        let selected = select_candidate_bounded(
            &candidates[1..],
            Some(&ToolIdentifier::Sox),
            false,
            &requirements,
            0,
        )
        .expect("valid current route must bypass alternative-search accounting");
        assert_eq!(selected.identity, "second-admitted");
    }

    #[test]
    fn reference_delivery_missing_route_facts_is_need_facts_not_ready() {
        let mut request = dsd_request(SampleGainPolicy::Off);
        request.settings.dsd = DsdSettings::reference();
        let Ok(PlanningOutcome::NeedFacts(facts)) = plan_typed(&request) else {
            panic!("Reference delivery without source-kind/output-target facts must not be Ready")
        };
        assert!(facts.iter().any(|fact| fact.key == "source.dsd_source_kind"));
        assert!(facts.iter().any(|fact| fact.key == "resolved_output_target"));
    }

    #[test]
    fn reference_delivery_missing_duration_is_need_facts_not_ready() {
        let mut request = dsd_request(SampleGainPolicy::Off);
        admit_reference(&mut request, ResolvedOutputTarget::FlacNative);
        request.source.duration = None;
        let Ok(PlanningOutcome::NeedFacts(facts)) = plan_typed(&request) else {
            panic!("Reference delivery without authoritative duration must not be Ready")
        };
        assert!(facts.iter().any(|fact| fact.key == "source.duration"));
    }

    #[test]
    fn reference_delivery_known_unsupported_cells_match_shared_admission() {
        let mut sacd = dsd_request(SampleGainPolicy::Off);
        admit_reference(&mut sacd, ResolvedOutputTarget::FlacNative);
        sacd.source.dsd_source_kind = Some(DsdSourceKind::SacdTrack {
            frame_format: SacdFrameEncoding::Dsd,
            selection: SacdTrackSelection {
                area: SacdAreaKind::Stereo,
                track_index_zero_based: 0,
                start_frame: 0,
                frame_count: 1,
                toc_digest: Sha256Digest([0; 32]),
            },
        });
        let Ok(PlanningOutcome::Refused(refusal)) = plan_typed(&sacd) else {
            panic!("unqualified SACD Reference front-end must refuse")
        };
        assert_eq!(refusal.code, "reference_admission_refused");

        let mut six_channel = dsd_request(SampleGainPolicy::Off);
        admit_reference(&mut six_channel, ResolvedOutputTarget::FlacNative);
        six_channel.source.channels = Some(6);
        let Ok(PlanningOutcome::Refused(refusal)) = plan_typed(&six_channel) else {
            panic!("six-channel Reference must refuse")
        };
        assert_eq!(refusal.code, "reference_admission_refused");

        let mut int16 = dsd_request(SampleGainPolicy::Off);
        admit_reference(&mut int16, ResolvedOutputTarget::FlacNative);
        int16.settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
        let Ok(PlanningOutcome::Refused(refusal)) = plan_typed(&int16) else {
            panic!("Reference Int16 must refuse")
        };
        assert_eq!(refusal.code, "reference_admission_refused");
    }

    #[test]
    fn admitted_reference_lossless_targets_are_ready_on_common_model() {
        for target in [
            ResolvedOutputTarget::WavW64,
            ResolvedOutputTarget::WavRiff,
            ResolvedOutputTarget::WavRf64,
            ResolvedOutputTarget::FlacNative,
            ResolvedOutputTarget::WavPackNative,
        ] {
            let mut request = dsd_request(SampleGainPolicy::Off);
            admit_reference(&mut request, target);
            let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
                panic!("admitted Reference target {target:?} should remain Ready")
            };
            assert_eq!(plan.execution_capability, ExecutionCapability::ExecutableNow);
            assert!(plan.bridges.is_empty());
            assert!(plan.nodes.iter().any(|node| matches!(
                node,
                TypedPlanNode::ReferenceTerminalRealization { .. }
            )));
        }
    }

    #[test]
    fn selected_ffmpeg_int32_terminal_carries_explicit_dither_into_parameters_and_execution_identity() {
        let mut request = float64_pcm_request(SampleGainPolicy::Off);
        request.settings.target_format = AudioFormat::Flac;
        request.settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        request.settings.dither_type = DitherType::Tpdf;
        request.settings.dither_explicit = true;

        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("ordinary explicit FFmpeg Int32 dither should remain plannable")
        };
        let (candidate, resolved) = selected_pcm_terminal(&plan);
        assert_eq!(candidate.tool, Some(ToolIdentifier::Ffmpeg));
        let Some(SelectedTerminalRealization::Pcm(realization)) =
            candidate.contract.terminal_realization.as_ref()
        else {
            panic!("selected PCM terminal must carry structured realization")
        };
        assert_eq!(realization.kind, PcmTerminalRealizationKind::FfmpegDirect);
        assert_eq!(realization.input_precision, StoragePrecision::Pcm(PcmBitDepth::Float64));
        assert_eq!(realization.input_value_domain, ValueDomain::FiniteFloating);
        assert_eq!(realization.effective_dither, Some(DitherType::Tpdf));
        assert_eq!(realization.dither_owner, PcmTerminalDitherOwner::SelectedTerminal);
        match resolved {
            ResolvedOperationParameters::EncodeFlac { effective_dither, .. } => {
                assert_eq!(*effective_dither, Some(DitherType::Tpdf));
            }
            other => panic!("expected FLAC terminal parameters, got {other:?}"),
        }
        let dithered_identity =
            crate::fingerprint::common_execution_plan_fingerprint_v1(&request, &plan);

        let mut control = request.clone();
        // Int32 does not acquire automatic dither merely because a global dither
        // type is configured; the FFmpeg Int32 exception is explicit-only.
        control.settings.dither_explicit = false;
        let Ok(PlanningOutcome::Ready(control_plan)) = plan_typed(&control) else {
            panic!("undithered Int32 control should remain plannable")
        };
        let (control_candidate, control_resolved) = selected_pcm_terminal(&control_plan);
        let Some(SelectedTerminalRealization::Pcm(control_realization)) =
            control_candidate.contract.terminal_realization.as_ref()
        else {
            panic!("control PCM terminal must carry structured realization")
        };
        assert_eq!(control_realization.effective_dither, None);
        match control_resolved {
            ResolvedOperationParameters::EncodeFlac { effective_dither, .. } => {
                assert_eq!(*effective_dither, None);
            }
            other => panic!("expected FLAC terminal parameters, got {other:?}"),
        }
        assert_ne!(
            dithered_identity,
            crate::fingerprint::common_execution_plan_fingerprint_v1(&control, &control_plan),
            "selected terminal dither is execution-identity material",
        );
    }

    #[test]
    fn explicit_int32_dither_does_not_turn_sox_into_an_equivalent_terminal() {
        let mut request = float64_pcm_request(SampleGainPolicy::Off);
        request.settings.target_format = AudioFormat::Wav;
        request.output_path = PathBuf::from("out.wav");
        request.settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        request.settings.dither_type = DitherType::Tpdf;
        request.settings.dither_explicit = true;

        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("ordinary WAV Int32 explicit dither should fall back to FFmpeg")
        };
        let (candidate, _) = selected_pcm_terminal(&plan);
        assert_eq!(candidate.tool, Some(ToolIdentifier::Ffmpeg));
        let Some(SelectedTerminalRealization::Pcm(realization)) =
            candidate.contract.terminal_realization.as_ref()
        else {
            panic!("selected PCM terminal realization")
        };
        assert_eq!(realization.effective_dither, Some(DitherType::Tpdf));
        assert!(plan.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::Operation {
                operation: PlanOperation::EncodePcm { .. },
                candidates,
                ..
            } if candidates.iter().all(|candidate|
                candidate.tool != Some(ToolIdentifier::Sox)
                    || candidate.contract.terminal_realization.is_none())
        )));
    }

    #[test]
    fn certified_ffmpeg_int32_explicit_triangular_dither_follows_arch_commissioning() {
        let policies = [
            SampleGainPolicy::TruePeakGuard {
                target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
                scope: TruePeakScope::Track,
                scan: TruePeakScanTier::Standard,
            },
            SampleGainPolicy::TruePeakNormalize {
                target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
                scope: TruePeakScope::Track,
                scan: TruePeakScanTier::Standard,
            },
            SampleGainPolicy::TruePeakGuard {
                target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
                scope: TruePeakScope::Album,
                scan: TruePeakScanTier::Standard,
            },
            SampleGainPolicy::TruePeakNormalize {
                target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
                scope: TruePeakScope::Album,
                scan: TruePeakScanTier::Standard,
            },
        ];

        for (index, policy) in policies.into_iter().enumerate() {
            let mut request = float64_pcm_request(policy);
            // WAV deliberately offers both SoX and FFmpeg candidates. SoX must
            // not become a semantic escape hatch by silently dropping the
            // explicit Int32 dither request.
            request.settings.target_format = AudioFormat::Wav;
            request.output_path = PathBuf::from(format!("out-{index}.wav"));
            request.settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
            request.settings.dither_type = DitherType::Tpdf;
            request.settings.dither_explicit = true;
            if policy.scope() == Some(TruePeakScope::Album) {
                request.plan_scope = PlanScope::submitted_batch(
                    format!("int32-album-{index}"),
                    "track-1",
                    Some(1),
                );
            }
            match plan_typed(&request) {
                Ok(PlanningOutcome::Ready(_)) if ffmpeg_int32_triangular_terminal_commissioned_for_current_arch() => {}
                Ok(PlanningOutcome::Refused(refusal)) if !ffmpeg_int32_triangular_terminal_commissioned_for_current_arch() => {
                    assert_eq!(refusal.code, "no_admitted_candidate", "{policy:?}");
                }
                outcome => panic!("FFmpeg Int32 triangular admission must follow architecture commissioning for {policy:?}: {outcome:?}"),
            }
        }
    }

    #[test]
    fn forced_ffmpeg_certified_int32_explicit_triangular_dither_follows_arch_commissioning() {
        let mut request = float64_pcm_request(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Standard,
        });
        request.settings.target_format = AudioFormat::Flac;
        request.settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
        request.settings.dither_type = DitherType::Tpdf;
        request.settings.dither_explicit = true;
        let forced = CandidateSearchPolicy {
            max_alternatives_to_inspect: DEFAULT_CANDIDATE_ALTERNATIVE_LIMIT,
            forced_tool: Some(ToolIdentifier::Ffmpeg),
            strip_terminal_proof_for_tool: None,
            ssrc_binary64_evidence_override: None,
        };
        match plan_typed_with_effects_and_policy(&request, &[], &forced) {
            Ok(PlanningOutcome::Ready(_)) if ffmpeg_int32_triangular_terminal_commissioned_for_current_arch() => {}
            Ok(PlanningOutcome::Refused(refusal)) if !ffmpeg_int32_triangular_terminal_commissioned_for_current_arch() => {
                assert_eq!(refusal.code, "forced_candidate_unavailable");
            }
            outcome => panic!("forced FFmpeg Int32 triangular admission must follow architecture commissioning: {outcome:?}"),
        }
    }

    #[test]
    fn ffmpeg_int32_triangular_authority_does_not_cross_format_or_hybrid_boundaries() {
        let mut realization = SelectedPcmTerminalRealization {
            kind: PcmTerminalRealizationKind::FfmpegDirect,
            selected_tool: ToolIdentifier::Ffmpeg,
            input_precision: StoragePrecision::Pcm(PcmBitDepth::Float64),
            input_value_domain: ValueDomain::FiniteFloating,
            target_format: AudioFormat::Flac,
            target_rate_hz: Some(48_000),
            target_bit_depth: PcmBitDepth::Int32,
            wavpack_hybrid: false,
            effective_dither: Some(DitherType::Tpdf),
            ssrc_dither: None,
            dither_owner: PcmTerminalDitherOwner::SelectedTerminal,
        };
        assert!(matches_ffmpeg_int32_triangular_terminal_model(&realization));
        assert!(FFMPEG_INT32_TRIANGULAR_X86_64_COMMISSIONED);
        assert!(!FFMPEG_INT32_TRIANGULAR_AARCH64_COMMISSIONED);
        assert_eq!(
            is_qualified_ffmpeg_int32_triangular_terminal(&realization),
            ffmpeg_int32_triangular_terminal_commissioned_for_current_arch()
        );

        realization.target_format = AudioFormat::Alac;
        assert!(!matches_ffmpeg_int32_triangular_terminal_model(&realization));

        realization.target_format = AudioFormat::WavPack;
        realization.wavpack_hybrid = true;
        assert!(!matches_ffmpeg_int32_triangular_terminal_model(&realization));
    }

    #[test]
    fn certified_ffmpeg_int32_unqualified_dither_modes_remain_refused() {
        for (index, (dither, expected_code)) in [
            (DitherType::SlopedTpdf, "forced_candidate_missing"),
            (DitherType::Shibata, "forced_candidate_unavailable"),
        ]
        .into_iter()
        .enumerate()
        {
            let policy = if index == 0 {
                SampleGainPolicy::TruePeakGuard {
                    target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
                    scope: TruePeakScope::Track,
                    scan: TruePeakScanTier::Standard,
                }
            } else {
                SampleGainPolicy::TruePeakNormalize {
                    target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
                    scope: TruePeakScope::Album,
                    scan: TruePeakScanTier::Standard,
                }
            };
            let mut request = float64_pcm_request(policy);
            request.settings.target_format = AudioFormat::Flac;
            request.output_path = PathBuf::from(format!("unsupported-{index}.flac"));
            request.settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int32);
            request.settings.dither_type = dither;
            request.settings.dither_explicit = true;
            if policy.scope() == Some(TruePeakScope::Album) {
                request.plan_scope = PlanScope::submitted_batch(
                    format!("unsupported-int32-album-{index}"),
                    "track-1",
                    Some(1),
                );
            }
            let forced = CandidateSearchPolicy {
                max_alternatives_to_inspect: DEFAULT_CANDIDATE_ALTERNATIVE_LIMIT,
                forced_tool: Some(ToolIdentifier::Ffmpeg),
                strip_terminal_proof_for_tool: None,
                ssrc_binary64_evidence_override: None,
            };
            let Ok(PlanningOutcome::Refused(refusal)) =
                plan_typed_with_effects_and_policy(&request, &[], &forced)
            else {
                panic!("unqualified FFmpeg Int32 dither mode must remain refused: {dither:?}")
            };
            assert_eq!(refusal.code, expected_code);
        }
    }

    #[test]
    fn actual_planner_p01_falls_back_from_unproved_preferred_candidate_and_force_refuses() {
        let mut request = pcm_request(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Standard,
        });
        request.settings.preferred_tool = PreferredTool::Sox;

        let search = CandidateSearchPolicy {
            max_alternatives_to_inspect: DEFAULT_CANDIDATE_ALTERNATIVE_LIMIT,
            forced_tool: None,
            strip_terminal_proof_for_tool: Some(ToolIdentifier::Sox),
            ssrc_binary64_evidence_override: None,
        };
        let Ok(PlanningOutcome::Ready(plan)) =
            plan_typed_with_effects_and_policy(&request, &[], &search)
        else {
            panic!("an admitted alternate terminal candidate should win")
        };
        let selected_tool = plan.nodes.iter().find_map(|node| match node {
            TypedPlanNode::Operation {
                operation: PlanOperation::EncodePcm { .. },
                candidates,
                selected_candidate,
                ..
            } => candidates.get(*selected_candidate).and_then(|candidate| candidate.tool.clone()),
            _ => None,
        });
        assert!(selected_tool.is_some());
        assert_ne!(selected_tool, Some(ToolIdentifier::Sox));

        let forced = CandidateSearchPolicy {
            max_alternatives_to_inspect: DEFAULT_CANDIDATE_ALTERNATIVE_LIMIT,
            forced_tool: Some(ToolIdentifier::Sox),
            strip_terminal_proof_for_tool: Some(ToolIdentifier::Sox),
            ssrc_binary64_evidence_override: None,
        };
        let Ok(PlanningOutcome::Refused(refusal)) =
            plan_typed_with_effects_and_policy(&request, &[], &forced)
        else {
            panic!("forcing the unproved candidate must refuse")
        };
        assert_eq!(refusal.code, "forced_candidate_unavailable");
    }

    #[test]
    fn pcm_hard_ceiling_rate_change_admits_only_float_overload_preserving_resampler() {
        let mut request = pcm_request(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Standard,
        });
        request.source.sample_rate_hz = Some(96_000);
        request.settings.target_sample_rate = RateTarget::PcmHz(44_100);
        request.settings.preferred_tool = PreferredTool::Sox;

        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("an admitted FFmpeg/soxr alternate should keep the semantic route plannable")
        };
        let (selected_tool, selected_contract) = plan
            .nodes
            .iter()
            .find_map(|node| match node {
                TypedPlanNode::Operation {
                    operation: PlanOperation::ResamplePcm { .. },
                    candidates,
                    selected_candidate,
                    ..
                } => candidates
                    .get(*selected_candidate)
                    .map(|candidate| (candidate.tool.clone(), candidate.contract.clone())),
                _ => None,
            })
            .expect("rate-changing hard-ceiling plan must select a resampler candidate");
        assert_eq!(selected_tool, Some(ToolIdentifier::Ffmpeg));
        assert!(selected_contract
            .runtime_obligations
            .contains("preserve_floating_overload_before_true_peak"));
        assert_eq!(
            plan.execution_capability,
            ExecutionCapability::RequiresPhase3CommonRealizer,
            "the retained explicit-SoX bridge cannot silently execute the admitted FFmpeg alternate",
        );

        request.settings.ssrc.force = true;
        let Ok(PlanningOutcome::Refused(refusal)) = plan_typed(&request) else {
            panic!("forcing SSRC must refuse while the exact strong-preservation cell remains uncommissioned")
        };
        assert_eq!(refusal.code, "binary64_resample_preservation_pending");
    }

    #[test]
    fn injected_established_ssrc_binary64_evidence_admits_only_exact_rate_pair() {
        use crate::ssrc_binary64::{
            Binary64ResampleEvidenceScope, Binary64ResamplePreservationEvidence,
            Binary64RuntimeAttestation, TONEPOET_BINARY64_OVERLOAD_PRESERVING_RESAMPLE_V1,
        };

        fn scope(source_rate_hz: u32) -> Binary64ResampleEvidenceScope {
            Binary64ResampleEvidenceScope {
                profile: SsrcProfile::High,
                source_rate_hz,
                target_rate_hz: 44_100,
                attenuation_db: Some("0.0".to_owned()),
                min_phase: false,
                architecture: std::env::consts::ARCH.to_owned(),
                input_container: "wav".to_owned(),
                input_sample_format: "pcm_f64le".to_owned(),
                output_container: "w64".to_owned(),
                output_sample_format: "pcm_f64le".to_owned(),
            }
        }

        fn established(
            scope: Binary64ResampleEvidenceScope,
        ) -> Binary64ResamplePreservationEvidence {
            Binary64ResamplePreservationEvidence::Established {
                authority_id: TONEPOET_BINARY64_OVERLOAD_PRESERVING_RESAMPLE_V1.to_owned(),
                evidence_id: format!(
                    "test:ssrc-binary64-established:{}-{}",
                    scope.source_rate_hz, scope.target_rate_hz
                ),
                qualification_report_sha256: "test-report-sha256".to_owned(),
                runtime_attestation: Binary64RuntimeAttestation {
                    expected_executable_sha256: "test-executable-sha256".to_owned(),
                    architecture: scope.architecture.clone(),
                    source_revision: crate::ssrc_binary64::PINNED_SSRC_SOURCE_REV.to_owned(),
                    build_identity: "test-build-identity".to_owned(),
                },
                scope,
            }
        }

        fn policy(scope: Binary64ResampleEvidenceScope) -> CandidateSearchPolicy {
            CandidateSearchPolicy {
                max_alternatives_to_inspect: DEFAULT_CANDIDATE_ALTERNATIVE_LIMIT,
                forced_tool: None,
                strip_terminal_proof_for_tool: None,
                ssrc_binary64_evidence_override: Some(TestBinary64SsrcEvidenceOverride {
                    scope: scope.clone(),
                    evidence: established(scope),
                }),
            }
        }

        fn request(source_rate_hz: u32) -> PlanRequest {
            let mut request = pcm_request(SampleGainPolicy::TruePeakGuard {
                target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
                scope: TruePeakScope::Track,
                scan: TruePeakScanTier::Standard,
            });
            request.source.sample_rate_hz = Some(source_rate_hz);
            request.source.frame_extent = Some(crate::source::SourceFrameExtent::Exact(
                u64::from(source_rate_hz) * 60,
            ));
            request.settings.target_sample_rate = RateTarget::PcmHz(44_100);
            request.settings.preferred_tool = PreferredTool::Ssrc;
            request.settings.ssrc.force = true;
            request.settings.ssrc.profile = Some(SsrcProfile::High);
            request
        }

        let exact_scope = scope(96_000);
        let Ok(PlanningOutcome::Ready(plan)) =
            plan_typed_with_effects_and_policy(&request(96_000), &[], &policy(exact_scope.clone()))
        else {
            panic!("exact 96 kHz -> 44.1 kHz Established fixture should admit the protected SSRC cell")
        };

        let (output_signal, candidate, resolved) = plan
            .nodes
            .iter()
            .find_map(|node| match node {
                TypedPlanNode::Operation {
                    operation: PlanOperation::ResamplePcm { .. },
                    output_signal: Some(output_signal),
                    candidates,
                    selected_candidate,
                    resolved_parameters,
                    ..
                } if candidates
                    .get(*selected_candidate)
                    .and_then(|candidate| candidate.tool.as_ref())
                    == Some(&ToolIdentifier::Ssrc) => candidates
                    .get(*selected_candidate)
                    .map(|candidate| (*output_signal, candidate, resolved_parameters)),
                _ => None,
            })
            .expect("protected plan must select the exact SSRC resampler cell");

        assert!(matches!(
            candidate.binary64_resample_preservation_evidence.as_ref(),
            Some(Binary64ResamplePreservationEvidence::Established { scope, .. })
                if scope == &exact_scope
        ));
        assert!(candidate.protected_float64_ingress_authority.is_some());
        assert_eq!(
            candidate.contract.representation.emitted_processing_domain,
            Some(ProcessingDomain::Binary64),
        );
        assert!(matches!(
            resolved,
            ResolvedOperationParameters::ResampleSsrc {
                effective_profile: SsrcProfile::High,
                effective_output_depth: PcmBitDepth::Float64,
                output_role: SsrcOutputRole::Nonterminal,
                computation_precision: SsrcComputationPrecision::Double,
                emitted_processing_domain: ProcessingDomain::Binary64,
                ..
            }
        ));

        let output_state = plan
            .audio_states
            .iter()
            .find(|state| state.id == output_signal)
            .expect("selected resampler output must have a typed state");
        assert_eq!(output_state.precision, StoragePrecision::Pcm(PcmBitDepth::Float64));
        assert_eq!(output_state.processing_domain, Fact::Known(ProcessingDomain::Binary64));
        assert_eq!(output_state.value_domain, ValueDomain::FiniteFloating);
        assert!(output_state
            .claims
            .contains(&Claim::OverloadPreservedUntil(output_signal)));

        let post_effect = EffectIntent {
            id: EffectInstanceId(77),
            effect: RegisteredUnaryEffect::FfmpegLowPass { frequency_hz: 20_000 },
            after: Vec::new(),
            placement: EffectPlacement::AfterPcmResample,
        };
        let Ok(PlanningOutcome::Refused(refusal)) = plan_typed_with_effects_and_policy(
            &request(96_000),
            &[post_effect],
            &policy(exact_scope.clone()),
        ) else {
            panic!("an unqualified post-resample effect must break the protected observation chain")
        };
        assert_eq!(refusal.code, "protected_post_resample_effect_unqualified");

        let pre_effect = EffectIntent {
            id: EffectInstanceId(78),
            effect: RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
            after: Vec::new(),
            placement: EffectPlacement::BeforePcmResample,
        };
        let Ok(PlanningOutcome::Refused(refusal)) = plan_typed_with_effects_and_policy(
            &request(96_000),
            &[pre_effect],
            &policy(exact_scope.clone()),
        ) else {
            panic!("an unqualified pre-resample effect must not acquire protected SSRC ingress authority")
        };
        assert_eq!(refusal.code, "protected_float64_ingress_unavailable");

        for source_rate_hz in [48_000, 192_000] {
            let Ok(PlanningOutcome::Refused(refusal)) = plan_typed_with_effects_and_policy(
                &request(source_rate_hz),
                &[],
                &policy(exact_scope.clone()),
            ) else {
                panic!(
                    "96 kHz evidence must not admit {source_rate_hz} Hz -> 44.1 kHz"
                )
            };
            assert_eq!(refusal.code, "binary64_resample_preservation_pending");
        }

        let mut wrong_target_scope = exact_scope.clone();
        wrong_target_scope.target_rate_hz = 48_000;
        let Ok(PlanningOutcome::Refused(refusal)) = plan_typed_with_effects_and_policy(
            &request(96_000),
            &[],
            &policy(wrong_target_scope),
        ) else {
            panic!("96 kHz -> 48 kHz evidence must not admit 96 kHz -> 44.1 kHz")
        };
        assert_eq!(refusal.code, "binary64_resample_preservation_pending");

        let scope_48k = scope(48_000);
        let Ok(PlanningOutcome::Ready(plan_48k)) = plan_typed_with_effects_and_policy(
            &request(48_000),
            &[],
            &policy(scope_48k.clone()),
        ) else {
            panic!("separately Established 48 kHz -> 44.1 kHz evidence should admit that pair")
        };
        assert!(plan_48k.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::Operation {
                operation: PlanOperation::ResamplePcm { .. },
                candidates,
                selected_candidate,
                ..
            } if matches!(
                candidates
                    .get(*selected_candidate)
                    .and_then(|candidate| candidate.binary64_resample_preservation_evidence.as_ref()),
                Some(Binary64ResamplePreservationEvidence::Established { scope, .. })
                    if scope == &scope_48k
            )
        )));
    }

    #[test]
    fn protected_float64_riff_capacity_is_preselection_admission_only_for_strong_ssrc() {
        use crate::ssrc_binary64::{
            Binary64ResampleEvidenceScope, Binary64ResamplePreservationEvidence,
            Binary64RuntimeAttestation, TONEPOET_BINARY64_OVERLOAD_PRESERVING_RESAMPLE_V1,
        };

        let scope = Binary64ResampleEvidenceScope {
            profile: SsrcProfile::High,
            source_rate_hz: 96_000,
            target_rate_hz: 44_100,
            attenuation_db: Some("0.0".to_owned()),
            min_phase: false,
            architecture: std::env::consts::ARCH.to_owned(),
            input_container: "wav".to_owned(),
            input_sample_format: "pcm_f64le".to_owned(),
            output_container: "w64".to_owned(),
            output_sample_format: "pcm_f64le".to_owned(),
        };
        let evidence = Binary64ResamplePreservationEvidence::Established {
            authority_id: TONEPOET_BINARY64_OVERLOAD_PRESERVING_RESAMPLE_V1.to_owned(),
            evidence_id: "test:ssrc-binary64-capacity".to_owned(),
            qualification_report_sha256: "test-report-sha256".to_owned(),
            runtime_attestation: Binary64RuntimeAttestation {
                expected_executable_sha256: "test-executable-sha256".to_owned(),
                architecture: scope.architecture.clone(),
                source_revision: crate::ssrc_binary64::PINNED_SSRC_SOURCE_REV.to_owned(),
                build_identity: "test-build-identity".to_owned(),
            },
            scope: scope.clone(),
        };
        let policy = CandidateSearchPolicy {
            max_alternatives_to_inspect: DEFAULT_CANDIDATE_ALTERNATIVE_LIMIT,
            forced_tool: None,
            strip_terminal_proof_for_tool: None,
            ssrc_binary64_evidence_override: Some(TestBinary64SsrcEvidenceOverride {
                scope: scope.clone(),
                evidence,
            }),
        };
        let strong_request = |
            frame_extent: Option<crate::source::SourceFrameExtent>,
            duration: Option<Duration>,
            force_ssrc: bool,
        | {
            let mut request = pcm_request(SampleGainPolicy::TruePeakGuard {
                target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
                scope: TruePeakScope::Track,
                scan: TruePeakScanTier::Standard,
            });
            request.source.sample_rate_hz = Some(96_000);
            request.source.frame_extent = frame_extent;
            request.source.duration = duration;
            request.settings.target_sample_rate = RateTarget::PcmHz(44_100);
            request.settings.preferred_tool = if force_ssrc {
                PreferredTool::Ssrc
            } else {
                PreferredTool::Auto
            };
            request.settings.ssrc.force = force_ssrc;
            request.settings.ssrc.profile = Some(SsrcProfile::High);
            request
        };
        let one_minute_frames = 96_000_u64 * 60;
        let one_hour_frames = one_minute_frames * 60;

        let Ok(PlanningOutcome::Ready(below_bound)) = plan_typed_with_effects_and_policy(
            &strong_request(
                Some(crate::source::SourceFrameExtent::Exact(one_minute_frames)),
                Some(Duration::from_secs(60)),
                true,
            ),
            &[],
            &policy,
        ) else {
            panic!("clearly below-bound protected RIFF ingress should remain admissible")
        };
        assert!(below_bound.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::Operation {
                operation: PlanOperation::ResamplePcm { .. },
                candidates,
                selected_candidate,
                ..
            } if candidates
                .get(*selected_candidate)
                .and_then(|candidate| candidate.protected_float64_ingress_authority.as_ref())
                .is_some()
        )));

        let Ok(PlanningOutcome::Refused(over_forced)) = plan_typed_with_effects_and_policy(
            &strong_request(
                Some(crate::source::SourceFrameExtent::Exact(one_hour_frames)),
                Some(Duration::from_secs(60 * 60)),
                true,
            ),
            &[],
            &policy,
        ) else {
            panic!("forced strong SSRC must refuse an over-capacity RIFF carrier before execution")
        };
        assert_eq!(over_forced.code, "protected_float64_ingress_unavailable");

        let Ok(PlanningOutcome::Refused(unknown_forced)) = plan_typed_with_effects_and_policy(
            &strong_request(None, None, true),
            &[],
            &policy,
        ) else {
            panic!("forced strong SSRC must fail closed when protected RIFF capacity cannot be established")
        };
        assert_eq!(unknown_forced.code, "protected_float64_ingress_unavailable");

        let Ok(PlanningOutcome::Refused(estimated_only_forced)) =
            plan_typed_with_effects_and_policy(
                &strong_request(None, Some(Duration::from_secs(60)), true),
                &[],
                &policy,
            )
        else {
            panic!("duration estimate alone must not prove protected RIFF capacity")
        };
        assert_eq!(
            estimated_only_forced.code,
            "protected_float64_ingress_unavailable"
        );

        let Ok(PlanningOutcome::Ready(auto_over_bound)) = plan_typed_with_effects_and_policy(
            &strong_request(
                Some(crate::source::SourceFrameExtent::Exact(one_hour_frames)),
                Some(Duration::from_secs(60 * 60)),
                false,
            ),
            &[],
            &policy,
        ) else {
            panic!("Auto should keep an over-capacity request plannable with another Established candidate")
        };
        let (selected_tool, ssrc_ingress_authority) = auto_over_bound
            .nodes
            .iter()
            .find_map(|node| match node {
                TypedPlanNode::Operation {
                    operation: PlanOperation::ResamplePcm { .. },
                    candidates,
                    selected_candidate,
                    ..
                } => Some((
                    candidates
                        .get(*selected_candidate)
                        .and_then(|candidate| candidate.tool.clone()),
                    candidates
                        .iter()
                        .find(|candidate| candidate.tool == Some(ToolIdentifier::Ssrc))
                        .and_then(|candidate| candidate.protected_float64_ingress_authority.as_ref()),
                )),
                _ => None,
            })
            .expect("rate-change node must exist");
        assert_eq!(selected_tool, Some(ToolIdentifier::Ffmpeg));
        assert!(ssrc_ingress_authority.is_none());

        let mut ordinary = pcm_request(SampleGainPolicy::Off);
        ordinary.source.sample_rate_hz = Some(96_000);
        ordinary.source.duration = Some(Duration::from_secs(60 * 60));
        ordinary.settings.target_sample_rate = RateTarget::PcmHz(44_100);
        ordinary.settings.preferred_tool = PreferredTool::Ssrc;
        ordinary.settings.ssrc.force = true;
        ordinary.settings.ssrc.profile = Some(SsrcProfile::High);
        let Ok(PlanningOutcome::Ready(ordinary_plan)) = plan_typed(&ordinary) else {
            panic!("ordinary SSRC must not inherit the strong-contract RIFF capacity limit")
        };
        assert!(ordinary_plan.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::Operation {
                operation: PlanOperation::ResamplePcm { .. },
                candidates,
                selected_candidate,
                ..
            } if candidates
                .get(*selected_candidate)
                .and_then(|candidate| candidate.tool.as_ref())
                == Some(&ToolIdentifier::Ssrc)
        )));
    }

    #[test]
    fn forced_single_precision_ssrc_hard_ceiling_is_refuted_not_pending() {
        let mut request = pcm_request(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Standard,
        });
        request.source.sample_rate_hz = Some(96_000);
        request.settings.target_sample_rate = RateTarget::PcmHz(44_100);
        request.settings.preferred_tool = PreferredTool::Ssrc;
        request.settings.ssrc.force = true;
        request.settings.ssrc.profile = Some(SsrcProfile::Standard);

        let Ok(PlanningOutcome::Refused(refusal)) = plan_typed(&request) else {
            panic!("single-precision SSRC cannot satisfy the strong Binary64 contract")
        };
        assert_eq!(refusal.code, "binary64_resample_preservation_refuted");
    }

    #[test]
    fn actual_planner_p02_reports_bounded_search_exhaustion_as_resource_limit() {
        let mut request = pcm_request(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Standard,
        });
        request.settings.preferred_tool = PreferredTool::Sox;
        let search = CandidateSearchPolicy {
            max_alternatives_to_inspect: 0,
            forced_tool: None,
            strip_terminal_proof_for_tool: Some(ToolIdentifier::Sox),
            ssrc_binary64_evidence_override: None,
        };
        let error = plan_typed_with_effects_and_policy(&request, &[], &search)
            .expect_err("bounded alternative search must return PlanningResourceLimit");
        assert_eq!(error.resource, "physical_candidate_search");
        assert_eq!(error.limit, 0);
        assert!(error.requested >= 1);
    }

    #[test]
    fn admitted_ordinary_current_route_retains_backend_and_command_plan() {
        let mut request = pcm_request(SampleGainPolicy::Off);
        request.settings.force_encode = true;
        request.settings.metadata.transfer_tags = false;
        request.settings.metadata.preserve_artwork = false;
        request.settings.metadata.store_source_audio_md5 = false;
        request.settings.verification.verify_after_encode = false;
        request.settings.replay_gain.mode = None;

        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("ordinary admitted request should remain Ready")
        };
        assert_eq!(plan.execution_capability, ExecutionCapability::ExecutableNow);
        let selected_tool = plan
            .nodes
            .iter()
            .find_map(|node| match node {
                TypedPlanNode::Operation {
                    operation: PlanOperation::EncodePcm { .. },
                    candidates,
                    selected_candidate,
                    ..
                } => candidates
                    .get(*selected_candidate)
                    .and_then(|candidate| candidate.tool.clone()),
                _ => None,
            })
            .expect("typed terminal candidate");

        let retained = crate::plan::plan_conversion(&request)
            .expect("retained command planner should admit the same ordinary request");
        let commands = retained.commands();
        assert_eq!(commands.len(), 1, "test request deliberately disables metadata/verification side commands");
        assert_eq!(commands[0].tool, selected_tool);
    }

    #[test]
    fn dsd_track_certified_route_binds_connected_reader_observation_and_terminal_proof() {
        let request = dsd_request(SampleGainPolicy::TruePeakNormalize {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Reference,
        });
        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("DSD Track semantic route should be admitted")
        };
        assert_eq!(plan.execution_capability, ExecutionCapability::ExecutableByPhase3CommonRealizer);
        let observation = true_peak_observation(&plan);
        assert!(observation.complete_reader_required);
        let observed_state = plan
            .audio_states
            .iter()
            .find(|state| state.id == observation.subject)
            .expect("certified observation subject must be a named audio state");
        assert_eq!(observed_state.precision, StoragePrecision::Pcm(PcmBitDepth::Float64));
        assert_eq!(
            observed_state.processing_domain,
            Fact::Known(ProcessingDomain::Binary64),
        );
        assert_eq!(observed_state.value_domain, ValueDomain::FiniteFloating);
        let terminal = plan.nodes.iter().find_map(|node| match node {
            TypedPlanNode::Operation {
                operation: PlanOperation::EncodePcm { .. },
                candidates,
                selected_candidate,
                ..
            } => candidates.get(*selected_candidate),
            _ => None,
        }).expect("DSD Track plan must select its terminal candidate");
        assert!(terminal.executable, "the ordinary terminal backend itself is connected");
        assert!(terminal.contract.runtime_obligations.contains("complete_reader"));
        assert!(terminal.contract.runtime_obligations.contains("certified_true_peak_observation"));
        assert!(terminal.contract.runtime_obligations.contains(&format!(
            "observation_reader:{}", observation.read_contract.authority
        )));
        assert!(terminal.contract.terminal_proof.is_some());
        assert!(observation.read_contract.complete_reader);
        assert!(observation.read_contract.connected_executor);
        assert!(observation
            .read_contract
            .accepted_processing_domains
            .contains(&ProcessingDomain::Binary64));
        assert!(observation
            .read_contract
            .accepted_value_domains
            .contains(&ValueDomain::FiniteFloating));
        assert!(require_current_executor(&plan).is_err());
    }

    #[test]
    fn album_true_peak_observation_keys_are_submission_scoped_and_cross_scope_rejected() {
        let policy = SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Standard,
        };
        let mut batch_a = dsd_request(policy);
        batch_a.plan_scope = PlanScope::submitted_batch("album-a", "track-1", Some(1));
        let mut batch_b = batch_a.clone();
        batch_b.plan_scope = PlanScope::submitted_batch("album-b", "track-1", Some(1));

        let Ok(PlanningOutcome::Ready(plan_a)) = plan_typed(&batch_a) else {
            panic!("album A should be ready")
        };
        let Ok(PlanningOutcome::Ready(plan_b)) = plan_typed(&batch_b) else {
            panic!("album B should be ready")
        };
        let observation_a = true_peak_observation(&plan_a);
        let observation_b = true_peak_observation(&plan_b);
        assert_eq!(observation_a.id, ObservationId(0));
        assert_eq!(observation_b.id, ObservationId(0));
        assert_ne!(observation_a.scope, observation_b.scope);

        let decision_a = true_peak_decision(&plan_a);
        let decision_b = true_peak_decision(&plan_b);
        assert!(observation_satisfies_dependency(
            observation_a,
            &decision_a.observations[0]
        ));
        assert!(!observation_satisfies_dependency(
            observation_a,
            &decision_b.observations[0]
        ));

        let wrong_purpose = ScopedObservationId {
            scope: observation_a.scope.clone(),
            participant: observation_a.participant.clone(),
            observation: observation_a.id,
            purpose: ObservationClass::Loudness,
        };
        assert!(!observation_satisfies_dependency(observation_a, &wrong_purpose));

        assert_ne!(
            crate::fingerprint::common_semantic_plan_fingerprint_v1(&batch_a, &plan_a),
            crate::fingerprint::common_semantic_plan_fingerprint_v1(&batch_b, &plan_b),
            "changing submitted-batch ownership must invalidate a previously bound album scalar",
        );
        assert_eq!(observation_a.subject, observation_b.subject);
        assert_eq!(observation_a.kind, observation_b.kind);
        assert!(observation_result_reusable_for_rebind(
            observation_a,
            observation_b,
        ));
    }

    #[test]
    fn album_observation_keys_include_participant_within_one_submission() {
        let policy = SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Standard,
        };
        let mut first = dsd_request(policy);
        first.plan_scope = PlanScope::submitted_batch("album-shared", "track-1", Some(2));
        let mut second = first.clone();
        second.plan_scope = PlanScope::submitted_batch("album-shared", "track-2", Some(2));

        let Ok(PlanningOutcome::Ready(first_plan)) = plan_typed(&first) else {
            panic!("first album participant should be ready")
        };
        let Ok(PlanningOutcome::Ready(second_plan)) = plan_typed(&second) else {
            panic!("second album participant should be ready")
        };
        let first_observation = true_peak_observation(&first_plan);
        let second_observation = true_peak_observation(&second_plan);
        assert_eq!(first_observation.scope, second_observation.scope);
        assert_eq!(first_observation.id, second_observation.id);
        assert_ne!(first_observation.participant, second_observation.participant);
        let first_decision = true_peak_decision(&first_plan);
        let second_decision = true_peak_decision(&second_plan);
        assert!(observation_satisfies_dependency(
            first_observation,
            &first_decision.observations[0],
        ));
        assert!(!observation_satisfies_dependency(
            first_observation,
            &second_decision.observations[0],
        ));
        assert!(!observation_result_reusable_for_rebind(
            first_observation,
            second_observation,
        ));
    }

    #[test]
    fn album_true_peak_requires_persisted_submission_participant_count() {
        let mut request = dsd_request(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Standard,
        });
        request.plan_scope = PlanScope::submitted_batch("album-missing-count", "track-1", None);
        let Ok(PlanningOutcome::Refused(refusal)) = plan_typed(&request) else {
            panic!("Album scope without the persisted cohort size must not be Ready")
        };
        assert_eq!(refusal.code, "album_scope_requires_participant_count");
    }

    #[test]
    fn resolved_album_gain_continuation_does_not_reopen_the_submission_barrier() {
        let resolved_gain: DbNano = "-0.375000000".parse().expect("resolved album gain");
        let mut request = pcm_request(SampleGainPolicy::TruePeakNormalize {
            target_dbtp: "-0.500000000".parse().expect("target"),
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Standard,
        });
        request
            .settings
            .pcm_true_peak
            .bind_runtime_album_gain(resolved_gain);
        request.plan_scope = PlanScope::track("resolved-album-carrier");

        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("a coordinator-resolved Album continuation must be Ready without a second submitted-batch barrier")
        };
        assert!(plan.nodes.iter().all(|node| !matches!(
            node,
            TypedPlanNode::Observe(observation)
                if matches!(observation.kind, ObservationKind::CertifiedTruePeak { .. })
        )));
        assert!(plan.nodes.iter().all(|node| !matches!(
            node,
            TypedPlanNode::Decide(Decision {
                kind: DecisionKind::TruePeakGain { .. },
                ..
            })
        )));
        assert!(plan.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::ApplyGain {
                policy: SampleGainPolicy::FixedGain { gain_db },
                decision: None,
                ..
            } if *gain_db == resolved_gain
        )));
    }

    #[test]
    fn one_track_album_keeps_group_binding_with_track_equivalent_policy_and_terminal_proof() {
        let policy = SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Standard,
        };
        let mut album_request = dsd_request(policy);
        album_request.plan_scope = PlanScope::submitted_batch("album-one", "track-1", Some(1));
        let Ok(PlanningOutcome::Ready(album_plan)) = plan_typed(&album_request) else {
            panic!("one-track Album plan should be Ready")
        };

        let mut track_request = album_request.clone();
        track_request.settings.dsd.set_gain_policy(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Standard,
        });
        let Ok(PlanningOutcome::Ready(track_plan)) = plan_typed(&track_request) else {
            panic!("Track plan should be Ready")
        };

        let album_decision = true_peak_decision(&album_plan);
        let track_decision = true_peak_decision(&track_plan);
        let (album_target, album_boost, album_proof) = match &album_decision.kind {
            DecisionKind::TruePeakGain {
                target_dbtp,
                allow_boost,
                binding: GainDecisionBinding::SubmittedBatch { .. },
                album_participant: Some(participant),
                ..
            } => (*target_dbtp, *allow_boost, participant.terminal_proof.clone()),
            _ => panic!("one-track Album must remain group-bound"),
        };
        let (track_target, track_boost) = match &track_decision.kind {
            DecisionKind::TruePeakGain {
                target_dbtp,
                allow_boost,
                binding: GainDecisionBinding::Track { .. },
                ..
            } => (*target_dbtp, *allow_boost),
            _ => panic!("Track must remain track-bound"),
        };
        assert_eq!(album_target, track_target);
        assert_eq!(album_boost, track_boost);
        let album_proof = album_proof.expect("Album participant terminal proof");
        let track_terminal_proof = track_plan.nodes.iter().find_map(|node| match node {
            TypedPlanNode::Operation { candidates, selected_candidate, .. } => candidates
                .get(*selected_candidate)
                .and_then(|candidate| candidate.contract.terminal_proof.clone()),
            _ => None,
        }).expect("Track terminal proof");
        assert_eq!(album_proof, track_terminal_proof);
    }

    #[test]
    fn album_barrier_is_keyed_to_the_same_submission_and_participant_contract() {
        let mut request = dsd_request(SampleGainPolicy::TruePeakNormalize {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Fast,
        });
        request.plan_scope = PlanScope::submitted_batch("album-keyed", "track-7", Some(3));
        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("album plan should be ready")
        };
        let decision = true_peak_decision(&plan);
        let DecisionKind::TruePeakGain {
            binding: GainDecisionBinding::SubmittedBatch {
                scope,
                participant,
                expected_participants,
            },
            album_participant: Some(album_input),
            ..
        } = &decision.kind
        else {
            panic!("album gain must carry submitted-batch binding")
        };
        assert_eq!(scope.0, "submission:album-keyed");
        assert_eq!(participant.0, "track-7");
        assert_eq!(*expected_participants, Some(3));
        assert_eq!(album_input.scope, *scope);
        assert_eq!(album_input.participant, *participant);
        assert_eq!(album_input.expected_participants, *expected_participants);
        assert_eq!(album_input.observation, decision.observations[0]);
        assert!(album_input.terminal_proof.is_some());
        assert!(plan.audio_states.iter().any(|state| {
            state.id == album_input.terminal_subject
                && state
                    .obligations
                    .contains(&RuntimeObligation::TerminalErrorBound(album_input.terminal_subject))
        }));
        assert!(plan.audio_states.iter().any(|state| state.obligations.contains(
            &RuntimeObligation::AlbumParticipantBarrier {
                scope: scope.clone(),
                participant: participant.clone(),
                expected_participants: *expected_participants,
            }
        )));
    }

    #[test]
    fn track_true_peak_has_no_album_barrier_even_inside_a_submitted_batch() {
        let mut request = dsd_request(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Fast,
        });
        request.plan_scope = PlanScope::submitted_batch("album-present", "track-2", Some(4));
        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("track-scoped plan should be ready")
        };
        assert!(plan.audio_states.iter().all(|state| state.obligations.iter().all(|obligation| {
            !matches!(obligation, RuntimeObligation::AlbumParticipantBarrier { .. })
        })));
        let decision = true_peak_decision(&plan);
        assert!(matches!(
            decision.kind,
            DecisionKind::TruePeakGain {
                binding: GainDecisionBinding::Track { .. },
                album_participant: None,
                ..
            }
        ));
    }

    #[test]
    fn replaygain_album_observation_uses_the_same_submitted_batch_scope_authority() {
        let mut request = dsd_request(SampleGainPolicy::Off);
        request.settings.replay_gain.mode = Some(ReplayGainMode::Album);
        request.plan_scope = PlanScope::submitted_batch("rg-album", "track-3", Some(5));
        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("ReplayGain album plan should be ready")
        };
        let observation = plan.nodes.iter().find_map(|node| match node {
            TypedPlanNode::Observe(observation)
                if matches!(observation.kind, ObservationKind::ReplayGain { .. }) => Some(observation),
            _ => None,
        }).expect("ReplayGain observation");
        let decision = plan.nodes.iter().find_map(|node| match node {
            TypedPlanNode::Decide(decision)
                if matches!(decision.kind, DecisionKind::ReplayGainProjection { .. }) => Some(decision),
            _ => None,
        }).expect("ReplayGain decision");
        assert_eq!(observation.scope.0, "submission:rg-album");
        assert!(observation_satisfies_dependency(observation, &decision.observations[0]));
        assert!(matches!(
            &decision.kind,
            DecisionKind::ReplayGainProjection {
                group: ReplayGainGroupBinding::SubmittedBatch {
                    scope,
                    participant,
                    expected_participants: Some(5),
                },
                ..
            } if scope.0 == "submission:rg-album" && participant.0 == "track-3"
        ));
    }

    #[test]
    fn standalone_album_replaygain_is_a_complete_singleton_group() {
        let mut request = pcm_request(SampleGainPolicy::Off);
        request.settings.replay_gain.mode = Some(ReplayGainMode::Both);
        request.plan_scope = PlanScope::track("singleton-rg");

        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("standalone Album/Both ReplayGain must have singleton-group semantics")
        };
        let decision = plan.nodes.iter().find_map(|node| match node {
            TypedPlanNode::Decide(decision)
                if matches!(decision.kind, DecisionKind::ReplayGainProjection { .. }) => {
                    Some(decision)
                }
            _ => None,
        }).expect("ReplayGain decision");
        assert!(matches!(
            &decision.kind,
            DecisionKind::ReplayGainProjection {
                policy: ReplayGainProjectionPolicy {
                    mode: ReplayGainMode::Both,
                    ..
                },
                group: ReplayGainGroupBinding::Track { scope },
            } if scope.0 == "track:singleton-rg"
        ));
    }

    #[test]
    fn standalone_dsd_track_true_peak_with_album_replaygain_remains_ready() {
        let mut request = dsd_request(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Fast,
        });
        request.plan_scope = PlanScope::track("standalone-dsd-track");
        request.settings.target_sample_rate = RateTarget::PcmHz(44_100);
        request.settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int16);
        request.settings.replay_gain.mode = Some(ReplayGainMode::Both);

        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!(
                "track true-peak and singleton Album/Both ReplayGain must coexist without submitted-batch authority"
            )
        };

        assert!(plan.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::Decide(Decision {
                kind: DecisionKind::TruePeakGain {
                    scope: TruePeakScope::Track,
                    binding: GainDecisionBinding::Track { .. },
                    album_participant: None,
                    ..
                },
                ..
            })
        )));
        assert!(plan.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::Decide(Decision {
                kind: DecisionKind::ReplayGainProjection {
                    policy: ReplayGainProjectionPolicy {
                        mode: ReplayGainMode::Both,
                        ..
                    },
                    group: ReplayGainGroupBinding::Track { scope },
                },
                ..
            }) if scope.0 == "track:standalone-dsd-track"
        )));
    }

    #[test]
    fn protected_general_export_is_explicit_and_precedes_registered_effects() {
        let mut request = dsd_request(SampleGainPolicy::Off);
        request.settings.dsd.general_from_dsd.reconstruction =
            DsdGeneralReconstruction::ReferenceProtected;
        request.settings.dsd.general_from_dsd.export_level = DsdGeneralExportLevel::Native;
        let effects = explicit_effect_sequence([RegisteredUnaryEffect::SoxHighPass {
            frequency_hz: 20,
        }]);
        let Ok(PlanningOutcome::Ready(plan)) = plan_typed_with_effects(&request, &effects) else {
            panic!("protected general render should produce an inspectable plan")
        };
        let export_index = plan
            .nodes
            .iter()
            .position(|node| {
                matches!(
                    node,
                    TypedPlanNode::ExportDsdLevel {
                        gain_db: DbNano::HEADROOM_RESTORATION,
                        ..
                    }
                )
            })
            .expect("protected render must restore the native +12 dB base explicitly");
        let effect_index = plan
            .nodes
            .iter()
            .position(|node| matches!(node, TypedPlanNode::ApplyEffect { .. }))
            .expect("registered effect must be present");
        assert!(export_index < effect_index);
        assert_eq!(
            plan.execution_capability,
            ExecutionCapability::ExecutableByPhase3CommonRealizer,
        );
    }

    #[test]
    fn admitted_reference_terminal_does_not_package_the_protected_r64_carrier_as_output() {
        let mut request = dsd_request(SampleGainPolicy::Off);
        admit_reference(&mut request, ResolvedOutputTarget::FlacNative);
        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("reference candidate description should remain inspectable")
        };
        let (protected, qpcm) = plan
            .nodes
            .iter()
            .find_map(|node| match node {
                TypedPlanNode::ReferenceTerminalRealization { input, output, .. } => {
                    Some((*input, *output))
                }
                _ => None,
            })
            .expect("Reference plan must expose the common terminal realization");
        let protected_state = plan
            .audio_states
            .iter()
            .find(|state| state.id == protected)
            .expect("protected state");
        assert_eq!(protected_state.value_domain, ValueDomain::Q1_31DerivedBinary64);
        let qpcm_state = plan
            .audio_states
            .iter()
            .find(|state| state.id == qpcm)
            .expect("terminal QPCM state");
        assert!(!matches!(qpcm_state.value_domain, ValueDomain::Pending(_)));
        assert!(!qpcm_state.claims.contains(&Claim::ReferenceDsdDelivery));
        assert!(plan.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::PackageOutput { input, .. } if *input == qpcm
        )));
        assert!(plan.bridges.is_empty());
    }

    #[test]
    fn repeated_mixed_backend_effects_keep_explicit_order_and_real_arguments() {
        let request = dsd_request(SampleGainPolicy::Off);
        let effects = explicit_effect_sequence([
            RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
            RegisteredUnaryEffect::FfmpegLowPass { frequency_hz: 40_000 },
            RegisteredUnaryEffect::SoxHighPass { frequency_hz: 30 },
        ]);
        let Ok(PlanningOutcome::Ready(plan)) = plan_typed_with_effects(&request, &effects) else {
            panic!("explicit mixed chain should be planable")
        };
        let lowered = plan
            .nodes
            .iter()
            .filter_map(|node| match node {
                TypedPlanNode::ApplyEffect { lowering, .. } => Some(lowering),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(lowered.len(), 3);
        assert_eq!(lowered[0].tool, ToolIdentifier::Sox);
        assert!(matches!(
            &lowered[0].arguments,
            EffectArgumentMapping::SoxEffect(args) if args == &vec!["highpass".to_owned(), "20".to_owned()]
        ));
        assert_eq!(lowered[1].tool, ToolIdentifier::Ffmpeg);
        assert!(matches!(
            &lowered[1].arguments,
            EffectArgumentMapping::FfmpegAudioFilter(filter) if filter == "lowpass=f=40000"
        ));
        assert_eq!(lowered[2].tool, ToolIdentifier::Sox);
        assert!(matches!(
            &lowered[2].arguments,
            EffectArgumentMapping::SoxEffect(args) if args == &vec!["highpass".to_owned(), "30".to_owned()]
        ));
    }

    #[test]
    fn operation_contract_registry_covers_all_eleven_existing_operation_families() {
        let operations = vec![
            PlanOperation::DecodeToPcm { bit_depth: PcmBitDepth::Int24 },
            PlanOperation::ResamplePcm {
                target_rate_hz: 48_000,
                target_bit_depth: Some(PcmBitDepth::Int24),
                profile: Some(SsrcProfile::Standard),
                brick_wall: true,
            },
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Flac,
                target_rate_hz: Some(48_000),
                target_bit_depth: PcmBitDepth::Int24,
                apply_processing: true,
            },
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Opus,
                target_rate_hz: Some(48_000),
                apply_processing: true,
            },
            PlanOperation::PcmToDsd {
                target_format: AudioFormat::Dsf,
                target_rate: DsdRate::Dsd64,
                filter: DsdFilterPreset::Auto,
            },
            PlanOperation::DsdToPcm {
                target_format: AudioFormat::Wav,
                target_rate_hz: 176_400,
                target_bit_depth: PcmBitDepth::Float64,
                lowpass: DsdLowpassMethod::Auto,
            },
            PlanOperation::DsdRateChange {
                target_format: AudioFormat::Dsf,
                target_rate: DsdRate::Dsd128,
                lowpass: DsdLowpassMethod::Auto,
            },
            PlanOperation::MetadataTransfer {
                target_format: AudioFormat::Flac,
                transfer_tags: true,
                preserve_artwork: true,
            },
            PlanOperation::StoreSourceAudioMd5 { target_format: AudioFormat::Flac },
            PlanOperation::Verify { target_format: AudioFormat::Flac },
        ];
        assert_eq!(operations.len(), 10);
        for operation in operations {
            let contract = contract_for_operation(&operation);
            match operation {
                PlanOperation::MetadataTransfer { .. }
                | PlanOperation::StoreSourceAudioMd5 { .. } => assert!(contract
                    .produces_claims
                    .contains(&ClaimKind::MetadataEffectSatisfied)),
                PlanOperation::Verify { .. } => assert!(contract
                    .runtime_obligations
                    .contains("independent_decode")),
                _ => assert!(contract.runtime_obligations.contains("complete_reader")),
            }
        }
    }

    #[test]
    fn unresolved_dsd_route_facts_are_need_facts_not_false_incompatibility() {
        let mut request = dsd_request(SampleGainPolicy::Off);
        request.source.sample_rate_hz = None;
        let Ok(PlanningOutcome::NeedFacts(facts)) = plan_typed(&request) else {
            panic!("missing DSD source rate must remain a named pending fact")
        };
        assert!(facts.iter().any(|fact| fact.key == "source.sample_rate_hz"));
    }

    #[test]
    fn fixed_gain_does_not_schedule_or_claim_certified_true_peak() {
        let request = dsd_request(SampleGainPolicy::FixedGain {
            gain_db: "3.000000000".parse().unwrap(),
        });
        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("fixed gain should produce a plan")
        };
        assert!(!plan.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::Observe(Observation {
                kind: ObservationKind::CertifiedTruePeak { .. },
                ..
            })
        )));
        assert!(!plan.audio_states.iter().flat_map(|state| state.claims.iter()).any(|claim| {
            matches!(claim, Claim::CertifiedPcmCeiling { .. })
        }));
    }

    #[test]
    fn inherited_loudness_disposition_is_resolved_without_replaygain_scan() {
        let unchanged_request = dsd_request(SampleGainPolicy::Off);
        let unchanged = inherited_loudness_disposition(
            &unchanged_request,
            &normalize_intent(&unchanged_request).unwrap(),
            false,
        );
        assert_eq!(unchanged, InheritedLoudnessDisposition::PreserveApplicable);

        let changed = inherited_loudness_disposition(
            &unchanged_request,
            &normalize_intent(&unchanged_request).unwrap(),
            true,
        );
        assert_eq!(changed, InheritedLoudnessDisposition::DropInapplicable);
    }

    #[test]
    fn metadata_policy_preserves_tags_artwork_and_strip_as_distinct_semantics() {
        let mut tags_only = pcm_request(SampleGainPolicy::Off);
        tags_only.settings.metadata.transfer_tags = true;
        tags_only.settings.metadata.preserve_artwork = false;

        let mut artwork_only = tags_only.clone();
        artwork_only.settings.metadata.transfer_tags = false;
        artwork_only.settings.metadata.preserve_artwork = true;

        let mut strip_all = tags_only.clone();
        strip_all.settings.metadata.transfer_tags = false;
        strip_all.settings.metadata.preserve_artwork = false;

        let Ok(PlanningOutcome::Ready(tags_plan)) = plan_typed(&tags_only) else {
            panic!("tags-only plan should be ready")
        };
        let Ok(PlanningOutcome::Ready(artwork_plan)) = plan_typed(&artwork_only) else {
            panic!("artwork-only plan should be ready")
        };
        let Ok(PlanningOutcome::Ready(strip_plan)) = plan_typed(&strip_all) else {
            panic!("strip-all plan should be ready")
        };

        assert_eq!(
            tags_plan.intent.metadata,
            MetadataTransferPolicy {
                transfer_tags: true,
                preserve_artwork: false,
            }
        );
        assert_eq!(
            artwork_plan.intent.metadata,
            MetadataTransferPolicy {
                transfer_tags: false,
                preserve_artwork: true,
            }
        );
        assert_eq!(
            strip_plan.intent.metadata,
            MetadataTransferPolicy {
                transfer_tags: false,
                preserve_artwork: false,
            }
        );

        assert!(tags_plan.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::MutateArtifact {
                effect: MetadataEffect::TransferSource(MetadataTransferPolicy {
                    transfer_tags: true,
                    preserve_artwork: false,
                }),
                ..
            }
        )));
        assert!(artwork_plan.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::MutateArtifact {
                effect: MetadataEffect::TransferSource(MetadataTransferPolicy {
                    transfer_tags: false,
                    preserve_artwork: true,
                }),
                ..
            }
        )));
        assert!(!strip_plan.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::MutateArtifact {
                effect: MetadataEffect::TransferSource(_),
                ..
            }
        )));

        let tags_fp = crate::fingerprint::common_semantic_plan_fingerprint_v1(
            &tags_only,
            &tags_plan,
        );
        let artwork_fp = crate::fingerprint::common_semantic_plan_fingerprint_v1(
            &artwork_only,
            &artwork_plan,
        );
        let strip_fp = crate::fingerprint::common_semantic_plan_fingerprint_v1(
            &strip_all,
            &strip_plan,
        );
        assert_ne!(tags_fp, artwork_fp);
        assert_ne!(tags_fp, strip_fp);
        assert_ne!(artwork_fp, strip_fp);

        assert!(artwork_plan
            .artifacts
            .iter()
            .filter(|artifact| artifact.role == ArtifactRole::Output)
            .all(|artifact| artifact.inherited_loudness == InheritedLoudnessDisposition::NotCopied));
    }

    #[test]
    fn replaygain_prevention_changes_projection_not_measurement_subject() {
        let mut prevented = pcm_request(SampleGainPolicy::Off);
        prevented.settings.replay_gain.mode = Some(ReplayGainMode::Track);
        prevented.settings.replay_gain.prevent_clipping = true;

        let mut unrestricted = prevented.clone();
        unrestricted.settings.replay_gain.prevent_clipping = false;

        let Ok(PlanningOutcome::Ready(prevented_plan)) = plan_typed(&prevented) else {
            panic!("ReplayGain prevention plan should be ready")
        };
        let Ok(PlanningOutcome::Ready(unrestricted_plan)) = plan_typed(&unrestricted) else {
            panic!("ReplayGain unrestricted plan should be ready")
        };

        assert_eq!(
            prevented_plan.intent.replay_gain,
            Some(ReplayGainProjectionPolicy {
                mode: ReplayGainMode::Track,
                prevention_ceiling_dbtp: Some(ReplayGainProjectionPolicy::PREVENT_CLIPPING_CEILING),
            })
        );
        assert_eq!(
            unrestricted_plan.intent.replay_gain,
            Some(ReplayGainProjectionPolicy {
                mode: ReplayGainMode::Track,
                prevention_ceiling_dbtp: None,
            })
        );

        let replaygain_observation = |plan: &TypedConversionPlan| {
            plan.nodes.iter().find_map(|node| match node {
                TypedPlanNode::Observe(observation)
                    if matches!(observation.kind, ObservationKind::ReplayGain { .. }) =>
                {
                    Some((observation.subject, observation.kind.clone(), observation.read_contract.clone()))
                }
                _ => None,
            })
        };
        assert_eq!(
            replaygain_observation(&prevented_plan),
            replaygain_observation(&unrestricted_plan),
            "projection-only clipping prevention must not change the measurement subject or reader",
        );

        assert_ne!(
            crate::fingerprint::common_semantic_plan_fingerprint_v1(
                &prevented,
                &prevented_plan,
            ),
            crate::fingerprint::common_semantic_plan_fingerprint_v1(
                &unrestricted,
                &unrestricted_plan,
            ),
        );
        assert_ne!(
            crate::fingerprint::common_execution_plan_fingerprint_v1(
                &prevented,
                &prevented_plan,
            ),
            crate::fingerprint::common_execution_plan_fingerprint_v1(
                &unrestricted,
                &unrestricted_plan,
            ),
        );
    }

    #[test]
    fn normalized_semantic_fingerprint_ignores_dormant_general_dsd_controls_for_reference() {
        let mut first = dsd_request(SampleGainPolicy::Off);
        admit_reference(&mut first, ResolvedOutputTarget::FlacNative);
        let mut second = first.clone();
        second.settings.dsd.general_from_dsd.reconstruction =
            DsdGeneralReconstruction::ReferenceProtected;
        second.settings.dsd.general_from_dsd.export_level =
            DsdGeneralExportLevel::NativeWithOffset {
                offset_db: "1.250000000".parse().unwrap(),
            };
        second.settings.dsd.general_from_dsd.gain = SampleGainPolicy::TruePeakNormalize {
            target_dbtp: "-0.750000000".parse().unwrap(),
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Fast,
        };

        let Ok(PlanningOutcome::Ready(first_plan)) = plan_typed(&first) else {
            panic!("first reference plan should be inspectable")
        };
        let Ok(PlanningOutcome::Ready(second_plan)) = plan_typed(&second) else {
            panic!("second reference plan should be inspectable")
        };
        assert_eq!(first_plan.intent, second_plan.intent);
        assert_eq!(
            crate::fingerprint::common_semantic_plan_fingerprint_v1(&first, &first_plan),
            crate::fingerprint::common_semantic_plan_fingerprint_v1(&second, &second_plan),
            "inactive ordinary DSD controls must not compete with Reference delivery identity",
        );
    }

    #[test]
    fn common_semantic_fingerprint_distinguishes_guard_normalize_scope_and_tier() {
        let mut guard_request = dsd_request(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Fast,
        });
        let Ok(PlanningOutcome::Ready(guard_plan)) = plan_typed(&guard_request) else {
            panic!("guard plan")
        };
        let guard_fp = crate::fingerprint::common_semantic_plan_fingerprint_v1(
            &guard_request,
            &guard_plan,
        );

        guard_request.settings.dsd.set_gain_policy(SampleGainPolicy::TruePeakNormalize {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Fast,
        });
        let Ok(PlanningOutcome::Ready(normalize_plan)) = plan_typed(&guard_request) else {
            panic!("normalize plan")
        };
        assert_ne!(
            guard_fp,
            crate::fingerprint::common_semantic_plan_fingerprint_v1(
                &guard_request,
                &normalize_plan,
            )
        );

        guard_request.settings.dsd.set_gain_policy(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Reference,
        });
        let Ok(PlanningOutcome::Ready(album_plan)) = plan_typed(&guard_request) else {
            panic!("album plan")
        };
        assert_ne!(
            guard_fp,
            crate::fingerprint::common_semantic_plan_fingerprint_v1(
                &guard_request,
                &album_plan,
            )
        );
    }

    #[test]
    fn output_product_container_identity_changes_semantic_identity() {
        let first = dsd_request(SampleGainPolicy::Off);
        let mut second = first.clone();
        second.output_path = PathBuf::from("out.caf");
        second.container_ffmpeg_flags = vec!["-f".to_owned(), "caf".to_owned()];

        let Ok(PlanningOutcome::Ready(first_plan)) = plan_typed(&first) else {
            panic!("first product plan")
        };
        let Ok(PlanningOutcome::Ready(second_plan)) = plan_typed(&second) else {
            panic!("second product plan")
        };
        assert_ne!(
            crate::fingerprint::common_semantic_plan_fingerprint_v1(&first, &first_plan),
            crate::fingerprint::common_semantic_plan_fingerprint_v1(&second, &second_plan),
            "same codec settings with different concrete container identity must not collide",
        );
    }

    #[test]
    fn cyclic_effect_dependencies_refuse_with_cycle_diagnostic() {
        let effects = vec![
            EffectIntent {
                id: EffectInstanceId(20),
                effect: RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
                after: vec![EffectInstanceId(21)],
                placement: EffectPlacement::AfterPcmResample,
            },
            EffectIntent {
                id: EffectInstanceId(21),
                effect: RegisteredUnaryEffect::FfmpegLowPass { frequency_hz: 40_000 },
                after: vec![EffectInstanceId(20)],
                placement: EffectPlacement::AfterPcmResample,
            },
        ];
        let refusal = order_registered_effects(&effects).expect_err("cycle must refuse");
        assert_eq!(refusal.code, "effect_order_cycle");
    }

    #[test]
    fn unordered_noncommuting_effects_refuse_instead_of_guessing() {
        let effects = vec![
            EffectIntent {
                id: EffectInstanceId(10),
                effect: RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
                after: Vec::new(),
                placement: EffectPlacement::AfterPcmResample,
            },
            EffectIntent {
                id: EffectInstanceId(11),
                effect: RegisteredUnaryEffect::FfmpegLowPass { frequency_hz: 40_000 },
                after: Vec::new(),
                placement: EffectPlacement::AfterPcmResample,
            },
        ];
        let refusal = order_registered_effects(&effects).expect_err("order must be explicit");
        assert_eq!(refusal.code, "ambiguous_effect_order");
    }

    fn forced_ssrc_wav_request(depth: PcmBitDepth, dither: DitherType) -> PlanRequest {
        let mut request = pcm_request(SampleGainPolicy::Off);
        request.source.sample_rate_hz = Some(96_000);
        request.settings.target_format = AudioFormat::Wav;
        request.output_path = PathBuf::from("out.wav");
        request.settings.target_sample_rate = RateTarget::PcmHz(44_100);
        request.settings.target_bit_depth = BitDepthTarget::Pcm(depth);
        request.settings.dither_type = dither;
        request.settings.preferred_tool = PreferredTool::Ssrc;
        request.settings.ssrc.force = true;
        // Explicit authority is independent of the user-visible transition pill.
        request.settings.nyquist_transition = NyquistTransition::Gentle;
        request
    }

    #[test]
    fn ordinary_ssrc_float64_profiles_remain_pcm_floating() {
        for (profile, expected_precision) in [
            (SsrcProfile::Standard, SsrcComputationPrecision::Single),
            (SsrcProfile::High, SsrcComputationPrecision::Double),
            (SsrcProfile::Long, SsrcComputationPrecision::Double),
            (SsrcProfile::Insane, SsrcComputationPrecision::Double),
        ] {
            let mut request = forced_ssrc_wav_request(PcmBitDepth::Float64, DitherType::None);
            request.settings.ssrc.profile = Some(profile);
            let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
                panic!("ordinary Float64 SSRC profile {profile:?} should be admitted")
            };
            let (output_signal, candidate, resolved) = plan
                .nodes
                .iter()
                .find_map(|node| match node {
                    TypedPlanNode::Operation {
                        operation: PlanOperation::ResamplePcm { .. },
                        output_signal: Some(output_signal),
                        candidates,
                        selected_candidate,
                        resolved_parameters,
                        ..
                    } => candidates
                        .get(*selected_candidate)
                        .map(|candidate| (*output_signal, candidate, resolved_parameters)),
                    _ => None,
                })
                .expect("ordinary SSRC resampler");
            assert_eq!(candidate.tool, Some(ToolIdentifier::Ssrc));
            assert_eq!(
                candidate.contract.representation.emitted_processing_domain,
                Some(ProcessingDomain::PcmFloating),
            );
            assert!(matches!(
                resolved,
                ResolvedOperationParameters::ResampleSsrc {
                    effective_output_depth: PcmBitDepth::Float64,
                    computation_precision,
                    emitted_processing_domain: ProcessingDomain::PcmFloating,
                    ..
                } if *computation_precision == expected_precision
            ));
            let output_state = plan
                .audio_states
                .iter()
                .find(|state| state.id == output_signal)
                .expect("ordinary Float64 SSRC output state");
            assert_eq!(output_state.precision, StoragePrecision::Pcm(PcmBitDepth::Float64));
            assert_eq!(
                output_state.processing_domain,
                Fact::Known(ProcessingDomain::PcmFloating),
            );
        }
    }

    #[test]
    fn ordinary_ssrc_fingerprint_ignores_inactive_strong_evidence_identity() {
        use crate::ssrc_binary64::{
            Binary64ResampleEvidenceScope, Binary64ResamplePreservationEvidence,
            Binary64RuntimeAttestation, TONEPOET_BINARY64_OVERLOAD_PRESERVING_RESAMPLE_V1,
        };

        let mut request = forced_ssrc_wav_request(PcmBitDepth::Float64, DitherType::None);
        request.settings.ssrc.profile = Some(SsrcProfile::High);
        let scope = Binary64ResampleEvidenceScope {
            profile: SsrcProfile::High,
            source_rate_hz: 96_000,
            target_rate_hz: 44_100,
            attenuation_db: Some("0.0".to_owned()),
            min_phase: false,
            architecture: std::env::consts::ARCH.to_owned(),
            input_container: "wav".to_owned(),
            input_sample_format: "pcm_f64le".to_owned(),
            output_container: "w64".to_owned(),
            output_sample_format: "pcm_f64le".to_owned(),
        };
        let policy = |evidence_id: &str| CandidateSearchPolicy {
            max_alternatives_to_inspect: DEFAULT_CANDIDATE_ALTERNATIVE_LIMIT,
            forced_tool: None,
            strip_terminal_proof_for_tool: None,
            ssrc_binary64_evidence_override: Some(TestBinary64SsrcEvidenceOverride {
                scope: scope.clone(),
                evidence: Binary64ResamplePreservationEvidence::Established {
                    authority_id: TONEPOET_BINARY64_OVERLOAD_PRESERVING_RESAMPLE_V1.to_owned(),
                    evidence_id: evidence_id.to_owned(),
                    qualification_report_sha256: format!("report-{evidence_id}"),
                    runtime_attestation: Binary64RuntimeAttestation {
                        expected_executable_sha256: format!("exe-{evidence_id}"),
                        architecture: scope.architecture.clone(),
                        source_revision: crate::ssrc_binary64::PINNED_SSRC_SOURCE_REV.to_owned(),
                        build_identity: format!("build-{evidence_id}"),
                    },
                    scope: scope.clone(),
                },
            }),
        };
        let Ok(PlanningOutcome::Ready(first)) =
            plan_typed_with_effects_and_policy(&request, &[], &policy("first"))
        else {
            panic!("ordinary SSRC plan with inactive strong evidence should be ready")
        };
        let Ok(PlanningOutcome::Ready(second)) =
            plan_typed_with_effects_and_policy(&request, &[], &policy("second"))
        else {
            panic!("ordinary SSRC plan with changed inactive strong evidence should be ready")
        };
        assert!(first.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::Operation {
                resolved_parameters: ResolvedOperationParameters::ResampleSsrc {
                    emitted_processing_domain: ProcessingDomain::PcmFloating,
                    ..
                },
                ..
            }
        )));
        assert_eq!(
            crate::fingerprint::common_semantic_plan_fingerprint_v1(&request, &first),
            crate::fingerprint::common_semantic_plan_fingerprint_v1(&request, &second),
        );
        assert_eq!(
            crate::fingerprint::common_execution_plan_fingerprint_v1(&request, &first),
            crate::fingerprint::common_execution_plan_fingerprint_v1(&request, &second),
        );
    }

    #[test]
    fn typed_planner_does_not_silently_drop_forced_ssrc_without_rate_change() {
        let mut forced = forced_ssrc_wav_request(PcmBitDepth::Int24, DitherType::None);
        forced.settings.target_sample_rate = RateTarget::PcmHz(96_000);
        let expected_reason = plan_topology(&forced)
            .expect_err("legacy planner must refuse forced SSRC without a rate change")
            .to_string();
        let Ok(PlanningOutcome::Refused(refusal)) = plan_typed(&forced) else {
            panic!("typed planner must refuse forced SSRC without a rate change")
        };
        assert_eq!(refusal.code, "invalid_request");
        assert_eq!(refusal.reason, expected_reason);

        forced.settings.dither_type = DitherType::Tpdf;
        forced.settings.dither_explicit = true;
        let Ok(PlanningOutcome::Refused(refusal)) = plan_typed(&forced) else {
            panic!("dither must not introduce forced SSRC when no rate change exists")
        };
        assert_eq!(refusal.code, "invalid_request");

        let mut reference = dsd_request(SampleGainPolicy::Off);
        admit_reference(&mut reference, ResolvedOutputTarget::FlacNative);
        reference.settings.ssrc.force = true;
        let expected_reference_reason = plan_topology(&reference)
            .expect_err("legacy Reference route must reject forced SSRC")
            .to_string();
        let Ok(PlanningOutcome::Refused(reference_refusal)) = plan_typed(&reference) else {
            panic!("typed Reference route must not silently broaden forced SSRC")
        };
        assert_eq!(reference_refusal.code, "invalid_request");
        assert_eq!(reference_refusal.reason, expected_reference_reason);

        forced.settings.ssrc.force = false;
        forced.settings.preferred_tool = PreferredTool::Auto;
        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&forced) else {
            panic!("same-rate PCM without forced SSRC should remain a normal plan")
        };
        assert!(!plan.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::Operation {
                operation: PlanOperation::ResamplePcm { .. },
                ..
            }
        )));
    }

    #[test]
    fn forced_ssrc_int16_terminal_records_integer_domain_and_native_tpdf() {
        let request = forced_ssrc_wav_request(PcmBitDepth::Int16, DitherType::Tpdf);
        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("forced ordinary SSRC terminal should be admitted")
        };
        let (output_signal, resolved, selected) = plan
            .nodes
            .iter()
            .find_map(|node| match node {
                TypedPlanNode::Operation {
                    operation: PlanOperation::ResamplePcm { .. },
                    output_signal: Some(output_signal),
                    candidates,
                    selected_candidate,
                    resolved_parameters,
                    ..
                } => Some((*output_signal, resolved_parameters, &candidates[*selected_candidate])),
                _ => None,
            })
            .expect("selected PCM resampler");
        assert_eq!(selected.tool, Some(ToolIdentifier::Ssrc));
        match resolved {
            ResolvedOperationParameters::ResampleSsrc {
                effective_output_depth,
                output_role,
                emitted_processing_domain,
                effective_dither,
                authority_reason,
                ..
            } => {
                assert_eq!(*effective_output_depth, PcmBitDepth::Int16);
                assert_eq!(*output_role, SsrcOutputRole::Terminal);
                assert_eq!(*emitted_processing_domain, ProcessingDomain::PcmInteger(PcmBitDepth::Int16));
                assert_eq!(effective_dither.dither_id, Some(99));
                assert_eq!(effective_dither.pdf_type, Some(SsrcPdfType::Triangular));
                assert_eq!(effective_dither.availability, crate::plugins::SsrcDitherAvailability::Active);
                assert_eq!(*authority_reason, SsrcAuthorityReason::ExplicitForce);
            }
            other => panic!("unexpected SSRC resolved parameters: {other:?}"),
        }
        let output_state = plan
            .audio_states
            .iter()
            .find(|state| state.id == output_signal)
            .expect("resampler output state");
        assert_eq!(output_state.processing_domain, Fact::Known(ProcessingDomain::PcmInteger(PcmBitDepth::Int16)));
        assert!(
            !plan.nodes.iter().any(|node| matches!(
                node,
                TypedPlanNode::Operation { operation: PlanOperation::EncodePcm { .. }, .. }
            )),
            "SSRC-owned terminal must suppress a second PCM terminal",
        );
    }

    #[test]
    fn explicit_int32_dither_splits_ssrc_to_float64_before_later_terminal() {
        let mut request = forced_ssrc_wav_request(PcmBitDepth::Int32, DitherType::Tpdf);
        request.settings.dither_explicit = true;
        let Ok(PlanningOutcome::Ready(plan)) = plan_typed(&request) else {
            panic!("explicit Int32 dither should use the retained split terminal")
        };
        let resolved = plan
            .nodes
            .iter()
            .find_map(|node| match node {
                TypedPlanNode::Operation {
                    operation: PlanOperation::ResamplePcm { .. },
                    resolved_parameters,
                    ..
                } => Some(resolved_parameters),
                _ => None,
            })
            .expect("SSRC resampler parameters");
        match resolved {
            ResolvedOperationParameters::ResampleSsrc {
                effective_output_depth,
                output_role,
                effective_dither,
                ..
            } => {
                assert_eq!(*effective_output_depth, PcmBitDepth::Float64);
                assert_eq!(*output_role, SsrcOutputRole::Nonterminal);
                assert_eq!(effective_dither.availability, crate::plugins::SsrcDitherAvailability::Inactive);
                assert!(effective_dither.dither_id.is_none());
                assert!(effective_dither.pdf_type.is_none());
            }
            other => panic!("unexpected SSRC resolved parameters: {other:?}"),
        }
        assert!(plan.nodes.iter().any(|node| matches!(
            node,
            TypedPlanNode::Operation { operation: PlanOperation::EncodePcm { target_bit_depth: PcmBitDepth::Int32, .. }, .. }
        )));
    }

    #[test]
    fn pre_effect_then_ssrc_terminal_and_post_effect_split_preserve_barrier_order() {
        let request = forced_ssrc_wav_request(PcmBitDepth::Int16, DitherType::Tpdf);
        let pre = EffectIntent {
            id: EffectInstanceId(1),
            effect: RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
            after: Vec::new(),
            placement: EffectPlacement::BeforePcmResample,
        };
        let Ok(PlanningOutcome::Ready(pre_plan)) = plan_typed_with_effects(&request, &[pre]) else {
            panic!("pre-effect plus SSRC terminal should be admitted")
        };
        let mut pre_index = None;
        let mut resample_index = None;
        for (index, node) in pre_plan.nodes.iter().enumerate() {
            match node {
                TypedPlanNode::ApplyEffect { .. } => pre_index = Some(index),
                TypedPlanNode::Operation {
                    operation: PlanOperation::ResamplePcm { .. },
                    resolved_parameters: ResolvedOperationParameters::ResampleSsrc { output_role, .. },
                    ..
                } => {
                    assert_eq!(*output_role, SsrcOutputRole::Terminal);
                    resample_index = Some(index);
                }
                _ => {}
            }
        }
        assert!(pre_index.expect("pre effect") < resample_index.expect("resampler"));

        let post = EffectIntent {
            id: EffectInstanceId(2),
            effect: RegisteredUnaryEffect::FfmpegLowPass { frequency_hz: 20_000 },
            after: Vec::new(),
            placement: EffectPlacement::AfterPcmResample,
        };
        let Ok(PlanningOutcome::Ready(post_plan)) = plan_typed_with_effects(&request, &[post]) else {
            panic!("post-effect plus SSRC split should be admitted")
        };
        let (resample_index, output_signal) = post_plan
            .nodes
            .iter()
            .enumerate()
            .find_map(|(index, node)| match node {
                TypedPlanNode::Operation {
                    operation: PlanOperation::ResamplePcm { .. },
                    output_signal: Some(output_signal),
                    candidates,
                    selected_candidate,
                    resolved_parameters: ResolvedOperationParameters::ResampleSsrc {
                        effective_output_depth,
                        output_role,
                        emitted_processing_domain,
                        effective_dither,
                        ..
                    },
                    ..
                } => {
                    assert_eq!(*effective_output_depth, PcmBitDepth::Float64);
                    assert_eq!(*output_role, SsrcOutputRole::Nonterminal);
                    assert_eq!(*emitted_processing_domain, ProcessingDomain::PcmFloating);
                    assert_eq!(
                        candidates[*selected_candidate]
                            .contract
                            .representation
                            .emitted_processing_domain,
                        Some(ProcessingDomain::PcmFloating),
                    );
                    assert!(effective_dither.dither_id.is_none());
                    Some((index, *output_signal))
                }
                _ => None,
            })
            .expect("SSRC resampler");
        let effect_index = post_plan
            .nodes
            .iter()
            .position(|node| matches!(node, TypedPlanNode::ApplyEffect { .. }))
            .expect("post effect");
        assert!(resample_index < effect_index);
        let output_state = post_plan
            .audio_states
            .iter()
            .find(|state| state.id == output_signal)
            .expect("Float64 resampler state");
        assert_eq!(output_state.precision, StoragePrecision::Pcm(PcmBitDepth::Float64));
        assert_eq!(output_state.processing_domain, Fact::Known(ProcessingDomain::PcmFloating));
        let effect_output = post_plan
            .nodes
            .iter()
            .find_map(|node| match node {
                TypedPlanNode::ApplyEffect { output, .. } => Some(*output),
                _ => None,
            })
            .expect("post-resample effect output");
        let effect_state = post_plan
            .audio_states
            .iter()
            .find(|state| state.id == effect_output)
            .expect("post-resample effect output state");
        assert!(effect_state.claims.is_empty());
    }

    #[test]
    fn effect_partition_refuses_backwards_cross_resampler_dependency() {
        let effects = vec![
            EffectIntent {
                id: EffectInstanceId(1),
                effect: RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
                after: vec![EffectInstanceId(2)],
                placement: EffectPlacement::BeforePcmResample,
            },
            EffectIntent {
                id: EffectInstanceId(2),
                effect: RegisteredUnaryEffect::FfmpegLowPass { frequency_hz: 20_000 },
                after: Vec::new(),
                placement: EffectPlacement::AfterPcmResample,
            },
        ];
        let refusal = order_registered_effects(&effects)
            .expect_err("pre effect cannot depend on a post-resample effect");
        assert_eq!(refusal.code, "effect_dependency_crosses_resampler_backwards");
    }

    #[test]
    fn certified_pcm_pre_resample_effect_requires_protected_effect_authority() {
        let mut request = pcm_request(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Standard,
        });
        request.source.sample_rate_hz = Some(96_000);
        request.settings.target_sample_rate = RateTarget::PcmHz(44_100);
        request.settings.preferred_tool = PreferredTool::Auto;
        let effect = EffectIntent {
            id: EffectInstanceId(1),
            effect: RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
            after: Vec::new(),
            placement: EffectPlacement::BeforePcmResample,
        };

        let Ok(PlanningOutcome::Refused(refusal)) =
            plan_typed_with_effects(&request, &[effect])
        else {
            panic!("unqualified pre-resample effect must not enter certified PCM hard-ceiling execution")
        };
        assert_eq!(refusal.code, "protected_pre_resample_effect_unqualified");
    }

    #[test]
    fn certified_pcm_normalize_pre_resample_effect_requires_protected_effect_authority() {
        let mut request = pcm_request(SampleGainPolicy::TruePeakNormalize {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Standard,
        });
        request.source.sample_rate_hz = Some(96_000);
        request.settings.target_sample_rate = RateTarget::PcmHz(44_100);
        request.settings.preferred_tool = PreferredTool::Auto;
        let effect = EffectIntent {
            id: EffectInstanceId(1),
            effect: RegisteredUnaryEffect::FfmpegHighPass { frequency_hz: 20 },
            after: Vec::new(),
            placement: EffectPlacement::BeforePcmResample,
        };

        let Ok(PlanningOutcome::Refused(refusal)) =
            plan_typed_with_effects(&request, &[effect])
        else {
            panic!("unqualified pre-resample effect must not enter certified PCM normalize execution")
        };
        assert_eq!(refusal.code, "protected_pre_resample_effect_unqualified");
    }

    #[test]
    fn ordinary_pcm_pre_and_post_effects_still_span_the_resampler_barrier() {
        let mut request = pcm_request(SampleGainPolicy::Off);
        request.source.sample_rate_hz = Some(96_000);
        request.settings.target_sample_rate = RateTarget::PcmHz(44_100);
        request.settings.preferred_tool = PreferredTool::Auto;
        let pre = EffectIntent {
            id: EffectInstanceId(1),
            effect: RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
            after: Vec::new(),
            placement: EffectPlacement::BeforePcmResample,
        };
        let post = EffectIntent {
            id: EffectInstanceId(2),
            effect: RegisteredUnaryEffect::FfmpegLowPass { frequency_hz: 20_000 },
            after: vec![EffectInstanceId(1)],
            placement: EffectPlacement::AfterPcmResample,
        };

        let Ok(PlanningOutcome::Ready(plan)) = plan_typed_with_effects(&request, &[pre, post]) else {
            panic!("ordinary pre/resample/post effect route must remain admitted")
        };
        let pre_index = plan
            .nodes
            .iter()
            .position(|node| matches!(node, TypedPlanNode::ApplyEffect { instance, .. } if instance.placement == EffectPlacement::BeforePcmResample))
            .expect("pre-resample effect");
        let resampler_index = plan
            .nodes
            .iter()
            .position(|node| matches!(node, TypedPlanNode::Operation { operation: PlanOperation::ResamplePcm { .. }, .. }))
            .expect("PCM resampler");
        let post_index = plan
            .nodes
            .iter()
            .position(|node| matches!(node, TypedPlanNode::ApplyEffect { instance, .. } if instance.placement == EffectPlacement::AfterPcmResample))
            .expect("post-resample effect");
        assert!(pre_index < resampler_index && resampler_index < post_index);
    }

    #[test]
    fn before_effect_without_actual_rate_change_refuses() {
        let request = pcm_request(SampleGainPolicy::Off);
        let effect = EffectIntent {
            id: EffectInstanceId(1),
            effect: RegisteredUnaryEffect::SoxHighPass { frequency_hz: 20 },
            after: Vec::new(),
            placement: EffectPlacement::BeforePcmResample,
        };
        let Ok(PlanningOutcome::Refused(refusal)) = plan_typed_with_effects(&request, &[effect]) else {
            panic!("BeforePcmResample without a resample must refuse")
        };
        assert_eq!(refusal.code, "effect_before_resample_without_resample");
    }
}
