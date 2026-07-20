//! Shared worker pool and album completion accounting for Chunk 2.
//!
//! The queue has no job partitioning. Idle workers pull the next ready unit.
//! A multi-step encode chain remains sequential inside one track work unit;
//! different tracks and different queue items can run concurrently.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Notify, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::types::TrackId;

pub type JobId = String;
pub type WorkUnitId = String;
pub type BoxWorkFuture<R> = Pin<Box<dyn Future<Output = Result<R, String>> + Send + 'static>>;
pub type WorkFn<R> = Box<dyn FnOnce(CancellationToken) -> BoxWorkFuture<R> + Send + 'static>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkKind {
    MaterializeItem,
    PipelineItem,
    SingleFile,
    ArchiveExtract,
    SacdExtractTrack { track_id: TrackId },
    CueSplitTrack { track_id: TrackId },
    EncodeTrack { track_id: TrackId },
    AlbumPostProcess,
}

pub struct WorkUnit<R: Send + 'static> {
    pub job_id: JobId,
    pub unit_id: WorkUnitId,
    pub kind: WorkKind,
    pub task: WorkFn<R>,
}

#[derive(Debug)]
pub struct WorkResult<R> {
    pub job_id: JobId,
    pub unit_id: WorkUnitId,
    pub kind: WorkKind,
    pub outcome: Result<R, String>,
    pub elapsed: Duration,
}

/// Limits for ready work waiting in the shared scheduler queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolLimits {
    /// Maximum ready work units waiting in the queue. None keeps the legacy unbounded queue.
    pub ready_capacity: Option<usize>,
}

impl PoolLimits {
    fn normalized(self) -> Self {
        Self {
            ready_capacity: self.ready_capacity.map(|capacity| capacity.max(1)),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchedulerMetricsSnapshot {
    pub jobs_queued: u64,
    pub jobs_completed: u64,
    pub jobs_failed: u64,
    pub tracks_materialized: u64,
    /// Successfully encoded tracks only. Failed/cancelled track outputs are counted in
    /// `tracks_encode_failed` and every track encode output is counted in
    /// `tracks_encode_attempted`.
    pub tracks_encoded: u64,
    pub tracks_encode_attempted: u64,
    pub tracks_encode_failed: u64,
    pub commands_started: u64,
    pub commands_failed: u64,
    pub workers_busy: u64,
    pub worker_idle_ns: u64,
    /// Total ready scheduler pressure: worker-pool ready queue plus processor-side
    /// submission backlog.
    pub ready_queue_depth: u64,
    pub peak_queue_depth: u64,
    /// Worker-pool queue depth only.
    pub pool_ready_queue_depth: u64,
    /// Processor-side deferred/logical submission depth only.
    pub submission_backlog_depth: u64,
    pub peak_submission_backlog_depth: u64,
    pub tool_runtime_ns_total: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub cleanup_paths_deleted: u64,
    pub cleanup_paths_failed: u64,
}


#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerTestEvent {
    WorkerIdleWaitStarted,
    WorkerResultSendStarted,
    WorkerResultSendFinished,
    SubmitManyWaitingForCapacity,
}

/// Lock-free scheduler counters for TUI, logging, and tests.
#[derive(Debug, Default)]
pub struct SchedulerMetrics {
    pub jobs_queued: AtomicU64,
    pub jobs_completed: AtomicU64,
    pub jobs_failed: AtomicU64,
    pub tracks_materialized: AtomicU64,
    /// Successfully encoded tracks only.
    pub tracks_encoded: AtomicU64,
    pub tracks_encode_attempted: AtomicU64,
    pub tracks_encode_failed: AtomicU64,
    pub commands_started: AtomicU64,
    pub commands_failed: AtomicU64,
    pub workers_busy: AtomicU64,
    pub worker_idle_ns: AtomicU64,
    /// Total ready scheduler pressure: worker-pool queue + processor-side backlog.
    pub ready_queue_depth: AtomicU64,
    pub peak_queue_depth: AtomicU64,
    /// Worker-pool queue depth only.
    pub pool_ready_queue_depth: AtomicU64,
    /// Processor-side deferred/logical submission depth only.
    pub submission_backlog_depth: AtomicU64,
    pub peak_submission_backlog_depth: AtomicU64,
    pub tool_runtime_ns_total: AtomicU64,
    pub bytes_read: AtomicU64,
    pub bytes_written: AtomicU64,
    pub cleanup_paths_deleted: AtomicU64,
    pub cleanup_paths_failed: AtomicU64,
    #[cfg(test)]
    test_events: std::sync::Mutex<Option<mpsc::UnboundedSender<SchedulerTestEvent>>>,
}

impl SchedulerMetrics {
    #[cfg(test)]
    pub fn set_test_event_sender(&self, sender: mpsc::UnboundedSender<SchedulerTestEvent>) {
        if let Ok(mut guard) = self.test_events.lock() {
            *guard = Some(sender);
        }
    }

    #[cfg(test)]
    fn emit_test_event(&self, event: SchedulerTestEvent) {
        if let Ok(guard) = self.test_events.lock() {
            if let Some(sender) = guard.as_ref() {
                let _ = sender.send(event);
            }
        }
    }

    pub fn snapshot(&self) -> SchedulerMetricsSnapshot {
        SchedulerMetricsSnapshot {
            jobs_queued: self.jobs_queued.load(Ordering::Relaxed),
            jobs_completed: self.jobs_completed.load(Ordering::Relaxed),
            jobs_failed: self.jobs_failed.load(Ordering::Relaxed),
            tracks_materialized: self.tracks_materialized.load(Ordering::Relaxed),
            tracks_encoded: self.tracks_encoded.load(Ordering::Relaxed),
            tracks_encode_attempted: self.tracks_encode_attempted.load(Ordering::Relaxed),
            tracks_encode_failed: self.tracks_encode_failed.load(Ordering::Relaxed),
            commands_started: self.commands_started.load(Ordering::Relaxed),
            commands_failed: self.commands_failed.load(Ordering::Relaxed),
            workers_busy: self.workers_busy.load(Ordering::Acquire),
            worker_idle_ns: self.worker_idle_ns.load(Ordering::Relaxed),
            ready_queue_depth: self.ready_queue_depth.load(Ordering::Acquire),
            peak_queue_depth: self.peak_queue_depth.load(Ordering::Acquire),
            pool_ready_queue_depth: self.pool_ready_queue_depth.load(Ordering::Acquire),
            submission_backlog_depth: self.submission_backlog_depth.load(Ordering::Acquire),
            peak_submission_backlog_depth: self.peak_submission_backlog_depth.load(Ordering::Acquire),
            tool_runtime_ns_total: self.tool_runtime_ns_total.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            cleanup_paths_deleted: self.cleanup_paths_deleted.load(Ordering::Relaxed),
            cleanup_paths_failed: self.cleanup_paths_failed.load(Ordering::Relaxed),
        }
    }

    pub fn reset(&self) {
        self.jobs_queued.store(0, Ordering::Relaxed);
        self.jobs_completed.store(0, Ordering::Relaxed);
        self.jobs_failed.store(0, Ordering::Relaxed);
        self.tracks_materialized.store(0, Ordering::Relaxed);
        self.tracks_encoded.store(0, Ordering::Relaxed);
        self.tracks_encode_attempted.store(0, Ordering::Relaxed);
        self.tracks_encode_failed.store(0, Ordering::Relaxed);
        self.commands_started.store(0, Ordering::Relaxed);
        self.commands_failed.store(0, Ordering::Relaxed);
        self.workers_busy.store(0, Ordering::Release);
        self.worker_idle_ns.store(0, Ordering::Relaxed);
        self.ready_queue_depth.store(0, Ordering::Release);
        self.peak_queue_depth.store(0, Ordering::Release);
        self.pool_ready_queue_depth.store(0, Ordering::Release);
        self.submission_backlog_depth.store(0, Ordering::Release);
        self.peak_submission_backlog_depth.store(0, Ordering::Release);
        self.tool_runtime_ns_total.store(0, Ordering::Relaxed);
        self.bytes_read.store(0, Ordering::Relaxed);
        self.bytes_written.store(0, Ordering::Relaxed);
        self.cleanup_paths_deleted.store(0, Ordering::Relaxed);
        self.cleanup_paths_failed.store(0, Ordering::Relaxed);
    }

    pub fn record_jobs_queued(&self, count: u64) {
        if count > 0 {
            saturating_add(&self.jobs_queued, count);
        }
    }

    pub fn record_job_completed(&self) {
        saturating_add(&self.jobs_completed, 1);
    }

    pub fn record_job_failed(&self) {
        saturating_add(&self.jobs_failed, 1);
    }

    pub fn record_tracks_materialized(&self, count: u64) {
        if count > 0 {
            saturating_add(&self.tracks_materialized, count);
        }
    }

    pub fn record_track_encoded(&self) {
        saturating_add(&self.tracks_encoded, 1);
    }

    pub fn record_track_encode_output(&self, ok: bool) {
        saturating_add(&self.tracks_encode_attempted, 1);
        if ok {
            saturating_add(&self.tracks_encoded, 1);
        } else {
            saturating_add(&self.tracks_encode_failed, 1);
        }
    }

    pub fn record_command_started(&self) {
        saturating_add(&self.commands_started, 1);
    }

    pub fn record_command_failed(&self) {
        saturating_add(&self.commands_failed, 1);
    }

    pub fn record_worker_busy_started(&self) {
        saturating_add(&self.workers_busy, 1);
    }

    pub fn record_worker_busy_finished(&self) {
        saturating_sub(&self.workers_busy, 1);
    }

    pub fn record_worker_idle_duration(&self, duration: Duration) {
        record_duration(&self.worker_idle_ns, duration);
    }

    pub fn record_tool_runtime(&self, duration: Duration) {
        record_duration(&self.tool_runtime_ns_total, duration);
    }

    pub fn record_pool_queue_depth(&self, depth: usize) {
        self.pool_ready_queue_depth.store(depth as u64, Ordering::Release);
        self.refresh_total_ready_depth();
    }

    pub fn record_submission_backlog_depth(&self, depth: usize) {
        let depth = depth as u64;
        self.submission_backlog_depth.store(depth, Ordering::Release);
        record_peak(&self.peak_submission_backlog_depth, depth);
        self.refresh_total_ready_depth();
    }

    pub fn record_queue_depth(&self, depth: usize) {
        self.record_pool_queue_depth(depth);
    }

    fn refresh_total_ready_depth(&self) {
        let depth = self
            .pool_ready_queue_depth
            .load(Ordering::Acquire)
            .saturating_add(self.submission_backlog_depth.load(Ordering::Acquire));
        self.ready_queue_depth.store(depth, Ordering::Release);
        record_peak(&self.peak_queue_depth, depth);
    }
}

pub enum TrySubmitError<R: Send + 'static> {
    QueueFull {
        unit: WorkUnit<R>,
        capacity: usize,
        depth: usize,
    },
    /// Retained for source compatibility with the earlier non-blocking API shape.
    /// The current semaphore-backed implementation does not use transient queue-lock
    /// contention as a submit rejection reason.
    QueueBusy(WorkUnit<R>),
}

impl<R: Send + 'static> std::fmt::Debug for TrySubmitError<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrySubmitError::QueueFull { capacity, depth, .. } => f
                .debug_struct("QueueFull")
                .field("capacity", capacity)
                .field("depth", depth)
                .finish(),
            TrySubmitError::QueueBusy(_) => f.write_str("QueueBusy"),
        }
    }
}

impl<R: Send + 'static> TrySubmitError<R> {
    pub fn into_unit(self) -> WorkUnit<R> {
        match self {
            TrySubmitError::QueueFull { unit, .. } | TrySubmitError::QueueBusy(unit) => unit,
        }
    }
}

pub enum TrySubmitManyError<R: Send + 'static> {
    BatchTooLarge {
        units: Vec<WorkUnit<R>>,
        capacity: usize,
    },
    QueueFull {
        units: Vec<WorkUnit<R>>,
        capacity: usize,
        depth: usize,
    },
    /// Retained for source compatibility. Bounded capacity is decided by semaphore
    /// permits, so queue-lock contention is not reported as a batch rejection reason.
    QueueBusy(Vec<WorkUnit<R>>),
}

impl<R: Send + 'static> std::fmt::Debug for TrySubmitManyError<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrySubmitManyError::BatchTooLarge { units, capacity } => f
                .debug_struct("BatchTooLarge")
                .field("units", &units.len())
                .field("capacity", capacity)
                .finish(),
            TrySubmitManyError::QueueFull { units, capacity, depth } => f
                .debug_struct("QueueFull")
                .field("units", &units.len())
                .field("capacity", capacity)
                .field("depth", depth)
                .finish(),
            TrySubmitManyError::QueueBusy(units) => f
                .debug_struct("QueueBusy")
                .field("units", &units.len())
                .finish(),
        }
    }
}

