//! Application-wide, bounded directory accounting. Independent of Browse tabs.
//! The UI holds only projections. All SQL and filesystem calls run on workers.
pub mod model;
mod repository;
mod scanner;
mod volume;
pub use model::{AccountingEvent, CachedMeasurement, CompleteMeasurement, Counts, Freshness, ScanEnd, ScanProgress, format_bytes, format_exact};

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicBool, AtomicU8, Ordering}};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::Sender;
use crate::tui::message::AppMessage;
use model::AccountingEvent as Event;
use repository::Repository;
use volume::{Inventory, VolumeBudgets};

#[derive(Debug, Clone, Default)]
pub struct DirectoryView {
    pub cached: Option<CachedMeasurement>,
    pub local: Option<Counts>,
    pub progress: Option<ScanProgress>,
    pub running: bool,
    pub error: Option<String>,
    pub last_end: Option<ScanEnd>,
    /// Modal subscriber token, not a worker id; used to fence replaced dialogs.
    pub subscription: Option<u64>,
}

struct Job {
    id: u64,
    path: PathBuf,
    cancel: Arc<AtomicBool>,
    priority: Arc<AtomicU8>,
    explicit: bool,
    background_interest: bool,
    subscribers: HashSet<u64>,
    invalidation_generation: u64,
}
struct Pending { id: u64, path: PathBuf, explicit: bool, force: bool, subscriber: Option<u64> }

