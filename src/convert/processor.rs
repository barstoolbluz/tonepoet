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
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio_util::sync::CancellationToken;
use tonepoet_pipeline::PipelineSettings;

use crate::convert::pipeline::{
    boxed_work, build_pipeline_request, build_pipeline_request_from_settings, detect_source_kind,
    encode_realized_track_for_scheduler_with_tool_limits, encode_track_for_scheduler_with_tool_limits,
    finish_pipeline_album_for_scheduler_with_tool_limits, map_album_outcome,
    prepare_pipeline_item_for_scheduler, realize_track_for_scheduler_with_tool_limits,
    run_pipeline_item_with_tool_paths_and_tool_limits, scheduled_worker_failure_output,
    AlbumCompletionTracker,
    AlbumReadiness, BroadcastReporter, CueSidecarPolicy, FailurePolicy, LogPolicy,
    NamingCollisionPolicy, NamingPolicy, OverwritePolicy, PipelineRequest, PoolLimits,
    PreparedTrack, PublishPolicy, RealToolRunner, ScheduledAlbum, ScheduledMaterialization,
    ScheduledRealizedTrack, ScheduledTrackOutput, SchedulerMetrics, SchedulerMetricsSnapshot,
    SharedWorkerPool, SourceKind, SourceOptions, StagePolicy, StageRequirement, ToolConcurrencyLimits,
    TrackId, TrackMetadata, TrackSelection, TrackSourceRef, TrySubmitError, WorkKind, WorkUnit,
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
    lifecycle_tx: Option<mpsc::UnboundedSender<LifecycleEvent>>,
    pool_limits: PoolLimits,
    scheduler_metrics: Arc<SchedulerMetrics>,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
    /// External cancellation token from the TUI. When triggered, the
    /// scheduler cancels all in-flight workers and kills child processes.
    external_cancel: Option<CancellationToken>,
}

/// Progress update from a worker.
///
/// When `track_index` is `Some(idx)`, the update describes one concurrent
/// track inside a multi-track source (SACD ISO, CUE+image, 7z archive).
/// The TUI routes these to per-track sub-lines below the parent item row.
/// When `track_index` is `None`, the update applies to the item itself.
#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    pub item_id: String,
    pub track_index: Option<u32>,
    pub track_epoch: Option<u64>,
    pub progress: f32,
    pub status: ConversionStatus,
}

#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    ClearTrack {
        item_id: String,
        track_index: u32,
        track_epoch: u64,
    },
    ItemTerminal {
        item_id: String,
        progress: f32,
        status: ConversionStatus,
    },
}

/// Helper to send phase progress updates.
#[allow(dead_code)]
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
        track_index: None,
        track_epoch: None,
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

fn effective_worker_count_for_tool_limits(
    configured_worker_count: usize,
    tool_concurrency_limits: &ToolConcurrencyLimits,
) -> usize {
    configured_worker_count
        .max(1)
        .max(tool_concurrency_limits.max_tool_concurrency())
}

impl ConversionProcessor {
    /// Create a new processor.
    pub fn new(config: ProcessorConfig) -> Self {
        Self {
            config,
            progress_tx: None,
            lifecycle_tx: None,
            pool_limits: PoolLimits::default(),
            scheduler_metrics: Arc::new(SchedulerMetrics::default()),
            tool_concurrency_limits: Arc::new(ToolConcurrencyLimits::from_available_parallelism()),
            external_cancel: None,
        }
    }

    /// Set progress channel.
    pub fn set_progress_channel(&mut self, tx: broadcast::Sender<ProgressUpdate>) {
        self.progress_tx = Some(tx);
    }

    /// Set lifecycle channel.
    pub fn set_lifecycle_channel(&mut self, tx: mpsc::UnboundedSender<LifecycleEvent>) {
        self.lifecycle_tx = Some(tx);
    }

    /// Set an external cancellation token from the TUI. When triggered,
    /// the scheduler will cancel all workers and kill child processes.
    pub fn set_cancel_token(&mut self, token: CancellationToken) {
        self.external_cancel = Some(token);
    }

    /// Set shared scheduler queue limits. Defaults to the legacy unbounded queue.
    pub fn set_pool_limits(&mut self, limits: PoolLimits) {
        self.pool_limits = limits;
    }

    /// Return a lock-free metrics handle that remains valid while a conversion run is active.
    pub fn scheduler_metrics(&self) -> Arc<SchedulerMetrics> {
        self.scheduler_metrics.clone()
    }

