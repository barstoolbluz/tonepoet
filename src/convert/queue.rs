//! Conversion queue management

use super::formats::{AudioFormat, ConversionOptions, FileFormat};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

/// Phases of conversion process
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ConversionPhase {
    Extracting,
    Analyzing,
    Renaming,
    Tagging,
    Converting,
    PostProcessing,
    Finalizing,
}

impl ConversionPhase {
    pub fn display_name(&self) -> &str {
        match self {
            ConversionPhase::Extracting => "Extracting archive",
            ConversionPhase::Analyzing => "Analyzing files",
            ConversionPhase::Renaming => "Renaming files",
            ConversionPhase::Tagging => "Updating metadata",
            ConversionPhase::Converting => "Converting audio",
            ConversionPhase::PostProcessing => "Applying post-processing",
            ConversionPhase::Finalizing => "Finalizing",
        }
    }

    /// Short name for progress bar display
    pub fn short_name(&self) -> &str {
        match self {
            ConversionPhase::Extracting => "Extracting",
            ConversionPhase::Analyzing => "Analyzing",
            ConversionPhase::Renaming => "Renaming",
            ConversionPhase::Tagging => "Tagging",
            ConversionPhase::Converting => "Converting",
            ConversionPhase::PostProcessing => "Processing",
            ConversionPhase::Finalizing => "Finalizing",
        }
    }

    /// Weight of this phase in overall progress (0.0 to 1.0)
    pub fn weight(&self) -> f32 {
        match self {
            ConversionPhase::Extracting => 0.15,     // 15%
            ConversionPhase::Analyzing => 0.05,      // 5%
            ConversionPhase::Renaming => 0.10,       // 10%
            ConversionPhase::Tagging => 0.10,        // 10%
            ConversionPhase::Converting => 0.50,     // 50% (bulk of the work)
            ConversionPhase::PostProcessing => 0.05, // 5%
            ConversionPhase::Finalizing => 0.05,     // 5%
        }
    }

    /// Starting point of this phase in overall progress
    pub fn start_progress(&self) -> f32 {
        match self {
            ConversionPhase::Extracting => 0.0,
            ConversionPhase::Analyzing => 0.15,
            ConversionPhase::Renaming => 0.20,
            ConversionPhase::Tagging => 0.30,
            ConversionPhase::Converting => 0.40,
            ConversionPhase::PostProcessing => 0.90,
            ConversionPhase::Finalizing => 0.95,
        }
    }

    /// Calculate overall progress given phase progress
    pub fn calculate_overall_progress(&self, phase_progress: f32) -> f32 {
        let phase_progress_normalized = phase_progress.max(0.0).min(100.0) / 100.0;
        let start = self.start_progress();
        let weight = self.weight();
        (start + (weight * phase_progress_normalized)) * 100.0
    }
}

/// Status of a conversion item
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConversionStatus {
    /// Not configured yet - just added to the list
    NotConfigured,
    /// Waiting to be processed
    Queued,
    /// Currently being processed
    Processing {
        progress: f32, // Keep for backward compatibility
        /// Optional message like "Converting file 3 of 10..."
        message: Option<String>,
        /// For multi-file conversions: current file and total files
        file_progress: Option<(u32, u32)>,
        /// NEW: Phase information
        phase: Option<ConversionPhase>,
        phase_progress: Option<f32>,
    },
    /// Successfully completed
    Completed {
        output_path: PathBuf,
        /// Durable per-album run log, when one was written.
        #[serde(default)]
        log_path: Option<PathBuf>,
    },
    /// Completed with some tracks dropped (explicit partial opt-in).
    /// Terminal; never an alias for success.
    Partial {
        output_path: PathBuf,
        successful: u32,
        failed: u32,
        log_path: PathBuf,
    },
    /// Failed with error message
    Failed {
        error: String,
        /// Durable per-album run log, when one was written.
        #[serde(default)]
        log_path: Option<PathBuf>,
    },
    /// Paused by user
    Paused,
    /// Cancelled by user
    Cancelled,
}


/// Ephemeral progress for a concurrently processing album track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackProgress {
    pub track_label: String,
    pub step_description: String,
    pub progress_fraction: f32,
    /// Reporter generation that owns this row. Used to ignore stale lossy
    /// progress samples after a reliable lifecycle clear for the same track.
    pub lifecycle_epoch: u64,
}

