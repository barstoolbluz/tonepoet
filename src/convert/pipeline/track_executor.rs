//! Planned per-track conversion executor.
//!
//! This module is the only per-track conversion executor. The planner selects either
//! passthrough copy or a sequential command chain; this module executes that
//! result through `ToolRunner` and applies deterministic finalization.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use sacd_rs::{
    inspect_dsd_container, open_dsd_as_decoded_reader,
    write_decoded_dsd_to_dff_with_cancel, DsdCompression, DsdContainerFormat,
};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tonepoet_pipeline::{
    build_reference_silence_scan_command,
    extract_single_sox_stats_peak_report, parse_reference_sox_stats_true_peak_measurement,
    plan_conversion, resolve_reference_deferred_command,
    validate_post_final_true_peak, validate_reference_decode_mechanism,
    validate_signed_zero_f64le, reference_error_text, reference_scratch_paths, ConversionPlan,
    DsdReferencePlanSummary, DsdSourceKind,
    Finalization, MeasurementId, MeasurementParser, PlanAction, PlanRequest, PlannedCommand,
    PlannedCommandPipeline, PlannedDeferredCommand, PlannedExecutionStep, PlannedMeasurement,
    ReferenceDecodeAuthority, ReferenceDecodeMechanism, ReferenceDecodedCarrier,
    ReferenceDecodedCarrierSelector, ReferenceSampleHashEncoding,
    ReferenceScratchPaths, ReferenceErrorCode,
    SacdAreaKind, SacdFrameEncoding, Sha256Digest, ToolIdentifier,
    W64ExactStructure, W64PcmExpectation, W64PcmFormatExpectation, W64SampleEncoding,
    inspect_exact_w64_pcm, validate_exact_w64_pcm,
    TruePeakMeasurement, TruePeakPurpose,
};
use tonepoet_pipeline::fingerprint::{
    conversion_behavior_fingerprint_v1, execution_fingerprint_v1,
    reference_source_probe_digest_v1, settings_snapshot_fingerprint_v2,
    BehaviorFingerprintV1, ExecutionFingerprintV1, ReferenceExecutionIdentityInput,
    ReferenceMetadataMutatorIdentityInput, ReferenceMetadataMutatorToolchainInput,
    SemanticPlanHashV1, SettingsSnapshotFingerprintV2,
};

use super::errors::{ConvertError, ToolRunnerError};
use super::plan_bridge::{
    plan_request_for_track, planner_metadata_obligations_for_track, reference_sacd_source_kind,
    settings_request_metadata, source_info_for_realized_track,
};
use super::planned_adapter::{planned_command_to_tool_command, DEFAULT_PLANNED_COMMAND_TIMEOUT};
use super::progress::{
    probes, run_streaming_tool_with_probe_with_tool_paths, OperationProgressTracker,
    StreamSource, StreamingHeartbeat,
};
use super::tool::{
    parse_tool_version_output, BoundToolExecutable, CommandRecord, EnvVar, ProcessExit,
    ToolBinary, ToolCommand, ToolOutput, ToolRunner, ToolSegmentedPipelineError,
    ToolSegmentedPipelineOutput, ToolStreamSegment,
};
use super::types::{PlannedMetadataSatisfaction, PipelineRequest, PreparedTrack};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReferenceToolIdentity {
    pub canonical_path: PathBuf,
    pub executable_sha256: Sha256Digest,
    pub reported_version: String,
    pub version_probe_command: CommandRecord,
    pub closure_digest: Sha256Digest,
    pub behavior_probe_digest: Sha256Digest,
    pub behavior_probe_command: CommandRecord,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReferenceMetadataMutatorIdentity {
    pub canonical_path: PathBuf,
    pub executable_sha256: Sha256Digest,
    pub reported_version: String,
    pub version_probe_command: CommandRecord,
    pub closure_digest: Sha256Digest,
}

impl ReferenceMetadataMutatorIdentity {
    pub(crate) fn bound_executable(&self) -> BoundToolExecutable {
        BoundToolExecutable {
            canonical_path: self.canonical_path.clone(),
            executable_sha256: self.executable_sha256,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReferenceMetadataMutatorToolchain {
    pub metaflac: ReferenceMetadataMutatorIdentity,
    pub wvtag: ReferenceMetadataMutatorIdentity,
    pub atomic_parsley: ReferenceMetadataMutatorIdentity,
}

impl ReferenceMetadataMutatorToolchain {
    pub(crate) fn identity(&self, binary: ToolBinary) -> Option<&ReferenceMetadataMutatorIdentity> {
        match binary {
            ToolBinary::Metaflac => Some(&self.metaflac),
            ToolBinary::Wvtag => Some(&self.wvtag),
            ToolBinary::AtomicParsley => Some(&self.atomic_parsley),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReferenceToolchainEvidence {
    pub qualification_manifest_digest: Sha256Digest,
    pub sox_ng: ReferenceToolIdentity,
    pub ffmpeg: ReferenceToolIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_mutators: Option<ReferenceMetadataMutatorToolchain>,
    pub sacd_rs_build_identity: String,
    pub dst_fixture_digest: Sha256Digest,
    pub platform_abi_digest: Sha256Digest,
    pub runtime_dispatch_digest: Sha256Digest,
    pub reporting_uncertainty: tonepoet_pipeline::DbNano,
    pub analyzer_residual: tonepoet_pipeline::DbNano,
}

pub(crate) fn reference_execution_identity_input(
    toolchain: &ReferenceToolchainEvidence,
) -> ReferenceExecutionIdentityInput {
    ReferenceExecutionIdentityInput {
        planner_build_identity: super::manifest::tonepoet_pipeline_version().to_string(),
        platform_abi_digest: toolchain.platform_abi_digest,
        runtime_dispatch_digest: toolchain.runtime_dispatch_digest,
        sox_ng_sha256: toolchain.sox_ng.executable_sha256,
        sox_ng_version: toolchain.sox_ng.reported_version.clone(),
        sox_ng_closure_digest: toolchain.sox_ng.closure_digest,
        sox_ng_behavior_probe_digest: toolchain.sox_ng.behavior_probe_digest,
        ffmpeg_sha256: toolchain.ffmpeg.executable_sha256,
        ffmpeg_version: toolchain.ffmpeg.reported_version.clone(),
        ffmpeg_closure_digest: toolchain.ffmpeg.closure_digest,
        ffmpeg_behavior_probe_digest: toolchain.ffmpeg.behavior_probe_digest,
        metadata_mutators: toolchain.metadata_mutators.as_ref().map(|mutators| {
            let convert = |identity: &ReferenceMetadataMutatorIdentity| {
                ReferenceMetadataMutatorIdentityInput {
                    canonical_path: identity.canonical_path.display().to_string(),
                    executable_sha256: identity.executable_sha256,
                    reported_version: identity.reported_version.clone(),
                    closure_digest: identity.closure_digest,
                }
            };
            ReferenceMetadataMutatorToolchainInput {
                metaflac: convert(&mutators.metaflac),
                wvtag: convert(&mutators.wvtag),
                atomic_parsley: convert(&mutators.atomic_parsley),
            }
        }),
        sacd_rs_build_identity: toolchain.sacd_rs_build_identity.clone(),
        dst_fixture_digest: toolchain.dst_fixture_digest,
        reporting_uncertainty: toolchain.reporting_uncertainty,
        analyzer_residual: toolchain.analyzer_residual,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ReferenceRerunPreflightAuthority {
    pub track_identity: super::manifest::TrackIdentity,
    pub settings_snapshot_fingerprint_v2: SettingsSnapshotFingerprintV2,
    pub resolved_output_target: tonepoet_pipeline::ResolvedOutputTarget,
    pub policy: tonepoet_pipeline::DsdReferencePolicyVersion,
    pub qualification_manifest_digest: Sha256Digest,
    pub source_content_sha256: Sha256Digest,
    pub source_probe_digest: Sha256Digest,
    pub original_source_kind: DsdSourceKind,
    pub front_end: tonepoet_pipeline::DsdInputFrontEnd,
    pub behavior_fingerprint_v1: BehaviorFingerprintV1,
    pub execution_fingerprint_v1: ExecutionFingerprintV1,
    pub semantic_plan_hash_v1: SemanticPlanHashV1,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferencePackagedSampleIdentityMode {
    /// Historical evidence predating an explicit packaging-independence disposition.
    #[default]
    LegacyUnspecified,
    /// A distinct packaged carrier was decoded and compared with terminal QPCM.
    IndependentDecodeComparison,
    /// W64 delivers the terminal QPCM path directly after exact structure and full consumer traversal.
    DirectW64QpcmExactDelivery,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReferencePcmVerificationEvidence {
    /// Canonical probe digest for the 64-bit floating render carrier.
    pub r64_contract_digest: Sha256Digest,
    /// Canonical probe digest for the one terminal PCM carrier.
    pub qpcm_contract_digest: Sha256Digest,
    /// Hash of QPCM decoded to its exact terminal PCM representation.
    pub qpcm_sample_sha256: Sha256Digest,
    /// Hash of the delivered/package carrier decoded to the same representation.
    /// For direct W64 delivery this equals QPCM by identity, not by an independent package transform.
    pub packaged_sample_sha256: Sha256Digest,
    /// Whether package sample identity is an independent decode comparison or exact direct W64 delivery.
    #[serde(default)]
    pub packaged_sample_identity_mode: ReferencePackagedSampleIdentityMode,
    /// Hash repeated after metadata/artwork/replaygain mutation. Required before manifest authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_metadata_sample_sha256: Option<Sha256Digest>,
    /// Historical single-command post-metadata transcript retained for backward decoding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_metadata_verification_command: Option<CommandRecord>,
    /// Exact ordered post-metadata verification transcript. Policy v8 records both
    /// stages when Float64 W64 requires the qualified SoX-to-FFmpeg stream.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_metadata_verification_commands: Vec<CommandRecord>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReferenceExecutionEvidence {
    /// Qualified original DSD source/container identity.
    pub original_source_kind: DsdSourceKind,
    /// SHA-256 of the original admitted source authority (the ISO for SACD).
    pub source_content_sha256: Sha256Digest,
    /// Canonical probe identity for source facts that select the policy cell.
    pub source_probe_digest: Sha256Digest,
    /// SHA-256 of the private canonical source materialization consumed by tools.
    pub canonical_materialization_sha256: Sha256Digest,
    /// Immutable pure-plan facts.
    pub plan: DsdReferencePlanSummary,
    /// Typed measurements keyed by their plan-local IDs.
    pub measurements: BTreeMap<MeasurementId, TruePeakMeasurement>,
    /// Exact attested toolchain and analyzer policy.
    pub toolchain: ReferenceToolchainEvidence,
    /// Exact fully resolved command transcript hash, including carrier and pre-metadata package verification.
    pub resolved_command_hash: String,
    /// Executed carrier/package sample-identity authority.
    pub pcm_verification: ReferencePcmVerificationEvidence,
}

#[derive(Debug, Clone)]
pub struct ExecutedTrackPlan {
    pub commands: Vec<CommandRecord>,
    pub elapsed: Duration,
    /// Metadata obligations satisfied by the planner-owned per-track plan.
    /// This is tracked dimensionally so source tag transfer, artwork_transferred, source
    /// MD5, and materializer-authored tags cannot be collapsed into one flag.
    pub metadata_satisfaction: PlannedMetadataSatisfaction,
    /// Metadata obligations that were meaningful for this realized track after
    /// source facts such as `SourceInfo::audio_md5` were parsed. The stage gate
    /// compares planner satisfaction against this per-track requirement instead
    /// of inferring source-MD5 support from file names.
    pub metadata_required: PlannedMetadataSatisfaction,
    /// SHA-256 of the planned command sequence, for legacy manifest rerun identity.
    pub command_hash: Option<String>,
    /// Native-v2 Reference execution authority, absent for all existing routes.
    pub reference: Option<ReferenceExecutionEvidence>,
}

#[derive(Debug)]
pub struct TrackExecutionError {
    pub error: ConvertError,
    pub commands: Vec<CommandRecord>,
    message: Option<String>,
}

/// Planner-approved single-command encode used by the CUE image fan-out path.
/// The materializer remains descriptive; this record proves that the ordinary
/// per-track planner selected a source-rate Phase-1-compatible encode before
/// the executor feeds byte-exact sample windows from one shared decoded PCM
/// stream. FFmpeg is the common path; planner-approved single-step SoX PCM
/// encodes are also admitted when the apparent processing is only restoration
/// of the authoritative Int16/Int24 source width from the raw s32 transport.
#[derive(Debug, Clone)]
pub(crate) struct CueStreamDirectTrackPlan {
    pub planned_command: PlannedCommand,
    pub finalization: Finalization,
    pub cleanup_paths: Vec<PathBuf>,
    pub work_dir: PathBuf,
    pub metadata_satisfaction: PlannedMetadataSatisfaction,
    pub metadata_required: PlannedMetadataSatisfaction,
    pub command_hash: Option<String>,
}

impl CueStreamDirectTrackPlan {
    /// FFmpeg output-local arguments, excluding the standard file input/map
    /// prefix and the terminal output path.
    pub(crate) fn ffmpeg_output_args(&self) -> Result<&[String], ConvertError> {
        let args = &self.planned_command.args;
        if self.planned_command.tool != ToolIdentifier::Ffmpeg || args.len() < 8 {
            return Err(ConvertError::Backend(
                "streamable CUE FFmpeg plan is shorter than the canonical input/output shape".to_string(),
            ));
        }
        Ok(&args[7..args.len() - 1])
    }

    /// SoX output-local arguments for an admitted direct single-step PCM
    /// encode. The canonical planner shape is
    /// `sox -S -D <input> <output-options> <output>`; `-D` is the planner's
    /// narrow suppression of SoX implicit dither for synthetic carrier-width
    /// restoration. Direct admission separately
    /// proves that no processing effects follow the output path.
    pub(crate) fn sox_output_args(&self) -> Result<&[String], ConvertError> {
        let args = &self.planned_command.args;
        if self.planned_command.tool != ToolIdentifier::Sox
            || args.len() < 5
            || args[0] != "-S"
            || args[1] != "-D"
        {
            return Err(ConvertError::Backend(
                "streamable CUE SoX plan is not the canonical no-implicit-dither input/output shape".to_string(),
            ));
        }
        Ok(&args[3..args.len() - 1])
    }

    pub(crate) fn work_output(&self) -> Result<&Path, ConvertError> {
        match &self.planned_command.output {
            tonepoet_pipeline::OutputSink::Path(path) => Ok(path.as_path()),
            _ => Err(ConvertError::Backend(
                "streamable CUE planner command does not have a path-backed output".to_string(),
            )),
        }
    }
}

impl TrackExecutionError {
    fn new(error: ConvertError, commands: Vec<CommandRecord>) -> Self {
        Self {
            error,
            commands,
            message: None,
        }
    }

    fn with_message(mut self, message: impl Into<String>) -> Self {
        let message = message.into();
        if !message.trim().is_empty() {
            self.message = Some(message);
        }
        self
    }
}

impl From<ConvertError> for TrackExecutionError {
    fn from(error: ConvertError) -> Self {
        Self::new(error, Vec::new())
    }
}

impl std::fmt::Display for TrackExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.message {
            Some(message) => f.write_str(message),
            None => write!(f, "{}", self.error),
        }
    }
}

impl std::error::Error for TrackExecutionError {}

#[derive(Debug, Clone)]
pub struct ToolConcurrencyLimits {
    sox: Arc<Semaphore>,
    ffmpeg: Arc<Semaphore>,
    ssrc: Arc<Semaphore>,
    sox_max_concurrent: usize,
    ffmpeg_max_concurrent: usize,
    ssrc_max_concurrent: usize,
    sox_omp_threads: u32,
}

impl ToolConcurrencyLimits {
    pub fn new(
        sox_max_concurrent: usize,
        ffmpeg_max_concurrent: usize,
        ssrc_max_concurrent: usize,
        sox_omp_threads: u32,
    ) -> Self {
        let sox_max_concurrent = sox_max_concurrent.max(1);
        let ffmpeg_max_concurrent = ffmpeg_max_concurrent.max(1);
        let ssrc_max_concurrent = ssrc_max_concurrent.max(1);
        let sox_omp_threads = sox_omp_threads.max(1);
        Self {
            sox: Arc::new(Semaphore::new(sox_max_concurrent)),
            ffmpeg: Arc::new(Semaphore::new(ffmpeg_max_concurrent)),
            ssrc: Arc::new(Semaphore::new(ssrc_max_concurrent)),
            sox_max_concurrent,
            ffmpeg_max_concurrent,
            ssrc_max_concurrent,
            sox_omp_threads,
        }
    }

    pub fn from_available_parallelism() -> Self {
        let total_cores = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1);
        Self::from_total_cores(total_cores)
    }

    pub fn from_total_cores(total_cores: usize) -> Self {
        let total_cores = total_cores.max(1);
        let sox_max_concurrent = (total_cores / 8).max(1);
        let sox_omp_threads = (total_cores / sox_max_concurrent)
            .max(1)
            .min(u32::MAX as usize) as u32;
        let ffmpeg_max_concurrent = (total_cores / 2).max(1);
        let ssrc_max_concurrent = (total_cores / 2).max(1);
        Self::new(
            sox_max_concurrent,
            ffmpeg_max_concurrent,
            ssrc_max_concurrent,
            sox_omp_threads,
        )
    }

    pub fn sox_omp_threads(&self) -> u32 {
        self.sox_omp_threads
    }

    pub fn sox_max_concurrent(&self) -> usize {
        self.sox_max_concurrent
    }

    pub fn ffmpeg_max_concurrent(&self) -> usize {
        self.ffmpeg_max_concurrent
    }

    pub fn ssrc_max_concurrent(&self) -> usize {
        self.ssrc_max_concurrent
    }

    pub fn max_tool_concurrency(&self) -> usize {
        self.sox_max_concurrent
            .max(self.ffmpeg_max_concurrent)
            .max(self.ssrc_max_concurrent)
    }

    #[cfg(test)]
    pub(crate) async fn hold_ffmpeg_permit_for_test(&self) -> OwnedSemaphorePermit {
        self.ffmpeg
            .clone()
            .acquire_owned()
            .await
            .expect("test holds FFmpeg-family permit")
    }

    #[cfg(test)]
    pub(crate) fn ffmpeg_available_permits_for_test(&self) -> usize {
        self.ffmpeg.available_permits()
    }

}

impl Default for ToolConcurrencyLimits {
    fn default() -> Self {
        Self::from_available_parallelism()
    }
}

pub(crate) async fn preflight_reference_rerun_authority(
    request: &PipelineRequest,
    track: &PreparedTrack,
    planned_output: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<Option<ReferenceRerunPreflightAuthority>, TrackExecutionError> {
    if !tonepoet_pipeline::selects_reference_dsd_to_pcm(&request.settings, true)
        || request.settings.dsd.from_dsd.pathway
            != tonepoet_pipeline::DsdSourcePathway::Reference
    {
        return Ok(None);
    }

    let presented_input = match &track.source_ref {
        super::types::TrackSourceRef::StagedFile(path) => path.clone(),
        super::types::TrackSourceRef::SacdTrack {
            iso,
            track_index,
            ..
        } => iso.with_file_name(format!(
            ".tonepoet-reference-preflight-{track_index}.dsf"
        )),
        _ => {
            return Ok(None);
        }
    };
    let intermediate_dir = planned_output
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".tonepoet-reference-preflight");
    let plan_request = plan_request_for_track(
        request,
        track,
        &presented_input,
        planned_output,
        intermediate_dir,
    )?;
    let plan = plan_conversion(&plan_request)
        .map_err(|err| ConvertError::Backend(format!("planner failed: {err}")))?;
    let Some(summary) = plan.reference else {
        return Ok(None);
    };
    let toolchain = attest_reference_toolchain(
        runner,
        cancel,
        summary.front_end,
        request.stages.metadata == super::types::StageRequirement::Enabled,
    )
    .await?;
    let original_authority = match &track.source_ref {
        super::types::TrackSourceRef::SacdTrack { iso, .. } => iso.as_path(),
        _ => presented_input.as_path(),
    };
    let source_content_sha256 = stable_file_sha256_cancel(original_authority, cancel)
        .map_err(|err| {
            reference_materialization_error(
                format!(
                    "failed to admit Reference source {} for rerun preflight",
                    original_authority.display()
                ),
                err,
            )
        })?;
    let original_source_kind = plan_request.source.dsd_source_kind.clone().ok_or_else(|| {
        TrackExecutionError::new(
            ConvertError::Backend(
                "Reference rerun preflight is missing DSD source identity".to_string(),
            ),
            Vec::new(),
        )
    })?;
    if let super::types::TrackSourceRef::SacdTrack {
        iso,
        track_index,
        area,
    } = &track.source_ref
    {
        let current_source_kind = reference_sacd_source_kind(iso, *track_index, *area)?;
        if current_source_kind != original_source_kind {
            return Err(TrackExecutionError::new(
                ConvertError::Backend(
                    "Reference SACD TOC selection changed during rerun admission".to_string(),
                ),
                Vec::new(),
            ));
        }
        let post_toc_sha256 = stable_file_sha256_cancel(iso, cancel).map_err(|err| {
            reference_materialization_error(
                format!(
                    "failed to re-verify Reference SACD source {} after TOC admission",
                    iso.display()
                ),
                err,
            )
        })?;
        if post_toc_sha256 != source_content_sha256 {
            return Err(TrackExecutionError::new(
                ConvertError::Backend(
                    "Reference SACD source changed during rerun admission".to_string(),
                ),
                Vec::new(),
            ));
        }
    }
    let source_probe_digest = reference_source_probe_digest_v1(&plan_request.source);
    let behavior_fingerprint_v1 =
        conversion_behavior_fingerprint_v1(&summary, &original_source_kind);
    let semantic_plan_hash_v1 = SemanticPlanHashV1(summary.semantic_plan_hash_v1);
    let execution_fingerprint_v1 = execution_fingerprint_v1(
        behavior_fingerprint_v1,
        semantic_plan_hash_v1,
        summary.qualification_manifest_digest,
        &reference_execution_identity_input(&toolchain),
    );

    Ok(Some(ReferenceRerunPreflightAuthority {
        track_identity: super::manifest::TrackIdentity {
            source_ordinal: track.id.source_ordinal as usize,
            disc_number: track.id.disc_number,
            track_number: Some(track.id.track_number),
        },
        settings_snapshot_fingerprint_v2: settings_snapshot_fingerprint_v2(&request.settings),
        resolved_output_target: summary.target,
        policy: summary.policy,
        qualification_manifest_digest: summary.qualification_manifest_digest,
        source_content_sha256,
        source_probe_digest,
        original_source_kind,
        front_end: summary.front_end,
        behavior_fingerprint_v1,
        execution_fingerprint_v1,
        semantic_plan_hash_v1,
    }))
}


#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackExecutionFailurePoint {
    PlanConstruction,
    Attestation,
    Materialization,
    ProducerLaunch,
    ConsumerLaunch,
    Measurement,
    TerminalProcessing,
    Packaging,
    Finalization,
    Cancellation,
}

#[cfg(test)]
impl TrackExecutionFailurePoint {
    fn label(self) -> &'static str {
        match self {
            Self::PlanConstruction => "plan-construction",
            Self::Attestation => "attestation",
            Self::Materialization => "materialization",
            Self::ProducerLaunch => "producer-launch",
            Self::ConsumerLaunch => "consumer-launch",
            Self::Measurement => "measurement",
            Self::TerminalProcessing => "terminal-processing",
            Self::Packaging => "packaging",
            Self::Finalization => "finalization",
            Self::Cancellation => "cancellation",
        }
    }
}

#[cfg(test)]
tokio::task_local! {
    static TRACK_EXECUTION_FAILURE_POINT: TrackExecutionFailurePoint;
}

#[cfg(test)]
tokio::task_local! {
    static TRACK_EXECUTION_USE_INJECTED_RUNNER: ();
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReferenceMaterializationPausePoint {
    BeforeScratchPathCreation,
    DuringSourceCopy,
    DuringDstDecode,
    DuringSacdExtraction,
}

#[cfg(not(test))]
struct ReferenceMaterializationPause;

#[cfg(test)]
struct ReferenceMaterializationPause {
    point: ReferenceMaterializationPausePoint,
    reached: std::sync::atomic::AtomicBool,
    released: Mutex<bool>,
    release_cv: std::sync::Condvar,
    reached_notify: tokio::sync::Notify,
}

#[cfg(test)]
impl ReferenceMaterializationPause {
    fn new(point: ReferenceMaterializationPausePoint) -> Arc<Self> {
        Arc::new(Self {
            point,
            reached: std::sync::atomic::AtomicBool::new(false),
            released: Mutex::new(false),
            release_cv: std::sync::Condvar::new(),
            reached_notify: tokio::sync::Notify::new(),
        })
    }

    fn pause_if_selected(&self, point: ReferenceMaterializationPausePoint) {
        if self.point != point {
            return;
        }
        if !self
            .reached
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.reached_notify.notify_waiters();
        }
        let mut released = self
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*released {
            released = self
                .release_cv
                .wait(released)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    async fn wait_until_reached(&self) {
        loop {
            let notified = self.reached_notify.notified();
            if self.reached.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    fn release(&self) {
        let mut released = self
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *released = true;
        self.release_cv.notify_all();
    }
}

#[cfg(test)]
tokio::task_local! {
    static REFERENCE_MATERIALIZATION_PAUSE: Arc<ReferenceMaterializationPause>;
}

#[cfg(test)]
tokio::task_local! {
    static REFERENCE_TEST_SKIP_ATTESTATION: ();
}

#[cfg(test)]
fn inject_track_execution_failure(
    point: TrackExecutionFailurePoint,
    work_dir: &Path,
    cleanup_paths: &[PathBuf],
) -> Result<(), TrackExecutionError> {
    let selected = TRACK_EXECUTION_FAILURE_POINT
        .try_with(|selected| *selected)
        .ok();
    if selected != Some(point) {
        return Ok(());
    }

    fs::create_dir_all(work_dir).map_err(|error| {
        TrackExecutionError::new(ConvertError::Io(error), Vec::new())
    })?;
    fs::write(
        work_dir.join(format!("injected-{}.partial", point.label())),
        b"partial",
    )
    .map_err(|error| TrackExecutionError::new(ConvertError::Io(error), Vec::new()))?;

    for path in cleanup_paths {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                TrackExecutionError::new(ConvertError::Io(error), Vec::new())
            })?;
        }
        if !path.is_dir() {
            fs::write(path, b"partial").map_err(|error| {
                TrackExecutionError::new(ConvertError::Io(error), Vec::new())
            })?;
        }
    }

    let error = if point == TrackExecutionFailurePoint::Cancellation {
        ConvertError::Realize("cancelled".to_string())
    } else {
        ConvertError::Backend(format!(
            "injected track-execution failure at {}",
            point.label()
        ))
    };
    Err(TrackExecutionError::new(error, Vec::new()))
}

fn cue_stream_ffmpeg_filter_is_only_carrier_depth_normalization(
    plan_request: &PlanRequest,
    command: &PlannedCommand,
) -> bool {
    use tonepoet_pipeline::{BitDepthTarget, DitherType, PcmBitDepth};

    if plan_request.settings.dither_type != DitherType::None {
        return false;
    }
    let target_depth = match plan_request.settings.target_bit_depth {
        BitDepthTarget::Pcm(depth @ (PcmBitDepth::Int16 | PcmBitDepth::Int24)) => depth,
        _ => return false,
    };
    if plan_request.source.authoritative_pcm_depth() != Some(target_depth)
        || plan_request.source.bit_depth != Some(PcmBitDepth::Int32)
    {
        return false;
    }

    let af_positions = command
        .args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| (arg == "-af").then_some(index))
        .collect::<Vec<_>>();
    if af_positions.len() != 1 {
        return false;
    }
    let Some(filter) = command.args.get(af_positions[0] + 1) else {
        return false;
    };
    // Compare against the exact filter the ordinary planner emits for a
    // source-rate carrier-width normalization. Do not accept merely "similar"
    // aresample filters: an output-rate term, dither, reordered/extra option,
    // or future processing knob must close direct admission automatically.
    let settings = &plan_request.settings;
    let mut expected_options = vec![
        "resampler=soxr".to_string(),
        format!(
            "precision={}",
            tonepoet_pipeline::soxr_precision(settings.resample_quality)
        ),
    ];
    let cutoff = settings
        .soxr_resampler
        .cutoff
        .unwrap_or_else(|| tonepoet_pipeline::ffmpeg_cutoff(settings.nyquist_transition));
    expected_options.push(format!("cutoff={cutoff:.3}"));
    if settings.soxr_resampler.chebyshev {
        expected_options.push("cheby=1".to_string());
    }
    if let Some(phase) = settings.soxr_resampler.phase {
        expected_options.push(format!("phase_shift={phase}"));
    }
    expected_options.push(format!(
        "out_sample_fmt={}",
        tonepoet_pipeline::ffmpeg_sample_fmt(target_depth)
    ));
    filter == &format!("aresample={}", expected_options.join(":"))
}

fn cue_stream_ffmpeg_filter_is_only_lossy_rate_conversion(
    plan_request: &PlanRequest,
    command: &PlannedCommand,
    target_rate_hz: u32,
) -> bool {
    use tonepoet_pipeline::DitherType;

    if plan_request.settings.dither_type != DitherType::None
        || plan_request.source.sample_rate_hz == Some(target_rate_hz)
    {
        return false;
    }
    let af_positions = command
        .args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| (arg == "-af").then_some(index))
        .collect::<Vec<_>>();
    if af_positions.len() != 1 {
        return false;
    }
    let Some(filter) = command.args.get(af_positions[0] + 1) else {
        return false;
    };

    let settings = &plan_request.settings;
    let mut expected_options = vec![
        "resampler=soxr".to_string(),
        format!("out_sample_rate={target_rate_hz}"),
        format!(
            "precision={}",
            tonepoet_pipeline::soxr_precision(settings.resample_quality)
        ),
    ];
    let cutoff = settings
        .soxr_resampler
        .cutoff
        .unwrap_or_else(|| tonepoet_pipeline::ffmpeg_cutoff(settings.nyquist_transition));
    expected_options.push(format!("cutoff={cutoff:.3}"));
    if settings.soxr_resampler.chebyshev {
        expected_options.push("cheby=1".to_string());
    }
    if let Some(phase) = settings.soxr_resampler.phase {
        expected_options.push(format!("phase_shift={phase}"));
    }
    filter == &format!("aresample={}", expected_options.join(":"))
}

fn cue_stream_ffmpeg_lossy_plan_is_phase1_direct(
    plan_request: &PlanRequest,
    command: &PlannedCommand,
) -> bool {
    use tonepoet_pipeline::{DitherType, RateTarget};

    if !plan_request.settings.target_format.is_lossy()
        || plan_request.settings.dither_type != DitherType::None
    {
        return false;
    }
    let requested_rate_hz = match plan_request.settings.target_sample_rate {
        RateTarget::Source => plan_request.source.sample_rate_hz,
        RateTarget::PcmHz(rate_hz) => Some(rate_hz),
        RateTarget::Dsd(_) => None,
    };
    let Some(requested_rate_hz) = requested_rate_hz else {
        return false;
    };
    let Some(target_rate_hz) =
        tonepoet_pipeline::mapping::ffmpeg_lossy_encoder_rate_for_request(
            &plan_request.settings.target_format,
            requested_rate_hz,
        )
    else {
        return false;
    };

    let ar_positions = command
        .args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| (arg == "-ar").then_some(index))
        .collect::<Vec<_>>();
    if ar_positions.len() != 1
        || command
            .args
            .get(ar_positions[0] + 1)
            .and_then(|value| value.parse::<u32>().ok())
            != Some(target_rate_hz)
    {
        return false;
    }

    let af_count = command.args.iter().filter(|arg| arg.as_str() == "-af").count();
    if plan_request.source.sample_rate_hz == Some(target_rate_hz) {
        af_count == 0
    } else {
        cue_stream_ffmpeg_filter_is_only_lossy_rate_conversion(
            plan_request,
            command,
            target_rate_hz,
        )
    }
}

fn cue_stream_ffmpeg_plan_is_phase1_direct(
    plan_request: &PlanRequest,
    command: &PlannedCommand,
    realized_input: &Path,
    planned_output: &Path,
) -> bool {
    let realized_input_text = realized_input.to_string_lossy();
    let planned_output_text = planned_output.to_string_lossy();
    let prefix = [
        "-y",
        "-hide_banner",
        "-nostdin",
        "-i",
        &*realized_input_text,
        "-map",
        "0:a:0",
    ];
    if command.args.len() < 8
        || command
            .args
            .iter()
            .take(prefix.len())
            .map(String::as_str)
            .ne(prefix.into_iter())
        || command.args.last().map(String::as_str) != Some(&*planned_output_text)
        || command
            .args
            .iter()
            .any(|arg| arg == "-filter_complex")
    {
        return false;
    }

    if plan_request.settings.target_format.is_lossy() {
        return cue_stream_ffmpeg_lossy_plan_is_phase1_direct(plan_request, command);
    }
    if command.args.iter().any(|arg| arg == "-ar") {
        return false;
    }
    let af_count = command.args.iter().filter(|arg| arg.as_str() == "-af").count();
    af_count == 0
        || (af_count == 1
            && cue_stream_ffmpeg_filter_is_only_carrier_depth_normalization(
                plan_request,
                command,
            ))
}

fn cue_stream_sox_plan_is_phase1_direct(
    plan_request: &PlanRequest,
    command: &PlannedCommand,
    realized_input: &Path,
    planned_output: &Path,
) -> bool {
    use tonepoet_pipeline::{AudioFormat, BitDepthTarget, DitherType, PcmBitDepth, RateTarget};

    if plan_request.settings.dither_type != DitherType::None
        || plan_request.source.bit_depth != Some(PcmBitDepth::Int32)
    {
        return false;
    }
    let authoritative_depth = match plan_request.source.authoritative_pcm_depth() {
        Some(depth @ (PcmBitDepth::Int16 | PcmBitDepth::Int24)) => depth,
        _ => return false,
    };
    if plan_request.settings.target_bit_depth != BitDepthTarget::Pcm(authoritative_depth) {
        return false;
    }
    if !match plan_request.settings.target_sample_rate {
        RateTarget::Source => true,
        RateTarget::PcmHz(rate) => plan_request.source.sample_rate_hz == Some(rate),
        RateTarget::Dsd(_) => false,
    } {
        return false;
    }

    let target_format = &plan_request.settings.target_format;
    if !matches!(
        target_format,
        AudioFormat::Flac | AudioFormat::Wav | AudioFormat::Aiff | AudioFormat::WavPack
    ) || (target_format == &AudioFormat::WavPack && plan_request.settings.wavpack.hybrid)
    {
        return false;
    }

    let input = realized_input.to_string_lossy().into_owned();
    let output = planned_output.to_string_lossy().into_owned();
    let mut expected = vec![
        "-S".to_string(),
        "-D".to_string(),
        input,
        "-b".to_string(),
        authoritative_depth.bits().to_string(),
    ];
    match target_format {
        AudioFormat::Flac => {
            expected.push("-C".to_string());
            expected.push(plan_request.settings.flac.compression_level.to_string());
        }
        AudioFormat::WavPack => {
            expected.push("-C".to_string());
            expected.push(
                tonepoet_pipeline::wavpack_compression_level(plan_request.settings.wavpack.mode)
                    .to_string(),
            );
        }
        AudioFormat::Wav | AudioFormat::Aiff => {}
        _ => return false,
    }
    expected.push(output);

    // Compare against the entire ordinary planner command. `-D` is required
    // here because the planner uses it narrowly to suppress SoX's implicit
    // automatic dither for synthetic carrier-width restoration. This admits no
    // trailing `rate`, explicit `dither`, `sinc`, or other effect and no extra
    // input/output option. Real depth changes, resampling, configured dither,
    // and future DSP additions therefore fail closed to the established path.
    command.args == expected
}

fn cue_stream_phase1_direct_plan_for_paths(
    request: &PipelineRequest,
    track: &PreparedTrack,
    realized_input: &Path,
    staged_output: &Path,
    work_dir: PathBuf,
) -> Result<Option<(PlanRequest, ConversionPlan)>, ConvertError> {
    let plan_request = plan_request_for_track(
        request,
        track,
        realized_input,
        staged_output,
        work_dir,
    )?;
    let plan = plan_conversion(&plan_request)
        .map_err(|err| ConvertError::Backend(format!("planner failed: {err}")))?;

    let PlanAction::Execute {
        commands,
        steps,
        finalization,
        ..
    } = &plan.action
    else {
        return Ok(None);
    };
    if plan.reference.is_some()
        || !steps.is_empty()
        || commands.len() != 1
        || finalization.is_none()
    {
        return Ok(None);
    }
    let command = &commands[0];
    let tonepoet_pipeline::InputSource::Path(planned_input) = &command.input else {
        return Ok(None);
    };
    if planned_input != realized_input {
        return Ok(None);
    }
    let tonepoet_pipeline::OutputSink::Path(planned_output) = &command.output else {
        return Ok(None);
    };

    // Phase 1 stays intentionally narrow. A 16/24-bit source carried as raw
    // s32 may legitimately look like a depth conversion to the ordinary
    // planner even when the requested output preserves the authoritative source
    // width. FFmpeg is admitted only for the exact carrier-normalization filter
    // or the exact ordinary lossy SoXR rate-conversion filter emitted by the
    // planner; SoX is admitted only when the complete canonical command is a
    // single no-effect Int16/Int24 lossless encode. Every other DSP shape stays
    // file-backed, and the existing WavPack-hybrid/FFmpeg-Int24 fidelity guards
    // remain planner authority rather than being bypassed here.
    let direct = match &command.tool {
        ToolIdentifier::Ffmpeg => cue_stream_ffmpeg_plan_is_phase1_direct(
            &plan_request,
            command,
            realized_input,
            planned_output,
        ),
        ToolIdentifier::Sox => cue_stream_sox_plan_is_phase1_direct(
            &plan_request,
            command,
            realized_input,
            planned_output,
        ),
        _ => false,
    };
    if !direct {
        return Ok(None);
    }

    Ok(Some((plan_request, plan)))
}

/// Pure Phase-1 direct-admission preflight used by scratch budgeting. This is
/// deliberately the same planner/admission logic as execution, but it creates
/// no work directories and runs no tools. Unknown/ineligible cells fail closed.
pub(crate) fn cue_stream_phase1_direct_plan_eligible(
    request: &PipelineRequest,
    track: &PreparedTrack,
    realized_input: &Path,
    staged_output: &Path,
    work_dir: &Path,
) -> Result<bool, ConvertError> {
    Ok(cue_stream_phase1_direct_plan_for_paths(
        request,
        track,
        realized_input,
        staged_output,
        work_dir.to_path_buf(),
    )?
    .is_some())
}

pub(crate) fn prepare_cue_stream_direct_track_plan(
    request: &PipelineRequest,
    track: &PreparedTrack,
    realized_input: &Path,
    staged_output: &Path,
    convert_root: &Path,
) -> Result<Option<CueStreamDirectTrackPlan>, ConvertError> {
    let work_dir = convert_root.join(format!(".track-{:04}.work", track.id.source_ordinal));
    reset_track_work_dir(&work_dir)?;

    let Some((plan_request, plan)) = cue_stream_phase1_direct_plan_for_paths(
        request,
        track,
        realized_input,
        staged_output,
        work_dir.clone(),
    )? else {
        let _ = fs::remove_dir_all(&work_dir);
        return Ok(None);
    };

    let PlanAction::Execute {
        cleanup_paths,
        finalization,
        ..
    } = &plan.action
    else {
        unreachable!("direct-plan helper admits only execute actions");
    };
    let Some(finalization) = finalization.clone() else {
        unreachable!("direct-plan helper requires finalization");
    };
    let command_hash = super::manifest::planned_command_hash(&plan).ok();
    let metadata_satisfaction = effective_metadata_satisfaction(&plan_request, &plan);
    let metadata_required = planner_metadata_obligations_for_track(request, track, &plan_request);
    let command = match &plan.action {
        PlanAction::Execute { commands, .. } => commands[0].clone(),
        _ => unreachable!("direct-plan helper admits only execute actions"),
    };

    Ok(Some(CueStreamDirectTrackPlan {
        planned_command: command,
        finalization,
        cleanup_paths: cleanup_paths.clone(),
        work_dir,
        metadata_satisfaction,
        metadata_required,
        command_hash,
    }))
}

pub(crate) fn finalize_cue_stream_direct_track_plan(
    plan: &CueStreamDirectTrackPlan,
) -> Result<(), ConvertError> {
    apply_finalization(&plan.finalization)
}

pub(crate) fn cleanup_cue_stream_direct_track_plan(plan: &CueStreamDirectTrackPlan) {
    for path in &plan.cleanup_paths {
        if path.is_dir() {
            let _ = fs::remove_dir_all(path);
        } else {
            let _ = fs::remove_file(path);
        }
    }
    let _ = fs::remove_dir_all(&plan.work_dir);
}

pub async fn execute_planned_track_conversion(
    request: &PipelineRequest,
    track: &PreparedTrack,
    realized_input: &Path,
    staged_output: &Path,
    convert_root: &Path,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_paths: &HashMap<String, PathBuf>,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
    progress: &mut OperationProgressTracker<'_>,
    start_fraction: f32,
    end_fraction: f32,
) -> Result<ExecutedTrackPlan, TrackExecutionError> {
    let work_dir = convert_root.join(format!(".track-{:04}.work", track.id.source_ordinal));
    let cleanup_guard = TrackExecutionCleanupGuard::acquire(work_dir.clone(), cancel).await?;
    let result = async {
        reset_track_work_dir(&work_dir)?;

        #[cfg(test)]
        inject_track_execution_failure(
            TrackExecutionFailurePoint::PlanConstruction,
            &work_dir,
            &[],
        )?;

        let mut plan_request = plan_request_for_track(
            request,
            track,
            realized_input,
            staged_output,
            work_dir.clone(),
        )?;
        // Run the pure planner first so unsupported cells fail deterministically
        // before any source copy, DST decode, executable probe, or process launch.
        let admitted_source_probe_digest = reference_source_probe_digest_v1(&plan_request.source);
        let admitted_plan = plan_conversion(&plan_request)
            .map_err(|err| ConvertError::Backend(format!("planner failed: {err}")))?;
        cleanup_guard.add_planner_paths(admitted_plan.cleanup_paths());
        let reference_scratch = if admitted_plan.reference.is_some() {
            let scratch = reference_scratch_paths(&plan_request)
                .map_err(|err| ConvertError::Backend(format!("planner failed to bind Reference scratch paths: {err}")))?;
            validate_reference_scratch_cleanup_authority(&admitted_plan, &scratch)?;
            Some(scratch)
        } else {
            None
        };

        #[cfg(test)]
        inject_track_execution_failure(
            TrackExecutionFailurePoint::Attestation,
            &work_dir,
            admitted_plan.cleanup_paths(),
        )?;

        #[cfg(test)]
        let skip_reference_attestation = REFERENCE_TEST_SKIP_ATTESTATION
            .try_with(|_| ())
            .is_ok();
        #[cfg(not(test))]
        let skip_reference_attestation = false;
        let reference_toolchain = if let Some(summary) = admitted_plan.reference.as_ref() {
            if skip_reference_attestation {
                None
            } else {
                Some(
                    attest_reference_toolchain(
                        runner,
                        cancel,
                        summary.front_end,
                        request.stages.metadata == super::types::StageRequirement::Enabled,
                    )
                    .await?,
                )
            }
        } else {
            None
        };

        #[cfg(test)]
        inject_track_execution_failure(
            TrackExecutionFailurePoint::Materialization,
            &work_dir,
            admitted_plan.cleanup_paths(),
        )?;

        let reference_materialization = if admitted_plan.reference.is_some() {
            let scratch = reference_scratch.as_ref().ok_or_else(|| {
                TrackExecutionError::new(
                    ConvertError::Backend("Reference scratch authority is missing".to_string()),
                    Vec::new(),
                )
            })?;
            let materialization = materialize_reference_source(
                &plan_request,
                track,
                realized_input,
                scratch,
                cancel,
                cleanup_guard.blocking_worker_lease()?,
            )
            .await?;
            let admitted_source_kind = plan_request.source.dsd_source_kind.clone();
            let admitted_target = plan_request.resolved_output_target;
            let admitted_scope = plan_request.reference_programme_scope.clone();
            let admitted_summary = admitted_plan.reference.as_ref().ok_or_else(|| {
                TrackExecutionError::new(
                    ConvertError::Backend("Reference plan authority is missing".to_string()),
                    Vec::new(),
                )
            })?;
            let materialized_source = source_info_for_realized_track(track, &materialization.path)?;
            if !materialized_source.codec.is_dsd()
                || materialized_source.sample_rate_hz != plan_request.source.sample_rate_hz
                || materialized_source.channels != plan_request.source.channels
                || materialized_source.sample_kind != Some(tonepoet_pipeline::SampleKind::Dsd)
            {
                return Err(TrackExecutionError::new(
                    ConvertError::Backend(
                        "Reference private materialization changed the admitted DSD rate, channel count, or representation"
                            .to_string(),
                    ),
                    Vec::new(),
                ));
            }

            // Rebind only the immutable private input path and carrier facts. Keep
            // the original container/front-end identity: DSDIFF/DST and SACD/DST
            // must remain qualified decode operations even though their private
            // carrier is now uncompressed DSDIFF/DSD or DSF.
            let mut rematerialized = plan_request.clone();
            rematerialized.input_path = materialization.path.clone();
            rematerialized.source.format = materialized_source.format;
            rematerialized.source.codec = materialized_source.codec;
            rematerialized.source.sample_rate_hz = materialized_source.sample_rate_hz;
            rematerialized.source.bit_depth = materialized_source.bit_depth;
            rematerialized.source.true_source_depth = materialized_source.true_source_depth;
            rematerialized.source.source_representation = materialized_source.source_representation;
            rematerialized.source.sample_kind = materialized_source.sample_kind;
            rematerialized.source.channels = materialized_source.channels;
            rematerialized.source.duration = materialized_source.duration;
            rematerialized.source.audio_md5 = materialized_source.audio_md5;
            rematerialized.source.dsd_source_kind = admitted_source_kind.clone();
            if rematerialized.resolved_output_target != admitted_target
                || rematerialized.reference_programme_scope != admitted_scope
            {
                return Err(TrackExecutionError::new(
                    ConvertError::Backend(
                        "Reference target or programme authority changed during materialization"
                            .to_string(),
                    ),
                    Vec::new(),
                ));
            }
            let rematerialized_plan = plan_conversion(&rematerialized)
                .map_err(|err| ConvertError::Backend(format!("planner failed after materialization: {err}")))?;
            cleanup_guard.add_planner_paths(rematerialized_plan.cleanup_paths());
            validate_reference_scratch_cleanup_authority(&rematerialized_plan, scratch)?;
            let rematerialized_summary = rematerialized_plan.reference.as_ref().ok_or_else(|| {
                TrackExecutionError::new(
                    ConvertError::Backend(
                        "Reference authority disappeared after source materialization".to_string(),
                    ),
                    Vec::new(),
                )
            })?;
            if admitted_summary.semantic_plan_hash_v1 != rematerialized_summary.semantic_plan_hash_v1
                || admitted_summary.policy != rematerialized_summary.policy
                || admitted_summary.qualification_manifest_digest
                    != rematerialized_summary.qualification_manifest_digest
            {
                return Err(TrackExecutionError::new(
                    ConvertError::Backend(
                        "Reference semantic plan changed during source materialization".to_string(),
                    ),
                    Vec::new(),
                ));
            }
            plan_request = rematerialized;
            Some((materialization, rematerialized_plan))
        } else {
            None
        };
        let plan = reference_materialization
            .as_ref()
            .map(|(_, plan)| plan.clone())
            .unwrap_or(admitted_plan);
        cleanup_guard.add_planner_paths(plan.cleanup_paths());
        let command_hash = super::manifest::planned_command_hash(&plan).ok();
        let metadata_satisfaction = effective_metadata_satisfaction(&plan_request, &plan);
        let metadata_required =
            planner_metadata_obligations_for_track(request, track, &plan_request);

        // These test-only failure checkpoints live in the production control-flow
        // frame. They prove that an error or cancellation returned with `?` at each
        // named phase cannot bypass cleanup; stage semantics remain covered by the
        // dedicated executor and qualification tests.
        #[cfg(test)]
        for point in [
            TrackExecutionFailurePoint::ProducerLaunch,
            TrackExecutionFailurePoint::ConsumerLaunch,
            TrackExecutionFailurePoint::Measurement,
            TrackExecutionFailurePoint::TerminalProcessing,
            TrackExecutionFailurePoint::Packaging,
            TrackExecutionFailurePoint::Cancellation,
        ] {
            inject_track_execution_failure(point, &work_dir, plan.cleanup_paths())?;
        }

        let started = Instant::now();
        match &plan.action {
            PlanAction::PassthroughCopy {
                input,
                work_path,
                finalization,
                reason,
                ..
            } => {
                progress
                    .estimated_with_key(
                        start_fraction,
                        format!("passthrough-start:{}", track.id.source_ordinal),
                        format!("Copying passthrough track {} ({reason})", track.id.source_ordinal),
                    )
                    .await;
                copy_to_work_path(input, work_path)?;
                #[cfg(test)]
                inject_track_execution_failure(
                    TrackExecutionFailurePoint::Finalization,
                    &work_dir,
                    plan.cleanup_paths(),
                )?;
                apply_finalization(finalization)?;
                progress
                    .estimated_with_key(
                        end_fraction,
                        format!("passthrough-finish:{}", track.id.source_ordinal),
                        format!("Finished passthrough track {}", track.id.source_ordinal),
                    )
                    .await;
                Ok(ExecutedTrackPlan {
                    commands: Vec::new(),
                    elapsed: started.elapsed(),
                    metadata_satisfaction,
                    metadata_required,
                    command_hash: command_hash.clone(),
                    reference: None,
                })
            }
            PlanAction::Execute {
                commands,
                steps,
                finalization,
                ..
            } => {
                let (commands, reference_runtime) = if steps.is_empty() {
                    (
                        execute_commands(
                            commands,
                            runner,
                            cancel,
                            tool_paths,
                            tool_concurrency_limits,
                            progress,
                            start_fraction,
                            end_fraction,
                            track_label(track),
                        )
                        .await?,
                        None,
                    )
                } else {
                    let runtime = execute_reference_steps(
                        steps,
                        plan.reference.as_ref().ok_or_else(|| {
                            TrackExecutionError::new(
                                ConvertError::Backend(
                                    "measurement-aware plan is missing Reference authority".to_string(),
                                ),
                                Vec::new(),
                            )
                        })?,
                        runner,
                        cancel,
                        tool_paths,
                        tool_concurrency_limits,
                        progress,
                        start_fraction,
                        end_fraction,
                        track_label(track),
                        &reference_scratch
                            .as_ref()
                            .ok_or_else(|| {
                                TrackExecutionError::new(
                                    ConvertError::Backend(
                                        "Reference scratch authority is missing during execution"
                                            .to_string(),
                                    ),
                                    Vec::new(),
                                )
                            })?
                            .silence_scan,
                        reference_toolchain.as_ref().ok_or_else(|| {
                            TrackExecutionError::new(
                                ConvertError::Backend("Reference toolchain evidence is missing".to_string()),
                                Vec::new(),
                            )
                        })?,
                    )
                    .await?;
                    (runtime.commands.clone(), Some(runtime))
                };
                if let Some(finalization) = finalization {
                    #[cfg(test)]
                    inject_track_execution_failure(
                        TrackExecutionFailurePoint::Finalization,
                        &work_dir,
                        plan.cleanup_paths(),
                    )?;
                    if let Err(err) = apply_finalization(finalization) {
                        return Err(TrackExecutionError::new(err, commands));
                    }
                }
                progress
                    .estimated_with_key(
                        end_fraction,
                        format!("plan-finish:{}", track.id.source_ordinal),
                        format!("Finished track {}", track.id.source_ordinal),
                    )
                    .await;
                let reference = match (reference_materialization.as_ref(), reference_toolchain, reference_runtime, plan.reference.clone()) {
                    (Some((materialization, _)), Some(toolchain), Some(runtime), Some(summary)) => Some(ReferenceExecutionEvidence {
                        original_source_kind: plan_request.source.dsd_source_kind.clone().ok_or_else(|| {
                            TrackExecutionError::new(
                                ConvertError::Backend("Reference source identity is missing after materialization".to_string()),
                                commands.clone(),
                            )
                        })?,
                        source_content_sha256: materialization.source_content_sha256,
                        source_probe_digest: admitted_source_probe_digest,
                        canonical_materialization_sha256: materialization.canonical_materialization_sha256,
                        plan: summary,
                        measurements: runtime.measurements,
                        toolchain,
                        resolved_command_hash: runtime.resolved_command_hash,
                        pcm_verification: runtime.pcm_verification,
                    }),
                    (None, None, None, None) => None,
                    _ => {
                        return Err(TrackExecutionError::new(
                            ConvertError::Backend(
                                "Reference plan/materialization/runtime authority is incomplete".to_string(),
                            ),
                            commands,
                        ));
                    }
                };
                Ok(ExecutedTrackPlan {
                    commands,
                    elapsed: started.elapsed(),
                    metadata_satisfaction,
                    metadata_required,
                    command_hash,
                    reference,
                })
            }
        }
    }
    .await;

    let cleanup_result = match cleanup_guard.cleanup_now() {
        Ok(TrackCleanupOutcome::Complete) => Ok(()),
        Ok(TrackCleanupOutcome::DeferredToBlockingWorker) => Err(io::Error::other(
            "track execution completed while a blocking materialization worker remained active",
        )),
        Err(error) => Err(error),
    };
    finish_track_execution(result, cleanup_result)
}

fn finish_track_execution(
    result: Result<ExecutedTrackPlan, TrackExecutionError>,
    cleanup_result: io::Result<()>,
) -> Result<ExecutedTrackPlan, TrackExecutionError> {
    match (result, cleanup_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(value), Err(cleanup_error)) => {
            let message = format!(
                "track conversion completed, but governed scratch cleanup failed: {cleanup_error}"
            );
            Err(TrackExecutionError::new(
                ConvertError::Io(cleanup_error),
                value.commands,
            )
            .with_message(message))
        }
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup_error)) => {
            let primary = error.to_string();
            Err(error.with_message(format!(
                "{primary}; governed scratch cleanup also failed: {cleanup_error}"
            )))
        }
    }
}

fn effective_metadata_satisfaction(
    plan_request: &PlanRequest,
    plan: &ConversionPlan,
) -> PlannedMetadataSatisfaction {
    if !settings_request_metadata(&plan_request.settings) {
        return PlannedMetadataSatisfaction::none();
    }

    match &plan.action {
        PlanAction::PassthroughCopy { .. } => {
            // A passthrough copy preserves source-container tags/artwork, but it
            // does not add new source-MD5 metadata and it does not write
            // materializer-authored album/track tags.
            PlannedMetadataSatisfaction {
                source_tags_transferred: plan_request.settings.metadata.transfer_tags,
                artwork_transferred: plan_request.settings.metadata.preserve_artwork,
                ..PlannedMetadataSatisfaction::none()
            }
        }
        PlanAction::Execute { commands, .. } => commands
            .iter()
            .map(|command| command.metadata_effect)
            .fold(PlannedMetadataSatisfaction::none(), |satisfaction, effect| {
                satisfaction.merge(PlannedMetadataSatisfaction {
                    source_tags_transferred: effect.source_tags_transferred_from_original_source,
                    artwork_transferred: effect.artwork_transferred_from_original_source,
                    source_audio_md5_written: effect.source_audio_md5_written,
                    ..PlannedMetadataSatisfaction::none()
                })
            }),
    }
}




// Append-only v15 checker markers. These strings identify immutable historical
// evidence; runtime activation and all current includes are v16.
const _V15_APPEND_ONLY_EXECUTOR_MARKERS: &str = concat!(
    "dsd_reference_sox_ng_14_8_0_1_v15.json",
    "manifest.schema_version != 15",
    "silent_float64_w64_open_defect_valid",
    "reproduced_and_classified",
    "all_w64_structure_probes_use_sox_info",
    "float64_w64_open_and_silence_proof_use_qualified_sox_route",
);

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedReferenceQualification {
    schema_version: u32,
    policy: String,
    status: String,
    sox_ng: EmbeddedQualifiedSox,
    ffmpeg: EmbeddedQualifiedFfmpeg,
    in_process: EmbeddedInProcessIdentity,
    analyzer: EmbeddedAnalyzerAuthority,
    packaging: EmbeddedReferencePackaging,
    w64_integrity: EmbeddedW64Integrity,
    sample_identity: EmbeddedReferenceSampleIdentity,
    subprocess_environment: EmbeddedSubprocessEnvironment,
    qualification_supervision: EmbeddedQualificationSupervision,
    profiles: EmbeddedReferenceProfiles,
    terminal_bounds: EmbeddedTerminalBounds,
    riff_capacity: EmbeddedRiffCapacity,
    streamed_wav_capacity: EmbeddedStreamedWavCapacity,
    cell_contract: EmbeddedQualifiedCellContract,
    qualification_report: EmbeddedQualificationReport,
    release_certification: EmbeddedReleaseCertification,
    qualification_basis: String,
    runtime_activation: String,
}

#[derive(Debug, serde::Deserialize)]
struct EmbeddedReferenceQualificationVersionProbe {
    schema_version: u32,
}

/// Historical policy manifests use their generation's immutable wire shape.
/// Keep those shapes parseable without weakening the strict active-v16 schema.
#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalEmbeddedReferenceQualification {
    schema_version: u32,
    policy: String,
    status: String,
    sox_ng: serde_json::Value,
    ffmpeg: serde_json::Value,
    in_process: serde_json::Value,
    analyzer: serde_json::Value,
    #[serde(default)]
    packaging: Option<serde_json::Value>,
    #[serde(default)]
    sample_identity: Option<serde_json::Value>,
    #[serde(default)]
    subprocess_environment: Option<serde_json::Value>,
    #[serde(default)]
    qualification_supervision: Option<serde_json::Value>,
    profiles: serde_json::Value,
    terminal_bounds: serde_json::Value,
    riff_capacity: serde_json::Value,
    #[serde(default)]
    streamed_wav_capacity: Option<serde_json::Value>,
    cell_contract: serde_json::Value,
    qualification_report: serde_json::Value,
    #[serde(default)]
    release_certification: Option<serde_json::Value>,
    qualification_basis: String,
    runtime_activation: String,
}

#[derive(Debug)]
enum EmbeddedReferenceQualificationWire {
    Historical(HistoricalEmbeddedReferenceQualification),
    Current(EmbeddedReferenceQualification),
}

fn parse_embedded_reference_qualification_wire(
    raw: &str,
) -> Result<EmbeddedReferenceQualificationWire, String> {
    let probe: EmbeddedReferenceQualificationVersionProbe = serde_json::from_str(raw)
        .map_err(|error| format!("qualification manifest version probe failed: {error}"))?;
    match probe.schema_version {
        1..=15 => serde_json::from_str(raw)
            .map(EmbeddedReferenceQualificationWire::Historical)
            .map_err(|error| {
                format!(
                    "historical qualification schema v{} is invalid: {error}",
                    probe.schema_version
                )
            }),
        16 => serde_json::from_str(raw)
            .map(EmbeddedReferenceQualificationWire::Current)
            .map_err(|error| format!("current qualification schema v16 is invalid: {error}")),
        other => Err(format!(
            "unsupported qualification schema version {other}; current runtime supports historical v1-v15 parsing and strict v16 activation"
        )),
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedReferencePackaging {
    schema: String,
    float64_wav_targets: Vec<String>,
    producer_tool: String,
    producer_args_template: Vec<String>,
    consumer_tool: String,
    consumer_args_template: Vec<String>,
    rf64_args: Vec<String>,
    transport: String,
    stream_encoding: String,
    stream_framing: String,
    endianness: String,
    disk_intermediate: bool,
    environment_policy: String,
    environment: std::collections::BTreeMap<String, String>,
    forbidden_route: String,
    w64_delivery_mode: String,
    w64_same_path_hash_disposition: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedW64Integrity {
    schema: String,
    parser: String,
    carrier_contract_digest: String,
    production_disposition: String,
    required_invariants: Vec<String>,
    enabled_depths: Vec<String>,
    rates_hz: Vec<u32>,
    channels: Vec<u16>,
    required_characterization_cell_count: u64,
    boundary_region_resolution_base_fraction: String,
    trigger_claim: String,
    same_path_qpcm_package_hash_is_independent_packaging_evidence: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedReferenceSampleIdentity {
    schema: String,
    route_authority: String,
    routes: EmbeddedReferenceSampleIdentityRoutes,
    hash_format: String,
    hash_codecs: EmbeddedReferenceSampleHashCodecs,
    forbidden_route: String,
    oracle_independence: String,
    environment_policy: String,
    environment: std::collections::BTreeMap<String, String>,
    metadata_mutation: EmbeddedReferenceMetadataMutation,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedReferenceMetadataMutation {
    w64: String,
    production_entry_point: String,
    shared_production_implementation: String,
    authoritative_tag_source: String,
    qualification_scope: String,
    environment_policy: String,
    environment: BTreeMap<String, String>,
    qualified_post_metadata_targets: Vec<String>,
    admitted_cell_count: u64,
    primary_mutator_case_counts: EmbeddedProductionMetadataMutatorCounts,
    m4a_atomicparsley_freeform_case_count: u64,
    w64_rejection: EmbeddedProductionW64RejectionEvidence,
    post_mutation_container_contract_rechecked: bool,
    rf64_preservation: String,
    w64_non_8_aligned_int24_mono_probe: String,
    riff_odd_byte_int24_mono_probe: String,
    runtime_identity_binding: String,
    execution_authority: String,
    pre_mutation_reverification: String,
    per_output_authority: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedProductionMetadataMutatorCounts {
    ffmpeg: u64,
    metaflac: u64,
    wvtag: u64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedProductionW64RejectionEvidence {
    planner_entry_point: String,
    planner_case_count: u64,
    metadata_entry_point: String,
    metadata_case_count: u64,
    code: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedReferenceSampleIdentityRoutes {
    r64_float64_w64: String,
    qpcm_int24_w64: String,
    qpcm_float32_w64: String,
    qpcm_float64_w64: String,
    packaged_int24_w64: String,
    packaged_float32_w64: String,
    packaged_float64_w64: String,
    packaged_non_w64: String,
    post_metadata_int24_w64: String,
    post_metadata_float32_w64: String,
    post_metadata_float64_w64: String,
    post_metadata_non_w64: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedReferenceSampleHashCodecs {
    int24: String,
    float32: String,
    float64: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedSubprocessEnvironment {
    schema: String,
    policy: String,
    variables: std::collections::BTreeMap<String, String>,
    scope: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedQualificationSupervision {
    schema: String,
    command_deadline_seconds: u64,
    pipeline_deadline_seconds: u64,
    termination_reap_deadline_seconds: u64,
    poll_interval_milliseconds: u64,
    failure_contract: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedQualificationReport {
    schema: String,
    path: String,
    sha256: String,
    guidance_sha256: String,
    decimation_report_sha256: String,
    commission_sha256: String,
    amendment_sha256: String,
    analyzer_corrective_brief_sha256: String,
    runtime_defaults_corrective_brief_sha256: String,
    expanded_supported_cell_count: u64,
    expanded_supported_cell_digest: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedReleaseCertification {
    schema: String,
    path: String,
    candidate_manifest_path: String,
    report_sha256: Option<String>,
    candidate_manifest_sha256: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedQualifiedSox {
    version: String,
    revision: String,
    nar_hash: String,
    required_probe_markers: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedQualifiedFfmpeg {
    major_version: u32,
    package_attribute: String,
    nixpkgs_revision: String,
    nixpkgs_nar_hash: String,
    required_probe_markers: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedInProcessIdentity {
    sacd_rs_build_identity: String,
    dst_fixture_digest: String,
    dst_fixture_manifest_digest: String,
    dst_fixture_provenance_digest: String,
    commission_attestation_digest: String,
    qualification_method: String,
    dst_case_count: u32,
    six_channel_decoder_only_cases: u32,
    standards_literal_oracle_sha256: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedAnalyzerAuthority {
    reporting_uncertainty_db: tonepoet_pipeline::DbNano,
    analyzer_residual_db: tonepoet_pipeline::DbNano,
    qualification_schema: String,
    carrier: EmbeddedAnalyzerCarrier,
    required_case_count: u64,
    target_rates_hz: Vec<u32>,
    channels: Vec<u16>,
    normalized_frequencies_cycles_per_sample: Vec<String>,
    #[serde(default)]
    fixed_frequencies_hz: Vec<u32>,
    #[serde(default)]
    fixed_frequency_max_normalized: String,
    #[serde(default)]
    fixed_frequency_duration_seconds: String,
    phases_radians: Vec<String>,
    analytic_true_peak_levels_dbfs: Vec<String>,
    durations_seconds: Vec<String>,
    peak_positions: Vec<String>,
    waveform_families: Vec<String>,
    aligned_multitone_normalized_frequencies_cycles_per_sample: Vec<String>,
    aligned_multitone_peak_offsets_samples: Vec<String>,
    aligned_multitone_duration_seconds: String,
    #[serde(default)]
    adversarial_peak_level_dbfs: String,
    #[serde(default)]
    adversarial_oracle_oversample_factor: u32,
    #[serde(default)]
    adversarial_case_count: u64,
    #[serde(default)]
    residual_authority: EmbeddedAnalyzerResidualAuthority,
    #[serde(default)]
    deadline_model: EmbeddedAnalyzerDeadlineModel,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedAnalyzerResidualAuthority {
    schema: String,
    ideal_grid_component_db: tonepoet_pipeline::DbNano,
    pinned_resampler_component_limit_db: tonepoet_pipeline::DbNano,
    reporting_quantization_component_db: tonepoet_pipeline::DbNano,
    analyzer_residual_sum_db: tonepoet_pipeline::DbNano,
    one_sided_total_db: tonepoet_pipeline::DbNano,
    resampler_authority_method: String,
    status: String,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedAnalyzerDeadlineModel {
    schema: String,
    startup_seconds: u64,
    minimum_oversampled_sample_values_per_second: u64,
    duration_guard_frames: u64,
    workload_rule: String,
    deadline_rule: String,
    max_admitted_workload_sample_values: u64,
    max_deadline_seconds: u64,
    required_benchmark: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedAnalyzerCarrier {
    schema: String,
    source_container: String,
    producer_tool: String,
    producer_args_template: Vec<String>,
    environment_policy: String,
    environment: std::collections::BTreeMap<String, String>,
    transport: String,
    consumer_tool: String,
    consumer_input_args: Vec<String>,
    consumer_args_template: Vec<String>,
    parser: String,
    stream_encoding: String,
    stream_header: String,
    disk_intermediate: bool,
    exact_recontainer: bool,
    overflow_fixture_required: bool,
    overflow_behavior: String,
    known_ffmpeg_w64_defect: String,
    routing_rule: String,
    direct_float32_input: String,
    direct_float32_consumer_args_template: Vec<String>,
    known_sox_float32_w64_defect: String,
    #[serde(default)]
    direct_tool: String,
    #[serde(default)]
    direct_args_template: Vec<String>,
    #[serde(default)]
    oversample_factor: u32,
    #[serde(default)]
    oversampled_rate_rule: String,
    #[serde(default)]
    analytic_grid_bound_db: tonepoet_pipeline::DbNano,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedStreamedWavCapacity {
    schema: String,
    applies_to: String,
    riff_size_field_max: u64,
    riff_size_overhead_bytes: u64,
    max_audio_payload_bytes: u64,
    sample_encoding: String,
    bytes_per_sample: u64,
    duration_guard_frames: u64,
    admission_rule: String,
    overflow_behavior: String,
    overflow_error_code: String,
    future_lift: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedRiffCapacity {
    max_file_bytes: u64,
    muxer_structure_upper_bound_bytes: u64,
    metadata_expansion_factor: u64,
    source_derived_tag_artwork_upper_bound: String,
    admission_rule: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedReferenceProfiles {
    b1: EmbeddedIntegratedProfile,
    b2: EmbeddedIntegratedProfile,
    b3: EmbeddedSincProfile,
    b4: EmbeddedSincProfile,
    b4w: EmbeddedSincProfile,
    b5: EmbeddedSincProfile,
    b6: EmbeddedDisabledSincProfile,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedIntegratedProfile {
    kind: String,
    target_rate_hz: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedSincProfile {
    passband_hz: u32,
    transition_hz: u32,
    center_hz: u32,
    stopband_hz: u32,
    attenuation_db: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedDisabledSincProfile {
    passband_hz: u32,
    transition_hz: u32,
    center_hz: u32,
    stopband_hz: u32,
    attenuation_db: u32,
    enabled: bool,
}


#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedQualifiedCellContract {
    schema: String,
    source_kinds: Vec<String>,
    source_rates_hz: Vec<u32>,
    source_rate_channel_cells: Vec<EmbeddedSourceRateChannelCell>,
    channels: Vec<u16>,
    gain_modes: Vec<String>,
    profile_cells: Vec<EmbeddedProfileCell>,
    target_depth_cells: Vec<EmbeddedTargetDepthCell>,
    package_compression_levels: Vec<EmbeddedPackageCompressionLevels>,
    package_required_args: Vec<EmbeddedPackageRequiredArgs>,
    expanded_supported_cell_count: u64,
    expanded_supported_cell_digest: String,
    qualification_dimensions: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedSourceRateChannelCell {
    source_kind: String,
    source_rate_hz: u32,
    channels: u16,
    result: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedProfileCell {
    source_rate_hz: u32,
    selection: String,
    target_rate_hz: u32,
    result: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedTargetDepthCell {
    target: String,
    depth: String,
    result: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedPackageCompressionLevels {
    target: String,
    levels: Vec<Option<u8>>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedPackageRequiredArgs {
    target: String,
    depth: String,
    args: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedTerminalBounds {
    target_rates_hz: Vec<u32>,
    derivation_schema: String,
    post_final_acceptance_reserve_db: tonepoet_pipeline::DbNano,
    post_final_acceptance_reserve_basis: String,
    int16_shibata: EmbeddedTerminalBound,
    int24_tpdf: EmbeddedTerminalBound,
    float32: EmbeddedTerminalBound,
    float64: EmbeddedTerminalBound,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedTerminalBound {
    realization: String,
    max_added_peak_fs_q63_ceil: u64,
    safe_pre_terminal_ceiling_dbtp: tonepoet_pipeline::DbNano,
}

fn json_object_u64(
    object: Option<&serde_json::Map<String, serde_json::Value>>,
    key: &str,
) -> Option<u64> {
    object
        .and_then(|value| value.get(key))
        .and_then(serde_json::Value::as_u64)
}

fn validate_terminal_effects_certification(
    packages: &serde_json::Map<String, serde_json::Value>,
    manifest: &EmbeddedReferenceQualification,
) -> Result<(), TrackExecutionError> {
    const Q63_SCALE: f64 = 9_223_372_036_854_775_808.0;
    const SOURCE_PROOF_PATH: &str = "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v8_terminal_source_proof.md";

    let expected_package_fields = BTreeSet::from([
        "status",
        "decode_route_table",
        "case_count",
        "empirical_terminal_bound_case_count",
        "terminal_observed_max_error_by_depth",
        "terminal_effects_boundary_audit",
        "terminal_effects_source_proof",
        "rates_hz",
        "channels",
        "depths",
        "targets",
        "flac_compression_levels",
        "wavpack_compression_levels",
        "wavpack_int24_required_args",
        "container_level_post_mutation_sample_identity",
        "production_metadata_mutation_qualification",
        "command_authority",
    ]);
    let actual_package_fields = packages
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if actual_package_fields != expected_package_fields {
        return Err(reference_toolchain_error(
            "the embedded release-certification package evidence has a non-canonical field set",
        ));
    }

    let observed = packages
        .get("terminal_observed_max_error_by_depth")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded release-certification report has no terminal maxima by depth",
            )
        })?;
    let expected_depth_keys = BTreeSet::from(["int24", "float32", "float64"]);
    if observed
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != expected_depth_keys
        || manifest.terminal_bounds.target_rates_hz.is_empty()
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification terminal maxima have a non-canonical depth set",
        ));
    }
    for (key, depth) in [
        ("int24", tonepoet_pipeline::PcmBitDepth::Int24),
        ("float32", tonepoet_pipeline::PcmBitDepth::Float32),
        ("float64", tonepoet_pipeline::PcmBitDepth::Float64),
    ] {
        let maximum = observed
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or_else(|| {
                reference_toolchain_error(format!(
                    "the embedded release-certification {key} terminal maximum is not a finite non-negative number"
                ))
            })?;
        for &rate_hz in &manifest.terminal_bounds.target_rates_hz {
            let compiled = tonepoet_pipeline::terminal_realization_bound(rate_hz, depth);
            let compiled_maximum = compiled.max_added_peak_fs_q63_ceil as f64 / Q63_SCALE;
            if maximum > compiled_maximum {
                return Err(reference_toolchain_error(format!(
                    "the embedded release-certification {key} terminal maximum {maximum:e} exceeds the compiled {rate_hz} Hz bound {compiled_maximum:e}"
                )));
            }
        }
    }

    let audit = packages
        .get("terminal_effects_boundary_audit")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded release-certification report has no terminal effects-boundary audit",
            )
        })?;
    let expected_audit_fields = BTreeSet::from([
        "sox_internal_sample_domain",
        "round_to_nearest_half_step_peak_bound",
        "inherited_float64_arithmetic_bound",
        "combined_float64_peak_bound",
        "int24_disposition",
        "float32_disposition",
        "float64_disposition",
        "enabled_cells_rejected",
    ]);
    if audit.keys().map(String::as_str).collect::<BTreeSet<_>>()
        != expected_audit_fields
        || audit
            .get("sox_internal_sample_domain")
            .and_then(serde_json::Value::as_str)
            != Some("signed_q1_31")
        || audit
            .get("round_to_nearest_half_step_peak_bound")
            .and_then(serde_json::Value::as_str)
            != Some("2^-32")
        || audit
            .get("inherited_float64_arithmetic_bound")
            .and_then(serde_json::Value::as_str)
            != Some("2^-51")
        || audit
            .get("combined_float64_peak_bound")
            .and_then(serde_json::Value::as_str)
            != Some("2^-32_plus_2^-51")
        || audit
            .get("int24_disposition")
            .and_then(serde_json::Value::as_str)
            != Some("retained_2^-22_bound_contains_effects_rounding")
        || audit
            .get("float32_disposition")
            .and_then(serde_json::Value::as_str)
            != Some("retained_2^-23_bound_contains_effects_and_carrier_rounding")
        || audit
            .get("float64_disposition")
            .and_then(serde_json::Value::as_str)
            != Some("corrected_to_2^-32_plus_2^-51")
        || audit
            .get("enabled_cells_rejected")
            .and_then(serde_json::Value::as_u64)
            != Some(0)
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification terminal effects-boundary audit is not canonical",
        ));
    }

    let source_proof = packages
        .get("terminal_effects_source_proof")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded release-certification report has no terminal effects source proof",
            )
        })?;
    let expected_source_proof_fields = BTreeSet::from([
        "schema",
        "policy",
        "sox_ng_revision",
        "sox_ng_nar_hash",
        "proof_path",
        "proof_sha256",
        "internal_sample_domain",
        "float64_carrier_grid_round_trip",
        "gain_rounding_site",
        "non_clipping_rounding_bound",
        "gain_mode_scope",
        "combined_float64_bound",
    ]);
    let source_proof_bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v8_terminal_source_proof.md"
    ));
    let source_proof_sha256 = Sha256Digest::of_bytes(source_proof_bytes).to_hex();
    if source_proof
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != expected_source_proof_fields
        || source_proof.get("schema").and_then(serde_json::Value::as_str)
            != Some("tonepoet-reference-terminal-effects-source-proof/v1")
        || source_proof.get("policy").and_then(serde_json::Value::as_str)
            != Some(tonepoet_pipeline::DSD_REFERENCE_POLICY_V8_KEY)
        || source_proof
            .get("sox_ng_revision")
            .and_then(serde_json::Value::as_str)
            != Some("324b8cf873fd7836e8848bd87f7a90d8faa6f849")
        || source_proof
            .get("sox_ng_nar_hash")
            .and_then(serde_json::Value::as_str)
            != Some("sha256-LjGx+yaWi5EcZsXhTmdRaf9utFXcCXASMmjRtm6vUc8=")
        || source_proof
            .get("proof_path")
            .and_then(serde_json::Value::as_str)
            != Some(SOURCE_PROOF_PATH)
        || source_proof
            .get("proof_sha256")
            .and_then(serde_json::Value::as_str)
            != Some(source_proof_sha256.as_str())
        || source_proof
            .get("internal_sample_domain")
            .and_then(serde_json::Value::as_str)
            != Some("signed_twos_complement_int32_q1_31")
        || source_proof
            .get("float64_carrier_grid_round_trip")
            .and_then(serde_json::Value::as_str)
            != Some("exact_for_every_sox_sample_t_grid_value")
        || source_proof
            .get("gain_rounding_site")
            .and_then(serde_json::Value::as_str)
            != Some("gain.c:flow_gain:SOX_ROUND_CLIP_COUNT(*ibuf * mult, effp->clips)")
        || source_proof
            .get("non_clipping_rounding_bound")
            .and_then(serde_json::Value::as_str)
            != Some("one_half_internal_sample_equals_2^-32_fs")
        || source_proof
            .get("gain_mode_scope")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|modes| {
                modes.len() != 3
                    || modes[0].as_str() != Some("reference_compensated")
                    || modes[1].as_str() != Some("native_level_exact")
                    || modes[2].as_str() != Some("fixed_exact")
            })
        || source_proof
            .get("combined_float64_bound")
            .and_then(serde_json::Value::as_str)
            != Some("2^-32_plus_2^-51")
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification terminal effects source proof is not canonical",
        ));
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct CertifiedMetadataMutatorIdentity {
    store_path: PathBuf,
    canonical_path: PathBuf,
    executable_sha256: Sha256Digest,
    reported_version: String,
}

#[derive(Debug, Clone)]
struct CertifiedMetadataMutatorToolchain {
    metaflac: CertifiedMetadataMutatorIdentity,
    wvtag: CertifiedMetadataMutatorIdentity,
    atomic_parsley: CertifiedMetadataMutatorIdentity,
}

impl CertifiedMetadataMutatorToolchain {
    fn identity(&self, binary: ToolBinary) -> Option<&CertifiedMetadataMutatorIdentity> {
        match binary {
            ToolBinary::Metaflac => Some(&self.metaflac),
            ToolBinary::Wvtag => Some(&self.wvtag),
            ToolBinary::AtomicParsley => Some(&self.atomic_parsley),
            _ => None,
        }
    }
}


/// True when `value` is exactly the given JSON string array, in order.
fn exact_string_array(value: Option<&serde_json::Value>, expected: &[&str]) -> bool {
    value
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            items.len() == expected.len()
                && items
                    .iter()
                    .zip(expected)
                    .all(|(item, want)| item.as_str() == Some(*want))
        })
}
fn validate_embedded_release_certification(
    manifest: &EmbeddedReferenceQualification,
) -> Result<CertifiedMetadataMutatorToolchain, TrackExecutionError> {
    let certification = &manifest.release_certification;
    if certification.schema != "tonepoet-dsd-reference-release-certification/v1"
        || certification.path
            != "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16_certification.json"
        || certification.candidate_manifest_path
            != "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16_candidate.json"
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification descriptor is not canonical",
        ));
    }
    let report_sha256 = certification.report_sha256.as_deref().ok_or_else(|| {
        reference_toolchain_error(
            "the embedded v16 policy has not bound a release-certification report",
        )
    })?;
    let candidate_manifest_sha256 = certification
        .candidate_manifest_sha256
        .as_deref()
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded v16 policy has not bound its qualified candidate manifest",
            )
        })?;
    let current_manifest_bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16.json"
    ));
    let report_bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16_certification.json"
    ));
    let candidate_manifest_bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16_candidate.json"
    ));
    let parse_digest = |label: &str, value: &str| {
        Sha256Digest::from_hex(value).map_err(|error| {
            reference_toolchain_error(format!(
                "invalid {label} SHA-256 in release certification: {error}"
            ))
        })
    };
    if parse_digest("report", report_sha256)? != Sha256Digest::of_bytes(report_bytes) {
        return Err(reference_toolchain_error(
            "the embedded release-certification report hash does not match its bytes",
        ));
    }
    if parse_digest("candidate manifest", candidate_manifest_sha256)?
        != Sha256Digest::of_bytes(candidate_manifest_bytes)
    {
        return Err(reference_toolchain_error(
            "the embedded qualified-candidate manifest hash does not match its preserved bytes",
        ));
    }
    let mut normalized_current: serde_json::Value =
        serde_json::from_slice(current_manifest_bytes).map_err(|error| {
            reference_toolchain_error(format!(
                "the embedded current v16 manifest is invalid JSON: {error}"
            ))
        })?;
    let candidate_value: serde_json::Value =
        serde_json::from_slice(candidate_manifest_bytes).map_err(|error| {
            reference_toolchain_error(format!(
                "the embedded preserved v16 candidate is invalid JSON: {error}"
            ))
        })?;
    if candidate_value.get("status").and_then(serde_json::Value::as_str)
        != Some("qualification_candidate")
        || candidate_value
            .pointer("/release_certification/report_sha256")
            .is_none_or(|value| !value.is_null())
        || candidate_value
            .pointer("/release_certification/candidate_manifest_sha256")
            .is_none_or(|value| !value.is_null())
    {
        return Err(reference_toolchain_error(
            "the preserved v16 candidate is not the canonical unpromoted policy snapshot",
        ));
    }
    normalized_current["status"] =
        serde_json::Value::String("qualification_candidate".to_string());
    normalized_current["release_certification"]["report_sha256"] =
        serde_json::Value::Null;
    normalized_current["release_certification"]["candidate_manifest_sha256"] =
        serde_json::Value::Null;
    if normalized_current != candidate_value {
        return Err(reference_toolchain_error(
            "the promoted v16 manifest differs from its qualified candidate outside the permitted certification fields",
        ));
    }
    let report: serde_json::Value = serde_json::from_slice(report_bytes).map_err(|error| {
        reference_toolchain_error(format!(
            "the embedded release-certification report is invalid JSON: {error}"
        ))
    })?;
    if report.get("schema_version").and_then(serde_json::Value::as_u64) != Some(16)
        || report.get("policy").and_then(serde_json::Value::as_str)
            != Some(tonepoet_pipeline::DSD_REFERENCE_POLICY_V16_KEY)
        || report.get("status").and_then(serde_json::Value::as_str) != Some("passed")
        || report.get("outcome").and_then(serde_json::Value::as_str) != Some("pass")
        || report
            .get("qualification_manifest_digest")
            .and_then(serde_json::Value::as_str)
            != Some(candidate_manifest_sha256)
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification report does not bind the qualified v16 candidate",
        ));
    }
    for required in [
        "toolchain",
        "runtime_metadata_mutator_binding",
        "default_settings_live_smoke",
        "subprocess_environment",
        "qualification_supervision",
        "subprocess_environment_probe",
        "streamed_wav_capacity",
        "streamed_wav_capacity_policy",
        "analyzer_carrier",
        "w64_exact_integrity",
        "production_true_peak_analyzer",
        "analyzer_deadline_model",
        "executor_liveness",
        "production_measurement_gain_terminal_chain",
        "production_source_front_end_integration",
        "dst_independent_oracle",
        "package_decode_back",
        "float64_package_pipeline",
        "sample_identity_oracle",
        "evidence_command_environment",
        "qualified_cell_contract",
    ] {
        if report.get(required).is_none() {
            return Err(reference_toolchain_error(format!(
                "the embedded release-certification report omits {required}"
            )));
        }
    }
    let executor_liveness = report
        .get("executor_liveness")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded release-certification report has no executor-liveness evidence",
            )
        })?;
    if executor_liveness
        .get("status")
        .and_then(serde_json::Value::as_str)
        != Some("passed_by_workspace_gate")
        || executor_liveness
            .get("test")
            .and_then(serde_json::Value::as_str)
            != Some("reference_pipeline_composite_permits_prevent_opposite_direction_deadlock")
        || !exact_string_array(
            executor_liveness.get("global_tool_family_order"),
            &["sox", "ffmpeg", "ssrc"],
        )
        || executor_liveness
            .get("permit_set")
            .and_then(serde_json::Value::as_str)
            != Some("deduplicated_cancellation_safe_raii")
        || executor_liveness
            .get("interleaving")
            .and_then(serde_json::Value::as_str)
            != Some("barrier_forced_no_sleep")
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification executor-liveness evidence is not canonical",
        ));
    }
    let deadline_evidence = report
        .get("analyzer_deadline_model")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded release-certification report has no analyzer-deadline evidence",
            )
        })?;
    let observed_throughput = deadline_evidence
        .get("observed_oversampled_sample_values_per_second")
        .and_then(serde_json::Value::as_f64);
    if deadline_evidence
        .get("status")
        .and_then(serde_json::Value::as_str)
        != Some("passed")
        || deadline_evidence
            .get("schema")
            .and_then(serde_json::Value::as_str)
            != Some("tonepoet-reference-analyzer-deadline-qualification/v1")
        || deadline_evidence
            .get("required_minimum_oversampled_sample_values_per_second")
            .and_then(serde_json::Value::as_u64)
            != Some(
                tonepoet_pipeline::REFERENCE_TRUE_PEAK_MIN_OVERSAMPLED_SAMPLE_VALUES_PER_SECOND,
            )
        || observed_throughput.is_none_or(|value| {
            value
                < tonepoet_pipeline::REFERENCE_TRUE_PEAK_MIN_OVERSAMPLED_SAMPLE_VALUES_PER_SECOND
                    as f64
        })
        || deadline_evidence
            .get("maximum_admitted_workload_sample_values")
            .and_then(serde_json::Value::as_u64)
            != Some(tonepoet_pipeline::REFERENCE_TRUE_PEAK_MAX_ADMITTED_WORKLOAD_SAMPLE_VALUES)
        || deadline_evidence
            .get("maximum_derived_deadline_seconds")
            .and_then(serde_json::Value::as_u64)
            != Some(tonepoet_pipeline::REFERENCE_TRUE_PEAK_MAX_DEADLINE_SECONDS)
        || deadline_evidence
            .get("planner_bound_identical_pipeline_deadlines")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification analyzer-deadline evidence is not canonical",
        ));
    }
    let default_smoke = report
        .get("default_settings_live_smoke")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded default-settings live smoke is not a structured result",
            )
        })?;
    let default_smoke_commands = default_smoke
        .get("commands")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded default-settings live smoke omits planned command evidence",
            )
        })?;
    if default_smoke.get("status").and_then(serde_json::Value::as_str) != Some("passed")
        || default_smoke.get("route").and_then(serde_json::Value::as_str)
            != Some("legacy_flat_v1")
        || default_smoke.get("source").and_then(serde_json::Value::as_str)
            != Some("dsd64_dsf")
        || default_smoke.get("target").and_then(serde_json::Value::as_str)
            != Some("flac_native")
        || default_smoke
            .get("sample_rate_hz")
            .and_then(serde_json::Value::as_u64)
            != Some(88_200)
        || default_smoke.get("channels").and_then(serde_json::Value::as_u64) != Some(2)
        || default_smoke.get("bit_depth").and_then(serde_json::Value::as_str) != Some("int24")
        || default_smoke
            .get("command_count")
            .and_then(serde_json::Value::as_u64)
            != u64::try_from(default_smoke_commands.len()).ok()
        || default_smoke_commands.is_empty()
        || default_smoke
            .get("output_sha256")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|digest| Sha256Digest::from_hex(digest).is_err())
    {
        return Err(reference_toolchain_error(
            "the embedded default-settings DSD64 live smoke is not canonical",
        ));
    }
    let report_environment = report
        .get("subprocess_environment")
        .and_then(serde_json::Value::as_object);
    let report_environment_variables = report_environment
        .and_then(|value| value.get("variables"))
        .and_then(serde_json::Value::as_object);
    let report_supervision = report
        .get("qualification_supervision")
        .and_then(serde_json::Value::as_object);
    let environment_probe = report
        .get("subprocess_environment_probe")
        .and_then(serde_json::Value::as_object);
    if report_environment
        .and_then(|value| value.get("schema"))
        .and_then(serde_json::Value::as_str)
        != Some("tonepoet-reference-subprocess-environment/v1")
        || report_environment
            .and_then(|value| value.get("policy"))
            .and_then(serde_json::Value::as_str)
            != Some("clear_and_set")
        || report_environment
            .and_then(|value| value.get("scope"))
            .and_then(serde_json::Value::as_str)
            != Some("all_reference_external_commands")
        || report_environment_variables.is_none_or(|variables| {
            variables.len() != 1
                || variables.get("LC_ALL").and_then(serde_json::Value::as_str) != Some("C")
        })
        || report_supervision
            .and_then(|value| value.get("schema"))
            .and_then(serde_json::Value::as_str)
            != Some("tonepoet-reference-qualification-supervision/v1")
        || report_supervision
            .and_then(|value| value.get("command_deadline_seconds"))
            .and_then(serde_json::Value::as_u64)
            != Some(1_200)
        || report_supervision
            .and_then(|value| value.get("pipeline_deadline_seconds"))
            .and_then(serde_json::Value::as_u64)
            != Some(3_600)
        || report_supervision
            .and_then(|value| value.get("termination_reap_deadline_seconds"))
            .and_then(serde_json::Value::as_u64)
            != Some(10)
        || report_supervision
            .and_then(|value| value.get("poll_interval_milliseconds"))
            .and_then(serde_json::Value::as_u64)
            != Some(10)
        || report_supervision
            .and_then(|value| value.get("failure_contract"))
            .and_then(serde_json::Value::as_str)
            != Some("terminate_and_reap_or_fail")
        || environment_probe
            .and_then(|value| value.get("status"))
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        || environment_probe
            .and_then(|value| value.get("schema"))
            .and_then(serde_json::Value::as_str)
            != Some("tonepoet-reference-subprocess-environment-probe/v1")
        || environment_probe
            .and_then(|value| value.get("policy"))
            .and_then(serde_json::Value::as_str)
            != Some("clear_and_set")
        || environment_probe
            .and_then(|value| value.get("ambient_poison_observed"))
            .and_then(serde_json::Value::as_bool)
            != Some(false)
        || environment_probe
            .and_then(|value| value.get("qualified_environment"))
            .and_then(serde_json::Value::as_object)
            .is_none_or(|variables| {
                variables.len() != 1
                    || variables.get("LC_ALL").and_then(serde_json::Value::as_str) != Some("C")
            })
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification report does not prove the frozen subprocess environment and bounded supervision contract",
        ));
    }

    let toolchain = report.get("toolchain").and_then(serde_json::Value::as_object);
    if toolchain
        .and_then(|value| value.get("integrated_rate_profiles"))
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        != Some(2)
        || toolchain
            .and_then(|value| value.get("explicit_composite_profiles"))
            .and_then(serde_json::Value::as_array)
            .map(Vec::len)
            != Some(5)
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification report has incomplete D1 profile evidence",
        ));
    }
    let production_mutators = toolchain
        .and_then(|value| value.get("production_metadata_mutators"))
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded release-certification report omits production metadata mutator identities",
            )
        })?;
    let expected_mutators = BTreeSet::from(["AtomicParsley", "metaflac", "wvtag"]);
    if production_mutators
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != expected_mutators
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification report has a non-canonical production metadata mutator set",
        ));
    }
    let mut certified_mutators = BTreeMap::new();
    for name in ["metaflac", "wvtag", "AtomicParsley"] {
        let identity = production_mutators
            .get(name)
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                reference_toolchain_error(format!(
                    "the embedded release-certification report omits {name} identity evidence",
                ))
            })?;
        let expected_identity_fields =
            BTreeSet::from(["canonical_path", "store_path", "executable_sha256", "reported_version"]);
        if identity
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != expected_identity_fields
        {
            return Err(reference_toolchain_error(format!(
                "the embedded release-certification report has a non-canonical identity object for {name}",
            )));
        }
        let store_path = identity
            .get("store_path")
            .and_then(serde_json::Value::as_str)
            .filter(|value| Path::new(value).is_absolute())
            .ok_or_else(|| {
                reference_toolchain_error(format!(
                    "the embedded release-certification report has no absolute store path for {name}",
                ))
            })?;
        let canonical_path = identity
            .get("canonical_path")
            .and_then(serde_json::Value::as_str)
            .filter(|value| Path::new(value).is_absolute())
            .ok_or_else(|| {
                reference_toolchain_error(format!(
                    "the embedded release-certification report has no absolute canonical path for {name}",
                ))
            })?;
        let executable_sha256 = identity
            .get("executable_sha256")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                reference_toolchain_error(format!(
                    "the embedded release-certification report has no executable digest for {name}",
                ))
            })?;
        let executable_sha256 = Sha256Digest::from_hex(executable_sha256).map_err(|error| {
            reference_toolchain_error(format!(
                "the embedded release-certification report has an invalid executable digest for {name}: {error}",
            ))
        })?;
        let reported_version = identity
            .get("reported_version")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                reference_toolchain_error(format!(
                    "the embedded release-certification report has no reported version for {name}",
                ))
            })?;
        certified_mutators.insert(
            name,
            CertifiedMetadataMutatorIdentity {
                store_path: PathBuf::from(store_path),
                canonical_path: PathBuf::from(canonical_path),
                executable_sha256,
                reported_version: reported_version.to_string(),
            },
        );
    }
    let certified_mutators = CertifiedMetadataMutatorToolchain {
        metaflac: certified_mutators
            .remove("metaflac")
            .expect("validated metaflac certification"),
        wvtag: certified_mutators
            .remove("wvtag")
            .expect("validated wvtag certification"),
        atomic_parsley: certified_mutators
            .remove("AtomicParsley")
            .expect("validated AtomicParsley certification"),
    };
    let runtime_binding = report
        .get("runtime_metadata_mutator_binding")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded release-certification report omits runtime metadata-mutator binding evidence",
            )
        })?;
    for (field, expected) in [
        ("schema", "tonepoet-reference-runtime-metadata-mutator-binding/v1"),
        ("status", "passed"),
        ("certified_identity_source", "toolchain.production_metadata_mutators"),
        ("compiled_store_binding", "required_for_metaflac_wvtag_atomicparsley"),
        (
            "activation_path_policy",
            "must_equal_compiled_store_and_certified_canonical_path",
        ),
        ("runner_resolution_policy", "resolved_canonical_path_must_equal_certified_path"),
        (
            "execution_authority",
            "exact_canonical_path_plus_executable_sha256",
        ),
        ("pre_mutation_reverification", "path_sha256_version_closure"),
        (
            "per_output_authority",
            "ReferenceToolchainEvidence.metadata_mutators_and_execution_fingerprint_v1",
        ),
    ] {
        if runtime_binding
            .get(field)
            .and_then(serde_json::Value::as_str)
            != Some(expected)
        {
            return Err(reference_toolchain_error(format!(
                "the embedded release-certification runtime metadata-mutator binding has non-canonical {field}",
            )));
        }
    }
    let w64_integrity = report
        .get("w64_exact_integrity")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded release-certification report has no exact Wave64 integrity evidence",
            )
        })?;
    let w64_cells = w64_integrity
        .get("cells")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded exact Wave64 integrity evidence omits its cell matrix",
            )
        })?;
    let expected_rates = [
        44_100_u64, 48_000, 88_200, 96_000, 176_400,
        192_000, 352_800, 384_000, 705_600, 768_000,
    ];
    let expected_channels = [1_u64, 2_u64];
    let expected_depths = ["int24", "float32", "float64"];
    let mut observed_cells = BTreeSet::new();
    let mut observed_malformed_all_zero_cells = 0_u64;
    let mut observed_valid_all_zero_cells = 0_u64;
    for cell in w64_cells {
        let rate = cell.get("sample_rate_hz").and_then(serde_json::Value::as_u64);
        let channels = cell.get("channels").and_then(serde_json::Value::as_u64);
        let depth = cell.get("depth").and_then(serde_json::Value::as_str);
        let threshold = cell
            .get("smallest_reachable_nonzero_power_of_two_exponent")
            .and_then(serde_json::Value::as_i64);
        let below = cell
            .get("immediately_below_boundary_exponent")
            .and_then(serde_json::Value::as_i64);
        let all_zero_structure = cell
            .get("all_zero_structure")
            .and_then(serde_json::Value::as_str);
        let below_boundary_structure = cell
            .get("below_boundary_structure")
            .and_then(serde_json::Value::as_str);
        let ffmpeg_all_zero_opened = cell
            .get("ffmpeg_all_zero_opened")
            .and_then(serde_json::Value::as_bool);
        let ffmpeg_below_boundary_opened = cell
            .get("ffmpeg_below_boundary_opened")
            .and_then(serde_json::Value::as_bool);
        let largest_zero_numerator = cell
            .get("largest_zero_multiplier_numerator")
            .and_then(serde_json::Value::as_u64);
        let smallest_nonzero_numerator = cell
            .get("smallest_nonzero_multiplier_numerator")
            .and_then(serde_json::Value::as_u64);
        if rate.is_none_or(|value| !expected_rates.contains(&value))
            || channels.is_none_or(|value| !expected_channels.contains(&value))
            || depth.is_none_or(|value| !expected_depths.contains(&value))
            || threshold.is_none_or(|value| !(-96..=-1).contains(&value))
            || below.zip(threshold).is_none_or(|(below, threshold)| below != threshold - 1)
            || cell.get("scan_exponents").and_then(serde_json::Value::as_array)
                .is_none_or(|range| {
                    range.len() != 2
                        || range[0].as_i64() != Some(-96)
                        || range[1].as_i64() != Some(-1)
                })
            || cell.get("boundary_probe_denominator").and_then(serde_json::Value::as_u64)
                != Some(510)
            || cell.get("boundary_probe_count").and_then(serde_json::Value::as_u64)
                != Some(256)
            || largest_zero_numerator.is_none_or(|value| !(255..510).contains(&value))
            || smallest_nonzero_numerator
                .zip(largest_zero_numerator)
                .is_none_or(|(nonzero, zero)| nonzero != zero + 1 || nonzero > 510)
            || cell.get("boundary_region_width_base_fraction")
                .and_then(serde_json::Value::as_str) != Some("1/510")
            || cell.get("boundary_neighborhood_structure")
                .and_then(serde_json::Value::as_str) != Some("exact")
            || cell.get("smallest_bracketed_nonzero_decoded_nonzero")
                .and_then(serde_json::Value::as_bool) != Some(true)
            || cell.get("all_zero_payload_physically_zero")
                .and_then(serde_json::Value::as_bool) != Some(true)
            || cell.get("below_boundary_payload_physically_zero")
                .and_then(serde_json::Value::as_bool) != Some(true)
            || !matches!(all_zero_structure, Some("exact") | Some("malformed_rejected"))
            || below_boundary_structure != all_zero_structure
            || ffmpeg_below_boundary_opened != ffmpeg_all_zero_opened
            || (match all_zero_structure {
                Some("exact") => ffmpeg_all_zero_opened != Some(true),
                Some("malformed_rejected") => ffmpeg_all_zero_opened != Some(false),
                _ => true,
            })
            || cell.get("at_boundary_structure").and_then(serde_json::Value::as_str)
                != Some("exact")
            || cell.get("at_boundary_decoded_nonzero")
                .and_then(serde_json::Value::as_bool) != Some(true)
            || cell.get("leading_silence_control").and_then(serde_json::Value::as_str)
                != Some("exact_and_decoded_nonzero")
            || cell.get("trailing_silence_control").and_then(serde_json::Value::as_str)
                != Some("exact_and_decoded_nonzero")
            || cell.get("exact_sample_frames").and_then(serde_json::Value::as_u64)
                != Some(257)
        {
            return Err(reference_toolchain_error(
                "the embedded exact Wave64 integrity matrix contains a non-canonical cell",
            ));
        }
        match all_zero_structure.expect("validated all-zero structure") {
            "exact" => observed_valid_all_zero_cells += 1,
            "malformed_rejected" => observed_malformed_all_zero_cells += 1,
            _ => unreachable!("validated all-zero disposition"),
        }
        let key = (
            rate.expect("validated rate"),
            channels.expect("validated channels"),
            depth.expect("validated depth").to_string(),
        );
        if !observed_cells.insert(key) {
            return Err(reference_toolchain_error(
                "the embedded exact Wave64 integrity matrix contains a duplicate cell",
            ));
        }
    }
    if w64_integrity.get("schema").and_then(serde_json::Value::as_str)
            != Some("tonepoet-reference-w64-exact-integrity/v1")
        || w64_integrity.get("status").and_then(serde_json::Value::as_str) != Some("passed")
        || w64_integrity.get("policy").and_then(serde_json::Value::as_str)
            != Some(tonepoet_pipeline::DSD_REFERENCE_POLICY_V16_KEY)
        || w64_integrity.get("parser_authority").and_then(serde_json::Value::as_str)
            != Some("independent_root_and_chunk_traversal_exact/v1")
        || w64_integrity.get("carrier_contract_digest").and_then(serde_json::Value::as_str)
            != Some("tonepoet-reference-carrier-probe/v2")
        || w64_integrity.get("declared_riff_extent_equals_physical_extent")
            .and_then(serde_json::Value::as_bool) != Some(true)
        || w64_integrity.get("declared_data_extent_equals_exact_payload")
            .and_then(serde_json::Value::as_bool) != Some(true)
        || w64_integrity.get("exact_frame_count_required")
            .and_then(serde_json::Value::as_bool) != Some(true)
        || w64_integrity.get("alignment_and_padding_validated")
            .and_then(serde_json::Value::as_bool) != Some(true)
        || w64_integrity.get("undeclared_trailing_bytes_rejected")
            .and_then(serde_json::Value::as_bool) != Some(true)
        || w64_integrity.get("independent_consumer").and_then(serde_json::Value::as_str)
            != Some("ffmpeg_full_decode_xerror")
        || w64_integrity.get("writer_trigger_classification")
            .and_then(serde_json::Value::as_str)
            != Some("encoded_all_zero_after_depth_and_effects_quantization; input_threshold_is_cell_specific_and_empirically_bounded")
        || w64_integrity.get("boundary_region_resolution_base_fraction")
            .and_then(serde_json::Value::as_str) != Some("1/510")
        || !exact_string_array(w64_integrity.get("enabled_depths"), &expected_depths)
        || w64_integrity.get("rates_hz").and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                values.iter().map(serde_json::Value::as_u64).collect::<Option<Vec<_>>>()
                    != Some(expected_rates.to_vec())
            })
        || w64_integrity.get("channels").and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                values.iter().map(serde_json::Value::as_u64).collect::<Option<Vec<_>>>()
                    != Some(expected_channels.to_vec())
            })
        || w64_integrity.get("cell_count").and_then(serde_json::Value::as_u64) != Some(60)
        || w64_integrity.get("malformed_all_zero_cell_count")
            .and_then(serde_json::Value::as_u64) != Some(observed_malformed_all_zero_cells)
        || w64_integrity.get("valid_all_zero_cell_count")
            .and_then(serde_json::Value::as_u64) != Some(observed_valid_all_zero_cells)
        || observed_malformed_all_zero_cells + observed_valid_all_zero_cells != 60
        || w64_integrity.get("uncharacterized_enabled_cells")
            .and_then(serde_json::Value::as_u64) != Some(0)
        || w64_integrity.get("same_path_qpcm_package_hash_counted_as_independent_packaging")
            .and_then(serde_json::Value::as_bool) != Some(false)
        || w64_integrity.get("w64_delivery_mode").and_then(serde_json::Value::as_str)
            != Some("terminal_qpcm_is_delivered_directly_after_exact_structure_and_full_consumer_traversal")
        || w64_cells.len() != 60
        || observed_cells.len() != 60
    {
        return Err(reference_toolchain_error(
            "the embedded exact Wave64 integrity evidence is incomplete or non-canonical",
        ));
    }

    let carrier = report
        .get("analyzer_carrier")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded release-certification report has no analyzer carrier evidence",
            )
        })?;
    let known_defect = carrier
        .get("known_defect")
        .and_then(serde_json::Value::as_object);
    let silent_w64_header_finalization_defect = carrier
        .get("silent_w64_header_finalization_defect")
        .and_then(serde_json::Value::as_object);
    let direct_sox_path = carrier
        .get("direct_sox_path")
        .and_then(serde_json::Value::as_object);
    let float32_pipe_path = carrier
        .get("float32_pipe_path")
        .and_then(serde_json::Value::as_object);
    let capacity_probe = carrier
        .get("historical_streamed_wav_capacity_probe")
        .and_then(serde_json::Value::as_object);
    let f1_reference_gain = carrier
        .get("f1_reference_gain_regression")
        .and_then(serde_json::Value::as_object);
    let streamed_capacity: Option<
        tonepoet_pipeline::ReferenceStreamedWavCapacityEvidenceV3,
    > = carrier
        .get("streamed_wav_capacity")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let exact_string_array = |value: Option<&serde_json::Value>, expected: &[&str]| {
        value
            .and_then(serde_json::Value::as_array)
            .is_some_and(|actual| {
                actual.len() == expected.len()
                    && actual
                        .iter()
                        .zip(expected)
                        .all(|(actual, expected)| actual.as_str() == Some(*expected))
            })
    };
    let direct_argv_valid = direct_sox_path
        .and_then(|value| value.get("command_argv"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|args| {
            args.len() == 10
                && args[0].as_str() == Some("-S")
                && args[1].as_str() == Some("-D")
                && args[2].as_str().is_some_and(|path| !path.is_empty())
                && args[3].as_str() == Some("-n")
                && args[4].as_str() == Some("rate")
                && args[5].as_str() == Some("-v")
                && args[6].as_str() == Some("-L")
                && args[7].as_str() == Some("-s")
                && args[8].as_str() == Some("768000")
                && args[9].as_str() == Some("stats")
        });
    let float32_producer_valid = float32_pipe_path
        .and_then(|value| value.get("producer_argv"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|args| {
            args.len() == 17
                && args[0].as_str() == Some("-nostdin")
                && args[1].as_str() == Some("-hide_banner")
                && args[2].as_str() == Some("-nostats")
                && args[3].as_str() == Some("-loglevel")
                && args[4].as_str() == Some("error")
                && args[5].as_str() == Some("-i")
                && args[6].as_str().is_some_and(|path| !path.is_empty())
                && args[7..]
                    .iter()
                    .zip([
                        "-map",
                        "0:a:0",
                        "-vn",
                        "-sn",
                        "-dn",
                        "-c:a",
                        "pcm_f64le",
                        "-f",
                        "f64le",
                        "pipe:1",
                    ])
                    .all(|(actual, expected)| actual.as_str() == Some(expected))
        });
    let float32_consumer_valid = exact_string_array(
        float32_pipe_path.and_then(|value| value.get("consumer_argv")),
        &[
            "-S",
            "-D",
            "-t",
            "raw",
            "-e",
            "floating-point",
            "-b",
            "64",
            "-L",
            "-r",
            "48000",
            "-c",
            "1",
            "-",
            "-n",
            "rate",
            "-v",
            "-L",
            "-s",
            "768000",
            "stats",
        ],
    );
    let analytic_peak = known_defect
        .and_then(|value| value.get("analytic_peak_dbfs"))
        .and_then(serde_json::Value::as_f64);
    let direct_input_tp = known_defect
        .and_then(|value| value.get("reported_input_tp_dbtp"))
        .and_then(serde_json::Value::as_f64);
    let scaling_delta = known_defect
        .and_then(|value| value.get("scaling_delta_db"))
        .and_then(serde_json::Value::as_f64);
    let silent_w64_u64 = |field: &str| {
        silent_w64_header_finalization_defect
            .and_then(|value| value.get(field))
            .and_then(serde_json::Value::as_u64)
    };
    let silent_w64_bool = |field: &str| {
        silent_w64_header_finalization_defect
            .and_then(|value| value.get(field))
            .and_then(serde_json::Value::as_bool)
    };
    let silent_w64_str = |field: &str| {
        silent_w64_header_finalization_defect
            .and_then(|value| value.get(field))
            .and_then(serde_json::Value::as_str)
    };
    let silent_w64_header_finalization_defect_valid =
        silent_w64_str("status") == Some("sox_writer_defect_reproduced_and_bounded")
            && silent_w64_str("writer") == Some("sox_ng_14_8_0_1")
            && silent_w64_str("container") == Some("w64")
            && silent_w64_str("sample_encoding") == Some("float64")
            && silent_w64_u64("sample_rate_hz") == Some(88_200)
            && silent_w64_u64("channels") == Some(1)
            && silent_w64_u64("sample_frames") == Some(8_820)
            && silent_w64_u64("file_bytes") == Some(70_696)
            && silent_w64_u64("data_chunk_offset_bytes") == Some(112)
            && silent_w64_u64("payload_offset_bytes") == Some(136)
            && silent_w64_u64("payload_bytes_present") == Some(70_560)
            && silent_w64_u64("nonzero_riff_size_field") == Some(70_696)
            && silent_w64_u64("nonzero_data_chunk_size_field") == Some(70_584)
            && silent_w64_u64("silence_riff_size_field") == Some(136)
            && silent_w64_u64("silence_data_chunk_size_field") == Some(24)
            && silent_w64_u64("sox_reported_silence_frames") == Some(8_820)
            && silent_w64_bool("direct_ffmpeg_silence_opened") == Some(false)
            && silent_w64_bool("direct_ffmpeg_tone_opened") == Some(true)
            && silent_w64_bool("direct_ffmpeg_tiny_nonzero_opened") == Some(true)
            && silent_w64_bool("direct_ffmpeg_leading_silence_opened") == Some(true)
            && silent_w64_bool("direct_ffmpeg_trailing_silence_opened") == Some(true)
            && silent_w64_str("trigger_classification")
                == Some("historical_float64_single_amplitude_witness_only")
            && silent_w64_str("ffmpeg_disposition")
                == Some("correctly_refuses_declared_empty_w64_payload")
            && silent_w64_str("qualification_probe_disposition")
                == Some("superseded_by_v16_independent_exact_parser")
            && silent_w64_bool("exact_parser_rejected_silence") == Some(true)
            && silent_w64_str("exact_parser_diagnostic_code") == Some("DSD-REF-P0-026")
            && silent_w64_str("exact_parser_error").is_some_and(|value| {
                value.contains("root declares 136 bytes")
                    && value.contains("physical file contains 70696 bytes")
            })
            && silent_w64_str("exact_parser_diagnostic").is_some_and(|value| {
                value.starts_with("DSD-REF-P0-026:")
                    && value.contains("qualification all-zero Wave64 witness")
            })
            && silent_w64_str("production_disposition")
                == Some("malformed_w64_rejected_before_publication_DSD-REF-P0-026")
            && silent_w64_str("ffmpeg_error").is_some_and(|value| !value.is_empty());
    let corrected_peak = direct_sox_path
        .and_then(|value| value.get("reported_peak_dbtp"))
        .and_then(serde_json::Value::as_f64);
    let float32_analytic_peak = float32_pipe_path
        .and_then(|value| value.get("analytic_peak_dbfs"))
        .and_then(serde_json::Value::as_f64);
    let float32_reported_peak = float32_pipe_path
        .and_then(|value| value.get("reported_peak_dbtp"))
        .and_then(serde_json::Value::as_f64);
    let f1_pre_reported = f1_reference_gain
        .and_then(|value| value.get("pre_reported_input_tp_dbtp"))
        .and_then(serde_json::Value::as_f64);
    let f1_applied_gain = f1_reference_gain
        .and_then(|value| value.get("applied_gain_db"))
        .and_then(serde_json::Value::as_f64);
    let f1_expected_post = f1_reference_gain
        .and_then(|value| value.get("expected_post_from_pre_and_gain_dbtp"))
        .and_then(serde_json::Value::as_f64);
    let f1_post_reported = f1_reference_gain
        .and_then(|value| value.get("post_reported_input_tp_dbtp"))
        .and_then(serde_json::Value::as_f64);
    let f1_post_upper = f1_reference_gain
        .and_then(|value| value.get("post_conservative_upper_dbtp"))
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse::<tonepoet_pipeline::DbNano>().ok());
    let streamed_capacity_policy = serde_json::to_value(&manifest.streamed_wav_capacity)
        .map_err(|error| {
            reference_toolchain_error(format!(
                "cannot serialize the embedded streamed-WAV capacity contract: {error}"
            ))
        })?;
    let grid_bound_rendered = tonepoet_pipeline::REFERENCE_TRUE_PEAK_GRID_BOUND.render(false);
    if carrier.get("status").and_then(serde_json::Value::as_str) != Some("passed")
        || carrier.get("contract").and_then(serde_json::Value::as_str)
            != Some("tonepoet-reference-analyzer-carrier/v4")
        || carrier.get("routing_rule").and_then(serde_json::Value::as_str)
            != Some("float32_w64_ffmpeg_f64le_raw_to_sox_else_sox_path")
        || known_defect
            .and_then(|value| value.get("status"))
            .and_then(serde_json::Value::as_str)
            != Some("reproduced")
        || known_defect
            .and_then(|value| value.get("expected_scaling"))
            .and_then(serde_json::Value::as_str)
            != Some("2^31")
        || !silent_w64_header_finalization_defect_valid
        || direct_input_tp.is_none_or(|value| value <= 100.0)
        || scaling_delta.is_none_or(|value| {
            (value - 20.0 * (2_f64.powi(31)).log10()).abs() > 0.02
        })
        || analytic_peak
            .zip(direct_input_tp)
            .zip(scaling_delta)
            .is_none_or(|((analytic, direct), delta)| {
                ((direct - analytic) - delta).abs() > 1e-9
            })
        || direct_sox_path
            .and_then(|value| value.get("status"))
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        || direct_sox_path
            .and_then(|value| value.get("parser"))
            .and_then(serde_json::Value::as_str)
            != Some("sox_stats_pk_lev_db_v1")
        || direct_sox_path
            .and_then(|value| value.get("oversample_factor"))
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(tonepoet_pipeline::REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR))
        || direct_sox_path
            .and_then(|value| value.get("analytic_grid_bound_db"))
            .and_then(serde_json::Value::as_str)
            != Some(grid_bound_rendered.as_str())
        || direct_sox_path
            .and_then(|value| value.get("environment_policy"))
            .and_then(serde_json::Value::as_str)
            != Some("clear_and_set")
        || direct_sox_path
            .and_then(|value| value.get("environment"))
            .and_then(serde_json::Value::as_object)
            .is_none_or(|variables| {
                variables.len() != 1
                    || variables.get("LC_ALL").and_then(serde_json::Value::as_str) != Some("C")
            })
        || analytic_peak
            .zip(corrected_peak)
            .is_none_or(|(analytic, corrected)| (corrected - analytic).abs() > 0.02)
        || !direct_argv_valid
        || float32_pipe_path
            .and_then(|value| value.get("status"))
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        || float32_pipe_path
            .and_then(|value| value.get("carrier_depth"))
            .and_then(serde_json::Value::as_str)
            != Some("float32")
        || float32_pipe_path
            .and_then(|value| value.get("carrier_container"))
            .and_then(serde_json::Value::as_str)
            != Some("w64")
        || float32_pipe_path
            .and_then(|value| value.get("disk_intermediate"))
            .and_then(serde_json::Value::as_bool)
            != Some(false)
        || float32_pipe_path
            .and_then(|value| value.get("package_step"))
            .and_then(serde_json::Value::as_bool)
            != Some(false)
        || float32_pipe_path
            .and_then(|value| value.get("parser"))
            .and_then(serde_json::Value::as_str)
            != Some("sox_stats_pk_lev_db_v1")
        || float32_pipe_path
            .and_then(|value| value.get("environment_policy"))
            .and_then(serde_json::Value::as_str)
            != Some("clear_and_set")
        || float32_pipe_path
            .and_then(|value| value.get("environment"))
            .and_then(serde_json::Value::as_object)
            .is_none_or(|variables| {
                variables.len() != 1
                    || variables.get("LC_ALL").and_then(serde_json::Value::as_str) != Some("C")
            })
        || float32_analytic_peak
            .zip(float32_reported_peak)
            .is_none_or(|(analytic, reported)| (reported - analytic).abs() > 0.02)
        || !float32_producer_valid
        || !float32_consumer_valid
        || capacity_probe
            .and_then(|value| value.get("status"))
            .and_then(serde_json::Value::as_str)
            != Some("retained_conservative_admission_witness")
        || capacity_probe
            .and_then(|value| value.get("producer_argv"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(|args| args.len() != 10)
        || f1_reference_gain
            .and_then(|value| value.get("status"))
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        || f1_reference_gain
            .and_then(|value| value.get("target_rate_hz"))
            .and_then(serde_json::Value::as_u64)
            != Some(44_100)
        || f1_reference_gain
            .and_then(|value| value.get("channels"))
            .and_then(serde_json::Value::as_u64)
            != Some(1)
        || f1_reference_gain
            .and_then(|value| value.get("depth"))
            .and_then(serde_json::Value::as_str)
            != Some("float32")
        || f1_reference_gain
            .and_then(|value| value.get("final_target"))
            .and_then(serde_json::Value::as_str)
            != Some("wav_riff")
        || f1_reference_gain
            .and_then(|value| value.get("qpcm_container"))
            .and_then(serde_json::Value::as_str)
            != Some("w64")
        || f1_pre_reported.is_none_or(|value| !(-20.20..=-19.80).contains(&value))
        || f1_pre_reported
            .zip(f1_applied_gain)
            .zip(f1_expected_post)
            .is_none_or(|((pre, gain), expected)| ((pre + gain) - expected).abs() > 1e-9)
        || f1_expected_post
            .zip(f1_post_reported)
            .is_none_or(|(expected, reported)| (reported - expected).abs() > 0.03)
        || f1_post_upper.is_none_or(|value| value > tonepoet_pipeline::DbNano::REFERENCE_CEILING)
        || f1_reference_gain
            .and_then(|value| value.get("terminal_argv"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty)
        || f1_reference_gain
            .and_then(|value| value.get("package_argv"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty)
        || streamed_capacity.as_ref().is_none_or(|value| !value.is_canonical_v13())
        || report.get("streamed_wav_capacity") != carrier.get("streamed_wav_capacity")
        || report.get("streamed_wav_capacity_policy") != Some(&streamed_capacity_policy)
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification report has incomplete analyzer carrier evidence",
        ));
    }

    let analyzer = report
        .get("production_true_peak_analyzer")
        .and_then(serde_json::Value::as_object);
    let analyzer_rates = [
        44_100_u64, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
        705_600, 768_000,
    ];
    let analyzer_channels = [1_u64, 2];
    let analyzer_case_count = analyzer
        .and_then(|value| value.get("case_count"))
        .and_then(serde_json::Value::as_u64);
    let analyzer_under = analyzer
        .and_then(|value| value.get("worst_under_report_db"))
        .and_then(serde_json::Value::as_f64);
    let analyzer_authority = analyzer
        .and_then(|value| value.get("one_sided_authority_db"))
        .and_then(serde_json::Value::as_f64);
    let analyzer_digest_valid = analyzer
        .and_then(|value| value.get("evidence_digest"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
    let analyzer_cells_valid = analyzer
        .and_then(|value| value.get("per_rate_channel"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|cells| {
            let expected = analyzer_rates
                .into_iter()
                .flat_map(|rate| {
                    analyzer_channels.into_iter().map(move |channels| {
                        (
                            format!("{rate}/{channels}"),
                            if rate <= 96_000 { 94_u64 } else { 118_u64 },
                        )
                    })
                })
                .collect::<BTreeMap<_, _>>();
            let actual = cells
                .iter()
                .filter_map(|cell| {
                    let key = cell.get("cell")?.as_str()?;
                    let count = cell.get("case_count")?.as_u64()?;
                    let under = cell.get("worst_under_report_db")?.as_f64()?;
                    let over = cell.get("worst_over_report_db")?.as_f64()?;
                    if expected.get(key).copied() != Some(count)
                        || under > 0.110_000_001
                        || !over.is_finite()
                    {
                        return None;
                    }
                    Some(key.to_string())
                })
                .collect::<BTreeSet<_>>();
            cells.len() == expected.len()
                && actual == expected.keys().cloned().collect::<BTreeSet<_>>()
        });
    if !analyzer_cells_valid
        || analyzer
            .and_then(|value| value.get("status"))
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        || analyzer_case_count != Some(manifest.analyzer.required_case_count)
        || analyzer
            .and_then(|value| value.get("required_case_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(manifest.analyzer.required_case_count)
        || analyzer
            .and_then(|value| value.get("single_tone_case_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(960)
        || analyzer
            .and_then(|value| value.get("fixed_frequency_single_tone_case_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(768)
        || analyzer
            .and_then(|value| value.get("phase_aligned_multitone_case_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(240)
        || analyzer
            .and_then(|value| value.get("adversarial_case_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(200)
        || analyzer
            .and_then(|value| value.get("waveform_families"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                !values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .eq([
                        "single_tone",
                        "fixed_frequency_single_tone",
                        "phase_aligned_multitone",
                        "impulse",
                        "near_band_edge_burst",
                        "alternating_sign",
                        "broadband_deterministic",
                        "boundary_transient",
                    ])
            })
        || analyzer
            .and_then(|value| value.get("rates_hz"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                !values
                    .iter()
                    .filter_map(serde_json::Value::as_u64)
                    .eq(analyzer_rates)
            })
        || analyzer
            .and_then(|value| value.get("channels"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                !values
                    .iter()
                    .filter_map(serde_json::Value::as_u64)
                    .eq(analyzer_channels)
            })
        || analyzer
            .and_then(|value| value.get("normalized_frequencies_cycles_per_sample"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                let expected = [0.25_f64, 0.45];
                values.len() != expected.len()
                    || values.iter().zip(expected).any(|(value, expected)| {
                        value
                            .as_f64()
                            .is_none_or(|actual| (actual - expected).abs() > 1e-12)
                    })
            })
        || analyzer
            .and_then(|value| value.get("fixed_frequencies_hz"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                !values
                    .iter()
                    .filter_map(serde_json::Value::as_u64)
                    .eq([1_000, 20_000, 48_000, 70_000])
            })
        || analyzer
            .and_then(|value| value.get("fixed_frequency_max_normalized"))
            .and_then(serde_json::Value::as_f64)
            .is_none_or(|value| (value - 0.49).abs() > 1e-12)
        || analyzer
            .and_then(|value| value.get("fixed_frequency_duration_seconds"))
            .and_then(serde_json::Value::as_f64)
            .is_none_or(|value| (value - 0.25).abs() > 1e-12)
        || analyzer
            .and_then(|value| value.get("phases_radians"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                let expected = [0.0_f64, std::f64::consts::FRAC_PI_4];
                values.len() != expected.len()
                    || values.iter().zip(expected).any(|(value, expected)| {
                        value
                            .as_f64()
                            .is_none_or(|actual| (actual - expected).abs() > 1e-12)
                    })
            })
        || analyzer
            .and_then(|value| value.get("analytic_true_peak_levels_dbfs"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                let expected = [-120.003_f64, -12.003, -0.5];
                values.len() != expected.len()
                    || values.iter().zip(expected).any(|(value, expected)| {
                        value
                            .as_f64()
                            .is_none_or(|actual| (actual - expected).abs() > 1e-12)
                    })
            })
        || analyzer
            .and_then(|value| value.get("durations_seconds"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                let expected = [0.125_f64, 0.5];
                values.len() != expected.len()
                    || values.iter().zip(expected).any(|(value, expected)| {
                        value
                            .as_f64()
                            .is_none_or(|actual| (actual - expected).abs() > 1e-12)
                    })
            })
        || analyzer
            .and_then(|value| value.get("peak_positions"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                !values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .eq(["early", "late"])
            })
        || analyzer
            .and_then(|value| {
                value.get("aligned_multitone_normalized_frequencies_cycles_per_sample")
            })
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                let expected = [0.03125_f64, 0.1171875, 0.2734375, 0.4453125];
                values.len() != expected.len()
                    || values.iter().zip(expected).any(|(value, expected)| {
                        value
                            .as_f64()
                            .is_none_or(|actual| (actual - expected).abs() > 1e-12)
                    })
            })
        || analyzer
            .and_then(|value| value.get("aligned_multitone_peak_offsets_samples"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                let expected = [0.25_f64, 0.75];
                values.len() != expected.len()
                    || values.iter().zip(expected).any(|(value, expected)| {
                        value
                            .as_f64()
                            .is_none_or(|actual| (actual - expected).abs() > 1e-12)
                    })
            })
        || analyzer
            .and_then(|value| value.get("aligned_multitone_duration_seconds"))
            .and_then(serde_json::Value::as_f64)
            .is_none_or(|value| (value - 0.25).abs() > 1e-12)
        || analyzer
            .and_then(|value| value.get("adversarial_peak_level_dbfs"))
            .and_then(serde_json::Value::as_f64)
            .is_none_or(|value| (value + 0.5).abs() > 1e-12)
        || analyzer
            .and_then(|value| value.get("adversarial_oracle_oversample_factor"))
            .and_then(serde_json::Value::as_u64)
            != Some(64)
        || analyzer
            .and_then(|value| value.get("maximum_adversarial_oracle_under_report_db"))
            .and_then(serde_json::Value::as_f64)
            .is_none_or(|value| value > 0.110_000_001)
        || analyzer
            .and_then(|value| value.get("maximum_empirical_resampler_component_db"))
            .and_then(serde_json::Value::as_f64)
            .is_none_or(|value| value > 0.058_074_044)
        || analyzer
            .and_then(|value| value.get("oversample_factor"))
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(
                tonepoet_pipeline::REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR,
            ))
        || analyzer
            .and_then(|value| value.get("analytic_grid_bound_db"))
            .and_then(serde_json::Value::as_str)
            != Some("0.041925957")
        || analyzer
            .and_then(|value| value.get("pinned_resampler_component_limit_db"))
            .and_then(serde_json::Value::as_f64)
            .is_none_or(|value| (value - 0.058_074_043).abs() > 1e-12)
        || analyzer
            .and_then(|value| value.get("reporting_quantization_component_db"))
            .and_then(serde_json::Value::as_f64)
            .is_none_or(|value| (value - 0.01).abs() > 1e-12)
        || analyzer
            .and_then(|value| value.get("analyzer_residual_sum_db"))
            .and_then(serde_json::Value::as_f64)
            .is_none_or(|value| (value - 0.1).abs() > 1e-12)
        || analyzer_under.is_none_or(|under| under > 0.110_000_001)
        || analyzer_authority.is_none_or(|authority| (authority - 0.11).abs() > 1e-12)
        || analyzer
            .and_then(|value| value.get("maximum_intersample_delta_db"))
            .and_then(serde_json::Value::as_f64)
            .is_none_or(|delta| delta <= 2.8)
        || analyzer
            .and_then(|value| value.get("monotonic_per_cell"))
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || analyzer
            .and_then(|value| value.get("nonzero_near_silence_remained_finite"))
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || !analyzer_digest_valid
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification report has incomplete analyzer evidence",
        ));
    }
    let chain = report
        .get("production_measurement_gain_terminal_chain")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded release-certification report has no production gain chain",
            )
        })?;
    for required in [
        "planner_render_end_to_end",
        "reference_constrained",
        "native_exact",
        "fixed_exact",
        "normalize",
        "verified_silence",
        "dither_semantics",
        "native_unsafe_refusal",
        "fixed_unsafe_refusal",
        "strict_parser_input_tp_and_q_plus_e",
    ] {
        if !chain.contains_key(required) {
            return Err(reference_toolchain_error(format!(
                "the embedded release-certification report omits production chain case {required}"
            )));
        }
    }
    let source_integration = report
        .get("production_source_front_end_integration")
        .and_then(serde_json::Value::as_object);
    let native_source_matrix_valid = source_integration
        .and_then(|value| value.get("native_cases"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|rows| {
            let expected = ["dsf_uncompressed", "dsdiff_uncompressed"]
                .into_iter()
                .flat_map(|kind| {
                    [2_822_400_u64, 5_644_800, 11_289_600]
                        .into_iter()
                        .flat_map(move |rate| [1_u64, 2].into_iter().map(move |channels| (kind, rate, channels)))
                })
                .collect::<BTreeSet<_>>();
            let actual = rows
                .iter()
                .filter_map(|row| {
                    let kind = row.get("source_kind")?.as_str()?;
                    let rate = row.get("source_rate_hz")?.as_u64()?;
                    let channels = row.get("channels")?.as_u64()?;
                    let source_sha256 = row.get("source_sha256")?.as_str()?;
                    let materialized_sha256 = row.get("materialized_sha256")?.as_str()?;
                    let identity = row.get("materialization_identity_digest")?.as_str()?;
                    if row.get("hard_link")?.as_bool()?
                        || row.get("planner_render")?.as_str()? != "passed"
                        || source_sha256 != materialized_sha256
                        || ![source_sha256, materialized_sha256, identity]
                            .into_iter()
                            .all(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
                        || row
                            .get("render_args_sha256")
                            .and_then(serde_json::Value::as_str)
                            .is_none_or(|digest| digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    {
                        return None;
                    }
                    Some((kind, rate, channels))
                })
                .collect::<BTreeSet<_>>();
            rows.len() == expected.len() && actual == expected
        });
    if !native_source_matrix_valid
        || source_integration
        .and_then(|value| value.get("status"))
        .and_then(serde_json::Value::as_str)
        != Some("passed")
        || source_integration
            .and_then(|value| value.get("native_case_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(12)
        || source_integration
            .and_then(|value| value.get("dsdiff_dst")).and_then(|value| value.get("source_rate_hz"))
            .and_then(serde_json::Value::as_u64)
            != Some(2_822_400)
        || source_integration
            .and_then(|value| value.get("dsdiff_dst")).and_then(|value| value.get("channels"))
            .and_then(serde_json::Value::as_u64)
            != Some(2)
        || source_integration
            .and_then(|value| value.get("dsdiff_dst")).and_then(|value| value.get("cmpr_classification"))
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        || source_integration
            .and_then(|value| value.get("dsdiff_dst")).and_then(|value| value.get("dstc_verification"))
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        || source_integration
            .and_then(|value| value.get("dsdiff_dst")).and_then(|value| value.get("canonical_dff_readback"))
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        || source_integration
            .and_then(|value| value.get("dsdiff_dst")).and_then(|value| value.get("planner_render"))
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        || source_integration
            .and_then(|value| value.get("dsdiff_dst")).and_then(|value| value.get("render_args_sha256"))
            .and_then(serde_json::Value::as_str)
            .is_none_or(|digest| {
                digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        || source_integration
            .and_then(|value| value.get("dsdiff_dst")).and_then(|value| value.get("executed_evidence_binding_schema"))
            .and_then(serde_json::Value::as_str)
            != Some("tonepoet-reference-executed-evidence/v2")
        || source_integration
            .and_then(|value| value.get("dsdiff_dst")).and_then(|value| value.get("materialization_identity_tamper_rejected"))
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || source_integration
            .and_then(|value| value.get("dsdiff_dst")).and_then(|value| value.get("materialization_identity_digest"))
            .and_then(serde_json::Value::as_str)
            .is_none_or(|digest| {
                digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        || source_integration
            .and_then(|value| value.get("sacd_dsd"))
            .and_then(serde_json::Value::as_str)
            != Some("unavailable:DSD-REF-P0-023")
        || source_integration
            .and_then(|value| value.get("sacd_dst"))
            .and_then(serde_json::Value::as_str)
            != Some("unavailable:DSD-REF-P0-023")
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification report has incomplete production source-front-end evidence",
        ));
    }
    let dst = report
        .get("dst_independent_oracle")
        .and_then(serde_json::Value::as_object);
    if dst.and_then(|value| value.get("status")).and_then(serde_json::Value::as_str)
        != Some("passed")
        || dst
            .and_then(|value| value.get("total_case_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(12)
        || dst
            .and_then(|value| value.get("predictive_independent_oracle_case_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(6)
        || dst
            .and_then(|value| value.get("predictive_stereo_reference_oracle_case_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(3)
        || dst
            .and_then(|value| value.get("predictive_six_channel_decoder_only_case_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(3)
        || dst
            .and_then(|value| value.get("standards_literal_geometry_case_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(6)
        || dst
            .and_then(|value| value.get("predictive_reference_cells"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(|cells| {
                cells.len() != 1
                    || cells[0].get("source_rate_hz").and_then(serde_json::Value::as_u64)
                        != Some(2_822_400)
                    || cells[0].get("channels").and_then(serde_json::Value::as_u64)
                        != Some(2)
            })
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification report has incomplete or overstated DST evidence",
        ));
    }
    let packages = report
        .get("package_decode_back")
        .and_then(serde_json::Value::as_object);
    let package_evidence = packages.ok_or_else(|| {
        reference_toolchain_error(
            "the embedded release-certification report has no package/terminal evidence",
        )
    })?;
    validate_terminal_effects_certification(package_evidence, manifest)?;
    if packages
        .and_then(|value| value.get("status"))
        .and_then(serde_json::Value::as_str)
        != Some("passed")
        || packages
            .and_then(|value| value.get("case_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(480)
        || packages
            .and_then(|value| value.get("empirical_terminal_bound_case_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(60)
        || packages
            .and_then(|value| value.get("container_level_post_mutation_sample_identity"))
            .and_then(serde_json::Value::as_str)
            != Some("passed_for_420_admitted_non_w64_cells")
        || packages
            .and_then(|value| value.get("command_authority"))
            .and_then(serde_json::Value::as_str)
            != Some("exact PlannedExecutionStep vectors from plan_reference_dsd plus the shared production per-file metadata implementation")
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification report has incomplete package/terminal evidence",
        ));
    }
    let sample_identity = report
        .get("sample_identity_oracle")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded release-certification report has no measured sample-identity oracle",
            )
        })?;
    let route_counts = sample_identity
        .get("measured_route_case_counts")
        .and_then(serde_json::Value::as_object);
    let encoding_counts = sample_identity
        .get("measured_hash_encoding_case_counts")
        .and_then(serde_json::Value::as_object);
    let terminal_route_counts = sample_identity
        .get("measured_terminal_realization_route_case_counts")
        .and_then(serde_json::Value::as_object);
    let hash_codecs = sample_identity
        .get("hash_codecs")
        .and_then(serde_json::Value::as_object);
    let forbidden = sample_identity
        .get("forbidden_float64_w64_direct_route_regression")
        .and_then(serde_json::Value::as_object);
    let alignment = sample_identity
        .get("metadata_alignment_probes")
        .and_then(serde_json::Value::as_object);
    let w64_alignment = alignment
        .and_then(|value| value.get("w64_non_8_aligned_int24_mono"))
        .and_then(serde_json::Value::as_object);
    let riff_alignment = alignment
        .and_then(|value| value.get("riff_odd_byte_int24_mono"))
        .and_then(serde_json::Value::as_object);
    let production_metadata = sample_identity
        .get("production_metadata_mutation")
        .and_then(serde_json::Value::as_object);
    let production_mutator_counts = production_metadata
        .and_then(|value| value.get("primary_mutator_case_counts"))
        .and_then(serde_json::Value::as_object);
    let production_w64_rejection = production_metadata
        .and_then(|value| value.get("w64_rejection"))
        .and_then(serde_json::Value::as_object);
    if sample_identity.len() != 15
        || route_counts.is_none_or(|value| value.len() != 5)
        || encoding_counts.is_none_or(|value| value.len() != 9)
        || terminal_route_counts.is_none_or(|value| value.len() != 3)
        || hash_codecs.is_none_or(|value| value.len() != 3)
        || forbidden.is_none_or(|value| value.len() != 6)
        || sample_identity.get("schema").and_then(serde_json::Value::as_str)
            != Some("tonepoet-reference-sample-identity-oracle/v4")
        || sample_identity.get("status").and_then(serde_json::Value::as_str)
            != Some("passed")
        || sample_identity
            .get("route_authority")
            .and_then(serde_json::Value::as_str)
            != Some("typed_plan_carrier_path_role_target_depth_v2")
        || sample_identity
            .get("hash_format")
            .and_then(serde_json::Value::as_str)
            != Some(tonepoet_pipeline::REFERENCE_SAMPLE_HASH_FORMAT)
        || hash_codecs
            .and_then(|value| value.get("int24"))
            .and_then(serde_json::Value::as_str)
            != Some(ReferenceSampleHashEncoding::SignedInt24Le.ffmpeg_codec())
        || hash_codecs
            .and_then(|value| value.get("float32"))
            .and_then(serde_json::Value::as_str)
            != Some(ReferenceSampleHashEncoding::Float32Le.ffmpeg_codec())
        || hash_codecs
            .and_then(|value| value.get("float64"))
            .and_then(serde_json::Value::as_str)
            != Some(ReferenceSampleHashEncoding::Float64Le.ffmpeg_codec())
        || json_object_u64(route_counts, "qpcm:ffmpeg_direct") != Some(420)
        || json_object_u64(route_counts, "qpcm:sox_f64le_raw_stream") != Some(60)
        || json_object_u64(route_counts, "packaged:ffmpeg_direct") != Some(460)
        || json_object_u64(route_counts, "packaged:sox_f64le_raw_stream") != Some(20)
        || json_object_u64(route_counts, "post_metadata:ffmpeg_direct") != Some(420)
        || json_object_u64(encoding_counts, "qpcm:int24_le") != Some(360)
        || json_object_u64(encoding_counts, "qpcm:float32_le") != Some(60)
        || json_object_u64(encoding_counts, "qpcm:float64_le") != Some(60)
        || json_object_u64(encoding_counts, "packaged:int24_le") != Some(360)
        || json_object_u64(encoding_counts, "packaged:float32_le") != Some(60)
        || json_object_u64(encoding_counts, "packaged:float64_le") != Some(60)
        || json_object_u64(encoding_counts, "post_metadata:int24_le") != Some(340)
        || json_object_u64(encoding_counts, "post_metadata:float32_le") != Some(40)
        || json_object_u64(encoding_counts, "post_metadata:float64_le") != Some(40)
        || json_object_u64(terminal_route_counts, "r64:sox_f64le_raw_stream") != Some(60)
        || json_object_u64(terminal_route_counts, "qpcm:ffmpeg_direct") != Some(40)
        || json_object_u64(terminal_route_counts, "qpcm:sox_f64le_raw_stream") != Some(20)
        || sample_identity
            .get("package_identity_comparison_count")
            .and_then(serde_json::Value::as_u64)
            != Some(480)
        || sample_identity
            .get("post_metadata_identity_comparison_count")
            .and_then(serde_json::Value::as_u64)
            != Some(420)
        || production_metadata.is_none_or(|value| value.len() != 14)
        || production_metadata
            .and_then(|value| value.get("schema"))
            .and_then(serde_json::Value::as_str)
            != Some("tonepoet-reference-production-metadata-mutation/v1")
        || production_metadata
            .and_then(|value| value.get("entry_point"))
            .and_then(serde_json::Value::as_str)
            != Some("tonepoet::convert::pipeline::qualify_production_metadata_mutation")
        || production_metadata
            .and_then(|value| value.get("shared_production_implementation"))
            .and_then(serde_json::Value::as_str)
            != Some("apply_production_metadata_to_file")
        || production_metadata
            .and_then(|value| value.get("authoritative_tag_source"))
            .and_then(serde_json::Value::as_str)
            != Some("authoritative_metadata_tags")
        || production_metadata
            .and_then(|value| value.get("qualification_scope"))
            .and_then(serde_json::Value::as_str)
            != Some("authoritative_tag_mutation_without_artwork_or_replaygain")
        || production_metadata
            .and_then(|value| value.get("environment_policy"))
            .and_then(serde_json::Value::as_str)
            != Some("clear_and_set")
        || production_metadata
            .and_then(|value| value.get("environment"))
            .and_then(serde_json::Value::as_object)
            .is_none_or(|environment| {
                environment.len() != 1
                    || environment.get("LC_ALL").and_then(serde_json::Value::as_str)
                        != Some("C")
            })
        || json_object_u64(production_metadata, "admitted_cell_count") != Some(420)
        || production_mutator_counts.is_none_or(|value| value.len() != 3)
        || json_object_u64(production_mutator_counts, "ffmpeg") != Some(160)
        || json_object_u64(production_mutator_counts, "metaflac") != Some(180)
        || json_object_u64(production_mutator_counts, "wvtag") != Some(80)
        || json_object_u64(production_metadata, "m4a_atomicparsley_freeform_case_count")
            != Some(20)
        || json_object_u64(production_metadata, "post_mutation_sample_identity_count")
            != Some(420)
        || production_metadata
            .and_then(|value| value.get("post_mutation_container_contract_rechecked"))
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || production_metadata
            .and_then(|value| value.get("rf64_preservation"))
            .and_then(serde_json::Value::as_str)
            != Some("source_magic_RF64_requires_ffmpeg_-rf64_always")
        || production_w64_rejection.is_none_or(|value| value.len() != 5)
        || production_w64_rejection
            .and_then(|value| value.get("planner_entry_point"))
            .and_then(serde_json::Value::as_str)
            != Some("plan_request_for_track")
        || json_object_u64(production_w64_rejection, "planner_case_count") != Some(60)
        || production_w64_rejection
            .and_then(|value| value.get("metadata_entry_point"))
            .and_then(serde_json::Value::as_str)
            != Some("qualify_production_metadata_mutation")
        || json_object_u64(production_w64_rejection, "metadata_case_count") != Some(60)
        || production_w64_rejection
            .and_then(|value| value.get("code"))
            .and_then(serde_json::Value::as_str)
            != Some("DSD-REF-P0-024")
        || package_evidence.get("production_metadata_mutation_qualification")
            != sample_identity.get("production_metadata_mutation")
        || alignment.is_none_or(|value| value.len() != 4)
        || alignment
            .and_then(|value| value.get("schema"))
            .and_then(serde_json::Value::as_str)
            != Some("tonepoet-reference-metadata-alignment-probes/v1")
        || alignment
            .and_then(|value| value.get("status"))
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        || w64_alignment.is_none_or(|value| value.len() != 9)
        || json_object_u64(w64_alignment, "sample_rate_hz") != Some(88_200)
        || json_object_u64(w64_alignment, "channels") != Some(1)
        || json_object_u64(w64_alignment, "sample_count_before") != Some(8_820)
        || json_object_u64(w64_alignment, "sample_count_after_ffmpeg_w64_remux") != Some(8_821)
        || json_object_u64(w64_alignment, "data_bytes_before") != Some(26_460)
        || w64_alignment
            .and_then(|value| value.get("decoded_prefix_identity"))
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        || w64_alignment
            .and_then(|value| value.get("phantom_trailing_sample"))
            .and_then(serde_json::Value::as_str)
            != Some("000000")
        || w64_alignment
            .and_then(|value| value.get("disposition"))
            .and_then(serde_json::Value::as_str)
            != Some("known_muxer_defect_route_rejected")
        || w64_alignment
            .and_then(|value| value.get("rejection_code"))
            .and_then(serde_json::Value::as_str)
            != Some("DSD-REF-P0-024")
        || riff_alignment.is_none_or(|value| value.len() != 8)
        || json_object_u64(riff_alignment, "sample_rate_hz") != Some(88_200)
        || json_object_u64(riff_alignment, "channels") != Some(1)
        || json_object_u64(riff_alignment, "sample_count") != Some(8_821)
        || json_object_u64(riff_alignment, "data_bytes") != Some(26_463)
        || riff_alignment
            .and_then(|value| value.get("post_metadata_sample_identity"))
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        || riff_alignment
            .and_then(|value| value.get("production_entry_point"))
            .and_then(serde_json::Value::as_str)
            != Some("qualify_production_metadata_mutation")
        || riff_alignment
            .and_then(|value| value.get("production_primary_mutator"))
            .and_then(serde_json::Value::as_str)
            != Some("ffmpeg")
        || riff_alignment
            .and_then(|value| value.get("disposition"))
            .and_then(serde_json::Value::as_str)
            != Some("qualified")
        || sample_identity
            .get("independent_float64_riff_rf64_case_count")
            .and_then(serde_json::Value::as_u64)
            != Some(40)
        || sample_identity
            .get("oracle_independence")
            .and_then(serde_json::Value::as_str)
            != Some("float64_w64_source_sox_decode_vs_riff_rf64_output_ffmpeg_decode")
        || forbidden
            .and_then(|value| value.get("status"))
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        || forbidden
            .and_then(|value| value.get("attempted_mechanism"))
            .and_then(serde_json::Value::as_str)
            != Some(ReferenceDecodeMechanism::DirectFfmpeg.key())
        || forbidden
            .and_then(|value| value.get("required_mechanism"))
            .and_then(serde_json::Value::as_str)
            != Some(ReferenceDecodeMechanism::SoxFloat64W64RawStream.key())
        || forbidden
            .and_then(|value| value.get("rejected_role_count"))
            .and_then(serde_json::Value::as_u64)
            != Some(4)
        || forbidden
            .and_then(|value| value.get("rejected_roles"))
            .and_then(serde_json::Value::as_array)
            .is_none_or(|roles| {
                roles.iter().map(serde_json::Value::as_str).ne([
                    Some("r64_float64_w64"),
                    Some("qpcm_float64_w64"),
                    Some("packaged_float64_w64"),
                    Some("post_metadata_float64_w64"),
                ])
            })
        || forbidden
            .and_then(|value| value.get("mislabeled_carrier_regression"))
            .and_then(serde_json::Value::as_object)
            .is_none_or(|regression| {
                regression.len() != 3
                    || regression.get("status").and_then(serde_json::Value::as_str)
                        != Some("passed")
                    || regression
                        .get("attempted_path_role")
                        .and_then(serde_json::Value::as_str)
                        != Some("qpcm_w64_as_packaged_riff")
                    || regression
                        .get("rejected_before_command_construction")
                        .and_then(serde_json::Value::as_bool)
                        != Some(true)
            })
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification report has incomplete or false \
             decoded-sample route evidence",
        ));
    }

    let decode_routes = packages
        .and_then(|value| value.get("decode_route_table"))
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded release-certification report omits the compiled decode-route table",
            )
        })?;
    if decode_routes.len() != tonepoet_pipeline::REFERENCE_DECODE_ROUTE_RULES.len() {
        return Err(reference_toolchain_error(
            "the embedded release-certification report decode-route table has the wrong \
             cardinality",
        ));
    }
    for rule in tonepoet_pipeline::REFERENCE_DECODE_ROUTE_RULES {
        let key = format!(
            "{}:{}",
            rule.role_class().key(),
            rule.hash_encoding().key(),
        );
        let entry = decode_routes
            .get(&key)
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                reference_toolchain_error(format!(
                    "the embedded release-certification report omits decode route {key}",
                ))
            })?;
        if entry.len() != 3
            || entry
            .get("bit_depth")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(rule.bit_depth().bits()))
            || entry
                .get("mechanism")
                .and_then(serde_json::Value::as_str)
                != Some(rule.mechanism().key())
            || entry
                .get("hash_encoding")
                .and_then(serde_json::Value::as_str)
                != Some(rule.hash_encoding().key())
        {
            return Err(reference_toolchain_error(format!(
                "the embedded release-certification report decode route {key} disagrees \
                 with the compiled v15 authority",
            )));
        }
    }

    let cells = report
        .get("qualified_cell_contract")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            reference_toolchain_error(
                "the embedded release-certification report has no cell contract",
            )
        })?;
    if cells
        .get("expanded_supported_cell_count")
        .and_then(serde_json::Value::as_u64)
        != Some(manifest.cell_contract.expanded_supported_cell_count)
        || cells
            .get("expanded_supported_cell_digest")
            .and_then(serde_json::Value::as_str)
            != Some(manifest.cell_contract.expanded_supported_cell_digest.as_str())
    {
        return Err(reference_toolchain_error(
            "the embedded release-certification report disagrees with the v8 cell contract",
        ));
    }
    Ok(certified_mutators)
}

fn validate_embedded_reference_policy_tables(
    manifest: &EmbeddedReferenceQualification,
) -> Result<(), TrackExecutionError> {
    use tonepoet_pipeline::{
        resolve_reference_profile, terminal_realization_bound, typed_b6_profile,
        DsdRate, DsdReconstructionSelection, PcmBitDepth, ResolvedDsdProfile,
    };

    const ANALYZER_RATES: [u32; 10] = [
        44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
        705_600, 768_000,
    ];
    const ANALYZER_CHANNELS: [u16; 2] = [1, 2];
    const ANALYZER_FREQUENCIES: [&str; 2] = ["0.250000000", "0.450000000"];
    const ANALYZER_PHASES: [&str; 2] = ["0.0000000000000000", "0.7853981633974483"];
    const ANALYZER_LEVELS: [&str; 3] = ["-120.003000000", "-12.003000000", "-0.500000000"];
    const ANALYZER_DURATIONS: [&str; 2] = ["0.125000000", "0.500000000"];
    const ANALYZER_POSITIONS: [&str; 2] = ["early", "late"];
    const ANALYZER_FIXED_FREQUENCIES_HZ: [u32; 4] = [1_000, 20_000, 48_000, 70_000];
    const ANALYZER_WAVEFORMS: [&str; 8] = [
        "single_tone",
        "fixed_frequency_single_tone",
        "phase_aligned_multitone",
        "impulse",
        "near_band_edge_burst",
        "alternating_sign",
        "broadband_deterministic",
        "boundary_transient",
    ];
    const ANALYZER_MULTITONE_FREQUENCIES: [&str; 4] = [
        "0.031250000",
        "0.117187500",
        "0.273437500",
        "0.445312500",
    ];
    const ANALYZER_MULTITONE_OFFSETS: [&str; 2] = ["0.250000000", "0.750000000"];
    const ANALYZER_FLOAT32_PRODUCER_ARGS: [&str; 17] = [
        "-nostdin",
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "error",
        "-i",
        "{carrier_w64}",
        "-map",
        "0:a:0",
        "-vn",
        "-sn",
        "-dn",
        "-c:a",
        "pcm_f64le",
        "-f",
        "f64le",
        "pipe:1",
    ];
    const ANALYZER_RAW_INPUT_ARGS: [&str; 12] = [
        "-t",
        "raw",
        "-e",
        "floating-point",
        "-b",
        "64",
        "-L",
        "-r",
        "{sample_rate_hz}",
        "-c",
        "{channels}",
        "-",
    ];
    const ANALYZER_RAW_CONSUMER_ARGS: [&str; 21] = [
        "-S",
        "-D",
        "-t",
        "raw",
        "-e",
        "floating-point",
        "-b",
        "64",
        "-L",
        "-r",
        "{sample_rate_hz}",
        "-c",
        "{channels}",
        "-",
        "-n",
        "rate",
        "-v",
        "-L",
        "-s",
        "{sample_rate_hz_x16}",
        "stats",
    ];
    const ANALYZER_DIRECT_ARGS: [&str; 10] = [
        "-S",
        "-D",
        "{carrier_w64}",
        "-n",
        "rate",
        "-v",
        "-L",
        "-s",
        "{sample_rate_hz_x16}",
        "stats",
    ];
    let expected_environment = std::collections::BTreeMap::from([(
        "LC_ALL".to_string(),
        "C".to_string(),
    )]);
    const PACKAGE_PRODUCER_ARGS: [&str; 11] = [
        "-S", "-D", "{qpcm_w64}", "-t", "raw", "-e", "floating-point", "-b", "64", "-L", "-",
    ];
    const PACKAGE_CONSUMER_ARGS: [&str; 24] = [
        "-y", "-hide_banner", "-nostdin", "-f", "f64le", "-ar", "{sample_rate_hz}",
        "-ac", "{channels}", "-i", "pipe:0", "-map", "0:a:0", "-map_metadata", "-1",
        "-vn", "-sn", "-dn", "-c:a", "pcm_f64le", "-f", "wav", "{rf64_args}", "{output}",
    ];
    if manifest.packaging.schema != "tonepoet-reference-lossless-packaging/v3"
        || !manifest
            .packaging
            .float64_wav_targets
            .iter()
            .map(String::as_str)
            .eq(["wav_riff", "wav_rf64"])
        || manifest.packaging.producer_tool != "sox_ng"
        || !manifest
            .packaging
            .producer_args_template
            .iter()
            .map(String::as_str)
            .eq(PACKAGE_PRODUCER_ARGS)
        || manifest.packaging.consumer_tool != "ffmpeg"
        || !manifest
            .packaging
            .consumer_args_template
            .iter()
            .map(String::as_str)
            .eq(PACKAGE_CONSUMER_ARGS)
        || !manifest
            .packaging
            .rf64_args
            .iter()
            .map(String::as_str)
            .eq(["-rf64", "always"])
        || manifest.packaging.transport != "direct_stdout_to_stdin_no_shell"
        || manifest.packaging.stream_encoding != "pcm_f64le"
        || manifest.packaging.stream_framing != "headerless_raw_pcm"
        || manifest.packaging.endianness != "little"
        || manifest.packaging.disk_intermediate
        || manifest.packaging.environment_policy != "clear_and_set"
        || manifest.packaging.environment != expected_environment
        || manifest.packaging.forbidden_route
            != "ffmpeg_direct_decode_of_float64_qpcm_w64"
        || manifest.packaging.w64_delivery_mode
            != "terminal_qpcm_direct_delivery_after_exact_structure_and_independent_consumer_traversal"
        || manifest.packaging.w64_same_path_hash_disposition
            != "identity continuity only; not independent packaging evidence"
    {
        return Err(reference_toolchain_error(
            "embedded Float64 package contract disagrees with the compiled v16 policy",
        ));
    }
    let expected_w64_invariants = [
        "declared_riff_extent_equals_physical_file_extent",
        "declared_data_extent_equals_exact_pcm_payload",
        "complete_alignment_valid_chunk_traversal",
        "exact_upstream_r64_frame_authority_for_terminal_qpcm",
        "no_undeclared_trailing_bytes",
        "cell_specific_boundary_region_bracketed_at_1_over_510_base",
        "independent_ffmpeg_full_decode_xerror",
    ];
    let expected_w64_rates = [
        44_100_u32, 48_000, 88_200, 96_000, 176_400,
        192_000, 352_800, 384_000, 705_600, 768_000,
    ];
    if manifest.w64_integrity.schema != "tonepoet-reference-w64-exact-integrity/v1"
        || manifest.w64_integrity.parser != "independent_root_and_chunk_traversal_exact/v1"
        || manifest.w64_integrity.carrier_contract_digest
            != "tonepoet-reference-carrier-probe/v2"
        || manifest.w64_integrity.production_disposition
            != "reject_before_publication_with_DSD-REF-P0-026"
        || !manifest
            .w64_integrity
            .required_invariants
            .iter()
            .map(String::as_str)
            .eq(expected_w64_invariants)
        || !manifest
            .w64_integrity
            .enabled_depths
            .iter()
            .map(String::as_str)
            .eq(["int24", "float32", "float64"])
        || manifest.w64_integrity.rates_hz.as_slice() != expected_w64_rates
        || manifest.w64_integrity.channels.as_slice() != [1_u16, 2_u16]
        || manifest.w64_integrity.required_characterization_cell_count != 60
        || manifest.w64_integrity.boundary_region_resolution_base_fraction != "1/510"
        || manifest.w64_integrity.trigger_claim
            != "encoded_all_zero_after_depth_and_effects_quantization; input threshold is measured per cell and is not assumed"
        || manifest
            .w64_integrity
            .same_path_qpcm_package_hash_is_independent_packaging_evidence
    {
        return Err(reference_toolchain_error(
            "embedded exact Wave64 integrity contract disagrees with the compiled v16 policy",
        ));
    }

    let direct_route = ReferenceDecodeMechanism::DirectFfmpeg.key();
    let streamed_route = ReferenceDecodeMechanism::SoxFloat64W64RawStream.key();
    let routes = &manifest.sample_identity.routes;
    let codecs = &manifest.sample_identity.hash_codecs;
    if manifest.sample_identity.schema != "tonepoet-reference-sample-identity/v7"
        || manifest.sample_identity.route_authority
            != "typed_plan_carrier_path_role_target_depth_v2"
        || routes.r64_float64_w64 != streamed_route
        || routes.qpcm_int24_w64 != direct_route
        || routes.qpcm_float32_w64 != direct_route
        || routes.qpcm_float64_w64 != streamed_route
        || routes.packaged_int24_w64 != direct_route
        || routes.packaged_float32_w64 != direct_route
        || routes.packaged_float64_w64 != streamed_route
        || routes.packaged_non_w64 != direct_route
        || routes.post_metadata_int24_w64 != direct_route
        || routes.post_metadata_float32_w64 != direct_route
        || routes.post_metadata_float64_w64 != streamed_route
        || routes.post_metadata_non_w64 != direct_route
        || manifest.sample_identity.hash_format
            != tonepoet_pipeline::REFERENCE_SAMPLE_HASH_FORMAT
        || codecs.int24 != ReferenceSampleHashEncoding::SignedInt24Le.ffmpeg_codec()
        || codecs.float32 != ReferenceSampleHashEncoding::Float32Le.ffmpeg_codec()
        || codecs.float64 != ReferenceSampleHashEncoding::Float64Le.ffmpeg_codec()
        || manifest.sample_identity.forbidden_route
            != "ffmpeg_direct_decode_of_float64_w64"
        || manifest.sample_identity.oracle_independence
            != "float64_w64_source_sox_decode_vs_riff_rf64_output_ffmpeg_decode"
        || manifest.sample_identity.environment_policy != "clear_and_set"
        || manifest.sample_identity.environment != expected_environment
        || manifest.sample_identity.metadata_mutation.w64 != "error:DSD-REF-P0-024"
        || manifest.sample_identity.metadata_mutation.production_entry_point
            != "tonepoet::convert::pipeline::qualify_production_metadata_mutation"
        || manifest.sample_identity.metadata_mutation.shared_production_implementation
            != "apply_production_metadata_to_file"
        || manifest.sample_identity.metadata_mutation.authoritative_tag_source
            != "authoritative_metadata_tags"
        || manifest.sample_identity.metadata_mutation.qualification_scope
            != "authoritative_tag_mutation_without_artwork_or_replaygain"
        || manifest.sample_identity.metadata_mutation.environment_policy != "clear_and_set"
        || manifest.sample_identity.metadata_mutation.environment != expected_environment
        || !manifest
            .sample_identity
            .metadata_mutation
            .qualified_post_metadata_targets
            .iter()
            .map(String::as_str)
            .eq([
                "flac_native",
                "wav_riff",
                "wav_rf64",
                "aiff_native",
                "wavpack_native",
                "alac_m4a",
            ])
        || manifest.sample_identity.metadata_mutation.admitted_cell_count != 420
        || manifest.sample_identity.metadata_mutation.primary_mutator_case_counts.ffmpeg != 160
        || manifest.sample_identity.metadata_mutation.primary_mutator_case_counts.metaflac != 180
        || manifest.sample_identity.metadata_mutation.primary_mutator_case_counts.wvtag != 80
        || manifest.sample_identity.metadata_mutation.m4a_atomicparsley_freeform_case_count != 20
        || manifest.sample_identity.metadata_mutation.w64_rejection.planner_entry_point
            != "plan_request_for_track"
        || manifest.sample_identity.metadata_mutation.w64_rejection.planner_case_count != 60
        || manifest.sample_identity.metadata_mutation.w64_rejection.metadata_entry_point
            != "qualify_production_metadata_mutation"
        || manifest.sample_identity.metadata_mutation.w64_rejection.metadata_case_count != 60
        || manifest.sample_identity.metadata_mutation.w64_rejection.code != "DSD-REF-P0-024"
        || !manifest
            .sample_identity
            .metadata_mutation
            .post_mutation_container_contract_rechecked
        || manifest.sample_identity.metadata_mutation.rf64_preservation
            != "source_magic_RF64_requires_ffmpeg_-rf64_always"
        || manifest
            .sample_identity
            .metadata_mutation
            .w64_non_8_aligned_int24_mono_probe
            != "known_muxer_defect_phantom_sample"
        || manifest
            .sample_identity
            .metadata_mutation
            .riff_odd_byte_int24_mono_probe
            != "qualified_via_exact_production_ffmpeg_route"
        || manifest.sample_identity.metadata_mutation.runtime_identity_binding
            != "certified_report_to_compiled_store_to_runner_resolution"
        || manifest.sample_identity.metadata_mutation.execution_authority
            != "exact_canonical_path_plus_executable_sha256"
        || manifest.sample_identity.metadata_mutation.pre_mutation_reverification
            != "path_sha256_version_closure"
        || manifest.sample_identity.metadata_mutation.per_output_authority
            != "ReferenceToolchainEvidence.metadata_mutators_and_execution_fingerprint_v1"
    {
        return Err(reference_toolchain_error(
            "embedded decoded-sample identity contract disagrees with the compiled v16 policy",
        ));
    }
    if manifest.subprocess_environment.schema
        != "tonepoet-reference-subprocess-environment/v1"
        || manifest.subprocess_environment.policy != "clear_and_set"
        || manifest.subprocess_environment.variables != expected_environment
        || manifest.subprocess_environment.scope != "all_reference_external_commands"
    {
        return Err(reference_toolchain_error(
            "embedded Reference subprocess environment policy is not canonical",
        ));
    }
    if manifest.qualification_supervision.schema
        != "tonepoet-reference-qualification-supervision/v1"
        || manifest.qualification_supervision.command_deadline_seconds != 1_200
        || manifest.qualification_supervision.pipeline_deadline_seconds != 3_600
        || manifest
            .qualification_supervision
            .termination_reap_deadline_seconds
            != 10
        || manifest
            .qualification_supervision
            .poll_interval_milliseconds
            != 10
        || manifest.qualification_supervision.failure_contract != "terminate_and_reap_or_fail"
    {
        return Err(reference_toolchain_error(
            "embedded Reference qualification supervision policy is not canonical",
        ));
    }
    let streamed_capacity = &manifest.streamed_wav_capacity;
    if streamed_capacity.schema != "tonepoet-reference-streamed-wav-capacity/v1"
        || streamed_capacity.applies_to != "all_reference_float64_wav_streams"
        || streamed_capacity.riff_size_field_max
            != tonepoet_pipeline::REFERENCE_STREAMED_WAV_RIFF_SIZE_FIELD_MAX
        || streamed_capacity.riff_size_overhead_bytes
            != tonepoet_pipeline::REFERENCE_STREAMED_WAV_RIFF_SIZE_OVERHEAD_BYTES
        || streamed_capacity.max_audio_payload_bytes
            != tonepoet_pipeline::REFERENCE_STREAMED_WAV_MAX_AUDIO_PAYLOAD_BYTES
        || streamed_capacity.sample_encoding != "pcm_f64le"
        || streamed_capacity.bytes_per_sample
            != tonepoet_pipeline::REFERENCE_STREAMED_WAV_BYTES_PER_SAMPLE
        || streamed_capacity.duration_guard_frames
            != tonepoet_pipeline::REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES
        || streamed_capacity.admission_rule
            != "(ceil(duration_ns * target_rate_hz / 1000000000) + duration_guard_frames) * channels * bytes_per_sample <= max_audio_payload_bytes"
        || streamed_capacity.overflow_behavior
            != "sox_ng_unseekable_wav_overflow_riff_size_58_data_size_modulo_2^32"
        || streamed_capacity.overflow_error_code != "DSD-REF-P0-025"
        || streamed_capacity.future_lift
            != "append_only_policy_with_corrected_sox_ng_pin_or_independently_qualified_transport"
    {
        return Err(reference_toolchain_error(
            "embedded streamed-WAV capacity contract disagrees with the compiled v16 policy",
        ));
    }

    let carrier = &manifest.analyzer.carrier;
    if carrier.schema != "tonepoet-reference-analyzer-carrier/v4"
        || carrier.source_container != "carrier_sensitive_w64"
        || carrier.producer_tool != "ffmpeg"
        || !carrier
            .producer_args_template
            .iter()
            .map(String::as_str)
            .eq(ANALYZER_FLOAT32_PRODUCER_ARGS)
        || carrier.environment_policy != "clear_and_set"
        || carrier.environment != expected_environment
        || carrier.transport != "direct_stdout_to_stdin_no_shell"
        || carrier.consumer_tool != "sox_ng"
        || !carrier
            .consumer_input_args
            .iter()
            .map(String::as_str)
            .eq(ANALYZER_RAW_INPUT_ARGS)
        || !carrier
            .consumer_args_template
            .iter()
            .map(String::as_str)
            .eq(ANALYZER_RAW_CONSUMER_ARGS)
        || carrier.parser != "sox_stats_pk_lev_db_v1"
        || carrier.stream_encoding != "pcm_f64le"
        || carrier.stream_header != "headerless_raw_pcm"
        || carrier.disk_intermediate
        || carrier.exact_recontainer
        || carrier.overflow_fixture_required
        || carrier.overflow_behavior != "not_applicable_to_v15_analyzer"
        || carrier.known_ffmpeg_w64_defect
            != "ffmpeg_7_1_scales_sox_ieee_float64_w64_by_2^31"
        || carrier.routing_rule
            != "float32_w64_ffmpeg_f64le_raw_to_sox_else_sox_path"
        || carrier.direct_float32_input
            != "ffmpeg_direct_w64_to_headerless_f64le_stdout"
        || !carrier
            .direct_float32_consumer_args_template
            .iter()
            .map(String::as_str)
            .eq(ANALYZER_RAW_CONSUMER_ARGS)
        || carrier.known_sox_float32_w64_defect
            != "sox_ng_14_8_0_1_misscales_its_float32_w64_on_decode"
        || carrier.direct_tool != "sox_ng"
        || !carrier
            .direct_args_template
            .iter()
            .map(String::as_str)
            .eq(ANALYZER_DIRECT_ARGS)
        || carrier.oversample_factor
            != tonepoet_pipeline::REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR
        || carrier.oversampled_rate_rule != "sample_rate_hz * oversample_factor"
        || carrier.analytic_grid_bound_db
            != tonepoet_pipeline::REFERENCE_TRUE_PEAK_GRID_BOUND
        || carrier.analytic_grid_bound_db > manifest.analyzer.analyzer_residual_db
    {
        return Err(reference_toolchain_error(
            "embedded analyzer carrier contract disagrees with the compiled v16 policy",
        ));
    }
    let residual = &manifest.analyzer.residual_authority;
    let deadline = &manifest.analyzer.deadline_model;
    if manifest.analyzer.qualification_schema
        != "tonepoet-reference-analyzer-qualification/v6"
        || manifest.analyzer.required_case_count != 2_168
        || manifest.analyzer.target_rates_hz.as_slice() != ANALYZER_RATES.as_slice()
        || manifest.analyzer.channels.as_slice() != ANALYZER_CHANNELS.as_slice()
        || !manifest
            .analyzer
            .normalized_frequencies_cycles_per_sample
            .iter()
            .map(String::as_str)
            .eq(ANALYZER_FREQUENCIES)
        || manifest.analyzer.fixed_frequencies_hz.as_slice()
            != ANALYZER_FIXED_FREQUENCIES_HZ.as_slice()
        || manifest.analyzer.fixed_frequency_max_normalized != "0.490000000"
        || manifest.analyzer.fixed_frequency_duration_seconds != "0.250000000"
        || !manifest
            .analyzer
            .phases_radians
            .iter()
            .map(String::as_str)
            .eq(ANALYZER_PHASES)
        || !manifest
            .analyzer
            .analytic_true_peak_levels_dbfs
            .iter()
            .map(String::as_str)
            .eq(ANALYZER_LEVELS)
        || !manifest
            .analyzer
            .durations_seconds
            .iter()
            .map(String::as_str)
            .eq(ANALYZER_DURATIONS)
        || !manifest
            .analyzer
            .peak_positions
            .iter()
            .map(String::as_str)
            .eq(ANALYZER_POSITIONS)
        || !manifest
            .analyzer
            .waveform_families
            .iter()
            .map(String::as_str)
            .eq(ANALYZER_WAVEFORMS)
        || !manifest
            .analyzer
            .aligned_multitone_normalized_frequencies_cycles_per_sample
            .iter()
            .map(String::as_str)
            .eq(ANALYZER_MULTITONE_FREQUENCIES)
        || !manifest
            .analyzer
            .aligned_multitone_peak_offsets_samples
            .iter()
            .map(String::as_str)
            .eq(ANALYZER_MULTITONE_OFFSETS)
        || manifest.analyzer.aligned_multitone_duration_seconds != "0.250000000"
        || manifest.analyzer.adversarial_peak_level_dbfs != "-0.500000000"
        || manifest.analyzer.adversarial_oracle_oversample_factor != 64
        || manifest.analyzer.adversarial_case_count != 200
        || residual.schema != "tonepoet-reference-analyzer-residual-authority/v1"
        || residual.ideal_grid_component_db
            != tonepoet_pipeline::REFERENCE_TRUE_PEAK_GRID_BOUND
        || residual.pinned_resampler_component_limit_db
            != tonepoet_pipeline::REFERENCE_TRUE_PEAK_RESAMPLER_COMPONENT_LIMIT
        || residual.reporting_quantization_component_db
            != tonepoet_pipeline::DbNano::POST_FINAL_ACCEPTANCE_RESERVE
        || residual.analyzer_residual_sum_db != manifest.analyzer.analyzer_residual_db
        || residual.analyzer_residual_sum_db
            != tonepoet_pipeline::REFERENCE_TRUE_PEAK_ANALYZER_RESIDUAL
        || residual.one_sided_total_db
            != tonepoet_pipeline::REFERENCE_TRUE_PEAK_ONE_SIDED_AUTHORITY
        || residual.ideal_grid_component_db
            .checked_add(residual.pinned_resampler_component_limit_db)
            != Some(residual.analyzer_residual_sum_db)
        || residual.analyzer_residual_sum_db
            .checked_add(residual.reporting_quantization_component_db)
            != Some(residual.one_sided_total_db)
        || residual.resampler_authority_method
            != "pinned_sox_ng_14_8_0_1_empirical_matrix_with_64x_adversarial_oracle"
        || residual.status != "requires_pinned_real_tool_qualification"
        || deadline.schema != "tonepoet-reference-analyzer-deadline/v1"
        || deadline.startup_seconds
            != tonepoet_pipeline::REFERENCE_TRUE_PEAK_DEADLINE_STARTUP_SECONDS
        || deadline.minimum_oversampled_sample_values_per_second
            != tonepoet_pipeline::REFERENCE_TRUE_PEAK_MIN_OVERSAMPLED_SAMPLE_VALUES_PER_SECOND
        || deadline.duration_guard_frames
            != tonepoet_pipeline::REFERENCE_STREAMED_WAV_DURATION_GUARD_FRAMES
        || deadline.workload_rule
            != "(ceil(duration_ns * sample_rate_hz / 1000000000) + duration_guard_frames) * channels * oversample_factor"
        || deadline.deadline_rule
            != "startup_seconds + ceil(workload_sample_values / minimum_oversampled_sample_values_per_second)"
        || deadline.max_admitted_workload_sample_values
            != tonepoet_pipeline::REFERENCE_TRUE_PEAK_MAX_ADMITTED_WORKLOAD_SAMPLE_VALUES
        || deadline.max_deadline_seconds
            != tonepoet_pipeline::REFERENCE_TRUE_PEAK_MAX_DEADLINE_SECONDS
        || deadline.required_benchmark
            != "pinned_toolchain_throughput_floor_and_maximum_admission_arithmetic"
    {
        return Err(reference_toolchain_error(
            "embedded analyzer qualification matrix disagrees with the compiled v16 policy",
        ));
    }

    if manifest.riff_capacity.max_file_bytes
        != tonepoet_pipeline::REFERENCE_RIFF_MAX_FILE_BYTES
        || manifest.riff_capacity.muxer_structure_upper_bound_bytes
            != tonepoet_pipeline::REFERENCE_RIFF_MUXER_STRUCTURE_UPPER_BOUND_BYTES
        || manifest.riff_capacity.metadata_expansion_factor
            != tonepoet_pipeline::REFERENCE_RIFF_METADATA_EXPANSION_FACTOR
        || manifest
            .riff_capacity
            .source_derived_tag_artwork_upper_bound
            .trim()
            .is_empty()
        || manifest.riff_capacity.admission_rule.trim().is_empty()
    {
        return Err(reference_toolchain_error(
            "embedded RIFF capacity authority disagrees with the compiled policy",
        ));
    }

    if manifest.profiles.b1.kind != "integrated_rate_u"
        || manifest.profiles.b1.target_rate_hz != 44_100
        || resolve_reference_profile(
            DsdRate::Dsd64,
            44_100,
            DsdReconstructionSelection::Reference,
        )
        .map_err(|err| reference_toolchain_error(format!(
            "compiled B1 policy cannot be resolved: {err}"
        )))?
            != ResolvedDsdProfile::B1RateOnly
        || manifest.profiles.b2.kind != "integrated_rate_u"
        || manifest.profiles.b2.target_rate_hz != 48_000
        || resolve_reference_profile(
            DsdRate::Dsd64,
            48_000,
            DsdReconstructionSelection::Reference,
        )
        .map_err(|err| reference_toolchain_error(format!(
            "compiled B2 policy cannot be resolved: {err}"
        )))?
            != ResolvedDsdProfile::B2RateOnly
    {
        return Err(reference_toolchain_error(
            "embedded integrated-rate profiles disagree with the compiled policy",
        ));
    }

    let compiled_profiles = [
        (
            "b3",
            resolve_reference_profile(
                DsdRate::Dsd64,
                176_400,
                DsdReconstructionSelection::Reference,
            )
            .map_err(|err| reference_toolchain_error(format!(
                "compiled B3 policy cannot be resolved: {err}"
            )))?,
            &manifest.profiles.b3,
        ),
        (
            "b4",
            resolve_reference_profile(
                DsdRate::Dsd128,
                176_400,
                DsdReconstructionSelection::Reference,
            )
            .map_err(|err| reference_toolchain_error(format!(
                "compiled B4 policy cannot be resolved: {err}"
            )))?,
            &manifest.profiles.b4,
        ),
        (
            "b4w",
            resolve_reference_profile(
                DsdRate::Dsd128,
                176_400,
                DsdReconstructionSelection::Wideband,
            )
            .map_err(|err| reference_toolchain_error(format!(
                "compiled B4W policy cannot be resolved: {err}"
            )))?,
            &manifest.profiles.b4w,
        ),
        (
            "b5",
            resolve_reference_profile(
                DsdRate::Dsd256,
                176_400,
                DsdReconstructionSelection::Reference,
            )
            .map_err(|err| reference_toolchain_error(format!(
                "compiled B5 policy cannot be resolved: {err}"
            )))?,
            &manifest.profiles.b5,
        ),
    ];
    for (name, compiled, embedded) in compiled_profiles {
        validate_embedded_sinc_profile(name, compiled, embedded)?;
    }

    let b6 = &manifest.profiles.b6;
    let b6_common = EmbeddedSincProfile {
        passband_hz: b6.passband_hz,
        transition_hz: b6.transition_hz,
        center_hz: b6.center_hz,
        stopband_hz: b6.stopband_hz,
        attenuation_db: b6.attenuation_db,
    };
    validate_embedded_sinc_profile("b6", typed_b6_profile(), &b6_common)?;
    if b6.enabled {
        return Err(reference_toolchain_error(
            "embedded B6 profile must remain typed but disabled under policy v15",
        ));
    }

    let terminal_bounds = [
        (
            "int16_shibata",
            PcmBitDepth::Int16,
            &manifest.terminal_bounds.int16_shibata,
        ),
        (
            "int24_tpdf",
            PcmBitDepth::Int24,
            &manifest.terminal_bounds.int24_tpdf,
        ),
        (
            "float32",
            PcmBitDepth::Float32,
            &manifest.terminal_bounds.float32,
        ),
        (
            "float64",
            PcmBitDepth::Float64,
            &manifest.terminal_bounds.float64,
        ),
    ];
    const P0_TARGET_RATES_HZ: [u32; 10] = [
        44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
        705_600, 768_000,
    ];
    if manifest.terminal_bounds.target_rates_hz != P0_TARGET_RATES_HZ
        || manifest.terminal_bounds.derivation_schema
            != "tonepoet-reference-terminal-bound/v3"
        || manifest.terminal_bounds.post_final_acceptance_reserve_db
            != tonepoet_pipeline::DbNano::POST_FINAL_ACCEPTANCE_RESERVE
        || manifest.terminal_bounds.post_final_acceptance_reserve_basis
            != "one_analyzer_reporting_quantum"
    {
        return Err(reference_toolchain_error(
            "embedded terminal-bound cell domain disagrees with the compiled policy",
        ));
    }
    if manifest.terminal_bounds.post_final_acceptance_reserve_db
        != manifest.analyzer.reporting_uncertainty_db
    {
        return Err(reference_toolchain_error(
            "embedded terminal reserve must equal the analyzer reporting uncertainty",
        ));
    }
    for (name, depth, embedded) in terminal_bounds {
        let expected_realization = match depth {
            PcmBitDepth::Int16 => "int16-shibata-unqualified-no-conservative-bound",
            PcmBitDepth::Int24 => "int24-tpdf-2lsb",
            PcmBitDepth::Float32 => "float32-2^-23",
            PcmBitDepth::Float64 => "float64-sox-s32-effects-half-lsb-plus-f64-2^-51",
            PcmBitDepth::Int8 | PcmBitDepth::Int32 => {
                return Err(reference_toolchain_error(
                    "compiled terminal-bound table contains an unsupported depth",
                ));
            }
        };
        if embedded.realization != expected_realization {
            return Err(reference_toolchain_error(format!(
                "embedded terminal realization {name} disagrees with the compiled policy"
            )));
        }
        for target_rate_hz in P0_TARGET_RATES_HZ {
            let compiled = terminal_realization_bound(target_rate_hz, depth);
            if embedded.max_added_peak_fs_q63_ceil != compiled.max_added_peak_fs_q63_ceil
                || embedded.safe_pre_terminal_ceiling_dbtp
                    != compiled.safe_pre_terminal_ceiling_dbtp
            {
                return Err(reference_toolchain_error(format!(
                    "embedded terminal bound {name} at {target_rate_hz} Hz disagrees with the compiled policy"
                )));
            }
        }
    }

    validate_embedded_reference_cell_contract(&manifest.cell_contract)?;
    validate_embedded_qualification_report(manifest)?;

    Ok(())
}


fn validate_embedded_qualification_report(
    manifest: &EmbeddedReferenceQualification,
) -> Result<(), TrackExecutionError> {
    let report = &manifest.qualification_report;
    let report_bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16_report.md"
    ));
    let guidance = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/dsd_reference/tonepoet_dsd_to_pcm_guidance_evidence_based_v9.md"
    ));
    let decimation = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/dsd_reference/sox_ng_dsd_decimation_test_report_v5.md"
    ));
    let commission = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/dsd_reference/brief_dsd_reference_p0_scope_and_commission.md"
    ));
    let amendment = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/dsd_reference/brief_dsd_reference_p0_policy_v3_amendment.md"
    ));
    let analyzer_corrective_brief = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/dsd_reference/brief_dsd_reference_p0_corrective_analyzer_carrier.md"
    ));
    let runtime_defaults_corrective_brief = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/dsd_reference/brief_dsd_reference_p0_corrective_runtime_defaults.md"
    ));
    let parse = |label: &str, value: &str| {
        Sha256Digest::from_hex(value).map_err(|error| {
            reference_toolchain_error(format!(
                "invalid embedded {label} qualification digest: {error}"
            ))
        })
    };
    if report.schema != "tonepoet-dsd-reference-policy-qualification-report/v1"
        || report.path
            != "tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16_report.md"
        || parse("policy report", &report.sha256)? != Sha256Digest::of_bytes(report_bytes)
        || parse("guidance", &report.guidance_sha256)? != Sha256Digest::of_bytes(guidance)
        || parse("decimation report", &report.decimation_report_sha256)?
            != Sha256Digest::of_bytes(decimation)
        || parse("commission", &report.commission_sha256)?
            != Sha256Digest::of_bytes(commission)
        || parse("v3 amendment", &report.amendment_sha256)?
            != Sha256Digest::of_bytes(amendment)
        || parse(
            "analyzer corrective brief",
            &report.analyzer_corrective_brief_sha256,
        )? != Sha256Digest::of_bytes(analyzer_corrective_brief)
        || parse(
            "runtime-defaults corrective brief",
            &report.runtime_defaults_corrective_brief_sha256,
        )? != Sha256Digest::of_bytes(runtime_defaults_corrective_brief)
        || report.expanded_supported_cell_count
            != manifest.cell_contract.expanded_supported_cell_count
        || report.expanded_supported_cell_digest
            != manifest.cell_contract.expanded_supported_cell_digest
    {
        return Err(reference_toolchain_error(
            "embedded policy qualification report/evidence does not match the manifest",
        ));
    }
    Ok(())
}

fn embedded_error_code(error: &impl std::fmt::Display) -> String {
    let rendered = error.to_string();
    rendered
        .split(|character: char| character.is_whitespace() || character == ':' || character == ',')
        .find(|token| token.starts_with("DSD-REF-P0-"))
        .map_or_else(|| format!("error:unclassified:{rendered}"), |code| format!("error:{code}"))
}

fn embedded_target_from_key(key: &str) -> Option<tonepoet_pipeline::ResolvedOutputTarget> {
    use tonepoet_pipeline::ResolvedOutputTarget as Target;
    Some(match key {
        "flac_native" => Target::FlacNative,
        "wav_riff" => Target::WavRiff,
        "wav_rf64" => Target::WavRf64,
        "wav_w64" => Target::WavW64,
        "aiff_native" => Target::AiffNative,
        "wavpack_native" => Target::WavPackNative,
        "alac_m4a" => Target::AlacM4a,
        _ => return None,
    })
}

fn embedded_depth_from_key(key: &str) -> Option<tonepoet_pipeline::PcmBitDepth> {
    use tonepoet_pipeline::PcmBitDepth as Depth;
    Some(match key {
        "int16" => Depth::Int16,
        "int24" => Depth::Int24,
        "float32" => Depth::Float32,
        "float64" => Depth::Float64,
        _ => return None,
    })
}

fn validate_embedded_reference_cell_contract(
    contract: &EmbeddedQualifiedCellContract,
) -> Result<(), TrackExecutionError> {
    use tonepoet_pipeline::{
        resolve_reference_profile, validate_reference_target_depth, DsdRate,
        DsdReconstructionSelection,
    };

    const SOURCE_KINDS: [&str; 5] = [
        "dsf_uncompressed",
        "dsdiff_uncompressed",
        "dsdiff_dst",
        "sacd_dsd",
        "sacd_dst",
    ];
    const SOURCE_RATES: [u32; 3] = [2_822_400, 5_644_800, 11_289_600];
    const CHANNELS: [u16; 2] = [1, 2];
    const GAIN_MODES: [&str; 4] = ["reference", "native_level", "fixed", "normalize_peak"];
    const TARGET_RATES: [u32; 10] = [
        44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
        705_600, 768_000,
    ];
    const TARGETS: [&str; 7] = [
        "flac_native",
        "wav_riff",
        "wav_rf64",
        "wav_w64",
        "aiff_native",
        "wavpack_native",
        "alac_m4a",
    ];
    const DEPTHS: [&str; 4] = ["int16", "int24", "float32", "float64"];
    const DIMENSIONS: [&str; 10] = [
        "source_kind",
        "source_rate_hz",
        "channels",
        "profile_selection",
        "target_rate_hz",
        "resolved_profile",
        "target",
        "depth",
        "package_compression_level",
        "gain_mode",
    ];

    if contract.schema != "tonepoet-dsd-reference-qualified-cells/v3"
        || !contract
            .source_kinds
            .iter()
            .map(String::as_str)
            .eq(SOURCE_KINDS)
        || contract.source_rates_hz.as_slice() != SOURCE_RATES.as_slice()
        || contract.channels.as_slice() != CHANNELS.as_slice()
        || !contract
            .gain_modes
            .iter()
            .map(String::as_str)
            .eq(GAIN_MODES)
        || !contract
            .qualification_dimensions
            .iter()
            .map(String::as_str)
            .eq(DIMENSIONS)
    {
        return Err(reference_toolchain_error(
            "embedded qualified-cell dimensions disagree with the compiled P0 contract",
        ));
    }

    let mut source_cell_index = 0_usize;
    let mut supported_source_cells = BTreeSet::new();
    for source_kind in SOURCE_KINDS {
        for source_rate_hz in SOURCE_RATES {
            for channels in CHANNELS {
                let embedded = contract
                    .source_rate_channel_cells
                    .get(source_cell_index)
                    .ok_or_else(|| {
                        reference_toolchain_error(
                            "embedded source-kind/rate/channel matrix is truncated",
                        )
                    })?;
                source_cell_index += 1;
                if embedded.source_kind != source_kind
                    || embedded.source_rate_hz != source_rate_hz
                    || embedded.channels != channels
                {
                    return Err(reference_toolchain_error(
                        "embedded source-kind/rate/channel ordering disagrees with policy",
                    ));
                }
                let expected = match source_kind {
                    "dsf_uncompressed" | "dsdiff_uncompressed" => "supported",
                    "dsdiff_dst" if source_rate_hz == 2_822_400 && channels == 2 => {
                        "supported"
                    }
                    "dsdiff_dst" => "error:DSD-REF-P0-021",
                    "sacd_dsd" | "sacd_dst" => "error:DSD-REF-P0-023",
                    _ => unreachable!("source-kind table is fixed above"),
                };
                if embedded.result != expected {
                    return Err(reference_toolchain_error(format!(
                        "embedded source-kind/rate/channel cell {source_kind}/{source_rate_hz}/{channels} disagrees with compiled result: embedded={}, compiled={expected}",
                        embedded.result,
                    )));
                }
                if expected == "supported" {
                    supported_source_cells.insert((source_kind, source_rate_hz, channels));
                }
            }
        }
    }
    if source_cell_index != contract.source_rate_channel_cells.len() {
        return Err(reference_toolchain_error(
            "embedded source-kind/rate/channel matrix contains extra rows",
        ));
    }

    let mut profile_index = 0_usize;
    let mut supported_profiles = Vec::new();
    let mut seen_profiles = BTreeSet::new();
    for (source_rate_hz, source_rate) in [
        (2_822_400, DsdRate::Dsd64),
        (5_644_800, DsdRate::Dsd128),
        (11_289_600, DsdRate::Dsd256),
    ] {
        for (selection_key, selection) in [
            ("reference", DsdReconstructionSelection::Reference),
            ("wideband", DsdReconstructionSelection::Wideband),
        ] {
            for target_rate_hz in TARGET_RATES {
                let embedded = contract.profile_cells.get(profile_index).ok_or_else(|| {
                    reference_toolchain_error("embedded profile-cell matrix is truncated")
                })?;
                profile_index += 1;
                if embedded.source_rate_hz != source_rate_hz
                    || embedded.selection != selection_key
                    || embedded.target_rate_hz != target_rate_hz
                    || !seen_profiles.insert((source_rate_hz, selection_key, target_rate_hz))
                {
                    return Err(reference_toolchain_error(
                        "embedded profile-cell ordering or uniqueness disagrees with policy",
                    ));
                }
                let compiled = match resolve_reference_profile(source_rate, target_rate_hz, selection) {
                    Ok(profile) => profile.key().to_string(),
                    Err(error) => embedded_error_code(&error),
                };
                if embedded.result != compiled {
                    return Err(reference_toolchain_error(format!(
                        "embedded profile cell {source_rate_hz}/{selection_key}/{target_rate_hz} disagrees with compiled result: embedded={}, compiled={compiled}",
                        embedded.result,
                    )));
                }
                if !compiled.starts_with("error:") {
                    supported_profiles.push((
                        source_rate_hz,
                        selection_key,
                        target_rate_hz,
                        compiled,
                    ));
                }
            }
        }
    }
    if profile_index != contract.profile_cells.len() {
        return Err(reference_toolchain_error(
            "embedded profile-cell matrix contains extra rows",
        ));
    }

    let mut target_depth_index = 0_usize;
    let mut supported_target_depths = Vec::new();
    let mut seen_target_depths = BTreeSet::new();
    for target_key in TARGETS {
        let target = embedded_target_from_key(target_key).ok_or_else(|| {
            reference_toolchain_error(format!(
                "compiled target key table omitted {target_key}"
            ))
        })?;
        for depth_key in DEPTHS {
            let depth = embedded_depth_from_key(depth_key).ok_or_else(|| {
                reference_toolchain_error(format!(
                    "compiled depth key table omitted {depth_key}"
                ))
            })?;
            let embedded = contract
                .target_depth_cells
                .get(target_depth_index)
                .ok_or_else(|| {
                    reference_toolchain_error("embedded target/depth matrix is truncated")
                })?;
            target_depth_index += 1;
            if embedded.target != target_key
                || embedded.depth != depth_key
                || !seen_target_depths.insert((target_key, depth_key))
            {
                return Err(reference_toolchain_error(
                    "embedded target/depth ordering or uniqueness disagrees with policy",
                ));
            }
            let compiled = match validate_reference_target_depth(target, depth) {
                Ok(()) => "supported".to_string(),
                Err(error) => embedded_error_code(&error),
            };
            if embedded.result != compiled {
                return Err(reference_toolchain_error(format!(
                    "embedded target/depth cell {target_key}/{depth_key} disagrees with compiled result: embedded={}, compiled={compiled}",
                    embedded.result,
                )));
            }
            if compiled == "supported" {
                supported_target_depths.push((target_key, depth_key));
            }
        }
    }
    if target_depth_index != contract.target_depth_cells.len() {
        return Err(reference_toolchain_error(
            "embedded target/depth matrix contains extra rows",
        ));
    }

    let expected_compression_levels: [(&str, &[Option<u8>]); 7] = [
        (
            "flac_native",
            &[Some(0), Some(1), Some(2), Some(3), Some(4), Some(5), Some(6), Some(7), Some(8)],
        ),
        ("wav_riff", &[None]),
        ("wav_rf64", &[None]),
        ("wav_w64", &[None]),
        ("aiff_native", &[None]),
        ("wavpack_native", &[Some(0), Some(1), Some(2), Some(3)]),
        ("alac_m4a", &[None]),
    ];
    if contract.package_compression_levels.len() != expected_compression_levels.len() {
        return Err(reference_toolchain_error(
            "embedded package-compression matrix has the wrong size",
        ));
    }
    for (embedded, (target, levels)) in contract
        .package_compression_levels
        .iter()
        .zip(expected_compression_levels)
    {
        if embedded.target != target || embedded.levels.as_slice() != levels {
            return Err(reference_toolchain_error(format!(
                "embedded package-compression levels for {target} disagree with policy"
            )));
        }
    }

    let expected_required_args = [(
        "wavpack_native",
        "int24",
        ["-bits_per_raw_sample", "24"],
    )];
    if contract.package_required_args.len() != expected_required_args.len() {
        return Err(reference_toolchain_error(
            "embedded package-required-argv table has the wrong size",
        ));
    }
    for (embedded, (target, depth, args)) in contract
        .package_required_args
        .iter()
        .zip(expected_required_args)
    {
        if embedded.target != target
            || embedded.depth != depth
            || !embedded.args.iter().map(String::as_str).eq(args)
        {
            return Err(reference_toolchain_error(format!(
                "embedded required package argv for {target}/{depth} disagrees with policy"
            )));
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(b"tonepoet-dsd-reference-expanded-cells/v3\0");
    let mut count = 0_u64;
    for source_kind in SOURCE_KINDS {
        for channels in CHANNELS {
            for (source_rate_hz, selection, target_rate_hz, profile) in &supported_profiles {
                if !supported_source_cells.contains(&(source_kind, *source_rate_hz, channels)) {
                    continue;
                }
                for (target, depth) in &supported_target_depths {
                    let levels = expected_compression_levels
                        .iter()
                        .find_map(|(candidate, levels)| (*candidate == *target).then_some(*levels))
                        .ok_or_else(|| {
                            reference_toolchain_error(format!(
                                "compiled compression-level table omitted {target}"
                            ))
                        })?;
                    for level in levels {
                        let level = (*level)
                            .map_or_else(|| "none".to_string(), |value| value.to_string());
                        for gain_mode in GAIN_MODES {
                            hasher.update(format!(
                                "{source_kind}|{source_rate_hz}|{channels}|{selection}|{target_rate_hz}|{profile}|{target}|{depth}|{level}|{gain_mode}\n"
                            ));
                            count = count.checked_add(1).ok_or_else(|| {
                                reference_toolchain_error("qualified-cell count overflow")
                            })?;
                        }
                    }
                }
            }
        }
    }
    let digest = format!("{:x}", hasher.finalize());
    if count != contract.expanded_supported_cell_count
        || digest != contract.expanded_supported_cell_digest
    {
        return Err(reference_toolchain_error(format!(
            "expanded qualified-cell authority disagrees with compiled policy: embedded={}/{}, compiled={count}/{digest}",
            contract.expanded_supported_cell_count,
            contract.expanded_supported_cell_digest,
        )));
    }
    Ok(())
}

fn validate_embedded_sinc_profile(
    name: &str,
    compiled: tonepoet_pipeline::ResolvedDsdProfile,
    embedded: &EmbeddedSincProfile,
) -> Result<(), TrackExecutionError> {
    let Some((transition_hz, center_hz)) = compiled.sinc() else {
        return Err(reference_toolchain_error(format!(
            "compiled profile {name} is not an explicit-sinc profile"
        )));
    };
    let Some(passband_hz) = compiled.passband_hz() else {
        return Err(reference_toolchain_error(format!(
            "compiled profile {name} has no passband edge"
        )));
    };
    let Some(stopband_hz) = compiled.stopband_hz() else {
        return Err(reference_toolchain_error(format!(
            "compiled profile {name} has no stopband edge"
        )));
    };
    if embedded.passband_hz != passband_hz
        || embedded.transition_hz != transition_hz
        || embedded.center_hz != center_hz
        || embedded.stopband_hz != stopband_hz
        || embedded.attenuation_db != 180
    {
        return Err(reference_toolchain_error(format!(
            "embedded profile {name} disagrees with the compiled PB/TW/center/stopband/attenuation contract"
        )));
    }
    Ok(())
}

async fn attest_reference_toolchain(
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    front_end: tonepoet_pipeline::DsdInputFrontEnd,
    metadata_enabled: bool,
) -> Result<ReferenceToolchainEvidence, TrackExecutionError> {
    let raw = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16.json"
    ));
    let manifest = match parse_embedded_reference_qualification_wire(raw)
        .map_err(reference_toolchain_error)?
    {
        EmbeddedReferenceQualificationWire::Current(manifest) => manifest,
        EmbeddedReferenceQualificationWire::Historical(historical) => {
            return Err(reference_toolchain_error(format!(
                "the embedded policy artifact is historical schema v{} ({}) and cannot activate the v16 runtime",
                historical.schema_version, historical.policy,
            )));
        }
    };
    if manifest.schema_version != 16
        || manifest.policy != tonepoet_pipeline::DSD_REFERENCE_POLICY_V16_KEY
        || manifest.status != "qualified_release"
    {
        return Err(reference_toolchain_error(
            "the embedded policy artifact is not a qualified v16 release",
        ));
    }
    let certified_metadata_mutators = validate_embedded_release_certification(&manifest)?;
    if manifest.qualification_basis.trim().is_empty()
        || manifest.runtime_activation.trim().is_empty()
    {
        return Err(reference_toolchain_error(
            "the embedded qualification basis/runtime activation contract is empty",
        ));
    }
    validate_embedded_reference_policy_tables(&manifest)?;

    let (locked_sox_revision, locked_sox_nar_hash) = embedded_flake_lock_input("sox_ng")?;
    if manifest.sox_ng.version != tonepoet_pipeline::DSD_REFERENCE_SOX_NG_VERSION
        || manifest.sox_ng.revision != tonepoet_pipeline::DSD_REFERENCE_SOX_NG_REVISION
        || manifest.sox_ng.revision != locked_sox_revision
        || manifest.sox_ng.nar_hash != locked_sox_nar_hash
    {
        return Err(reference_toolchain_error(
            "the embedded SoX-ng source lock does not match the immutable policy",
        ));
    }

    let (locked_nixpkgs_revision, locked_nixpkgs_nar_hash) = embedded_flake_lock_input("nixpkgs")?;
    if manifest.ffmpeg.major_version != 7
        || manifest.ffmpeg.package_attribute != "ffmpeg_7-full"
        || manifest.ffmpeg.nixpkgs_revision != locked_nixpkgs_revision
        || manifest.ffmpeg.nixpkgs_nar_hash != locked_nixpkgs_nar_hash
    {
        return Err(reference_toolchain_error(
            "the embedded FFmpeg package lock does not match the immutable policy",
        ));
    }

    let front_end_requires_attestation = !matches!(
        front_end,
        tonepoet_pipeline::DsdInputFrontEnd::NativeUncompressed
    );
    if front_end_requires_attestation {
        if manifest.in_process.sacd_rs_build_identity != sacd_rs::REFERENCE_BUILD_ID {
            return Err(reference_front_end_error(
                "the in-process SACD/DST implementation does not match the qualified source identity",
            ));
        }
        let expected_fixture_identity =
            format!("sha256:{}", manifest.in_process.dst_fixture_digest);
        let expected_fixture_manifest_identity =
            format!("sha256:{}", manifest.in_process.dst_fixture_manifest_digest);
        let expected_fixture_provenance_identity =
            format!("sha256:{}", manifest.in_process.dst_fixture_provenance_digest);
        let commission_attestation_digest = Sha256Digest::of_bytes(
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/dsd_reference/brief_dsd_reference_p0_scope_and_commission.md"
            ))
            .as_bytes(),
        );
        let expected_commission_attestation_digest = Sha256Digest::from_hex(
            &manifest.in_process.commission_attestation_digest,
        )
        .map_err(|err| {
            reference_front_end_error(format!(
                "invalid commission attestation digest: {err}"
            ))
        })?;
        if expected_fixture_identity != sacd_rs::DST_REFERENCE_FIXTURE_CORPUS_ID
            || expected_fixture_manifest_identity != sacd_rs::DST_REFERENCE_FIXTURE_MANIFEST_ID
            || expected_fixture_provenance_identity
                != sacd_rs::DST_REFERENCE_FIXTURE_PROVENANCE_ID
            || commission_attestation_digest != expected_commission_attestation_digest
            || manifest.in_process.qualification_method
                != "compressed_dsd64_independent_oracle_plus_standards_literal_geometry_corpus"
            || manifest.in_process.dst_case_count != 12
            || manifest.in_process.six_channel_decoder_only_cases != 4
            || manifest.in_process.standards_literal_oracle_sha256
                != Sha256Digest::of_bytes(include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/crates/sacd-rs/src/dst/fixtures/verify_p0_raw_oracle.py"
                )))
                .to_hex()
        {
            return Err(reference_front_end_error(
                "the in-process DST fixture/provenance authority does not match the qualified release",
            ));
        }
    }

    if manifest.analyzer.reporting_uncertainty_db < tonepoet_pipeline::DbNano::ZERO
        || manifest.analyzer.analyzer_residual_db < tonepoet_pipeline::DbNano::ZERO
    {
        return Err(reference_toolchain_error(
            "qualified analyzer bounds must be non-negative",
        ));
    }

    let sox_ng = attest_external_reference_tool(
        ToolBinary::Sox,
        &manifest.sox_ng.version,
        None,
        &manifest.sox_ng.required_probe_markers,
        runner,
        cancel,
    )
    .await?;
    let ffmpeg = attest_external_reference_tool(
        ToolBinary::Ffmpeg,
        "",
        Some(manifest.ffmpeg.major_version),
        &manifest.ffmpeg.required_probe_markers,
        runner,
        cancel,
    )
    .await?;
    let metadata_mutators = if metadata_enabled {
        Some(ReferenceMetadataMutatorToolchain {
            metaflac: attest_certified_metadata_mutator(
                ToolBinary::Metaflac,
                certified_metadata_mutators
                    .identity(ToolBinary::Metaflac)
                    .expect("validated metaflac identity"),
                runner,
                cancel,
            )
            .await?,
            wvtag: attest_certified_metadata_mutator(
                ToolBinary::Wvtag,
                certified_metadata_mutators
                    .identity(ToolBinary::Wvtag)
                    .expect("validated wvtag identity"),
                runner,
                cancel,
            )
            .await?,
            atomic_parsley: attest_certified_metadata_mutator(
                ToolBinary::AtomicParsley,
                certified_metadata_mutators
                    .identity(ToolBinary::AtomicParsley)
                    .expect("validated AtomicParsley identity"),
                runner,
                cancel,
            )
            .await?,
        })
    } else {
        None
    };

    let platform_abi_digest = reference_platform_abi_digest();
    let runtime_dispatch_digest = reference_runtime_dispatch_digest();
    let actual_fixture_digest = sacd_rs::DST_REFERENCE_FIXTURE_CORPUS_ID
        .strip_prefix("sha256:")
        .ok_or_else(|| reference_toolchain_error(
            "the in-process DST fixture identity is not a SHA-256 authority",
        ))?;
    let dst_fixture_digest = Sha256Digest::from_hex(actual_fixture_digest)
        .map_err(|err| reference_toolchain_error(format!("invalid DST fixture digest: {err}")))?;
    let qualification_manifest_digest = Sha256Digest::of_bytes(raw.as_bytes());
    if qualification_manifest_digest != tonepoet_pipeline::qualification_manifest_digest() {
        return Err(reference_toolchain_error(
            "compiled and packaged qualification digests disagree",
        ));
    }

    Ok(ReferenceToolchainEvidence {
        qualification_manifest_digest,
        sox_ng,
        ffmpeg,
        metadata_mutators,
        sacd_rs_build_identity: sacd_rs::REFERENCE_BUILD_ID.to_string(),
        dst_fixture_digest,
        platform_abi_digest,
        runtime_dispatch_digest,
        reporting_uncertainty: manifest.analyzer.reporting_uncertainty_db,
        analyzer_residual: manifest.analyzer.analyzer_residual_db,
    })
}

pub(crate) fn reference_bound_metadata_executable(
    toolchain: &ReferenceToolchainEvidence,
    binary: ToolBinary,
) -> Option<BoundToolExecutable> {
    match binary {
        ToolBinary::Ffmpeg => Some(BoundToolExecutable {
            canonical_path: toolchain.ffmpeg.canonical_path.clone(),
            executable_sha256: toolchain.ffmpeg.executable_sha256,
        }),
        ToolBinary::Metaflac | ToolBinary::Wvtag | ToolBinary::AtomicParsley => toolchain
            .metadata_mutators
            .as_ref()?
            .identity(binary)
            .map(ReferenceMetadataMutatorIdentity::bound_executable),
        _ => None,
    }
}

pub(crate) fn reference_metadata_toolchains_match(
    left: &ReferenceToolchainEvidence,
    right: &ReferenceToolchainEvidence,
) -> bool {
    let primary_matches = left.ffmpeg.canonical_path == right.ffmpeg.canonical_path
        && left.ffmpeg.executable_sha256 == right.ffmpeg.executable_sha256
        && left.ffmpeg.reported_version == right.ffmpeg.reported_version
        && left.ffmpeg.closure_digest == right.ffmpeg.closure_digest;
    let mutator_matches = match (&left.metadata_mutators, &right.metadata_mutators) {
        (Some(left), Some(right)) => [
            (&left.metaflac, &right.metaflac),
            (&left.wvtag, &right.wvtag),
            (&left.atomic_parsley, &right.atomic_parsley),
        ]
        .into_iter()
        .all(|(left, right)| {
            left.canonical_path == right.canonical_path
                && left.executable_sha256 == right.executable_sha256
                && left.reported_version == right.reported_version
                && left.closure_digest == right.closure_digest
        }),
        (None, None) => true,
        _ => false,
    };
    primary_matches && mutator_matches
}

pub(crate) async fn verify_reference_metadata_toolchain_before_mutation(
    toolchain: &ReferenceToolchainEvidence,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<(), TrackExecutionError> {
    let mutators = toolchain.metadata_mutators.as_ref().ok_or_else(|| {
        reference_toolchain_error(
            "Reference metadata mutation has no attested metadata-mutator toolchain",
        )
    })?;
    for binary in [
        ToolBinary::Ffmpeg,
        ToolBinary::Metaflac,
        ToolBinary::Wvtag,
        ToolBinary::AtomicParsley,
    ] {
        let (canonical_path, executable_sha256, reported_version, closure_digest) = match binary {
            ToolBinary::Ffmpeg => (
                &toolchain.ffmpeg.canonical_path,
                toolchain.ffmpeg.executable_sha256,
                toolchain.ffmpeg.reported_version.as_str(),
                toolchain.ffmpeg.closure_digest,
            ),
            _ => {
                let identity = mutators.identity(binary).expect("closed mutator set");
                (
                    &identity.canonical_path,
                    identity.executable_sha256,
                    identity.reported_version.as_str(),
                    identity.closure_digest,
                )
            }
        };
        let resolved_path = runner.resolved_tool_path(binary).ok_or_else(|| {
            reference_toolchain_error(format!(
                "{} runner cannot prove the executable path before metadata mutation",
                binary.canonical_name()
            ))
        })?;
        let activation_path = resolve_policy_owned_reference_tool_path(binary).map_err(|error| {
            reference_toolchain_error(format!(
                "could not resolve policy-owned {} before metadata mutation: {error}",
                binary.canonical_name()
            ))
        })?;
        let compiled_path = compiled_reference_executable_path(binary).map_err(|error| {
            reference_toolchain_error(format!(
                "could not resolve compiled {} before metadata mutation: {error}",
                binary.canonical_name()
            ))
        })?;
        if resolved_path.as_path() != canonical_path.as_path()
            || activation_path.as_path() != canonical_path.as_path()
            || compiled_path.as_path() != canonical_path.as_path()
        {
            return Err(reference_toolchain_error(format!(
                "{} metadata path drift: attested {}, runtime {}, activation {}, compiled {}",
                binary.canonical_name(),
                canonical_path.display(),
                resolved_path.display(),
                activation_path.display(),
                compiled_path.display(),
            )));
        }
        let actual_sha256 = stable_file_sha256(canonical_path).map_err(|error| {
            reference_toolchain_error(format!(
                "could not re-hash {} before metadata mutation: {error}",
                canonical_path.display()
            ))
        })?;
        if actual_sha256 != executable_sha256 {
            return Err(reference_toolchain_error(format!(
                "{} metadata executable digest drift at {}",
                binary.canonical_name(),
                canonical_path.display()
            )));
        }
        let actual_closure = reference_installation_identity(
            binary,
            canonical_path,
            executable_sha256,
            reported_version,
        )?;
        if actual_closure != closure_digest {
            return Err(reference_toolchain_error(format!(
                "{} metadata closure identity drift",
                binary.canonical_name()
            )));
        }
        let bound = BoundToolExecutable {
            canonical_path: canonical_path.clone(),
            executable_sha256,
        };
        let output = runner
            .run_bound(reference_version_probe_command(binary)?, &bound, cancel)
            .await
            .map_err(|error| {
                reference_toolchain_error(format!(
                    "{} pre-mutation version probe failed: {error}",
                    binary.canonical_name()
                ))
            })?;
        let actual_version = parse_tool_version_output(
            binary,
            &output.stdout_tail,
            &output.stderr_tail,
        )
        .ok_or_else(|| {
            reference_toolchain_error(format!(
                "{} pre-mutation version is not parseable",
                binary.canonical_name()
            ))
        })?;
        if actual_version != reported_version {
            return Err(reference_toolchain_error(format!(
                "{} pre-mutation version drift: attested {}, runtime {}",
                binary.canonical_name(),
                reported_version,
                actual_version
            )));
        }
    }
    Ok(())
}

fn embedded_flake_lock_input(input: &str) -> Result<(String, String), TrackExecutionError> {
    let raw = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/flake.lock"));
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|err| reference_toolchain_error(format!("flake.lock is invalid: {err}")))?;
    let root_key = value
        .get("root")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| reference_toolchain_error("flake.lock has no root node"))?;
    let input_node = value
        .pointer(&format!("/nodes/{root_key}/inputs/{input}"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| reference_toolchain_error(format!("flake.lock has no {input} input")))?;
    let locked = value
        .pointer(&format!("/nodes/{input_node}/locked"))
        .ok_or_else(|| reference_toolchain_error(format!("flake.lock {input} input is unlocked")))?;
    let revision = locked
        .get("rev")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| reference_toolchain_error(format!("flake.lock {input} revision is absent")))?;
    let nar_hash = locked
        .get("narHash")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| reference_toolchain_error(format!("flake.lock {input} narHash is absent")))?;
    Ok((revision.to_string(), nar_hash.to_string()))
}

async fn attest_external_reference_tool(
    binary: ToolBinary,
    exact_version: &str,
    required_major_version: Option<u32>,
    required_probe_markers: &[String],
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<ReferenceToolIdentity, TrackExecutionError> {
    if !runner.tool_available(binary) {
        return Err(reference_toolchain_error(format!(
            "{} is not available",
            binary.canonical_name()
        )));
    }
    let path = runner.resolved_tool_path(binary).ok_or_else(|| {
        reference_toolchain_error(format!(
            "{} runner cannot prove the executable path it will spawn",
            binary.canonical_name()
        ))
    })?;
    let policy_path = resolve_policy_owned_reference_tool_path(binary).map_err(|err| {
        reference_toolchain_error(format!(
            "could not resolve policy-owned {}: {err}",
            binary.canonical_name()
        ))
    })?;
    let compiled_path = compiled_reference_executable_path(binary).map_err(|err| {
        reference_toolchain_error(format!(
            "could not resolve the compiled {} closure: {err}",
            binary.canonical_name()
        ))
    })?;
    if path != policy_path || path != compiled_path {
        return Err(reference_toolchain_error(format!(
            "{} runner path {}, activation path {}, and compiled closure path {} do not match",
            binary.canonical_name(),
            path.display(),
            policy_path.display(),
            compiled_path.display()
        )));
    }
    let executable_sha256 = stable_file_sha256(&path).map_err(|err| {
        reference_toolchain_error(format!("could not hash {}: {err}", path.display()))
    })?;
    let bound_executable = BoundToolExecutable {
        canonical_path: path.clone(),
        executable_sha256,
    };
    let version_probe = reference_version_probe_command(binary)?;
    let version_output = runner
        .run_bound(version_probe, &bound_executable, cancel)
        .await
        .map_err(|err| reference_toolchain_error(format!(
            "{} version probe failed: {err}",
            binary.canonical_name()
        )))?;
    if version_output.exit != ProcessExit::Code(0) {
        return Err(reference_toolchain_error(format!(
            "{} version probe did not exit successfully",
            binary.canonical_name()
        )));
    }
    let reported_version = parse_tool_version_output(
        binary,
        &version_output.stdout_tail,
        &version_output.stderr_tail,
    )
    .ok_or_else(|| {
        reference_toolchain_error(format!(
            "{} did not report a parseable version",
            binary.canonical_name()
        ))
    })?;
    if !exact_version.is_empty() && reported_version != exact_version {
        return Err(reference_toolchain_error(format!(
            "{} reported version {reported_version}, expected {exact_version}",
            binary.canonical_name()
        )));
    }
    if let Some(required_major) = required_major_version {
        let actual_major = reported_version
            .split('.')
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or_else(|| {
                reference_toolchain_error(format!(
                    "{} reported an unparseable version {reported_version}",
                    binary.canonical_name()
                ))
            })?;
        let required_for_binary = match binary {
            ToolBinary::Ffmpeg => required_major,
            _ => actual_major,
        };
        if actual_major != required_for_binary {
            return Err(reference_toolchain_error(format!(
                "{} reported major version {actual_major}, expected {required_for_binary}",
                binary.canonical_name()
            )));
        }
    }

    let probe = reference_behavior_probe_command(binary)?;
    let output = runner
        .run_bound(probe, &bound_executable, cancel)
        .await
        .map_err(|err| reference_toolchain_error(format!(
            "{} behavior probe failed: {err}",
            binary.canonical_name()
        )))?;
    if output.exit != ProcessExit::Code(0) {
        return Err(reference_toolchain_error(format!(
            "{} behavior probe did not exit successfully",
            binary.canonical_name()
        )));
    }
    let probe_text = format!("{}\n{}", output.stdout_tail, output.stderr_tail).to_lowercase();
    for marker in required_probe_markers {
        if !probe_text.contains(&marker.to_lowercase()) {
            return Err(reference_toolchain_error(format!(
                "{} behavior probe omitted required marker {marker:?}",
                binary.canonical_name()
            )));
        }
    }
    let behavior_probe_digest = Sha256Digest::of_bytes(
        format!(
            "tonepoet-reference-tool-probe/v1\0{}\0{}\0{}",
            binary.canonical_name(),
            output.command.sanitized_args.join("\0"),
            probe_text.trim()
        )
        .as_bytes(),
    );
    let closure_digest =
        reference_installation_identity(binary, &path, executable_sha256, &reported_version)?;

    Ok(ReferenceToolIdentity {
        canonical_path: path,
        executable_sha256,
        reported_version,
        version_probe_command: version_output.command,
        closure_digest,
        behavior_probe_digest,
        behavior_probe_command: output.command,
    })
}

async fn attest_certified_metadata_mutator(
    binary: ToolBinary,
    certified: &CertifiedMetadataMutatorIdentity,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
) -> Result<ReferenceMetadataMutatorIdentity, TrackExecutionError> {
    if !runner.tool_available(binary) {
        return Err(reference_toolchain_error(format!(
            "{} is not available",
            binary.canonical_name()
        )));
    }
    let resolved_path = runner.resolved_tool_path(binary).ok_or_else(|| {
        reference_toolchain_error(format!(
            "{} runner cannot prove the executable path it will spawn",
            binary.canonical_name()
        ))
    })?;
    let activation_path = resolve_policy_owned_reference_tool_path(binary).map_err(|error| {
        reference_toolchain_error(format!(
            "could not resolve policy-owned {}: {error}",
            binary.canonical_name()
        ))
    })?;
    let compiled_store = compiled_reference_store_path(binary).ok_or_else(|| {
        reference_toolchain_error(format!(
            "{} has no compiled Reference store binding",
            binary.canonical_name()
        ))
    })?;
    let compiled_path = compiled_reference_executable_path(binary).map_err(|error| {
        reference_toolchain_error(format!(
            "could not resolve the compiled {} closure: {error}",
            binary.canonical_name()
        ))
    })?;
    if certified.store_path.as_path() != Path::new(compiled_store)
        || resolved_path != activation_path
        || resolved_path != compiled_path
        || resolved_path.as_path() != certified.canonical_path.as_path()
    {
        return Err(reference_toolchain_error(format!(
            "{} certified store {}, compiled store {}, runtime path {}, activation path {}, compiled closure path {}, and certified path {} do not match",
            binary.canonical_name(),
            certified.store_path.display(),
            compiled_store,
            resolved_path.display(),
            activation_path.display(),
            compiled_path.display(),
            certified.canonical_path.display(),
        )));
    }
    let executable_sha256 = stable_file_sha256(&resolved_path).map_err(|error| {
        reference_toolchain_error(format!(
            "could not hash certified {} at {}: {error}",
            binary.canonical_name(),
            resolved_path.display()
        ))
    })?;
    if executable_sha256 != certified.executable_sha256 {
        return Err(reference_toolchain_error(format!(
            "{} executable digest drift at {}: certified {}, runtime {}",
            binary.canonical_name(),
            resolved_path.display(),
            certified.executable_sha256,
            executable_sha256,
        )));
    }
    let bound_executable = BoundToolExecutable {
        canonical_path: resolved_path.clone(),
        executable_sha256,
    };
    let version_output = runner
        .run_bound(reference_version_probe_command(binary)?, &bound_executable, cancel)
        .await
        .map_err(|error| {
            reference_toolchain_error(format!(
                "{} certified version probe failed: {error}",
                binary.canonical_name()
            ))
        })?;
    let reported_version = parse_tool_version_output(
        binary,
        &version_output.stdout_tail,
        &version_output.stderr_tail,
    )
    .ok_or_else(|| {
        reference_toolchain_error(format!(
            "{} did not report a parseable version",
            binary.canonical_name()
        ))
    })?;
    let certified_version = parse_tool_version_output(binary, &certified.reported_version, "")
        .ok_or_else(|| {
            reference_toolchain_error(format!(
                "the certified {} version {:?} is not canonical",
                binary.canonical_name(),
                certified.reported_version
            ))
        })?;
    if reported_version != certified_version {
        return Err(reference_toolchain_error(format!(
            "{} reported version {}, certified {}",
            binary.canonical_name(),
            reported_version,
            certified_version,
        )));
    }
    let closure_digest = reference_installation_identity(
        binary,
        &resolved_path,
        executable_sha256,
        &reported_version,
    )?;
    Ok(ReferenceMetadataMutatorIdentity {
        canonical_path: resolved_path,
        executable_sha256,
        reported_version,
        version_probe_command: version_output.command,
        closure_digest,
    })
}

fn reference_version_probe_command(
    binary: ToolBinary,
) -> Result<ToolCommand, TrackExecutionError> {
    let args = match binary {
        ToolBinary::Sox => vec!["--version".to_string()],
        ToolBinary::Ffmpeg => vec!["-version".to_string()],
        ToolBinary::Metaflac | ToolBinary::Wvtag => vec!["--version".to_string()],
        ToolBinary::AtomicParsley => Vec::new(),
        _ => {
            return Err(reference_toolchain_error(format!(
                "{} has no Reference version probe",
                binary.canonical_name()
            )))
        }
    };
    Ok(ToolCommand {
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet,
        binary,
        args,
        secret_args: Vec::new(),
        cwd: None,
        env: vec![EnvVar {
            key: "LC_ALL".to_string(),
            value: super::types::SecretString::new("C"),
            secret: false,
        }],
        timeout: Duration::from_secs(10),
    })
}

fn reference_behavior_probe_command(
    binary: ToolBinary,
) -> Result<ToolCommand, TrackExecutionError> {
    let args = match binary {
        ToolBinary::Sox => vec!["--help-effect".to_string(), "sinc".to_string()],
        ToolBinary::Ffmpeg => vec![
            "-hide_banner".to_string(),
            "-h".to_string(),
            "filter=loudnorm".to_string(),
        ],
        _ => {
            return Err(reference_toolchain_error(format!(
                "{} has no Reference behavior probe",
                binary.canonical_name()
            )))
        }
    };
    Ok(ToolCommand {
        environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet,
        binary,
        args,
        secret_args: Vec::new(),
        cwd: None,
        env: vec![EnvVar {
            key: "LC_ALL".to_string(),
            value: super::types::SecretString::new("C"),
            secret: false,
        }],
        timeout: Duration::from_secs(10),
    })
}

fn compiled_reference_store_path(binary: ToolBinary) -> Option<&'static str> {
    match binary {
        ToolBinary::Sox => option_env!("TONEPOET_REFERENCE_SOX_STORE_PATH"),
        ToolBinary::Ffmpeg => option_env!("TONEPOET_REFERENCE_FFMPEG_STORE_PATH"),
        ToolBinary::Metaflac => option_env!("TONEPOET_REFERENCE_METAFLAC_STORE_PATH"),
        ToolBinary::Wvtag => option_env!("TONEPOET_REFERENCE_WVTAG_STORE_PATH"),
        ToolBinary::AtomicParsley => option_env!("TONEPOET_REFERENCE_ATOMIC_PARSLEY_STORE_PATH"),
        _ => None,
    }
}

fn compiled_reference_executable_path(binary: ToolBinary) -> io::Result<PathBuf> {
    let store = compiled_reference_store_path(binary).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the binary was not built with an immutable {} Reference store binding",
                binary.canonical_name()
            ),
        )
    })?;
    let executable = match binary {
        ToolBinary::Sox => "sox",
        ToolBinary::Ffmpeg => "ffmpeg",
        ToolBinary::Metaflac => "metaflac",
        ToolBinary::Wvtag => "wvtag",
        ToolBinary::AtomicParsley => "AtomicParsley",
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} has no Reference executable binding", binary.canonical_name()),
            ));
        }
    };
    fs::canonicalize(Path::new(store).join("bin").join(executable))
}

fn reference_installation_identity(
    binary: ToolBinary,
    path: &Path,
    executable_sha256: Sha256Digest,
    reported_version: &str,
) -> Result<Sha256Digest, TrackExecutionError> {
    let store = compiled_reference_store_path(binary).ok_or_else(|| {
        reference_toolchain_error(format!(
            "the binary lacks an immutable {} Reference store binding",
            binary.canonical_name()
        ))
    })?;
    let installation = format!(
        "nix-store-closure/v2\0{}\0{}\0{}\0{}",
        store,
        path.display(),
        executable_sha256.to_hex(),
        reported_version
    );
    Ok(Sha256Digest::of_bytes(installation.as_bytes()))
}

fn resolve_policy_owned_reference_tool_path(binary: ToolBinary) -> io::Result<PathBuf> {
    let packaged_env = match binary {
        ToolBinary::Sox => Some("TONEPOET_REFERENCE_SOX_PATH"),
        ToolBinary::Ffmpeg => Some("TONEPOET_REFERENCE_FFMPEG_PATH"),
        ToolBinary::Metaflac => Some("TONEPOET_REFERENCE_METAFLAC_PATH"),
        ToolBinary::Wvtag => Some("TONEPOET_REFERENCE_WVTAG_PATH"),
        ToolBinary::AtomicParsley => Some("TONEPOET_REFERENCE_ATOMIC_PARSLEY_PATH"),
        _ => None,
    };
    let variable = packaged_env.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no Reference package binding", binary.canonical_name()),
        )
    })?;
    let path = std::env::var_os(variable).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{variable} is not set; run the packaged binary or qualified dev shell"),
        )
    })?;
    fs::canonicalize(path)
}

fn reference_platform_abi_digest() -> Sha256Digest {
    let identity = format!(
        "tonepoet-reference-platform-abi/v1\0{}\0{}\0{}\0{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::consts::FAMILY,
        std::mem::size_of::<usize>() * 8,
    );
    Sha256Digest::of_bytes(identity.as_bytes())
}

fn reference_runtime_dispatch_digest() -> Sha256Digest {
    let mut features = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        for (name, enabled) in [
            ("sse2", std::is_x86_feature_detected!("sse2")),
            ("sse3", std::is_x86_feature_detected!("sse3")),
            ("ssse3", std::is_x86_feature_detected!("ssse3")),
            ("sse4.1", std::is_x86_feature_detected!("sse4.1")),
            ("sse4.2", std::is_x86_feature_detected!("sse4.2")),
            ("avx", std::is_x86_feature_detected!("avx")),
            ("avx2", std::is_x86_feature_detected!("avx2")),
            ("fma", std::is_x86_feature_detected!("fma")),
        ] {
            if enabled {
                features.push(name);
            }
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        for (name, enabled) in [
            ("neon", std::arch::is_aarch64_feature_detected!("neon")),
        ] {
            if enabled {
                features.push(name);
            }
        }
    }
    let identity = format!(
        "tonepoet-reference-runtime-dispatch/v1\0{}\0{}",
        std::env::consts::ARCH,
        features.join(",")
    );
    Sha256Digest::of_bytes(identity.as_bytes())
}


fn reference_front_end_error(detail: impl AsRef<str>) -> TrackExecutionError {
    TrackExecutionError::new(
        ConvertError::Backend(format!(
            "{} ({})",
            tonepoet_pipeline::reference_error_text(
                tonepoet_pipeline::ReferenceErrorCode::FrontEndUnattested,
            ),
            detail.as_ref()
        )),
        Vec::new(),
    )
}

fn reference_toolchain_error(detail: impl AsRef<str>) -> TrackExecutionError {
    TrackExecutionError::new(
        ConvertError::Backend(format!(
            "{} ({})",
            tonepoet_pipeline::reference_error_text(
                tonepoet_pipeline::ReferenceErrorCode::Toolchain,
            ),
            detail.as_ref()
        )),
        Vec::new(),
    )
}

#[derive(Debug, Clone)]
struct ReferenceMaterialization {
    path: PathBuf,
    source_content_sha256: Sha256Digest,
    canonical_materialization_sha256: Sha256Digest,
}

/// Release-qualification view of the exact production standalone source
/// materialization seam. This is intentionally narrow: it exposes no alternate
/// decoder or writer and calls the same private-copy/container-validation/DST
/// decode path used by production execution.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceSourceMaterializationQualification {
    pub materialized_path: PathBuf,
    pub source_content_sha256: Sha256Digest,
    pub canonical_materialization_sha256: Sha256Digest,
    pub materialization_identity_digest: Sha256Digest,
}

/// Exercise the exact production materialization seam for a standalone DSF,
/// DSDIFF/DSD, or qualified DSDIFF/DST source. SACD remains unavailable until
/// an end-to-end ISO extraction corpus is commissioned.
#[doc(hidden)]
pub fn qualify_reference_source_materialization(
    source_kind: &DsdSourceKind,
    source: &Path,
    work_dir: &Path,
) -> io::Result<ReferenceSourceMaterializationQualification> {
    let cancel = CancellationToken::new();
    let scratch = ReferenceScratchPaths::for_source_kind(work_dir, source_kind);
    let materialization = materialize_reference_presented_source(
        source_kind,
        source,
        &scratch,
        &cancel,
        None,
    )?;
    Ok(ReferenceSourceMaterializationQualification {
        materialized_path: materialization.path,
        source_content_sha256: materialization.source_content_sha256,
        canonical_materialization_sha256: materialization.canonical_materialization_sha256,
        materialization_identity_digest: reference_materialization_identity_digest(
            source_kind,
            materialization.source_content_sha256,
            materialization.canonical_materialization_sha256,
        ),
    })
}

#[doc(hidden)]
pub fn qualify_reference_materialization_identity_digest(
    source_kind: &DsdSourceKind,
    source_content_sha256: Sha256Digest,
    canonical_materialization_sha256: Sha256Digest,
) -> Sha256Digest {
    reference_materialization_identity_digest(
        source_kind,
        source_content_sha256,
        canonical_materialization_sha256,
    )
}

pub(crate) fn reference_materialization_identity_digest(
    source_kind: &DsdSourceKind,
    source_content_sha256: Sha256Digest,
    canonical_materialization_sha256: Sha256Digest,
) -> Sha256Digest {
    fn field(hasher: &mut Sha256, value: &[u8]) {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value);
    }

    let mut hasher = Sha256::new();
    hasher.update(b"tonepoet-reference-materialization-identity/v1\0");
    match source_kind {
        DsdSourceKind::DsfUncompressed => field(&mut hasher, b"dsf_uncompressed"),
        DsdSourceKind::DsdiffUncompressed => field(&mut hasher, b"dsdiff_uncompressed"),
        DsdSourceKind::DsdiffDst => field(&mut hasher, b"dsdiff_dst"),
        DsdSourceKind::SacdTrack {
            frame_format,
            selection,
        } => {
            field(&mut hasher, b"sacd_track");
            field(
                &mut hasher,
                match frame_format {
                    SacdFrameEncoding::Dsd => b"dsd",
                    SacdFrameEncoding::Dst => b"dst",
                },
            );
            field(
                &mut hasher,
                match selection.area {
                    SacdAreaKind::Stereo => b"stereo",
                    SacdAreaKind::Multichannel => b"multichannel",
                },
            );
            field(
                &mut hasher,
                &selection.track_index_zero_based.to_be_bytes(),
            );
            field(&mut hasher, &selection.start_frame.to_be_bytes());
            field(&mut hasher, &selection.frame_count.to_be_bytes());
            field(&mut hasher, &selection.toc_digest.0);
        }
        DsdSourceKind::UnknownDsdContainer => {
            field(&mut hasher, b"unknown_dsd_container")
        }
    }
    field(&mut hasher, &source_content_sha256.0);
    field(&mut hasher, &canonical_materialization_sha256.0);
    Sha256Digest(hasher.finalize().into())
}

#[derive(Debug, Clone)]
struct ReferenceRuntimeResult {
    commands: Vec<CommandRecord>,
    measurements: BTreeMap<MeasurementId, TruePeakMeasurement>,
    resolved_command_hash: String,
    pcm_verification: ReferencePcmVerificationEvidence,
}

fn validate_reference_scratch_cleanup_authority(
    plan: &ConversionPlan,
    scratch: &ReferenceScratchPaths,
) -> Result<(), TrackExecutionError> {
    let cleanup = plan.cleanup_paths();
    let missing = scratch
        .all()
        .into_iter()
        .filter(|path| !cleanup.iter().any(|candidate| candidate.as_path() == *path))
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(TrackExecutionError::new(
            ConvertError::Backend(format!(
                "Reference plan omitted governed scratch paths: {}",
                missing.join(", ")
            )),
            Vec::new(),
        ))
    }
}

async fn materialize_reference_source(
    plan_request: &PlanRequest,
    track: &PreparedTrack,
    realized_input: &Path,
    scratch: &ReferenceScratchPaths,
    cancel: &CancellationToken,
    worker_lease: TrackExecutionBlockingWorkerLease,
) -> Result<ReferenceMaterialization, TrackExecutionError> {
    let plan_request = plan_request.clone();
    let track = track.clone();
    let realized_input = realized_input.to_path_buf();
    let scratch = scratch.clone();
    let worker_cancel = cancel.child_token();
    let cancel_on_drop = worker_cancel.clone().drop_guard();
    #[cfg(test)]
    let materialization_pause = REFERENCE_MATERIALIZATION_PAUSE
        .try_with(Arc::clone)
        .ok();
    #[cfg(not(test))]
    let materialization_pause: Option<Arc<ReferenceMaterializationPause>> = None;
    let result = tokio::task::spawn_blocking(move || {
        let _worker_lease = worker_lease;
        materialize_reference_source_blocking(
            &plan_request,
            &track,
            &realized_input,
            &scratch,
            &worker_cancel,
            materialization_pause.as_deref(),
        )
    })
    .await
    .map_err(|err| {
        TrackExecutionError::new(
            ConvertError::Backend(format!(
                "Reference source materialization task failed: {err}"
            )),
            Vec::new(),
        )
    })?;
    drop(cancel_on_drop);
    result
}

fn materialize_reference_source_blocking(
    plan_request: &PlanRequest,
    track: &PreparedTrack,
    realized_input: &Path,
    scratch: &ReferenceScratchPaths,
    cancel: &CancellationToken,
    _materialization_pause: Option<&ReferenceMaterializationPause>,
) -> Result<ReferenceMaterialization, TrackExecutionError> {
    if cancel.is_cancelled() {
        return Err(reference_cancelled_error());
    }
    let source_kind = plan_request
        .source
        .dsd_source_kind
        .as_ref()
        .ok_or_else(|| {
            TrackExecutionError::new(
                ConvertError::Backend(
                    "Reference source materialization is missing DSD source identity".to_string(),
                ),
                Vec::new(),
            )
        })?;
    let original_authority = match &track.source_ref {
        super::types::TrackSourceRef::SacdTrack { iso, .. } => iso.as_path(),
        _ => realized_input,
    };
    let source_content_sha256 = stable_file_sha256_cancel(original_authority, cancel)
        .map_err(|err| reference_materialization_error(
            format!("failed to admit Reference source {}", original_authority.display()),
            err,
        ))?;

    let presented_source = match &track.source_ref {
        super::types::TrackSourceRef::SacdTrack {
            iso,
            track_index,
            area,
        } => {
            let current_source_kind = reference_sacd_source_kind(iso, *track_index, *area)
                .map_err(|err| TrackExecutionError::new(err, Vec::new()))?;
            if &current_source_kind != source_kind {
                return Err(TrackExecutionError::new(
                    ConvertError::Backend(
                        "Reference SACD TOC selection changed after planning".to_string(),
                    ),
                    Vec::new(),
                ));
            }
            // Threat boundary: the pre/post SHA-256 checks detect ordinary
            // same-user mutation while TOC selection and extraction run. They
            // are deliberately not described as a pathname-race proof against
            // a privileged adversary or a filesystem that violates stable-open
            // semantics: the ISO is reopened by path for each check and by the
            // extractor. Promotion of SACD Reference cells therefore still
            // requires commissioned end-to-end fixtures and an explicit review
            // of this reopen boundary; these hashes are fail-closed mutation
            // detection, not a capability-style identity guarantee.
            let post_toc_source_sha256 = stable_file_sha256_cancel(iso, cancel)
                .map_err(|err| {
                    reference_materialization_error(
                        format!(
                            "failed to re-verify Reference SACD source {} after TOC admission",
                            iso.display()
                        ),
                        err,
                    )
                })?;
            if post_toc_source_sha256 != source_content_sha256 {
                return Err(TrackExecutionError::new(
                    ConvertError::Backend(
                        "Reference SACD source changed during TOC admission".to_string(),
                    ),
                    Vec::new(),
                ));
            }
            let extracted = super::stages::realize_reference_sacd_track_blocking(
                iso,
                *track_index,
                *area,
                &scratch.sacd_extracted_source,
                &scratch.sacd_extracted_source_temporary,
                || {
                    #[cfg(test)]
                    if let Some(pause) = _materialization_pause {
                        pause.pause_if_selected(
                            ReferenceMaterializationPausePoint::DuringSacdExtraction,
                        );
                    }
                    cancel.is_cancelled()
                },
            )
            .map_err(|err| {
                if cancel.is_cancelled() {
                    reference_cancelled_error()
                } else {
                    TrackExecutionError::new(err, Vec::new())
                }
            })?;
            let post_extract_source_sha256 = stable_file_sha256_cancel(iso, cancel)
                .map_err(|err| {
                    reference_materialization_error(
                        format!(
                            "failed to re-verify Reference SACD source {} after extraction",
                            iso.display()
                        ),
                        err,
                    )
                })?;
            if post_extract_source_sha256 != source_content_sha256 {
                return Err(TrackExecutionError::new(
                    ConvertError::Backend(
                        "Reference SACD source changed during qualified track extraction"
                            .to_string(),
                    ),
                    Vec::new(),
                ));
            }
            extracted
        }
        _ => realized_input.to_path_buf(),
    };

    let materialized = materialize_reference_presented_source(
        source_kind,
        &presented_source,
        scratch,
        cancel,
        _materialization_pause,
    )
    .map_err(|err| {
        if err.kind() == io::ErrorKind::Interrupted {
            reference_cancelled_error()
        } else {
            reference_materialization_error(
                format!(
                    "failed to materialize verified Reference source {}",
                    presented_source.display()
                ),
                err,
            )
        }
    })?;
    let copied_sha256 = materialized.source_content_sha256;
    let path = materialized.path;
    let canonical_materialization_sha256 = materialized.canonical_materialization_sha256;

    // A standalone source's original authority is the same byte object that was
    // copied. SACD binds the ISO hash separately while the canonical hash binds
    // the decoded per-track carrier.
    if !matches!(source_kind, DsdSourceKind::SacdTrack { .. })
        && source_content_sha256 != copied_sha256
    {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(
                "Reference source changed between admission and private materialization"
                    .to_string(),
            ),
            Vec::new(),
        ));
    }

    Ok(ReferenceMaterialization {
        path,
        source_content_sha256,
        canonical_materialization_sha256,
    })
}

fn materialize_reference_presented_source(
    source_kind: &DsdSourceKind,
    presented_source: &Path,
    scratch: &ReferenceScratchPaths,
    cancel: &CancellationToken,
    _materialization_pause: Option<&ReferenceMaterializationPause>,
) -> io::Result<ReferenceMaterialization> {
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    let mut source_file = File::open(presented_source)?;
    let source_info = inspect_dsd_container(&mut source_file)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    let expected = match source_kind {
        DsdSourceKind::DsfUncompressed => {
            (DsdContainerFormat::Dsf, DsdCompression::Dsd)
        }
        DsdSourceKind::DsdiffUncompressed => {
            (DsdContainerFormat::Dsdiff, DsdCompression::Dsd)
        }
        DsdSourceKind::DsdiffDst => {
            (DsdContainerFormat::Dsdiff, DsdCompression::Dst)
        }
        DsdSourceKind::SacdTrack { .. } => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                tonepoet_pipeline::reference_error_text(
                    tonepoet_pipeline::ReferenceErrorCode::SacdFrontEndIntegrationUnqualified,
                ),
            ));
        }
        DsdSourceKind::UnknownDsdContainer => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                tonepoet_pipeline::reference_error_text(
                    tonepoet_pipeline::ReferenceErrorCode::UnknownEncoding,
                ),
            ));
        }
    };
    if (source_info.format, source_info.compression) != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Reference source classification mismatch: declared {source_kind:?}, inspected {:?}/{:?}",
                source_info.format, source_info.compression
            ),
        ));
    }

    #[cfg(test)]
    if let Some(pause) = _materialization_pause {
        pause.pause_if_selected(
            ReferenceMaterializationPausePoint::BeforeScratchPathCreation,
        );
    }
    let admitted = &scratch.admitted_source;
    let copied_sha256 = verified_private_copy(
        presented_source,
        admitted,
        &scratch.admitted_source_temporary,
        cancel,
        _materialization_pause,
    )?;
    let (path, canonical_materialization_sha256) = if matches!(source_kind, DsdSourceKind::DsdiffDst) {
        decode_dsdiff_dst_to_canonical_dff(
            admitted,
            &scratch.canonical_dsd,
            &scratch.canonical_dsd_temporary,
            cancel,
            _materialization_pause,
        )?;
        let digest = stable_file_sha256_cancel(&scratch.canonical_dsd, cancel)?;
        (scratch.canonical_dsd.clone(), digest)
    } else {
        (admitted.clone(), copied_sha256)
    };
    Ok(ReferenceMaterialization {
        path,
        source_content_sha256: copied_sha256,
        canonical_materialization_sha256,
    })
}

fn verified_private_copy(
    source: &Path,
    destination: &Path,
    temporary: &Path,
    cancel: &CancellationToken,
    _materialization_pause: Option<&ReferenceMaterializationPause>,
) -> io::Result<Sha256Digest> {
    let resolved = fs::canonicalize(source)?;
    let mut input = File::open(&resolved)?;
    if !input.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Reference source is not a regular file",
        ));
    }
    let admitted = sha256_reader_cancel(&mut input, cancel)?;
    input.seek(SeekFrom::Start(0))?;

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_cleanup_path(temporary)?;
    let result = (|| {
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temporary)?;
        copy_reader_cancel(
            &mut input,
            &mut output,
            cancel,
            _materialization_pause,
        )?;
        output.sync_all()?;
        drop(output);

        input.seek(SeekFrom::Start(0))?;
        let after_copy = sha256_reader_cancel(&mut input, cancel)?;
        if admitted != after_copy {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source bytes changed while materializing",
            ));
        }
        let copied = stable_file_sha256_cancel(temporary, cancel)?;
        if admitted != copied {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "private materialization hash does not match admitted source",
            ));
        }
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        remove_cleanup_path(destination)?;
        fs::rename(temporary, destination)?;
        Ok(admitted)
    })();
    if result.is_err() {
        let _ = remove_cleanup_path(temporary);
    }
    result
}

fn decode_dsdiff_dst_to_canonical_dff(
    source: &Path,
    destination: &Path,
    temporary: &Path,
    cancel: &CancellationToken,
    _materialization_pause: Option<&ReferenceMaterializationPause>,
) -> io::Result<()> {
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    let input = File::open(source)?;
    let mut reader = open_dsd_as_decoded_reader(input)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    remove_cleanup_path(temporary)?;
    let mut output = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(temporary)?;
    let result = (|| {
        let stats = write_decoded_dsd_to_dff_with_cancel(&mut reader, &mut output, || {
            #[cfg(test)]
            if let Some(pause) = _materialization_pause {
                pause.pause_if_selected(ReferenceMaterializationPausePoint::DuringDstDecode);
            }
            cancel.is_cancelled()
        })
        .map_err(|err| match err {
            sacd_rs::DsdReadError::Io(inner)
                if inner.kind() == io::ErrorKind::Interrupted => inner,
            other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
        })?;
        if stats.frames_read == 0 || stats.bytes_written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "DST decoder produced no DSD audio",
            ));
        }
        output.sync_all()?;
        output.seek(SeekFrom::Start(0))?;
        let info = inspect_dsd_container(&mut output)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
        if info.format != DsdContainerFormat::Dsdiff || info.compression != DsdCompression::Dsd {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "DST decoder did not produce canonical uncompressed DSDIFF/DSD",
            ));
        }
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        drop(output);
        remove_cleanup_path(destination)?;
        fs::rename(temporary, destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = remove_cleanup_path(temporary);
    }
    result
}

fn stable_file_sha256(path: &Path) -> io::Result<Sha256Digest> {
    let resolved = fs::canonicalize(path)?;
    let mut file = File::open(&resolved)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a regular file",
        ));
    }
    let first = sha256_reader(&mut file)?;
    file.seek(SeekFrom::Start(0))?;
    let second = sha256_reader(&mut file)?;
    if first != second {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file changed while hashing",
        ));
    }
    Ok(first)
}

fn stable_file_sha256_cancel(
    path: &Path,
    cancel: &CancellationToken,
) -> io::Result<Sha256Digest> {
    let resolved = fs::canonicalize(path)?;
    let mut file = File::open(&resolved)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a regular file",
        ));
    }
    let first = sha256_reader_cancel(&mut file, cancel)?;
    file.seek(SeekFrom::Start(0))?;
    let second = sha256_reader_cancel(&mut file, cancel)?;
    if first != second {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file changed while hashing",
        ));
    }
    Ok(first)
}

fn sha256_reader(reader: &mut File) -> io::Result<Sha256Digest> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(&digest);
    Ok(Sha256Digest(bytes))
}

fn sha256_reader_cancel(
    reader: &mut File,
    cancel: &CancellationToken,
) -> io::Result<Sha256Digest> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(&digest);
    Ok(Sha256Digest(bytes))
}

fn copy_reader_cancel(
    input: &mut File,
    output: &mut File,
    cancel: &CancellationToken,
    _materialization_pause: Option<&ReferenceMaterializationPause>,
) -> io::Result<u64> {
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 128 * 1024];
    #[cfg(test)]
    let mut copy_pause_reached = false;
    loop {
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
        #[cfg(test)]
        if !copy_pause_reached {
            if let Some(pause) = _materialization_pause {
                pause.pause_if_selected(ReferenceMaterializationPausePoint::DuringSourceCopy);
            }
            copy_pause_reached = true;
        }
        copied = copied
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "copy size overflow"))?;
    }
    Ok(copied)
}

fn reference_materialization_error(
    context: impl AsRef<str>,
    error: io::Error,
) -> TrackExecutionError {
    if error.kind() == io::ErrorKind::Interrupted {
        reference_cancelled_error()
    } else {
        TrackExecutionError::new(
            ConvertError::Backend(format!("{}: {error}", context.as_ref())),
            Vec::new(),
        )
    }
}

fn reference_cancelled_error() -> TrackExecutionError {
    TrackExecutionError::new(ConvertError::Realize("cancelled".to_string()), Vec::new())
}

async fn execute_reference_steps(
    steps: &[PlannedExecutionStep],
    summary: &DsdReferencePlanSummary,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_paths: &HashMap<String, PathBuf>,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
    progress: &mut OperationProgressTracker<'_>,
    start_fraction: f32,
    end_fraction: f32,
    track_label: String,
    silence_scan_path: &Path,
    toolchain: &ReferenceToolchainEvidence,
) -> Result<ReferenceRuntimeResult, TrackExecutionError> {
    let mut records = Vec::new();
    let mut measurements = BTreeMap::new();
    let mut r64_probe = None;
    let mut qpcm_probe = None;
    let mut qpcm_hash = None;
    let mut packaged_hash = None;
    let mut packaged_identity_mode = None;
    let step_count = steps.len().max(1) as f32;
    let total_width = (end_fraction - start_fraction).max(0.0);

    for (index, step) in steps.iter().enumerate() {
        if cancel.is_cancelled() {
            progress.cancel_requested().await;
            return Err(TrackExecutionError::new(
                ConvertError::Realize("cancelled".to_string()),
                records,
            ));
        }
        let window_start = start_fraction + total_width * (index as f32 / step_count);
        let window_end = start_fraction + total_width * ((index + 1) as f32 / step_count);
        match step {
            PlannedExecutionStep::Command(command) => {
                let mut step_records = execute_commands(
                    std::slice::from_ref(command),
                    runner,
                    cancel,
                    tool_paths,
                    tool_concurrency_limits.clone(),
                    progress,
                    window_start,
                    window_end,
                    track_label.clone(),
                )
                .await?;
                records.append(&mut step_records);

                if command.output.as_path() == Some(summary.r64_path.as_path()) {
                    let (mut verification_records, probe) = verify_reference_r64_contract(
                        summary,
                        runner,
                        cancel,
                        tool_paths,
                        tool_concurrency_limits.as_ref(),
                        progress,
                        window_start,
                        window_end,
                        &track_label,
                    )
                    .await
                    .map_err(|mut err| {
                        let mut all = records.clone();
                        all.append(&mut err.commands);
                        err.commands = all;
                        err
                    })?;
                    records.append(&mut verification_records);
                    r64_probe = Some(probe);
                } else if command.output.as_path() == Some(summary.packaged_path.as_path())
                    && summary.packaged_path != summary.qpcm_path
                {
                    let expected = qpcm_hash.ok_or_else(|| {
                        TrackExecutionError::new(
                            ConvertError::Backend(
                                "Reference package was produced before QPCM verification"
                                    .to_string(),
                            ),
                            records.clone(),
                        )
                    })?;
                    let (mut verification_records, digest) =
                        reference_decoded_sample_hash_with_plan_carrier(
                            summary,
                            ReferenceDecodedCarrierSelector::PackagedOutput,
                            "Verify packaged output decoded samples",
                            runner,
                            cancel,
                            tool_paths,
                            tool_concurrency_limits.as_ref(),
                            progress,
                            window_start,
                            window_end,
                            &track_label,
                        )
                        .await
                        .map_err(|mut err| {
                            let mut all = records.clone();
                            all.append(&mut err.commands);
                            err.commands = all;
                            err
                        })?;
                    records.append(&mut verification_records);
                    if digest != expected {
                        return Err(TrackExecutionError::new(
                            ConvertError::Backend(format!(
                                "Reference lossless package changed decoded samples: QPCM={}, packaged={}",
                                expected.to_hex(),
                                digest.to_hex()
                            )),
                            records,
                        ));
                    }
                    packaged_hash = Some(digest);
                    packaged_identity_mode = Some(
                        ReferencePackagedSampleIdentityMode::IndependentDecodeComparison,
                    );
                }
            }
            PlannedExecutionStep::Pipeline(pipeline) => {
                validate_reference_package_pipeline(summary, pipeline).map_err(|mut err| {
                    err.commands = records.clone();
                    err
                })?;
                let (mut step_records, _) = run_reference_capture_pipeline(
                    pipeline,
                    runner,
                    cancel,
                    tool_concurrency_limits.as_ref(),
                    progress,
                    &format!("{track_label} - {}", pipeline.description),
                )
                .await
                .map_err(|mut err| {
                    let mut all = records.clone();
                    all.append(&mut err.commands);
                    err.commands = all;
                    err
                })?;
                records.append(&mut step_records);

                let expected = qpcm_hash.ok_or_else(|| {
                    TrackExecutionError::new(
                        ConvertError::Backend(
                            "Reference package pipeline ran before QPCM verification".to_string(),
                        ),
                        records.clone(),
                    )
                })?;
                let (mut verification_records, digest) =
                    reference_decoded_sample_hash_with_plan_carrier(
                        summary,
                        ReferenceDecodedCarrierSelector::PackagedOutput,
                        "Verify packaged output decoded samples",
                        runner,
                        cancel,
                        tool_paths,
                        tool_concurrency_limits.as_ref(),
                        progress,
                        window_start,
                        window_end,
                        &track_label,
                    )
                    .await
                    .map_err(|mut err| {
                        let mut all = records.clone();
                        all.append(&mut err.commands);
                        err.commands = all;
                        err
                    })?;
                records.append(&mut verification_records);
                if digest != expected {
                    return Err(TrackExecutionError::new(
                        ConvertError::Backend(format!(
                            "Reference Float64 package changed decoded samples: QPCM={}, packaged={}",
                            expected.to_hex(),
                            digest.to_hex()
                        )),
                        records,
                    ));
                }
                packaged_hash = Some(digest);
                packaged_identity_mode = Some(
                    ReferencePackagedSampleIdentityMode::IndependentDecodeComparison,
                );
            }
            PlannedExecutionStep::Measurement(measurement) => {
                if measurements.contains_key(&measurement.id) {
                    return Err(TrackExecutionError::new(
                        ConvertError::Backend(format!(
                            "duplicate Reference measurement id {}",
                            measurement.id.0
                        )),
                        records,
                    ));
                }
                validate_reference_measurement_binding(summary, measurement).map_err(|mut err| {
                    err.commands = records.clone();
                    err
                })?;
                match measurement.purpose {
                    TruePeakPurpose::GainAuthority if r64_probe.is_none() => {
                        return Err(TrackExecutionError::new(
                            ConvertError::Backend(
                                "Reference gain measurement was scheduled before R64 verification"
                                    .to_string(),
                            ),
                            records,
                        ));
                    }
                    TruePeakPurpose::PostFinalAcceptance if qpcm_probe.is_none() => {
                        return Err(TrackExecutionError::new(
                            ConvertError::Backend(
                                "Reference post-final measurement was scheduled before QPCM verification"
                                    .to_string(),
                            ),
                            records,
                        ));
                    }
                    _ => {}
                }
                let (mut measurement_records, parsed) = execute_reference_measurement(
                    summary,
                    measurement,
                    runner,
                    cancel,
                    tool_paths,
                    tool_concurrency_limits.as_ref(),
                    progress,
                    window_start,
                    window_end,
                    &track_label,
                    silence_scan_path,
                    toolchain.reporting_uncertainty,
                    toolchain.analyzer_residual,
                )
                .await
                .map_err(|mut err| {
                    let mut all = records.clone();
                    all.append(&mut err.commands);
                    err.commands = all;
                    err
                })?;
                records.append(&mut measurement_records);
                if measurement.purpose == TruePeakPurpose::PostFinalAcceptance {
                    validate_post_final_true_peak(parsed.conservative_upper, summary.gain_policy)
                        .map_err(|err| {
                            TrackExecutionError::new(
                                ConvertError::Backend(format!(
                                    "Reference post-final acceptance failed: {err}"
                                )),
                                records.clone(),
                            )
                        })?;
                }
                measurements.insert(measurement.id, parsed);
            }
            PlannedExecutionStep::DeferredCommand(command) => {
                let resolved = resolve_deferred_command(command, &measurements)?;
                let mut step_records = execute_commands(
                    std::slice::from_ref(&resolved),
                    runner,
                    cancel,
                    tool_paths,
                    tool_concurrency_limits.clone(),
                    progress,
                    window_start,
                    window_end,
                    track_label.clone(),
                )
                .await?;
                records.append(&mut step_records);

                if command.output.as_path() == Some(summary.qpcm_path.as_path()) {
                    let r64 = r64_probe.ok_or_else(|| {
                        TrackExecutionError::new(
                            ConvertError::Backend(
                                "Reference terminal realization ran without verified R64 authority"
                                    .to_string(),
                            ),
                            records.clone(),
                        )
                    })?;
                    let (mut verification_records, probe, digest) =
                        verify_reference_qpcm_contract(
                            summary,
                            r64,
                            runner,
                            cancel,
                            tool_paths,
                            tool_concurrency_limits.as_ref(),
                            progress,
                            window_start,
                            window_end,
                            &track_label,
                        )
                        .await
                        .map_err(|mut err| {
                            let mut all = records.clone();
                            all.append(&mut err.commands);
                            err.commands = all;
                            err
                        })?;
                    records.append(&mut verification_records);
                    qpcm_probe = Some(probe);
                    qpcm_hash = Some(digest);
                    if summary.packaged_path == summary.qpcm_path {
                        packaged_hash = Some(digest);
                        packaged_identity_mode = Some(
                            ReferencePackagedSampleIdentityMode::DirectW64QpcmExactDelivery,
                        );
                    }
                }
            }
        }
    }

    let r64_probe = r64_probe.ok_or_else(|| {
        TrackExecutionError::new(
            ConvertError::Backend("Reference execution omitted R64 verification".to_string()),
            records.clone(),
        )
    })?;
    let qpcm_probe = qpcm_probe.ok_or_else(|| {
        TrackExecutionError::new(
            ConvertError::Backend("Reference execution omitted QPCM verification".to_string()),
            records.clone(),
        )
    })?;
    let qpcm_hash = qpcm_hash.ok_or_else(|| {
        TrackExecutionError::new(
            ConvertError::Backend("Reference execution omitted QPCM sample identity".to_string()),
            records.clone(),
        )
    })?;
    let packaged_hash = packaged_hash.ok_or_else(|| {
        TrackExecutionError::new(
            ConvertError::Backend("Reference execution omitted package sample identity".to_string()),
            records.clone(),
        )
    })?;
    let packaged_identity_mode = packaged_identity_mode.ok_or_else(|| {
        TrackExecutionError::new(
            ConvertError::Backend(
                "Reference execution omitted package sample-identity disposition".to_string(),
            ),
            records.clone(),
        )
    })?;
    let pcm_verification = ReferencePcmVerificationEvidence {
        r64_contract_digest: reference_carrier_probe_digest(summary.policy, "r64", r64_probe),
        qpcm_contract_digest: reference_carrier_probe_digest(summary.policy, "qpcm", qpcm_probe),
        qpcm_sample_sha256: qpcm_hash,
        packaged_sample_sha256: packaged_hash,
        packaged_sample_identity_mode: packaged_identity_mode,
        post_metadata_sample_sha256: None,
        post_metadata_verification_command: None,
        post_metadata_verification_commands: Vec::new(),
    };
    let resolved_command_hash = command_records_hash(&records);
    Ok(ReferenceRuntimeResult {
        commands: records,
        measurements,
        resolved_command_hash,
        pcm_verification,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReferenceCarrierProbe {
    sample_rate_hz: u32,
    channels: u16,
    bits_per_sample: u16,
    samples_per_channel: u64,
    floating_point: bool,
    w64_structure: Option<W64ExactStructure>,
}

fn canonical_reference_environment() -> BTreeMap<String, String> {
    BTreeMap::from([("LC_ALL".to_string(), "C".to_string())])
}

fn build_reference_carrier_probe_command(
    path: &Path,
    description: &str,
    flag: &'static str,
    field: &'static str,
) -> PlannedCommand {
    let mut command = PlannedCommand::new(
        ToolIdentifier::Sox,
        vec!["--i".to_string(), flag.to_string(), path.display().to_string()],
        tonepoet_pipeline::InputSource::Path(path.to_path_buf()),
        tonepoet_pipeline::OutputSink::Stdout,
        None,
        format!("Verify {description} {field}"),
    );
    command.environment_policy = tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet;
    command.environment = canonical_reference_environment();
    command
}

#[derive(Debug, Clone)]
enum PlannedReferenceSampleHash {
    Direct {
        authority: ReferenceDecodeAuthority,
        command: PlannedCommand,
    },
    StreamedFloat64W64 {
        authority: ReferenceDecodeAuthority,
        pipeline: PlannedCommandPipeline,
    },
}

impl PlannedReferenceSampleHash {
    fn authority(&self) -> ReferenceDecodeAuthority {
        match self {
            Self::Direct { authority, .. } | Self::StreamedFloat64W64 { authority, .. } => {
                *authority
            }
        }
    }
}

fn build_reference_direct_hash_command(
    carrier: &ReferenceDecodedCarrier,
    description: &str,
) -> Result<PlannedCommand, String> {
    let path = carrier.path();
    let authority = carrier.authority();
    validate_reference_decode_mechanism(
        authority.role(),
        authority.contract(),
        ReferenceDecodeMechanism::DirectFfmpeg,
    )
    .map_err(|error| error.to_string())?;
    let mut command = PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        reference_hash_args(carrier),
        tonepoet_pipeline::InputSource::Path(path.to_path_buf()),
        tonepoet_pipeline::OutputSink::Stdout,
        None,
        description.to_string(),
    );
    command.environment_policy = tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet;
    command.environment = canonical_reference_environment();
    Ok(command)
}

fn build_reference_float64_w64_hash_pipeline(
    carrier: &ReferenceDecodedCarrier,
    description: &str,
) -> Result<PlannedCommandPipeline, String> {
    let path = carrier.path();
    let authority = carrier.authority();
    validate_reference_decode_mechanism(
        authority.role(),
        authority.contract(),
        ReferenceDecodeMechanism::SoxFloat64W64RawStream,
    )
    .map_err(|error| error.to_string())?;
    let contract = authority.contract();
    if contract.bit_depth != tonepoet_pipeline::PcmBitDepth::Float64 {
        return Err("streamed W64 sample hashing is reserved for Float64".to_string());
    }
    let mut producer = PlannedCommand::new(
        ToolIdentifier::Sox,
        vec![
            "-S".to_string(),
            "-D".to_string(),
            path.display().to_string(),
            "-t".to_string(),
            "raw".to_string(),
            "-e".to_string(),
            "floating-point".to_string(),
            "-b".to_string(),
            "64".to_string(),
            "-L".to_string(),
            "-".to_string(),
        ],
        tonepoet_pipeline::InputSource::Path(path.to_path_buf()),
        tonepoet_pipeline::OutputSink::Stdout,
        None,
        format!("{description}: stream exact Float64 W64"),
    );
    producer.environment_policy = tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet;
    producer.environment = canonical_reference_environment();

    let mut consumer = PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        vec![
            "-hide_banner".to_string(),
            "-nostdin".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
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
            authority.hash_encoding().ffmpeg_codec().to_string(),
            "-f".to_string(),
            "hash".to_string(),
            "-hash".to_string(),
            "sha256".to_string(),
            "-".to_string(),
        ],
        tonepoet_pipeline::InputSource::Stdin,
        tonepoet_pipeline::OutputSink::Stdout,
        None,
        description.to_string(),
    );
    consumer.environment_policy = tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet;
    consumer.environment = canonical_reference_environment();

    Ok(PlannedCommandPipeline {
        producer,
        consumer,
        description: description.to_string(),
    })
}

fn build_reference_sample_hash_plan(
    carrier: &ReferenceDecodedCarrier,
    description: &str,
) -> Result<PlannedReferenceSampleHash, String> {
    let authority = carrier.authority();
    match authority.mechanism() {
        ReferenceDecodeMechanism::DirectFfmpeg => Ok(PlannedReferenceSampleHash::Direct {
            command: build_reference_direct_hash_command(carrier, description)?,
            authority,
        }),
        ReferenceDecodeMechanism::SoxFloat64W64RawStream => {
            Ok(PlannedReferenceSampleHash::StreamedFloat64W64 {
                pipeline: build_reference_float64_w64_hash_pipeline(carrier, description)?,
                authority,
            })
        }
    }
}

async fn run_reference_capture_command(
    planned: &PlannedCommand,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_paths: &HashMap<String, PathBuf>,
    limits: Option<&Arc<ToolConcurrencyLimits>>,
    progress: &mut OperationProgressTracker<'_>,
    window_start: f32,
    window_end: f32,
    label: &str,
) -> Result<ToolOutput, TrackExecutionError> {
    let mut command = planned_command_to_tool_command(planned, DEFAULT_PLANNED_COMMAND_TIMEOUT)
        .map_err(TrackExecutionError::from)?;
    let _permit = acquire_tool_permit(&mut command, limits, cancel)
        .await
        .map_err(TrackExecutionError::from)?;
    let mut output = run_planned_command(
        command,
        planned,
        runner,
        cancel,
        tool_paths,
        progress,
        window_start,
        window_end,
        label,
    )
    .await
    .map_err(|error| track_execution_error_from_tool_error(0, planned, error, Vec::new()))?;
    output.command.description = non_empty_planned_description(planned);
    Ok(output)
}

async fn run_reference_capture_pipeline(
    pipeline: &PlannedCommandPipeline,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    limits: Option<&Arc<ToolConcurrencyLimits>>,
    progress: &mut OperationProgressTracker<'_>,
    label: &str,
) -> Result<(Vec<CommandRecord>, String), TrackExecutionError> {
    let producer = planned_command_to_tool_command(
        &pipeline.producer,
        DEFAULT_PLANNED_COMMAND_TIMEOUT,
    )
    .map_err(TrackExecutionError::from)?;
    let consumer = planned_command_to_tool_command(
        &pipeline.consumer,
        DEFAULT_PLANNED_COMMAND_TIMEOUT,
    )
    .map_err(TrackExecutionError::from)?;
    let _pipeline_permits = acquire_reference_pipeline_permits(
        [producer.binary, consumer.binary],
        limits,
        cancel,
    )
    .await
    .map_err(TrackExecutionError::from)?;
    progress
        .unknown_alive_with_key(
            format!("reference-pipeline:{label}"),
            label.to_string(),
        )
        .await;
    let output = runner
        .run_pipeline(producer, consumer, cancel)
        .await
        .map_err(|pipeline_error| {
            let mut commands = pipeline_error.other_commands;
            if let Some(command) = command_record_from_tool_error(&pipeline_error.error) {
                commands.push(command);
            }
            TrackExecutionError::new(ConvertError::Tool(pipeline_error.error), commands)
        })?;
    let text = format!(
        "{}\n{}",
        output.consumer.stdout_tail, output.consumer.stderr_tail
    );
    let mut producer_record = output.producer.command;
    producer_record.description = Some(pipeline.producer.description.clone());
    let mut consumer_record = output.consumer.command;
    consumer_record.description = Some(pipeline.consumer.description.clone());
    Ok((vec![producer_record, consumer_record], text))
}

async fn probe_reference_carrier(
    path: &Path,
    description: &str,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_paths: &HashMap<String, PathBuf>,
    limits: Option<&Arc<ToolConcurrencyLimits>>,
    progress: &mut OperationProgressTracker<'_>,
    window_start: f32,
    window_end: f32,
    track_label: &str,
) -> Result<(Vec<CommandRecord>, ReferenceCarrierProbe), TrackExecutionError> {
    let mut records = Vec::new();
    let query = |flag: &'static str, field: &'static str| {
        build_reference_carrier_probe_command(path, description, flag, field)
    };

    async fn capture(
        planned: PlannedCommand,
        runner: &dyn ToolRunner,
        cancel: &CancellationToken,
        tool_paths: &HashMap<String, PathBuf>,
        limits: Option<&Arc<ToolConcurrencyLimits>>,
        progress: &mut OperationProgressTracker<'_>,
        window_start: f32,
        window_end: f32,
        label: &str,
    ) -> Result<(CommandRecord, String), TrackExecutionError> {
        let output = run_reference_capture_command(
            &planned,
            runner,
            cancel,
            tool_paths,
            limits,
            progress,
            window_start,
            window_end,
            label,
        )
        .await?;
        let text = format!("{}\n{}", output.stdout_tail, output.stderr_tail)
            .trim()
            .to_string();
        Ok((output.command, text))
    }

    let label = format!("{track_label} - verify {description}");
    let (record, sample_rate) = capture(
        query("-r", "sample rate"), runner, cancel, tool_paths, limits, progress,
        window_start, window_end, &label,
    ).await?;
    records.push(record);
    let (record, channels) = capture(
        query("-c", "channel count"), runner, cancel, tool_paths, limits, progress,
        window_start, window_end, &label,
    ).await?;
    records.push(record);
    let (record, bits) = capture(
        query("-b", "bit depth"), runner, cancel, tool_paths, limits, progress,
        window_start, window_end, &label,
    ).await?;
    records.push(record);
    let (record, samples) = capture(
        query("-s", "sample count"), runner, cancel, tool_paths, limits, progress,
        window_start, window_end, &label,
    ).await?;
    records.push(record);
    let (record, encoding) = capture(
        query("-e", "sample encoding"), runner, cancel, tool_paths, limits, progress,
        window_start, window_end, &label,
    ).await?;
    records.push(record);

    let parse = |value: &str, field: &str| -> Result<u64, TrackExecutionError> {
        value.trim().parse::<u64>().map_err(|_| {
            TrackExecutionError::new(
                ConvertError::Backend(format!(
                    "Reference {description} probe returned invalid {field}: {value:?}"
                )),
                records.clone(),
            )
        })
    };
    let sample_rate_hz = u32::try_from(parse(&sample_rate, "sample rate")?).map_err(|_| {
        TrackExecutionError::new(
            ConvertError::Backend(format!("Reference {description} sample rate overflows u32")),
            records.clone(),
        )
    })?;
    let channels = u16::try_from(parse(&channels, "channel count")?).map_err(|_| {
        TrackExecutionError::new(
            ConvertError::Backend(format!("Reference {description} channel count overflows u16")),
            records.clone(),
        )
    })?;
    let bits_per_sample = u16::try_from(parse(&bits, "bit depth")?).map_err(|_| {
        TrackExecutionError::new(
            ConvertError::Backend(format!("Reference {description} bit depth overflows u16")),
            records.clone(),
        )
    })?;
    let samples_per_channel = parse(&samples, "sample count")?;
    let encoding_lower = encoding.to_ascii_lowercase();
    let floating_point = encoding_lower.contains("floating") || encoding_lower.contains("float");
    let signed_integer = encoding_lower.contains("signed") && encoding_lower.contains("integer");
    if !floating_point && !signed_integer {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(format!(
                "Reference {description} probe returned unsupported encoding: {encoding:?}"
            )),
            records,
        ));
    }
    Ok((records, ReferenceCarrierProbe {
        sample_rate_hz,
        channels,
        bits_per_sample,
        samples_per_channel,
        floating_point,
        w64_structure: None,
    }))
}

fn reference_carrier_probe_digest(
    policy: tonepoet_pipeline::DsdReferencePolicyVersion,
    role: &str,
    probe: ReferenceCarrierProbe,
) -> Sha256Digest {
    let mut hasher = Sha256::new();
    if policy == tonepoet_pipeline::DsdReferencePolicyVersion::SoxNg14801V16 {
        hasher.update(b"tonepoet-reference-carrier-probe/v2\0");
    } else {
        // Preserve the frozen v1 identity exactly for append-only historical policies.
        hasher.update(b"tonepoet-reference-carrier-probe/v1\0");
    }
    hasher.update(role.as_bytes());
    hasher.update([0]);
    hasher.update(probe.sample_rate_hz.to_be_bytes());
    hasher.update(probe.channels.to_be_bytes());
    hasher.update(probe.bits_per_sample.to_be_bytes());
    hasher.update(probe.samples_per_channel.to_be_bytes());
    hasher.update([u8::from(probe.floating_point)]);
    if policy == tonepoet_pipeline::DsdReferencePolicyVersion::SoxNg14801V16 {
        match probe.w64_structure {
            Some(structure) => {
                hasher.update([1]);
                hasher.update(structure.physical_file_bytes.to_be_bytes());
                hasher.update(structure.declared_file_bytes.to_be_bytes());
                hasher.update(structure.chunk_count.to_be_bytes());
                hasher.update(structure.format_chunk_offset.to_be_bytes());
                match structure.fact_chunk_offset {
                    Some(offset) => {
                        hasher.update([1]);
                        hasher.update(offset.to_be_bytes());
                    }
                    None => hasher.update([0]),
                }
                hasher.update(structure.data_chunk_offset.to_be_bytes());
                hasher.update(structure.declared_data_bytes.to_be_bytes());
                hasher.update(structure.sample_frames.to_be_bytes());
                hasher.update(structure.alignment_padding_bytes.to_be_bytes());
            }
            None => hasher.update([0]),
        }
    }
    Sha256Digest(hasher.finalize().into())
}

fn reference_hash_args(carrier: &ReferenceDecodedCarrier) -> Vec<String> {
    let path = carrier.path();
    let authority = carrier.authority();
    vec![
        "-hide_banner".to_string(),
        "-nostdin".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-i".to_string(),
        path.display().to_string(),
        "-map".to_string(),
        "0:a:0".to_string(),
        "-map_metadata".to_string(),
        "-1".to_string(),
        "-vn".to_string(),
        "-sn".to_string(),
        "-dn".to_string(),
        "-c:a".to_string(),
        authority.hash_encoding().ffmpeg_codec().to_string(),
        "-f".to_string(),
        "hash".to_string(),
        "-hash".to_string(),
        "sha256".to_string(),
        "-".to_string(),
    ]
}

fn parse_reference_hash_output(text: &str) -> Result<Sha256Digest, String> {
    let mut values = text.lines().filter_map(|line| {
        let line = line.trim();
        line.strip_prefix("SHA256=")
            .or_else(|| line.strip_prefix("sha256="))
    });
    let value = values
        .next()
        .ok_or_else(|| "FFmpeg hash output omitted SHA256".to_string())?;
    if values.next().is_some() {
        return Err("FFmpeg hash output contained multiple SHA256 values".to_string());
    }
    Sha256Digest::from_hex(value)
}

async fn reference_decoded_sample_hash_with_plan_carrier(
    summary: &DsdReferencePlanSummary,
    selector: ReferenceDecodedCarrierSelector,
    description: &str,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_paths: &HashMap<String, PathBuf>,
    limits: Option<&Arc<ToolConcurrencyLimits>>,
    progress: &mut OperationProgressTracker<'_>,
    window_start: f32,
    window_end: f32,
    track_label: &str,
) -> Result<(Vec<CommandRecord>, Sha256Digest), TrackExecutionError> {
    let carrier = summary.decoded_carrier(selector).map_err(|error| {
        TrackExecutionError::new(ConvertError::Backend(error.to_string()), Vec::new())
    })?;
    let plan = build_reference_sample_hash_plan(&carrier, description).map_err(|message| {
        TrackExecutionError::new(ConvertError::Backend(message), Vec::new())
    })?;
    let (records, text) = match plan {
        PlannedReferenceSampleHash::Direct { command, .. } => {
            let output = run_reference_capture_command(
                &command,
                runner,
                cancel,
                tool_paths,
                limits,
                progress,
                window_start,
                window_end,
                &format!("{track_label} - {description}"),
            )
            .await?;
            let text = format!("{}\n{}", output.stdout_tail, output.stderr_tail);
            (vec![output.command], text)
        }
        PlannedReferenceSampleHash::StreamedFloat64W64 { pipeline, .. } => {
            run_reference_capture_pipeline(
                &pipeline,
                runner,
                cancel,
                limits,
                progress,
                &format!("{track_label} - {description}"),
            )
            .await?
        }
    };
    let digest = parse_reference_hash_output(&text).map_err(|message| {
        TrackExecutionError::new(ConvertError::Backend(message), records.clone())
    })?;
    Ok((records, digest))
}


fn inspect_reference_w64_structure(
    path: &Path,
    expected: W64PcmFormatExpectation,
    description: &str,
    records: &[CommandRecord],
) -> Result<W64ExactStructure, TrackExecutionError> {
    let mut file = File::open(path).map_err(|error| {
        TrackExecutionError::new(
            ConvertError::Backend(format!(
                "{} {description}: cannot open {}: {error}",
                reference_error_text(ReferenceErrorCode::W64StructuralIntegrity),
                path.display(),
            )),
            records.to_vec(),
        )
    })?;
    inspect_exact_w64_pcm(&mut file, expected).map_err(|error| {
        TrackExecutionError::new(
            ConvertError::Backend(format!(
                "{} {description}: {error}",
                reference_error_text(ReferenceErrorCode::W64StructuralIntegrity),
            )),
            records.to_vec(),
        )
    })
}

fn exact_reference_w64_structure(
    path: &Path,
    expected: W64PcmExpectation,
    description: &str,
    records: &[CommandRecord],
) -> Result<W64ExactStructure, TrackExecutionError> {
    let mut file = File::open(path).map_err(|error| {
        TrackExecutionError::new(
            ConvertError::Backend(format!(
                "{} {description}: cannot open {}: {error}",
                reference_error_text(ReferenceErrorCode::W64StructuralIntegrity),
                path.display(),
            )),
            records.to_vec(),
        )
    })?;
    validate_exact_w64_pcm(&mut file, expected).map_err(|error| {
        TrackExecutionError::new(
            ConvertError::Backend(format!(
                "{} {description}: {error}",
                reference_error_text(ReferenceErrorCode::W64StructuralIntegrity),
            )),
            records.to_vec(),
        )
    })
}

fn reference_w64_format_expectation(
    summary: &DsdReferencePlanSummary,
    bits_per_sample: u16,
    floating_point: bool,
) -> W64PcmFormatExpectation {
    W64PcmFormatExpectation {
        sample_rate_hz: summary.final_pcm.sample_rate_hz,
        channels: summary.final_pcm.channels,
        bits_per_sample,
        encoding: if floating_point {
            W64SampleEncoding::FloatingPoint
        } else {
            W64SampleEncoding::SignedInteger
        },
    }
}

fn reference_w64_expectation(
    summary: &DsdReferencePlanSummary,
    sample_frames: u64,
    bits_per_sample: u16,
    floating_point: bool,
) -> W64PcmExpectation {
    W64PcmExpectation {
        sample_rate_hz: summary.final_pcm.sample_rate_hz,
        channels: summary.final_pcm.channels,
        bits_per_sample,
        sample_frames,
        encoding: if floating_point {
            W64SampleEncoding::FloatingPoint
        } else {
            W64SampleEncoding::SignedInteger
        },
    }
}

fn build_reference_ffmpeg_full_traversal_command(
    path: &Path,
    description: &str,
) -> PlannedCommand {
    let mut command = PlannedCommand::new(
        ToolIdentifier::Ffmpeg,
        vec![
            "-hide_banner".to_string(),
            "-nostdin".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-xerror".to_string(),
            "-i".to_string(),
            path.display().to_string(),
            "-map".to_string(),
            "0:a:0".to_string(),
            "-map_metadata".to_string(),
            "-1".to_string(),
            "-vn".to_string(),
            "-sn".to_string(),
            "-dn".to_string(),
            "-f".to_string(),
            "null".to_string(),
            "-".to_string(),
        ],
        tonepoet_pipeline::InputSource::Path(path.to_path_buf()),
        tonepoet_pipeline::OutputSink::Stdout,
        None,
        description.to_string(),
    );
    command.environment_policy = tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet;
    command.environment = canonical_reference_environment();
    command
}

async fn verify_reference_r64_contract(
    summary: &DsdReferencePlanSummary,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_paths: &HashMap<String, PathBuf>,
    limits: Option<&Arc<ToolConcurrencyLimits>>,
    progress: &mut OperationProgressTracker<'_>,
    window_start: f32,
    window_end: f32,
    track_label: &str,
) -> Result<(Vec<CommandRecord>, ReferenceCarrierProbe), TrackExecutionError> {
    // Establish structural authority before invoking SoX's EOF-permissive reader.
    // The exact R64 data extent becomes the upstream frame authority for QPCM.
    let structure = inspect_reference_w64_structure(
        &summary.r64_path,
        reference_w64_format_expectation(summary, 64, true),
        "R64 exact structure rejected",
        &[],
    )?;
    let (records, mut probe) = probe_reference_carrier(
        &summary.r64_path,
        "R64",
        runner,
        cancel,
        tool_paths,
        limits,
        progress,
        window_start,
        window_end,
        track_label,
    )
    .await?;
    if probe.sample_rate_hz != summary.final_pcm.sample_rate_hz
        || probe.channels != summary.final_pcm.channels
        || probe.bits_per_sample != 64
        || !probe.floating_point
        || probe.samples_per_channel != structure.sample_frames
        || structure.sample_frames == 0
    {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(format!(
                "Reference R64 carrier contract mismatch: structure={structure:?}, probe={probe:?}"
            )),
            records,
        ));
    }
    probe.w64_structure = Some(structure);
    Ok((records, probe))
}

async fn verify_reference_qpcm_contract(
    summary: &DsdReferencePlanSummary,
    r64_probe: ReferenceCarrierProbe,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_paths: &HashMap<String, PathBuf>,
    limits: Option<&Arc<ToolConcurrencyLimits>>,
    progress: &mut OperationProgressTracker<'_>,
    window_start: f32,
    window_end: f32,
    track_label: &str,
) -> Result<(Vec<CommandRecord>, ReferenceCarrierProbe, Sha256Digest), TrackExecutionError> {
    let expected_bits = match summary.final_pcm.bit_depth {
        tonepoet_pipeline::PcmBitDepth::Int16 => 16,
        tonepoet_pipeline::PcmBitDepth::Int24 => 24,
        tonepoet_pipeline::PcmBitDepth::Float32 => 32,
        tonepoet_pipeline::PcmBitDepth::Float64 => 64,
        tonepoet_pipeline::PcmBitDepth::Int8 | tonepoet_pipeline::PcmBitDepth::Int32 => {
            return Err(TrackExecutionError::new(
                ConvertError::Backend(
                    "Reference verification received an unsupported terminal depth".to_string(),
                ),
                Vec::new(),
            ));
        }
    };
    let expected_float = matches!(
        summary.final_pcm.bit_depth,
        tonepoet_pipeline::PcmBitDepth::Float32 | tonepoet_pipeline::PcmBitDepth::Float64
    );

    let expected_sample_frames = r64_probe
        .w64_structure
        .ok_or_else(|| {
            TrackExecutionError::new(
                ConvertError::Backend(
                    "Reference QPCM verification lacks independent R64 frame authority".to_string(),
                ),
                Vec::new(),
            )
        })?
        .sample_frames;

    // Container authority is checked before the permissive SoX metadata reader.
    // A false RIFF/data extent can therefore never be concealed by reading to EOF.
    let structure = exact_reference_w64_structure(
        &summary.qpcm_path,
        reference_w64_expectation(
            summary,
            expected_sample_frames,
            expected_bits,
            expected_float,
        ),
        "QPCM exact structure rejected before publication",
        &[],
    )?;

    let (mut records, mut qpcm_probe) = probe_reference_carrier(
        &summary.qpcm_path,
        "QPCM",
        runner,
        cancel,
        tool_paths,
        limits,
        progress,
        window_start,
        window_end,
        track_label,
    )
    .await?;
    qpcm_probe.w64_structure = Some(structure);
    if qpcm_probe.sample_rate_hz != summary.final_pcm.sample_rate_hz
        || qpcm_probe.channels != summary.final_pcm.channels
        || qpcm_probe.bits_per_sample != expected_bits
        || qpcm_probe.floating_point != expected_float
        || qpcm_probe.samples_per_channel != expected_sample_frames
        || structure.sample_frames != expected_sample_frames
    {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(format!(
                "Reference QPCM carrier contract mismatch: r64={r64_probe:?}, qpcm={qpcm_probe:?}"
            )),
            records,
        ));
    }

    // FFmpeg is the independent standards-observing consumer. Decode the entire
    // carrier with -xerror even for Float64, where sample identity remains bound
    // through the qualified SoX raw-stream route because of FFmpeg scaling.
    let traversal = build_reference_ffmpeg_full_traversal_command(
        &summary.qpcm_path,
        "Verify complete QPCM Wave64 traversal with FFmpeg",
    );
    let traversal_output = run_reference_capture_command(
        &traversal,
        runner,
        cancel,
        tool_paths,
        limits,
        progress,
        window_start,
        window_end,
        &format!("{track_label} - verify exact QPCM Wave64 consumer traversal"),
    )
    .await
    .map_err(|mut error| {
        if matches!(
            &error.error,
            ConvertError::Tool(ToolRunnerError::Cancelled { .. })
        ) || matches!(
            &error.error,
            ConvertError::Realize(message) if message == "cancelled"
        ) {
            return error;
        }
        let detail = error.error.to_string();
        let mut all_records = records.clone();
        all_records.append(&mut error.commands);
        TrackExecutionError::new(
            ConvertError::Backend(format!(
                "{} independent FFmpeg full traversal rejected the exact Wave64 carrier: {detail}",
                reference_error_text(ReferenceErrorCode::W64StructuralIntegrity),
            )),
            all_records,
        )
    })?;
    records.push(traversal_output.command);

    let (mut hash_records, digest) = reference_decoded_sample_hash_with_plan_carrier(
        summary,
        ReferenceDecodedCarrierSelector::TerminalQpcm,
        "Hash QPCM decoded samples",
        runner,
        cancel,
        tool_paths,
        limits,
        progress,
        window_start,
        window_end,
        track_label,
    )
    .await?;
    records.append(&mut hash_records);
    Ok((records, qpcm_probe, digest))
}

pub(crate) async fn verify_reference_output_after_metadata(
    artifact: &mut super::types::TrackArtifact,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<(), ConvertError> {
    let path = artifact.staged_path.clone();
    let evidence = artifact.reference_evidence.as_mut().ok_or_else(|| {
        ConvertError::TrackValidation(
            "Reference post-metadata verification requires execution evidence".to_string(),
        )
    })?;
    let carrier = evidence
        .plan
        .bind_decoded_carrier(ReferenceDecodedCarrierSelector::PostMetadataOutput, &path)
        .map_err(|error| ConvertError::TrackValidation(error.to_string()))?;

    let resolved_ffmpeg = runner.resolved_tool_path(ToolBinary::Ffmpeg).ok_or_else(|| {
        ConvertError::TrackValidation(
            "Reference post-metadata verification cannot resolve the attested FFmpeg path"
                .to_string(),
        )
    })?;
    if resolved_ffmpeg != evidence.toolchain.ffmpeg.canonical_path {
        return Err(ConvertError::TrackValidation(format!(
            "Reference post-metadata verification FFmpeg changed after attestation: expected {}, got {}",
            evidence.toolchain.ffmpeg.canonical_path.display(),
            resolved_ffmpeg.display(),
        )));
    }

    let hash_plan = build_reference_sample_hash_plan(
        &carrier,
        "Verify post-metadata Reference decoded samples",
    )
    .map_err(ConvertError::TrackValidation)?;
    if hash_plan.authority().mechanism()
        == ReferenceDecodeMechanism::SoxFloat64W64RawStream
    {
        let resolved_sox = runner.resolved_tool_path(ToolBinary::Sox).ok_or_else(|| {
            ConvertError::TrackValidation(
                "Reference post-metadata verification cannot resolve the attested SoX path"
                    .to_string(),
            )
        })?;
        if resolved_sox != evidence.toolchain.sox_ng.canonical_path {
            return Err(ConvertError::TrackValidation(format!(
                "Reference post-metadata verification SoX changed after attestation: expected {}, got {}",
                evidence.toolchain.sox_ng.canonical_path.display(),
                resolved_sox.display(),
            )));
        }
    }

    let (records, text) = match hash_plan {
        PlannedReferenceSampleHash::StreamedFloat64W64 { pipeline, .. } => {
            let producer = planned_command_to_tool_command(
                &pipeline.producer,
                DEFAULT_PLANNED_COMMAND_TIMEOUT,
            )
            .map_err(ConvertError::from)?;
            let consumer = planned_command_to_tool_command(
                &pipeline.consumer,
                DEFAULT_PLANNED_COMMAND_TIMEOUT,
            )
            .map_err(ConvertError::from)?;
            let _pipeline_permits = acquire_reference_pipeline_permits(
                [producer.binary, consumer.binary],
                limits,
                cancel,
            )
            .await?;
            let output = runner
                .run_pipeline(producer, consumer, cancel)
                .await
                .map_err(|pipeline_error| ConvertError::Tool(pipeline_error.error))?;
            let text = format!(
                "{}\n{}",
                output.consumer.stdout_tail, output.consumer.stderr_tail
            );
            let mut producer_record = output.producer.command;
            producer_record.description = Some(pipeline.producer.description);
            let mut consumer_record = output.consumer.command;
            consumer_record.description = Some(pipeline.consumer.description);
            (vec![producer_record, consumer_record], text)
        }
        PlannedReferenceSampleHash::Direct { command: planned, .. } => {
            let command = planned_command_to_tool_command(
                &planned,
                DEFAULT_PLANNED_COMMAND_TIMEOUT,
            )
            .map_err(ConvertError::from)?;
            let output = run_tool_command_with_concurrency(command, runner, cancel, limits)
                .await
                .map_err(ConvertError::Tool)?;
            let text = format!("{}\n{}", output.stdout_tail, output.stderr_tail);
            let mut record = output.command;
            record.description = Some(planned.description);
            (vec![record], text)
        }
    };

    let digest = parse_reference_hash_output(&text).map_err(ConvertError::TrackValidation)?;
    if digest != evidence.pcm_verification.qpcm_sample_sha256 {
        return Err(ConvertError::TrackValidation(format!(
            "Reference metadata/artwork processing changed decoded samples for {}: expected {}, got {}",
            carrier.path().display(),
            evidence.pcm_verification.qpcm_sample_sha256.to_hex(),
            digest.to_hex(),
        )));
    }
    evidence.pcm_verification.post_metadata_sample_sha256 = Some(digest);
    evidence.pcm_verification.post_metadata_verification_command = records.last().cloned();
    evidence.pcm_verification.post_metadata_verification_commands = records;
    Ok(())
}

fn resolve_deferred_command(
    deferred: &PlannedDeferredCommand,
    measurements: &BTreeMap<MeasurementId, TruePeakMeasurement>,
) -> Result<PlannedCommand, TrackExecutionError> {
    resolve_reference_deferred_command(deferred, measurements).map_err(|message| {
        TrackExecutionError::new(ConvertError::Backend(message), Vec::new())
    })
}

enum ReferenceMeasurementContract<'a> {
    SoxPathOversampledStats,
    Float32FfmpegRawToSoxOversampledStats(&'a PlannedCommand),
}

fn reference_measurement_environment_is_canonical(command: &PlannedCommand) -> bool {
    command.environment_policy
        == tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
        && command.environment.len() == 1
        && command.environment.get("LC_ALL").map(String::as_str) == Some("C")
}

fn reference_measurement_deadline_is_canonical(
    summary: &DsdReferencePlanSummary,
    command: &PlannedCommand,
) -> bool {
    summary.analyzer_deadline
        >= Duration::from_secs(
            tonepoet_pipeline::REFERENCE_TRUE_PEAK_DEADLINE_STARTUP_SECONDS,
        )
        && summary.analyzer_deadline
            <= Duration::from_secs(
                tonepoet_pipeline::REFERENCE_TRUE_PEAK_MAX_DEADLINE_SECONDS,
            )
        && command.expected_duration == Some(summary.analyzer_deadline)
        && command.timeout_budget == Some(summary.analyzer_deadline)
}

fn validate_reference_package_pipeline(
    summary: &DsdReferencePlanSummary,
    pipeline: &PlannedCommandPipeline,
) -> Result<(), TrackExecutionError> {
    if summary.policy != tonepoet_pipeline::DsdReferencePolicyVersion::SoxNg14801V16
        || summary.final_pcm.bit_depth != tonepoet_pipeline::PcmBitDepth::Float64
        || !matches!(
            summary.target,
            tonepoet_pipeline::ResolvedOutputTarget::WavRiff
                | tonepoet_pipeline::ResolvedOutputTarget::WavRf64
        )
        || summary.packaged_path == summary.qpcm_path
    {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(
                "Reference policy v15 package pipeline is bound to an invalid plan cell"
                    .to_string(),
            ),
            Vec::new(),
        ));
    }

    let qpcm = summary.qpcm_path.display().to_string();
    let expected_producer = [
        "-S",
        "-D",
        qpcm.as_str(),
        "-t",
        "raw",
        "-e",
        "floating-point",
        "-b",
        "64",
        "-L",
        "-",
    ];
    if pipeline.producer.tool != ToolIdentifier::Sox
        || pipeline.producer.input.as_path() != Some(summary.qpcm_path.as_path())
        || pipeline.producer.output != tonepoet_pipeline::OutputSink::Stdout
        || !pipeline
            .producer
            .args
            .iter()
            .map(String::as_str)
            .eq(expected_producer)
        || !reference_measurement_environment_is_canonical(&pipeline.producer)
    {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(
                "Reference policy v15 package pipeline has a noncanonical SoX producer"
                    .to_string(),
            ),
            Vec::new(),
        ));
    }

    let packaged = summary.packaged_path.display().to_string();
    let sample_rate = summary.final_pcm.sample_rate_hz.to_string();
    let channels = summary.final_pcm.channels.to_string();
    let mut expected_consumer = vec![
        "-y",
        "-hide_banner",
        "-nostdin",
        "-f",
        "f64le",
        "-ar",
        sample_rate.as_str(),
        "-ac",
        channels.as_str(),
        "-i",
        "pipe:0",
        "-map",
        "0:a:0",
        "-map_metadata",
        "-1",
        "-vn",
        "-sn",
        "-dn",
        "-c:a",
        "pcm_f64le",
        "-f",
        "wav",
    ];
    if summary.target == tonepoet_pipeline::ResolvedOutputTarget::WavRf64 {
        expected_consumer.extend(["-rf64", "always"]);
    }
    expected_consumer.push(packaged.as_str());
    if pipeline.consumer.tool != ToolIdentifier::Ffmpeg
        || pipeline.consumer.input != tonepoet_pipeline::InputSource::Stdin
        || pipeline.consumer.output.as_path() != Some(summary.packaged_path.as_path())
        || !pipeline
            .consumer
            .args
            .iter()
            .map(String::as_str)
            .eq(expected_consumer)
        || !reference_measurement_environment_is_canonical(&pipeline.consumer)
    {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(
                "Reference policy v15 package pipeline has a noncanonical FFmpeg consumer"
                    .to_string(),
            ),
            Vec::new(),
        ));
    }
    Ok(())
}

fn validate_reference_measurement_binding(
    summary: &DsdReferencePlanSummary,
    measurement: &PlannedMeasurement,
) -> Result<(), TrackExecutionError> {
    let authority_count = summary
        .operations
        .iter()
        .filter(|operation| {
            matches!(
                operation,
                tonepoet_pipeline::DsdReferenceOperation::MeasureTruePeak {
                    measurement_id,
                    scope,
                    purpose,
                } if *measurement_id == measurement.id
                    && *scope == measurement.scope
                    && *purpose == measurement.purpose
            )
        })
        .count();
    if authority_count != 1 {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(format!(
                "Reference measurement {} is not bound exactly once in the plan summary",
                measurement.id.0
            )),
            Vec::new(),
        ));
    }

    let expected_carrier = match measurement.purpose {
        TruePeakPurpose::GainAuthority => summary.r64_path.as_path(),
        TruePeakPurpose::PostFinalAcceptance => summary.qpcm_path.as_path(),
    };
    if measurement.carrier_path() != Some(expected_carrier) {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(format!(
                "Reference measurement {} is bound to the wrong carrier path",
                measurement.id.0
            )),
            Vec::new(),
        ));
    }

    let float32_pipe = measurement.purpose == TruePeakPurpose::PostFinalAcceptance
        && summary.final_pcm.bit_depth == tonepoet_pipeline::PcmBitDepth::Float32;
    if measurement.input_stage.is_some() != float32_pipe {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(format!(
                "Reference measurement {} uses the wrong analyzer carrier route for {:?} {:?}",
                measurement.id.0, measurement.purpose, summary.final_pcm.bit_depth
            )),
            Vec::new(),
        ));
    }

    Ok(())
}

fn validate_reference_measurement_contract<'a>(
    summary: &DsdReferencePlanSummary,
    measurement: &'a PlannedMeasurement,
) -> Result<ReferenceMeasurementContract<'a>, TrackExecutionError> {
    if summary.policy != tonepoet_pipeline::DsdReferencePolicyVersion::SoxNg14801V16
        || measurement.parser != MeasurementParser::SoxStatsPkLevDbV1
    {
        return Err(TrackExecutionError::new(
            ConvertError::Backend("unknown or historical Reference measurement parser".to_string()),
            Vec::new(),
        ));
    }
    if measurement.command.tool != ToolIdentifier::Sox
        || measurement.command.output != tonepoet_pipeline::OutputSink::Stdout
        || !reference_measurement_environment_is_canonical(&measurement.command)
        || !reference_measurement_deadline_is_canonical(summary, &measurement.command)
    {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(
                "Reference policy v15 measurement has a noncanonical SoX analyzer command"
                    .to_string(),
            ),
            Vec::new(),
        ));
    }

    let expected_carrier = match measurement.purpose {
        TruePeakPurpose::GainAuthority => summary.r64_path.as_path(),
        TruePeakPurpose::PostFinalAcceptance => summary.qpcm_path.as_path(),
    };
    let expected_float32_pipe = measurement.purpose == TruePeakPurpose::PostFinalAcceptance
        && summary.final_pcm.bit_depth == tonepoet_pipeline::PcmBitDepth::Float32;
    if measurement.input_stage.is_some() != expected_float32_pipe {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(format!(
                "Reference measurement {} uses a transport that is not authorized for {:?} {:?}",
                measurement.id.0, measurement.purpose, summary.final_pcm.bit_depth
            )),
            Vec::new(),
        ));
    }
    let sample_rate = summary.final_pcm.sample_rate_hz.to_string();
    let channels = summary.final_pcm.channels.to_string();
    let oversampled_rate = summary
        .final_pcm
        .sample_rate_hz
        .checked_mul(tonepoet_pipeline::REFERENCE_TRUE_PEAK_OVERSAMPLE_FACTOR)
        .ok_or_else(|| {
            TrackExecutionError::new(
                ConvertError::Backend(
                    "Reference policy v15 analyzer rate exceeds the execution integer range"
                        .to_string(),
                ),
                Vec::new(),
            )
        })?
        .to_string();

    if let Some(producer) = measurement.input_stage.as_ref() {
        let carrier = producer.input.as_path().ok_or_else(|| {
            TrackExecutionError::new(
                ConvertError::Backend(
                    "Reference policy v15 Float32 measurement requires a path-backed W64 carrier"
                        .to_string(),
                ),
                Vec::new(),
            )
        })?;
        if carrier != expected_carrier {
            return Err(TrackExecutionError::new(
                ConvertError::Backend(format!(
                    "Reference measurement {} producer is bound to the wrong carrier path",
                    measurement.id.0
                )),
                Vec::new(),
            ));
        }
        let carrier_arg = expected_carrier.display().to_string();
        let producer_args = [
            "-nostdin", "-hide_banner", "-nostats", "-loglevel", "error", "-i",
            carrier_arg.as_str(), "-map", "0:a:0", "-vn", "-sn", "-dn", "-c:a",
            "pcm_f64le", "-f", "f64le", "pipe:1",
        ];
        let consumer_args = [
            "-S", "-D", "-t", "raw", "-e", "floating-point", "-b", "64", "-L",
            "-r", sample_rate.as_str(), "-c", channels.as_str(), "-", "-n", "rate",
            "-v", "-L", "-s", oversampled_rate.as_str(), "stats",
        ];
        if producer.tool != ToolIdentifier::Ffmpeg
            || producer.output != tonepoet_pipeline::OutputSink::Stdout
            || !producer.args.iter().map(String::as_str).eq(producer_args)
            || !reference_measurement_environment_is_canonical(producer)
            || producer.expected_duration != measurement.command.expected_duration
            || !reference_measurement_deadline_is_canonical(summary, producer)
            || measurement.command.input != tonepoet_pipeline::InputSource::Stdin
            || !measurement
                .command
                .args
                .iter()
                .map(String::as_str)
                .eq(consumer_args)
        {
            return Err(TrackExecutionError::new(
                ConvertError::Backend(
                    "Reference policy v15 measurement has a noncanonical FFmpeg-f64le-to-SoX oversampled contract"
                        .to_string(),
                ),
                Vec::new(),
            ));
        }
        return Ok(ReferenceMeasurementContract::Float32FfmpegRawToSoxOversampledStats(
            producer,
        ));
    }

    let carrier = measurement.command.input.as_path().ok_or_else(|| {
        TrackExecutionError::new(
            ConvertError::Backend(
                "Reference policy v15 direct SoX measurement requires a path-backed W64 carrier"
                    .to_string(),
            ),
            Vec::new(),
        )
    })?;
    if carrier != expected_carrier {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(format!(
                "Reference measurement {} direct analyzer is bound to the wrong carrier path",
                measurement.id.0
            )),
            Vec::new(),
        ));
    }
    let carrier_arg = expected_carrier.display().to_string();
    let direct_args = [
        "-S", "-D", carrier_arg.as_str(), "-n", "rate", "-v", "-L", "-s",
        oversampled_rate.as_str(), "stats",
    ];
    if !measurement
        .command
        .args
        .iter()
        .map(String::as_str)
        .eq(direct_args)
    {
        return Err(TrackExecutionError::new(
            ConvertError::Backend(
                "Reference policy v15 measurement has a noncanonical direct SoX oversampled contract"
                    .to_string(),
            ),
            Vec::new(),
        ));
    }
    Ok(ReferenceMeasurementContract::SoxPathOversampledStats)
}

async fn execute_reference_measurement(
    summary: &DsdReferencePlanSummary,
    measurement: &PlannedMeasurement,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    _tool_paths: &HashMap<String, PathBuf>,
    limits: Option<&Arc<ToolConcurrencyLimits>>,
    progress: &mut OperationProgressTracker<'_>,
    _window_start: f32,
    window_end: f32,
    track_label: &str,
    silence_scan_path: &Path,
    reporting_uncertainty: tonepoet_pipeline::DbNano,
    analyzer_residual: tonepoet_pipeline::DbNano,
) -> Result<(Vec<CommandRecord>, TruePeakMeasurement), TrackExecutionError> {
    let contract = validate_reference_measurement_contract(summary, measurement)?;
    let label = format!("{track_label} - {}", measurement.command.description);

    let (stderr_tail, mut records) = match contract {
        ReferenceMeasurementContract::Float32FfmpegRawToSoxOversampledStats(input_stage) => {
            let producer = planned_command_to_tool_command(
                input_stage,
                DEFAULT_PLANNED_COMMAND_TIMEOUT,
            )
            .map_err(TrackExecutionError::from)?;
            let consumer = planned_command_to_tool_command(
                &measurement.command,
                DEFAULT_PLANNED_COMMAND_TIMEOUT,
            )
            .map_err(TrackExecutionError::from)?;
            let _pipeline_permits = acquire_reference_pipeline_permits(
                [producer.binary, consumer.binary],
                limits,
                cancel,
            )
            .await
            .map_err(TrackExecutionError::from)?;
            progress
                .unknown_alive_with_key(
                    format!("reference-measurement-pipe:{label}"),
                    format!("{label} - decoding Float32 W64 and measuring qualified 16x view"),
                )
                .await;
            let output = runner
                .run_pipeline(producer, consumer, cancel)
                .await
                .map_err(|pipeline_error| {
                    let mut commands = pipeline_error.other_commands;
                    if let Some(command) = command_record_from_tool_error(&pipeline_error.error) {
                        commands.push(command);
                    }
                    TrackExecutionError::new(ConvertError::Tool(pipeline_error.error), commands)
                })?;
            let stderr_tail = output.consumer.stderr_tail.clone();
            let mut producer_record = output.producer.command;
            producer_record.description = Some(input_stage.description.clone());
            let mut consumer_record = output.consumer.command;
            consumer_record.description = Some(measurement.command.description.clone());
            (stderr_tail, vec![producer_record, consumer_record])
        }
        ReferenceMeasurementContract::SoxPathOversampledStats => {
            let command = planned_command_to_tool_command(
                &measurement.command,
                DEFAULT_PLANNED_COMMAND_TIMEOUT,
            )
            .map_err(TrackExecutionError::from)?;
            progress
                .unknown_alive_with_key(
                    format!("reference-measurement-direct:{}", measurement.id.0),
                    format!("{label} - measuring qualified 16x view"),
                )
                .await;
            let output = run_tool_command_with_concurrency(command, runner, cancel, limits)
                .await
                .map_err(|error| {
                    let commands = command_record_from_tool_error(&error).into_iter().collect();
                    TrackExecutionError::new(ConvertError::Tool(error), commands)
                })?;
            let stderr_tail = output.stderr_tail.clone();
            let mut record = output.command;
            record.description = Some(measurement.command.description.clone());
            (stderr_tail, vec![record])
        }
    };

    progress
        .estimated_with_key(
            window_end,
            format!("reference-measurement-finish:{}", measurement.id.0),
            format!("Finished {label}"),
        )
        .await;

    let raw_peak =
        extract_single_sox_stats_peak_report(&stderr_tail, summary.final_pcm.channels).map_err(
            |message| {
                TrackExecutionError::new(ConvertError::Backend(message), records.clone())
            },
        )?;
    let needs_silence_proof = raw_peak == "-inf";
    if needs_silence_proof {
        let selector = match measurement.purpose {
            TruePeakPurpose::GainAuthority => ReferenceDecodedCarrierSelector::ReconstructionR64,
            TruePeakPurpose::PostFinalAcceptance => ReferenceDecodedCarrierSelector::TerminalQpcm,
        };
        let carrier = summary.decoded_carrier(selector).map_err(|error| {
            TrackExecutionError::new(
                ConvertError::Backend(format!(
                    "Reference silence verification cannot authorize its carrier route: {error}"
                )),
                records.clone(),
            )
        })?;
        let silence_record = verify_signed_zero_audio(
            &carrier,
            runner,
            cancel,
            limits,
            silence_scan_path,
        )
            .await
            .map_err(|err| {
                let mut commands = records.clone();
                commands.extend(err.commands);
                TrackExecutionError::new(err.error, commands)
            })?;
        records.push(silence_record);
    }
    let parsed = parse_reference_sox_stats_true_peak_measurement(
        measurement.id,
        measurement.scope,
        measurement.purpose,
        raw_peak,
        reporting_uncertainty,
        analyzer_residual,
        needs_silence_proof,
    )
    .map_err(|message| {
        TrackExecutionError::new(ConvertError::Backend(message), records.clone())
    })?;
    Ok((records, parsed))
}

async fn verify_signed_zero_audio(
    carrier: &ReferenceDecodedCarrier,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    limits: Option<&Arc<ToolConcurrencyLimits>>,
    planned_raw_path: &Path,
) -> Result<CommandRecord, TrackExecutionError> {
    let mut raw = TemporaryFileCleanupGuard::new(planned_raw_path.to_path_buf())
    .map_err(|err| TrackExecutionError::new(ConvertError::Io(err), Vec::new()))?;
    let planned = build_reference_silence_scan_command(carrier, raw.path());
    let command = planned_command_to_tool_command(&planned, DEFAULT_PLANNED_COMMAND_TIMEOUT)
        .map_err(TrackExecutionError::from)?;
    let output = run_tool_command_with_concurrency(command, runner, cancel, limits)
        .await
        .map_err(|err| {
            let record = command_record_from_tool_error(&err).into_iter().collect();
            TrackExecutionError::new(ConvertError::Tool(err), record)
        })?;
    let scan_result = (|| -> Result<(), TrackExecutionError> {
        let mut file = File::open(raw.path()).map_err(|err| {
            TrackExecutionError::new(ConvertError::Io(err), vec![output.command.clone()])
        })?;
        let len = file
            .metadata()
            .map_err(|err| {
                TrackExecutionError::new(ConvertError::Io(err), vec![output.command.clone()])
            })?
            .len();
        if len == 0 || len % 8 != 0 {
            return Err(TrackExecutionError::new(
                ConvertError::Backend(
                    "Reference silence scan produced an empty or truncated f64 stream".to_string(),
                ),
                vec![output.command.clone()],
            ));
        }
        let mut buffer = [0_u8; 8 * 4096];
        let mut remaining = len;
        while remaining > 0 {
            let count = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
                TrackExecutionError::new(
                    ConvertError::Backend(
                        "Reference silence scan length does not fit this platform".to_string(),
                    ),
                    vec![output.command.clone()],
                )
            })?;
            file.read_exact(&mut buffer[..count]).map_err(|err| {
                TrackExecutionError::new(ConvertError::Io(err), vec![output.command.clone()])
            })?;
            validate_signed_zero_f64le(&buffer[..count]).map_err(|message| {
                TrackExecutionError::new(
                    ConvertError::Backend(message),
                    vec![output.command.clone()],
                )
            })?;
            remaining -= count as u64;
        }
        Ok(())
    })();
    scan_result?;
    raw.cleanup_now().map_err(|err| {
        TrackExecutionError::new(ConvertError::Io(err), vec![output.command.clone()])
    })?;
    Ok(output.command)
}

fn command_records_hash(records: &[CommandRecord]) -> String {
    fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }

    let mut hasher = Sha256::new();
    hasher.update(b"tonepoet-reference-resolved-command-transcript/v2\0");
    hasher.update((records.len() as u64).to_be_bytes());
    for record in records {
        let binary = format!("{:?}", record.binary);
        hash_bytes(&mut hasher, binary.as_bytes());
        hasher.update((record.sanitized_args.len() as u64).to_be_bytes());
        for arg in &record.sanitized_args {
            hash_bytes(&mut hasher, arg.as_bytes());
        }
        hasher.update([match record.environment_policy {
            tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet => 0_u8,
            tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet => 1_u8,
        }]);
        hasher.update((record.environment.len() as u64).to_be_bytes());
        for (key, value) in &record.environment {
            hash_bytes(&mut hasher, key.as_bytes());
            hash_bytes(&mut hasher, value.as_bytes());
        }
    }
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn execute_commands(
    commands: &[PlannedCommand],
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_paths: &HashMap<String, PathBuf>,
    tool_concurrency_limits: Option<Arc<ToolConcurrencyLimits>>,
    progress: &mut OperationProgressTracker<'_>,
    start_fraction: f32,
    end_fraction: f32,
    track_label: String,
) -> Result<Vec<CommandRecord>, TrackExecutionError> {
    let mut records = Vec::with_capacity(commands.len());
    let windows = command_windows(commands, start_fraction, end_fraction);

    for (index, planned) in commands.iter().enumerate() {
        if cancel.is_cancelled() {
            progress.cancel_requested().await;
            return Err(TrackExecutionError::new(
                ConvertError::Realize("cancelled".to_string()),
                records,
            ));
        }
        let (window_start, window_end) = windows[index];
        let description = user_facing_planned_description(&planned.description);
        let label = format!(
            "{} - step {} of {} - {}",
            track_label,
            index + 1,
            commands.len(),
            description
        );
        progress
            .estimated_with_key(
                window_start,
                format!("cmd-start:{index}"),
                format!("Starting {label}"),
            )
            .await;

        let mut cmd = match planned_command_to_tool_command(planned, DEFAULT_PLANNED_COMMAND_TIMEOUT) {
            Ok(cmd) => cmd,
            Err(err) => return Err(TrackExecutionError::new(err, records)),
        };
        let _tool_permit = match acquire_tool_permit(&mut cmd, tool_concurrency_limits.as_ref(), cancel).await {
            Ok(permit) => permit,
            Err(err) => {
                let err = annotate_tool_error(tool_permit_error_to_runner_error(err, &cmd), planned);
                return Err(track_execution_error_from_tool_error(index, planned, err, records));
            }
        };
        let output = match run_planned_command(
            cmd,
            planned,
            runner,
            cancel,
            tool_paths,
            progress,
            window_start,
            window_end,
            &label,
        )
        .await
        {
            Ok(output) => output,
            Err(err) => {
                let err = annotate_tool_error(err, planned);
                return Err(track_execution_error_from_tool_error(index, planned, err, records));
            }
        };
        let mut record = output.command;
        record.description = non_empty_planned_description(planned);
        records.push(record);

        progress
            .estimated_with_key(
                window_end,
                format!("cmd-finish:{index}"),
                format!("Finished {label}"),
            )
            .await;
    }

    Ok(records)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ToolConcurrencyFamily {
    // This declaration order is the frozen global multi-resource acquisition rank.
    Sox,
    Ffmpeg,
    Ssrc,
}

fn tool_concurrency_family(binary: ToolBinary) -> Option<ToolConcurrencyFamily> {
    match binary {
        ToolBinary::Sox => Some(ToolConcurrencyFamily::Sox),
        ToolBinary::Ffmpeg | ToolBinary::Ffprobe => Some(ToolConcurrencyFamily::Ffmpeg),
        ToolBinary::Ssrc => Some(ToolConcurrencyFamily::Ssrc),
        _ => None,
    }
}

#[derive(Debug)]
struct ReferencePipelinePermitSet {
    // A single RAII owner makes partial acquisition cancellation-safe: any
    // already-acquired permit is dropped automatically if a later acquire fails.
    _permits: Vec<OwnedSemaphorePermit>,
}

#[derive(Debug)]
pub(crate) enum SegmentedPipelineExecutionError {
    Admission(ConvertError),
    Tool(ToolSegmentedPipelineError),
}

pub(crate) async fn run_segmented_tool_pipeline_with_concurrency(
    producer: ToolCommand,
    segments: Vec<ToolStreamSegment>,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<ToolSegmentedPipelineOutput, SegmentedPipelineExecutionError> {
    let binaries = std::iter::once(producer.binary)
        .chain(segments.iter().map(|segment| segment.consumer.binary));
    let _permits = acquire_reference_pipeline_permits(binaries, limits, cancel)
        .await
        .map_err(SegmentedPipelineExecutionError::Admission)?;
    runner
        .run_segmented_pipeline(producer, segments, cancel)
        .await
        .map_err(SegmentedPipelineExecutionError::Tool)
}

async fn acquire_reference_pipeline_permits(
    binaries: impl IntoIterator<Item = ToolBinary>,
    limits: Option<&Arc<ToolConcurrencyLimits>>,
    cancel: &CancellationToken,
) -> Result<Option<ReferencePipelinePermitSet>, ConvertError> {
    let Some(limits) = limits else {
        return Ok(None);
    };

    let families = binaries
        .into_iter()
        .filter_map(tool_concurrency_family)
        .collect::<BTreeSet<_>>();
    let mut permits = Vec::with_capacity(families.len());
    for family in families {
        let semaphore = match family {
            ToolConcurrencyFamily::Sox => limits.sox.clone(),
            ToolConcurrencyFamily::Ffmpeg => limits.ffmpeg.clone(),
            ToolConcurrencyFamily::Ssrc => limits.ssrc.clone(),
        };
        permits.push(acquire_owned_permit(semaphore, cancel).await?);
    }

    Ok(Some(ReferencePipelinePermitSet { _permits: permits }))
}

async fn acquire_tool_permit(
    cmd: &mut ToolCommand,
    limits: Option<&Arc<ToolConcurrencyLimits>>,
    cancel: &CancellationToken,
) -> Result<Option<OwnedSemaphorePermit>, ConvertError> {
    let Some(limits) = limits else {
        return Ok(None);
    };

    match cmd.binary {
        ToolBinary::Sox => {
            if cmd.environment_policy
                == tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet
            {
                set_sox_omp_threads(cmd, limits.sox_omp_threads());
            }
            acquire_owned_permit(limits.sox.clone(), cancel)
                .await
                .map(Some)
        }
        ToolBinary::Ffmpeg | ToolBinary::Ffprobe => acquire_owned_permit(limits.ffmpeg.clone(), cancel)
            .await
            .map(Some),
        ToolBinary::Ssrc => acquire_owned_permit(limits.ssrc.clone(), cancel)
            .await
            .map(Some),
        _ => Ok(None),
    }
}

async fn acquire_owned_permit(
    semaphore: Arc<Semaphore>,
    cancel: &CancellationToken,
) -> Result<OwnedSemaphorePermit, ConvertError> {
    if cancel.is_cancelled() {
        return Err(ConvertError::Realize("cancelled".to_string()));
    }

    tokio::select! {
        biased;

        _ = cancel.cancelled() => Err(ConvertError::Realize("cancelled".to_string())),
        permit = semaphore.acquire_owned() => permit.map_err(|_| {
            ConvertError::Backend("tool concurrency semaphore closed".to_string())
        }),
    }
}

pub(crate) async fn run_tool_command_with_concurrency(
    mut cmd: ToolCommand,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    limits: Option<&Arc<ToolConcurrencyLimits>>,
) -> Result<ToolOutput, ToolRunnerError> {
    let _tool_permit = acquire_tool_permit(&mut cmd, limits, cancel)
        .await
        .map_err(|err| tool_permit_error_to_runner_error(err, &cmd))?;
    runner.run(cmd, cancel).await
}

fn tool_permit_error_to_runner_error(err: ConvertError, cmd: &ToolCommand) -> ToolRunnerError {
    match err {
        ConvertError::Realize(message) if message == "cancelled" => ToolRunnerError::Cancelled {
            command: command_record_for_unstarted_command(cmd),
        },
        other => ToolRunnerError::Io(io::Error::new(io::ErrorKind::Other, other.to_string())),
    }
}

fn command_record_for_unstarted_command(cmd: &ToolCommand) -> CommandRecord {
    CommandRecord {
        environment_policy: cmd.environment_policy,
        environment: cmd.sanitized_environment(),
        description: None,
        binary: cmd.binary,
        sanitized_args: cmd.sanitized_args(),
        cwd: cmd.cwd.clone(),
        env_keys: cmd.env_keys(),
        exit: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        elapsed: Duration::ZERO,
    }
}

fn non_empty_planned_description(planned: &PlannedCommand) -> Option<String> {
    let description = user_facing_planned_description(&planned.description);
    let description = description.trim();
    if description.is_empty() {
        None
    } else {
        Some(description.to_string())
    }
}

fn user_facing_planned_description(description: &str) -> String {
    // FFmpeg still requires `-strict experimental` for true 32-bit FLAC on
    // supported builds, but that compatibility switch is not a meaningful
    // user-facing encoder qualification. Keep the operational argument while
    // removing the derived qualifier from both live progress and command logs.
    description
        .replace(" with FFmpeg experimental encoder", " with FFmpeg")
        .replace("FFmpeg experimental encoder", "FFmpeg")
}

fn set_sox_omp_threads(cmd: &mut ToolCommand, threads: u32) {
    cmd.env.retain(|var| !is_omp_num_threads_key(&var.key));
    cmd.env.push(EnvVar {
        key: "OMP_NUM_THREADS".to_string(),
        value: super::types::SecretString::new(threads.to_string()),
        secret: false,
    });
}

#[cfg(windows)]
fn is_omp_num_threads_key(key: &str) -> bool {
    key.eq_ignore_ascii_case("OMP_NUM_THREADS")
}

#[cfg(not(windows))]
fn is_omp_num_threads_key(key: &str) -> bool {
    key == "OMP_NUM_THREADS"
}

#[cfg(test)]
const CAPTURE_PLANNED_COMMAND_FOR_TEST_ARG: &str = "__tonepoet_capture_planned_command_for_test__";

#[cfg(test)]
fn should_capture_planned_command_for_test(cmd: &ToolCommand) -> bool {
    cmd.args
        .iter()
        .any(|arg| arg == CAPTURE_PLANNED_COMMAND_FOR_TEST_ARG)
}

#[cfg(test)]
fn successful_captured_tool_output_for_test(cmd: ToolCommand) -> ToolOutput {
    let elapsed = Duration::from_millis(1);
    let sanitized_args = cmd.sanitized_args();
    let environment = cmd.sanitized_environment();
    let env_keys = cmd.env_keys();
    let stdout_tail = cmd
        .env
        .iter()
        .map(|var| format!("{}={}", var.key, var.value.expose()))
        .collect::<Vec<_>>()
        .join("\n");
    ToolOutput {
        exit: ProcessExit::Code(0),
        stdout_tail: stdout_tail.clone(),
        stderr_tail: String::new(),
        elapsed,
        command: CommandRecord {
            environment_policy: cmd.environment_policy,
            environment,
            description: None,
            binary: cmd.binary,
            sanitized_args,
            cwd: cmd.cwd.clone(),
            env_keys,
            exit: Some(ProcessExit::Code(0)),
            stdout_tail,
            stderr_tail: String::new(),
            elapsed,
        },
    }
}

async fn run_planned_command(
    cmd: ToolCommand,
    planned: &PlannedCommand,
    runner: &dyn ToolRunner,
    cancel: &CancellationToken,
    tool_paths: &HashMap<String, PathBuf>,
    progress: &mut OperationProgressTracker<'_>,
    window_start: f32,
    window_end: f32,
    label: &str,
) -> Result<super::tool::ToolOutput, ToolRunnerError> {
    #[cfg(test)]
    if TRACK_EXECUTION_USE_INJECTED_RUNNER.try_with(|_| ()).is_ok() {
        return runner.run(cmd, cancel).await;
    }

    #[cfg(test)]
    if should_capture_planned_command_for_test(&cmd) {
        return Ok(successful_captured_tool_output_for_test(cmd));
    }

    match cmd.binary {
        ToolBinary::Ffmpeg => {
            let expected = planned.expected_duration.unwrap_or(Duration::ZERO);
            run_streaming_tool_with_probe_with_tool_paths(
                cmd,
                cancel,
                Some(progress),
                Some(StreamingHeartbeat::new(
                    format!("ffmpeg-heartbeat:{label}"),
                    format!("{label} - still running"),
                )),
                tool_paths,
                move |source: StreamSource, line: &str| {
                    if source == StreamSource::Stderr {
                        probes::ffmpeg::parse_line(line, expected, window_start, window_end, label)
                    } else {
                        None
                    }
                },
            )
            .await
        }
        ToolBinary::Sox => {
            run_streaming_tool_with_probe_with_tool_paths(
                cmd,
                cancel,
                Some(progress),
                Some(StreamingHeartbeat::new(
                    format!("sox-heartbeat:{label}"),
                    probes::sox::unknown_fallback_message(label),
                )),
                tool_paths,
                move |_source: StreamSource, line: &str| {
                    probes::sox::parse_line(line, window_start, window_end, label)
                },
            )
            .await
        }
        _ => {
            progress
                .unknown_alive_with_key(
                    format!("tool-heartbeat:{label}"),
                    format!("{label} - running"),
                )
                .await;
            runner.run(cmd, cancel).await
        }
    }
}

fn command_windows(
    commands: &[PlannedCommand],
    start_fraction: f32,
    end_fraction: f32,
) -> Vec<(f32, f32)> {
    if commands.is_empty() {
        return Vec::new();
    }
    let weights: Vec<f32> = commands
        .iter()
        .map(|cmd| cmd.expected_duration.map(|d| d.as_secs_f32()).unwrap_or(1.0).max(1.0))
        .collect();
    let total: f32 = weights.iter().sum();
    let width = (end_fraction - start_fraction).max(0.0);
    let mut cursor = start_fraction;
    weights
        .into_iter()
        .map(|weight| {
            let next = cursor + width * weight / total;
            let window = (cursor, next);
            cursor = next;
            window
        })
        .collect()
}


struct TrackExecutionCleanupState {
    planner_paths: Vec<PathBuf>,
    active_blocking_workers: usize,
    cleanup_requested: bool,
    cleanup_complete: bool,
}

struct TrackExecutionCleanupShared {
    work_dir: PathBuf,
    state: Mutex<TrackExecutionCleanupState>,
    _work_permit: OwnedSemaphorePermit,
}

struct TrackExecutionCleanupGuard {
    shared: Arc<TrackExecutionCleanupShared>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackCleanupOutcome {
    Complete,
    DeferredToBlockingWorker,
}

static TRACK_WORK_SEMAPHORES: OnceLock<Mutex<HashMap<PathBuf, Weak<Semaphore>>>> =
    OnceLock::new();

fn track_work_semaphore(work_dir: &Path) -> Arc<Semaphore> {
    let registry = TRACK_WORK_SEMAPHORES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.retain(|_, semaphore| semaphore.strong_count() != 0);
    if let Some(semaphore) = registry.get(work_dir).and_then(Weak::upgrade) {
        return semaphore;
    }
    let semaphore = Arc::new(Semaphore::new(1));
    registry.insert(work_dir.to_path_buf(), Arc::downgrade(&semaphore));
    semaphore
}

impl TrackExecutionCleanupGuard {
    async fn acquire(
        work_dir: PathBuf,
        cancel: &CancellationToken,
    ) -> Result<Self, TrackExecutionError> {
        if cancel.is_cancelled() {
            return Err(TrackExecutionError::new(
                ConvertError::Realize("cancelled".to_string()),
                Vec::new(),
            ));
        }

        let semaphore = track_work_semaphore(&work_dir);
        let work_permit = tokio::select! {
            biased;

            _ = cancel.cancelled() => {
                return Err(TrackExecutionError::new(
                    ConvertError::Realize("cancelled".to_string()),
                    Vec::new(),
                ));
            }
            permit = semaphore.acquire_owned() => permit.map_err(|_| {
                TrackExecutionError::new(
                    ConvertError::Backend(
                        "track work-directory coordination semaphore was closed".to_string(),
                    ),
                    Vec::new(),
                )
            })?,
        };
        Ok(Self {
            shared: Arc::new(TrackExecutionCleanupShared {
                work_dir,
                state: Mutex::new(TrackExecutionCleanupState {
                    planner_paths: Vec::new(),
                    active_blocking_workers: 0,
                    cleanup_requested: false,
                    cleanup_complete: false,
                }),
                _work_permit: work_permit,
            }),
        })
    }

    fn add_planner_paths(&self, paths: &[PathBuf]) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for path in paths {
            if !state.planner_paths.contains(path) {
                state.planner_paths.push(path.clone());
            }
        }
    }

    fn blocking_worker_lease(&self) -> Result<TrackExecutionBlockingWorkerLease, TrackExecutionError> {
        let mut state = self.shared.state.lock().map_err(|_| {
            TrackExecutionError::new(
                ConvertError::Backend("track cleanup authority lock was poisoned".to_string()),
                Vec::new(),
            )
        })?;
        if state.cleanup_requested || state.cleanup_complete {
            return Err(TrackExecutionError::new(
                ConvertError::Backend(
                    "cannot start Reference materialization after cleanup was requested".to_string(),
                ),
                Vec::new(),
            ));
        }
        state.active_blocking_workers = state
            .active_blocking_workers
            .checked_add(1)
            .ok_or_else(|| {
                TrackExecutionError::new(
                    ConvertError::Backend("track cleanup worker count overflow".to_string()),
                    Vec::new(),
                )
            })?;
        drop(state);
        Ok(TrackExecutionBlockingWorkerLease {
            shared: self.shared.clone(),
            released: false,
        })
    }

    fn cleanup_now(&self) -> io::Result<TrackCleanupOutcome> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| io::Error::other("track cleanup authority lock was poisoned"))?;
        if state.cleanup_complete {
            return Ok(TrackCleanupOutcome::Complete);
        }
        state.cleanup_requested = true;
        if state.active_blocking_workers != 0 {
            return Ok(TrackCleanupOutcome::DeferredToBlockingWorker);
        }
        cleanup_declared_paths_and_work_dir(&state.planner_paths, &self.shared.work_dir)?;
        state.cleanup_complete = true;
        Ok(TrackCleanupOutcome::Complete)
    }
}

impl Drop for TrackExecutionCleanupGuard {
    fn drop(&mut self) {
        match self.cleanup_now() {
            Ok(TrackCleanupOutcome::Complete | TrackCleanupOutcome::DeferredToBlockingWorker) => {}
            Err(error) => log::warn!(
                "best-effort track cleanup failed for {}: {error}",
                self.shared.work_dir.display()
            ),
        }
    }
}

struct TrackExecutionBlockingWorkerLease {
    shared: Arc<TrackExecutionCleanupShared>,
    released: bool,
}

impl TrackExecutionBlockingWorkerLease {
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active_blocking_workers = state.active_blocking_workers.saturating_sub(1);
        if state.active_blocking_workers == 0
            && state.cleanup_requested
            && !state.cleanup_complete
        {
            match cleanup_declared_paths_and_work_dir(
                &state.planner_paths,
                &self.shared.work_dir,
            ) {
                Ok(()) => state.cleanup_complete = true,
                Err(error) => log::warn!(
                    "deferred track cleanup failed for {} after blocking materialization exited: {error}",
                    self.shared.work_dir.display()
                ),
            }
        }
    }
}

impl Drop for TrackExecutionBlockingWorkerLease {
    fn drop(&mut self) {
        self.release();
    }
}

struct TemporaryFileCleanupGuard {
    path: PathBuf,
    cleaned: bool,
}

impl TemporaryFileCleanupGuard {
    fn new(path: PathBuf) -> io::Result<Self> {
        remove_cleanup_path(&path)?;
        Ok(Self {
            path,
            cleaned: false,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn cleanup_now(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        remove_cleanup_path(&self.path)?;
        if cleanup_path_is_present(&self.path)? {
            return Err(io::Error::other(format!(
                "temporary cleanup path remains: {}",
                self.path.display()
            )));
        }
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for TemporaryFileCleanupGuard {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup_now() {
            log::warn!(
                "best-effort temporary cleanup failed for {}: {error}",
                self.path.display()
            );
        }
    }
}

fn reset_track_work_dir(work_dir: &Path) -> Result<(), ConvertError> {
    if work_dir.exists() {
        fs::remove_dir_all(work_dir)?;
    }
    fs::create_dir_all(work_dir)?;
    Ok(())
}

fn remove_cleanup_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "planner-declared file cleanup path is a directory: {}",
                path.display()
            ),
        )),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn cleanup_path_is_present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn cleanup_paths_checked(paths: &[PathBuf]) -> io::Result<()> {
    let mut failures = Vec::new();
    for path in paths {
        if let Err(error) = remove_cleanup_path(path) {
            failures.push(format!("{}: {error}", path.display()));
        }
    }
    let mut remaining = Vec::new();
    for path in paths {
        match cleanup_path_is_present(path) {
            Ok(true) => remaining.push(path.display().to_string()),
            Ok(false) => {}
            Err(error) => failures.push(format!(
                "could not verify cleanup of {}: {error}",
                path.display()
            )),
        }
    }
    if failures.is_empty() && remaining.is_empty() {
        Ok(())
    } else {
        let mut details = failures;
        if !remaining.is_empty() {
            details.push(format!("paths remain: {}", remaining.join(", ")));
        }
        Err(io::Error::other(format!(
            "planner cleanup failed: {}",
            details.join("; ")
        )))
    }
}

fn cleanup_track_work_dir(work_dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(work_dir) {
        Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir_all(work_dir)?,
        Ok(_) => fs::remove_file(work_dir)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if cleanup_path_is_present(work_dir)? {
        Err(io::Error::other(format!(
            "track work directory remains: {}",
            work_dir.display()
        )))
    } else {
        Ok(())
    }
}

fn cleanup_declared_paths_and_work_dir(
    paths: &[PathBuf],
    work_dir: &Path,
) -> io::Result<()> {
    let mut first_error = cleanup_paths_checked(paths).err();
    if let Err(error) = cleanup_track_work_dir(work_dir) {
        if first_error.is_none() {
            first_error = Some(error);
        }
    }

    let mut remaining = Vec::new();
    for path in paths {
        if cleanup_path_is_present(path).unwrap_or(true) {
            remaining.push(path.display().to_string());
        }
    }
    if cleanup_path_is_present(work_dir).unwrap_or(true) {
        remaining.push(work_dir.display().to_string());
    }
    if remaining.is_empty() {
        Ok(())
    } else {
        Err(first_error.unwrap_or_else(|| {
            io::Error::other(format!(
                "track cleanup left governed paths: {}",
                remaining.join(", ")
            ))
        }))
    }
}

fn copy_to_work_path(input: &Path, work_path: &Path) -> Result<(), ConvertError> {
    if let Some(parent) = work_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = work_path.with_extension(format!(
        "{}.tmp",
        work_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("tonepoet")
    ));
    let _ = fs::remove_file(&tmp);
    fs::copy(input, &tmp)?;
    // Cross-platform deterministic replacement: Windows refuses to rename over
    // an existing destination, while Unix replaces it. Remove stale work output
    // explicitly so reruns after interruption behave the same everywhere.
    let _ = fs::remove_file(work_path);
    fs::rename(&tmp, work_path)?;
    Ok(())
}

fn apply_finalization(finalization: &Finalization) -> Result<(), ConvertError> {
    match finalization {
        Finalization::AtomicRename { from, to } => {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            // The publish stage owns final destination collision behavior. Track
            // conversion finalization targets only staging paths. Replace stale
            // staging files from an interrupted previous run deterministically.
            let _ = fs::remove_file(to);
            fs::rename(from, to)?;
            Ok(())
        }
    }
}

fn annotate_tool_error(mut err: ToolRunnerError, planned: &PlannedCommand) -> ToolRunnerError {
    let description = non_empty_planned_description(planned);
    match &mut err {
        ToolRunnerError::Spawn { command }
        | ToolRunnerError::Timeout { command, .. }
        | ToolRunnerError::Cancelled { command }
        | ToolRunnerError::Termination { command, .. }
        | ToolRunnerError::NonZeroExit { command, .. } => {
            command.description = description;
        }
        ToolRunnerError::UnsupportedPipeline | ToolRunnerError::Io(_) => {}
    }
    err
}

fn track_execution_error_from_tool_error(
    index: usize,
    planned: &PlannedCommand,
    err: ToolRunnerError,
    mut records: Vec<CommandRecord>,
) -> TrackExecutionError {
    let message = planned_tool_error_message(index, planned, &err);
    if let Some(record) = command_record_from_tool_error(&err) {
        records.push(record);
    }
    TrackExecutionError::new(format_tool_error(index, planned, err), records).with_message(message)
}

fn planned_tool_error_message(
    index: usize,
    planned: &PlannedCommand,
    err: &ToolRunnerError,
) -> String {
    let description = user_facing_planned_description(&planned.description);
    match err {
        ToolRunnerError::NonZeroExit { stderr_tail, .. } => format!(
            "planned command {} failed ({}): {}",
            index + 1,
            description,
            stderr_tail
        ),
        ToolRunnerError::Timeout { elapsed, .. } => format!(
            "planned command {} timed out after {:?} ({})",
            index + 1,
            elapsed,
            description
        ),
        ToolRunnerError::Cancelled { .. } => "cancelled".to_string(),
        ToolRunnerError::Spawn { .. } => format!(
            "planned command {} failed to start ({})",
            index + 1,
            description
        ),
        ToolRunnerError::UnsupportedPipeline => format!(
            "planned command {} requires a runner with typed pipeline support ({})",
            index + 1,
            description
        ),
        ToolRunnerError::Termination { message, .. } => format!(
            "planned command {} could not prove process termination/reaping ({}): {}",
            index + 1,
            description,
            message
        ),
        ToolRunnerError::Io(err) => format!(
            "planned command {} failed before execution ({}): {}",
            index + 1,
            description,
            err
        ),
    }
}

fn format_tool_error(
    _index: usize,
    _planned: &PlannedCommand,
    err: ToolRunnerError,
) -> ConvertError {
    match err {
        ToolRunnerError::Cancelled { .. } => ConvertError::Realize("cancelled".to_string()),
        other => ConvertError::Tool(other),
    }
}

fn command_record_from_tool_error(err: &ToolRunnerError) -> Option<CommandRecord> {
    match err {
        ToolRunnerError::Spawn { command }
        | ToolRunnerError::Timeout { command, .. }
        | ToolRunnerError::Cancelled { command }
        | ToolRunnerError::Termination { command, .. }
        | ToolRunnerError::NonZeroExit { command, .. } => Some(command.clone()),
        ToolRunnerError::UnsupportedPipeline | ToolRunnerError::Io(_) => None,
    }
}

fn track_label(track: &PreparedTrack) -> String {
    track
        .metadata
        .title
        .as_ref()
        .map(|title| format!("track {} ({title})", track.id.track_number))
        .unwrap_or_else(|| format!("track {}", track.id.track_number))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use tonepoet_pipeline::{extract_single_loudnorm_report, parse_reference_true_peak_measurement};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use async_trait::async_trait;

    use crate::convert::pipeline::tool::blocking_test_runner::{
        tool_gate, BlockingToolRunner, ToolBehavior,
    };
    use crate::convert::pipeline::tool::StubToolRunner;
    use crate::convert::pipeline::types::{
        CueSidecarPolicy, DvdaDownmixPolicy, DvdaGroupSelection, FailurePolicy, LogPolicy,
        NamingCollisionPolicy, NamingPolicy, OverwritePolicy, PipelineStage, PublishPolicy,
        SacdArea, SourceAudioCoding, SourceAudioDescriptor, SourceOptions, StagePolicy,
        StageRequirement, TrackId, TrackMetadata, TrackSelection, TrackSourceRef,
    };
    use tempfile::TempDir;
    use tonepoet_pipeline::{AudioFormat, InputSource, OutputSink, PipelineSettings, ToolIdentifier};

    use super::*;

    #[test]
    fn true_32_bit_flac_description_omits_experimental_encoder_qualifier() {
        assert_eq!(
            user_facing_planned_description(
                "Encode true 32-bit FLAC with FFmpeg experimental encoder",
            ),
            "Encode true 32-bit FLAC with FFmpeg",
        );
        assert_eq!(
            user_facing_planned_description("Encode FLAC with FFmpeg"),
            "Encode FLAC with FFmpeg",
        );
    }

    fn valid_loudnorm_json(input_tp: &str) -> String {
        format!(
            r#"{{
                "input_i": "-23.00",
                "input_tp": "{input_tp}",
                "input_lra": "0.10",
                "input_thresh": "-33.00",
                "output_i": "-23.00",
                "output_tp": "-1.00",
                "output_lra": "0.10",
                "output_thresh": "-33.00",
                "normalization_type": "linear",
                "target_offset": "0.00"
            }}"#
        )
    }

    #[test]
    fn loudnorm_parser_accepts_only_one_complete_strict_report() {
        let json = valid_loudnorm_json("-3.125000000");
        assert_eq!(
            extract_single_loudnorm_report(&format!("prefix\n{json}\nsuffix")).unwrap(),
            json
        );
        let finite = parse_reference_true_peak_measurement(
            MeasurementId(1),
            tonepoet_pipeline::MeasurementScope::Plan,
            TruePeakPurpose::GainAuthority,
            json.clone(),
            tonepoet_pipeline::DbNano::ZERO,
            tonepoet_pipeline::DbNano::ZERO,
            false,
        )
        .unwrap();
        assert_eq!(
            finite.reported,
            tonepoet_pipeline::TruePeakValue::Finite("-3.125000000".parse().unwrap())
        );
        let silence = parse_reference_true_peak_measurement(
            MeasurementId(2),
            tonepoet_pipeline::MeasurementScope::Plan,
            TruePeakPurpose::GainAuthority,
            valid_loudnorm_json("-inf"),
            tonepoet_pipeline::DbNano::ZERO,
            tonepoet_pipeline::DbNano::ZERO,
            true,
        )
        .unwrap();
        assert_eq!(silence.reported, tonepoet_pipeline::TruePeakValue::VerifiedSilence);
    }

    #[test]
    fn loudnorm_parser_rejects_ambiguous_or_malformed_reports() {
        let json = valid_loudnorm_json("-3.00");
        assert!(extract_single_loudnorm_report("no report").is_err());
        assert!(extract_single_loudnorm_report(&format!("{json}\n{json}")).is_err());
        assert!(extract_single_loudnorm_report(&json[..json.len() - 1]).is_err());

        for invalid in ["1e2", "1E2", "+1.0", "1,0", "inf", "+inf", "NaN", "-1000.000000001", "100.000000001"] {
            assert!(
                parse_reference_true_peak_measurement(
                    MeasurementId(1),
                    tonepoet_pipeline::MeasurementScope::Plan,
                    TruePeakPurpose::GainAuthority,
                    valid_loudnorm_json(invalid),
                    tonepoet_pipeline::DbNano::ZERO,
                    tonepoet_pipeline::DbNano::ZERO,
                    false,
                )
                .is_err(),
                "unsupported input_tp syntax was accepted: {invalid}"
            );
        }

        let missing = json.replace("\n                \"target_offset\": \"0.00\"", "");
        assert!(parse_reference_true_peak_measurement(
            MeasurementId(1),
            tonepoet_pipeline::MeasurementScope::Plan,
            TruePeakPurpose::GainAuthority,
            missing,
            tonepoet_pipeline::DbNano::ZERO,
            tonepoet_pipeline::DbNano::ZERO,
            false,
        )
        .is_err());
        let unknown = json.replacen(
            "\n            }",
            ",\n                \"unexpected\": \"value\"\n            }",
            1,
        );
        assert!(parse_reference_true_peak_measurement(
            MeasurementId(1),
            tonepoet_pipeline::MeasurementScope::Plan,
            TruePeakPurpose::GainAuthority,
            unknown,
            tonepoet_pipeline::DbNano::ZERO,
            tonepoet_pipeline::DbNano::ZERO,
            false,
        )
        .is_err());
    }

    fn reference_wav_plan(
        depth: tonepoet_pipeline::PcmBitDepth,
        target: tonepoet_pipeline::ResolvedOutputTarget,
    ) -> tonepoet_pipeline::ConversionPlan {
        let mut settings = tonepoet_pipeline::PipelineSettings::default();
        settings.dsd = tonepoet_pipeline::DsdSettings::native_v2();
        settings.target_format = tonepoet_pipeline::AudioFormat::Wav;
        settings.target_sample_rate = tonepoet_pipeline::RateTarget::PcmHz(88_200);
        settings.target_bit_depth = tonepoet_pipeline::BitDepthTarget::Pcm(depth);
        let output = match target {
            tonepoet_pipeline::ResolvedOutputTarget::WavW64 => "output.w64",
            tonepoet_pipeline::ResolvedOutputTarget::WavRiff
            | tonepoet_pipeline::ResolvedOutputTarget::WavRf64 => "output.wav",
            _ => panic!("test helper accepts WAV-family targets only"),
        };
        tonepoet_pipeline::plan_reference_dsd(&tonepoet_pipeline::PlanRequest {
            input_path: PathBuf::from("source.dff"),
            output_path: PathBuf::from(output),
            source: tonepoet_pipeline::SourceInfo {
                format: tonepoet_pipeline::AudioFormat::Dff,
                codec: tonepoet_pipeline::AudioCodec::Dsd,
                sample_rate_hz: Some(2_822_400),
                bit_depth: None,
                true_source_depth: None,
                source_representation: tonepoet_pipeline::SourceRepresentationKind::Dsd,
                sample_kind: Some(tonepoet_pipeline::SampleKind::Dsd),
                channels: Some(2),
                duration: Some(Duration::from_secs(60)),
                dsd_source_kind: Some(tonepoet_pipeline::DsdSourceKind::DsdiffUncompressed),
                audio_md5: None,
            },
            settings,
            intermediate_dir: Some(PathBuf::from("work")),
            container_ffmpeg_flags: Vec::new(),
            resolved_output_target: Some(target),
            reference_programme_scope: tonepoet_pipeline::ReferenceProgrammeScope::Singleton,
            // RIFF-family targets require the planner size-bound preflight;
            // sibling fixtures pin the same zero upper bound.
            planned_riff_non_audio_upper_bound_bytes: Some(0),
        })
        .expect("Reference WAV-family plan")
    }

    fn reference_w64_plan(depth: tonepoet_pipeline::PcmBitDepth) -> tonepoet_pipeline::ConversionPlan {
        reference_wav_plan(depth, tonepoet_pipeline::ResolvedOutputTarget::WavW64)
    }

    #[test]
    fn reference_measurements_are_bound_to_their_summary_carrier_and_route() {
        for target in [
            tonepoet_pipeline::ResolvedOutputTarget::WavW64,
            tonepoet_pipeline::ResolvedOutputTarget::WavRiff,
            tonepoet_pipeline::ResolvedOutputTarget::WavRf64,
        ] {
            let f32 = reference_wav_plan(tonepoet_pipeline::PcmBitDepth::Float32, target);
            let f32_summary = f32.reference.as_ref().expect("Reference summary");
            let f32_measurements = f32
                .steps()
                .iter()
                .filter_map(|step| match step {
                    tonepoet_pipeline::PlannedExecutionStep::Measurement(value) => Some(value),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(f32_measurements.len(), 2);
            for measurement in &f32_measurements {
                validate_reference_measurement_binding(f32_summary, measurement)
                    .expect("planner measurement binding is canonical");
                validate_reference_measurement_contract(f32_summary, measurement)
                    .expect("planner measurement transport is canonical");
            }

            let mut crossed_path = (*f32_measurements
                .iter()
                .find(|measurement| {
                    measurement.purpose == TruePeakPurpose::PostFinalAcceptance
                })
                .expect("Float32 post measurement"))
            .clone();
            let wrong_path = f32_summary.r64_path.display().to_string();
            crossed_path.input_stage.as_mut().unwrap().input =
                InputSource::Path(f32_summary.r64_path.clone());
            crossed_path.input_stage.as_mut().unwrap().args[6] = wrong_path;
            assert!(
                validate_reference_measurement_binding(f32_summary, &crossed_path).is_err(),
                "crossed Float32 producer binding was accepted for {target:?}"
            );
            assert!(
                validate_reference_measurement_contract(f32_summary, &crossed_path).is_err(),
                "crossed Float32 producer contract was accepted for {target:?}"
            );
        }

        let f64 = reference_w64_plan(tonepoet_pipeline::PcmBitDepth::Float64);
        let f64_summary = f64.reference.as_ref().expect("Reference summary");
        let f64_post = f64
            .steps()
            .iter()
            .find_map(|step| match step {
                tonepoet_pipeline::PlannedExecutionStep::Measurement(value)
                    if value.purpose == TruePeakPurpose::PostFinalAcceptance =>
                {
                    Some(value)
                }
                _ => None,
            })
            .expect("Float64 post measurement");
        validate_reference_measurement_binding(f64_summary, f64_post)
            .expect("Float64 post measurement uses direct SoX route");
        assert!(matches!(
            validate_reference_measurement_contract(f64_summary, f64_post)
                .expect("Float64 direct SoX route is canonical"),
            ReferenceMeasurementContract::SoxPathOversampledStats
        ));
    }

    #[test]
    fn reference_measurement_contract_rejects_transport_or_argv_drift() {
        let f32 = reference_w64_plan(tonepoet_pipeline::PcmBitDepth::Float32);
        let f32_summary = f32.reference.as_ref().expect("Reference summary");
        let f32_post = f32
            .steps()
            .iter()
            .find_map(|step| match step {
                tonepoet_pipeline::PlannedExecutionStep::Measurement(value)
                    if value.purpose == TruePeakPurpose::PostFinalAcceptance =>
                {
                    Some(value)
                }
                _ => None,
            })
            .expect("Float32 post measurement");
        assert!(matches!(
            validate_reference_measurement_contract(f32_summary, f32_post)
                .expect("canonical v15 Float32 measurement contract is accepted"),
            ReferenceMeasurementContract::Float32FfmpegRawToSoxOversampledStats(_)
        ));

        let mut drifted = f32_post.clone();
        drifted.input_stage.as_mut().unwrap().args[13] = "pcm_f32le".to_string();
        assert!(validate_reference_measurement_contract(f32_summary, &drifted).is_err());

        let mut drifted = f32_post.clone();
        drifted.command.args[19] = "705600".to_string();
        assert!(validate_reference_measurement_contract(f32_summary, &drifted).is_err());

        let mut drifted = f32_post.clone();
        drifted.command.environment_policy =
            tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet;
        assert!(validate_reference_measurement_contract(f32_summary, &drifted).is_err());

        let f64 = reference_w64_plan(tonepoet_pipeline::PcmBitDepth::Float64);
        let f64_summary = f64.reference.as_ref().expect("Reference summary");
        let f64_pre = f64
            .steps()
            .iter()
            .find_map(|step| match step {
                tonepoet_pipeline::PlannedExecutionStep::Measurement(value)
                    if value.purpose == TruePeakPurpose::GainAuthority => Some(value),
                _ => None,
            })
            .expect("Float64 pre measurement");
        assert!(matches!(
            validate_reference_measurement_contract(f64_summary, f64_pre)
                .expect("canonical v15 direct SoX contract is accepted"),
            ReferenceMeasurementContract::SoxPathOversampledStats
        ));
        let mut drifted = f64_pre.clone();
        drifted.command.args[8] = "705600".to_string();
        assert!(validate_reference_measurement_contract(f64_summary, &drifted).is_err());
        let mut drifted = f64_pre.clone();
        drifted.input_stage = Some(PlannedCommand::new(
            ToolIdentifier::Ffmpeg,
            Vec::new(),
            InputSource::Path(f64_summary.r64_path.clone()),
            OutputSink::Stdout,
            None,
            "invalid producer",
        ));
        assert!(validate_reference_measurement_contract(f64_summary, &drifted).is_err());
    }

    #[test]
    fn v7_float64_package_pipeline_binding_rejects_route_and_environment_drift() {
        let plan = reference_wav_plan(
            tonepoet_pipeline::PcmBitDepth::Float64,
            tonepoet_pipeline::ResolvedOutputTarget::WavRf64,
        );
        let summary = plan.reference.as_ref().expect("Reference summary");
        let pipeline = plan
            .steps()
            .iter()
            .find_map(|step| match step {
                tonepoet_pipeline::PlannedExecutionStep::Pipeline(value) => Some(value),
                _ => None,
            })
            .expect("Float64 RF64 package pipeline");
        validate_reference_package_pipeline(summary, pipeline)
            .expect("planner-owned package pipeline is canonical");

        let mut direct_decode = pipeline.clone();
        direct_decode.consumer.input = InputSource::Path(summary.qpcm_path.clone());
        direct_decode.consumer.args = vec![
            "-i".to_string(),
            summary.qpcm_path.display().to_string(),
        ];
        assert!(validate_reference_package_pipeline(summary, &direct_decode).is_err());

        let mut inherited = pipeline.clone();
        inherited.producer.environment_policy =
            tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet;
        assert!(validate_reference_package_pipeline(summary, &inherited).is_err());

        let mut wrong_rate = pipeline.clone();
        let rate_index = wrong_rate
            .consumer
            .args
            .iter()
            .position(|arg| arg == "-ar")
            .expect("raw input rate option");
        wrong_rate.consumer.args[rate_index + 1] = "96000".to_string();
        assert!(validate_reference_package_pipeline(summary, &wrong_rate).is_err());
    }

    #[test]
    fn reference_evidence_subprocesses_clear_and_set_exact_environment() {
        let expected = BTreeMap::from([("LC_ALL".to_string(), "C".to_string())]);
        let path = PathBuf::from("carrier.w64");
        let probe = build_reference_carrier_probe_command(&path, "carrier", "-r", "sample rate");
        assert_eq!(
            probe.environment_policy,
            tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
        );
        assert_eq!(probe.environment, expected);

        let direct_plan = reference_wav_plan(
            tonepoet_pipeline::PcmBitDepth::Float64,
            tonepoet_pipeline::ResolvedOutputTarget::WavRiff,
        );
        let direct_summary = direct_plan.reference.as_ref().expect("Reference summary");
        let direct_carrier = direct_summary
            .decoded_carrier(ReferenceDecodedCarrierSelector::PackagedOutput)
            .expect("Float64 RIFF packaged carrier");
        let direct = build_reference_direct_hash_command(&direct_carrier, "direct hash")
            .expect("direct hash command");
        assert_eq!(
            direct.environment_policy,
            tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
        );
        assert_eq!(direct.environment, expected);

        let stream_plan = reference_w64_plan(tonepoet_pipeline::PcmBitDepth::Float64);
        let stream_summary = stream_plan.reference.as_ref().expect("Reference summary");
        let stream_carrier = stream_summary
            .decoded_carrier(ReferenceDecodedCarrierSelector::TerminalQpcm)
            .expect("Float64 W64 QPCM carrier");
        let pipeline = build_reference_float64_w64_hash_pipeline(
            &stream_carrier,
            "stream hash",
        )
        .expect("Float64 W64 hash pipeline");
        for command in [&pipeline.producer, &pipeline.consumer] {
            assert_eq!(
                command.environment_policy,
                tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet
            );
            assert_eq!(command.environment, expected);
        }

        let rejected = build_reference_direct_hash_command(
            &stream_carrier,
            "forbidden direct Float64 W64 hash",
        )
        .expect_err("typed builder must reject direct FFmpeg for Float64 W64");
        assert!(rejected.contains("required route is sox_f64le_raw_stream"));

        let mislabeled = direct_summary
            .bind_decoded_carrier(
                ReferenceDecodedCarrierSelector::PackagedOutput,
                &direct_summary.qpcm_path,
            )
            .expect_err("QPCM path cannot impersonate the RIFF packaged carrier");
        assert!(mislabeled.to_string().contains("carrier path mismatch"));
    }

    #[test]
    fn every_checked_in_reference_qualification_manifest_parses_through_current_wire_dispatch() {
        let qualification_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tonepoet-pipeline/qualification");
        let mut paths = std::fs::read_dir(&qualification_dir)
            .expect("read qualification directory")
            .map(|entry| entry.expect("read qualification directory entry").path())
            .filter(|path| {
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    return false;
                };
                name.starts_with("dsd_reference_sox_ng_14_8_0_1_v")
                    && name.ends_with(".json")
                    && !name.contains("_certification")
            })
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(
            paths.len(),
            31,
            "qualification manifest inventory changed; update the permanent parse census intentionally"
        );

        for path in paths {
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let parsed = parse_embedded_reference_qualification_wire(&raw)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            match parsed {
                EmbeddedReferenceQualificationWire::Historical(manifest) => {
                    assert!(manifest.schema_version <= 15);
                    assert!(manifest.policy.starts_with("sox_ng_14_8_0_1_v"));
                    assert!(!manifest.status.trim().is_empty());
                }
                EmbeddedReferenceQualificationWire::Current(manifest) => {
                    assert_eq!(manifest.schema_version, 16);
                    assert_eq!(
                        manifest.policy,
                        tonepoet_pipeline::DSD_REFERENCE_POLICY_V16_KEY,
                    );
                }
            }
        }
    }

    #[test]
    fn historical_v12_streamed_wav_capacity_schema_remains_linked() {
        let raw = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v12_candidate.json"
        ));
        let historical = match parse_embedded_reference_qualification_wire(raw)
            .expect("historical v12 candidate JSON parses")
        {
            EmbeddedReferenceQualificationWire::Historical(manifest) => manifest,
            EmbeddedReferenceQualificationWire::Current(_) => {
                panic!("historical v12 candidate dispatched as current schema")
            }
        };
        assert_eq!(historical.schema_version, 12);
        assert_eq!(
            historical.policy,
            tonepoet_pipeline::DSD_REFERENCE_POLICY_V12_KEY,
        );
        // The v12 generation serialized a DIFFERENT wire shape than the
        // current evidence struct (documentary schema/applies_to/... fields;
        // no edge observations). Per the F11.1 lineage rule, historical
        // artifacts parse through their own generation's wire struct.
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct HistoricalV12StreamedWavCapacityWire {
            schema: String,
            applies_to: String,
            riff_size_field_max: u64,
            riff_size_overhead_bytes: u64,
            max_audio_payload_bytes: u64,
            sample_encoding: String,
            bytes_per_sample: u64,
            duration_guard_frames: u64,
            admission_rule: String,
            overflow_behavior: String,
            overflow_error_code: String,
            future_lift: String,
        }
        let streamed_wav_capacity: HistoricalV12StreamedWavCapacityWire =
            serde_json::from_value(
                historical
                    .streamed_wav_capacity
                    .expect("historical v12 streamed-WAV capacity evidence"),
            )
            .expect("historical v12 streamed-WAV capacity schema parses");
        assert!(!streamed_wav_capacity.schema.is_empty());
        assert!(!streamed_wav_capacity.applies_to.is_empty());
        assert!(!streamed_wav_capacity.admission_rule.is_empty());
        assert!(!streamed_wav_capacity.overflow_behavior.is_empty());
        assert!(!streamed_wav_capacity.overflow_error_code.is_empty());
        assert!(!streamed_wav_capacity.future_lift.is_empty());
        assert_eq!(streamed_wav_capacity.sample_encoding, "pcm_f64le");
        assert_eq!(streamed_wav_capacity.bytes_per_sample, 8);
        assert!(streamed_wav_capacity.riff_size_field_max >= streamed_wav_capacity.max_audio_payload_bytes);
        assert!(streamed_wav_capacity.duration_guard_frames > 0);
        assert_eq!(streamed_wav_capacity.riff_size_overhead_bytes, 58);
        assert_eq!(streamed_wav_capacity.max_audio_payload_bytes, 4_294_967_237);

        let validator: fn(
            &tonepoet_pipeline::ReferenceStreamedWavCapacityEvidenceV2,
        ) -> bool = tonepoet_pipeline::ReferenceStreamedWavCapacityEvidenceV2::is_canonical_v12;
        let _ = validator;
    }

    #[test]
    fn embedded_reference_qualification_matches_compiled_policy_tables() {
        let manifest: EmbeddedReferenceQualification = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16.json"
        )))
        .expect("embedded Reference qualification JSON parses");
        assert_eq!(
            manifest
                .terminal_bounds
                .int16_shibata
                .safe_pre_terminal_ceiling_dbtp,
            tonepoet_pipeline::DbNano(i64::MIN)
        );
        validate_embedded_reference_policy_tables(&manifest)
            .expect("embedded Reference qualification matches compiled policy tables");

        let mut reserve_drift: EmbeddedReferenceQualification =
            serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16.json"
            )))
            .expect("embedded Reference qualification JSON parses for drift test");
        reserve_drift.analyzer.reporting_uncertainty_db =
            tonepoet_pipeline::DbNano(11_000_000);
        let error = validate_embedded_reference_policy_tables(&reserve_drift)
            .expect_err("terminal reserve and analyzer uncertainty must be directly bound");
        assert!(
            error
                .to_string()
                .contains("terminal reserve must equal the analyzer reporting uncertainty"),
            "unexpected invariant failure: {error}"
        );

        let mut streamed_capacity_drift: EmbeddedReferenceQualification =
            serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16.json"
            )))
            .expect("embedded Reference qualification JSON parses for capacity drift test");
        streamed_capacity_drift.streamed_wav_capacity.max_audio_payload_bytes += 1;
        let error = validate_embedded_reference_policy_tables(&streamed_capacity_drift)
            .expect_err("the streamed-WAV capacity must be directly bound");
        assert!(
            error
                .to_string()
                .contains("streamed-WAV capacity contract disagrees"),
            "unexpected streamed-capacity invariant failure: {error}"
        );

        let mut hash_contract_drift: EmbeddedReferenceQualification =
            serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16.json"
            )))
            .expect("embedded Reference qualification JSON parses for hash-contract drift test");
        hash_contract_drift.sample_identity.hash_format =
            "interleaved_f64le_sha256".to_string();
        let error = validate_embedded_reference_policy_tables(&hash_contract_drift)
            .expect_err("the immutable sample-hash byte contract must be truthful");
        assert!(
            error
                .to_string()
                .contains("decoded-sample identity contract disagrees"),
            "unexpected hash-contract invariant failure: {error}"
        );

        let mut route_contract_drift: EmbeddedReferenceQualification =
            serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16.json"
            )))
            .expect("embedded Reference qualification JSON parses for route-contract drift test");
        route_contract_drift
            .sample_identity
            .routes
            .qpcm_float64_w64 = ReferenceDecodeMechanism::DirectFfmpeg.key().to_string();
        let error = validate_embedded_reference_policy_tables(&route_contract_drift)
            .expect_err("the immutable Float64-W64 route must reject direct FFmpeg");
        assert!(
            error
                .to_string()
                .contains("decoded-sample identity contract disagrees"),
            "unexpected route-contract invariant failure: {error}"
        );

        let candidate: EmbeddedReferenceQualification = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tonepoet-pipeline/qualification/dsd_reference_sox_ng_14_8_0_1_v16_candidate.json"
        )))
        .expect("preserved v16 candidate JSON parses");
        assert_eq!(
            candidate
                .terminal_bounds
                .int16_shibata
                .safe_pre_terminal_ceiling_dbtp,
            tonepoet_pipeline::DbNano(i64::MIN)
        );
        assert_eq!(candidate.status, "qualification_candidate");
        assert!(validate_embedded_release_certification(&candidate).is_err());
    }

    #[test]
    fn v16_carrier_digest_is_append_only_and_binds_exact_w64_structure() {
        let legacy = ReferenceCarrierProbe {
            sample_rate_hz: 88_200,
            channels: 2,
            bits_per_sample: 64,
            samples_per_channel: 257,
            floating_point: true,
            w64_structure: None,
        };
        let exact = ReferenceCarrierProbe {
            w64_structure: Some(W64ExactStructure {
                physical_file_bytes: 4_256,
                declared_file_bytes: 4_256,
                chunk_count: 3,
                format_chunk_offset: 40,
                fact_chunk_offset: Some(80),
                data_chunk_offset: 112,
                declared_data_bytes: 4_112,
                sample_frames: 257,
                alignment_padding_bytes: 0,
            }),
            ..legacy
        };
        assert_eq!(
            reference_carrier_probe_digest(
                tonepoet_pipeline::DsdReferencePolicyVersion::SoxNg14801V15,
                "r64",
                legacy,
            ),
            reference_carrier_probe_digest(
                tonepoet_pipeline::DsdReferencePolicyVersion::SoxNg14801V15,
                "r64",
                exact,
            ),
            "historical v1 carrier identity changed",
        );
        assert_ne!(
            reference_carrier_probe_digest(
                tonepoet_pipeline::DsdReferencePolicyVersion::SoxNg14801V16,
                "r64",
                legacy,
            ),
            reference_carrier_probe_digest(
                tonepoet_pipeline::DsdReferencePolicyVersion::SoxNg14801V16,
                "r64",
                exact,
            ),
            "v16 carrier identity omitted exact Wave64 structure",
        );
    }

    fn test_tool_command(binary: ToolBinary) -> ToolCommand {
        ToolCommand {
            environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
            binary,
            args: vec!["--test".to_string()],
            secret_args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            timeout: Duration::from_secs(30),
        }
    }

    fn env_value<'a>(cmd: &'a ToolCommand, key: &str) -> Option<&'a str> {
        cmd.env
            .iter()
            .find(|var| var.key == key)
            .map(|var| var.value.expose())
    }

    fn env_value_for_key(var: &EnvVar, key: &str) -> bool {
        var.key == key
    }

    fn planned_command_for_test(binary: ToolIdentifier, args: Vec<String>, out: &Path) -> PlannedCommand {
        PlannedCommand::new(
            binary,
            args,
            InputSource::Path(PathBuf::from("input.wav")),
            OutputSink::Path(out.to_path_buf()),
            Some(Duration::from_secs(1)),
            "test command",
        )
    }

    fn metadata_test_request(root: &Path) -> PipelineRequest {
        PipelineRequest {
            job_id: "metadata-job".to_string(),
            actions: crate::convert::pipeline::ActionPipeline::default(),
            item_id: "metadata-item".to_string(),
            container: root.join("album.iso"),
            source: SourceOptions {
                archive_password: None,
                sacd_area: Some(SacdArea::Stereo),
                dvda_group: None,
                dvda_group_selection: DvdaGroupSelection::Default,
                dvda_assume_decrypted: false,
                dvda_downmix_policy: DvdaDownmixPolicy::Auto,
                dvdv_vts: None,
                dvdv_title: None,
                dvdv_audio_stream: None,
                dvdv_angle: None,
                bluray_playlist: None,
                bluray_audio_pid: None,
                bluray_audio_stream: None,
                bluray_angle: None,
                sidecar_cue_track_metadata: None,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            settings: PipelineSettings::default(),
            worker_count: Some(1),
            scratch_staging: None,
            merge: false,
            output_root: root.join("out"),
            naming: NamingPolicy {
                windows_portable: false,
                template: "%NN% - %TITLE%".to_string(),
                folder_template: None,
                per_album_subdir: true,
                collision_policy: NamingCollisionPolicy::Fail,
            },
            publish: PublishPolicy {
                overwrite: OverwritePolicy::FailIfExists,
                same_filesystem_required: false,
                write_manifest: false,
            },
            log: LogPolicy {
                root: root.join("logs"),
                write_for_blocked: false,
                write_json_log: false,
                write_conversion_log: true,
            },
            stages: StagePolicy {
                metadata: StageRequirement::Enabled,
                replaygain: StageRequirement::Disabled,
                features: StageRequirement::Disabled,
                generate_cue: false,
            },
            failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
            album_batch: None,
            album_batch_track: None,
            suppress_incremental_conversion_log_append: false,
            companion: Default::default(),
            pre_extracted_staging: None,
            archive_metadata_overrides: Vec::new(),
            expected_album_track_count: None,
            container_extension: None,
            container_ffmpeg_flags: Vec::new(),
            batch_resolved_identity: None,
            metadata_overrides: Default::default(),
        }
    }

    fn metadata_test_track(source_ref: TrackSourceRef) -> PreparedTrack {
        PreparedTrack {
            id: TrackId {
                source_ordinal: 1,
                disc_number: None,
                track_number: 1,
            },
            source_ref,
            metadata: TrackMetadata {
                title: Some("Track One".to_string()),
                track_number: Some(1),
                ..TrackMetadata::default()
            },
            expected_samples: Some(1_000),
            sample_rate: Some(2_822_400),
            bit_depth: None,
            source_audio: SourceAudioDescriptor::default(),
            warnings: Vec::new(),
        }
    }

    async fn execute_commands_for_test(
        commands: Vec<PlannedCommand>,
        runner: &dyn ToolRunner,
        cancel: &CancellationToken,
        limits: Option<Arc<ToolConcurrencyLimits>>,
    ) -> Result<Vec<CommandRecord>, TrackExecutionError> {
        let mut progress = OperationProgressTracker::new(
            "track-executor-test".to_string(),
            PipelineStage::Convert,
            None,
        );
        execute_commands(
            &commands,
            runner,
            cancel,
            &HashMap::new(),
            limits,
            &mut progress,
            0.0,
            1.0,
            "test track".to_string(),
        )
        .await
    }

    fn command_record_for_test_tool_command(
        cmd: &ToolCommand,
        exit: Option<ProcessExit>,
        stderr: &str,
        elapsed: Duration,
    ) -> CommandRecord {
        CommandRecord {
            environment_policy: cmd.environment_policy,
            environment: cmd.sanitized_environment(),
            description: None,
            binary: cmd.binary,
            sanitized_args: cmd.sanitized_args(),
            cwd: cmd.cwd.clone(),
            env_keys: cmd.env_keys(),
            exit,
            stdout_tail: String::new(),
            stderr_tail: stderr.to_string(),
            elapsed,
        }
    }

    struct TimeoutToolRunnerForTest;

    #[async_trait]
    impl ToolRunner for TimeoutToolRunnerForTest {
        async fn run(
            &self,
            cmd: ToolCommand,
            _cancel: &CancellationToken,
        ) -> Result<ToolOutput, ToolRunnerError> {
            let elapsed = Duration::from_secs(42);
            Err(ToolRunnerError::Timeout {
                elapsed,
                command: command_record_for_test_tool_command(
                    &cmd,
                    None,
                    "timed out in test",
                    elapsed,
                ),
            })
        }
    }

    struct CancelledToolRunnerForTest;

    #[async_trait]
    impl ToolRunner for CancelledToolRunnerForTest {
        async fn run(
            &self,
            cmd: ToolCommand,
            _cancel: &CancellationToken,
        ) -> Result<ToolOutput, ToolRunnerError> {
            Err(ToolRunnerError::Cancelled {
                command: command_record_for_test_tool_command(
                    &cmd,
                    None,
                    "cancelled in test",
                    Duration::from_secs(7),
                ),
            })
        }
    }


    #[tokio::test]
    async fn failed_planned_command_preserves_partial_records_and_planned_description() {
        let temp = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();
        let runner = StubToolRunner::new();
        runner.push_output(ToolOutput {
            exit: ProcessExit::Code(0),
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            elapsed: Duration::ZERO,
            command: CommandRecord {
                environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
                environment: std::collections::BTreeMap::new(),

                description: None,
                binary: ToolBinary::Ssrc,
                sanitized_args: Vec::new(),
                cwd: None,
                env_keys: Vec::new(),
                exit: Some(ProcessExit::Code(0)),
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                elapsed: Duration::ZERO,
            },
        });
        runner.push_failure("encoder exploded");

        let mut decode = planned_command_for_test(
            ToolIdentifier::Ssrc,
            vec!["decode".to_string()],
            &temp.path().join("decoded.wav"),
        );
        decode.description = "Decode FLAC to PCM".to_string();
        let mut encode = planned_command_for_test(
            ToolIdentifier::Ssrc,
            vec!["encode".to_string()],
            &temp.path().join("encoded.flac"),
        );
        encode.description = "Encode FLAC".to_string();

        let err = execute_commands_for_test(vec![decode, encode], &runner, &cancel, None)
            .await
            .expect_err("second planned command fails");

        assert!(matches!(
            &err.error,
            ConvertError::Tool(ToolRunnerError::NonZeroExit { .. })
        ));
        assert_eq!(err.commands.len(), 2);
        assert_eq!(
            err.commands[0].description.as_deref(),
            Some("Decode FLAC to PCM")
        );
        assert_eq!(err.commands[1].description.as_deref(), Some("Encode FLAC"));
        assert_eq!(err.commands[1].stderr_tail, "encoder exploded");
        assert!(
            err.to_string().contains("planned command 2 failed (Encode FLAC)"),
            "display error keeps the planned step context"
        );
    }


    #[tokio::test]
    async fn non_zero_planned_command_failure_retains_command_record_description() {
        let temp = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();
        let runner = StubToolRunner::new();
        runner.push_failure("encoder returned non-zero");

        let mut encode = planned_command_for_test(
            ToolIdentifier::Ssrc,
            vec!["encode".to_string(), "out.flac".to_string()],
            &temp.path().join("encoded.flac"),
        );
        encode.description = "Encode FLAC level 8".to_string();

        let err = execute_commands_for_test(vec![encode], &runner, &cancel, None)
            .await
            .expect_err("planned command should fail");

        assert!(matches!(
            &err.error,
            ConvertError::Tool(ToolRunnerError::NonZeroExit { .. })
        ));
        assert_eq!(err.commands.len(), 1);
        assert_eq!(
            err.commands[0].description.as_deref(),
            Some("Encode FLAC level 8")
        );
        assert_eq!(err.commands[0].stderr_tail, "encoder returned non-zero");
        assert_eq!(err.commands[0].exit, Some(ProcessExit::Code(1)));
    }

    #[tokio::test]
    async fn timeout_planned_command_preserves_failing_command_record() {
        let temp = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();
        let runner = TimeoutToolRunnerForTest;
        let mut encode = planned_command_for_test(
            ToolIdentifier::Ssrc,
            vec!["encode".to_string(), "slow.flac".to_string()],
            &temp.path().join("slow.flac"),
        );
        encode.description = "Encode slow FLAC".to_string();

        let err = execute_commands_for_test(vec![encode], &runner, &cancel, None)
            .await
            .expect_err("planned command should time out");

        assert!(matches!(
            &err.error,
            ConvertError::Tool(ToolRunnerError::Timeout { .. })
        ));
        assert_eq!(err.commands.len(), 1);
        assert_eq!(
            err.commands[0].description.as_deref(),
            Some("Encode slow FLAC")
        );
        assert!(err.commands[0].sanitized_args.iter().any(|arg| arg == "encode"));
        assert_eq!(err.commands[0].exit, None);
    }

    #[tokio::test]
    async fn cancelled_planned_command_preserves_failing_command_record() {
        let temp = TempDir::new().expect("temp dir");
        let cancel = CancellationToken::new();
        let runner = CancelledToolRunnerForTest;
        let mut encode = planned_command_for_test(
            ToolIdentifier::Ssrc,
            vec!["encode".to_string(), "cancel.flac".to_string()],
            &temp.path().join("cancel.flac"),
        );
        encode.description = "Encode cancellable FLAC".to_string();

        let err = execute_commands_for_test(vec![encode], &runner, &cancel, None)
            .await
            .expect_err("planned command should be cancelled");

        assert!(matches!(
            &err.error,
            ConvertError::Realize(message) if message == "cancelled"
        ));
        assert_eq!(err.commands.len(), 1);
        assert_eq!(
            err.commands[0].description.as_deref(),
            Some("Encode cancellable FLAC")
        );
        assert!(err.commands[0].sanitized_args.iter().any(|arg| arg == "encode"));
        assert_eq!(err.commands[0].exit, None);
    }


    #[test]
    fn metadata_satisfaction_uses_typed_effects_not_argv_spelling() {
        let temp = TempDir::new().expect("temp dir");
        let mut request = metadata_test_request(temp.path());
        request.settings.target_format = AudioFormat::Flac;
        // Unknown Source depth now fails closed; these tests pin metadata effects.
        request.settings.target_bit_depth =
            tonepoet_pipeline::BitDepthTarget::Pcm(tonepoet_pipeline::PcmBitDepth::Int16);
        request.settings.metadata.transfer_tags = true;
        request.settings.metadata.preserve_artwork = true;
        let track = metadata_test_track(TrackSourceRef::StagedFile(temp.path().join("source.flac")));
        let plan_request = plan_request_for_track(
            &request,
            &track,
            &temp.path().join("source.flac"),
            &temp.path().join("out.flac"),
            temp.path().join("work"),
        )
        .expect("per-track plan request builds");
        let mut command = PlannedCommand::new(
            ToolIdentifier::Ffmpeg,
            vec!["arguments".into(), "intentionally".into(), "irrelevant".into()],
            InputSource::Path(temp.path().join("encoded.flac")),
            OutputSink::Path(temp.path().join("out.flac")),
            None,
            "synthetic metadata transfer",
        );
        command.metadata_effect.source_tags_transferred_from_original_source = true;
        command.metadata_effect.artwork_transferred_from_original_source = true;
        let plan = ConversionPlan::execute_with_cleanup(vec![command], Vec::new(), None);

        let satisfaction = effective_metadata_satisfaction(&plan_request, &plan);

        assert!(satisfaction.source_tags_transferred);
        assert!(satisfaction.artwork_transferred);
        assert!(!satisfaction.source_audio_md5_written);
        assert!(!satisfaction.authoritative_tags_applied);
    }

    #[test]
    fn current_input_preservation_does_not_satisfy_original_source_metadata_obligations() {
        let temp = TempDir::new().expect("temp dir");
        let mut request = metadata_test_request(temp.path());
        request.settings.target_format = AudioFormat::Flac;
        // Unknown Source depth now fails closed; these tests pin metadata effects.
        request.settings.target_bit_depth =
            tonepoet_pipeline::BitDepthTarget::Pcm(tonepoet_pipeline::PcmBitDepth::Int16);
        request.settings.metadata.transfer_tags = true;
        request.settings.metadata.preserve_artwork = true;
        let track = metadata_test_track(TrackSourceRef::StagedFile(temp.path().join("source.flac")));
        let plan_request = plan_request_for_track(
            &request,
            &track,
            &temp.path().join("source.flac"),
            &temp.path().join("out.flac"),
            temp.path().join("work"),
        )
        .expect("per-track plan request builds");
        let mut command = PlannedCommand::new(
            ToolIdentifier::Ffmpeg,
            vec!["arguments".into(), "remain".into(), "irrelevant".into()],
            InputSource::Path(temp.path().join("intermediate.wav")),
            OutputSink::Path(temp.path().join("out.flac")),
            None,
            "synthetic current-input metadata preservation",
        );
        command.metadata_effect.tags_preserved_from_command_input = true;
        command.metadata_effect.artwork_preserved_from_command_input = true;
        let plan = ConversionPlan::execute_with_cleanup(vec![command], Vec::new(), None);

        let satisfaction = effective_metadata_satisfaction(&plan_request, &plan);

        assert!(!satisfaction.source_tags_transferred);
        assert!(!satisfaction.artwork_transferred);
        assert!(!satisfaction.source_audio_md5_written);
        assert!(!satisfaction.authoritative_tags_applied);
    }

    #[test]
    fn planner_owned_metadata_effects_do_not_apply_authoritative_tags() {
        let temp = TempDir::new().expect("temp dir");
        let mut request = metadata_test_request(temp.path());
        request.settings.target_format = AudioFormat::Flac;
        // Unknown Source depth now fails closed; these tests pin metadata effects.
        request.settings.target_bit_depth =
            tonepoet_pipeline::BitDepthTarget::Pcm(tonepoet_pipeline::PcmBitDepth::Int16);
        request.settings.metadata.transfer_tags = true;
        request.settings.metadata.preserve_artwork = true;
        request.settings.metadata.store_source_audio_md5 = true;
        let track = metadata_test_track(TrackSourceRef::StagedFile(temp.path().join("source.flac")));
        let plan_request = plan_request_for_track(
            &request,
            &track,
            &temp.path().join("source.flac"),
            &temp.path().join("out.flac"),
            temp.path().join("work"),
        )
        .expect("per-track plan request builds");
        let mut command = PlannedCommand::new(
            ToolIdentifier::Ffmpeg,
            vec!["arguments".into(), "remain".into(), "irrelevant".into()],
            InputSource::Path(temp.path().join("encoded.flac")),
            OutputSink::Path(temp.path().join("out.flac")),
            None,
            "synthetic planner-owned metadata effects",
        );
        command.metadata_effect.source_tags_transferred_from_original_source = true;
        command.metadata_effect.artwork_transferred_from_original_source = true;
        command.metadata_effect.source_audio_md5_written = true;
        let plan = ConversionPlan::execute_with_cleanup(vec![command], Vec::new(), None);

        let satisfaction = effective_metadata_satisfaction(&plan_request, &plan);

        assert!(satisfaction.source_tags_transferred);
        assert!(satisfaction.artwork_transferred);
        assert!(satisfaction.source_audio_md5_written);
        assert!(
            !satisfaction.authoritative_tags_applied,
            "planner-owned source tag, artwork, and MD5 effects must not stand in for orchestrator-owned authoritative tags"
        );
    }

    #[test]
    fn sacd_track_plan_reports_no_planner_metadata_satisfaction_for_source_tag_policy() {
        let temp = TempDir::new().expect("temp dir");
        let realized_input = temp.path().join("realized.dsf");
        std::fs::write(&realized_input, b"DSD ").expect("realized dsf placeholder");
        let staged_output = temp.path().join("out.flac");
        let mut request = metadata_test_request(temp.path());
        request.settings.target_format = AudioFormat::Flac;
        request.settings.target_bit_depth =
            tonepoet_pipeline::BitDepthTarget::Pcm(tonepoet_pipeline::PcmBitDepth::Int24);
        request.settings.metadata.transfer_tags = true;
        request.settings.metadata.preserve_artwork = true;
        let track = metadata_test_track(TrackSourceRef::SacdTrack {
            iso: temp.path().join("album.iso"),
            track_index: 1,
            area: SacdArea::Stereo,
        });
        let plan_request = plan_request_for_track(
            &request,
            &track,
            &realized_input,
            &staged_output,
            temp.path().join("work"),
        )
        .expect("SACD per-track plan request builds");

        let plan = plan_conversion(&plan_request).expect("SACD per-track plan builds");
        let satisfaction = effective_metadata_satisfaction(&plan_request, &plan);

        assert_eq!(
            satisfaction,
            PlannedMetadataSatisfaction::none(),
            "suppressed original-source tag/artwork transfer must not report planner metadata satisfaction"
        );
    }

    #[test]
    fn tool_concurrency_limits_from_total_cores_match_expected_defaults() {
        let limits = ToolConcurrencyLimits::from_total_cores(16);
        assert_eq!(limits.sox_max_concurrent(), 2);
        assert_eq!(limits.ffmpeg_max_concurrent(), 8);
        assert_eq!(limits.ssrc_max_concurrent(), 8);
        assert_eq!(limits.sox_omp_threads(), 8);
        assert_eq!(limits.max_tool_concurrency(), 8);

        let tiny = ToolConcurrencyLimits::from_total_cores(1);
        assert_eq!(tiny.sox_max_concurrent(), 1);
        assert_eq!(tiny.ffmpeg_max_concurrent(), 1);
        assert_eq!(tiny.ssrc_max_concurrent(), 1);
        assert_eq!(tiny.sox_omp_threads(), 1);
    }

    #[tokio::test]
    async fn sox_permit_caps_concurrent_sox_and_sets_omp_threads() {
        let limits = Arc::new(ToolConcurrencyLimits::new(1, 4, 4, 6));
        let cancel = CancellationToken::new();
        let mut first = test_tool_command(ToolBinary::Sox);
        first.env.push(EnvVar {
            key: "OMP_NUM_THREADS".to_string(),
            value: crate::convert::pipeline::types::SecretString::new("99".to_string()),
            secret: false,
        });
        let first_permit = acquire_tool_permit(&mut first, Some(&limits), &cancel)
            .await
            .expect("first SoX permit acquired")
            .expect("SoX has a permit");
        assert_eq!(env_value(&first, "OMP_NUM_THREADS"), Some("6"));
        assert_eq!(
            first
                .env
                .iter()
                .filter(|var| env_value_for_key(var, "OMP_NUM_THREADS"))
                .count(),
            1
        );

        let mut second = test_tool_command(ToolBinary::Sox);
        let blocked = tokio::time::timeout(
            Duration::from_millis(25),
            acquire_tool_permit(&mut second, Some(&limits), &cancel),
        )
        .await;
        assert!(blocked.is_err(), "second SoX waits while first permit is held");

        drop(first_permit);
        let second_permit = tokio::time::timeout(
            Duration::from_secs(1),
            acquire_tool_permit(&mut second, Some(&limits), &cancel),
        )
        .await
        .expect("second SoX permit available after release")
        .expect("second SoX acquire succeeds")
        .expect("SoX has a permit");
        assert_eq!(env_value(&second, "OMP_NUM_THREADS"), Some("6"));
        drop(second_permit);
    }

    #[tokio::test]
    async fn reference_pipeline_permit_preserves_frozen_sox_environment() {
        let limits = Arc::new(ToolConcurrencyLimits::new(1, 1, 1, 6));
        let cancel = CancellationToken::new();
        let mut producer = test_tool_command(ToolBinary::Sox);
        producer.environment_policy =
            tonepoet_pipeline::CommandEnvironmentPolicy::ClearAndSet;
        producer.env.push(EnvVar {
            key: "LC_ALL".to_string(),
            value: crate::convert::pipeline::types::SecretString::new("C".to_string()),
            secret: false,
        });
        let permit = acquire_reference_pipeline_permits(
            [producer.binary],
            Some(&limits),
            &cancel,
        )
        .await
        .expect("reference SoX permit acquired")
        .expect("reference SoX has a permit set");

        assert_eq!(producer.env.len(), 1);
        assert_eq!(env_value(&producer, "LC_ALL"), Some("C"));
        assert_eq!(
            env_value(&producer, "OMP_NUM_THREADS"),
            None,
            "reference execution must not inject OMP_NUM_THREADS into the qualified environment"
        );
        assert_eq!(limits.sox.available_permits(), 0);
        drop(permit);
        assert_eq!(limits.sox.available_permits(), 1);
    }

    #[tokio::test]
    async fn reference_pipeline_composite_permits_prevent_opposite_direction_deadlock() {
        let limits = Arc::new(ToolConcurrencyLimits::new(1, 1, 1, 4));

        // First force the exact circular-wait ownership that the former
        // producer-first acquisition protocol permitted. Barriers, not sleeps,
        // prove both first permits are held before either task requests its second.
        let legacy_cancel = CancellationToken::new();
        let legacy_interleaving = Arc::new(tokio::sync::Barrier::new(3));
        let legacy_float32_limits = limits.clone();
        let legacy_float32_cancel = legacy_cancel.clone();
        let legacy_float32_barrier = legacy_interleaving.clone();
        let legacy_float32 = tokio::spawn(async move {
            let _ffmpeg = acquire_owned_permit(
                legacy_float32_limits.ffmpeg.clone(),
                &legacy_float32_cancel,
            )
            .await
            .expect("legacy Float32 route acquires FFmpeg first");
            legacy_float32_barrier.wait().await;
            acquire_owned_permit(
                legacy_float32_limits.sox.clone(),
                &legacy_float32_cancel,
            )
            .await
        });
        let legacy_float64_limits = limits.clone();
        let legacy_float64_cancel = legacy_cancel.clone();
        let legacy_float64_barrier = legacy_interleaving.clone();
        let legacy_float64 = tokio::spawn(async move {
            let _sox = acquire_owned_permit(
                legacy_float64_limits.sox.clone(),
                &legacy_float64_cancel,
            )
            .await
            .expect("legacy Float64 route acquires SoX first");
            legacy_float64_barrier.wait().await;
            acquire_owned_permit(
                legacy_float64_limits.ffmpeg.clone(),
                &legacy_float64_cancel,
            )
            .await
        });

        legacy_interleaving.wait().await;
        assert_eq!(limits.sox.available_permits(), 0);
        assert_eq!(limits.ffmpeg.available_permits(), 0);
        legacy_cancel.cancel();
        for result in [legacy_float32.await, legacy_float64.await] {
            let error = result
                .expect("legacy interleaving task joins")
                .expect_err("forced circular wait exits only by cancellation");
            assert!(matches!(error, ConvertError::Realize(message) if message == "cancelled"));
        }
        assert_eq!(limits.sox.available_permits(), 1);
        assert_eq!(limits.ffmpeg.available_permits(), 1);

        // The same opposite data-flow declarations now normalize to the frozen
        // SoX-before-FFmpeg family order. One pipeline may wait behind the other,
        // but neither can own FFmpeg while waiting for SoX.
        let start = Arc::new(tokio::sync::Barrier::new(3));
        let (acquired_tx, mut acquired_rx) = tokio::sync::mpsc::unbounded_channel();
        let (release_float32_tx, release_float32_rx) = tokio::sync::oneshot::channel();
        let (release_float64_tx, release_float64_rx) = tokio::sync::oneshot::channel();
        let mut release_float32_tx = Some(release_float32_tx);
        let mut release_float64_tx = Some(release_float64_tx);

        let float32_limits = limits.clone();
        let float32_start = start.clone();
        let float32_tx = acquired_tx.clone();
        let float32 = tokio::spawn(async move {
            float32_start.wait().await;
            let cancel = CancellationToken::new();
            let permits = acquire_reference_pipeline_permits(
                [ToolBinary::Ffmpeg, ToolBinary::Sox],
                Some(&float32_limits),
                &cancel,
            )
            .await
            .expect("Float32 composite acquire succeeds")
            .expect("Float32 composite acquire returns permits");
            float32_tx.send("float32").expect("record Float32 acquire");
            release_float32_rx.await.expect("release Float32 permits");
            drop(permits);
        });

        let float64_limits = limits.clone();
        let float64_start = start.clone();
        let float64_tx = acquired_tx.clone();
        let float64 = tokio::spawn(async move {
            float64_start.wait().await;
            let cancel = CancellationToken::new();
            let permits = acquire_reference_pipeline_permits(
                [ToolBinary::Sox, ToolBinary::Ffmpeg],
                Some(&float64_limits),
                &cancel,
            )
            .await
            .expect("Float64 composite acquire succeeds")
            .expect("Float64 composite acquire returns permits");
            float64_tx.send("float64").expect("record Float64 acquire");
            release_float64_rx.await.expect("release Float64 permits");
            drop(permits);
        });
        drop(acquired_tx);
        start.wait().await;

        let first = tokio::time::timeout(Duration::from_secs(1), acquired_rx.recv())
            .await
            .expect("one composite pipeline acquires both families")
            .expect("acquisition channel remains open");
        match first {
            "float32" => release_float32_tx
                .take()
                .expect("Float32 release sender is present")
                .send(())
                .expect("release Float32 first"),
            "float64" => release_float64_tx
                .take()
                .expect("Float64 release sender is present")
                .send(())
                .expect("release Float64 first"),
            other => panic!("unexpected first composite acquisition {other}"),
        }
        let second = tokio::time::timeout(Duration::from_secs(1), acquired_rx.recv())
            .await
            .expect("second composite pipeline proceeds after the first releases")
            .expect("acquisition channel remains open");
        match second {
            "float32" => release_float32_tx
                .take()
                .expect("Float32 release sender was not already consumed")
                .send(())
                .expect("release Float32 second"),
            "float64" => release_float64_tx
                .take()
                .expect("Float64 release sender was not already consumed")
                .send(())
                .expect("release Float64 second"),
            other => panic!("unexpected second composite acquisition {other}"),
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            float32.await.expect("Float32 composite task joins");
            float64.await.expect("Float64 composite task joins");
        })
        .await
        .expect("opposite-direction composite pipelines cannot deadlock");
        assert_eq!(limits.sox.available_permits(), 1);
        assert_eq!(limits.ffmpeg.available_permits(), 1);
    }

    #[tokio::test]
    async fn reference_pipeline_composite_permits_deduplicate_and_release_partial_acquisition() {
        let limits = Arc::new(ToolConcurrencyLimits::new(1, 1, 1, 4));
        let cancel = CancellationToken::new();

        let duplicate_set = tokio::time::timeout(
            Duration::from_secs(1),
            acquire_reference_pipeline_permits(
                [
                    ToolBinary::Sox,
                    ToolBinary::Sox,
                    ToolBinary::Ffmpeg,
                    ToolBinary::Ffprobe,
                ],
                Some(&limits),
                &cancel,
            ),
        )
        .await
        .expect("duplicate tool binaries collapse to two resource families")
        .expect("deduplicated composite acquisition succeeds")
        .expect("deduplicated composite acquisition returns permits");
        assert_eq!(limits.sox.available_permits(), 0);
        assert_eq!(limits.ffmpeg.available_permits(), 0);
        drop(duplicate_set);
        assert_eq!(limits.sox.available_permits(), 1);
        assert_eq!(limits.ffmpeg.available_permits(), 1);

        let held_ffmpeg = acquire_owned_permit(limits.ffmpeg.clone(), &cancel)
            .await
            .expect("test holds FFmpeg so composite acquisition blocks on its second family");
        let blocked_limits = limits.clone();
        let blocked_cancel = CancellationToken::new();
        let task_cancel = blocked_cancel.clone();
        let blocked = tokio::spawn(async move {
            acquire_reference_pipeline_permits(
                [ToolBinary::Ffmpeg, ToolBinary::Sox],
                Some(&blocked_limits),
                &task_cancel,
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            while limits.sox.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("composite acquisition holds SoX before waiting for FFmpeg");
        blocked_cancel.cancel();
        let error = tokio::time::timeout(Duration::from_secs(1), blocked)
            .await
            .expect("cancellation releases a partial composite acquisition")
            .expect("partial-acquisition task joins")
            .expect_err("cancelled partial acquisition fails closed");
        assert!(matches!(error, ConvertError::Realize(message) if message == "cancelled"));
        assert_eq!(limits.sox.available_permits(), 1);
        assert_eq!(limits.ffmpeg.available_permits(), 0);
        drop(held_ffmpeg);
        assert_eq!(limits.ffmpeg.available_permits(), 1);
    }

    #[tokio::test]
    async fn ffmpeg_uses_its_own_higher_limit_without_omp_injection() {
        let limits = Arc::new(ToolConcurrencyLimits::new(1, 3, 1, 8));
        let cancel = CancellationToken::new();
        let mut held = Vec::new();
        for _ in 0..3 {
            let mut cmd = test_tool_command(ToolBinary::Ffmpeg);
            let permit = acquire_tool_permit(&mut cmd, Some(&limits), &cancel)
                .await
                .expect("FFmpeg permit acquired")
                .expect("FFmpeg has a permit");
            assert_eq!(env_value(&cmd, "OMP_NUM_THREADS"), None);
            held.push(permit);
        }

        let mut fourth = test_tool_command(ToolBinary::Ffmpeg);
        let blocked = tokio::time::timeout(
            Duration::from_millis(25),
            acquire_tool_permit(&mut fourth, Some(&limits), &cancel),
        )
        .await;
        assert!(blocked.is_err(), "fourth FFmpeg waits at its own limit");
        drop(held.pop());

        let permit = tokio::time::timeout(
            Duration::from_secs(1),
            acquire_tool_permit(&mut fourth, Some(&limits), &cancel),
        )
        .await
        .expect("FFmpeg permit available after release")
        .expect("fourth FFmpeg acquire succeeds")
        .expect("FFmpeg has a permit");
        drop(permit);
    }

    #[tokio::test]
    async fn ffprobe_shares_the_ffmpeg_limit() {
        let limits = Arc::new(ToolConcurrencyLimits::new(1, 1, 1, 8));
        let cancel = CancellationToken::new();

        let mut ffmpeg = test_tool_command(ToolBinary::Ffmpeg);
        let ffmpeg_permit = acquire_tool_permit(&mut ffmpeg, Some(&limits), &cancel)
            .await
            .expect("FFmpeg permit acquired")
            .expect("FFmpeg has a permit");

        let mut ffprobe = test_tool_command(ToolBinary::Ffprobe);
        let blocked = tokio::time::timeout(
            Duration::from_millis(25),
            acquire_tool_permit(&mut ffprobe, Some(&limits), &cancel),
        )
        .await;
        assert!(blocked.is_err(), "FFprobe waits behind the FFmpeg family limit");

        drop(ffmpeg_permit);
        let ffprobe_permit = tokio::time::timeout(
            Duration::from_secs(1),
            acquire_tool_permit(&mut ffprobe, Some(&limits), &cancel),
        )
        .await
        .expect("FFprobe permit available after FFmpeg release")
        .expect("FFprobe acquire succeeds")
        .expect("FFprobe has a permit");
        drop(ffprobe_permit);
    }

    #[tokio::test]
    async fn cancellation_while_waiting_for_tool_permit_returns_promptly() {
        let limits = Arc::new(ToolConcurrencyLimits::new(1, 1, 1, 4));
        let cancel = CancellationToken::new();
        let mut first = test_tool_command(ToolBinary::Sox);
        let _held = acquire_tool_permit(&mut first, Some(&limits), &cancel)
            .await
            .expect("first SoX permit acquired")
            .expect("SoX has a permit");

        let waiter_limits = limits.clone();
        let waiter_cancel = cancel.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut waiter = tokio::spawn(async move {
            let mut waiting = test_tool_command(ToolBinary::Sox);
            let _ = started_tx.send(());
            let result = acquire_tool_permit(&mut waiting, Some(&waiter_limits), &waiter_cancel)
                .await;
            matches!(result, Err(ConvertError::Realize(message)) if message == "cancelled")
        });

        started_rx
            .await
            .expect("waiter task reached the permit acquire path");
        tokio::select! {
            joined = &mut waiter => {
                let cancelled = joined.expect("waiter task joined");
                panic!("waiter completed before cancellation; cancelled={cancelled}");
            }
            _ = tokio::time::sleep(Duration::from_millis(25)) => {}
        }

        cancel.cancel();
        let cancelled = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("blocked semaphore wait wakes after cancellation")
            .expect("waiter task joined");
        assert!(
            cancelled,
            "cancelled acquire returns ConvertError::Realize after the waiter is blocked"
        );
    }

    #[tokio::test]
    async fn cancelled_token_wins_over_immediately_available_tool_permit() {
        let limits = Arc::new(ToolConcurrencyLimits::new(1, 1, 1, 4));
        let cancel = CancellationToken::new();
        cancel.cancel();

        let mut cmd = test_tool_command(ToolBinary::Sox);
        let err = acquire_tool_permit(&mut cmd, Some(&limits), &cancel)
            .await
            .expect_err("cancelled token is checked before taking an available permit");

        assert!(
            matches!(err, ConvertError::Realize(message) if message == "cancelled"),
            "cancelled acquire returns ConvertError::Realize"
        );
        assert_eq!(
            limits.sox.available_permits(),
            1,
            "pre-cancelled acquire must not consume the immediately available SoX permit"
        );
    }

    #[tokio::test]
    async fn missing_tool_limits_leave_commands_unchanged() {
        let cancel = CancellationToken::new();
        let mut cmd = test_tool_command(ToolBinary::Sox);
        let permit = acquire_tool_permit(&mut cmd, None, &cancel)
            .await
            .expect("unlimited acquire succeeds");
        assert!(permit.is_none());
        assert!(cmd.env.is_empty());
    }

    #[tokio::test]
    async fn execute_commands_holds_tool_permit_until_command_future_finishes() {
        let temp = TempDir::new().expect("temp dir");
        let output = temp.path().join("ssrc-output.wav");
        let limits = Arc::new(ToolConcurrencyLimits::new(4, 4, 1, 4));
        let (gate, blocker) = tool_gate();
        let runner = Arc::new(BlockingToolRunner::with_behaviors([
            ToolBehavior::BlockThenSucceedAndWrite {
                gate: blocker,
                path: output.clone(),
                bytes: b"audio".to_vec(),
            },
        ]));
        let cancel = CancellationToken::new();
        let commands = vec![planned_command_for_test(
            ToolIdentifier::Ssrc,
            vec!["hold-permit".to_string()],
            &output,
        )];
        let run_runner = runner.clone();
        let run_limits = limits.clone();
        let run_cancel = cancel.clone();
        let handle = tokio::spawn(async move {
            execute_commands_for_test(commands, run_runner.as_ref(), &run_cancel, Some(run_limits)).await
        });

        let release = gate.wait_started().await;
        assert_eq!(
            limits.ssrc.available_permits(),
            0,
            "execute_commands keeps the SSRC permit while the runner future is still pending"
        );

        release.release();
        let records = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("blocked command completes after gate release")
            .expect("execute task joins")
            .expect("command succeeds");
        assert_eq!(records.len(), 1);
        assert_eq!(limits.ssrc.available_permits(), 1);
        assert_eq!(std::fs::read(&output).expect("output written"), b"audio");
    }

    #[tokio::test]
    async fn execute_commands_passes_sox_omp_threads_to_streaming_dispatch() {
        let temp = TempDir::new().expect("temp dir");
        let output = temp.path().join("sox-output.flac");
        let limits = Arc::new(ToolConcurrencyLimits::new(1, 4, 4, 7));
        let cancel = CancellationToken::new();
        let runner = BlockingToolRunner::new();
        let commands = vec![planned_command_for_test(
            ToolIdentifier::Sox,
            vec![CAPTURE_PLANNED_COMMAND_FOR_TEST_ARG.to_string()],
            &output,
        )];

        let records = execute_commands_for_test(commands, &runner, &cancel, Some(limits))
            .await
            .expect("captured SoX command succeeds");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].binary, ToolBinary::Sox);
        assert!(
            records[0].env_keys.iter().any(|key| key == "OMP_NUM_THREADS"),
            "SoX command reaches run_planned_command with the injected OpenMP variable"
        );
        assert!(
            records[0].stdout_tail.lines().any(|line| line == "OMP_NUM_THREADS=7"),
            "test capture preserves the injected OpenMP value passed into the streaming dispatch point"
        );
    }

    #[tokio::test]
    async fn execute_commands_uses_ssrc_semaphore_for_ssrc_commands() {
        let temp = TempDir::new().expect("temp dir");
        let output = temp.path().join("ssrc-gated.wav");
        let limits = Arc::new(ToolConcurrencyLimits::new(8, 8, 1, 4));
        let cancel = CancellationToken::new();
        let held_ssrc = limits
            .ssrc
            .clone()
            .acquire_owned()
            .await
            .expect("test holds the only SSRC permit");
        let (gate, blocker) = tool_gate();
        let runner = Arc::new(BlockingToolRunner::with_behaviors([
            ToolBehavior::BlockThenSucceedAndWrite {
                gate: blocker,
                path: output.clone(),
                bytes: b"audio".to_vec(),
            },
        ]));
        let commands = vec![planned_command_for_test(
            ToolIdentifier::Ssrc,
            vec!["wait-for-ssrc".to_string()],
            &output,
        )];
        let run_runner = runner.clone();
        let run_limits = limits.clone();
        let run_cancel = cancel.clone();
        let handle = tokio::spawn(async move {
            execute_commands_for_test(commands, run_runner.as_ref(), &run_cancel, Some(run_limits)).await
        });

        // Verify the command hasn't started yet (SSRC permit exhausted)
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(
            runner.transcript().len(),
            0,
            "SSRC command must not reach the runner while its own semaphore is exhausted"
        );
        assert_eq!(
            limits.ffmpeg.available_permits(),
            8,
            "waiting for SSRC does not consume FFmpeg-family permits"
        );

        drop(held_ssrc);
        let release = tokio::time::timeout(Duration::from_secs(1), gate.wait_started())
            .await
            .expect("SSRC command starts after SSRC permit release");
        release.release();
        let records = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("SSRC command completes after permit release")
            .expect("execute task joins")
            .expect("command succeeds");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].binary, ToolBinary::Ssrc);
    }

    #[test]
    fn deterministic_work_dir_reset_removes_stale_intermediates() {
        let root = std::env::temp_dir().join(format!(
            "tonepoet-track-executor-test-{}",
            std::process::id()
        ));
        let stale = root.join("stale.tmp");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&stale, b"stale").unwrap();

        reset_track_work_dir(&root).unwrap();

        assert!(root.is_dir());
        assert!(!stale.exists());
        cleanup_track_work_dir(&root).unwrap();
    }

    #[test]
    fn weighted_windows_cover_requested_range() {
        let commands = vec![
            PlannedCommand::new(
                ToolIdentifier::Ffmpeg,
                vec![],
                InputSource::Path(PathBuf::from("a")),
                OutputSink::Path(PathBuf::from("b")),
                Some(Duration::from_secs(1)),
                "one",
            ),
            PlannedCommand::new(
                ToolIdentifier::Sox,
                vec![],
                InputSource::Path(PathBuf::from("b")),
                OutputSink::Path(PathBuf::from("c")),
                Some(Duration::from_secs(3)),
                "two",
            ),
        ];
        let windows = command_windows(&commands, 0.2, 1.0);
        assert!((windows[0].0 - 0.2).abs() < 0.001);
        assert!((windows[0].1 - 0.4).abs() < 0.001);
        assert!((windows[1].1 - 1.0).abs() < 0.001);
    }

    #[test]
    fn cleanup_paths_removes_planner_declared_intermediates_idempotently() {
        let root = std::env::temp_dir().join(format!(
            "tonepoet-cleanup-paths-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let first = root.join("a.tmp");
        let second = root.join("b.tmp");
        std::fs::write(&first, b"a").unwrap();
        std::fs::write(&second, b"b").unwrap();
        let paths = vec![first.clone(), second.clone()];

        cleanup_paths_checked(&paths).unwrap();
        cleanup_paths_checked(&paths).unwrap();

        assert!(!first.exists());
        assert!(!second.exists());
        cleanup_track_work_dir(&root).unwrap();
    }

    #[test]
    fn copy_to_work_path_replaces_stale_tmp_and_final_deterministically() {
        let root = std::env::temp_dir().join(format!(
            "tonepoet-copy-work-path-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let input = root.join("input.flac");
        let work = root.join("work.flac");
        let stale_tmp = root.join("work.flac.tmp");
        std::fs::write(&input, b"new").unwrap();
        std::fs::write(&work, b"old-final").unwrap();
        std::fs::write(&stale_tmp, b"old-tmp").unwrap();

        copy_to_work_path(&input, &work).unwrap();
        assert_eq!(std::fs::read(&work).unwrap(), b"new");
        assert!(!stale_tmp.exists());

        std::fs::write(&input, b"newer").unwrap();
        copy_to_work_path(&input, &work).unwrap();
        assert_eq!(std::fs::read(&work).unwrap(), b"newer");
        cleanup_track_work_dir(&root).unwrap();
    }



    struct ProductionCleanupFixture {
        _temp: TempDir,
        request: PipelineRequest,
        track: PreparedTrack,
        realized_input: PathBuf,
        staged_output: PathBuf,
        convert_root: PathBuf,
        work_dir: PathBuf,
        cleanup_paths: Vec<PathBuf>,
    }

    impl ProductionCleanupFixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("temp dir");
            let realized_input = temp.path().join("input.wav");
            std::fs::write(&realized_input, b"pcm").expect("write source placeholder");
            let staged_output = temp.path().join("staged-output.flac");
            let convert_root = temp.path().join("convert");
            let work_dir = convert_root.join(".track-0001.work");

            let mut request = metadata_test_request(temp.path());
            request.settings.force_encode = true;
            request.settings.metadata.transfer_tags = false;
            request.settings.metadata.preserve_artwork = false;
            request.settings.metadata.store_source_audio_md5 = false;
            request.stages.metadata = StageRequirement::Disabled;

            let track = PreparedTrack {
                id: TrackId {
                    source_ordinal: 1,
                    disc_number: None,
                    track_number: 1,
                },
                source_ref: TrackSourceRef::StagedFile(realized_input.clone()),
                metadata: TrackMetadata {
                    title: Some("Cleanup Fixture".to_string()),
                    track_number: Some(1),
                    ..TrackMetadata::default()
                },
                expected_samples: Some(44_100),
                sample_rate: Some(44_100),
                bit_depth: Some(16),
                source_audio: SourceAudioDescriptor::from_scalar(
                    Some(44_100),
                    Some(16),
                    Some(SourceAudioCoding::Pcm),
                ),
                warnings: Vec::new(),
            };
            let plan_request = plan_request_for_track(
                &request,
                &track,
                &realized_input,
                &staged_output,
                work_dir.clone(),
            )
            .expect("cleanup fixture plan request");
            let plan = plan_conversion(&plan_request).expect("cleanup fixture plan");
            match &plan.action {
                PlanAction::Execute {
                    commands,
                    steps,
                    finalization,
                    ..
                } => {
                    assert!(
                        !commands.is_empty() || !steps.is_empty(),
                        "cleanup fixture must exercise production command execution"
                    );
                    assert!(
                        finalization.is_some(),
                        "cleanup fixture must exercise production finalization"
                    );
                }
                PlanAction::PassthroughCopy { .. } => {
                    panic!("cleanup fixture unexpectedly planned passthrough")
                }
            }
            let cleanup_paths = plan.cleanup_paths().to_vec();

            Self {
                _temp: temp,
                request,
                track,
                realized_input,
                staged_output,
                convert_root,
                work_dir,
                cleanup_paths,
            }
        }

        fn assert_clean(&self) {
            assert!(!self.work_dir.exists(), "deterministic track work directory remains");
            for path in &self.cleanup_paths {
                assert!(!path.exists(), "planner cleanup path remains: {}", path.display());
            }
        }

        fn progress(&self) -> OperationProgressTracker<'static> {
            OperationProgressTracker::new(
                "production-cleanup".to_string(),
                PipelineStage::Convert,
                None,
            )
        }
    }


    fn reference_materialization_request(
        source: &Path,
        work_dir: &Path,
        source_kind: tonepoet_pipeline::DsdSourceKind,
    ) -> tonepoet_pipeline::PlanRequest {
        let mut settings = tonepoet_pipeline::PipelineSettings::default();
        settings.dsd = tonepoet_pipeline::DsdSettings::native_v2();
        settings.target_format = tonepoet_pipeline::AudioFormat::Wav;
        settings.target_sample_rate = tonepoet_pipeline::RateTarget::PcmHz(88_200);
        settings.target_bit_depth = tonepoet_pipeline::BitDepthTarget::Pcm(
            tonepoet_pipeline::PcmBitDepth::Float64,
        );
        let format = match &source_kind {
            tonepoet_pipeline::DsdSourceKind::DsfUncompressed
            | tonepoet_pipeline::DsdSourceKind::SacdTrack { .. } => {
                tonepoet_pipeline::AudioFormat::Dsf
            }
            tonepoet_pipeline::DsdSourceKind::DsdiffUncompressed
            | tonepoet_pipeline::DsdSourceKind::DsdiffDst
            | tonepoet_pipeline::DsdSourceKind::UnknownDsdContainer => {
                tonepoet_pipeline::AudioFormat::Dff
            }
        };
        tonepoet_pipeline::PlanRequest {
            input_path: source.to_path_buf(),
            output_path: work_dir
                .parent()
                .unwrap_or(work_dir)
                .join("reference-output.w64"),
            source: tonepoet_pipeline::SourceInfo {
                format,
                codec: tonepoet_pipeline::AudioCodec::Dsd,
                sample_rate_hz: Some(2_822_400),
                bit_depth: None,
                true_source_depth: None,
                source_representation: tonepoet_pipeline::SourceRepresentationKind::Dsd,
                sample_kind: Some(tonepoet_pipeline::SampleKind::Dsd),
                channels: Some(2),
                duration: Some(Duration::from_secs(1)),
                dsd_source_kind: Some(source_kind),
                audio_md5: None,
            },
            settings,
            intermediate_dir: Some(work_dir.to_path_buf()),
            container_ffmpeg_flags: Vec::new(),
            resolved_output_target: Some(tonepoet_pipeline::ResolvedOutputTarget::WavW64),
            reference_programme_scope: tonepoet_pipeline::ReferenceProgrammeScope::Singleton,
            planned_riff_non_audio_upper_bound_bytes: Some(0),
        }
    }

    fn reference_materialization_track(source_ref: TrackSourceRef) -> PreparedTrack {
        PreparedTrack {
            id: TrackId {
                source_ordinal: 1,
                disc_number: None,
                track_number: 1,
            },
            source_ref,
            metadata: TrackMetadata {
                title: Some("Reference cleanup fixture".to_string()),
                track_number: Some(1),
                ..TrackMetadata::default()
            },
            expected_samples: None,
            sample_rate: Some(2_822_400),
            bit_depth: None,
            source_audio: SourceAudioDescriptor::from_scalar(
                Some(2_822_400),
                None,
                Some(SourceAudioCoding::Dsd),
            ),
            warnings: Vec::new(),
        }
    }

    fn write_reference_dsf_fixture(path: &Path) {
        let file = std::fs::File::create(path).expect("create DSF cleanup fixture");
        let mut writer = sacd_rs::dsf_writer::DsfWriter::new(file, 2, 2_822_400)
            .expect("create DSF cleanup writer");
        writer
            .write_interleaved(&vec![0x69; 2 * 256 * 1024])
            .expect("write DSF cleanup payload");
        writer.finish().expect("finish DSF cleanup fixture");
    }

    fn write_reference_dst_fixture(path: &Path) {
        let file = std::fs::File::create(path).expect("create DST cleanup fixture");
        let mut writer = sacd_rs::dff_dst_writer::DffDstWriter::new(file, 2, 2_822_400)
            .expect("create DST cleanup writer");
        writer
            .write_encoded_frame(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/crates/sacd-rs/src/dst/fixtures/frame_001.dst.bin"
                )),
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/crates/sacd-rs/src/dst/fixtures/frame_001.dsd.bin"
                )),
            )
            .expect("write DST cleanup frame");
        writer.finish().expect("finish DST cleanup fixture");
    }

    fn write_reference_sacd_fixture(path: &Path) {
        use std::io::{Seek, SeekFrom, Write};

        const SECTOR_SIZE: u64 = 2048;
        let file = std::fs::File::create(path).expect("create SACD cleanup fixture");
        file.set_len(700 * SECTOR_SIZE)
            .expect("size SACD cleanup fixture");
        drop(file);
        let mut file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open SACD cleanup fixture");

        let mut master = vec![0_u8; 0xa8];
        master[0..8].copy_from_slice(b"SACDMTOC");
        master[0x08] = 1;
        master[0x09] = 20;
        master[0x10..0x12].copy_from_slice(&1_u16.to_be_bytes());
        master[0x12..0x14].copy_from_slice(&1_u16.to_be_bytes());
        master[0x40..0x44].copy_from_slice(&540_u32.to_be_bytes());
        master[0x54..0x56].copy_from_slice(&3_u16.to_be_bytes());
        master[0x80] = 1;
        master[0x88..0x8a].copy_from_slice(b"en");
        master[0x8a] = 2;
        file.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        file.write_all(&master).unwrap();

        let mut area = vec![0_u8; SECTOR_SIZE as usize];
        area[0..8].copy_from_slice(b"TWOCHTOC");
        area[0x08] = 1;
        area[0x09] = 20;
        area[0x0a..0x0c].copy_from_slice(&3_u16.to_be_bytes());
        area[0x10..0x14].copy_from_slice(&64_000_u32.to_be_bytes());
        area[0x14] = 0x04;
        area[0x15] = 2;
        area[0x20] = 2;
        area[0x22] = 2;
        area[0x40] = 0;
        area[0x41] = 0;
        area[0x42] = 8;
        area[0x45] = 1;
        area[0x48..0x4c].copy_from_slice(&650_u32.to_be_bytes());
        area[0x4c..0x50].copy_from_slice(&658_u32.to_be_bytes());
        area[0x50] = 1;
        area[0x58..0x5a].copy_from_slice(b"en");
        area[0x5a] = 2;
        file.seek(SeekFrom::Start(540 * SECTOR_SIZE)).unwrap();
        file.write_all(&area).unwrap();

        let mut track_lsns = vec![0_u8; SECTOR_SIZE as usize];
        track_lsns[0..8].copy_from_slice(b"SACDTRL1");
        track_lsns[8..12].copy_from_slice(&650_u32.to_be_bytes());
        let length_offset = 8 + 255 * 4;
        track_lsns[length_offset..length_offset + 4]
            .copy_from_slice(&8_u32.to_be_bytes());
        file.seek(SeekFrom::Start(541 * SECTOR_SIZE)).unwrap();
        file.write_all(&track_lsns).unwrap();

        let mut track_times = vec![0_u8; SECTOR_SIZE as usize];
        track_times[0..8].copy_from_slice(b"SACDTRL2");
        let duration_offset = 8 + 255 * 4;
        track_times[duration_offset + 2] = 8;
        file.seek(SeekFrom::Start(542 * SECTOR_SIZE)).unwrap();
        file.write_all(&track_times).unwrap();
        file.sync_all().unwrap();
    }

    async fn wait_for_reference_cleanup(work_dir: &Path, cleanup_paths: &[PathBuf]) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !work_dir.exists() && cleanup_paths.iter().all(|path| !path.exists()) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("blocking materializer exits and performs deferred cleanup");
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(!work_dir.exists(), "work directory was recreated after cleanup");
        for path in cleanup_paths {
            assert!(!path.exists(), "Reference scratch path remains: {}", path.display());
        }
    }

    fn reference_production_request(root: &Path) -> PipelineRequest {
        let mut request = metadata_test_request(root);
        request.settings.dsd = tonepoet_pipeline::DsdSettings::native_v2();
        request.settings.target_format = tonepoet_pipeline::AudioFormat::Wav;
        request.settings.target_sample_rate = tonepoet_pipeline::RateTarget::PcmHz(88_200);
        request.settings.target_bit_depth = tonepoet_pipeline::BitDepthTarget::Pcm(
            tonepoet_pipeline::PcmBitDepth::Float64,
        );
        request.settings.force_encode = true;
        request.settings.metadata.transfer_tags = false;
        request.settings.metadata.preserve_artwork = false;
        request.settings.metadata.store_source_audio_md5 = false;
        request.stages.metadata = StageRequirement::Disabled;
        request.container_extension = Some("w64".to_string());
        request.container_ffmpeg_flags.clear();
        request
    }

    async fn assert_reference_production_abort_cleanup(
        source: PathBuf,
        source_kind: tonepoet_pipeline::DsdSourceKind,
        point: ReferenceMaterializationPausePoint,
    ) {
        let temp = tempfile::tempdir().expect("Reference production abort tempdir");
        let staged_output = temp.path().join("reference-output.w64");
        let convert_root = temp.path().join("convert");
        let work_dir = convert_root.join(".track-0001.work");
        let request = reference_production_request(temp.path());
        let track = reference_materialization_track(TrackSourceRef::StagedFile(source.clone()));
        let plan_request = plan_request_for_track(
            &request,
            &track,
            &source,
            &staged_output,
            work_dir.clone(),
        )
        .expect("Reference production abort plan request");
        assert_eq!(
            plan_request.source.dsd_source_kind.as_ref(),
            Some(&source_kind),
            "production plan bridge must classify the intended Reference source kind"
        );
        let plan = tonepoet_pipeline::plan_conversion(&plan_request)
            .expect("Reference production abort fixture plans");
        assert!(plan.reference.is_some(), "fixture must select Reference execution");
        let scratch = reference_scratch_paths(&plan_request).expect("planned scratch paths");
        validate_reference_scratch_cleanup_authority(&plan, &scratch)
            .expect("planner owns every Reference scratch path");
        let cleanup_paths = plan.cleanup_paths().to_vec();

        let pause = ReferenceMaterializationPause::new(point);
        let task_pause = pause.clone();
        let task_request = Arc::new(request);
        let task_track = Arc::new(track);
        let task_source = source.clone();
        let task_staged_output = staged_output.clone();
        let task_convert_root = convert_root.clone();
        let handle = tokio::spawn(REFERENCE_TEST_SKIP_ATTESTATION.scope(
            (),
            REFERENCE_MATERIALIZATION_PAUSE.scope(task_pause, async move {
                let runner = StubToolRunner::new();
                let cancel = CancellationToken::new();
                let mut progress = OperationProgressTracker::new(
                    "reference-production-abort".to_string(),
                    PipelineStage::Convert,
                    None,
                );
                execute_planned_track_conversion(
                    task_request.as_ref(),
                    task_track.as_ref(),
                    &task_source,
                    &task_staged_output,
                    &task_convert_root,
                    &runner,
                    &cancel,
                    &HashMap::new(),
                    None,
                    &mut progress,
                    0.0,
                    1.0,
                )
                .await
            }),
        ));

        tokio::time::timeout(Duration::from_secs(5), pause.wait_until_reached())
            .await
            .expect("production Reference materializer reached abort barrier");
        handle.abort();
        let join_error = handle.await.expect_err("outer production executor is aborted");
        assert!(join_error.is_cancelled());
        assert!(
            work_dir.exists(),
            "outer abort must defer work-root removal until the blocking worker exits"
        );

        let retry_cancel = CancellationToken::new();
        let retry_acquire = TrackExecutionCleanupGuard::acquire(work_dir.clone(), &retry_cancel);
        tokio::pin!(retry_acquire);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), retry_acquire.as_mut())
                .await
                .is_err(),
            "a retry must not reset the deterministic work root while the old worker is live"
        );

        pause.release();
        let retry_guard = tokio::time::timeout(Duration::from_secs(5), retry_acquire.as_mut())
            .await
            .expect("retry authority becomes available after the old worker exits")
            .expect("retry authority acquisition succeeds");
        wait_for_reference_cleanup(&work_dir, &cleanup_paths).await;
        drop(retry_guard);
    }

    async fn assert_reference_sacd_materialization_abort_cleanup(
        source: PathBuf,
        source_kind: tonepoet_pipeline::DsdSourceKind,
        source_ref: TrackSourceRef,
    ) {
        let temp = tempfile::tempdir().expect("Reference SACD abort tempdir");
        let work_dir = temp.path().join("convert/.track-0001.work");
        let request = reference_materialization_request(&source, &work_dir, source_kind);
        let scratch = reference_scratch_paths(&request).expect("planned scratch paths");
        // SACD remains an intentionally unadmitted v15 cell. Exercise the
        // production blocking extractor seam with the same deterministic
        // planner namespace so future admission cannot regress ownership.
        let cleanup_paths = scratch
            .all()
            .into_iter()
            .map(Path::to_path_buf)
            .collect::<Vec<_>>();
        let track = reference_materialization_track(source_ref);
        let pause = ReferenceMaterializationPause::new(
            ReferenceMaterializationPausePoint::DuringSacdExtraction,
        );
        let task_pause = pause.clone();
        let task_work_dir = work_dir.clone();
        let task_source = source.clone();
        let task_cleanup_paths = cleanup_paths.clone();
        let handle = tokio::spawn(REFERENCE_MATERIALIZATION_PAUSE.scope(task_pause, async move {
            let cancel = CancellationToken::new();
            let cleanup_guard =
                TrackExecutionCleanupGuard::acquire(task_work_dir.clone(), &cancel).await?;
            reset_track_work_dir(&task_work_dir)?;
            cleanup_guard.add_planner_paths(&task_cleanup_paths);
            materialize_reference_source(
                &request,
                &track,
                &task_source,
                &scratch,
                &cancel,
                cleanup_guard.blocking_worker_lease()?,
            )
            .await
        }));

        tokio::time::timeout(Duration::from_secs(5), pause.wait_until_reached())
            .await
            .expect("Reference SACD materializer reached abort barrier");
        handle.abort();
        let join_error = handle.await.expect_err("outer SACD materialization task is aborted");
        assert!(join_error.is_cancelled());
        assert!(
            work_dir.exists(),
            "outer abort must defer work-root removal until the blocking worker exits"
        );

        let retry_cancel = CancellationToken::new();
        let retry_acquire = TrackExecutionCleanupGuard::acquire(work_dir.clone(), &retry_cancel);
        tokio::pin!(retry_acquire);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), retry_acquire.as_mut())
                .await
                .is_err(),
            "a retry must not reset the deterministic work root while the old worker is live"
        );

        pause.release();
        let retry_guard = tokio::time::timeout(Duration::from_secs(5), retry_acquire.as_mut())
            .await
            .expect("retry authority becomes available after the old worker exits")
            .expect("retry authority acquisition succeeds");
        wait_for_reference_cleanup(&work_dir, &cleanup_paths).await;
        drop(retry_guard);
    }

    #[test]
    fn reference_plan_declares_every_materialization_and_verification_path() {
        for source_kind in [
            tonepoet_pipeline::DsdSourceKind::DsfUncompressed,
            tonepoet_pipeline::DsdSourceKind::DsdiffUncompressed,
            tonepoet_pipeline::DsdSourceKind::DsdiffDst,
        ] {
            let request = reference_materialization_request(
                Path::new("source.dsd"),
                Path::new("work"),
                source_kind,
            );
            let plan = tonepoet_pipeline::plan_conversion(&request)
                .expect("Reference scratch declaration fixture plans");
            let scratch = reference_scratch_paths(&request).unwrap();
            validate_reference_scratch_cleanup_authority(&plan, &scratch).unwrap();
            assert_eq!(
                scratch
                    .all()
                    .into_iter()
                    .filter(|path| plan.cleanup_paths().iter().any(|item| item.as_path() == *path))
                    .count(),
                scratch.all().len()
            );
        }
    }

    #[tokio::test]
    async fn reference_abort_before_materialization_creation_cannot_race_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.dsf");
        write_reference_dsf_fixture(&source);
        assert_reference_production_abort_cleanup(
            source,
            tonepoet_pipeline::DsdSourceKind::DsfUncompressed,
            ReferenceMaterializationPausePoint::BeforeScratchPathCreation,
        )
        .await;
    }

    #[tokio::test]
    async fn reference_abort_during_source_copy_cannot_recreate_work_root() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.dsf");
        write_reference_dsf_fixture(&source);
        assert_reference_production_abort_cleanup(
            source,
            tonepoet_pipeline::DsdSourceKind::DsfUncompressed,
            ReferenceMaterializationPausePoint::DuringSourceCopy,
        )
        .await;
    }

    #[tokio::test]
    async fn reference_abort_during_dst_decode_cannot_recreate_work_root() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.dff");
        write_reference_dst_fixture(&source);
        assert_reference_production_abort_cleanup(
            source,
            tonepoet_pipeline::DsdSourceKind::DsdiffDst,
            ReferenceMaterializationPausePoint::DuringDstDecode,
        )
        .await;
    }

    #[tokio::test]
    async fn reference_abort_during_sacd_extraction_cannot_recreate_work_root() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.iso");
        write_reference_sacd_fixture(&source);
        let source_kind = reference_sacd_source_kind(&source, 0, SacdArea::Stereo)
            .expect("synthetic SACD cleanup fixture parses");
        assert_reference_sacd_materialization_abort_cleanup(
            source.clone(),
            source_kind,
            TrackSourceRef::SacdTrack {
                iso: source,
                track_index: 0,
                area: SacdArea::Stereo,
            },
        )
        .await;
    }

    #[tokio::test]
    async fn cancellation_while_waiting_for_track_work_authority_returns_promptly() {
        let temp = tempfile::tempdir().unwrap();
        let work_dir = temp.path().join("work");
        let owner_cancel = CancellationToken::new();
        let owner = TrackExecutionCleanupGuard::acquire(work_dir.clone(), &owner_cancel)
            .await
            .expect("initial track work authority is acquired");
        reset_track_work_dir(&work_dir).unwrap();
        let worker_lease = owner
            .blocking_worker_lease()
            .expect("blocking worker retains cleanup authority");
        drop(owner);
        assert!(
            work_dir.exists(),
            "blocking worker defers cleanup while retaining the work-root permit"
        );

        let waiter_cancel = CancellationToken::new();
        let task_cancel = waiter_cancel.clone();
        let task_work_dir = work_dir.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            let _ = started_tx.send(());
            TrackExecutionCleanupGuard::acquire(task_work_dir, &task_cancel).await
        });

        started_rx
            .await
            .expect("retry reached track work-authority acquisition");
        tokio::time::sleep(Duration::from_millis(25)).await;
        waiter_cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("cancelled retry wakes while the blocking worker still owns the permit")
            .expect("retry task joins");
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("cancelled retry must not acquire track work authority"),
        };
        assert!(
            matches!(error.error, ConvertError::Realize(message) if message == "cancelled"),
            "waiting cleanup-authority acquisition reports cancellation"
        );
        assert!(
            work_dir.exists(),
            "cancelling the waiter must not disturb the active worker's work root"
        );

        drop(worker_lease);
        assert!(
            !work_dir.exists(),
            "the final worker release performs the previously deferred cleanup"
        );
    }

    #[test]
    fn successful_command_transcript_is_preserved_when_final_cleanup_fails() {
        let command = test_tool_command(ToolBinary::Ffmpeg);
        let record = command_record_for_unstarted_command(&command);
        let error = finish_track_execution(
            Ok(ExecutedTrackPlan {
                commands: vec![record.clone()],
                elapsed: Duration::from_millis(10),
                metadata_satisfaction: PlannedMetadataSatisfaction::default(),
                metadata_required: PlannedMetadataSatisfaction::default(),
                command_hash: None,
                reference: None,
            }),
            Err(io::Error::other("injected governed cleanup failure")),
        )
        .expect_err("cleanup failure converts successful execution into an error");

        assert_eq!(error.commands.len(), 1);
        assert_eq!(error.commands[0].binary, record.binary);
        assert_eq!(error.commands[0].sanitized_args, record.sanitized_args);
        assert!(
            error.to_string().contains("governed scratch cleanup failed"),
            "cleanup failure remains explicit while preserving the transcript"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn governed_cleanup_failure_is_reported_by_execution_guard() {
        let temp = tempfile::tempdir().unwrap();
        let work_dir = temp.path().join("work");
        let cancel = CancellationToken::new();
        let cleanup_guard = TrackExecutionCleanupGuard::acquire(work_dir.clone(), &cancel)
            .await
            .unwrap();
        reset_track_work_dir(&work_dir).unwrap();
        cleanup_guard.add_planner_paths(&[PathBuf::from("/proc/self/status")]);

        let error = cleanup_guard
            .cleanup_now()
            .expect_err("an unremovable governed path must make cleanup fail");
        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(!work_dir.exists(), "independent work-root cleanup still runs");
        let state = cleanup_guard
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(state.cleanup_requested);
        assert!(
            !state.cleanup_complete,
            "failed cleanup must remain retryable instead of being marked complete"
        );
    }

    #[tokio::test]
    async fn production_executor_cleans_every_injected_failure_and_cancellation_boundary() {
        for point in [
            TrackExecutionFailurePoint::PlanConstruction,
            TrackExecutionFailurePoint::Attestation,
            TrackExecutionFailurePoint::Materialization,
            TrackExecutionFailurePoint::ProducerLaunch,
            TrackExecutionFailurePoint::ConsumerLaunch,
            TrackExecutionFailurePoint::Measurement,
            TrackExecutionFailurePoint::TerminalProcessing,
            TrackExecutionFailurePoint::Packaging,
            TrackExecutionFailurePoint::Finalization,
            TrackExecutionFailurePoint::Cancellation,
        ] {
            let fixture = ProductionCleanupFixture::new();
            let runner = StubToolRunner::new();
            let cancel = CancellationToken::new();
            let mut progress = fixture.progress();
            let result = TRACK_EXECUTION_USE_INJECTED_RUNNER
                .scope(
                    (),
                    TRACK_EXECUTION_FAILURE_POINT.scope(
                        point,
                        execute_planned_track_conversion(
                            &fixture.request,
                            &fixture.track,
                            &fixture.realized_input,
                            &fixture.staged_output,
                            &fixture.convert_root,
                            &runner,
                            &cancel,
                            &HashMap::new(),
                            None,
                            &mut progress,
                            0.0,
                            1.0,
                        ),
                    ),
                )
                .await;

            let error = result.expect_err("injected production failure must abort execution");
            if point == TrackExecutionFailurePoint::Cancellation {
                assert!(error.to_string().contains("cancelled"));
            } else {
                assert!(error.to_string().contains(point.label()));
            }
            fixture.assert_clean();
        }
    }

    #[tokio::test]
    async fn production_executor_cleans_partial_output_on_real_tool_failure() {
        let fixture = ProductionCleanupFixture::new();
        let partial = fixture.work_dir.join("runner.partial");
        let runner = BlockingToolRunner::with_behaviors([ToolBehavior::FailAfterWriting {
            path: partial.clone(),
            bytes: b"partial".to_vec(),
            stderr: "injected tool failure".to_string(),
        }]);
        let cancel = CancellationToken::new();
        let mut progress = fixture.progress();

        TRACK_EXECUTION_USE_INJECTED_RUNNER
            .scope(
                (),
                execute_planned_track_conversion(
                    &fixture.request,
                    &fixture.track,
                    &fixture.realized_input,
                    &fixture.staged_output,
                    &fixture.convert_root,
                    &runner,
                    &cancel,
                    &HashMap::new(),
                    None,
                    &mut progress,
                    0.0,
                    1.0,
                ),
            )
            .await
            .expect_err("tool failure must abort production execution");

        assert!(!partial.exists());
        fixture.assert_clean();
    }

    #[tokio::test]
    async fn production_executor_cleans_when_real_finalization_fails() {
        let fixture = ProductionCleanupFixture::new();
        let runner = StubToolRunner::new();
        let cancel = CancellationToken::new();
        let mut progress = fixture.progress();

        TRACK_EXECUTION_USE_INJECTED_RUNNER
            .scope(
                (),
                execute_planned_track_conversion(
                    &fixture.request,
                    &fixture.track,
                    &fixture.realized_input,
                    &fixture.staged_output,
                    &fixture.convert_root,
                    &runner,
                    &cancel,
                    &HashMap::new(),
                    None,
                    &mut progress,
                    0.0,
                    1.0,
                ),
            )
            .await
            .expect_err("missing command output must fail finalization");

        fixture.assert_clean();
    }

    #[tokio::test]
    async fn production_executor_cleans_partial_output_on_real_cancellation() {
        let fixture = Arc::new(ProductionCleanupFixture::new());
        let partial = fixture.work_dir.join("cancelled.partial");
        let (gate, blocker) = tool_gate();
        let runner = Arc::new(BlockingToolRunner::with_behaviors([
            ToolBehavior::BlockThenSucceed(blocker),
        ]));
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let run_fixture = fixture.clone();
        let run_runner = runner.clone();
        let handle = tokio::spawn(async move {
            let mut progress = run_fixture.progress();
            TRACK_EXECUTION_USE_INJECTED_RUNNER
                .scope(
                    (),
                    execute_planned_track_conversion(
                        &run_fixture.request,
                        &run_fixture.track,
                        &run_fixture.realized_input,
                        &run_fixture.staged_output,
                        &run_fixture.convert_root,
                        run_runner.as_ref(),
                        &run_cancel,
                        &HashMap::new(),
                        None,
                        &mut progress,
                        0.0,
                        1.0,
                    ),
                )
                .await
        });

        let release = gate.wait_started().await;
        std::fs::write(&partial, b"partial").expect("seed partial output before cancellation");
        cancel.cancel();
        let error = handle
            .await
            .expect("production execution task joins")
            .expect_err("cancellation must abort production execution");

        assert!(error.to_string().contains("cancelled"));
        assert!(!partial.exists());
        fixture.assert_clean();
        drop(release);
    }

    #[tokio::test]
    async fn dropping_production_executor_future_runs_cleanup_guard() {
        let fixture = Arc::new(ProductionCleanupFixture::new());
        let (gate, blocker) = tool_gate();
        let runner = Arc::new(BlockingToolRunner::with_behaviors([
            ToolBehavior::BlockThenSucceed(blocker),
        ]));
        let run_fixture = fixture.clone();
        let run_runner = runner.clone();
        let handle = tokio::spawn(async move {
            let cancel = CancellationToken::new();
            let mut progress = run_fixture.progress();
            TRACK_EXECUTION_USE_INJECTED_RUNNER
                .scope(
                    (),
                    execute_planned_track_conversion(
                        &run_fixture.request,
                        &run_fixture.track,
                        &run_fixture.realized_input,
                        &run_fixture.staged_output,
                        &run_fixture.convert_root,
                        run_runner.as_ref(),
                        &cancel,
                        &HashMap::new(),
                        None,
                        &mut progress,
                        0.0,
                        1.0,
                    ),
                )
                .await
        });

        let release = gate.wait_started().await;
        std::fs::write(fixture.work_dir.join("aborted.partial"), b"partial")
            .expect("seed partial output before abort");
        handle.abort();
        let join_error = handle.await.expect_err("aborted task must not join successfully");
        assert!(join_error.is_cancelled());
        fixture.assert_clean();
        drop(release);
    }

    #[tokio::test]
    async fn silence_scan_temporary_is_removed_on_failure_and_success() {
        let plan = reference_w64_plan(tonepoet_pipeline::PcmBitDepth::Float64);
        let carrier = plan
            .reference
            .as_ref()
            .expect("Reference summary")
            .decoded_carrier(ReferenceDecodedCarrierSelector::TerminalQpcm)
            .expect("qualified Float64 W64 carrier");

        for behavior in [
            ToolBehavior::FailAfterWriting {
                path: PathBuf::from("placeholder"),
                bytes: vec![0; 8],
                stderr: "decoder failed".to_string(),
            },
            ToolBehavior::SucceedAndWrite {
                path: PathBuf::from("placeholder"),
                bytes: vec![0; 8],
            },
        ] {
            let temp = tempfile::tempdir().expect("temp dir");
            let raw = temp.path().join("reference-silence-scan.f64le");
            let behavior = match behavior {
                ToolBehavior::FailAfterWriting { bytes, stderr, .. } => {
                    ToolBehavior::FailAfterWriting {
                        path: raw.clone(),
                        bytes,
                        stderr,
                    }
                }
                ToolBehavior::SucceedAndWrite { bytes, .. } => ToolBehavior::SucceedAndWrite {
                    path: raw.clone(),
                    bytes,
                },
                _ => unreachable!(),
            };
            let runner = BlockingToolRunner::with_behaviors([behavior]);
            let cancel = CancellationToken::new();
            let _ = verify_signed_zero_audio(&carrier, &runner, &cancel, None, &raw).await;
            assert!(!raw.exists(), "silence scan raw stream remains after return");
        }
    }

    #[tokio::test]
    async fn silence_scan_temporary_is_removed_on_decoder_cancellation() {
        let plan = reference_w64_plan(tonepoet_pipeline::PcmBitDepth::Float64);
        let carrier = plan
            .reference
            .as_ref()
            .expect("Reference summary")
            .decoded_carrier(ReferenceDecodedCarrierSelector::TerminalQpcm)
            .expect("qualified Float64 W64 carrier");
        let temp = tempfile::tempdir().expect("temp dir");
        let raw = temp.path().join("reference-silence-scan.f64le");
        let (gate, blocker) = tool_gate();
        let runner = Arc::new(BlockingToolRunner::with_behaviors([
            ToolBehavior::BlockThenSucceed(blocker),
        ]));
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let run_runner = runner.clone();
        let run_carrier = carrier.clone();
        let planned_raw_path = raw.clone();
        let handle = tokio::spawn(async move {
            verify_signed_zero_audio(
                &run_carrier,
                run_runner.as_ref(),
                &run_cancel,
                None,
                &planned_raw_path,
            )
            .await
        });

        let release = gate.wait_started().await;
        std::fs::write(&raw, vec![0; 8]).expect("seed partial silence stream");
        cancel.cancel();
        let error = handle
            .await
            .expect("silence task joins")
            .expect_err("decoder cancellation must fail silence verification");
        assert!(error.to_string().contains("cancelled"));
        assert!(!raw.exists());
        drop(release);
    }

    #[test]
    fn weighted_windows_handle_ffmpeg_ssrc_sox_chain_deterministically() {
        let commands = vec![
            PlannedCommand::new(
                ToolIdentifier::Ffmpeg,
                vec![],
                InputSource::Path(PathBuf::from("input.wav")),
                OutputSink::Path(PathBuf::from("stage1.wav")),
                Some(Duration::from_secs(2)),
                "decode",
            ),
            PlannedCommand::new(
                ToolIdentifier::Ssrc,
                vec![],
                InputSource::Path(PathBuf::from("stage1.wav")),
                OutputSink::Path(PathBuf::from("stage2.wav")),
                Some(Duration::from_secs(6)),
                "resample",
            ),
            PlannedCommand::new(
                ToolIdentifier::Sox,
                vec![],
                InputSource::Path(PathBuf::from("stage2.wav")),
                OutputSink::Path(PathBuf::from("output.flac")),
                Some(Duration::from_secs(2)),
                "dither",
            ),
        ];

        let windows = command_windows(&commands, 0.1, 0.9);
        assert_eq!(windows.len(), 3);
        assert!((windows[0].0 - 0.1).abs() < 0.001);
        assert!((windows[0].1 - 0.26).abs() < 0.001);
        assert!((windows[1].1 - 0.74).abs() < 0.001);
        assert!((windows[2].1 - 0.9).abs() < 0.001);
    }

    #[test]
    fn atomic_finalization_replaces_stale_staged_output_deterministically() {
        let root = std::env::temp_dir().join(format!(
            "tonepoet-finalization-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let from = root.join("from.tmp");
        let to = root.join("to.flac");
        std::fs::write(&from, b"new-output").unwrap();
        std::fs::write(&to, b"stale-output").unwrap();

        apply_finalization(&Finalization::AtomicRename { from: from.clone(), to: to.clone() }).unwrap();

        assert!(!from.exists());
        assert_eq!(std::fs::read(&to).unwrap(), b"new-output");
        cleanup_track_work_dir(&root).unwrap();
    }

}

#[cfg(test)]
mod chunk_2_1_3_mid_chain_failure_and_cancel_tests {
    use super::*;
    use crate::convert::pipeline::stages::ScheduledTrackOutput;
    use crate::convert::pipeline::tool::blocking_test_runner::{
        tool_gate, BlockingToolRunner, ToolBehavior,
    };
    use crate::convert::pipeline::types::{
        PipelineStage, SourceAudioDescriptor, TrackId, TrackMetadata, TrackOutcome, TrackRecord,
        TrackSourceRef,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;
    use tonepoet_pipeline::plan::ConversionPlan;
    use tonepoet_pipeline::{Finalization, InputSource, OutputSink, PlanAction, ToolIdentifier};

    struct SyntheticChain {
        _temp: TempDir,
        work_dir: PathBuf,
        stage1: PathBuf,
        stage2: PathBuf,
        final_work: PathBuf,
        final_output: PathBuf,
        plan: ConversionPlan,
    }

    impl SyntheticChain {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("temp dir");
            let work_dir = temp.path().join(".track-0001.work");
            let stage1 = work_dir.join("stage-1.wav");
            let stage2 = work_dir.join("stage-2.wav");
            let final_work = work_dir.join("final-work.flac");
            let final_output = temp.path().join("01 - output.flac");
            let plan = ConversionPlan::execute_with_cleanup(
                vec![
                    PlannedCommand::new(
                        ToolIdentifier::Ssrc,
                        vec!["step-1".to_string()],
                        InputSource::Path(temp.path().join("source.flac")),
                        OutputSink::Path(stage1.clone()),
                        Some(Duration::from_secs(1)),
                        "decode to pcm",
                    ),
                    PlannedCommand::new(
                        ToolIdentifier::Ssrc,
                        vec!["step-2".to_string()],
                        InputSource::Path(stage1.clone()),
                        OutputSink::Path(stage2.clone()),
                        Some(Duration::from_secs(1)),
                        "resample pcm",
                    ),
                    PlannedCommand::new(
                        ToolIdentifier::Ssrc,
                        vec!["step-3".to_string()],
                        InputSource::Path(stage2.clone()),
                        OutputSink::Path(final_work.clone()),
                        Some(Duration::from_secs(1)),
                        "encode final",
                    ),
                ],
                vec![stage1.clone(), stage2.clone(), final_work.clone()],
                Some(Finalization::AtomicRename {
                    from: final_work.clone(),
                    to: final_output.clone(),
                }),
            );
            Self {
                _temp: temp,
                work_dir,
                stage1,
                stage2,
                final_work,
                final_output,
                plan,
            }
        }

        fn all_cleanup_paths_absent(&self) -> bool {
            self.plan.cleanup_paths().iter().all(|path| !path.exists())
        }
    }

    async fn execute_synthetic_plan(
        chain: &SyntheticChain,
        runner: &dyn ToolRunner,
        cancel: &CancellationToken,
    ) -> Result<Vec<CommandRecord>, TrackExecutionError> {
        reset_track_work_dir(&chain.work_dir)?;
        cleanup_paths_checked(chain.plan.cleanup_paths()).map_err(ConvertError::Io)?;
        let mut progress = OperationProgressTracker::new(
            "chunk-2-1-3".to_string(),
            PipelineStage::Convert,
            None,
        );

        let result = match &chain.plan.action {
            PlanAction::Execute {
                commands,
                finalization,
                ..
            } => {
                match execute_commands(
                    commands,
                    runner,
                    cancel,
                    &HashMap::new(),
                    None,
                    &mut progress,
                    0.0,
                    1.0,
                    "synthetic track".to_string(),
                )
                .await
                {
                    Ok(records) => {
                        if let Some(finalization) = finalization {
                            if let Err(err) = apply_finalization(finalization) {
                                return Err(TrackExecutionError::new(err, records));
                            }
                        }
                        Ok(records)
                    }
                    Err(err) => Err(err),
                }
            }
            PlanAction::PassthroughCopy { .. } => unreachable!("synthetic chain is executable"),
        };

        match result {
            Ok(records) => {
                cleanup_paths_checked(chain.plan.cleanup_paths()).map_err(ConvertError::Io)?;
                cleanup_track_work_dir(&chain.work_dir).map_err(ConvertError::Io)?;
                Ok(records)
            }
            Err(err) => {
                if let Err(cleanup_error) = cleanup_declared_paths_and_work_dir(
                    chain.plan.cleanup_paths(),
                    &chain.work_dir,
                ) {
                    let primary = err.to_string();
                    return Err(err.with_message(format!(
                        "{primary}; synthetic cleanup also failed: {cleanup_error}"
                    )));
                }
                Err(err)
            }
        }
    }

    fn success_behavior_for(path: &Path) -> ToolBehavior {
        ToolBehavior::SucceedAndWrite {
            path: path.to_path_buf(),
            bytes: b"audio".to_vec(),
        }
    }

    fn assert_transcript_prefix(runner: &BlockingToolRunner, invoked: usize) {
        let transcript = runner.transcript();
        assert_eq!(transcript.len(), invoked);
        for (index, record) in transcript.iter().enumerate() {
            assert_eq!(record.binary, ToolBinary::Ssrc);
            assert!(
                record
                    .sanitized_args
                    .iter()
                    .any(|arg| arg == &format!("step-{}", index + 1)),
                "transcript entry {} should contain step argument; got {:?}",
                index + 1,
                record.sanitized_args
            );
        }
    }

    fn synthetic_track(chain: &SyntheticChain) -> PreparedTrack {
        PreparedTrack {
            id: TrackId {
                source_ordinal: 1,
                disc_number: None,
                track_number: 1,
            },
            source_ref: TrackSourceRef::StagedFile(chain._temp.path().join("source.flac")),
            metadata: TrackMetadata {
                title: Some("Synthetic Track".to_string()),
                track_number: Some(1),
                ..TrackMetadata::default()
            },
            expected_samples: Some(44_100),
            sample_rate: Some(44_100),
            bit_depth: Some(16),
            source_audio: SourceAudioDescriptor::default(),
            warnings: Vec::new(),
        }
    }

    fn scheduled_failure_output_for_chain(
        chain: &SyntheticChain,
        track: &PreparedTrack,
        error: &TrackExecutionError,
        commands: Vec<CommandRecord>,
    ) -> ScheduledTrackOutput {
        ScheduledTrackOutput {
            index: 0,
            record: TrackRecord {
                track_id: track.id.clone(),
                outcome: TrackOutcome::Err(error.to_string()),
                source_ref: track.source_ref.clone(),
                realized_input: Some(chain._temp.path().join("source.flac")),
                output_file: Some(chain.final_output.clone()),
                commands,
                bytes_in: None,
                bytes_out: None,
                duration: None,
                verified_output_bit_depth: None,
                dsd_dst_stats: None,
            },
            artifact: None,
            ok: false,
            metadata_satisfaction: PlannedMetadataSatisfaction::none(),
        }
    }

    fn assert_failed_scheduled_shape(
        output: &ScheduledTrackOutput,
        failed_step: usize,
        expected_commands: usize,
    ) {
        assert_eq!(output.index, 0);
        assert!(!output.ok);
        assert!(output.artifact.is_none());
        assert!(matches!(output.record.outcome, TrackOutcome::Err(_)));
        assert_eq!(output.record.commands.len(), expected_commands);
        assert!(
            output
                .record
                .commands
                .iter()
                .enumerate()
                .all(|(index, record)| record
                    .sanitized_args
                    .iter()
                    .any(|arg| arg == &format!("step-{}", index + 1))),
            "scheduled failure output preserved command order up to failed step"
        );
        assert!(
            matches!(&output.record.outcome, TrackOutcome::Err(message) if message.contains(&format!("planned command {} failed", failed_step + 1)))
        );
    }

    #[tokio::test]
    async fn three_step_chain_cleans_intermediates_at_each_failure_position() {
        for failed_step in 0..3 {
            let chain = SyntheticChain::new();
            let mut behaviors = Vec::new();
            for step in 0..3 {
                if step == failed_step {
                    let failed_path = match step {
                        0 => &chain.stage1,
                        1 => &chain.stage2,
                        _ => &chain.final_work,
                    };
                    behaviors.push(ToolBehavior::FailAfterWriting {
                        path: failed_path.clone(),
                        bytes: b"partial".to_vec(),
                        stderr: format!("step {} failed", step + 1),
                    });
                    break;
                }
                let path = match step {
                    0 => &chain.stage1,
                    1 => &chain.stage2,
                    _ => &chain.final_work,
                };
                behaviors.push(success_behavior_for(path));
            }
            let runner = BlockingToolRunner::with_behaviors(behaviors);
            let cancel = CancellationToken::new();

            let err = execute_synthetic_plan(&chain, &runner, &cancel)
                .await
                .expect_err("failed step should abort chain");
            let scheduled = scheduled_failure_output_for_chain(
                &chain,
                &synthetic_track(&chain),
                &err,
                err.commands.clone(),
            );

            assert!(err.to_string().contains(&format!(
                "planned command {} failed",
                failed_step + 1
            )));
            assert_failed_scheduled_shape(&scheduled, failed_step, failed_step + 1);
            assert!(chain.all_cleanup_paths_absent(), "planner-declared files are gone (failed_step={failed_step})");
            assert!(!chain.final_output.exists(), "failed chain did not publish final output");
            assert!(!chain.work_dir.exists(), "track work dir was deleted (failed_step={failed_step})");
            assert_transcript_prefix(&runner, failed_step + 1);
        }
    }

    #[tokio::test]
    async fn three_step_chain_success_keeps_final_and_deletes_work_files() {
        let chain = SyntheticChain::new();
        let runner = BlockingToolRunner::with_behaviors([
            success_behavior_for(&chain.stage1),
            success_behavior_for(&chain.stage2),
            success_behavior_for(&chain.final_work),
        ]);
        let cancel = CancellationToken::new();

        let records = execute_synthetic_plan(&chain, &runner, &cancel)
            .await
            .expect("all steps succeed");

        assert_eq!(records.len(), 3);
        assert_transcript_prefix(&runner, 3);
        assert_eq!(std::fs::read(&chain.final_output).unwrap(), b"audio");
        assert!(chain.all_cleanup_paths_absent());
        assert!(!chain.work_dir.exists());
    }

    #[tokio::test]
    async fn cancellation_during_first_command_cleans_partial_outputs() {
        let chain = Arc::new(SyntheticChain::new());
        let (gate, blocker) = tool_gate();
        let runner = Arc::new(BlockingToolRunner::with_behaviors([
            ToolBehavior::BlockThenSucceedAndWrite {
                gate: blocker,
                path: chain.stage1.clone(),
                bytes: b"partial".to_vec(),
            },
        ]));
        let cancel = CancellationToken::new();
        let run_cancel = cancel.clone();
        let run_chain = chain.clone();
        let run_runner = runner.clone();
        let handle = tokio::spawn(async move {
            execute_synthetic_plan(run_chain.as_ref(), run_runner.as_ref(), &run_cancel).await
        });

        let release = gate.wait_started().await;
        cancel.cancel();
        let err = handle
            .await
            .expect("conversion task joins")
            .expect_err("cancellation aborts command");

        assert!(err.to_string().contains("cancelled"));
        assert!(chain.all_cleanup_paths_absent());
        assert!(!chain.final_output.exists());
        assert!(!chain.work_dir.exists());
        assert_transcript_prefix(&runner, 1);
        drop(release);
    }

    #[tokio::test]
    async fn cancellation_between_command_one_and_two_skips_second_command() {
        let chain = SyntheticChain::new();
        let runner = BlockingToolRunner::with_behaviors([
            ToolBehavior::SucceedAndWriteThenCancel {
                path: chain.stage1.clone(),
                bytes: b"stage1".to_vec(),
            },
            success_behavior_for(&chain.stage2),
            success_behavior_for(&chain.final_work),
        ]);
        let cancel = CancellationToken::new();

        let err = execute_synthetic_plan(&chain, &runner, &cancel)
            .await
            .expect_err("cancelled token aborts before command 2");

        assert!(err.to_string().contains("cancelled"));
        assert_transcript_prefix(&runner, 1);
        assert!(chain.all_cleanup_paths_absent());
        assert!(!chain.final_output.exists());
        assert!(!chain.work_dir.exists());
    }
}