/// One UI-owned registry. It has *at most* one active explicit and one active
/// background worker. A promoted background worker is never duplicated.
pub struct AccountingService {
    db_path: Option<PathBuf>,
    active: HashMap<PathBuf, Job>,
    queued: VecDeque<Pending>,
    next_id: u64,
    pub views: HashMap<PathBuf, DirectoryView>,
    browse_dwell: Option<(PathBuf, Instant)>,
    highlight_dwell: Option<(PathBuf, Instant)>,
    background_tried: HashMap<PathBuf, Instant>,
    explicit_pause: Arc<AtomicBool>,
    background_busy: Arc<AtomicBool>,
    volume_budgets: Arc<VolumeBudgets>,
    invalidation_generation: u64,
    invalidation_tx: Option<std::sync::mpsc::SyncSender<Vec<PathBuf>>>,
    invalidation_overflow: Arc<AtomicBool>,
}
impl AccountingService {
    pub fn new(db_path: Option<PathBuf>) -> Self {
        // A full queue must never discard a filesystem mutation. Mark the
        // entire completed cache dirty instead, using a single atomic bit.
        // Both the queue and its retained path lists are bounded.
        let invalidation_overflow = Arc::new(AtomicBool::new(false));
        let overflow_worker = invalidation_overflow.clone();
        let invalidation_tx = db_path.clone().and_then(|db_path| {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<PathBuf>>(64);
            let thread = std::thread::Builder::new().name("tonepoet-accounting-invalidations".into())
                .spawn(move || {
                    while let Ok(first) = rx.recv() {
                        // Coalesce repeated writes before taking the SQLite lock.
                        let mut batch = first.into_iter().collect::<std::collections::HashSet<_>>();
                        for _ in 0..127 {
                            match rx.try_recv() {
                                Ok(paths) => batch.extend(paths),
                                Err(_) => break,
                            }
                        }
                        let overflow = overflow_worker.swap(false, Ordering::AcqRel);
                        match Repository::open(&db_path).and_then(|mut repo| {
                            if overflow { repo.mark_all_dirty() }
                            else { repo.mark_dirty(&batch.into_iter().collect::<Vec<_>>()) }
                        }) {
                            Ok(()) => {},
                            Err(error) => log::warn!("directory accounting invalidation: {error}"),
                        }
                    }
                });
            match thread {
                Ok(_) => Some(tx),
                Err(error) => { log::warn!("directory accounting invalidation worker: {error}"); None }
            }
        });
        Self { db_path, active: HashMap::new(), queued: VecDeque::new(), next_id: 0,
            views: HashMap::new(), browse_dwell: None, highlight_dwell: None,
            background_tried: HashMap::new(),
            explicit_pause: Arc::new(AtomicBool::new(false)),
            background_busy: Arc::new(AtomicBool::new(false)),
            volume_budgets: Arc::new(VolumeBudgets::default()),
            invalidation_generation: 0, invalidation_tx, invalidation_overflow }
    }
    pub fn view(&self, path: &Path) -> Option<&DirectoryView> { self.views.get(path) }
    pub fn set_interactive_busy(&mut self, busy: bool) {
        self.background_busy.store(busy, Ordering::Release);
    }
    fn refresh_pause(&self) {
        self.explicit_pause.store(self.active.values().any(|job| job.explicit), Ordering::Release);
    }
    fn next_id(&mut self) -> u64 {
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        self.next_id
    }
    /// Return an *independent subscriber token*. A scan id is never used as
    /// authority to cancel another consumer's work.
    pub fn request_explicit(&mut self, path: PathBuf, force: bool, tx: &Sender<AppMessage>) -> u64 {
        let subscriber = self.next_id();
        let another_explicit = self.active.iter().any(|(other_path, job)|
            other_path != &path && job.explicit);
        if let Some(job) = self.active.get_mut(&path) {
            if !force && !another_explicit && !job.cancel.load(Ordering::Acquire) {
                job.subscribers.insert(subscriber);
                job.explicit = true;
                job.priority.store(2, Ordering::Release);
                self.views.entry(path).or_default().subscription = Some(subscriber);
                self.refresh_pause();
                return subscriber;
            }
            // Force replaces the previous generation. A second explicit scan
            // must queue until the worker acknowledges cancellation; never
            // start two independent walkers for the same root.
            job.cancel.store(true, Ordering::Release);
        }
        self.queued.retain(|p| p.path != path);
        let scan_id = self.next_id();
        self.views.entry(path.clone()).or_default().subscription = Some(subscriber);
        if self.active.contains_key(&path) || self.active.values().any(|job| job.explicit) {
            self.queued.push_front(Pending { id: scan_id, path, explicit: true, force, subscriber: Some(subscriber) });
        } else {
            self.launch(path, true, force, scan_id, Some(subscriber), tx);
        }
        subscriber
    }
    /// Detach only this modal. Background-originated work survives a close or
    /// cancel, even if its priority had been promoted for Folder Properties.
    pub fn detach_explicit(&mut self, path: &Path, subscriber: u64) {
        self.queued.retain(|p| !(p.path == path && p.subscriber == Some(subscriber)));
        if let Some(job) = self.active.get_mut(path) {
            if job.subscribers.remove(&subscriber) && job.subscribers.is_empty() {
                if job.background_interest {
                    job.explicit = false;
                    job.priority.store(1, Ordering::Release);
                } else {
                    job.cancel.store(true, Ordering::Release);
                }
            }
        }
        if let Some(view) = self.views.get_mut(path) {
            if view.subscription == Some(subscriber) { view.subscription = None; }
        }
        self.refresh_pause();
    }
    fn settled(dwell: &mut Option<(PathBuf, Instant)>, path: Option<PathBuf>, delay: Duration) -> Option<PathBuf> {
        let Some(path) = path else { *dwell = None; return None; };
        match dwell {
            Some((old, started)) if *old == path => {
                if started.elapsed() >= delay { Some(path) } else { None }
            }
            _ => { *dwell = Some((path, Instant::now())); None }
        }
    }
    pub fn consider_background(&mut self, current: Option<PathBuf>, highlighted: Option<PathBuf>, tx: &Sender<AppMessage>) {
        // Navigation changes immediately discard not-yet-started roots. An
        // active unrelated walk is cancelled at the next metadata I/O token.
        // Completed subtrees remain independently cacheable after cancellation.
        let relevant = |path: &Path| current.as_deref() == Some(path) || highlighted.as_deref() == Some(path);
        self.queued.retain(|pending| pending.explicit || relevant(&pending.path));
        for job in self.active.values() {
            if !job.explicit && !relevant(&job.path) { job.cancel.store(true, Ordering::Release); }
        }
        let settled_current = Self::settled(&mut self.browse_dwell, current, Duration::from_millis(750));
        let settled_highlight = Self::settled(&mut self.highlight_dwell, highlighted, Duration::from_millis(1500));
        // Prefer current Browse folder, then a cursor-stable highlighted folder.
        for candidate in [settled_current, settled_highlight].into_iter().flatten() {
            if candidate.is_absolute() { self.queue_background_if_needed(candidate, tx); }
        }
        if self.background_tried.len() > 4096 {
            self.background_tried.retain(|_, time| time.elapsed() < Duration::from_secs(86400));
            // A user may navigate more than 4096 distinct folders per day.
            // A TTL alone is not a memory bound: evict the oldest attempts.
            if self.background_tried.len() > 4096 {
                let excess = self.background_tried.len() - 4096;
                let mut oldest = self.background_tried.iter()
                    .map(|(path, time)| (path.clone(), *time))
                    .collect::<Vec<_>>();
                oldest.sort_unstable_by_key(|(_, time)| *time);
                for (path, _) in oldest.into_iter().take(excess) {
                    self.background_tried.remove(&path);
                }
            }
        }
    }
    fn queue_background_if_needed(&mut self, path: PathBuf, tx: &Sender<AppMessage>) {
        if self.background_tried.get(&path).is_some_and(|t| t.elapsed() < Duration::from_secs(86400)) { return; }
        if self.active.contains_key(&path) || self.queued.iter().any(|p| p.path == path) { return; }
        if self.queued.len() >= 64 { return; }
        self.background_tried.insert(path.clone(), Instant::now());
        if self.active.values().any(|j| !j.explicit) || self.active.values().any(|j| j.explicit) {
            self.queued.push_back(Pending { id: 0, path, explicit: false, force: false, subscriber: None });
        } else {
            let id = self.next_id();
            self.launch(path, false, false, id, None, tx);
        }
    }
    fn launch(&mut self, path: PathBuf, explicit: bool, force: bool, id: u64, subscriber: Option<u64>, tx: &Sender<AppMessage>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let priority = Arc::new(AtomicU8::new(if explicit { 2 } else { 1 }));
        self.active.insert(path.clone(), Job {
            id, path: path.clone(), cancel: cancel.clone(), priority: priority.clone(),
            explicit, background_interest: !explicit,
            subscribers: subscriber.into_iter().collect(),
            invalidation_generation: self.invalidation_generation,
        });
        let view = self.views.entry(path.clone()).or_default();
        view.running = true;
        if subscriber.is_some() { view.subscription = subscriber; }
        view.progress = Some(ScanProgress::default());
        view.last_end = None;
        view.error = None;
        self.refresh_pause();
        let db_path = self.db_path.clone();
        let tx = tx.clone();
        let explicit_pause = self.explicit_pause.clone();
        let busy = self.background_busy.clone();
        let volume_budgets = self.volume_budgets.clone();
        let path_for_failure = path.clone();
        let started = std::thread::Builder::new().name("tonepoet-directory-accounting".into()).spawn(move || {
            // Reject non-filesystem roots before showing any historical size.
            // Never follow a symlink or Windows reparse point to reach a root.
            let admissible = std::fs::symlink_metadata(&path).map(|metadata| {
                metadata.is_dir() && !scanner::is_excluded(&metadata)
            });
            if !matches!(&admissible, Ok(true)) {
                let reason = match admissible {
                    Ok(false) => format!("{} is not a real filesystem directory", path.display()),
                    Err(error) => format!("{}: {error}", path.display()),
                    Ok(true) => unreachable!(),
                };
                let _ = tx.blocking_send(AppMessage::DirectoryAccounting(Event::Cached {
                    id, path: path.clone(), value: None, error: Some(reason.clone()),
                }));
                let _ = tx.blocking_send(AppMessage::DirectoryAccounting(Event::Finished {
                    id, path, end: ScanEnd::Failed { reason, progress: ScanProgress::default() },
                }));
                return;
            }
            let (mut repo, open_error) = match db_path.as_deref().map(Repository::open).transpose() {
                Ok(repository) => (repository, None),
                Err(error) => (None, Some(format!("cache unavailable: {error}"))),
            };
            let (cached, cache_error) = match repo.as_mut().map(|r| r.lookup(&path)).transpose() {
                Ok(value) => (value.flatten(), open_error),
                Err(error) => (None, Some(format!("cache read failed: {error}"))),
            };
            let _ = tx.blocking_send(AppMessage::DirectoryAccounting(Event::Cached {
                id, path: path.clone(), value: cached.clone(), error: cache_error,
            }));
            if cancel.load(Ordering::Acquire) {
                let _ = tx.blocking_send(AppMessage::DirectoryAccounting(Event::Finished {
                    id, path, end: ScanEnd::Cancelled(ScanProgress::default()),
                }));
                return;
            }
            // Explicit opens display an existing measurement immediately.
            // Opportunistic revalidation runs only for old/known-invalid data.
            let recent_cache = cached.as_ref().is_some_and(|entry| {
                entry.freshness != Freshness::KnownInvalid
                    && entry.measurement.finished.elapsed().is_ok_and(|age| age < Duration::from_secs(24 * 3600))
            });
            if cached.is_some() && !force && (recent_cache || (explicit && cached.as_ref().is_some_and(|v| v.freshness != Freshness::KnownInvalid))) {
                let _ = tx.blocking_send(AppMessage::DirectoryAccounting(Event::Ready { id, path }));
                return;
            }
            let inventory = Inventory::inspect();
            if !explicit && !inventory.permits_background_root(&path) {
                let _ = tx.blocking_send(AppMessage::DirectoryAccounting(Event::Ready { id, path }));
                return;
            }
            let generation = repo.as_mut().and_then(|r| r.allocate_generation().ok()).unwrap_or(id.min(i64::MAX as u64) as i64);
            let mut governor = scanner::RateGovernor::with_budgets(
                cancel, priority, explicit_pause, busy.clone(), volume_budgets, inventory);
            let root_for_local = path.clone();
            let root_for_progress = path.clone();
            let tx_local = tx.clone();
            let tx_progress = tx.clone();
            let mut last_progress = Instant::now() - Duration::from_secs(1);
            let result = scanner::scan(&path, generation, &mut repo, &mut governor,
                |progress| {
                    // Keep producer nonblocking: progress may be conflated/dropped.
                    if !explicit && busy.load(Ordering::Acquire) {
                        // Background pauses at the next token; UI remains responsive.
                    }
                    if last_progress.elapsed() >= Duration::from_millis(250) {
                        let _ = tx_progress.try_send(AppMessage::DirectoryAccounting(Event::Progress {
                            id, path: root_for_progress.clone(), progress,
                        }));
                        last_progress = Instant::now();
                    }
                },
                |counts| {
                    // One local summary per root: deliver reliably (not as
                    // lossy progress) so immediate counts remain available.
                    let _ = tx_local.blocking_send(AppMessage::DirectoryAccounting(Event::Local {
                        id, path: root_for_local.clone(), counts,
                    }));
                });
            // Completing a scan does not mutate the filesystem. Do NOT bump
            // the global mutation sequence simply because a child finished:
            // doing so would spuriously reject concurrently completing parent
            // scans. Invalidate an existing historical ancestor only when an
            // independent child measurement actually changes its totals, or
            // the prior child record has been evicted and disagreement cannot
            // be ruled out. A real TonePoet mutation has its own invalidation
            // fence, regardless of whether total byte counts change.
            if let ScanEnd::Complete { value, saved: true, .. } = &result {
                if let (Some(repository), Some(parent)) = (repo.as_mut(), path.parent()) {
                    let child_may_differ = cached.as_ref().is_none_or(|prior|
                        prior.measurement.counts != value.counts);
                    if child_may_differ {
                        match repository.lookup(parent) {
                            Ok(Some(_)) => {
                                if let Err(error) = repository.mark_dirty(&[parent.to_path_buf()]) {
                                    log::warn!("accounting ancestor invalidation after changed child: {error}");
                                }
                            }
                            Ok(None) => {},
                            Err(error) => log::warn!("accounting ancestor lookup after child scan: {error}"),
                        }
                    }
                }
            }
            // Cache maintenance is worker-only and deliberately bounded per batch.
            if let Some(repository) = repo.as_mut() {
                if let Err(error) = repository.prune() { log::warn!("directory accounting cleanup: {error}"); }
            }
            let _ = tx.blocking_send(AppMessage::DirectoryAccounting(Event::Finished { id, path, end: result }));
        });
        if let Err(error) = started {
            self.active.remove(&path_for_failure);
            self.refresh_pause();
            let view = self.views.entry(path_for_failure).or_default();
            view.running = false;
            view.error = Some(format!("could not start directory worker: {error}"));
        }
    }
    pub fn accept_event(&mut self, event: &Event, tx: &Sender<AppMessage>) -> bool {
        let (id, path) = match event {
            Event::Cached { id, path, .. } | Event::Local { id, path, .. } |
            Event::Progress { id, path, .. } | Event::Finished { id, path, .. } |
            Event::Ready { id, path } => (*id, path),
        };
        if self.active.get(path).map(|j| j.id) != Some(id) { return false; }
        let superseded = self.queued.iter().any(|p| p.path == *path && p.explicit);
        let unchanged_since_launch = self.active.get(path)
            .is_some_and(|j| j.invalidation_generation == self.invalidation_generation);
        let view = self.views.entry(path.clone()).or_default();
        // An older scan may finish after an explicit forced refresh was
        // queued for the same path. Retire its permit without updating the
        // newly requested modal's projection.
        if !superseded { match event {
            Event::Cached { value, error, .. } => {
                if let Some(value) = value {
                    let mut value = value.clone();
                    // A DB lookup may finish after an application mutation;
                    // never let that response revive invalidated measurements.
                    if !unchanged_since_launch { value.freshness = Freshness::KnownInvalid; }
                    view.cached = Some(value);
                }
                if error.is_some() && value.is_none() { view.cached = None; }
                if let Some(error) = error { view.error = Some(error.clone()); }
            }
            Event::Local { counts, .. } => {
                if unchanged_since_launch { view.local = Some(*counts); }
            },
            Event::Progress { progress, .. } => view.progress = Some(progress.clone()),
            Event::Ready { .. } => { view.running = false; view.progress = None; }
            Event::Finished { end, .. } => {
                view.running = false;
                view.progress = None;
                view.last_end = Some(end.clone());
                match end {
                    ScanEnd::Complete { value, saved, .. } => {
                        view.cached = Some(CachedMeasurement {
                            measurement: value.clone(),
                            freshness: if !unchanged_since_launch { Freshness::KnownInvalid }
                                else if !saved { Freshness::Unverified }
                                else { Freshness::AsObserved },
                        });
                    }
                    ScanEnd::Failed { reason, .. } => view.error = Some(reason.clone()),
                    ScanEnd::Changed { reason, .. } => { view.error = Some(reason.clone()); view.local = None; },
                    ScanEnd::Cancelled(_) => {},
                }
            }
        }
        }
        if let Event::Finished { end: ScanEnd::Complete { saved: true, .. }, .. } = event {
            for ancestor in path.ancestors().skip(1) {
                if let Some(parent) = self.views.get_mut(ancestor) {
                    if let Some(value) = parent.cached.as_mut() { value.freshness = Freshness::KnownInvalid; }
                }
            }
        }
        if matches!(event, Event::Ready { .. } | Event::Finished { .. }) {
            if matches!(event, Event::Finished { end: ScanEnd::Cancelled(_), .. }) {
                self.background_tried.remove(path);
            }
            self.active.remove(path);
            self.refresh_pause();
            // Never run two foreground walks or two background roots. A
            // queued explicit may coexist with the one paused background root.
            let foreground_active = self.active.values().any(|j| j.explicit);
            let background_active = self.active.values().any(|j| !j.explicit);
            let index = if foreground_active { None } else {
                self.queued.iter().position(|p| p.explicit)
                    .or_else(|| if !background_active { self.queued.iter().position(|p| !p.explicit) } else { None })
            };
            if let Some(index) = index {
                if let Some(pending) = self.queued.remove(index) {
                    let id = if pending.id == 0 { self.next_id() } else { pending.id };
                    self.launch(pending.path, pending.explicit, pending.force, id, pending.subscriber, tx);
                }
            }
        }
        if self.views.len() > 1024 {
            let pinned = self.active.keys().chain(self.queued.iter().map(|p| &p.path))
                .cloned().collect::<std::collections::HashSet<_>>();
            let overflow = self.views.len() - 1024;
            let to_remove = self.views.keys().filter(|path| !pinned.contains(*path))
                .take(overflow).cloned().collect::<Vec<_>>();
            for obsolete in to_remove { self.views.remove(&obsolete); }
        }
        true
    }
    pub fn invalidate(&mut self, paths: Vec<PathBuf>) {
        self.invalidation_generation = self.invalidation_generation.checked_add(1).unwrap_or(1);
        for path in &paths {
            for ancestor in path.ancestors() {
                self.background_tried.remove(ancestor);
                if let Some(view) = self.views.get_mut(ancestor) {
                    view.local = None;
                    if let Some(cached) = &mut view.cached { cached.freshness = Freshness::KnownInvalid; }
                }
            }
        }
        if let Some(tx) = &self.invalidation_tx {
            if paths.len() > 4096 {
                self.invalidation_overflow.store(true, Ordering::Release);
                // Wake the consumer using one bounded signal.
                let _ = tx.try_send(Vec::new());
            } else {
                match tx.try_send(paths) {
                    Ok(()) => {},
                    Err(std::sync::mpsc::TrySendError::Full(_)) => {
                        // At least one item is already queued to wake the
                        // worker. The global invalidation is fail-closed.
                        self.invalidation_overflow.store(true, Ordering::Release);
                    },
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        log::warn!("directory accounting invalidation worker disconnected");
                    },
                }
            }
        }
    }
}
impl Drop for AccountingService {
    fn drop(&mut self) {
        for job in self.active.values() { job.cancel.store(true, Ordering::Release); }
    }
}


