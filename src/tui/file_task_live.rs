//! Session-independent observation and control for durable Browse copy/move workers.
//!
//! The helper owns a small, replace-atomically progress snapshot and a bounded
//! command mailbox beside its journal, never in a transfer destination. A TUI
//! started later may observe or request a control without taking the helper's
//! durable filesystem lease. Commands are bound to one journal generation;
//! they can never be replayed by a resumed generation.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tui_file_picker::{ConflictPromptState, FileTaskPhase, FileTaskProgressUpdate, ProgressItem, ProgressTotals};

use super::app::FileTaskControl;
use crate::concurrency::OwnerProcessIdentity;

const SNAPSHOT_SCHEMA: u32 = 1;
const MAX_CONTROL_BYTES: u64 = 16 * 1024;
const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024;
const CONTROL_POLL: Duration = Duration::from_millis(180);
const SNAPSHOT_PERIOD: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveWorkerRef {
    pub journal_path: PathBuf,
    pub job_id: String,
    pub generation: u64,
}

impl LiveWorkerRef {
    pub fn new(journal_path: PathBuf, job_id: String, generation: u64) -> Result<Self, String> {
        if uuid::Uuid::parse_str(&job_id).is_err() || generation == 0 {
            return Err("invalid file task observation identity".into());
        }
        if journal_path.parent() != Some(super::file_task_runtime::file_task_journal_dir().as_path())
            || journal_path.file_name().and_then(|s| s.to_str()) != Some(format!("{job_id}.jsonl").as_str())
        {
            return Err("file task observation path is outside the journal namespace".into());
        }
        Ok(Self { journal_path, job_id, generation })
    }

    fn dir(&self) -> PathBuf {
        self.journal_path.parent().unwrap_or_else(|| Path::new(".")).join(format!(
            ".live-{}-{}", self.job_id, self.generation
        ))
    }

