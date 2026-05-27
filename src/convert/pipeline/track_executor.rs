//! Planned per-track conversion executor.
//!
//! This module is the only per-track conversion executor. The planner selects either
//! passthrough copy or a sequential command chain; this module executes that
//! result through `ToolRunner` and applies deterministic finalization.

use std::collections::HashMap;
use std::{fs, io};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tonepoet_pipeline::{
    plan_conversion, plan_topology, Finalization, MetadataDisposition,
    PlanAction, PlanOperation, PlanRequest, PlanStep, PlannedCommand,
    ToolRegistry, TopologyPlan,
};

use super::errors::{ConvertError, ToolRunnerError};
use super::plan_bridge::{plan_request_for_track, settings_request_metadata};
use super::planned_adapter::{planned_command_to_tool_command, DEFAULT_PLANNED_COMMAND_TIMEOUT};
use super::progress::{
    probes, run_streaming_tool_with_probe_with_tool_paths, OperationProgressTracker,
    StreamSource, StreamingHeartbeat,
};
use super::tool::{CommandRecord, EnvVar, ProcessExit, ToolBinary, ToolCommand, ToolOutput, ToolRunner};
use super::types::{PipelineRequest, PreparedTrack};

#[derive(Debug, Clone)]
pub struct ExecutedTrackPlan {
    pub commands: Vec<CommandRecord>,
    pub elapsed: Duration,
    /// Effective post-planner metadata disposition for this track. This is
    /// `WritesRequestedPolicy` only when the planner request asked for metadata
    /// and the resulting plan will satisfy that policy by passthrough, metadata
    /// commands, or disposition-pruned encoder behavior.
    pub metadata_disposition: MetadataDisposition,
    pub metadata_written_by_plan: bool,
    /// SHA-256 of the planned command sequence, for manifest rerun identity.
    pub command_hash: Option<String>,
}

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
) -> Result<ExecutedTrackPlan, ConvertError> {
    let work_dir = convert_root.join(format!(".track-{:04}.work", track.id.source_ordinal));
    reset_track_work_dir(&work_dir)?;

    let plan_request = plan_request_for_track(
        request,
        track,
        realized_input,
        staged_output,
        work_dir.clone(),
    )?;
    let plan = plan_conversion(&plan_request)
        .map_err(|err| ConvertError::Backend(format!("planner failed: {err}")))?;
    let command_hash = super::manifest::planned_command_hash(&plan).ok();
    let metadata_disposition = effective_metadata_disposition(&plan_request)?;
    let metadata_written_by_plan = metadata_disposition.writes_requested_policy();

    cleanup_paths(plan.cleanup_paths());
    let started = Instant::now();
    let result = match &plan.action {
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
                metadata_disposition,
                metadata_written_by_plan,
                command_hash: command_hash.clone(),
            })
        }
        PlanAction::Execute {
            commands,
            finalization,
            ..
        } => {
            let commands = execute_commands(
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
            .await?;
            if let Some(finalization) = finalization {
                apply_finalization(finalization)?;
            }
            progress
                .estimated_with_key(
                    end_fraction,
                    format!("plan-finish:{}", track.id.source_ordinal),
                    format!("Finished track {}", track.id.source_ordinal),
                )
                .await;
            Ok(ExecutedTrackPlan {
                commands,
                elapsed: started.elapsed(),
                metadata_disposition,
                metadata_written_by_plan,
                command_hash,
            })
        }
    };

    match result {
        Ok(value) => {
            cleanup_paths(plan.cleanup_paths());
            cleanup_track_work_dir(&work_dir);
            Ok(value)
        }
        Err(err) => {
            cleanup_paths(plan.cleanup_paths());
            cleanup_track_work_dir(&work_dir);
            Err(err)
        }
    }
}

