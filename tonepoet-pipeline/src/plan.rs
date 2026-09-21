//! Deterministic conversion-chain planner.

use crate::dsd_reference::{
    DsdReferencePlanSummary, ReferenceProgrammeScope, ResolvedOutputTarget,
};
use crate::enums::{
    AudioCodec, AudioFormat, BitDepthTarget, DitherType, DsdFilterPreset, DsdLowpassMethod,
    DsdRate, NyquistTransition, PcmBitDepth, RateTarget, SampleKind, SsrcProfile,
};
use crate::error::{PlanningError, Result};
use crate::mapping;
use crate::settings::{
    default_pcm_depth_for_format, FlacSettings, PipelineSettings, WavPackSettings,
};
use crate::source::{SourceInfo, SourceRepresentationKind};
use crate::tools::{ToolIdentifier, ToolRegistry};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

/// Opaque owner of a planning/measurement scope.  Submitted-batch scopes use
/// the queue's existing persisted submission identity; track-local scopes use
/// the owning participant identity and never infer album membership from tags.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PlanScopeId(
    /// Stable serialized scope identifier.
    pub String,
);

/// Stable participant identity inside a planning scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PlanParticipantId(
    /// Stable serialized participant identifier.
    pub String,
);

/// Existing execution ownership projected into the pure planner.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PlanScope {
    /// One independent track.
    Track {
        /// Stable identifier for the owning planning scope.
        scope_id: PlanScopeId,
        /// Stable participant identity within the owning scope.
        participant_id: PlanParticipantId,
    },
    /// One exact submitted queue cohort.
    SubmittedBatch {
        /// Stable identifier for the owning planning scope.
        scope_id: PlanScopeId,
        /// Stable participant identity within the owning scope.
        participant_id: PlanParticipantId,
        /// Expected submitted-batch participant count, when known.
        expected_participants: Option<u32>,
    },
}

impl PlanScope {
    /// Construct an explicitly track-local scope.
    #[must_use]
    pub fn track(participant_id: impl Into<String>) -> Self {
        let participant_id = participant_id.into();
        Self::Track {
            scope_id: PlanScopeId(format!("track:{participant_id}")),
            participant_id: PlanParticipantId(participant_id),
        }
    }

    /// Construct an exact submitted-batch scope using the queue's authority.
    #[must_use]
    pub fn submitted_batch(
        submission_id: impl Into<String>,
        participant_id: impl Into<String>,
        expected_participants: Option<u32>,
    ) -> Self {
        let submission_id = submission_id.into();
        Self::SubmittedBatch {
            scope_id: PlanScopeId(format!("submission:{submission_id}")),
            participant_id: PlanParticipantId(participant_id.into()),
            expected_participants,
        }
    }

    /// Scope identity used to bind observations and common decisions.
    #[must_use]
    pub const fn scope_id(&self) -> &PlanScopeId {
        match self {
            Self::Track { scope_id, .. } | Self::SubmittedBatch { scope_id, .. } => scope_id,
        }
    }

    /// Participant identity within the scope.
    #[must_use]
    pub const fn participant_id(&self) -> &PlanParticipantId {
        match self {
            Self::Track { participant_id, .. }
            | Self::SubmittedBatch { participant_id, .. } => participant_id,
        }
    }
}

/// Request passed to the pure planner.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PlanRequest {
    /// Input path as known to the caller.
    pub input_path: PathBuf,
    /// Final output path requested by the caller.
    pub output_path: PathBuf,
    /// Source facts supplied by probing/extraction outside this crate.
    pub source: SourceInfo,
    /// Conversion parameters.
    pub settings: PipelineSettings,
    /// Existing submission/participant ownership for measurement and album decisions.
    pub plan_scope: PlanScope,
    /// Optional work directory for deterministic intermediate paths.
    pub intermediate_dir: Option<PathBuf>,
    /// Extra ffmpeg output flags for the selected container (e.g., `["-rf64", "auto"]`).
    /// Inserted before the output path in the ffmpeg command. Empty for most containers.
    #[cfg_attr(feature = "serde", serde(default))]
    pub container_ffmpeg_flags: Vec<String>,
    /// Exact format/container product identity resolved by the trusted catalog.
    #[cfg_attr(feature = "serde", serde(default))]
    pub resolved_output_target: Option<ResolvedOutputTarget>,
    /// Dispatcher-authored programme classification. P0 accepts only Singleton.
    #[cfg_attr(feature = "serde", serde(default))]
    pub reference_programme_scope: ReferenceProgrammeScope,
    /// Conservative upper bound for every non-audio RIFF byte that the complete
    /// metadata/artwork plan may add. Required for Reference RIFF admission;
    /// computed by the orchestrator from the exact source and metadata plan.
    #[cfg_attr(feature = "serde", serde(default))]
    pub planned_riff_non_audio_upper_bound_bytes: Option<u64>,
}

/// Return whether authoritative source classification and settings select the
/// qualified Reference DSD-to-PCM pathway.
///
/// This is the sole admission authority shared by the pure planner and every
/// orchestrator preflight. Callers with complete source facts pass
/// `SourceInfo::is_dsd()`. A pre-realization path may pass a known DSD source
/// classification (for example, an SACD track); a source-agnostic scheduling
/// guard may conservatively pass `true` only to defer work, and must recheck
/// exact source facts before any Reference-only probing or execution.
#[must_use]
pub fn selects_reference_dsd_to_pcm(
    settings: &PipelineSettings,
    source_is_dsd: bool,
) -> bool {
    source_is_dsd
        && settings.dsd.reference_delivery_selected()
        && !settings.target_format.is_dsd()
}

impl PlanRequest {
    /// Borrow this request as a plugin planning context.
    #[must_use]
    pub fn context(&self) -> PlanContext<'_> {
        PlanContext { request: self }
    }
}

/// Borrowed context supplied to plugins.
#[derive(Debug, Clone, Copy)]
pub struct PlanContext<'a> {
    /// Original request.
    pub request: &'a PlanRequest,
}

impl PlanContext<'_> {
    /// Container extension selected by the caller for the final artifact.
    ///
    /// The planner must respect this rather than deriving every work path from
    /// the codec enum. In particular AAC and ALAC are published as MP4/M4A
    /// containers so metadata and artwork can be represented by the muxer.
    #[must_use]
    pub fn target_container_extension(&self) -> String {
        self.request
            .output_path
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| !extension.trim().is_empty())
            .map(|extension| extension.to_ascii_lowercase())
            .unwrap_or_else(|| default_container_extension_for_format(&self.request.settings.target_format).to_string())
    }

    /// Deterministic path for an intermediate stage.
    #[must_use]
    pub fn intermediate_path(&self, step_index: usize, extension: &str) -> PathBuf {
        let base_dir = self
            .request
            .intermediate_dir
            .clone()
            .or_else(|| {
                self.request
                    .output_path
                    .parent()
                    .map(std::path::Path::to_path_buf)
            })
            .unwrap_or_else(|| PathBuf::from("."));
        let stem = self
            .request
            .output_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("tonepoet-output");
        base_dir.join(format!(
            ".{stem}.tonepoet-stage-{step_index:02}.{extension}"
        ))
    }

    /// Deterministic first work path used before the caller atomically renames to the requested path.
    #[must_use]
    pub fn final_work_path(&self) -> PathBuf {
        let extension = self.target_container_extension();
        let base_dir = self
            .request
            .intermediate_dir
            .clone()
            .or_else(|| {
                self.request
                    .output_path
                    .parent()
                    .map(std::path::Path::to_path_buf)
            })
            .unwrap_or_else(|| PathBuf::from("."));
        let stem = self
            .request
            .output_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("tonepoet-output");
        base_dir.join(format!(".{stem}.tonepoet-final.{extension}"))
    }
}

/// Source for a planned command.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum InputSource {
    /// Read from a filesystem path.
    Path(PathBuf),
    /// Read from standard input.
    Stdin,
}

impl InputSource {
    /// Return a path when this input is path-backed.
    #[must_use]
    pub fn as_path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Path(path) => Some(path.as_path()),
            Self::Stdin => None,
        }
    }
}

/// Sink for a planned command.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OutputSink {
    /// Write to a filesystem path.
    Path(PathBuf),
    /// Write to standard output.
    Stdout,
    /// Command modifies the input file in place.
    InPlace(PathBuf),
}

impl OutputSink {
    /// Return a path when this output is path-backed.
    #[must_use]
    pub fn as_path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Path(path) | Self::InPlace(path) => Some(path.as_path()),
            Self::Stdout => None,
        }
    }
}


/// Typed metadata effects produced by a planned command.
///
/// These facts describe only planner-owned metadata writes without requiring
/// executors or orchestrators to infer policy satisfaction from command-line
/// argument spelling. The distinction between original-source transfer and
/// immediate-input preservation is intentional: only effects that explicitly
/// read metadata from the original request input may satisfy the source-tag or
/// artwork obligations used by the orchestrator. An encoder that maps metadata
/// from its current input records the preservation fact separately, because the
/// current input may be an intermediate created by an earlier audio-only step.
///
/// Authoritative Tonepoet/materializer album and track tags are currently
/// orchestrator-owned and are intentionally not representable as a planner
/// command effect. If a future planner operation writes those tags, add an
/// explicit typed effect for that operation rather than reusing source tag,
/// artwork, or MD5 effects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MetadataPlanEffect {
    /// Original source-container text tags were transferred into the output.
    #[cfg_attr(feature = "serde", serde(default))]
    pub source_tags_transferred_from_original_source: bool,
    /// Original source artwork/video metadata was transferred into the output.
    #[cfg_attr(feature = "serde", serde(default))]
    pub artwork_transferred_from_original_source: bool,
    /// Text tags from the command's immediate input were preserved.
    #[cfg_attr(feature = "serde", serde(default))]
    pub tags_preserved_from_command_input: bool,
    /// Artwork/video metadata from the command's immediate input was preserved.
    #[cfg_attr(feature = "serde", serde(default))]
    pub artwork_preserved_from_command_input: bool,
    /// Source-audio MD5 metadata was written.
    #[cfg_attr(feature = "serde", serde(default))]
    pub source_audio_md5_written: bool,
}

impl MetadataPlanEffect {
    /// No metadata effect.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            source_tags_transferred_from_original_source: false,
            artwork_transferred_from_original_source: false,
            tags_preserved_from_command_input: false,
            artwork_preserved_from_command_input: false,
            source_audio_md5_written: false,
        }
    }

    /// Merge two effect records.
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        Self {
            source_tags_transferred_from_original_source: self.source_tags_transferred_from_original_source
                || other.source_tags_transferred_from_original_source,
            artwork_transferred_from_original_source: self.artwork_transferred_from_original_source
                || other.artwork_transferred_from_original_source,
            tags_preserved_from_command_input: self.tags_preserved_from_command_input
                || other.tags_preserved_from_command_input,
            artwork_preserved_from_command_input: self.artwork_preserved_from_command_input
                || other.artwork_preserved_from_command_input,
            source_audio_md5_written: self.source_audio_md5_written || other.source_audio_md5_written,
        }
    }
}

/// Environment inheritance semantics for one external command.
///
/// `InheritAndSet` preserves the ambient process environment and overlays the
/// command's explicit variables. `ClearAndSet` starts from an empty environment
/// and installs only the explicitly planned variables. Qualified Reference
/// subprocesses use the latter so execution cannot depend on unrecorded ambient
/// state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum CommandEnvironmentPolicy {
    /// Preserve the parent environment, then overlay explicit variables.
    #[default]
    InheritAndSet,
    /// Clear the parent environment, then install only explicit variables.
    ClearAndSet,
}

/// One command ready for an executor to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PlannedCommand {
    /// Tool selected by the registry.
    pub tool: ToolIdentifier,
    /// Argument vector, excluding argv[0]. Arguments include any input/output paths required by the tool.
    pub args: Vec<String>,
    /// Logical command input.
    pub input: InputSource,
    /// Logical command output.
    pub output: OutputSink,
    /// Environment inheritance policy.
    #[cfg_attr(feature = "serde", serde(default))]
    pub environment_policy: CommandEnvironmentPolicy,
    /// Stable environment variables requested by this command.
    pub environment: BTreeMap<String, String>,
    /// Optional progress estimate. This is media/progress time, not a wall-clock deadline.
    pub expected_duration: Option<Duration>,
    /// Optional explicit wall-clock process deadline. Executors may derive a broader
    /// budget when this is absent, but must honor an explicit budget when present.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub timeout_budget: Option<Duration>,
    /// User-facing description.
    pub description: String,
    /// Typed metadata effects produced by this command.
    #[cfg_attr(feature = "serde", serde(default))]
    pub metadata_effect: MetadataPlanEffect,
}

impl PlannedCommand {
    /// Construct a command with no special environment.
    #[must_use]
    pub fn new(
        tool: ToolIdentifier,
        args: Vec<String>,
        input: InputSource,
        output: OutputSink,
        expected_duration: Option<Duration>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            tool,
            args,
            input,
            output,
            environment_policy: CommandEnvironmentPolicy::InheritAndSet,
            environment: BTreeMap::new(),
            expected_duration,
            timeout_budget: None,
            description: description.into(),
            metadata_effect: MetadataPlanEffect::none(),
        }
    }

    /// Return this command with an explicit wall-clock process deadline.
    #[must_use]
    pub fn with_timeout_budget(mut self, timeout_budget: Duration) -> Self {
        self.timeout_budget = Some(timeout_budget);
        self
    }

    /// Return this command annotated with a typed metadata effect.
    #[must_use]
    pub fn with_metadata_effect(mut self, metadata_effect: MetadataPlanEffect) -> Self {
        self.metadata_effect = metadata_effect;
        self
    }
}

/// Typed two-process pipeline whose producer stdout is connected directly to
/// the consumer stdin. No shell participates in the transport.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(deny_unknown_fields))]
pub struct PlannedCommandPipeline {
    /// Path-backed producer that writes the canonical stream to stdout.
    pub producer: PlannedCommand,
    /// Stdin-backed consumer that writes the planned output.
    pub consumer: PlannedCommand,
    /// User-facing description for the pipeline as one semantic operation.
    pub description: String,
}

/// Post-command finalization the caller performs atomically.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Finalization {
    /// Rename a completed work file into the requested final path.
    AtomicRename {
        /// Completed work file.
        from: PathBuf,
        /// Requested final path.
        to: PathBuf,
    },
}

/// High-level plan action.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PlanAction {
    /// No encode commands are required. Caller should copy source to destination atomically.
    PassthroughCopy {
        /// Source path.
        input: PathBuf,
        /// Requested destination path.
        output: PathBuf,
        /// Deterministic work path the caller should write first.
        work_path: PathBuf,
        /// Deterministic work files the executor may delete after success or interruption.
        cleanup_paths: Vec<PathBuf>,
        /// Final atomic rename from the completed work path to the requested destination.
        finalization: Finalization,
        /// Reason selected by the planner.
        reason: String,
    },
    /// Execute commands in order, then perform finalization.
    Execute {
        /// Static planned command list. Empty for a qualified Reference plan,
        /// whose authoritative common graph is lowered by the shared executor.
        commands: Vec<PlannedCommand>,
        /// Deterministic work files the executor may delete after success or failure.
        /// Paths are listed here so interrupted reruns can clean or overwrite known
        /// stage files instead of leaving untracked outputs.
        cleanup_paths: Vec<PathBuf>,
        /// Final atomic rename, when the command sequence writes to a work path.
        finalization: Option<Finalization>,
    },
}

/// Full conversion plan returned by [`plan_conversion`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ConversionPlan {
    /// Chosen action.
    pub action: PlanAction,
    /// Qualified Reference policy facts, absent for general plans.
    #[cfg_attr(feature = "serde", serde(default))]
    pub reference: Option<DsdReferencePlanSummary>,
}

impl ConversionPlan {
    /// Create a passthrough-copy plan.
    #[must_use]
    pub fn passthrough(
        input: PathBuf,
        output: PathBuf,
        work_path: PathBuf,
        reason: impl Into<String>,
    ) -> Self {
        let finalization = Finalization::AtomicRename {
            from: work_path.clone(),
            to: output.clone(),
        };
        Self {
            action: PlanAction::PassthroughCopy {
                input,
                output,
                work_path: work_path.clone(),
                cleanup_paths: vec![work_path],
                finalization,
                reason: reason.into(),
            },
            reference: None,
        }
    }

    /// Create an executable plan.
    #[must_use]
    pub fn execute(commands: Vec<PlannedCommand>, finalization: Option<Finalization>) -> Self {
        Self::execute_with_cleanup(commands, Vec::new(), finalization)
    }

    /// Create an executable plan and list deterministic work paths for executor cleanup.
    #[must_use]
    pub fn execute_with_cleanup(
        commands: Vec<PlannedCommand>,
        cleanup_paths: Vec<PathBuf>,
        finalization: Option<Finalization>,
    ) -> Self {
        Self {
            action: PlanAction::Execute {
                commands,
                cleanup_paths,
                finalization,
            },
            reference: None,
        }
    }

    /// Create a common-model Reference staging plan.
    ///
    /// The executable Reference region is lowered by the common runtime from
    /// the authoritative typed plan; no independent command/step vector is
    /// stored here.
    #[must_use]
    pub fn execute_reference_with_cleanup(
        cleanup_paths: Vec<PathBuf>,
        finalization: Option<Finalization>,
        reference: DsdReferencePlanSummary,
    ) -> Self {
        Self {
            action: PlanAction::Execute {
                commands: Vec::new(),
                cleanup_paths,
                finalization,
            },
            reference: Some(reference),
        }
    }

    /// Return command slice, or an empty slice for passthrough.
    #[must_use]
    pub fn commands(&self) -> &[PlannedCommand] {
        match &self.action {
            PlanAction::PassthroughCopy { .. } => &[],
            PlanAction::Execute { commands, .. } => commands,
        }
    }

    /// Return deterministic work paths that an executor may delete after success or failure.
    #[must_use]
    pub fn cleanup_paths(&self) -> &[PathBuf] {
        match &self.action {
            PlanAction::PassthroughCopy { cleanup_paths, .. } => cleanup_paths,
            PlanAction::Execute { cleanup_paths, .. } => cleanup_paths,
        }
    }
}

/// Provenance of the emitted command used by the Stage A selected-vs-emitted observer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum StageALoweringProvenance {
    /// The command was built with the physical tool already frozen by typed terminal selection.
    FixedTool,
    /// The retained command-plan bridge selected a plugin from the registry.
    RegistryReselection,
}

/// Measurement-only Stage A record comparing one typed built-in physical selection
/// with the command realization emitted by the retained lowerer.
///
/// These records are diagnostics only. They do not participate in planning,
/// admission, lowering, or execution decisions.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StageASelectedVsEmittedRecord {
    /// Index of the typed operation node in the common semantic plan.
    pub typed_node_index: usize,
    /// Typed logical operation whose selected physical candidate is being checked.
    pub operation: PlanOperation,
    /// Stable identity of the selected physical candidate.
    pub candidate_identity: String,
    /// Tool selected by the typed planner.
    pub selected_tool: ToolIdentifier,
    /// Index of the retained logical step that physically realizes this operation.
    pub emitted_step_index: usize,
    /// Retained logical operation owning the emitted command. Folded typed
    /// operations can therefore name a different operation here.
    pub emitted_operation: PlanOperation,
    /// Tool on the command emitted by the retained lowerer.
    pub emitted_tool: ToolIdentifier,
    /// Typed resolved parameters relevant to the selected operation.
    pub resolved_parameters: crate::semantic_plan::ResolvedOperationParameters,
    /// Operation-specific semantic projection of the command expected from the
    /// selected tool. Paths and unrelated command plumbing are excluded.
    pub selected_parameter_signature: Vec<String>,
    /// Operation-specific semantic projection of the actual emitted command.
    pub emitted_parameter_signature: Vec<String>,
    /// Whether command construction used a frozen tool or registry reselection.
    pub lowering_provenance: StageALoweringProvenance,
}

impl StageASelectedVsEmittedRecord {
    /// True when both the selected physical tool and the operation-relevant
    /// emitted parameters agree with the typed selection.
    #[must_use]
    pub fn matches_selected_realization(&self) -> bool {
        self.selected_tool == self.emitted_tool
            && self.selected_parameter_signature == self.emitted_parameter_signature
    }
}

