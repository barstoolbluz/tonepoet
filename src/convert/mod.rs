//! Audio conversion functionality for tonepoet
//!
//! This module provides comprehensive audio format conversion with support for:
//! - Multiple input/output formats (FLAC, WAV, AIFF, WavPack, MP3, AAC, Opus)
//! - Parallel processing with progress tracking
//! - Metadata preservation and ReplayGain calculation
//! - 7z archive extraction and processing

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
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

#[derive(Default)]
struct DirectoryQueuePlan {
    cue_sheets: Vec<PathBuf>,
    audio_files: Vec<PathBuf>,
    other_queueable: Vec<PathBuf>,
}

impl DirectoryQueuePlan {
    fn add_detected(&mut self, path: PathBuf, format: FileFormat) {
        match format {
            FileFormat::CueSheet => push_unique_path(&mut self.cue_sheets, path),
            FileFormat::Audio(_) => push_unique_path(&mut self.audio_files, path),
            FileFormat::SevenZip => push_unique_path(&mut self.other_queueable, path),
        }
    }

    fn into_queue_paths(self) -> Vec<PathBuf> {
        let referenced_audio = cue_referenced_audio_paths(&self.cue_sheets);
        let mut result = Vec::new();

        for cue in self.cue_sheets {
            push_unique_path(&mut result, cue);
        }
        for audio in self.audio_files {
            if !path_list_contains(&referenced_audio, &audio) {
                push_unique_path(&mut result, audio);
            }
        }
        for path in self.other_queueable {
            push_unique_path(&mut result, path);
        }

        result
    }
}

fn cue_referenced_audio_paths(cue_paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut referenced = Vec::new();
    for cue_path in cue_paths {
        match materializable_cue_referenced_audio_paths(cue_path) {
            Ok(paths) => {
                for path in paths {
                    push_unique_path(&mut referenced, path);
                }
            }
            Err(err) => {
                log::warn!(
                    "CUE {} is not materializer-compatible; not suppressing any referenced audio files: {}",
                    cue_path.display(),
                    err
                );
            }
        }
    }
    referenced
}

fn materializable_cue_referenced_audio_paths(cue_path: &Path) -> Result<Vec<PathBuf>, String> {
    let sheet = crate::tui::cue_parser::parse_cue_file(cue_path)
        .map_err(|err| format!("failed to parse CUE: {err}"))?;
    let parent = cue_path
        .parent()
        .ok_or_else(|| "CUE path has no parent directory".to_string())?;

    if sheet.tracks.is_empty() {
        return Err("CUE sheet has no tracks".to_string());
    }

    let mut referenced = Vec::new();
    let mut resolved_tracks = Vec::with_capacity(sheet.tracks.len());
    for track in &sheet.tracks {
        let index01 = track
            .index01_frames
            .ok_or_else(|| format!("track {} has no INDEX 01", track.number))?;
        let file_ref = track
            .file
            .as_deref()
            .ok_or_else(|| format!("track {} has no FILE reference", track.number))?;

        let resolved = match resolve_cue_file_reference(parent, file_ref) {
            CueReferenceResolution::Resolved(path) => path,
            CueReferenceResolution::Missing => {
                return Err(format!(
                    "track {} FILE reference {:?} was not found",
                    track.number, file_ref
                ));
            }
            CueReferenceResolution::Ambiguous(candidates) => {
                return Err(format!(
                    "track {} FILE reference {:?} was ambiguous: {}",
                    track.number,
                    file_ref,
                    format_candidate_paths_for_log(&candidates)
                ));
            }
        };

        if !matches!(FormatDetector::detect(&resolved), Ok(FileFormat::Audio(_))) {
            return Err(format!(
                "track {} FILE reference {:?} did not resolve to a supported audio file: {}",
                track.number,
                file_ref,
                resolved.display()
            ));
        }

        push_unique_path(&mut referenced, resolved.clone());
        resolved_tracks.push((track.number, resolved, index01));
    }

    validate_queue_cue_index_order(&resolved_tracks)?;
    Ok(referenced)
}

