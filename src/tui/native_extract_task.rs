//! Native disc extraction on the existing durable, detached file-task channel.
//!
//! The TUI owns only a progress surface and a supervisor. The helper owns the
//! filesystem lease, native materializers, progress feed and journal. A TUI
//! exit does not cancel the helper. Recovery uses the same live observation
//! mailbox and durable journal as Browse copy/move, not Convert's queue.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tui_file_picker::{
    FileTaskKind, FileTaskPhase, FileTaskProgressState, FileTaskProgressUpdate,
    FileTaskScope, PasteMapping, ProgressItem, ProgressTotals, ProgressUnit,
    VerificationMode,
};

use super::app::{AppState, FileTaskControl, FileTaskProgressSession};
use super::file_task_runtime::{
    self, DurableFileTaskLifecycle, DurableFileTaskRecord, DurableNativePreparedCue, FileTaskJournalHandle,
    FileTaskWireEvent,
};
use super::message::AppMessage;
use crate::convert::pipeline::native_extract::{
    self, NativeExtractionControl, NativeExtractionJob, NativeExtractionProgress,
    NativeExtractionReport, NativeSkippedSelection, NativeLogOutcome,
    NativeWatchSource, NativeWriteWatch,
};

const SAMPLING_INTERVAL: Duration = Duration::from_millis(175);
const ABORT_GRACE: Duration = Duration::from_secs(15);

/// Payload stored in the existing file-task journal. The authored job remains
/// intact across generations; finished outputs are recorded separately so a
/// recovered helper cannot silently repeat already-published selections.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableNativeExtractionTask {
    session_id: u64,
    job_id: String,
    generation: u64,
    dest: String,
    native_extraction: NativeExtractionJob,
}

fn new_progress(app: &AppState, job: &NativeExtractionJob) -> FileTaskProgressState {
    let total = job.programs.iter().map(|p| p.tracks.len()).sum::<usize>();
    let mut progress = FileTaskProgressState::new(
        FileTaskKind::Extract, "Extract tracks",
        super::keybindings::file_picker_theme_from_theme(&app.theme),
    );
    progress.set_scope(FileTaskScope {
        source_root: Some(job.source.clone()),
        source_summary: format!("{total} selected tracks/chapters"),
        destination: Some(job.destination.clone()),
        destination_summary: None,
    });
    progress.set_task_controls_available(true);
    progress.set_minimize_available(true);
    progress
}

/// Freeze the user's selection and join the same FIFO as clipboard copy/move.
/// No journal or detached helper is started until the scheduler dispatches it.
/// Convert has an independent queue and is never involved in this path.
pub(super) fn launch(
    app: &mut AppState,
    job: NativeExtractionJob,
    tx: &mpsc::Sender<AppMessage>,
) -> Result<(), String> {
    if job.programs.is_empty() || !job.programs.iter().any(|p| !p.tracks.is_empty()) {
        return Err("Nothing selected for extraction".into());
    }
    let queued = super::app::QueuedNativeExtraction {
        queue_id: super::app::next_file_transfer_queue_id(),
        job,
    };
    let queue_id = queued.queue_id;
    app.file_transfers.queued.push_back(super::app::QueuedFileTask::Extract(queued));
    // Starting or queuing extraction closes its selection dialog, just as
    // starting a paste returns to Browse. Existing minimized work stays live.
    if matches!(&app.active_overlay, super::app::ActiveOverlay::DiscBrowser(_)) {
        app.active_overlay = super::app::ActiveOverlay::None;
    }
    app.sync_file_transfer_queue_surfaces();
    if app.file_transfers.active_session_id.is_none() && !app.file_transfers.blocked_for_attention {
        super::keybindings::maybe_start_next_file_transfer(app, tx);
    }
    if let Some(index) = app.file_transfers.queued.iter().position(|entry| entry.queue_id() == queue_id) {
        app.set_status(format!("Queued extraction at position {} in the file-task queue", index + 1));
    }
    Ok(())
}

/// Called by the existing file-task FIFO only when its active slot is free.
pub(super) fn dispatch_queued(
    app: &mut AppState,
    mut job: NativeExtractionJob,
    tx: &mpsc::Sender<AppMessage>,
    start_minimized: bool,
) -> Result<(), String> {
    // Queueing has not created a journal. Revisit the log namespace at
    // dispatch: another finished task may have published extraction.log while
    // this selection waited in the FIFO. After this point the plan is durable.
    if job.round4_layout {
        native_extract::plan_native_program_logs(&mut job)?;
    }
    native_extract::validate_job(&job)?;
    launch_session(app, job, None, tx, start_minimized)
}

/// Called only after a reviewed, ownerless native journal is selected in the
/// existing Recovery window. Other outstanding file tasks still own their
/// serial slot; an interrupted extraction does not reserve its own UI forever.
pub(super) fn resume_from_journal(
    app: &mut AppState,
    journal_path: &Path,
    tx: &mpsc::Sender<AppMessage>,
) -> Result<(), String> {
    if app.file_transfers.active_session_id.is_some()
        || !app.file_transfers.queued.is_empty()
        || !app.file_transfers.recovery_queued.is_empty()
        || app.minimized_file_task_progress.is_some()
        || app.file_task_preempted_overlay.is_some()
    {
        return Err("Finish the other file tasks and unresolved recoveries before resuming this extraction".into());
    }
    let record = file_task_runtime::load_record(journal_path)?;
    if !record.is_native_extraction() || record.lifecycle == DurableFileTaskLifecycle::Completed
        || file_task_runtime::recovery_record_is_discard_cleanup(&record)
    {
        return Err("Selected recovery is not an unfinished native extraction".into());
    }
    let saved: DurableNativeExtractionTask = serde_json::from_value(record.job)
        .map_err(|error| format!("Decode saved extraction selection: {error}"))?;
    launch_session(app, saved.native_extraction, Some(journal_path.to_path_buf()), tx, false)
}

fn launch_session(
    app: &mut AppState,
    job: NativeExtractionJob,
    resume_path: Option<PathBuf>,
    tx: &mpsc::Sender<AppMessage>,
    start_minimized: bool,
) -> Result<(), String> {
    let progress = new_progress(app, &job);
    let (control_tx, control_rx) = std_mpsc::channel();
    let session = FileTaskProgressSession::new(progress, control_tx);
    let session_id = session.session_id;
    let task = DurableNativeExtractionTask {
        session_id,
        job_id: uuid::Uuid::new_v4().to_string(),
        generation: 1,
        dest: job.destination.to_string_lossy().into_owned(),
        native_extraction: job,
    };
    #[cfg(test)]
    if let Some(dispatches) = app.test_file_task_dispatches.as_mut() {
        dispatches.push((session_id, vec![task.native_extraction.source.clone()],
            vec![task.native_extraction.destination.clone()]));
        drop(control_rx);
        app.file_transfers.active_session_id = Some(session_id);
        app.install_file_task_progress_with_visibility(session, start_minimized);
        return Ok(());
    }
    let tx = tx.clone();
    std::thread::Builder::new()
        .name(format!("native-extraction-supervisor-{session_id}"))
        .spawn(move || supervise_detached_extraction(task, resume_path, tx, control_rx))
        .map_err(|error| format!("Start native extraction supervisor: {error}"))?;
    app.file_transfers.active_session_id = Some(session_id);
    app.install_file_task_progress_with_visibility(session, start_minimized);
    Ok(())
}

fn supervisor_failure(tx: &mpsc::Sender<AppMessage>, session_id: u64, error: String) {
    let update = FileTaskProgressUpdate::Failed {
        status: error.clone(), totals: ProgressTotals::default(),
    };
    send_to_tui(tx, session_id, AppMessage::FileTaskProgress { session_id, update });
    send_to_tui(tx, session_id,
        AppMessage::NativeExtractComplete { session_id, result: Err(error) });
}

fn send_to_tui(tx: &mpsc::Sender<AppMessage>, _session_id: u64, message: AppMessage) {
    match tx.try_send(message) {
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
        Err(mpsc::error::TrySendError::Full(message)) => {
            // Snapshots are replaced every 175 ms; don't manufacture one
            // blocked delivery thread per frame when the TUI is busy. Never
            // discard a terminal result or failure notification.
            if matches!(&message, AppMessage::FileTaskProgress {
                update: FileTaskProgressUpdate::Snapshot { .. }, ..
            }) { return; }
            let _ = tx.blocking_send(message);
        }
    }
}