/// Logical operation assigned to a tool plugin.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PlanOperation {
    /// Decode source audio to a PCM WAV intermediate.
    DecodeToPcm {
        /// Intermediate PCM bit depth.
        bit_depth: PcmBitDepth,
    },
    /// PCM resampling step. Brick-wall requests are normally handled by SSRC.
    ResamplePcm {
        /// Target PCM sample rate in Hz.
        target_rate_hz: u32,
        /// Target bit depth if the resampler owns bit-depth reduction.
        target_bit_depth: Option<PcmBitDepth>,
        /// SSRC profile when `brick_wall` is true.
        profile: Option<SsrcProfile>,
        /// Whether this is a brick-wall resampling step.
        brick_wall: bool,
    },
    /// Encode PCM or decoded audio to a PCM-capable lossless target.
    EncodePcm {
        /// Target format.
        target_format: AudioFormat,
        /// Optional target rate when a rate change is required.
        target_rate_hz: Option<u32>,
        /// Target bit depth.
        target_bit_depth: PcmBitDepth,
        /// Apply rate/depth/dither processing during this encode.
        apply_processing: bool,
    },
    /// Encode to a lossy target.
    EncodeLossy {
        /// Target format.
        target_format: AudioFormat,
        /// Optional target rate when a rate change is required.
        target_rate_hz: Option<u32>,
        /// Apply rate processing during this encode.
        apply_processing: bool,
    },
    /// Convert PCM to DSD.
    PcmToDsd {
        /// Target container.
        target_format: AudioFormat,
        /// Target DSD rate.
        target_rate: DsdRate,
        /// Filter preset.
        filter: DsdFilterPreset,
    },
    /// Convert DSD to a PCM target or PCM intermediate.
    DsdToPcm {
        /// Target format.
        target_format: AudioFormat,
        /// Target PCM rate in Hz.
        target_rate_hz: u32,
        /// Target PCM bit depth.
        target_bit_depth: PcmBitDepth,
        /// DSD low-pass method.
        lowpass: DsdLowpassMethod,
    },
    /// DSD-to-DSD rate/container change.
    DsdRateChange {
        /// Target format.
        target_format: AudioFormat,
        /// Target DSD rate.
        target_rate: DsdRate,
        /// DSD low-pass method used before remodulation.
        lowpass: DsdLowpassMethod,
    },
    /// Rewrite tags/artwork deterministically by copying encoded audio and applying the requested metadata policy.
    MetadataTransfer {
        /// Target format.
        target_format: AudioFormat,
        /// Copy source tags from the original source.
        transfer_tags: bool,
        /// Copy artwork/video streams from the original source.
        preserve_artwork: bool,
    },
    /// Store source-audio MD5 using a format-appropriate metadata mechanism.
    StoreSourceAudioMd5 {
        /// Target format.
        target_format: AudioFormat,
    },
    /// Decode verification command.
    Verify {
        /// Target format.
        target_format: AudioFormat,
    },
}

impl PlanOperation {
    /// Stable operation label.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::DecodeToPcm { .. } => "decode_to_pcm",
            Self::ResamplePcm { .. } => "resample_pcm",
            Self::EncodePcm { .. } => "encode_pcm",
            Self::EncodeLossy { .. } => "encode_lossy",
            Self::PcmToDsd { .. } => "pcm_to_dsd",
            Self::DsdToPcm { .. } => "dsd_to_pcm",
            Self::DsdRateChange { .. } => "dsd_rate_change",
            Self::MetadataTransfer { .. } => "metadata_transfer",
            Self::StoreSourceAudioMd5 { .. } => "store_source_audio_md5",
            Self::Verify { .. } => "verify",
        }
    }
}

/// Logical step before plugin command construction.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PlanStep {
    /// Step index in execution order.
    pub index: usize,
    /// Logical operation.
    pub operation: PlanOperation,
    /// Logical input.
    pub input: InputSource,
    /// Logical output.
    pub output: OutputSink,
    /// User-facing description.
    pub description: String,
}

impl PlanStep {
    /// Construct a step.
    #[must_use]
    pub fn new(
        index: usize,
        operation: PlanOperation,
        input: InputSource,
        output: OutputSink,
        description: impl Into<String>,
    ) -> Self {
        Self {
            index,
            operation,
            input,
            output,
            description: description.into(),
        }
    }
}

/// Logical topology plan.
#[derive(Debug, Clone, PartialEq)]
pub enum TopologyPlan {
    /// Caller should copy source to destination atomically.
    Passthrough {
        /// Reason selected by the planner.
        reason: String,
    },
    /// Execute logical steps and then finalization.
    Execute {
        /// Logical steps.
        steps: Vec<PlanStep>,
        /// Finalization instruction.
        finalization: Option<Finalization>,
    },
}

/// Build logical steps without constructing argv arrays.
pub fn plan_topology(request: &PlanRequest) -> Result<TopologyPlan> {
    request.settings.validate()?;
    request.source.validate()?;
    validate_request_paths(request)?;
    validate_request_semantics(request)?;
    validate_post_processing_inputs(request)?;

    let context = request.context();
    if is_passthrough(request) {
        validate_atomic_work_path(request, &context.final_work_path())?;
        return Ok(TopologyPlan::Passthrough {
            reason:
                "source format, rate, depth, metadata, ReplayGain, and verification already match"
                    .into(),
        });
    }

    let mut steps = Vec::new();
    let first_work = context.final_work_path();
    let mut current_input = InputSource::Path(request.input_path.clone());
    let mut current_output_path = first_work.clone();

    if conversion_is_stream_copy_only(request) {
        push_metadata_transfer(
            request,
            &mut steps,
            current_input.clone(),
            current_output_path.as_path(),
        );
    } else if request.settings.target_format.is_dsd() {
        plan_to_dsd(
            request,
            &context,
            &mut steps,
            &mut current_input,
            first_work.clone(),
        )?;
        current_output_path = first_work;
    } else if request.source.is_dsd() {
        plan_from_dsd(
            request,
            &context,
            &mut steps,
            &mut current_input,
            first_work.clone(),
        )?;
        current_output_path = first_work;
    } else {
        plan_from_pcm(
            request,
            &context,
            &mut steps,
            &mut current_input,
            first_work.clone(),
        )?;
        current_output_path = first_work;
    }

    append_post_processing(request, &context, &mut steps, &mut current_output_path)?;

    let finalization = Some(Finalization::AtomicRename {
        from: current_output_path,
        to: request.output_path.clone(),
    });
    validate_step_paths(request, &steps, &finalization)?;

    Ok(TopologyPlan::Execute {
        steps,
        finalization,
    })
}

/// Build a complete command plan with the built-in registry.
pub fn plan_conversion(request: &PlanRequest) -> Result<ConversionPlan> {
    plan_conversion_with_registry(request, &ToolRegistry::with_builtin_tools())
}

/// Build a complete command plan with a caller-provided registry.
pub fn plan_conversion_with_registry(
    request: &PlanRequest,
    registry: &ToolRegistry,
) -> Result<ConversionPlan> {
    let reference_delivery =
        selects_reference_dsd_to_pcm(&request.settings, request.source.is_dsd());
    if request.source.is_dsd()
        && !request.settings.target_format.is_dsd()
        && !matches!(
            request.settings.dsd.from_dsd.pathway,
            crate::dsd_reference::DsdSourcePathway::Custom
        )
    {
        // Preserve the sealed Reference/Manual pathway admission precedence and
        // exact public diagnostics before the common planner wraps typed refusals.
        crate::dsd_reference::resolve_reference_static_admission(request)?;
    }

    // Preserve the established public validation and topology error precedence.
    // The typed planner adds authority/selection proof; it must not mask basic
    // settings, source, or path errors that the executable topology already owns.
    let topology = if reference_delivery {
        None
    } else {
        Some(plan_topology(request)?)
    };

    let topology_has_sample_processing = topology.as_ref().is_some_and(|topology| match topology {
        TopologyPlan::Passthrough { .. } => false,
        TopologyPlan::Execute { steps, .. } => steps.iter().any(|step| {
            matches!(
                step.operation,
                PlanOperation::DecodeToPcm { .. }
                    | PlanOperation::ResamplePcm { .. }
                    | PlanOperation::EncodePcm { .. }
                    | PlanOperation::EncodeLossy { .. }
                    | PlanOperation::PcmToDsd { .. }
                    | PlanOperation::DsdToPcm { .. }
                    | PlanOperation::DsdRateChange { .. }
            )
        }),
    });
    // The common typed planner presently knows only the built-in physical
    // registry. Preserve the existing caller-provided registry contract for
    // custom formats until that registry is plumbed into typed selection.
    let typed_required = reference_delivery
        || (topology_has_sample_processing
            && !matches!(request.settings.target_format, AudioFormat::Custom { .. }));

    let typed = if typed_required {
        Some(match crate::semantic_plan::plan_typed(request) {
            Ok(crate::semantic_plan::PlanningOutcome::Ready(typed)) => {
                crate::semantic_plan::require_current_executor(&typed)?;
                typed
            }
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
        })
    } else {
        None
    };
    if reference_delivery {
        let typed = typed
            .as_ref()
            .expect("reference delivery always requires the typed common plan");
        let semantic_plan_hash_v1 =
            crate::fingerprint::common_semantic_plan_fingerprint_v1(request, typed).0;
        return crate::dsd_reference::plan_reference_dsd_with_common_hash(
            request,
            semantic_plan_hash_v1,
        );
    }
    match topology.expect("non-reference planning preflights executable topology") {
        TopologyPlan::Passthrough { reason } => {
            let work_path = request.context().final_work_path();
            Ok(ConversionPlan::passthrough(
                request.input_path.clone(),
                request.output_path.clone(),
                work_path,
                reason,
            ))
        }
        TopologyPlan::Execute {
            steps,
            finalization,
        } => {
            let context = request.context();
            let (steps, finalization) =
                prune_redundant_metadata_steps(&context, registry, &steps, finalization)?;
            let selected_direct_pcm_terminal = typed
                .as_ref()
                .map(selected_terminal_realization)
                .transpose()?
                .flatten()
                .and_then(|realization| match realization {
                    crate::semantic_plan::SelectedTerminalRealization::Pcm(realization)
                        if matches!(
                            realization.kind,
                            crate::semantic_plan::PcmTerminalRealizationKind::SoxDirect
                                | crate::semantic_plan::PcmTerminalRealizationKind::FfmpegDirect
                        ) => Some(realization),
                    _ => None,
                });
            let mut commands = Vec::with_capacity(steps.len());
            for step in &steps {
                let frozen_tool = selected_direct_pcm_terminal.and_then(|realization| {
                    matches!(
                        &step.operation,
                        PlanOperation::EncodePcm {
                            target_format,
                            target_rate_hz,
                            target_bit_depth,
                            ..
                        } if target_format == &realization.target_format
                            && target_rate_hz == &realization.target_rate_hz
                            && target_bit_depth == &realization.target_bit_depth
                    )
                    .then_some(&realization.selected_tool)
                });
                commands.push(match frozen_tool {
                    Some(tool) => registry.build_command_for_tool(&context, step, tool)?,
                    None => registry.build_command(&context, step)?,
                });
            }
            if let Some(typed) = typed.as_ref() {
                validate_selected_terminal_lowering(typed, &steps, &commands)?;
            }
            let cleanup_paths =
                collect_cleanup_paths(&commands, &finalization, &request.output_path);
            Ok(ConversionPlan::execute_with_cleanup(
                commands,
                cleanup_paths,
                finalization,
            ))
        }
    }
}

/// Build the Stage A selected-vs-emitted diagnostic records using the built-in registry.
///
/// This is an observational planning pass only. It does not alter the command plan
/// returned by [`plan_conversion`] and must not be used as execution authority.
pub fn stage_a_selected_vs_emitted_diagnostics(
    request: &PlanRequest,
) -> Result<Vec<StageASelectedVsEmittedRecord>> {
    stage_a_selected_vs_emitted_diagnostics_with_registry(
        request,
        &ToolRegistry::with_builtin_tools(),
    )
}

/// Build Stage A selected-vs-emitted diagnostic records with an explicit registry.
///
/// Caller-defined target formats intentionally return no typed records because the
/// common typed planner still models only built-in physical candidates. Existing
/// caller-registry semantics therefore remain unchanged.
pub fn stage_a_selected_vs_emitted_diagnostics_with_registry(
    request: &PlanRequest,
    registry: &ToolRegistry,
) -> Result<Vec<StageASelectedVsEmittedRecord>> {
    if selects_reference_dsd_to_pcm(&request.settings, request.source.is_dsd())
        || matches!(request.settings.target_format, AudioFormat::Custom { .. })
    {
        return Ok(Vec::new());
    }

    let topology = plan_topology(request)?;
    let TopologyPlan::Execute { steps, finalization } = topology else {
        return Ok(Vec::new());
    };
    let topology_has_sample_processing = steps.iter().any(|step| {
        matches!(
            &step.operation,
            PlanOperation::DecodeToPcm { .. }
                | PlanOperation::ResamplePcm { .. }
                | PlanOperation::EncodePcm { .. }
                | PlanOperation::EncodeLossy { .. }
                | PlanOperation::PcmToDsd { .. }
                | PlanOperation::DsdToPcm { .. }
                | PlanOperation::DsdRateChange { .. }
        )
    });
    if !topology_has_sample_processing {
        return Ok(Vec::new());
    }

    let typed = match crate::semantic_plan::plan_typed(request) {
        Ok(crate::semantic_plan::PlanningOutcome::Ready(typed)) => {
            crate::semantic_plan::require_current_executor(&typed)?;
            typed
        }
        Ok(crate::semantic_plan::PlanningOutcome::NeedFacts(facts)) => {
            let reason = facts
                .into_iter()
                .map(|fact| format!("{}: {}", fact.key, fact.reason))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(PlanningError::invalid_source(
                "stage_a_selected_vs_emitted",
                format!("diagnostic requires authoritative source facts: {reason}"),
            ));
        }
        Ok(crate::semantic_plan::PlanningOutcome::Refused(refusal)) => {
            return Err(PlanningError::invalid_settings(
                "stage_a_selected_vs_emitted",
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

    let context = request.context();
    let (steps, _) = prune_redundant_metadata_steps(
        &context,
        registry,
        &steps,
        finalization,
    )?;
    let lowered = plan_conversion_with_registry(request, registry)?;
    let commands = lowered.commands();
    stage_a_selected_vs_emitted_records_from_lowered(
        &typed,
        &context,
        registry,
        &steps,
        commands,
    )
}

fn stage_a_is_builtin_tool(tool: &ToolIdentifier) -> bool {
    !matches!(tool, ToolIdentifier::Custom(_))
}

fn stage_a_associated_step_index(
    operation: &PlanOperation,
    steps: &[PlanStep],
    exact_claimed: &mut [bool],
) -> Option<usize> {
    if let Some((index, _)) = steps
        .iter()
        .enumerate()
        .find(|(index, step)| !exact_claimed[*index] && &step.operation == operation)
    {
        exact_claimed[index] = true;
        return Some(index);
    }

    match operation {
        // The retained bridge can fold an ordinary PCM resample into the
        // terminal encoder command. Associate the typed resampler with that
        // physical realization instead of silently dropping the node.
        PlanOperation::ResamplePcm { target_rate_hz, .. } => steps
            .iter()
            .position(|step| match &step.operation {
                PlanOperation::EncodePcm {
                    target_rate_hz: Some(rate),
                    apply_processing: true,
                    ..
                }
                | PlanOperation::EncodeLossy {
                    target_rate_hz: Some(rate),
                    apply_processing: true,
                    ..
                } => rate == target_rate_hz,
                _ => false,
            }),
        // General DSD-to-PCM can still be one retained SoX command even though
        // the common semantic spine exposes reconstruction and terminal encode
        // as distinct typed operations.
        PlanOperation::EncodePcm {
            target_format,
            target_rate_hz: Some(target_rate_hz),
            target_bit_depth,
            ..
        } => steps.iter().position(|step| {
            matches!(
                &step.operation,
                PlanOperation::DsdToPcm {
                    target_format: emitted_format,
                    target_rate_hz: emitted_rate_hz,
                    target_bit_depth: emitted_bit_depth,
                    ..
                } if emitted_format == target_format
                    && emitted_rate_hz == target_rate_hz
                    && emitted_bit_depth == target_bit_depth
            )
        }),
        _ => None,
    }
}

fn stage_a_normalized_command_args(command: &PlannedCommand) -> Vec<String> {
    let input = command
        .input
        .as_path()
        .map(|path| path.to_string_lossy().into_owned());
    let output = command
        .output
        .as_path()
        .map(|path| path.to_string_lossy().into_owned());
    command
        .args
        .iter()
        .map(|arg| {
            if input.as_deref() == Some(arg.as_str()) {
                "<input>".to_owned()
            } else if output.as_deref() == Some(arg.as_str()) {
                "<output>".to_owned()
            } else {
                arg.clone()
            }
        })
        .collect()
}

fn stage_a_ffmpeg_aresample_filter(command: &PlannedCommand) -> Option<String> {
    command
        .args
        .windows(2)
        .filter(|pair| pair[0] == "-af")
        .flat_map(|pair| pair[1].split(','))
        .find(|filter| filter.starts_with("aresample="))
        .map(str::to_owned)
}

fn stage_a_ffmpeg_aresample_field(command: &PlannedCommand, key: &str) -> Option<String> {
    let filter = stage_a_ffmpeg_aresample_filter(command)?;
    filter
        .strip_prefix("aresample=")?
        .split(':')
        .find_map(|field| {
            let (field_key, value) = field.split_once('=')?;
            (field_key == key).then(|| value.to_owned())
        })
}

fn stage_a_last_flag_value(command: &PlannedCommand, flag: &str) -> Option<String> {
    command
        .args
        .windows(2)
        .filter_map(|pair| (pair[0] == flag).then(|| pair[1].clone()))
        .last()
}

fn stage_a_resample_signature(
    emitted_operation: &PlanOperation,
    command: &PlannedCommand,
) -> Vec<String> {
    match &command.tool {
        ToolIdentifier::Ffmpeg => {
            let mut signature = Vec::new();
            if let Some(filter) = stage_a_ffmpeg_aresample_filter(command) {
                signature.push(filter);
            }
            if matches!(emitted_operation, PlanOperation::EncodeLossy { .. }) {
                if let Some(rate) = stage_a_last_flag_value(command, "-ar") {
                    signature.push(format!("encoder_rate={rate}"));
                }
            }
            signature
        }
        ToolIdentifier::Sox => {
            let output_index = command
                .output
                .as_path()
                .map(|path| path.to_string_lossy())
                .and_then(|output| command.args.iter().position(|arg| arg == output.as_ref()));
            let tail = output_index
                .map(|index| &command.args[index + 1..])
                .unwrap_or(command.args.as_slice());
            let start = tail
                .iter()
                .position(|arg| arg == "sinc")
                .or_else(|| tail.iter().position(|arg| arg == "rate"));
            start.map_or_else(Vec::new, |index| tail[index..].to_vec())
        }
        ToolIdentifier::Ssrc => stage_a_normalized_command_args(command),
        _ => stage_a_normalized_command_args(command),
    }
}

fn stage_a_terminal_signature(
    operation: &PlanOperation,
    command: &PlannedCommand,
) -> Vec<String> {
    match &command.tool {
        ToolIdentifier::Ffmpeg => {
            let output_index = command
                .output
                .as_path()
                .map(|path| path.to_string_lossy())
                .and_then(|output| command.args.iter().position(|arg| arg == output.as_ref()))
                .unwrap_or(command.args.len());
            let mut signature = command
                .args
                .iter()
                .position(|arg| arg == "-c:a")
                .map(|start| command.args[start..output_index].to_vec())
                .unwrap_or_default();
            if matches!(operation, PlanOperation::EncodeLossy { .. }) {
                if let Some(rate) = stage_a_last_flag_value(command, "-ar") {
                    signature.push(format!("encoder_rate={rate}"));
                }
            }
            for key in ["out_sample_fmt", "dither_method"] {
                if let Some(value) = stage_a_ffmpeg_aresample_field(command, key) {
                    signature.push(format!("{key}={value}"));
                }
            }
            signature
        }
        ToolIdentifier::Sox => {
            let input_index = command
                .input
                .as_path()
                .map(|path| path.to_string_lossy())
                .and_then(|input| command.args.iter().position(|arg| arg == input.as_ref()));
            let output_index = command
                .output
                .as_path()
                .map(|path| path.to_string_lossy())
                .and_then(|output| command.args.iter().position(|arg| arg == output.as_ref()));
            let mut signature = match (input_index, output_index) {
                (Some(input), Some(output)) if input < output => {
                    command.args[input + 1..output].to_vec()
                }
                _ => Vec::new(),
            };
            if command.args.iter().any(|arg| arg == "-D") {
                signature.push("implicit_dither_disabled".to_owned());
            }
            if let Some(dither) = command.args.iter().position(|arg| arg == "dither") {
                signature.extend(command.args[dither..].iter().cloned());
            }
            signature
        }
        _ => stage_a_normalized_command_args(command),
    }
}

fn stage_a_semantic_parameter_signature(
    operation: &PlanOperation,
    emitted_operation: &PlanOperation,
    command: &PlannedCommand,
) -> Vec<String> {
    match operation {
        PlanOperation::ResamplePcm { .. } => {
            stage_a_resample_signature(emitted_operation, command)
        }
        PlanOperation::EncodePcm { .. } | PlanOperation::EncodeLossy { .. } => {
            stage_a_terminal_signature(operation, command)
        }
        _ => stage_a_normalized_command_args(command),
    }
}

fn stage_a_matching_fixed_terminal_tool<'a>(
    typed: &'a crate::semantic_plan::TypedConversionPlan,
    operation: &PlanOperation,
) -> Option<&'a ToolIdentifier> {
    selected_terminal_realization(typed)
        .ok()
        .flatten()
        .and_then(|realization| match realization {
            crate::semantic_plan::SelectedTerminalRealization::Pcm(realization)
                if matches!(
                    realization.kind,
                    crate::semantic_plan::PcmTerminalRealizationKind::SoxDirect
                        | crate::semantic_plan::PcmTerminalRealizationKind::FfmpegDirect
                ) && matches!(
                    operation,
                    PlanOperation::EncodePcm {
                        target_format,
                        target_rate_hz,
                        target_bit_depth,
                        ..
                    } if target_format == &realization.target_format
                        && target_rate_hz == &realization.target_rate_hz
                        && target_bit_depth == &realization.target_bit_depth
                ) => Some(&realization.selected_tool),
            _ => None,
        })
}

fn stage_a_selected_vs_emitted_records_from_lowered(
    typed: &crate::semantic_plan::TypedConversionPlan,
    context: &PlanContext<'_>,
    registry: &ToolRegistry,
    steps: &[PlanStep],
    commands: &[PlannedCommand],
) -> Result<Vec<StageASelectedVsEmittedRecord>> {
    if steps.len() != commands.len() {
        return Err(PlanningError::invalid_settings(
            "stage_a_selected_vs_emitted",
            "pruned operation list no longer has a one-to-one relationship with emitted commands",
        ));
    }

    let mut exact_claimed = vec![false; steps.len()];
    let mut records = Vec::new();
    for (typed_node_index, node) in typed.nodes.iter().enumerate() {
        let crate::semantic_plan::TypedPlanNode::Operation {
            operation,
            candidates,
            selected_candidate,
            resolved_parameters,
            ..
        } = node
        else {
            continue;
        };
        let Some(candidate) = candidates.get(*selected_candidate) else {
            return Err(PlanningError::invalid_settings(
                "stage_a_selected_vs_emitted",
                format!(
                    "typed operation node {typed_node_index} selected candidate index {selected_candidate} outside its candidate set"
                ),
            ));
        };
        let Some(selected_tool) = candidate.tool.as_ref() else {
            continue;
        };
        if !stage_a_is_builtin_tool(selected_tool) {
            continue;
        }

        let emitted_step_index = stage_a_associated_step_index(
            operation,
            steps,
            &mut exact_claimed,
        )
        .ok_or_else(|| {
            PlanningError::invalid_settings(
                "stage_a_selected_vs_emitted",
                format!(
                    "typed built-in operation node {typed_node_index} ({}) has no emitted realization",
                    operation.label(),
                ),
            )
        })?;
        let step = &steps[emitted_step_index];
        let emitted = &commands[emitted_step_index];
        let selected = registry.build_command_for_tool(context, step, selected_tool)?;
        let selected_parameter_signature = stage_a_semantic_parameter_signature(
            operation,
            &step.operation,
            &selected,
        );
        let emitted_parameter_signature = stage_a_semantic_parameter_signature(
            operation,
            &step.operation,
            emitted,
        );
        let lowering_provenance = if stage_a_matching_fixed_terminal_tool(typed, &step.operation).is_some() {
            StageALoweringProvenance::FixedTool
        } else {
            StageALoweringProvenance::RegistryReselection
        };

        records.push(StageASelectedVsEmittedRecord {
            typed_node_index,
            operation: operation.clone(),
            candidate_identity: candidate.identity.clone(),
            selected_tool: selected_tool.clone(),
            emitted_step_index,
            emitted_operation: step.operation.clone(),
            emitted_tool: emitted.tool.clone(),
            resolved_parameters: resolved_parameters.clone(),
            selected_parameter_signature,
            emitted_parameter_signature,
            lowering_provenance,
        });
    }
    Ok(records)
}

fn selected_terminal_realization(
    typed: &crate::semantic_plan::TypedConversionPlan,
) -> Result<Option<&crate::semantic_plan::SelectedTerminalRealization>> {
    let mut selected = typed.nodes.iter().filter_map(|node| {
        let crate::semantic_plan::TypedPlanNode::Operation {
            candidates,
            selected_candidate,
            ..
        } = node
        else {
            return None;
        };
        candidates
            .get(*selected_candidate)
            .and_then(|candidate| candidate.contract.terminal_realization.as_ref())
    });
    let realization = selected.next();
    if selected.next().is_some() {
        return Err(PlanningError::invalid_settings(
            "terminal_realization",
            "typed plan selected more than one physical terminal realization",
        ));
    }
    Ok(realization)
}

fn command_args_contain_sequence(args: &[String], sequence: &[String]) -> bool {
    !sequence.is_empty()
        && args
            .windows(sequence.len())
            .any(|window| window == sequence)
}

fn command_ffmpeg_dither_method_count(command: &PlannedCommand) -> usize {
    command
        .args
        .iter()
        .map(|arg| arg.match_indices("dither_method=").count())
        .sum()
}

fn command_flag_values<'a>(command: &'a PlannedCommand, flag: &str) -> Vec<&'a str> {
    command
        .args
        .windows(2)
        .filter_map(|pair| (pair[0] == flag).then_some(pair[1].as_str()))
        .collect()
}