fn validate_queue_cue_index_order(resolved_tracks: &[(u32, PathBuf, u32)]) -> Result<(), String> {
    let mut previous_by_file: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for (track_number, path, index01) in resolved_tracks {
        let key = deterministic_path_sort_key(path);
        if let Some((previous_track, previous_index)) = previous_by_file.get(&key) {
            if index01 <= previous_index {
                return Err(format!(
                    "non-increasing INDEX 01 for track {} in {}; previous track {} was at frame {}",
                    track_number,
                    path.display(),
                    previous_track,
                    previous_index
                ));
            }
        }
        previous_by_file.insert(key, (*track_number, *index01));
    }
    Ok(())
}

#[derive(Debug)]
enum CueReferenceResolution {
    Resolved(PathBuf),
    Missing,
    Ambiguous(Vec<PathBuf>),
}

fn resolve_cue_file_reference(parent: &Path, file_ref: &str) -> CueReferenceResolution {
    let normalized_ref = file_ref.replace('\\', &std::path::MAIN_SEPARATOR.to_string());
    let raw_path = PathBuf::from(&normalized_ref);

    if raw_path.is_absolute() && raw_path.is_file() {
        return CueReferenceResolution::Resolved(raw_path);
    }

    let direct = parent.join(&raw_path);
    if direct.is_file() {
        return CueReferenceResolution::Resolved(direct);
    }

    let wanted_name = raw_path.file_name().and_then(|value| value.to_str());
    let wanted_stem = raw_path.file_stem().and_then(|value| value.to_str());
    let fallback_search_dir = cue_reference_fallback_search_dir(parent, &raw_path);

    if let Some(wanted) = wanted_name {
        let name_matches = collect_audio_reference_candidates(&fallback_search_dir, |path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(wanted))
                .unwrap_or(false)
        });
        match unique_queue_reference_candidate(name_matches) {
            CueReferenceResolution::Missing => {}
            other => return other,
        }
    }

    if let Some(wanted) = wanted_stem {
        let stem_matches = collect_audio_reference_candidates(&fallback_search_dir, |path| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .map(|value| value.eq_ignore_ascii_case(wanted))
                .unwrap_or(false)
        });
        return unique_queue_reference_candidate(stem_matches);
    }

    CueReferenceResolution::Missing
}


fn cue_reference_fallback_search_dir(parent: &Path, raw_path: &Path) -> PathBuf {
    raw_path
        .parent()
        .filter(|component| !component.as_os_str().is_empty())
        .map(|component| parent.join(component))
        .unwrap_or_else(|| parent.to_path_buf())
}

fn collect_audio_reference_candidates(
    parent: &Path,
    matches_reference: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };

    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && matches!(FormatDetector::detect(path), Ok(FileFormat::Audio(_)))
                && matches_reference(path)
        })
        .collect();
    candidates.sort_by_key(|path| deterministic_path_sort_key(path));
    candidates.dedup_by(|left, right| same_path_for_queue(left, right));
    candidates
}

fn unique_queue_reference_candidate(candidates: Vec<PathBuf>) -> CueReferenceResolution {
    match candidates.len() {
        0 => CueReferenceResolution::Missing,
        1 => CueReferenceResolution::Resolved(candidates.into_iter().next().unwrap()),
        _ => CueReferenceResolution::Ambiguous(candidates),
    }
}

