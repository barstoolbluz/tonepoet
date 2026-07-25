//! Dedicated, bounded filesystem workers for bookmark reachability, activation,
//! and detail inspection.
//!
//! Filesystem metadata and directory reads can block indefinitely on unhealthy
//! network/FUSE/automount paths. These workers deliberately do not use Tokio's
//! general blocking pool, so pathological bookmark targets cannot starve
//! unrelated application work. Queues are bounded and jobs carry a lifecycle
//! generation; superseded queued jobs are discarded before touching the
//! filesystem.

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc, Condvar, Mutex, OnceLock,
};
use std::thread;

use tokio::sync::mpsc as tokio_mpsc;

use super::bookmarks::BookmarkTargetStatus;
use super::message::AppMessage;

const STATUS_WORKERS: usize = 4;
const STATUS_QUEUE_CAPACITY: usize = 128;
const ACTIVATION_WORKERS: usize = 2;
const ACTIVATION_QUEUE_CAPACITY: usize = 16;
const DETAIL_WORKERS: usize = 2;
const DETAIL_QUEUE_CAPACITY: usize = 16;

#[derive(Debug)]
struct StatusJob {
    generation: u64,
    path: PathBuf,
    generation_guard: Arc<AtomicU64>,
    tx: tokio_mpsc::Sender<AppMessage>,
}

#[derive(Debug)]
struct ActivationJob {
    generation: u64,
    request_id: u64,
    path: PathBuf,
    generation_guard: Arc<AtomicU64>,
    tx: tokio_mpsc::Sender<AppMessage>,
}

#[derive(Debug)]
struct DetailJob {
    generation: u64,
    path: PathBuf,
    generation_guard: Arc<AtomicU64>,
    tx: tokio_mpsc::Sender<AppMessage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookmarkEnqueueFailure {
    StaleGeneration,
    QueueFull,
    WorkersUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkEnqueueError {
    pub failure: BookmarkEnqueueFailure,
    pub path: PathBuf,
}

trait GenerationJob: Send + 'static {
    fn is_current(&self) -> bool;
    fn into_path(self) -> PathBuf;
}

impl GenerationJob for StatusJob {
    fn is_current(&self) -> bool {
        generation_is_current(self.generation, &self.generation_guard)
    }

    fn into_path(self) -> PathBuf {
        self.path
    }
}

impl GenerationJob for ActivationJob {
    fn is_current(&self) -> bool {
        generation_is_current(self.generation, &self.generation_guard)
    }

    fn into_path(self) -> PathBuf {
        self.path
    }
}

impl GenerationJob for DetailJob {
    fn is_current(&self) -> bool {
        generation_is_current(self.generation, &self.generation_guard)
    }

    fn into_path(self) -> PathBuf {
        self.path
    }
}

#[derive(Debug)]
enum QueuePushError<T> {
    Stale(T),
    Full(T),
    WorkersUnavailable(T),
}

struct BoundedJobQueue<T> {
    capacity: usize,
    jobs: Mutex<VecDeque<T>>,
    ready: Condvar,
    workers: AtomicUsize,
}

impl<T: GenerationJob> BoundedJobQueue<T> {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            jobs: Mutex::new(VecDeque::with_capacity(capacity)),
            ready: Condvar::new(),
            workers: AtomicUsize::new(0),
        }
    }

    fn set_worker_count(&self, count: usize) {
        self.workers.store(count, Ordering::Release);
    }

    fn worker_count(&self) -> usize {
        self.workers.load(Ordering::Acquire)
    }

    fn purge_stale(&self) {
        let mut jobs = match self.jobs.lock() {
            Ok(jobs) => jobs,
            Err(poisoned) => poisoned.into_inner(),
        };
        jobs.retain(|job| job.is_current());
    }

    fn try_push(&self, job: T) -> Result<(), QueuePushError<T>> {
        if !job.is_current() {
            return Err(QueuePushError::Stale(job));
        }
        if self.worker_count() == 0 {
            return Err(QueuePushError::WorkersUnavailable(job));
        }
        let mut jobs = match self.jobs.lock() {
            Ok(jobs) => jobs,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Lifecycle invalidation is cancellation for work that has not begun:
        // remove it now instead of allowing stale jobs to occupy bounded slots.
        jobs.retain(|job| job.is_current());
        if !job.is_current() {
            return Err(QueuePushError::Stale(job));
        }
        if jobs.len() >= self.capacity {
            return Err(QueuePushError::Full(job));
        }
        jobs.push_back(job);
        self.ready.notify_one();
        Ok(())
    }

    fn pop_current(&self) -> T {
        loop {
            let mut jobs = match self.jobs.lock() {
                Ok(jobs) => jobs,
                Err(poisoned) => poisoned.into_inner(),
            };
            while jobs.is_empty() {
                jobs = match self.ready.wait(jobs) {
                    Ok(jobs) => jobs,
                    Err(poisoned) => poisoned.into_inner(),
                };
            }
            let job = jobs.pop_front().expect("queue was non-empty");
            drop(jobs);
            if job.is_current() {
                return job;
            }
        }
    }
}