fn validate_ssrc_terminal_command(
    command: &PlannedCommand,
    realization: &crate::semantic_plan::SelectedPcmTerminalRealization,
) -> Result<()> {
    if command.tool != ToolIdentifier::Ssrc {
        return Err(PlanningError::invalid_settings(
            "terminal_realization",
            format!(
                "selected SSRC terminal lowered with {} instead of ssrc",
                command.tool,
            ),
        ));
    }
    let bits = command_flag_values(command, "--bits");
    let expected_bits = crate::plugins::ssrc_bits_arg(realization.target_bit_depth);
    if bits.as_slice() != [expected_bits.as_str()] {
        return Err(PlanningError::invalid_settings(
            "terminal_realization",
            format!(
                "selected SSRC terminal expected exactly one --bits {expected_bits}; observed {bits:?}",
            ),
        ));
    }
    let resolved = realization.ssrc_dither.as_ref().ok_or_else(|| {
        PlanningError::invalid_settings(
            "terminal_realization",
            "selected SSRC terminal is missing its structured native dither resolution",
        )
    })?;
    if matches!(
        resolved.availability,
        crate::plugins::SsrcDitherAvailability::UnavailableForSsrcTerminal { .. }
    ) {
        return Err(PlanningError::invalid_settings(
            "terminal_realization",
            "selected SSRC terminal carries an unavailable native dither resolution",
        ));
    }
    let dither_values = command_flag_values(command, "--dither");
    match resolved.dither_id {
        Some(id) => {
            let expected_id = id.to_string();
            if dither_values.len() == 1 && dither_values[0] == expected_id {
                // Exact one native dither id.
            } else {
            return Err(PlanningError::invalid_settings(
                "terminal_realization",
                format!(
                    "selected SSRC terminal expected exactly one --dither {id}; observed {dither_values:?}",
                ),
            ));
            }
        }
        None if dither_values.is_empty() => {}
        None => {
            return Err(PlanningError::invalid_settings(
                "terminal_realization",
                "selected SSRC terminal says native dither is inactive but lowering emits --dither",
            ));
        }
    }
    let pdf_values = command_flag_values(command, "--pdf");
    let expected_pdf = resolved.pdf_type.map(|pdf| match pdf {
        crate::enums::SsrcPdfType::Rectangular => "0",
        crate::enums::SsrcPdfType::Triangular => "1",
    });
    match expected_pdf {
        Some(pdf) if pdf_values.as_slice() == [pdf] => {}
        Some(pdf) => {
            return Err(PlanningError::invalid_settings(
                "terminal_realization",
                format!(
                    "selected SSRC terminal expected exactly one --pdf {pdf}; observed {pdf_values:?}",
                ),
            ));
        }
        None if pdf_values.is_empty() => {}
        None => {
            return Err(PlanningError::invalid_settings(
                "terminal_realization",
                "selected SSRC terminal says no native PDF is active but lowering emits --pdf",
            ));
        }
    }
    Ok(())
}

fn validate_direct_terminal_dither(
    command: &PlannedCommand,
    realization: &crate::semantic_plan::SelectedPcmTerminalRealization,
) -> Result<()> {
    use crate::semantic_plan::PcmTerminalRealizationKind;

    match realization.kind {
        PcmTerminalRealizationKind::SsrcDirectWav => {
            validate_ssrc_terminal_command(command, realization)?;
        }
        PcmTerminalRealizationKind::FfmpegDirect => {
            if command.tool != ToolIdentifier::Ffmpeg {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    format!(
                        "selected FFmpeg terminal lowered with {} instead of ffmpeg",
                        command.tool,
                    ),
                ));
            }
            let emitted = command_ffmpeg_dither_method_count(command);
            match realization.effective_dither {
                Some(dither) => {
                    let method = mapping::soxr_dither_method(dither).ok_or_else(|| {
                        PlanningError::invalid_settings(
                            "terminal_realization",
                            format!(
                                "selected FFmpeg terminal dither {dither:?} has no FFmpeg mapping",
                            ),
                        )
                    })?;
                    let expected = format!("dither_method={method}");
                    if emitted != 1 || !command.args.iter().any(|arg| arg.contains(&expected)) {
                        return Err(PlanningError::invalid_settings(
                            "terminal_realization",
                            format!(
                                "lowered FFmpeg terminal disagrees with selected dither {dither:?}: expected exactly one {expected}, observed {emitted} dither_method option(s)",
                            ),
                        ));
                    }
                }
                None if emitted != 0 => {
                    return Err(PlanningError::invalid_settings(
                        "terminal_realization",
                        "lowered FFmpeg terminal emits dither_method while the selected terminal realization says dither=none",
                    ));
                }
                None => {}
            }
        }
        PcmTerminalRealizationKind::SoxDirect => {
            if command.tool != ToolIdentifier::Sox {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    format!(
                        "selected SoX terminal lowered with {} instead of sox",
                        command.tool,
                    ),
                ));
            }
            match realization.effective_dither {
                Some(dither) => {
                    let expected = mapping::sox_dither_args(dither);
                    if !command_args_contain_sequence(&command.args, &expected) {
                        return Err(PlanningError::invalid_settings(
                            "terminal_realization",
                            format!(
                                "lowered SoX terminal omits selected dither {dither:?}",
                            ),
                        ));
                    }
                }
                None if command.args.iter().any(|arg| arg == "dither") => {
                    return Err(PlanningError::invalid_settings(
                        "terminal_realization",
                        "lowered SoX terminal contains an explicit dither effect while the selected terminal realization says dither=none",
                    ));
                }
                None => {}
            }
        }
        PcmTerminalRealizationKind::SsrcPreterminalFfmpegPackage
        | PcmTerminalRealizationKind::SoxPreterminalFfmpegPackage
        | PcmTerminalRealizationKind::SoxPreterminalWavPackHybrid
        | PcmTerminalRealizationKind::FfmpegPreterminalWavPackHybrid
        | PcmTerminalRealizationKind::NativeWavPackHybridPackage => {
            return Err(PlanningError::invalid_settings(
                "terminal_realization",
                "non-direct terminal realization was passed to the direct-terminal dither validator",
            ));
        }
    }
    Ok(())
}

fn validate_selected_terminal_lowering(
    typed: &crate::semantic_plan::TypedConversionPlan,
    steps: &[PlanStep],
    commands: &[PlannedCommand],
) -> Result<()> {
    use crate::semantic_plan::{
        PcmTerminalRealizationKind, SelectedTerminalRealization,
    };

    let Some(realization) = selected_terminal_realization(typed)? else {
        return Ok(());
    };
    let SelectedTerminalRealization::Pcm(realization) = realization else {
        return Ok(());
    };
    if steps.len() != commands.len() {
        return Err(PlanningError::invalid_settings(
            "terminal_realization",
            "lowered command list no longer has a one-to-one relationship with the pruned operation list",
        ));
    }

    let direct_indices = steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| match &step.operation {
            PlanOperation::EncodePcm {
                target_format,
                target_rate_hz,
                target_bit_depth,
                ..
            } if target_format == &realization.target_format
                && target_rate_hz == &realization.target_rate_hz
                && target_bit_depth == &realization.target_bit_depth => Some(index),
            _ => None,
        })
        .collect::<Vec<_>>();
    let fused_dsd_indices = steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| match &step.operation {
            PlanOperation::DsdToPcm {
                target_format,
                target_rate_hz,
                target_bit_depth,
                ..
            } if target_format == &realization.target_format
                && Some(*target_rate_hz) == realization.target_rate_hz
                && target_bit_depth == &realization.target_bit_depth => Some(index),
            _ => None,
        })
        .collect::<Vec<_>>();
    let ssrc_indices = steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| match &step.operation {
            PlanOperation::ResamplePcm {
                target_rate_hz,
                target_bit_depth: Some(target_bit_depth),
                ..
            } if Some(*target_rate_hz) == realization.target_rate_hz
                && target_bit_depth == &realization.target_bit_depth => Some(index),
            _ => None,
        })
        .collect::<Vec<_>>();

    match realization.kind {
        PcmTerminalRealizationKind::SsrcDirectWav => {
            if realization.target_format != AudioFormat::Wav
                || realization.selected_tool != ToolIdentifier::Ssrc
                || ssrc_indices.len() != 1
            {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    format!(
                        "selected direct SSRC WAV terminal has invalid binding or {} matching resample operations",
                        ssrc_indices.len(),
                    ),
                ));
            }
            if !direct_indices.is_empty() {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    "selected direct SSRC WAV terminal lowered with a second PCM terminal operation",
                ));
            }
            let terminal_index = ssrc_indices[0];
            validate_direct_terminal_dither(&commands[terminal_index], realization)?;
            if steps.iter().skip(terminal_index + 1).any(|step| {
                matches!(
                    step.operation,
                    PlanOperation::ResamplePcm { .. }
                        | PlanOperation::EncodePcm { .. }
                        | PlanOperation::EncodeLossy { .. }
                        | PlanOperation::PcmToDsd { .. }
                        | PlanOperation::DsdRateChange { .. }
                )
            }) {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    "selected direct SSRC terminal is followed by another sample-changing terminal operation",
                ));
            }
            if commands
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != terminal_index)
                .any(|(_, command)| {
                    command_ffmpeg_dither_method_count(command) != 0
                        || command.args.iter().any(|arg| arg == "--dither" || arg == "dither")
                })
            {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    "selected direct SSRC terminal lowered with downstream or duplicate dither",
                ));
            }
        }
        PcmTerminalRealizationKind::SoxDirect | PcmTerminalRealizationKind::FfmpegDirect => {
            if direct_indices.is_empty() && fused_dsd_indices.len() == 1 {
                // The semantic spine names DSD reconstruction and terminal PCM
                // separately, while the retained executor can fuse both into
                // one SoX DsdToPcm command. There is no standalone EncodePcm
                // command whose backend can be bound to the synthetic terminal.
                // Still require the one registered fused physical owner.
                let fused = &commands[fused_dsd_indices[0]];
                if fused.tool != ToolIdentifier::Sox {
                    return Err(PlanningError::invalid_settings(
                        "terminal_realization",
                        format!(
                            "fused DSD-to-PCM terminal lowered with {} instead of sox",
                            fused.tool,
                        ),
                    ));
                }
                return Ok(());
            }
            if direct_indices.len() != 1 {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    format!(
                        "selected direct terminal has {} matching lowered PCM operations instead of one",
                        direct_indices.len(),
                    ),
                ));
            }
            validate_direct_terminal_dither(&commands[direct_indices[0]], realization)?;
        }
        PcmTerminalRealizationKind::SsrcPreterminalFfmpegPackage => {
            return Err(PlanningError::invalid_settings(
                "terminal_realization",
                "SSRC preterminal package-only cells are not admitted in this build",
            ));
        }
        PcmTerminalRealizationKind::SoxPreterminalFfmpegPackage
        | PcmTerminalRealizationKind::SoxPreterminalWavPackHybrid => {
            if direct_indices.len() != 1 {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    format!(
                        "selected compound terminal has {} matching package operations instead of one",
                        direct_indices.len(),
                    ),
                ));
            }
            let package = &commands[direct_indices[0]];
            let expected_package_tool = match realization.kind {
                PcmTerminalRealizationKind::SoxPreterminalFfmpegPackage => ToolIdentifier::Ffmpeg,
                PcmTerminalRealizationKind::SoxPreterminalWavPackHybrid => {
                    ToolIdentifier::Custom("wavpack".to_owned())
                }
                _ => unreachable!("guarded by compound-terminal match"),
            };
            if package.tool != expected_package_tool {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    format!(
                        "selected compound terminal package lowered with {} instead of {}",
                        package.tool, expected_package_tool,
                    ),
                ));
            }
            if command_ffmpeg_dither_method_count(package) != 0 {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    "compound terminal package command emits FFmpeg dither although selected realization assigns dither ownership to the SoX preterminal",
                ));
            }

            let sox_dither_commands = match realization.effective_dither {
                Some(dither) => {
                    let expected = mapping::sox_dither_args(dither);
                    commands
                        .iter()
                        .filter(|command| {
                            command.tool == ToolIdentifier::Sox
                                && command_args_contain_sequence(&command.args, &expected)
                        })
                        .count()
                }
                None => commands
                    .iter()
                    .filter(|command| {
                        command.tool == ToolIdentifier::Sox
                            && command.args.iter().any(|arg| arg == "dither")
                    })
                    .count(),
            };
            match realization.effective_dither {
                Some(dither) if sox_dither_commands != 1 => {
                    return Err(PlanningError::invalid_settings(
                        "terminal_realization",
                        format!(
                            "selected compound terminal dither {dither:?} must be emitted by exactly one SoX preterminal; observed {sox_dither_commands}",
                        ),
                    ));
                }
                None if sox_dither_commands != 0 => {
                    return Err(PlanningError::invalid_settings(
                        "terminal_realization",
                        "lowered compound terminal contains explicit SoX dither while the selected terminal realization says dither=none",
                    ));
                }
                _ => {}
            }
        }
        PcmTerminalRealizationKind::FfmpegPreterminalWavPackHybrid => {
            if direct_indices.len() != 1 {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    format!(
                        "selected FFmpeg-preterminal WavPack hybrid terminal has {} matching package operations instead of one",
                        direct_indices.len(),
                    ),
                ));
            }
            let package_index = direct_indices[0];
            let package = &commands[package_index];
            if package.tool != ToolIdentifier::Custom("wavpack".to_owned()) {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    format!(
                        "selected FFmpeg-preterminal WavPack hybrid package lowered with {} instead of wavpack",
                        package.tool,
                    ),
                ));
            }
            if realization.effective_dither != Some(DitherType::Tpdf)
                || realization.dither_owner
                    != crate::semantic_plan::PcmTerminalDitherOwner::FfmpegPreterminal
            {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    "FFmpeg-preterminal WavPack hybrid realization must assign exactly TPDF to the FFmpeg preterminal",
                ));
            }
            let expected = mapping::soxr_dither_method(DitherType::Tpdf)
                .expect("TPDF has a canonical FFmpeg/SoXR mapping");
            let expected = format!("dither_method={expected}");
            let ffmpeg_preterminals = commands
                .iter()
                .enumerate()
                .filter(|(index, command)| {
                    *index != package_index
                        && command.tool == ToolIdentifier::Ffmpeg
                        && command_ffmpeg_dither_method_count(command) == 1
                        && command.args.iter().any(|arg| arg.contains(&expected))
                })
                .collect::<Vec<_>>();
            if ffmpeg_preterminals.len() != 1 {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    format!(
                        "selected FFmpeg-preterminal WavPack hybrid terminal requires exactly one FFmpeg triangular-dither preterminal; observed {}",
                        ffmpeg_preterminals.len(),
                    ),
                ));
            }
            let (preterminal_index, preterminal) = ffmpeg_preterminals[0];
            if preterminal_index >= package_index {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    "FFmpeg WavPack-hybrid preterminal must precede native packaging",
                ));
            }
            if !matches!(
                &steps[preterminal_index].operation,
                PlanOperation::EncodePcm {
                    target_format: AudioFormat::Wav,
                    target_bit_depth: PcmBitDepth::Int32,
                    apply_processing: true,
                    ..
                }
            ) {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    "qualified FFmpeg WavPack-hybrid dither owner is not the expected processing WAV/Int32 preterminal step",
                ));
            }
            if preterminal.output.as_path() != package.input.as_path() {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    "native WavPack hybrid package is not consuming the FFmpeg preterminal artifact",
                ));
            }
            if commands.iter().enumerate().any(|(index, command)| {
                index != preterminal_index
                    && index != package_index
                    && (command_ffmpeg_dither_method_count(command) != 0
                        || command.args.iter().any(|arg| arg == "dither" || arg == "--dither"))
            }) {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    "FFmpeg-preterminal WavPack hybrid lowering contains duplicate downstream dither ownership",
                ));
            }
        }
        PcmTerminalRealizationKind::NativeWavPackHybridPackage => {
            if direct_indices.len() != 1 {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    format!(
                        "selected native WavPack hybrid terminal has {} matching package operations instead of one",
                        direct_indices.len(),
                    ),
                ));
            }
            let package = &commands[direct_indices[0]];
            if package.tool != ToolIdentifier::Custom("wavpack".to_owned()) {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    format!(
                        "selected native WavPack hybrid terminal lowered with {} instead of wavpack",
                        package.tool,
                    ),
                ));
            }
            if realization.effective_dither.is_some()
                || command_ffmpeg_dither_method_count(package) != 0
            {
                return Err(PlanningError::invalid_settings(
                    "terminal_realization",
                    "native WavPack hybrid package unexpectedly carries terminal dither",
                ));
            }
        }
    }

    Ok(())
}

