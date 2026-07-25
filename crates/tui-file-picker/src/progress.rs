use crate::theme::FilePickerTheme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_MAX_ERROR_RECORDS: usize = 8;
const DEFAULT_MAX_CONFLICT_RECORDS: usize = 8;
const MIN_PROGRESS_DIALOG_WIDTH: u16 = 52;
const PROGRESS_DIALOG_HEIGHT: u16 = 12;
const CONFLICT_DIALOG_HEIGHT: u16 = 15;

/// Generic category label for a long-running file-oriented task.
///
/// The picker crate uses this only for wording. Hosts remain responsible for
/// performing the operation and enforcing task semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileTaskKind {
    Copy,
    Move,
    Delete,
    Scan,
    Import,
    Export,
    Extract,
    Archive,
    Unarchive,
    Checksum,
    Verify,
    MetadataWriteback,
    Custom(String),
}

impl FileTaskKind {
    pub fn label(&self) -> &str {
        match self {
            Self::Copy => "Copy",
            Self::Move => "Move",
            Self::Delete => "Delete",
            Self::Scan => "Scan",
            Self::Import => "Import",
            Self::Export => "Export",
            Self::Extract => "Extract",
            Self::Archive => "Archive",
            Self::Unarchive => "Unarchive",
            Self::Checksum => "Checksum",
            Self::Verify => "Verify",
            Self::MetadataWriteback => "Metadata writeback",
            Self::Custom(label) => label.as_str(),
        }
    }
}

/// Generic task phase shown in the progress overlay.
///
/// Hosts may use [`FileTaskPhase::Custom`] for task-specific phases such as
/// "Removing source..." after a cross-device move has been verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileTaskPhase {
    Preparing,
    Running,
    Paused,
    Verifying,
    CleaningUp,
    Completed,
    Failed,
    Aborted,
    Custom(String),
}

impl FileTaskPhase {
    pub fn label(&self) -> &str {
        match self {
            Self::Preparing => "Preparing...",
            Self::Running => "Running...",
            Self::Paused => "Paused",
            Self::Verifying => "Verifying...",
            Self::CleaningUp => "Cleaning up...",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Aborted => "Aborted",
            Self::Custom(label) => label.as_str(),
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Aborted)
    }
}

/// One item currently being processed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressItem {
    pub label: String,
    pub source: Option<PathBuf>,
    pub destination: Option<PathBuf>,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
}

impl ProgressItem {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            source: None,
            destination: None,
            bytes_done: 0,
            bytes_total: None,
        }
    }

    pub fn ratio(&self) -> Option<f64> {
        ratio(self.bytes_done, self.bytes_total)
    }
}

/// Stable job-level paths and labels shown at the top of the overlay.
///
/// This is intentionally generic: a host may use it for copy/move endpoints,
/// scan/import roots, archive sources, or any other long-running file-oriented
/// task. The picker crate only displays these fields; it does not interpret or
/// mutate the referenced paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTaskScope {
    /// Stable source/root path for the job, when a path is meaningful.
    pub source_root: Option<PathBuf>,
    /// Human-oriented source summary, for example "7 selected files".
    pub source_summary: String,
    /// Stable destination/output path for the job, when a path is meaningful.
    pub destination: Option<PathBuf>,
    /// Optional human-oriented destination summary. When omitted, the renderer
    /// falls back to `destination`.
    pub destination_summary: Option<String>,
}

impl FileTaskScope {
    pub fn new(source_summary: impl Into<String>) -> Self {
        Self {
            source_root: None,
            source_summary: source_summary.into(),
            destination: None,
            destination_summary: None,
        }
    }
}

/// One retained error or warning record for a file-task progress overlay.
///
/// The overlay stores a bounded list of recent records so terminal partial or
/// failed states can show which files failed without coupling the crate to a
/// host logging system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTaskErrorRecord {
    pub item_label: String,
    pub source: Option<PathBuf>,
    pub destination: Option<PathBuf>,
    pub message: String,
}

impl FileTaskErrorRecord {
    pub fn new(item_label: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            item_label: item_label.into(),
            source: None,
            destination: None,
            message: message.into(),
        }
    }
}

/// Unit label for the aggregate item counter in [`ProgressTotals`].
///
/// The progress overlay is generic across file-oriented tasks. Hosts should use
/// `Files` when the total represents recursive files inside selected folders,
/// `Entries` when directories/symlinks/files are all counted together, and
/// `Items` for task-specific units that are not naturally files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressUnit {
    Items,
    Files,
    Entries,
}

impl Default for ProgressUnit {
    fn default() -> Self {
        Self::Items
    }
}

impl ProgressUnit {
    fn label(self, count: u64) -> &'static str {
        match (self, count) {
            (Self::Files, 1) => "file",
            (Self::Files, _) => "files",
            (Self::Entries, 1) => "entry",
            (Self::Entries, _) => "entries",
            (Self::Items, 1) => "item",
            (Self::Items, _) => "items",
        }
    }
}

/// Aggregate counters for the whole job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProgressTotals {
    pub items_done: u64,
    pub items_total: Option<u64>,
    pub item_unit: ProgressUnit,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub folders_done: u64,
    pub folders_total: Option<u64>,
    pub unknown_size_items: u64,
    pub completed: u64,
    pub skipped: u64,
    pub errors: u64,
    /// Files deliberately overwritten after explicit host/user policy.
    pub overwritten: u64,
    /// Files written to a deterministic non-conflicting destination.
    pub renamed: u64,
    /// Directories deliberately merged into existing destination directories.
    pub merged: u64,
    /// Planned files not attempted because the job aborted before reaching them.
    pub not_attempted: u64,
}

impl ProgressTotals {
    /// Ratio of transferred bytes to total bytes, when the host knows a byte total.
    ///
    /// This is intentionally separate from [`ProgressTotals::work_ratio`]: a
    /// skipped, failed, or otherwise accounted-for item may advance overall work
    /// without transferring bytes.
    pub fn byte_ratio(&self) -> Option<f64> {
        ratio(self.bytes_done, self.bytes_total)
    }

    /// Ratio of accounted items to total items, when the host knows an item total.
    ///
    /// `items_done` means dispositioned work, not necessarily successful output:
    /// completed, skipped, and failed entries can all count as accounted work.
    pub fn work_ratio(&self) -> Option<f64> {
        ratio(self.items_done, self.items_total)
    }

    /// Overall job-progress ratio used by the aggregate progress bar.
    ///
    /// The whole-job bar represents accounted operation progress. Byte progress
    /// remains useful while a large file is transferring, but it must not pin a
    /// terminal skipped/failed job at 0%. Use the more advanced of byte progress
    /// and item-disposition progress.
    pub fn ratio(&self) -> Option<f64> {
        match (self.byte_ratio(), self.work_ratio()) {
            (Some(bytes), Some(work)) => Some(bytes.max(work)),
            (Some(bytes), None) => Some(bytes),
            (None, Some(work)) => Some(work),
            (None, None) => None,
        }
    }
}

/// Disposition of one top-level root in a file task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileTaskRootDisposition {
    Completed,
    /// The destination is complete and authoritative, but a post-commit step
    /// such as source cleanup or durability synchronization raised a warning.
    CompletedWithWarning,
    Skipped,
    Failed,
    NotAttempted,
}

impl FileTaskRootDisposition {
    pub fn is_completed(self) -> bool {
        matches!(self, Self::Completed | Self::CompletedWithWarning)
    }
}

/// Terminal accounting for one top-level source and its resolved destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTaskRootResult {
    pub source: std::path::PathBuf,
    pub destination: std::path::PathBuf,
    pub disposition: FileTaskRootDisposition,
    pub message: Option<String>,
}

/// Structured terminal report emitted separately from the presentation-oriented
/// progress update. Hosts use this to reconcile cut clipboards, navigation
/// history, and retry state after partial success, failure, skip, or abort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTaskCompletionReport {
    pub is_move: bool,
    pub roots: Vec<FileTaskRootResult>,
}

impl FileTaskCompletionReport {
    pub fn completed_mappings(&self) -> Vec<(std::path::PathBuf, std::path::PathBuf)> {
        self.roots
            .iter()
            .filter(|root| root.disposition.is_completed())
            .map(|root| (root.source.clone(), root.destination.clone()))
            .collect()
    }

    pub fn retry_sources(&self) -> Vec<std::path::PathBuf> {
        self.roots
            .iter()
            .filter(|root| !root.disposition.is_completed())
            .map(|root| root.source.clone())
            .collect()
    }

    pub fn is_complete(&self) -> bool {
        self.roots
            .iter()
            .all(|root| root.disposition.is_completed())
    }
}