struct BookmarkWorkerPool {
    status: Arc<BoundedJobQueue<StatusJob>>,
    activation: Arc<BoundedJobQueue<ActivationJob>>,
    detail: Arc<BoundedJobQueue<DetailJob>>,
    status_start: Mutex<()>,
    activation_start: Mutex<()>,
    detail_start: Mutex<()>,
}

impl BookmarkWorkerPool {
    fn start() -> Self {
        let status = Arc::new(BoundedJobQueue::new(STATUS_QUEUE_CAPACITY));
        let status_workers = spawn_workers(
            "bookmark-status",
            STATUS_WORKERS,
            Arc::clone(&status),
            run_status_job,
        );
        status.set_worker_count(status_workers);

        let activation = Arc::new(BoundedJobQueue::new(ACTIVATION_QUEUE_CAPACITY));
        let activation_workers = spawn_workers(
            "bookmark-activation",
            ACTIVATION_WORKERS,
            Arc::clone(&activation),
            run_activation_job,
        );
        activation.set_worker_count(activation_workers);

        let detail = Arc::new(BoundedJobQueue::new(DETAIL_QUEUE_CAPACITY));
        let detail_workers = spawn_workers(
            "bookmark-detail",
            DETAIL_WORKERS,
            Arc::clone(&detail),
            run_detail_job,
        );
        detail.set_worker_count(detail_workers);

        Self {
            status,
            activation,
            detail,
            status_start: Mutex::new(()),
            activation_start: Mutex::new(()),
            detail_start: Mutex::new(()),
        }
    }

    fn ensure_status_workers(&self) {
        ensure_workers(
            &self.status_start,
            "bookmark-status",
            STATUS_WORKERS,
            &self.status,
            run_status_job,
        );
    }

    fn ensure_activation_workers(&self) {
        ensure_workers(
            &self.activation_start,
            "bookmark-activation",
            ACTIVATION_WORKERS,
            &self.activation,
            run_activation_job,
        );
    }

    fn ensure_detail_workers(&self) {
        ensure_workers(
            &self.detail_start,
            "bookmark-detail",
            DETAIL_WORKERS,
            &self.detail,
            run_detail_job,
        );
    }

    fn purge_stale(&self) {
        self.status.purge_stale();
        self.activation.purge_stale();
        self.detail.purge_stale();
    }
}

static POOL: OnceLock<BookmarkWorkerPool> = OnceLock::new();

fn pool() -> &'static BookmarkWorkerPool {
    POOL.get_or_init(BookmarkWorkerPool::start)
}

/// Immediately remove superseded queued jobs without initializing the worker
/// pool merely to perform cancellation. Active filesystem syscalls cannot be
/// force-cancelled portably, but they remain bounded by the dedicated workers.
pub fn cancel_superseded_jobs() {
    if let Some(pool) = POOL.get() {
        pool.purge_stale();
    }
}

fn ensure_workers<T: GenerationJob>(
    start_lock: &Mutex<()>,
    name_prefix: &str,
    desired_count: usize,
    queue: &Arc<BoundedJobQueue<T>>,
    run: fn(T),
) {
    if queue.worker_count() > 0 {
        return;
    }
    let _guard = match start_lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if queue.worker_count() > 0 {
        return;
    }
    let started = spawn_workers(name_prefix, desired_count, Arc::clone(queue), run);
    queue.set_worker_count(started);
}