fn prune_redundant_metadata_steps(
    context: &PlanContext<'_>,
    registry: &ToolRegistry,
    steps: &[PlanStep],
    finalization: Option<Finalization>,
) -> Result<(Vec<PlanStep>, Option<Finalization>)> {
    let mut pruned = steps.to_vec();
    let mut adjusted_finalization = finalization;
    let mut original_source_metadata_by_path: BTreeMap<PathBuf, MetadataPlanEffect> = BTreeMap::new();
    let mut index = 0;

    while index < pruned.len() {
        if let Some(required) = metadata_transfer_required_effect(&pruned[index].operation) {
            // A MetadataTransfer with both policy flags false is a STRIP:
            // its purpose is the rewrite itself (-map_metadata -1), so an
            // empty requirement must never count as vacuously satisfied —
            // pruning it would publish the source with tags intact and
            // redirect finalization to rename the plan input.
            let is_strip = !required.source_tags_transferred_from_original_source
                && !required.artwork_transferred_from_original_source;
            if is_strip {
                index += 1;
                continue;
            }
            let Some(input_path) = pruned[index]
                .input
                .as_path()
                .map(std::path::Path::to_path_buf)
            else {
                index += 1;
                continue;
            };
            let available = original_source_metadata_by_path
                .get(&input_path)
                .copied()
                .unwrap_or_else(MetadataPlanEffect::none);

            if !metadata_effect_satisfies_original_source_transfer(available, required) {
                let effect = registry.metadata_effect_for_step(context, &pruned[index])?;
                record_original_source_metadata_effect(
                    &mut original_source_metadata_by_path,
                    &pruned[index],
                    effect,
                );
                index += 1;
                continue;
            }

            let Some(from_path) = pruned[index]
                .output
                .as_path()
                .map(std::path::Path::to_path_buf)
            else {
                index += 1;
                continue;
            };
            let to_path = input_path;

            pruned.remove(index);
            for later in &mut pruned[index..] {
                replace_input_path(&mut later.input, &from_path, &to_path);
                replace_output_path(&mut later.output, &from_path, &to_path);
            }
            if let Some(Finalization::AtomicRename { from, .. }) = &mut adjusted_finalization {
                if from == &from_path {
                    *from = to_path;
                }
            }
            continue;
        }

        let effect = registry.metadata_effect_for_step(context, &pruned[index])?;
        record_original_source_metadata_effect(
            &mut original_source_metadata_by_path,
            &pruned[index],
            effect,
        );
        index += 1;
    }

    Ok((pruned, adjusted_finalization))
}

fn metadata_transfer_required_effect(operation: &PlanOperation) -> Option<MetadataPlanEffect> {
    match operation {
        PlanOperation::MetadataTransfer {
            transfer_tags,
            preserve_artwork,
            ..
        } => Some(MetadataPlanEffect {
            source_tags_transferred_from_original_source: *transfer_tags,
            artwork_transferred_from_original_source: *preserve_artwork,
            ..MetadataPlanEffect::none()
        }),
        _ => None,
    }
}

fn metadata_effect_satisfies_original_source_transfer(
    available: MetadataPlanEffect,
    required: MetadataPlanEffect,
) -> bool {
    (!required.source_tags_transferred_from_original_source
        || available.source_tags_transferred_from_original_source)
        && (!required.artwork_transferred_from_original_source
            || available.artwork_transferred_from_original_source)
}

fn record_original_source_metadata_effect(
    by_path: &mut BTreeMap<PathBuf, MetadataPlanEffect>,
    step: &PlanStep,
    effect: MetadataPlanEffect,
) {
    let Some(output_path) = step.output.as_path().map(std::path::Path::to_path_buf) else {
        return;
    };

    let input_state = step
        .input
        .as_path()
        .and_then(|path| by_path.get(path).copied())
        .unwrap_or_else(MetadataPlanEffect::none);

    let mut output_state = MetadataPlanEffect::none();
    if effect.source_tags_transferred_from_original_source {
        output_state.source_tags_transferred_from_original_source = true;
    }
    if effect.artwork_transferred_from_original_source {
        output_state.artwork_transferred_from_original_source = true;
    }
    if effect.tags_preserved_from_command_input {
        output_state.source_tags_transferred_from_original_source |=
            input_state.source_tags_transferred_from_original_source;
    }
    if effect.artwork_preserved_from_command_input {
        output_state.artwork_transferred_from_original_source |=
            input_state.artwork_transferred_from_original_source;
    }

    if step.input.as_path() == Some(output_path.as_path())
        || matches!(step.output, OutputSink::InPlace(_))
    {
        output_state = output_state.merge(input_state);
    }

    by_path.insert(output_path, output_state);
}

fn replace_input_path(input: &mut InputSource, from: &std::path::Path, to: &std::path::Path) {
    if let InputSource::Path(path) = input {
        if path.as_path() == from {
            *path = to.to_path_buf();
        }
    }
}

fn replace_output_path(output: &mut OutputSink, from: &std::path::Path, to: &std::path::Path) {
    match output {
        OutputSink::Path(path) | OutputSink::InPlace(path) => {
            if path.as_path() == from {
                *path = to.to_path_buf();
            }
        }
        OutputSink::Stdout => {}
    }
}

fn collect_cleanup_paths(
    commands: &[PlannedCommand],
    finalization: &Option<Finalization>,
    requested_output: &std::path::Path,
) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    for command in commands {
        if let Some(path) = command.output.as_path() {
            if path != requested_output {
                paths.insert(path.to_path_buf());
            }
        }
    }
    if let Some(Finalization::AtomicRename { from, to }) = finalization {
        if from != to && from.as_path() != requested_output {
            paths.insert(from.clone());
        }
    }
    paths.into_iter().collect()
}


fn default_container_extension_for_format(format: &AudioFormat) -> &str {
    match format {
        AudioFormat::Aac | AudioFormat::Alac => "m4a",
        _ => format.extension(),
    }
}

fn validate_requested_container_extension(request: &PlanRequest) -> Result<()> {
    let extension = request
        .output_path
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.trim().is_empty())
        .map(|extension| extension.to_ascii_lowercase());

    match &request.settings.target_format {
        AudioFormat::Aac => match extension.as_deref() {
            None | Some("m4a" | "m4b" | "mp4") => Ok(()),
            Some("aac") => Err(PlanningError::invalid_settings(
                "output_path",
                "AAC output is muxed as MP4-family M4A/M4B/MP4 by this pipeline; raw .aac output is not implemented, so use .m4a/.m4b/.mp4 or add an explicit raw-AAC mode",
            )),
            Some(_) => Err(PlanningError::invalid_settings(
                "output_path",
                "AAC output must use an .m4a, .m4b, or .mp4 container extension unless an explicit raw-AAC mode is implemented",
            )),
        },
        AudioFormat::Alac => match extension.as_deref() {
            None | Some("m4a" | "mp4") => Ok(()),
            Some(_) => Err(PlanningError::invalid_settings(
                "output_path",
                "ALAC output must use an .m4a or .mp4 container extension",
            )),
        },
        _ => Ok(()),
    }
}

fn validate_request_paths(request: &PlanRequest) -> Result<()> {
    if request.input_path.as_os_str().is_empty() {
        return Err(PlanningError::invalid_settings(
            "input_path",
            "input path cannot be empty",
        ));
    }
    if request.output_path.as_os_str().is_empty() {
        return Err(PlanningError::invalid_settings(
            "output_path",
            "output path cannot be empty",
        ));
    }
    if request.input_path == request.output_path {
        return Err(PlanningError::invalid_settings(
            "output_path",
            "input and output paths must differ; callers that want replacement should execute the work-file plan and atomically rename after success",
        ));
    }
    validate_requested_container_extension(request)?;
    if let Some(work_dir) = &request.intermediate_dir {
        if work_dir.as_os_str().is_empty() {
            return Err(PlanningError::invalid_settings(
                "intermediate_dir",
                "intermediate directory cannot be an empty path",
            ));
        }
    }
    Ok(())
}

fn validate_atomic_work_path(request: &PlanRequest, work_path: &std::path::Path) -> Result<()> {
    if work_path == request.input_path.as_path() {
        return Err(PlanningError::invalid_settings(
            "intermediate_dir/output_path",
            "deterministic work path would overwrite the input path",
        ));
    }
    if work_path == request.output_path.as_path() {
        return Err(PlanningError::invalid_settings(
            "output_path",
            "deterministic work path must differ from the requested output path",
        ));
    }
    Ok(())
}

fn validate_step_paths(
    request: &PlanRequest,
    steps: &[PlanStep],
    finalization: &Option<Finalization>,
) -> Result<()> {
    for step in steps {
        if let Some(path) = step.output.as_path() {
            validate_atomic_work_path(request, path)?;
        }
        if matches!(step.output, OutputSink::InPlace(_)) {
            if let Some(path) = step.output.as_path() {
                if path == request.input_path.as_path() || path == request.output_path.as_path() {
                    return Err(PlanningError::invalid_settings(
                        "output_path",
                        "in-place post-processing may only target deterministic work files",
                    ));
                }
            }
        }
    }
    if let Some(Finalization::AtomicRename { from, to }) = finalization {
        validate_atomic_work_path(request, from)?;
        if to != &request.output_path {
            return Err(PlanningError::invalid_settings(
                "finalization",
                "atomic finalization target must be the requested output path",
            ));
        }
    }
    Ok(())
}

/// Validate semantics that apply when the request forces the SSRC backend.
pub(crate) fn validate_forced_ssrc_semantics(request: &PlanRequest) -> Result<()> {
    if request.settings.ssrc.force
        && (request.source.is_dsd()
            || request.settings.target_format.is_dsd()
            || rate_change_for_pcm(request)?.is_none())
    {
        return Err(PlanningError::invalid_settings(
            "ssrc.force",
            "forced SSRC requires a PCM source, a PCM target, and an actual PCM sample-rate change",
        ));
    }
    Ok(())
}

fn validate_request_semantics(request: &PlanRequest) -> Result<()> {
    validate_forced_ssrc_semantics(request)?;
    if request.source.is_dsd()
        && request.settings.dsd.general_from_dsd.lowpass == DsdLowpassMethod::Sinc
        && request.settings.dsd.general_from_dsd.sinc.allow_aliasing
    {
        return Err(PlanningError::invalid_settings(
            "dsd.general_from_dsd.sinc.allow_aliasing",
            "general DSD-to-PCM sinc alias permission is not supported by the retained SoX lowerer",
        ));
    }
    Ok(())
}

fn validate_post_processing_inputs(request: &PlanRequest) -> Result<()> {
    if request.settings.metadata.store_source_audio_md5 && request.source.audio_md5.is_none() {
        return Err(PlanningError::invalid_source(
            "audio_md5",
            "metadata.store_source_audio_md5 requires SourceInfo::audio_md5",
        ));
    }
    Ok(())
}

fn is_passthrough(request: &PlanRequest) -> bool {
    audio_content_matches_requested(request)
        && metadata_passthrough_safe(&request.settings)
        && !requires_post_processing(request)
}

fn metadata_passthrough_safe(settings: &PipelineSettings) -> bool {
    settings.metadata.transfer_tags && settings.metadata.preserve_artwork
}

fn requires_post_processing(request: &PlanRequest) -> bool {
    request.settings.metadata.store_source_audio_md5
        || request.settings.verification.verify_after_encode
        || flac_verify_requested(request)
        || request.settings.replay_gain.mode.is_some()
}

fn flac_verify_requested(request: &PlanRequest) -> bool {
    request.settings.target_format == AudioFormat::Flac && request.settings.flac.verify
}

fn conversion_is_stream_copy_only(request: &PlanRequest) -> bool {
    audio_content_matches_requested(request)
        && (!metadata_passthrough_safe(&request.settings) || requires_post_processing(request))
}

fn audio_content_matches_requested(request: &PlanRequest) -> bool {
    let settings = &request.settings;
    if settings.force_encode {
        return false;
    }
    // Explicit FFmpeg Int32 dither is a real terminal transformation even
    // when source and target PCM lattices already match. Resolve Source to
    // the authoritative PCM depth so passthrough cannot swallow that request.
    let requested_pcm_depth = match settings.target_bit_depth {
        BitDepthTarget::Source => request.source.authoritative_pcm_depth().map(|source_depth| {
            crate::settings::source_pcm_depth_for_target(
                &settings.target_format,
                settings.wavpack.hybrid,
                source_depth,
            )
        }),
        BitDepthTarget::Pcm(depth) => Some(depth),
    };
    if settings.target_format.is_pcm_lossless()
        && crate::plugins::int32_dither_requested(request, requested_pcm_depth)
    {
        return false;
    }
    if settings.dither_type != DitherType::None && !requested_depth_matches_source(request) {
        return false;
    }
    if request.source.format != settings.target_format {
        return false;
    }
    if !source_codec_matches_target(request) {
        return false;
    }
    if !encoder_settings_allow_stream_copy(settings) {
        return false;
    }
    if !requested_rate_matches_source(request) || !requested_depth_matches_source(request) {
        return false;
    }
    true
}

fn requested_rate_matches_source(request: &PlanRequest) -> bool {
    match request.settings.target_sample_rate {
        RateTarget::Source => true,
        RateTarget::PcmHz(hz) => request.source.sample_rate_hz == Some(hz),
        RateTarget::Dsd(rate) => request.source.dsd_rate() == Some(rate),
    }
}

fn requested_depth_matches_source(request: &PlanRequest) -> bool {
    match request.settings.target_bit_depth {
        BitDepthTarget::Source
            if request.settings.target_format.is_pcm_lossless()
                && request.source.representation_kind() == SourceRepresentationKind::Pcm =>
        {
            let Some(source_depth) = request.source.authoritative_pcm_depth() else {
                return false;
            };
            let target_depth = crate::settings::source_pcm_depth_for_target(
                &request.settings.target_format,
                request.settings.wavpack.hybrid,
                source_depth,
            );
            // Preserve established Source passthrough for representable source
            // depths. Only the new format-safe remap (for example WavPack
            // Float32 -> Int32) makes Source itself a content change.
            target_depth == source_depth || request.source.bit_depth == Some(target_depth)
        }
        BitDepthTarget::Source => true,
        BitDepthTarget::Pcm(depth) => request.source.bit_depth == Some(depth),
    }
}

fn source_codec_matches_target(request: &PlanRequest) -> bool {
    let source = &request.source;
    match &request.settings.target_format {
        AudioFormat::Flac => source.codec == AudioCodec::Flac,
        AudioFormat::Wav | AudioFormat::Aiff => {
            matches!(
                source.codec,
                AudioCodec::PcmSigned | AudioCodec::PcmUnsigned | AudioCodec::PcmFloat
            ) && matches!(
                source.sample_kind,
                None | Some(
                    SampleKind::SignedInteger | SampleKind::UnsignedInteger | SampleKind::Float
                )
            )
        }
        AudioFormat::WavPack => source.codec == AudioCodec::WavPack,
        AudioFormat::Alac => source.codec == AudioCodec::Alac,
        AudioFormat::Dsf | AudioFormat::Dff => source.is_dsd(),
        // Lossy streams have user-controlled rate-control settings that SourceInfo
        // cannot currently prove equal to the requested target. Re-encode rather
        // than silently preserving an unwanted bitrate, profile, or quality.
        AudioFormat::Mp3 | AudioFormat::Aac | AudioFormat::Opus | AudioFormat::Dts | AudioFormat::Ac3 => false,
        // A caller-defined plugin owns the meaning of equality for custom formats;
        // the built-in planner will ask the plugin to encode instead of copying.
        AudioFormat::Custom { .. } => false,
    }
}

fn encoder_settings_allow_stream_copy(settings: &PipelineSettings) -> bool {
    match &settings.target_format {
        AudioFormat::Flac => {
            settings.flac.compression_level == FlacSettings::default().compression_level
        }
        AudioFormat::WavPack => settings.wavpack == WavPackSettings::default(),
        AudioFormat::Wav
        | AudioFormat::Aiff
        | AudioFormat::Alac
        | AudioFormat::Dsf
        | AudioFormat::Dff => true,
        AudioFormat::Mp3 | AudioFormat::Aac | AudioFormat::Opus | AudioFormat::Dts | AudioFormat::Ac3 | AudioFormat::Custom { .. } => {
            false
        }
    }
}

fn plan_to_dsd(
    request: &PlanRequest,
    context: &PlanContext<'_>,
    steps: &mut Vec<PlanStep>,
    current_input: &mut InputSource,
    final_work: PathBuf,
) -> Result<()> {
    let target_rate = resolve_target_dsd_rate(request)?;
    let output = OutputSink::Path(final_work);
    let operation = if request.source.is_dsd() {
        PlanOperation::DsdRateChange {
            target_format: request.settings.target_format.clone(),
            target_rate,
            lowpass: request.settings.dsd.general_from_dsd.lowpass,
        }
    } else {
        PlanOperation::PcmToDsd {
            target_format: request.settings.target_format.clone(),
            target_rate,
            filter: request.settings.dsd.pcm_to_dsd.filter,
        }
    };
    push_step(
        steps,
        operation,
        current_input.clone(),
        output,
        "Create DSD output",
    );
    *current_input = InputSource::Path(context.final_work_path());
    Ok(())
}

