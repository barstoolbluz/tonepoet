//! Conversion queue management

use std::path::PathBuf;
use std::collections::VecDeque;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use super::formats::{AudioFormat, FileFormat, ConversionOptions};

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
            ConversionPhase::Extracting => 0.15,      // 15%
            ConversionPhase::Analyzing => 0.05,       // 5%
            ConversionPhase::Renaming => 0.10,        // 10%
            ConversionPhase::Tagging => 0.10,         // 10%
            ConversionPhase::Converting => 0.50,      // 50% (bulk of the work)
            ConversionPhase::PostProcessing => 0.05,  // 5%
            ConversionPhase::Finalizing => 0.05,      // 5%
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
        progress: f32,             // Keep for backward compatibility
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
    /// Archive password (for 7z files)
    pub archive_password: Option<String>,
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
            archive_password: None,
            pipeline_request: None,
        }
    }
}

impl ConversionItem {
    /// Create a new conversion item
    pub fn new(
        input_path: PathBuf,
        input_format: FileFormat,
        options: ConversionOptions,
    ) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let file_size = std::fs::metadata(&input_path)
            .map(|m| m.len())
            .unwrap_or(0);
        
        let archive_password = None;
        
        Self {
            id,
            input_path,
            input_format,
            output_format: options.output_format,
            output_path: None,
            options,
            status: ConversionStatus::NotConfigured,  // Items start as NotConfigured; user must select settings
            queued_at: Utc::now(),
            started_at: None,
            completed_at: None,
            file_size,
            selected: false,
            archive_password,
            pipeline_request: None,
        }
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



fn status_progress(status: &ConversionStatus) -> f32 {
    match status {
        ConversionStatus::Processing { progress, .. } => *progress,
        ConversionStatus::Completed { .. } | ConversionStatus::Partial { .. } => 100.0,
        ConversionStatus::Queued | ConversionStatus::Paused | ConversionStatus::NotConfigured => 0.0,
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
    
    /// Add an item to the queue
    pub fn add_item(&mut self, path: PathBuf, format: FileFormat, options: ConversionOptions) {
        let item = ConversionItem::new(path, format, options);
        self.items.push_back(item);
    }

    /// Add a pre-configured item directly to the queue
    pub fn add_item_direct(&mut self, item: ConversionItem) {
        self.items.push_back(item);
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
        self.items.iter()
            .filter(|item| item.status == ConversionStatus::Queued)
            .collect()
    }
    
    /// Get completed items
    pub fn completed_items(&self) -> usize {
        self.completed.iter()
            .filter(|item| matches!(item.status, ConversionStatus::Completed { .. }))
            .count()
    }
    
    /// Get failed items
    pub fn failed_items(&self) -> usize {
        self.completed.iter()
            .filter(|item| matches!(item.status, ConversionStatus::Failed { .. }))
            .count()
    }

    /// Get partial items (counted separately from completed and failed)
    pub fn partial_items(&self) -> usize {
        self.completed.iter()
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
        self.items.retain(|item| !matches!(item.status, ConversionStatus::Completed { .. }));
        self.completed.retain(|item| !matches!(item.status, ConversionStatus::Completed { .. }));
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
    
    /// Update item status by ID
    pub fn update_item_status(&mut self, item_id: &str, status: ConversionStatus) -> bool {
        if let Some(item) = self.find_item_mut(item_id) {
            item.status = status;
            true
        } else {
            false
        }
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
            !(item.selected && matches!(item.status, ConversionStatus::Queued | ConversionStatus::Paused))
        });
    }
    
    /// Retry failed items
    pub fn retry_failed(&mut self) {
        let mut to_retry = Vec::new();

        for item in &mut self.completed {
            if item.selected && item.can_retry() {
                let mut new_item = item.clone();
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