/// Snapshot or event supplied by the host to update the reusable overlay.
///
/// Applying an update is a pure state transition: no filesystem mutation,
/// cancellation, conflict resolution, or thread control happens in the crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileTaskProgressUpdate {
    /// Replace the stable job-level source/destination summary.
    SetScope {
        scope: FileTaskScope,
    },
    /// Replace the live progress snapshot for the current task.
    Snapshot {
        phase: FileTaskPhase,
        status: String,
        current_item: Option<ProgressItem>,
        totals: ProgressTotals,
        rate_bytes_per_sec: Option<u64>,
    },
    /// Append one retained error record and update aggregate counters.
    RecordError {
        error: FileTaskErrorRecord,
        totals: ProgressTotals,
    },
    /// Show a reusable conflict prompt. The host owns the underlying policy and
    /// filesystem side effects; the crate only renders the prompt and emits
    /// semantic conflict choices.
    ShowConflict {
        conflict: ConflictPromptState,
    },
    /// Refine the displayed comparison data for the currently active conflict.
    /// Hosts use this for non-blocking best-effort metadata such as directory
    /// content size. Stale request ids are ignored by `FileTaskProgressState`.
    UpdateConflictExistingStats {
        request_id: u64,
        size: u64,
        modified: Option<SystemTime>,
    },
    /// Clear the active conflict prompt after the host has applied or abandoned
    /// a resolution.
    ClearConflict,
    /// Mark the task as successfully completed.
    Finished {
        status: String,
        totals: ProgressTotals,
    },
    /// Mark the task as failed or partially failed.
    Failed {
        status: String,
        totals: ProgressTotals,
    },
    /// Mark the task as aborted by host policy or user request.
    Aborted {
        status: String,
        totals: ProgressTotals,
    },
}

/// Semantic user intent emitted by the overlay.
///
/// The crate does not pause, resume, skip, abort, acknowledge, or resolve a
/// conflict directly. Hosts must translate these actions into their own job
/// control and policy layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileTaskUserAction {
    None,
    Pause,
    Resume,
    SkipCurrent,
    Abort,
    Acknowledge,
    ChooseConflictResolution(ConflictResolution),
}

/// Reusable conflict action displayed by the progress overlay.
///
/// These are semantic choices only. The host must decide whether a choice is
/// valid for a specific file operation, perform race-conscious filesystem
/// mutation, and account the resulting disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictAction {
    /// Replace the existing destination. Hosts interpret this contextually:
    /// directory-to-directory conflicts merge into the destination directory,
    /// while file/symlink/item conflicts replace the existing path.
    Overwrite,
    /// Skip this item and continue the job.
    Skip,
    /// Ask the host to choose a deterministic non-conflicting destination.
    Rename,
    /// Abort the whole job.
    Abort,
}

impl ConflictAction {
    fn label(self) -> &'static str {
        match self {
            Self::Overwrite => "Overwrite",
            Self::Skip => "Skip",
            Self::Rename => "Rename",
            Self::Abort => "Abort",
        }
    }

    fn supports_apply_to_all(self) -> bool {
        matches!(self, Self::Overwrite | Self::Skip)
    }
}

/// Generic conflict-resolution choice emitted to the host.
///
/// The crate does not overwrite, merge, rename, skip, or abort anything. Hosts
/// must validate the request id, re-check the destination as needed, and then
/// apply the chosen policy safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictResolution {
    /// Replace or merge, depending on the current conflict context. Directory
    /// into existing directory means merge; other conflicts mean replace.
    Overwrite { apply_to_all: bool },
    Skip { apply_to_all: bool },
    Rename,
    Abort,
}

/// The kind of destination conflict being presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictItemKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// Optional generic conflict prompt data.
///
/// Hosts own conflict detection and resolution. This state is deliberately
/// reusable across copy, move, import, extract, archive, checksum sidecar writes,
/// or any other file-oriented task that may pause for a user decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictPromptState {
    /// Host-scoped request identity. Responses echo this id so stale conflict
    /// resolutions can be ignored safely.
    pub request_id: u64,
    pub title: String,
    pub message: String,
    pub source: Option<PathBuf>,
    pub destination: Option<PathBuf>,
    pub item_kind: ConflictItemKind,
    pub choices: Vec<ConflictAction>,
    pub selected: usize,
    pub apply_to_all: bool,
    /// 1-based index for this conflict within the current job.
    pub conflict_index: u64,
    /// Total known conflicts for the current job, if the host computed one from
    /// its plan. `None` avoids presenting a misleading "N of M" count.
    pub conflict_count: Option<u64>,
    pub existing_size: Option<u64>,
    pub existing_modified: Option<SystemTime>,
    pub incoming_size: Option<u64>,
    pub incoming_modified: Option<SystemTime>,
}

impl ConflictPromptState {
    pub fn new(
        request_id: u64,
        title: impl Into<String>,
        message: impl Into<String>,
        item_kind: ConflictItemKind,
    ) -> Self {
        Self {
            request_id,
            title: title.into(),
            message: message.into(),
            source: None,
            destination: None,
            item_kind,
            choices: vec![
                ConflictAction::Overwrite,
                ConflictAction::Skip,
                ConflictAction::Rename,
                ConflictAction::Abort,
            ],
            selected: 0,
            apply_to_all: false,
            conflict_index: request_id,
            conflict_count: None,
            existing_size: None,
            existing_modified: None,
            incoming_size: None,
            incoming_modified: None,
        }
    }

    pub fn selected_action(&self) -> Option<ConflictAction> {
        self.choices.get(self.selected).copied()
    }