#[cfg(test)]
mod service_tests {
    use super::*;
    use std::time::SystemTime;
    use model::{CompleteMeasurement, Observation};

    fn job(path: &Path, id: u64, explicit: bool, generation: u64) -> Job {
        Job {
            id, path: path.to_path_buf(), cancel: Arc::new(AtomicBool::new(false)),
            priority: Arc::new(AtomicU8::new(if explicit { 2 } else { 1 })),
            explicit, background_interest: !explicit,
            subscribers: HashSet::new(), invalidation_generation: generation,
        }
    }

    #[test]
    fn old_request_progress_cannot_update_new_folder_view() {
        let mut service = AccountingService::new(None);
        let current = PathBuf::from("/test/new");
        service.active.insert(current.clone(), job(&current, 9, false, 0));
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        assert!(!service.accept_event(&Event::Progress {
            id: 8, path: current.clone(), progress: ScanProgress {
                files_examined: 9_999, ..ScanProgress::default()
            },
        }, &tx));
        assert!(service.view(&current).is_none());
        assert!(service.accept_event(&Event::Progress {
            id: 9, path: current.clone(), progress: ScanProgress {
                files_examined: 7, ..ScanProgress::default()
            },
        }, &tx));
        assert_eq!(service.view(&current).unwrap().progress.as_ref().unwrap().files_examined, 7);
    }

