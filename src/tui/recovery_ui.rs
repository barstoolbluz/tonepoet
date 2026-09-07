//! Interrupted copy/move recovery surfaces.
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
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
    Frame,
};
use tokio::sync::mpsc;

use super::app::{ActiveOverlay, AppState, QueuedFileTransfer};
use super::button_map::{ButtonRenderMap, TuiButton};
use super::file_task_runtime::{
    DurableFileTaskRecord, DurableQuarantineState,
    RecoveryRecordVersion, RecoverySurfaceAvailability, RecoverySurfaceRecord,
};
use super::message::AppMessage;

const PROMPT_WIDTH: u16 = 84;
const PROMPT_HEIGHT: u16 = 14;
const WINDOW_WIDTH: u16 = 116;
const WINDOW_HEIGHT: u16 = 23;
const DETAILS_WIDTH: u16 = 78;
const DETAILS_MAX_HEIGHT: u16 = 21;
const CONFIRM_WIDTH: u16 = 78;
const CONFIRM_MAX_HEIGHT: u16 = 21;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverySurface {
    Prompt,
    Window,
    Details,
    DiscardConfirm,
    BulkDiscardConfirm,
    Inspector,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryDetailsTab {
    BlockedPaths,
    IncompleteFiles,
    RenamedSources,
}

impl RecoveryDetailsTab {
    const ALL: [Self; 3] = [
        Self::BlockedPaths,
        Self::IncompleteFiles,
        Self::RenamedSources,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::BlockedPaths => "Blocked paths",
            Self::IncompleteFiles => "Incomplete files",
            Self::RenamedSources => "Renamed sources",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDetailsReason {
    Ordinary,
    Refusal(PathBuf),
    BeforeDecision,
    CleanupPending,
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
    /// The user confirmed Discard and its worker is taking over the durable
    /// record. Keep all competing decisions unavailable until that attempt
    /// reports success or failure.
    pub discard_committed: bool,
    pub session_deferred: bool,
}

impl RecoveryEntry {
    fn kind_label(&self) -> &'static str {
        match self.record.as_ref() {
            Some(record) if record.is_move => "move",
            Some(_) => "copy",
            None => "operation",
        }
    }

    fn item_count(&self) -> usize {
        self.record.as_ref().map_or(0, |record| record.mappings.len())
    }

    fn is_live(&self) -> bool {
        self.availability == RecoverySurfaceAvailability::Live
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
            && self.queue_id.is_some()
    }

    fn can_defer(&self) -> bool {
        self.is_recoverable()
            && self.queue_id.is_some()
            && !self.resume_committed
            && !self.discard_committed
    }

    fn can_discard(&self) -> bool {
        self.is_recoverable()
            && !self.resume_committed
            && !self.discard_committed
            && self.record.is_some()
            && self.version.is_some()
    }

    fn can_bulk_discard_restore(&self) -> bool {
        self.can_discard() && !self.irreversible() && !self.is_discard_cleanup()
    }

    fn show_in_bulk_discard_confirm(&self) -> bool {
        self.can_bulk_discard_restore()
            || (self.discard_committed
                && self.is_recoverable()
                && !self.irreversible()
                && !self.is_discard_cleanup())
    }

    fn completed_count(&self) -> usize {
        self.record.as_ref().map_or(0, |record| {
            record
                .roots
                .iter()
                .filter(|root| root.disposition.is_completed())
                .count()
        })
    }

    fn skipped_count(&self) -> usize {
        self.record.as_ref().map_or(0, |record| {
            record
                .roots
                .iter()
                .filter(|root| {
                    root.disposition == tui_file_picker::FileTaskRootDisposition::Skipped
                })
                .count()
        })
    }

    fn failed_count(&self) -> usize {
        self.record.as_ref().map_or(0, |record| {
            record
                .roots
                .iter()
                .filter(|root| {
                    root.disposition == tui_file_picker::FileTaskRootDisposition::Failed
                })
                .count()
        })
    }

    fn remaining_count(&self) -> usize {
        self.item_count().saturating_sub(
            self.completed_count()
                .saturating_add(self.skipped_count())
                .saturating_add(self.failed_count()),
        )
    }

    fn state_label(&self) -> &'static str {
        if self.is_unreadable() {
            return "record unreadable";
        }
        if self.is_live() {
            return "still running";
        }
        if self.is_discard_cleanup() {
            return "cleanup pending";
        }
        let Some(record) = self.record.as_ref() else {
            return "record unreadable";
        };
        let completed = self.completed_count();
        let total = self.item_count();
        let attempted = completed
            .saturating_add(self.skipped_count())
            .saturating_add(self.failed_count());
        let has_leftovers = !record.temp_artifacts.is_empty()
            || !record.quarantine_artifacts.is_empty()
            || !record.native_rename_intents.is_empty();
        if total > 0 && completed == total {
            if has_leftovers {
                "cleanup pending"
            } else {
                "completed"
            }
        } else if attempted == 0 {
            "not started"
        } else {
            "partly completed"
        }
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

    fn needs_attention(&self) -> bool {
        self.is_live()
            || self.is_unreadable()
            || self.is_discard_cleanup()
            || self.irreversible()
    }
}

#[derive(Debug, Clone)]
pub struct RecoveryUiState {
    pub entries: Vec<RecoveryEntry>,
    pub selected: usize,
    pub list_scroll: usize,
    pub surface: RecoverySurface,
    pub details_tab: RecoveryDetailsTab,
    pub details_scroll: usize,
    pub details_reason: RecoveryDetailsReason,
    pub focus: usize,
    pub maximized: bool,
    pub prompt_pending: bool,
    pub prompt_shown: bool,
    pub incomplete_sizes: BTreeMap<PathBuf, Option<u64>>,
    pub size_requests: BTreeSet<PathBuf>,
    pub discard_in_flight: BTreeSet<PathBuf>,
    /// Recovery records explicitly deferred with `:recovery-defer` during
    /// this process. The durable record stays unresolved, but its executable
    /// retry is not recreated until the next session.
    pub session_deferred: BTreeSet<PathBuf>,
}

impl Default for RecoveryUiState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            selected: 0,
            list_scroll: 0,
            surface: RecoverySurface::Window,
            details_tab: RecoveryDetailsTab::BlockedPaths,
            details_scroll: 0,
            details_reason: RecoveryDetailsReason::Ordinary,
            focus: 0,
            maximized: false,
            prompt_pending: false,
            prompt_shown: false,
            incomplete_sizes: BTreeMap::new(),
            size_requests: BTreeSet::new(),
            discard_in_flight: BTreeSet::new(),
            session_deferred: BTreeSet::new(),
        }
    }
}

impl RecoveryUiState {
    pub fn from_records(records: Vec<RecoverySurfaceRecord>, queued: &VecDeque<QueuedFileTransfer>) -> Self {
        let mut state = Self::default();
        state.replace_records(records, queued);
        state.prompt_pending = !state.entries.is_empty();
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
        let session_deferred = self.session_deferred.clone();
        let discard_in_flight = self.discard_in_flight.clone();
        self.entries = records
            .into_iter()
            .map(|surface| RecoveryEntry {
                queue_id: queue_id_for_journal(queued, &surface.journal_path),
                resume_committed: false,
                discard_committed: discard_in_flight.contains(&surface.journal_path),
                session_deferred: session_deferred.contains(&surface.journal_path),
                journal_path: surface.journal_path,
                record: surface.record,
                version: surface.version,
                availability: surface.availability,
                diagnostic: surface.diagnostic,
            })
            .collect();
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
        self.session_deferred
            .retain(|path| self.entries.iter().any(|entry| &entry.journal_path == path));
        self.size_requests
            .retain(|path| self.entries.iter().any(|entry| &entry.journal_path == path));
        let current_incomplete = self
            .entries
            .iter()
            .flat_map(RecoveryEntry::incomplete_paths)
            .collect::<BTreeSet<_>>();
        self.incomplete_sizes
            .retain(|path, _| current_incomplete.contains(path));
        if self.entries.is_empty() && !self.prompt_shown {
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

    fn open_prompt(&mut self) {
        self.surface = RecoverySurface::Prompt;
        self.focus = 0;
        self.prompt_pending = false;
        self.prompt_shown = true;
    }

    pub(super) fn minimize(&mut self) {
        self.surface = RecoverySurface::Window;
        self.focus = 0;
    }

    fn open_details(&mut self, tab: RecoveryDetailsTab, reason: RecoveryDetailsReason) {
        self.consume_startup_prompt();
        self.surface = RecoverySurface::Details;
        self.details_tab = tab;
        self.details_reason = reason;
        self.details_scroll = 0;
        self.focus = 0;
    }

    fn details_count(&self, tab: RecoveryDetailsTab) -> usize {
        self.selected_entry().map_or(0, |entry| match tab {
            RecoveryDetailsTab::BlockedPaths => entry.blocked_paths().len(),
            RecoveryDetailsTab::IncompleteFiles => entry.incomplete_paths().len(),
            RecoveryDetailsTab::RenamedSources => entry.renamed_sources().len(),
        })
    }

    fn resume_all_available(&self) -> bool {
        !self.entries.is_empty()
            && self.entries.iter().all(|entry| {
                self.can_resume_unattended(entry)
                    && entry.record.as_ref().is_some_and(|record| {
                        record.quarantine_artifacts.is_empty()
                            && record.native_rename_intents.is_empty()
                    })
            })
    }

    fn can_resume_unattended(&self, entry: &RecoveryEntry) -> bool {
        entry.can_resume() && !entry_has_destination_overlap(&self.entries, entry)
    }

    fn bulk_discard_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.can_bulk_discard_restore())
            .count()
    }

    fn resume_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| self.can_resume_unattended(entry))
            .count()
    }

    fn needs_attention_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.needs_attention()).count()
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
        self.open_details(
            RecoveryDetailsTab::BlockedPaths,
            RecoveryDetailsReason::Refusal(blocked),
        );
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
        self.open_details(
            RecoveryDetailsTab::BlockedPaths,
            RecoveryDetailsReason::Refusal(blocked),
        );
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
        queued_recovery_journal(job) == Some(journal_path)
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
        if app
            .recovery_ui
            .session_deferred
            .contains(&recovery.journal_path)
            || app
                .recovery_ui
                .discard_in_flight
                .contains(&recovery.journal_path)
        {
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

pub fn refresh_from_runtime(app: &mut AppState) {
    let super::file_task_runtime::StartupFileTaskRecoveryInventory {
        recoveries,
        recovery_surface_records,
        ..
    } = super::file_task_runtime::startup_file_task_recovery_inventory();
    sync_recovery_review_queue(app, recoveries);
    app.file_transfers.unresolved_recovery_count = recovery_surface_records.len();
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
        .filter_map(queued_recovery_journal)
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
    }
}