fn supervise_detached_extraction(
    mut task: DurableNativeExtractionTask,
    resume_path: Option<PathBuf>,
    tx: mpsc::Sender<AppMessage>,
    controls: std_mpsc::Receiver<FileTaskControl>,
) {
    let session_id = task.session_id;
    let journal = if let Some(path) = resume_path {
        let prior = match file_task_runtime::load_record(&path) {
            Ok(record) => record,
            Err(error) => { supervisor_failure(&tx, session_id, error); return; }
        };
        if !prior.is_native_extraction() {
            supervisor_failure(&tx, session_id, "Recovery journal is not native extraction".into());
            return;
        }
        task.job_id = prior.job_id.clone();
        task.generation = prior.generation.saturating_add(1);
        let value = match serde_json::to_value(&task) {
            Ok(value) => value,
            Err(error) => { supervisor_failure(&tx, session_id, error.to_string()); return; }
        };
        FileTaskJournalHandle::resume(
            path, task.generation, session_id, false, prior.verification,
            prior.stall_timeout_secs, prior.mappings.clone(), None, value,
        )
    } else {
        let value = match serde_json::to_value(&task) {
            Ok(value) => value,
            Err(error) => { supervisor_failure(&tx, session_id, error.to_string()); return; }
        };
        FileTaskJournalHandle::create(
            task.job_id.clone(), task.generation, session_id, false,
            VerificationMode::Standard, 600,
            vec![PasteMapping {
                source: task.native_extraction.source.clone(),
                destination: task.native_extraction.destination.clone(),
            }],
            None, value,
        )
    };
    let journal = match journal {
        Ok(journal) => journal,
        Err(error) => { supervisor_failure(&tx, session_id, error); return; }
    };
    let executable = match crate::reexec::current_executable_for_reexec() {
        Ok(executable) => executable,
        Err(error) => {
            let error = format!("Locate native extraction helper: {error}");
            let _ = journal.mark_lifecycle(DurableFileTaskLifecycle::Failed, error.clone());
            supervisor_failure(&tx, session_id, error);
            return;
        }
    };
    #[cfg(unix)]
    let lease_fd = match journal.lease_fd() {
        Ok(fd) => fd,
        Err(error) => { supervisor_failure(&tx, session_id, error); return; }
    };
    let mut command = Command::new(executable);
    command.arg("__file-task-worker").arg("--journal").arg(journal.path());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.arg("--lease-fd").arg(lease_fd.to_string());
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(lease_fd, libc::F_GETFD);
                if flags < 0 { return Err(std::io::Error::last_os_error()); }
                if libc::fcntl(lease_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let detail = format!("Start detached native extraction helper: {error}");
            let _ = journal.mark_lifecycle(DurableFileTaskLifecycle::Failed, detail.clone());
            supervisor_failure(&tx, session_id, detail);
            return;
        }
    };
    let _ = journal.mark_lifecycle(DurableFileTaskLifecycle::Running, "native extraction helper started");

    let (wire_tx, wire_rx) = std_mpsc::channel::<Result<FileTaskWireEvent, String>>();
    if let Some(stdout) = child.stdout.take() {
        let _ = std::thread::Builder::new()
            .name(format!("native-extraction-events-{session_id}"))
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let value = line.map_err(|error| error.to_string())
                        .and_then(|line| serde_json::from_str::<FileTaskWireEvent>(&line)
                            .map_err(|error| error.to_string()));
                    if wire_tx.send(value).is_err() { break; }
                }
            });
    }
    if let Some(stderr) = child.stderr.take() {
        let _ = std::thread::Builder::new()
            .name(format!("native-extraction-stderr-{session_id}"))
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    log::warn!("native extraction helper {session_id}: {line}");
                }
            });
    }
    let (control_writer_tx, control_writer_rx) = std_mpsc::channel::<FileTaskControl>();
    if let Some(mut stdin) = child.stdin.take() {
        let _ = std::thread::Builder::new()
            .name(format!("native-extraction-controls-{session_id}"))
            .spawn(move || {
                while let Ok(control) = control_writer_rx.recv() {
                    if file_task_runtime::write_wire_control(&mut stdin, &control).is_err() { break; }
                }
                let _ = stdin.flush();
            });
    }
    let mut abort_since: Option<Instant> = None;
    loop {
        while let Ok(control) = controls.try_recv() {
            if matches!(control, FileTaskControl::Abort) {
                abort_since.get_or_insert_with(Instant::now);
            }
            let _ = control_writer_tx.send(control);
        }
        if abort_since.is_some_and(|since| since.elapsed() > ABORT_GRACE) {
            let _ = child.kill();
            let error = "Native extraction did not acknowledge Abort; helper was terminated, and Recovery retains its journal".to_owned();
            let _ = journal.mark_lifecycle(DurableFileTaskLifecycle::AwaitingReconciliation, error.clone());
            supervisor_failure(&tx, session_id, error);
            let _ = child.wait();
            return;
        }
        match wire_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(FileTaskWireEvent::Progress { update })) => {
                send_to_tui(&tx, session_id, AppMessage::FileTaskProgress { session_id, update });
            }
            Ok(Ok(FileTaskWireEvent::NativeComplete { result })) => {
                let _ = child.wait();
                let completed = journal.load().ok().filter(|record| record.is_native_extraction()
                    && record.lifecycle == DurableFileTaskLifecycle::Completed);
                // Both the helper and this supervisor have held the same
                // journal lease. Drop the last supervisor holder before
                // retirement, but retain the journal as a Recovery receipt.
                drop(journal);
                if let Some(record) = completed {
                    if let Err(error) = file_task_runtime::retire_completed_native_reservation(&record) {
                        log::warn!("Native extraction completed but reservation retirement is pending: {error}");
                    }
                }
                send_to_tui(&tx, session_id, AppMessage::NativeExtractComplete { session_id, result });
                return;
            }
            Ok(Ok(FileTaskWireEvent::Complete { .. })) => {
                log::warn!("native extraction helper sent copy/move completion");
            }
            Ok(Err(error)) => log::warn!("native extraction helper protocol: {error}"),
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
            Err(std_mpsc::RecvTimeoutError::Disconnected) => {}
        }
        if let Ok(Some(status)) = child.try_wait() {
            // The kernel can reap the helper before its pipe-reader thread
            // parses the final newline. Give that thread a bounded chance to
            // forward the already-written terminal event.
            if let Ok(Ok(FileTaskWireEvent::NativeComplete { result })) =
                wire_rx.recv_timeout(Duration::from_millis(200))
            {
                let completed = journal.load().ok().filter(|record| record.is_native_extraction()
                    && record.lifecycle == DurableFileTaskLifecycle::Completed);
                drop(journal);
                if let Some(record) = completed {
                    if let Err(error) = file_task_runtime::retire_completed_native_reservation(&record) {
                        log::warn!("Native completion reservation retirement pending: {error}");
                    }
                }
                send_to_tui(&tx, session_id, AppMessage::NativeExtractComplete { session_id, result });
                return;
            }
            let error = format!("Native extraction helper exited without a terminal result ({status}); inspect Recovery");
            let _ = journal.mark_lifecycle(DurableFileTaskLifecycle::AwaitingReconciliation, error.clone());
            supervisor_failure(&tx, session_id, error);
            return;
        }
    }
}

/// The existing hidden `__file-task-worker` subcommand selects this branch
/// from the journal kind, retaining the inherited OFD lease and the same
/// restart-observable progress/control mailbox as copy and move.
pub(super) fn run_detached_file_task_helper(
    journal: FileTaskJournalHandle,
    record: DurableFileTaskRecord,
) -> Result<(), String> {
    let task: DurableNativeExtractionTask = serde_json::from_value(record.job.clone())
        .map_err(|error| format!("Decode durable native extraction: {error}"))?;
    if task.job_id != journal.job_id() || task.generation != journal.generation()
        || task.session_id != record.session_id
    {
        return Err("Native extraction helper journal identity mismatch".into());
    }
    let (control_tx, control_rx) = std_mpsc::channel();
    let live = super::file_task_live::LiveWorkerRef::new(
        journal.path().to_path_buf(), task.job_id.clone(), task.generation,
    )?;
    let (detached, _detached_guard) = match super::file_task_live::start_worker(live, control_tx.clone()) {
        Ok((sender, guard)) => (Some(sender), Some(guard)),
        Err(error) => {
            log::warn!("native extraction live observation unavailable: {error}");
            (None, None)
        }
    };
    let _ = std::thread::Builder::new()
        .name(format!("native-helper-stdin-{}", task.session_id))
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break; };
                if let Ok(control) = serde_json::from_str::<FileTaskControl>(&line) {
                    if control_tx.send(control).is_err() { break; }
                }
            }
        });
    let sink = NativeWireSink {
        writer: Arc::new(Mutex::new(std::io::BufWriter::new(std::io::stdout()))),
        detached,
    };
    journal.mark_lifecycle(DurableFileTaskLifecycle::Running, "native extraction executing")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all().worker_threads(2).build()
        .map_err(|error| format!("Start native extraction worker runtime: {error}"))?;
    runtime.block_on(run_native_extraction(task, record, journal, sink, control_rx));
    Ok(())
}

struct NativeWireSink {
    writer: Arc<Mutex<std::io::BufWriter<std::io::Stdout>>>,
    detached: Option<super::file_task_live::LiveProgressSender>,
}

impl NativeWireSink {
    fn progress(&self, update: FileTaskProgressUpdate) {
        if let Some(detached) = &self.detached { detached.publish(&update); }
        // The original TUI can close its pipe without cancelling the helper.
        if let Ok(mut output) = self.writer.lock() {
            let _ = file_task_runtime::write_wire_event(&mut *output, &FileTaskWireEvent::Progress { update });
        }
    }
    fn complete(&self, result: Result<NativeExtractionReport, String>) {
        if let Ok(mut output) = self.writer.lock() {
            let _ = file_task_runtime::write_wire_event(&mut *output, &FileTaskWireEvent::NativeComplete { result });
        }
    }
}

/// A terminated helper can publish a complete file in the instant before
/// its next durable journal append. For an explicit Replace job, refuse to
/// replay any unrecorded existing output rather than silently replacing it.
/// This ambiguity is presented in Recovery for a deliberate user decision.
fn refuse_ambiguous_recovery_replacement(
    job: &NativeExtractionJob, published: &[PathBuf], owned_logs: &HashSet<PathBuf>,
) -> Result<(), String> {
    if job.overwrite_policy != native_extract::NativeOverwritePolicy::Replace { return Ok(()); }
    let recorded = published.iter().collect::<HashSet<_>>();
    let mut planned = Vec::new();
    for program in &job.programs {
        if job.file_per_program {
            let audio = native_extract::proposed_destination_for_program(job, program);
            if job.cue { planned.push(audio.with_extension("cue")); }
            planned.push(audio);
        } else {
            for track in &program.tracks {
                planned.push(native_extract::proposed_destination(job, program, track));
            }
        }
    }
    if job.extraction_log {
        planned.extend(native_extract::native_log_targets(job)
            .into_iter().filter(|path| !owned_logs.contains(path)));
    }
    for path in planned {
        if !recorded.contains(&path) && std::fs::symlink_metadata(&path).is_ok() {
            return Err(format!(
                "Unrecorded existing output {} may have been published before the helper stopped; review it before resuming a Replace extraction",
                path.display(),
            ));
        }
    }
    Ok(())
}

fn is_skipped_selection(
    program: &native_extract::NativeProgramChoice, number: u32,
    skipped: &[NativeSkippedSelection],
) -> bool {
    skipped.iter().any(|selection|selection.same_selection(&NativeSkippedSelection::program(program))
        || selection.same_selection(&NativeSkippedSelection::track(program, number)))
}

/// Recovery filters work, not the original authored selection. Confirmed
/// skips remain skipped; a continuous audio file with a missing CUE is already
/// finished audio, provided its exact CUE was journaled before publication.
fn remaining_native_job(
    mut job: NativeExtractionJob, published: &[PathBuf],
    skipped: &[NativeSkippedSelection], prepared_cues: &[DurableNativePreparedCue],
) -> Result<NativeExtractionJob, String> {
    let done = published.iter().cloned().collect::<HashSet<_>>();
    let original_job = job.clone();
    if job.file_per_program {
        for program in &mut job.programs {
            let audio = native_extract::proposed_destination_for_program(&original_job, program);
            if done.contains(&audio) {
                if original_job.cue && !done.contains(&audio.with_extension("cue"))
                    && !prepared_cues.iter().any(|cue|cue.destination == audio.with_extension("cue")) {
                    return Err(format!(
                        "Published continuous audio {} has no durable CUE contents; review it before recovery",
                        audio.display(),
                    ));
                }
                program.tracks.clear();
            } else {
                let original_program = program.clone();
                program.tracks.retain(|track|!is_skipped_selection(
                    &original_program, track.number, skipped));
            }
        }
        job.programs.retain(|program| !program.tracks.is_empty());
    } else {
        for program in &mut job.programs {
            let original_program = program.clone();
            program.tracks.retain(|track|
                !done.contains(&native_extract::proposed_destination(&original_job, &original_program, track))
                    && !is_skipped_selection(&original_program, track.number, skipped));
        }
        job.programs.retain(|program| !program.tracks.is_empty());
    }
    for path in published {
        if !path.is_file() {
            return Err(format!("Previously published extraction output is missing: {}. Resolve in Recovery rather than silently re-extracting",path.display()));
        }
    }
    Ok(job)
}

