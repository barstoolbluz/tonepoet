//! Audio conversion functionality for tonepoet
//!
//! This module provides comprehensive audio format conversion with support for:
//! - Multiple input/output formats (FLAC, WAV, AIFF, WavPack, MP3, AAC, Opus)
//! - Parallel processing with progress tracking
//! - Metadata preservation and ReplayGain calculation
//! - 7z archive extraction and processing

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

pub mod classify;
pub mod cap_fs;
pub mod cue_parser;
pub mod formats;
pub mod queue_expansion;
pub mod sacd;
pub mod script_supervisor;
pub mod labels;
pub mod metadata;
pub mod pipeline;
pub mod processor;
pub mod queue;
pub mod rename_plan;
pub mod renaming;
pub mod simple_wizard;
pub mod wizard;
pub mod wizard_integration;

pub use formats::{
    AacProfile, AudioFormat, ConversionOptions, FileFormat, FormatDetector, Mp3BitrateMode,
    QualitySettings, WavPackMode,
};
pub use classify::{classify_file, EntryKind};
pub use cue_parser::{parse_cue, parse_cue_file, CueSheet, CueTrack};
pub use queue_expansion::{
    count_audio_files_bounded, cue_sidecar_override_for_commit_path, expand_paths_to_audio,
    expand_paths_to_audio_with_metadata, expand_paths_to_audio_with_metadata_limited,
    QueueExpansionLimitedError, QueueExpansionResult,
};
pub use labels::{detect_pressing_info, LabelInfo};
pub use metadata::{extract_metadata_from_flac, extract_year_from_flac_files, FlacMetadata};
pub use processor::{process_item, ConversionProcessor, LifecycleEvent, ProcessorConfig, ProgressUpdate};
pub use queue::{ConversionItem, ConversionPhase, ConversionQueue, ConversionStatus};
pub use renaming::{
    apply_all_tags, apply_folder_renaming, rename_audio_files, update_album_tags, update_title_tags,
};
pub use wizard::{ConversionWizard, WizardStep};
pub use wizard_integration::{
    apply_settings_to_queue, extract_wizard_settings, validate_conversion_ready,
    validate_wizard_selections,
};

/// Tagging module alias for backward compatibility
pub mod tagging {
    pub use super::renaming::{apply_all_tags, update_album_tags, update_title_tags};
}

/// Result type for conversion operations
pub type ConversionResult<T> = Result<T, ConversionError>;

/// Errors that can occur during conversion
#[derive(Debug, thiserror::Error)]
pub enum ConversionError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Format not supported: {0}")]
    UnsupportedFormat(String),

    #[error("Conversion failed: {0}")]
    ConversionFailed(String),

    #[error("Metadata error: {0}")]
    MetadataError(String),

    #[error("External tool error: {0}")]
    ToolError(String),

    #[error("Validation error: {0}")]
    ValidationError(String),
}

/// Main conversion manager
pub struct ConversionManager {
    pub queue: Arc<RwLock<ConversionQueue>>,
    pub processor: ConversionProcessor,
    pub config: ConversionConfig,
    paused: bool,
    stop_requested: bool,
    /// Cancellation token for the active conversion run. Triggered by
    /// `stop_all_conversions()` to kill in-flight child processes.
    cancel_token: tokio_util::sync::CancellationToken,
}

/// Configuration for the conversion system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionConfig {
    /// Default output format
    pub default_format: AudioFormat,

    /// Default conversion options
    pub default_options: ConversionOptions,

    /// Number of parallel workers
    pub worker_count: usize,

    /// Output directory template
    pub output_template: String,

    /// Whether to preserve folder structure
    pub preserve_structure: bool,

    /// Calculate ReplayGain by default
    pub calculate_replaygain: bool,

    /// Tool paths (if not in PATH)
    pub tool_paths: HashMap<String, PathBuf>,
}

impl Default for ConversionConfig {
    fn default() -> Self {
        Self {
            default_format: AudioFormat::Flac,
            default_options: ConversionOptions::default(),
            worker_count: num_cpus::get().saturating_sub(1).max(1),
            output_template: "{artist}/{album}/{track} - {title}".to_string(),
            preserve_structure: true,
            calculate_replaygain: true,
            tool_paths: HashMap::new(),
        }
    }
}


pub(crate) fn queue_identity_path(path: &Path) -> PathBuf {
    let identity = if path.is_dir() {
        crate::disc::bluray_utils::bluray_source_path_for_backend(path)
            .ok()
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    };

    identity.canonicalize().unwrap_or(identity)
}

fn same_path_for_queue(left: &Path, right: &Path) -> bool {
    queue_identity_path(left) == queue_identity_path(right)
}

fn validate_pipeline_request_item_id(
    request: &crate::convert::pipeline::PipelineRequest,
) -> ConversionResult<()> {
    if !request.item_id.trim().is_empty() {
        return Ok(());
    }

    Err(ConversionError::ConversionFailed(
        "prebuilt PipelineRequest item_id must be non-empty for ready queue insertion".to_string(),
    ))
}

fn validate_pipeline_request_container_matches_path(
    path: &Path,
    request: &crate::convert::pipeline::PipelineRequest,
) -> ConversionResult<()> {
    if same_path_for_queue(path, &request.container) {
        return Ok(());
    }

    Err(ConversionError::ConversionFailed(format!(
        "prebuilt PipelineRequest container '{}' does not match ready queue path '{}'",
        request.container.display(),
        path.display()
    )))
}

fn validate_pipeline_request_queue_metadata(
    request: &crate::convert::pipeline::PipelineRequest,
    archive_password: Option<&str>,
    cue_sidecar_override: Option<crate::convert::pipeline::CueSidecarPolicy>,
) -> ConversionResult<()> {
    let request_archive_password = request
        .source
        .archive_password
        .as_ref()
        .map(|password| password.expose());

    if let Some(queue_archive_password) = archive_password {
        if request_archive_password != Some(queue_archive_password) {
            return Err(ConversionError::ConversionFailed(
                "prebuilt PipelineRequest is the executable contract; queue archive_password must be omitted or match request.source.archive_password".to_string(),
            ));
        }
    }

    if let Some(queue_cue_policy) = cue_sidecar_override {
        if queue_cue_policy != request.source.cue_sidecar {
            return Err(ConversionError::ConversionFailed(
                "prebuilt PipelineRequest is the executable contract; queue CUE sidecar override must be omitted or match request.source.cue_sidecar".to_string(),
            ));
        }
    }

    Ok(())
}

fn _status_progress_for_update(status: &ConversionStatus, progress_hint: f32) -> f32 {
    match status {
        ConversionStatus::Processing { progress, .. } => *progress,
        ConversionStatus::Completed { .. }
        | ConversionStatus::CompletedWithActionErrors { .. }
        | ConversionStatus::Partial { .. } => 100.0,
        ConversionStatus::Failed { .. } | ConversionStatus::Cancelled => {
            progress_hint.clamp(0.0, 100.0)
        }
        ConversionStatus::Queued | ConversionStatus::Paused | ConversionStatus::NotConfigured => {
            0.0
        }
    }
}