impl<R: Send + 'static> TrySubmitManyError<R> {
    pub fn into_units(self) -> Vec<WorkUnit<R>> {
        match self {
            TrySubmitManyError::BatchTooLarge { units, .. }
            | TrySubmitManyError::QueueFull { units, .. }
            | TrySubmitManyError::QueueBusy(units) => units,
        }
    }
}

pub enum SubmitManyError<R: Send + 'static> {
    BatchTooLarge {
        units: Vec<WorkUnit<R>>,
        capacity: usize,
    },
    Cancelled(Vec<WorkUnit<R>>),
}

impl<R: Send + 'static> std::fmt::Debug for SubmitManyError<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubmitManyError::BatchTooLarge { units, capacity } => f
                .debug_struct("BatchTooLarge")
                .field("units", &units.len())
                .field("capacity", capacity)
                .finish(),
            SubmitManyError::Cancelled(units) => f
                .debug_struct("Cancelled")
                .field("units", &units.len())
                .finish(),
        }
    }
}

impl<R: Send + 'static> SubmitManyError<R> {
    pub fn into_units(self) -> Vec<WorkUnit<R>> {
        match self {
            SubmitManyError::BatchTooLarge { units, .. } | SubmitManyError::Cancelled(units) => units,
        }
    }
}

#[derive(Clone)]
pub struct SharedWorkerPool<R: Send + 'static> {
    queue: Arc<Mutex<VecDeque<WorkUnit<R>>>>,
    notify: Arc<Notify>,
    space: Option<Arc<Semaphore>>,
    cancel: CancellationToken,
    worker_count: usize,
    limits: PoolLimits,
    metrics: Arc<SchedulerMetrics>,
}

impl<R: Send + 'static> SharedWorkerPool<R> {
    pub fn new(worker_count: Option<usize>, cancel: CancellationToken) -> Self {
        Self::with_limits(worker_count, cancel, PoolLimits::default())
    }

    pub fn new_with_limits(
        worker_count: Option<usize>,
        cancel: CancellationToken,
        limits: PoolLimits,
    ) -> Self {
        Self::with_limits(worker_count, cancel, limits)
    }

    pub fn new_with_limits_and_metrics(
        worker_count: Option<usize>,
        cancel: CancellationToken,
        limits: PoolLimits,
        metrics: Arc<SchedulerMetrics>,
    ) -> Self {
        Self::with_limits_and_metrics(worker_count, cancel, limits, metrics)
    }

    pub fn with_limits(
        worker_count: Option<usize>,
        cancel: CancellationToken,
        limits: PoolLimits,
    ) -> Self {
        Self::with_limits_and_metrics(
            worker_count,
            cancel,
            limits,
            Arc::new(SchedulerMetrics::default()),
        )
    }

    pub fn with_limits_and_metrics(
        worker_count: Option<usize>,
        cancel: CancellationToken,
        limits: PoolLimits,
        metrics: Arc<SchedulerMetrics>,
    ) -> Self {
        let detected = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .saturating_sub(1)
            .max(1);
        let limits = limits.normalized();
        Self {
            queue: Arc::new(Mutex::new(VecDeque::new())),
            notify: Arc::new(Notify::new()),
            space: limits
                .ready_capacity
                .map(|capacity| Arc::new(Semaphore::new(capacity))),
            cancel,
            worker_count: worker_count.unwrap_or(detected).max(1),
            limits,
            metrics,
        }
    }

    pub fn worker_count(&self) -> usize {
        self.worker_count
    }

    pub fn limits(&self) -> PoolLimits {
        self.limits
    }

    pub fn metrics(&self) -> &SchedulerMetrics {
        self.metrics.as_ref()
    }

    pub fn metrics_handle(&self) -> Arc<SchedulerMetrics> {
        self.metrics.clone()
    }

    pub async fn submit(&self, unit: WorkUnit<R>) {
        if let Some(space) = self.space.as_ref() {
            let acquired = tokio::select! {
                permit = space.clone().acquire_owned() => permit,
                _ = self.cancel.cancelled() => return,
            };
            let Ok(permit) = acquired else {
                return;
            };
            std::mem::forget(permit);
        }

        {
            let mut queue = self.queue.lock().expect("worker queue mutex poisoned");
            queue.push_back(unit);
            let new_depth = queue.len();
            self.metrics.record_jobs_queued(1);
            self.metrics.record_queue_depth(new_depth);
        }
        self.notify.notify_one();
    }

    pub fn try_submit(&self, unit: WorkUnit<R>) -> Result<(), TrySubmitError<R>> {
        self.try_submit_inner(unit, true)
    }

    pub(crate) fn try_submit_counted(&self, unit: WorkUnit<R>) -> Result<(), TrySubmitError<R>> {
        self.try_submit_inner(unit, false)
    }

    fn try_submit_inner(
        &self,
        unit: WorkUnit<R>,
        record_queued: bool,
    ) -> Result<(), TrySubmitError<R>> {
        if let Some(space) = self.space.as_ref() {
            match space.clone().try_acquire_owned() {
                Ok(permit) => std::mem::forget(permit),
                Err(_) => {
                    let capacity = self.limits.ready_capacity.unwrap_or(0);
                    let depth = capacity.saturating_sub(space.available_permits());
                    return Err(TrySubmitError::QueueFull { unit, capacity, depth });
                }
            }
        }

        {
            let mut queue = self.queue.lock().expect("worker queue mutex poisoned");
            queue.push_back(unit);
            let new_depth = queue.len();
            if record_queued {
                self.metrics.record_jobs_queued(1);
            }
            self.metrics.record_queue_depth(new_depth);
        }
        self.notify.notify_one();
        Ok(())
    }

    /// Enqueue a batch as one admission operation.
    ///
    /// With no ready-capacity limit this preserves the legacy behavior: all units are
    /// appended under one queue lock. With a limit, the method waits until the full
    /// batch fits and then appends all units under one lock. It never admits a
    /// prefix of a batch.
    ///
    /// A batch larger than `ready_capacity` cannot ever fit without violating the
    /// configured limit, so the method returns the full batch in `BatchTooLarge`
    /// instead of waiting forever.
    pub async fn submit_many<I>(&self, units: I) -> Result<(), SubmitManyError<R>>
    where
        I: IntoIterator<Item = WorkUnit<R>>,
    {
        let mut units: Vec<_> = units.into_iter().collect();
        if units.is_empty() {
            return Ok(());
        }

        let added = units.len();
        if let Some(space) = self.space.as_ref() {
            let capacity = self.limits.ready_capacity.unwrap_or(0);
            if added > capacity {
                return Err(SubmitManyError::BatchTooLarge { units, capacity });
            }
            #[cfg(test)]
            if space.available_permits() < added {
                self.metrics.emit_test_event(SchedulerTestEvent::SubmitManyWaitingForCapacity);
            }
            let permit_count = match u32::try_from(added) {
                Ok(count) => count,
                Err(_) => return Err(SubmitManyError::BatchTooLarge { units, capacity }),
            };
            let acquired = tokio::select! {
                permit = space.clone().acquire_many_owned(permit_count) => permit,
                _ = self.cancel.cancelled() => return Err(SubmitManyError::Cancelled(units)),
            };
            let Ok(permits) = acquired else {
                return Err(SubmitManyError::Cancelled(units));
            };
            std::mem::forget(permits);
        }

        {
            let mut queue = self.queue.lock().expect("worker queue mutex poisoned");
            queue.extend(units.drain(..));
            self.metrics.record_jobs_queued(added as u64);
            self.metrics.record_queue_depth(queue.len());
        }
        for _ in 0..added {
            self.notify.notify_one();
        }
        Ok(())
    }

    pub fn try_submit_many<I>(&self, units: I) -> Result<(), TrySubmitManyError<R>>
    where
        I: IntoIterator<Item = WorkUnit<R>>,
    {
        let units: Vec<_> = units.into_iter().collect();
        if units.is_empty() {
            return Ok(());
        }

        let added = units.len();
        if let Some(space) = self.space.as_ref() {
            let capacity = self.limits.ready_capacity.unwrap_or(0);
            if added > capacity {
                return Err(TrySubmitManyError::BatchTooLarge { units, capacity });
            }
            let permit_count = match u32::try_from(added) {
                Ok(count) => count,
                Err(_) => return Err(TrySubmitManyError::BatchTooLarge { units, capacity }),
            };
            match space.clone().try_acquire_many_owned(permit_count) {
                Ok(permits) => std::mem::forget(permits),
                Err(_) => {
                    let depth = capacity.saturating_sub(space.available_permits());
                    return Err(TrySubmitManyError::QueueFull { units, capacity, depth });
                }
            }
        }

        {
            let mut queue = self.queue.lock().expect("worker queue mutex poisoned");
            queue.extend(units);
            self.metrics.record_jobs_queued(added as u64);
            self.metrics.record_queue_depth(queue.len());
        }
        for _ in 0..added {
            self.notify.notify_one();
        }
        Ok(())
    }

    pub fn start(&self) -> WorkerPoolRun<R> {
        let (result_tx, result_rx) = mpsc::channel(self.worker_count * 4);
        let mut workers = JoinSet::new();
        for worker_index in 0..self.worker_count {
            let queue = self.queue.clone();
            let notify = self.notify.clone();
            let space = self.space.clone();
            let cancel = self.cancel.clone();
            let result_tx = result_tx.clone();
            let metrics = self.metrics.clone();
            workers.spawn(async move {
                worker_loop(
                    worker_index,
                    queue,
                    notify,
                    space,
                    cancel,
                    result_tx,
                    metrics,
                )
                .await;
            });
        }
        WorkerPoolRun {
            results: result_rx,
            workers,
            cancel: self.cancel.clone(),
        }
    }
}

pub struct WorkerPoolRun<R: Send + 'static> {
    pub results: mpsc::Receiver<WorkResult<R>>,
    workers: JoinSet<()>,
    cancel: CancellationToken,
}

impl<R: Send + 'static> WorkerPoolRun<R> {
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        while self.workers.join_next().await.is_some() {}
    }
}

async fn worker_loop<R: Send + 'static>(
    _worker_index: usize,
    queue: Arc<Mutex<VecDeque<WorkUnit<R>>>>,
    notify: Arc<Notify>,
    space: Option<Arc<Semaphore>>,
    cancel: CancellationToken,
    result_tx: mpsc::Sender<WorkResult<R>>,
    metrics: Arc<SchedulerMetrics>,
) {
    loop {
        if cancel.is_cancelled() {
            break;
        }

        let unit = {
            let mut queue = queue.lock().expect("worker queue mutex poisoned");
            let unit = queue.pop_front();
            if unit.is_some() {
                metrics.record_queue_depth(queue.len());
            }
            unit
        };
        if unit.is_some() {
            if let Some(space) = space.as_ref() {
                space.add_permits(1);
            }
        }

        let Some(unit) = unit else {
            let idle_started = Instant::now();
            #[cfg(test)]
            metrics.emit_test_event(SchedulerTestEvent::WorkerIdleWaitStarted);
            tokio::select! {
                _ = notify.notified() => {
                    metrics.record_worker_idle_duration(idle_started.elapsed());
                    continue;
                }
                _ = cancel.cancelled() => {
                    metrics.record_worker_idle_duration(idle_started.elapsed());
                    break;
                }
            }
        };

        metrics.record_command_started();
        metrics.record_worker_busy_started();
        let busy_guard = BusyWorkerGuard::new(metrics.as_ref());

        let job_id = unit.job_id.clone();
        let unit_id = unit.unit_id.clone();
        let kind = unit.kind.clone();
        let started = Instant::now();
        let outcome = (unit.task)(cancel.clone()).await;
        let elapsed = started.elapsed();
        metrics.record_tool_runtime(elapsed);
        if outcome.is_err() {
            metrics.record_command_failed();
        }
        let result = WorkResult {
            job_id,
            unit_id,
            kind,
            outcome,
            elapsed,
        };
        #[cfg(test)]
        metrics.emit_test_event(SchedulerTestEvent::WorkerResultSendStarted);
        let send_failed = result_tx.send(result).await.is_err();
        #[cfg(test)]
        metrics.emit_test_event(SchedulerTestEvent::WorkerResultSendFinished);
        drop(busy_guard);
        if send_failed {
            break;
        }
    }
}

struct BusyWorkerGuard<'a> {
    metrics: &'a SchedulerMetrics,
}

impl<'a> BusyWorkerGuard<'a> {
    fn new(metrics: &'a SchedulerMetrics) -> Self {
        Self { metrics }
    }
}