/// A single item in the conversion queue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionItem {
    /// Unique identifier
    pub id: String,
    /// Input file path
    pub input_path: PathBuf,
    /// Detected input format (archive or audio)
    pub input_format: FileFormat,
    /// Target output format
    pub output_format: AudioFormat,
    /// Output file path (once determined)
    pub output_path: Option<PathBuf>,
    /// Conversion options
    pub options: ConversionOptions,
    /// Current status
    pub status: ConversionStatus,
    /// When the item was added to queue
    pub queued_at: DateTime<Utc>,
    /// When processing started
    pub started_at: Option<DateTime<Utc>>,
    /// When processing completed
    pub completed_at: Option<DateTime<Utc>>,
    /// File size in bytes
    pub file_size: u64,
    /// Whether selected in UI
    #[serde(skip)]
    pub selected: bool,
    /// Per-track progress currently visible in the TUI for multi-track items.
    #[serde(skip)]
    pub active_tracks: BTreeMap<u32, TrackProgress>,
    /// Last reliably cleared reporter generation by track index. This prevents
    /// delayed lossy broadcast progress from resurrecting a row after cleanup.
    #[serde(skip)]
    pub closed_track_epochs: BTreeMap<u32, u64>,
    /// Archive password (for 7z files)
    pub archive_password: Option<String>,
    /// Exact Chunk 1 planner settings selected by the UI/CLI.
    ///
    /// Kept on the queue item so persisted queued work can recover without
    /// projecting through the legacy option surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_settings: Option<tonepoet_pipeline::PipelineSettings>,
    /// New pipeline request (populated during migration; legacy fields
    /// remain until PR 10 finishes CLI/TUI surface).
    #[serde(default)]
    pub pipeline_request: Option<crate::convert::pipeline::PipelineRequest>,
}

impl Default for ConversionItem {
    fn default() -> Self {
        Self {
            id: String::new(),
            input_path: PathBuf::new(),
            input_format: FileFormat::Audio(AudioFormat::Flac),
            output_format: AudioFormat::Flac,
            output_path: None,
            options: ConversionOptions::default(),
            status: ConversionStatus::NotConfigured,
            queued_at: Utc::now(),
            started_at: None,
            completed_at: None,
            file_size: 0,
            selected: false,
            active_tracks: BTreeMap::new(),
            closed_track_epochs: BTreeMap::new(),
            archive_password: None,
            pipeline_settings: None,
            pipeline_request: None,
        }
    }
}

impl ConversionItem {
    /// Create a new conversion item
    pub fn new(input_path: PathBuf, input_format: FileFormat, options: ConversionOptions) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let file_size = std::fs::metadata(&input_path).map(|m| m.len()).unwrap_or(0);

        let archive_password = None;
        let pipeline_settings = options.pipeline_settings.clone();

        Self {
            id,
            input_path,
            input_format,
            output_format: options.output_format,
            output_path: None,
            options,
            status: ConversionStatus::NotConfigured, // Items start as NotConfigured; user must select settings
            queued_at: Utc::now(),
            started_at: None,
            completed_at: None,
            file_size,
            selected: false,
            active_tracks: BTreeMap::new(),
            closed_track_epochs: BTreeMap::new(),
            archive_password,
            pipeline_settings,
            pipeline_request: None,
        }
    }

    /// Create a new conversion item with exact Chunk 1 planner settings attached.
    pub fn new_with_pipeline_settings(
        input_path: PathBuf,
        input_format: FileFormat,
        mut options: ConversionOptions,
        settings: tonepoet_pipeline::PipelineSettings,
    ) -> Self {
        options.pipeline_settings = Some(settings.clone());
        let mut item = Self::new(input_path, input_format, options);
        item.pipeline_settings = Some(settings);
        item
    }

    /// Attach exact Chunk 1 planner settings to an existing queue item.
    pub fn set_pipeline_settings(&mut self, settings: tonepoet_pipeline::PipelineSettings) {
        self.options.pipeline_settings = Some(settings.clone());
        self.pipeline_settings = Some(settings);
        self.pipeline_request = None;
    }

    /// Check if the item is in a terminal state
    pub fn is_finished(&self) -> bool {
        matches!(
            self.status,
            ConversionStatus::Completed { .. }
                | ConversionStatus::Partial { .. }
                | ConversionStatus::Failed { .. }
                | ConversionStatus::Cancelled
        )
    }

    /// Check if the item can be retried
    pub fn can_retry(&self) -> bool {
        matches!(
            self.status,
            ConversionStatus::Failed { .. }
                | ConversionStatus::Partial { .. }
                | ConversionStatus::Cancelled
        )
    }
}

fn _status_progress(status: &ConversionStatus) -> f32 {
    match status {
        ConversionStatus::Processing { progress, .. } => *progress,
        ConversionStatus::Completed { .. } | ConversionStatus::Partial { .. } => 100.0,
        ConversionStatus::Queued | ConversionStatus::Paused | ConversionStatus::NotConfigured => {
            0.0
        }
        ConversionStatus::Failed { .. } | ConversionStatus::Cancelled => 0.0,
    }
}

/// Manages the queue of files to be converted
pub struct ConversionQueue {
    /// Items waiting to be processed
    items: VecDeque<ConversionItem>,
    /// Currently processing item
    current: Option<ConversionItem>,
    /// Completed items (kept for history)
    completed: Vec<ConversionItem>,
}

impl ConversionQueue {
    /// Create a new empty queue
    pub fn new() -> Self {
        Self {
            items: VecDeque::new(),
            current: None,
            completed: Vec::new(),
        }
    }