fn plan_from_dsd(
    request: &PlanRequest,
    context: &PlanContext<'_>,
    steps: &mut Vec<PlanStep>,
    current_input: &mut InputSource,
    final_work: PathBuf,
) -> Result<()> {
    if request.settings.pcm_true_peak.fixed_gain_db().is_some() {
        return Err(PlanningError::invalid_settings(
            "pcm_true_peak.policy.gain_db",
            "PCM fixed gain is a PCM-source control; use the DSD Fixed gain policy for DSD-to-PCM conversion",
        ));
    }
    let requested_target_rate_hz = match request.settings.target_sample_rate {
        RateTarget::PcmHz(hz) => hz,
        RateTarget::Source => request
            .source
            .dsd_rate()
            .map(DsdRate::default_pcm_target_hz)
            .ok_or_else(|| {
                PlanningError::invalid_source(
                    "sample_rate_hz",
                    "DSD to PCM needs source DSD rate or explicit PCM target rate",
                )
            })?,
        RateTarget::Dsd(_) => {
            return Err(PlanningError::invalid_settings(
                "target_sample_rate",
                "PCM targets cannot use RateTarget::Dsd",
            ));
        }
    };
    let target_rate_hz = if request.settings.target_format.is_lossy() {
        resolve_lossy_encoder_target_rate(request, requested_target_rate_hz)?
    } else {
        requested_target_rate_hz
    };
    let target_depth = resolve_target_bit_depth(request)?;
    reject_unsupported_resolved_depth(
        &request.settings.target_format,
        request.settings.wavpack.hybrid,
        target_depth,
    )?;

    // Combos SoX silently substitutes (FLAC Int32; AIFF/WavPack float)
    // must NOT take the direct SoX DsdToPcm-to-final branch: route them
    // through the WAV intermediate so the final EncodePcm step carries the
    // per-tool eligibility and ffmpeg encoder flags (D1/D4).
    let sox_silently_substitutes = matches!(
        (&request.settings.target_format, target_depth),
        (AudioFormat::Flac, PcmBitDepth::Int32)
            | (AudioFormat::Aiff, PcmBitDepth::Float32 | PcmBitDepth::Float64)
            | (AudioFormat::WavPack, PcmBitDepth::Float32 | PcmBitDepth::Float64)
    );
    if request.settings.target_format.is_pcm_lossless()
        && request.settings.target_format.sox_encodable()
        && !sox_silently_substitutes
    {
        push_step(
            steps,
            PlanOperation::DsdToPcm {
                target_format: request.settings.target_format.clone(),
                target_rate_hz,
                target_bit_depth: target_depth,
                lowpass: request.settings.dsd.general_from_dsd.lowpass,
            },
            current_input.clone(),
            OutputSink::Path(final_work.clone()),
            "Convert DSD to PCM output",
        );
        *current_input = InputSource::Path(final_work);
        return Ok(());
    }

    let pcm_intermediate = context.intermediate_path(steps.len(), "wav");
    push_step(
        steps,
        PlanOperation::DsdToPcm {
            target_format: AudioFormat::Wav,
            target_rate_hz,
            target_bit_depth: target_depth,
            lowpass: request.settings.dsd.general_from_dsd.lowpass,
        },
        current_input.clone(),
        OutputSink::Path(pcm_intermediate.clone()),
        "Convert DSD to PCM intermediate",
    );
    *current_input = InputSource::Path(pcm_intermediate);
    push_encode_final(
        request,
        steps,
        current_input,
        final_work,
        request
            .settings
            .target_format
            .is_lossy()
            .then_some(target_rate_hz),
        target_depth,
        false,
    )?;
    Ok(())
}

fn plan_from_pcm(
    request: &PlanRequest,
    context: &PlanContext<'_>,
    steps: &mut Vec<PlanStep>,
    current_input: &mut InputSource,
    final_work: PathBuf,
) -> Result<()> {
    let lossy_encoder_rate = resolved_lossy_encoder_target_rate(request)?;
    let processing_rate = rate_change_for_pcm(request)?;
    if request.settings.dsd.runtime_album_gain_db().is_some() {
        if request.source.representation_kind() != SourceRepresentationKind::Dsd {
            return Err(PlanningError::invalid_source(
                "source_representation",
                "runtime DSD album gain may be applied only to a retained carrier whose original source representation is DSD",
            ));
        }
        if processing_rate.is_some() {
            return Err(PlanningError::invalid_source(
                "sample_rate_hz",
                "runtime DSD album gain carrier rate must already equal the requested final PCM rate",
            ));
        }
        if request.settings.target_format.is_lossy() {
            let carrier_rate_hz = request.source.sample_rate_hz.ok_or_else(|| {
                PlanningError::invalid_source(
                    "sample_rate_hz",
                    "runtime DSD album gain lossy carrier requires an authoritative PCM sample rate",
                )
            })?;
            if mapping::ffmpeg_lossy_encoder_accepts_rate_directly(
                &request.settings.target_format,
                carrier_rate_hz,
            ) != Some(true)
            {
                return Err(PlanningError::invalid_settings(
                    "target_sample_rate",
                    format!(
                        "runtime DSD album gain requires {} to accept {} Hz directly; FFmpeg rate conversion after the proved gain is forbidden",
                        request.settings.target_format,
                        carrier_rate_hz,
                    ),
                ));
            }
        }
    }
    let target_depth = resolve_target_bit_depth(request)?;
    reject_unsupported_resolved_depth(
        &request.settings.target_format,
        request.settings.wavpack.hybrid,
        target_depth,
    )?;

    let wavpack_hybrid = request.settings.target_format == AudioFormat::WavPack
        && request.settings.wavpack.hybrid;
    let hybrid_float_source_integer_landing = wavpack_hybrid
        && request.settings.target_bit_depth == BitDepthTarget::Source
        && request
            .source
            .authoritative_pcm_depth()
            .is_some_and(PcmBitDepth::is_float)
        && target_depth == PcmBitDepth::Int32;
    let pcm_gain_wavpack_hybrid = (request.settings.pcm_true_peak.is_true_peak()
        || request.settings.pcm_true_peak.fixed_gain_db().is_some())
        && wavpack_hybrid;
    if pcm_gain_wavpack_hybrid || hybrid_float_source_integer_landing {
        if request.settings.pcm_true_peak.is_true_peak() && processing_rate.is_some() {
            return Err(PlanningError::invalid_source(
                "sample_rate_hz",
                "PCM true-peak WavPack hybrid carrier must already be at the final sample rate; post-measurement resampling is forbidden",
            ));
        }
        let encoder_input = context.intermediate_path(steps.len(), "wav");
        push_step(
            steps,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Wav,
                // Automatic true-peak gain has already proved that no rate
                // change remains. Fixed gain and ordinary float-Source hybrid
                // conversion may share this single encoder-input realization
                // step with an ordinary requested resample.
                target_rate_hz: processing_rate,
                target_bit_depth: target_depth,
                apply_processing: true,
            },
            current_input.clone(),
            OutputSink::Path(encoder_input.clone()),
            "Realize final PCM for WavPack hybrid encoder input",
        );
        *current_input = InputSource::Path(encoder_input);
        push_encode_final(
            request,
            steps,
            current_input,
            final_work,
            None,
            target_depth,
            false,
        )?;
        return Ok(());
    }
    let depth_change = match request.settings.target_bit_depth {
        BitDepthTarget::Source => request
            .source
            .authoritative_pcm_depth()
            .is_some_and(|source_depth| source_depth != target_depth),
        BitDepthTarget::Pcm(_) => request.source.bit_depth != Some(target_depth),
    };
    let effective_dither = crate::plugins::effective_pcm_dither(request, Some(target_depth));
    let int32_dither = request.settings.target_format.is_pcm_lossless()
        && crate::plugins::int32_dither_requested(request, Some(target_depth));
    let needs_processing = processing_rate.is_some()
        || depth_change
        || int32_dither
        || request.settings.dsd.runtime_album_gain_db().is_some()
        || request.settings.pcm_true_peak.fixed_gain_db().is_some();
    let needs_ssrc = processing_rate.is_some()
        && (request.settings.nyquist_transition == NyquistTransition::BrickWall
            || request.settings.ssrc.force);

    if needs_ssrc {
        let Some(ssrc_target_rate_hz) = processing_rate else {
            return Err(PlanningError::invalid_settings(
                "target_sample_rate",
                "SSRC processing requires an explicit PCM rate change",
            ));
        };
        let decode_path = context.intermediate_path(steps.len(), "wav");
        push_step(
            steps,
            PlanOperation::DecodeToPcm {
                bit_depth: PcmBitDepth::Float64,
            },
            current_input.clone(),
            OutputSink::Path(decode_path.clone()),
            "Decode to PCM for SSRC",
        );
        *current_input = InputSource::Path(decode_path);

        let profile =
            mapping::ssrc_profile(request.settings.ssrc, request.settings.resample_quality);
        // Resolve the same immediate representation the typed semantic planner
        // uses. A direct-WAV terminal lets SSRC own the final integer/float
        // write; any later sample work or split-only cell keeps SSRC Float64.
        let immediate = crate::semantic_plan::resolve_ssrc_immediate_output(
            request,
            ssrc_target_rate_hz,
            Some(target_depth),
            false,
            request.settings.pcm_true_peak.policy,
        )
        .map_err(|refusal| {
            PlanningError::invalid_settings(
                "ssrc_terminal",
                format!("{}: {}", refusal.code, refusal.reason),
            )
        })?;
        let ssrc_path = if immediate.role == crate::semantic_plan::SsrcOutputRole::Terminal {
            final_work.clone()
        } else {
            context.intermediate_path(steps.len(), "wav")
        };
        push_step(
            steps,
            PlanOperation::ResamplePcm {
                target_rate_hz: ssrc_target_rate_hz,
                target_bit_depth: Some(immediate.depth),
                profile: Some(profile),
                brick_wall: true,
            },
            current_input.clone(),
            OutputSink::Path(ssrc_path.clone()),
            "Brick-wall PCM resampling with SSRC",
        );
        *current_input = InputSource::Path(ssrc_path);
        if immediate.role == crate::semantic_plan::SsrcOutputRole::Terminal {
            return Ok(());
        }
        push_encode_final(
            request,
            steps,
            current_input,
            final_work,
            lossy_encoder_rate,
            target_depth,
            request.settings.pcm_true_peak.fixed_gain_db().is_some()
                || int32_dither
                || effective_dither != DitherType::None,
        )?;
        return Ok(());
    }

    let hard_ceiling_needs_proved_dither_terminal = (request
        .settings
        .dsd
        .runtime_album_gain_db()
        .is_some()
        || request.settings.pcm_true_peak.is_true_peak())
        && request.settings.target_format.is_pcm_lossless()
        && effective_dither != DitherType::None
        && matches!(
            target_depth,
            PcmBitDepth::Int8 | PcmBitDepth::Int16 | PcmBitDepth::Int24
        );
    let needs_sox_preprocess = effective_dither != DitherType::None
        && (mapping::requires_sox_dither(effective_dither)
            || hard_ceiling_needs_proved_dither_terminal)
        && !request.settings.target_format.sox_encodable()
        && request.settings.target_format.ffmpeg_encodable();

    if needs_sox_preprocess {
        let preprocessed = context.intermediate_path(steps.len(), "wav");
        push_step(
            steps,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Wav,
                target_rate_hz: processing_rate,
                target_bit_depth: target_depth,
                apply_processing: true,
            },
            current_input.clone(),
            OutputSink::Path(preprocessed.clone()),
            "Apply SoX-only PCM processing before final encode",
        );
        *current_input = InputSource::Path(preprocessed);
        push_encode_final(
            request,
            steps,
            current_input,
            final_work,
            lossy_encoder_rate,
            target_depth,
            false,
        )?;
        return Ok(());
    }

    push_encode_final(
        request,
        steps,
        current_input,
        final_work,
        lossy_encoder_rate.or(processing_rate),
        target_depth,
        needs_processing,
    )
}

fn push_encode_final(
    request: &PlanRequest,
    steps: &mut Vec<PlanStep>,
    current_input: &mut InputSource,
    final_work: PathBuf,
    target_rate_hz: Option<u32>,
    target_depth: PcmBitDepth,
    apply_processing: bool,
) -> Result<()> {
    if request.settings.target_format.is_lossy() {
        let target_rate_hz = if request.settings.dsd.runtime_album_gain_db().is_some() {
            let carrier_rate_hz = request.source.sample_rate_hz.ok_or_else(|| {
                PlanningError::invalid_source(
                    "sample_rate_hz",
                    "runtime DSD album gain lossy encode requires an authoritative carrier sample rate",
                )
            })?;
            if mapping::ffmpeg_lossy_encoder_accepts_rate_directly(
                &request.settings.target_format,
                carrier_rate_hz,
            ) != Some(true)
            {
                return Err(PlanningError::invalid_settings(
                    "target_sample_rate",
                    format!(
                        "runtime DSD album gain requires {} to accept {} Hz directly; FFmpeg rate conversion after the proved gain is forbidden",
                        request.settings.target_format,
                        carrier_rate_hz,
                    ),
                ));
            }
            if let Some(planned_rate_hz) = target_rate_hz {
                if planned_rate_hz != carrier_rate_hz {
                    return Err(PlanningError::invalid_source(
                        "sample_rate_hz",
                        "runtime DSD album gain lossy encode rate must equal the measured carrier rate",
                    ));
                }
            }
            Some(carrier_rate_hz)
        } else {
            target_rate_hz
        };
        push_step(
            steps,
            PlanOperation::EncodeLossy {
                target_format: request.settings.target_format.clone(),
                target_rate_hz,
                apply_processing,
            },
            current_input.clone(),
            OutputSink::Path(final_work.clone()),
            "Encode lossy output",
        );
    } else if request.settings.target_format.is_pcm_lossless() {
        let description = if request.settings.target_format == AudioFormat::Flac
            && target_depth == PcmBitDepth::Int32
        {
            "Encode true 32-bit FLAC with FFmpeg experimental encoder"
        } else {
            "Encode PCM output"
        };
        push_step(
            steps,
            PlanOperation::EncodePcm {
                target_format: request.settings.target_format.clone(),
                target_rate_hz,
                target_bit_depth: target_depth,
                apply_processing,
            },
            current_input.clone(),
            OutputSink::Path(final_work.clone()),
            description,
        );
    } else if matches!(request.settings.target_format, AudioFormat::Custom { .. }) {
        push_step(
            steps,
            PlanOperation::EncodePcm {
                target_format: request.settings.target_format.clone(),
                target_rate_hz,
                target_bit_depth: target_depth,
                apply_processing,
            },
            current_input.clone(),
            OutputSink::Path(final_work.clone()),
            "Encode custom output",
        );
    } else {
        return Err(PlanningError::unsupported_format(
            request.settings.target_format.clone(),
            "target format is not handled by PCM encoder planning",
        ));
    }
    *current_input = InputSource::Path(final_work);
    Ok(())
}

fn append_post_processing(
    request: &PlanRequest,
    context: &PlanContext<'_>,
    steps: &mut Vec<PlanStep>,
    current_output_path: &mut PathBuf,
) -> Result<()> {
    if needs_metadata_transfer_step(request, steps) {
        let next = context.intermediate_path(steps.len(), &context.target_container_extension());
        let input = InputSource::Path(current_output_path.clone());
        push_metadata_transfer(request, steps, input, next.as_path());
        *current_output_path = next;
    }

    if request.settings.metadata.store_source_audio_md5 {
        push_step(
            steps,
            PlanOperation::StoreSourceAudioMd5 {
                target_format: request.settings.target_format.clone(),
            },
            InputSource::Path(current_output_path.clone()),
            OutputSink::InPlace(current_output_path.clone()),
            "Store source audio MD5 metadata",
        );
    }
    if request.settings.verification.verify_after_encode || flac_verify_requested(request) {
        push_step(
            steps,
            PlanOperation::Verify {
                target_format: request.settings.target_format.clone(),
            },
            InputSource::Path(current_output_path.clone()),
            OutputSink::Stdout,
            "Verify encoded output by decoding it",
        );
    }
    Ok(())
}

fn needs_metadata_transfer_step(request: &PlanRequest, _steps: &[PlanStep]) -> bool {
    if conversion_is_stream_copy_only(request) {
        return false;
    }
    metadata_policy_requires_command(request)
}

fn metadata_policy_requires_command(request: &PlanRequest) -> bool {
    // Format-specific support is registry-owned. A built-in registry may return
    // NoPluginForOperation for formats whose tag/artwork policy it cannot write;
    // a caller-provided plugin can support the same logical operation.
    request.settings.metadata.transfer_tags || request.settings.metadata.preserve_artwork
}

fn push_metadata_transfer(
    request: &PlanRequest,
    steps: &mut Vec<PlanStep>,
    input: InputSource,
    current_output_path: &std::path::Path,
) {
    let output = OutputSink::Path(current_output_path.to_path_buf());
    push_step(
        steps,
        PlanOperation::MetadataTransfer {
            target_format: request.settings.target_format.clone(),
            transfer_tags: request.settings.metadata.transfer_tags,
            preserve_artwork: request.settings.metadata.preserve_artwork,
        },
        input,
        output,
        "Apply metadata and artwork policy",
    );
}

fn push_step(
    steps: &mut Vec<PlanStep>,
    operation: PlanOperation,
    input: InputSource,
    output: OutputSink,
    description: &str,
) {
    let index = steps.len();
    steps.push(PlanStep::new(index, operation, input, output, description));
}

/// Reject resolved depths that no available encoder honors — the explicit
/// settings validation only sees `BitDepthTarget::Pcm(...)`; a
/// `BitDepthTarget::Source` over a 32-bit source resolves AFTER validation
/// and must not become a silent encoder downgrade (the ALAC 32->24 door).
fn reject_unsupported_resolved_depth(
    format: &AudioFormat,
    wavpack_hybrid: bool,
    depth: PcmBitDepth,
) -> Result<()> {
    match (format, depth) {
        (AudioFormat::Alac, PcmBitDepth::Int32) => Err(PlanningError::invalid_settings(
            "target_bit_depth",
            "ALAC 32-bit is not supported by available encoders; choose 24-bit or WavPack/WAV (source resolves to 32-bit)",
        )),
        (AudioFormat::Flac | AudioFormat::Alac, PcmBitDepth::Float32 | PcmBitDepth::Float64) => {
            Err(PlanningError::invalid_settings(
                "target_bit_depth",
                "FLAC/ALAC floating-point output is not supported; choose 24-bit integer or WAV",
            ))
        }
        (AudioFormat::WavPack, PcmBitDepth::Float32) if wavpack_hybrid => {
            Err(PlanningError::invalid_settings(
                "target_bit_depth",
                "32-bit float WavPack output is lossless-only; hybrid WavPack uses integer encoder-input PCM",
            ))
        }
        (AudioFormat::WavPack, PcmBitDepth::Float64) => Err(PlanningError::invalid_settings(
            "target_bit_depth",
            "WavPack supports native 32-bit float output, not 64-bit float; choose 32f or an integer depth",
        )),
        _ => Ok(()),
    }
}

fn resolve_target_bit_depth(request: &PlanRequest) -> Result<PcmBitDepth> {
    match request.settings.target_bit_depth {
        BitDepthTarget::Source => match request.source.representation_kind() {
            // Non-PCM-lossless targets make no bit-depth promise: the encode
            // needs SOME working width, and the format default is not a
            // substitution. Only PCM-lossless targets fail closed on an
            // unmeasurable source (handled below); everything else resolves.
            _ if !request.settings.target_format.is_pcm_lossless() => {
                Ok(request
                    .source
                    .authoritative_pcm_depth()
                    .unwrap_or_else(|| {
                        default_pcm_depth_for_format(&request.settings.target_format)
                    }))
            }
            // DSD and lossy sources have no authoritative PCM word length.
            // Resolve Source to the documented format default even when the
            // realized decoder carrier reports an integer width.
            SourceRepresentationKind::Dsd | SourceRepresentationKind::Lossy => {
                Ok(default_pcm_depth_for_format(&request.settings.target_format))
            }
            SourceRepresentationKind::Pcm => request
                .source
                .authoritative_pcm_depth()
                .map(|source_depth| {
                    crate::settings::source_pcm_depth_for_target(
                        &request.settings.target_format,
                        request.settings.wavpack.hybrid,
                        source_depth,
                    )
                })
                .ok_or_else(|| {
                    PlanningError::invalid_source(
                        "bit_depth",
                        "a PCM-lossless Source target requires an authoritative source PCM representation; choose an explicit target bit depth",
                    )
                }),
            // `representation_kind()` resolves Unspecified to an inferred
            // class before returning, so the Unspecified arm is unreachable
            // by contract here — grouped with Unknown only to satisfy
            // exhaustiveness, sharing its fail-closed answer.
            SourceRepresentationKind::Unknown | SourceRepresentationKind::Unspecified => {
                Err(PlanningError::invalid_source(
                    "bit_depth",
                    "the source PCM representation is unknown; choose an explicit target bit depth",
                ))
            }
        },
        BitDepthTarget::Pcm(depth) => Ok(depth),
    }
}