impl ConversionManager {
    /// Create a new conversion manager
    pub fn new(config: ConversionConfig) -> Self {
        let processor = ConversionProcessor::new(ProcessorConfig {
            worker_count: config.worker_count,
            tool_paths: config.tool_paths.clone(),
            default_destination_directory: None,
            scratch_directory: None,
            scratch_memory_limit_percent: crate::config::DEFAULT_SCRATCH_MEMORY_LIMIT_PERCENT,
        });

        Self {
            queue: Arc::new(RwLock::new(ConversionQueue::new())),
            processor,
            config,
            paused: false,
            stop_requested: false,
            cancel_token: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// Add files to the conversion queue
    pub async fn add_files(
        &mut self,
        files: Vec<PathBuf>,
        options: ConversionOptions,
    ) -> ConversionResult<()> {
        let mut queue = self.queue.write().await;
        for file in files {
            let format = FormatDetector::detect(&file)?;
            queue.add_item(file, format, options.clone());
        }
        Ok(())
    }

    /// Add a directory to the conversion queue
    pub async fn add_directory(
        &mut self,
        dir: &Path,
        options: ConversionOptions,
    ) -> ConversionResult<()> {
        let files = self.scan_directory(dir)?;
        self.add_files(files, options).await
    }

    /// Scan a directory for queueable conversion inputs.
    ///
    /// Keep directory admission as a thin compatibility adapter over the
    /// canonical conversion-domain queue expansion planner. This avoids a
    /// second CUE/queue implementation inside `convert::mod` and keeps CLI,
    /// TUI, and manager directory scans on one set of semantics.
    fn scan_directory(&self, dir: &Path) -> ConversionResult<Vec<PathBuf>> {
        if !dir.is_dir() {
            return Err(ConversionError::ValidationError(format!(
                "not a directory: {}",
                dir.display()
            )));
        }

        Ok(crate::convert::queue_expansion::expand_paths_to_audio(&[
            dir.to_path_buf(),
        ]))
    }

    /// Start processing the conversion queue
    pub async fn start_processing(&mut self) -> ConversionResult<()> {
        // This method is deprecated - use the new shared queue approach in main.rs
        let mut queue = self.queue.write().await;
        self.processor.process_queue(&mut *queue).await
    }

    /// Get current progress
    pub fn get_progress(&self) -> ConversionProgress {
        // Since this is called from UI thread and we need non-blocking access,
        // we'll use try_read() and return defaults if busy
        if let Ok(queue) = self.queue.try_read() {
            let total = queue.total_items();
            let completed = queue.completed_items();
            let failed = queue.failed_items();
            let current = queue.current_item();

            ConversionProgress {
                total_files: total,
                completed_files: completed,
                failed_files: failed,
                current_file: current.map(|item| item.input_path.display().to_string()),
                overall_progress: if total > 0 {
                    (completed as f32 / total as f32) * 100.0
                } else {
                    0.0
                },
            }
        } else {
            // Queue is busy - return last known state or defaults
            ConversionProgress {
                total_files: 0,
                completed_files: 0,
                failed_files: 0,
                current_file: None,
                overall_progress: 0.0,
            }
        }
    }

    /// Add a single file synchronously (blocking) - for UI compatibility
    pub fn add_file_blocking(
        &mut self,
        file: std::path::PathBuf,
        options: ConversionOptions,
    ) -> ConversionResult<()> {
        let format = FormatDetector::detect(&file)?;
        // Use try_write() instead of blocking_write() to avoid panic in async context
        let mut queue = self.queue.try_write().map_err(|_| {
            ConversionError::ConversionFailed("Queue is busy, try again".to_string())
        })?;
        queue.add_item(file, format, options);
        Ok(())
    }

    /// Add a file that's already configured and ready for processing
    /// Used for download+convert workflows and presets where settings are pre-configured
    pub fn add_file_ready_for_processing(
        &mut self,
        file: std::path::PathBuf,
        options: ConversionOptions,
        archive_password: Option<String>,
    ) -> ConversionResult<String> {
        self.add_file_ready_for_processing_with_cue_sidecar_override(
            file,
            options,
            archive_password,
            None,
        )
    }

    /// Add a file that is already configured and ready for processing,
    /// optionally attaching a queue-time CUE sidecar policy override.
    ///
    /// The override is written to the `ConversionItem` before insertion into
    /// the shared queue. Callers use this when browse queue expansion already
    /// evaluated a sibling CUE and classified it as a metadata artifact, so the
    /// downstream materializer must not rediscover that sidecar later.
    pub fn add_file_ready_for_processing_with_cue_sidecar_override(
        &mut self,
        file: std::path::PathBuf,
        options: ConversionOptions,
        archive_password: Option<String>,
        cue_sidecar_override: Option<crate::convert::pipeline::CueSidecarPolicy>,
    ) -> ConversionResult<String> {
        let format = FormatDetector::detect(&file)?;

        // Create item and mark as Queued (ready for processing). A runnable
        // queue item must carry the exact pipeline handoff now rather than
        // relying on later best-effort projection from legacy options. Keep
        // NotConfigured insertion paths unchanged; this guard only applies to
        // the ready-to-process admission helper.
        let mut item = ConversionItem::new_with_cue_sidecar_override(
            file.clone(),
            format,
            options,
            cue_sidecar_override,
        );
        item.archive_password = archive_password;
        item.status = ConversionStatus::Queued;
        if item.pipeline_settings.is_none() && item.options.pipeline_settings.is_none() {
            return Err(ConversionError::ConversionFailed(
                "ready queue insertion requires exact PipelineSettings".to_string(),
            ));
        }

        // Use try_write() instead of blocking_write() to avoid panic in async context.
        let mut queue = self.queue.try_write().map_err(|_| {
            ConversionError::ConversionFailed("Queue is busy, try again".to_string())
        })?;

        let id = item.id.clone();
        queue.items_mut().push_back(item);

        log::info!("Added file ready for processing: {:?} (id: {})", file, id);
        Ok(id)
    }

    /// Add a file that is already configured and ready for processing with a
    /// prebuilt orchestrator request.
    ///
    /// This is the explicit ready-queue handoff path for callers that already
    /// own a complete `PipelineRequest`. The request `item_id` must be non-empty
    /// and unique in the queue, and its `container` must identify the same
    /// source as `file` after queue path normalization. Because the request is
    /// the executable contract, any out-of-band queue archive password or CUE
    /// sidecar override must be omitted or match the request source options.
    pub fn add_file_ready_for_processing_with_pipeline_request(
        &mut self,
        file: std::path::PathBuf,
        options: ConversionOptions,
        request: crate::convert::pipeline::PipelineRequest,
        archive_password: Option<String>,
    ) -> ConversionResult<String> {
        self.add_file_ready_for_processing_with_pipeline_request_and_cue_sidecar_override(
            file,
            options,
            request,
            archive_password,
            None,
        )
    }

    /// Add a ready-to-run file with a prebuilt `PipelineRequest` and an optional
    /// queue-time CUE sidecar policy override.
    pub fn add_file_ready_for_processing_with_pipeline_request_and_cue_sidecar_override(
        &mut self,
        file: std::path::PathBuf,
        options: ConversionOptions,
        request: crate::convert::pipeline::PipelineRequest,
        archive_password: Option<String>,
        cue_sidecar_override: Option<crate::convert::pipeline::CueSidecarPolicy>,
    ) -> ConversionResult<String> {
        let format = FormatDetector::detect(&file)?;
        validate_pipeline_request_item_id(&request)?;
        validate_pipeline_request_container_matches_path(&file, &request)?;
        validate_pipeline_request_queue_metadata(
            &request,
            archive_password.as_deref(),
            cue_sidecar_override,
        )?;
        let request_item_id = request.item_id.clone();

        let mut item = ConversionItem::new_with_pipeline_request(
            file.clone(),
            format,
            options,
            request,
            cue_sidecar_override,
        );
        item.archive_password = archive_password;
        item.status = ConversionStatus::Queued;

        if item.pipeline_request.is_none() {
            return Err(ConversionError::ConversionFailed(
                "ready queue insertion requires a prebuilt PipelineRequest".to_string(),
            ));
        }

        // Use try_write() instead of blocking_write() to avoid panic in async context.
        let mut queue = self.queue.try_write().map_err(|_| {
            ConversionError::ConversionFailed("Queue is busy, try again".to_string())
        })?;
        if queue
            .all_items()
            .into_iter()
            .any(|existing| existing.id.as_str() == request_item_id.as_str())
        {
            return Err(ConversionError::ConversionFailed(format!(
                "prebuilt PipelineRequest item_id '{request_item_id}' already exists in the conversion queue"
            )));
        }

        let id = item.id.clone();
        queue.items_mut().push_back(item);

        log::info!(
            "Added file ready for processing with prebuilt PipelineRequest: {:?} (id: {})",
            file,
            id
        );
        Ok(id)
    }

    /// Get queue size blocking - for UI compatibility
    pub fn queue_size(&self) -> usize {
        if let Ok(queue) = self.queue.try_read() {
            queue.total_items()
        } else {
            0
        }
    }

    /// Get count of active items (excluding completed/failed) - for routing logic
    pub fn active_items_count(&self) -> usize {
        if let Ok(queue) = self.queue.try_read() {
            queue.active_items_count()
        } else {
            0
        }
    }

    /// Clear queue blocking - for UI compatibility
    pub fn clear_queue(&mut self) {
        if let Ok(mut queue) = self.queue.try_write() {
            queue.clear();
        }
    }

    fn record_closed_track_epoch(item: &mut ConversionItem, track_index: u32, epoch: u64) {
        item.closed_track_epochs
            .entry(track_index)
            .and_modify(|closed| *closed = (*closed).max(epoch))
            .or_insert(epoch);
    }

    fn record_and_clear_active_tracks(item: &mut ConversionItem) {
        let epochs: Vec<(u32, u64)> = item
            .active_tracks
            .iter()
            .map(|(track_index, progress)| (*track_index, progress.epoch))
            .collect();
        for (track_index, epoch) in epochs {
            Self::record_closed_track_epoch(item, track_index, epoch);
        }
        item.active_tracks.clear();
    }

    /// Update item status by ID
    pub fn update_item_status(&self, id: &str, status: ConversionStatus, _progress: f32) -> bool {
        if let Ok(mut queue) = self.queue.try_write() {
            if let Some(item) = queue.find_item_mut(id) {
                // Clear per-track sub-lines when the item-level phase moves
                // past Converting (e.g. into Tagging, PostProcessing, Finalizing).
                if !item.active_tracks.is_empty() {
                    let new_phase = match &status {
                        ConversionStatus::Processing { phase, .. } => *phase,
                        _ => None,
                    };
                    let dominated = matches!(
                        new_phase,
                        Some(crate::convert::ConversionPhase::Tagging)
                            | Some(crate::convert::ConversionPhase::PostProcessing)
                            | Some(crate::convert::ConversionPhase::Finalizing)
                    ) || !matches!(&status, ConversionStatus::Processing { .. });
                    if dominated {
                        Self::record_and_clear_active_tracks(item);
                    }
                }

                item.status = status.clone();

                // Update timestamps based on status
                match &status {
                    ConversionStatus::Processing { .. } if item.started_at.is_none() => {
                        item.started_at = Some(chrono::Utc::now());
                    }
                    ConversionStatus::Completed { .. }
                    | ConversionStatus::CompletedWithActionErrors { .. }
                    | ConversionStatus::Partial { .. }
                    | ConversionStatus::Failed { .. }
                    | ConversionStatus::Cancelled => {
                        Self::record_and_clear_active_tracks(item);
                        item.closed_track_epochs.clear();
                        item.completed_at = Some(chrono::Utc::now());
                        match &status {
                            ConversionStatus::Completed { output_path, .. }
                            | ConversionStatus::CompletedWithActionErrors { output_path, .. }
                            | ConversionStatus::Partial { output_path, .. } => {
                                item.output_path = Some(output_path.clone());
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                return true;
            }
        }
        false
    }

    /// Update per-track progress for a multi-track source. Track-scoped
    /// updates affect display sub-lines AND advance the item's progress
    /// bar (so it doesn't freeze during encoding). They never touch item
    /// status text, timestamps, or output paths.
    pub fn update_track_progress(
        &self,
        id: &str,
        track_index: u32,
        status: &ConversionStatus,
        progress: f32,
        track_epoch: Option<u64>,
    ) {
        let Some(incoming_epoch) = track_epoch else {
            return;
        };

        if let Ok(mut queue) = self.queue.try_write() {
            if let Some(item) = queue.find_item_mut(id) {
                if matches!(
                    item.closed_track_epochs.get(&track_index),
                    Some(closed_epoch) if *closed_epoch >= incoming_epoch
                ) {
                    return;
                }
                if matches!(
                    item.active_tracks.get(&track_index),
                    Some(active) if active.epoch > incoming_epoch
                ) {
                    return;
                }

                if let ConversionStatus::Processing { message, phase, phase_progress, .. } = status {
                    // Track rows are only valid while the parent item can still be in
                    // track conversion. Epoch checks cover known cleared generations;
                    // this state gate also rejects delayed ticks when no prior row
                    // existed from which a closed epoch could be recorded.
                    if matches!(
                        &item.status,
                        ConversionStatus::Processing {
                            phase: Some(
                                ConversionPhase::Tagging
                                    | ConversionPhase::PostProcessing
                                    | ConversionPhase::Finalizing,
                            ),
                            ..
                        } | ConversionStatus::Completed { .. }
                            | ConversionStatus::CompletedWithActionErrors { .. }
                            | ConversionStatus::Partial { .. }
                            | ConversionStatus::Failed { .. }
                            | ConversionStatus::Cancelled
                    ) {
                        return;
                    }

                    let msg = message.as_deref().unwrap_or("");
                    let (label, step, tool_pct) = parse_track_message(msg);
                    item.active_tracks.insert(track_index, crate::convert::queue::TrackProgress {
                        track_label: label,
                        step_description: step,
                        progress_fraction: tool_pct,
                        epoch: incoming_epoch,
                    });
                    // Advance the item progress bar so it doesn't freeze during encoding.
                    // Use the overall windowed progress which advances monotonically.
                    if let ConversionStatus::Processing {
                        progress: ref mut item_progress,
                        phase: ref mut item_phase,
                        phase_progress: ref mut item_phase_progress,
                        ..
                    } = item.status
                    {
                        if progress > *item_progress {
                            *item_progress = progress;
                        }
                        *item_phase = *phase;
                        *item_phase_progress = *phase_progress;
                    }
                } else {
                    let cleared_epoch = item
                        .active_tracks
                        .get(&track_index)
                        .map(|active| active.epoch.max(incoming_epoch))
                        .unwrap_or(incoming_epoch);
                    Self::record_closed_track_epoch(item, track_index, cleared_epoch);
                    item.active_tracks.remove(&track_index);
                }
            }
        }
    }

    pub fn clear_track_progress(&self, id: &str, track_index: u32, track_epoch: u64) {
        if let Ok(mut queue) = self.queue.try_write() {
            if let Some(item) = queue.find_item_mut(id) {
                if matches!(
                    &item.status,
                    ConversionStatus::Completed { .. }
                        | ConversionStatus::CompletedWithActionErrors { .. }
                        | ConversionStatus::Partial { .. }
                        | ConversionStatus::Failed { .. }
                        | ConversionStatus::Cancelled
                ) {
                    return;
                }
                if matches!(
                    item.active_tracks.get(&track_index),
                    Some(active) if active.epoch > track_epoch
                ) {
                    return;
                }
                Self::record_closed_track_epoch(item, track_index, track_epoch);
                item.active_tracks.remove(&track_index);
            }
        }
    }

    /// Toggle the collapsed/expanded state for per-track sub-lines.
    pub fn toggle_track_collapse(&self, id: &str) {
        if let Ok(mut queue) = self.queue.try_write() {
            if let Some(item) = queue.find_item_mut(id) {
                item.tracks_collapsed = !item.tracks_collapsed;
            }
        }
    }

    /// Get a cancellation token for a new conversion run. Each call
    /// replaces the stored token so the previous run's token becomes inert.
    pub fn conversion_cancel_token(&mut self) -> tokio_util::sync::CancellationToken {
        self.cancel_token = tokio_util::sync::CancellationToken::new();
        self.stop_requested = false;
        self.cancel_token.clone()
    }

    /// Stop all conversions by cancelling the active token and marking
    /// queued items. The cancellation propagates through the worker pool
    /// to kill in-flight SoX/ffmpeg child processes.
    pub fn stop_all_conversions(&mut self) {
        self.stop_requested = true;
        self.paused = false;
        self.cancel_token.cancel();
        // Cancel all queued items
        if let Ok(mut queue) = self.queue.try_write() {
            for item in queue.all_items_mut() {
                match &item.status {
                    ConversionStatus::Queued | ConversionStatus::Processing { .. } => {
                        item.status = ConversionStatus::Cancelled;
                    }
                    _ => {}
                }
            }
        }
    }

    /// Pause conversions
    pub fn pause_conversions(&mut self) {
        self.paused = true;
        // Mark queued items as paused
        if let Ok(mut queue) = self.queue.try_write() {
            for item in queue.all_items_mut() {
                if item.status == ConversionStatus::Queued {
                    item.status = ConversionStatus::Paused;
                }
            }
        }
    }

    /// Resume conversions  
    pub fn resume_conversions(&mut self) {
        self.paused = false;
        // Mark paused items as queued
        if let Ok(mut queue) = self.queue.try_write() {
            for item in queue.all_items_mut() {
                if item.status == ConversionStatus::Paused {
                    item.status = ConversionStatus::Queued;
                }
            }
        }
    }

    /// Check if conversions are paused
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Check if stop has been requested
    pub fn is_stop_requested(&self) -> bool {
        self.stop_requested
    }

    /// Clear stop request flag (call after processing stops)
    pub fn clear_stop_request(&mut self) {
        self.stop_requested = false;
    }

    /// Select all items in the queue
    pub fn select_all(&mut self) {
        if let Ok(mut queue) = self.queue.try_write() {
            for item in queue.all_items_mut() {
                item.selected = true;
            }
        }
    }

    /// Clear all selections
    pub fn clear_selection(&mut self) {
        log::warn!(
            "⚠️  CLEARING ALL SELECTIONS IN CONVERT QUEUE - backtrace would be helpful here"
        );
        if let Ok(mut queue) = self.queue.try_write() {
            for item in queue.all_items_mut() {
                log::warn!("  Clearing selection on item {}", item.id);
                item.selected = false;
            }
        }
    }

    /// Invert selection
    pub fn invert_selection(&mut self) {
        if let Ok(mut queue) = self.queue.try_write() {
            for item in queue.all_items_mut() {
                item.selected = !item.selected;
            }
        }
    }

    /// Remove selected items from queue
    pub fn remove_selected(&mut self) -> usize {
        if let Ok(mut queue) = self.queue.try_write() {
            queue.remove_selected()
        } else {
            0
        }
    }

    /// Get all items as a cloned vector for UI display
    pub fn get_items_clone(&self) -> Vec<ConversionItem> {
        if let Ok(queue) = self.queue.try_read() {
            queue.all_items().iter().map(|&item| item.clone()).collect()
        } else {
            Vec::new()
        }
    }

    /// Clear completed items from queue
    pub fn clear_completed(&mut self) {
        if let Ok(mut queue) = self.queue.try_write() {
            queue.clear_completed();
        }
    }

    pub fn clear_finished(&mut self) {
        if let Ok(mut queue) = self.queue.try_write() {
            queue.clear_finished();
        }
    }

    pub fn clear_all(&mut self) {
        if let Ok(mut queue) = self.queue.try_write() {
            queue.clear();
        }
    }

    /// Get path to persisted conversion queue file
    fn queue_path() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tonepoet")
            .join("conversion_queue.json")
    }

    /// Load persisted queue from disk
    pub fn load_persisted_queue(&mut self) {
        let loaded_items = Self::load_queue();
        if !loaded_items.is_empty() {
            if let Ok(mut q) = self.queue.try_write() {
                for item in loaded_items {
                    q.items_mut().push_back(item);
                }
            }
        }
    }

    /// Load queue items from disk with path validation
    fn load_queue() -> Vec<ConversionItem> {
        let queue_path = Self::queue_path();

        // Return empty if file doesn't exist
        if !queue_path.exists() {
            return Vec::new();
        }

        // Read and parse JSON
        match std::fs::read_to_string(&queue_path) {
            Ok(content) => {
                match serde_json::from_str::<Vec<ConversionItem>>(&content) {
                    Ok(items) => {
                        // Validate paths for security (prevent path traversal attacks)
                        items
                            .into_iter()
                            .filter(|item| {
                                // Filter out items with suspicious paths
                                let path_str = item.input_path.to_string_lossy();
                                if path_str.contains("..") {
                                    log::warn!(
                                        "Filtered out queue item with suspicious path: {:?}",
                                        item.input_path
                                    );
                                    return false;
                                }

                                // Filter out items where file no longer exists
                                if !item.input_path.exists() {
                                    log::info!(
                                        "Filtered out queue item - file no longer exists: {:?}",
                                        item.input_path
                                    );
                                    return false;
                                }

                                true
                            })
                            .collect()
                    }
                    Err(e) => {
                        log::error!(
                            "Failed to parse conversion queue from {:?}: {}",
                            queue_path,
                            e
                        );
                        Vec::new()
                    }
                }
            }
            Err(e) => {
                log::error!(
                    "Failed to read conversion queue from {:?}: {}",
                    queue_path,
                    e
                );
                Vec::new()
            }
        }
    }

    /// Save queue to disk with atomic writes
    pub fn save_queue(&self, persist_enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
        // Don't save if persistence is disabled
        if !persist_enabled {
            return Ok(());
        }

        let queue_path = Self::queue_path();

        // Ensure parent directory exists
        if let Some(parent) = queue_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Collect items to save (filter by status)
        let items_to_save: Vec<ConversionItem> = if let Ok(queue) = self.queue.try_read() {
            queue
                .all_items()
                .iter()
                .filter(|item| {
                    // Save: NotConfigured, Queued, Paused, Completed, Failed
                    // Don't save: Processing, Cancelled
                    matches!(
                        item.status,
                        ConversionStatus::NotConfigured
                            | ConversionStatus::Queued
                            | ConversionStatus::Paused
                            | ConversionStatus::Completed { .. }
                            | ConversionStatus::CompletedWithActionErrors { .. }
                            | ConversionStatus::Failed { .. }
                    )
                })
                .map(|&item| item.clone())
                .collect()
        } else {
            return Err("Queue is busy".into());
        };

        // Serialize to JSON
        let json = serde_json::to_string_pretty(&items_to_save)?;

        // Atomic write: write to temp file, then rename
        let temp_path = queue_path.with_extension("json.tmp");
        std::fs::write(&temp_path, json)?;
        std::fs::rename(&temp_path, &queue_path)?;

        Ok(())
    }
}

/// Progress information for the conversion process
#[derive(Debug, Clone)]
pub struct ConversionProgress {
    pub total_files: usize,
    pub completed_files: usize,
    pub failed_files: usize,
    pub current_file: Option<String>,
    pub overall_progress: f32,
}

/// Parse a track progress message into (label, step_description, tool_pct).
///
/// Messages from track workers follow the pattern:
///   "{label} - step {n} of {m} - {description} · {pct}% of current track · elapsed 0:03"
///
/// `tool_pct` is the tool's own percentage (0.0–1.0), NOT the windowed item
/// progress. This comes from the "N% of current track" suffix that the SoX
/// probe injects.
fn parse_track_message(message: &str) -> (String, String, f32) {
    let mut tool_pct: f32 = 0.0;

    // Extract tool percentage from "· N% of current track" suffix.
    for segment in message.split(" · ") {
        let segment = segment.trim();
        if let Some(rest) = segment.strip_suffix("% of current track") {
            if let Ok(pct) = rest.trim().parse::<f32>() {
                tool_pct = (pct / 100.0).clamp(0.0, 1.0);
            }
        }
    }

    // Strip all "· ..." suffixes to get the core label.
    let core = message.split(" · ").next().unwrap_or(message);
    let core = core.strip_prefix("Starting ").unwrap_or(core);
    let core = core.strip_prefix("Finished ").unwrap_or(core);

    // Split on " - step N of M - " to get track label and step description.
    if let Some(step_pos) = core.find(" - step ") {
        let label = &core[..step_pos];
        let after_step = &core[step_pos + 3..]; // skip " - "
        if let Some(desc_pos) = after_step.find(" - ") {
            let desc = &after_step[desc_pos + 3..];
            return (label.to_string(), desc.to_string(), tool_pct);
        }
        return (label.to_string(), after_step.to_string(), tool_pct);
    }

    (core.to_string(), String::new(), tool_pct)
}

#[cfg(test)]
mod bluray_queue_admission_tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            let path = std::env::temp_dir().join(format!(
                "tonepoet-bluray-queue-{name}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn ready_options() -> ConversionOptions {
        let mut options = ConversionOptions::default();
        options.pipeline_settings = Some(tonepoet_pipeline::PipelineSettings::default());
        options
    }

    fn pipeline_request_for(path: &Path, item_id: &str) -> crate::convert::pipeline::PipelineRequest {
        use crate::convert::pipeline::{
            CueSidecarPolicy, DvdaDownmixPolicy, DvdaGroupSelection, FailurePolicy, LogPolicy,
            NamingCollisionPolicy, NamingPolicy, OverwritePolicy, PublishPolicy, SourceOptions,
            StagePolicy, StageRequirement, TrackSelection,
        };

        crate::convert::pipeline::PipelineRequest {
            actions: crate::convert::pipeline::ActionPipeline::default(),
            job_id: format!("job-{item_id}"),
            item_id: item_id.to_string(),
            container: path.to_path_buf(),
            source: SourceOptions {
                archive_password: None,
                sacd_area: None,
                dvda_group_selection: DvdaGroupSelection::Default,
                dvda_group: None,
                dvda_assume_decrypted: false,
                dvda_downmix_policy: DvdaDownmixPolicy::Auto,
                dvdv_vts: None,
                dvdv_title: None,
                dvdv_audio_stream: None,
                dvdv_angle: None,
                bluray_playlist: None,
                bluray_audio_pid: None,
                bluray_audio_stream: None,
                bluray_angle: None,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            settings: tonepoet_pipeline::PipelineSettings::default(),
            worker_count: None,
            scratch_staging: None,
            merge: false,
            output_root: path
                .parent()
                .map(|parent| parent.join("out"))
                .unwrap_or_else(|| PathBuf::from("out")),
            naming: NamingPolicy {
                template: "%NN% - %TITLE%".to_string(),
                folder_template: None,
                per_album_subdir: false,
                collision_policy: NamingCollisionPolicy::Fail,
            },
            publish: PublishPolicy {
                overwrite: OverwritePolicy::FailIfExists,
                same_filesystem_required: false,
                write_manifest: false,
            },
            log: LogPolicy {
                root: path
                    .parent()
                    .map(|parent| parent.join("logs"))
                    .unwrap_or_else(|| PathBuf::from("logs")),
                write_for_blocked: true,
                write_json_log: false,
                write_conversion_log: true,
            },
            stages: StagePolicy {
                metadata: StageRequirement::Enabled,
                replaygain: StageRequirement::Disabled,
                features: StageRequirement::Enabled,
                generate_cue: false,
            },
            failure_policy: FailurePolicy::AllowPartialAlbum,
            album_batch: None,
            album_batch_track: None,
            suppress_incremental_conversion_log_append: false,
            companion: Default::default(),
            pre_extracted_staging: None,
            archive_metadata_overrides: Vec::new(),
            metadata_overrides: Default::default(),
            batch_resolved_identity: None,
            expected_album_track_count: None,
            container_extension: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }

    fn write_minimal_bluray_layout(root: &Path) {
        let bdmv = root.join("BDMV");
        fs::create_dir_all(bdmv.join("PLAYLIST")).expect("create PLAYLIST");
        fs::create_dir_all(bdmv.join("STREAM")).expect("create STREAM");
        fs::write(bdmv.join("index.bdmv"), b"index").expect("write index.bdmv");
        fs::write(bdmv.join("MovieObject.bdmv"), b"movie").expect("write MovieObject.bdmv");
        fs::write(bdmv.join("PLAYLIST").join("00000.mpls"), b"playlist")
            .expect("write playlist");
        fs::write(bdmv.join("STREAM").join("00000.m2ts"), b"stream").expect("write stream");
    }

    fn queued_item<'a>(manager: &'a ConversionManager, item_id: &str) -> ConversionItem {
        let queue = manager.queue.try_read().expect("queue read lock");
        queue
            .all_items()
            .into_iter()
            .find(|item| item.id.as_str() == item_id)
            .cloned()
            .expect("queued item exists")
    }

    #[test]
    fn ready_queue_admission_accepts_bluray_disc_root_with_pipeline_settings() {
        let temp = TempDir::new("disc-root");
        write_minimal_bluray_layout(&temp.path);
        let mut manager = ConversionManager::new(ConversionConfig::default());

        let item_id = manager
            .add_file_ready_for_processing(temp.path.clone(), ready_options(), None)
            .expect("Blu-ray directory should pass queue admission");

        let item = queued_item(&manager, &item_id);
        assert_eq!(item.input_path.as_path(), temp.path.as_path());
        assert_eq!(item.input_format, FileFormat::Archive);
        assert!(matches!(&item.status, ConversionStatus::Queued));
        assert!(item.pipeline_settings.is_some());
        assert!(item.options.pipeline_settings.is_some());
    }

    #[test]
    fn ready_queue_admission_accepts_bdmv_directory_with_pipeline_settings() {
        let temp = TempDir::new("bdmv-dir");
        write_minimal_bluray_layout(&temp.path);
        let bdmv = temp.path.join("BDMV");
        let mut manager = ConversionManager::new(ConversionConfig::default());

        let item_id = manager
            .add_file_ready_for_processing(bdmv.clone(), ready_options(), None)
            .expect("BDMV directory should pass queue admission");

        let item = queued_item(&manager, &item_id);
        assert_eq!(item.input_path.as_path(), bdmv.as_path());
        assert_eq!(item.input_format, FileFormat::Archive);
        assert!(matches!(&item.status, ConversionStatus::Queued));
        assert!(item.pipeline_settings.is_some());
        assert!(item.options.pipeline_settings.is_some());
    }

    #[test]
    fn ready_queue_admission_rejects_missing_pipeline_handoff_without_inserting() {
        let temp = TempDir::new("missing-settings");
        let input = temp.path.join("track.flac");
        fs::write(&input, b"not real audio").expect("write input placeholder");
        let mut manager = ConversionManager::new(ConversionConfig::default());

        let err = manager
            .add_file_ready_for_processing(input, ConversionOptions::default(), None)
            .expect_err("ready insertion without settings must fail")
            .to_string();

        assert!(
            err.contains("ready queue insertion requires exact PipelineSettings")
                && !err.contains("PipelineRequest"),
            "unexpected error: {err}"
        );
        let queue = manager.queue.try_read().expect("queue read lock");
        assert_eq!(queue.total_items(), 0);
    }

    #[test]
    fn ready_queue_admission_with_pipeline_request_succeeds_without_pipeline_settings() {
        let temp = TempDir::new("request-ready");
        let input = temp.path.join("track.flac");
        fs::write(&input, b"not real audio").expect("write input placeholder");
        let mut manager = ConversionManager::new(ConversionConfig::default());
        let request = pipeline_request_for(&input, "request-item-1");

        let item_id = manager
            .add_file_ready_for_processing_with_pipeline_request(
                input.clone(),
                ConversionOptions::default(),
                request,
                None,
            )
            .expect("prebuilt PipelineRequest should pass ready admission");

        assert_eq!(item_id, "request-item-1");
        let item = queued_item(&manager, &item_id);
        assert_eq!(item.input_path.as_path(), input.as_path());
        assert!(matches!(&item.status, ConversionStatus::Queued));
        assert!(item.pipeline_request.is_some());
        assert!(item.pipeline_settings.is_none());
        assert!(item.options.pipeline_settings.is_none());
    }

    #[test]
    fn ready_queue_admission_with_pipeline_request_rejects_empty_item_id_without_inserting() {
        let temp = TempDir::new("request-empty-id");
        let input = temp.path.join("track.flac");
        fs::write(&input, b"not real audio").expect("write input placeholder");
        let mut manager = ConversionManager::new(ConversionConfig::default());
        let request = pipeline_request_for(&input, "");

        let err = manager
            .add_file_ready_for_processing_with_pipeline_request(
                input,
                ConversionOptions::default(),
                request,
                None,
            )
            .expect_err("empty PipelineRequest item_id must fail")
            .to_string();

        assert!(
            err.contains("prebuilt PipelineRequest item_id must be non-empty"),
            "unexpected error: {err}"
        );
        let queue = manager.queue.try_read().expect("queue read lock");
        assert_eq!(queue.total_items(), 0);
    }

    #[test]
    fn ready_queue_admission_with_pipeline_request_rejects_existing_item_id_without_inserting() {
        let temp = TempDir::new("request-id-collision");
        let first = temp.path.join("first.flac");
        let second = temp.path.join("second.flac");
        fs::write(&first, b"not real audio").expect("write first placeholder");
        fs::write(&second, b"not real audio").expect("write second placeholder");
        let mut manager = ConversionManager::new(ConversionConfig::default());

        manager
            .add_file_ready_for_processing_with_pipeline_request(
                first.clone(),
                ConversionOptions::default(),
                pipeline_request_for(&first, "duplicate-request-id"),
                None,
            )
            .expect("first request should insert");

        let err = manager
            .add_file_ready_for_processing_with_pipeline_request(
                second.clone(),
                ConversionOptions::default(),
                pipeline_request_for(&second, "duplicate-request-id"),
                None,
            )
            .expect_err("duplicate PipelineRequest item_id must fail")
            .to_string();

        assert!(
            err.contains("prebuilt PipelineRequest item_id 'duplicate-request-id' already exists"),
            "unexpected error: {err}"
        );
        let queue = manager.queue.try_read().expect("queue read lock");
        assert_eq!(queue.total_items(), 1);
    }

    #[test]
    fn ready_queue_admission_with_pipeline_request_rejects_archive_password_mismatch() {
        let temp = TempDir::new("request-password-mismatch");
        let input = temp.path.join("track.flac");
        fs::write(&input, b"not real audio").expect("write input placeholder");
        let mut manager = ConversionManager::new(ConversionConfig::default());
        let mut request = pipeline_request_for(&input, "request-password-mismatch");
        request.source.archive_password = Some(crate::convert::pipeline::SecretString::new(
            "request-secret",
        ));

        let err = manager
            .add_file_ready_for_processing_with_pipeline_request(
                input,
                ConversionOptions::default(),
                request,
                Some("queue-secret".to_string()),
            )
            .expect_err("queue password must not conflict with PipelineRequest")
            .to_string();

        assert!(
            err.contains(
                "queue archive_password must be omitted or match request.source.archive_password"
            ),
            "unexpected error: {err}"
        );
        let queue = manager.queue.try_read().expect("queue read lock");
        assert_eq!(queue.total_items(), 0);
    }

    #[test]
    fn ready_queue_admission_with_pipeline_request_rejects_cue_sidecar_override_mismatch() {
        let temp = TempDir::new("request-cue-mismatch");
        let input = temp.path.join("track.flac");
        fs::write(&input, b"not real audio").expect("write input placeholder");
        let mut manager = ConversionManager::new(ConversionConfig::default());
        let request = pipeline_request_for(&input, "request-cue-mismatch");

        let err = manager
            .add_file_ready_for_processing_with_pipeline_request_and_cue_sidecar_override(
                input,
                ConversionOptions::default(),
                request,
                None,
                Some(crate::convert::pipeline::CueSidecarPolicy::EmbeddedOnly),
            )
            .expect_err("queue CUE policy must not conflict with PipelineRequest")
            .to_string();

        assert!(
            err.contains(
                "queue CUE sidecar override must be omitted or match request.source.cue_sidecar"
            ),
            "unexpected error: {err}"
        );
        let queue = manager.queue.try_read().expect("queue read lock");
        assert_eq!(queue.total_items(), 0);
    }

    #[test]
    fn ready_queue_admission_with_pipeline_request_accepts_bluray_root_bdmv_identity() {
        let temp = TempDir::new("request-bdmv-identity");
        write_minimal_bluray_layout(&temp.path);
        let bdmv = temp.path.join("BDMV");
        let mut manager = ConversionManager::new(ConversionConfig::default());
        let request = pipeline_request_for(&temp.path, "request-bluray-identity");

        let item_id = manager
            .add_file_ready_for_processing_with_pipeline_request(
                bdmv.clone(),
                ConversionOptions::default(),
                request,
                None,
            )
            .expect("Blu-ray root and BDMV child should share queue identity");

        let item = queued_item(&manager, &item_id);
        assert_eq!(item.input_path.as_path(), bdmv.as_path());
        assert!(item.pipeline_request.is_some());
        assert!(item.pipeline_settings.is_none());
    }

    #[test]
    fn ready_queue_admission_with_pipeline_request_rejects_path_mismatch_without_inserting() {
        let temp = TempDir::new("request-mismatch");
        let input = temp.path.join("track.flac");
        let other = temp.path.join("other.flac");
        fs::write(&input, b"not real audio").expect("write input placeholder");
        fs::write(&other, b"not real audio").expect("write other placeholder");
        let mut manager = ConversionManager::new(ConversionConfig::default());
        let request = pipeline_request_for(&other, "request-mismatch");

        let err = manager
            .add_file_ready_for_processing_with_pipeline_request(
                input,
                ConversionOptions::default(),
                request,
                None,
            )
            .expect_err("mismatched request container must fail")
            .to_string();

        assert!(
            err.contains("prebuilt PipelineRequest container")
                && err.contains("does not match ready queue path"),
            "unexpected error: {err}"
        );
        let queue = manager.queue.try_read().expect("queue read lock");
        assert_eq!(queue.total_items(), 0);
    }

    #[test]
    fn ready_queue_admission_keeps_iso_extension_admission_and_settings() {
        let temp = TempDir::new("iso-admission");
        let iso = temp.path.join("candidate.iso");
        fs::write(&iso, b"not a real Blu-ray fixture").expect("write iso placeholder");
        let mut manager = ConversionManager::new(ConversionConfig::default());

        let item_id = manager
            .add_file_ready_for_processing(iso.clone(), ready_options(), None)
            .expect(".iso extension admission should remain queueable");

        let item = queued_item(&manager, &item_id);
        assert_eq!(item.input_path.as_path(), iso.as_path());
        assert_eq!(item.input_format, FileFormat::Archive);
        assert!(matches!(&item.status, ConversionStatus::Queued));
        assert!(item.pipeline_settings.is_some());
        assert!(item.options.pipeline_settings.is_some());
    }

    #[test]
    fn queue_identity_normalizes_bluray_root_and_bdmv_child_both_directions() {
        let temp = TempDir::new("identity");
        write_minimal_bluray_layout(&temp.path);
        let bdmv = temp.path.join("BDMV");

        assert_eq!(queue_identity_path(&temp.path), queue_identity_path(&bdmv));
        assert_eq!(queue_identity_path(&bdmv), queue_identity_path(&temp.path));
    }

    #[test]
    fn queue_identity_preserves_ordinary_canonical_path_behavior() {
        let temp = TempDir::new("ordinary-identity");
        let first = temp.path.join("one.flac");
        let second = temp.path.join("two.flac");
        fs::write(&first, b"one").expect("write first");
        fs::write(&second, b"two").expect("write second");

        assert_eq!(
            queue_identity_path(&first),
            queue_identity_path(&temp.path.join(".").join("one.flac"))
        );
        assert_ne!(queue_identity_path(&first), queue_identity_path(&second));
    }
}

#[cfg(test)]
mod per_track_epoch_tests {
    use super::*;
    use std::path::PathBuf;

    fn test_manager_with_item() -> (ConversionManager, String) {
        let manager = ConversionManager::new(ConversionConfig::default());
        let mut item = ConversionItem::default();
        item.id = "item-1".to_string();
        item.input_path = PathBuf::from("/tmp/input.flac");
        item.status = ConversionStatus::Processing {
            progress: 0.0,
            message: Some("Starting".to_string()),
            file_progress: None,
            phase: Some(ConversionPhase::Converting),
            phase_progress: Some(0.0),
        };
        manager
            .queue
            .try_write()
            .expect("queue write lock")
            .items_mut()
            .push_back(item);
        (manager, "item-1".to_string())
    }

    fn processing_status(message: &str, progress: f32, phase: ConversionPhase) -> ConversionStatus {
        ConversionStatus::Processing {
            progress,
            message: Some(message.to_string()),
            file_progress: None,
            phase: Some(phase),
            phase_progress: Some(progress),
        }
    }

    #[test]
    fn stale_track_epoch_is_rejected_after_newer_clear() {
        let (manager, item_id) = test_manager_with_item();
        let (progress_tx, _progress_rx) = tokio::sync::broadcast::channel(4);
        let old_reporter = crate::convert::pipeline::BroadcastReporter::new(
            progress_tx.clone(),
            None,
            item_id.clone(),
            Some(0),
        );
        let new_reporter = crate::convert::pipeline::BroadcastReporter::new(
            progress_tx,
            None,
            item_id.clone(),
            Some(0),
        );
        let old_epoch = old_reporter.track_epoch().expect("old reporter epoch");
        let new_epoch = new_reporter.track_epoch().expect("new reporter epoch");
        assert!(old_epoch < new_epoch);

        let old_update = processing_status(
            "Track 1 - step 1 of 2 - encoding · 10% of current track",
            25.0,
            ConversionPhase::Converting,
        );
        manager.update_track_progress(&item_id, 0, &old_update, 25.0, Some(old_epoch));
        manager.clear_track_progress(&item_id, 0, new_epoch);
        manager.update_track_progress(&item_id, 0, &old_update, 25.0, Some(old_epoch));

        let queue = manager.queue.try_read().expect("queue read lock");
        let item = queue
            .all_items()
            .into_iter()
            .find(|item| item.id == item_id)
            .expect("item exists");
        assert!(item.active_tracks.get(&0).is_none());
        assert_eq!(item.closed_track_epochs.get(&0), Some(&new_epoch));
    }

    #[test]
    fn item_terminal_status_clears_active_tracks_and_closed_epochs() {
        let (manager, item_id) = test_manager_with_item();
        {
            let mut queue = manager.queue.try_write().expect("queue write lock");
            let item = queue.find_item_mut(&item_id).expect("item exists");
            item.active_tracks.insert(
                0,
                crate::convert::queue::TrackProgress {
                    track_label: "Track 1".to_string(),
                    step_description: "encoding".to_string(),
                    progress_fraction: 0.5,
                    epoch: 7,
                },
            );
            item.closed_track_epochs.insert(0, 7);
        }

        assert!(manager.update_item_status(
            &item_id,
            ConversionStatus::Completed {
                output_path: PathBuf::from("/tmp/out.flac"),
                log_path: None,
            },
            100.0,
        ));

        let queue = manager.queue.try_read().expect("queue read lock");
        let item = queue
            .all_items()
            .into_iter()
            .find(|item| item.id == item_id)
            .expect("item exists");
        assert!(item.active_tracks.is_empty());
        assert!(item.closed_track_epochs.is_empty());
    }

    #[test]
    fn phase_change_backstop_clears_active_tracks_and_records_epochs() {
        let (manager, item_id) = test_manager_with_item();
        {
            let mut queue = manager.queue.try_write().expect("queue write lock");
            let item = queue.find_item_mut(&item_id).expect("item exists");
            item.active_tracks.insert(
                0,
                crate::convert::queue::TrackProgress {
                    track_label: "Track 1".to_string(),
                    step_description: "encoding".to_string(),
                    progress_fraction: 0.5,
                    epoch: 7,
                },
            );
        }

        assert!(manager.update_item_status(
            &item_id,
            processing_status("Writing metadata", 85.0, ConversionPhase::Tagging),
            85.0,
        ));

        let queue = manager.queue.try_read().expect("queue read lock");
        let item = queue
            .all_items()
            .into_iter()
            .find(|item| item.id == item_id)
            .expect("item exists");
        assert!(item.active_tracks.is_empty());
        assert_eq!(item.closed_track_epochs.get(&0), Some(&7));
    }

    #[test]
    fn delayed_track_processing_after_tagging_is_rejected() {
        let (manager, item_id) = test_manager_with_item();
        assert!(manager.update_item_status(
            &item_id,
            processing_status("Writing metadata", 85.0, ConversionPhase::Tagging),
            85.0,
        ));

        let delayed_update = processing_status(
            "Track 1 - step 1 of 2 - encoding · 10% of current track",
            45.0,
            ConversionPhase::Converting,
        );
        manager.update_track_progress(&item_id, 0, &delayed_update, 45.0, Some(99));

        let queue = manager.queue.try_read().expect("queue read lock");
        let item = queue
            .all_items()
            .into_iter()
            .find(|item| item.id == item_id)
            .expect("item exists");
        assert!(item.active_tracks.is_empty());
        assert!(item.closed_track_epochs.is_empty());
    }

    #[test]
    fn delayed_track_processing_after_terminal_is_rejected() {
        let (manager, item_id) = test_manager_with_item();
        assert!(manager.update_item_status(
            &item_id,
            ConversionStatus::Completed {
                output_path: PathBuf::from("/tmp/out.flac"),
                log_path: None,
            },
            100.0,
        ));

        let delayed_update = processing_status(
            "Track 1 - step 1 of 2 - encoding · 10% of current track",
            45.0,
            ConversionPhase::Converting,
        );
        manager.update_track_progress(&item_id, 0, &delayed_update, 45.0, Some(99));

        let queue = manager.queue.try_read().expect("queue read lock");
        let item = queue
            .all_items()
            .into_iter()
            .find(|item| item.id == item_id)
            .expect("item exists");
        assert!(item.active_tracks.is_empty());
        assert!(item.closed_track_epochs.is_empty());
    }


    #[test]
    fn delayed_clear_track_after_terminal_does_not_record_closed_epoch() {
        let (manager, item_id) = test_manager_with_item();
        assert!(manager.update_item_status(
            &item_id,
            ConversionStatus::Completed {
                output_path: PathBuf::from("/tmp/out.flac"),
                log_path: None,
            },
            100.0,
        ));

        manager.clear_track_progress(&item_id, 0, 99);

        let queue = manager.queue.try_read().expect("queue read lock");
        let item = queue
            .all_items()
            .into_iter()
            .find(|item| item.id == item_id)
            .expect("item exists");
        assert!(item.active_tracks.is_empty());
        assert!(item.closed_track_epochs.is_empty());
    }

    #[test]
    fn scan_directory_delegates_to_canonical_queue_expansion() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.flac");
        let loose = td.path().join("loose.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(&loose, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:12:00
"#,
        )
        .unwrap();

        let manager = ConversionManager::new(ConversionConfig::default());
        let files = manager.scan_directory(td.path()).expect("scan directory");
        let expected = crate::convert::queue_expansion::expand_paths_to_audio(&[
            td.path().to_path_buf(),
        ]);

        assert_eq!(files, expected);
        assert_eq!(files, vec![cue, loose]);
    }

    #[test]
    fn scan_directory_rejects_non_directory_without_queue_walk() {
        let td = tempfile::tempdir().expect("tempdir");
        let not_dir = td.path().join("track.flac");
        std::fs::write(&not_dir, b"not real flac").unwrap();

        let manager = ConversionManager::new(ConversionConfig::default());
        let err = manager
            .scan_directory(&not_dir)
            .expect_err("non-directory should be rejected explicitly");

        assert!(matches!(err, ConversionError::ValidationError(_)));
    }

}