fn effective_metadata_disposition(
    plan_request: &PlanRequest,
) -> Result<MetadataDisposition, ConvertError> {
    if !settings_request_metadata(&plan_request.settings) {
        return Ok(MetadataDisposition::DoesNotWrite);
    }

    let topology = plan_topology(plan_request)
        .map_err(|err| ConvertError::Backend(format!("metadata topology inspection failed: {err}")))?;
    let registry = ToolRegistry::with_builtin_tools();
    let context = plan_request.context();

    match topology {
        TopologyPlan::Passthrough { .. } => Ok(MetadataDisposition::WritesRequestedPolicy),
        TopologyPlan::Execute { steps, .. } => {
            let needs_tag_or_artwork = plan_request.settings.metadata.transfer_tags
                || plan_request.settings.metadata.preserve_artwork;
            let needs_source_md5 = plan_request.settings.metadata.store_source_audio_md5;

            let mut tag_or_artwork_satisfied = !needs_tag_or_artwork;
            let mut source_md5_satisfied = !needs_source_md5;

            for (index, step) in steps.iter().enumerate() {
                if needs_tag_or_artwork && operation_can_write_metadata(&step.operation) {
                    let disposition = registry
                        .metadata_disposition_for_step(&context, step)
                        .map_err(|err| ConvertError::Backend(format!(
                            "metadata disposition lookup failed: {err}"
                        )))?;
                    if disposition.writes_requested_policy() {
                        tag_or_artwork_satisfied = true;
                    }
                }

                match &step.operation {
                    PlanOperation::MetadataTransfer { .. } => {
                        let explicit_transfer = registry
                            .metadata_disposition_for_step(&context, step)
                            .map_err(|err| ConvertError::Backend(format!(
                                "metadata disposition lookup failed: {err}"
                            )))?;
                        let previous_writer = previous_metadata_writer(&steps[..index], &registry, &context)?;
                        let transfer_satisfies_policy = explicit_transfer.writes_requested_policy()
                            || matches!(
                                previous_writer.as_ref(),
                                Some(disposition) if disposition.writes_requested_policy()
                            );
                        if transfer_satisfies_policy {
                            tag_or_artwork_satisfied = true;
                            log::debug!(
                                "metadata transfer for step {} is satisfied by explicit {:?} or prior {:?} disposition",
                                step.index,
                                explicit_transfer,
                                previous_writer
                            );
                        } else {
                            log::debug!(
                                "metadata transfer for step {} does not satisfy requested metadata policy; legacy metadata stage remains required",
                                step.index
                            );
                        }
                    }
                    PlanOperation::StoreSourceAudioMd5 { .. } => {
                        source_md5_satisfied = true;
                    }
                    _ => {}
                }
            }

            if tag_or_artwork_satisfied && source_md5_satisfied {
                Ok(MetadataDisposition::WritesRequestedPolicy)
            } else {
                Ok(MetadataDisposition::DoesNotWrite)
            }
        }
    }
}