    /// Get mutable access to the items queue (for loading persisted items)
    pub(crate) fn items_mut(&mut self) -> &mut VecDeque<ConversionItem> {
        &mut self.items
    }

    /// Add an item to the queue only when options already carry exact Chunk 1 planner settings.
    ///
    /// Older UI code used this method with legacy `ConversionOptions` only. That is now
    /// intentionally rejected at the queue boundary instead of failing later in
    /// `process_queue()`: a queued production item must be lossless before it can enter
    /// the shared scheduler. Compatibility/import code that cannot yet construct
    /// `PipelineSettings` should leave the item `NotConfigured` and call
    /// `set_pipeline_settings()` before marking it `Queued`.
    pub fn add_item(&mut self, path: PathBuf, format: FileFormat, options: ConversionOptions) {
        let item = ConversionItem::new(path, format, options);
        debug_assert!(
            item.pipeline_settings.is_some() || item.pipeline_request.is_some(),
            "ConversionQueue::add_item requires full PipelineSettings for production queued work; use add_item_with_pipeline_settings"
        );
        self.items.push_back(item);
    }

    /// Add an item to the queue with exact Chunk 1 planner settings.
    ///
    /// UI/CLI callers should use this when they have collected the full
    /// `PipelineSettings` value from user selections. This keeps normal queue
    /// processing lossless without prebuilding a full orchestrator request.
    pub fn add_item_with_pipeline_settings(
        &mut self,
        path: PathBuf,
        format: FileFormat,
        options: ConversionOptions,
        settings: tonepoet_pipeline::PipelineSettings,
    ) {
        let item = ConversionItem::new_with_pipeline_settings(path, format, options, settings);
        self.items.push_back(item);
    }

    /// Add a pre-configured item directly to the queue.
    ///
    /// The item may be `NotConfigured`, but any item whose status is `Queued` must
    /// already contain full planner settings or a prebuilt `PipelineRequest`.
    pub fn add_item_direct(&mut self, item: ConversionItem) {
        debug_assert!(
            item.status != ConversionStatus::Queued
                || item.pipeline_settings.is_some()
                || item.pipeline_request.is_some()
                || item.options.pipeline_settings.is_some(),
            "queued ConversionItem must contain full PipelineSettings or a prebuilt PipelineRequest"
        );
        self.items.push_back(item);
    }

    /// Return every queued item missing a lossless Chunk 1 settings handoff.
    pub fn queued_items_missing_pipeline_settings(&self) -> Vec<&ConversionItem> {
        self.items
            .iter()
            .filter(|item| item.status == ConversionStatus::Queued)
            .filter(|item| {
                item.pipeline_request.is_none()
                    && item.pipeline_settings.is_none()
                    && item.options.pipeline_settings.is_none()
            })
            .collect()
    }

