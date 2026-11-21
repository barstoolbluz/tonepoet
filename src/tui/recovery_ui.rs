//! Interrupted jobs presentation and recovery coordination.
//!
//! The durable file-task record remains authoritative. This module derives
//! presentation state from immutable snapshots and sends explicit resume or
//! discard work back through the existing file-task machinery.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Clear, Paragraph},
    Frame,
};
use tokio::sync::mpsc;

use super::app::{ActiveOverlay, AppState, QueuedFileTransfer};
use super::button_map::{ButtonRenderMap, TuiButton};
use super::file_task_runtime::{
    DurableFileTaskLifecycle, DurableFileTaskRecord, DurableQuarantineState,
    RecoveryRecordVersion, RecoverySurfaceAvailability, RecoverySurfaceRecord,
};
use super::message::AppMessage;
use super::file_task_live::{LiveWorkerRef, LiveWorkerSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverySurface {
    Window,
    Files,
    Refusal,
    DiscardConfirm,
    BulkDiscardConfirm,
    StopConfirm,
    ForceStopConfirm,
    SetAsideConfirm,
    Error,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDetailsReason {
    Ordinary,
    Refusal(PathBuf),
}

#[derive(Debug, Clone)]
pub struct RecoveryEntry {
    pub journal_path: PathBuf,
    pub record: Option<DurableFileTaskRecord>,
    pub version: Option<RecoveryRecordVersion>,
    pub availability: RecoverySurfaceAvailability,
    pub diagnostic: Option<String>,
    pub queue_id: Option<u64>,
    /// The user already approved Resume and this record is queued for, or
    /// entering, execution outside the review queue. Such an entry remains
    /// visible until it resolves, but must not concurrently offer Discard.
    pub resume_committed: bool,
    /// Queue identity of an approved Resume that has not yet been dispatched.
    /// Different from the dormant review queue's `queue_id`.
    pub approved_resume_queue_id: Option<u64>,
    /// The user confirmed Discard and its worker is taking over the durable
    /// record. Keep all competing decisions unavailable until that attempt
    /// reports success or failure.
    pub discard_committed: bool,
    pub live_snapshot: Option<LiveWorkerSnapshot>,
    pub stop_requested_unix_ms: Option<u64>,
}

/// Only the last checkpoint for a selected root represents its outcome.
/// Count facts and the Files popup must use the same recorded disposition.
fn last_recorded_root_disposition(
    record: &DurableFileTaskRecord,
    mapping: &tui_file_picker::PasteMapping,
) -> Option<tui_file_picker::FileTaskRootDisposition> {
    record.roots.iter().rev().find(|root|
        root.source == mapping.source && root.destination == mapping.destination)
        .map(|root| root.disposition)
}

impl RecoveryEntry {
    fn worker_ref(&self) -> Option<LiveWorkerRef> {
        let record = self.record.as_ref()?;
        LiveWorkerRef::new(self.journal_path.clone(), record.job_id.clone(), record.generation).ok()
    }

    fn worker_pid(&self) -> Option<u64> {
        self.live_snapshot.as_ref().filter(|snapshot| snapshot.worker.appears_active())
            .map(|snapshot| snapshot.worker.pid)
    }

    fn is_stalled(&self) -> bool {
        if !self.is_live() { return false; }
        let Some(snapshot) = self.live_snapshot.as_ref() else { return false; };
        if matches!(snapshot.phase, tui_file_picker::FileTaskPhase::Paused) { return false; }
        matches!(snapshot.phase, tui_file_picker::FileTaskPhase::Stalled)
            || snapshot.age_since_forward_ms() >= self.record.as_ref().map_or(8_000, |r| r.stall_timeout_secs.max(8).saturating_mul(1_000))
    }

    pub(super) fn kind_label(&self) -> &'static str {
        match self.record.as_ref() {
            Some(record) if record.is_native_extraction() => "extract",
            Some(record) if record.is_move => "move",
            Some(_) => "copy",
            None => "operation",
        }
    }

    fn item_count(&self) -> usize {
        self.record.as_ref().map_or(0, |record| {
            if record.is_native_extraction() {
                return super::native_extract_task::recorded_selection_counts(record)
                    .map_or(0, |(_, _, total)| total);
            }
            record.mappings.len()
        })
    }

    pub(super) fn is_live(&self) -> bool {
        self.availability == RecoverySurfaceAvailability::Live
    }

    fn has_observed_control(&self) -> bool {
        self.is_live() && self.live_snapshot.as_ref()
            .is_some_and(|snapshot| snapshot.worker.appears_active())
    }

    fn is_unreadable(&self) -> bool {
        self.availability == RecoverySurfaceAvailability::Unreadable
    }

    fn is_recoverable(&self) -> bool {
        self.availability == RecoverySurfaceAvailability::Recoverable
    }

    fn is_discard_cleanup(&self) -> bool {
        self.record.as_ref().is_some_and(
            super::file_task_runtime::recovery_record_is_discard_cleanup,
        )
    }

    fn max_quarantine_state(&self) -> Option<DurableQuarantineState> {
        self.record.as_ref().and_then(|record| {
            record
                .quarantine_artifacts
                .iter()
                .map(|artifact| artifact.state)
                .max_by_key(|state| match state {
                    DurableQuarantineState::IntentRecorded => 0,
                    DurableQuarantineState::RenameConfirmed => 1,
                    DurableQuarantineState::DeletionStarted => 2,
                })
        })
    }

    fn irreversible(&self) -> bool {
        self.max_quarantine_state() == Some(DurableQuarantineState::DeletionStarted)
    }

    fn can_resume(&self) -> bool {
        self.is_recoverable()
            && !self.is_discard_cleanup()
            && !self.discard_committed
            && !self.resume_committed
            && match self.record.as_ref() {
                Some(record) if record.is_native_extraction() =>
                    record.lifecycle != DurableFileTaskLifecycle::Completed,
                Some(record) => self.queue_id.is_some()
                    && (record.lifecycle != DurableFileTaskLifecycle::Completed
                        || !record.temp_artifacts.is_empty()),
                None => false,
            }
    }

    fn can_discard(&self) -> bool {
        self.is_recoverable()
            && !self.is_completed_clean()
            && !self.resume_committed
            && !self.discard_committed
            && self.record.is_some()
            && self.version.is_some()
    }

    fn completed_count(&self) -> usize {
        self.record.as_ref().map_or(0, |record| {
            if record.is_native_extraction() {
                return super::native_extract_task::recorded_selection_counts(record)
                    .map_or(0, |(done, _, _)| done);
            }
            // A durable root can have multiple checkpoints: only its final
            // disposition is the outcome of the user's selected item.
            record.mappings.iter().filter(|mapping|
                last_recorded_root_disposition(record, mapping)
                    .is_some_and(tui_file_picker::FileTaskRootDisposition::is_completed)).count()
        })
    }

    fn skipped_count(&self) -> usize {
        self.record.as_ref().map_or(0, |record| {
            if record.is_native_extraction() {
                return super::native_extract_task::recorded_selection_counts(record)
                    .map_or(0, |(_, skipped, _)| skipped);
            }
            record.mappings.iter().filter(|mapping|
                last_recorded_root_disposition(record, mapping)
                    == Some(tui_file_picker::FileTaskRootDisposition::Skipped)).count()
        })
    }

    fn failed_count(&self) -> usize {
        self.record.as_ref().map_or(0, |record| {
            if record.is_native_extraction() { return 0; }
            record.mappings.iter().filter(|mapping|
                last_recorded_root_disposition(record, mapping)
                    == Some(tui_file_picker::FileTaskRootDisposition::Failed)).count()
        })
    }

    fn remaining_count(&self) -> usize {
        self.item_count().saturating_sub(
            self.completed_count()
                .saturating_add(self.skipped_count())
                .saturating_add(self.failed_count()),
        )
    }

    fn blocked_paths(&self) -> Vec<PathBuf> {
        let Some(record) = self.record.as_ref() else {
            return Vec::new();
        };
        record
            .path_claims
            .iter()
            .map(|claim| claim.identity.original.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn incomplete_paths(&self) -> Vec<PathBuf> {
        self.record
            .as_ref()
            .map(|record| {
                record
                    .temp_artifacts
                    .iter()
                    .map(|artifact| artifact.path.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn renamed_sources(&self) -> Vec<PathBuf> {
        self.record
            .as_ref()
            .map(|record| {
                record
                    .quarantine_artifacts
                    .iter()
                    .map(|artifact| artifact.original_source.clone())
                    .chain(
                        record
                            .native_rename_intents
                            .iter()
                            .map(|intent| intent.source.clone()),
                    )
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn is_completed_clean(&self) -> bool {
        self.record.as_ref().is_some_and(|record| {
            record.lifecycle == DurableFileTaskLifecycle::Completed
                && record.temp_artifacts.is_empty()
                && record.quarantine_artifacts.is_empty()
                && record.native_rename_intents.is_empty()
        })
    }

    fn can_dismiss(&self) -> bool {
        self.is_recoverable() && self.is_completed_clean()
            && self.record.as_ref().is_some_and(DurableFileTaskRecord::is_native_extraction)
            && self.version.is_some() && !self.resume_committed && !self.discard_committed
    }

    fn can_tidy(&self) -> bool {
        (self.can_discard() || self.can_resume())
            && (self.is_discard_cleanup() || self.record.as_ref().is_some_and(|record|
                record.lifecycle == DurableFileTaskLifecycle::Completed
                    && !record.temp_artifacts.is_empty()
                    && (record.is_native_extraction() || self.queue_id.is_some())))
    }

    fn needs_attention(&self) -> bool {
        if self.is_unreadable() { return true; }
        if self.is_live() {
            return self.is_stalled() || self.live_snapshot.as_ref().is_some_and(|s|
                matches!(s.phase, tui_file_picker::FileTaskPhase::Stalled));
        }
        !self.resume_committed && !self.discard_committed
    }

    /// A live worker, even a stalled one, is running rather than interrupted.
    fn needs_review(&self) -> bool {
        !self.is_live() && self.needs_attention()
    }

}

#[derive(Debug, Clone)]
pub struct RecoveryUiState {
    pub entries: Vec<RecoveryEntry>,
    pub selected: usize,
    pub list_scroll: usize,
    pub surface: RecoverySurface,
    pub details_scroll: usize,
    pub details_reason: RecoveryDetailsReason,
    pub focus: usize,
    pub maximized: bool,
    /// At narrow widths show the selected job in its own tab.
    pub small_job_tab: bool,
    /// Actual screen rectangle from the last draw, used for mouse hit testing.
    pub last_screen: Rect,
    /// Derived from the last window draw; no terminal-size guesses in input handling.
    pub compact: bool,
    pub prompt_pending: bool,
    pub prompt_shown: bool,
    pub incomplete_sizes: BTreeMap<PathBuf, Option<u64>>,
    pub size_requests: BTreeSet<PathBuf>,
    pub discard_in_flight: BTreeSet<PathBuf>,
    /// Confirmed stop-and-undo requests wait for the worker's durable lease
    /// to be released before entering the existing discard machinery.
    pub stop_and_undo_pending: BTreeSet<PathBuf>,
    pub stop_requested_at: BTreeMap<PathBuf, u64>,
    pub aside_in_flight: BTreeSet<PathBuf>,
}

impl Default for RecoveryUiState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            selected: 0,
            list_scroll: 0,
            surface: RecoverySurface::Window,
            details_scroll: 0,
            details_reason: RecoveryDetailsReason::Ordinary,
            focus: 0,
            maximized: false,
            small_job_tab: false,
            last_screen: Rect::default(),
            compact: false,
            prompt_pending: false,
            prompt_shown: false,
            incomplete_sizes: BTreeMap::new(),
            size_requests: BTreeSet::new(),
            discard_in_flight: BTreeSet::new(),
            stop_and_undo_pending: BTreeSet::new(),
            stop_requested_at: BTreeMap::new(),
            aside_in_flight: BTreeSet::new(),
        }
    }
}

impl RecoveryUiState {
    pub fn from_records(records: Vec<RecoverySurfaceRecord>, queued: &VecDeque<QueuedFileTransfer>) -> Self {
        let mut state = Self::default();
        state.replace_records(records, queued);
        state.prompt_pending = state.entries.iter().any(RecoveryEntry::needs_review);
        state
    }

    pub fn replace_records(
        &mut self,
        records: Vec<RecoverySurfaceRecord>,
        queued: &VecDeque<QueuedFileTransfer>,
    ) {
        let selected_path = self
            .entries
            .get(self.selected)
            .map(|entry| entry.journal_path.clone());
        let discard_in_flight = self.discard_in_flight.clone();
        let stop_requested_at = self.stop_requested_at.clone();
        self.entries = records
            .into_iter()
            .map(|surface| RecoveryEntry {
                live_snapshot: if surface.availability == RecoverySurfaceAvailability::Live {
                    surface.record.as_ref().and_then(|record| {
                        LiveWorkerRef::new(surface.journal_path.clone(), record.job_id.clone(), record.generation)
                            .ok().and_then(|identity| super::file_task_live::read_snapshot(&identity))
                    })
                } else { None },
                queue_id: queue_id_for_journal(queued, &surface.journal_path),
                resume_committed: false,
                approved_resume_queue_id: None,
                stop_requested_unix_ms: stop_requested_at.get(&surface.journal_path).copied(),
                discard_committed: discard_in_flight.contains(&surface.journal_path),
                journal_path: surface.journal_path,
                record: surface.record,
                version: surface.version,
                availability: surface.availability,
                diagnostic: surface.diagnostic,
            })
            .collect();
        self.entries.sort_by(|a, b| {
            b.needs_attention().cmp(&a.needs_attention())
                .then_with(|| b.record.as_ref().map_or(0, |r| r.created_unix_ms)
                    .cmp(&a.record.as_ref().map_or(0, |r| r.created_unix_ms)))
                .then_with(|| a.journal_path.cmp(&b.journal_path))
        });
        self.selected = selected_path
            .as_ref()
            .and_then(|path| {
                self.entries
                    .iter()
                    .position(|entry| &entry.journal_path == path)
            })
            .unwrap_or_else(|| self.selected.min(self.entries.len().saturating_sub(1)));
        self.list_scroll = self.list_scroll.min(self.selected);
        self.discard_in_flight
            .retain(|path| self.entries.iter().any(|entry| &entry.journal_path == path));
        self.stop_requested_at
            .retain(|path, _| self.entries.iter().any(|entry| &entry.journal_path == path));
        self.size_requests
            .retain(|path| self.entries.iter().any(|entry| &entry.journal_path == path));
        let current_incomplete = self
            .entries
            .iter()
            .flat_map(RecoveryEntry::incomplete_paths)
            .collect::<BTreeSet<_>>();
        self.incomplete_sizes
            .retain(|path, _| current_incomplete.contains(path));
        if !self.entries.iter().any(RecoveryEntry::needs_review) && !self.prompt_shown {
            // The startup set was resolved by another process before the
            // prompt could be shown. Do not let a later unrelated recovery
            // resurrect a stale "session-start" prompt.
            self.prompt_pending = false;
        }
    }

    fn selected_entry(&self) -> Option<&RecoveryEntry> {
        self.entries.get(self.selected)
    }

    fn consume_startup_prompt(&mut self) {
        self.prompt_pending = false;
        self.prompt_shown = true;
    }

    fn open_window(&mut self) {
        self.consume_startup_prompt();
        self.surface = RecoverySurface::Window;
        self.focus = 0;
        self.details_reason = RecoveryDetailsReason::Ordinary;
    }

    pub(super) fn minimize(&mut self) {
        self.surface = RecoverySurface::Window;
        self.focus = 0;
    }

    fn open_details(&mut self, reason: RecoveryDetailsReason) {
        self.consume_startup_prompt();
        self.surface = RecoverySurface::Files;
        self.details_reason = reason;
        self.details_scroll = 0;
        self.focus = 0;
    }

    fn resume_all_available(&self) -> bool {
        let decisions = self.entries.iter().filter(|entry| entry.needs_attention()).collect::<Vec<_>>();
        decisions.len() >= 2 && decisions.iter().all(|entry| {
            self.can_resume_unattended(entry)
                && entry.record.as_ref().is_some_and(|record| {
                    record.quarantine_artifacts.is_empty()
                        && record.native_rename_intents.is_empty()
                })
        })
    }

    fn can_resume_unattended(&self, entry: &RecoveryEntry) -> bool {
        !entry.record.as_ref().is_some_and(DurableFileTaskRecord::is_native_extraction)
            && entry.can_resume() && !entry_has_destination_overlap(&self.entries, entry)
    }

    fn bulk_discard_count(&self) -> usize {
        let decisions = self.entries.iter().filter(|entry| entry.needs_attention()).collect::<Vec<_>>();
        if decisions.len() >= 2 && decisions.iter().all(|entry| entry.can_discard()) {
            decisions.len()
        } else { 0 }
    }

    #[cfg(test)]
    fn resume_count(&self) -> usize {
        self.entries.iter().filter(|entry| self.can_resume_unattended(entry)).count()
    }

    pub(super) fn needs_attention_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.needs_attention()).count()
    }

    pub(super) fn review_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.needs_review()).count()
    }

    pub(super) fn running_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.is_live()).count()
    }

    fn open_refusal(&mut self, attempted: &Path) -> bool {
        let match_row = self.entries.iter().enumerate().find_map(|(entry_index, entry)| {
            entry
                .blocked_paths()
                .into_iter()
                .enumerate()
                .find(|(_, blocked)| paths_overlap(blocked, attempted))
                .map(|(row, blocked)| (entry_index, row, blocked))
        });
        let Some((entry_index, row, blocked)) = match_row else {
            return false;
        };
        self.selected = entry_index;
        self.open_details(RecoveryDetailsReason::Refusal(blocked));
        self.surface = RecoverySurface::Refusal;
        self.details_scroll = row;
        true
    }

    fn open_refusal_from_status(&mut self, message: &str) -> bool {
        if !message.contains("filesystem mutation conflicts with recovery reservation") {
            return false;
        }
        let found = self.entries.iter().enumerate().find_map(|(entry_index, entry)| {
            entry
                .blocked_paths()
                .into_iter()
                .enumerate()
                .filter(|(_, path)| message.contains(&path.display().to_string()))
                .max_by_key(|(_, path)| path.as_os_str().len())
                .map(|(row, path)| (entry_index, row, path))
        });
        let Some((entry_index, row, blocked)) = found else {
            return false;
        };
        self.selected = entry_index;
        self.open_details(RecoveryDetailsReason::Refusal(blocked));
        self.surface = RecoverySurface::Refusal;
        self.details_scroll = row;
        true
    }
}

fn queue_id_for_journal(
    queued: &VecDeque<QueuedFileTransfer>,
    journal_path: &Path,
) -> Option<u64> {
    queued.iter().find_map(|job| {
        job.retry_plan
            .as_ref()
            .and_then(|retry| retry.recovery_journal_path.as_ref())
            .filter(|path| path.as_path() == journal_path)
            .map(|_| job.queue_id)
    })
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn queued_recovery_journal(job: &QueuedFileTransfer) -> Option<&Path> {
    job.retry_plan
        .as_ref()
        .and_then(|retry| retry.recovery_journal_path.as_deref())
}

fn recovery_is_already_runnable_or_active(app: &AppState, journal_path: &Path) -> bool {
    app.file_transfers.queued.iter().any(|job| {
        job.transfer().and_then(queued_recovery_journal) == Some(journal_path)
    }) || app.file_transfers.pending_by_session.values().any(|pending| {
        pending
            .retry_plan
            .as_ref()
            .and_then(|retry| retry.recovery_journal_path.as_deref())
            == Some(journal_path)
    })
}

/// Keep the review queue as a presentation/execution cache of the current
/// durable records. A record that becomes recoverable after being live must
/// not require an application restart, while records that were resolved or
/// became unreadable must not leave stale executable snapshots behind.
fn sync_recovery_review_queue(
    app: &mut AppState,
    recoveries: Vec<super::file_task_runtime::StartupFileTaskRecovery>,
) {
    let previous = app
        .file_transfers
        .recovery_queued
        .drain(..)
        .filter_map(|job| {
            let path = queued_recovery_journal(&job)?.to_path_buf();
            Some((path, job))
        })
        .collect::<BTreeMap<_, _>>();
    let mut previous = previous;
    let mut rebuilt = VecDeque::new();

    for recovery in recoveries {
        if app.recovery_ui.discard_in_flight.contains(&recovery.journal_path) {
            continue;
        }
        if recovery_is_already_runnable_or_active(app, &recovery.journal_path) {
            continue;
        }
        let Some(destination_dir) = recovery.destination_dir else {
            continue;
        };
        let old = previous.remove(&recovery.journal_path);
        rebuilt.push_back(QueuedFileTransfer {
            queue_id: old
                .as_ref()
                .map_or_else(super::app::next_file_transfer_queue_id, |job| job.queue_id),
            clipboard: recovery.clipboard,
            clipboard_owner_generation: old
                .as_ref()
                .and_then(|job| job.clipboard_owner_generation),
            destination_dir,
            enqueue_plan: recovery.retry_plan.plan.clone(),
            retry_plan: Some(recovery.retry_plan),
            recovered: true,
        });
    }

    app.file_transfers.recovery_queued = rebuilt;
    app.sync_file_transfer_queue_surfaces();
}

/// Cheap periodic observation of worker-owned progress; journal replay runs
/// separately at a lower cadence. No source or destination directory walk.
pub fn refresh_live_observation(app: &mut AppState) -> bool {
    let remote = match &app.active_overlay {
        ActiveOverlay::FileTaskProgress(session) if session.is_recovery_monitor() => session.remote_worker.clone(),
        _ => None,
    };
    let mut monitor_snapshot = None;
    let mut changed = false;
    for entry in &mut app.recovery_ui.entries {
        if !entry.is_live() { continue; }
        let identity = entry.worker_ref();
        let next = identity.as_ref().and_then(super::file_task_live::read_snapshot);
        if next != entry.live_snapshot {
            entry.live_snapshot = next.clone();
            changed = true;
        }
        if identity == remote && entry.has_observed_control() { monitor_snapshot = next; }
        // The UI must repaint a stalled-age label as wall time advances even
        // when the producer has not emitted a new snapshot.
        if entry.is_stalled() { changed = true; }
    }
    // A missing/corrupt/pre-#81 feed is NOT a 0-byte transfer. Journal
    // checkpoints are independent of that feed and remain useful.
    if let Some(entry) = app.recovery_ui.entries.iter().find(|entry| {
        entry.is_live() && entry.worker_ref() == remote
    }) {
        let checkpoint = (!entry.has_observed_control()).then(|| journal_checkpoint(entry));
        let controls = entry.has_observed_control();
        if let ActiveOverlay::FileTaskProgress(session) = &mut app.active_overlay {
            if session.is_recovery_monitor() {
                changed |= session.progress.set_recovery_feed_state(checkpoint, controls);
            }
        }
    }
    if let Some(snapshot) = monitor_snapshot {
        if let ActiveOverlay::FileTaskProgress(session) = &mut app.active_overlay {
            if session.is_recovery_monitor()
                && (session.progress.phase != snapshot.phase
                    || session.progress.status != snapshot.status
                    || session.progress.current_item != snapshot.current_item
                    || session.progress.totals != snapshot.totals
                    || session.progress.rate_bytes_per_sec != snapshot.rate_bytes_per_sec)
            {
                session.progress.apply_update(tui_file_picker::FileTaskProgressUpdate::Snapshot {
                    phase: snapshot.phase, status: snapshot.status,
                    current_item: snapshot.current_item, totals: snapshot.totals,
                    rate_bytes_per_sec: snapshot.rate_bytes_per_sec,
                });
                changed = true;
            }
        }
    }
    // Conflict prompts outlive individual progress frames. Apply only changes
    // in request identity to avoid resetting the viewer's selection/checkbox.
    // Same-request metadata updates are delivered through the existing reducer.
    if let Some(snapshot) = app.recovery_ui.entries.iter()
        .filter(|entry| entry.is_live() && entry.worker_ref() == remote)
        .find_map(|entry| entry.live_snapshot.as_ref().filter(|_| entry.has_observed_control()))
    {
        if let ActiveOverlay::FileTaskProgress(session) = &mut app.active_overlay {
            let current = session.progress.conflict.as_ref().map(|c| c.request_id);
            let new = snapshot.conflict.as_ref().map(|c| c.request_id);
            if current != new {
                if let Some(conflict) = snapshot.conflict.clone() {
                    session.progress.apply_update(tui_file_picker::FileTaskProgressUpdate::ShowConflict { conflict });
                } else {
                    session.progress.apply_update(tui_file_picker::FileTaskProgressUpdate::ClearConflict);
                }
                changed = true;
            } else if let (Some(old), Some(new)) = (session.progress.conflict.as_ref(), snapshot.conflict.as_ref()) {
                if new.existing_size.is_some() && (old.existing_size != new.existing_size
                    || old.existing_modified != new.existing_modified)
                {
                    session.progress.apply_update(tui_file_picker::FileTaskProgressUpdate::UpdateConflictExistingStats {
                        request_id: new.request_id,
                        size: new.existing_size.unwrap_or_default(),
                        modified: new.existing_modified,
                    });
                    changed = true;
                }
            }
        }
    }
    // A minimized remote monitor is still a live progress observer. Keep its
    // footer snapshot current, even though no progress overlay is visible.
    if let Some(session) = app.minimized_file_task_progress.as_mut().filter(|s| s.is_recovery_monitor()) {
        if let Some(entry) = app.recovery_ui.entries.iter().find(|entry| {
            entry.is_live() && entry.worker_ref() == session.remote_worker
        }) {
            let checkpoint = (!entry.has_observed_control()).then(|| journal_checkpoint(entry));
            changed |= session.progress.set_recovery_feed_state(checkpoint, entry.has_observed_control());
            if let Some(snapshot) = entry.live_snapshot.as_ref() {
                if session.progress.phase != snapshot.phase || session.progress.status != snapshot.status
                    || session.progress.current_item != snapshot.current_item
                    || session.progress.totals != snapshot.totals
                    || session.progress.rate_bytes_per_sec != snapshot.rate_bytes_per_sec
                {
                    session.progress.apply_update(tui_file_picker::FileTaskProgressUpdate::Snapshot {
                        phase: snapshot.phase.clone(), status: snapshot.status.clone(),
                        current_item: snapshot.current_item.clone(), totals: snapshot.totals.clone(),
                        rate_bytes_per_sec: snapshot.rate_bytes_per_sec,
                    });
                    changed = true;
                }
                let current = session.progress.conflict.as_ref().map(|c| c.request_id);
                let next = snapshot.conflict.as_ref().map(|c| c.request_id);
                if current != next {
                    if let Some(conflict) = snapshot.conflict.clone() {
                        session.progress.apply_update(tui_file_picker::FileTaskProgressUpdate::ShowConflict { conflict });
                    } else { session.progress.apply_update(tui_file_picker::FileTaskProgressUpdate::ClearConflict); }
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Once the worker releases its durable lease, use precisely the established
/// discard implementation. A running process is never cleaned up underfoot.
pub fn advance_stop_and_undo(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let pending = app.recovery_ui.stop_and_undo_pending.iter().cloned().collect::<Vec<_>>();
    for path in pending {
        let selected = app.recovery_ui.entries.iter().find(|entry| entry.journal_path == path);
        let Some(entry) = selected else {
            app.recovery_ui.stop_and_undo_pending.remove(&path);
            app.recovery_ui.stop_requested_at.remove(&path);
            app.set_status("Stopped transfer record is no longer recoverable; completed output may remain on disk");
            continue;
        };
        if entry.is_live() || app.recovery_ui.discard_in_flight.contains(&path) { continue; }
        if !entry.can_discard() {
            // A corrupt or changed record is not silently deleted. Stop is
            // complete, but its recovery decision still requires review.
            app.recovery_ui.stop_and_undo_pending.remove(&path);
            app.recovery_ui.stop_requested_at.remove(&path);
            app.set_status("Worker stopped, but automatic cleanup is not safe; review Interrupted jobs before changing files");
            continue;
        }
        let Some(version) = entry.version else { continue; };
        app.recovery_ui.stop_and_undo_pending.remove(&path);
        app.recovery_ui.stop_requested_at.remove(&path);
        app.recovery_ui.discard_in_flight.insert(path.clone());
        commit_discard_choices_in_session(app, &BTreeSet::from([path.clone()]));
        let tx = tx.clone();
        std::thread::spawn(move || {
            let result = super::keybindings::discard_file_transfer_recovery(&path, version);
            let _ = tx.blocking_send(AppMessage::RecoveryDiscardComplete { journal_path: path, result });
        });
    }
}

pub fn reconcile_progress_watch(app: &mut AppState) {
    // A minimized watcher does not own a worker and is not a durable record.
    // Retire its meter when the remote job stops; the journal inventory then
    // determines whether a decision indicator must replace it.
    if app.minimized_file_task_progress.as_ref().is_some_and(|session| {
        session.is_recovery_monitor()
            && !app.recovery_ui.entries.iter().any(|entry| entry.is_live()
                && entry.worker_ref() == session.remote_worker)
    }) {
        app.minimized_file_task_progress = None;
    }
    let remote = match &app.active_overlay {
        ActiveOverlay::FileTaskProgress(session) if session.is_recovery_monitor() => session.remote_worker.clone(),
        _ => None,
    };
    let Some(remote) = remote else { return; };
    if app.recovery_ui.entries.iter().any(|entry| entry.is_live()
        && entry.worker_ref().as_ref() == Some(&remote)) { return; }
    return_from_progress_watch(app);
    app.set_status("The worker is no longer running; review its final state and any remaining cleanup");
}

pub fn return_from_progress_watch(app: &mut AppState) {
    app.recovery_ui.open_window();
    app.active_overlay = ActiveOverlay::FileRecovery;
}

pub fn open_stop_confirm_from_progress_watch(app: &mut AppState, remote: &LiveWorkerRef) {
    if let Some(index) = app.recovery_ui.entries.iter().position(|entry| {
        entry.worker_ref().as_ref() == Some(remote) && entry.has_observed_control()
    }) {
        app.recovery_ui.selected = index;
        app.recovery_ui.surface = RecoverySurface::StopConfirm;
        app.recovery_ui.focus = 1;
    } else {
        app.recovery_ui.open_window();
        app.set_status("This transfer changed since it was opened; review its current state in Interrupted jobs");
    }
    app.active_overlay = ActiveOverlay::FileRecovery;
}

/// Only durable journal root outcomes are shown without a live feed. Multiple
/// checkpoints for the same root must not count as multiple completed files.
fn journal_checkpoint(entry: &RecoveryEntry) -> (String, String) {
    let Some(record) = entry.record.as_ref() else {
        return ("Journal checkpoint unreadable".into(),
            "Open Recovery details for the recorded error".into());
    };
    let mut last_roots = BTreeMap::new();
    for root in &record.roots {
        last_roots.insert((root.source.clone(), root.destination.clone()), root.disposition);
    }
    let completed = last_roots.values().filter(|state| state.is_completed()).count();
    let skipped = last_roots.values().filter(|state|
        **state == tui_file_picker::FileTaskRootDisposition::Skipped).count();
    let failed = last_roots.values().filter(|state|
        **state == tui_file_picker::FileTaskRootDisposition::Failed).count();
    let total = record.mappings.len();
    (
        format!("Journal: {completed}/{total} roots completed · {skipped} skipped · {failed} failed"),
        format!("Last journal status: {}", record.last_status.as_deref().unwrap_or("not recorded")),
    )
}

fn watch_selected_progress(app: &mut AppState) {
    let Some(entry) = app.recovery_ui.selected_entry().filter(|entry| entry.is_live()) else {
        app.set_status("This transfer is no longer live; refresh Recovery before watching");
        return;
    };
    let Some(remote) = entry.worker_ref() else {
        app.set_status("The running worker's observation identity cannot be verified");
        return;
    };
    let Some(record) = entry.record.as_ref() else { return; };
    let mut progress = tui_file_picker::FileTaskProgressState::new(
        if record.is_native_extraction() { tui_file_picker::FileTaskKind::Extract }
        else if record.is_move { tui_file_picker::FileTaskKind::Move }
        else { tui_file_picker::FileTaskKind::Copy },
        format!("{} (reconnected)", if record.is_native_extraction() { "Extraction" }
            else if record.is_move { "Move" } else { "Copy" }),
        super::keybindings::file_picker_theme_from_theme(&app.theme),
    );
    let mut scope = tui_file_picker::FileTaskScope::new(summarize_sources(record));
    scope.source_root = record.mappings.first().map(|m| m.source.clone());
    scope.destination = record.admitted_destination_root.clone();
    progress.set_scope(scope);
    if let Some(snapshot) = entry.live_snapshot.as_ref().filter(|_| entry.has_observed_control()) {
        progress.apply_update(tui_file_picker::FileTaskProgressUpdate::Snapshot {
            phase: snapshot.phase.clone(), status: snapshot.status.clone(),
            current_item: snapshot.current_item.clone(), totals: snapshot.totals,
            rate_bytes_per_sec: snapshot.rate_bytes_per_sec,
        });
        if let Some(conflict) = snapshot.conflict.clone() {
            progress.apply_update(tui_file_picker::FileTaskProgressUpdate::ShowConflict { conflict });
        }
    }
    let checkpoint = entry.live_snapshot.is_none().then(|| journal_checkpoint(entry));
    let controls = entry.has_observed_control();
    let mut session = super::app::FileTaskProgressSession::recovery_monitor(progress, remote);
    session.progress.set_recovery_feed_state(checkpoint, controls);
    app.active_overlay = ActiveOverlay::FileTaskProgress(session);
}

fn request_running_control(app: &mut AppState, control: super::app::FileTaskControl) {
    let Some(remote) = app.recovery_ui.selected_entry().filter(|entry| entry.has_observed_control())
        .and_then(RecoveryEntry::worker_ref) else {
        app.set_status("Worker has no verified progress/control feed; this command is unavailable");
        return;
    };
    if !super::file_task_live::read_snapshot(&remote)
        .is_some_and(|snapshot| snapshot.worker.appears_active()) {
        app.set_status("The worker feed disappeared; this command was not sent");
        return;
    }
    match super::file_task_live::request_control(&remote, control) {
        Ok(()) => app.set_status("Request sent; watch worker progress for acknowledgment"),
        Err(error) => app.set_status(format!("Cannot send worker control: {error}")),
    }
}

fn open_safety_confirm(app: &mut AppState, surface: RecoverySurface) {
    app.recovery_ui.surface = surface;
    app.recovery_ui.focus = 1; // cancellation is the safe default
    app.recovery_ui.details_scroll = 0;
}

fn activate_safety_confirmation(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    if app.recovery_ui.focus != 0 { app.recovery_ui.open_window(); return; }
    match app.recovery_ui.surface {
        RecoverySurface::StopConfirm => {
            let entry = app.recovery_ui.selected_entry();
            let Some((path, remote)) = entry.filter(|e| e.has_observed_control())
                .and_then(|e| Some((e.journal_path.clone(), e.worker_ref()?))) else {
                app.recovery_ui.open_window();
                app.set_status("Worker no longer live; reopen Interrupted jobs before stopping");
                return;
            };
            if !super::file_task_live::read_snapshot(&remote)
                .is_some_and(|snapshot| snapshot.worker.appears_active()) {
                app.recovery_ui.open_window();
                app.set_status("The worker feed disappeared; Stop was not sent");
                return;
            }
            match super::file_task_live::request_control(&remote, super::app::FileTaskControl::Abort) {
                Ok(()) => {
                    app.recovery_ui.stop_and_undo_pending.insert(path.clone());
                    app.recovery_ui.stop_requested_at.insert(path, unix_ms_now());
                    app.recovery_ui.open_window();
                    app.set_status("Stop requested; once the worker exits, its unfinished paths will be cleaned. Finished output stays in place.");
                }
                Err(error) => { app.recovery_ui.open_window(); app.set_status(format!("Stop request failed; worker still running: {error}")); }
            }
        }
        RecoverySurface::ForceStopConfirm => {
            let entry = app.recovery_ui.selected_entry();
            let Some((path, remote, worker)) = entry.filter(|e| e.is_live())
                .and_then(|e| Some((e.journal_path.clone(), e.worker_ref()?, e.live_snapshot.as_ref()?.worker))) else {
                app.recovery_ui.open_window();
                app.set_status("The worker identity cannot be verified; no process was signaled");
                return;
            };
            // Re-read generation-bound snapshot; only the current worker gets a signal.
            let result = super::file_task_live::read_snapshot(&remote)
                .filter(|fresh| fresh.worker == worker && worker.appears_active())
                .ok_or_else(|| "Worker identity changed; no process was signaled".to_string())
                .and_then(|_| super::file_task_live::force_stop(worker));
            app.recovery_ui.open_window();
            match result {
                Ok(()) => {
                    app.recovery_ui.stop_and_undo_pending.insert(path.clone());
                    app.recovery_ui.stop_requested_at.insert(path, unix_ms_now());
                    app.set_status("Identity-checked force stop sent; cleanup starts only after the worker releases its lease");
                }
                Err(error) => app.set_status(format!("Could not force stop worker: {error}")),
            }
        }
        RecoverySurface::SetAsideConfirm => {
            let Some(path) = app.recovery_ui.selected_entry().filter(|entry| entry.is_unreadable())
                .map(|entry| entry.journal_path.clone()) else {
                app.recovery_ui.open_window();
                app.set_status("This record is not unreadable; review its current state");
                return;
            };
            if !app.recovery_ui.aside_in_flight.insert(path.clone()) { return; }
            app.recovery_ui.open_window();
            app.set_status("Checking ownership and preserving the unreadable record before releasing reservations");
            let tx = tx.clone();
            std::thread::spawn(move || {
                let result = super::file_task_runtime::set_aside_unreadable_recovery(&path);
                let _ = tx.blocking_send(AppMessage::RecoverySetAsideComplete { journal_path: path, result });
            });
        }
        _ => app.recovery_ui.open_window(),
    }
}

pub fn complete_set_aside(app: &mut AppState, journal_path: PathBuf, result: Result<PathBuf,String>) {
    app.recovery_ui.aside_in_flight.remove(&journal_path);
    match result {
        Ok(saved) => {
            refresh_from_runtime(app);
            if app.recovery_ui.entries.is_empty() && matches!(app.active_overlay, ActiveOverlay::FileRecovery) {
                app.active_overlay = ActiveOverlay::None;
            } else { app.recovery_ui.open_window(); }
            app.set_status(format!("Unreadable record preserved at {}; its proved stale reservations were released. Source and destination files were not altered",saved.display()));
        }
        Err(error) => {
            refresh_from_runtime(app);
            app.recovery_ui.open_window();
            app.set_status(format!("Could not release blocked paths safely: {error}"));
        }
    }
}

pub fn refresh_from_runtime(app: &mut AppState) {
    let super::file_task_runtime::StartupFileTaskRecoveryInventory {
        recoveries,
        recovery_surface_records,
        ..
    } = super::file_task_runtime::startup_file_task_recovery_inventory();
    sync_recovery_review_queue(app, recoveries);
    app.file_transfers.unresolved_recovery_count =
        super::file_task_runtime::recovery_reservation_count(&recovery_surface_records);
    app.recovery_ui.replace_records(
        recovery_surface_records,
        &app.file_transfers.recovery_queued,
    );

    // Once Resume has been approved, the same durable record can briefly be
    // RecoveryReserved while its job waits in the serial queue or hands off to
    // the helper. Keep showing the unresolved record, but do not let a second
    // destructive decision race the already-approved execution.
    let resume_committed = app
        .file_transfers
        .queued
        .iter()
        .filter_map(|job| job.transfer().and_then(queued_recovery_journal))
        .chain(app.file_transfers.pending_by_session.values().filter_map(|pending| {
            pending
                .retry_plan
                .as_ref()
                .and_then(|retry| retry.recovery_journal_path.as_deref())
        }))
        .map(|path| path.to_path_buf())
        .collect::<BTreeSet<_>>();
    for entry in &mut app.recovery_ui.entries {
        entry.resume_committed = resume_committed.contains(&entry.journal_path);
        entry.approved_resume_queue_id = app.file_transfers.queued.iter()
            .find(|job| job.transfer().and_then(queued_recovery_journal)
                == Some(entry.journal_path.as_path()))
            .map(|job| job.queue_id());
    }
}

/// Startup only presents the status-bar notice. The first screen is never a modal.
pub fn maybe_open_startup_prompt(_app: &mut AppState) {}

pub fn hide_startup_notice(app: &mut AppState) {
    app.recovery_ui.consume_startup_prompt();
}

pub fn open_recovery_window(app: &mut AppState) {
    refresh_from_runtime(app);
    app.recovery_ui.open_window();
    app.recovery_ui.small_job_tab = false;
    app.active_overlay = ActiveOverlay::FileRecovery;
}

pub fn selected_queue_id(app: &AppState) -> Option<u64> {
    app.recovery_ui.selected_entry().and_then(|entry| entry.queue_id)
}

pub fn select_queue_id(app: &mut AppState, queue_id: Option<u64>) -> bool {
    let Some(queue_id) = queue_id else {
        return !app.recovery_ui.entries.is_empty();
    };
    if let Some(index) = app
        .recovery_ui
        .entries
        .iter()
        .position(|entry| entry.queue_id == Some(queue_id))
    {
        app.recovery_ui.selected = index;
        true
    } else {
        false
    }
}

pub fn open_discard_for_queue_id(app: &mut AppState, queue_id: Option<u64>) {
    refresh_from_runtime(app);
    if !select_queue_id(app, queue_id) {
        app.set_status("No matching interrupted operation is available");
        return;
    }
    app.active_overlay = ActiveOverlay::FileRecovery;
    open_selected_discard_confirm(app);
}

pub fn open_details_for_queue_id(
    app: &mut AppState,
    queue_id: Option<u64>,
    tx: &mpsc::Sender<AppMessage>,
) {
    refresh_from_runtime(app);
    if !select_queue_id(app, queue_id) {
        app.set_status("No matching interrupted operation is available");
        return;
    }
    app.active_overlay = ActiveOverlay::FileRecovery;
    open_selected_details(app, tx, RecoveryDetailsReason::Ordinary);
}

pub fn open_blocked_refusal(app: &mut AppState, attempted: &Path) -> bool {
    refresh_from_runtime(app);
    if !app.recovery_ui.open_refusal(attempted) {
        return false;
    }
    app.active_overlay = ActiveOverlay::FileRecovery;
    true
}

/// Convert the low-level shared-mutation conflict into the operator-specified
/// recovery surface and plain user language. The original diagnostic remains
/// available to logging at its source; it is not repeated in the UI.
pub fn intercept_recovery_conflict_status(app: &mut AppState, message: &str) -> bool {
    if !message.contains("filesystem mutation conflicts with recovery reservation") {
        return false;
    }
    // The refusal can arrive from a worker before the next ordinary footer
    // refresh. Re-read only the small control-plane records here so the path
    // in the refusal always has a chance to resolve to its owning operation.
    refresh_from_runtime(app);
    if app.recovery_ui.open_refusal_from_status(message) {
        app.active_overlay = ActiveOverlay::FileRecovery;
        true
    } else {
        false
    }
}

/// Request source-derived unfinished-path sizes while the job is visible;
/// never block the event loop or assume that a folder is an empty file.
pub(super) fn refresh_visible_sizes(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    if matches!(app.active_overlay, ActiveOverlay::FileRecovery) {
        request_incomplete_sizes_if_needed(app, tx);
    }
}

fn measured_incomplete_path_size(path: &Path) -> Option<u64> {
    use std::time::{Duration, Instant};
    let began = Instant::now();
    let mut remaining = vec![path.to_path_buf()];
    let mut total = 0u64;
    let mut visited = 0usize;
    while let Some(path) = remaining.pop() {
        visited = visited.checked_add(1)?;
        // A directory with a pathological number of descendants must not
        // occupy a background worker indefinitely. Unknown is honest.
        if visited > 50_000 || began.elapsed() > Duration::from_millis(350) {
            return None;
        }
        let meta = std::fs::symlink_metadata(&path).ok()?;
        if meta.is_file() {
            total = total.checked_add(meta.len())?;
        } else if meta.is_dir() {
            for child in std::fs::read_dir(&path).ok()? {
                remaining.push(child.ok()?.path());
            }
        } else {
            // Never follow a symlink out of the destination's private
            // unfinished tree or pretend that its target was measured.
            return None;
        }
    }
    Some(total)
}

fn request_incomplete_sizes_if_needed(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let Some(entry) = app.recovery_ui.selected_entry() else {
        return;
    };
    let journal_path = entry.journal_path.clone();
    let paths = entry
        .incomplete_paths()
        .into_iter()
        .filter(|path| !app.recovery_ui.incomplete_sizes.contains_key(path))
        .collect::<Vec<_>>();
    if paths.is_empty() || !app.recovery_ui.size_requests.insert(journal_path.clone()) {
        return;
    }
    let tx = tx.clone();
    std::thread::spawn(move || {
        let sizes = paths
            .into_iter()
            .map(|path| {
                let size = measured_incomplete_path_size(&path);
                (path, size)
            })
            .collect();
        let _ = tx.blocking_send(AppMessage::RecoveryIncompleteSizesLoaded {
            journal_path,
            sizes,
        });
    });
}

pub fn complete_incomplete_sizes(
    app: &mut AppState,
    journal_path: PathBuf,
    sizes: Vec<(PathBuf, Option<u64>)>,
) {
    app.recovery_ui.size_requests.remove(&journal_path);
    if !app
        .recovery_ui
        .entries
        .iter()
        .any(|entry| entry.journal_path == journal_path)
    {
        return;
    }
    for (path, size) in sizes {
        app.recovery_ui.incomplete_sizes.insert(path, size);
    }
}

fn selected_discard_snapshot(app: &AppState) -> Option<(PathBuf, RecoveryRecordVersion)> {
    let entry = app.recovery_ui.selected_entry()?;
    if !entry.can_discard()
        || app
            .recovery_ui
            .discard_in_flight
            .contains(&entry.journal_path)
    {
        return None;
    }
    Some((entry.journal_path.clone(), entry.version?))
}

fn commit_discard_choices_in_session(app: &mut AppState, paths: &BTreeSet<PathBuf>) {
    // A confirmed discard and a reviewed resume are mutually exclusive session
    // decisions. Remove the corresponding review jobs immediately so command
    // mode cannot promote one while the discard worker is acquiring its durable
    // lease. A failed discard refresh reconstructs the review job from disk.
    app.file_transfers.recovery_queued.retain(|job| {
        queued_recovery_journal(job)
            .map(|path| !paths.contains(path))
            .unwrap_or(true)
    });
    for entry in &mut app.recovery_ui.entries {
        if paths.contains(&entry.journal_path) {
            entry.queue_id = None;
            entry.discard_committed = true;
        }
    }
}

fn begin_single_discard(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let Some((journal_path, version)) = selected_discard_snapshot(app) else {
        app.set_status("Undo is unavailable for the current job state");
        return;
    };
    app.recovery_ui
        .discard_in_flight
        .insert(journal_path.clone());
    commit_discard_choices_in_session(app, &BTreeSet::from([journal_path.clone()]));
    let tx = tx.clone();
    std::thread::spawn(move || {
        let result = super::keybindings::discard_file_transfer_recovery(&journal_path, version);
        let _ = tx.blocking_send(AppMessage::RecoveryDiscardComplete {
            journal_path,
            result,
        });
    });
}

fn begin_bulk_discard(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let work = app
        .recovery_ui
        .entries
        .iter()
        .filter(|entry| entry.needs_attention() && entry.can_discard())
        .filter_map(|entry| Some((entry.journal_path.clone(), entry.version?)))
        .filter(|(path, _)| !app.recovery_ui.discard_in_flight.contains(path))
        .collect::<Vec<_>>();
    if work.len() < 2 || app.recovery_ui.bulk_discard_count() == 0 {
        app.set_status("Undo all is available only when every waiting job supports Undo");
        return;
    }
    let paths = work
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<BTreeSet<_>>();
    app.recovery_ui
        .discard_in_flight
        .extend(paths.iter().cloned());
    commit_discard_choices_in_session(app, &paths);
    let tx = tx.clone();
    std::thread::spawn(move || {
        for (journal_path, version) in work {
            let result = super::keybindings::discard_file_transfer_recovery(&journal_path, version);
            if tx
                .blocking_send(AppMessage::RecoveryDiscardComplete {
                    journal_path,
                    result,
                })
                .is_err()
            {
                break;
            }
        }
    });
}

pub fn complete_discard(
    app: &mut AppState,
    journal_path: PathBuf,
    result: Result<super::file_task_runtime::RecoveryDiscardSummary, String>,
) {
    app.recovery_ui.discard_in_flight.remove(&journal_path);
    match result {
        Ok(summary) => {
            refresh_from_runtime(app);
            if app.recovery_ui.entries.is_empty() {
                if matches!(app.active_overlay, ActiveOverlay::FileRecovery) {
                    app.active_overlay = ActiveOverlay::None;
                }
                app.set_status(format!(
                    "Undo complete: {} unfinished path{} deleted, {} original name{} restored",
                    summary.deleted_incomplete,
                    plural(summary.deleted_incomplete),
                    summary.restored_sources,
                    plural(summary.restored_sources),
                ));
            } else {
                app.recovery_ui.open_window();
                app.set_status(format!(
                    "Undo complete; {} locked path{} released",
                    summary.released_paths,
                    plural(summary.released_paths),
                ));
            }
        }
        Err(error) => {
            refresh_from_runtime(app);
            if app.recovery_ui.entries.is_empty() {
                if matches!(app.active_overlay, ActiveOverlay::FileRecovery) {
                    app.active_overlay = ActiveOverlay::None;
                }
            } else {
                app.recovery_ui.open_window();
                app.active_overlay = ActiveOverlay::FileRecovery;
            }
            app.set_status(format!(
                "Undo did not complete: {}",
                user_safe_discard_error(&error)
            ));
        }
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn resume_selected(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    if let Some(entry) = app.recovery_ui.selected_entry().filter(|entry| {
        entry.can_resume() && entry.record.as_ref().is_some_and(DurableFileTaskRecord::is_native_extraction)
    }) {
        let path = entry.journal_path.clone();
        app.recovery_ui.minimize();
        app.active_overlay = ActiveOverlay::None;
        match super::native_extract_task::resume_from_journal(app, &path, tx) {
            Ok(()) => app.set_status("Finishing extraction in the file-task worker"),
            Err(error) => {
                app.recovery_ui.open_window();
                app.active_overlay = ActiveOverlay::FileRecovery;
                app.set_status(format!("Cannot finish extraction: {error}"));
            }
        }
        return;
    }
    let Some(queue_id) = app
        .recovery_ui
        .selected_entry()
        .and_then(|entry| entry.queue_id)
    else {
        app.set_status("This interrupted job cannot be finished automatically");
        return;
    };
    // The serial scheduler intentionally will not launch underneath an
    // unrelated modal. Explicit Resume therefore dismisses this review layer
    // before promoting the exact durable retry. The live progress surface may
    // then take ownership normally.
    app.recovery_ui.minimize();
    app.active_overlay = ActiveOverlay::None;
    super::keybindings::resume_file_transfer_recovery(app, Some(queue_id), tx);
    refresh_from_runtime(app);
}

fn resume_queue_ids(app: &mut AppState, ids: Vec<u64>, tx: &mpsc::Sender<AppMessage>) {
    if ids.is_empty() {
        app.set_status("No interrupted jobs are ready to finish");
        return;
    }
    app.recovery_ui.minimize();
    app.active_overlay = ActiveOverlay::None;
    super::keybindings::resume_file_transfer_recoveries(app, &ids, tx);
    refresh_from_runtime(app);
}

fn resume_all(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    if !app.recovery_ui.resume_all_available() {
        app.set_status("Finish all is unavailable while a job needs a decision");
        return;
    }
    let ids = app
        .recovery_ui
        .entries
        .iter()
        .filter_map(|entry| entry.queue_id)
        .collect::<Vec<_>>();
    resume_queue_ids(app, ids, tx);
}

// Single implementation of the interrupted-jobs UI.
include!("interrupted_jobs_view.rs");

fn summarize_sources(record: &DurableFileTaskRecord) -> String {
    match record.mappings.as_slice() {
        [] => "unavailable".to_string(),
        [one] => one.source.display().to_string(),
        many => {
            let parent = many[0].source.parent();
            if parent.is_some()
                && many
                    .iter()
                    .all(|mapping| mapping.source.parent() == parent)
            {
                format!(
                    "{} ({} items)",
                    parent.unwrap_or_else(|| Path::new(".")).display(),
                    many.len()
                )
            } else {
                format!("{} selected locations", many.len())
            }
        }
    }
}

fn summarize_destination(record: &DurableFileTaskRecord) -> String {
    if let Some(root) = record.admitted_destination_root.as_ref() {
        return root.display().to_string();
    }
    record
        .mappings
        .first()
        .and_then(|mapping| mapping.destination.parent())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unavailable".to_string())
}

fn entries_share_folder(left: &RecoveryEntry, right: &RecoveryEntry) -> bool {
    let (Some(a), Some(b)) = (left.record.as_ref(), right.record.as_ref()) else { return false; };
    let mut a_paths = a.mappings.iter().flat_map(|m| [&m.source, &m.destination]).collect::<Vec<_>>();
    let mut b_paths = b.mappings.iter().flat_map(|m| [&m.source, &m.destination]).collect::<Vec<_>>();
    if let Some(root) = a.admitted_destination_root.as_ref() { a_paths.push(root); }
    if let Some(root) = b.admitted_destination_root.as_ref() { b_paths.push(root); }
    a_paths.iter().any(|a_path| b_paths.iter().any(|b_path| paths_overlap(a_path, b_path)))
}

/// An overlapping record remains an ordering predecessor until its work and
/// cleanup are finished, even after Finish was approved or the worker started.
/// A clean completed record left on screen for Dismiss is no longer a blocker.
fn unresolved_overlap_predecessor(entry: &RecoveryEntry) -> bool {
    entry.record.is_some()
        && (entry.is_live() || entry.resume_committed || entry.discard_committed
            || !entry.is_completed_clean())
}

/// The queue command and window share this eligibility check. In particular,
/// an approved but not yet dispatched older retry still blocks the newer one.
pub(super) fn resume_has_older_overlap(app: &AppState, journal_path: &Path) -> bool {
    app.recovery_ui.entries.iter()
        .find(|entry| entry.journal_path == journal_path)
        .is_some_and(|entry| older_interrupted_job(&app.recovery_ui.entries, entry).is_some())
}

fn older_interrupted_job(entries: &[RecoveryEntry], selected: &RecoveryEntry) -> Option<usize> {
    let current = selected.record.as_ref()?;
    entries.iter().enumerate().filter(|(_, entry)| {
        entry.journal_path != selected.journal_path && unresolved_overlap_predecessor(entry)
            && entries_share_folder(selected, entry)
    }).filter(|(_, entry)| {
        let other = entry.record.as_ref().unwrap();
        (other.created_unix_ms, entry.journal_path.as_path())
            < (current.created_unix_ms, selected.journal_path.as_path())
    }).min_by_key(|(_, entry)| {
        let other = entry.record.as_ref().unwrap();
        (other.created_unix_ms, entry.journal_path.clone())
    }).map(|(index, _)| index)
}

fn newer_interrupted_job(entries: &[RecoveryEntry], selected: &RecoveryEntry) -> Option<usize> {
    let current = selected.record.as_ref()?;
    entries.iter().enumerate().filter(|(_, entry)| {
        entry.journal_path != selected.journal_path && unresolved_overlap_predecessor(entry)
            && entries_share_folder(selected, entry)
    }).filter(|(_, entry)| {
        let other = entry.record.as_ref().unwrap();
        (other.created_unix_ms, entry.journal_path.as_path())
            > (current.created_unix_ms, selected.journal_path.as_path())
    }).min_by_key(|(_, entry)| {
        let other = entry.record.as_ref().unwrap();
        (other.created_unix_ms, entry.journal_path.clone())
    }).map(|(index, _)| index)
}

fn entry_has_destination_overlap(entries: &[RecoveryEntry], selected: &RecoveryEntry) -> bool {
    entries.iter().filter(|other| other.journal_path != selected.journal_path)
        .any(|other| unresolved_overlap_predecessor(other) && entries_share_folder(selected, other))
}

#[cfg(test)]
fn overlapping_entry_count(entries: &[RecoveryEntry], selected: &RecoveryEntry) -> usize {
    entries.iter().filter(|other| other.journal_path != selected.journal_path)
        .filter(|other| unresolved_overlap_predecessor(other) && entries_share_folder(selected, other)).count()
}

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn age_label(updated_unix_ms: u64) -> String {
    let seconds = unix_ms_now().saturating_sub(updated_unix_ms) / 1000;
    match seconds {
        0..=59 => format!("{seconds}s ago"),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86_399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1000.0;
    const MB: f64 = KB * 1000.0;
    const GB: f64 = MB * 1000.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.1} GB", value / GB)
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

fn user_safe_discard_error(raw: &str) -> String {
    let lowered = raw.to_ascii_lowercase();
    let prohibited = [
        "journal",
        "descriptor",
        "recoveryreserved",
        "recovery reservation",
        "reservation",
        "quarantine",
        "paths claimed",
        "temp artifact",
        "disposition",
        "unreconstruct",
        "lifecycle",
        "admitted mapping",
        "owner gone",
        "parked",
        "sources set aside",
    ];
    if prohibited.iter().any(|term| lowered.contains(term)) {
        log::warn!("internal interrupted-operation discard detail hidden from UI: {raw}");
        "The saved operation changed or could not be taken over safely; review it again."
            .to_string()
    } else {
        raw.to_string()
    }
}

fn user_safe_diagnostic(raw: &str) -> String {
    // This is the only saved explanation for an unreadable record. Translate
    // implementation nouns without discarding the distinguishing I/O or parse
    // failure (permission denied, unexpected EOF, malformed data, etc.).
    let message = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if message.is_empty() {
        return "No read error details were saved for this record.".into();
    }
    message
        .replace("RecoveryReserved", "waiting for review")
        .replace("owner gone", "worker no longer present")
        .replace("Journal", "Saved record")
        .replace("journal", "saved record")
        .replace("Descriptor", "information")
        .replace("descriptor", "information")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_record(is_move: bool, source: &str, destination: &str) -> DurableFileTaskRecord {
        let mappings = vec![tui_file_picker::PasteMapping {
            source: PathBuf::from(source),
            destination: PathBuf::from(destination),
        }];
        DurableFileTaskRecord {
            schema: crate::tui::file_task_runtime::FILE_TASK_JOURNAL_SCHEMA,
            job_id: format!("test-{source}-{destination}"),
            generation: 1,
            session_id: 1,
            created_unix_ms: 1,
            updated_unix_ms: 1,
            lifecycle: crate::tui::file_task_runtime::DurableFileTaskLifecycle::AwaitingReconciliation,
            is_move,
            verification: tui_file_picker::VerificationMode::Standard,
            stall_timeout_secs: 8,
            mappings: mappings.clone(),
            admitted_mappings: mappings,
            admitted_destination_root: None,
            job: serde_json::json!({}),
            native_published_paths: Vec::new(),
            native_skipped_selections: Vec::new(),
            native_unfinished_outputs: Vec::new(),
            native_log_sha256: None,
            native_log_digests: Default::default(),
            native_prepared_cues: Vec::new(),
            retry_plan: None,
            roots: Vec::new(),
            temp_artifacts: Vec::new(),
            artifact_generations: vec![1],
            quarantine_artifacts: Vec::new(),
            endpoint_identity_protocol: false,
            endpoint_identities: Vec::new(),
            native_rename_intents: Vec::new(),
            last_status: None,
            abandoned_reason: None,
            origin_owner: None,
            lease_descriptor: None,
            path_claims: Vec::new(),
            legacy_journal_path: None,
            legacy_job_id: None,
        }
    }

    fn test_entry(
        name: &str,
        destination: &str,
        is_move: bool,
        queue_id: Option<u64>,
    ) -> RecoveryEntry {
        RecoveryEntry {
            journal_path: PathBuf::from("recovery").join(format!("{name}.jsonl")),
            record: Some(test_record(
                is_move,
                &format!("source/{name}"),
                destination,
            )),
            version: Some(RecoveryRecordVersion {
                generation: 1,
                updated_unix_ms: 1,
                committed_len: 1,
                committed_sha256: [0; 32],
            }),
            availability: RecoverySurfaceAvailability::Recoverable,
            diagnostic: None,
            queue_id,
            resume_committed: false,
            approved_resume_queue_id: None,
            discard_committed: false,
            live_snapshot: None,
            stop_requested_unix_ms: None,
        }
    }

    #[test]
    fn path_overlap_is_symmetric_and_tree_aware() {
        assert!(paths_overlap(Path::new("/a/b"), Path::new("/a")));
        assert!(paths_overlap(Path::new("/a"), Path::new("/a/b")));
        assert!(!paths_overlap(Path::new("/a/b"), Path::new("/a/c")));
    }
    #[test]
    fn overlapping_destinations_are_excluded_from_unattended_bulk_resume_only() {
        let a = test_entry("a", "dest/album", false, Some(21));
        let b = test_entry("b", "dest/album/disc-1", false, Some(22));
        let c = test_entry("c", "elsewhere/c", false, Some(23));
        let mut state = RecoveryUiState::default();
        state.entries = vec![a, b, c];

        assert!(!state.resume_all_available());
        assert_eq!(state.resume_count(), 1);
        assert_eq!(overlapping_entry_count(&state.entries, &state.entries[0]), 1);
        assert_eq!(overlapping_entry_count(&state.entries, &state.entries[1]), 1);
        assert_eq!(overlapping_entry_count(&state.entries, &state.entries[2]), 0);
        assert!(state.entries[0].can_resume());
        assert!(state.entries[1].can_resume());
        assert!(state.entries[2].can_resume());
        let bulk_ids = state
            .entries
            .iter()
            .filter(|entry| state.can_resume_unattended(entry))
            .filter_map(|entry| entry.queue_id)
            .collect::<Vec<_>>();
        assert_eq!(bulk_ids, vec![23]);
    }
    #[test]
    fn adjacent_destinations_remain_eligible_for_unattended_resume() {
        let mut state = RecoveryUiState::default();
        state.entries = vec![
            test_entry("a", "dest/a", false, Some(31)),
            test_entry("b", "dest/b", false, Some(32)),
        ];

        assert!(state.resume_all_available());
        assert_eq!(state.resume_count(), 2);
        assert!(!entry_has_destination_overlap(
            &state.entries,
            &state.entries[0]
        ));
        assert!(!entry_has_destination_overlap(
            &state.entries,
            &state.entries[1]
        ));
    }
    #[test]
    fn diagnostic_sanitizer_does_not_expose_reserved_implementation_terms() {
        let text = user_safe_diagnostic("journal ownership descriptor missing");
        for prohibited in ["journal", "descriptor", "owner gone", "RecoveryReserved"] {
            assert!(!text.contains(prohibited));
        }
    }
    #[test]
    fn discard_error_sanitizer_hides_internal_reservation_language() {
        let text = user_safe_discard_error(
            "recovery reservation changed while the journal descriptor was reacquired",
        );
        for prohibited in ["journal", "descriptor", "recovery reservation"] {
            assert!(!text.to_ascii_lowercase().contains(prohibited));
        }
    }
    #[test]
    fn cancel_approved_queue_retry_preserves_source_and_destination() {
        let _home = crate::tui::test_support::XdgConfigHomeGuard::new(
            "tonepoet-issue81-queued-recovery-cancel");
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("track.flac");
        let dest = temp.path().join("output");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(&source, b"original").unwrap();
        let target = dest.join("track.flac");
        let plan = tui_file_picker::PastePlan {
            mode: tui_file_picker::FilePickerClipboardMode::Copy,
            mappings: vec![tui_file_picker::PasteMapping { source: source.clone(), destination: target.clone() }],
        };
        let clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Copy, vec![source.clone()]).unwrap();
        let entry = {
            let mut entry = test_entry("retry", target.to_string_lossy().as_ref(), false, None);
            entry.resume_committed = true;
            entry.approved_resume_queue_id = Some(88);
            entry
        };
        let mut retry = crate::tui::browse::BrowsePasteRetryPlan::from_plan(plan.clone());
        retry.recovery_journal_path = Some(entry.journal_path.clone());
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.file_transfers.active_session_id = Some(777); // serial queue occupied
        app.file_transfers.queued.push_back(QueuedFileTransfer {
            queue_id: 88, clipboard, clipboard_owner_generation: None,
            destination_dir: dest, enqueue_plan: plan, retry_plan: Some(retry), recovered: true,
        }.into());
        app.recovery_ui.entries.push(entry);
        app.recovery_ui.focus = 0;
        let (tx, _rx) = mpsc::channel(8);
        activate_window_focus(&mut app, &tx);
        assert!(app.file_transfers.queued.is_empty());
        assert_eq!(std::fs::read(&source).unwrap(), b"original");
        assert!(!target.exists(), "queue cancellation must not touch user files");
    }
    #[test]
    fn stop_and_undo_never_cleans_a_live_lease() {
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let mut live = test_entry("live", "dest/live", false, None);
        live.availability = RecoverySurfaceAvailability::Live;
        let path = live.journal_path.clone();
        app.recovery_ui.entries.push(live);
        app.recovery_ui.stop_and_undo_pending.insert(path.clone());
        let (tx, _rx) = mpsc::channel(8);
        advance_stop_and_undo(&mut app, &tx);
        assert!(app.recovery_ui.stop_and_undo_pending.contains(&path));
        assert!(!app.recovery_ui.discard_in_flight.contains(&path));
    }
    #[test]
    fn reconnect_conflict_abort_requires_stop_confirmation_before_any_worker_command() {
        use super::super::app::{FileTaskControl, FileTaskProgressSession};
        use tui_file_picker::{ConflictItemKind, ConflictPromptState, ConflictResolution,
            FileTaskKind, FileTaskProgressState, FileTaskProgressUpdate, FileTaskUserAction};

        let _journal_lock = crate::tui::file_task_runtime::test_environment_lock();
        let _home = crate::tui::test_support::XdgConfigHomeGuard::new(
            "tonepoet-reconnect-conflict-confirmation",
        );
        let job_id = uuid::Uuid::new_v4().to_string();
        let journal_dir = crate::tui::file_task_runtime::file_task_journal_dir();
        std::fs::create_dir_all(&journal_dir).expect("create isolated journal directory");
        let journal_path = journal_dir.join(format!("{job_id}.jsonl"));
        let remote = LiveWorkerRef::new(journal_path.clone(), job_id.clone(), 1)
            .expect("valid generation-bound identity");
        let (controls, received) = std::sync::mpsc::channel();
        let (publisher, _worker_guard) = crate::tui::file_task_live::start_worker(
            remote.clone(), controls,
        ).expect("start isolated private command listener");

        let mut entry = test_entry("reconnected", "output/reconnected", false, None);
        entry.journal_path = journal_path.clone();
        entry.record.as_mut().unwrap().job_id = job_id.clone();
        entry.availability = RecoverySurfaceAvailability::Live;
        let now = unix_ms_now();
        entry.live_snapshot = Some(LiveWorkerSnapshot {
            schema: 1, job_id, generation: 1,
            worker: crate::concurrency::OwnerProcessIdentity::current(),
            observed_unix_ms: now, forward_unix_ms: now,
            phase: tui_file_picker::FileTaskPhase::Paused,
            status: "Waiting for a destination conflict decision".into(),
            current_item: None, totals: tui_file_picker::ProgressTotals::default(),
            rate_bytes_per_sec: None,
            conflict: Some(ConflictPromptState::new(71, "Existing file", "Choose",
                ConflictItemKind::File)),
        });
        // This is a connected #81 worker, not a legacy worker without a
        // progress feed. Establish the on-disk feed before confirming Stop.
        publisher.publish(&FileTaskProgressUpdate::Snapshot {
            phase: tui_file_picker::FileTaskPhase::Paused,
            status: "Waiting for a destination conflict decision".into(),
            current_item: None, totals: tui_file_picker::ProgressTotals::default(),
            rate_bytes_per_sec: None,
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while crate::tui::file_task_live::read_snapshot(&remote).is_none()
            && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(crate::tui::file_task_live::read_snapshot(&remote).is_some());
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.recovery_ui.entries.push(entry);
        let (tx, _rx) = mpsc::channel::<AppMessage>(8);
        let mut progress = FileTaskProgressState::new(FileTaskKind::Copy,
            "Reconnected copy", tui_file_picker::FilePickerTheme::default());
        progress.apply_update(FileTaskProgressUpdate::ShowConflict {
            conflict: ConflictPromptState::new(71, "Existing file", "Choose",
                ConflictItemKind::File),
        });
        let mut session = FileTaskProgressSession::recovery_monitor(progress, remote);
        session.progress.set_recovery_feed_state(None, true);
        let abort = FileTaskUserAction::ChooseConflictResolution(ConflictResolution::Abort);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        // Key, Ctrl+C, and the rendered mouse Abort all produce this same
        // semantic action, so the host must guard the shared action dispatcher.
        for action in [
            session.progress.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            session.progress.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            abort.clone(), // mouse hit action, verified in the picker crate test
        ] {
            assert_eq!(action, abort);
            assert!(!super::super::keybindings::handle_file_task_user_action(
                &mut app, &mut session, action, &tx));
            assert_eq!(app.recovery_ui.surface, RecoverySurface::StopConfirm);
            assert!(matches!(&app.active_overlay, ActiveOverlay::FileRecovery));
            assert_eq!(app.recovery_ui.focus, 1, "decline must be the safe default");
            assert!(!app.recovery_ui.stop_and_undo_pending.contains(&journal_path));
            assert!(received.recv_timeout(std::time::Duration::from_millis(220)).is_err(),
                "Abort before confirmation must not reach the worker");
            handle_key(&mut app, enter, &tx); // Decline confirmation.
            assert_eq!(app.recovery_ui.surface, RecoverySurface::Window);
            assert!(session.progress.conflict.as_ref().is_some_and(|c| c.request_id == 71),
                "declining must leave the same conflict available to the worker");
            assert!(received.try_recv().is_err());
        }

        // A reconnected watcher may return to Interrupted jobs with Esc, without minimizing.
        let back = session.progress.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(back, FileTaskUserAction::Acknowledge);
        assert!(!super::super::keybindings::handle_file_task_user_action(
            &mut app, &mut session, back, &tx));
        assert!(received.try_recv().is_err());
        assert!(!app.recovery_ui.stop_and_undo_pending.contains(&journal_path));

        // Other conflict decisions continue through the original mailbox,
        // with the untouched generation and request ID (including Apply all).
        for choice in [
            ConflictResolution::Overwrite { apply_to_all: true },
            ConflictResolution::Skip { apply_to_all: true },
            ConflictResolution::Rename,
        ] {
            assert!(super::super::keybindings::handle_file_task_user_action(
                &mut app, &mut session,
                FileTaskUserAction::ChooseConflictResolution(choice.clone()), &tx));
            assert_eq!(received.recv_timeout(std::time::Duration::from_secs(2))
                .expect("control mailbox accepted conflict resolution"),
                FileTaskControl::ConflictResolution { request_id: 71, resolution: choice });
        }

        assert!(!super::super::keybindings::handle_file_task_user_action(
            &mut app, &mut session, abort.clone(), &tx));
        assert_eq!(app.recovery_ui.surface, RecoverySurface::StopConfirm);
        app.recovery_ui.focus = 0; // Explicitly choose the destructive affirmative.
        handle_key(&mut app, enter, &tx);
        assert_eq!(received.recv_timeout(std::time::Duration::from_secs(2))
            .expect("confirmed Stop reaches existing worker mailbox"), FileTaskControl::Abort);
        assert!(app.recovery_ui.stop_and_undo_pending.contains(&journal_path));

        // A locally launched job keeps its original direct conflict behavior.
        let (local_tx, local_rx) = std::sync::mpsc::channel();
        let mut local_progress = FileTaskProgressState::new(FileTaskKind::Copy,
            "Locally launched", tui_file_picker::FilePickerTheme::default());
        local_progress.apply_update(FileTaskProgressUpdate::ShowConflict {
            conflict: ConflictPromptState::new(72, "Local collision", "Choose",
                ConflictItemKind::File),
        });
        let mut local = FileTaskProgressSession::new(local_progress, local_tx);
        assert!(super::super::keybindings::handle_file_task_user_action(
            &mut app, &mut local, abort, &tx));
        assert_eq!(local_rx.try_recv().unwrap(), FileTaskControl::ConflictResolution {
            request_id: 72, resolution: ConflictResolution::Abort,
        });
    }
    #[test]
    fn reconnected_progress_closes_when_its_live_worker_is_gone() {
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let id = uuid::Uuid::new_v4().to_string();
        let remote = LiveWorkerRef::new(
            crate::tui::file_task_runtime::file_task_journal_dir().join(format!("{id}.jsonl")),
            id,
            1,
        ).unwrap();
        let progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Copy,
            "Recovery watch",
            crate::tui::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        app.active_overlay = ActiveOverlay::FileTaskProgress(
            crate::tui::app::FileTaskProgressSession::recovery_monitor(progress, remote),
        );
        reconcile_progress_watch(&mut app);
        assert!(matches!(app.active_overlay, ActiveOverlay::FileRecovery));
    }

    #[test]
    fn older_interrupted_job_precedes_newer_overlapping_job() {
        let mut older = test_entry("older", "dest/album", false, Some(10));
        older.record.as_mut().unwrap().created_unix_ms = 2;
        let mut newer = test_entry("newer", "dest/album/disc", true, Some(11));
        newer.record.as_mut().unwrap().created_unix_ms = 3;
        let entries = vec![newer, older]; // Review list is newest first.
        assert_eq!(older_interrupted_job(&entries, &entries[0]), Some(1));
        assert_eq!(older_interrupted_job(&entries, &entries[1]), None);
        assert!(window_actions(&entries[0], &entries).contains(&WindowAction::Older));
        assert!(!window_actions(&entries[0], &entries).contains(&WindowAction::Finish));
        assert!(window_actions(&entries[1], &entries).contains(&WindowAction::Finish));
        assert!(window_actions(&entries[0], &entries).contains(&WindowAction::Undo));
    }

    #[test]
    fn running_without_feed_offers_no_false_worker_controls() {
        let mut entry = test_entry("no-feed", "dest/music", false, None);
        entry.availability = RecoverySurfaceAvailability::Live;
        entry.live_snapshot = None;
        assert_eq!(ui_state(&entry, &[entry.clone()]), "running");
        assert!(window_actions(&entry, &[entry.clone()]).is_empty());
        assert!(!entry.needs_attention());
        assert_eq!(entry_progress(&entry), "still running");
    }

    #[test]
    fn unreadable_record_does_not_invent_locks_or_items() {
        let mut entry = test_entry("bad", "dest/bad", false, None);
        entry.record = None;
        entry.availability = RecoverySurfaceAvailability::Unreadable;
        assert!(entry.blocked_paths().is_empty());
        assert_eq!(entry.item_count(), 0);
        let entries = vec![entry];
        assert_eq!(locked_total(&entries), (0, true));
        assert_eq!(window_actions(&entries[0], &entries),
            vec![WindowAction::Error, WindowAction::Release]);
    }

    #[test]
    fn selected_item_count_never_counts_directory_contents_or_temp_paths() {
        let mut entry = test_entry("folder", "dest/album", true, Some(42));
        let record = entry.record.as_mut().unwrap();
        record.path_claims.clear();
        record.mappings[0].source = PathBuf::from("/source/album");
        // Even a folder with thousands of files counts as one selected item.
        assert_eq!(entry.item_count(), 1);
        assert_eq!(entry.completed_count(), 0);
        assert_eq!(entry.remaining_count(), 1);
        assert_eq!(entry_progress(&entry), "0 of 1 items");
    }

    #[test]
    fn completed_clean_extraction_offers_dismiss_without_undo() {
        let mut entry = test_entry("extract", "dest/album", false, None);
        let record = entry.record.as_mut().unwrap();
        record.job = serde_json::json!({"kind": "native_extraction", "native_extraction": {"source":"disc.iso","destination":"dest/album"}});
        record.lifecycle = DurableFileTaskLifecycle::Completed;
        assert!(record.is_native_extraction());
        let actions = window_actions(&entry, &[entry.clone()]);
        assert_eq!(actions, vec![WindowAction::Dismiss]);
    }

    #[test]
    fn queued_finish_cannot_be_undone_while_job_is_owned() {
        let mut entry = test_entry("queued", "dest/queued", true, None);
        entry.resume_committed = true;
        entry.approved_resume_queue_id = Some(12);
        let actions = window_actions(&entry, &[entry.clone()]);
        assert_eq!(actions, vec![WindowAction::CancelFinish]);
        assert!(!entry.can_discard());
    }

    #[test]
    fn body_footer_and_popup_sizing_fit_small_terminals() {
        let area = Rect::new(0, 0, 80, 24);
        let main = window_rect(area, false);
        let popup = popup_rect(area, false);
        assert_eq!(main, area);
        assert!(popup.right() <= area.right());
        assert!(popup.bottom() <= area.bottom());
    }

    #[test]
    fn live_recovery_is_running_not_a_startup_decision() {
        let mut running = test_entry("running", "dest/running", false, None);
        running.availability = RecoverySurfaceAvailability::Live;
        let surface = RecoverySurfaceRecord {
            journal_path: running.journal_path.clone(),
            record: running.record.clone(),
            version: running.version,
            availability: running.availability,
            diagnostic: None,
        };
        let state = RecoveryUiState::from_records(vec![surface], &VecDeque::new());
        assert_eq!(state.running_count(), 1);
        assert_eq!(state.review_count(), 0);
        assert!(!state.prompt_pending, "live-only work is not a startup interruption");

        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.recovery_ui = state;
        let footer = app.file_task_footer_state().unwrap();
        assert_eq!((footer.recovery_running, footer.recovery_unresolved), (1, 0));
        assert!(!footer.startup_notice);

        app.recovery_ui.entries.push(test_entry("review", "dest/review", false, None));
        let footer = app.file_task_footer_state().unwrap();
        assert_eq!((footer.recovery_running, footer.recovery_unresolved), (1, 1));
    }

    #[test]
    fn empty_window_has_close_but_no_dummy_job_actions() {
        let state = RecoveryUiState::default();
        assert_eq!(state.needs_attention_count(), 0);
        assert_eq!(state.bulk_discard_count(), 0);
        assert!(!state.resume_all_available());
    }

    #[test]
    fn stopped_job_counts_are_selection_not_live_transfer_totals() {
        let entry = test_entry("folder", "dest/album", true, Some(13));
        assert_eq!(entry_progress(&entry), "0 of 1 items");
        assert!(!entry_progress(&entry).contains("bytes"));
        assert!(!entry_progress(&entry).contains("files"));
    }

    #[test]
    fn cancellation_popup_initially_focuses_cancel() {
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.recovery_ui.entries.push(test_entry("copy", "dest/copy", false, Some(1)));
        open_selected_discard_confirm(&mut app);
        assert_eq!(app.recovery_ui.surface, RecoverySurface::DiscardConfirm);
        assert_eq!(app.recovery_ui.focus, 1);
        let (tx, _rx) = mpsc::channel(8);
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tx);
        assert_eq!(app.recovery_ui.surface, RecoverySurface::Window);
        assert!(app.recovery_ui.discard_in_flight.is_empty());
    }

    #[test]
    fn completed_records_are_not_bulk_finished_or_undone() {
        let mut a = test_entry("a", "dest/a", false, Some(1));
        let b = test_entry("b", "dest/b", false, Some(2));
        a.record.as_mut().unwrap().lifecycle = DurableFileTaskLifecycle::Completed;
        let state = RecoveryUiState { entries: vec![a,b], ..Default::default() };
        assert!(!state.resume_all_available());
        assert_eq!(state.bulk_discard_count(), 0);
    }

    #[test]
    fn approved_queued_and_live_older_jobs_keep_the_newer_finish_blocked() {
        let mut older = test_entry("older", "dest/album", true, Some(51));
        older.record.as_mut().unwrap().created_unix_ms = 1;
        let mut newer = test_entry("newer", "dest/album/disc", true, Some(52));
        newer.record.as_mut().unwrap().created_unix_ms = 2;
        older.resume_committed = true;
        older.approved_resume_queue_id = Some(51);
        older.queue_id = None;
        let mut state = RecoveryUiState { entries: vec![newer, older], ..Default::default() };
        assert_eq!(older_interrupted_job(&state.entries, &state.entries[0]), Some(1));
        assert_eq!(window_actions(&state.entries[0], &state.entries),
            vec![WindowAction::Older, WindowAction::Undo]);
        assert!(entry_has_destination_overlap(&state.entries, &state.entries[0]));
        assert!(!state.can_resume_unattended(&state.entries[0]));
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.recovery_ui = state.clone();
        assert!(resume_has_older_overlap(&app, &state.entries[0].journal_path));

        state.entries[1].availability = RecoverySurfaceAvailability::Live;
        assert_eq!(older_interrupted_job(&state.entries, &state.entries[0]), Some(1));
        state.entries[1].availability = RecoverySurfaceAvailability::Recoverable;
        state.entries[1].resume_committed = false;
        state.entries[1].approved_resume_queue_id = None;
        state.entries[1].discard_committed = true; // Undo cleanup is still in progress.
        assert_eq!(older_interrupted_job(&state.entries, &state.entries[0]), Some(1));
        state.entries[1].discard_committed = false;
        state.entries[1].record.as_mut().unwrap().lifecycle = DurableFileTaskLifecycle::Completed;
        assert!(state.entries[1].is_completed_clean());
        assert_eq!(older_interrupted_job(&state.entries, &state.entries[0]), None);
        assert!(window_actions(&state.entries[0], &state.entries).contains(&WindowAction::Finish));
        assert!(!entry_has_destination_overlap(&state.entries, &state.entries[0]));
    }

    fn queued_test_retry(entry: &RecoveryEntry, id: u64) -> QueuedFileTransfer {
        let mapping = entry.record.as_ref().unwrap().mappings[0].clone();
        let clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Copy, vec![mapping.source.clone()],
        ).expect("clipboard for a test recovery");
        let plan = tui_file_picker::PastePlan {
            mode: tui_file_picker::FilePickerClipboardMode::Copy,
            mappings: vec![mapping],
        };
        let mut retry = super::super::browse::BrowsePasteRetryPlan::from_plan(plan.clone());
        retry.recovery_journal_path = Some(entry.journal_path.clone());
        QueuedFileTransfer {
            queue_id: id,
            clipboard,
            clipboard_owner_generation: None,
            destination_dir: PathBuf::from("dest"),
            enqueue_plan: plan,
            retry_plan: Some(retry),
            recovered: true,
        }
    }

    #[test]
    fn direct_resume_command_cannot_prepend_newer_job_before_approved_older_one() {
        let mut older = test_entry("older-cmd", "dest/album", false, Some(61));
        older.record.as_mut().unwrap().created_unix_ms = 1;
        let mut newer = test_entry("newer-cmd", "dest/album/disc", false, Some(62));
        newer.record.as_mut().unwrap().created_unix_ms = 2;
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.file_transfers.active_session_id = Some(9901); // unrelated task occupies scheduler
        app.file_transfers.recovery_queued.push_back(queued_test_retry(&older, 61));
        app.file_transfers.recovery_queued.push_back(queued_test_retry(&newer, 62));
        app.recovery_ui.entries = vec![newer.clone(), older.clone()];
        let (tx, _rx) = mpsc::channel(8);
        super::super::keybindings::resume_file_transfer_recovery(&mut app, Some(61), &tx);
        assert_eq!(app.file_transfers.queued.len(), 1);
        assert_eq!(app.file_transfers.recovery_queued.len(), 1);
        let old_path = older.journal_path.clone();
        let new_path = newer.journal_path.clone();
        assert_eq!(app.file_transfers.queued[0].transfer().and_then(queued_recovery_journal),
            Some(old_path.as_path()));
        app.recovery_ui.entries[1].resume_committed = true;
        app.recovery_ui.entries[1].approved_resume_queue_id = Some(61);
        app.recovery_ui.entries[1].queue_id = None;
        assert!(resume_has_older_overlap(&app, &new_path));
        // The typed command uses the shared promotion boundary, not a UI-only check.
        super::super::keybindings::resume_file_transfer_recovery(&mut app, Some(62), &tx);
        assert_eq!(app.file_transfers.queued.len(), 1);
        assert_eq!(app.file_transfers.recovery_queued.len(), 1);
        assert_eq!(app.file_transfers.recovery_queued[0].queue_id, 62);
        assert_eq!(app.file_transfers.queued[0].transfer().and_then(queued_recovery_journal),
            Some(old_path.as_path()));
        // Once A has finished, an unretired clean record no longer blocks B.
        app.file_transfers.queued.clear();
        app.recovery_ui.entries[1].resume_committed = false;
        app.recovery_ui.entries[1].approved_resume_queue_id = None;
        app.recovery_ui.entries[1].record.as_mut().unwrap().lifecycle = DurableFileTaskLifecycle::Completed;
        assert!(!resume_has_older_overlap(&app, &new_path));
        super::super::keybindings::resume_file_transfer_recovery(&mut app, Some(62), &tx);
        assert!(app.file_transfers.recovery_queued.is_empty());
        assert_eq!(app.file_transfers.queued[0].transfer().and_then(queued_recovery_journal),
            Some(new_path.as_path()));
    }

    fn root_test_result(
        mapping: &tui_file_picker::PasteMapping,
        disposition: tui_file_picker::FileTaskRootDisposition,
    ) -> tui_file_picker::FileTaskRootResult {
        tui_file_picker::FileTaskRootResult {
            source: mapping.source.clone(), destination: mapping.destination.clone(),
            disposition, message: None,
            undo_disposition: tui_file_picker::FileTaskUndoDisposition::NotReversible,
            proof: None,
        }
    }

    #[test]
    fn files_popup_uses_last_selected_root_checkpoint_and_groups_folders() {
        let mut entry = test_entry("mixed", "dest/album-a/first", false, Some(71));
        let record = entry.record.as_mut().unwrap();
        record.mappings[0].source = PathBuf::from("/source/album-a/first");
        record.mappings[0].destination = PathBuf::from("/dest/album-a/first");
        let skipped = tui_file_picker::PasteMapping {
            source: PathBuf::from("/source/album-b/skipped"),
            destination: PathBuf::from("/dest/album-b/skipped"),
        };
        record.mappings.push(skipped.clone());
        record.roots.push(root_test_result(&record.mappings[0], tui_file_picker::FileTaskRootDisposition::Failed));
        record.roots.push(root_test_result(&record.mappings[0], tui_file_picker::FileTaskRootDisposition::Completed));
        record.roots.push(root_test_result(&skipped, tui_file_picker::FileTaskRootDisposition::Skipped));
        assert_eq!(entry.completed_count(), 1);
        assert_eq!(entry.failed_count(), 0);
        assert_eq!(entry.skipped_count(), 1);
        for number in 0..9 {
            let parent = if number < 5 { "/dest/album-a" } else { "/dest/album-b" };
            let record = entry.record.as_mut().unwrap();
            record.temp_artifacts.push(super::super::file_task_runtime::DurableTempArtifact {
                path: PathBuf::from(format!("{parent}/.partial-{number}.tmp")),
                destination: PathBuf::from(format!("{parent}/track-{number}.flac")),
                kind: super::super::file_task_runtime::DurableTempArtifactKind::File,
                owner_job_id: record.job_id.clone(), owner_generation: 1,
            });
        }
        let state = RecoveryUiState { entries: vec![entry], ..Default::default() };
        let rows = files_popup_lines(&state);
        assert_eq!(rows.iter().filter(|(line, _)| line == "/dest/album-a").count(), 1);
        assert_eq!(rows.iter().filter(|(line, _)| line == "/dest/album-b").count(), 1);
        assert_eq!(rows.iter().filter(|(line, _)| line.starts_with("  track-")).count(), 9);
        assert_eq!(rows.iter().filter(|(line, _)| line.starts_with("    on disk:")).count(), 9);
        assert!(rows.iter().all(|(line, _)| line != "FAILED"));
        assert!(rows.iter().any(|(line, _)| line == "SKIPPED"));
        assert!(rows.iter().any(|(line, _)| line == "  skipped"));
    }

    #[test]
    fn undo_single_and_bulk_share_record_derived_effects_and_keep_error_color() {
        let mut first = test_entry("undo-one", "/dest/album-one", true, Some(81));
        let record = first.record.as_mut().unwrap();
        record.temp_artifacts.push(super::super::file_task_runtime::DurableTempArtifact {
            path: PathBuf::from("/dest/album-one/.partial.tmp"),
            destination: PathBuf::from("/dest/album-one/unfinished"),
            kind: super::super::file_task_runtime::DurableTempArtifactKind::File,
            owner_job_id: record.job_id.clone(), owner_generation: 1,
        });
        record.quarantine_artifacts.extend([
            super::super::file_task_runtime::DurableQuarantineArtifact {
                path: PathBuf::from("/source/.held-a/payload"),
                original_source: PathBuf::from("/source/old-a"),
                destination: PathBuf::from("/dest/album-one/a"),
                state: DurableQuarantineState::RenameConfirmed,
            },
            super::super::file_task_runtime::DurableQuarantineArtifact {
                path: PathBuf::from("/source/.held-b/payload"),
                original_source: PathBuf::from("/source/old-b"),
                destination: PathBuf::from("/dest/album-one/b"),
                state: DurableQuarantineState::DeletionStarted,
            },
        ]);
        record.roots.push(root_test_result(&record.mappings[0],
            tui_file_picker::FileTaskRootDisposition::Completed));
        let second = test_entry("undo-two", "/dest/album-two", false, Some(82));
        let mut state = RecoveryUiState { entries: vec![first, second], ..Default::default() };
        state.incomplete_sizes.insert(PathBuf::from("/dest/album-one/.partial.tmp"), Some(520));
        let theme = super::super::theme::theme_by_slug_or_default("tokyo-night");
        state.surface = RecoverySurface::DiscardConfirm;
        let (_, one, _) = confirm_description(&state, theme);
        let text = one.iter().map(|(line, _)| line.as_str()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("520 B so far"));
        assert!(text.contains("Check 1 original name"));
        assert!(text.contains("whose deletion began"));
        assert!(text.contains("1 finished item"));
        assert!(!text.contains("Restore 2 original"));
        assert!(one.iter().any(|(line, color)| line.starts_with('×')
            && line.contains("deleted content") && *color == theme.error));
        state.surface = RecoverySurface::BulkDiscardConfirm;
        let (_, all, _) = confirm_description(&state, theme);
        let all_text = all.iter().map(|(line, _)| line.as_str()).collect::<Vec<_>>().join("\n");
        assert!(all_text.contains("MOVE  undo-one"));
        assert!(all_text.contains("COPY  undo-two"));
        assert!(all_text.contains("520 B so far"));
        assert!(all_text.contains("deleted content"));
        assert!(all.iter().any(|(line, color)| line.contains("deleted content") && *color == theme.error));
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.recovery_ui = state;
        open_safety_confirm(&mut app, RecoverySurface::BulkDiscardConfirm);
        assert_eq!(app.recovery_ui.focus, 1);
        let (tx, _rx) = mpsc::channel(8);
        handle_key(&mut app, KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), &tx);
        assert_eq!(app.recovery_ui.details_scroll, 8);
        assert_eq!(app.recovery_ui.focus, 1);
        handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &tx);
        assert_eq!(app.recovery_ui.details_scroll, 7);
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tx);
        assert_eq!(app.recovery_ui.surface, RecoverySurface::Window);
        assert!(app.recovery_ui.discard_in_flight.is_empty());
    }

    #[test]
    fn unreadable_records_keep_path_and_distinguishing_error_for_popup() {
        let mut first = test_entry("truncated", "unused/path", false, None);
        first.journal_path = PathBuf::from("/saved/jobs/truncated.jsonl");
        first.record = None;
        first.availability = RecoverySurfaceAvailability::Unreadable;
        first.diagnostic = Some("parse journal: unexpected end of file at byte 40".into());
        let mut second = first.clone();
        second.journal_path = PathBuf::from("/saved/jobs/protected.jsonl");
        second.diagnostic = Some("read journal: Permission denied (os error 13)".into());
        assert_ne!(first.journal_path, second.journal_path);
        let text_a = user_safe_diagnostic(first.diagnostic.as_deref().unwrap());
        let text_b = user_safe_diagnostic(second.diagnostic.as_deref().unwrap());
        assert!(text_a.contains("unexpected end of file at byte 40"));
        assert!(text_b.contains("Permission denied (os error 13)"));
        assert!(!text_a.contains("journal"));
        assert!(first.blocked_paths().is_empty());
        assert_eq!(first.item_count(), 0);
        assert!(!job_has_files(&first));
        assert!(files_popup_lines(&RecoveryUiState { entries: vec![first], ..Default::default() }).is_empty());
    }

    #[test]
    fn popup_scroll_clamps_at_render_before_the_next_page_up() {
        use ratatui::{backend::TestBackend, Terminal};

        let theme = super::super::theme::theme_by_slug_or_default("tokyo-night");
        let (tx, _rx) = mpsc::channel(8);
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        for number in 0..12 {
            app.recovery_ui.entries.push(test_entry(
                &format!("scroll-{number}"),
                &format!("dest/album-{number}"),
                false,
                Some(number + 1),
            ));
        }
        let record = app.recovery_ui.entries[0].record.as_mut().unwrap();
        let job_id = record.job_id.clone();
        for number in 0..30 {
            record.temp_artifacts.push(super::super::file_task_runtime::DurableTempArtifact {
                path: PathBuf::from(format!("dest/album-0/.partial-{number}")),
                destination: PathBuf::from(format!("dest/album-0/track-{number}")),
                kind: super::super::file_task_runtime::DurableTempArtifactKind::File,
                owner_job_id: job_id.clone(),
                owner_generation: 1,
            });
        }

        // Help is shorter than its viewport at 80x24 with today's actions;
        // use a shorter terminal for that popup to exercise its scroll path.
        for (surface, height) in [
            (RecoverySurface::BulkDiscardConfirm, 24),
            (RecoverySurface::Files, 24),
            (RecoverySurface::Help, 12),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(80, height)).unwrap();
            app.recovery_ui.surface = surface;
            app.recovery_ui.details_scroll = 0;
            app.recovery_ui.focus = 1; // Cancel is the default in confirmations.
            for _ in 0..30 {
                handle_key(&mut app, KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), &tx);
            }
            let before_draw = app.recovery_ui.details_scroll;
            terminal.draw(|frame| match surface {
                RecoverySurface::BulkDiscardConfirm => draw_confirm_popup(frame, &mut app, theme),
                RecoverySurface::Files => draw_files_popup(frame, &mut app, theme),
                RecoverySurface::Help => draw_help_popup(frame, &mut app, theme),
                _ => unreachable!(),
            }).unwrap();
            let last_page = app.recovery_ui.details_scroll;
            assert!(last_page < before_draw, "scroll overshoot not clamped: {surface:?}");
            assert!(last_page > 0, "popup must have more rows than fit: {surface:?}");
            handle_key(&mut app, KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE), &tx);
            assert_eq!(app.recovery_ui.details_scroll, last_page.saturating_sub(8),
                "first PageUp must move immediately: {surface:?}");
            terminal.draw(|frame| match surface {
                RecoverySurface::BulkDiscardConfirm => draw_confirm_popup(frame, &mut app, theme),
                RecoverySurface::Files => draw_files_popup(frame, &mut app, theme),
                RecoverySurface::Help => draw_help_popup(frame, &mut app, theme),
                _ => unreachable!(),
            }).unwrap();
            assert_eq!(app.recovery_ui.details_scroll, last_page.saturating_sub(8));
            assert_eq!(app.recovery_ui.focus, 1);
        }
    }

    #[test]
    fn interrupted_help_only_advertises_actions_of_the_current_job() {
        let mut clean = test_entry("finished", "dest/finished", false, None);
        clean.record.as_mut().unwrap().job = serde_json::json!({"native_extraction": {"source": "disc.iso"}});
        clean.record.as_mut().unwrap().lifecycle = DurableFileTaskLifecycle::Completed;
        let clean_lines = interrupted_help_lines(&RecoveryUiState { entries: vec![clean], ..Default::default() });
        assert!(clean_lines.iter().any(|line| line == "d  Dismiss"));
        assert!(!clean_lines.iter().any(|line| line == "f  Finish"));
        assert!(clean_lines.iter().any(|line| line == "Enter / Esc  close Help"));
        assert!(clean_lines.iter().any(|line| line == "AFTER CLOSING HELP, IN INTERRUPTED JOBS"));
        let mut unreadable = test_entry("bad", "dest/bad", false, None);
        unreadable.record = None;
        unreadable.availability = RecoverySurfaceAvailability::Unreadable;
        let keys = interrupted_help_lines(&RecoveryUiState { entries: vec![unreadable], ..Default::default() });
        assert!(keys.iter().any(|line| line == "e  Error…"));
        assert!(keys.iter().any(|line| line == "r  Release…"));
        assert!(!keys.iter().any(|line| line == "f  Finish"));
        let mut no_feed = test_entry("live", "dest/live", false, None);
        no_feed.availability = RecoverySurfaceAvailability::Live;
        let keys = interrupted_help_lines(&RecoveryUiState { entries: vec![no_feed], ..Default::default() });
        assert!(!keys.iter().any(|line| line == "p  Pause" || line == "x  Stop…"));
        let mut older = test_entry("old", "dest/album", false, Some(91));
        older.record.as_mut().unwrap().created_unix_ms = 1;
        let mut newer = test_entry("new", "dest/album/track", false, Some(92));
        newer.record.as_mut().unwrap().created_unix_ms = 2;
        let keys = interrupted_help_lines(&RecoveryUiState { entries: vec![newer, older], ..Default::default() });
        assert!(keys.iter().any(|line| line == "o  Go to older job"));
        assert!(!keys.iter().any(|line| line == "f  Finish"));
    }
}