fn dsd_hard_ceiling_requires_exact_encoder_rate(request: &PlanRequest) -> bool {
    request.source.representation_kind() == SourceRepresentationKind::Dsd
        && (request.settings.dsd.gain_policy().is_true_peak()
            || request.settings.dsd.runtime_album_gain_db().is_some())
}

fn resolve_lossy_encoder_target_rate(request: &PlanRequest, requested_hz: u32) -> Result<u32> {
    match mapping::ffmpeg_lossy_encoder_accepts_rate_directly(
        &request.settings.target_format,
        requested_hz,
    ) {
        Some(true) => Ok(requested_hz),
        Some(false) if dsd_hard_ceiling_requires_exact_encoder_rate(request) => {
            Err(PlanningError::invalid_settings(
                "target_sample_rate",
                format!(
                    "album-scoped DSD hard-ceiling gain requires {} to accept {} Hz directly; rate adaptation after a hard-ceiling measurement is forbidden",
                    request.settings.target_format,
                    requested_hz,
                ),
            ))
        }
        Some(false) => mapping::ffmpeg_lossy_encoder_rate_for_request(
            &request.settings.target_format,
            requested_hz,
        )
        .ok_or_else(|| {
            PlanningError::invalid_settings(
                "target_format",
                format!(
                    "{} is marked as lossy but has no configured ordinary FFmpeg rate resolution for {} Hz",
                    request.settings.target_format,
                    requested_hz,
                ),
            )
        }),
        None => Err(PlanningError::invalid_settings(
            "target_format",
            format!(
                "{} is marked as lossy but has no configured FFmpeg direct-rate capability table",
                request.settings.target_format,
            ),
        )),
    }
}

fn resolved_lossy_encoder_target_rate(request: &PlanRequest) -> Result<Option<u32>> {
    if !request.settings.target_format.is_lossy() {
        return Ok(None);
    }
    let requested_hz = match request.settings.target_sample_rate {
        RateTarget::Source => request.source.sample_rate_hz.ok_or_else(|| {
            PlanningError::invalid_source(
                "sample_rate_hz",
                "a lossy same-as-source target requires an authoritative source PCM sample rate before encoder planning",
            )
        })?,
        RateTarget::PcmHz(hz) => hz,
        RateTarget::Dsd(_) => {
            return Err(PlanningError::invalid_settings(
                "target_sample_rate",
                "lossy PCM targets cannot use RateTarget::Dsd",
            ));
        }
    };
    resolve_lossy_encoder_target_rate(request, requested_hz).map(Some)
}

fn rate_change_for_pcm(request: &PlanRequest) -> Result<Option<u32>> {
    if let Some(effective_hz) = resolved_lossy_encoder_target_rate(request)? {
        return Ok((request.source.sample_rate_hz != Some(effective_hz)).then_some(effective_hz));
    }
    Ok(match request.settings.target_sample_rate {
        RateTarget::Source | RateTarget::Dsd(_) => None,
        RateTarget::PcmHz(hz) if request.source.sample_rate_hz == Some(hz) => None,
        RateTarget::PcmHz(hz) => Some(hz),
    })
}

fn resolve_target_dsd_rate(request: &PlanRequest) -> Result<DsdRate> {
    match request.settings.target_sample_rate {
        RateTarget::Dsd(rate) => Ok(rate),
        RateTarget::Source => request.source.dsd_rate().ok_or_else(|| {
            PlanningError::invalid_settings(
                "target_sample_rate",
                "PCM to DSD requires an explicit DSD target rate",
            )
        }),
        RateTarget::PcmHz(_) => Err(PlanningError::invalid_settings(
            "target_sample_rate",
            "DSD targets cannot use a PCM rate",
        )),
    }
}

#[cfg(test)]
mod phase2_gain_and_reference_planning_tests {
    use super::*;
    use crate::enums::{AudioCodec, SampleKind, TruePeakScanTier, TruePeakScope};
    use crate::settings::{SampleGainPolicy, PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP};
    use std::path::PathBuf;

    fn dsd_source() -> SourceInfo {
        SourceInfo {
            format: AudioFormat::Dsf,
            codec: AudioCodec::Dsd,
            sample_rate_hz: Some(DsdRate::Dsd64.hz()),
            bit_depth: None,
            true_source_depth: None,
            source_representation: SourceRepresentationKind::Dsd,
            sample_kind: Some(SampleKind::Dsd),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            dsd_source_kind: None,
            audio_md5: None,
        }
    }

    fn dsd_request(policy: SampleGainPolicy) -> PlanRequest {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.target_sample_rate = RateTarget::PcmHz(96_000);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
        settings.dsd.set_gain_policy(policy);
        PlanRequest {
            input_path: PathBuf::from("input.dsf"),
            output_path: PathBuf::from("output.flac"),
            source: dsd_source(),
            settings,
            plan_scope: crate::plan::PlanScope::submitted_batch("test-submission", "test-track", Some(1)),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
        }
    }

    #[test]
    fn shared_reference_admission_requires_explicit_reference_pathway() {
        let source = dsd_source();
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        assert!(!selects_reference_dsd_to_pcm(&settings, source.is_dsd()));

        settings.dsd.from_dsd.pathway = crate::dsd_reference::DsdSourcePathway::Reference;
        assert!(selects_reference_dsd_to_pcm(&settings, source.is_dsd()));

        settings.target_format = AudioFormat::Dsf;
        assert!(!selects_reference_dsd_to_pcm(&settings, source.is_dsd()));
        settings.target_format = AudioFormat::Flac;
        assert!(!selects_reference_dsd_to_pcm(&settings, false));
    }

    #[test]
    fn dsd_track_certified_policy_is_owned_by_phase3_common_realizer_not_legacy_lowerer() {
        let request = dsd_request(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Fast,
        });
        let error = plan_conversion(&request)
            .expect_err("legacy command lowering must not claim the Phase-3 common-realizer route");
        assert!(matches!(error, PlanningError::CapabilityUnavailable { capability: "phase3_common_realizer", .. }));
    }

    #[test]
    fn dsd_album_certified_policy_remains_an_executable_existing_bridge() {
        let request = dsd_request(SampleGainPolicy::TruePeakNormalize {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Standard,
        });
        let typed = crate::semantic_plan::plan_typed(&request);
        let Ok(crate::semantic_plan::PlanningOutcome::Ready(typed)) = typed else { panic!("typed plan should be ready") };
        assert_eq!(typed.execution_capability, crate::semantic_plan::ExecutionCapability::ExecutableNow);
    }

    #[test]
    fn pcm_fixed_gain_forces_processing_without_claiming_a_ceiling() {
        let mut request = dsd_request(SampleGainPolicy::Off);
        request.source = SourceInfo {
            format: AudioFormat::Wav,
            codec: AudioCodec::PcmFloat,
            sample_rate_hz: Some(96_000),
            bit_depth: Some(PcmBitDepth::Float64),
            true_source_depth: Some(PcmBitDepth::Float64),
            source_representation: SourceRepresentationKind::Pcm,
            sample_kind: Some(SampleKind::Float),
            channels: Some(2),
            duration: None,
            frame_extent: None,
            dsd_source_kind: None,
            audio_md5: None,
        };
        request.settings.pcm_true_peak.set_policy(SampleGainPolicy::FixedGain {
            gain_db: "2.500000000".parse().unwrap(),
        });
        let topology = plan_topology(&request).expect("fixed PCM gain topology");
        let TopologyPlan::Execute { steps, .. } = topology else { panic!("fixed gain must execute") };
        assert!(steps.iter().any(|step| matches!(step.operation, PlanOperation::EncodePcm { apply_processing: true, .. })));
    }
}

#[cfg(test)]
mod ordinary_lossy_rate_resolution_tests {
    use super::*;
    use crate::enums::{AudioCodec, AudioFormat, BitDepthTarget, PcmBitDepth, RateTarget, SampleKind};
    use crate::settings::PipelineSettings;
    use crate::source::{SourceInfo, SourceRepresentationKind};
    use std::path::PathBuf;

    fn pcm_request(source_rate_hz: u32, target: RateTarget) -> PlanRequest {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Aac;
        settings.target_sample_rate = target;
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
        PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
            input_path: PathBuf::from("input.wav"),
            output_path: PathBuf::from("output.m4a"),
            source: SourceInfo {
                dsd_source_kind: None,
                format: AudioFormat::Wav,
                codec: AudioCodec::PcmSigned,
                sample_rate_hz: Some(source_rate_hz),
                bit_depth: Some(PcmBitDepth::Int24),
                true_source_depth: Some(PcmBitDepth::Int24),
                source_representation: SourceRepresentationKind::Pcm,
                sample_kind: Some(SampleKind::SignedInteger),
                channels: Some(2),
                duration: None,
                frame_extent: None,
                audio_md5: None,
            },
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }

    fn dsd_request() -> PlanRequest {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Aac;
        settings.target_sample_rate = RateTarget::Source;
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
        PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
            input_path: PathBuf::from("input.dsf"),
            output_path: PathBuf::from("output.m4a"),
            source: SourceInfo {
                dsd_source_kind: None,
                format: AudioFormat::Dsf,
                codec: AudioCodec::Dsd,
                sample_rate_hz: Some(DsdRate::Dsd128.hz()),
                bit_depth: None,
                true_source_depth: None,
                source_representation: SourceRepresentationKind::Dsd,
                sample_kind: Some(SampleKind::Dsd),
                channels: Some(2),
                duration: None,
                frame_extent: None,
                audio_md5: None,
            },
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }

    fn lossy_step_rate(steps: &[PlanStep]) -> Option<u32> {
        steps.iter().find_map(|step| match &step.operation {
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Aac,
                target_rate_hz,
                ..
            } => *target_rate_hz,
            _ => None,
        })
    }

    #[test]
    fn pcm_same_as_source_aac_192k_resolves_to_96k_and_pins_the_encoder() {
        let request = pcm_request(192_000, RateTarget::Source);
        let topology = plan_topology(&request).expect("ordinary AAC Source rate should adapt before encode");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("lossy target must execute");
        };
        assert_eq!(lossy_step_rate(&steps), Some(96_000), "{steps:#?}");
        assert!(steps.iter().any(|step| matches!(
            &step.operation,
            PlanOperation::EncodeLossy {
                target_rate_hz: Some(96_000),
                apply_processing: true,
                ..
            }
        )), "192 -> 96 must be explicit processing, not FFmpeg negotiation: {steps:#?}");
    }

    #[test]
    fn pcm_explicit_aac_176k4_resolves_to_96k_while_supported_rate_stays_exact() {
        let request = pcm_request(176_400, RateTarget::PcmHz(176_400));
        let topology = plan_topology(&request).expect("ordinary AAC 176.4 kHz should adapt");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("lossy target must execute");
        };
        assert_eq!(lossy_step_rate(&steps), Some(96_000), "{steps:#?}");

        let supported = pcm_request(96_000, RateTarget::Source);
        let topology = plan_topology(&supported).expect("AAC 96 kHz Source rate should stay exact");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("lossy target must execute");
        };
        assert_eq!(lossy_step_rate(&steps), Some(96_000), "{steps:#?}");
    }

    #[test]
    fn pcm_same_as_source_mp3_high_rate_resolves_to_48k() {
        let mut request = pcm_request(192_000, RateTarget::Source);
        request.settings.target_format = AudioFormat::Mp3;
        request.output_path = PathBuf::from("output.mp3");
        let topology = plan_topology(&request).expect("ordinary MP3 Source rate should adapt before encode");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("lossy target must execute");
        };
        assert!(steps.iter().any(|step| matches!(
            &step.operation,
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Mp3,
                target_rate_hz: Some(48_000),
                ..
            }
        )), "192 -> 48 must be a planner-owned MP3 rate choice: {steps:#?}");
    }

    #[test]
    fn pcm_same_as_source_opus_44k1_resolves_to_48k() {
        let mut request = pcm_request(44_100, RateTarget::Source);
        request.settings.target_format = AudioFormat::Opus;
        request.output_path = PathBuf::from("output.opus");
        let topology = plan_topology(&request)
            .expect("ordinary Opus Source rate should resolve to the fixed 48 kHz encoder boundary");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("lossy target must execute");
        };
        assert!(steps.iter().any(|step| matches!(
            &step.operation,
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Opus,
                target_rate_hz: Some(48_000),
                apply_processing: true,
            }
        )), "44.1 -> 48 must be explicit Tonepoet processing before Opus encode: {steps:#?}");
    }

    #[test]
    fn pcm_track_carrying_album_auto_gain_settings_still_uses_ordinary_lossy_fallback() {
        let mut request = pcm_request(192_000, RateTarget::Source);
        request.settings.dsd.set_gain_policy(crate::settings::SampleGainPolicy::TruePeakGuard {
            target_dbtp: crate::settings::PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: crate::enums::TruePeakScope::Album,
            scan: crate::enums::TruePeakScanTier::Fast,
        });

        let topology = plan_topology(&request)
            .expect("non-DSD tracks are excluded from album gain and retain ordinary fallback");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("lossy target must execute");
        };
        assert_eq!(lossy_step_rate(&steps), Some(96_000), "{steps:#?}");
    }

    #[test]
    fn dsd_same_as_source_aac_resolves_before_carrier_creation_and_pins_final_encode() {
        let request = dsd_request();
        let topology = plan_topology(&request).expect("ordinary DSD to AAC should resolve a supported PCM rate");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("lossy target must execute");
        };
        assert!(steps.iter().any(|step| matches!(
            &step.operation,
            PlanOperation::DsdToPcm { target_rate_hz: 96_000, .. }
        )), "DSD carrier itself should be built at the resolved rate: {steps:#?}");
        assert_eq!(lossy_step_rate(&steps), Some(96_000), "{steps:#?}");
    }

    #[test]
    fn album_hard_ceiling_rejects_unsupported_rate_before_runtime_gain_is_bound() {
        let mut request = dsd_request();
        request.settings.dsd.set_gain_policy(crate::settings::SampleGainPolicy::TruePeakGuard {
            target_dbtp: crate::settings::PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: crate::enums::TruePeakScope::Album,
            scan: crate::enums::TruePeakScanTier::Fast,
        });
        request.plan_scope = PlanScope::submitted_batch(
            "album-hard-ceiling-rate",
            "track-1",
            Some(1),
        );

        let error = plan_conversion(&request)
            .expect_err("hard-ceiling DSD128 Source -> AAC must not adapt 176.4 kHz to 96 kHz");
        let message = error.to_string();
        assert!(message.contains("album-scoped DSD hard-ceiling gain"), "{message}");
        assert!(message.contains("accept 176400 Hz directly"), "{message}");
        assert!(message.contains("rate adaptation"), "{message}");
    }

    #[test]
    fn below_minimum_ac3_source_rate_resolves_upward_to_32k() {
        let mut request = pcm_request(22_050, RateTarget::Source);
        request.settings.target_format = AudioFormat::Ac3;
        request.output_path = PathBuf::from("output.ac3");
        let topology = plan_topology(&request)
            .expect("22.05 kHz AC-3 source should resolve upward instead of being refused");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("lossy target must execute");
        };
        assert!(steps.iter().any(|step| matches!(
            &step.operation,
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Ac3,
                target_rate_hz: Some(32_000),
                apply_processing: true,
            }
        )), "22.05 -> 32 must preserve available source bandwidth: {steps:#?}");
    }
}

#[cfg(test)]
mod resolved_depth_rejection_tests {
    use super::*;
    use crate::enums::{AudioFormat, PcmBitDepth};

    #[test]
    fn alac_int32_resolved_from_source_is_rejected_at_plan_time() {
        // The settings validator only sees BitDepthTarget::Pcm; a Source
        // target over a 32-bit source resolves AFTER validation and must be
        // rejected here instead of silently downgrading at the encoder.
        let err = reject_unsupported_resolved_depth(&AudioFormat::Alac, false, PcmBitDepth::Int32)
            .expect_err("resolved ALAC Int32 must fail closed");
        assert!(err.to_string().contains("ALAC 32-bit"), "{err}");
    }

    #[test]
    fn honored_resolved_depths_pass() {
        reject_unsupported_resolved_depth(&AudioFormat::Alac, false, PcmBitDepth::Int24)
            .expect("alac 24");
        reject_unsupported_resolved_depth(&AudioFormat::Flac, false, PcmBitDepth::Int32)
            .expect("flac 32");
        reject_unsupported_resolved_depth(&AudioFormat::WavPack, false, PcmBitDepth::Int32)
            .expect("wv 32");
        reject_unsupported_resolved_depth(&AudioFormat::WavPack, false, PcmBitDepth::Float32)
            .expect("lossless WavPack f32");
        reject_unsupported_resolved_depth(&AudioFormat::Aiff, false, PcmBitDepth::Float32)
            .expect("aiff f32");

        reject_unsupported_resolved_depth(&AudioFormat::WavPack, true, PcmBitDepth::Float32)
            .expect_err("hybrid WavPack f32 must fail closed");
        reject_unsupported_resolved_depth(&AudioFormat::WavPack, false, PcmBitDepth::Float64)
            .expect_err("WavPack f64 must fail closed");
    }
}

#[cfg(test)]
mod metadata_pruning_tests {
    #[test]
    fn strip_mode_metadata_transfer_is_never_pruned() {
        // Both policy flags false = strip (-map_metadata -1). The empty
        // requirement is vacuously "satisfied" by any prior effect; without
        // the explicit guard the pruner deletes the only step that performs
        // the strip and rewrites finalization to rename the plan input.
        let required = metadata_transfer_required_effect(&PlanOperation::MetadataTransfer {
            transfer_tags: false,
            preserve_artwork: false,
            target_format: AudioFormat::Flac,
        })
        .expect("strip is a metadata transfer");
        assert!(
            metadata_effect_satisfies_original_source_transfer(
                MetadataPlanEffect::none(),
                required
            ),
            "premise: an empty requirement IS vacuously satisfiable — the guard must catch it first"
        );
    }

    use super::*;
    use crate::enums::{AudioCodec, AudioFormat, PcmBitDepth, SampleKind};
    use crate::settings::PipelineSettings;
    use crate::source::SourceInfo;
    use crate::tools::{MetadataDisposition, ToolIdentifier, ToolPlugin, ToolSupport};
    use std::path::PathBuf;

    #[derive(Debug, Clone, Copy)]
    struct MetadataPruningPlugin {
        encode_effect: MetadataPlanEffect,
        disposition: MetadataDisposition,
    }

    impl ToolPlugin for MetadataPruningPlugin {
        fn id(&self) -> ToolIdentifier {
            ToolIdentifier::Ffmpeg
        }

        fn supports(&self, _context: &PlanContext<'_>, step: &PlanStep) -> ToolSupport {
            match &step.operation {
                PlanOperation::EncodePcm { .. } | PlanOperation::MetadataTransfer { .. } => {
                    ToolSupport::CANONICAL
                }
                _ => ToolSupport::UNSUPPORTED,
            }
        }

        fn metadata_effect(&self, _context: &PlanContext<'_>, step: &PlanStep) -> MetadataPlanEffect {
            match &step.operation {
                PlanOperation::EncodePcm { .. } => self.encode_effect,
                PlanOperation::MetadataTransfer {
                    transfer_tags,
                    preserve_artwork,
                    ..
                } => MetadataPlanEffect {
                    source_tags_transferred_from_original_source: *transfer_tags,
                    artwork_transferred_from_original_source: *preserve_artwork,
                    ..MetadataPlanEffect::none()
                },
                _ => MetadataPlanEffect::none(),
            }
        }

        fn metadata_disposition(
            &self,
            _context: &PlanContext<'_>,
            _step: &PlanStep,
        ) -> MetadataDisposition {
            self.disposition
        }

