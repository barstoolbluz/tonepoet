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

pub mod formats;
pub mod labels;
pub mod metadata;
pub mod pipeline;
pub mod processor;
pub mod queue;
pub mod renaming;
pub mod simple_wizard;
pub mod wizard;
pub mod wizard_integration;

pub use formats::{
    AacProfile, AudioFormat, ConversionOptions, FileFormat, FormatDetector, Mp3BitrateMode,
    QualitySettings, WavPackMode,
};
pub use labels::{detect_pressing_info, LabelInfo};
pub use metadata::{extract_metadata_from_flac, extract_year_from_flac_files, FlacMetadata};
pub use processor::{process_item, ConversionProcessor, ProcessorConfig, ProgressUpdate};
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

fn _status_progress_for_update(status: &ConversionStatus, progress_hint: f32) -> f32 {
    match status {
        ConversionStatus::Processing { progress, .. } => *progress,
        ConversionStatus::Completed { .. } | ConversionStatus::Partial { .. } => 100.0,
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

    /// Scan a directory for audio files
    fn scan_directory(&self, dir: &Path) -> ConversionResult<Vec<PathBuf>> {
        let mut files = Vec::new();

        for entry in walkdir::WalkDir::new(dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.is_file() {
                if let Ok(_format) = FormatDetector::detect(path) {
                    files.push(path.to_path_buf());
                }
            }
        }

        Ok(files)
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
        let format = FormatDetector::detect(&file)?;
        // Use try_write() instead of blocking_write() to avoid panic in async context
        let mut queue = self.queue.try_write().map_err(|_| {
            ConversionError::ConversionFailed("Queue is busy, try again".to_string())
        })?;

        // Create item and mark as Queued (ready for processing)
        let mut item = ConversionItem::new(file.clone(), format, options);
        item.archive_password = archive_password;
        item.status = ConversionStatus::Queued;
        let id = item.id.clone();
        queue.items_mut().push_back(item);

        log::info!("Added file ready for processing: {:?} (id: {})", file, id);
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

    /// Update item status by ID
    pub fn update_item_status(&self, id: &str, status: ConversionStatus, _progress: f32) -> bool {
        if let Ok(mut queue) = self.queue.try_write() {
            if let Some(item) = queue.find_item_mut(id) {
                // Just use the status as-is - it already has all the correct fields including phase
                item.status = status.clone();

                // Update timestamps based on status
                match &status {
                    ConversionStatus::Processing { .. } if item.started_at.is_none() => {
                        item.started_at = Some(chrono::Utc::now());
                    }
                    ConversionStatus::Completed { output_path, .. } => {
                        item.completed_at = Some(chrono::Utc::now());
                        item.output_path = Some(output_path.clone());
                    }
                    ConversionStatus::Partial { output_path, .. } => {
                        item.completed_at = Some(chrono::Utc::now());
                        item.output_path = Some(output_path.clone());
                    }
                    ConversionStatus::Failed { .. } | ConversionStatus::Cancelled => {
                        item.completed_at = Some(chrono::Utc::now());
                    }
                    _ => {}
                }
                return true;
            }
        }
        false
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