    pub fn selected_resolution(&self) -> Option<ConflictResolution> {
        let apply_to_all = self
            .selected_action()
            .map(|action| action.supports_apply_to_all() && self.apply_to_all)
            .unwrap_or(false);
        match self.selected_action()? {
            ConflictAction::Overwrite => Some(ConflictResolution::Overwrite { apply_to_all }),
            ConflictAction::Skip => Some(ConflictResolution::Skip { apply_to_all }),
            ConflictAction::Rename => Some(ConflictResolution::Rename),
            ConflictAction::Abort => Some(ConflictResolution::Abort),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressHitAction {
    PauseResume,
    SkipCurrent,
    Abort,
    Acknowledge,
    Conflict(ConflictAction),
    ToggleApplyAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProgressHitRegion {
    rect: Rect,
    action: ProgressHitAction,
}

/// Reusable state and renderer for a file-task progress overlay.
///
/// Hosts feed progress with [`FileTaskProgressState::apply_update`] and route
/// keyboard/mouse results from [`FileTaskProgressState::handle_key`] or
/// [`FileTaskProgressState::handle_mouse`] to their own task controller.
#[derive(Debug, Clone)]
pub struct FileTaskProgressState {
    pub title: String,
    pub kind: FileTaskKind,
    pub phase: FileTaskPhase,
    pub status: String,
    pub scope: Option<FileTaskScope>,
    pub current_item: Option<ProgressItem>,
    pub totals: ProgressTotals,
    pub rate_bytes_per_sec: Option<u64>,
    pub theme: FilePickerTheme,
    pub conflict: Option<ConflictPromptState>,
    pub conflict_records: Vec<ConflictPromptState>,
    pub error_records: Vec<FileTaskErrorRecord>,
    pub max_error_records: usize,
    pub max_conflict_records: usize,
    started_at: Instant,
    updated_at: Instant,
    hit_regions: Vec<ProgressHitRegion>,
    last_area: Option<Rect>,
}

impl FileTaskProgressState {
    pub fn new(kind: FileTaskKind, title: impl Into<String>, theme: FilePickerTheme) -> Self {
        let now = Instant::now();
        Self {
            title: title.into(),
            kind,
            phase: FileTaskPhase::Preparing,
            status: "Preparing...".to_string(),
            scope: None,
            current_item: None,
            totals: ProgressTotals::default(),
            rate_bytes_per_sec: None,
            theme,
            conflict: None,
            conflict_records: Vec::new(),
            error_records: Vec::new(),
            max_error_records: DEFAULT_MAX_ERROR_RECORDS,
            max_conflict_records: DEFAULT_MAX_CONFLICT_RECORDS,
            started_at: now,
            updated_at: now,
            hit_regions: Vec::new(),
            last_area: None,
        }
    }

    pub fn set_scope(&mut self, scope: FileTaskScope) {
        self.scope = Some(scope);
        self.updated_at = Instant::now();
    }

    /// Return the concrete theme currently used by this progress overlay.
    pub fn theme(&self) -> &FilePickerTheme {
        &self.theme
    }

    /// Replace the concrete theme used by this already-open progress overlay.
    ///
    /// This keeps long-running copy/move/delete progress dialogs visually in
    /// sync when the host application changes themes while the task is active.
    pub fn set_theme(&mut self, theme: FilePickerTheme) {
        self.theme = theme;
        self.updated_at = Instant::now();
    }

    pub fn record_error(&mut self, error: FileTaskErrorRecord) {
        self.error_records.push(error);
        let max = self.max_error_records.max(1);
        if self.error_records.len() > max {
            let overflow = self.error_records.len() - max;
            self.error_records.drain(0..overflow).for_each(drop);
        }
        self.updated_at = Instant::now();
    }

    pub fn show_conflict(&mut self, conflict: ConflictPromptState) {
        self.conflict = Some(conflict.clone());
        self.conflict_records.push(conflict);
        let max = self.max_conflict_records.max(1);
        if self.conflict_records.len() > max {
            let overflow = self.conflict_records.len() - max;
            self.conflict_records.drain(0..overflow).for_each(drop);
        }
        self.updated_at = Instant::now();
    }

    pub fn clear_conflict(&mut self) {
        self.conflict = None;
        self.updated_at = Instant::now();
    }

    pub fn apply_update(&mut self, update: FileTaskProgressUpdate) {
        self.updated_at = Instant::now();
        match update {
            FileTaskProgressUpdate::SetScope { scope } => {
                self.scope = Some(scope);
            }
            FileTaskProgressUpdate::Snapshot { phase, status, current_item, totals, rate_bytes_per_sec } => {
                self.phase = phase;
                self.status = status;
                self.current_item = current_item;
                self.totals = totals;
                self.rate_bytes_per_sec = rate_bytes_per_sec;
            }
            FileTaskProgressUpdate::RecordError { error, totals } => {
                self.totals = totals;
                self.record_error(error);
            }
            FileTaskProgressUpdate::ShowConflict { conflict } => {
                self.show_conflict(conflict);
            }
            FileTaskProgressUpdate::UpdateConflictExistingStats { request_id, size, modified } => {
                let active_matches = self
                    .conflict
                    .as_ref()
                    .map(|conflict| conflict.request_id == request_id)
                    .unwrap_or(false);
                if active_matches {
                    if let Some(conflict) = self.conflict.as_mut() {
                        conflict.existing_size = Some(size);
                        if let Some(modified) = modified.clone() {
                            conflict.existing_modified = Some(modified);
                        }
                    }
                    if let Some(record) = self
                        .conflict_records
                        .iter_mut()
                        .rev()
                        .find(|conflict| conflict.request_id == request_id)
                    {
                        record.existing_size = Some(size);
                        if let Some(modified) = modified.clone() {
                            record.existing_modified = Some(modified);
                        }
                    }
                }
            }
            FileTaskProgressUpdate::ClearConflict => {
                self.clear_conflict();
            }
            FileTaskProgressUpdate::Finished { status, totals } => {
                self.phase = FileTaskPhase::Completed;
                self.status = status;
                self.current_item = None;
                self.conflict = None;
                self.totals = totals;
                self.rate_bytes_per_sec = None;
            }
            FileTaskProgressUpdate::Failed { status, totals } => {
                self.phase = FileTaskPhase::Failed;
                self.status = status;
                self.current_item = None;
                self.conflict = None;
                self.totals = totals;
                self.rate_bytes_per_sec = None;
            }
            FileTaskProgressUpdate::Aborted { status, totals } => {
                self.phase = FileTaskPhase::Aborted;
                self.status = status;
                self.current_item = None;
                self.conflict = None;
                self.totals = totals;
                self.rate_bytes_per_sec = None;
            }
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.phase.is_terminal()
    }

    /// Elapsed time for display.
    ///
    /// Active tasks use the current monotonic clock so elapsed time keeps
    /// advancing during quiet phases such as slow directory scans or blocked
    /// filesystem calls. Terminal tasks freeze at the final update time so a
    /// completed/failed/aborted overlay remains stable while awaiting
    /// acknowledgement.
    pub fn elapsed(&self) -> Duration {
        let end = if self.is_terminal() {
            self.updated_at
        } else {
            Instant::now()
        };
        end.saturating_duration_since(self.started_at)
    }

    pub fn eta(&self) -> Option<Duration> {
        let rate = self.rate_bytes_per_sec?;
        let total = self.totals.bytes_total?;
        if rate == 0 || self.totals.bytes_done >= total {
            return None;
        }
        Some(Duration::from_secs((total - self.totals.bytes_done) / rate))
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FileTaskUserAction {
        if self.conflict.is_some() {
            return self.handle_conflict_key(key);
        }

        if self.is_terminal() {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => FileTaskUserAction::Acknowledge,
                _ => FileTaskUserAction::None,
            };
        }

        match key.code {
            KeyCode::Esc => FileTaskUserAction::Abort,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => FileTaskUserAction::Abort,
            KeyCode::Char('q') => FileTaskUserAction::Abort,
            KeyCode::Char('p') | KeyCode::Char(' ') => {
                if matches!(self.phase, FileTaskPhase::Paused) {
                    FileTaskUserAction::Resume
                } else {
                    FileTaskUserAction::Pause
                }
            }
            KeyCode::Char('s') => FileTaskUserAction::SkipCurrent,
            _ => FileTaskUserAction::None,
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> FileTaskUserAction {
        if area != Rect::default() && self.last_area != Some(area) {
            self.hit_regions.clear();
            return FileTaskUserAction::None;
        }
        let MouseEventKind::Down(MouseButton::Left) = mouse.kind else {
            return FileTaskUserAction::None;
        };
        let Some(hit) = self.hit_regions.iter().rev().find(|hit| point_in_rect(mouse.column, mouse.row, hit.rect)) else {
            return FileTaskUserAction::None;
        };
        match hit.action {
            ProgressHitAction::PauseResume => {
                if matches!(self.phase, FileTaskPhase::Paused) {
                    FileTaskUserAction::Resume
                } else {
                    FileTaskUserAction::Pause
                }
            }
            ProgressHitAction::SkipCurrent => FileTaskUserAction::SkipCurrent,
            ProgressHitAction::Abort => FileTaskUserAction::Abort,
            ProgressHitAction::Acknowledge => FileTaskUserAction::Acknowledge,
            ProgressHitAction::Conflict(action) => self.choose_conflict_action(action),
            ProgressHitAction::ToggleApplyAll => {
                if let Some(conflict) = self.conflict.as_mut() {
                    conflict.apply_to_all = !conflict.apply_to_all;
                }
                FileTaskUserAction::None
            }
        }
    }

    fn handle_conflict_key(&mut self, key: KeyEvent) -> FileTaskUserAction {
        let Some(conflict) = self.conflict.as_mut() else {
            return FileTaskUserAction::None;
        };
        match key.code {
            KeyCode::Left | KeyCode::BackTab => {
                if conflict.choices.is_empty() {
                    return FileTaskUserAction::None;
                }
                conflict.selected = if conflict.selected == 0 {
                    conflict.choices.len() - 1
                } else {
                    conflict.selected - 1
                };
                FileTaskUserAction::None
            }
            KeyCode::Right | KeyCode::Tab => {
                if !conflict.choices.is_empty() {
                    conflict.selected = (conflict.selected + 1) % conflict.choices.len();
                }
                FileTaskUserAction::None
            }
            KeyCode::Char(' ') => {
                conflict.apply_to_all = !conflict.apply_to_all;
                FileTaskUserAction::None
            }
            KeyCode::Enter => conflict
                .selected_resolution()
                .map(FileTaskUserAction::ChooseConflictResolution)
                .unwrap_or(FileTaskUserAction::None),
            KeyCode::Char('o') => self.choose_conflict_action(ConflictAction::Overwrite),
            KeyCode::Char('s') => self.choose_conflict_action(ConflictAction::Skip),
            KeyCode::Char('r') => self.choose_conflict_action(ConflictAction::Rename),
            KeyCode::Char('a') => self.choose_conflict_action(ConflictAction::Abort),
            KeyCode::Esc => self.choose_conflict_action(ConflictAction::Abort),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.choose_conflict_action(ConflictAction::Abort)
            }
            _ => FileTaskUserAction::None,
        }
    }

    fn choose_conflict_action(&mut self, action: ConflictAction) -> FileTaskUserAction {
        let Some(conflict) = self.conflict.as_mut() else {
            return FileTaskUserAction::None;
        };
        let Some(index) = conflict.choices.iter().position(|choice| *choice == action) else {
            return FileTaskUserAction::None;
        };
        conflict.selected = index;
        conflict
            .selected_resolution()
            .map(FileTaskUserAction::ChooseConflictResolution)
            .unwrap_or(FileTaskUserAction::None)
    }

    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.hit_regions.clear();
        self.last_area = Some(area);
        let dialog_height = if self.conflict.is_some() {
            CONFLICT_DIALOG_HEIGHT
        } else {
            PROGRESS_DIALOG_HEIGHT
        };
        if area.width < MIN_PROGRESS_DIALOG_WIDTH || area.height < dialog_height {
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(format!(
                    "Progress overlay needs at least {}x{} cells",
                    MIN_PROGRESS_DIALOG_WIDTH,
                    dialog_height
                ))
                .style(self.theme.error),
                area,
            );
            return;
        }

        // Use the full available width (clamped to minimum), center vertically.
        let dialog_width = area.width.max(MIN_PROGRESS_DIALOG_WIDTH);
        let dialog_area = centered_fixed_rect(area, dialog_width, dialog_height);
        frame.render_widget(Clear, dialog_area);
        if self.conflict.is_some() {
            self.render_conflict(frame, dialog_area);
        } else {
            self.render_progress_dialog(frame, dialog_area);
        }
    }

    fn render_progress_dialog(&mut self, frame: &mut Frame<'_>, area: Rect) {
        debug_assert!(area.width >= MIN_PROGRESS_DIALOG_WIDTH);
        debug_assert_eq!(area.height, PROGRESS_DIALOG_HEIGHT);

        frame.render_widget(Paragraph::new("").style(self.theme.progress_dialog), area);
        self.render_title_bar(frame, area, 0);
        self.render_source_row(frame, area, 1);
        self.render_destination_row(frame, area, 2);
        self.render_empty_row(frame, area, 3);
        self.render_current_item_row(frame, area, 4);
        self.render_current_progress_row(frame, area, 5);
        self.render_total_summary_row(frame, area, 6);
        self.render_total_progress_row(frame, area, 7);
        self.render_transfer_stats_row(frame, area, 8);
        self.render_rule(frame, area, 9, '\u{251c}', '\u{2524}');
        self.render_action_row(frame, area, 10, self.progress_buttons());
        self.render_rule(frame, area, 11, '\u{2514}', '\u{2518}');
    }

    fn render_conflict(&mut self, frame: &mut Frame<'_>, area: Rect) {
        debug_assert!(area.width >= MIN_PROGRESS_DIALOG_WIDTH);
        debug_assert_eq!(area.height, CONFLICT_DIALOG_HEIGHT);

        frame.render_widget(Paragraph::new("").style(self.theme.progress_dialog), area);
        if let Some(conflict) = self.conflict.as_ref() {
            let title = conflict_dialog_title(conflict);
            self.render_title_bar_with_title(frame, area, 0, &title);
            self.render_empty_row(frame, area, 1);
            self.render_bordered_text(
                frame,
                area,
                2,
                item_kind_conflict_statement(conflict.item_kind),
                self.theme.progress_text,
            );
            self.render_empty_row(frame, area, 3);

            let inner_width = area.width.saturating_sub(2) as usize;
            let item_name = conflict_item_name(conflict);
            let item_text = format!("    {}", fit_text(&item_name, inner_width.saturating_sub(4)).0);
            self.render_bordered_text(frame, area, 4, &item_text, self.theme.progress_current_file);

            let parent = conflict_parent_display(conflict);
            let parent_text = format!("    {}", fit_text(&parent, inner_width.saturating_sub(4)).0);
            self.render_bordered_text(frame, area, 5, &parent_text, self.theme.progress_text_dim);

            self.render_empty_row(frame, area, 6);
            self.render_bordered_text(
                frame,
                area,
                7,
                &conflict_comparison_text(
                    "existing",
                    conflict.existing_size,
                    conflict.existing_modified,
                ),
                self.theme.progress_text_dim,
            );
            self.render_bordered_text(
                frame,
                area,
                8,
                &conflict_comparison_text(
                    "incoming",
                    conflict.incoming_size,
                    conflict.incoming_modified,
                ),
                self.theme.progress_text,
            );
            self.render_empty_row(frame, area, 9);

            let marker = if conflict.apply_to_all { "\u{00d7}" } else { " " };
            let checkbox = format!("    [{}] Apply to all remaining conflicts", marker);
            self.render_bordered_text(frame, area, 10, &checkbox, self.theme.progress_text_dim);
            let width = crate::display_width::width(&checkbox).min(inner_width) as u16;
            let rect = Rect::new(area.x.saturating_add(1), area.y.saturating_add(10), width, 1);
            if let Some(clipped) = intersect_rect(rect, area) {
                self.hit_regions.push(ProgressHitRegion {
                    rect: clipped,
                    action: ProgressHitAction::ToggleApplyAll,
                });
            }

            self.render_empty_row(frame, area, 11);
        }
        self.render_rule(frame, area, 12, '\u{251c}', '\u{2524}');
        self.render_action_row(frame, area, 13, self.conflict_buttons());
        self.render_rule(frame, area, 14, '\u{2514}', '\u{2518}');
    }

    fn render_title_bar(&self, frame: &mut Frame<'_>, area: Rect, row: u16) {
        self.render_title_bar_with_title(frame, area, row, &self.title);
    }

    fn render_title_bar_with_title(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        row: u16,
        title: &str,
    ) {
        let width = area.width as usize;
        let available_title = width.saturating_sub(6);
        let title = fit_text(title, available_title).0;
        let title_width = crate::display_width::width(&title);
        let filler = width.saturating_sub(3 + title_width + 1 + 1);
        let line = Line::from(vec![
            Span::styled("\u{250c}\u{2500} ", self.theme.progress_border),
            Span::styled(title, self.theme.progress_title),
            Span::styled(format!(" {}\u{2510}", "\u{2500}".repeat(filler)), self.theme.progress_border),
        ]);
        self.render_full_line(frame, area, row, line);
    }

    fn render_rule(&self, frame: &mut Frame<'_>, area: Rect, row: u16, left: char, right: char) {
        let width = area.width as usize;
        let inner = width.saturating_sub(2);
        let line = Line::from(Span::styled(
            format!("{}{}{}", left, "─".repeat(inner), right),
            self.theme.progress_border,
        ));
        self.render_full_line(frame, area, row, line);
    }

    fn render_empty_row(&self, frame: &mut Frame<'_>, area: Rect, row: u16) {
        self.render_bordered_spans(frame, area, row, Vec::new());
    }

    fn render_source_row(&self, frame: &mut Frame<'_>, area: Rect, row: u16) {
        let value = self
            .scope
            .as_ref()
            .map(|scope| {
                scope
                    .source_root
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| scope.source_summary.clone())
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "—".to_string());
        self.render_label_value_row(frame, area, row, "  From:  ", &value);
    }

    fn render_destination_row(&self, frame: &mut Frame<'_>, area: Rect, row: u16) {
        let value = self
            .scope
            .as_ref()
            .and_then(|scope| {
                scope
                    .destination_summary
                    .clone()
                    .or_else(|| scope.destination.as_ref().map(|path| path.display().to_string()))
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "—".to_string());
        self.render_label_value_row(frame, area, row, "  To:    ", &value);
    }

    fn render_label_value_row(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        row: u16,
        label: &'static str,
        value: &str,
    ) {
        let inner_width = area.width.saturating_sub(2) as usize;
        let label_width = crate::display_width::width(label);
        let value = fit_text(value, inner_width.saturating_sub(label_width)).0;
        self.render_bordered_spans(
            frame,
            area,
            row,
            vec![Span::styled(label, self.theme.progress_label), Span::styled(value, self.theme.progress_text)],
        );
    }

    fn render_current_item_row(&self, frame: &mut Frame<'_>, area: Rect, row: u16) {
        let (label, style) = if let Some(item) = self.current_item.as_ref() {
            (item.label.clone(), self.theme.progress_current_file)
        } else if self.is_terminal() && !self.error_records.is_empty() {
            let error = self.error_records.last().expect("checked non-empty");
            let label = if error.item_label.is_empty() {
                error
                    .source
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "unknown item".to_string())
            } else {
                error.item_label.clone()
            };
            (format!("{}: {} — {}", self.phase.label(), label, error.message), self.theme.error)
        } else if self.is_terminal() && !self.status.is_empty() {
            (self.status.clone(), phase_style(&self.theme, &self.phase))
        } else {
            ("—".to_string(), self.theme.progress_current_file)
        };
        let inner_width = area.width.saturating_sub(2) as usize;
        let prefix = "  ▸ ";
        let label = fit_text(&label, inner_width.saturating_sub(crate::display_width::width(prefix))).0;
        self.render_bordered_spans(
            frame,
            area,
            row,
            vec![Span::styled(prefix, self.theme.progress_text_dim), Span::styled(label, style)],
        );
    }

    fn render_current_progress_row(&self, frame: &mut Frame<'_>, area: Rect, row: u16) {
        let ratio = self
            .current_item
            .as_ref()
            .and_then(ProgressItem::ratio)
            .unwrap_or(if self.is_terminal() { 1.0 } else { 0.0 });
        self.render_bordered_spans(frame, area, row, self.progress_bar_spans(area, ratio));
    }

    fn render_total_summary_row(&self, frame: &mut Frame<'_>, area: Rect, row: u16) {
        let summary = format!("  Total   {}  ·  {}", self.total_items_text(), self.total_bytes_text());
        self.render_bordered_text(frame, area, row, &summary, self.theme.progress_text);
    }

    fn render_total_progress_row(&self, frame: &mut Frame<'_>, area: Rect, row: u16) {
        let ratio = self.totals.ratio().unwrap_or(if self.is_terminal() { 1.0 } else { 0.0 });
        self.render_bordered_spans(frame, area, row, self.progress_bar_spans(area, ratio));
    }

    fn render_transfer_stats_row(&self, frame: &mut Frame<'_>, area: Rect, row: u16) {
        let rate = self.rate_bytes_per_sec.map(format_rate).unwrap_or_else(|| "--/s".to_string());
        let elapsed = format_duration(self.elapsed());
        let eta = self.eta().map(format_duration).unwrap_or_else(|| "--".to_string());
        let text = format!("  {}     {} elapsed     {} left", rate, elapsed, eta);
        self.render_bordered_text(frame, area, row, &text, self.theme.progress_text_dim);
    }

    fn render_action_row(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        row: u16,
        buttons: Vec<(String, ProgressHitAction, Style)>,
    ) {
        let inner_width = area.width.saturating_sub(2) as usize;
        let gap = "   ";
        let button_width: usize = buttons.iter().map(|(label, _, _)| crate::display_width::width(label)).sum();
        let gap_width = crate::display_width::width(gap) * buttons.len().saturating_sub(1);
        let group_width = button_width + gap_width;
        let left_padding = inner_width.saturating_sub(group_width) / 2;
        let mut spans = Vec::new();
        let mut cursor_x = area.x.saturating_add(1).saturating_add(left_padding as u16);
        spans.push(Span::styled(" ".repeat(left_padding), self.theme.progress_text));

        for (idx, (label, action, style)) in buttons.iter().enumerate() {
            if idx > 0 {
                spans.push(Span::styled(gap, self.theme.progress_text));
                cursor_x = cursor_x.saturating_add(crate::display_width::width(gap) as u16);
            }
            let width = crate::display_width::width(label) as u16;
            spans.push(Span::styled(label.clone(), *style));
            let rect = Rect::new(cursor_x, area.y.saturating_add(row), width, 1);
            if let Some(clipped) = intersect_rect(rect, area) {
                self.hit_regions.push(ProgressHitRegion { rect: clipped, action: *action });
            }
            cursor_x = cursor_x.saturating_add(width);
        }
        self.render_bordered_spans(frame, area, row, spans);
    }

    fn progress_buttons(&self) -> Vec<(String, ProgressHitAction, Style)> {
        if self.is_terminal() {
            return vec![(" OK ".to_string(), ProgressHitAction::Acknowledge, self.theme.progress_button)];
        }
        if matches!(&self.kind, FileTaskKind::Archive) {
            return vec![(
                " Esc Abort ".to_string(),
                ProgressHitAction::Abort,
                self.theme.progress_destructive,
            )];
        }
        let pause_label = if matches!(self.phase, FileTaskPhase::Paused) {
            " p Resume "
        } else {
            " p Pause "
        };
        vec![
            (pause_label.to_string(), ProgressHitAction::PauseResume, self.theme.progress_button),
            (" s Skip ".to_string(), ProgressHitAction::SkipCurrent, self.theme.progress_button),
            (" Esc Abort ".to_string(), ProgressHitAction::Abort, self.theme.progress_destructive),
        ]
    }

    fn conflict_buttons(&self) -> Vec<(String, ProgressHitAction, Style)> {
        let Some(conflict) = self.conflict.as_ref() else {
            return self.progress_buttons();
        };
        conflict
            .choices
            .iter()
            .enumerate()
            .map(|(idx, action)| {
                let selected = idx == conflict.selected;
                let style = if *action == ConflictAction::Abort {
                    self.theme.progress_destructive
                } else if selected {
                    self.theme.progress_button_focused
                } else {
                    self.theme.progress_button
                };
                let label = if *action == ConflictAction::Abort {
                    " Esc Abort ".to_string()
                } else {
                    format!(" {} ", action.label())
                };
                (label, ProgressHitAction::Conflict(*action), style)
            })
            .collect()
    }

    fn progress_bar_spans(&self, area: Rect, ratio: f64) -> Vec<Span<'static>> {
        let inner_width = area.width.saturating_sub(2) as usize;
        let ratio = normalized_ratio(ratio);
        let percent = (ratio * 100.0).round() as u32;
        let percent_text = format!("{}%", percent.min(100));
        let percent_width = crate::display_width::width(&percent_text).max(3);
        let trailing_width = usize::from(percent_width <= 3);
        let fixed_width = 3 + 3 + percent_width + trailing_width;
        let bar_width = inner_width.saturating_sub(fixed_width);
        let filled = ((bar_width as f64) * ratio).round() as usize;
        let filled = filled.min(bar_width);
        let unfilled = bar_width.saturating_sub(filled);
        let mut spans = vec![
            Span::styled("  [", self.theme.progress_text_dim),
            Span::styled("█".repeat(filled), self.theme.progress_filled),
            Span::styled("░".repeat(unfilled), self.theme.progress_unfilled),
            Span::styled("]  ", self.theme.progress_text_dim),
        ];
        if percent_width > crate::display_width::width(&percent_text) {
            spans.push(Span::styled(
                " ".repeat(percent_width - crate::display_width::width(&percent_text)),
                self.theme.progress_percent,
            ));
        }
        spans.push(Span::styled(percent_text, self.theme.progress_percent));
        if trailing_width > 0 {
            spans.push(Span::styled(" ", self.theme.progress_text));
        }
        spans
    }

    fn total_items_text(&self) -> String {
        match self.totals.items_total {
            Some(total) => format!(
                "{} of {} {}",
                self.totals.items_done,
                total,
                self.totals.item_unit.label(total)
            ),
            None => format!(
                "{} {}",
                self.totals.items_done,
                self.totals.item_unit.label(self.totals.items_done)
            ),
        }
    }

    fn total_bytes_text(&self) -> String {
        match self.totals.bytes_total {
            Some(total) => format!("{} / {}", format_bytes(self.totals.bytes_done), format_bytes(total)),
            None => format_bytes(self.totals.bytes_done),
        }
    }

    fn render_bordered_text(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        row: u16,
        text: &str,
        style: Style,
    ) {
        self.render_bordered_spans(frame, area, row, vec![Span::styled(text.to_string(), style)]);
    }

    fn render_bordered_spans(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        row: u16,
        spans: Vec<Span<'static>>,
    ) {
        let inner_width = area.width.saturating_sub(2) as usize;
        let mut line_spans = Vec::new();
        line_spans.push(Span::styled("│", self.theme.progress_border));
        line_spans.extend(fit_spans(spans, inner_width, self.theme.progress_text));
        line_spans.push(Span::styled("│", self.theme.progress_border));
        self.render_full_line(frame, area, row, Line::from(line_spans));
    }

    fn render_full_line(&self, frame: &mut Frame<'_>, area: Rect, row: u16, line: Line<'static>) {
        if row >= area.height {
            return;
        }
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(area.x, area.y.saturating_add(row), area.width, 1),
        );
    }

}

fn centered_fixed_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let y = area.y.saturating_add(area.height.saturating_sub(height) / 2);
    Rect::new(x, y, width, height)
}

fn ratio(done: u64, total: Option<u64>) -> Option<f64> {
    let total = total?;
    if total == 0 {
        Some(1.0)
    } else {
        Some((done as f64 / total as f64).clamp(0.0, 1.0))
    }
}

fn phase_style(theme: &FilePickerTheme, phase: &FileTaskPhase) -> Style {
    match phase {
        FileTaskPhase::Failed | FileTaskPhase::Aborted => theme.error,
        FileTaskPhase::Completed => theme.status,
        FileTaskPhase::Paused => theme.button_focused,
        _ => theme.text,
    }
}

fn point_in_rect(x: u16, y: u16, rect: Rect) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

fn intersect_rect(a: Rect, b: Rect) -> Option<Rect> {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = a.x.saturating_add(a.width).min(b.x.saturating_add(b.width));
    let y2 = a.y.saturating_add(a.height).min(b.y.saturating_add(b.height));
    (x2 > x1 && y2 > y1).then(|| Rect::new(x1, y1, x2 - x1, y2 - y1))
}

fn fit_text(text: &str, max: usize) -> (String, bool) {
    let truncated = crate::display_width::width(text) > max;
    (crate::display_width::truncate_right(text, max), truncated)
}



fn conflict_dialog_title(conflict: &ConflictPromptState) -> String {
    let index = conflict.conflict_index.max(1);
    match conflict.conflict_count {
        Some(count) if count > 0 => format!("Conflict {} of {}", index, count.max(index)),
        _ => format!("Conflict {}", index),
    }
}

fn item_kind_conflict_statement(kind: ConflictItemKind) -> &'static str {
    match kind {
        ConflictItemKind::Directory => "  A folder of the same name already exists here:",
        ConflictItemKind::File => "  A file of the same name already exists here:",
        ConflictItemKind::Symlink => "  A symlink of the same name already exists here:",
        ConflictItemKind::Other => "  An item of the same name already exists here:",
    }
}