fn spawn_workers<T: GenerationJob>(
    name_prefix: &str,
    count: usize,
    queue: Arc<BoundedJobQueue<T>>,
    run: fn(T),
) -> usize {
    let mut started = 0usize;
    for index in 0..count {
        let queue = Arc::clone(&queue);
        let name = format!("{name_prefix}-{index}");
        let thread_name = name.clone();
        let spawn_error_name = name.clone();
        match thread::Builder::new().name(name).spawn(move || loop {
            let job = queue.pop_current();
            if catch_unwind(AssertUnwindSafe(|| run(job))).is_err() {
                log::error!(
                    "bookmark worker {thread_name} panicked while processing a job; continuing"
                );
            }
        }) {
            Ok(_) => started = started.saturating_add(1),
            Err(error) => {
                // Do not crash the TUI under process/thread resource pressure.
                // A queue with no successfully started workers rejects work.
                log::error!("could not start {spawn_error_name}: {error}");
            }
        }
    }
    started
}

fn generation_is_current(generation: u64, guard: &AtomicU64) -> bool {
    guard.load(Ordering::Acquire) == generation
}

fn target_status(path: &std::path::Path) -> BookmarkTargetStatus {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => BookmarkTargetStatus::Reachable,
        Ok(_) => BookmarkTargetStatus::Missing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            BookmarkTargetStatus::Missing
        }
        Err(_) => BookmarkTargetStatus::Unavailable,
    }
}

fn activation_result(path: &std::path::Path) -> Result<(), String> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(format!(
            "bookmark target is not a directory: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "bookmark target no longer exists: {}",
            path.display()
        )),
        Err(error) => Err(format!(
            "bookmark target is unavailable ({}): {}",
            error,
            path.display()
        )),
    }
}

fn run_status_job(job: StatusJob) {
    if !generation_is_current(job.generation, &job.generation_guard) {
        return;
    }
    let status = target_status(&job.path);
    if !generation_is_current(job.generation, &job.generation_guard) {
        return;
    }
    let _ = job.tx.blocking_send(AppMessage::BookmarkTargetsLoaded {
        generation: job.generation,
        statuses: vec![(job.path, status)],
    });
}

fn run_activation_job(job: ActivationJob) {
    if !generation_is_current(job.generation, &job.generation_guard) {
        return;
    }
    let result = activation_result(&job.path);
    if !generation_is_current(job.generation, &job.generation_guard) {
        return;
    }
    let _ = job.tx.blocking_send(AppMessage::BookmarkActivationResolved {
        generation: job.generation,
        request_id: job.request_id,
        path: job.path,
        result,
    });
}

fn run_detail_job(job: DetailJob) {
    if !generation_is_current(job.generation, &job.generation_guard) {
        return;
    }
    if job
        .tx
        .blocking_send(AppMessage::BookmarkDetailStarted {
            generation: job.generation,
            path: job.path.clone(),
        })
        .is_err()
    {
        return;
    }
    if !generation_is_current(job.generation, &job.generation_guard) {
        return;
    }
    let result = super::bookmarks_overlay::load_bookmark_detail(&job.path);
    if !generation_is_current(job.generation, &job.generation_guard) {
        return;
    }
    let _ = job.tx.blocking_send(AppMessage::BookmarkDetailLoaded {
        generation: job.generation,
        path: job.path,
        result,
    });
}

fn enqueue_error<T: GenerationJob>(error: QueuePushError<T>) -> BookmarkEnqueueError {
    match error {
        QueuePushError::Stale(job) => BookmarkEnqueueError {
            failure: BookmarkEnqueueFailure::StaleGeneration,
            path: job.into_path(),
        },
        QueuePushError::Full(job) => BookmarkEnqueueError {
            failure: BookmarkEnqueueFailure::QueueFull,
            path: job.into_path(),
        },
        QueuePushError::WorkersUnavailable(job) => BookmarkEnqueueError {
            failure: BookmarkEnqueueFailure::WorkersUnavailable,
            path: job.into_path(),
        },
    }
}

pub fn try_queue_status(
    generation: u64,
    path: PathBuf,
    generation_guard: Arc<AtomicU64>,
    tx: tokio_mpsc::Sender<AppMessage>,
) -> Result<(), BookmarkEnqueueError> {
    let job = StatusJob {
        generation,
        path,
        generation_guard,
        tx,
    };
    let pool = pool();
    pool.ensure_status_workers();
    pool.status.try_push(job).map_err(enqueue_error)
}

