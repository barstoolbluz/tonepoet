//! Deterministic conversion-chain planner.

use crate::dsd_reference::{
    plan_reference_dsd, DsdReferencePlanSummary, PlannedDeferredCommand, PlannedMeasurement,
    ReferenceProgrammeScope, ResolvedOutputTarget,
};
use crate::enums::{
    AudioCodec, AudioFormat, BitDepthTarget, DitherType, DsdFilterPreset, DsdLowpassMethod,
    DsdRate, NyquistTransition, PcmBitDepth, RateTarget, ReplayGainMode, SampleKind, SsrcProfile,
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
    /// Stable environment variables requested by this command.
    pub environment: BTreeMap<String, String>,
    /// Optional progress estimate.
    pub expected_duration: Option<Duration>,
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
            environment: BTreeMap::new(),
            expected_duration,
            description: description.into(),
            metadata_effect: MetadataPlanEffect::none(),
        }
    }

    /// Return this command annotated with a typed metadata effect.
    #[must_use]
    pub fn with_metadata_effect(mut self, metadata_effect: MetadataPlanEffect) -> Self {
        self.metadata_effect = metadata_effect;
        self
    }
}

/// One executable P0 step. Existing plans use `Command`; Reference plans may
/// additionally measure and bind a later command without replanning.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PlannedExecutionStep {
    /// Ordinary fully resolved command.
    Command(PlannedCommand),
    /// Typed measurement whose result is recorded under a stable ID.
    Measurement(PlannedMeasurement),
    /// Command with one or more typed arguments resolved from measurements.
    DeferredCommand(PlannedDeferredCommand),
}