    /// Read a point-in-time snapshot of scheduler metrics.
    pub fn scheduler_metrics_snapshot(&self) -> SchedulerMetricsSnapshot {
        self.scheduler_metrics.snapshot()
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
                queue_item.active_tracks.clear();
                queue_item.closed_track_epochs.clear();
                queue_item.status = ConversionStatus::Processing {
                    progress: 0.0,
                    message: Some(format!("Starting conversion to {}", item.output_format)),
                    file_progress: None,
                    phase: Some(ConversionPhase::Extracting),
                    phase_progress: Some(0.0),
                };
            }
        }

        self.scheduler_metrics.reset();
        let effective_worker_count = effective_worker_count_for_tool_limits(
            self.config.worker_count,
            self.tool_concurrency_limits.as_ref(),
        );
        let outcomes = run_queue_with_shared_orchestrator(
            queued_items,
            progress_tx.clone(),
            self.lifecycle_tx.clone(),
            progress_rx,
            self.config.tool_paths.clone(),
            effective_worker_count,
            self.pool_limits,
            self.scheduler_metrics.clone(),
            self.tool_concurrency_limits.clone(),
            self.external_cancel.take(),
        )
        .await;

        for (item_id, final_status, last_progress) in outcomes {
            let progress = terminal_progress_for_status(&final_status, last_progress);
            let _ = progress_tx.send(ProgressUpdate {
                item_id: item_id.clone(),
                track_index: None,
                track_epoch: None,
                progress,
                status: final_status.clone(),
            });
            let mut q = queue.write().await;
            if let Some(queue_item) = q.find_item_mut(&item_id) {
                queue_item.active_tracks.clear();
                queue_item.closed_track_epochs.clear();
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
    next_source_track: usize,
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
    #[cfg(test)]
    SyntheticEncoded {
        job_id: String,
        track_index: usize,
    },
}

#[cfg(test)]
struct SyntheticFanout {
    job_id: String,
    _item_id: String,
    remaining: usize,
    next_index: usize,
}

struct DeferredSubmission {
    unit: WorkUnit<QueueWorkOutput>,
    counted: bool,
}

struct SubmissionPump {
    initial_items: VecDeque<ConversionItem>,
    album_fanout: VecDeque<String>,
    realized_encodes: VecDeque<(String, ScheduledRealizedTrack, CancellationToken)>,
    album_postprocess: VecDeque<(ScheduledAlbum, Vec<ScheduledTrackOutput>)>,
    #[cfg(test)]
    synthetic_fanout: VecDeque<SyntheticFanout>,
    deferred_unit: Option<DeferredSubmission>,
    retry_after_busy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PumpFlushProgress {
    Progress,
    Blocked,
    Empty,
}

impl SubmissionPump {
    fn new(items: Vec<ConversionItem>) -> Self {
        Self {
            initial_items: items.into_iter().collect(),
            album_fanout: VecDeque::new(),
            realized_encodes: VecDeque::new(),
            album_postprocess: VecDeque::new(),
            #[cfg(test)]
            synthetic_fanout: VecDeque::new(),
            deferred_unit: None,
            retry_after_busy: false,
        }
    }

    fn enqueue_album_fanout(&mut self, job_id: String) {
        self.album_fanout.push_back(job_id);
    }

    fn enqueue_realized_encode(
        &mut self,
        job_id: String,
        realized: ScheduledRealizedTrack,
        job_cancel: CancellationToken,
    ) {
        self.realized_encodes.push_back((job_id, realized, job_cancel));
    }

    fn enqueue_album_postprocess(
        &mut self,
        album: ScheduledAlbum,
        outputs: Vec<ScheduledTrackOutput>,
    ) {
        self.album_postprocess.push_back((album, outputs));
    }

    #[cfg(test)]
    fn enqueue_synthetic_album_fanout(
        &mut self,
        job_id: String,
        item_id: String,
        tracks: usize,
        metrics: &SchedulerMetrics,
    ) {
        if tracks > 0 {
            metrics.record_jobs_queued(tracks as u64);
            self.synthetic_fanout.push_back(SyntheticFanout {
                job_id,
                _item_id: item_id,
                remaining: tracks,
                next_index: 0,
            });
        }
    }

    fn should_retry_after_busy(&self) -> bool {
        self.retry_after_busy
    }

    fn try_submit_unit(
        &mut self,
        pool: &SharedWorkerPool<QueueWorkOutput>,
        unit: WorkUnit<QueueWorkOutput>,
        counted: bool,
    ) -> bool {
        let submitted = if counted {
            pool.try_submit_counted(unit)
        } else {
            pool.try_submit(unit)
        };

        match submitted {
            Ok(()) => true,
            Err(TrySubmitError::QueueBusy(unit)) => {
                if !counted {
                    pool.metrics().record_jobs_queued(1);
                }
                self.retry_after_busy = true;
                self.deferred_unit = Some(DeferredSubmission { unit, counted: true });
                false
            }
            Err(TrySubmitError::QueueFull { unit, .. }) => {
                if !counted {
                    pool.metrics().record_jobs_queued(1);
                }
                self.retry_after_busy = false;
                self.deferred_unit = Some(DeferredSubmission { unit, counted: true });
                false
            }
        }
    }

    fn flush_album_fanout_once<F>(
        &mut self,
        pool: &SharedWorkerPool<QueueWorkOutput>,
        mut next_source_work: F,
    ) -> PumpFlushProgress
    where
        F: FnMut(&str) -> Option<WorkUnit<QueueWorkOutput>>,
    {
        let Some(job_id) = self.album_fanout.pop_front() else {
            return PumpFlushProgress::Empty;
        };

        let Some(unit) = next_source_work(&job_id) else {
            return PumpFlushProgress::Progress;
        };

        self.album_fanout.push_back(job_id);
        if self.try_submit_unit(pool, unit, true) {
            PumpFlushProgress::Progress
        } else {
            PumpFlushProgress::Blocked
        }
    }

    #[cfg(test)]
    fn flush_synthetic_fanout_once(
        &mut self,
        pool: &SharedWorkerPool<QueueWorkOutput>,
    ) -> PumpFlushProgress {
        let Some(mut fanout) = self.synthetic_fanout.pop_front() else {
            return PumpFlushProgress::Empty;
        };

        if fanout.remaining == 0 {
            return PumpFlushProgress::Progress;
        }

        let track_index = fanout.next_index;
        fanout.next_index += 1;
        fanout.remaining -= 1;
        let job_id = fanout.job_id.clone();
        let unit_id = format!("synthetic-encode-{track_index:04}");
        let unit = synthetic_queue_work_unit_for_processor_test(&job_id, unit_id, track_index);
        if fanout.remaining > 0 {
            self.synthetic_fanout.push_back(fanout);
        }

        if self.try_submit_unit(pool, unit, true) {
            PumpFlushProgress::Progress
        } else {
            PumpFlushProgress::Blocked
        }
    }

    fn flush(
        &mut self,
        pool: &SharedWorkerPool<QueueWorkOutput>,
        pending_albums: &mut BTreeMap<String, PendingAlbum>,
        terminal: &mut BTreeMap<String, ConversionStatus>,
        job_to_item: &mut BTreeMap<String, String>,
        tool_paths: &HashMap<String, PathBuf>,
        progress_tx: &broadcast::Sender<ProgressUpdate>,
        lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
        _cancel: &CancellationToken,
        worker_count: usize,
        tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
    ) {
        self.retry_after_busy = false;
        loop {
            if let Some(deferred) = self.deferred_unit.take() {
                if !self.try_submit_unit(pool, deferred.unit, deferred.counted) {
                    return;
                }
                continue;
            }

            if let Some((album, outputs)) = self.album_postprocess.pop_front() {
                let unit = build_album_postprocess_work(
                    album,
                    outputs,
                    tool_paths,
                    progress_tx,
                    lifecycle_tx,
                    tool_concurrency_limits.clone(),
                );
                if !self.try_submit_unit(pool, unit, true) {
                    return;
                }
                continue;
            }

            if let Some((job_id, realized, job_cancel)) = self.realized_encodes.pop_front() {
                let unit = build_realized_encode_work(
                    job_id,
                    realized,
                    tool_paths,
                    progress_tx,
                    lifecycle_tx,
                    job_cancel,
                    tool_concurrency_limits.clone(),
                );
                if !self.try_submit_unit(pool, unit, true) {
                    return;
                }
                continue;
            }

            match self.flush_album_fanout_once(pool, |job_id| {
                next_album_source_work(
                    pending_albums,
                    job_id,
                    tool_paths,
                    progress_tx,
                    lifecycle_tx,
                    tool_concurrency_limits.clone(),
                )
            }) {
                PumpFlushProgress::Progress => continue,
                PumpFlushProgress::Blocked => return,
                PumpFlushProgress::Empty => {}
            }

            #[cfg(test)]
            match self.flush_synthetic_fanout_once(pool) {
                PumpFlushProgress::Progress => continue,
                PumpFlushProgress::Blocked => return,
                PumpFlushProgress::Empty => {}
            }

            if let Some(item) = self.initial_items.pop_front() {
                if let Some(unit) = build_initial_work(
                    item,
                    pool,
                    terminal,
                    job_to_item,
                    tool_paths,
                    progress_tx,
                    lifecycle_tx,
                    worker_count,
                    tool_concurrency_limits.clone(),
                ) {
                    if !self.try_submit_unit(pool, unit, false) {
                        return;
                    }
                }
                continue;
            }

            break;
        }
    }

    fn record_backlog(
        &self,
        metrics: &SchedulerMetrics,
        pending_albums: &BTreeMap<String, PendingAlbum>,
    ) {
        metrics.record_submission_backlog_depth(self.submission_backlog_depth(pending_albums));
    }

    fn submission_backlog_depth(&self, pending_albums: &BTreeMap<String, PendingAlbum>) -> usize {
        self.initial_items
            .len()
            .saturating_add(usize::from(self.deferred_unit.is_some()))
            .saturating_add(self.realized_encodes.len())
            .saturating_add(self.album_postprocess.len())
            .saturating_add(self.pending_album_source_units(pending_albums))
            .saturating_add(self.synthetic_fanout_backlog_depth())
    }

    #[cfg(test)]
    fn synthetic_fanout_backlog_depth(&self) -> usize {
        self.synthetic_fanout.iter().map(|fanout| fanout.remaining).sum()
    }

    #[cfg(not(test))]
    fn synthetic_fanout_backlog_depth(&self) -> usize {
        0
    }

    fn pending_album_source_units(&self, pending_albums: &BTreeMap<String, PendingAlbum>) -> usize {
        self.album_fanout
            .iter()
            .filter_map(|job_id| pending_albums.get(job_id))
            .map(|pending| {
                let Some(album) = pending.album.as_ref() else {
                    return 0;
                };
                album
                    .source
                    .tracks
                    .iter()
                    .skip(pending.next_source_track)
                    .filter(|track| album.planned_final_path(&track.id).is_some())
                    .count()
            })
            .sum()
    }

    #[cfg(test)]
    fn pending_work_units(&self) -> usize {
        usize::from(self.deferred_unit.is_some())
    }
}

fn record_terminal_status(
    pool: &SharedWorkerPool<QueueWorkOutput>,
    status: &ConversionStatus,
) {
    match status {
        ConversionStatus::Completed { .. } | ConversionStatus::Partial { .. } => {
            pool.metrics().record_job_completed();
        }
        ConversionStatus::Failed { .. } | ConversionStatus::Cancelled => {
            pool.metrics().record_job_failed();
        }
        ConversionStatus::Queued
        | ConversionStatus::Paused
        | ConversionStatus::Processing { .. }
        | ConversionStatus::NotConfigured => {}
    }
}

async fn run_queue_with_shared_orchestrator(
    queued_items: Vec<ConversionItem>,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    lifecycle_tx: Option<mpsc::UnboundedSender<LifecycleEvent>>,
    mut progress_rx: Option<broadcast::Receiver<ProgressUpdate>>,
    tool_paths: HashMap<String, PathBuf>,
    worker_count: usize,
    pool_limits: PoolLimits,
    metrics: Arc<SchedulerMetrics>,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
    external_cancel: Option<CancellationToken>,
) -> Vec<(String, ConversionStatus, Option<f32>)> {
    let cancel = external_cancel.unwrap_or_else(CancellationToken::new);
    let pool = SharedWorkerPool::<QueueWorkOutput>::new_with_limits_and_metrics(
        Some(worker_count.max(1)),
        cancel.clone(),
        pool_limits,
        metrics,
    );
    let total_items = queued_items.len();
    let mut submissions = SubmissionPump::new(queued_items);
    let mut run = pool.start();
    let mut pending_albums: BTreeMap<String, PendingAlbum> = BTreeMap::new();
    let mut tracker = AlbumCompletionTracker::default();
    let mut terminal: BTreeMap<String, ConversionStatus> = BTreeMap::new();
    let mut job_to_item: BTreeMap<String, String> = BTreeMap::new();
    let mut last_progress_by_item: HashMap<String, f32> = HashMap::new();
    submissions.record_backlog(pool.metrics(), &pending_albums);

    while terminal.len() < total_items {
        submissions.flush(
            &pool,
            &mut pending_albums,
            &mut terminal,
            &mut job_to_item,
            &tool_paths,
            &progress_tx,
            lifecycle_tx.as_ref(),
            &cancel,
            worker_count.max(1),
            tool_concurrency_limits.clone(),
        );
        submissions.record_backlog(pool.metrics(), &pending_albums);
        if terminal.len() >= total_items {
            break;
        }
        tokio::select! {
            _ = tokio::task::yield_now(), if submissions.should_retry_after_busy() => {},
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
                                if terminal.insert(item_id, status.clone()).is_none() {
                                    record_terminal_status(&pool, &status);
                                }
                            }
                            ScheduledMaterialization::Ready(album) => {
                                let job_id = album.req.job_id.clone();
                                let expected = album
                                    .source
                                    .tracks
                                    .iter()
                                    .filter(|track| album.planned_final_path(&track.id).is_some())
                                    .count();
                                let staged_tracks_ready_for_encode = album
                                    .source
                                    .tracks
                                    .iter()
                                    .filter(|track| album.planned_final_path(&track.id).is_some())
                                    .filter(|track| matches!(&track.source_ref, TrackSourceRef::StagedFile(_)))
                                    .count();
                                pool.metrics()
                                    .record_tracks_materialized(staged_tracks_ready_for_encode as u64);
                                tracker.register_album(job_id.clone(), expected, album.allow_partial());
                                pool.metrics().record_jobs_queued(expected as u64);
                                if expected == 0 {
                                    pool.metrics().record_jobs_queued(1);
                                    submissions.enqueue_album_postprocess(album, Vec::new());
                                    submissions.record_backlog(pool.metrics(), &pending_albums);
                                } else {
                                    let job_cancel = cancel.child_token();
                                    pending_albums.insert(job_id.clone(), PendingAlbum {
                                        album: Some(album),
                                        outputs: Vec::with_capacity(expected),
                                        finished: 0,
                                        expected,
                                        next_source_track: 0,
                                        job_cancel,
                                        cancel_requested: false,
                                    });
                                    submissions.enqueue_album_fanout(job_id.clone());
                                    submissions.record_backlog(pool.metrics(), &pending_albums);
                                }
                            }
                        }
                    }
                    Ok(QueueWorkOutput::Realized { job_id, track }) => {
                        if pending_albums.contains_key(&job_id) {
                            pool.metrics().record_tracks_materialized(1);
                            let job_cancel = pending_albums
                                .get(&job_id)
                                .map(|pending| pending.job_cancel.clone())
                                .unwrap_or_else(|| cancel.child_token());
                            pool.metrics().record_jobs_queued(1);
                            submissions.enqueue_realized_encode(job_id, track, job_cancel);
                            submissions.record_backlog(pool.metrics(), &pending_albums);
                        }
                    }
                    Ok(QueueWorkOutput::Encoded { job_id, output }) => {
                        if !output.ok {
                            log::warn!("track encode failed: job={job_id} track={} outcome={:?}", output.index, output.record.outcome);
                        }
                        pool.metrics().record_track_encode_output(output.ok);
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
                            pool.metrics().record_jobs_queued(1);
                            submissions.enqueue_album_postprocess(album, outputs);
                            submissions.record_backlog(pool.metrics(), &pending_albums);
                        }
                    }
                    Ok(QueueWorkOutput::PostProcessed { item_id, status }) => {
                        if terminal.insert(item_id, status.clone()).is_none() {
                            record_terminal_status(&pool, &status);
                        }
                    }
                    #[cfg(test)]
                    Ok(QueueWorkOutput::SyntheticEncoded { job_id: _, track_index: _ }) => {
                        // Test-only processor harness output. Production workers never emit this variant.
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
                        if !terminal.contains_key(&item_id) {
                            pool.metrics().record_job_failed();
                            terminal.insert(item_id, ConversionStatus::Failed {
                                error: format!("scheduler work unit failed: {err}"),
                                log_path: None,
                            });
                        }
                    }
                }
            }
            else => break,
        }
    }

    submissions.record_backlog(pool.metrics(), &pending_albums);
    pool.metrics().record_submission_backlog_depth(0);
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