        fn build_command(
            &self,
            _context: &PlanContext<'_>,
            step: &PlanStep,
        ) -> Result<PlannedCommand> {
            Ok(PlannedCommand::new(
                self.id(),
                Vec::new(),
                step.input.clone(),
                step.output.clone(),
                None,
                step.description.clone(),
            )
            .with_metadata_effect(self.metadata_effect(_context, step)))
        }
    }

    fn metadata_pruning_request() -> PlanRequest {

        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.metadata.transfer_tags = true;
        settings.metadata.preserve_artwork = false;
        PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,

            input_path: PathBuf::from("source.wav"),
            output_path: PathBuf::from("output.flac"),
            source: SourceInfo {
                dsd_source_kind: None,

                format: AudioFormat::Wav,
                codec: AudioCodec::PcmSigned,
                sample_rate_hz: Some(44_100),
                bit_depth: Some(PcmBitDepth::Int16),
                true_source_depth: Some(PcmBitDepth::Int16),
                source_representation: Default::default(),
                sample_kind: Some(SampleKind::SignedInteger),
                channels: Some(2),
                duration: None,
                frame_extent: None,
                audio_md5: None,
            },
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }

    #[test]
    fn plan_context_uses_requested_output_container_extension_for_work_paths() {
        let mut request = metadata_pruning_request();
        request.settings.target_format = AudioFormat::Aac;
        request.output_path = PathBuf::from("track.m4a");
        request.intermediate_dir = Some(PathBuf::from("work"));
        let context = request.context();

        assert_eq!(context.target_container_extension(), "m4a");
        assert_eq!(
            context.final_work_path(),
            PathBuf::from("work/.track.tonepoet-final.m4a")
        );
        assert_eq!(
            context.intermediate_path(2, &context.target_container_extension()),
            PathBuf::from("work/.track.tonepoet-stage-02.m4a")
        );
    }

    #[test]
    fn plan_accepts_explicit_aac_m4b_and_preserves_work_extension() {
        let mut request = metadata_pruning_request();
        request.settings.target_format = AudioFormat::Aac;
        request.output_path = PathBuf::from("track.m4b");
        request.intermediate_dir = Some(PathBuf::from("work"));

        let context = request.context();
        assert_eq!(context.target_container_extension(), "m4b");
        assert_eq!(
            context.final_work_path(),
            PathBuf::from("work/.track.tonepoet-final.m4b")
        );
        plan_conversion(&request).expect("explicit AAC M4B must pass pure planning");
    }

    #[test]
    fn plan_context_defaults_aac_to_m4a_when_no_extension_is_requested() {
        let mut request = metadata_pruning_request();
        request.settings.target_format = AudioFormat::Aac;
        request.output_path = PathBuf::from("track");
        let context = request.context();

        assert_eq!(context.target_container_extension(), "m4a");
        assert_eq!(context.final_work_path(), PathBuf::from(".track.tonepoet-final.m4a"));
    }

