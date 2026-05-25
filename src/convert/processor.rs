//! Audio conversion processor backed by one shared pipeline scheduler.
//!
//! Queue items materialize first. Each materialized track then enters the same
//! worker pool as every other track from every other item. Album-level stages
//! run only after the scheduler has observed completion of all tracks for that
//! album.

use super::{
    ConversionError, ConversionItem, ConversionPhase, ConversionQueue, ConversionResult,
    ConversionStatus,
};
use log::info;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, Semaphore};
use tokio_util::sync::CancellationToken;
use tonepoet_pipeline::PipelineSettings;

use crate::convert::pipeline::{
    boxed_work, build_pipeline_request, build_pipeline_request_from_settings, detect_source_kind, encode_realized_track_for_scheduler,
    encode_track_for_scheduler, finish_pipeline_album_for_scheduler, map_album_outcome,
    prepare_pipeline_item_for_scheduler, realize_track_for_scheduler, run_pipeline_item_with_tool_paths, scheduled_worker_failure_output, AlbumCompletionTracker,
    AlbumReadiness, BroadcastReporter, PipelineRequest, RealToolRunner, ScheduledAlbum,
    ScheduledMaterialization, ScheduledRealizedTrack, ScheduledTrackOutput, SharedWorkerPool,
    SourceKind, TrackSourceRef, WorkKind, WorkUnit,
};

/// Configuration for the conversion processor.
pub struct ProcessorConfig {
    /// Number of shared workers. The default is supplied by the UI/config layer.
    pub worker_count: usize,
    /// Paths to external tools.
    pub tool_paths: HashMap<String, PathBuf>,
    /// Default destination directory for converted output.
    pub default_destination_directory: Option<PathBuf>,
    /// Local scratch directory for extraction.
    pub scratch_directory: Option<PathBuf>,
}

/// Handles the actual conversion of audio files.
pub struct ConversionProcessor {
    config: ProcessorConfig,
    progress_tx: Option<broadcast::Sender<ProgressUpdate>>,
}

/// Progress update from a worker.
#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    pub item_id: String,
    pub progress: f32,
    pub status: ConversionStatus,
}

/// Helper to send phase progress updates.
async fn send_phase_update(
    tx: &broadcast::Sender<ProgressUpdate>,
    item_id: &str,
    phase: ConversionPhase,
    phase_progress: f32,
    message: Option<String>,
    file_progress: Option<(u32, u32)>,
) {
    let overall_progress = phase.calculate_overall_progress(phase_progress);

    let _ = tx.send(ProgressUpdate {
        item_id: item_id.to_string(),
        progress: overall_progress,
        status: ConversionStatus::Processing {
            progress: overall_progress,
            message,
            file_progress,
            phase: Some(phase),
            phase_progress: Some(phase_progress),
        },
    });
}

fn terminal_progress_for_status(
    status: &ConversionStatus,
    last_known_progress: Option<f32>,
) -> f32 {
    match status {
        ConversionStatus::Processing { progress, .. } => *progress,
        ConversionStatus::Completed { .. } | ConversionStatus::Partial { .. } => 100.0,
        ConversionStatus::Failed { .. } | ConversionStatus::Cancelled => {
            last_known_progress.unwrap_or(0.0).clamp(0.0, 100.0)
        }
        ConversionStatus::Queued | ConversionStatus::Paused | ConversionStatus::NotConfigured => 0.0,
    }
}

impl ConversionProcessor {
    /// Create a new processor.
    pub fn new(config: ProcessorConfig) -> Self {
        Self {
            config,
            progress_tx: None,
        }
    }

    /// Set progress channel.
    pub fn set_progress_channel(&mut self, tx: broadcast::Sender<ProgressUpdate>) {
        self.progress_tx = Some(tx);
    }

    /// Process the conversion queue.
    pub async fn process_queue(&mut self, queue: &mut ConversionQueue) -> ConversionResult<()> {
        let queue_arc = std::sync::Arc::new(tokio::sync::RwLock::new(std::mem::take(queue)));
        let result = self.process_queue_with_progress_arc(queue_arc.clone(), None).await;
        *queue = std::mem::take(&mut *queue_arc.write().await);
        result
    }