    /// Validate that all runnable queue entries have exact planner settings.
    pub fn validate_full_settings_handoff(&self) -> Result<(), String> {
        let missing: Vec<String> = self
            .queued_items_missing_pipeline_settings()
            .into_iter()
            .map(|item| item.id.clone())
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "queued conversion items missing full PipelineSettings: {}",
                missing.join(",")
            ))
        }
    }

    /// Get the next item to process
    pub fn next_item(&mut self) -> Option<ConversionItem> {
        // Move current to completed if it exists
        if let Some(current) = self.current.take() {
            if current.is_finished() {
                self.completed.push(current);
            } else {
                // Put it back if not finished
                self.items.push_front(current);
            }
        }

        // Find next queued item
        for i in 0..self.items.len() {
            if self.items[i].status == ConversionStatus::Queued {
                self.current = Some(self.items.remove(i).unwrap());
                return self.current.clone();
            }
        }

        None
    }

    /// Update the status of the current item
    pub fn update_current_status(&mut self, status: ConversionStatus) {
        if let Some(ref mut current) = self.current {
            if matches!(
                status,
                ConversionStatus::Completed { .. }
                    | ConversionStatus::Partial { .. }
                    | ConversionStatus::Failed { .. }
                    | ConversionStatus::Cancelled
            ) {
                current.active_tracks.clear();
                current.closed_track_epochs.clear();
            }
            current.status = status;

            match &current.status {
                ConversionStatus::Processing { .. } => {
                    if current.started_at.is_none() {
                        current.started_at = Some(Utc::now());
                    }
                }
                ConversionStatus::Completed { .. }
                | ConversionStatus::Partial { .. }
                | ConversionStatus::Failed { .. }
                | ConversionStatus::Cancelled => {
                    current.completed_at = Some(Utc::now());
                }
                _ => {}
            }
        }
    }

    /// Get the current item being processed
    pub fn current_item(&self) -> Option<&ConversionItem> {
        self.current.as_ref()
    }

    /// Get all items (queued + current + completed)
    pub fn all_items(&self) -> Vec<&ConversionItem> {
        let mut items: Vec<&ConversionItem> = self.items.iter().collect();
        if let Some(ref current) = self.current {
            items.push(current);
        }
        items.extend(self.completed.iter());
        items
    }

    /// Get all items mutably (queued + current + completed)
    pub fn all_items_mut(&mut self) -> Vec<&mut ConversionItem> {
        let mut items: Vec<&mut ConversionItem> = self.items.iter_mut().collect();
        if let Some(ref mut current) = self.current {
            items.push(current);
        }
        items.extend(self.completed.iter_mut());
        items
    }

    /// Get queued items (only those ready for conversion, not NotConfigured)
    pub fn queued_items(&self) -> Vec<&ConversionItem> {
        self.items
            .iter()
            .filter(|item| item.status == ConversionStatus::Queued)
            .collect()
    }

    /// Get completed items
    pub fn completed_items(&self) -> usize {
        self.completed
            .iter()
            .filter(|item| matches!(item.status, ConversionStatus::Completed { .. }))
            .count()
    }

    /// Get failed items
    pub fn failed_items(&self) -> usize {
        self.completed
            .iter()
            .filter(|item| matches!(item.status, ConversionStatus::Failed { .. }))
            .count()
    }

    /// Get partial items (counted separately from completed and failed)
    pub fn partial_items(&self) -> usize {
        self.completed
            .iter()
            .filter(|item| matches!(item.status, ConversionStatus::Partial { .. }))
            .count()
    }

    /// Get total items
    pub fn total_items(&self) -> usize {
        self.items.len() + (if self.current.is_some() { 1 } else { 0 }) + self.completed.len()
    }

    /// Get count of active items (excluding completed/failed)
    /// This counts items waiting in queue plus currently processing item
    pub fn active_items_count(&self) -> usize {
        self.items.len() + (if self.current.is_some() { 1 } else { 0 })
    }

    /// Clear completed items only
    pub fn clear_completed(&mut self) {
        self.items
            .retain(|item| !matches!(item.status, ConversionStatus::Completed { .. }));
        self.completed
            .retain(|item| !matches!(item.status, ConversionStatus::Completed { .. }));
    }

    /// Clear all terminal items (Completed, Failed, Partial, Cancelled)
    pub fn clear_finished(&mut self) {
        let is_terminal = |item: &ConversionItem| {
            matches!(
                item.status,
                ConversionStatus::Completed { .. }
                    | ConversionStatus::Partial { .. }
                    | ConversionStatus::Failed { .. }
                    | ConversionStatus::Cancelled
            )
        };
        self.items.retain(|item| !is_terminal(item));
        self.completed.retain(|item| !is_terminal(item));
    }

    /// Clear all items from the queue
    pub fn clear(&mut self) {
        self.items.clear();
        self.current = None;
        self.completed.clear();
    }

    /// Find a specific item by ID for updating
    pub fn find_item_mut(&mut self, item_id: &str) -> Option<&mut ConversionItem> {
        // Check in the main items queue
        if let Some(item) = self.items.iter_mut().find(|item| item.id == item_id) {
            return Some(item);
        }

        // Check if it's the current item
        if let Some(ref mut current) = self.current {
            if current.id == item_id {
                return Some(current);
            }
        }

        // Check in completed items
        if let Some(item) = self.completed.iter_mut().find(|item| item.id == item_id) {
            return Some(item);
        }

        None
    }

    /// Update item status by ID.
    ///
    /// A `track_index` scopes the update to one concurrent track. Track-scoped
    /// terminal states are display-only and must not finalize the whole item.
    /// Track-scoped processing states update only `active_tracks`, so delayed
    /// per-track updates cannot overwrite the parent row's status/progress.
    /// Callers that go directly through `ConversionQueue` get the same behavior
    /// as callers that go through `ConversionManager`.
    pub fn update_item_status(
        &mut self,
        item_id: &str,
        status: ConversionStatus,
        progress: f32,
        track_index: Option<u32>,
        track_epoch: Option<u64>,
    ) -> bool {
        let Some(item) = self.find_item_mut(item_id) else {
            return false;
        };

        apply_item_status_update(item, status, progress, track_index, track_epoch);
        true
    }

    /// Clear one active per-track display row without changing item-level status.
    pub fn clear_track_progress(&mut self, item_id: &str, track_index: u32, track_epoch: u64) -> bool {
        let Some(item) = self.find_item_mut(item_id) else {
            return false;
        };

        clear_item_track_progress(item, track_index, track_epoch);
        true
    }

    /// Pause selected items
    pub fn pause_selected(&mut self) {
        for item in &mut self.items {
            if item.selected && item.status == ConversionStatus::Queued {
                item.status = ConversionStatus::Paused;
            }
        }
    }

    /// Resume selected items
    pub fn resume_selected(&mut self) {
        for item in &mut self.items {
            if item.selected && item.status == ConversionStatus::Paused {
                item.status = ConversionStatus::Queued;
            }
        }
    }

    /// Cancel selected items
    pub fn cancel_selected(&mut self) {
        self.items.retain(|item| {
            !(item.selected
                && matches!(
                    item.status,
                    ConversionStatus::Queued | ConversionStatus::Paused
                ))
        });
    }

    /// Retry failed items
    pub fn retry_failed(&mut self) {
        let mut to_retry = Vec::new();

        for item in &mut self.completed {
            if item.selected && item.can_retry() {
                let mut new_item = item.clone();
                new_item.active_tracks.clear();
                new_item.closed_track_epochs.clear();
                new_item.status = ConversionStatus::Queued;
                new_item.started_at = None;
                new_item.completed_at = None;
                to_retry.push(new_item);
            }
        }

        for item in to_retry {
            self.items.push_back(item);
        }
    }

    /// Remove selected items from the queue
    pub fn remove_selected(&mut self) -> usize {
        let initial_count = self.items.len();
        self.items.retain(|item| !item.selected);
        initial_count - self.items.len()
    }
}