impl<'a> Drop for BusyWorkerGuard<'a> {
    fn drop(&mut self) {
        self.metrics.record_worker_busy_finished();
    }
}

fn duration_ns_u64(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}

fn record_duration(counter: &AtomicU64, duration: Duration) {
    saturating_add(counter, duration_ns_u64(duration).max(1));
}

fn saturating_add(counter: &AtomicU64, value: u64) {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_add(value);
        match counter.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn saturating_sub(counter: &AtomicU64, value: u64) {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let next = current.saturating_sub(value);
        match counter.compare_exchange_weak(
            current,
            next,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn record_peak(counter: &AtomicU64, value: u64) {
    let mut current = counter.load(Ordering::Acquire);
    while value > current {
        match counter.compare_exchange_weak(
            current,
            value,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlbumReadiness {
    Waiting { finished: usize, expected: usize },
    ReadyForPostProcess,
    Failed { finished: usize, expected: usize, failed: usize },
}

#[derive(Debug, Clone)]
struct AlbumState {
    expected_tracks: usize,
    finished_tracks: usize,
    failed_tracks: usize,
    allow_partial: bool,
}

#[derive(Debug, Default)]
pub struct AlbumCompletionTracker {
    albums: BTreeMap<JobId, AlbumState>,
}

impl AlbumCompletionTracker {
    pub fn register_album(&mut self, job_id: JobId, expected_tracks: usize, allow_partial: bool) {
        self.albums.insert(
            job_id,
            AlbumState {
                expected_tracks,
                finished_tracks: 0,
                failed_tracks: 0,
                allow_partial,
            },
        );
    }

    pub fn mark_track_finished(&mut self, job_id: &str, ok: bool) -> AlbumReadiness {
        let Some(state) = self.albums.get_mut(job_id) else {
            return AlbumReadiness::Failed { finished: 1, expected: 1, failed: 1 };
        };
        state.finished_tracks += 1;
        if !ok {
            state.failed_tracks += 1;
        }
        if state.failed_tracks > 0 && !state.allow_partial {
            return AlbumReadiness::Failed {
                finished: state.finished_tracks,
                expected: state.expected_tracks,
                failed: state.failed_tracks,
            };
        }
        if state.finished_tracks >= state.expected_tracks {
            AlbumReadiness::ReadyForPostProcess
        } else {
            AlbumReadiness::Waiting {
                finished: state.finished_tracks,
                expected: state.expected_tracks,
            }
        }
    }
}

pub fn boxed_work<R, F, Fut>(f: F) -> WorkFn<R>
where
    R: Send + 'static,
    F: FnOnce(CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = Result<R, String>> + Send + 'static,
{
    Box::new(move |cancel| Box::pin(f(cancel)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn album_tracker_blocks_failed_album_when_partial_not_allowed() {
        let mut tracker = AlbumCompletionTracker::default();
        tracker.register_album("job".to_string(), 2, false);
        assert_eq!(
            tracker.mark_track_finished("job", false),
            AlbumReadiness::Failed { finished: 1, expected: 2, failed: 1 }
        );
    }

    #[test]
    fn failed_album_keeps_waiting_until_every_expected_track_has_terminal_accounting() {
        let mut tracker = AlbumCompletionTracker::default();
        tracker.register_album("job".to_string(), 3, false);
        assert_eq!(
            tracker.mark_track_finished("job", false),
            AlbumReadiness::Failed { finished: 1, expected: 3, failed: 1 }
        );
        assert_eq!(
            tracker.mark_track_finished("job", false),
            AlbumReadiness::Failed { finished: 2, expected: 3, failed: 2 }
        );
        assert_eq!(
            tracker.mark_track_finished("job", false),
            AlbumReadiness::Failed { finished: 3, expected: 3, failed: 3 }
        );
    }

    #[test]
    fn album_tracker_allows_partial_when_configured() {
        let mut tracker = AlbumCompletionTracker::default();
        tracker.register_album("job".to_string(), 2, true);
        assert!(matches!(tracker.mark_track_finished("job", false), AlbumReadiness::Waiting { .. }));
        assert_eq!(tracker.mark_track_finished("job", true), AlbumReadiness::ReadyForPostProcess);
    }

    #[tokio::test]
    async fn shared_pool_executes_ready_units_from_multiple_jobs() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<usize>::new(Some(2), cancel.clone());
        let mut run = pool.start();

        for (job, value) in [("a", 1usize), ("b", 2usize), ("a", 3usize), ("b", 4usize)] {
            pool.submit(WorkUnit {
                job_id: job.to_string(),
                unit_id: format!("unit-{job}-{value}"),
                kind: WorkKind::SingleFile,
                task: boxed_work(move |_cancel| async move { Ok(value) }),
            })
            .await;
        }

        let mut seen = Vec::new();
        while seen.len() < 4 {
            let result = run.results.recv().await.expect("worker result");
            seen.push((result.job_id, result.outcome.unwrap()));
        }
        run.shutdown().await;
        seen.sort();
        assert_eq!(seen, vec![
            ("a".to_string(), 1),
            ("a".to_string(), 3),
            ("b".to_string(), 2),
            ("b".to_string(), 4),
        ]);
    }

    fn usize_test_unit(unit_id: String, value: usize) -> WorkUnit<usize> {
        WorkUnit {
            job_id: "job".to_string(),
            unit_id,
            kind: WorkKind::SingleFile,
            task: boxed_work(move |_cancel| async move { Ok(value) }),
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum TestSchedulerOutput {
        Materialized { job_id: String, tracks: usize },
        Encoded { job_id: String, track_index: usize },
        PostProcessed { job_id: String },
    }

    fn test_track_id(index: usize) -> TrackId {
        TrackId {
            source_ordinal: index as u32,
            disc_number: None,
            track_number: index as u32 + 1,
        }
    }

    fn scheduler_test_unit(
        job_id: &str,
        unit_id: String,
        kind: WorkKind,
        output: TestSchedulerOutput,
    ) -> WorkUnit<TestSchedulerOutput> {
        WorkUnit {
            job_id: job_id.to_string(),
            unit_id,
            kind,
            task: boxed_work(move |_cancel| async move { Ok(output) }),
        }
    }

    async fn expect_scheduler_event(
        events: &mut mpsc::UnboundedReceiver<SchedulerTestEvent>,
        expected: SchedulerTestEvent,
    ) {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for scheduler event {expected:?}"))
                .expect("scheduler event channel open");
            if event == expected {
                return;
            }
        }
    }

    #[derive(Default)]
    struct LazyOrchestratorBacklog {
        deferred_unit: Option<WorkUnit<TestSchedulerOutput>>,
        album_fanout: VecDeque<(String, usize, usize)>,
        postprocess: VecDeque<String>,
        track_gate: Option<Arc<tokio::sync::Semaphore>>,
        peak_deferred_units: usize,
    }

    impl LazyOrchestratorBacklog {
        fn with_track_gate(gate: Arc<tokio::sync::Semaphore>) -> Self {
            Self {
                track_gate: Some(gate),
                ..Self::default()
            }
        }

        fn enqueue_album_fanout(&mut self, job_id: String, tracks: usize) {
            self.album_fanout.push_back((job_id, 0, tracks));
        }

        fn enqueue_postprocess(&mut self, job_id: String) {
            self.postprocess.push_back(job_id);
        }

        fn store_deferred(&mut self, unit: WorkUnit<TestSchedulerOutput>) {
            assert!(self.deferred_unit.is_none(), "lazy backlog stores at most one materialized WorkUnit");
            self.deferred_unit = Some(unit);
            self.peak_deferred_units = self.peak_deferred_units.max(1);
        }

        fn try_submit_unit(
            &mut self,
            pool: &SharedWorkerPool<TestSchedulerOutput>,
            unit: WorkUnit<TestSchedulerOutput>,
        ) -> bool {
            match pool.try_submit(unit) {
                Ok(()) => true,
                Err(err) => {
                    self.store_deferred(err.into_unit());
                    false
                }
            }
        }

        fn flush(&mut self, pool: &SharedWorkerPool<TestSchedulerOutput>) {
            loop {
                if let Some(unit) = self.deferred_unit.take() {
                    if !self.try_submit_unit(pool, unit) {
                        return;
                    }
                    continue;
                }

                if let Some(job_id) = self.postprocess.pop_front() {
                    let unit = scheduler_test_unit(
                        &job_id,
                        format!("{job_id}-postprocess"),
                        WorkKind::AlbumPostProcess,
                        TestSchedulerOutput::PostProcessed { job_id: job_id.clone() },
                    );
                    if !self.try_submit_unit(pool, unit) {
                        return;
                    }
                    continue;
                }

                if let Some((job_id, next_track, tracks)) = self.album_fanout.pop_front() {
                    if next_track >= tracks {
                        continue;
                    }
                    let output = TestSchedulerOutput::Encoded { job_id: job_id.clone(), track_index: next_track };
                    let unit = if let Some(gate) = self.track_gate.clone() {
                        WorkUnit {
                            job_id: job_id.clone(),
                            unit_id: format!("{job_id}-encode-{next_track}"),
                            kind: WorkKind::EncodeTrack { track_id: test_track_id(next_track) },
                            task: boxed_work(move |_cancel| async move {
                                let permit = gate.acquire_owned().await.map_err(|_| "track gate closed".to_string())?;
                                drop(permit);
                                Ok(output)
                            }),
                        }
                    } else {
                        scheduler_test_unit(
                            &job_id,
                            format!("{job_id}-encode-{next_track}"),
                            WorkKind::EncodeTrack { track_id: test_track_id(next_track) },
                            output,
                        )
                    };
                    if next_track + 1 < tracks {
                        self.album_fanout.push_back((job_id, next_track + 1, tracks));
                    }
                    if !self.try_submit_unit(pool, unit) {
                        return;
                    }
                    continue;
                }

                break;
            }
        }
    }

    #[tokio::test]
    async fn bounded_queue_rejects_when_full_and_accepts_after_worker_pop() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<usize>::new_with_limits(
            Some(1),
            cancel.clone(),
            PoolLimits { ready_capacity: Some(1) },
        );
        let mut run = pool.start();
        let (a_started_tx, a_started_rx) = tokio::sync::oneshot::channel();
        let (release_a_tx, release_a_rx) = tokio::sync::oneshot::channel::<()>();
        let (b_started_tx, b_started_rx) = tokio::sync::oneshot::channel();
        let (release_b_tx, release_b_rx) = tokio::sync::oneshot::channel::<()>();

        pool.submit(WorkUnit {
            job_id: "job".to_string(),
            unit_id: "a".to_string(),
            kind: WorkKind::SingleFile,
            task: boxed_work(move |_cancel| async move {
                let _ = a_started_tx.send(());
                let _ = release_a_rx.await;
                Ok(1)
            }),
        })
        .await;
        a_started_rx.await.expect("first task starts");

        pool.try_submit(WorkUnit {
            job_id: "job".to_string(),
            unit_id: "b".to_string(),
            kind: WorkKind::SingleFile,
            task: boxed_work(move |_cancel| async move {
                let _ = b_started_tx.send(());
                let _ = release_b_rx.await;
                Ok(2)
            }),
        })
        .expect("one queued unit fits");

        let rejected = pool.try_submit(WorkUnit {
            job_id: "job".to_string(),
            unit_id: "c".to_string(),
            kind: WorkKind::SingleFile,
            task: boxed_work(move |_cancel| async move { Ok(3) }),
        });
        let c = match rejected {
            Err(TrySubmitError::QueueFull { unit, capacity, depth }) => {
                assert_eq!(capacity, 1);
                assert_eq!(depth, 1);
                unit
            }
            Err(TrySubmitError::QueueBusy(unit)) => unit,
            Ok(()) => panic!("third unit must not fit while capacity is full"),
        };

        release_a_tx.send(()).expect("release first task");
        let first = run.results.recv().await.expect("first result");
        assert_eq!(first.outcome.expect("first succeeds"), 1);
        b_started_rx.await.expect("second task starts after pop");
        pool.try_submit(c).expect("space opened after worker pop");

        release_b_tx.send(()).expect("release second task");
        let mut values = vec![run.results.recv().await.expect("second result").outcome.unwrap()];
        values.push(run.results.recv().await.expect("third result").outcome.unwrap());
        values.sort();
        run.shutdown().await;

        assert_eq!(values, vec![2, 3]);
        let snapshot = pool.metrics().snapshot();
        assert_eq!(snapshot.jobs_queued, 3);
        assert_eq!(snapshot.commands_started, 3);
        assert_eq!(snapshot.peak_queue_depth, 1);
        assert_eq!(snapshot.workers_busy, 0);
    }

    #[tokio::test]
    async fn default_unbounded_queue_accepts_more_than_bounded_capacity_would_allow() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<usize>::new(Some(1), cancel);
        for index in 0..64usize {
            pool.try_submit(WorkUnit {
                job_id: "job".to_string(),
                unit_id: format!("unit-{index}"),
                kind: WorkKind::SingleFile,
                task: boxed_work(move |_cancel| async move { Ok(index) }),
            })
            .expect("legacy unbounded queue accepts all ready work");
        }

        let snapshot = pool.metrics().snapshot();
        assert_eq!(snapshot.jobs_queued, 64);
        assert_eq!(snapshot.ready_queue_depth, 64);
        assert_eq!(snapshot.peak_queue_depth, 64);
        assert_eq!(pool.limits().ready_capacity, None);
    }

    #[tokio::test]
    async fn try_submit_many_respects_ready_capacity_as_one_batch() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<usize>::new_with_limits(
            Some(1),
            cancel,
            PoolLimits { ready_capacity: Some(2) },
        );

        pool.try_submit_many((0..2usize).map(|index| WorkUnit {
            job_id: "job".to_string(),
            unit_id: format!("fit-{index}"),
            kind: WorkKind::SingleFile,
            task: boxed_work(move |_cancel| async move { Ok(index) }),
        }))
        .expect("batch fits at capacity");

        let overflow = pool.try_submit_many((2..4usize).map(|index| WorkUnit {
            job_id: "job".to_string(),
            unit_id: format!("overflow-{index}"),
            kind: WorkKind::SingleFile,
            task: boxed_work(move |_cancel| async move { Ok(index) }),
        }));
        match overflow {
            Err(TrySubmitManyError::QueueFull { units, capacity, depth }) => {
                assert_eq!(units.len(), 2);
                assert_eq!(capacity, 2);
                assert_eq!(depth, 2);
            }
            Err(TrySubmitManyError::BatchTooLarge { units, capacity }) => {
                panic!("batch of {} should fit empty capacity {capacity}", units.len());
            }
            Err(TrySubmitManyError::QueueBusy(units)) => panic!("unexpected queue contention for {} units", units.len()),
            Ok(()) => panic!("batch must not fit once ready queue is full"),
        }

        let snapshot = pool.metrics().snapshot();
        assert_eq!(snapshot.jobs_queued, 2);
        assert_eq!(snapshot.ready_queue_depth, 2);
        assert_eq!(snapshot.peak_queue_depth, 2);
    }

    #[tokio::test]
    async fn submit_many_waits_for_full_batch_capacity_and_enqueues_batch_together() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<usize>::new_with_limits(
            Some(1),
            cancel.clone(),
            PoolLimits { ready_capacity: Some(2) },
        );
        let mut run = pool.start();

        let (head_started_tx, head_started_rx) = tokio::sync::oneshot::channel();
        let (release_head_tx, release_head_rx) = tokio::sync::oneshot::channel::<()>();
        pool.submit(WorkUnit {
            job_id: "job".to_string(),
            unit_id: "head".to_string(),
            kind: WorkKind::SingleFile,
            task: boxed_work(move |_cancel| async move {
                let _ = head_started_tx.send(());
                let _ = release_head_rx.await;
                Ok(10)
            }),
        })
        .await;
        head_started_rx.await.expect("head unit starts");

        let (resident_started_tx, resident_started_rx) = tokio::sync::oneshot::channel();
        let (release_resident_tx, release_resident_rx) = tokio::sync::oneshot::channel::<()>();
        pool.submit(WorkUnit {
            job_id: "job".to_string(),
            unit_id: "resident".to_string(),
            kind: WorkKind::SingleFile,
            task: boxed_work(move |_cancel| async move {
                let _ = resident_started_tx.send(());
                let _ = release_resident_rx.await;
                Ok(20)
            }),
        })
        .await;

        let before_batch = pool.metrics().snapshot();
        assert_eq!(before_batch.jobs_queued, 2);
        assert_eq!(before_batch.ready_queue_depth, 1);

        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        pool.metrics().set_test_event_sender(events_tx);
        let submit_pool = pool.clone();
        let submit_task = tokio::spawn(async move {
            submit_pool
                .submit_many(vec![usize_test_unit("batch-a".to_string(), 30), usize_test_unit("batch-b".to_string(), 40)])
                .await
        });

        expect_scheduler_event(&mut events_rx, SchedulerTestEvent::SubmitManyWaitingForCapacity).await;
        let while_waiting = pool.metrics().snapshot();
        assert_eq!(while_waiting.jobs_queued, 2, "submit_many must not admit a partial batch");
        assert_eq!(while_waiting.ready_queue_depth, 1);

        release_head_tx.send(()).expect("release head unit");
        let head = run.results.recv().await.expect("head result");
        assert_eq!(head.outcome.expect("head succeeds"), 10);
        resident_started_rx.await.expect("resident unit starts");

        submit_task.await.expect("submit task joins").expect("batch fits after queue drains");
        let after_batch = pool.metrics().snapshot();
        assert_eq!(after_batch.jobs_queued, 4);
        assert_eq!(after_batch.ready_queue_depth, 2);
        assert_eq!(after_batch.peak_queue_depth, 2);

        release_resident_tx.send(()).expect("release resident unit");
        let mut values = vec![run.results.recv().await.expect("resident result").outcome.unwrap()];
        values.push(run.results.recv().await.expect("batch a result").outcome.unwrap());
        values.push(run.results.recv().await.expect("batch b result").outcome.unwrap());
        values.sort();
        run.shutdown().await;

        assert_eq!(values, vec![20, 30, 40]);
        assert_eq!(pool.metrics().snapshot().workers_busy, 0);
    }


    #[tokio::test]
    async fn bounded_submit_waiters_admit_one_unit_per_freed_slot() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<usize>::new_with_limits(
            Some(1),
            cancel.clone(),
            PoolLimits { ready_capacity: Some(1) },
        );
        let mut run = pool.start();

        let (head_started_tx, head_started_rx) = tokio::sync::oneshot::channel();
        let (release_head_tx, release_head_rx) = tokio::sync::oneshot::channel::<()>();
        pool.submit(WorkUnit {
            job_id: "job".to_string(),
            unit_id: "head".to_string(),
            kind: WorkKind::SingleFile,
            task: boxed_work(move |_cancel| async move {
                let _ = head_started_tx.send(());
                let _ = release_head_rx.await;
                Ok(1)
            }),
        })
        .await;
        head_started_rx.await.expect("head starts and holds the only worker");

        let (resident_started_tx, resident_started_rx) = tokio::sync::oneshot::channel();
        let (release_resident_tx, release_resident_rx) = tokio::sync::oneshot::channel::<()>();
        pool.submit(WorkUnit {
            job_id: "job".to_string(),
            unit_id: "resident".to_string(),
            kind: WorkKind::SingleFile,
            task: boxed_work(move |_cancel| async move {
                let _ = resident_started_tx.send(());
                let _ = release_resident_rx.await;
                Ok(2)
            }),
        })
        .await;
        assert_eq!(pool.metrics().snapshot().ready_queue_depth, 1);

        let (admitted_tx, mut admitted_rx) = mpsc::unbounded_channel::<usize>();
        for value in [3usize, 4usize] {
            let submit_pool = pool.clone();
            let admitted_tx = admitted_tx.clone();
            tokio::spawn(async move {
                submit_pool.submit(usize_test_unit(format!("waiter-{value}"), value)).await;
                admitted_tx.send(value).expect("admission notification is received");
            });
        }
        drop(admitted_tx);

        release_head_tx.send(()).expect("release head unit");
        let head = run.results.recv().await.expect("head result");
        assert_eq!(head.outcome.expect("head succeeds"), 1);
        resident_started_rx.await.expect("resident starts before another slot is freed");

        let first_admitted = admitted_rx.recv().await.expect("one waiter receives the freed slot");
        assert!([3, 4].contains(&first_admitted));
        assert!(
            admitted_rx.try_recv().is_err(),
            "only one waiting submitter may complete for one popped ready unit"
        );
        assert_eq!(pool.metrics().snapshot().ready_queue_depth, 1);

        release_resident_tx.send(()).expect("release resident unit");
        let resident = run.results.recv().await.expect("resident result");
        assert_eq!(resident.outcome.expect("resident succeeds"), 2);
        let second_admitted = admitted_rx.recv().await.expect("second waiter receives the next freed slot");
        assert_ne!(first_admitted, second_admitted);

        let mut waiter_results = vec![
            run.results.recv().await.expect("first waiter result").outcome.unwrap(),
            run.results.recv().await.expect("second waiter result").outcome.unwrap(),
        ];
        waiter_results.sort();
        assert_eq!(waiter_results, vec![3, 4]);
        run.shutdown().await;
    }

    #[tokio::test]
    async fn submit_many_returns_oversized_batch_without_enqueueing() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<usize>::new_with_limits(
            Some(1),
            cancel,
            PoolLimits { ready_capacity: Some(2) },
        );

        let err = pool
            .submit_many((0..3usize).map(|index| usize_test_unit(format!("oversized-{index}"), index)))
            .await
            .expect_err("oversized batch cannot fit bounded ready capacity");

        match err {
            SubmitManyError::BatchTooLarge { units, capacity } => {
                assert_eq!(units.len(), 3);
                assert_eq!(capacity, 2);
            }
            SubmitManyError::Cancelled(units) => panic!("unexpected cancellation for {} units", units.len()),
        }

        let snapshot = pool.metrics().snapshot();
        assert_eq!(snapshot.jobs_queued, 0);
        assert_eq!(snapshot.ready_queue_depth, 0);
        assert_eq!(snapshot.peak_queue_depth, 0);
    }

    #[tokio::test]
    async fn try_submit_many_returns_oversized_batch_without_enqueueing() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<usize>::new_with_limits(
            Some(1),
            cancel,
            PoolLimits { ready_capacity: Some(2) },
        );

        let err = pool
            .try_submit_many((0..3usize).map(|index| usize_test_unit(format!("oversized-{index}"), index)))
            .expect_err("oversized try_submit_many must return the whole batch");

        match err {
            TrySubmitManyError::BatchTooLarge { units, capacity } => {
                assert_eq!(units.len(), 3);
                assert_eq!(capacity, 2);
            }
            TrySubmitManyError::QueueFull { units, capacity, depth } => {
                panic!("oversized empty-queue batch was reported as full: {} units, capacity {capacity}, depth {depth}", units.len());
            }
            TrySubmitManyError::QueueBusy(units) => panic!("unexpected queue contention for {} units", units.len()),
        }

        let snapshot = pool.metrics().snapshot();
        assert_eq!(snapshot.jobs_queued, 0);
        assert_eq!(snapshot.ready_queue_depth, 0);
        assert_eq!(snapshot.peak_queue_depth, 0);
    }

    #[tokio::test]
    async fn lazy_orchestrator_flush_returns_with_full_ready_queue_and_saturated_results() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<TestSchedulerOutput>::new_with_limits(
            Some(1),
            cancel.clone(),
            PoolLimits { ready_capacity: Some(5) },
        );
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        pool.metrics().set_test_event_sender(events_tx);
        let mut run = pool.start();

        for index in 0..5usize {
            pool.submit(scheduler_test_unit(
                "sat",
                format!("preload-{index}"),
                WorkKind::SingleFile,
                TestSchedulerOutput::Encoded { job_id: "sat".to_string(), track_index: index },
            ))
            .await;
        }
        for _ in 0..5 {
            expect_scheduler_event(&mut events_rx, SchedulerTestEvent::WorkerResultSendStarted).await;
        }

        for index in 0..5usize {
            pool.try_submit(scheduler_test_unit(
                "resident",
                format!("resident-{index}"),
                WorkKind::SingleFile,
                TestSchedulerOutput::Encoded { job_id: "resident".to_string(), track_index: index },
            ))
            .expect("worker is blocked on saturated result channel, so ready queue accepts residents until full");
        }
        assert_eq!(pool.metrics().snapshot().pool_ready_queue_depth, 5);

        let mut backlog = LazyOrchestratorBacklog::default();
        backlog.enqueue_album_fanout("overflow".to_string(), 64);
        tokio::time::timeout(Duration::from_secs(5), async { backlog.flush(&pool) })
            .await
            .expect("nonblocking flush must return even when ready queue and result channel are both full");
        assert_eq!(backlog.peak_deferred_units, 1);
        assert!(backlog.deferred_unit.is_some());

        let drained = tokio::time::timeout(Duration::from_secs(5), run.results.recv())
            .await
            .expect("orchestrator can still drain a saturated result channel")
            .expect("preloaded worker result");
        assert!(matches!(drained.outcome, Ok(TestSchedulerOutput::Encoded { .. })));

        for _ in 0..9 {
            let _ = tokio::time::timeout(Duration::from_secs(5), run.results.recv())
                .await
                .expect("remaining results drain without blocking after channel saturation is relieved")
                .expect("remaining worker result");
        }
        run.shutdown().await;
    }

    #[tokio::test]
    async fn lazy_orchestrator_model_completes_when_ready_queue_is_full() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<TestSchedulerOutput>::new_with_limits(
            Some(1),
            cancel.clone(),
            PoolLimits { ready_capacity: Some(1) },
        );
        let mut run = pool.start();
        let track_gate = Arc::new(tokio::sync::Semaphore::new(0));
        let mut backlog = LazyOrchestratorBacklog::with_track_gate(track_gate.clone());
        let mut tracker = AlbumCompletionTracker::default();
        tracker.register_album("album".to_string(), 4, false);

        pool.submit(scheduler_test_unit(
            "album",
            "materialize".to_string(),
            WorkKind::ArchiveExtract,
            TestSchedulerOutput::Materialized { job_id: "album".to_string(), tracks: 4 },
        ))
        .await;

        let result = run.results.recv().await.expect("materialization result");
        match result.outcome.expect("materialization output") {
            TestSchedulerOutput::Materialized { job_id, tracks } => backlog.enqueue_album_fanout(job_id, tracks),
            other => panic!("unexpected first output: {other:?}"),
        }

        backlog.flush(&pool);
        assert_eq!(backlog.peak_deferred_units, 1, "fanout must pause after one deferred unit");
        assert!(backlog.deferred_unit.is_some());
        track_gate.add_permits(4);

        let mut postprocessed = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while postprocessed.len() < 1 {
                backlog.flush(&pool);
                let result = run.results.recv().await.expect("orchestrator result");
                match result.outcome.expect("work output") {
                    TestSchedulerOutput::Materialized { .. } => panic!("materialization should run only once"),
                    TestSchedulerOutput::Encoded { job_id, .. } => {
                        if tracker.mark_track_finished(&job_id, true) == AlbumReadiness::ReadyForPostProcess {
                            backlog.enqueue_postprocess(job_id);
                        }
                    }
                    TestSchedulerOutput::PostProcessed { job_id } => postprocessed.push(job_id),
                }
            }
        })
        .await
        .expect("lazy orchestrator must make bounded-queue progress without hanging");

        run.shutdown().await;
        assert_eq!(postprocessed, vec!["album".to_string()]);
        assert!(backlog.deferred_unit.is_none());
        assert_eq!(backlog.peak_deferred_units, 1);
        let snapshot = pool.metrics().snapshot();
        assert_eq!(snapshot.jobs_queued, 6);
        assert_eq!(snapshot.commands_started, 6);
        assert_eq!(snapshot.peak_queue_depth, 1);
        assert_eq!(snapshot.workers_busy, 0);
    }

    #[tokio::test]
    async fn bounded_queue_multiple_albums_complete_without_starvation() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<TestSchedulerOutput>::new_with_limits(
            Some(2),
            cancel.clone(),
            PoolLimits { ready_capacity: Some(2) },
        );
        let mut run = pool.start();
        let mut backlog = LazyOrchestratorBacklog::default();
        let mut tracker = AlbumCompletionTracker::default();
        for job in ["album-a", "album-b"] {
            tracker.register_album(job.to_string(), 3, false);
            pool.try_submit(scheduler_test_unit(
                job,
                format!("materialize-{job}"),
                WorkKind::ArchiveExtract,
                TestSchedulerOutput::Materialized { job_id: job.to_string(), tracks: 3 },
            ))
            .expect("initial materialization units fit bounded ready queue");
        }

        let mut postprocessed = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while postprocessed.len() < 2 {
                backlog.flush(&pool);
                let result = run.results.recv().await.expect("orchestrator result");
                match result.outcome.expect("work output") {
                    TestSchedulerOutput::Materialized { job_id, tracks } => {
                        backlog.enqueue_album_fanout(job_id, tracks);
                    }
                    TestSchedulerOutput::Encoded { job_id, .. } => {
                        if tracker.mark_track_finished(&job_id, true) == AlbumReadiness::ReadyForPostProcess {
                            backlog.enqueue_postprocess(job_id);
                        }
                    }
                    TestSchedulerOutput::PostProcessed { job_id } => postprocessed.push(job_id),
                }
            }
        })
        .await
        .expect("multiple bounded albums must make progress without hanging");

        run.shutdown().await;
        postprocessed.sort();
        assert_eq!(postprocessed, vec!["album-a".to_string(), "album-b".to_string()]);
        assert!(backlog.deferred_unit.is_none());
        assert!(backlog.peak_deferred_units <= 1);
    }

    #[tokio::test]
    async fn scheduler_metrics_track_worker_and_orchestrator_counters() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<TestSchedulerOutput>::new_with_limits(
            Some(2),
            cancel.clone(),
            PoolLimits { ready_capacity: Some(8) },
        );
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        pool.metrics().set_test_event_sender(events_tx);
        let mut run = pool.start();
        expect_scheduler_event(&mut events_rx, SchedulerTestEvent::WorkerIdleWaitStarted).await;

        for index in 0..3usize {
            pool.submit(scheduler_test_unit(
                "album",
                format!("postprocess-{index}"),
                WorkKind::AlbumPostProcess,
                TestSchedulerOutput::PostProcessed { job_id: format!("album-{index}") },
            ))
            .await;
        }
        pool.submit(WorkUnit {
            job_id: "failed".to_string(),
            unit_id: "failed-unit".to_string(),
            kind: WorkKind::SingleFile,
            task: boxed_work(move |_cancel| async move { Err("boom".to_string()) }),
        })
        .await;

        let mut seen = 0usize;
        while seen < 4 {
            let result = run.results.recv().await.expect("worker result");
            match result.outcome {
                Ok(TestSchedulerOutput::PostProcessed { .. }) => {
                    pool.metrics().record_job_completed();
                }
                Ok(_) => {}
                Err(_) => {
                    pool.metrics().record_job_failed();
                }
            }
            seen += 1;
        }

        run.shutdown().await;
        let snapshot = pool.metrics().snapshot();
        assert_eq!(snapshot.commands_started, 4);
        assert_eq!(snapshot.commands_failed, 1);
        assert_eq!(snapshot.jobs_completed, 3);
        assert_eq!(snapshot.jobs_failed, 1);
        assert_eq!(snapshot.workers_busy, 0);
        assert!(snapshot.peak_queue_depth >= 1);
        assert!(snapshot.tool_runtime_ns_total > 0);
        assert!(snapshot.worker_idle_ns > 0);
    }

    #[tokio::test]
    async fn scheduler_metrics_show_busy_worker_while_task_is_running() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<usize>::new(Some(1), cancel);
        let mut run = pool.start();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();

        pool.submit(WorkUnit {
            job_id: "job".to_string(),
            unit_id: "blocked".to_string(),
            kind: WorkKind::SingleFile,
            task: boxed_work(move |_cancel| async move {
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Ok(7)
            }),
        })
        .await;

        started_rx.await.expect("worker starts blocked task");
        let running = pool.metrics().snapshot();
        assert_eq!(running.jobs_queued, 1);
        assert_eq!(running.commands_started, 1);
        assert_eq!(running.workers_busy, 1);
        assert_eq!(running.ready_queue_depth, 0);

        release_tx.send(()).expect("release blocked task");
        let result = run.results.recv().await.expect("worker result");
        assert_eq!(result.outcome.expect("task succeeds"), 7);
        run.shutdown().await;

        let finished = pool.metrics().snapshot();
        assert_eq!(finished.commands_started, 1);
        assert_eq!(finished.commands_failed, 0);
        assert_eq!(finished.workers_busy, 0);
        assert!(finished.tool_runtime_ns_total > 0);
    }

    #[test]
    fn scheduler_metrics_busy_gauge_does_not_underflow() {
        let metrics = SchedulerMetrics::default();
        metrics.record_worker_busy_finished();
        assert_eq!(metrics.snapshot().workers_busy, 0);

        metrics.record_worker_busy_started();
        metrics.record_worker_busy_finished();
        metrics.record_worker_busy_finished();
        assert_eq!(metrics.snapshot().workers_busy, 0);
    }

    #[test]
    fn scheduler_metrics_include_processor_submission_backlog_in_ready_depth() {
        let metrics = SchedulerMetrics::default();

        metrics.record_pool_queue_depth(2);
        metrics.record_submission_backlog_depth(5);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.pool_ready_queue_depth, 2);
        assert_eq!(snapshot.submission_backlog_depth, 5);
        assert_eq!(snapshot.ready_queue_depth, 7);
        assert_eq!(snapshot.peak_queue_depth, 7);
        assert_eq!(snapshot.peak_submission_backlog_depth, 5);

        metrics.record_submission_backlog_depth(1);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.ready_queue_depth, 3);
        assert_eq!(snapshot.peak_queue_depth, 7);
        assert_eq!(snapshot.peak_submission_backlog_depth, 5);
    }

    #[test]
    fn scheduler_metrics_distinguish_encode_outputs_from_successes() {
        let metrics = SchedulerMetrics::default();

        metrics.record_track_encode_output(true);
        metrics.record_track_encode_output(false);
        let snapshot = metrics.snapshot();

        assert_eq!(snapshot.tracks_encode_attempted, 2);
        assert_eq!(snapshot.tracks_encoded, 1);
        assert_eq!(snapshot.tracks_encode_failed, 1);
    }

    #[test]
    fn scheduler_metrics_reset_clears_external_processor_handle_state() {
        let metrics = SchedulerMetrics::default();
        metrics.record_jobs_queued(3);
        metrics.record_pool_queue_depth(2);
        metrics.record_submission_backlog_depth(4);
        metrics.record_track_encode_output(false);
        metrics.record_worker_busy_started();

        metrics.reset();
        assert_eq!(metrics.snapshot(), SchedulerMetricsSnapshot::default());
    }

    #[tokio::test]
    async fn materialization_feeds_encode_units_and_postprocess_waits_for_album_completion() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<TestSchedulerOutput>::new(Some(2), cancel.clone());
        let mut run = pool.start();
        let mut tracker = AlbumCompletionTracker::default();
        tracker.register_album("archive".to_string(), 2, false);

        pool.submit(WorkUnit {
            job_id: "archive".to_string(),
            unit_id: "archive-extract".to_string(),
            kind: WorkKind::ArchiveExtract,
            task: boxed_work(|_cancel| async move {
                Ok(TestSchedulerOutput::Materialized {
                    job_id: "archive".to_string(),
                    tracks: 2,
                })
            }),
        })
        .await;
        pool.submit(WorkUnit {
            job_id: "single".to_string(),
            unit_id: "single-file".to_string(),
            kind: WorkKind::SingleFile,
            task: boxed_work(|_cancel| async move {
                Ok(TestSchedulerOutput::PostProcessed {
                    job_id: "single".to_string(),
                })
            }),
        })
        .await;

        let mut seen_postprocess = Vec::new();
        let mut encoded = 0usize;
        while seen_postprocess.len() < 2 {
            let result = run.results.recv().await.expect("worker result");
            match result.outcome.expect("work output") {
                TestSchedulerOutput::Materialized { job_id, tracks } => {
                    for track_index in 0..tracks {
                        let submit_job = job_id.clone();
                        pool.submit(WorkUnit {
                            job_id: job_id.clone(),
                            unit_id: format!("encode-{track_index}"),
                            kind: WorkKind::EncodeTrack {
                                track_id: TrackId {
                                    source_ordinal: track_index as u32,
                                    disc_number: None,
                                    track_number: track_index as u32 + 1,
                                },
                            },
                            task: boxed_work(move |_cancel| async move {
                                Ok(TestSchedulerOutput::Encoded {
                                    job_id: submit_job,
                                    track_index,
                                })
                            }),
                        })
                        .await;
                    }
                }
                TestSchedulerOutput::Encoded { job_id, .. } => {
                    encoded += 1;
                    if tracker.mark_track_finished(&job_id, true) == AlbumReadiness::ReadyForPostProcess {
                        pool.submit(WorkUnit {
                            job_id: job_id.clone(),
                            unit_id: "album-postprocess".to_string(),
                            kind: WorkKind::AlbumPostProcess,
                            task: boxed_work(move |_cancel| async move {
                                Ok(TestSchedulerOutput::PostProcessed { job_id })
                            }),
                        })
                        .await;
                    }
                }
                TestSchedulerOutput::PostProcessed { job_id } => {
                    if job_id == "archive" {
                        assert_eq!(encoded, 2, "album postprocess must wait for all tracks");
                    }
                    seen_postprocess.push(job_id);
                }
            }
        }

        run.shutdown().await;
        seen_postprocess.sort();
        assert_eq!(seen_postprocess, vec!["archive".to_string(), "single".to_string()]);
    }

    #[tokio::test]
    async fn fail_fast_cancellation_still_collects_all_terminal_track_records() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<TestSchedulerOutput>::new(Some(2), cancel.clone());
        let mut run = pool.start();
        let mut tracker = AlbumCompletionTracker::default();
        tracker.register_album("album".to_string(), 3, false);
        let album_cancel = cancel.child_token();

        for track_index in 0..3usize {
            let track_cancel = album_cancel.clone();
            pool.submit(WorkUnit {
                job_id: "album".to_string(),
                unit_id: format!("encode-{track_index}"),
                kind: WorkKind::EncodeTrack {
                    track_id: TrackId {
                        source_ordinal: track_index as u32,
                        disc_number: None,
                        track_number: track_index as u32 + 1,
                    },
                },
                task: boxed_work(move |_pool_cancel| async move {
                    let _was_cancelled = track_cancel.is_cancelled();
                    Ok(TestSchedulerOutput::Encoded {
                        job_id: "album".to_string(),
                        track_index,
                    })
                }),
            })
            .await;
        }

        let mut terminal = Vec::new();
        while terminal.len() < 3 {
            let result = run.results.recv().await.expect("worker result");
            if let TestSchedulerOutput::Encoded { job_id, track_index } = result.outcome.expect("encoded") {
                let readiness = tracker.mark_track_finished(&job_id, false);
                if matches!(readiness, AlbumReadiness::Failed { .. }) {
                    album_cancel.cancel();
                }
                terminal.push(track_index);
            }
        }

        run.shutdown().await;
        terminal.sort();
        assert_eq!(terminal, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn cancellation_reaches_running_worker_tasks() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<&'static str>::new(Some(1), cancel.clone());
        let mut run = pool.start();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();

        pool.submit(WorkUnit {
            job_id: "job".to_string(),
            unit_id: "cancel-aware".to_string(),
            kind: WorkKind::EncodeTrack {
                track_id: TrackId {
                    source_ordinal: 0,
                    disc_number: None,
                    track_number: 1,
                },
            },
            task: boxed_work(move |task_cancel| async move {
                let _ = started_tx.send(());
                task_cancel.cancelled().await;
                Ok("cancelled")
            }),
        })
        .await;

        started_rx.await.expect("task started");
        cancel.cancel();
        let result = run.results.recv().await.expect("cancel-aware result");
        assert_eq!(result.outcome.unwrap(), "cancelled");
        run.shutdown().await;
    }

    #[tokio::test]
    async fn mixed_source_units_share_workers_without_job_partitioning() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<(&'static str, usize)>::new(Some(3), cancel.clone());
        let mut run = pool.start();
        let (started_tx, mut started_rx) = tokio::sync::mpsc::channel::<(&'static str, usize)>(8);

        for (job, unit, kind) in [
            ("single-a", 0usize, WorkKind::SingleFile),
            ("sacd", 1usize, WorkKind::SacdExtractTrack { track_id: TrackId { source_ordinal: 1, disc_number: None, track_number: 1 } }),
            ("archive", 2usize, WorkKind::ArchiveExtract),
            ("cue", 3usize, WorkKind::CueSplitTrack { track_id: TrackId { source_ordinal: 3, disc_number: None, track_number: 3 } }),
        ] {
            let started_tx = started_tx.clone();
            pool.submit(WorkUnit {
                job_id: job.to_string(),
                unit_id: format!("{job}-{unit}"),
                kind,
                task: boxed_work(move |_cancel| async move {
                    started_tx.send((job, unit)).await.map_err(|err| err.to_string())?;
                    Ok((job, unit))
                }),
            }).await;
        }
        drop(started_tx);

        let mut initially_started = Vec::new();
        while initially_started.len() < 3 {
            initially_started.push(started_rx.recv().await.expect("worker start"));
        }
        initially_started.sort();
        assert_eq!(initially_started.len(), 3, "three different ready units should occupy three workers immediately");

        let mut finished = Vec::new();
        while finished.len() < 4 {
            let result = run.results.recv().await.expect("worker result");
            finished.push(result.outcome.unwrap());
        }
        run.shutdown().await;
        finished.sort_by_key(|(_, unit)| *unit);
        assert_eq!(finished, vec![("single-a", 0), ("sacd", 1), ("archive", 2), ("cue", 3)]);
    }

    #[test]
    fn deterministic_album_accounting_waits_then_sorts_outputs() {
        let mut terminal = vec![2usize, 0usize, 1usize];
        let mut tracker = AlbumCompletionTracker::default();
        tracker.register_album("album".to_string(), terminal.len(), false);
        assert!(matches!(tracker.mark_track_finished("album", true), AlbumReadiness::Waiting { finished: 1, expected: 3 }));
        assert!(matches!(tracker.mark_track_finished("album", true), AlbumReadiness::Waiting { finished: 2, expected: 3 }));
        assert_eq!(tracker.mark_track_finished("album", true), AlbumReadiness::ReadyForPostProcess);
        terminal.sort();
        assert_eq!(terminal, vec![0, 1, 2]);
    }


    #[tokio::test]
    async fn sacd_cue_archive_and_single_workloads_feed_shared_encode_graph() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<TestSchedulerOutput>::new(Some(4), cancel.clone());
        let mut run = pool.start();
        let mut tracker = AlbumCompletionTracker::default();
        for job in ["sacd", "cue", "archive"] {
            tracker.register_album(job.to_string(), 2, false);
        }

        for (job, kind) in [
            ("sacd", WorkKind::SacdExtractTrack { track_id: TrackId { source_ordinal: 0, disc_number: None, track_number: 1 } }),
            ("cue", WorkKind::CueSplitTrack { track_id: TrackId { source_ordinal: 0, disc_number: None, track_number: 1 } }),
            ("archive", WorkKind::ArchiveExtract),
            ("single", WorkKind::SingleFile),
        ] {
            pool.submit(WorkUnit {
                job_id: job.to_string(),
                unit_id: format!("materialize-{job}"),
                kind,
                task: boxed_work(move |_cancel| async move {
                    if job == "single" {
                        Ok(TestSchedulerOutput::PostProcessed { job_id: job.to_string() })
                    } else {
                        Ok(TestSchedulerOutput::Materialized { job_id: job.to_string(), tracks: 2 })
                    }
                }),
            }).await;
        }

        let mut postprocessed = Vec::new();
        let mut encoded_by_job: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        while postprocessed.len() < 4 {
            let result = run.results.recv().await.expect("worker result");
            match result.outcome.expect("work output") {
                TestSchedulerOutput::Materialized { job_id, tracks } => {
                    for track_index in 0..tracks {
                        let encode_job = job_id.clone();
                        pool.submit(WorkUnit {
                            job_id: job_id.clone(),
                            unit_id: format!("{job_id}-encode-{track_index}"),
                            kind: WorkKind::EncodeTrack {
                                track_id: TrackId {
                                    source_ordinal: track_index as u32,
                                    disc_number: None,
                                    track_number: track_index as u32 + 1,
                                },
                            },
                            task: boxed_work(move |_cancel| async move {
                                Ok(TestSchedulerOutput::Encoded { job_id: encode_job, track_index })
                            }),
                        }).await;
                    }
                }
                TestSchedulerOutput::Encoded { job_id, track_index } => {
                    encoded_by_job.entry(job_id.clone()).or_default().push(track_index);
                    if tracker.mark_track_finished(&job_id, true) == AlbumReadiness::ReadyForPostProcess {
                        pool.submit(WorkUnit {
                            job_id: job_id.clone(),
                            unit_id: format!("{job_id}-postprocess"),
                            kind: WorkKind::AlbumPostProcess,
                            task: boxed_work(move |_cancel| async move {
                                Ok(TestSchedulerOutput::PostProcessed { job_id })
                            }),
                        }).await;
                    }
                }
                TestSchedulerOutput::PostProcessed { job_id } => postprocessed.push(job_id),
            }
        }

        run.shutdown().await;
        postprocessed.sort();
        assert_eq!(postprocessed, vec!["archive".to_string(), "cue".to_string(), "sacd".to_string(), "single".to_string()]);
        for job in ["archive", "cue", "sacd"] {
            let mut tracks = encoded_by_job.remove(job).expect("encoded job");
            tracks.sort();
            assert_eq!(tracks, vec![0, 1], "{job} should encode both materialized tracks");
        }
    }

    #[tokio::test]
    async fn worker_pool_stress_processes_many_single_files_deterministically() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<usize>::new(Some(8), cancel.clone());
        let mut run = pool.start();
        for index in 0..100usize {
            pool.submit(WorkUnit {
                job_id: format!("single-{index:03}"),
                unit_id: format!("encode-{index:03}"),
                kind: WorkKind::SingleFile,
                task: boxed_work(move |_cancel| async move { Ok(index) }),
            }).await;
        }

        let mut completed = Vec::new();
        while completed.len() < 100 {
            let result = run.results.recv().await.expect("worker result");
            completed.push(result.outcome.unwrap());
        }
        run.shutdown().await;
        completed.sort();
        assert_eq!(completed, (0usize..100).collect::<Vec<_>>());
    }

    /// 4.2: 1 archive with 20 tracks — extraction gates track fanout correctly.
    ///
    /// One materialization fans out 20 encode units into a 4-worker pool.
    /// Postprocess fires only after all 20 tracks reach terminal state.
    #[tokio::test]
    async fn archive_twenty_track_fanout_gates_postprocess() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<TestSchedulerOutput>::new(Some(4), cancel.clone());
        let mut run = pool.start();
        let mut tracker = AlbumCompletionTracker::default();
        let track_count = 20usize;
        tracker.register_album("archive".to_string(), track_count, false);

        pool.submit(WorkUnit {
            job_id: "archive".to_string(),
            unit_id: "archive-extract".to_string(),
            kind: WorkKind::ArchiveExtract,
            task: boxed_work(move |_cancel| async move {
                Ok(TestSchedulerOutput::Materialized {
                    job_id: "archive".to_string(),
                    tracks: track_count,
                })
            }),
        })
        .await;

        let mut encoded = Vec::new();
        #[allow(unused_assignments)]
        let mut postprocessed = false;
        loop {
            let result = run.results.recv().await.expect("worker result");
            match result.outcome.expect("work output") {
                TestSchedulerOutput::Materialized { job_id, tracks } => {
                    for track_index in 0..tracks {
                        let encode_job = job_id.clone();
                        pool.submit(WorkUnit {
                            job_id: job_id.clone(),
                            unit_id: format!("encode-{track_index}"),
                            kind: WorkKind::EncodeTrack {
                                track_id: TrackId {
                                    source_ordinal: track_index as u32,
                                    disc_number: None,
                                    track_number: track_index as u32 + 1,
                                },
                            },
                            task: boxed_work(move |_cancel| async move {
                                Ok(TestSchedulerOutput::Encoded {
                                    job_id: encode_job,
                                    track_index,
                                })
                            }),
                        })
                        .await;
                    }
                }
                TestSchedulerOutput::Encoded { job_id, track_index } => {
                    encoded.push(track_index);
                    if tracker.mark_track_finished(&job_id, true)
                        == AlbumReadiness::ReadyForPostProcess
                    {
                        assert_eq!(
                            encoded.len(),
                            track_count,
                            "postprocess must wait for all {track_count} tracks"
                        );
                        pool.submit(WorkUnit {
                            job_id: job_id.clone(),
                            unit_id: "postprocess".to_string(),
                            kind: WorkKind::AlbumPostProcess,
                            task: boxed_work(move |_cancel| async move {
                                Ok(TestSchedulerOutput::PostProcessed { job_id })
                            }),
                        })
                        .await;
                    }
                }
                TestSchedulerOutput::PostProcessed { .. } => {
                    postprocessed = true;
                    break;
                }
            }
        }

        run.shutdown().await;
        encoded.sort();
        assert_eq!(encoded, (0..track_count).collect::<Vec<_>>());
        assert!(postprocessed, "postprocess must run after all tracks complete");
    }

}