    #[test]
    fn late_cached_response_cannot_undo_local_invalidation() {
        let mut service = AccountingService::new(None);
        let path = PathBuf::from("/test/album");
        service.active.insert(path.clone(), job(&path, 21, true, 0));
        service.invalidation_generation = 1; // a committed source mutation
        let now = SystemTime::now();
        let value = CachedMeasurement {
            measurement: CompleteMeasurement {
                path: path.clone(), counts: Counts { files: 1, bytes: 4096, ..Counts::default() },
                started: now, finished: now, created: None, modified: None,
                generation: 1, observation: Observation::AsObserved,
            },
            freshness: Freshness::Unverified,
        };
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        assert!(service.accept_event(&Event::Cached {
            id: 21, path: path.clone(), value: Some(value), error: None,
        }, &tx));
        assert_eq!(service.view(&path).unwrap().cached.as_ref().unwrap().freshness, Freshness::KnownInvalid);
        assert!(service.accept_event(&Event::Local {
            id: 21, path: path.clone(), counts: Counts { folders: 3, ..Counts::default() },
        }, &tx));
        assert!(service.view(&path).unwrap().local.is_none());
    }

    #[test]
    fn detaching_properties_preserves_promoted_background_scan() {
        let mut service = AccountingService::new(None);
        let path = PathBuf::from("/tmp/SharedCollection");
        service.active.insert(path.clone(), job(&path, 5, false, 0));
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let modal = service.request_explicit(path.clone(), false, &tx);
        let active = service.active.get(&path).unwrap();
        assert_eq!(active.id, 5, "promotion must not launch a second walker");
        assert!(active.explicit);
        assert_eq!(active.subscribers.len(), 1);
        service.detach_explicit(&path, modal);
        let active = service.active.get(&path).unwrap();
        assert!(!active.cancel.load(Ordering::Acquire));
        assert!(!active.explicit);
        assert_eq!(active.priority.load(Ordering::Acquire), 1);
        assert!(active.subscribers.is_empty());
        assert_eq!(service.view(&path).unwrap().subscription, None);
    }