    /// Process the conversion queue with fine-grained locking for UI updates.
    pub async fn process_queue_with_progress(
        &mut self,
        queue: std::sync::Arc<tokio::sync::RwLock<ConversionQueue>>,
        progress_rx: Option<broadcast::Receiver<ProgressUpdate>>,
    ) -> ConversionResult<()> {
        self.process_queue_with_progress_arc(queue, progress_rx).await
    }

    async fn process_queue_with_progress_arc(
        &mut self,
        queue: std::sync::Arc<tokio::sync::RwLock<ConversionQueue>>,
        progress_rx: Option<broadcast::Receiver<ProgressUpdate>>,
    ) -> ConversionResult<()> {
        info!("ConversionProcessor::process_queue starting shared pipeline scheduler");

        let progress_tx = if let Some(tx) = &self.progress_tx {
            tx.clone()
        } else {
            let (tx, _rx) = broadcast::channel::<ProgressUpdate>(100);
            self.progress_tx = Some(tx.clone());
            tx
        };

        let mut queued_items: Vec<_> = {
            let q = queue.read().await;
            if let Err(err) = q.validate_full_settings_handoff() {
                return Err(ConversionError::ValidationError(err));
            }
            q.queued_items().into_iter().cloned().collect()
        };
        if queued_items.is_empty() {
            return Ok(());
        }

        for item in queued_items.iter_mut() {
            if item.options.output_dir.is_none() {
                if let Some(ref default_dest) = self.config.default_destination_directory {
                    item.options.output_dir = Some(default_dest.clone());
                }
            }
            let mut q = queue.write().await;
            if let Some(queue_item) = q.find_item_mut(&item.id) {
                queue_item.status = ConversionStatus::Processing {
                    progress: 0.0,
                    message: Some(format!("Starting conversion to {}", item.output_format)),
                    file_progress: None,
                    phase: Some(ConversionPhase::Extracting),
                    phase_progress: Some(0.0),
                };
            }
        }

        let outcomes = run_queue_with_shared_orchestrator(
            queued_items,
            progress_tx.clone(),
            progress_rx,
            self.config.tool_paths.clone(),
            self.config.worker_count.max(1),
        )
        .await;

        for (item_id, final_status, last_progress) in outcomes {
            let progress = terminal_progress_for_status(&final_status, last_progress);
            let _ = progress_tx.send(ProgressUpdate {
                item_id: item_id.clone(),
                progress,
                status: final_status.clone(),
            });
            let mut q = queue.write().await;
            if let Some(queue_item) = q.find_item_mut(&item_id) {
                queue_item.status = final_status;
                queue_item.completed_at = Some(chrono::Utc::now());
            }
        }

        Ok(())
    }
}

struct PendingAlbum {
    album: Option<ScheduledAlbum>,
    outputs: Vec<ScheduledTrackOutput>,
    finished: usize,
    expected: usize,
    job_cancel: CancellationToken,
    cancel_requested: bool,
}

enum QueueWorkOutput {
    Materialized {
        item_id: String,
        result: ScheduledMaterialization,
    },
    Realized {
        job_id: String,
        track: ScheduledRealizedTrack,
    },
    Encoded {
        job_id: String,
        output: ScheduledTrackOutput,
    },
    PostProcessed {
        item_id: String,
        status: ConversionStatus,
    },
}