    fn snapshot(&self) -> PathBuf { self.dir().join("progress.json") }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveWorkerSnapshot {
    pub schema: u32,
    pub job_id: String,
    pub generation: u64,
    pub worker: OwnerProcessIdentity,
    pub observed_unix_ms: u64,
    /// Time of last actual byte, item, or current-file change. Status-only
    /// repaints cannot disguise a stalled transfer as forward progress.
    pub forward_unix_ms: u64,
    pub phase: FileTaskPhase,
    pub status: String,
    pub current_item: Option<ProgressItem>,
    pub totals: ProgressTotals,
    pub rate_bytes_per_sec: Option<u64>,
    /// Unresolved conflict belongs to this worker generation, not to the viewer.
    #[serde(default)]
    pub conflict: Option<ConflictPromptState>,
}

impl LiveWorkerSnapshot {
    pub fn age_since_forward_ms(&self) -> u64 {
        unix_ms().saturating_sub(self.forward_unix_ms)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemoteCommand {
    job_id: String,
    generation: u64,
    issued_unix_ms: u64,
    control: FileTaskControl,
}

pub fn read_snapshot(identity: &LiveWorkerRef) -> Option<LiveWorkerSnapshot> {
    let path = identity.snapshot();
    let file = File::open(&path).ok()?;
    if file.metadata().ok()?.len() > MAX_SNAPSHOT_BYTES { return None; }
    let mut bytes = Vec::new();
    file.take(MAX_SNAPSHOT_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES { return None; }
    let snapshot: LiveWorkerSnapshot = serde_json::from_slice(&bytes).ok()?;
    (snapshot.schema == SNAPSHOT_SCHEMA
        && snapshot.job_id == identity.job_id
        && snapshot.generation == identity.generation
        && snapshot.observed_unix_ms >= snapshot.forward_unix_ms)
        .then_some(snapshot)
}

/// Atomic publication is deliberately best effort: a full or unreachable
/// config device must never stop a copy that is writing to another device.
fn publish_snapshot(identity: &LiveWorkerRef, snapshot: &LiveWorkerSnapshot) -> Result<(), String> {
    let dir = identity.dir();
    let name = format!(".progress-{}.tmp", uuid::Uuid::new_v4());
    let temp = dir.join(name);
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)
            .map_err(|error| format!("create progress snapshot: {error}"))?;
        let bytes = serde_json::to_vec(snapshot)
            .map_err(|error| format!("serialize progress: {error}"))?;
        if bytes.len() as u64 > MAX_SNAPSHOT_BYTES { return Err("progress snapshot too large".into()); }
        file.write_all(&bytes).map_err(|error| format!("write progress: {error}"))?;
        drop(file);
        fs::rename(&temp, identity.snapshot())
            .map_err(|error| format!("publish progress snapshot: {error}"))
    })();
    if result.is_err() { let _ = fs::remove_file(&temp); }
    result
}

#[derive(Clone)]
pub struct LiveProgressSender {
    sender: mpsc::SyncSender<FileTaskProgressUpdate>,
    /// Never coalesce away an unresolved conflict, even if the progress queue
    /// is full of byte snapshots. This small state is published independently.
    conflict: Arc<Mutex<Option<ConflictPromptState>>>,
}

impl LiveProgressSender {
    pub fn publish(&self, update: &FileTaskProgressUpdate) {
        match update {
            FileTaskProgressUpdate::ShowConflict { conflict } => {
                *self.conflict.lock().unwrap_or_else(|p| p.into_inner()) = Some(conflict.clone());
            }
            FileTaskProgressUpdate::UpdateConflictExistingStats { request_id, size, modified } => {
                let mut active = self.conflict.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(conflict) = active.as_mut().filter(|c| c.request_id == *request_id) {
                    conflict.existing_size = Some(*size);
                    if let Some(modified) = modified {
                        conflict.existing_modified = Some(*modified);
                    }
                }
            }
            FileTaskProgressUpdate::ClearConflict
            | FileTaskProgressUpdate::Finished { .. }
            | FileTaskProgressUpdate::Failed { .. }
            | FileTaskProgressUpdate::Aborted { .. } => {
                *self.conflict.lock().unwrap_or_else(|p| p.into_inner()) = None;
            }
            _ => {}
        }
        // Never block the transfer. The publisher also polls the shared
        // conflict state, so a full bounded channel cannot lose a prompt.
        let _ = self.sender.try_send(update.clone());
    }
}

fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[cfg(unix)]
fn private_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    match fs::create_dir(path) {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
        Err(error) => return Err(format!("create live worker directory: {error}")),
    }
    let meta = fs::symlink_metadata(path).map_err(|e| format!("stat live worker directory: {e}"))?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("live worker directory is not a real directory".into());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("protect live worker directory: {e}"))
}

#[cfg(not(unix))]
fn private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| format!("create live worker directory: {e}"))
}