pub fn maybe_open_startup_prompt(app: &mut AppState) {
    if app.recovery_ui.prompt_pending
        && !app.recovery_ui.prompt_shown
        && !app.recovery_ui.entries.is_empty()
        && matches!(app.active_overlay, ActiveOverlay::None)
    {
        app.recovery_ui.open_prompt();
        app.active_overlay = ActiveOverlay::FileRecovery;
    }
}

pub fn open_recovery_window(app: &mut AppState) {
    refresh_from_runtime(app);
    if app.recovery_ui.entries.is_empty() {
        app.set_status("No interrupted copy or move operations need attention");
        return;
    }
    app.recovery_ui.open_window();
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
    open_selected_details(app, tx, RecoveryDetailsReason::BeforeDecision);
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

fn request_incomplete_sizes_if_needed(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    if app.recovery_ui.details_tab != RecoveryDetailsTab::IncompleteFiles {
        return;
    }
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
                let size = std::fs::symlink_metadata(&path)
                    .ok()
                    .map(|metadata| metadata.len());
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
        app.set_status("This interrupted operation cannot be discarded from its current state");
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
        .filter(|entry| entry.can_bulk_discard_restore())
        .filter_map(|entry| Some((entry.journal_path.clone(), entry.version?)))
        .filter(|(path, _)| !app.recovery_ui.discard_in_flight.contains(path))
        .collect::<Vec<_>>();
    if work.is_empty() {
        app.set_status("No reversible interrupted operations are available to discard");
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
                    "Interrupted operation discarded: {} incomplete output{} deleted, {} source name{} restored",
                    summary.deleted_incomplete,
                    plural(summary.deleted_incomplete),
                    summary.restored_sources,
                    plural(summary.restored_sources),
                ));
            } else {
                app.recovery_ui.open_window();
                app.set_status(format!(
                    "Interrupted operation discarded; {} blocked path{} released",
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
                "Discard did not complete: {}",
                user_safe_discard_error(&error)
            ));
        }
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn single_discard_header(entry: &RecoveryEntry) -> String {
    format!("Discard this interrupted {} operation:", entry.kind_label())
}

fn bulk_discard_header(count: usize) -> String {
    format!(
        "Discard {count} reversible interrupted copy/move operation{}:",
        plural(count)
    )
}

const INSPECTOR_SCOPE_LINE: &str =
    "This interrupted copy/move operation cannot be changed automatically.";

fn decide_later(app: &mut AppState) {
    let count = app.recovery_ui.entries.len();
    app.recovery_ui.minimize();
    app.active_overlay = ActiveOverlay::None;
    app.set_status(format!(
        "{count} interrupted operation{} remain unresolved; overlapping file changes and undo/redo remain unavailable until they are resolved",
        plural(count),
    ));
}

fn defer_selected(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let Some(queue_id) = app
        .recovery_ui
        .selected_entry()
        .filter(|entry| entry.can_defer())
        .and_then(|entry| entry.queue_id)
    else {
        app.set_status(
            "The selected interrupted operation is already saved for later or is not available to defer",
        );
        return;
    };
    super::keybindings::defer_file_transfer_recovery(app, Some(queue_id), tx);
    refresh_from_runtime(app);
    if !app.recovery_ui.entries.is_empty() {
        app.recovery_ui.surface = RecoverySurface::Window;
        app.recovery_ui.focus = 0;
        app.active_overlay = ActiveOverlay::FileRecovery;
    }
}

fn resume_selected(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let Some(queue_id) = app
        .recovery_ui
        .selected_entry()
        .and_then(|entry| entry.queue_id)
    else {
        app.set_status("This interrupted operation cannot be resumed automatically");
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
        app.set_status("No interrupted operations are ready to resume");
        return;
    }
    app.recovery_ui.minimize();
    app.active_overlay = ActiveOverlay::None;
    super::keybindings::resume_file_transfer_recoveries(app, &ids, tx);
    refresh_from_runtime(app);
}