async fn run_queue_with_shared_orchestrator(
    queued_items: Vec<ConversionItem>,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    mut progress_rx: Option<broadcast::Receiver<ProgressUpdate>>,
    tool_paths: HashMap<String, PathBuf>,
    worker_count: usize,
) -> Vec<(String, ConversionStatus, Option<f32>)> {
    let cancel = CancellationToken::new();
    let pool = SharedWorkerPool::<QueueWorkOutput>::new(Some(worker_count.max(1)), cancel.clone());
    let mut run = pool.start();
    let mut pending_albums: BTreeMap<String, PendingAlbum> = BTreeMap::new();
    let mut tracker = AlbumCompletionTracker::default();
    let mut terminal: BTreeMap<String, ConversionStatus> = BTreeMap::new();
    let mut job_to_item: BTreeMap<String, String> = BTreeMap::new();
    let mut last_progress_by_item: HashMap<String, f32> = HashMap::new();
    let total_items = queued_items.len();

    for item in queued_items {
        let item_id = item.id.clone();
        if !item.input_path.exists() {
            terminal.insert(
                item_id.clone(),
                ConversionStatus::Failed {
                    error: format!("Source file not found: {}", item.input_path.display()),
                    log_path: None,
                },
            );
            continue;
        }

        let request = match build_pipeline_request(&item) {
            Ok(mut req) => {
                req.worker_count = Some(worker_count.max(1));
                req
            }
            Err(err) => {
                terminal.insert(
                    item_id.clone(),
                    ConversionStatus::Failed {
                        error: err.to_string(),
                        log_path: None,
                    },
                );
                continue;
            }
        };

        job_to_item.insert(request.job_id.clone(), item_id.clone());

        let source_kind = detect_source_kind(&request).ok();
        if matches!(source_kind, Some(SourceKind::SingleFile)) {
            submit_single_file_work(&pool, request, &tool_paths, &progress_tx).await;
            continue;
        }

        let materialize_kind = match source_kind {
            Some(SourceKind::SevenZip) => WorkKind::ArchiveExtract,
            Some(SourceKind::CueImage) => WorkKind::MaterializeItem,
            Some(SourceKind::SacdIso) => WorkKind::MaterializeItem,
            Some(SourceKind::SingleFile) => unreachable!("single files are submitted as immediate work units"),
            None => WorkKind::MaterializeItem,
        };
        let unit_prefix = match source_kind {
            Some(SourceKind::SevenZip) => "archive-extract",
            Some(SourceKind::CueImage) => "cue-materialize",
            Some(SourceKind::SacdIso) => "sacd-materialize",
            Some(SourceKind::SingleFile) => unreachable!("single files are submitted as immediate work units"),
            None => "materialize",
        };
        let submit_tool_paths = tool_paths.clone();
        let submit_progress_tx = progress_tx.clone();
        let submit_item_id = item_id.clone();
        pool.submit(WorkUnit {
            job_id: request.job_id.clone(),
            unit_id: format!("{unit_prefix}:{submit_item_id}"),
            kind: materialize_kind,
            task: boxed_work(move |worker_cancel| async move {
                let runner = RealToolRunner::new(submit_tool_paths.clone());
                let reporter = BroadcastReporter::new(submit_progress_tx, submit_item_id.clone());
                let result = prepare_pipeline_item_for_scheduler(
                    request,
                    &runner,
                    &reporter,
                    &worker_cancel,
                    &submit_tool_paths,
                )
                .await;
                Ok(QueueWorkOutput::Materialized {
                    item_id: submit_item_id,
                    result,
                })
            }),
        })
        .await;
    }

    while terminal.len() < total_items {
        tokio::select! {
            Ok(update) = async {
                if let Some(ref mut rx) = progress_rx {
                    rx.recv().await
                } else {
                    std::future::pending::<Result<ProgressUpdate, broadcast::error::RecvError>>().await
                }
            }, if progress_rx.is_some() => {
                last_progress_by_item.insert(update.item_id.clone(), update.progress);
            }
            Some(result) = run.results.recv() => {
                match result.outcome {
                    Ok(QueueWorkOutput::Materialized { item_id, result }) => {
                        match result {
                            ScheduledMaterialization::Finished(report) => {
                                let status = map_album_outcome(
                                    &report.outcome,
                                    report.published.as_ref(),
                                    report.durable_log.as_deref(),
                                );
                                terminal.insert(item_id, status);
                            }
                            ScheduledMaterialization::Ready(album) => {
                                let job_id = album.req.job_id.clone();
                                let expected = album
                                    .source
                                    .tracks
                                    .iter()
                                    .filter(|track| album.planned_final_path(&track.id).is_some())
                                    .count();
                                tracker.register_album(job_id.clone(), expected, album.allow_partial());
                                if expected == 0 {
                                    submit_album_postprocess(
                                        &pool,
                                        album,
                                        Vec::new(),
                                        &tool_paths,
                                        &progress_tx,
                                    )
                                    .await;
                                } else {
                                    let job_cancel = cancel.child_token();
                                    pending_albums.insert(job_id.clone(), PendingAlbum {
                                        album: Some(album),
                                        outputs: Vec::with_capacity(expected),
                                        finished: 0,
                                        expected,
                                        job_cancel: job_cancel.clone(),
                                        cancel_requested: false,
                                    });
                                    if let Some(pending) = pending_albums.get(&job_id) {
                                        if let Some(album) = pending.album.as_ref() {
                                            submit_album_source_work(
                                                &pool,
                                                album,
                                                &tool_paths,
                                                &progress_tx,
                                                job_cancel,
                                            )
                                            .await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(QueueWorkOutput::Realized { job_id, track }) => {
                        if pending_albums.contains_key(&job_id) {
                            let job_cancel = pending_albums
                                .get(&job_id)
                                .map(|pending| pending.job_cancel.clone())
                                .unwrap_or_else(|| cancel.child_token());
                            submit_realized_encode_work(
                                &pool,
                                job_id,
                                track,
                                &tool_paths,
                                &progress_tx,
                                job_cancel,
                            )
                            .await;
                        }
                    }
                    Ok(QueueWorkOutput::Encoded { job_id, output }) => {
                        let readiness = tracker.mark_track_finished(&job_id, output.ok);
                        let mut submit_postprocess: Option<(ScheduledAlbum, Vec<ScheduledTrackOutput>)> = None;

                        if let Some(pending) = pending_albums.get_mut(&job_id) {
                            pending.finished += 1;
                            pending.outputs.push(output);

                            match readiness {
                                AlbumReadiness::Waiting { .. } | AlbumReadiness::ReadyForPostProcess => {}
                                AlbumReadiness::Failed { .. } => {
                                    // Do not remove the pending album on the first failure.
                                    // Fail-fast policy cancels not-yet-started and in-flight units,
                                    // but each expected track must still report a deterministic
                                    // terminal ScheduledTrackOutput before album post-processing,
                                    // durable logging, and queue accounting run.
                                    if !pending.cancel_requested {
                                        pending.job_cancel.cancel();
                                        pending.cancel_requested = true;
                                    }
                                }
                            }

                            if pending.finished >= pending.expected {
                                if let Some(mut pending) = pending_albums.remove(&job_id) {
                                    pending.outputs.sort_by_key(|output| output.index);
                                    if let Some(album) = pending.album.take() {
                                        submit_postprocess = Some((album, pending.outputs));
                                    }
                                }
                            }
                        } else {
                            log::error!(
                                "encoded track output arrived after album accounting closed: job={} track_index={}",
                                job_id,
                                output.index
                            );
                        }

                        if let Some((album, outputs)) = submit_postprocess {
                            submit_album_postprocess(
                                &pool,
                                album,
                                outputs,
                                &tool_paths,
                                &progress_tx,
                            )
                            .await;
                        }
                    }
                    Ok(QueueWorkOutput::PostProcessed { item_id, status }) => {
                        terminal.insert(item_id, status);
                    }
                    Err(err) => {
                        let item_id = job_to_item
                            .get(&result.job_id)
                            .cloned()
                            .unwrap_or_else(|| result.job_id.clone());
                        log::error!(
                            "shared scheduler work unit failed: job={} unit={} kind={:?}: {err}",
                            result.job_id,
                            result.unit_id,
                            result.kind
                        );
                        terminal.entry(item_id).or_insert_with(|| ConversionStatus::Failed {
                            error: format!("scheduler work unit failed: {err}"),
                            log_path: None,
                        });
                    }
                }
            }
            else => break,
        }
    }

    cancel.cancel();
    run.shutdown().await;

    terminal
        .into_iter()
        .map(|(item_id, status)| {
            let last = last_progress_by_item.get(&item_id).copied();
            (item_id, status, last)
        })
        .collect()
}

async fn submit_single_file_work(
    pool: &SharedWorkerPool<QueueWorkOutput>,
    request: PipelineRequest,
    tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
) {
    let item_id = request.item_id.clone();
    let job_id = request.job_id.clone();
    let tool_paths = tool_paths.clone();
    let progress_tx = progress_tx.clone();
    pool.submit(WorkUnit {
        job_id,
        unit_id: format!("single-file:{item_id}"),
        kind: WorkKind::SingleFile,
        task: boxed_work(move |worker_cancel| async move {
            let runner = RealToolRunner::new(tool_paths.clone());
            let reporter = BroadcastReporter::new(progress_tx, item_id.clone());
            let report = run_pipeline_item_with_tool_paths(
                request,
                &runner,
                &reporter,
                &worker_cancel,
                &tool_paths,
            )
            .await;
            let status = map_album_outcome(
                &report.outcome,
                report.published.as_ref(),
                report.durable_log.as_deref(),
            );
            Ok(QueueWorkOutput::PostProcessed { item_id, status })
        }),
    })
    .await;
}

async fn submit_album_source_work(
    pool: &SharedWorkerPool<QueueWorkOutput>,
    album: &ScheduledAlbum,
    tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    job_cancel: CancellationToken,
) {
    let convert_root = album.convert_root();
    let _ = std::fs::create_dir_all(&convert_root);
    for (track_index, track) in album.source.tracks.iter().cloned().enumerate() {
        let Some(final_path) = album.planned_final_path(&track.id) else {
            log::error!("missing planned final path for {}", track.id.source_ordinal);
            continue;
        };
        match &track.source_ref {
            TrackSourceRef::StagedFile(_) => {
                let kind = if album.source.kind == SourceKind::SingleFile {
                    WorkKind::SingleFile
                } else {
                    WorkKind::EncodeTrack { track_id: track.id.clone() }
                };
                submit_staged_encode_work(
                    pool,
                    album,
                    track_index,
                    track,
                    final_path,
                    convert_root.clone(),
                    kind,
                    tool_paths,
                    progress_tx,
                    job_cancel.clone(),
                )
                .await;
            }
            TrackSourceRef::ImageSegment { .. } | TrackSourceRef::SacdTrack { .. } => {
                submit_realize_work(
                    pool,
                    album,
                    track_index,
                    track,
                    final_path,
                    convert_root.clone(),
                    tool_paths,
                    progress_tx,
                    job_cancel.clone(),
                )
                .await;
            }
        }
    }
}

async fn submit_realize_work(
    pool: &SharedWorkerPool<QueueWorkOutput>,
    album: &ScheduledAlbum,
    track_index: usize,
    track: crate::convert::pipeline::PreparedTrack,
    final_path: PathBuf,
    convert_root: PathBuf,
    tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    job_cancel: CancellationToken,
) {
    let req = album.req.clone();
    let staging_root = album.staging.root.clone();
    let staging_job = album.staging.job_id.clone();
    let tool_paths = tool_paths.clone();
    let progress_tx = progress_tx.clone();
    let job_id = album.req.job_id.clone();
    let item_id = album.req.item_id.clone();
    let track_id = track.id.clone();
    let kind = match &track.source_ref {
        TrackSourceRef::ImageSegment { .. } => WorkKind::CueSplitTrack { track_id: track_id.clone() },
        TrackSourceRef::SacdTrack { .. } => WorkKind::SacdExtractTrack { track_id: track_id.clone() },
        TrackSourceRef::StagedFile(_) => WorkKind::EncodeTrack { track_id: track_id.clone() },
    };
    pool.submit(WorkUnit {
        job_id: job_id.clone(),
        unit_id: format!("realize-track:{:04}", track_id.source_ordinal),
        kind,
        task: boxed_work(move |_worker_cancel| async move {
            let reporter = BroadcastReporter::new(progress_tx, item_id);
            if job_cancel.is_cancelled() {
                let output = scheduled_worker_failure_output(
                    track_index,
                    &track,
                    None,
                    Some(final_path.clone()),
                    "album cancelled before track realization".to_string(),
                );
                return Ok(QueueWorkOutput::Encoded { job_id, output });
            }
            match realize_track_for_scheduler(
                track_index,
                track,
                final_path,
                req,
                staging_root,
                staging_job,
                convert_root,
                tool_paths,
                &reporter,
                job_cancel,
            )
            .await
            {
                Ok(track) => Ok(QueueWorkOutput::Realized { job_id, track }),
                Err(output) => Ok(QueueWorkOutput::Encoded { job_id, output }),
            }
        }),
    })
    .await;
}

async fn submit_staged_encode_work(
    pool: &SharedWorkerPool<QueueWorkOutput>,
    album: &ScheduledAlbum,
    track_index: usize,
    track: crate::convert::pipeline::PreparedTrack,
    final_path: PathBuf,
    convert_root: PathBuf,
    kind: WorkKind,
    tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    job_cancel: CancellationToken,
) {
    let req = album.req.clone();
    let staging_root = album.staging.root.clone();
    let staging_job = album.staging.job_id.clone();
    let tool_paths = tool_paths.clone();
    let progress_tx = progress_tx.clone();
    let job_id = album.req.job_id.clone();
    let item_id = album.req.item_id.clone();
    let track_id = track.id.clone();
    pool.submit(WorkUnit {
        job_id: job_id.clone(),
        unit_id: format!("encode-track:{:04}", track_id.source_ordinal),
        kind,
        task: boxed_work(move |_worker_cancel| async move {
            let reporter = BroadcastReporter::new(progress_tx, item_id);
            if job_cancel.is_cancelled() {
                let output = scheduled_worker_failure_output(
                    track_index,
                    &track,
                    None,
                    Some(final_path.clone()),
                    "album cancelled before staged encode".to_string(),
                );
                return Ok(QueueWorkOutput::Encoded { job_id, output });
            }
            let output = match encode_track_for_scheduler(
                track_index,
                track.clone(),
                final_path.clone(),
                req,
                staging_root,
                staging_job,
                convert_root,
                tool_paths,
                &reporter,
                job_cancel,
            )
            .await
            {
                Ok(output) => output,
                Err(err) => scheduled_worker_failure_output(
                    track_index,
                    &track,
                    None,
                    Some(final_path),
                    format!("encode worker failed: {err}"),
                ),
            };
            Ok(QueueWorkOutput::Encoded { job_id, output })
        }),
    })
    .await;
}

async fn submit_realized_encode_work(
    pool: &SharedWorkerPool<QueueWorkOutput>,
    job_id: String,
    realized: ScheduledRealizedTrack,
    tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    job_cancel: CancellationToken,
) {
    let tool_paths = tool_paths.clone();
    let progress_tx = progress_tx.clone();
    let item_id = realized.req.item_id.clone();
    let track_id = realized.track.id.clone();
    let unit_index = realized.index;
    let failure_track = realized.track.clone();
    let failure_realized_input = Some(realized.realized_path.clone());
    let failure_final_path = Some(realized.final_path.clone());
    pool.submit(WorkUnit {
        job_id: job_id.clone(),
        unit_id: format!("encode-realized-track:{:04}", track_id.source_ordinal),
        kind: WorkKind::EncodeTrack { track_id },
        task: boxed_work(move |_worker_cancel| async move {
            let reporter = BroadcastReporter::new(progress_tx, item_id);
            if job_cancel.is_cancelled() {
                let output = scheduled_worker_failure_output(
                    unit_index,
                    &failure_track,
                    failure_realized_input.clone(),
                    failure_final_path.clone(),
                    "album cancelled before realized encode".to_string(),
                );
                return Ok(QueueWorkOutput::Encoded { job_id, output });
            }
            let output = match encode_realized_track_for_scheduler(
                realized,
                tool_paths,
                &reporter,
            )
            .await
            {
                Ok(output) => output,
                Err(err) => scheduled_worker_failure_output(
                    unit_index,
                    &failure_track,
                    failure_realized_input.clone(),
                    failure_final_path.clone(),
                    format!("realized encode worker failed: {err}"),
                ),
            };
            Ok(QueueWorkOutput::Encoded { job_id, output })
        }),
    })
    .await;
}

async fn submit_album_postprocess(
    pool: &SharedWorkerPool<QueueWorkOutput>,
    album: ScheduledAlbum,
    outputs: Vec<ScheduledTrackOutput>,
    tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
) {
    let item_id = album.req.item_id.clone();
    let job_id = album.req.job_id.clone();
    let tool_paths = tool_paths.clone();
    let progress_tx = progress_tx.clone();
    pool.submit(WorkUnit {
        job_id,
        unit_id: format!("album-postprocess:{item_id}"),
        kind: WorkKind::AlbumPostProcess,
        task: boxed_work(move |worker_cancel| async move {
            let runner = RealToolRunner::new(tool_paths);
            let reporter = BroadcastReporter::new(progress_tx, item_id.clone());
            let report = finish_pipeline_album_for_scheduler(
                album,
                outputs,
                &runner,
                &reporter,
                &worker_cancel,
            )
            .await;
            let status = map_album_outcome(
                &report.outcome,
                report.published.as_ref(),
                report.durable_log.as_deref(),
            );
            Ok(QueueWorkOutput::PostProcessed { item_id, status })
        }),
    })
    .await;
}

/// Run one conversion item through the shared scheduler graph.
///
/// This helper is intentionally the same path as queue processing: materialized
/// multi-track jobs feed track realization, encode, and post-processing work
/// back into `SharedWorkerPool`. The serial `run_pipeline_item_with_tool_paths`
/// function remains in `stages.rs` only as a unit-test/compatibility fallback.
async fn run_single_item_with_shared_scheduler(
    item: ConversionItem,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    tool_paths: HashMap<String, PathBuf>,
    worker_count: usize,
) -> ConversionResult<(String, ConversionStatus)> {
    let item_id = item.id.clone();
    let mut outcomes = run_queue_with_shared_orchestrator(
        vec![item],
        progress_tx,
        None,
        tool_paths,
        worker_count.max(1),
    )
    .await;

    let (_, status, _) = outcomes
        .pop()
        .unwrap_or_else(|| (item_id.clone(), ConversionStatus::Failed {
            error: "shared scheduler produced no terminal result".to_string(),
            log_path: None,
        }, None));
    Ok((item_id, status))
}

/// Construct a single `ConversionItem` request from an already unified
/// `PipelineSettings` value and then run it through the same shared scheduler
/// graph as queue processing. UI and CLI entry points that expose Chunk 1
/// planner settings should call this API instead of relying on legacy
/// `ConversionOptions` projection.
pub async fn process_item_with_pipeline_settings(
    mut item: ConversionItem,
    settings: PipelineSettings,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    tool_paths: HashMap<String, PathBuf>,
    _file_semaphore: Arc<Semaphore>,
    worker_count: usize,
    _scratch_directory: Option<PathBuf>,
) -> ConversionResult<(String, ConversionStatus)> {
    if !item.input_path.exists() {
        let error_msg = format!("Source file not found: {}", item.input_path.display());
        log::error!("{}", error_msg);
        return Ok((
            item.id.clone(),
            ConversionStatus::Failed {
                error: error_msg,
                log_path: None,
            },
        ));
    }

    item.options.pipeline_settings = Some(settings.clone());
    item.pipeline_settings = Some(settings.clone());
    let mut request = build_pipeline_request_from_settings(&item, settings)?;
    request.worker_count = Some(worker_count.max(1));
    item.pipeline_request = Some(request);
    process_item(item, progress_tx, tool_paths, _file_semaphore, worker_count, _scratch_directory).await
}

/// Process a single conversion item. This direct path now delegates to the
/// shared scheduler graph, so SACD/CUE/archive requests get the same
/// materialization -> encode -> album-postprocess behavior as queued batches.
pub async fn process_item(
    item: ConversionItem,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    tool_paths: HashMap<String, PathBuf>,
    _file_semaphore: Arc<Semaphore>,
    worker_count: usize,
    _scratch_directory: Option<PathBuf>,
) -> ConversionResult<(String, ConversionStatus)> {
    if !item.input_path.exists() {
        let error_msg = format!("Source file not found: {}", item.input_path.display());
        log::error!("{}", error_msg);
        return Ok((
            item.id.clone(),
            ConversionStatus::Failed {
                error: error_msg,
                log_path: None,
            },
        ));
    }

    let _ = progress_tx.send(ProgressUpdate {
        item_id: item.id.clone(),
        progress: 0.0,
        status: ConversionStatus::Processing {
            progress: 0.0,
            message: Some(format!("Starting conversion to {}", item.output_format)),
            file_progress: None,
            phase: None,
            phase_progress: None,
        },
    });

    run_single_item_with_shared_scheduler(item, progress_tx, tool_paths, worker_count).await
}