/// Start two detached, bounded threads: the publisher coalesces noisy byte
/// updates, and the mailbox reader forwards small control messages to the
/// existing worker receiver. The returned guard stops polling on worker exit.
pub fn start_worker(
    identity: LiveWorkerRef,
    controls: mpsc::Sender<FileTaskControl>,
) -> Result<(LiveProgressSender, LiveWorkerGuard), String> {
    private_dir(&identity.dir())?;
    // The same generation cannot have two valid workers: the durable lease
    // prevents that. Discard stale mailbox files left by an earlier crash.
    let dir = identity.dir();
    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())?.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("control") {
            let _ = fs::remove_file(path);
        }
    }

    let alive = Arc::new(AtomicBool::new(true));
    let control_alive = Arc::clone(&alive);
    let control_identity = identity.clone();
    let _ = std::thread::Builder::new()
        .name("file-task-detached-controls".into())
        .spawn(move || {
            while control_alive.load(Ordering::Acquire) {
                for path in read_control_paths(&control_identity.dir()) {
                    if let Ok(Some(control)) = consume_command(&control_identity, &path) {
                        if controls.send(control).is_err() { return; }
                    }
                }
                std::thread::sleep(CONTROL_POLL);
            }
        })
        .map_err(|e| format!("start worker control listener: {e}"))?;

    let (sender, receiver) = mpsc::sync_channel::<FileTaskProgressUpdate>(2);
    let conflict = Arc::new(Mutex::new(None::<ConflictPromptState>));
    let published_conflict = Arc::clone(&conflict);
    std::thread::Builder::new()
        .name("file-task-detached-progress".into())
        .spawn(move || {
            let worker = OwnerProcessIdentity::current();
            let mut previous = None::<LiveWorkerSnapshot>;
            loop {
                // Always retain the latest byte snapshot, even when a later
                // control event occupies the queue. Conflict transitions are
                // independently protected by the shared state below.
                let mut latest = match receiver.recv_timeout(SNAPSHOT_PERIOD) {
                    Ok(update @ FileTaskProgressUpdate::Snapshot { .. }) => Some(update),
                    Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                while let Ok(next) = receiver.try_recv() {
                    if matches!(next, FileTaskProgressUpdate::Snapshot { .. }) {
                        latest = Some(next);
                    }
                }
                let conflict = published_conflict.lock()
                    .unwrap_or_else(|p| p.into_inner()).clone();
                let (phase, status, current_item, totals, rate_bytes_per_sec) = match latest {
                    Some(FileTaskProgressUpdate::Snapshot {
                        phase, status, current_item, totals, rate_bytes_per_sec,
                    }) => (phase, status, current_item, totals, rate_bytes_per_sec),
                    _ => match &previous {
                        // A full progress queue can drop the Paused snapshot.
                        // The independently retained conflict is still proof
                        // that the worker is awaiting a destination decision.
                        Some(old) if conflict.is_some()
                            && !matches!(&old.phase, FileTaskPhase::Paused | FileTaskPhase::Stalled) =>
                            (FileTaskPhase::Paused, "Waiting for conflict resolution".into(),
                                old.current_item.clone(), old.totals, old.rate_bytes_per_sec),
                        Some(old) => (old.phase.clone(), old.status.clone(), old.current_item.clone(),
                            old.totals, old.rate_bytes_per_sec),
                        None if conflict.is_some() => (FileTaskPhase::Paused,
                            "Waiting for conflict resolution".into(), None,
                            ProgressTotals::default(), None),
                        None => continue,
                    },
                };
                let now = unix_ms();
                let forward = previous.as_ref().map_or(true, |old| {
                    old.totals.bytes_done != totals.bytes_done
                        || old.totals.items_done != totals.items_done
                        || old.current_item.as_ref().map(|item| (&item.source, item.bytes_done))
                            != current_item.as_ref().map(|item| (&item.source, item.bytes_done))
                });
                let forward_unix_ms = if forward { now } else {
                    previous.as_ref().map_or(now, |old| old.forward_unix_ms)
                };
                let snapshot = LiveWorkerSnapshot {
                    schema: SNAPSHOT_SCHEMA,
                    job_id: identity.job_id.clone(), generation: identity.generation,
                    worker, observed_unix_ms: now, forward_unix_ms,
                    phase, status, current_item, totals, rate_bytes_per_sec, conflict,
                };
                let must_publish = forward
                    || previous.as_ref().is_none_or(|old|
                        old.phase != snapshot.phase || old.status != snapshot.status
                        || old.conflict != snapshot.conflict)
                    || previous.as_ref().is_some_and(|old|
                        now.saturating_sub(old.observed_unix_ms) >= 2_000);
                if must_publish { let _ = publish_snapshot(&identity, &snapshot); }
                previous = Some(snapshot);
            }
        })
        .map_err(|e| format!("start worker progress publisher: {e}"))?;
    Ok((LiveProgressSender { sender, conflict }, LiveWorkerGuard { alive }))
}

pub struct LiveWorkerGuard { alive: Arc<AtomicBool> }
impl Drop for LiveWorkerGuard {
    fn drop(&mut self) { self.alive.store(false, Ordering::Release); }
}

fn read_control_paths(dir: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(dir).ok().into_iter().flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("control"))
        .collect::<Vec<_>>();
    paths.sort();
    paths.truncate(32);
    paths
}

