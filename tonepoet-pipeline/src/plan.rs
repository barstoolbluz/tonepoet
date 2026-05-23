//! Deterministic conversion-chain planner.

use crate::enums::{
    AudioCodec, AudioFormat, BitDepthTarget, DitherType, DsdFilterPreset, DsdLowpassMethod,
    DsdRate, NyquistTransition, PcmBitDepth, RateTarget, ReplayGainMode, SampleKind, SsrcProfile,
};
use crate::error::{PlanningError, Result};
use crate::mapping;
use crate::settings::{
    default_pcm_depth_for_format, FlacSettings, PipelineSettings, WavPackSettings,
};
use crate::source::SourceInfo;
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
        let extension = self.request.settings.target_format.extension();
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
        /// Planned command list.
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
    let mut index = 0;

    while index < pruned.len() {
        if !matches!(
            pruned[index].operation,
            PlanOperation::MetadataTransfer { .. }
        ) {
            index += 1;
            continue;
        }

        let Some(previous_index) = pruned[..index]
            .iter()
            .rposition(|candidate| operation_can_write_metadata(&candidate.operation))
        else {
            index += 1;
            continue;
        };

        let previous_step = &pruned[previous_index];
        let disposition = registry.metadata_disposition_for_step(context, previous_step)?;
        if !disposition.writes_requested_policy() {
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
        let Some(to_path) = previous_step
            .output
            .as_path()
            .map(std::path::Path::to_path_buf)
        else {
            index += 1;
            continue;
        };

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
    }

    Ok((pruned, adjusted_finalization))
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

fn operation_can_write_metadata(operation: &PlanOperation) -> bool {
    matches!(
        operation,
        PlanOperation::EncodePcm { .. }
            | PlanOperation::EncodeLossy { .. }
            | PlanOperation::PcmToDsd { .. }
            | PlanOperation::DsdToPcm { .. }
            | PlanOperation::DsdRateChange { .. }
            | PlanOperation::MetadataTransfer { .. }
    )
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
    if settings.force_encode || settings.dither_type != DitherType::None {
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
        AudioFormat::Mp3 | AudioFormat::Aac | AudioFormat::Opus => false,
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
        AudioFormat::Mp3 | AudioFormat::Aac | AudioFormat::Opus | AudioFormat::Custom { .. } => {
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
            lowpass: request.settings.dsd.dsd_to_pcm_lowpass,
        }
    } else {
        PlanOperation::PcmToDsd {
            target_format: request.settings.target_format.clone(),
            target_rate,
            filter: request.settings.dsd.pcm_to_dsd_filter,
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
    let target_depth = resolve_target_bit_depth(request);

    if request.settings.target_format.is_pcm_lossless()
        && request.settings.target_format.sox_encodable()
    {
        push_step(
            steps,
            PlanOperation::DsdToPcm {
                target_format: request.settings.target_format.clone(),
                target_rate_hz,
                target_bit_depth: target_depth,
                lowpass: request.settings.dsd.dsd_to_pcm_lowpass,
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
            lowpass: request.settings.dsd.dsd_to_pcm_lowpass,
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
    let target_depth = resolve_target_bit_depth(request);
    let depth_change = match request.settings.target_bit_depth {
        BitDepthTarget::Source => false,
        BitDepthTarget::Pcm(depth) => request.source.bit_depth != Some(depth),
    };
    let needs_processing = processing_rate.is_some()
        || depth_change
        || request.settings.dither_type != DitherType::None;
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
            "Encode PCM output",
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
        let next =
            context.intermediate_path(steps.len(), request.settings.target_format.extension());
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

fn resolve_target_bit_depth(request: &PlanRequest) -> PcmBitDepth {
    match request.settings.target_bit_depth {
        BitDepthTarget::Source => request
            .source
            .bit_depth
            .unwrap_or_else(|| default_pcm_depth_for_format(&request.settings.target_format)),
        BitDepthTarget::Pcm(depth) => depth,
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