    #[test]
    fn separate_explicit_subscribers_share_work_without_mutual_cancellation() {
        let mut service = AccountingService::new(None);
        let path = PathBuf::from("/tmp/TwoModalSubscribers");
        let mut active = job(&path, 7, true, 0);
        active.subscribers.insert(70);
        service.active.insert(path.clone(), active);
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let other = service.request_explicit(path.clone(), false, &tx);
        assert_ne!(other, 7);
        service.detach_explicit(&path, other);
        assert!(service.active.get(&path).unwrap().subscribers.contains(&70));
        assert!(!service.active.get(&path).unwrap().cancel.load(Ordering::Acquire));
        service.detach_explicit(&path, 70);
        assert!(service.active.get(&path).unwrap().cancel.load(Ordering::Acquire),
            "a foreground-only scan cancels after its last subscriber detaches");
    }

    #[test]
    fn opening_other_folder_does_not_promote_second_foreground_worker() {
        let mut service = AccountingService::new(None);
        let background = PathBuf::from("/test/background");
        let foreground = PathBuf::from("/test/foreground");
        service.active.insert(background.clone(), job(&background, 4, false, 0));
        service.active.insert(foreground, job(Path::new("/test/foreground"), 5, true, 0));
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let queued_id = service.request_explicit(background.clone(), false, &tx);
        assert_ne!(queued_id, 4);
        assert!(service.active.get(&background).unwrap().cancel.load(Ordering::Acquire));
        assert!(!service.active.get(&background).unwrap().explicit);
        assert!(service.queued.iter().any(|p| p.path == background && p.explicit));
        assert_eq!(service.active.values().filter(|j| j.explicit).count(), 1);
    }
}