impl Default for ConversionQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Apply the canonical item/track status mutation for a conversion item.
///
/// This is the single source of truth for status and `active_tracks` updates.
/// `ConversionQueue::update_item_status()` and `ConversionManager::update_item_status()`
/// both call this helper so direct queue callers cannot drift from the manager path.
/// Manager-only concerns such as timestamps, output paths, persistence, and UI history
/// remain outside this function.
pub(crate) fn apply_item_status_update(
    item: &mut ConversionItem,
    status: ConversionStatus,
    progress: f32,
    track_index: Option<u32>,
    track_epoch: Option<u64>,
) {
    match track_index {
        Some(track_index) => {
            // Track-scoped updates are display-only. They may create, update, or
            // remove the corresponding active track row, but they must never
            // mutate the parent queue item's status/progress/lifecycle state.
            // The item-level reporter (`track_index == None`) remains the sole
            // authority for the parent row.
            //
            // Track progress arrives over a lossy broadcast channel. Each track
            // reporter carries a lifecycle epoch; reliable ClearTrack events
            // record the last closed epoch. Delayed progress for an already
            // closed epoch is ignored so cleanup cannot be undone by old ticks.
            // Delayed progress for an older epoch is also ignored when a newer
            // active row already exists, so stale broadcast telemetry cannot
            // overwrite the currently displayed reporter generation.
            if is_terminal_status(&item.status) {
                if is_terminal_status(&status) {
                    let epoch = track_epoch.unwrap_or(0);
                    clear_item_track_progress(item, track_index, epoch);
                }
                return;
            }

            let epoch = track_epoch.unwrap_or(0);
            if item
                .closed_track_epochs
                .get(&track_index)
                .map_or(false, |closed_epoch| *closed_epoch >= epoch)
            {
                return;
            }

            if item
                .active_tracks
                .get(&track_index)
                .map_or(false, |active| active.lifecycle_epoch > epoch)
            {
                return;
            }

            if is_terminal_status(&status)
                || is_track_scoped_processing_complete(&status, progress)
            {
                clear_item_track_progress(item, track_index, epoch);
            } else if let Some(mut track_progress) =
                track_progress_from_status(track_index, &status, progress)
            {
                track_progress.lifecycle_epoch = epoch;
                item.active_tracks.insert(track_index, track_progress);
            }
        }
        None => {
            // Item/album-scoped processing updates are allowed to interleave with
            // concurrent track workers. They update the parent row only; explicit
            // typed lifecycle updates and track-scoped terminal states remove
            // individual track rows. Whole-item terminal states clear all track
            // display state and closed-epoch barriers.
            let starts_new_processing_run = matches!(&status, ConversionStatus::Processing { .. })
                && !matches!(&item.status, ConversionStatus::Processing { .. });
            if starts_new_processing_run {
                item.closed_track_epochs.clear();
            }
            if is_terminal_status(&status) {
                item.active_tracks.clear();
                item.closed_track_epochs.clear();
            }

            item.status = status;
        }
    }
}

pub(crate) fn clear_item_track_progress(
    item: &mut ConversionItem,
    track_index: u32,
    track_epoch: u64,
) {
    let remove_active = item
        .active_tracks
        .get(&track_index)
        .map(|active| active.lifecycle_epoch <= track_epoch)
        .unwrap_or(false);
    if remove_active {
        item.active_tracks.remove(&track_index);
    }

    item.closed_track_epochs
        .entry(track_index)
        .and_modify(|existing| *existing = (*existing).max(track_epoch))
        .or_insert(track_epoch);
}

pub(crate) fn is_terminal_status(status: &ConversionStatus) -> bool {
    matches!(
        status,
        ConversionStatus::Completed { .. }
            | ConversionStatus::Partial { .. }
            | ConversionStatus::Failed { .. }
            | ConversionStatus::Cancelled
    )
}

const TRACK_SCOPED_COMPLETE_PROGRESS_PERCENT: f32 = 99.95;