fn resume_all(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    if !app.recovery_ui.resume_all_available() {
        app.set_status("Resume all is not available while an operation needs review");
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

fn resume_reviewed_set(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let ids = app
        .recovery_ui
        .entries
        .iter()
        .filter(|entry| app.recovery_ui.can_resume_unattended(entry))
        .filter_map(|entry| entry.queue_id)
        .collect::<Vec<_>>();
    resume_queue_ids(app, ids, tx);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowAction {
    Resume,
    Discard,
    Details,
    Inspect,
    Later,
}

fn window_actions(entry: &RecoveryEntry) -> Vec<WindowAction> {
    if entry.is_unreadable() {
        return vec![WindowAction::Inspect];
    }
    if entry.is_live() {
        // Details is informational, not a decision control, and remains
        // available while another process still owns the operation.
        return vec![WindowAction::Details];
    }
    let mut actions = Vec::new();
    if entry.can_resume() {
        actions.push(WindowAction::Resume);
    }
    if entry.can_discard() {
        actions.push(WindowAction::Discard);
    }
    actions.push(WindowAction::Details);
    if entry.can_defer() {
        actions.push(WindowAction::Later);
    }
    actions
}

fn visible_action_count(state: &RecoveryUiState) -> usize {
    match state.surface {
        RecoverySurface::Prompt => 2 + usize::from(state.resume_all_available()),
        RecoverySurface::Window => {
            let primary = state
                .selected_entry()
                .map(window_actions)
                .map_or(0, |actions| actions.len());
            primary
                + usize::from(state.resume_count() > 0)
                + usize::from(state.bulk_discard_count() > 0)
        }
        RecoverySurface::Details => 1,
        RecoverySurface::DiscardConfirm | RecoverySurface::BulkDiscardConfirm => 2,
        RecoverySurface::Inspector => 1,
    }
}

fn cycle_focus(state: &mut RecoveryUiState, reverse: bool) {
    let count = visible_action_count(state);
    if count == 0 {
        state.focus = 0;
        return;
    }
    state.focus = if reverse {
        if state.focus == 0 { count - 1 } else { state.focus - 1 }
    } else {
        (state.focus + 1) % count
    };
}

fn move_selection(state: &mut RecoveryUiState, delta: isize) {
    if state.entries.is_empty() {
        return;
    }
    let last = state.entries.len() - 1;
    state.selected = if delta < 0 {
        state.selected.saturating_sub(delta.unsigned_abs())
    } else {
        state.selected.saturating_add(delta as usize).min(last)
    };
    state.focus = 0;
    state.details_scroll = 0;
}

fn single_discard_effect_count(entry: &RecoveryEntry) -> usize {
    let direct_rename = entry
        .record
        .as_ref()
        .map_or(0, |record| record.native_rename_intents.len());
    let base = 4 + usize::from(direct_rename > 0) + if entry.irreversible() { 3 } else { 0 };
    if entry.is_discard_cleanup() {
        // Cleanup may have stopped at any durable obligation, so keep enough
        // scroll range for the same itemized consequences plus the final
        // record-retirement line. Overestimating by one is harmless;
        // underestimating could make a pending effect unreachable.
        base.saturating_add(1)
    } else {
        base
    }
}

fn auxiliary_scroll_count(state: &RecoveryUiState) -> usize {
    match state.surface {
        RecoverySurface::Details => state.details_count(state.details_tab),
        RecoverySurface::DiscardConfirm => state
            .selected_entry()
            .map_or(0, single_discard_effect_count),
        RecoverySurface::BulkDiscardConfirm => state.bulk_discard_count().saturating_mul(2),
        RecoverySurface::Inspector | RecoverySurface::Prompt | RecoverySurface::Window => 0,
    }
}

fn scroll_auxiliary(state: &mut RecoveryUiState, delta: isize) {
    let max_scroll = auxiliary_scroll_count(state).saturating_sub(1);
    if delta < 0 {
        state.details_scroll = state.details_scroll.saturating_sub(delta.unsigned_abs());
    } else {
        state.details_scroll = state
            .details_scroll
            .saturating_add(delta as usize)
            .min(max_scroll);
    }
}

pub fn handle_key(app: &mut AppState, key: KeyEvent, tx: &mpsc::Sender<AppMessage>) {
    if key.code == KeyCode::Char('m') && key.modifiers == KeyModifiers::ALT {
        app.recovery_ui.maximized = !app.recovery_ui.maximized;
        return;
    }
    match app.recovery_ui.surface {
        RecoverySurface::Prompt => match key.code {
            KeyCode::Esc => decide_later(app),
            KeyCode::Tab => cycle_focus(
                &mut app.recovery_ui,
                key.modifiers.contains(KeyModifiers::SHIFT),
            ),
            KeyCode::BackTab => cycle_focus(&mut app.recovery_ui, true),
            KeyCode::Enter => activate_prompt_focus(app, tx),
            _ => {}
        },
        RecoverySurface::Window => match key.code {
            KeyCode::Esc => {
                app.recovery_ui.minimize();
                app.active_overlay = ActiveOverlay::None;
            }
            KeyCode::Up => move_selection(&mut app.recovery_ui, -1),
            KeyCode::Down => move_selection(&mut app.recovery_ui, 1),
            KeyCode::PageUp => move_selection(&mut app.recovery_ui, -8),
            KeyCode::PageDown => move_selection(&mut app.recovery_ui, 8),
            KeyCode::Tab => cycle_focus(
                &mut app.recovery_ui,
                key.modifiers.contains(KeyModifiers::SHIFT),
            ),
            KeyCode::BackTab => cycle_focus(&mut app.recovery_ui, true),
            KeyCode::Enter => activate_window_focus(app, tx),
            _ => {}
        },
        RecoverySurface::Details => match key.code {
            KeyCode::Esc => app.recovery_ui.open_window(),
            KeyCode::Left => select_adjacent_tab(app, -1, tx),
            KeyCode::Right => select_adjacent_tab(app, 1, tx),
            KeyCode::Up => scroll_auxiliary(&mut app.recovery_ui, -1),
            KeyCode::Down => scroll_auxiliary(&mut app.recovery_ui, 1),
            KeyCode::PageUp => scroll_auxiliary(&mut app.recovery_ui, -8),
            KeyCode::PageDown => scroll_auxiliary(&mut app.recovery_ui, 8),
            KeyCode::Tab => cycle_focus(
                &mut app.recovery_ui,
                key.modifiers.contains(KeyModifiers::SHIFT),
            ),
            KeyCode::BackTab => cycle_focus(&mut app.recovery_ui, true),
            KeyCode::Enter => activate_details_focus(app),
            _ => {}
        },
        RecoverySurface::DiscardConfirm => match key.code {
            KeyCode::Esc => app.recovery_ui.open_window(),
            KeyCode::Up => scroll_auxiliary(&mut app.recovery_ui, -1),
            KeyCode::Down => scroll_auxiliary(&mut app.recovery_ui, 1),
            KeyCode::Tab => cycle_focus(
                &mut app.recovery_ui,
                key.modifiers.contains(KeyModifiers::SHIFT),
            ),
            KeyCode::BackTab => cycle_focus(&mut app.recovery_ui, true),
            KeyCode::Enter => {
                if app.recovery_ui.focus == 0 {
                    begin_single_discard(app, tx);
                } else {
                    app.recovery_ui.open_window();
                }
            }
            _ => {}
        },
        RecoverySurface::BulkDiscardConfirm => match key.code {
            KeyCode::Esc => app.recovery_ui.open_window(),
            KeyCode::Up => scroll_auxiliary(&mut app.recovery_ui, -1),
            KeyCode::Down => scroll_auxiliary(&mut app.recovery_ui, 1),
            KeyCode::PageUp => scroll_auxiliary(&mut app.recovery_ui, -6),
            KeyCode::PageDown => scroll_auxiliary(&mut app.recovery_ui, 6),
            KeyCode::Tab => cycle_focus(
                &mut app.recovery_ui,
                key.modifiers.contains(KeyModifiers::SHIFT),
            ),
            KeyCode::BackTab => cycle_focus(&mut app.recovery_ui, true),
            KeyCode::Enter => {
                if app.recovery_ui.focus == 0 {
                    begin_bulk_discard(app, tx);
                } else {
                    app.recovery_ui.open_window();
                }
            }
            _ => {}
        },
        RecoverySurface::Inspector => match key.code {
            KeyCode::Esc | KeyCode::Enter => app.recovery_ui.open_window(),
            KeyCode::Up => scroll_auxiliary(&mut app.recovery_ui, -1),
            KeyCode::Down => scroll_auxiliary(&mut app.recovery_ui, 1),
            _ => {}
        },
    }
}

fn activate_prompt_focus(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let has_resume_all = app.recovery_ui.resume_all_available();
    match app.recovery_ui.focus {
        0 => app.recovery_ui.open_window(),
        1 if has_resume_all => resume_all(app, tx),
        _ => decide_later(app),
    }
}

fn activate_window_focus(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let Some(entry) = app.recovery_ui.selected_entry() else {
        return;
    };
    let actions = window_actions(entry);
    let focus = app.recovery_ui.focus;
    if let Some(action) = actions.get(focus).copied() {
        match action {
            WindowAction::Resume => resume_selected(app, tx),
            WindowAction::Discard => open_selected_discard_confirm(app),
            WindowAction::Details => {
                open_selected_details(app, tx, RecoveryDetailsReason::BeforeDecision)
            }
            WindowAction::Inspect => {
                app.recovery_ui.surface = RecoverySurface::Inspector;
                app.recovery_ui.focus = 0;
                app.recovery_ui.details_scroll = 0;
            }
            WindowAction::Later => defer_selected(app, tx),
        }
        return;
    }

    let mut index = actions.len();
    if app.recovery_ui.resume_count() > 0 {
        if focus == index {
            resume_reviewed_set(app, tx);
            return;
        }
        index += 1;
    }
    if app.recovery_ui.bulk_discard_count() > 0 && focus == index {
        app.recovery_ui.surface = RecoverySurface::BulkDiscardConfirm;
        app.recovery_ui.focus = 1;
        app.recovery_ui.details_scroll = 0;
    }
}

fn open_selected_discard_confirm(app: &mut AppState) {
    let Some(entry) = app.recovery_ui.selected_entry() else {
        return;
    };
    if !entry.can_discard() {
        if entry.resume_committed {
            app.set_status("Resume is already approved for this interrupted operation; wait for or cancel the queued transfer before choosing a different outcome");
        } else {
            app.set_status("This interrupted operation needs review before it can be discarded");
        }
        return;
    }
    app.recovery_ui.consume_startup_prompt();
    app.recovery_ui.surface = RecoverySurface::DiscardConfirm;
    app.recovery_ui.focus = 1;
    app.recovery_ui.details_scroll = 0;
}

fn open_selected_details(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    requested_reason: RecoveryDetailsReason,
) {
    let Some(entry) = app.recovery_ui.selected_entry() else {
        return;
    };
    let reason = if entry.state_label() == "cleanup pending" {
        RecoveryDetailsReason::CleanupPending
    } else {
        requested_reason
    };
    let tab = match reason {
        RecoveryDetailsReason::BeforeDecision => RecoveryDetailsTab::IncompleteFiles,
        RecoveryDetailsReason::Refusal(_) => RecoveryDetailsTab::BlockedPaths,
        RecoveryDetailsReason::CleanupPending => {
            if !entry.incomplete_paths().is_empty() {
                RecoveryDetailsTab::IncompleteFiles
            } else if !entry.renamed_sources().is_empty() {
                RecoveryDetailsTab::RenamedSources
            } else {
                RecoveryDetailsTab::BlockedPaths
            }
        }
        RecoveryDetailsReason::Ordinary => {
            if !entry.blocked_paths().is_empty() {
                RecoveryDetailsTab::BlockedPaths
            } else if !entry.incomplete_paths().is_empty() {
                RecoveryDetailsTab::IncompleteFiles
            } else {
                RecoveryDetailsTab::RenamedSources
            }
        }
    };
    app.recovery_ui.open_details(tab, reason);
    request_incomplete_sizes_if_needed(app, tx);
}

fn select_adjacent_tab(app: &mut AppState, delta: isize, tx: &mpsc::Sender<AppMessage>) {
    let current = RecoveryDetailsTab::ALL
        .iter()
        .position(|tab| *tab == app.recovery_ui.details_tab)
        .unwrap_or(0);
    let next = if delta < 0 {
        current.saturating_sub(1)
    } else {
        (current + 1).min(RecoveryDetailsTab::ALL.len() - 1)
    };
    app.recovery_ui.details_tab = RecoveryDetailsTab::ALL[next];
    app.recovery_ui.details_scroll = 0;
    request_incomplete_sizes_if_needed(app, tx);
}

fn activate_details_focus(app: &mut AppState) {
    app.recovery_ui.open_window();
}

pub fn handle_mouse(app: &mut AppState, mouse: MouseEvent, tx: &mpsc::Sender<AppMessage>) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            match app.recovery_ui.surface {
                RecoverySurface::Window => move_selection(&mut app.recovery_ui, -3),
                RecoverySurface::Details
                | RecoverySurface::DiscardConfirm
                | RecoverySurface::BulkDiscardConfirm
                | RecoverySurface::Inspector => scroll_auxiliary(&mut app.recovery_ui, -3),
                RecoverySurface::Prompt => {}
            }
            return;
        }
        MouseEventKind::ScrollDown => {
            match app.recovery_ui.surface {
                RecoverySurface::Window => move_selection(&mut app.recovery_ui, 3),
                RecoverySurface::Details
                | RecoverySurface::DiscardConfirm
                | RecoverySurface::BulkDiscardConfirm
                | RecoverySurface::Inspector => scroll_auxiliary(&mut app.recovery_ui, 3),
                RecoverySurface::Prompt => {}
            }
            return;
        }
        MouseEventKind::Down(MouseButton::Left) => {}
        _ => return,
    }

    let Some(button) = app.button_map.find_button_at(mouse.column, mouse.row) else {
        return;
    };
    match button {
        TuiButton::RecoveryTitle => {
            if app.double_click.register_click(
                TuiButton::RecoveryTitle,
                mouse.column,
                mouse.row,
                tui_file_picker::DOUBLE_CLICK_WINDOW,
            ) {
                app.recovery_ui.maximized = !app.recovery_ui.maximized;
            }
        }
        TuiButton::RecoveryPromptAction(index) => {
            app.recovery_ui.focus = index as usize;
            activate_prompt_focus(app, tx);
        }
        TuiButton::RecoveryListRow(index) if index < app.recovery_ui.entries.len() => {
            app.recovery_ui.selected = index;
            app.recovery_ui.focus = 0;
        }
        TuiButton::RecoveryAction(index) => {
            app.recovery_ui.focus = index as usize;
            activate_window_focus(app, tx);
        }
        TuiButton::RecoveryBulkResume => {
            if app.recovery_ui.resume_count() > 0 {
                resume_reviewed_set(app, tx);
            }
        }
        TuiButton::RecoveryBulkDiscard => {
            if app.recovery_ui.bulk_discard_count() > 0 {
                app.recovery_ui.surface = RecoverySurface::BulkDiscardConfirm;
                app.recovery_ui.focus = 1;
                app.recovery_ui.details_scroll = 0;
            }
        }
        TuiButton::RecoveryDetailsTab(index)
            if (index as usize) < RecoveryDetailsTab::ALL.len() =>
        {
            app.recovery_ui.details_tab = RecoveryDetailsTab::ALL[index as usize];
            app.recovery_ui.details_scroll = 0;
            request_incomplete_sizes_if_needed(app, tx);
        }
        TuiButton::RecoveryDetailsClose => app.recovery_ui.open_window(),
        TuiButton::RecoveryConfirm(index) => {
            app.recovery_ui.focus = index as usize;
            match app.recovery_ui.surface {
                RecoverySurface::DiscardConfirm => {
                    if index == 0 {
                        begin_single_discard(app, tx);
                    } else {
                        app.recovery_ui.open_window();
                    }
                }
                RecoverySurface::BulkDiscardConfirm => {
                    if index == 0 {
                        begin_bulk_discard(app, tx);
                    } else {
                        app.recovery_ui.open_window();
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

pub fn draw(f: &mut Frame, app: &mut AppState, theme: super::theme::Theme) {
    match app.recovery_ui.surface {
        RecoverySurface::Prompt => draw_prompt(f, app, theme),
        RecoverySurface::Window => draw_window(f, app, theme),
        RecoverySurface::Details => draw_details(f, app, theme),
        RecoverySurface::DiscardConfirm => draw_discard_confirm(f, app, theme, false),
        RecoverySurface::BulkDiscardConfirm => draw_discard_confirm(f, app, theme, true),
        RecoverySurface::Inspector => draw_inspector(f, app, theme),
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn surface_rect(area: Rect, width: u16, height: u16, maximized: bool) -> Rect {
    if maximized {
        Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1),
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        )
    } else {
        centered_rect(width, height, area)
    }
}

fn recovery_block(
    f: &mut Frame,
    popup: Rect,
    label: &str,
    accent: Color,
    buttons: &mut ButtonRenderMap,
    theme: super::theme::Theme,
    maximized: bool,
) -> Rect {
    f.render_widget(Clear, popup);
    let indicator = if maximized { '▾' } else { '▸' };
    let block = super::draw::solid_title_block(
        popup,
        format!("{indicator} {label}"),
        accent,
        theme,
    );
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    buttons.record_button(
        TuiButton::RecoveryTitle,
        Rect::new(
            popup.x.saturating_add(1),
            popup.y,
            popup.width.saturating_sub(2),
            1,
        ),
    );
    inner
}

fn draw_prompt(f: &mut Frame, app: &mut AppState, theme: super::theme::Theme) {
    let popup = surface_rect(
        f.size(),
        PROMPT_WIDTH,
        PROMPT_HEIGHT,
        app.recovery_ui.maximized,
    );
    let count = app.recovery_ui.entries.len();
    let inner = recovery_block(
        f,
        popup,
        "interrupted operations",
        theme.purple,
        &mut app.button_map,
        theme,
        app.recovery_ui.maximized,
    );
    if inner.height == 0 {
        return;
    }
    f.render_widget(
        Paragraph::new(format!("{count} {}", if count == 1 { "entry" } else { "entries" }))
            .alignment(Alignment::Right)
            .style(Style::default().fg(theme.text_muted)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    if inner.height >= 3 {
        f.render_widget(
            Paragraph::new(format!(
                "{count} file operation{} did not finish.\nCopy and move are recoverable. Interrupted deletes are not recorded.",
                plural(count),
            ))
            .style(Style::default().fg(theme.text)),
            Rect::new(
                inner.x.saturating_add(1),
                inner.y.saturating_add(1),
                inner.width.saturating_sub(2),
                2,
            ),
        );
    }

    let row_y = inner.y.saturating_add(4);
    let max_rows = inner.height.saturating_sub(8) as usize;
    let shown = app.recovery_ui.entries.len().min(max_rows.max(1));
    for (row, entry) in app.recovery_ui.entries.iter().take(shown).enumerate() {
        draw_compact_entry_row(
            f,
            entry,
            Rect::new(
                inner.x.saturating_add(1),
                row_y.saturating_add(row as u16),
                inner.width.saturating_sub(2),
                1,
            ),
            theme,
        );
    }
    if app.recovery_ui.entries.len() > shown && row_y + (shown as u16) < inner.y + inner.height {
        f.render_widget(
            Paragraph::new(format!(
                "... and {} more",
                app.recovery_ui.entries.len() - shown
            ))
            .style(Style::default().fg(theme.text_dim)),
            Rect::new(
                inner.x.saturating_add(2),
                row_y.saturating_add(shown as u16),
                inner.width.saturating_sub(3),
                1,
            ),
        );
    }

    let action_y = inner.y + inner.height.saturating_sub(3);
    let mut labels = vec![("Review".to_string(), theme.cyan)];
    if app.recovery_ui.resume_all_available() {
        labels.push(("Resume all".to_string(), theme.success));
    }
    labels.push(("Later".to_string(), theme.dismiss));
    draw_button_row_owned(
        f,
        Rect::new(
            inner.x.saturating_add(1),
            action_y,
            inner.width.saturating_sub(2),
            1,
        ),
        labels,
        app.recovery_ui.focus,
        |index| TuiButton::RecoveryPromptAction(index as u8),
        &mut app.button_map,
        theme,
    );
    f.render_widget(
        Paragraph::new(
            "▲ Later keeps their paths blocked and undo unavailable.",
        )
        .style(Style::default().fg(theme.warning)),
        Rect::new(
            inner.x.saturating_add(1),
            inner.y + inner.height.saturating_sub(1),
            inner.width.saturating_sub(2),
            1,
        ),
    );
}

fn draw_compact_entry_row(
    f: &mut Frame,
    entry: &RecoveryEntry,
    area: Rect,
    theme: super::theme::Theme,
) {
    let total = entry.item_count();
    let completed = entry.completed_count().min(total);
    let filled = if total == 0 {
        0
    } else {
        completed.saturating_mul(8) / total
    };
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(8 - filled));
    let destination = entry
        .record
        .as_ref()
        .map(summarize_destination)
        .unwrap_or_else(|| "record inspection required".to_string());
    let line = Line::from(vec![
        Span::styled(
            format!("{:<5}", entry.kind_label()),
            Style::default().fg(theme.text_bright),
        ),
        Span::styled(
            format!(" {:>3} item{} ", total, plural(total)),
            Style::default().fg(theme.text_muted),
        ),
        Span::styled(
            format!("{:<18}", entry.state_label()),
            Style::default().fg(state_color(entry, theme)),
        ),
        Span::styled(format!(" {bar} "), Style::default().fg(theme.cyan)),
        Span::styled(
            format!("{completed:>2}/{total:<2}  "),
            Style::default().fg(theme.text_muted),
        ),
        Span::styled(destination, Style::default().fg(theme.text)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn state_color(entry: &RecoveryEntry, theme: super::theme::Theme) -> Color {
    if entry.is_unreadable() || entry.irreversible() {
        theme.error
    } else if entry.is_live() {
        theme.info
    } else if entry.state_label() == "cleanup pending" {
        theme.warning
    } else if entry.state_label() == "completed" {
        theme.success
    } else {
        theme.info
    }
}

fn draw_window(f: &mut Frame, app: &mut AppState, theme: super::theme::Theme) {
    let popup = surface_rect(
        f.size(),
        WINDOW_WIDTH,
        WINDOW_HEIGHT,
        app.recovery_ui.maximized,
    );
    let count = app.recovery_ui.entries.len();
    let inner = recovery_block(
        f,
        popup,
        "recovery · copy and move",
        theme.cyan,
        &mut app.button_map,
        theme,
        app.recovery_ui.maximized,
    );
    if inner.width < 20 || inner.height < 5 {
        return;
    }
    f.render_widget(
        Paragraph::new(format!("{count} interrupted"))
            .alignment(Alignment::Right)
            .style(Style::default().fg(theme.text_muted)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let footer_height = 3u16.min(inner.height.saturating_sub(1));
    let body_y = inner.y.saturating_add(1);
    let body_height = inner.height.saturating_sub(1 + footer_height);
    let left_width = if inner.width >= 70 {
        (inner.width * 43 / 100).max(30).min(inner.width.saturating_sub(20))
    } else {
        inner.width / 2
    };
    let divider_x = inner.x.saturating_add(left_width);
    let left = Rect::new(inner.x, body_y, left_width, body_height);
    let right = Rect::new(
        divider_x.saturating_add(1),
        body_y,
        inner.width.saturating_sub(left_width + 1),
        body_height,
    );
    if body_height > 0 {
        f.render_widget(
            Paragraph::new("│\n".repeat(body_height as usize))
                .style(Style::default().fg(theme.border_dim)),
            Rect::new(divider_x, body_y, 1, body_height),
        );
    }
    draw_recovery_list(f, app, left, theme);
    draw_recovery_detail_pane(f, app, right, theme);

    let footer_y = inner.y + inner.height.saturating_sub(2);
    let partition_y = footer_y.saturating_sub(1);
    if popup.width >= 3 && partition_y > popup.y {
        let divider_offset = divider_x.saturating_sub(popup.x) as usize;
        let mut partition = vec!['─'; popup.width as usize];
        partition[0] = '├';
        if let Some(last) = partition.last_mut() {
            *last = '┤';
        }
        if divider_offset < partition.len().saturating_sub(1) {
            partition[divider_offset] = '┴';
        }
        f.render_widget(
            Paragraph::new(partition.into_iter().collect::<String>())
                .style(Style::default().fg(theme.border_dim)),
            Rect::new(popup.x, partition_y, popup.width, 1),
        );
    }
    let resume = app.recovery_ui.resume_count();
    let discard = app.recovery_ui.bulk_discard_count();
    let attention = app.recovery_ui.needs_attention_count();
    let primary_count = app
        .recovery_ui
        .selected_entry()
        .map(window_actions)
        .map_or(0, |actions| actions.len());
    let resume_focus = (resume > 0).then_some(primary_count);
    let discard_focus = (discard > 0)
        .then_some(primary_count + usize::from(resume > 0));
    let resume_label = format!("Resume {resume}");
    let discard_label = format!("Discard and restore {discard}");
    let separator = " · ";
    let resume_style = if resume_focus == Some(app.recovery_ui.focus) {
        Style::default()
            .fg(theme.bg)
            .bg(theme.success)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.success).add_modifier(Modifier::BOLD)
    };
    let discard_style = if discard_focus == Some(app.recovery_ui.focus) {
        Style::default()
            .fg(theme.bg)
            .bg(theme.warning)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.warning).add_modifier(Modifier::BOLD)
    };

    let footer_x = inner.x.saturating_add(1);
    let mut cursor_x = footer_x;
    let mut footer_spans = Vec::new();
    if resume > 0 {
        app.button_map.record_button(
            TuiButton::RecoveryBulkResume,
            Rect::new(cursor_x, footer_y, resume_label.chars().count() as u16, 1),
        );
        footer_spans.push(Span::styled(resume_label.clone(), resume_style));
        cursor_x = cursor_x.saturating_add(resume_label.chars().count() as u16);
    }
    if discard > 0 {
        if !footer_spans.is_empty() {
            footer_spans.push(Span::styled(
                separator,
                Style::default().fg(theme.text_muted),
            ));
            cursor_x = cursor_x.saturating_add(separator.chars().count() as u16);
        }
        app.button_map.record_button(
            TuiButton::RecoveryBulkDiscard,
            Rect::new(
                cursor_x,
                footer_y,
                discard_label.chars().count() as u16,
                1,
            ),
        );
        footer_spans.push(Span::styled(discard_label.clone(), discard_style));
    }
    if !footer_spans.is_empty() {
        footer_spans.push(Span::styled(
            separator,
            Style::default().fg(theme.text_muted),
        ));
    }
    footer_spans.push(Span::styled(
        format!("{attention} needs attention"),
        Style::default().fg(theme.text_muted),
    ));
    f.render_widget(
        Paragraph::new(Line::from(footer_spans)),
        Rect::new(
            footer_x,
            footer_y,
            inner.width.saturating_sub(2),
            1,
        ),
    );
    f.render_widget(
        Paragraph::new(format!(
            "▲ {count} unresolved · some paths remain blocked and undo remains unavailable"
        ))
        .style(Style::default().fg(theme.warning)),
        Rect::new(
            inner.x.saturating_add(1),
            footer_y.saturating_add(1),
            inner.width.saturating_sub(2),
            1,
        ),
    );
}

fn draw_recovery_list(f: &mut Frame, app: &mut AppState, area: Rect, theme: super::theme::Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let visible = area.height as usize;
    if app.recovery_ui.selected < app.recovery_ui.list_scroll {
        app.recovery_ui.list_scroll = app.recovery_ui.selected;
    }
    if app.recovery_ui.selected >= app.recovery_ui.list_scroll.saturating_add(visible) {
        app.recovery_ui.list_scroll = app
            .recovery_ui
            .selected
            .saturating_add(1)
            .saturating_sub(visible);
    }
    for row in 0..visible {
        let index = app.recovery_ui.list_scroll + row;
        let Some(entry) = app.recovery_ui.entries.get(index) else {
            break;
        };
        let y = area.y.saturating_add(row as u16);
        let selected = index == app.recovery_ui.selected;
        let background = if selected {
            theme.selection_bg
        } else {
            theme.panel_bg
        };
        let total = entry.item_count();
        let completed = entry.completed_count().min(total);
        let filled = if total == 0 {
            0
        } else {
            completed.saturating_mul(8) / total
        };
        let bar = format!("{}{}", "█".repeat(filled), "░".repeat(8 - filled));
        let line = Line::from(vec![
            Span::styled(
                format!(" {:<4} {:>2} ", entry.kind_label(), total),
                Style::default().fg(theme.text_bright).bg(background),
            ),
            Span::styled(
                format!("{:<17}", entry.state_label()),
                Style::default()
                    .fg(state_color(entry, theme))
                    .bg(background),
            ),
            Span::styled(
                format!(" {bar} {completed:>2}/{total:<2}"),
                Style::default().fg(theme.text).bg(background),
            ),
        ]);
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(background)),
            Rect::new(area.x, y, area.width, 1),
        );
        app.button_map.record_button(
            TuiButton::RecoveryListRow(index),
            Rect::new(area.x, y, area.width, 1),
        );
    }
}

fn draw_recovery_detail_pane(
    f: &mut Frame,
    app: &mut AppState,
    area: Rect,
    theme: super::theme::Theme,
) {
    let Some(entry) = app.recovery_ui.selected_entry() else {
        f.render_widget(
            Paragraph::new("No interrupted operations.")
                .style(Style::default().fg(theme.text_muted)),
            area,
        );
        return;
    };
    if area.height == 0 || area.width == 0 {
        return;
    }
    if entry.is_unreadable() || entry.record.is_none() {
        let diagnostic = entry
            .diagnostic
            .as_deref()
            .unwrap_or("The operation record could not be interpreted safely.");
        let text = format!(
            "Record unreadable\n\nNo automatic resume or discard is offered.\n\n{}",
            user_safe_diagnostic(diagnostic)
        );
        let text_height = area.height.saturating_sub(2);
        f.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(theme.error)),
            Rect::new(
                area.x.saturating_add(1),
                area.y,
                area.width.saturating_sub(2),
                text_height,
            ),
        );
        draw_window_actions(f, app, area, theme);
        return;
    }
    let Some(record) = entry.record.as_ref() else {
        return;
    };
    let source = summarize_sources(record);
    let destination = summarize_destination(record);
    let overlap = overlapping_entry_count(&app.recovery_ui.entries, entry);
    let action_height = if window_actions(entry)
        .iter()
        .any(|action| *action != WindowAction::Details)
    {
        3
    } else {
        1
    };
    let text_area = Rect::new(
        area.x.saturating_add(1),
        area.y,
        area.width.saturating_sub(2),
        area.height.saturating_sub(action_height),
    );
    let header_left = format!(
        "{} · {} item{}",
        entry.kind_label(),
        entry.item_count(),
        plural(entry.item_count())
    );
    let header_right = format!(
        "{} · {}",
        if entry.is_live() {
            "still running"
        } else {
            "no longer running"
        },
        age_label(record.created_unix_ms)
    );
    let header_width = text_area.width as usize;
    let header_gap = header_width.saturating_sub(
        header_left
            .chars()
            .count()
            .saturating_add(header_right.chars().count()),
    );
    let header = if header_gap > 0 {
        format!("{header_left}{}{header_right}", " ".repeat(header_gap))
    } else {
        format!("{header_left} · {}", age_label(record.created_unix_ms))
    };
    let mut lines = vec![
        Line::from(Span::styled(
            header,
            Style::default()
                .fg(theme.text_bright)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("From: ", Style::default().fg(theme.text_muted)),
            Span::styled(source, Style::default().fg(theme.text)),
        ]),
        Line::from(vec![
            Span::styled("To:   ", Style::default().fg(theme.text_muted)),
            Span::styled(destination, Style::default().fg(theme.text)),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            outcome_summary(entry),
            Style::default().fg(theme.info),
        )),
        Line::from(Span::styled(
            reversibility_statement(entry),
            Style::default().fg(if entry.irreversible() {
                theme.error
            } else {
                theme.warning
            }),
        )),
        Line::from(Span::styled(
            if record.native_rename_intents.is_empty() {
                format!(
                    "On disk: {} incomplete destination file{} · {} renamed source file{}",
                    record.temp_artifacts.len(),
                    plural(record.temp_artifacts.len()),
                    record.quarantine_artifacts.len(),
                    plural(record.quarantine_artifacts.len())
                )
            } else {
                format!(
                    "On disk: {} incomplete destination file{} · {} renamed source file{} · {} direct move{} to recheck",
                    record.temp_artifacts.len(),
                    plural(record.temp_artifacts.len()),
                    record.quarantine_artifacts.len(),
                    plural(record.quarantine_artifacts.len()),
                    record.native_rename_intents.len(),
                    plural(record.native_rename_intents.len())
                )
            },
            Style::default().fg(theme.text_muted),
        )),
        Line::from(Span::styled(
            format!(
                "Blocking: {} path{} unavailable to other operations until resolved",
                entry.blocked_paths().len(),
                plural(entry.blocked_paths().len())
            ),
            Style::default().fg(theme.text_muted),
        )),
        Line::raw(""),
    ];
    if entry.is_live() {
        let process = record
            .origin_owner
            .map(|owner| format!("process {}", owner.pid))
            .unwrap_or_else(|| "another process".to_string());
        lines.push(Line::from(Span::styled(
            format!(
                "Still running: {process} · last progress {}",
                age_label(record.updated_unix_ms)
            ),
            Style::default().fg(theme.warning),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("Last progress {}", age_label(record.updated_unix_ms)),
            Style::default().fg(theme.text_muted),
        )));
    }
    if entry.resume_committed {
        lines.push(Line::from(Span::styled(
            "Resume approved; this operation is waiting in or entering the transfer queue.",
            Style::default().fg(theme.info),
        )));
    }
    if entry.discard_committed {
        lines.push(Line::from(Span::styled(
            "Discard approved; reviewed cleanup is in progress.",
            Style::default().fg(theme.warning),
        )));
    }
    if entry.session_deferred {
        lines.push(Line::from(Span::styled(
            "Saved for later in this session; restart Tonepoet to make Resume available again",
            Style::default().fg(theme.warning),
        )));
    }
    if !record.native_rename_intents.is_empty() {
        lines.push(Line::from(Span::styled(
            "This move stopped around a direct rename. Discard rechecks both paths and restores the original source only when the saved file identity makes that safe.",
            Style::default().fg(theme.warning),
        )));
    }
    if overlap > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "Review order: this destination overlaps {overlap} other unresolved operation{}.",
                plural(overlap)
            ),
            Style::default().fg(theme.warning),
        )));
    }
    lines.push(Line::from(Span::styled(
        format!(
            "Started {} · last progress {}",
            timestamp_clock(record.created_unix_ms),
            timestamp_clock(record.updated_unix_ms)
        ),
        Style::default().fg(theme.text_dim),
    )));
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        text_area,
    );
    draw_window_actions(f, app, area, theme);
}

fn draw_window_actions(f: &mut Frame, app: &mut AppState, area: Rect, theme: super::theme::Theme) {
    let Some(entry) = app.recovery_ui.selected_entry() else {
        return;
    };
    let actions = window_actions(entry);
    if actions.is_empty() || area.height == 0 {
        return;
    }

    // The details control belongs with the On disk / Blocking facts, not in
    // the decision row. Keep its action index stable so keyboard and mouse
    // activation share the same dispatch path.
    if let Some(details_index) = actions
        .iter()
        .position(|action| *action == WindowAction::Details)
    {
        let label = " Show these files and paths ";
        let width = (label.chars().count() as u16).min(area.width.saturating_sub(2));
        let y = area
            .y
            .saturating_add(8)
            .min(area.y.saturating_add(area.height.saturating_sub(1)));
        let style = if app.recovery_ui.focus == details_index {
            Style::default()
                .fg(theme.bg)
                .bg(theme.cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD)
        };
        let rect = Rect::new(area.x.saturating_add(1), y, width, 1);
        f.render_widget(Paragraph::new(label).style(style), rect);
        app.button_map.record_button(
            TuiButton::RecoveryAction(details_index as u8),
            rect,
        );
    }

    let decision = actions
        .iter()
        .enumerate()
        .filter(|(_, action)| **action != WindowAction::Details)
        .collect::<Vec<_>>();
    if decision.is_empty() {
        return;
    }
    let labels = decision
        .iter()
        .map(|(_, action)| match action {
            WindowAction::Resume => ("Resume".to_string(), theme.success),
            WindowAction::Discard if entry.is_discard_cleanup() => {
                ("Finish cleanup".to_string(), theme.warning)
            }
            WindowAction::Discard if entry.irreversible() => {
                ("Discard, keep what is on disk".to_string(), theme.error)
            }
            WindowAction::Discard => ("Discard and restore".to_string(), theme.warning),
            WindowAction::Inspect => ("Record inspector".to_string(), theme.cyan),
            WindowAction::Later => ("Decide later".to_string(), theme.dismiss),
            WindowAction::Details => unreachable!("details is rendered above the decision row"),
        })
        .collect::<Vec<_>>();
    let action_indices = decision.iter().map(|(index, _)| *index).collect::<Vec<_>>();
    let decision_focus = action_indices
        .iter()
        .position(|index| *index == app.recovery_ui.focus)
        .unwrap_or(usize::MAX);
    let y = area.y + area.height.saturating_sub(2);
    draw_button_row_owned(
        f,
        Rect::new(
            area.x.saturating_add(1),
            y,
            area.width.saturating_sub(2),
            1,
        ),
        labels,
        decision_focus,
        |local_index| TuiButton::RecoveryAction(action_indices[local_index] as u8),
        &mut app.button_map,
        theme,
    );
}

fn details_restored_height(state: &RecoveryUiState) -> u16 {
    let mut rows = state.details_count(state.details_tab);
    if state.details_tab == RecoveryDetailsTab::BlockedPaths
        && matches!(state.details_reason, RecoveryDetailsReason::Refusal(_))
        && rows > 0
    {
        rows = rows.saturating_add(1);
    }
    (15u16)
        .saturating_add(rows.min(6) as u16)
        .clamp(15, DETAILS_MAX_HEIGHT)
}

fn draw_details(f: &mut Frame, app: &mut AppState, theme: super::theme::Theme) {
    let popup = surface_rect(
        f.size(),
        DETAILS_WIDTH,
        details_restored_height(&app.recovery_ui),
        app.recovery_ui.maximized,
    );
    let inner = recovery_block(
        f,
        popup,
        "recovery details",
        theme.cyan,
        &mut app.button_map,
        theme,
        app.recovery_ui.maximized,
    );
    let Some(entry) = app.recovery_ui.selected_entry() else {
        return;
    };
    if inner.height < 8 {
        return;
    }

    let context_height = 3u16.min(inner.height);
    let context_area = Rect::new(inner.x, inner.y, inner.width, context_height);
    f.render_widget(
        Block::default().style(Style::default().bg(theme.surface)),
        context_area,
    );
    let context = format!(
        " {} · {} item{}\n {} → {}",
        entry.kind_label(),
        entry.item_count(),
        plural(entry.item_count()),
        entry
            .record
            .as_ref()
            .map(summarize_sources)
            .unwrap_or_else(|| "unavailable".to_string()),
        entry
            .record
            .as_ref()
            .map(summarize_destination)
            .unwrap_or_else(|| "unavailable".to_string()),
    );
    f.render_widget(
        Paragraph::new(context).style(Style::default().fg(theme.text)),
        context_area,
    );

    let tabs_area = Rect::new(
        inner.x,
        inner.y.saturating_add(context_height + 1),
        inner.width,
        2,
    );
    draw_details_tabs(f, app, tabs_area, theme);
    let footer_height = 5;
    let inventory_y = tabs_area.y.saturating_add(2);
    let inventory_height = inner
        .y
        .saturating_add(inner.height)
        .saturating_sub(inventory_y)
        .saturating_sub(footer_height);
    draw_details_inventory(
        f,
        app,
        Rect::new(
            inner.x.saturating_add(1),
            inventory_y,
            inner.width.saturating_sub(2),
            inventory_height,
        ),
        theme,
    );
    draw_details_footer(
        f,
        app,
        Rect::new(
            inner.x.saturating_add(1),
            inner.y + inner.height.saturating_sub(footer_height),
            inner.width.saturating_sub(2),
            footer_height,
        ),
        theme,
    );
}

fn draw_details_tabs(f: &mut Frame, app: &mut AppState, area: Rect, theme: super::theme::Theme) {
    let border_style = Style::default().fg(theme.cyan);
    let active_style = Style::default()
        .fg(theme.cyan)
        .add_modifier(Modifier::BOLD);
    let inactive_style = Style::default().fg(theme.text_dim);
    let mut top = vec![Span::raw(" ")];
    let mut x = area.x.saturating_add(1);
    let mut slots = Vec::new();
    for (index, tab) in RecoveryDetailsTab::ALL.iter().copied().enumerate() {
        let label = tab.label();
        let count = format!(" · {}", app.recovery_ui.details_count(tab));
        let active = tab == app.recovery_ui.details_tab;
        let natural_width = (label.chars().count()
            + count.chars().count()
            + if active { 6 } else { 2 }) as u16;
        let remaining = area.x.saturating_add(area.width).saturating_sub(x);
        if remaining == 0 {
            break;
        }
        let width = natural_width.min(remaining);
        slots.push((active, width));
        if active {
            top.push(Span::styled("┌─ ", border_style));
            top.push(Span::styled(label, active_style));
            top.push(Span::styled(count, Style::default().fg(theme.text_muted)));
            top.push(Span::styled(" ─┐", border_style));
        } else {
            top.push(Span::styled(format!(" {label}"), inactive_style));
            top.push(Span::styled(count, Style::default().fg(theme.text_dim)));
            top.push(Span::raw(" "));
        }
        app.button_map.record_button(
            TuiButton::RecoveryDetailsTab(index as u8),
            Rect::new(x, area.y, width, 1),
        );
        top.push(Span::raw(" "));
        x = x.saturating_add(width.saturating_add(1));
    }
    f.render_widget(
        Paragraph::new(Line::from(top)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    if area.height >= 2 {
        let mut bottom = String::from(" ");
        for (active, width) in slots {
            if active {
                bottom.push('┘');
                bottom.push_str(&" ".repeat(width.saturating_sub(2) as usize));
                bottom.push('└');
            } else {
                bottom.push_str(&"─".repeat(width as usize));
            }
            bottom.push('─');
        }
        bottom.push_str(&"─".repeat(
            (area.width as usize).saturating_sub(super::display_width::width(&bottom)),
        ));
        f.render_widget(
            Paragraph::new(Span::styled(bottom, border_style)),
            Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
        );
    }
}

fn draw_details_inventory(f: &mut Frame, app: &AppState, area: Rect, theme: super::theme::Theme) {
    let Some(entry) = app.recovery_ui.selected_entry() else {
        return;
    };
    if area.height == 0 {
        return;
    }
    match app.recovery_ui.details_tab {
        RecoveryDetailsTab::BlockedPaths => {
            let rows = entry.blocked_paths();
            let start = app.recovery_ui.details_scroll.min(rows.len());
            let mut y = area.y;
            for path in rows.into_iter().skip(start) {
                if y >= area.y.saturating_add(area.height) {
                    break;
                }
                let highlighted = matches!(
                    &app.recovery_ui.details_reason,
                    RecoveryDetailsReason::Refusal(blocked) if blocked == &path
                );
                let style = if highlighted {
                    Style::default()
                        .fg(theme.text_bright)
                        .bg(theme.selection_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text)
                };
                f.render_widget(
                    Paragraph::new(path.display().to_string()).style(style),
                    Rect::new(area.x, y, area.width, 1),
                );
                y = y.saturating_add(1);
                if highlighted && y < area.y.saturating_add(area.height) {
                    f.render_widget(
                        Paragraph::new("Blocked your attempted move")
                            .style(Style::default().fg(theme.warning).bg(theme.selection_bg)),
                        Rect::new(area.x, y, area.width, 1),
                    );
                    y = y.saturating_add(1);
                }
            }
        }
        RecoveryDetailsTab::IncompleteFiles => {
            let rows = entry.incomplete_paths();
            let start = app.recovery_ui.details_scroll.min(rows.len());
            for (offset, path) in rows
                .into_iter()
                .skip(start)
                .take(area.height as usize)
                .enumerate()
            {
                let y = area.y.saturating_add(offset as u16);
                let right = match app.recovery_ui.incomplete_sizes.get(&path) {
                    Some(Some(size)) => human_bytes(*size),
                    Some(None) => "size unavailable".to_string(),
                    None if app
                        .recovery_ui
                        .size_requests
                        .contains(&entry.journal_path) =>
                    {
                        "checking...".to_string()
                    }
                    None => "size unavailable".to_string(),
                };
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                draw_left_right_row(f, area, y, &name, &right, theme);
            }
        }
        RecoveryDetailsTab::RenamedSources => {
            let rows = entry.renamed_sources();
            let start = app.recovery_ui.details_scroll.min(rows.len());
            for (offset, path) in rows
                .into_iter()
                .skip(start)
                .take(area.height as usize)
                .enumerate()
            {
                let y = area.y.saturating_add(offset as u16);
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                f.render_widget(
                    Paragraph::new(name).style(Style::default().fg(theme.text)),
                    Rect::new(area.x, y, area.width, 1),
                );
            }
        }
    }
}

fn draw_left_right_row(
    f: &mut Frame,
    area: Rect,
    y: u16,
    left: &str,
    right: &str,
    theme: super::theme::Theme,
) {
    let right_width = right.chars().count() as u16;
    let left_width = area.width.saturating_sub(right_width.saturating_add(1));
    f.render_widget(
        Paragraph::new(left).style(Style::default().fg(theme.text)),
        Rect::new(area.x, y, left_width, 1),
    );
    f.render_widget(
        Paragraph::new(right)
            .alignment(Alignment::Right)
            .style(Style::default().fg(theme.text_muted)),
        Rect::new(
            area.x.saturating_add(left_width),
            y,
            area.width.saturating_sub(left_width),
            1,
        ),
    );
}

fn draw_details_footer(f: &mut Frame, app: &mut AppState, area: Rect, theme: super::theme::Theme) {
    let Some(entry) = app.recovery_ui.selected_entry() else {
        return;
    };
    if area.height == 0 || area.width == 0 {
        return;
    }
    let record = entry.record.as_ref();
    let incomplete = record.map_or(0, |record| record.temp_artifacts.len());
    let renamed = record.map_or(0, |record| record.quarantine_artifacts.len());
    let released = entry.blocked_paths().len();

    f.render_widget(
        Paragraph::new("─".repeat(area.width as usize))
            .style(Style::default().fg(theme.border_dim)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    if area.height >= 2 {
        f.render_widget(
            Paragraph::new("If resumed     continue from partial files · paths remain blocked")
                .style(Style::default().fg(theme.text_muted)),
            Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
        );
    }
    if area.height >= 4 {
        let discarded = if entry.is_discard_cleanup() {
            format!(
                "If discarded   finish cleanup · release {released} blocked path{}",
                plural(released)
            )
        } else if entry.irreversible() {
            format!(
                "If discarded   delete {incomplete} incomplete · restore surviving renamed sources · keep current output · release {released} blocked path{}",
                plural(released)
            )
        } else if record.is_some_and(|record| !record.native_rename_intents.is_empty()) {
            format!(
                "If discarded   delete {incomplete} incomplete · restore source names where the saved file identity still matches · release {released} blocked path{}",
                plural(released)
            )
        } else {
            format!(
                "If discarded   delete {incomplete} incomplete · restore {renamed} source name{} · release {released} blocked path{}",
                plural(renamed),
                plural(released)
            )
        };
        f.render_widget(
            Paragraph::new(discarded)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(if entry.irreversible() {
                    theme.error
                } else {
                    theme.warning
                })),
            Rect::new(area.x, area.y.saturating_add(2), area.width, 2),
        );
    }

    let y = area.y + area.height.saturating_sub(1);
    let close = " Close ";
    let close_width = (close.chars().count() as u16).min(area.width);
    let close_x = area
        .x
        .saturating_add(area.width.saturating_sub(close_width));
    let close_style = if app.recovery_ui.focus == 0 {
        Style::default()
            .fg(theme.bg)
            .bg(theme.dismiss)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme.dismiss)
            .add_modifier(Modifier::BOLD)
    };
    f.render_widget(
        Paragraph::new(close).style(close_style),
        Rect::new(close_x, y, close_width, 1),
    );
    app.button_map.record_button(
        TuiButton::RecoveryDetailsClose,
        Rect::new(close_x, y, close_width, 1),
    );

    let esc = "Esc";
    let esc_width = esc.chars().count() as u16;
    if close_x >= area.x.saturating_add(esc_width.saturating_add(2)) {
        f.render_widget(
            Paragraph::new(esc)
                .style(Style::default().fg(theme.text_dim)),
            Rect::new(close_x.saturating_sub(esc_width + 2), y, esc_width, 1),
        );
    }
}

fn draw_discard_confirm(
    f: &mut Frame,
    app: &mut AppState,
    theme: super::theme::Theme,
    bulk: bool,
) {
    let itemized_rows = if bulk {
        app.recovery_ui
            .entries
            .iter()
            .filter(|entry| entry.show_in_bulk_discard_confirm())
            .count()
            .saturating_mul(2)
    } else {
        6
    };
    let restored_height = (9u16)
        .saturating_add(itemized_rows.min(12) as u16)
        .clamp(13, CONFIRM_MAX_HEIGHT);
    let popup = surface_rect(
        f.size(),
        CONFIRM_WIDTH,
        restored_height,
        app.recovery_ui.maximized,
    );
    let irreversible = !bulk
        && app
            .recovery_ui
            .selected_entry()
            .is_some_and(RecoveryEntry::irreversible);
    let label = if bulk {
        "discard and restore interrupted operations"
    } else {
        "discard interrupted operation"
    };
    let inner = recovery_block(
        f,
        popup,
        label,
        if irreversible { theme.error } else { theme.warning },
        &mut app.button_map,
        theme,
        app.recovery_ui.maximized,
    );
    if inner.height < 5 {
        return;
    }

    let header = if bulk {
        let count = app
            .recovery_ui
            .entries
            .iter()
            .filter(|entry| entry.show_in_bulk_discard_confirm())
            .count();
        bulk_discard_header(count)
    } else {
        app.recovery_ui
            .selected_entry()
            .map(single_discard_header)
            .unwrap_or_else(|| "Discard this interrupted copy/move operation:".to_string())
    };
    f.render_widget(
        Paragraph::new(header)
            .style(Style::default().fg(theme.text_bright).add_modifier(Modifier::BOLD)),
        Rect::new(
            inner.x.saturating_add(1),
            inner.y.saturating_add(1),
            inner.width.saturating_sub(2),
            1,
        ),
    );

    let body_y = inner.y.saturating_add(3);
    let body_height = inner.height.saturating_sub(6);
    if bulk {
        draw_bulk_discard_items(
            f,
            app,
            Rect::new(
                inner.x.saturating_add(1),
                body_y,
                inner.width.saturating_sub(2),
                body_height,
            ),
            theme,
        );
    } else {
        draw_single_discard_effects(
            f,
            app,
            Rect::new(
                inner.x.saturating_add(1),
                body_y,
                inner.width.saturating_sub(2),
                body_height,
            ),
            theme,
        );
    }

    let working = if bulk {
        app.recovery_ui.entries.iter().any(|entry| {
            app.recovery_ui
                .discard_in_flight
                .contains(&entry.journal_path)
        })
    } else {
        app.recovery_ui.selected_entry().is_some_and(|entry| {
            app.recovery_ui
                .discard_in_flight
                .contains(&entry.journal_path)
        })
    };
    let y = inner.y + inner.height.saturating_sub(2);
    if working {
        f.render_widget(
            Paragraph::new("Applying reviewed discard...")
                .style(Style::default().fg(theme.warning)),
            Rect::new(
                inner.x.saturating_add(1),
                y,
                inner.width.saturating_sub(2),
                1,
            ),
        );
    } else {
        let confirm = if bulk {
            "Discard and restore"
        } else if app
            .recovery_ui
            .selected_entry()
            .is_some_and(RecoveryEntry::is_discard_cleanup)
        {
            "Finish cleanup"
        } else if irreversible {
            "Discard, keep what is on disk"
        } else {
            "Discard and restore"
        };
        draw_button_row_owned(
            f,
            Rect::new(
                inner.x.saturating_add(1),
                y,
                inner.width.saturating_sub(2),
                1,
            ),
            vec![
                (
                    confirm.to_string(),
                    if irreversible { theme.error } else { theme.warning },
                ),
                ("Cancel".to_string(), theme.dismiss),
            ],
            app.recovery_ui.focus,
            |index| TuiButton::RecoveryConfirm(index as u8),
            &mut app.button_map,
            theme,
        );
    }
}

fn draw_single_discard_effects(
    f: &mut Frame,
    app: &AppState,
    area: Rect,
    theme: super::theme::Theme,
) {
    let Some(entry) = app.recovery_ui.selected_entry() else {
        return;
    };
    let record = entry.record.as_ref();
    let incomplete = record.map_or(0, |record| record.temp_artifacts.len());
    let renamed = record.map_or(0, |record| record.quarantine_artifacts.len());
    let direct_rename = record.map_or(0, |record| record.native_rename_intents.len());
    let released = entry.blocked_paths().len();
    let mut lines = Vec::new();
    if incomplete > 0 || !entry.is_discard_cleanup() {
        lines.push(Line::from(format!(
            "[x] delete {incomplete} incomplete destination file{}",
            plural(incomplete)
        )));
    }
    if entry.irreversible() {
        let deletion_started = record.map_or(0, |record| {
            record
                .quarantine_artifacts
                .iter()
                .filter(|artifact| artifact.state == DurableQuarantineState::DeletionStarted)
                .count()
        });
        lines.push(Line::from(format!(
            "[x] source deletion had begun for {deletion_started} source root{}; deleted entries cannot be restored",
            plural(deletion_started)
        )));
        if renamed > 0 {
            lines.push(Line::from("[x] restore any surviving source names that were renamed"));
        }
        lines.push(Line::from("[x] keep completed output on disk"));
    } else if renamed > 0 || !entry.is_discard_cleanup() {
        lines.push(Line::from(format!(
            "[x] restore source names where needed for {renamed} affected source root{}",
            plural(renamed)
        )));
    }
    if direct_rename > 0 {
        lines.push(Line::from(format!(
            "[x] recheck {direct_rename} direct move{}; restore the original source only when its saved identity matches",
            plural(direct_rename)
        )));
    }
    lines.push(Line::from(format!(
        "[x] release {released} blocked path{}",
        plural(released)
    )));
    lines.push(Line::from(if entry.is_discard_cleanup() {
        "[x] finish removing this operation from recovery"
    } else {
        "[x] remove this operation from recovery"
    }));
    if entry.irreversible() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "Some source files were already deleted and cannot be restored.",
            Style::default().fg(theme.error).add_modifier(Modifier::BOLD),
        )));
    }
    let start = app.recovery_ui.details_scroll.min(lines.len());
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(start).collect::<Vec<_>>())
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(theme.text)),
        area,
    );
}