fn deterministic_path_sort_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn format_candidate_paths_for_log(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn push_unique_path(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !path_list_contains(paths, &candidate) {
        paths.push(candidate);
    }
}

fn path_list_contains(paths: &[PathBuf], candidate: &Path) -> bool {
    paths
        .iter()
        .any(|existing| same_path_for_queue(existing, candidate))
}

fn same_path_for_queue(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
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

    /// Scan a directory for queueable conversion inputs.
    ///
    /// Build a complete directory plan before returning paths: CUE sheets are
    /// album control files, so any audio file referenced by any CUE in the scan
    /// tree is suppressed from the standalone file list. This keeps directory
    /// conversion idempotent for parent-CUE/child-audio layouts, per-track CUEs,
    /// single-image CUEs, and relative references such as `../disc.flac`.
    fn scan_directory(&self, dir: &Path) -> ConversionResult<Vec<PathBuf>> {
        let mut plan = DirectoryQueuePlan::default();

        for entry in walkdir::WalkDir::new(dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Ok(format) = FormatDetector::detect(path) {
                plan.add_detected(path.to_path_buf(), format);
            }
        }

        Ok(plan.into_queue_paths())
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
        // Use try_write() instead of blocking_write() to avoid panic in async context
        let mut queue = self.queue.try_write().map_err(|_| {
            ConversionError::ConversionFailed("Queue is busy, try again".to_string())
        })?;

        // Create item and mark as Queued (ready for processing)
        let mut item = ConversionItem::new_with_cue_sidecar_override(
            file.clone(),
            format,
            options,
            cue_sidecar_override,
        );
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
                    | ConversionStatus::Partial { .. }
                    | ConversionStatus::Failed { .. }
                    | ConversionStatus::Cancelled => {
                        Self::record_and_clear_active_tracks(item);
                        item.closed_track_epochs.clear();
                        item.completed_at = Some(chrono::Utc::now());
                        match &status {
                            ConversionStatus::Completed { output_path, .. }
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
    fn scan_directory_suppresses_cue_referenced_child_audio() {
        let td = tempfile::tempdir().expect("tempdir");
        let subdir = td.path().join("disc");
        std::fs::create_dir(&subdir).unwrap();
        let image = subdir.join("image.flac");
        let loose = td.path().join("loose.flac");
        let cue = td.path().join("album.cue");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(&loose, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "disc/image.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let manager = ConversionManager::new(ConversionConfig::default());
        let files = manager.scan_directory(td.path()).expect("scan directory");
        assert!(path_list_contains(&files, &cue));
        assert!(path_list_contains(&files, &loose));
        assert!(!path_list_contains(&files, &image));
    }

    #[test]
    fn scan_directory_suppresses_relative_parent_audio_referenced_by_cue() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue_dir = td.path().join("cue_dir");
        let audio_dir = td.path().join("audio");
        std::fs::create_dir(&cue_dir).unwrap();
        std::fs::create_dir(&audio_dir).unwrap();
        let image = audio_dir.join("image.flac");
        let cue = cue_dir.join("album.cue");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "../audio/image.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let manager = ConversionManager::new(ConversionConfig::default());
        let files = manager.scan_directory(td.path()).expect("scan directory");
        assert!(path_list_contains(&files, &cue));
        assert!(!path_list_contains(&files, &image));
    }

    #[test]
    fn scan_directory_keeps_all_ambiguous_stem_matches() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let flac = td.path().join("album.flac");
        let wav = td.path().join("album.wav");
        std::fs::write(&flac, b"not real flac").unwrap();
        std::fs::write(&wav, b"not real wav").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album.ape" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let manager = ConversionManager::new(ConversionConfig::default());
        let files = manager.scan_directory(td.path()).expect("scan directory");
        assert!(path_list_contains(&files, &cue));
        assert!(path_list_contains(&files, &flac));
        assert!(path_list_contains(&files, &wav));
    }

    #[test]
    fn scan_directory_suppresses_subdirectory_extension_mismatch_reference() {
        let td = tempfile::tempdir().expect("tempdir");
        let disc = td.path().join("disc");
        std::fs::create_dir(&disc).unwrap();
        let cue = td.path().join("album.cue");
        let image = disc.join("image.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "disc/image.wav" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:00:00
"#,
        )
        .unwrap();

        let manager = ConversionManager::new(ConversionConfig::default());
        let files = manager.scan_directory(td.path()).expect("scan directory");
        assert!(path_list_contains(&files, &cue));
        assert!(!path_list_contains(&files, &image));
    }

    #[test]
    fn scan_directory_does_not_suppress_audio_for_cue_missing_index01() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "No INDEX 01"
"#,
        )
        .unwrap();

        let manager = ConversionManager::new(ConversionConfig::default());
        let files = manager.scan_directory(td.path()).expect("scan directory");
        assert!(path_list_contains(&files, &cue));
        assert!(path_list_contains(&files, &image));
    }

    #[test]
    fn scan_directory_does_not_suppress_audio_for_non_increasing_cue_indexes() {
        let td = tempfile::tempdir().expect("tempdir");
        let cue = td.path().join("album.cue");
        let image = td.path().join("album.flac");
        std::fs::write(&image, b"not real flac").unwrap();
        std::fs::write(
            &cue,
            r#"FILE "album.flac" WAVE
  TRACK 01 AUDIO
    INDEX 01 00:10:00
  TRACK 02 AUDIO
    INDEX 01 00:05:00
"#,
        )
        .unwrap();

        let manager = ConversionManager::new(ConversionConfig::default());
        let files = manager.scan_directory(td.path()).expect("scan directory");
        assert!(path_list_contains(&files, &cue));
        assert!(path_list_contains(&files, &image));
    }

}