impl PlannedExecutionStep {
    /// Logical output path, when path-backed.
    #[must_use]
    pub fn output_path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Command(command) => command.output.as_path(),
            Self::Measurement(measurement) => measurement.command.output.as_path(),
            Self::DeferredCommand(command) => command.output.as_path(),
        }
    }
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
        /// Legacy/static planned command list. Empty for a native-v2 Reference plan.
        commands: Vec<PlannedCommand>,
        /// Measurement-aware execution steps. Empty for existing legacy/static plans.
        #[cfg_attr(feature = "serde", serde(default))]
        steps: Vec<PlannedExecutionStep>,
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
    /// Native-v2 Reference policy facts, absent for legacy/general plans.
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
                steps: Vec::new(),
                cleanup_paths,
                finalization,
            },
            reference: None,
        }
    }

    /// Create a measurement-aware Reference plan.
    #[must_use]
    pub fn execute_steps_with_cleanup(
        steps: Vec<PlannedExecutionStep>,
        cleanup_paths: Vec<PathBuf>,
        finalization: Option<Finalization>,
        reference: DsdReferencePlanSummary,
    ) -> Self {
        Self {
            action: PlanAction::Execute {
                commands: Vec::new(),
                steps,
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

    /// Return measurement-aware steps, or an empty slice for legacy plans.
    #[must_use]
    pub fn steps(&self) -> &[PlannedExecutionStep] {
        match &self.action {
            PlanAction::PassthroughCopy { .. } => &[],
            PlanAction::Execute { steps, .. } => steps,
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
    /// ReplayGain scan/tag command.
    ReplayGain {
        /// Target format being tagged.
        target_format: AudioFormat,
        /// ReplayGain mode.
        mode: ReplayGainMode,
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
            Self::ReplayGain { .. } => "replaygain",
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
    if request.source.is_dsd()
        && !request.settings.target_format.is_dsd()
        && request.settings.dsd.is_native_v2()
    {
        return plan_reference_dsd(request);
    }
    match plan_topology(request)? {
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
            let mut commands = Vec::with_capacity(steps.len());
            for step in &steps {
                commands.push(registry.build_command(&context, step)?);
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
            None | Some("m4a" | "mp4") => Ok(()),
            Some("aac") => Err(PlanningError::invalid_settings(
                "output_path",
                "AAC output is muxed as MP4/M4A by this pipeline; raw .aac output is not implemented, so use .m4a/.mp4 or add an explicit raw-AAC mode",
            )),
            Some(_) => Err(PlanningError::invalid_settings(
                "output_path",
                "AAC output must use an .m4a or .mp4 container extension unless an explicit raw-AAC mode is implemented",
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

fn validate_request_semantics(request: &PlanRequest) -> Result<()> {
    if request.settings.ssrc.force
        && (request.source.is_dsd()
            || request.settings.target_format.is_dsd()
            || rate_change_for_pcm(request).is_none())
    {
        return Err(PlanningError::invalid_settings(
            "ssrc.force",
            "forced SSRC requires a PCM source, a PCM target, and an actual PCM sample-rate change",
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
            lowpass: request.settings.dsd.legacy_dsd_to_pcm_lowpass(),
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
    let target_rate_hz = match request.settings.target_sample_rate {
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
    let target_depth = resolve_target_bit_depth(request)?;
    reject_unsupported_resolved_depth(&request.settings.target_format, target_depth)?;

    // Combos sox silently substitutes (FLAC 32-bit -> 24; AIFF float -> int)
    // must NOT take the direct sox DsdToPcm-to-final branch: route them
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
                lowpass: request.settings.dsd.legacy_dsd_to_pcm_lowpass(),
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
            lowpass: request.settings.dsd.legacy_dsd_to_pcm_lowpass(),
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
        None,
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
    let processing_rate = rate_change_for_pcm(request);
    let target_depth = resolve_target_bit_depth(request)?;
    reject_unsupported_resolved_depth(&request.settings.target_format, target_depth)?;
    let depth_change = match request.settings.target_bit_depth {
        BitDepthTarget::Source => false,
        BitDepthTarget::Pcm(depth) => request.source.bit_depth != Some(depth),
    };
    let needs_processing = processing_rate.is_some() || depth_change;
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

        let ssrc_path = context.intermediate_path(steps.len(), "wav");
        let profile =
            mapping::ssrc_profile(request.settings.ssrc, request.settings.resample_quality);
        push_step(
            steps,
            PlanOperation::ResamplePcm {
                target_rate_hz: ssrc_target_rate_hz,
                target_bit_depth: Some(target_depth),
                profile: Some(profile),
                brick_wall: true,
            },
            current_input.clone(),
            OutputSink::Path(ssrc_path.clone()),
            "Brick-wall PCM resampling with SSRC",
        );
        *current_input = InputSource::Path(ssrc_path);
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

    let needs_sox_preprocess = request.settings.dither_type != DitherType::None
        && mapping::requires_sox_dither(request.settings.dither_type)
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
            None,
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
        processing_rate,
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
    if let Some(mode) = request.settings.replay_gain.mode {
        push_step(
            steps,
            PlanOperation::ReplayGain {
                target_format: request.settings.target_format.clone(),
                mode,
            },
            InputSource::Path(current_output_path.clone()),
            OutputSink::InPlace(current_output_path.clone()),
            "ReplayGain scan",
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
        (AudioFormat::WavPack, PcmBitDepth::Float32 | PcmBitDepth::Float64) => {
            Err(PlanningError::invalid_settings(
                "target_bit_depth",
                "WavPack float output is not supported by the conversion carrier; choose 32-bit integer or WAV",
            ))
        }
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

fn rate_change_for_pcm(request: &PlanRequest) -> Option<u32> {
    match request.settings.target_sample_rate {
        RateTarget::Source | RateTarget::Dsd(_) => None,
        RateTarget::PcmHz(hz) if request.source.sample_rate_hz == Some(hz) => None,
        RateTarget::PcmHz(hz) => Some(hz),
    }
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
mod resolved_depth_rejection_tests {
    use super::*;
    use crate::enums::{AudioFormat, PcmBitDepth};

    #[test]
    fn alac_int32_resolved_from_source_is_rejected_at_plan_time() {
        // The settings validator only sees BitDepthTarget::Pcm; a Source
        // target over a 32-bit source resolves AFTER validation and must be
        // rejected here instead of silently downgrading at the encoder.
        let err = reject_unsupported_resolved_depth(&AudioFormat::Alac, PcmBitDepth::Int32)
            .expect_err("resolved ALAC Int32 must fail closed");
        assert!(err.to_string().contains("ALAC 32-bit"), "{err}");
    }

    #[test]
    fn honored_resolved_depths_pass() {
        reject_unsupported_resolved_depth(&AudioFormat::Alac, PcmBitDepth::Int24).expect("alac 24");
        reject_unsupported_resolved_depth(&AudioFormat::Flac, PcmBitDepth::Int32).expect("flac 32");
        reject_unsupported_resolved_depth(&AudioFormat::WavPack, PcmBitDepth::Int32).expect("wv 32");
        reject_unsupported_resolved_depth(&AudioFormat::Aiff, PcmBitDepth::Float32).expect("aiff f32");
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
    use crate::source::{SourceInfo, SourceRepresentationKind};
    use crate::tools::{MetadataDisposition, ToolIdentifier, ToolPlugin, ToolSupport};
    use std::path::PathBuf;

    #[derive(Debug, Clone, Copy)]
    struct MetadataPruningPlugin {
        encode_effect: MetadataPlanEffect,
        disposition: MetadataDisposition,
    }

    impl ToolPlugin for MetadataPruningPlugin {
        fn id(&self) -> ToolIdentifier {
            ToolIdentifier::Custom("metadata-pruning-test".into())
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
                audio_md5: None,
            },
            settings,
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