fn draw_bulk_discard_items(
    f: &mut Frame,
    app: &AppState,
    area: Rect,
    theme: super::theme::Theme,
) {
    let mut rows = Vec::new();
    for entry in app
        .recovery_ui
        .entries
        .iter()
        .filter(|entry| entry.show_in_bulk_discard_confirm())
    {
        let Some(record) = entry.record.as_ref() else {
            continue;
        };
        let incomplete = record.temp_artifacts.len();
        let renamed = record.quarantine_artifacts.len();
        let direct_rename = record.native_rename_intents.len();
        let released = entry.blocked_paths().len();
        rows.push(Line::from(Span::styled(
            format!(
                "{} · {} item{} · {}",
                entry.kind_label(),
                entry.item_count(),
                plural(entry.item_count()),
                summarize_destination(record)
            ),
            Style::default().fg(theme.text_bright).add_modifier(Modifier::BOLD),
        )));
        let effects = if direct_rename > 0 {
            format!(
                "  delete {incomplete} incomplete · restore {renamed} source name{} · recheck {direct_rename} direct move{} · release {released} path{}",
                plural(renamed),
                plural(direct_rename),
                plural(released)
            )
        } else {
            format!(
                "  delete {incomplete} incomplete · restore {renamed} source name{} · release {released} path{}",
                plural(renamed),
                plural(released)
            )
        };
        rows.push(Line::from(Span::styled(
            effects,
            Style::default().fg(theme.text_muted),
        )));
    }
    let start = app.recovery_ui.details_scroll.min(rows.len());
    f.render_widget(
        Paragraph::new(rows.into_iter().skip(start).collect::<Vec<_>>())
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_inspector(f: &mut Frame, app: &mut AppState, theme: super::theme::Theme) {
    let popup = surface_rect(
        f.size(),
        DETAILS_WIDTH,
        13,
        app.recovery_ui.maximized,
    );
    let inner = recovery_block(
        f,
        popup,
        "record inspector",
        theme.cyan,
        &mut app.button_map,
        theme,
        app.recovery_ui.maximized,
    );
    let Some(entry) = app.recovery_ui.selected_entry() else {
        return;
    };
    let diagnostic = entry
        .diagnostic
        .as_deref()
        .map(user_safe_diagnostic)
        .unwrap_or_else(|| "The saved recovery record could not be interpreted safely.".to_string());
    let lines = vec![
        Line::from(INSPECTOR_SCOPE_LINE),
        Line::raw(""),
        Line::from(Span::styled(
            "Record location:",
            Style::default().fg(theme.text_muted),
        )),
        Line::from(entry.journal_path.display().to_string()),
        Line::raw(""),
        Line::from(Span::styled(
            "Diagnostic:",
            Style::default().fg(theme.text_muted),
        )),
        Line::from(diagnostic),
        Line::raw(""),
        Line::from(Span::styled(
            "Esc returns to recovery.",
            Style::default().fg(theme.text_dim),
        )),
    ];
    let start = app.recovery_ui.details_scroll.min(lines.len());
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(start).collect::<Vec<_>>())
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(theme.text)),
        Rect::new(
            inner.x.saturating_add(1),
            inner.y.saturating_add(1),
            inner.width.saturating_sub(2),
            inner.height.saturating_sub(2),
        ),
    );
}

