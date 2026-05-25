//! Planned per-track conversion executor.
//!
//! This module is the only per-track conversion executor. The planner selects either
//! passthrough copy or a sequential command chain; this module executes that
//! result through `ToolRunner` and applies deterministic finalization.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
use super::tool::{CommandRecord, ToolBinary, ToolCommand, ToolRunner};
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

        let cmd = planned_command_to_tool_command(planned, DEFAULT_PLANNED_COMMAND_TIMEOUT)?;
        let output = run_planned_command(
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
        .map_err(|err| format_tool_error(index, planned, err))?;
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
    use std::path::PathBuf;

    use tonepoet_pipeline::{InputSource, OutputSink, ToolIdentifier};

    use super::*;

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