#[derive(Debug, Clone, Copy)]
struct NativeRecoveryBaseline {
    total: usize,
    estimated_total: Option<u64>,
    done: usize,
    skipped: usize,
    published_files: usize,
    completed_bytes: u64,
    skipped_bytes: u64,
}

/// The original preflight remains the denominator after Recovery removes
/// already decided selections. CUEs and logs are published files, but do not
/// count as additional selected audio items or estimated audio bytes.
fn native_recovery_baseline(
    job: &NativeExtractionJob, published: &[PathBuf], skipped: &[NativeSkippedSelection],
) -> NativeRecoveryBaseline {
    let mut base = NativeRecoveryBaseline {
        total: 0,
        estimated_total: job.programs.iter().flat_map(|p|p.tracks.iter())
            .try_fold(0u64, |sum, track|sum.checked_add(track.estimated_bytes?)),
        done: 0, skipped: 0, published_files: published.len(),
        completed_bytes: 0, skipped_bytes: 0,
    };
    for program in &job.programs {
        let program_published = job.file_per_program && published.contains(
            &native_extract::proposed_destination_for_program(job, program));
        for track in &program.tracks {
            base.total += 1;
            let skipped = is_skipped_selection(program, track.number, skipped);
            let completed = program_published || (!job.file_per_program && published.contains(
                &native_extract::proposed_destination(job, program, track)));
            if skipped {
                base.done += 1;
                base.skipped += 1;
                base.skipped_bytes = base.skipped_bytes.saturating_add(track.estimated_bytes.unwrap_or(0));
            } else if completed {
                base.done += 1;
                base.completed_bytes = base.completed_bytes.saturating_add(track.estimated_bytes.unwrap_or(0));
            }
        }
    }
    base
}

fn add_native_recovery_baseline(
    mut progress: NativeExtractionProgress, base: NativeRecoveryBaseline,
) -> NativeExtractionProgress {
    progress.done += base.done;
    progress.total = base.total;
    progress.skipped += base.skipped;
    progress.published += base.published_files;
    progress.estimated_bytes_done = progress.estimated_bytes_done
        .saturating_add(base.completed_bytes).saturating_add(base.skipped_bytes);
    progress.estimated_bytes_published = progress.estimated_bytes_published.saturating_add(base.completed_bytes);
    progress.estimated_bytes_skipped = progress.estimated_bytes_skipped.saturating_add(base.skipped_bytes);
    progress.estimated_bytes_total = base.estimated_total;
    progress
}

fn pending_native_cues(
    job: &NativeExtractionJob, published: &[PathBuf],
    prepared: &[DurableNativePreparedCue],
) -> Result<Vec<DurableNativePreparedCue>, String> {
    if !job.file_per_program || !job.cue { return Ok(Vec::new()); }
    let mut pending = Vec::new();
    for program in &job.programs {
        let audio = native_extract::proposed_destination_for_program(job, program);
        let cue_path = audio.with_extension("cue");
        if published.contains(&audio) && !published.contains(&cue_path) {
            let cue = prepared.iter().find(|cue|cue.destination == cue_path)
                .ok_or_else(||format!("No prepared CUE in recovery journal for {}",audio.display()))?;
            pending.push(cue.clone());
        }
    }
    Ok(pending)
}