pub(crate) fn is_track_scoped_processing_complete(
    status: &ConversionStatus,
    fallback_progress: f32,
) -> bool {
    let ConversionStatus::Processing { progress, .. } = status else {
        return false;
    };

    let item_progress = progress.max(fallback_progress);
    item_progress.is_finite() && item_progress >= TRACK_SCOPED_COMPLETE_PROGRESS_PERCENT
}

pub(crate) fn track_progress_from_status(
    track_index: u32,
    status: &ConversionStatus,
    _fallback_progress: f32,
) -> Option<TrackProgress> {
    let ConversionStatus::Processing {
        progress,
        message,
        phase_progress,
        ..
    } = status
    else {
        return None;
    };

    let message = message.as_deref().unwrap_or("").trim();
    let parsed = parse_track_progress_message(message);
    let track_label = parsed
        .track_label
        .filter(|label| !label.trim().is_empty())
        .unwrap_or_else(|| format!("track {}", track_index.saturating_add(1)));
    let step_description = parsed
        .step_description
        .filter(|step| !step.trim().is_empty())
        .unwrap_or_else(|| "Processing".to_string());
    let progress_percent = parsed
        .progress_percent
        .or(*phase_progress)
        .unwrap_or(*progress)
        .clamp(0.0, 100.0);

    Some(TrackProgress {
        track_label,
        step_description,
        progress_fraction: progress_percent / 100.0,
        lifecycle_epoch: 0,
    })
}

#[derive(Debug, Default)]
struct ParsedTrackProgressMessage {
    track_label: Option<String>,
    step_description: Option<String>,
    progress_percent: Option<f32>,
}

fn parse_track_progress_message(message: &str) -> ParsedTrackProgressMessage {
    let message = message.trim();
    if message.is_empty() {
        return ParsedTrackProgressMessage::default();
    }

    let parts = message
        .split('·')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    if parts.len() >= 3 {
        let progress_percent = parse_percent(parts[parts.len() - 1]);
        let step_end = if progress_percent.is_some() {
            parts.len() - 1
        } else {
            parts.len()
        };
        return ParsedTrackProgressMessage {
            track_label: Some(parts[0].to_string()),
            step_description: Some(parts[1..step_end].join(" · ")),
            progress_percent: progress_percent.or_else(|| parse_trailing_percent(message)),
        };
    }

    if parts.len() == 2 {
        let progress_percent = parse_percent(parts[1]);
        return ParsedTrackProgressMessage {
            track_label: Some(parts[0].to_string()),
            step_description: if progress_percent.is_some() {
                None
            } else {
                Some(parts[1].to_string())
            },
            progress_percent: progress_percent.or_else(|| parse_trailing_percent(message)),
        };
    }

    ParsedTrackProgressMessage {
        track_label: None,
        step_description: Some(message.to_string()),
        progress_percent: parse_trailing_percent(message),
    }
}

fn parse_percent(text: &str) -> Option<f32> {
    let number = text.trim().strip_suffix('%')?.trim();
    number
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
}