    #[test]
    fn plan_rejects_aac_with_raw_aac_suffix_without_explicit_raw_mode() {
        let mut request = metadata_pruning_request();
        request.settings.target_format = AudioFormat::Aac;
        request.output_path = PathBuf::from("track.aac");

        let err = plan_conversion(&request).expect_err("raw AAC suffix should not pass MP4 muxer planning");
        assert!(
            err.to_string().contains("raw .aac output is not implemented"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn plan_rejects_alac_m4b_because_catalog_support_is_aac_only() {
        let mut request = metadata_pruning_request();
        request.settings.target_format = AudioFormat::Alac;
        request.output_path = PathBuf::from("track.m4b");

        let err = plan_conversion(&request).expect_err("ALAC M4B is not an enabled output product");
        assert!(err.to_string().contains("ALAC output must use"), "unexpected error: {err}");
    }

    #[test]
    fn plan_rejects_alac_with_non_mp4_suffix() {
        let mut request = metadata_pruning_request();
        request.settings.target_format = AudioFormat::Alac;
        request.output_path = PathBuf::from("track.alac");

        let err = plan_conversion(&request).expect_err("ALAC must be planned as M4A/MP4");
        assert!(
            err.to_string().contains("ALAC output must use"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn pruning_ignores_legacy_metadata_disposition_without_typed_effect() {
        let mut registry = ToolRegistry::empty();
        registry
            .register(Box::new(MetadataPruningPlugin {
                encode_effect: MetadataPlanEffect::none(),
                disposition: MetadataDisposition::WritesRequestedPolicy,
            }))
            .unwrap();

        let plan = plan_conversion_with_registry(&metadata_pruning_request(), &registry).unwrap();

        match plan.action {
            PlanAction::Execute { commands, .. } => {
                assert_eq!(
                    commands.len(),
                    2,
                    "a coarse plugin disposition must not prune MetadataTransfer without a typed original-source metadata effect"
                );
                assert!(matches!(
                    commands[1].metadata_effect,
                    MetadataPlanEffect {
                        source_tags_transferred_from_original_source: true,
                        ..
                    }
                ));
            }
            other => panic!("expected executable plan, got {other:?}"),
        }
    }

    #[test]
    fn pruning_uses_typed_original_source_effects() {
        let mut registry = ToolRegistry::empty();
        registry
            .register(Box::new(MetadataPruningPlugin {
                encode_effect: MetadataPlanEffect {
                    source_tags_transferred_from_original_source: true,
                    ..MetadataPlanEffect::none()
                },
                disposition: MetadataDisposition::DoesNotWrite,
            }))
            .unwrap();

        let plan = plan_conversion_with_registry(&metadata_pruning_request(), &registry).unwrap();

        match plan.action {
            PlanAction::Execute { commands, .. } => {
                assert_eq!(
                    commands.len(),
                    1,
                    "typed original-source metadata effects are sufficient to remove the redundant transfer step"
                );
                assert!(commands[0]
                    .metadata_effect
                    .source_tags_transferred_from_original_source);
            }
            other => panic!("expected executable plan, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod phase2_retained_gain_carrier_planning_tests {
    use super::*;
    use crate::enums::{
        AudioCodec, AudioFormat, BitDepthTarget, PcmBitDepth, RateTarget, SampleKind,
        TruePeakScanTier, TruePeakScope,
    };
    use crate::settings::{PipelineSettings, SampleGainPolicy, PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP};
    use crate::source::{SourceInfo, SourceRepresentationKind};
    use std::path::PathBuf;

    fn carrier_request(source_rate_hz: u32) -> PlanRequest {
        let mut settings = PipelineSettings::default();
        settings.target_format = AudioFormat::Flac;
        settings.target_sample_rate = RateTarget::PcmHz(96_000);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
        settings.dsd.set_gain_policy(SampleGainPolicy::TruePeakNormalize {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Album,
            scan: TruePeakScanTier::Standard,
        });
        settings
            .dsd
            .set_runtime_album_gain_db(Some("2.125000000".parse().unwrap()));
        PlanRequest {
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
            input_path: PathBuf::from("album-carrier.f64le"),
            output_path: PathBuf::from("output.flac"),
            source: SourceInfo {
                dsd_source_kind: None,
                format: AudioFormat::Wav,
                codec: AudioCodec::PcmFloat,
                sample_rate_hz: Some(source_rate_hz),
                bit_depth: Some(PcmBitDepth::Float64),
                true_source_depth: None,
                source_representation: SourceRepresentationKind::Dsd,
                sample_kind: Some(SampleKind::Float),
                channels: Some(2),
                duration: None,
                frame_extent: None,
                audio_md5: None,
            },
            settings,
            plan_scope: crate::plan::PlanScope::track("test-track"),
            intermediate_dir: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }

    #[test]
    fn runtime_album_gain_requires_dsd_semantic_carrier() {
        let mut request = carrier_request(96_000);
        request.source.source_representation = SourceRepresentationKind::Pcm;
        let error = plan_topology(&request).expect_err("PCM authority must not receive DSD album gain");
        assert!(
            error.to_string().contains("runtime DSD album gain may be applied only"),
            "{error}"
        );
    }

    #[test]
    fn runtime_album_gain_carrier_must_already_be_at_final_rate() {
        let request = carrier_request(88_200);
        let error = plan_topology(&request).expect_err("post-measurement resampling must be refused");
        assert!(error.to_string().contains("carrier rate must already equal"), "{error}");
    }

    #[test]
    fn runtime_album_gain_forces_one_processing_encode_without_resample() {
        let request = carrier_request(96_000);
        let topology = plan_topology(&request).expect("valid retained DSD carrier topology");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("runtime album gain must force an executable encode");
        };
        let processing_encodes = steps
            .iter()
            .filter(|step| {
                matches!(
                    &step.operation,
                    PlanOperation::EncodePcm {
                        apply_processing: true,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(processing_encodes, 1, "{steps:#?}");
        assert!(
            !steps
                .iter()
                .any(|step| matches!(&step.operation, PlanOperation::ResamplePcm { .. })),
            "post-measurement resampling would invalidate the measured peak: {steps:#?}"
        );
    }

    #[test]
    fn runtime_album_gain_rejects_aac_rate_that_would_be_resampled_after_gain() {
        let mut request = carrier_request(192_000);
        request.settings.target_format = AudioFormat::Aac;
        request.settings.target_sample_rate = RateTarget::PcmHz(192_000);
        request.output_path = PathBuf::from("output.m4a");

        let error = plan_topology(&request)
            .expect_err("AAC 192 kHz must not negotiate a post-gain encoder rate");
        assert!(error.to_string().contains("accept 192000 Hz directly"), "{error}");
    }

    #[test]
    fn runtime_album_gain_pins_supported_aac_encoder_input_rate() {
        let mut request = carrier_request(96_000);
        request.settings.target_format = AudioFormat::Aac;
        request.settings.target_sample_rate = RateTarget::PcmHz(96_000);
        request.output_path = PathBuf::from("output.m4a");

        let topology = plan_topology(&request).expect("AAC 96 kHz hard-ceiling topology");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("runtime album gain must force executable AAC encode");
        };
        let audio_steps: Vec<_> = steps
            .iter()
            .filter(|step| !matches!(&step.operation, PlanOperation::MetadataTransfer { .. }))
            .collect();
        assert_eq!(audio_steps.len(), 1, "{steps:#?}");
        assert!(matches!(
            &audio_steps[0].operation,
            PlanOperation::EncodeLossy {
                target_format: AudioFormat::Aac,
                target_rate_hz: Some(96_000),
                apply_processing: true,
            }
        ));
        assert!(
            !steps
                .iter()
                .any(|step| matches!(&step.operation, PlanOperation::ResamplePcm { .. })),
            "supported encoder-input rate must not add a post-measurement resample: {steps:#?}"
        );
    }

    #[test]
    fn runtime_album_gain_routes_non_sox_lossless_dither_through_one_sox_pcm_terminal() {
        let mut request = carrier_request(96_000);
        request.settings.target_format = AudioFormat::Alac;
        request.settings.dither_type = crate::enums::DitherType::Tpdf;
        request.output_path = PathBuf::from("output.m4a");

        let topology = plan_topology(&request).expect("ALAC hard-ceiling topology");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("runtime album gain must force executable ALAC encode");
        };
        let audio_steps: Vec<_> = steps
            .iter()
            .filter(|step| !matches!(&step.operation, PlanOperation::MetadataTransfer { .. }))
            .collect();
        assert_eq!(audio_steps.len(), 2, "{steps:#?}");
        assert!(matches!(
            &audio_steps[0].operation,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Wav,
                target_bit_depth: PcmBitDepth::Int24,
                apply_processing: true,
                ..
            }
        ));
        assert!(matches!(
            &audio_steps[1].operation,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Alac,
                target_bit_depth: PcmBitDepth::Int24,
                apply_processing: false,
                ..
            }
        ));
    }

    #[test]
    fn pcm_true_peak_routes_non_sox_lossless_dither_through_one_sox_pcm_terminal() {
        let mut request = carrier_request(96_000);
        request.settings.dsd = crate::settings::DsdSettings::default();
        request.settings.pcm_true_peak.set_policy(SampleGainPolicy::TruePeakGuard {
            target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
            scope: TruePeakScope::Track,
            scan: TruePeakScanTier::Fast,
        });
        request.settings.target_format = AudioFormat::Alac;
        request.settings.dither_type = crate::enums::DitherType::Tpdf;
        request.source.source_representation = SourceRepresentationKind::Pcm;
        request.output_path = PathBuf::from("output.m4a");

        let topology = plan_topology(&request).expect("PCM true-peak ALAC hard-ceiling topology");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("PCM true-peak carrier must force executable ALAC encode");
        };
        let audio_steps: Vec<_> = steps
            .iter()
            .filter(|step| !matches!(&step.operation, PlanOperation::MetadataTransfer { .. }))
            .collect();
        assert_eq!(audio_steps.len(), 2, "{steps:#?}");
        assert!(matches!(
            &audio_steps[0].operation,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Wav,
                target_bit_depth: PcmBitDepth::Int24,
                apply_processing: true,
                ..
            }
        ));
        assert!(matches!(
            &audio_steps[1].operation,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Alac,
                target_bit_depth: PcmBitDepth::Int24,
                apply_processing: false,
                ..
            }
        ));
    }

    #[test]
    fn pcm_fixed_gain_forces_one_processing_encode_without_measurement_authority() {
        let mut request = carrier_request(96_000);
        request.settings.dsd = crate::settings::DsdSettings::default();
        request.settings.pcm_true_peak.set_policy(SampleGainPolicy::FixedGain {
            gain_db: "2.500000000".parse().expect("fixed gain"),
        });
        request.settings.target_format = AudioFormat::Flac;
        request.settings.target_sample_rate = RateTarget::PcmHz(96_000);
        request.settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
        request.source.source_representation = SourceRepresentationKind::Pcm;
        request.output_path = PathBuf::from("output.flac");

        let topology = plan_topology(&request).expect("fixed PCM gain topology");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("fixed PCM gain must force executable processing");
        };
        let audio_steps: Vec<_> = steps
            .iter()
            .filter(|step| !matches!(&step.operation, PlanOperation::MetadataTransfer { .. }))
            .collect();
        assert_eq!(audio_steps.len(), 1, "{steps:#?}");
        assert!(matches!(
            &audio_steps[0].operation,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Flac,
                target_rate_hz: None,
                target_bit_depth: PcmBitDepth::Int24,
                apply_processing: true,
            }
        ));
    }

    #[test]
    fn pcm_fixed_gain_wavpack_hybrid_fuses_resample_gain_and_integer_realization() {
        let mut request = carrier_request(44_100);
        request.settings.dsd = crate::settings::DsdSettings::default();
        request.settings.pcm_true_peak.set_policy(SampleGainPolicy::FixedGain {
            gain_db: "-3.250000000".parse().expect("fixed gain"),
        });
        request.settings.target_format = AudioFormat::WavPack;
        request.settings.wavpack.hybrid = true;
        request.settings.target_sample_rate = RateTarget::PcmHz(96_000);
        request.settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
        request.source.source_representation = SourceRepresentationKind::Pcm;
        request.output_path = PathBuf::from("output.wv");

        let topology = plan_topology(&request).expect("fixed-gain WavPack hybrid topology");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("fixed-gain WavPack hybrid must execute");
        };
        let audio_steps: Vec<_> = steps
            .iter()
            .filter(|step| !matches!(&step.operation, PlanOperation::MetadataTransfer { .. }))
            .collect();
        assert_eq!(audio_steps.len(), 2, "{steps:#?}");
        assert!(matches!(
            &audio_steps[0].operation,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::Wav,
                target_rate_hz: Some(96_000),
                target_bit_depth: PcmBitDepth::Int24,
                apply_processing: true,
            }
        ));
        assert!(matches!(
            &audio_steps[1].operation,
            PlanOperation::EncodePcm {
                target_format: AudioFormat::WavPack,
                target_rate_hz: None,
                target_bit_depth: PcmBitDepth::Int24,
                apply_processing: false,
            }
        ));
        assert!(
            !steps
                .iter()
                .any(|step| matches!(&step.operation, PlanOperation::ResamplePcm { .. })),
            "fixed gain should realize resampling exactly once: {steps:#?}",
        );
    }

    #[test]
    fn pcm_true_peak_wavpack_hybrid_realizes_one_integer_encoder_input_without_resampling() {
        for scope in [TruePeakScope::Track, TruePeakScope::Album] {
            let mut request = carrier_request(96_000);
            request.settings.dsd = crate::settings::DsdSettings::default();
            request.settings.pcm_true_peak.set_policy(SampleGainPolicy::TruePeakGuard {
                target_dbtp: PCM_TRUE_PEAK_DEFAULT_TARGET_DBTP,
                scope,
                scan: TruePeakScanTier::Fast,
            });
            request.settings.target_format = AudioFormat::WavPack;
            request.settings.wavpack.hybrid = true;
            request.settings.target_sample_rate = RateTarget::PcmHz(96_000);
            request.settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
            request.source.source_representation = SourceRepresentationKind::Pcm;
            request.output_path = PathBuf::from("output.wv");

            let topology =
                plan_topology(&request).expect("PCM true-peak hybrid WavPack topology");
            let TopologyPlan::Execute { steps, .. } = topology else {
                panic!("PCM true-peak WavPack hybrid must force executable encode");
            };
            let audio_steps: Vec<_> = steps
                .iter()
                .filter(|step| !matches!(&step.operation, PlanOperation::MetadataTransfer { .. }))
                .collect();
            assert_eq!(audio_steps.len(), 2, "scope={scope:?}: {steps:#?}");
            assert!(matches!(
                &audio_steps[0].operation,
                PlanOperation::EncodePcm {
                    target_format: AudioFormat::Wav,
                    target_rate_hz: None,
                    target_bit_depth: PcmBitDepth::Int24,
                    apply_processing: true,
                }
            ));
            assert!(matches!(
                &audio_steps[1].operation,
                PlanOperation::EncodePcm {
                    target_format: AudioFormat::WavPack,
                    target_rate_hz: None,
                    target_bit_depth: PcmBitDepth::Int24,
                    apply_processing: false,
                }
            ));
            assert_eq!(
                audio_steps[0].output.as_path(),
                audio_steps[1].input.as_path(),
                "native wavpack must receive the exact admitted integer PCM carrier",
            );
            assert!(
                !steps
                    .iter()
                    .any(|step| matches!(&step.operation, PlanOperation::ResamplePcm { .. })),
                "scope={scope:?}: post-measurement resampling would invalidate the governed encoder input: {steps:#?}",
            );
        }
    }
}

#[cfg(test)]
mod stage_a_lowering_selection_diagnostics {
    use super::*;
    use crate::enums::{
        AudioCodec, AudioFormat, BitDepthTarget, PcmBitDepth, PreferredTool, RateTarget,
        SampleKind,
    };
    use crate::semantic_plan::{plan_typed, PlanningOutcome, TypedPlanNode};
    use crate::settings::PipelineSettings;
    use crate::source::{SourceInfo, SourceRepresentationKind};
    use std::path::PathBuf;

    fn pcm_request(
        route: &str,
        source_rate_hz: u32,
        target_format: AudioFormat,
        target_rate_hz: u32,
        preferred_tool: PreferredTool,
    ) -> PlanRequest {
        let mut settings = PipelineSettings::default();
        settings.target_format = target_format.clone();
        settings.target_sample_rate = RateTarget::PcmHz(target_rate_hz);
        settings.target_bit_depth = BitDepthTarget::Pcm(PcmBitDepth::Int24);
        settings.preferred_tool = preferred_tool;
        PlanRequest {
            input_path: PathBuf::from(format!("{route}-input.wav")),
            output_path: PathBuf::from(format!("{route}-output.{}", target_format.extension())),
            source: SourceInfo {
                format: AudioFormat::Wav,
                codec: AudioCodec::PcmSigned,
                sample_rate_hz: Some(source_rate_hz),
                bit_depth: Some(PcmBitDepth::Int24),
                true_source_depth: Some(PcmBitDepth::Int24),
                source_representation: SourceRepresentationKind::Pcm,
                sample_kind: Some(SampleKind::SignedInteger),
                channels: Some(2),
                duration: None,
                frame_extent: None,
                dsd_source_kind: None,
                audio_md5: None,
            },
            settings,
            plan_scope: PlanScope::track(format!("stage-a-{route}")),
            intermediate_dir: Some(PathBuf::from(format!("{route}-work")),),
            container_ffmpeg_flags: Vec::new(),
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
        }
    }

    fn eligible_builtin_operation_count(typed: &crate::semantic_plan::TypedConversionPlan) -> usize {
        typed
            .nodes
            .iter()
            .filter(|node| {
                let TypedPlanNode::Operation {
                    candidates,
                    selected_candidate,
                    ..
                } = node
                else {
                    return false;
                };
                candidates
                    .get(*selected_candidate)
                    .and_then(|candidate| candidate.tool.as_ref())
                    .is_some_and(stage_a_is_builtin_tool)
            })
            .count()
    }

    fn validate_records(
        route: &str,
        records: &[StageASelectedVsEmittedRecord],
    ) -> std::result::Result<(), String> {
        for record in records {
            if !record.matches_selected_realization() {
                return Err(format!(
                    "{route}: typed operation {} selected {:?} {:?}, emitted {:?} {:?}",
                    record.operation.label(),
                    record.selected_tool,
                    record.selected_parameter_signature,
                    record.emitted_tool,
                    record.emitted_parameter_signature,
                ));
            }
        }
        Ok(())
    }

    fn assert_no_selected_vs_emitted_divergence(
        route: &str,
        request: &PlanRequest,
    ) -> Vec<StageASelectedVsEmittedRecord> {
        let typed = match plan_typed(request).expect("typed planner must stay within resource bounds") {
            PlanningOutcome::Ready(plan) => plan,
            other => panic!("{route}: typed plan was not ready: {other:?}"),
        };
        crate::semantic_plan::require_current_executor(&typed)
            .unwrap_or_else(|error| panic!("{route}: route must use the current executor: {error}"));

        let eligible = eligible_builtin_operation_count(&typed);
        let records = stage_a_selected_vs_emitted_diagnostics(request)
            .unwrap_or_else(|error| panic!("{route}: diagnostic failed: {error}"));
        assert_eq!(
            records.len(), eligible,
            "{route}: every typed operation with a selected built-in candidate must have one diagnostic record"
        );
        assert!(eligible > 0, "{route}: diagnostic fixture has no eligible typed operation");

        for record in &records {
            eprintln!(
                "I1_DIAG route={route} typed_node={} emitted_step={} operation={} emitted_operation={} candidate={:?} selected_tool={:?} emitted_tool={:?} lowering_mode={:?} resolved={:?} selected_params={:?} emitted_params={:?}",
                record.typed_node_index,
                record.emitted_step_index,
                record.operation.label(),
                record.emitted_operation.label(),
                record.candidate_identity,
                record.selected_tool,
                record.emitted_tool,
                record.lowering_provenance,
                record.resolved_parameters,
                record.selected_parameter_signature,
                record.emitted_parameter_signature,
            );
        }
        validate_records(route, &records).unwrap_or_else(|error| panic!("{error}"));
        records
    }

    #[test]
    fn stage_a_builtin_selected_tools_and_parameters_match_every_emitted_operation() {
        let mut ssrc = pcm_request(
            "pcm-flac-resample-ssrc",
            96_000,
            AudioFormat::Flac,
            44_100,
            PreferredTool::Ssrc,
        );
        ssrc.settings.ssrc.force = true;

        let routes = [
            (
                "pcm-flac-resample-auto",
                pcm_request(
                    "pcm-flac-resample-auto",
                    96_000,
                    AudioFormat::Flac,
                    44_100,
                    PreferredTool::Auto,
                ),
            ),
            (
                "pcm-flac-resample-ffmpeg",
                pcm_request(
                    "pcm-flac-resample-ffmpeg",
                    96_000,
                    AudioFormat::Flac,
                    44_100,
                    PreferredTool::Ffmpeg,
                ),
            ),
            (
                "pcm-flac-resample-sox",
                pcm_request(
                    "pcm-flac-resample-sox",
                    96_000,
                    AudioFormat::Flac,
                    44_100,
                    PreferredTool::Sox,
                ),
            ),
            ("pcm-flac-resample-ssrc", ssrc),
            (
                "pcm-flac-direct-auto",
                pcm_request(
                    "pcm-flac-direct-auto",
                    44_100,
                    AudioFormat::Flac,
                    44_100,
                    PreferredTool::Auto,
                ),
            ),
            (
                "pcm-aac-resample-auto",
                pcm_request(
                    "pcm-aac-resample-auto",
                    96_000,
                    AudioFormat::Aac,
                    48_000,
                    PreferredTool::Auto,
                ),
            ),
        ];

        for (route, request) in routes {
            let source_rate = request.source.sample_rate_hz;
            let target_rate = match &request.settings.target_sample_rate {
                RateTarget::PcmHz(rate) => Some(*rate),
                _ => None,
            };
            let records = assert_no_selected_vs_emitted_divergence(route, &request);
            if source_rate != target_rate {
                assert!(
                    records.iter().any(|record| matches!(&record.operation, PlanOperation::ResamplePcm { .. })),
                    "{route}: resampling route did not account for typed ResamplePcm"
                );
                assert!(
                    records.iter().any(|record| matches!(&record.operation, PlanOperation::EncodePcm { .. } | PlanOperation::EncodeLossy { .. })),
                    "{route}: resampling route did not separately account for terminal encoding"
                );
            }
            if route == "pcm-flac-resample-ssrc" {
                let resample = records
                    .iter()
                    .find(|record| matches!(&record.operation, PlanOperation::ResamplePcm { .. }))
                    .expect("forced SSRC fixture must expose typed resampling");
                assert_eq!(resample.selected_tool, ToolIdentifier::Ssrc);
                assert_eq!(resample.emitted_tool, ToolIdentifier::Ssrc);
                assert!(matches!(&resample.emitted_operation, PlanOperation::ResamplePcm { .. }));
            }
        }
    }

    #[test]
    fn stage_a_diagnostic_rejects_tool_or_semantic_parameter_perturbation() {
        let request = pcm_request(
            "pcm-flac-resample-sox-perturb",
            96_000,
            AudioFormat::Flac,
            44_100,
            PreferredTool::Sox,
        );
        let registry = ToolRegistry::with_builtin_tools();
        let typed = match plan_typed(&request).unwrap() {
            PlanningOutcome::Ready(plan) => plan,
            other => panic!("typed plan was not ready: {other:?}"),
        };
        let TopologyPlan::Execute { steps, finalization } = plan_topology(&request).unwrap() else {
            panic!("perturbation fixture unexpectedly planned passthrough")
        };
        let context = request.context();
        let (steps, _) = prune_redundant_metadata_steps(&context, &registry, &steps, finalization).unwrap();
        let emitted = plan_conversion_with_registry(&request, &registry).unwrap();
        let commands = emitted.commands();
        let clean = stage_a_selected_vs_emitted_records_from_lowered(
            &typed, &context, &registry, &steps, commands,
        ).unwrap();
        validate_records("clean", &clean).unwrap();

        let mut parameter_commands = commands.to_vec();
        let changed = parameter_commands.iter_mut().any(|command| {
            if command.tool != ToolIdentifier::Sox {
                return false;
            }
            if let Some(arg) = command.args.iter_mut().rfind(|arg| arg.as_str() == "44100") {
                *arg = "48000".to_string();
                true
            } else {
                false
            }
        });
        assert!(changed, "fixture did not expose a semantic resample-rate argument to perturb");
        let parameter_records = stage_a_selected_vs_emitted_records_from_lowered(
            &typed, &context, &registry, &steps, &parameter_commands,
        ).unwrap();
        assert!(validate_records("parameter-perturbed", &parameter_records).is_err());

        let mut tool_commands = commands.to_vec();
        tool_commands[0].tool = ToolIdentifier::Ffmpeg;
        let tool_records = stage_a_selected_vs_emitted_records_from_lowered(
            &typed, &context, &registry, &steps, &tool_commands,
        ).unwrap();
        assert!(validate_records("tool-perturbed", &tool_records).is_err());
    }
}


#[cfg(test)]
mod source_float_source_depth_policy_tests {
    use super::*;
    use crate::enums::{
        AudioCodec, AudioFormat, BitDepthTarget, DitherType, PcmBitDepth, RateTarget, SampleKind,
    };
    use crate::settings::PipelineSettings;
    use crate::source::{SourceInfo, SourceRepresentationKind};
    use std::path::PathBuf;

    fn request(
        source_format: AudioFormat,
        source_depth: PcmBitDepth,
        target_format: AudioFormat,
    ) -> PlanRequest {
        let mut settings = PipelineSettings::default();
        settings.target_format = target_format.clone();
        settings.target_sample_rate = RateTarget::Source;
        settings.target_bit_depth = BitDepthTarget::Source;
        settings.dither_type = DitherType::None;
        settings.dither_explicit = false;
        // The built-in registry has no metadata-transfer plugin; sibling tests
        // disable transfer so planning exercises the audio topology only.
        settings.metadata.transfer_tags = false;
        settings.metadata.preserve_artwork = false;
        PlanRequest {
            input_path: PathBuf::from(format!("input.{}", source_format.extension())),
            output_path: PathBuf::from(format!("output.{}", target_format.extension())),
            source: SourceInfo {
                format: source_format.clone(),
                codec: if source_format == AudioFormat::WavPack {
                    AudioCodec::WavPack
                } else {
                    AudioCodec::PcmFloat
                },
                sample_rate_hz: Some(192_000),
                bit_depth: Some(source_depth),
                true_source_depth: Some(source_depth),
                source_representation: SourceRepresentationKind::Pcm,
                sample_kind: Some(SampleKind::Float),
                channels: Some(2),
                duration: None,
                frame_extent: None,
                dsd_source_kind: None,
                audio_md5: None,
            },
            settings,
            plan_scope: PlanScope::track("source-float-source-depth"),
            intermediate_dir: Some(PathBuf::from("work")),
            container_ffmpeg_flags: Vec::new(),
            resolved_output_target: None,
            reference_programme_scope: Default::default(),
            planned_riff_non_audio_upper_bound_bytes: None,
        }
    }

    fn planned_pcm_depth(request: &PlanRequest) -> PcmBitDepth {
        let topology = plan_topology(request).expect("Source float topology must plan");
        let TopologyPlan::Execute { steps, .. } = topology else {
            panic!("float Source mapping must execute rather than passthrough");
        };
        steps
            .iter()
            .rev()
            .find_map(|step| match &step.operation {
                PlanOperation::EncodePcm {
                    target_bit_depth, ..
                } => Some(*target_bit_depth),
                _ => None,
            })
            .expect("PCM target must include an encode operation")
    }

    fn emitted_triangular_dither(request: &PlanRequest) -> bool {
        let plan = plan_conversion(request).expect("Source float command plan must lower");
        let PlanAction::Execute { commands, .. } = plan.action else {
            panic!("float Source mapping must execute rather than passthrough");
        };
        commands.iter().any(|command| {
            command
                .args
                .iter()
                .any(|arg| arg.contains("dither_method=triangular"))
        })
    }

    #[test]
    fn float32_source_lands_at_format_safe_integer_depths() {
        let flac = request(AudioFormat::Wav, PcmBitDepth::Float32, AudioFormat::Flac);
        assert_eq!(planned_pcm_depth(&flac), PcmBitDepth::Int32);
        assert!(!emitted_triangular_dither(&flac));

        let alac = request(AudioFormat::Wav, PcmBitDepth::Float32, AudioFormat::Alac);
        assert_eq!(planned_pcm_depth(&alac), PcmBitDepth::Int24);
        assert!(emitted_triangular_dither(&alac));

        let wavpack = request(
            AudioFormat::Wav,
            PcmBitDepth::Float32,
            AudioFormat::WavPack,
        );
        assert_eq!(planned_pcm_depth(&wavpack), PcmBitDepth::Int32);
        assert!(!emitted_triangular_dither(&wavpack));
    }

    #[test]
    fn float64_source_uses_tpdf_for_each_integer_landing() {
        for (format, expected_depth) in [
            (AudioFormat::Flac, PcmBitDepth::Int32),
            (AudioFormat::Alac, PcmBitDepth::Int24),
            (AudioFormat::WavPack, PcmBitDepth::Int32),
        ] {
            let request = request(AudioFormat::Wav, PcmBitDepth::Float64, format.clone());
            assert_eq!(planned_pcm_depth(&request), expected_depth, "{format}");
            assert!(emitted_triangular_dither(&request), "{format}");
        }
    }

    #[test]
    fn same_format_float_wavpack_source_cannot_passthrough_the_integer_policy() {
        let request = request(
            AudioFormat::WavPack,
            PcmBitDepth::Float32,
            AudioFormat::WavPack,
        );
        assert_eq!(planned_pcm_depth(&request), PcmBitDepth::Int32);
        let plan = plan_conversion(&request).expect("lossless WavPack Source must re-encode");
        assert!(matches!(plan.action, PlanAction::Execute { .. }));
    }

    fn hybrid_request(source_depth: PcmBitDepth) -> PlanRequest {
        let mut request = request(AudioFormat::Wav, source_depth, AudioFormat::WavPack);
        request.settings.wavpack.hybrid = true;
        request
    }

    #[test]
    fn wavpack_hybrid_float32_source_realizes_int32_without_automatic_dither() {
        let request = hybrid_request(PcmBitDepth::Float32);
        assert_eq!(planned_pcm_depth(&request), PcmBitDepth::Int32);
        assert!(!emitted_triangular_dither(&request));

        let plan = plan_conversion(&request).expect("Float32 hybrid Source command plan");
        let PlanAction::Execute { commands, .. } = plan.action else {
            panic!("Float32 hybrid Source must execute");
        };
        let wavpack_index = commands
            .iter()
            .position(|command| command.tool == ToolIdentifier::Custom("wavpack".to_owned()))
            .expect("native WavPack package command");
        let sox_index = commands
            .iter()
            .position(|command| command.tool == ToolIdentifier::Sox)
            .expect("undithered Int32 SoX preterminal");
        assert!(sox_index < wavpack_index);
        assert_eq!(commands[sox_index].output.as_path(), commands[wavpack_index].input.as_path());
        assert!(commands.iter().all(|command| {
            !command.args.iter().any(|arg| {
                arg.contains("dither_method=") || arg == "dither" || arg == "--dither"
            })
        }));
    }

    #[test]
    fn wavpack_hybrid_float32_source_keeps_sox_preterminal_under_ffmpeg_preference() {
        let mut request = hybrid_request(PcmBitDepth::Float32);
        request.settings.preferred_tool = crate::enums::PreferredTool::Ffmpeg;

        let plan = plan_conversion(&request)
            .expect("Float32 hybrid Source must keep a truthful SoX preterminal");
        let PlanAction::Execute { commands, .. } = plan.action else {
            panic!("Float32 hybrid Source must execute");
        };
        let wavpack_index = commands
            .iter()
            .position(|command| command.tool == ToolIdentifier::Custom("wavpack".to_owned()))
            .expect("native WavPack package command");
        let package_input = commands[wavpack_index].input.as_path();
        let (producer_index, producer) = commands
            .iter()
            .enumerate()
            .find(|(index, command)| {
                *index < wavpack_index && command.output.as_path() == package_input
            })
            .expect("a preterminal command must feed native WavPack directly");
        assert!(producer_index < wavpack_index);
        assert_eq!(
            producer.tool,
            ToolIdentifier::Sox,
            "FFmpeg preference must not change the semantic/physical owner of the undithered Float32 Source preterminal: {commands:#?}",
        );
        assert!(commands.iter().all(|command| {
            !command.args.iter().any(|arg| {
                arg.contains("dither_method=") || arg == "dither" || arg == "--dither"
            })
        }));
    }

    #[test]
    fn wavpack_hybrid_float32_source_explicit_tpdf_uses_exactly_one_ffmpeg_soxr_preterminal() {
        let mut request = hybrid_request(PcmBitDepth::Float32);
        request.settings.dither_type = DitherType::Tpdf;
        request.settings.dither_explicit = true;
        assert_eq!(planned_pcm_depth(&request), PcmBitDepth::Int32);

        let plan = plan_conversion(&request)
            .expect("Float32 hybrid Source explicit TPDF command plan");
        let PlanAction::Execute { commands, .. } = plan.action else {
            panic!("Float32 hybrid Source explicit TPDF must execute");
        };
        let wavpack_index = commands
            .iter()
            .position(|command| command.tool == ToolIdentifier::Custom("wavpack".to_owned()))
            .expect("native WavPack package command");
        let package_input = commands[wavpack_index].input.as_path();
        let (producer_index, producer) = commands
            .iter()
            .enumerate()
            .find(|(index, command)| {
                *index < wavpack_index && command.output.as_path() == package_input
            })
            .expect("FFmpeg preterminal must feed native WavPack directly");
        assert!(producer_index < wavpack_index, "{commands:#?}");
        assert_eq!(producer.tool, ToolIdentifier::Ffmpeg, "{commands:#?}");
        assert_eq!(
            producer
                .args
                .iter()
                .filter(|arg| arg.contains("dither_method=triangular"))
                .count(),
            1,
            "{commands:#?}",
        );
        assert_eq!(
            commands
                .iter()
                .flat_map(|command| command.args.iter())
                .filter(|arg| arg.contains("dither_method=triangular"))
                .count(),
            1,
            "explicit TPDF must have exactly one physical owner",
        );
        assert!(commands.iter().all(|command| {
            command.tool != ToolIdentifier::Sox
                || !command.args.iter().any(|arg| arg == "dither" || arg == "--dither")
        }));
    }

    #[test]
    fn wavpack_hybrid_float64_source_uses_exactly_one_ffmpeg_soxr_tpdf_preterminal() {
        for explicit_tpdf in [false, true] {
            let mut request = hybrid_request(PcmBitDepth::Float64);
            if explicit_tpdf {
                request.settings.dither_type = DitherType::Tpdf;
                request.settings.dither_explicit = true;
            }
            assert_eq!(planned_pcm_depth(&request), PcmBitDepth::Int32);

            let plan = plan_conversion(&request).expect("Float64 hybrid Source command plan");
            let PlanAction::Execute { commands, .. } = plan.action else {
                panic!("Float64 hybrid Source must execute");
            };
            let wavpack_index = commands
                .iter()
                .position(|command| command.tool == ToolIdentifier::Custom("wavpack".to_owned()))
                .expect("native WavPack package command");
            let ffmpeg_dither = commands
                .iter()
                .enumerate()
                .filter(|(_, command)| {
                    command.tool == ToolIdentifier::Ffmpeg
                        && command
                            .args
                            .iter()
                            .filter(|arg| arg.contains("dither_method=triangular"))
                            .count()
                            == 1
                })
                .collect::<Vec<_>>();
            assert_eq!(ffmpeg_dither.len(), 1, "explicit={explicit_tpdf}: {commands:#?}");
            let (ffmpeg_index, ffmpeg) = ffmpeg_dither[0];
            assert!(ffmpeg_index < wavpack_index, "explicit={explicit_tpdf}: {commands:#?}");
            assert_eq!(ffmpeg.output.as_path(), commands[wavpack_index].input.as_path());
            assert_eq!(
                commands
                    .iter()
                    .flat_map(|command| command.args.iter())
                    .filter(|arg| arg.contains("dither_method=triangular"))
                    .count(),
                1,
                "automatic and explicit TPDF must each have exactly one physical owner",
            );
        }
    }

    #[test]
    fn wavpack_hybrid_integer_source_retains_each_authoritative_width() {
        for depth in [PcmBitDepth::Int16, PcmBitDepth::Int24, PcmBitDepth::Int32] {
            let mut request = hybrid_request(depth);
            request.source.codec = AudioCodec::PcmSigned;
            request.source.sample_kind = Some(SampleKind::SignedInteger);
            request.source.bit_depth = Some(depth);
            request.source.true_source_depth = Some(depth);
            assert_eq!(planned_pcm_depth(&request), depth, "{depth:?}");
            assert!(!emitted_triangular_dither(&request), "{depth:?}");
        }
    }

    #[test]
    fn explicit_dither_none_suppresses_automatic_source_tpdf() {
        let mut request = request(AudioFormat::Wav, PcmBitDepth::Float64, AudioFormat::Flac);
        request.settings.dither_explicit = true;
        assert_eq!(planned_pcm_depth(&request), PcmBitDepth::Int32);
        assert!(!emitted_triangular_dither(&request));
    }
}