async fn run_native_extraction(
    task: DurableNativeExtractionTask,
    record: DurableFileTaskRecord,
    journal: FileTaskJournalHandle,
    sink: NativeWireSink,
    control_rx: std_mpsc::Receiver<FileTaskControl>,
) {
    let cancel = CancellationToken::new();
    let controls = Arc::new(NativeExtractionControl::default());
    let errors = Arc::new(Mutex::new(None::<String>));
    let hook_journal = journal.clone();
    let skip_journal = journal.clone();
    let cue_journal = journal.clone();
    let unfinished_journal = journal.clone();
    let hook_generation = task.generation;
    controls.set_publication_hook(native_extract::NativePublicationHook {
        job_id: task.job_id.clone(), generation: task.generation,
        notify: Arc::new(move |temporary, destination, beginning| {
            if beginning {
                hook_journal.record_recovered_temp_artifact(
                    temporary, destination,
                    file_task_runtime::DurableTempArtifactKind::File,
                    hook_generation,
                )
            } else {
                hook_journal.clear_recovered_temp_artifact(temporary)
            }
        }),
        skipped: Arc::new(move |selection| skip_journal.record_native_skipped(selection)),
        prepared_cue: Arc::new(move |destination, text|
            cue_journal.record_native_prepared_cue(destination, text)),
        unfinished: Arc::new(move |output|
            unfinished_journal.record_native_unfinished(output)),
    });
    let mut terminal_log_job = task.native_extraction.clone();
    if let Some(mapping) = record.admitted_mappings.first() {
        terminal_log_job.source = mapping.source.clone();
    }
    if let Some(root) = &record.admitted_destination_root {
        if let Err(error) = native_extract::admit_native_destination(&mut terminal_log_job,root.clone()) {
            let _ = journal.mark_lifecycle(DurableFileTaskLifecycle::Failed,error.clone());
            sink.complete(Err(error));
            return;
        }
    }
    let mut final_totals = ProgressTotals::default();
    let result = async {
        // An abandoned generation may have left a partial destination-side
        // cross-device copy. Remove only exact, journal-owned private paths.
        clean_native_partial_publications(&journal, &record)?;
        let mut job = task.native_extraction;
        let admitted = record.admitted_mappings.first()
            .ok_or("Native extraction journal has no admitted source")?;
        job.source = admitted.source.clone();
        native_extract::admit_native_destination(&mut job,
            record.admitted_destination_root.clone()
                .ok_or("Native extraction journal has no admitted destination")?)?;
        if record.generation > 1 {
            let mut owned_logs=record.native_log_digests.keys().cloned().collect::<HashSet<_>>();
            if record.native_log_sha256.is_some() && job.program_log_paths.is_empty() {
                owned_logs.insert(job.destination.join(native_extract::EXTRACTION_LOG_FILE));
            }
            refuse_ambiguous_recovery_replacement(&job,&record.native_published_paths,&owned_logs)?;
        }
        let original_job = job.clone();
        // Reject a conflicting/unowned log before any resumed audio work.
        // Direct runs and the UI enforce the same policy in validate_job.
        if original_job.extraction_log {
            verify_native_log_ownership(&original_job, &record)?;
        }
        let baseline = native_recovery_baseline(&original_job,
            &record.native_published_paths, &record.native_skipped_selections);
        let pending_cues = pending_native_cues(&original_job,
            &record.native_published_paths, &record.native_prepared_cues)?;
        job = remaining_native_job(job, &record.native_published_paths,
            &record.native_skipped_selections, &record.native_prepared_cues)?;
        let log_targets = native_extract::native_log_targets(&original_job);
        // An earlier cancelled/failed run may already have left its own
        // receipt. Recovery replaces that exact journal-owned log after all
        // outstanding media/CUEs are handled, never a user-created file.
        let log_needed = original_job.extraction_log;
        // The detached helper publishes the log *after* all pending recovered
        // CUEs. The media engine otherwise keeps its normal direct-call API.
        job.extraction_log = false;
        let total = job.programs.iter().map(|p| p.tracks.len()).sum::<usize>();
        let estimated_bytes_total = job.programs.iter().flat_map(|p| p.tracks.iter())
            .try_fold(0u64, |sum, track| sum.checked_add(track.estimated_bytes?));
        let initial = add_native_recovery_baseline(NativeExtractionProgress {
            done: 0, total, published: 0, published_paths: vec![], skipped: 0,
            current: if total == 0 { "Finishing requested extraction artifacts".into() }
                else { "Preparing native extraction".into() },
            current_file: None, current_estimated_bytes: None,
            estimated_bytes_done: 0, estimated_bytes_published: 0,
            estimated_bytes_skipped: 0, estimated_bytes_total,
        }, baseline);
        let (progress_tx, progress_rx) = watch::channel(initial);
        let journal_for_progress = journal.clone();
        let journal_for_sidecars = journal.clone();
        let errors_for_progress = Arc::clone(&errors);
        let worker_cancel = cancel.clone();
        let worker_controls = Arc::clone(&controls);
        let mut known_published = record.native_published_paths.iter().cloned().collect::<HashSet<_>>();
        let prior_published = record.native_published_paths.clone();
        let prior_skipped = record.native_skipped_selections.clone();
        let prior_unfinished = record.native_unfinished_outputs.clone();
        let continuous = job.file_per_program;
        let mut worker = tokio::spawn(async move {
            let media_progress_tx = progress_tx.clone();
            let media_cancel = worker_cancel.clone();
            let mut report = if job.programs.is_empty() {
                NativeExtractionReport { published: Vec::new(), skipped: 0 }
            } else {
                native_extract::execute_with_controls(job, worker_cancel.clone(), Arc::clone(&worker_controls),
                move |snapshot| {
                    for path in &snapshot.published_paths {
                        if known_published.insert(path.clone()) {
                            if let Err(error) = journal_for_progress.record_native_published(path) {
                                *errors_for_progress.lock().unwrap_or_else(|e|e.into_inner()) =
                                    Some(format!("Persist completed output {}: {error}",path.display()));
                                media_cancel.cancel();
                                break;
                            }
                        }
                    }
                    media_progress_tx.send_replace(add_native_recovery_baseline(snapshot, baseline));
                }).await?
            };
            // Sidecar-only recovery is legitimate: the audio is already
            // complete and must not be decoded or replaced for a missing CUE
            // or extraction log. All publications use the same checked path.
            for cue in pending_cues {
                let mut state = (*progress_tx.borrow()).clone();
                state.current = format!("Publishing recovered CUE {}", cue.destination.display());
                state.current_file = None;
                progress_tx.send_replace(state);
                if let Err(error) = native_extract::publish_native_recovery_sidecar(
                    &original_job, &cue.destination, &cue.content,
                    &worker_cancel, &worker_controls,
                ) {
                    let failure = native_extract::NativeUnfinishedOutput {
                        destination: cue.destination.clone(), reason: error.clone(),
                        cancelled: worker_cancel.is_cancelled(),
                    };
                    journal_for_sidecars.record_native_unfinished(&failure)
                        .map_err(|e| format!("{error}; cannot record failed CUE: {e}"))?;
                    return Err(error);
                }
                journal_for_sidecars.record_native_published(&cue.destination)?;
                let mut state = (*progress_tx.borrow()).clone();
                state.current = "Published recovered CUE".into();
                state.published += 1;
                progress_tx.send_replace(state);
                report.published.push(cue.destination);
            }
            if log_needed {
                let mut all_published = prior_published;
                all_published.extend(report.published.iter().cloned());
                let mut all_skipped = prior_skipped;
                all_skipped.extend(worker_controls.skipped_receipts());
                let mut all_unfinished = prior_unfinished;
                all_unfinished.extend(worker_controls.unfinished_receipts());
                for (index, log_target) in log_targets.into_iter().enumerate() {
                    let content = native_extract::native_log_content_for_index(&original_job,index,
                        &all_published, &all_skipped, &all_unfinished, NativeLogOutcome::Complete, None);
                    let mut state = (*progress_tx.borrow()).clone();
                    state.current = "Publishing extraction log".into();
                    state.current_file = None;
                    progress_tx.send_replace(state);
                    publish_journaled_native_log_at_path(&original_job, &journal_for_sidecars,
                        &worker_controls, &log_target, &content, &worker_cancel)?;
                    let mut state = (*progress_tx.borrow()).clone();
                    state.current = "Published extraction log".into();
                    state.published += 1;
                    progress_tx.send_replace(state);
                    report.published.push(log_target);
                }
            }
            report.skipped += baseline.skipped;
            Ok::<NativeExtractionReport,String>(report)
        });
        let mut ticker = tokio::time::interval(SAMPLING_INTERVAL);
        let mut last_sample: Option<(NativeWatchSource, String, usize, u8, u64, Instant)> = None;
        let mut last_display: Option<(usize, usize, String, u64, FileTaskPhase)> = None;
        let mut latest_totals = totals_from(&progress_rx.borrow(), 0);
        let mut byte_high_water = 0u64;
        let result = loop {
            tokio::select! {
                joined = &mut worker => {
                    break joined.unwrap_or_else(|error| Err(format!("Native extraction task: {error}")));
                }
                _ = ticker.tick() => {
                    while let Ok(control) = control_rx.try_recv() {
                        match control {
                            FileTaskControl::Pause => controls.pause(),
                            FileTaskControl::Resume => controls.resume(),
                            FileTaskControl::SkipCurrent => controls.skip_current(),
                            FileTaskControl::Abort => cancel.cancel(),
                            FileTaskControl::ConflictResolution { .. } => {}
                        }
                    }
                    let progress = (*progress_rx.borrow()).clone();
                    let watch = controls.watch_snapshot();
                    let source = watch.as_ref().map(|watch| watch.source.clone())
                        .or_else(|| progress.current_file.clone().map(NativeWatchSource::File));
                    let reported = watch.as_ref().map(NativeWriteWatch::measured_bytes)
                        .unwrap_or_else(|| progress.current_file.as_ref()
                            .and_then(|p| std::fs::metadata(p).ok()).map_or(0, |m|m.len()));
                    let label = watch.as_ref().map_or_else(||progress.current.clone(),|w|w.label.clone());
                    let stage = watch.as_ref().map_or(0,|w|w.fraction_start);
                    let same_phase = last_sample.as_ref().is_some_and(|(old_source, old_label, old_done, old_stage, _, _)| {
                        source.as_ref() == Some(old_source) && *old_label == label
                            && *old_done == progress.done && *old_stage == stage
                    });
                    // A temporary spool may be renamed or removed during a
                    // stage. Its completed work must not make progress go back.
                    let measured = if same_phase {
                        reported.max(last_sample.as_ref().map_or(0,|old|old.4))
                    } else { reported };
                    let now = Instant::now();
                    let rate = match &last_sample {
                        Some((_,_,_,_, old_bytes, time)) if same_phase && measured >= *old_bytes => {
                            let seconds = now.duration_since(*time).as_secs_f64();
                            (seconds > 0.0).then(|| ((measured - *old_bytes) as f64 / seconds) as u64)
                        }
                        _ => None,
                    };
                    last_sample = source.map(|path|(path,label,progress.done,stage,measured,now));
                    let phase = if cancel.is_cancelled() { FileTaskPhase::Cancelling }
                        else if controls.is_pause_acknowledged() { FileTaskPhase::Paused }
                        else { FileTaskPhase::Running };
                    latest_totals = totals_for_native_watch(&progress, watch.as_ref(), measured, continuous);
                    // The private working file can disappear on a phase
                    // transition. The *job* has not undone previously written
                    // bytes, so its progress bar must not jump backwards.
                    byte_high_water = byte_high_water.max(latest_totals.bytes_done);
                    latest_totals.bytes_done = byte_high_water;
                    let status = watch.as_ref().map_or_else(|| progress.current.clone(), |w|w.label.clone());
                    let fingerprint = (progress.done, progress.skipped, status.clone(),
                        latest_totals.bytes_done, phase.clone());
                    if last_display.as_ref() != Some(&fingerprint) || rate.is_some_and(|n|n>0) {
                        let item_estimate = watch.as_ref().and_then(|w|w.estimated_bytes)
                            .or(progress.current_estimated_bytes);
                        sink.progress(FileTaskProgressUpdate::Snapshot {
                            phase,
                            status: if controls.is_paused() && !controls.is_pause_acknowledged() {
                                format!("Pausing · {status}")
                            } else { status.clone() },
                            current_item: watch.is_some().then(||ProgressItem {
                                label: status.clone(), source: None, destination: None,
                                bytes_done: measured.min(item_estimate.unwrap_or(u64::MAX)),
                                bytes_total: item_estimate,
                            }).or_else(|| progress.current_file.as_ref().map(|_|ProgressItem {
                                label: status.clone(), source: None, destination: None,
                                bytes_done: measured.min(item_estimate.unwrap_or(u64::MAX)),
                                bytes_total: item_estimate,
                            })),
                            totals: latest_totals,
                            rate_bytes_per_sec: rate,
                        });
                        last_display = Some(fingerprint);
                    }
                }
            }
        };
        final_totals = latest_totals;
        let last_progress = (*progress_rx.borrow()).clone();
        if result.is_ok() {
            final_totals.items_done = last_progress.done as u64;
            final_totals.completed = last_progress.done.saturating_sub(last_progress.skipped) as u64;
            final_totals.skipped = last_progress.skipped as u64;
            final_totals.bytes_done = last_progress.estimated_bytes_total.map_or(
                final_totals.bytes_done,
                |total| total.saturating_sub(last_progress.estimated_bytes_skipped),
            );
        }
        if let Some(error) = errors.lock().unwrap_or_else(|e|e.into_inner()).take() {
            return Err(error);
        }
        let mut result = result?;
        for path in &result.published {
            if !known_published_path(&journal, path)? {
                journal.record_native_published(path)?;
            }
        }
        let mut combined = record.native_published_paths.clone();
        for path in result.published.drain(..) {
            if !combined.contains(&path) { combined.push(path); }
        }
        result.published = combined;
        Ok::<NativeExtractionReport,String>(result)
    }.await;
    let mut result = result;
    if let Err(error) = &mut result {
        // The media worker has stopped. The regular cancellation token must
        // not suppress a terminal receipt for files already published.
        if terminal_log_job.extraction_log && (controls.execution_has_started()
            || !record.native_published_paths.is_empty()) {
            let outcome = if cancel.is_cancelled() { NativeLogOutcome::Cancelled }
                else { NativeLogOutcome::Failed };
            let saved_error = error.clone();
            let log_result = (|| {
                let latest = journal.load()?;
                controls.resume(); // a paused worker must not keep a terminal log paused
                for (index, target) in native_extract::native_log_targets(&terminal_log_job)
                    .into_iter().enumerate() {
                    let body = native_extract::native_log_content_for_index(&terminal_log_job,index,
                        &latest.native_published_paths, &latest.native_skipped_selections,
                        &latest.native_unfinished_outputs, outcome, Some(&saved_error));
                    publish_journaled_native_log_at_path(&terminal_log_job, &journal, &controls,
                        &target, &body, &CancellationToken::new())?;
                }
                Ok::<(),String>(())
            })();
            if let Err(log_error) = log_result {
                error.push_str(&format!("; extraction log could not be written: {log_error}"));
            }
        }
    }
    let totals = final_totals;
    let lifecycle = if result.is_ok() { DurableFileTaskLifecycle::Completed }
        else { DurableFileTaskLifecycle::AwaitingReconciliation };
    let status = match &result {
        Ok(report) => format!("Extraction completed: {} files published; {} tracks skipped",
            report.published.len(),report.skipped),
        Err(error) => format!("Extraction interrupted: {error}"),
    };
    let _ = journal.mark_lifecycle(lifecycle, status.clone());
    let terminal = match &result {
        Ok(_) => FileTaskProgressUpdate::Finished { status, totals },
        Err(_) if cancel.is_cancelled() => FileTaskProgressUpdate::Aborted { status, totals },
        Err(_) => FileTaskProgressUpdate::Failed { status, totals },
    };
    sink.progress(terminal);
    sink.complete(result);
}

/// Only a previously journaled receipt with unchanged bytes can be
/// replaced on Recovery. The initial run may replace an unrelated log only
/// when the user explicitly selected Replace before starting extraction.
/// A receipt may only be overwritten when its exact bytes were durably
/// recorded by this task. Separate proof per program prevents a valid receipt
/// for one area from authorizing a replacement in a different area.
fn verify_native_log_target_ownership(
    job: &NativeExtractionJob, target: &Path, record: &DurableFileTaskRecord,
) -> Result<bool, String> {
    if !native_extract::native_log_targets(job).iter().any(|path| path == target) {
        return Err(format!("Log target is not in the queued plan: {}",target.display()));
    }
    let expected = if job.program_log_paths.is_empty() {
        record.native_log_sha256.as_ref()
    } else { record.native_log_digests.get(target) };
    match std::fs::symlink_metadata(target) {
        Ok(meta) => {
            if let Some(expected) = expected {
                if !meta.is_file() || meta.file_type().is_symlink() {
                    return Err(format!("Previously journal-owned extraction log changed type: {}", target.display()));
                }
                let mut file = std::fs::File::open(target)
                    .map_err(|e|format!("Inspect journal-owned extraction log: {e}"))?;
                let mut digest = Sha256::new();
                let mut buffer = [0u8; 65536];
                loop {
                    let count = file.read(&mut buffer)
                        .map_err(|e|format!("Read journal-owned extraction log: {e}"))?;
                    if count == 0 { break; }
                    digest.update(&buffer[..count]);
                }
                if format!("{:x}", digest.finalize()) != *expected {
                    return Err(format!("Extraction log was changed outside this task: {}; Recovery will not overwrite it", target.display()));
                }
                Ok(true)
            } else if record.generation != 1
                || job.overwrite_policy != native_extract::NativeOverwritePolicy::Replace {
                Err(format!("Existing extraction log is not owned by this task: {}; choose a different destination or resolve it in Recovery",target.display()))
            } else { Ok(false) }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("Inspect destination extraction log: {e}")),
    }
}