fn draw_button_row_owned<F>(
    f: &mut Frame,
    area: Rect,
    labels: Vec<(String, Color)>,
    focus: usize,
    button: F,
    buttons: &mut ButtonRenderMap,
    theme: super::theme::Theme,
) where
    F: Fn(usize) -> TuiButton,
{
    let mut x = area.x;
    for (index, (label, color)) in labels.into_iter().enumerate() {
        if x >= area.x.saturating_add(area.width) {
            break;
        }
        let text = format!(" {label} ");
        let width = (text.chars().count() as u16)
            .min(area.x.saturating_add(area.width).saturating_sub(x));
        let style = if index == focus {
            Style::default()
                .fg(theme.bg)
                .bg(color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        };
        f.render_widget(Paragraph::new(text).style(style), Rect::new(x, area.y, width, 1));
        buttons.record_button(button(index), Rect::new(x, area.y, width, 1));
        x = x.saturating_add(width.saturating_add(1));
    }
}

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

fn outcome_summary(entry: &RecoveryEntry) -> String {
    let completed = entry.completed_count();
    let skipped = entry.skipped_count();
    let failed = entry.failed_count();
    let remaining = entry.remaining_count();
    let done_label = if entry.kind_label() == "move" {
        "moved"
    } else {
        "copied"
    };
    format!(
        "Outcome: {done_label} {completed} · remaining {remaining} · skipped {skipped} · failed {failed}"
    )
}

fn reversibility_statement(entry: &RecoveryEntry) -> String {
    let Some(record) = entry.record.as_ref() else {
        return "Automatic changes are unavailable.".to_string();
    };
    if entry.is_discard_cleanup() {
        return "Discard was confirmed; cleanup and release of the remaining blocked paths are not finished."
            .to_string();
    }
    if entry.max_quarantine_state() == Some(DurableQuarantineState::DeletionStarted) {
        let affected = record
            .quarantine_artifacts
            .iter()
            .filter(|artifact| artifact.state == DurableQuarantineState::DeletionStarted)
            .count();
        return format!(
            "Source deletion had begun for {affected} source root{}; some source files may already be gone and cannot be restored.",
            plural(affected)
        );
    }
    if !record.native_rename_intents.is_empty() {
        return format!(
            "{} source root{} stopped during a direct rename; discard will recheck both paths and will not overwrite either one.",
            record.native_rename_intents.len(),
            plural(record.native_rename_intents.len())
        );
    }
    match entry.max_quarantine_state() {
        Some(DurableQuarantineState::DeletionStarted) =>
            "Source deletion had begun; some source files may already be gone and cannot be restored.".to_string(),
        Some(DurableQuarantineState::RenameConfirmed) => format!(
            "{} source root{} temporarily renamed. No source deletion had been recorded yet.",
            record.quarantine_artifacts.len(),
            plural(record.quarantine_artifacts.len())
        ),
        Some(DurableQuarantineState::IntentRecorded) => {
            "A source rename may have started; discard will restore the original name if needed."
                .to_string()
        }
        None => "No source names are waiting to be restored.".to_string(),
    }
}

fn entries_have_destination_overlap(left: &RecoveryEntry, right: &RecoveryEntry) -> bool {
    let (Some(left_record), Some(right_record)) = (left.record.as_ref(), right.record.as_ref())
    else {
        return false;
    };
    left_record.mappings.iter().any(|left_mapping| {
        right_record.mappings.iter().any(|right_mapping| {
            paths_overlap(
                left_mapping.destination.as_path(),
                right_mapping.destination.as_path(),
            )
        })
    })
}

fn entry_has_destination_overlap(entries: &[RecoveryEntry], selected: &RecoveryEntry) -> bool {
    entries
        .iter()
        .filter(|entry| entry.journal_path != selected.journal_path)
        .any(|entry| entries_have_destination_overlap(selected, entry))
}

fn overlapping_entry_count(entries: &[RecoveryEntry], selected: &RecoveryEntry) -> usize {
    entries
        .iter()
        .filter(|entry| entry.journal_path != selected.journal_path)
        .filter(|entry| entries_have_destination_overlap(selected, entry))
        .count()
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

fn timestamp_clock(unix_ms: u64) -> String {
    let Ok(unix_ms) = i64::try_from(unix_ms) else {
        return "--:--".to_string();
    };
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(unix_ms)
        .map(|time| time.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_else(|| "--:--".to_string())
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
    // The specification reserves implementation vocabulary for diagnostics,
    // not ordinary UI. Keep the inspector useful without surfacing those
    // internal nouns verbatim.
    let lowered = raw.to_ascii_lowercase();
    if lowered.contains("schema") {
        "The saved record uses a format this build cannot interpret safely.".to_string()
    } else if lowered.contains("ownership") || lowered.contains("descriptor") {
        "Tonepoet cannot prove that this operation is safe to take over automatically.".to_string()
    } else if lowered.contains("uuid") || lowered.contains("identifier") {
        "The saved operation identifier is invalid.".to_string()
    } else {
        "The saved record could not be interpreted safely. No automatic changes are available."
            .to_string()
    }
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
            discard_committed: false,
            session_deferred: false,
        }
    }

    #[test]
    fn path_overlap_is_symmetric_and_tree_aware() {
        assert!(paths_overlap(Path::new("/a/b"), Path::new("/a")));
        assert!(paths_overlap(Path::new("/a"), Path::new("/a/b")));
        assert!(!paths_overlap(Path::new("/a/b"), Path::new("/a/c")));
    }

    #[test]
    fn byte_sizes_follow_mockup_decimal_units() {
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1_000), "1.0 KB");
        assert_eq!(human_bytes(1_000_000), "1.0 MB");
    }

    #[test]
    fn recovery_tab_labels_match_operator_language() {
        assert_eq!(RecoveryDetailsTab::BlockedPaths.label(), "Blocked paths");
        assert_eq!(RecoveryDetailsTab::IncompleteFiles.label(), "Incomplete files");
        assert_eq!(RecoveryDetailsTab::RenamedSources.label(), "Renamed sources");
    }

    #[test]
    fn decide_later_is_only_offered_for_a_current_deferable_review_entry() {
        let available = test_entry("available", "dest/available", false, Some(11));
        assert!(available.can_defer());
        assert!(window_actions(&available).contains(&WindowAction::Later));

        let mut deferred = available.clone();
        deferred.queue_id = None;
        deferred.session_deferred = true;
        assert!(!deferred.can_defer());
        let deferred_actions = window_actions(&deferred);
        assert!(!deferred_actions.contains(&WindowAction::Resume));
        assert!(!deferred_actions.contains(&WindowAction::Later));

        let mut resume_committed = available.clone();
        resume_committed.resume_committed = true;
        assert!(!resume_committed.can_defer());

        let mut discard_committed = available;
        discard_committed.discard_committed = true;
        assert!(!discard_committed.can_defer());
    }

    #[test]
    fn startup_later_and_window_escape_remain_global_minimize_actions() {
        let _home = crate::tui::test_support::XdgConfigHomeGuard::new(
            "tonepoet-recovery-global-later-and-escape",
        );
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.recovery_ui.entries = vec![test_entry(
            "available",
            "dest/available",
            false,
            Some(11),
        )];
        let (tx, _rx) = mpsc::channel(8);

        app.recovery_ui.surface = RecoverySurface::Prompt;
        app.active_overlay = ActiveOverlay::FileRecovery;
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &tx,
        );
        assert!(app.recovery_ui.session_deferred.is_empty());
        assert_eq!(app.recovery_ui.surface, RecoverySurface::Window);
        assert!(matches!(&app.active_overlay, ActiveOverlay::None));

        app.recovery_ui.surface = RecoverySurface::Window;
        app.active_overlay = ActiveOverlay::FileRecovery;
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &tx,
        );
        assert!(app.recovery_ui.session_deferred.is_empty());
        assert_eq!(app.recovery_ui.surface, RecoverySurface::Window);
        assert!(matches!(&app.active_overlay, ActiveOverlay::None));
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
    fn layer_three_recovery_surfaces_identify_copy_move_scope() {
        let copy = test_entry("copy", "dest/copy", false, Some(41));
        let move_entry = test_entry("move", "dest/move", true, Some(42));

        assert_eq!(
            single_discard_header(&copy),
            "Discard this interrupted copy operation:"
        );
        assert_eq!(
            single_discard_header(&move_entry),
            "Discard this interrupted move operation:"
        );
        assert_eq!(
            bulk_discard_header(2),
            "Discard 2 reversible interrupted copy/move operations:"
        );
        assert_eq!(
            INSPECTOR_SCOPE_LINE,
            "This interrupted copy/move operation cannot be changed automatically."
        );
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
    fn inspector_scroll_cannot_overshoot_empty_content_model() {
        let mut state = RecoveryUiState::default();
        state.surface = RecoverySurface::Inspector;
        state.details_scroll = 99;
        scroll_auxiliary(&mut state, 3);
        assert_eq!(state.details_scroll, 0);
    }
}