fn previous_metadata_writer(
    previous_steps: &[PlanStep],
    registry: &ToolRegistry,
    context: &tonepoet_pipeline::PlanContext<'_>,
) -> Result<Option<MetadataDisposition>, ConvertError> {
    for step in previous_steps.iter().rev() {
        if !operation_can_write_metadata(&step.operation) {
            continue;
        }
        let disposition = registry
            .metadata_disposition_for_step(context, step)
            .map_err(|err| ConvertError::Backend(format!("metadata disposition lookup failed: {err}")))?;
        if disposition.writes_requested_policy() {
            return Ok(Some(disposition));
        }
    }
    Ok(None)
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
) -> Result<Vec<CommandRecord>, ConvertError> {
    let mut records = Vec::with_capacity(commands.len());
    let windows = command_windows(commands, start_fraction, end_fraction);

    for (index, planned) in commands.iter().enumerate() {
        if cancel.is_cancelled() {
            progress.cancel_requested().await;
            return Err(ConvertError::Realize("cancelled".to_string()));
        }
        let (window_start, window_end) = windows[index];
        let label = format!(
            "{} - step {} of {} - {}",
            track_label,
            index + 1,
            commands.len(),
            planned.description
        );
        progress
            .estimated_with_key(
                window_start,
                format!("cmd-start:{index}"),
                format!("Starting {label}"),
            )
            .await;

        let mut cmd = planned_command_to_tool_command(planned, DEFAULT_PLANNED_COMMAND_TIMEOUT)?;
        let output = {
            let _tool_permit =
                acquire_tool_permit(&mut cmd, tool_concurrency_limits.as_ref(), cancel).await?;
            run_planned_command(
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
            .map_err(|err| format_tool_error(index, planned, err))?
        };
        records.push(output.command);

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
            set_sox_omp_threads(cmd, limits.sox_omp_threads());
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
        binary: cmd.binary,
        sanitized_args: cmd.args.clone(),
        cwd: cmd.cwd.clone(),
        env_keys: cmd.env.iter().map(|var| var.key.clone()).collect(),
        exit: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        elapsed: Duration::ZERO,
    }
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
            binary: cmd.binary,
            sanitized_args: cmd.args.clone(),
            cwd: cmd.cwd.clone(),
            env_keys: cmd.env.iter().map(|var| var.key.clone()).collect(),
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

fn reset_track_work_dir(work_dir: &Path) -> Result<(), ConvertError> {
    if work_dir.exists() {
        fs::remove_dir_all(work_dir)?;
    }
    fs::create_dir_all(work_dir)?;
    Ok(())
}

fn cleanup_track_work_dir(work_dir: &Path) {
    // Best-effort: all planner-declared intermediate files are already removed
    // above. Removing the deterministic per-track directory prevents interrupted
    // runs from leaving orphan scratch trees, while ignoring errors avoids
    // masking the primary conversion outcome.
    let _ = fs::remove_dir_all(work_dir);
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

fn cleanup_paths(paths: &[PathBuf]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn format_tool_error(
    index: usize,
    planned: &PlannedCommand,
    err: ToolRunnerError,
) -> ConvertError {
    match err {
        ToolRunnerError::NonZeroExit { stderr_tail, .. } => ConvertError::Backend(format!(
            "planned command {} failed ({}): {}",
            index + 1,
            planned.description,
            stderr_tail
        )),
        ToolRunnerError::Timeout { elapsed, .. } => ConvertError::Backend(format!(
            "planned command {} timed out after {:?} ({})",
            index + 1,
            elapsed,
            planned.description
        )),
        ToolRunnerError::Cancelled { .. } => ConvertError::Realize("cancelled".to_string()),
        other => ConvertError::Tool(other),
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
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use crate::convert::pipeline::tool::blocking_test_runner::{
        tool_gate, BlockingToolRunner, ToolBehavior,
    };
    use crate::convert::pipeline::types::PipelineStage;
    use tempfile::TempDir;
    use tonepoet_pipeline::{InputSource, OutputSink, ToolIdentifier};

    use super::*;

    fn test_tool_command(binary: ToolBinary) -> ToolCommand {
        ToolCommand {
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

    async fn execute_commands_for_test(
        commands: Vec<PlannedCommand>,
        runner: &dyn ToolRunner,
        cancel: &CancellationToken,
        limits: Option<Arc<ToolConcurrencyLimits>>,
    ) -> Result<Vec<CommandRecord>, ConvertError> {
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
        cleanup_track_work_dir(&root);
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

        cleanup_paths(&paths);
        cleanup_paths(&paths);

        assert!(!first.exists());
        assert!(!second.exists());
        cleanup_track_work_dir(&root);
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
        cleanup_track_work_dir(&root);
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
        cleanup_track_work_dir(&root);
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
        PipelineStage, TrackId, TrackMetadata, TrackOutcome, TrackRecord, TrackSourceRef,
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
    ) -> Result<Vec<CommandRecord>, ConvertError> {
        reset_track_work_dir(&chain.work_dir)?;
        cleanup_paths(chain.plan.cleanup_paths());
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
                            apply_finalization(finalization)?;
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
                cleanup_paths(chain.plan.cleanup_paths());
                cleanup_track_work_dir(&chain.work_dir);
                Ok(records)
            }
            Err(err) => {
                cleanup_paths(chain.plan.cleanup_paths());
                cleanup_track_work_dir(&chain.work_dir);
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
            sample_rate: 44_100,
            bit_depth: Some(16),
        }
    }

    fn scheduled_failure_output_for_chain(
        chain: &SyntheticChain,
        track: &PreparedTrack,
        error: &ConvertError,
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
            },
            artifact: None,
            ok: false,
            metadata_written_by_plan: false,
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
                runner.transcript(),
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