fn verify_native_log_ownership(
    job: &NativeExtractionJob, record: &DurableFileTaskRecord,
) -> Result<bool, String> {
    let mut all_owned = true;
    for target in native_extract::native_log_targets(job) {
        all_owned &= verify_native_log_target_ownership(job,&target,record)?;
    }
    Ok(all_owned)
}

fn publish_journaled_native_log_at_path(
    job: &NativeExtractionJob, journal: &FileTaskJournalHandle,
    controls: &NativeExtractionControl, target: &Path, content: &str,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let record = journal.load()?;
    let mut publishing_job = job.clone();
    if verify_native_log_target_ownership(job, target, &record)? {
        publishing_job.overwrite_policy = native_extract::NativeOverwritePolicy::Replace;
    }
    native_extract::publish_native_recovery_sidecar(&publishing_job,
        target, content, cancel, controls)?;
    // Record the ownership digest before completion. A crash between the
    // atomic rename and the journal append remains an explicit conflict.
    let digest = format!("{:x}", Sha256::digest(content.as_bytes()));
    if job.program_log_paths.is_empty() {
        journal.record_native_log_sha256(&digest)?;
    } else {
        journal.record_native_program_log(target,&digest)?;
    }
    if !record.native_published_paths.iter().any(|path| path == target) {
        journal.record_native_published(target)?;
    }
    Ok(())
}

/// Compatibility entrypoint for the old single-log test and journal route.
#[cfg(test)]
fn publish_journaled_native_log(
    job: &NativeExtractionJob, journal: &FileTaskJournalHandle,
    controls: &NativeExtractionControl, content: &str, cancel: &CancellationToken,
) -> Result<(), String> {
    let target = job.destination.join(native_extract::EXTRACTION_LOG_FILE);
    publish_journaled_native_log_at_path(job,journal,controls,&target,content,cancel)
}

fn known_published_path(journal: &FileTaskJournalHandle, path: &Path) -> Result<bool,String> {
    Ok(journal.load()?.native_published_paths.iter().any(|known|known == path))
}