fn parse_trailing_percent(message: &str) -> Option<f32> {
    let marker = message.rfind('%')?;
    let before_percent = &message[..marker];
    let start = before_percent
        .rfind(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .map(|idx| idx + 1)
        .unwrap_or(0);
    before_percent[start..]
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn processing_status(message: &str, progress: f32) -> ConversionStatus {
        ConversionStatus::Processing {
            progress,
            message: Some(message.to_string()),
            file_progress: None,
            phase: Some(ConversionPhase::Converting),
            phase_progress: Some(progress),
        }
    }

    fn queue_with_item() -> ConversionQueue {
        let mut queue = ConversionQueue::new();
        let mut item = ConversionItem::default();
        item.id = "item-1".to_string();
        queue.add_item_direct(item);
        queue
    }

    fn item(queue: &mut ConversionQueue) -> &mut ConversionItem {
        queue.find_item_mut("item-1").expect("test item exists")
    }

    #[test]
    fn parses_track_label_step_and_percent_from_pipeline_message() {
        let status = processing_status("track 1 (Right Off) · Convert DSD to PCM · 14%", 42.0);
        let progress = track_progress_from_status(0, &status, 42.0)
            .expect("processing status yields track progress");

        assert_eq!(progress.track_label, "track 1 (Right Off)");
        assert_eq!(progress.step_description, "Convert DSD to PCM");
        assert!((progress.progress_fraction - 0.14).abs() < 0.0001);
    }

    #[test]
    fn track_scoped_updates_are_idempotent_upserts() {
        let mut queue = queue_with_item();

        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Convert DSD to PCM · 14%", 20.0),
            20.0,
            Some(0),
        Some(1)));
        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Encode FLAC · 18%", 24.0),
            24.0,
            Some(0),
        Some(1)));

        let item = item(&mut queue);
        assert_eq!(item.active_tracks.len(), 1);
        let track = item.active_tracks.get(&0).expect("track remains present");
        assert_eq!(track.track_label, "track 1 (Right Off)");
        assert_eq!(track.step_description, "Encode FLAC");
        assert!((track.progress_fraction - 0.18).abs() < 0.0001);
    }

    #[test]
    fn track_scoped_processing_does_not_mutate_parent_status() {
        let mut queue = queue_with_item();
        assert!(queue.update_item_status(
            "item-1",
            ConversionStatus::Processing {
                progress: 35.0,
                message: Some("Album materialization".to_string()),
                file_progress: None,
                phase: Some(ConversionPhase::Analyzing),
                phase_progress: Some(35.0),
            },
            35.0,
            None,
        None));
        let parent_status = item(&mut queue).status.clone();

        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Encode FLAC · 18%", 68.0),
            68.0,
            Some(0),
        Some(1)));

        let item = item(&mut queue);
        assert_eq!(item.status, parent_status);
        assert!(item.active_tracks.contains_key(&0));
    }

    #[test]
    fn late_track_scoped_processing_after_item_terminal_is_ignored() {
        let mut queue = queue_with_item();
        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Encode FLAC · 18%", 18.0),
            18.0,
            Some(0),
        Some(1)));
        assert!(item(&mut queue).active_tracks.contains_key(&0));

        assert!(queue.update_item_status(
            "item-1",
            ConversionStatus::Completed {
                output_path: std::path::PathBuf::from("/tmp/out.flac"),
                log_path: None,
            },
            100.0,
            None,
        None));
        let terminal_status = item(&mut queue).status.clone();
        assert!(item(&mut queue).active_tracks.is_empty());

        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Encode FLAC · 31%", 31.0),
            31.0,
            Some(0),
        Some(1)));

        let item = item(&mut queue);
        assert_eq!(item.status, terminal_status);
        assert!(item.active_tracks.is_empty());
    }

    #[test]
    fn queue_public_update_path_matches_canonical_helper() {
        let mut queue = queue_with_item();
        let mut direct_item = ConversionItem::default();
        direct_item.id = "item-1".to_string();

        let status = processing_status("track 2 (Yesternow) · Convert DSD to PCM · 7%", 21.0);
        assert!(queue.update_item_status("item-1", status.clone(), 21.0, Some(1), Some(2)));
        apply_item_status_update(&mut direct_item, status, 21.0, Some(1), Some(2));

        let queued_item = item(&mut queue);
        assert_eq!(queued_item.status, direct_item.status);
        assert_eq!(queued_item.active_tracks, direct_item.active_tracks);

        let terminal = ConversionStatus::Failed {
            error: "track encode failed".to_string(),
            log_path: None,
        };
        assert!(queue.update_item_status("item-1", terminal.clone(), 21.0, Some(1), Some(2)));
        apply_item_status_update(&mut direct_item, terminal, 21.0, Some(1), Some(2));

        let queued_item = item(&mut queue);
        assert_eq!(queued_item.status, direct_item.status);
        assert_eq!(queued_item.active_tracks, direct_item.active_tracks);
    }

    #[test]
    fn track_scoped_terminal_update_clears_only_that_track() {
        let mut queue = queue_with_item();
        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Convert DSD to PCM · 14%", 20.0),
            20.0,
            Some(0),
        Some(1)));
        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 2 (Yesternow) · Convert DSD to PCM · 7%", 21.0),
            21.0,
            Some(1),
        Some(2)));

        assert!(queue.update_item_status(
            "item-1",
            ConversionStatus::Failed {
                error: "track encode failed".to_string(),
                log_path: None,
            },
            21.0,
            Some(0),
        Some(1)));

        let item = item(&mut queue);
        assert!(!item.active_tracks.contains_key(&0));
        assert!(item.active_tracks.contains_key(&1));
        assert!(matches!(&item.status, ConversionStatus::NotConfigured));
    }

    #[test]
    fn track_scoped_processing_at_item_completion_removes_that_track() {
        let mut queue = queue_with_item();
        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Encode FLAC · 98%", 98.0),
            98.0,
            Some(0),
        Some(1)));
        assert!(item(&mut queue).active_tracks.contains_key(&0));

        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Encode FLAC · 100%", 100.0),
            100.0,
            Some(0),
        Some(1)));

        let item = item(&mut queue);
        assert!(!item.active_tracks.contains_key(&0));
        assert!(matches!(
            &item.status,
            ConversionStatus::NotConfigured
        ), "track-scoped completion must not mutate parent status");
    }

    #[test]
    fn clear_track_progress_removes_only_that_track_without_changing_item_status() {
        let mut queue = queue_with_item();
        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Convert DSD to PCM · 14%", 20.0),
            20.0,
            Some(0),
        Some(1)));
        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 2 (Yesternow) · Convert DSD to PCM · 7%", 21.0),
            21.0,
            Some(1),
        Some(2)));
        let status_before_clear = item(&mut queue).status.clone();

        assert!(queue.clear_track_progress("item-1", 0, 1));

        let item = item(&mut queue);
        assert!(!item.active_tracks.contains_key(&0));
        assert!(item.active_tracks.contains_key(&1));
        assert_eq!(item.status, status_before_clear);
    }


    #[test]
    fn stale_track_progress_after_reliable_clear_is_ignored_by_epoch() {
        let mut queue = queue_with_item();
        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Convert DSD to PCM · 14%", 20.0),
            20.0,
            Some(0),
            Some(11),
        ));
        assert!(item(&mut queue).active_tracks.contains_key(&0));

        assert!(queue.clear_track_progress("item-1", 0, 11));
        assert!(item(&mut queue).active_tracks.is_empty());

        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Convert DSD to PCM · 31%", 31.0),
            31.0,
            Some(0),
            Some(11),
        ));
        assert!(item(&mut queue).active_tracks.is_empty());

        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Encode FLAC · 2%", 2.0),
            2.0,
            Some(0),
            Some(12),
        ));
        let item = item(&mut queue);
        assert!(item.active_tracks.contains_key(&0));
        assert_eq!(item.active_tracks.get(&0).unwrap().lifecycle_epoch, 12);
    }


    #[test]
    fn old_clear_event_cannot_remove_newer_track_epoch() {
        let mut queue = queue_with_item();
        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Encode FLAC · 2%", 2.0),
            2.0,
            Some(0),
            Some(12),
        ));

        assert!(queue.clear_track_progress("item-1", 0, 11));

        let item = item(&mut queue);
        assert!(item.active_tracks.contains_key(&0));
        assert_eq!(item.active_tracks.get(&0).unwrap().lifecycle_epoch, 12);
    }

    #[test]
    fn older_track_progress_cannot_overwrite_newer_active_epoch() {
        let mut queue = queue_with_item();

        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Encode FLAC · 2%", 2.0),
            2.0,
            Some(0),
            Some(12),
        ));

        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Convert DSD to PCM · 31%", 31.0),
            31.0,
            Some(0),
            Some(11),
        ));

        let item = item(&mut queue);
        let active = item.active_tracks.get(&0).expect("newer active epoch remains");
        assert_eq!(active.lifecycle_epoch, 12);
        assert_eq!(active.step_description, "Encode FLAC");
    }

    #[test]
    fn item_scoped_processing_does_not_clear_active_tracks() {
        let mut queue = queue_with_item();
        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Convert DSD to PCM · 14%", 20.0),
            20.0,
            Some(0),
        Some(1)));
        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 2 (Yesternow) · Convert DSD to PCM · 7%", 21.0),
            21.0,
            Some(1),
        Some(2)));

        assert!(queue.update_item_status(
            "item-1",
            ConversionStatus::Processing {
                progress: 30.0,
                message: Some("Preparing album post-processing".to_string()),
                file_progress: None,
                phase: Some(ConversionPhase::PostProcessing),
                phase_progress: Some(0.0),
            },
            30.0,
            None,
        None));

        let item = item(&mut queue);
        assert!(item.active_tracks.contains_key(&0));
        assert!(item.active_tracks.contains_key(&1));
        assert!(matches!(
            &item.status,
            ConversionStatus::Processing {
                message: Some(message),
                phase: Some(ConversionPhase::PostProcessing),
                ..
            } if message.as_str() == "Preparing album post-processing"
        ));
    }

    #[test]
    fn item_scoped_terminal_update_clears_all_tracks_and_finalizes_item() {
        let mut queue = queue_with_item();
        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 1 (Right Off) · Convert DSD to PCM · 14%", 20.0),
            20.0,
            Some(0),
        Some(1)));
        assert!(queue.update_item_status(
            "item-1",
            processing_status("track 2 (Yesternow) · Convert DSD to PCM · 7%", 21.0),
            21.0,
            Some(1),
        Some(2)));

        assert!(queue.update_item_status("item-1", ConversionStatus::Cancelled, 21.0, None, None));

        let item = item(&mut queue);
        assert!(item.active_tracks.is_empty());
        assert!(matches!(item.status, ConversionStatus::Cancelled));
    }

    #[test]
    fn active_tracks_are_skipped_by_serde() {
        let mut item = ConversionItem::default();
        item.id = "item-1".to_string();
        item.active_tracks.insert(
            0,
            TrackProgress {
                track_label: "track 1 (Right Off)".to_string(),
                step_description: "Convert DSD to PCM".to_string(),
                progress_fraction: 0.14,
                lifecycle_epoch: 1,
            },
        );

        let encoded = serde_json::to_string(&item).expect("ConversionItem serializes");
        assert!(!encoded.contains("active_tracks"));
        assert!(!encoded.contains("closed_track_epochs"));
        assert!(!encoded.contains("Right Off"));

        let decoded: ConversionItem = serde_json::from_str(&encoded)
            .expect("ConversionItem deserializes");
        assert!(decoded.active_tracks.is_empty());
    }
}
