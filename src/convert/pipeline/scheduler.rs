//! Shared worker pool and album completion accounting for Chunk 2.
//!
//! The queue has no job partitioning. Idle workers pull the next ready unit.
//! A multi-step encode chain remains sequential inside one track work unit;
//! different tracks and different queue items can run concurrently.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex, Notify};
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

#[derive(Clone)]
pub struct SharedWorkerPool<R: Send + 'static> {
    queue: Arc<Mutex<VecDeque<WorkUnit<R>>>>,
    notify: Arc<Notify>,
    cancel: CancellationToken,
    worker_count: usize,
}

impl<R: Send + 'static> SharedWorkerPool<R> {
    pub fn new(worker_count: Option<usize>, cancel: CancellationToken) -> Self {
        let detected = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .saturating_sub(1)
            .max(1);
        Self {
            queue: Arc::new(Mutex::new(VecDeque::new())),
            notify: Arc::new(Notify::new()),
            cancel,
            worker_count: worker_count.unwrap_or(detected).max(1),
        }
    }

    pub fn worker_count(&self) -> usize {
        self.worker_count
    }

    pub async fn submit(&self, unit: WorkUnit<R>) {
        self.queue.lock().await.push_back(unit);
        self.notify.notify_one();
    }

    pub async fn submit_many<I>(&self, units: I)
    where
        I: IntoIterator<Item = WorkUnit<R>>,
    {
        let mut added = 0usize;
        let mut queue = self.queue.lock().await;
        for unit in units {
            queue.push_back(unit);
            added += 1;
        }
        drop(queue);
        for _ in 0..added {
            self.notify.notify_one();
        }
    }

    pub fn start(&self) -> WorkerPoolRun<R> {
        let (result_tx, result_rx) = mpsc::channel(self.worker_count * 4);
        let mut workers = JoinSet::new();
        for worker_index in 0..self.worker_count {
            let queue = self.queue.clone();
            let notify = self.notify.clone();
            let cancel = self.cancel.clone();
            let result_tx = result_tx.clone();
            workers.spawn(async move {
                worker_loop(worker_index, queue, notify, cancel, result_tx).await;
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
    cancel: CancellationToken,
    result_tx: mpsc::Sender<WorkResult<R>>,
) {
    loop {
        if cancel.is_cancelled() {
            break;
        }

        let unit = queue.lock().await.pop_front();
        let Some(unit) = unit else {
            tokio::select! {
                _ = notify.notified() => continue,
                _ = cancel.cancelled() => break,
            }
        };

        let job_id = unit.job_id.clone();
        let unit_id = unit.unit_id.clone();
        let kind = unit.kind.clone();
        let started = std::time::Instant::now();
        let outcome = (unit.task)(cancel.clone()).await;
        let result = WorkResult {
            job_id,
            unit_id,
            kind,
            outcome,
            elapsed: started.elapsed(),
        };
        if result_tx.send(result).await.is_err() {
            break;
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

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum TestSchedulerOutput {
        Materialized { job_id: String, tracks: usize },
        Encoded { job_id: String, track_index: usize },
        PostProcessed { job_id: String },
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
                    tokio::time::sleep(Duration::from_millis(5)).await;
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

}