/// Only remove a recorded hidden cross-device staging artifact. Do not scan
/// destinations by extension and do not touch a published output or a symlink.
pub(super) fn clean_native_partial_publications(
    journal: &FileTaskJournalHandle,
    record: &DurableFileTaskRecord,
) -> Result<(), String> {
    for artifact in &record.temp_artifacts {
        if !artifact.is_safe_private_artifact(&record.job_id) ||
            artifact.kind != file_task_runtime::DurableTempArtifactKind::File
        {
            return Err(format!("Recovery refused an unrecognized staged output: {}",artifact.path.display()));
        }
        if let Some(root) = &record.admitted_destination_root {
            if !artifact.path.starts_with(root) {
                return Err(format!("Recovery artifact is outside the admitted destination: {}",artifact.path.display()));
            }
            // Check the *resolved parent* before deleting a file. A changed
            // destination symlink must not redirect recovery cleanup outside
            // the originally admitted path subtree.
            let admitted = std::fs::canonicalize(root)
                .map_err(|e|format!("Resolve admitted destination for cleanup: {e}"))?;
            let parent = std::fs::canonicalize(artifact.path.parent().ok_or("No artifact parent")?)
                .map_err(|e|format!("Resolve recovery artifact parent: {e}"))?;
            if !parent.starts_with(&admitted) {
                return Err(format!("Recovery artifact parent changed outside destination: {}",artifact.path.display()));
            }
        }
        match std::fs::symlink_metadata(&artifact.path) {
            Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() =>
                std::fs::remove_file(&artifact.path).map_err(|error|error.to_string())?,
            Ok(_) => return Err(format!("Recovery artifact changed type: {}",artifact.path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(format!("Inspect recorded recovery artifact: {error}")),
        }
        journal.clear_recovered_temp_artifact(&artifact.path)?;
    }
    Ok(())
}

fn totals_from(progress: &NativeExtractionProgress, current_written: u64) -> ProgressTotals {
    totals_for_native_watch(progress, None, current_written, false)
}

/// Keep the byte estimate honest across intermediate writes, joins and
/// destination-side publication. Each phase consumes a bounded portion of
/// the preflight's estimated *final* bytes. The transfer rate, by contrast,
/// always measures the active file/tree's actual byte growth.
fn totals_for_native_watch(
    progress: &NativeExtractionProgress,
    watch: Option<&NativeWriteWatch>,
    measured: u64, continuous: bool,
) -> ProgressTotals {
    let complete_bytes = progress.estimated_bytes_published;
    let staged_bytes = progress.estimated_bytes_done
        .saturating_sub(progress.estimated_bytes_skipped)
        .saturating_sub(complete_bytes);
    let (start, end, active_estimate) = match watch {
        Some(watch) => (watch.fraction_start,watch.fraction_end,
            watch.estimated_bytes.or(progress.current_estimated_bytes)),
        None => (0, 85, progress.current_estimated_bytes),
    };
    let phase_bytes = active_estimate.filter(|size|*size != 0).map(|estimate| {
        let portion = (u128::from(estimate) * u128::from(end.saturating_sub(start))) / 100;
        ((portion * u128::from(measured.min(estimate))) / u128::from(estimate)) as u64
    }).unwrap_or(measured);
    let bytes_done = if continuous {
        let staged_share = (u128::from(staged_bytes) * u128::from(
            if start < 75 { 75 } else { start }
        ) / 100) as u64;
        // Track stages use 0..85 of their own output. In continuous mode
        // they must fit inside the program's 0..75 staging allocation.
        let phase_share = if start < 75 {
            (u128::from(phase_bytes) * 75 / 85) as u64
        } else { phase_bytes };
        complete_bytes.saturating_add(staged_share).saturating_add(phase_share)
    } else {
        complete_bytes.saturating_add(
            (u128::from(active_estimate.unwrap_or(0)) * u128::from(start) / 100) as u64
        ).saturating_add(phase_bytes)
    };
    let bytes_done = match progress.estimated_bytes_total {
        Some(total) if watch.is_some() => bytes_done.min(total.saturating_sub(1)),
        Some(total) => bytes_done.min(total),
        None => bytes_done,
    };
    // A continuous program is still being assembled when all source tracks
    // have been staged. Do not show the whole job completed before publish.
    let unfinished_continuous = continuous && progress.done == progress.total
        && (progress.published == 0 || watch.is_some_and(|watch|watch.finalizing));
    let items_done = if unfinished_continuous {
        (progress.done as u64).saturating_sub(1)
    } else { progress.done as u64 };
    ProgressTotals {
        items_done,
        items_total: Some(progress.total as u64),
        item_unit: ProgressUnit::Items,
        bytes_done,
        bytes_total: progress.estimated_bytes_total,
        completed: (progress.done.saturating_sub(progress.skipped) as u64).min(items_done),
        skipped: progress.skipped as u64,
        ..ProgressTotals::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recovery_fixture_job(destination: &Path) -> NativeExtractionJob {
        use crate::convert::pipeline::native_extract::{
            NativeOverwritePolicy, NativeProgramChoice, NativeTagSource,
            NativeTagValues, NativeTrackChoice, NativeWavContainer,
        };
        NativeExtractionJob { disc_identity: Default::default(),
            source: std::env::current_exe().unwrap(),
            format: crate::disc::DiscFormat::Sacd,
            programs: vec![NativeProgramChoice {
                id: crate::disc::PresentationId::SacdArea(crate::disc::SacdAreaId::Stereo),
                angle: 1, label: "Stereo".into(),
                tracks: (1..=2).map(|number| NativeTrackChoice { duration_secs: None,
                    number, name: format!("Track {number}"),
                    tags: NativeTagValues::default(), estimated_bytes: Some(10_000),
                }).collect(),
                keep_dst: false, source_depth: Some(1), source_channels: Some(2),
                source_rate: Some(2_822_400), source_layout: None, is_lossless: true,
                tags: NativeTagValues::default(), authored_tags: None,
            }],
            destination: destination.to_path_buf(),
            working_root: destination.join("private-work"),
            wav_container: NativeWavContainer::Automatic,
            assume_decrypted: false, downmix: crate::convert::pipeline::types::DvdaDownmixPolicy::None,
            naming_template: "{track} - {title}".into(),
            program_subfolders: true, overwrite_policy: NativeOverwritePolicy::Replace,
            extraction_log: false, program_log_paths: Vec::new(), round4_layout: false, file_per_program: false, cue: false,
            transfer_tags: false, sacd_dff: false, tag_source: NativeTagSource::DiscAndSidecar,
            tag_fields: 0x7f, tag_artwork: false,
        }
    }

    #[test]
    fn native_extractions_share_fifo_order_with_copy_and_can_be_cancelled_while_queued() {
        use crate::config::TonepoetConfig;
        use tui_file_picker::{FilePickerClipboardMode, FilesystemClipboard};
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("destination");
        std::fs::create_dir_all(&dest).unwrap();
        let first = temp.path().join("first.flac");
        let last = temp.path().join("last.flac");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&last, b"last").unwrap();
        let clipboard = |path: &Path| FilesystemClipboard::new(
            FilePickerClipboardMode::Copy, vec![path.to_path_buf()],
        ).unwrap();

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.test_file_task_dispatches = Some(Vec::new());
        app.file_transfers.active_session_id = Some(9001);
        app.file_transfers.keep_minimized_across_jobs = true;
        let (tx, _rx) = mpsc::channel(16);
        super::super::keybindings::start_filesystem_clipboard_paste(
            &mut app, clipboard(&first), dest.clone(), &tx);
        let native = recovery_fixture_job(&dest);
        let source = native.source.clone();
        launch(&mut app, native, &tx).unwrap();
        super::super::keybindings::start_filesystem_clipboard_paste(
            &mut app, clipboard(&last), dest.clone(), &tx);

        let order = app.file_transfers.queued.iter().map(|entry| entry.queue_id())
            .collect::<Vec<_>>();
        assert_eq!(order.len(), 3);
        assert!(matches!(&app.file_transfers.queued[1], super::super::app::QueuedFileTask::Extract(_)));
        assert!(app.file_transfers.queued_summaries()[1].source_summary.contains("Extract 2"));
        assert!(app.test_file_task_dispatches.as_ref().unwrap().is_empty());

        // The previous file task releases its slot; each FIFO successor gets
        // its own session and the minimized mode remains in force.
        for expected_source in [&first, &source, &last] {
            app.file_transfers.active_session_id = None;
            app.active_overlay = super::super::app::ActiveOverlay::None;
            super::super::keybindings::maybe_start_next_file_transfer(&mut app, &tx);
            let dispatches = app.test_file_task_dispatches.as_ref().unwrap();
            assert_eq!(dispatches.last().unwrap().1, vec![expected_source.to_path_buf()]);
            assert!(app.minimized_file_task_progress.is_some());
        }
        assert!(app.file_transfers.queued.is_empty());
        assert_eq!(app.test_file_task_dispatches.as_ref().unwrap().len(), 3);

        // Unstarted extraction has no helper, journal, or destination side
        // effects. Cancelling its row must never cancel the running copy.
        app.file_transfers.active_session_id = Some(9002);
        let before = app.test_file_task_dispatches.as_ref().unwrap().len();
        let pending = recovery_fixture_job(&dest);
        launch(&mut app, pending, &tx).unwrap();
        let queue_id = app.file_transfers.queued[0].queue_id();
        super::super::keybindings::cancel_queued_file_transfer(&mut app, queue_id, &tx);
        assert!(app.file_transfers.queued.is_empty());
        assert_eq!(app.file_transfers.active_session_id, Some(9002));
        assert_eq!(app.test_file_task_dispatches.as_ref().unwrap().len(), before);
    }

    #[test]
    fn native_completion_advances_fifo_only_after_success() {
        use crate::config::TonepoetConfig;
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel(16);

        let mut successful = AppState::new_for_test(TonepoetConfig::default());
        successful.test_file_task_dispatches = Some(Vec::new());
        launch(&mut successful, recovery_fixture_job(dir.path()), &tx).unwrap();
        let first_session = successful.file_transfers.active_session_id.unwrap();
        launch(&mut successful, recovery_fixture_job(dir.path()), &tx).unwrap();
        assert_eq!(successful.file_transfers.queued.len(), 1);
        super::super::event_loop::handle_message(
            &mut successful,
            AppMessage::NativeExtractComplete {
                session_id: first_session,
                result: Ok(NativeExtractionReport { published: vec![], skipped: 0 }),
            },
            &tx,
        );
        assert!(successful.file_transfers.queued.is_empty());
        assert_ne!(successful.file_transfers.active_session_id, Some(first_session));
        assert_eq!(successful.test_file_task_dispatches.as_ref().unwrap().len(), 2);
        assert!(!successful.file_transfers.blocked_for_attention);

        // A failed helper must not launch another queued extraction behind
        // the user's back. Copy/move use the same attention barrier.
        let mut interrupted = AppState::new_for_test(TonepoetConfig::default());
        interrupted.test_file_task_dispatches = Some(Vec::new());
        launch(&mut interrupted, recovery_fixture_job(dir.path()), &tx).unwrap();
        let failed_session = interrupted.file_transfers.active_session_id.unwrap();
        launch(&mut interrupted, recovery_fixture_job(dir.path()), &tx).unwrap();
        super::super::event_loop::handle_message(
            &mut interrupted,
            AppMessage::NativeExtractComplete {
                session_id: failed_session,
                result: Err("worker stopped".into()),
            },
            &tx,
        );
        assert!(interrupted.file_transfers.blocked_for_attention);
        assert_eq!(interrupted.file_transfers.active_session_id, None);
        assert_eq!(interrupted.file_transfers.queued.len(), 1);
        assert_eq!(interrupted.test_file_task_dispatches.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn queued_native_jobs_are_snapshots_and_do_not_enter_convert_queue() {
        use crate::config::TonepoetConfig;
        let dir = tempfile::tempdir().unwrap();
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.file_transfers.active_session_id = Some(42);
        let (tx, _rx) = mpsc::channel(8);
        let conversion_queue_before = app.manager.queue.try_read().unwrap().all_items().len();
        let mut original = recovery_fixture_job(dir.path());
        launch(&mut app, original.clone(), &tx).unwrap();
        original.destination = dir.path().join("new destination");
        launch(&mut app, original, &tx).unwrap();
        assert_eq!(app.file_transfers.queued.len(), 2);
        let first_dest = match &app.file_transfers.queued[0] {
            super::super::app::QueuedFileTask::Extract(task) => &task.job.destination,
            _ => panic!("first job must be an extraction"),
        };
        assert_eq!(first_dest.as_path(), dir.path());
        let second_dest = match &app.file_transfers.queued[1] {
            super::super::app::QueuedFileTask::Extract(task) => &task.job.destination,
            _ => panic!("second job must be an extraction"),
        };
        assert_eq!(second_dest, &dir.path().join("new destination"));
        assert_eq!(app.manager.queue.try_read().unwrap().all_items().len(),
            conversion_queue_before, "native extraction must not enter Convert");
    }

    #[test]
    fn recovery_skips_committed_outputs_without_republishing_them() {
        let dir = tempfile::tempdir().unwrap();
        let job = recovery_fixture_job(dir.path());
        let program = &job.programs[0];
        let done = native_extract::proposed_destination(&job, program, &program.tracks[0]);
        std::fs::create_dir_all(done.parent().unwrap()).unwrap();
        std::fs::write(&done, b"published audio remains unchanged").unwrap();
        let remaining = remaining_native_job(job, std::slice::from_ref(&done), &[], &[]).unwrap();
        assert_eq!(remaining.programs.len(), 1);
        assert_eq!(remaining.programs[0].tracks.len(), 1);
        assert_eq!(remaining.programs[0].tracks[0].number, 2);
        assert_eq!(std::fs::read(done).unwrap(), b"published audio remains unchanged");
    }

    #[test]
    fn recovered_log_contains_original_and_resumed_publications_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut job = recovery_fixture_job(dir.path());
        job.extraction_log = true;
        job.programs[0].tracks.push(native_extract::NativeTrackChoice { duration_secs: None,
            number: 3, name: "Track 3".into(), tags: Default::default(),
            estimated_bytes: Some(10_000),
        });
        let paths: Vec<_> = job.programs[0].tracks.iter().map(|track|
            native_extract::proposed_destination(&job, &job.programs[0], track)).collect();
        std::fs::create_dir_all(paths[0].parent().unwrap()).unwrap();
        std::fs::write(&paths[0], b"first published generation").unwrap();
        let remaining = remaining_native_job(job.clone(), &paths[..1], &[], &[]).unwrap();
        assert_eq!(remaining.programs[0].tracks.len(), 2);
        let body = native_extract::render_native_extraction_log(&job,
            &[paths[0].clone(),paths[1].clone(),paths[2].clone(),paths[0].clone()], &[]);
        for path in &paths {
            assert_eq!(body.matches(&format!("File: {:?}",path.to_string_lossy())).count(), 1);
        }
        assert_eq!(body.matches("Outcome: written").count(), 3);
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"first published generation");
    }

    #[test]
    fn log_only_recovery_does_not_reextract_or_modify_completed_audio() {
        let dir = tempfile::tempdir().unwrap();
        let mut job = recovery_fixture_job(&dir.path().join("output"));
        job.working_root = dir.path().join("scratch");
        job.extraction_log = true;
        let published = job.programs[0].tracks.iter().map(|track|
            native_extract::proposed_destination(&job, &job.programs[0], track))
            .collect::<Vec<_>>();
        for path in &published {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"already completed audio").unwrap();
        }
        assert!(remaining_native_job(job.clone(), &published, &[], &[])
            .unwrap().programs.is_empty());
        let log_path = job.destination.join(native_extract::EXTRACTION_LOG_FILE);
        let text = native_extract::render_native_extraction_log(&job, &published, &[]);
        native_extract::publish_native_recovery_sidecar(&job, &log_path, &text,
            &CancellationToken::new(), &NativeExtractionControl::default()).unwrap();
        let actual = std::fs::read_to_string(&log_path).unwrap();
        assert_eq!(actual, text);
        assert_eq!(actual.matches("Outcome: written").count(), published.len());
        for path in &published {
            assert_eq!(std::fs::read(path).unwrap(), b"already completed audio");
        }
    }

    #[test]
    fn recovered_continuous_cue_uses_saved_text_without_rewriting_audio() {
        let dir = tempfile::tempdir().unwrap();
        let mut job = recovery_fixture_job(&dir.path().join("output"));
        job.working_root = dir.path().join("scratch");
        job.format = crate::disc::DiscFormat::DvdAudio;
        job.programs[0].id = crate::disc::PresentationId::DvdAudioGroup(1);
        job.file_per_program = true;
        job.cue = true;
        let audio = native_extract::proposed_destination_for_program(&job, &job.programs[0]);
        let cue = audio.with_extension("cue");
        std::fs::create_dir_all(audio.parent().unwrap()).unwrap();
        std::fs::write(&audio, b"continuous PCM is already published").unwrap();
        let prepared = vec![DurableNativePreparedCue {
            destination: cue.clone(),
            content: "FILE \"Stereo.wav\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n".into(),
        }];
        assert!(remaining_native_job(job.clone(), std::slice::from_ref(&audio), &[], &[]).is_err());
        assert!(remaining_native_job(job.clone(), std::slice::from_ref(&audio), &[], &prepared)
            .unwrap().programs.is_empty());
        let pending = pending_native_cues(&job, std::slice::from_ref(&audio), &prepared).unwrap();
        assert_eq!(pending, prepared);
        native_extract::publish_native_recovery_sidecar(&job, &cue, &pending[0].content,
            &CancellationToken::new(), &NativeExtractionControl::default()).unwrap();
        assert_eq!(std::fs::read_to_string(&cue).unwrap(), prepared[0].content);
        assert_eq!(std::fs::read(&audio).unwrap(), b"continuous PCM is already published");
        assert!(pending_native_cues(&job, &[audio, cue], &prepared).unwrap().is_empty());
    }

    #[test]
    fn recovery_preserves_confirmed_skips_and_original_preflight_totals() {
        let dir = tempfile::tempdir().unwrap();
        let mut job = recovery_fixture_job(dir.path());
        job.programs[0].tracks.push(native_extract::NativeTrackChoice { duration_secs: None,
            number: 3, name: "Track 3".into(), tags: Default::default(),
            estimated_bytes: Some(10_000),
        });
        let program = &job.programs[0];
        let skipped = vec![NativeSkippedSelection::track(program, 1)];
        let published = vec![native_extract::proposed_destination(&job, program, &program.tracks[1])];
        std::fs::create_dir_all(published[0].parent().unwrap()).unwrap();
        std::fs::write(&published[0], b"second track is unchanged").unwrap();
        let remaining = remaining_native_job(job.clone(), &published, &skipped, &[]).unwrap();
        assert_eq!(remaining.programs[0].tracks.iter().map(|track|track.number).collect::<Vec<_>>(), vec![3]);
        let baseline = native_recovery_baseline(&job, &published, &skipped);
        let resumed_start = add_native_recovery_baseline(NativeExtractionProgress {
            done: 0, total: 1, published: 0, published_paths: vec![], skipped: 0,
            current: String::new(), current_file: None, current_estimated_bytes: None,
            estimated_bytes_done: 0, estimated_bytes_published: 0,
            estimated_bytes_skipped: 0, estimated_bytes_total: Some(10_000),
        }, baseline);
        let start = totals_from(&resumed_start, 0);
        assert_eq!((start.items_done, start.items_total, start.completed, start.skipped),
            (2, Some(3), 1, 1));
        assert_eq!((start.bytes_done, start.bytes_total), (10_000, Some(30_000)));
        let finished = add_native_recovery_baseline(NativeExtractionProgress {
            done: 1, total: 1, published: 1, published_paths: vec![], skipped: 0,
            current: String::new(), current_file: None, current_estimated_bytes: None,
            estimated_bytes_done: 10_000, estimated_bytes_published: 10_000,
            estimated_bytes_skipped: 0, estimated_bytes_total: Some(10_000),
        }, baseline);
        let end = totals_from(&finished, 0);
        assert_eq!((end.items_done, end.items_total, end.completed, end.skipped),
            (3, Some(3), 2, 1));
        assert_eq!((end.bytes_done, end.bytes_total), (20_000, Some(30_000)));
        assert_eq!(std::fs::read(&published[0]).unwrap(), b"second track is unchanged");
    }

    #[test]
    fn partial_continuous_skip_has_stable_progress_through_next_program_and_recovery() {
        const TRACK: u64 = 100 * 1024 * 1024;
        let dir = tempfile::tempdir().unwrap();
        let mut job = recovery_fixture_job(dir.path());
        job.file_per_program = true;
        job.programs[0].label = "First program".into();
        job.programs[0].tracks.iter_mut().for_each(|t| t.estimated_bytes = Some(TRACK));
        let mut second = job.programs[0].clone();
        second.id = crate::disc::PresentationId::SacdArea(
            crate::disc::SacdAreaId::MultiChannel);
        second.label = "Second program".into();
        second.tracks.push(native_extract::NativeTrackChoice { duration_secs: None,
            number: 3, name: "Track 3".into(), tags: Default::default(),
            estimated_bytes: Some(TRACK),
        });
        job.programs.push(second);

        let skipped = vec![NativeSkippedSelection::track(&job.programs[0], 1)];
        let first_audio = native_extract::proposed_destination_for_program(&job, &job.programs[0]);
        std::fs::create_dir_all(first_audio.parent().unwrap()).unwrap();
        std::fs::write(&first_audio, b"original published continuous audio").unwrap();
        let prior = vec![first_audio.clone()];
        let remaining = remaining_native_job(job.clone(), &prior, &skipped, &[]).unwrap();
        assert_eq!(remaining.programs.len(), 1);
        assert_eq!(remaining.programs[0].label, "Second program");
        assert_eq!(remaining.programs[0].tracks.len(), 3);

        // The 175 ms sampler can observe this intermediate snapshot while
        // the second (long-running) program is still being written.
        let after_first_publish = NativeExtractionProgress {
            done: 2, total: 5, published: 1, published_paths: prior.clone(), skipped: 1,
            current: "Published First program".into(), current_file: None,
            current_estimated_bytes: None, estimated_bytes_done: 2 * TRACK,
            estimated_bytes_published: TRACK, estimated_bytes_skipped: TRACK,
            estimated_bytes_total: Some(5 * TRACK),
        };
        let first_totals = totals_from(&after_first_publish, 0);
        assert_eq!((first_totals.items_done, first_totals.items_total), (2, Some(5)));
        assert_eq!((first_totals.completed, first_totals.skipped), (1, 1));
        assert_eq!((first_totals.bytes_done, first_totals.bytes_total),
            (TRACK, Some(5 * TRACK)));
        let mut before_first_publish = after_first_publish.clone();
        before_first_publish.published = 0;
        before_first_publish.estimated_bytes_published = 0;
        let publishing_first = NativeWriteWatch {
            label: "Publishing First program".into(),
            source: NativeWatchSource::File("/not-a-real-private-file".into()),
            baseline: 0, estimated_bytes: Some(TRACK), finalizing: true,
            fraction_start: 90, fraction_end: 100,
        };
        let before = totals_for_native_watch(
            &before_first_publish, Some(&publishing_first), TRACK / 2, true);
        assert!(before.bytes_done <= first_totals.bytes_done,
            "publishing the partially skipped program must not roll progress backward");

        let writing_second = NativeWriteWatch {
            label: "Writing second program".into(),
            source: NativeWatchSource::File("/not-a-real-private-file".into()),
            baseline: 0, estimated_bytes: Some(TRACK), finalizing: false,
            fraction_start: 0, fraction_end: 85,
        };
        let during_second = totals_for_native_watch(
            &after_first_publish, Some(&writing_second), TRACK / 2, true);
        assert!(during_second.bytes_done > first_totals.bytes_done);
        assert!(during_second.bytes_done < 4 * TRACK);

        let at_completion = NativeExtractionProgress {
            done: 5, total: 5, published: 2, published_paths: prior.clone(), skipped: 1,
            current: "Complete".into(), current_file: None,
            current_estimated_bytes: None, estimated_bytes_done: 5 * TRACK,
            estimated_bytes_published: 4 * TRACK, estimated_bytes_skipped: TRACK,
            estimated_bytes_total: Some(5 * TRACK),
        };
        let terminal = totals_from(&at_completion, 0);
        assert_eq!((terminal.items_done, terminal.items_total), (5, Some(5)));
        assert_eq!((terminal.completed, terminal.skipped), (4, 1));
        assert_eq!((terminal.bytes_done, terminal.bytes_total),
            (4 * TRACK, Some(5 * TRACK)));
        assert!(terminal.bytes_done >= during_second.bytes_done);

        // A new detached helper generation retains the original preflight
        // and the one skipped track without rewriting the published program.
        let base = native_recovery_baseline(&job, &prior, &skipped);
        assert_eq!((base.total, base.done, base.skipped), (5, 2, 1));
        assert_eq!((base.completed_bytes, base.skipped_bytes), (TRACK, TRACK));
        let resumed_start = add_native_recovery_baseline(NativeExtractionProgress {
            done: 0, total: 3, published: 0, published_paths: Vec::new(), skipped: 0,
            current: "Resumed".into(), current_file: None,
            current_estimated_bytes: None, estimated_bytes_done: 0,
            estimated_bytes_published: 0, estimated_bytes_skipped: 0,
            estimated_bytes_total: Some(3 * TRACK),
        }, base);
        assert_eq!(totals_from(&resumed_start, 0).bytes_done, TRACK);
        let resumed_end = add_native_recovery_baseline(NativeExtractionProgress {
            done: 3, total: 3, published: 1, published_paths: Vec::new(), skipped: 0,
            current: "Complete".into(), current_file: None,
            current_estimated_bytes: None, estimated_bytes_done: 3 * TRACK,
            estimated_bytes_published: 3 * TRACK, estimated_bytes_skipped: 0,
            estimated_bytes_total: Some(3 * TRACK),
        }, base);
        let recovered = totals_from(&resumed_end, 0);
        assert_eq!((recovered.items_done, recovered.items_total), (5, Some(5)));
        assert_eq!((recovered.completed, recovered.skipped), (4, 1));
        assert_eq!((recovered.bytes_done, recovered.bytes_total),
            (4 * TRACK, Some(5 * TRACK)));
        assert_eq!(std::fs::read(first_audio).unwrap(), b"original published continuous audio");
    }

    #[test]
    fn whole_continuous_program_skip_is_not_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let mut job = recovery_fixture_job(dir.path());
        job.file_per_program = true;
        let skipped = vec![NativeSkippedSelection::program(&job.programs[0])];
        assert!(remaining_native_job(job.clone(), &[], &skipped, &[])
            .unwrap().programs.is_empty());
        let prior = native_recovery_baseline(&job, &[], &skipped);
        assert_eq!((prior.total, prior.done, prior.skipped), (2, 2, 2));
        assert_eq!(prior.skipped_bytes, 20_000);
        assert_eq!(prior.completed_bytes, 0);
    }

    #[test]
    fn prior_continuous_publication_does_not_hide_unfinished_final_join() {
        let progress = NativeExtractionProgress {
            done: 4, total: 4, published: 1, published_paths: vec![], skipped: 0,
            current: "Joining second program".into(), current_file: None,
            current_estimated_bytes: Some(20_000),
            estimated_bytes_done: 40_000, estimated_bytes_published: 20_000,
            estimated_bytes_skipped: 0, estimated_bytes_total: Some(40_000),
        };
        let watch = NativeWriteWatch {
            label: "Joining second program".into(),
            source: NativeWatchSource::File("/not-a-real-staged-file".into()),
            baseline: 0, estimated_bytes: Some(20_000), finalizing: true,
            fraction_start: 75, fraction_end: 90,
        };
        let totals = totals_for_native_watch(&progress, Some(&watch), 10_000, true);
        assert_eq!(totals.items_total, Some(4));
        assert_eq!(totals.items_done, 3);
        assert_eq!(totals.completed, 3);
        assert!(totals.bytes_done < 40_000);
    }

    #[test]
    fn replace_recovery_refuses_ambiguous_already_published_filename() {
        let dir = tempfile::tempdir().unwrap();
        let job = recovery_fixture_job(dir.path());
        let path = native_extract::proposed_destination(
            &job, &job.programs[0], &job.programs[0].tracks[0],
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"uncertain publication").unwrap();
        assert!(refuse_ambiguous_recovery_replacement(&job, &[], &HashSet::new()).is_err());
        assert!(refuse_ambiguous_recovery_replacement(&job, std::slice::from_ref(&path), &HashSet::new()).is_ok());
        assert_eq!(std::fs::read(path).unwrap(), b"uncertain publication");
    }

    #[test]
    fn r34_program_logs_have_independent_durable_ownership() {
        let _lock=file_task_runtime::test_environment_lock();
        let _coordination=crate::concurrency::scoped_test_coordination_root();
        struct RestoreJournalDir(Option<std::ffi::OsString>);
        impl Drop for RestoreJournalDir {
            fn drop(&mut self) {
                if let Some(old)=self.0.take() {
                    std::env::set_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR",old);
                } else {std::env::remove_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR");}
            }
        }
        let temp=tempfile::tempdir().unwrap();
        let _restore=RestoreJournalDir(std::env::var_os("TONEPOET_FILE_OPERATION_JOURNAL_DIR"));
        std::env::set_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR",temp.path().join("journals"));
        let mut job=recovery_fixture_job(&temp.path().join("destination"));
        job.working_root=temp.path().join("scratch");
        job.round4_layout=true;
        job.program_subfolders=false;
        job.extraction_log=true;
        job.overwrite_policy=native_extract::NativeOverwritePolicy::Replace;
        job.naming_template="%AREA% - %TRACKNN% - %TITLE%".into();
        let mut second=job.programs[0].clone();
        second.id=crate::disc::PresentationId::SacdArea(crate::disc::SacdAreaId::MultiChannel);
        second.label="Multichannel".into();
        job.programs.push(second);
        std::fs::create_dir_all(&job.destination).unwrap();
        native_extract::plan_native_program_logs(&mut job).unwrap();
        assert_eq!(job.program_log_paths.len(),2);
        let paths=job.program_log_paths.clone();
        let mapping=PasteMapping{source:job.source.clone(),destination:job.destination.clone()};
        let payload=serde_json::json!({"native_extraction":job.clone(),
            "dest":job.destination.to_string_lossy()});
        let journal=FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(),1,3,false,
            VerificationMode::Standard,600,vec![mapping],None,payload).unwrap();
        let controls=NativeExtractionControl::default();
        for (index,path) in paths.iter().enumerate() {
            let content=native_extract::native_log_content_for_index(&job,index,&[],&[],&[],
                NativeLogOutcome::Complete,None);
            publish_journaled_native_log_at_path(&job,&journal,&controls,path,&content,
                &CancellationToken::new()).unwrap();
        }
        let recorded=journal.load().unwrap();
        assert_eq!(recorded.native_log_digests.len(),2);
        assert!(recorded.native_log_sha256.is_none());
        assert!(verify_native_log_ownership(&job,&recorded).unwrap());
        let saved0=std::fs::read(&paths[0]).unwrap();
        std::fs::write(&paths[1],b"outside changes to the second log").unwrap();
        assert!(verify_native_log_target_ownership(&job,&paths[0],&recorded).unwrap());
        assert!(verify_native_log_target_ownership(&job,&paths[1],&recorded).is_err());
        assert!(publish_journaled_native_log_at_path(&job,&journal,&controls,&paths[1],
            "an attempted replacement",&CancellationToken::new()).is_err());
        assert_eq!(std::fs::read(&paths[1]).unwrap(),b"outside changes to the second log");
        assert_eq!(std::fs::read(&paths[0]).unwrap(),saved0);
    }

    #[test]
    fn interrupted_receipt_is_replaced_only_after_owned_recovery_and_never_rewrites_audio() {
        // Use the ordinary journal and sidecar publisher, not a test-only
        // surrogate for their ownership checks or publication policy.
        let _lock = file_task_runtime::test_environment_lock();
        let _coordination = crate::concurrency::scoped_test_coordination_root();
        struct RestoreJournalDir(Option<std::ffi::OsString>);
        impl Drop for RestoreJournalDir {
            fn drop(&mut self) {
                if let Some(old) = self.0.take() {
                    std::env::set_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR", old);
                } else {
                    std::env::remove_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR");
                }
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let _restore = RestoreJournalDir(std::env::var_os("TONEPOET_FILE_OPERATION_JOURNAL_DIR"));
        std::env::set_var("TONEPOET_FILE_OPERATION_JOURNAL_DIR", temp.path().join("journals"));
        let mut job = recovery_fixture_job(&temp.path().join("destination"));
        job.working_root = temp.path().join("scratch");
        job.extraction_log = true;
        job.overwrite_policy = native_extract::NativeOverwritePolicy::Fail;
        std::fs::create_dir_all(&job.destination).unwrap();
        let first_audio = native_extract::proposed_destination(&job, &job.programs[0],
            &job.programs[0].tracks[0]);
        let second_audio = native_extract::proposed_destination(&job, &job.programs[0],
            &job.programs[0].tracks[1]);
        let log_path = job.destination.join(native_extract::EXTRACTION_LOG_FILE);
        let mapping = PasteMapping { source: job.source.clone(), destination: job.destination.clone() };
        let payload = serde_json::json!({"native_extraction":job.clone(),
            "dest":job.destination.to_string_lossy()});
        let journal = FileTaskJournalHandle::create(
            uuid::Uuid::new_v4().to_string(), 1, 3, false,
            VerificationMode::Standard, 600, vec![mapping.clone()], None, payload.clone(),
        ).unwrap();
        std::fs::create_dir_all(first_audio.parent().unwrap()).unwrap();
        std::fs::write(&first_audio, b"generation one audio").unwrap();
        journal.record_native_published(&first_audio).unwrap();
        let interrupted = native_extract::NativeUnfinishedOutput {
            destination: second_audio.clone(), reason: "source read failed".into(),
            cancelled: true,
        };
        journal.record_native_unfinished(&interrupted).unwrap();
        let body = native_extract::render_native_extraction_log_with_outcome(&job,
            std::slice::from_ref(&first_audio), &[], &[interrupted.clone()],
            NativeLogOutcome::Cancelled, Some("Extraction cancelled"));
        let controls = NativeExtractionControl::default();
        publish_journaled_native_log(&job, &journal, &controls, &body, &CancellationToken::new()).unwrap();
        assert_eq!(std::fs::read_to_string(&log_path).unwrap(), body);
        assert!(body.contains("Outcome: cancelled"));
        assert!(body.contains("source read failed"));
        let recorded = journal.load().unwrap();
        assert!(recorded.native_published_paths.contains(&log_path));
        assert!(recorded.native_log_sha256.is_some());
        assert!(verify_native_log_ownership(&job, &recorded).unwrap());
        let resume_path = journal.path().to_path_buf();
        drop(journal);

        let resumed = FileTaskJournalHandle::resume(resume_path, 2, 4, false,
            VerificationMode::Standard, 600, vec![mapping], None, payload).unwrap();
        let prior = resumed.load().unwrap();
        let pending = remaining_native_job(job.clone(), &prior.native_published_paths,
            &prior.native_skipped_selections, &prior.native_prepared_cues).unwrap();
        assert_eq!(pending.programs[0].tracks.len(), 1);
        assert_eq!(pending.programs[0].tracks[0].number, 2);
        // A successful recovery publishes only the remaining track. The
        // original audio must not be processed or replaced again.
        std::fs::write(&second_audio, b"generation two audio").unwrap();
        resumed.record_native_published(&second_audio).unwrap();
        let recovered = resumed.load().unwrap();
        let body = native_extract::render_native_extraction_log_with_outcome(&job,
            &recovered.native_published_paths, &recovered.native_skipped_selections,
            &recovered.native_unfinished_outputs, NativeLogOutcome::Complete, None);
        publish_journaled_native_log(&job, &resumed, &controls, &body, &CancellationToken::new()).unwrap();
        assert!(body.contains("Outcome: complete"), "{body}");
        assert_eq!(body.matches("Outcome: written").count(), 2);
        assert!(body.contains("Failed outputs: 0"));
        assert_eq!(std::fs::read_to_string(&log_path).unwrap(), body);
        assert_eq!(std::fs::read(&first_audio).unwrap(), b"generation one audio");
        let after = resumed.load().unwrap();
        assert_eq!(after.native_published_paths.iter().filter(|path| **path == log_path).count(), 1);
        assert_ne!(after.native_log_sha256, recorded.native_log_sha256);

        // This pathname no longer contains the journal's receipt. Recovery
        // must not adopt its contents merely because the name matches.
        std::fs::write(&log_path, b"user edited this receipt").unwrap();
        let error = publish_journaled_native_log(&job, &resumed, &controls,
            &body, &CancellationToken::new()).unwrap_err();
        assert!(error.contains("changed outside this task"), "{error}");
        assert_eq!(std::fs::read(&log_path).unwrap(), b"user edited this receipt");
        std::fs::remove_file(&log_path).unwrap();
        std::fs::create_dir(&log_path).unwrap();
        let error = publish_journaled_native_log(&job, &resumed, &controls,
            &body, &CancellationToken::new()).unwrap_err();
        assert!(error.contains("changed type"), "{error}");
    }

    #[test]
    fn continuous_staged_tracks_are_not_lost_between_watch_phases() {
        let mut progress = NativeExtractionProgress {
            done: 1, total: 2, published: 0, published_paths: vec![], skipped: 0,
            current: "next part".into(), current_file: None,
            current_estimated_bytes: Some(1000),
            estimated_bytes_done: 1000, estimated_bytes_published: 0,
            estimated_bytes_skipped: 0, estimated_bytes_total: Some(2000),
        };
        let stage = NativeWriteWatch {
            label: "writing second track".into(),
            source: NativeWatchSource::File("/does-not-exist".into()),
            baseline: 0, estimated_bytes: Some(1000), finalizing: false,
            fraction_start: 0, fraction_end: 85,
        };
        let writing = totals_for_native_watch(&progress, Some(&stage), 500, true);
        assert!(writing.bytes_done >= 750);
        progress.done = 2;
        progress.estimated_bytes_done = 2000;
        let joined = NativeWriteWatch {
            label: "joining".into(), fraction_start: 75, fraction_end: 90,
            estimated_bytes: Some(2000), finalizing: true, ..stage.clone()
        };
        let join_start = totals_for_native_watch(&progress, Some(&joined), 0, true);
        let join_mid = totals_for_native_watch(&progress, Some(&joined), 1000, true);
        assert!(join_start.bytes_done >= writing.bytes_done);
        assert!(join_mid.bytes_done > join_start.bytes_done);
        assert!(join_mid.items_done < 2, "joining is not published");
        assert!(join_mid.completed <= join_mid.items_done);
        let publication = NativeWriteWatch {
            label: "publishing".into(), fraction_start: 90, fraction_end: 100,
            ..joined
        };
        let copied = totals_for_native_watch(&progress, Some(&publication), 1000, true);
        assert!(copied.bytes_done > join_mid.bytes_done);
        assert!(copied.bytes_done < 2000);
    }

    #[test]
    fn terminal_bytes_do_not_claim_skipped_work_was_written() {
        let progress = NativeExtractionProgress {
            done: 2, total: 3, published: 1, published_paths: vec![], skipped: 1,
            current: String::new(), current_file: None, current_estimated_bytes: None,
            estimated_bytes_done: 1000, estimated_bytes_published: 600,
            estimated_bytes_skipped: 400,
            estimated_bytes_total: Some(1500),
        };
        let totals = totals_from(&progress, 200);
        assert_eq!(totals.items_done, 2);
        assert_eq!(totals.completed, 1);
        assert_eq!(totals.skipped, 1);
        assert_eq!(totals.bytes_done, 800);
        assert_eq!(totals.bytes_total, Some(1500));
    }
}