fn build_initial_work(
    item: ConversionItem,
    pool: &SharedWorkerPool<QueueWorkOutput>,
    terminal: &mut BTreeMap<String, ConversionStatus>,
    job_to_item: &mut BTreeMap<String, String>,
    tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    worker_count: usize,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
) -> Option<WorkUnit<QueueWorkOutput>> {
    let item_id = item.id.clone();
    if !item.input_path.exists() {
        pool.metrics().record_job_failed();
        terminal.insert(
            item_id.clone(),
            ConversionStatus::Failed {
                error: format!("Source file not found: {}", item.input_path.display()),
                log_path: None,
            },
        );
        return None;
    }

    let request = match build_pipeline_request(&item) {
        Ok(mut req) => {
            req.worker_count = Some(worker_count.max(1));
            req
        }
        Err(err) => {
            pool.metrics().record_job_failed();
            terminal.insert(
                item_id.clone(),
                ConversionStatus::Failed {
                    error: err.to_string(),
                    log_path: None,
                },
            );
            return None;
        }
    };

    job_to_item.insert(request.job_id.clone(), item_id.clone());

    let source_kind = detect_source_kind(&request).ok();
    if matches!(source_kind, Some(SourceKind::SingleFile)) {
        return Some(build_single_file_work(
            request,
            tool_paths,
            progress_tx,
            lifecycle_tx,
            tool_concurrency_limits,
        ));
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
    Some(WorkUnit {
        job_id: request.job_id.clone(),
        unit_id: format!("{unit_prefix}:{submit_item_id}"),
        kind: materialize_kind,
        task: boxed_work(move |worker_cancel| async move {
            let runner = RealToolRunner::new(submit_tool_paths.clone());
            let reporter = BroadcastReporter::new(submit_progress_tx, None, submit_item_id.clone(), None);
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
}

fn build_single_file_work(
    request: PipelineRequest,
    tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    _lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
) -> WorkUnit<QueueWorkOutput> {
    let item_id = request.item_id.clone();
    let job_id = request.job_id.clone();
    let tool_paths = tool_paths.clone();
    let progress_tx = progress_tx.clone();
    let tool_concurrency_limits = tool_concurrency_limits.clone();
    WorkUnit {
        job_id,
        unit_id: format!("single-file:{item_id}"),
        kind: WorkKind::SingleFile,
        task: boxed_work(move |worker_cancel| async move {
            let runner = RealToolRunner::new(tool_paths.clone());
            let reporter = BroadcastReporter::new(progress_tx, None, item_id.clone(), None);
            let report = run_pipeline_item_with_tool_paths_and_tool_limits(
                request,
                &runner,
                &reporter,
                &worker_cancel,
                &tool_paths,
                Some(tool_concurrency_limits),
            )
            .await;
            let status = map_album_outcome(
                &report.outcome,
                report.published.as_ref(),
                report.durable_log.as_deref(),
            );
            Ok(QueueWorkOutput::PostProcessed { item_id, status })
        }),
    }
}

fn next_album_source_work(
    pending_albums: &mut BTreeMap<String, PendingAlbum>,
    job_id: &str,
    tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
) -> Option<WorkUnit<QueueWorkOutput>> {
    let pending = pending_albums.get_mut(job_id)?;
    loop {
        let track_index = pending.next_source_track;
        let album = pending.album.as_ref()?;
        if track_index >= album.source.tracks.len() {
            return None;
        }

        let track = album.source.tracks[track_index].clone();
        pending.next_source_track += 1;
        let Some(final_path) = album.planned_final_path(&track.id) else {
            log::error!("missing planned final path for {}", track.id.source_ordinal);
            continue;
        };

        let convert_root = album.convert_root();
        let _ = std::fs::create_dir_all(&convert_root);
        return Some(match &track.source_ref {
            TrackSourceRef::StagedFile(_) => {
                let kind = if album.source.kind == SourceKind::SingleFile {
                    WorkKind::SingleFile
                } else {
                    WorkKind::EncodeTrack { track_id: track.id.clone() }
                };
                build_staged_encode_work(
                    album,
                    track_index,
                    track,
                    final_path,
                    convert_root,
                    kind,
                    tool_paths,
                    progress_tx,
                    lifecycle_tx,
                    pending.job_cancel.clone(),
                    tool_concurrency_limits.clone(),
                )
            }
            TrackSourceRef::CueSegmentCarrier { .. }
            | TrackSourceRef::ImageSegment { .. }
            | TrackSourceRef::SacdTrack { .. } => {
                build_realize_work(
                    album,
                    track_index,
                    track,
                    final_path,
                    convert_root,
                    tool_paths,
                    progress_tx,
                    lifecycle_tx,
                    pending.job_cancel.clone(),
                    tool_concurrency_limits.clone(),
                )
            }
        });
    }
}

fn build_realize_work(
    album: &ScheduledAlbum,
    track_index: usize,
    track: crate::convert::pipeline::PreparedTrack,
    final_path: PathBuf,
    convert_root: PathBuf,
    tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    job_cancel: CancellationToken,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
) -> WorkUnit<QueueWorkOutput> {
    let req = album.req.clone();
    let staging_root = album.staging.root.clone();
    let staging_job = album.staging.job_id.clone();
    let tool_paths = tool_paths.clone();
    let progress_tx = progress_tx.clone();
    let lifecycle_tx = lifecycle_tx.cloned();
    let tool_concurrency_limits = tool_concurrency_limits.clone();
    let job_id = album.req.job_id.clone();
    let item_id = album.req.item_id.clone();
    let track_id = track.id.clone();
    let kind = match &track.source_ref {
        TrackSourceRef::CueSegmentCarrier { .. }
        | TrackSourceRef::ImageSegment { .. } => WorkKind::CueSplitTrack { track_id: track_id.clone() },
        TrackSourceRef::SacdTrack { .. } => WorkKind::SacdExtractTrack { track_id: track_id.clone() },
        TrackSourceRef::StagedFile(_) => WorkKind::EncodeTrack { track_id: track_id.clone() },
    };
    WorkUnit {
        job_id: job_id.clone(),
        unit_id: format!("realize-track:{:04}", track_id.source_ordinal),
        kind,
        task: boxed_work(move |_worker_cancel| async move {
            let reporter = BroadcastReporter::new(progress_tx, lifecycle_tx, item_id, Some(track_index as u32));
            let _guard = reporter.track_lifecycle_guard();
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
            match realize_track_for_scheduler_with_tool_limits(
                track_index,
                track,
                final_path,
                req,
                staging_root,
                staging_job,
                convert_root,
                tool_paths,
                Some(tool_concurrency_limits),
                &reporter,
                job_cancel,
            )
            .await
            {
                Ok(track) => Ok(QueueWorkOutput::Realized { job_id, track }),
                Err(output) => {
                    log::warn!("track realization failed: {:?}", output.record.outcome);
                    Ok(QueueWorkOutput::Encoded { job_id, output })
                }
            }
        }),
    }
}

fn build_staged_encode_work(
    album: &ScheduledAlbum,
    track_index: usize,
    track: crate::convert::pipeline::PreparedTrack,
    final_path: PathBuf,
    convert_root: PathBuf,
    kind: WorkKind,
    tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    job_cancel: CancellationToken,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
) -> WorkUnit<QueueWorkOutput> {
    let req = album.req.clone();
    let staging_root = album.staging.root.clone();
    let staging_job = album.staging.job_id.clone();
    let tool_paths = tool_paths.clone();
    let progress_tx = progress_tx.clone();
    let lifecycle_tx = lifecycle_tx.cloned();
    let tool_concurrency_limits = tool_concurrency_limits.clone();
    let job_id = album.req.job_id.clone();
    let item_id = album.req.item_id.clone();
    let track_id = track.id.clone();
    WorkUnit {
        job_id: job_id.clone(),
        unit_id: format!("encode-track:{:04}", track_id.source_ordinal),
        kind,
        task: boxed_work(move |_worker_cancel| async move {
            let reporter = BroadcastReporter::new(progress_tx, lifecycle_tx, item_id, Some(track_index as u32));
            let _guard = reporter.track_lifecycle_guard();
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
            let output = match encode_track_for_scheduler_with_tool_limits(
                track_index,
                track.clone(),
                final_path.clone(),
                req,
                staging_root,
                staging_job,
                convert_root,
                tool_paths,
                Some(tool_concurrency_limits),
                &reporter,
                job_cancel,
            )
            .await
            {
                Ok(output) => output,
                Err(err) => {
                    log::warn!("staged encode worker failed for track {}: {err}", track_index);
                    scheduled_worker_failure_output(
                        track_index,
                        &track,
                        None,
                        Some(final_path),
                        format!("encode worker failed: {err}"),
                    )
                }
            };
            Ok(QueueWorkOutput::Encoded { job_id, output })
        }),
    }
}

fn build_realized_encode_work(
    job_id: String,
    realized: ScheduledRealizedTrack,
    tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    job_cancel: CancellationToken,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
) -> WorkUnit<QueueWorkOutput> {
    let tool_paths = tool_paths.clone();
    let progress_tx = progress_tx.clone();
    let lifecycle_tx = lifecycle_tx.cloned();
    let tool_concurrency_limits = tool_concurrency_limits.clone();
    let item_id = realized.req.item_id.clone();
    let track_id = realized.track.id.clone();
    let unit_index = realized.index;
    let failure_track = realized.track.clone();
    let failure_realized_input = Some(realized.realized_path.clone());
    let failure_final_path = Some(realized.final_path.clone());
    WorkUnit {
        job_id: job_id.clone(),
        unit_id: format!("encode-realized-track:{:04}", track_id.source_ordinal),
        kind: WorkKind::EncodeTrack { track_id },
        task: boxed_work(move |_worker_cancel| async move {
            let reporter = BroadcastReporter::new(progress_tx, lifecycle_tx, item_id, Some(unit_index as u32));
            let _guard = reporter.track_lifecycle_guard();
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
            let output = match encode_realized_track_for_scheduler_with_tool_limits(
                realized,
                tool_paths,
                Some(tool_concurrency_limits),
                &reporter,
            )
            .await
            {
                Ok(output) => output,
                Err(err) => {
                    log::warn!("realized encode worker failed for track {}: {err}", unit_index);
                    scheduled_worker_failure_output(
                        unit_index,
                        &failure_track,
                        failure_realized_input.clone(),
                        failure_final_path.clone(),
                        format!("realized encode worker failed: {err}"),
                    )
                }
            };
            Ok(QueueWorkOutput::Encoded { job_id, output })
        }),
    }
}

fn build_album_postprocess_work(
    album: ScheduledAlbum,
    outputs: Vec<ScheduledTrackOutput>,
    tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    _lifecycle_tx: Option<&mpsc::UnboundedSender<LifecycleEvent>>,
    tool_concurrency_limits: Arc<ToolConcurrencyLimits>,
) -> WorkUnit<QueueWorkOutput> {
    let item_id = album.req.item_id.clone();
    let job_id = album.req.job_id.clone();
    let tool_paths = tool_paths.clone();
    let progress_tx = progress_tx.clone();
    let tool_concurrency_limits = tool_concurrency_limits.clone();
    WorkUnit {
        job_id,
        unit_id: format!("album-postprocess:{item_id}"),
        kind: WorkKind::AlbumPostProcess,
        task: boxed_work(move |worker_cancel| async move {
            let runner = RealToolRunner::new(tool_paths);
            let reporter = BroadcastReporter::new(progress_tx, None, item_id.clone(), None);
            let report = finish_pipeline_album_for_scheduler_with_tool_limits(
                album,
                outputs,
                &runner,
                &reporter,
                &worker_cancel,
                Some(tool_concurrency_limits),
            )
            .await;
            let status = map_album_outcome(
                &report.outcome,
                report.published.as_ref(),
                report.durable_log.as_deref(),
            );
            Ok(QueueWorkOutput::PostProcessed { item_id, status })
        }),
    }
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
    let tool_concurrency_limits = Arc::new(ToolConcurrencyLimits::from_available_parallelism());
    let effective_worker_count = effective_worker_count_for_tool_limits(
        worker_count,
        tool_concurrency_limits.as_ref(),
    );
    let mut outcomes = run_queue_with_shared_orchestrator(
        vec![item],
        progress_tx,
        None,
        None,
        tool_paths,
        effective_worker_count,
        PoolLimits::default(),
        Arc::new(SchedulerMetrics::default()),
        tool_concurrency_limits,
        None,
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

#[cfg(test)]
fn synthetic_queue_work_unit_for_processor_test(
    job_id: &str,
    unit_id: String,
    track_index: usize,
) -> WorkUnit<QueueWorkOutput> {
    let job_id = job_id.to_string();
    let output_job_id = job_id.clone();
    WorkUnit {
        job_id,
        unit_id,
        kind: WorkKind::EncodeTrack {
            track_id: crate::convert::pipeline::TrackId {
                source_ordinal: track_index as u32,
                disc_number: None,
                track_number: track_index as u32 + 1,
            },
        },
        task: boxed_work(move |_cancel| async move {
            Ok(QueueWorkOutput::SyntheticEncoded {
                job_id: output_job_id,
                track_index,
            })
        }),
    }
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
        track_index: None,
        track_epoch: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_queue_work_unit(job_id: &str, unit_id: String) -> WorkUnit<QueueWorkOutput> {
        let item_id = unit_id.clone();
        WorkUnit {
            job_id: job_id.to_string(),
            unit_id,
            kind: WorkKind::EncodeTrack {
                track_id: crate::convert::pipeline::TrackId {
                    source_ordinal: 0,
                    disc_number: None,
                    track_number: 1,
                },
            },
            task: boxed_work(move |_cancel| async move {
                let _ = item_id;
                Err("test unit should remain queued".to_string())
            }),
        }
    }

    fn blocking_synthetic_processor_unit(
        job_id: &str,
        unit_id: &str,
        track_index: usize,
        started_tx: tokio::sync::oneshot::Sender<()>,
        release_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> WorkUnit<QueueWorkOutput> {
        let job_id = job_id.to_string();
        let output_job_id = job_id.clone();
        WorkUnit {
            job_id,
            unit_id: unit_id.to_string(),
            kind: WorkKind::EncodeTrack {
                track_id: crate::convert::pipeline::TrackId {
                    source_ordinal: track_index as u32,
                    disc_number: None,
                    track_number: track_index as u32 + 1,
                },
            },
            task: boxed_work(move |_cancel| async move {
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Ok(QueueWorkOutput::SyntheticEncoded {
                    job_id: output_job_id,
                    track_index,
                })
            }),
        }
    }

    async fn expect_processor_scheduler_event(
        events_rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::convert::pipeline::SchedulerTestEvent>,
        expected: crate::convert::pipeline::SchedulerTestEvent,
    ) {
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), events_rx.recv())
            .await
            .expect("scheduler test event arrives before timeout")
            .expect("scheduler test event sender remains live");
        assert_eq!(event, expected);
    }

    fn pipeline_request_for_processor_limit_test(root: &std::path::Path) -> PipelineRequest {
        PipelineRequest {
            job_id: "processor-limit-job".to_string(),
            item_id: "processor-limit-item".to_string(),
            container: root.join("input.flac"),
            source: SourceOptions {
                archive_password: None,
                sacd_area: None,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            settings: PipelineSettings::default(),
            worker_count: Some(1),
            merge: false,
            output_root: root.join("out"),
            naming: NamingPolicy {
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
                write_for_blocked: true,
                write_json_log: true,
            },
            stages: StagePolicy {
                metadata: StageRequirement::Disabled,
                replaygain: StageRequirement::Disabled,
                features: StageRequirement::Disabled,
                generate_cue: false,
            },
            failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
            container_extension: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }

    fn prepared_track_for_processor_limit_test(root: &std::path::Path) -> PreparedTrack {
        PreparedTrack {
            id: TrackId {
                source_ordinal: 1,
                disc_number: None,
                track_number: 1,
            },
            source_ref: TrackSourceRef::StagedFile(root.join("input.flac")),
            metadata: TrackMetadata {
                title: Some("Track 1".to_string()),
                track_number: Some(1),
                ..TrackMetadata::default()
            },
            expected_samples: Some(44_100),
            sample_rate: 44_100,
            bit_depth: Some(16),
        }
    }

    #[test]
    fn submission_pump_large_album_fanout_generates_only_one_deferred_unit_when_pool_is_full() {
        let pool = SharedWorkerPool::<QueueWorkOutput>::new_with_limits(
            Some(1),
            CancellationToken::new(),
            PoolLimits { ready_capacity: Some(1) },
        );
        pool.try_submit(synthetic_queue_work_unit("resident", "resident".to_string()))
            .expect("resident unit fills ready queue");

        let mut pump = SubmissionPump::new(Vec::new());
        pump.enqueue_album_fanout("album".to_string());
        let mut generated = 0usize;
        let progress = pump.flush_album_fanout_once(&pool, |job_id| {
            assert_eq!(job_id, "album");
            let unit_id = format!("album-track-{generated}");
            generated += 1;
            Some(synthetic_queue_work_unit(job_id, unit_id))
        });

        assert_eq!(progress, PumpFlushProgress::Blocked);
        assert_eq!(generated, 1, "large fanout must stop after producing one deferred WorkUnit");
        assert_eq!(pump.pending_work_units(), 1);
        assert_eq!(pump.album_fanout.len(), 1, "album cursor remains pending for later capacity");
        assert_eq!(pool.metrics().snapshot().pool_ready_queue_depth, 1);
    }

    #[tokio::test]
    async fn processor_orchestrator_large_fanout_drains_saturated_results_without_unbounded_backlog() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<QueueWorkOutput>::new_with_limits(
            Some(1),
            cancel.clone(),
            PoolLimits { ready_capacity: Some(1) },
        );
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        pool.metrics().set_test_event_sender(events_tx);
        let mut run = pool.start();

        for index in 0..5usize {
            pool.submit(synthetic_queue_work_unit_for_processor_test(
                "preload",
                format!("preload-{index}"),
                index,
            ))
            .await;
        }
        // With ready_capacity: 1, submits are serialized through the semaphore.
        // The worker interleaves Started/Finished events for each unit. Wait for
        // 5 Started events (ignoring Finished events between them). The 5th Started
        // is the blocked send (channel full at capacity 4, 5th send pending).
        {
            let mut started_count = 0usize;
            while started_count < 5 {
                let event = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    events_rx.recv(),
                )
                .await
                .expect("scheduler event arrives before timeout")
                .expect("event sender remains live");
                if event == crate::convert::pipeline::SchedulerTestEvent::WorkerResultSendStarted {
                    started_count += 1;
                }
            }
        }

        let (resident_started_tx, resident_started_rx) = tokio::sync::oneshot::channel();
        let (release_resident_tx, release_resident_rx) = tokio::sync::oneshot::channel();
        pool.try_submit(blocking_synthetic_processor_unit(
            "resident",
            "resident",
            10_000,
            resident_started_tx,
            release_resident_rx,
        ))
        .expect("blocked worker leaves exactly one ready slot to fill");
        assert_eq!(pool.metrics().snapshot().pool_ready_queue_depth, 1);

        let mut submissions = SubmissionPump::new(Vec::new());
        let mut pending_albums = BTreeMap::new();
        let mut terminal = BTreeMap::new();
        let mut job_to_item = BTreeMap::new();
        let tool_paths = HashMap::new();
        let (progress_tx, _progress_rx) = broadcast::channel(16);

        submissions.enqueue_synthetic_album_fanout(
            "large-album".to_string(),
            "large-item".to_string(),
            64,
            pool.metrics(),
        );
        submissions.record_backlog(pool.metrics(), &pending_albums);
        assert_eq!(pool.metrics().snapshot().submission_backlog_depth, 64);

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            submissions.flush(
                &pool,
                &mut pending_albums,
                &mut terminal,
                &mut job_to_item,
                &tool_paths,
                &progress_tx,
                None,
                &cancel,
                1,
                Arc::new(ToolConcurrencyLimits::new(1, 1, 1, 1)),
            );
        })
        .await
        .expect("processor submission flush returns while result channel and ready queue are full");
        submissions.record_backlog(pool.metrics(), &pending_albums);
        let blocked = pool.metrics().snapshot();
        assert_eq!(submissions.pending_work_units(), 1);
        assert_eq!(blocked.pool_ready_queue_depth, 1);
        assert_eq!(blocked.submission_backlog_depth, 64);
        assert_eq!(blocked.ready_queue_depth, 65);

        let first_drained = tokio::time::timeout(std::time::Duration::from_secs(5), run.results.recv())
            .await
            .expect("processor can drain one result even while a large fanout is pending")
            .expect("preloaded result is present");
        assert!(matches!(
            first_drained.outcome,
            Ok(QueueWorkOutput::SyntheticEncoded { job_id, .. }) if job_id == "preload"
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), resident_started_rx)
            .await
            .expect("resident work starts after draining a result")
            .expect("resident start signal is delivered");

        submissions.flush(
            &pool,
            &mut pending_albums,
            &mut terminal,
            &mut job_to_item,
            &tool_paths,
            &progress_tx,
            None,
            &cancel,
            1,
            Arc::new(ToolConcurrencyLimits::new(1, 1, 1, 1)),
        );
        submissions.record_backlog(pool.metrics(), &pending_albums);
        let resumed = pool.metrics().snapshot();
        assert_eq!(submissions.pending_work_units(), 1);
        assert_eq!(resumed.pool_ready_queue_depth, 1);
        assert_eq!(resumed.submission_backlog_depth, 63);
        assert_eq!(resumed.ready_queue_depth, 64);
        assert_eq!(resumed.peak_submission_backlog_depth, 64);

        for _ in 0..4 {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), run.results.recv())
                .await
                .expect("preloaded results drain after progress resumes")
                .expect("preloaded result remains available");
        }
        release_resident_tx.send(()).expect("release resident worker");
        let resident = tokio::time::timeout(std::time::Duration::from_secs(5), run.results.recv())
            .await
            .expect("resident result drains")
            .expect("resident result exists");
        assert!(matches!(
            resident.outcome,
            Ok(QueueWorkOutput::SyntheticEncoded { job_id, .. }) if job_id == "resident"
        ));
        let fanout = tokio::time::timeout(std::time::Duration::from_secs(5), run.results.recv())
            .await
            .expect("queued fanout unit drains")
            .expect("fanout result exists");
        assert!(matches!(
            fanout.outcome,
            Ok(QueueWorkOutput::SyntheticEncoded { job_id, .. }) if job_id == "large-album"
        ));

        run.shutdown().await;
    }

    #[test]
    fn single_file_and_realized_work_units_capture_the_same_tool_limit_arc() {
        let temp = tempfile::tempdir().expect("temp dir");
        let request = pipeline_request_for_processor_limit_test(temp.path());
        let track = prepared_track_for_processor_limit_test(temp.path());
        let (progress_tx, _progress_rx) = broadcast::channel(4);
        let tool_paths = HashMap::new();
        let limits = Arc::new(ToolConcurrencyLimits::new(2, 8, 6, 8));
        let initial_count = Arc::strong_count(&limits);

        let single_file_unit = build_single_file_work(
            request.clone(),
            &tool_paths,
            &progress_tx,
            None,
            limits.clone(),
        );
        assert_eq!(
            Arc::strong_count(&limits),
            initial_count + 1,
            "single-file fallback work captures a clone of the shared tool limit Arc"
        );

        let realized = ScheduledRealizedTrack {
            index: 0,
            track,
            final_path: temp.path().join("out/track.flac"),
            realized_path: temp.path().join("realized.wav"),
            realized_dsd_dst_stats: None,
            req: request,
            staging_root: temp.path().join("staging"),
            staging_job: "processor-limit-job".to_string(),
            convert_root: temp.path().join("converted"),
            cancel: CancellationToken::new(),
        };
        let realized_unit = build_realized_encode_work(
            "processor-limit-job".to_string(),
            realized,
            &tool_paths,
            &progress_tx,
            None,
            CancellationToken::new(),
            limits.clone(),
        );
        assert_eq!(
            Arc::strong_count(&limits),
            initial_count + 2,
            "realized-track encode work captures another clone of the same shared tool limit Arc"
        );

        drop(single_file_unit);
        drop(realized_unit);
        assert_eq!(
            Arc::strong_count(&limits),
            initial_count,
            "dropping the work units releases only their captured clones, not a separate limit object"
        );
    }

    #[test]
    fn configured_worker_count_is_escalated_to_max_tool_concurrency() {
        let limits = ToolConcurrencyLimits::new(2, 8, 6, 8);

        assert_eq!(
            effective_worker_count_for_tool_limits(1, &limits),
            8,
            "configured worker_count=1 is raised so the largest tool family cannot be starved"
        );
        assert_eq!(effective_worker_count_for_tool_limits(0, &limits), 8);
        assert_eq!(effective_worker_count_for_tool_limits(12, &limits), 12);
    }

    #[test]
    fn conversion_processor_exposes_stable_scheduler_metrics_handle() {
        let processor = ConversionProcessor::new(ProcessorConfig {
            worker_count: 1,
            tool_paths: HashMap::new(),
            default_destination_directory: None,
            scratch_directory: None,
        });

        let metrics = processor.scheduler_metrics();
        metrics.record_jobs_queued(2);
        metrics.record_submission_backlog_depth(1);

        let snapshot = processor.scheduler_metrics_snapshot();
        assert_eq!(snapshot.jobs_queued, 2);
        assert_eq!(snapshot.submission_backlog_depth, 1);
        assert_eq!(snapshot.ready_queue_depth, 1);
    }
}