#[cfg(test)]
mod chunk_2_1_3_worker_recovery_tests {
    use super::*;
    use crate::convert::pipeline::tool::blocking_test_runner::{
        tool_gate, BlockingToolRunner, ToolBehavior,
    };
    use crate::convert::pipeline::tool::{ToolBinary, ToolCommand, ToolRunner};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::time::Duration;

    fn track_id(index: usize) -> TrackId {
        TrackId {
            source_ordinal: index as u32,
            disc_number: None,
            track_number: index as u32 + 1,
        }
    }

    fn cmd(index: usize) -> ToolCommand {
        ToolCommand {
            environment_policy: tonepoet_pipeline::CommandEnvironmentPolicy::InheritAndSet,
            binary: ToolBinary::Ssrc,
            args: vec![format!("track-{index}")],
            secret_args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            timeout: Duration::from_secs(60),
        }
    }

    #[tokio::test]
    async fn fifteen_worker_pool_recovers_after_one_tool_failure() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<usize>::new(Some(15), cancel.clone());
        let mut run = pool.start();
        let mut behaviors = Vec::new();
        behaviors.push(ToolBehavior::FailWithStderr("track failed".to_string()));
        for _ in 1..15 {
            behaviors.push(ToolBehavior::Succeed);
        }
        let runner = Arc::new(BlockingToolRunner::with_behaviors(behaviors));

