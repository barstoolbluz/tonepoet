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