fn conflict_item_name(conflict: &ConflictPromptState) -> String {
    conflict
        .destination
        .as_deref()
        .and_then(Path::file_name)
        .or_else(|| conflict.source.as_deref().and_then(Path::file_name))
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| conflict.title.clone())
}

fn conflict_parent_display(conflict: &ConflictPromptState) -> String {
    let parent = conflict
        .destination
        .as_deref()
        .and_then(Path::parent)
        .or_else(|| conflict.source.as_deref().and_then(Path::parent));
    parent
        .map(|path| with_trailing_separator(display_path_with_home(path)))
        .unwrap_or_else(|| "-".to_string())
}

fn conflict_comparison_text(
    label: &str,
    size: Option<u64>,
    modified: Option<SystemTime>,
) -> String {
    let size = size.map(format_bytes).unwrap_or_else(|| "--".to_string());
    let modified = modified.map(format_system_date).unwrap_or_else(|| "--".to_string());
    format!("    {:<8}    {:<8}    {}", label, size, modified)
}

fn display_path_with_home(path: &Path) -> String {
    if let Some(home) = home_dir_for_display() {
        if let Ok(stripped) = path.strip_prefix(&home) {
            if stripped.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~{}{}", std::path::MAIN_SEPARATOR, stripped.display());
        }
    }
    path.display().to_string()
}