        for index in 0..15usize {
            let runner = runner.clone();
            pool.submit(WorkUnit {
                job_id: "album-a".to_string(),
                unit_id: format!("initial-{index}"),
                kind: WorkKind::EncodeTrack { track_id: track_id(index) },
                task: boxed_work(move |cancel| async move {
                    runner.run(cmd(index), &cancel).await.map_err(|err| err.to_string())?;
                    Ok(index)
                }),
            })
            .await;
        }

        let mut successes = BTreeSet::new();
        let mut failures = 0usize;
        while successes.len() + failures < 15 {
            let result = run.results.recv().await.expect("initial result");
            match result.outcome {
                Ok(index) => {
                    successes.insert(index);
                }
                Err(err) => {
                    assert!(err.contains("tool exited non-zero"), "got: {err}");
                    failures += 1;
                }
            }
        }
        assert_eq!(failures, 1);
        assert_eq!(successes.len(), 14);
        assert_eq!(runner.transcript().len(), 15);

        for index in 100..115usize {
            pool.submit(WorkUnit {
                job_id: "album-b".to_string(),
                unit_id: format!("followup-{index}"),
                kind: WorkKind::SingleFile,
                task: boxed_work(move |_cancel| async move { Ok(index) }),
            })
            .await;
        }

        let mut followup = BTreeSet::new();
        while followup.len() < 15 {
            let result = run.results.recv().await.expect("followup result");
            followup.insert(result.outcome.expect("followup succeeds"));
        }