pub fn try_queue_activation(
    generation: u64,
    request_id: u64,
    path: PathBuf,
    generation_guard: Arc<AtomicU64>,
    tx: tokio_mpsc::Sender<AppMessage>,
) -> Result<(), BookmarkEnqueueError> {
    let job = ActivationJob {
        generation,
        request_id,
        path,
        generation_guard,
        tx,
    };
    let pool = pool();
    pool.ensure_activation_workers();
    pool.activation.try_push(job).map_err(enqueue_error)
}

pub fn try_queue_detail(
    generation: u64,
    path: PathBuf,
    generation_guard: Arc<AtomicU64>,
    tx: tokio_mpsc::Sender<AppMessage>,
) -> Result<(), BookmarkEnqueueError> {
    let job = DetailJob {
        generation,
        path,
        generation_guard,
        tx,
    };
    let pool = pool();
    pool.ensure_detail_workers();
    pool.detail.try_push(job).map_err(enqueue_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_generation_is_detected_before_filesystem_work() {
        let guard = AtomicU64::new(8);
        assert!(generation_is_current(8, &guard));
        guard.store(9, Ordering::Release);
        assert!(!generation_is_current(8, &guard));
    }

    #[test]
    fn target_status_distinguishes_directory_and_non_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert_eq!(target_status(temp.path()), BookmarkTargetStatus::Reachable);
        let file = temp.path().join("file");
        std::fs::write(&file, b"x").expect("write");
        assert_eq!(target_status(&file), BookmarkTargetStatus::Missing);
    }

    #[test]
    fn bounded_queue_purges_superseded_jobs_before_capacity_check() {
        let guard = Arc::new(AtomicU64::new(1));
        let (tx, _rx) = tokio_mpsc::channel(1);
        let queue = BoundedJobQueue::new(1);
        queue.set_worker_count(1);
        assert!(queue
            .try_push(StatusJob {
                generation: 1,
                path: PathBuf::from("/stale"),
                generation_guard: Arc::clone(&guard),
                tx: tx.clone(),
            })
            .is_ok());

        guard.store(2, Ordering::Release);
        assert!(queue
            .try_push(StatusJob {
                generation: 2,
                path: PathBuf::from("/current"),
                generation_guard: guard,
                tx,
            })
            .is_ok());
        let jobs = queue.jobs.lock().expect("queue lock");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].path, PathBuf::from("/current"));
    }

    #[test]
    fn workerless_queue_reports_unavailable_not_full() {
        let guard = Arc::new(AtomicU64::new(1));
        let (tx, _rx) = tokio_mpsc::channel(1);
        let queue = BoundedJobQueue::new(1);
        let error = queue.try_push(StatusJob {
            generation: 1,
            path: PathBuf::from("/workerless"),
            generation_guard: guard,
            tx,
        });
        assert!(matches!(
            error,
            Err(QueuePushError::WorkersUnavailable(_))
        ));
    }

    #[test]
    fn public_enqueue_error_preserves_failure_class_and_path() {
        let guard = Arc::new(AtomicU64::new(1));
        let (tx, _rx) = tokio_mpsc::channel(1);
        let error = enqueue_error(QueuePushError::WorkersUnavailable(StatusJob {
            generation: 1,
            path: PathBuf::from("/workerless"),
            generation_guard: guard,
            tx,
        }));
        assert_eq!(error.failure, BookmarkEnqueueFailure::WorkersUnavailable);
        assert_eq!(error.path, PathBuf::from("/workerless"));
    }

    #[test]
    fn full_queue_is_distinct_from_worker_unavailability() {
        let guard = Arc::new(AtomicU64::new(1));
        let (tx, _rx) = tokio_mpsc::channel(1);
        let queue = BoundedJobQueue::new(1);
        queue.set_worker_count(1);
        assert!(queue
            .try_push(StatusJob {
                generation: 1,
                path: PathBuf::from("/first"),
                generation_guard: Arc::clone(&guard),
                tx: tx.clone(),
            })
            .is_ok());
        let error = queue.try_push(StatusJob {
            generation: 1,
            path: PathBuf::from("/second"),
            generation_guard: guard,
            tx,
        });
        assert!(matches!(error, Err(QueuePushError::Full(_))));
    }

}