fn home_dir_for_display() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

fn with_trailing_separator(mut path: String) -> String {
    let sep = std::path::MAIN_SEPARATOR.to_string();
    if !path.ends_with(&sep) {
        path.push(std::path::MAIN_SEPARATOR);
    }
    path
}

fn format_system_date(time: SystemTime) -> String {
    let days = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => (duration.as_secs() / 86_400) as i64,
        Err(err) => -(((err.duration().as_secs() + 86_399) / 86_400) as i64),
    };
    let (year, month, day) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}", year, month, day)
}

fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year as i32, month as u32, day as u32)
}

fn normalized_ratio(ratio: f64) -> f64 {
    if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn fit_spans(spans: Vec<Span<'static>>, width: usize, pad_style: Style) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut remaining = width;
    for span in spans {
        if remaining == 0 {
            break;
        }
        let style = span.style;
        let text = span.content.into_owned();
        let len = crate::display_width::width(&text);
        if len <= remaining {
            out.push(Span::styled(text, style));
            remaining -= len;
        } else {
            out.push(Span::styled(fit_text(&text, remaining).0, style));
            remaining = 0;
        }
    }
    if remaining > 0 {
        out.push(Span::styled(" ".repeat(remaining), pad_style));
    }
    out
}

fn format_rate(bytes_per_sec: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else if value >= 10.0 {
        format!("{:.0} {}", value, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{}:{:02}", minutes, seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;
    use std::path::{Path, PathBuf};


    fn buffer_row(buffer: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
        buffer_row_slice(buffer, 0, y, width)
    }

    fn buffer_row_slice(buffer: &ratatui::buffer::Buffer, x: u16, y: u16, width: u16) -> String {
        let mut row = String::new();
        for col in x..x.saturating_add(width) {
            row.push_str(buffer.get(col, y).symbol());
        }
        row
    }

    #[test]
    fn progress_update_is_pure_state_transition() {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Copy,
            "Copying files",
            FilePickerTheme::default(),
        );
        let mut item = ProgressItem::new("track.flac");
        item.bytes_done = 50;
        item.bytes_total = Some(100);
        state.apply_update(FileTaskProgressUpdate::Snapshot {
            phase: FileTaskPhase::Running,
            status: "Copying track.flac".to_string(),
            current_item: Some(item.clone()),
            totals: ProgressTotals {
                items_done: 0,
                items_total: Some(1),
                item_unit: ProgressUnit::Files,
                bytes_done: 50,
                bytes_total: Some(100),
                folders_done: 0,
                folders_total: Some(0),
                unknown_size_items: 0,
                completed: 0,
                skipped: 0,
                errors: 0,
                overwritten: 0,
                renamed: 0,
                merged: 0,
                not_attempted: 0,
            },
            rate_bytes_per_sec: Some(25),
        });
        assert_eq!(state.phase, FileTaskPhase::Running);
        assert_eq!(state.current_item, Some(item));
        assert_eq!(state.totals.ratio(), Some(0.5));
    }



    fn mockup_state() -> FileTaskProgressState {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Copy,
            "Copying files",
            FilePickerTheme::default(),
        );
        state.started_at = Instant::now() - Duration::from_secs(2);
        state.set_scope(FileTaskScope {
            source_root: Some(PathBuf::from("~/Documents/Audio")),
            source_summary: "".to_string(),
            destination: Some(PathBuf::from("~/library/abb/Gregg Allman - Laid Back")),
            destination_summary: None,
        });
        state.apply_update(FileTaskProgressUpdate::Snapshot {
            phase: FileTaskPhase::Running,
            status: "Copying".to_string(),
            current_item: Some(ProgressItem {
                label: "03 - Midnight Rider.flac".to_string(),
                source: None,
                destination: None,
                bytes_done: 62,
                bytes_total: Some(100),
            }),
            totals: ProgressTotals {
                items_done: 4,
                items_total: Some(10),
                item_unit: ProgressUnit::Files,
                bytes_done: 47,
                bytes_total: Some(100),
                folders_done: 0,
                folders_total: Some(0),
                unknown_size_items: 0,
                completed: 4,
                skipped: 0,
                errors: 0,
                overwritten: 0,
                renamed: 0,
                merged: 0,
                not_attempted: 0,
            },
            rate_bytes_per_sec: Some(82 * 1024 * 1024),
        });
        state
    }

    #[test]
    fn progress_dialog_renders_mockup_frame_manual_bars_styles_and_clickable_buttons() {
        let mut state = mockup_state();

        let backend = TestBackend::new(52, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| state.render(frame, Rect::new(0, 0, 52, 12)))
            .expect("draw");
        let buffer = terminal.backend().buffer();

        assert_eq!(
            buffer_row(buffer, 0, 52),
            "┌─ Copying files ──────────────────────────────────┐"
        );
        assert_eq!(
            buffer_row(buffer, 1, 52),
            "│  From:  ~/Documents/Audio                        │"
        );
        assert_eq!(
            buffer_row(buffer, 2, 52),
            "│  To:    ~/library/abb/Gregg Allman - Laid Back   │"
        );
        assert_eq!(
            buffer_row(buffer, 4, 52),
            "│  ▸ 03 - Midnight Rider.flac                      │"
        );
        assert_eq!(
            buffer_row(buffer, 5, 52),
            "│  [█████████████████████████░░░░░░░░░░░░░░░]  62% │"
        );
        assert_eq!(
            buffer_row(buffer, 6, 52),
            "│  Total   4 of 10 files  ·  47 B / 100 B          │"
        );
        assert_eq!(
            buffer_row(buffer, 7, 52),
            "│  [███████████████████░░░░░░░░░░░░░░░░░░░░░]  47% │"
        );
        let stats_row = buffer_row(buffer, 8, 52);
        assert!(stats_row.starts_with("│  82 MB/s"));
        assert!(stats_row.contains(" elapsed"));
        assert!(stats_row.contains(" left"));
        assert_eq!(
            buffer_row(buffer, 9, 52),
            "├──────────────────────────────────────────────────┤"
        );
        assert_eq!(
            buffer_row(buffer, 10, 52),
            "│         p Pause     s Skip     Esc Abort         │"
        );
        assert_eq!(
            buffer_row(buffer, 11, 52),
            "└──────────────────────────────────────────────────┘"
        );

        assert_eq!(buffer.get(0, 1).fg, Color::Cyan);
        assert_eq!(buffer.get(3, 0).fg, Color::White);
        assert_eq!(buffer.get(3, 1).fg, Color::DarkGray);
        assert_eq!(buffer.get(10, 1).fg, Color::White);
        assert_eq!(buffer.get(5, 4).fg, Color::Cyan);
        assert_eq!(buffer.get(4, 5).fg, Color::Cyan);
        assert_eq!(buffer.get(29, 5).fg, Color::DarkGray);
        assert_eq!(buffer.get(47, 5).fg, Color::White);
        assert_eq!(buffer.get(10, 10).fg, Color::Black);
        assert_eq!(buffer.get(10, 10).bg, Color::Cyan);
        assert_eq!(buffer.get(33, 10).fg, Color::Black);
        assert_eq!(buffer.get(33, 10).bg, Color::Red);

        let pause = state.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 10,
                row: 10,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 52, 12),
        );
        assert_eq!(pause, FileTaskUserAction::Pause);

        let skip = state.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 22,
                row: 10,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 52, 12),
        );
        assert_eq!(skip, FileTaskUserAction::SkipCurrent);

        let abort = state.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 34,
                row: 10,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 52, 12),
        );
        assert_eq!(abort, FileTaskUserAction::Abort);
    }

    #[test]
    fn progress_dialog_fills_available_width_inside_larger_host_area() {
        let mut state = mockup_state();

        let host_w: u16 = 96;
        let host_h: u16 = 18;
        let backend = TestBackend::new(host_w, host_h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| state.render(frame, Rect::new(0, 0, host_w, host_h)))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        // Dialog uses the full host width (>= minimum), centered vertically.
        let dialog_w = host_w;
        let y = (host_h - PROGRESS_DIALOG_HEIGHT) / 2;

        let title_row = buffer_row(buffer, y, dialog_w);
        assert!(title_row.starts_with("┌─ Copying files "));
        assert!(title_row.ends_with("┐"));
        assert_eq!(crate::display_width::width(&title_row), dialog_w as usize);

        let from_row = buffer_row(buffer, y + 1, dialog_w);
        assert!(from_row.contains("From:  ~/Documents/Audio"));

        let to_row = buffer_row(buffer, y + 2, dialog_w);
        assert!(to_row.contains("To:    ~/library/abb/Gregg Allman - Laid Back"));

        let stats_row = buffer_row(buffer, y + 8, dialog_w);
        assert!(stats_row.starts_with("│  82 MB/s"));
        assert!(stats_row.contains(" elapsed"));

        let rule_row = buffer_row(buffer, y + 9, dialog_w);
        assert!(rule_row.starts_with("├"));
        assert!(rule_row.ends_with("┤"));

        let button_row = buffer_row(buffer, y + 10, dialog_w);
        assert!(button_row.contains("p Pause"));
        assert!(button_row.contains("s Skip"));
        assert!(button_row.contains("Esc Abort"));

        let bottom_row = buffer_row(buffer, y + 11, dialog_w);
        assert!(bottom_row.starts_with("└"));
        assert!(bottom_row.ends_with("┘"));

        // Row below dialog is not part of the dialog — should be empty.
        assert_eq!(buffer_row(buffer, y + 12, dialog_w), " ".repeat(dialog_w as usize));
    }

    #[test]
    fn progress_current_file_style_is_independent_of_progress_fill_style() {
        let mut theme = FilePickerTheme::default();
        theme.progress_current_file = Style::default().fg(Color::Yellow);
        theme.progress_filled = Style::default().fg(Color::Green);
        let mut state = FileTaskProgressState::new(FileTaskKind::Copy, "Copying files", theme);
        state.apply_update(FileTaskProgressUpdate::Snapshot {
            phase: FileTaskPhase::Running,
            status: "Copying".to_string(),
            current_item: Some(ProgressItem {
                label: "track.flac".to_string(),
                source: None,
                destination: None,
                bytes_done: 1,
                bytes_total: Some(2),
            }),
            totals: ProgressTotals {
                items_done: 1,
                items_total: Some(2),
                item_unit: ProgressUnit::Files,
                bytes_done: 1,
                bytes_total: Some(2),
                folders_done: 0,
                folders_total: Some(0),
                unknown_size_items: 0,
                completed: 1,
                skipped: 0,
                errors: 0,
                overwritten: 0,
                renamed: 0,
                merged: 0,
                not_attempted: 0,
            },
            rate_bytes_per_sec: Some(1),
        });

        let backend = TestBackend::new(52, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| state.render(frame, Rect::new(0, 0, 52, 12)))
            .expect("draw");
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer.get(5, 4).fg, Color::Yellow);
        assert_eq!(buffer.get(4, 5).fg, Color::Green);
    }

    #[test]
    fn aggregate_ratio_uses_accounted_work_for_skipped_terminal_items() {
        let totals = ProgressTotals {
            items_done: 3,
            items_total: Some(3),
            item_unit: ProgressUnit::Files,
            bytes_done: 0,
            bytes_total: Some(4096),
            folders_done: 0,
            folders_total: Some(0),
            unknown_size_items: 0,
            completed: 0,
            skipped: 3,
            errors: 0,
            overwritten: 0,
            renamed: 0,
            merged: 0,
            not_attempted: 0,
        };

        assert_eq!(totals.byte_ratio(), Some(0.0));
        assert_eq!(totals.work_ratio(), Some(1.0));
        assert_eq!(totals.ratio(), Some(1.0));
    }

    #[test]
    fn pause_resume_are_user_intent_only() {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Move,
            "Moving files",
            FilePickerTheme::default(),
        );
        let pause = state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        assert_eq!(pause, FileTaskUserAction::Pause);
        assert_eq!(state.phase, FileTaskPhase::Preparing);
        state.apply_update(FileTaskProgressUpdate::Snapshot {
            phase: FileTaskPhase::Paused,
            status: "Paused".to_string(),
            current_item: None,
            totals: ProgressTotals::default(),
            rate_bytes_per_sec: None,
        });
        let resume = state.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        assert_eq!(resume, FileTaskUserAction::Resume);
    }

    #[test]
    fn active_elapsed_uses_current_time_but_terminal_elapsed_freezes() {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Scan,
            "Scanning",
            FilePickerTheme::default(),
        );
        let now = Instant::now();
        state.started_at = now - Duration::from_secs(10);
        state.updated_at = now - Duration::from_secs(7);
        state.phase = FileTaskPhase::Running;

        assert!(
            state.elapsed() >= Duration::from_secs(9),
            "active elapsed should advance from the current clock, not last update"
        );

        state.phase = FileTaskPhase::Completed;
        assert_eq!(state.elapsed(), Duration::from_secs(3));
    }

    #[test]
    fn terminal_state_acknowledges_instead_of_aborting() {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Verify,
            "Verifying",
            FilePickerTheme::default(),
        );
        state.apply_update(FileTaskProgressUpdate::Finished {
            status: "Done".to_string(),
            totals: ProgressTotals::default(),
        });
        let action = state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, FileTaskUserAction::Acknowledge);
    }

    #[test]
    fn conflict_prompt_enter_emits_selected_resolution_with_apply_all() {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Copy,
            "Copying files",
            FilePickerTheme::default(),
        );
        let mut conflict = ConflictPromptState::new(
            7,
            "track.flac already exists",
            "file destination already exists",
            ConflictItemKind::File,
        );
        conflict.apply_to_all = true;
        state.apply_update(FileTaskProgressUpdate::ShowConflict { conflict });

        let action = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            action,
            FileTaskUserAction::ChooseConflictResolution(ConflictResolution::Overwrite {
                apply_to_all: true,
            })
        );
    }

    #[test]
    fn async_conflict_stats_update_only_mutates_matching_active_prompt() {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Copy,
            "Copying files",
            FilePickerTheme::default(),
        );
        state.apply_update(FileTaskProgressUpdate::ShowConflict {
            conflict: ConflictPromptState::new(
                41,
                "album already exists",
                "directory destination already exists",
                ConflictItemKind::Directory,
            ),
        });

        state.apply_update(FileTaskProgressUpdate::UpdateConflictExistingStats {
            request_id: 40,
            size: 1234,
            modified: Some(UNIX_EPOCH),
        });
        assert_eq!(
            state.conflict.as_ref().expect("conflict").existing_size,
            None,
            "stale async stats must not affect the active conflict"
        );

        state.apply_update(FileTaskProgressUpdate::UpdateConflictExistingStats {
            request_id: 41,
            size: 5678,
            modified: Some(UNIX_EPOCH + Duration::from_secs(86_400)),
        });
        let conflict = state.conflict.as_ref().expect("conflict");
        assert_eq!(conflict.existing_size, Some(5678));
        assert_eq!(conflict.existing_modified, Some(UNIX_EPOCH + Duration::from_secs(86_400)));

        state.apply_update(FileTaskProgressUpdate::ClearConflict);
        state.apply_update(FileTaskProgressUpdate::UpdateConflictExistingStats {
            request_id: 41,
            size: 9999,
            modified: None,
        });
        assert_eq!(
            state.conflict_records.last().expect("record").existing_size,
            Some(5678),
            "late async stats after the prompt closes must not rewrite history"
        );
    }

    #[test]
    fn conflict_prompt_tab_selects_skip_and_space_toggles_apply_all() {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Move,
            "Moving files",
            FilePickerTheme::default(),
        );
        state.apply_update(FileTaskProgressUpdate::ShowConflict {
            conflict: ConflictPromptState::new(
                8,
                "track.flac already exists",
                "file destination already exists",
                ConflictItemKind::File,
            ),
        });

        assert_eq!(state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)), FileTaskUserAction::None);
        assert_eq!(state.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)), FileTaskUserAction::None);
        let action = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            action,
            FileTaskUserAction::ChooseConflictResolution(ConflictResolution::Skip {
                apply_to_all: true,
            })
        );
    }

    #[test]
    fn conflict_prompt_renders_on_small_supported_area() {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Copy,
            "Copying files",
            FilePickerTheme::default(),
        );
        let mut conflict = ConflictPromptState::new(
            1,
            "track.flac already exists",
            "file destination already exists",
            ConflictItemKind::File,
        );
        conflict.destination = Some(PathBuf::from("/tmp/track.flac"));
        state.apply_update(FileTaskProgressUpdate::ShowConflict { conflict });

        let backend = TestBackend::new(60, 15);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| state.render(frame, Rect::new(0, 0, 60, 15)))
            .expect("draw");
        let rendered = format!("{:?}", terminal.backend().buffer());

        assert!(rendered.contains("Conflict 1"));
        assert!(rendered.contains("Overwrite"));
        assert!(rendered.contains("Skip"));
    }

    #[test]
    fn redesigned_conflict_dialog_renders_required_content_without_legacy_redundancy() {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Copy,
            "Copying files",
            FilePickerTheme::default(),
        );
        let mut conflict = ConflictPromptState::new(
            2,
            "legacy red title should not render",
            "directory destination already exists for Chicago",
            ConflictItemKind::Directory,
        );
        conflict.conflict_index = 2;
        conflict.conflict_count = Some(3);
        conflict.source = Some(PathBuf::from("/music/incoming/Chicago - Chicago II (1970)"));
        conflict.destination = Some(PathBuf::from("/tmp/Chicago - Chicago II (1970)"));
        conflict.existing_size = None;
        conflict.incoming_size = Some(9);
        conflict.existing_modified = Some(UNIX_EPOCH);
        conflict.incoming_modified = Some(UNIX_EPOCH + Duration::from_secs(86_400));
        conflict.apply_to_all = true;
        state.apply_update(FileTaskProgressUpdate::ShowConflict { conflict });

        let backend = TestBackend::new(72, 15);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| state.render(frame, Rect::new(0, 0, 72, 15)))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let rendered = format!("{:?}", buffer);

        assert!(buffer_row(buffer, 0, 72).contains("Conflict 2 of 3"));
        assert!(buffer_row(buffer, 2, 72).contains("A folder of the same name already exists here:"));
        assert!(buffer_row(buffer, 4, 72).contains("Chicago - Chicago II (1970)"));
        assert!(buffer_row(buffer, 5, 72).contains("/tmp/"));
        assert!(buffer_row(buffer, 7, 72).contains("existing"));
        assert!(buffer_row(buffer, 7, 72).contains("--"));
        assert!(buffer_row(buffer, 7, 72).contains("1970-01-01"));
        assert!(buffer_row(buffer, 8, 72).contains("incoming"));
        assert!(buffer_row(buffer, 8, 72).contains("1970-01-02"));
        assert!(buffer_row(buffer, 10, 72).contains("[×] Apply to all remaining conflicts"));
        assert!(buffer_row(buffer, 13, 72).contains("Overwrite"));
        assert!(buffer_row(buffer, 13, 72).contains("Skip"));
        assert!(buffer_row(buffer, 13, 72).contains("Rename"));
        assert!(buffer_row(buffer, 13, 72).contains("Esc Abort"));
        assert!(!rendered.contains("directory destination already exists for"));
        assert!(!rendered.contains("legacy red title should not render"));
        assert!(!rendered.contains("Merge"));
    }

    #[test]
    fn conflict_checkbox_mouse_hit_region_toggles_apply_to_all() {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Copy,
            "Copying files",
            FilePickerTheme::default(),
        );
        state.apply_update(FileTaskProgressUpdate::ShowConflict {
            conflict: ConflictPromptState::new(
                9,
                "track.flac already exists",
                "file destination already exists",
                ConflictItemKind::File,
            ),
        });

        let area = Rect::new(0, 0, 60, 15);
        let backend = TestBackend::new(60, 15);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| state.render(frame, area)).expect("draw");
        assert!(!state.conflict.as_ref().expect("conflict").apply_to_all);

        let action = state.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 6,
                row: 10,
                modifiers: KeyModifiers::NONE,
            },
            area,
        );

        assert_eq!(action, FileTaskUserAction::None);
        assert!(state.conflict.as_ref().expect("conflict").apply_to_all);
        assert_eq!(
            state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            FileTaskUserAction::ChooseConflictResolution(ConflictResolution::Overwrite {
                apply_to_all: true,
            })
        );
    }

    #[test]
    fn scope_and_error_updates_are_pure_state_transitions() {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Copy,
            "Copying files",
            FilePickerTheme::default(),
        );
        state.max_error_records = 2;
        state.apply_update(FileTaskProgressUpdate::SetScope {
            scope: FileTaskScope {
                source_root: Some(PathBuf::from("/music/inbox")),
                source_summary: "3 selected items".to_string(),
                destination: Some(PathBuf::from("/music/archive")),
                destination_summary: None,
            },
        });
        state.apply_update(FileTaskProgressUpdate::RecordError {
            error: FileTaskErrorRecord::new("one.flac", "permission denied"),
            totals: ProgressTotals::default(),
        });
        state.apply_update(FileTaskProgressUpdate::RecordError {
            error: FileTaskErrorRecord::new("two.flac", "read failed"),
            totals: ProgressTotals::default(),
        });
        state.apply_update(FileTaskProgressUpdate::RecordError {
            error: FileTaskErrorRecord::new("three.flac", "checksum mismatch"),
            totals: ProgressTotals::default(),
        });

        assert_eq!(state.scope.as_ref().unwrap().source_summary, "3 selected items");
        assert_eq!(state.error_records.len(), 2);
        assert_eq!(state.error_records[0].item_label, "two.flac");
        assert_eq!(state.error_records[1].item_label, "three.flac");
    }

    #[test]
    fn render_keeps_stable_destination_separate_from_current_file() {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Copy,
            "Copying files",
            FilePickerTheme::default(),
        );
        state.set_scope(FileTaskScope {
            source_root: Some(PathBuf::from("/source-root")),
            source_summary: "1 selected folder".to_string(),
            destination: Some(PathBuf::from("/stable-destination")),
            destination_summary: None,
        });
        state.apply_update(FileTaskProgressUpdate::Snapshot {
            phase: FileTaskPhase::Running,
            status: "Copying nested file".to_string(),
            current_item: Some(ProgressItem {
                label: "track.flac".to_string(),
                source: Some(PathBuf::from("/source-root/album/track.flac")),
                destination: Some(PathBuf::from("/stable-destination/album/track.flac")),
                bytes_done: 5,
                bytes_total: Some(10),
            }),
            totals: ProgressTotals {
                items_done: 0,
                items_total: Some(1),
                item_unit: ProgressUnit::Files,
                bytes_done: 5,
                bytes_total: Some(10),
                folders_done: 0,
                folders_total: Some(1),
                unknown_size_items: 0,
                completed: 0,
                skipped: 0,
                errors: 0,
                overwritten: 0,
                renamed: 0,
                merged: 0,
                not_attempted: 0,
            },
            rate_bytes_per_sec: Some(5),
        });

        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| state.render(frame, Rect::new(0, 0, 96, 18)))
            .expect("draw");
        let rendered = format!("{:?}", terminal.backend().buffer());

        assert!(rendered.contains("From:  /source-root"));
        assert!(rendered.contains("To:    /stable-destination"));
        assert!(rendered.contains("▸ track.flac"));
    }

    #[test]
    fn terminal_failure_renders_recent_error_record() {
        let mut state = FileTaskProgressState::new(
            FileTaskKind::Move,
            "Moving files",
            FilePickerTheme::default(),
        );
        state.record_error(FileTaskErrorRecord::new("broken.flac", "write failed"));
        state.apply_update(FileTaskProgressUpdate::Failed {
            status: "moved 2 file(s), 0 folder(s), 0 skipped, 1 errors".to_string(),
            totals: ProgressTotals {
                items_done: 3,
                items_total: Some(3),
                item_unit: ProgressUnit::Files,
                bytes_done: 12,
                bytes_total: Some(12),
                folders_done: 0,
                folders_total: Some(0),
                unknown_size_items: 0,
                completed: 2,
                skipped: 0,
                errors: 1,
                overwritten: 0,
                renamed: 0,
                merged: 0,
                not_attempted: 0,
            },
        });

        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| state.render(frame, Rect::new(0, 0, 96, 18)))
            .expect("draw");
        let rendered = format!("{:?}", terminal.backend().buffer());

        assert!(rendered.contains("Failed"));
        assert!(rendered.contains("broken.flac"));
        assert!(rendered.contains("write failed"));
    }
}