        run.shutdown().await;
        assert_eq!(followup, (100usize..115).collect::<BTreeSet<_>>());
    }

    #[tokio::test]
    async fn fail_fast_album_attempts_remaining_tracks_but_schedules_no_postprocess() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<usize>::new(Some(3), cancel.clone());
        let mut run = pool.start();
        let mut tracker = AlbumCompletionTracker::default();
        tracker.register_album("album".to_string(), 5, false);

        for index in 0..5usize {
            pool.submit(WorkUnit {
                job_id: "album".to_string(),
                unit_id: format!("track-{index}"),
                kind: WorkKind::EncodeTrack { track_id: track_id(index) },
                task: boxed_work(move |_cancel| async move {
                    if index == 1 {
                        Err("track 2 failed".to_string())
                    } else {
                        Ok(index)
                    }
                }),
            })
            .await;
        }

        let mut attempted = BTreeSet::new();
        let mut postprocess_submitted = false;
        while attempted.len() < 5 {
            let result = run.results.recv().await.expect("track result");
            assert_eq!(result.job_id, "album");
            let index = result
                .unit_id
                .strip_prefix("track-")
                .unwrap()
                .parse::<usize>()
                .unwrap();
            attempted.insert(index);
            let ok = result.outcome.is_ok();
            let readiness = tracker.mark_track_finished("album", ok);
            if readiness == AlbumReadiness::ReadyForPostProcess {
                postprocess_submitted = true;
            }
        }

        run.shutdown().await;
        assert_eq!(attempted, (0usize..5).collect::<BTreeSet<_>>());
        assert!(!postprocess_submitted, "fail-fast album must not enter postprocess");
    }

    #[tokio::test]
    async fn allow_partial_five_track_album_schedules_postprocess_after_all_terminals() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<&'static str>::new(Some(3), cancel.clone());
        let mut run = pool.start();
        let mut tracker = AlbumCompletionTracker::default();
        tracker.register_album("album".to_string(), 5, true);

        for index in 0..5usize {
            pool.submit(WorkUnit {
                job_id: "album".to_string(),
                unit_id: format!("track-{index}"),
                kind: WorkKind::EncodeTrack { track_id: track_id(index) },
                task: boxed_work(move |_cancel| async move {
                    if index == 1 {
                        Err("track 2 failed".to_string())
                    } else {
                        Ok("track-ok")
                    }
                }),
            })
            .await;
        }

        let mut terminal = 0usize;
        let mut postprocess_submitted = false;
        while terminal < 5 {
            let result = run.results.recv().await.expect("track result");
            let ok = result.outcome.is_ok();
            terminal += 1;
            if tracker.mark_track_finished("album", ok) == AlbumReadiness::ReadyForPostProcess {
                postprocess_submitted = true;
                pool.submit(WorkUnit {
                    job_id: "album".to_string(),
                    unit_id: "album-postprocess".to_string(),
                    kind: WorkKind::AlbumPostProcess,
                    task: boxed_work(move |_cancel| async move { Ok("postprocessed") }),
                })
                .await;
            }
        }

        assert!(postprocess_submitted);
        let result = run.results.recv().await.expect("postprocess result");
        assert_eq!(result.unit_id, "album-postprocess");
        assert_eq!(result.outcome.expect("postprocess succeeds"), "postprocessed");
        run.shutdown().await;
    }

    #[tokio::test]
    async fn mixed_queue_album_cancellation_does_not_stop_survivor_album_already_in_queue() {
        let pool_cancel = CancellationToken::new();
        let cancelled_album = CancellationToken::new();
        let pool = SharedWorkerPool::<(&'static str, usize)>::new(Some(4), pool_cancel.clone());
        let mut run = pool.start();
        let (first_gate, first_blocker) = tool_gate();
        let (second_gate, second_blocker) = tool_gate();
        let runner = Arc::new(BlockingToolRunner::with_behaviors([
            ToolBehavior::BlockThenSucceed(first_blocker),
            ToolBehavior::BlockThenSucceed(second_blocker),
        ]));

        for index in 0..2usize {
            let task_cancel = cancelled_album.clone();
            let runner = runner.clone();
            pool.submit(WorkUnit {
                job_id: "cancelled-album".to_string(),
                unit_id: format!("a-{index}"),
                kind: WorkKind::EncodeTrack { track_id: track_id(index) },
                task: boxed_work(move |_pool_cancel| async move {
                    runner.run(cmd(index), &task_cancel).await.map_err(|err| err.to_string())?;
                    Ok(("cancelled-album", index))
                }),
            })
            .await;
        }

        for index in 0..4usize {
            pool.submit(WorkUnit {
                job_id: "survivor-album".to_string(),
                unit_id: format!("b-{index}"),
                kind: WorkKind::EncodeTrack { track_id: track_id(index + 10) },
                task: boxed_work(move |_cancel| async move { Ok(("survivor-album", index)) }),
            })
            .await;
        }

        let first_release = first_gate.wait_started().await;
        let second_release = second_gate.wait_started().await;
        cancelled_album.cancel();
        drop(first_release);
        drop(second_release);

        let mut survivor = BTreeSet::new();
        let mut cancelled = 0usize;
        while survivor.len() < 4 || cancelled < 2 {
            let result = run.results.recv().await.expect("mixed queue result");
            match result.job_id.as_str() {
                "survivor-album" => {
                    survivor.insert(result.outcome.expect("survivor succeeds").1);
                }
                "cancelled-album" => {
                    assert!(result.outcome.expect_err("cancelled task fails").contains("cancelled"));
                    cancelled += 1;
                }
                other => panic!("unexpected job {other}"),
            }
        }

        run.shutdown().await;
        assert_eq!(survivor, (0usize..4).collect::<BTreeSet<_>>());
        assert_eq!(cancelled, 2);
    }

    #[test]
    fn album_completion_tracker_counts_failure_policy_boundaries() {
        let mut fail_fast = AlbumCompletionTracker::default();
        fail_fast.register_album("album".to_string(), 5, false);
        assert!(matches!(
            fail_fast.mark_track_finished("album", true),
            AlbumReadiness::Waiting { finished: 1, expected: 5 }
        ));
        assert!(matches!(
            fail_fast.mark_track_finished("album", false),
            AlbumReadiness::Failed { finished: 2, expected: 5, failed: 1 }
        ));
        assert!(matches!(
            fail_fast.mark_track_finished("album", true),
            AlbumReadiness::Failed { finished: 3, expected: 5, failed: 1 }
        ));

        let mut partial = AlbumCompletionTracker::default();
        partial.register_album("album".to_string(), 5, true);
        for ok in [true, false, true, true] {
            assert!(matches!(
                partial.mark_track_finished("album", ok),
                AlbumReadiness::Waiting { .. }
            ));
        }
        assert_eq!(
            partial.mark_track_finished("album", true),
            AlbumReadiness::ReadyForPostProcess
        );
    }

    #[tokio::test]
    async fn multi_album_queue_records_each_album_without_partition_deadlock() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<(&'static str, usize)>::new(Some(4), cancel.clone());
        let mut run = pool.start();
        for (job, count) in [("a", 5usize), ("b", 5usize), ("c", 5usize)] {
            for index in 0..count {
                pool.submit(WorkUnit {
                    job_id: job.to_string(),
                    unit_id: format!("{job}-{index}"),
                    kind: WorkKind::EncodeTrack { track_id: track_id(index) },
                    task: boxed_work(move |_cancel| async move { Ok((job, index)) }),
                })
                .await;
            }
        }

        let mut seen: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        while seen.values().map(Vec::len).sum::<usize>() < 15 {
            let result = run.results.recv().await.expect("result");
            let (job, index) = result.outcome.expect("task succeeds");
            seen.entry(job.to_string()).or_default().push(index);
        }
        run.shutdown().await;
        for job in ["a", "b", "c"] {
            let mut values = seen.remove(job).expect("job results");
            values.sort();
            assert_eq!(values, vec![0, 1, 2, 3, 4]);
        }
    }

    #[tokio::test]
    async fn worker_panic_does_not_deadlock_remaining_workers() {
        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<usize>::new(Some(3), cancel.clone());
        let mut run = pool.start();

        pool.submit(WorkUnit {
            job_id: "panic-album".to_string(),
            unit_id: "panic".to_string(),
            kind: WorkKind::EncodeTrack { track_id: track_id(999) },
            task: boxed_work(move |_cancel| async move {
                panic!("intentional worker panic for recovery test");
                #[allow(unreachable_code)]
                Ok::<usize, String>(0)
            }),
        })
        .await;

        for index in 0..6usize {
            pool.submit(WorkUnit {
                job_id: "survivor".to_string(),
                unit_id: format!("survivor-{index}"),
                kind: WorkKind::EncodeTrack { track_id: track_id(index) },
                task: boxed_work(move |_cancel| async move { Ok(index) }),
            })
            .await;
        }

        let mut completed = Vec::new();
        while completed.len() < 6 {
            let result = run.results.recv().await.expect("survivor result");
            if result.job_id == "survivor" {
                completed.push(result.outcome.expect("survivor succeeds"));
            }
        }
        run.shutdown().await;
        completed.sort();
        assert_eq!(completed, vec![0, 1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn fail_fast_album_drains_terminal_track_results_without_postprocess_submission() {
        #[derive(Debug, Clone, PartialEq, Eq)]
        enum Output {
            Track { index: usize, ok: bool },
            PostProcess,
        }

        let cancel = CancellationToken::new();
        let pool = SharedWorkerPool::<Output>::new(Some(3), cancel.clone());
        let mut run = pool.start();
        let mut tracker = AlbumCompletionTracker::default();
        tracker.register_album("album".to_string(), 5, false);

        for index in 0..5usize {
            pool.submit(WorkUnit {
                job_id: "album".to_string(),
                unit_id: format!("track-{index}"),
                kind: WorkKind::EncodeTrack { track_id: track_id(index) },
                task: boxed_work(move |_cancel| async move {
                    Ok(Output::Track { index, ok: index != 1 })
                }),
            })
            .await;
        }

        let mut finished_tracks = Vec::new();
        let mut failed_seen = false;
        let mut postprocess_submitted = false;
        while finished_tracks.len() < 5 {
            let result = run.results.recv().await.expect("track result");
            match result.outcome.expect("work completes") {
                Output::Track { index, ok } => {
                    finished_tracks.push(index);
                    match tracker.mark_track_finished(&result.job_id, ok) {
                        AlbumReadiness::Failed { .. } => failed_seen = true,
                        AlbumReadiness::ReadyForPostProcess => {
                            postprocess_submitted = true;
                            pool.submit(WorkUnit {
                                job_id: "album".to_string(),
                                unit_id: "postprocess".to_string(),
                                kind: WorkKind::AlbumPostProcess,
                                task: boxed_work(move |_cancel| async move { Ok(Output::PostProcess) }),
                            })
                            .await;
                        }
                        AlbumReadiness::Waiting { .. } => {}
                    }
                }
                Output::PostProcess => postprocess_submitted = true,
            }
        }

        run.shutdown().await;
        finished_tracks.sort();
        assert_eq!(finished_tracks, vec![0, 1, 2, 3, 4]);
        assert!(failed_seen, "fail-fast readiness was observed");
        assert!(!postprocess_submitted, "fail-fast album never reaches postprocess readiness");
    }

    /// 4.2: 5 singles + 2 multi-track sources + 1 archive — singles start
    /// immediately; materialized tracks join the same worker pool.
    ///
    /// Materializations are gated so they block until released. All 5 singles
    /// must complete while the materializations are still held.
    #[tokio::test]
    async fn mixed_workload_singles_complete_before_materialization_finishes() {
        let pool_cancel = CancellationToken::new();
        // 6 workers: enough to run all 5 singles + 1 materialization concurrently
        let pool = SharedWorkerPool::<(&'static str, usize)>::new(Some(6), pool_cancel.clone());
        let mut run = pool.start();

        // Gates hold the 3 multi-track materializations
        let (gate_sacd1, blocker_sacd1) = tool_gate();
        let (gate_sacd2, blocker_sacd2) = tool_gate();
        let (gate_archive, blocker_archive) = tool_gate();

        let runner = Arc::new(BlockingToolRunner::with_behaviors([
            ToolBehavior::BlockThenSucceed(blocker_sacd1),
            ToolBehavior::BlockThenSucceed(blocker_sacd2),
            ToolBehavior::BlockThenSucceed(blocker_archive),
        ]));

        // Submit 3 gated materializations
        for (index, job) in ["sacd-1", "sacd-2", "archive"].iter().enumerate() {
            let runner = runner.clone();
            pool.submit(WorkUnit {
                job_id: job.to_string(),
                unit_id: format!("materialize-{job}"),
                kind: WorkKind::MaterializeItem,
                task: boxed_work(move |cancel| async move {
                    runner
                        .run(cmd(index), &cancel)
                        .await
                        .map_err(|err| err.to_string())?;
                    Ok((*job, 0))
                }),
            })
            .await;
        }

        // Submit 5 singles (no gating — should complete immediately)
        for index in 0..5usize {
            pool.submit(WorkUnit {
                job_id: format!("single-{index}"),
                unit_id: format!("single-{index}"),
                kind: WorkKind::SingleFile,
                task: boxed_work(move |_cancel| async move { Ok(("single", index)) }),
            })
            .await;
        }

        // Wait for all 3 materializations to reach their gates
        let release_sacd1 = gate_sacd1.wait_started().await;
        let release_sacd2 = gate_sacd2.wait_started().await;
        let release_archive = gate_archive.wait_started().await;

        // Drain the 5 singles — they must complete while materializations are blocked
        let mut singles_done = BTreeSet::new();
        while singles_done.len() < 5 {
            let result = run.results.recv().await.expect("single result");
            let (tag, index) = result.outcome.expect("single succeeds");
            assert_eq!(tag, "single", "only singles should complete while gates are held");
            singles_done.insert(index);
        }
        assert_eq!(singles_done, (0usize..5).collect::<BTreeSet<_>>());

        // Release materializations — they should now complete
        release_sacd1.release();
        release_sacd2.release();
        release_archive.release();

        let mut materialized_jobs = BTreeSet::new();
        while materialized_jobs.len() < 3 {
            let result = run.results.recv().await.expect("materialize result");
            materialized_jobs.insert(result.job_id.clone());
        }
        assert_eq!(
            materialized_jobs,
            ["archive", "sacd-1", "sacd-2"]
                .iter()
                .map(|s| s.to_string())
                .collect::<BTreeSet<_>>()
        );

        run.shutdown().await;
    }

}

/// Test-only state machine model documenting the legal album lifecycle.
///
/// The scheduler primitives ([`SharedWorkerPool`] + [`AlbumCompletionTracker`])
/// don't enforce this state machine directly — the orchestrator in `processor.rs`
/// drives transitions. This model formalises the legal transitions so that
/// regressions in the orchestrator surface as explicit illegal-transition failures.
#[cfg(test)]
mod lifecycle_model_tests {
    use std::fmt;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Phase {
        Queued,
        Materializing,
        TracksReady,
        Encoding,
        AllTerminal,
        PostProcessing,
        Published,
        Failed,
        Cancelled,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Event {
        StartMaterialization,
        MaterializationDone,
        StartEncoding,
        TrackDone,
        AllTracksDone,
        TrackFailed,
        AllTracksTerminal,
        StartPostProcess,
        PostProcessDone,
        Cancel,
        /// fail-fast policy: first failure blocks the album
        FailFastTriggered,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct IllegalTransition {
        from: Phase,
        event: Event,
    }

    impl fmt::Display for IllegalTransition {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "illegal transition: {:?} on {:?}", self.event, self.from)
        }
    }

    /// Transition function encoding the legal album lifecycle.
    fn transition(phase: Phase, event: Event) -> Result<Phase, IllegalTransition> {
        use Event::*;
        use Phase::*;

        let next = match (phase, event) {
            // — happy path —
            (Queued, StartMaterialization) => Materializing,
            (Materializing, MaterializationDone) => TracksReady,
            (TracksReady, StartEncoding) => Encoding,
            (Encoding, TrackDone) => Encoding,          // more tracks pending
            (Encoding, AllTracksDone) => AllTerminal,    // all tracks ok
            (AllTerminal, StartPostProcess) => PostProcessing,
            (PostProcessing, PostProcessDone) => Published,

            // — failure paths —
            (Encoding, TrackFailed) => Encoding,         // allow-partial: keep going
            (Encoding, FailFastTriggered) => Failed,     // fail-fast: immediate block
            (Encoding, AllTracksTerminal) => AllTerminal, // allow-partial: some failed, all terminal
            (Materializing, TrackFailed) => Failed,      // materialization itself failed
            (AllTerminal, FailFastTriggered) => Failed,  // deferred failure accounting

            // — cancellation —
            (Queued, Cancel) => Cancelled,
            (Materializing, Cancel) => Cancelled,
            (TracksReady, Cancel) => Cancelled,
            (Encoding, Cancel) => Cancelled,

            // everything else is illegal
            _ => return Err(IllegalTransition { from: phase, event }),
        };
        Ok(next)
    }

    // ── illegal transition tests (guidance doc 4.1) ──

    #[test]
    fn illegal_encoding_to_published_without_postprocess() {
        // EncodingTrack -> Published without post-processing gate
        let result = transition(Phase::Encoding, Event::PostProcessDone);
        assert!(result.is_err(), "encoding must not skip to published");
    }

    #[test]
    fn illegal_failed_track_to_postprocess_under_fail_fast() {
        // TrackFailed -> AlbumReadyForPost under fail-fast policy
        let result = transition(Phase::Failed, Event::StartPostProcess);
        assert!(result.is_err(), "failed album must not enter postprocess");
    }

    #[test]
    fn illegal_cancelled_to_postprocessing() {
        let result = transition(Phase::Cancelled, Event::StartPostProcess);
        assert!(result.is_err(), "cancelled album must not enter postprocess");
    }

    #[test]
    fn illegal_queued_to_track_done() {
        let result = transition(Phase::Queued, Event::TrackDone);
        assert!(result.is_err(), "queued album must not jump to track done");
    }

    // ── additional illegal transition tests ──

    const ALL_EVENTS: [Event; 11] = [
        Event::StartMaterialization,
        Event::MaterializationDone,
        Event::StartEncoding,
        Event::TrackDone,
        Event::AllTracksDone,
        Event::TrackFailed,
        Event::AllTracksTerminal,
        Event::StartPostProcess,
        Event::PostProcessDone,
        Event::Cancel,
        Event::FailFastTriggered,
    ];

    #[test]
    fn illegal_published_accepts_no_events() {
        for event in ALL_EVENTS {
            assert!(
                transition(Phase::Published, event).is_err(),
                "published is terminal — {event:?} must be rejected"
            );
        }
    }

    #[test]
    fn illegal_failed_accepts_no_events() {
        for event in ALL_EVENTS {
            assert!(
                transition(Phase::Failed, event).is_err(),
                "failed is terminal — {event:?} must be rejected"
            );
        }
    }

    #[test]
    fn illegal_cancelled_accepts_no_events() {
        for event in ALL_EVENTS {
            assert!(
                transition(Phase::Cancelled, event).is_err(),
                "cancelled is terminal — {event:?} must be rejected"
            );
        }
    }

    #[test]
    fn illegal_queued_to_all_terminal() {
        assert!(transition(Phase::Queued, Event::AllTracksDone).is_err());
        assert!(transition(Phase::Queued, Event::AllTracksTerminal).is_err());
    }

    #[test]
    fn illegal_materializing_to_published() {
        assert!(transition(Phase::Materializing, Event::PostProcessDone).is_err());
    }

    // ── legal path tests ──

    #[test]
    fn legal_happy_path() {
        let mut phase = Phase::Queued;
        phase = transition(phase, Event::StartMaterialization).unwrap();
        assert_eq!(phase, Phase::Materializing);
        phase = transition(phase, Event::MaterializationDone).unwrap();
        assert_eq!(phase, Phase::TracksReady);
        phase = transition(phase, Event::StartEncoding).unwrap();
        assert_eq!(phase, Phase::Encoding);
        phase = transition(phase, Event::TrackDone).unwrap();
        assert_eq!(phase, Phase::Encoding);
        phase = transition(phase, Event::AllTracksDone).unwrap();
        assert_eq!(phase, Phase::AllTerminal);
        phase = transition(phase, Event::StartPostProcess).unwrap();
        assert_eq!(phase, Phase::PostProcessing);
        phase = transition(phase, Event::PostProcessDone).unwrap();
        assert_eq!(phase, Phase::Published);
    }

    #[test]
    fn legal_fail_fast_path() {
        let mut phase = Phase::Queued;
        phase = transition(phase, Event::StartMaterialization).unwrap();
        phase = transition(phase, Event::MaterializationDone).unwrap();
        phase = transition(phase, Event::StartEncoding).unwrap();
        phase = transition(phase, Event::TrackDone).unwrap();
        phase = transition(phase, Event::FailFastTriggered).unwrap();
        assert_eq!(phase, Phase::Failed);
    }

    #[test]
    fn legal_allow_partial_path() {
        let mut phase = Phase::Queued;
        phase = transition(phase, Event::StartMaterialization).unwrap();
        phase = transition(phase, Event::MaterializationDone).unwrap();
        phase = transition(phase, Event::StartEncoding).unwrap();
        // some tracks fail, but we keep encoding
        phase = transition(phase, Event::TrackFailed).unwrap();
        assert_eq!(phase, Phase::Encoding);
        phase = transition(phase, Event::TrackDone).unwrap();
        assert_eq!(phase, Phase::Encoding);
        // all tracks terminal (some ok, some failed)
        phase = transition(phase, Event::AllTracksTerminal).unwrap();
        assert_eq!(phase, Phase::AllTerminal);
        phase = transition(phase, Event::StartPostProcess).unwrap();
        phase = transition(phase, Event::PostProcessDone).unwrap();
        assert_eq!(phase, Phase::Published);
    }

    #[test]
    fn legal_cancellation_from_encoding() {
        let mut phase = Phase::Queued;
        phase = transition(phase, Event::StartMaterialization).unwrap();
        phase = transition(phase, Event::MaterializationDone).unwrap();
        phase = transition(phase, Event::StartEncoding).unwrap();
        phase = transition(phase, Event::Cancel).unwrap();
        assert_eq!(phase, Phase::Cancelled);
    }

    #[test]
    fn legal_cancellation_from_queued() {
        let phase = transition(Phase::Queued, Event::Cancel).unwrap();
        assert_eq!(phase, Phase::Cancelled);
    }

    #[test]
    fn legal_materialization_failure() {
        let mut phase = Phase::Queued;
        phase = transition(phase, Event::StartMaterialization).unwrap();
        phase = transition(phase, Event::TrackFailed).unwrap();
        assert_eq!(phase, Phase::Failed);
    }
}