fn consume_command(identity: &LiveWorkerRef, path: &Path) -> Result<Option<FileTaskControl>, String> {
    if path.parent() != Some(identity.dir().as_path()) { return Ok(None); }
    let file = File::open(path).map_err(|e| format!("open worker command: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_CONTROL_BYTES + 1).read_to_end(&mut bytes)
        .map_err(|e| format!("read worker command: {e}"))?;
    let parsed = if bytes.len() as u64 > MAX_CONTROL_BYTES { None } else {
        serde_json::from_slice::<RemoteCommand>(&bytes).ok()
    };
    let _ = fs::remove_file(path);
    Ok(parsed.filter(|cmd| cmd.job_id == identity.job_id && cmd.generation == identity.generation)
        .map(|cmd| cmd.control))
}

/// Queue a generation-bound request without touching the transfer destination.
/// The worker will acknowledge its effect in the next live progress snapshot.
pub fn request_control(identity: &LiveWorkerRef, control: FileTaskControl) -> Result<(), String> {
    let dir = identity.dir();
    let meta = fs::symlink_metadata(&dir).map_err(|_| "worker control channel unavailable".to_string())?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("worker control channel is not a real directory".into());
    }
    if read_control_paths(&dir).len() >= 16 {
        return Err("worker has not accepted earlier requests; use Force stop if appropriate".into());
    }
    let cmd = RemoteCommand {
        job_id: identity.job_id.clone(), generation: identity.generation,
        issued_unix_ms: unix_ms(), control,
    };
    let filename = format!("{:020}-{}.control", cmd.issued_unix_ms, uuid::Uuid::new_v4());
    let target = dir.join(filename);
    let temporary = dir.join(format!(".control-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new().create_new(true).write(true).open(&temporary)
            .map_err(|e| format!("create worker command: {e}"))?;
        serde_json::to_writer(&mut file, &cmd)
            .map_err(|e| format!("encode worker command: {e}"))?;
        file.sync_all().map_err(|e| format!("sync worker command: {e}"))?;
        fs::rename(&temporary, &target).map_err(|e| format!("publish worker command: {e}"))?;
        File::open(&dir).and_then(|f| f.sync_all())
            .map_err(|e| format!("sync worker command directory: {e}"))
    })();
    if result.is_err() { let _ = fs::remove_file(&temporary); }
    result
}

/// Linux ties the signal to an opened pidfd. macOS has no pidfd equivalent,
/// so we require the existing boot/start identity and recheck immediately
/// before using the native process-signal API. Unknown identity fails closed.
#[cfg(target_os = "linux")]
pub fn force_stop(worker: OwnerProcessIdentity) -> Result<(), String> {
    if !worker.appears_active() || worker.pid == u64::from(std::process::id()) {
        return Err("worker identity is no longer live".into());
    }
    let pid = i32::try_from(worker.pid).map_err(|_| "invalid worker pid".to_string())?;
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
    if fd < 0 { return Err(format!("open worker pidfd: {}", std::io::Error::last_os_error())); }
    let result = if !worker.appears_active() {
        Err("worker identity changed before the signal".into())
    } else {
        let rc = unsafe {
            libc::syscall(libc::SYS_pidfd_send_signal, fd, libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(), 0)
        };
        if rc < 0 { Err(format!("signal worker: {}", std::io::Error::last_os_error())) }
        else { Ok(()) }
    };
    unsafe { libc::close(fd); }
    result
}

#[cfg(target_os = "macos")]
pub fn force_stop(worker: OwnerProcessIdentity) -> Result<(), String> {
    // A PID alone can name a recycled process. Refuse when the existing
    // per-boot process-start proof is absent, and never kill this TUI.
    if worker.pid == u64::from(std::process::id())
        || worker.start_ticks == 0 || worker.boot_id_hash == 0
        || !worker.appears_active()
    {
        return Err("worker process identity cannot be verified".into());
    }
    let pid = libc::pid_t::try_from(worker.pid)
        .map_err(|_| "invalid worker pid".to_string())?;
    if unsafe { libc::kill(pid, 0) } != 0 {
        return Err(format!("worker signal permission/liveness check: {}", std::io::Error::last_os_error()));
    }
    // Re-read the same kernel start timeval/host boot identity that protected
    // the durable lease. Failure or mismatch must not produce a signal.
    if !worker.appears_active() {
        return Err("worker identity changed before the signal".into());
    }
    if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
        return Err(format!("signal verified worker: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn force_stop(_worker: OwnerProcessIdentity) -> Result<(), String> {
    Err("Force stop requires verifiable process start identity and is unavailable on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_snapshot_never_replays_a_previous_generation() {
        let _guard = super::super::file_task_runtime::test_environment_lock();
        let temp = tempfile::tempdir().unwrap();
        std::env::set_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR", temp.path());
        let id = uuid::Uuid::new_v4().to_string();
        let path = super::super::file_task_runtime::file_task_journal_dir().join(format!("{id}.jsonl"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let one = LiveWorkerRef::new(path.clone(), id.clone(), 1).unwrap();
        private_dir(&one.dir()).unwrap();
        let snapshot = LiveWorkerSnapshot {
            schema: SNAPSHOT_SCHEMA, job_id: id.clone(), generation: 1,
            worker: OwnerProcessIdentity::current(), observed_unix_ms: 100,
            forward_unix_ms: 100, phase: FileTaskPhase::Running, status: "copying".into(),
            current_item: None, totals: ProgressTotals::default(), rate_bytes_per_sec: None,
            conflict: None,
        };
        publish_snapshot(&one, &snapshot).unwrap();
        assert!(read_snapshot(&one).is_some());
        let two = LiveWorkerRef::new(path, id, 2).unwrap();
        assert!(read_snapshot(&two).is_none());
        std::env::remove_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR");
    }

    #[test]
    fn unresolved_conflicts_survive_snapshot_coalescing_and_clear_by_generation() {
        let _guard = super::super::file_task_runtime::test_environment_lock();
        let temp = tempfile::tempdir().expect("test journal namespace");
        std::env::set_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR", temp.path());
        let id = uuid::Uuid::new_v4().to_string();
        let path = super::super::file_task_runtime::file_task_journal_dir()
            .join(format!("{id}.jsonl"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let identity = LiveWorkerRef::new(path, id, 1).unwrap();
        let (tx, rx) = mpsc::channel();
        let (sender, guard) = start_worker(identity.clone(), tx).unwrap();

        let frame = |bytes| FileTaskProgressUpdate::Snapshot {
            phase: FileTaskPhase::Paused,
            status: "Waiting for a destination conflict decision".into(),
            current_item: Some(ProgressItem::new("incoming.flac")),
            totals: ProgressTotals { bytes_done: bytes, bytes_total: Some(10_000),
                ..ProgressTotals::default() },
            rate_bytes_per_sec: Some(123),
        };
        sender.publish(&frame(1_000));
        let mut conflict = ConflictPromptState::new(5, "Destination exists", "Choose policy",
            tui_file_picker::ConflictItemKind::File);
        conflict.existing_size = Some(100);
        sender.publish(&FileTaskProgressUpdate::ShowConflict { conflict: conflict.clone() });
        // The bounded queue may discard all these frames. It cannot discard
        // the pending prompt or its request identity.
        for step in 0..128 { sender.publish(&frame(step * 50)); }
        let until = std::time::Instant::now() + Duration::from_secs(5);
        let first = loop {
            if let Some(snapshot) = read_snapshot(&identity) {
                if snapshot.conflict.as_ref().map(|p| p.request_id) == Some(5) {
                    break snapshot;
                }
            }
            assert!(std::time::Instant::now() < until, "worker conflict not published");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(first.conflict.as_ref().unwrap().existing_size, Some(100));
        let modified = SystemTime::now();
        sender.publish(&FileTaskProgressUpdate::UpdateConflictExistingStats {
            request_id: 5, size: 456, modified: Some(modified),
        });
        sender.publish(&FileTaskProgressUpdate::UpdateConflictExistingStats {
            request_id: 4, size: 999, modified: None,
        });
        let until = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(snapshot) = read_snapshot(&identity) {
                if snapshot.conflict.as_ref().and_then(|p| p.existing_size) == Some(456) {
                    assert_eq!(snapshot.conflict.unwrap().existing_modified, Some(modified));
                    break;
                }
            }
            assert!(std::time::Instant::now() < until, "conflict stats not published");
            std::thread::sleep(Duration::from_millis(20));
        }
        request_control(&identity, FileTaskControl::ConflictResolution {
            request_id: 5, resolution: tui_file_picker::ConflictResolution::Rename,
        }).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            FileTaskControl::ConflictResolution {
                request_id: 5, resolution: tui_file_picker::ConflictResolution::Rename,
            });
        sender.publish(&FileTaskProgressUpdate::ClearConflict);
        let until = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if read_snapshot(&identity).is_some_and(|s| s.conflict.is_none()) { break; }
            assert!(std::time::Instant::now() < until, "conflict not cleared");
            std::thread::sleep(Duration::from_millis(20));
        }
        conflict.request_id = 6;
        conflict.existing_size = None;
        sender.publish(&FileTaskProgressUpdate::ShowConflict { conflict });
        let until = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if read_snapshot(&identity).is_some_and(|s| s.conflict.as_ref().map(|p| p.request_id) == Some(6)) { break; }
            assert!(std::time::Instant::now() < until, "second conflict not published");
            std::thread::sleep(Duration::from_millis(20));
        }
        let newer = LiveWorkerRef::new(identity.journal_path.clone(), identity.job_id.clone(), 2).unwrap();
        assert!(read_snapshot(&newer).is_none(), "new generations cannot inherit old prompts");
        sender.publish(&FileTaskProgressUpdate::ClearConflict);
        drop(sender);
        drop(guard);
        std::env::remove_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn force_stop_only_the_verified_macos_helper_process() {
        let mut helper = std::process::Command::new("/bin/sleep")
            .stdin(std::process::Stdio::null())
            .arg("30").spawn().expect("start unresponsive helper");
        let pid = helper.id();
        let identity_text = crate::convert::script_supervisor::local_process_start_identity(pid)
            .expect("macOS process identity query").expect("helper remains alive");
        let hash = identity_text.bytes().fold(0xcbf29ce484222325_u64, |state, byte| {
            (state ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        }).max(1);
        let worker = OwnerProcessIdentity {
            pid: u64::from(pid), start_ticks: hash,
            boot_id_hash: OwnerProcessIdentity::current().boot_id_hash,
            process_token: 0,
        };
        let mismatch = OwnerProcessIdentity { start_ticks: hash.wrapping_add(1), ..worker };
        let refused = force_stop(mismatch);
        let still_alive = helper.try_wait().expect("query helper").is_none();
        let terminated = force_stop(worker);
        let _ = helper.kill(); // cleanup on failed assertions, too
        let status = helper.wait().expect("reap helper");
        assert!(refused.is_err(), "must refuse a recycled-process identity");
        assert!(still_alive, "mismatched identity must not terminate a process");
        assert!(terminated.is_ok(), "verified macOS process should accept force-stop: {terminated:?}");
        assert!(!status.success(), "force-stopped helper should not exit normally");
    }

    #[test]
    fn command_mailbox_is_generation_bound() {
        let _guard = super::super::file_task_runtime::test_environment_lock();
        let temp = tempfile::tempdir().unwrap();
        std::env::set_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR", temp.path());
        let id = uuid::Uuid::new_v4().to_string();
        let path = super::super::file_task_runtime::file_task_journal_dir().join(format!("{id}.jsonl"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let one = LiveWorkerRef::new(path, id, 1).unwrap();
        private_dir(&one.dir()).unwrap();
        request_control(&one, FileTaskControl::Pause).unwrap();
        let pending = read_control_paths(&one.dir());
        assert_eq!(pending.len(), 1);
        assert_eq!(consume_command(&one, &pending[0]).unwrap(), Some(FileTaskControl::Pause));
        assert!(read_control_paths(&one.dir()).is_empty());
        std::env::remove_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR");
    }
}
